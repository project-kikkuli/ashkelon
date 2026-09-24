mod claude_channel;
mod claude_settings;
mod codex_config;
mod launch_agent;
mod shell_wrapper;
mod state;

use std::path::Path;

use anyhow::Context;

use crate::config::Config;
use crate::wake::runner::{CommandRunner, SystemRunner};

use state::InstallState;

/// `install`/`uninstall` only ever run against the real machine from the CLI; every code path
/// that actually writes a file or shells out to `launchctl` takes `home`/`runner` as parameters
/// so tests can point them at a throwaway `HOME` and a fake runner instead (see
/// `tests::` below and `wake::runner::CommandRunner`'s own doc comment for why that trait exists).
pub async fn install(cfg: &Config, config_path: Option<&Path>) -> anyhow::Result<()> {
    let home = dirs::home_dir().context("locating $HOME")?;
    let bin = std::env::current_exe().context("locating ashkelon's own executable path")?;
    let shell = std::env::var("SHELL").unwrap_or_default();
    let summary = install_with(cfg, config_path, &home, &bin, &shell, &SystemRunner).await?;
    for line in summary {
        println!("{line}");
    }
    Ok(())
}

pub async fn uninstall(cfg: &Config) -> anyhow::Result<()> {
    let home = dirs::home_dir().context("locating $HOME")?;
    let summary = uninstall_with(cfg, &home, &SystemRunner).await?;
    for line in summary {
        println!("{line}");
    }
    Ok(())
}

fn current_uid(home: &Path) -> anyhow::Result<u32> {
    use std::os::unix::fs::MetadataExt;
    Ok(std::fs::metadata(home)
        .with_context(|| format!("stat {}", home.display()))?
        .uid())
}

fn listen_addr(cfg: &Config) -> String {
    cfg.listen.clone().unwrap_or_else(|| "127.0.0.1:8484".to_string())
}

pub async fn install_with(
    cfg: &Config,
    config_path: Option<&Path>,
    home: &Path,
    bin: &Path,
    shell: &str,
    runner: &dyn CommandRunner,
) -> anyhow::Result<Vec<String>> {
    let mut state = InstallState::load(&cfg.state_dir());
    let mut summary = Vec::new();
    let listen = listen_addr(cfg);

    let plist_path = launch_agent::plist_path(home);
    state.track_file(&plist_path)?;
    let plist = launch_agent::render(bin, config_path, &cfg.log_dir());
    if let Some(parent) = plist_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(&plist_path, plist).with_context(|| format!("writing {}", plist_path.display()))?;
    let uid = current_uid(home)?;
    launch_agent::bootstrap(runner, uid, &plist_path).await?;
    state.launch_agent_loaded = true;
    summary.push(format!(
        "{}: LaunchAgent {} loaded (RunAtLoad, KeepAlive)",
        plist_path.display(),
        launch_agent::LABEL
    ));

    summary.push(claude_settings::apply(
        home,
        &mut state,
        &format!("http://{listen}/anthropic"),
    )?);

    summary.push(codex_config::apply(
        home,
        &mut state,
        &format!("http://{listen}/chatgpt/backend-api/codex"),
        &format!("http://{listen}/openai/v1"),
    )?);

    let socket_dir = claude_channel::default_socket_dir(&cfg.state_dir());
    summary.push(claude_channel::apply(
        home,
        &mut state,
        bin,
        &socket_dir,
        &format!("http://{listen}/internal/claude-channel"),
    )?);

    summary.push(shell_wrapper::apply(home, shell, &mut state)?);

    state.save(&cfg.state_dir())?;
    Ok(summary)
}

