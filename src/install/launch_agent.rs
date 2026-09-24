use std::path::{Path, PathBuf};

use crate::wake::runner::CommandRunner;

pub const LABEL: &str = "com.ashkelon.serve";

pub fn plist_path(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents").join(format!("{LABEL}.plist"))
}

/// `bin` and `config_path` are always absolute: a LaunchAgent runs with no shell and no working
/// directory the caller controls, so a relative path would resolve against launchd's own cwd.
pub fn render(bin: &Path, config_path: Option<&Path>, log_dir: &Path) -> String {
    let mut program_args = format!(
        "        <string>{}</string>\n        <string>serve</string>\n",
        xml_escape(&bin.to_string_lossy())
    );
    if let Some(config_path) = config_path {
        program_args.push_str(&format!(
            "        <string>--config</string>\n        <string>{}</string>\n",
            xml_escape(&config_path.to_string_lossy())
        ));
    }
    let stdout = log_dir.join("serve.stdout.log");
    let stderr = log_dir.join("serve.stderr.log");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
{program_args}    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{}</string>
    <key>StandardErrorPath</key>
    <string>{}</string>
</dict>
</plist>
"#,
        xml_escape(&stdout.to_string_lossy()),
        xml_escape(&stderr.to_string_lossy()),
    )
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn domain(uid: u32) -> String {
    format!("gui/{uid}")
}

fn service_target(uid: u32) -> String {
    format!("{}/{LABEL}", domain(uid))
}

/// Loads the LaunchAgent, replacing any already-loaded instance (an install rerun, or a stale
/// load from a previous binary path) rather than failing on it.
pub async fn bootstrap(runner: &dyn CommandRunner, uid: u32, plist_path: &Path) -> anyhow::Result<()> {
    // Best-effort: fails harmlessly when nothing is loaded yet.
    let _ = runner
        .run("launchctl", &["bootout".to_string(), service_target(uid)])
        .await;
    let out = runner
        .run(
            "launchctl",
            &[
                "bootstrap".to_string(),
                domain(uid),
                plist_path.to_string_lossy().into_owned(),
            ],
        )
        .await?;
    anyhow::ensure!(
        out.success,
        "launchctl bootstrap failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(())
}

/// Unloads the LaunchAgent. Not an error if it was never loaded (uninstall on a broken or
/// never-completed install).
pub async fn bootout(runner: &dyn CommandRunner, uid: u32) -> anyhow::Result<()> {
    let _ = runner
        .run("launchctl", &["bootout".to_string(), service_target(uid)])
        .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_absolute_paths_and_keepalive() {
        let xml = render(
            Path::new("/usr/local/bin/ashkelon"),
            Some(Path::new("/home/x/config.toml")),
            Path::new("/home/x/logs"),
        );
        assert!(xml.contains("<string>/usr/local/bin/ashkelon</string>"));
        assert!(xml.contains("<string>serve</string>"));
        assert!(xml.contains("<string>--config</string>"));
        assert!(xml.contains("<string>/home/x/config.toml</string>"));
        assert!(xml.contains("<key>KeepAlive</key>\n    <true/>"));
        assert!(xml.contains("/home/x/logs/serve.stdout.log"));
    }

    #[test]
    fn renders_without_config_path() {
        let xml = render(Path::new("/bin/ashkelon"), None, Path::new("/logs"));
        assert!(!xml.contains("--config"));
    }
}