pub async fn uninstall_with(cfg: &Config, home: &Path, runner: &dyn CommandRunner) -> anyhow::Result<Vec<String>> {
    let state = InstallState::load(&cfg.state_dir());
    let mut summary = Vec::new();

    if state.launch_agent_loaded {
        let uid = current_uid(home)?;
        launch_agent::bootout(runner, uid).await?;
        summary.push(format!("{} unloaded", launch_agent::LABEL));
    }

    summary.extend(state.restore_files());

    let state_file = cfg.state_dir().join("install-state.json");
    let _ = std::fs::remove_file(state_file);
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wake::runner::CommandOutput;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Mutex;
    use tempfile::tempdir;

    #[derive(Default)]
    struct FakeRunner {
        calls: Mutex<Vec<(String, Vec<String>)>>,
    }

    impl CommandRunner for FakeRunner {
        fn run<'a>(
            &'a self,
            program: &'a str,
            args: &'a [String],
        ) -> Pin<Box<dyn Future<Output = anyhow::Result<CommandOutput>> + Send + 'a>> {
            self.calls.lock().unwrap().push((program.to_string(), args.to_vec()));
            Box::pin(async move {
                Ok(CommandOutput {
                    success: true,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                })
            })
        }
    }

    fn cfg_for(state_dir: &Path) -> Config {
        Config {
            listen: Some("127.0.0.1:8484".to_string()),
            state_dir: Some(state_dir.to_path_buf()),
            log_dir: Some(state_dir.join("logs")),
            ..Config::default()
        }
    }

    #[tokio::test]
    async fn install_writes_every_expected_file_and_loads_the_agent() {
        let home = tempdir().unwrap();
        let state_dir = tempdir().unwrap();
        let cfg = cfg_for(state_dir.path());
        let runner = FakeRunner::default();

        let summary = install_with(
            &cfg,
            None,
            home.path(),
            Path::new("/opt/ashkelon/bin/ashkelon"),
            "/bin/zsh",
            &runner,
        )
        .await
        .unwrap();
        assert_eq!(summary.len(), 5);

        assert!(launch_agent::plist_path(home.path()).exists());
        assert!(home.path().join(".claude/settings.json").exists());
        assert!(home.path().join(".codex/config.toml").exists());
        assert!(home.path().join(".claude.json").exists());
        assert!(home.path().join(".zshrc").exists());

        let calls = runner.calls.lock().unwrap();
        assert!(calls
            .iter()
            .any(|(p, a)| p == "launchctl" && a.contains(&"bootstrap".to_string())));

        let claude_settings: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(home.path().join(".claude/settings.json")).unwrap()).unwrap();
        assert_eq!(
            claude_settings["env"]["ANTHROPIC_BASE_URL"],
            "http://127.0.0.1:8484/anthropic"
        );
    }

    #[tokio::test]
    async fn install_is_idempotent() {
        let home = tempdir().unwrap();
        let state_dir = tempdir().unwrap();
        let cfg = cfg_for(state_dir.path());
        let runner = FakeRunner::default();

        install_with(&cfg, None, home.path(), Path::new("/bin/ashkelon"), "/bin/zsh", &runner)
            .await
            .unwrap();
        install_with(&cfg, None, home.path(), Path::new("/bin/ashkelon"), "/bin/zsh", &runner)
            .await
            .unwrap();

        let content = std::fs::read_to_string(home.path().join(".zshrc")).unwrap();
        assert_eq!(content.matches("ashkelon channel wrapper").count(), 2); // one BEGIN + one END marker line
    }

    #[tokio::test]
    async fn uninstall_reverses_a_fresh_install_completely() {
        let home = tempdir().unwrap();
        let state_dir = tempdir().unwrap();
        let cfg = cfg_for(state_dir.path());
        let runner = FakeRunner::default();

        install_with(&cfg, None, home.path(), Path::new("/bin/ashkelon"), "/bin/zsh", &runner)
            .await
            .unwrap();
        uninstall_with(&cfg, home.path(), &runner).await.unwrap();

        assert!(!launch_agent::plist_path(home.path()).exists());
        assert!(!home.path().join(".claude/settings.json").exists());
        assert!(!home.path().join(".codex/config.toml").exists());
        assert!(!home.path().join(".claude.json").exists());
        assert!(!home.path().join(".zshrc").exists());

        let calls = runner.calls.lock().unwrap();
        assert!(calls
            .iter()
            .any(|(p, a)| p == "launchctl" && a.contains(&"bootout".to_string())));
    }

    #[tokio::test]
    async fn uninstall_restores_pre_existing_content_instead_of_deleting() {
        let home = tempdir().unwrap();
        let state_dir = tempdir().unwrap();
        let cfg = cfg_for(state_dir.path());
        let runner = FakeRunner::default();

        std::fs::create_dir_all(home.path().join(".claude")).unwrap();
        std::fs::write(home.path().join(".claude/settings.json"), r#"{"model":"opus"}"#).unwrap();
        std::fs::write(home.path().join(".zshrc"), "export FOO=bar\n").unwrap();

        install_with(&cfg, None, home.path(), Path::new("/bin/ashkelon"), "/bin/zsh", &runner)
            .await
            .unwrap();
        uninstall_with(&cfg, home.path(), &runner).await.unwrap();

        assert_eq!(
            std::fs::read_to_string(home.path().join(".claude/settings.json")).unwrap(),
            r#"{"model":"opus"}"#
        );
        assert_eq!(
            std::fs::read_to_string(home.path().join(".zshrc")).unwrap(),
            "export FOO=bar\n"
        );
    }
}
