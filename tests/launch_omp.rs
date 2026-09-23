//! omp routes through a temporary `PI_CODING_AGENT_DIR` overlay (see `launch::omp`), never the
//! user's real `~/.omp/agent` — every fixture here is a fake agent dir under a tempdir.
//! `PI_CODING_AGENT_DIR` is process-global, so these tests share one lock and always set it
//! explicitly (never relying on ambient absence, which would otherwise touch the real dir).
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ashkelon::launch::{self, LaunchOptions};

static ENV_LOCK: Mutex<()> = Mutex::new(());

const BASE: &str = "http://127.0.0.1:9999/s/deadbeef";

struct EnvGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl EnvGuard {
    fn set(agent_dir: &Path) -> EnvGuard {
        let lock = ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        std::env::set_var("PI_CODING_AGENT_DIR", agent_dir);
        EnvGuard { _lock: lock }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        std::env::remove_var("PI_CODING_AGENT_DIR");
    }
}

fn fixture(name: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("ashkelon-omp-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let agent_dir = root.join("agent");
    let state_dir = root.join("state");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::create_dir_all(&state_dir).unwrap();
    (agent_dir, state_dir)
}

fn options(state_dir: &Path) -> LaunchOptions {
    LaunchOptions { state_dir: state_dir.to_path_buf(), no_channel: false }
}

fn env_value<'a>(env: &'a [(String, String)], key: &str) -> Option<&'a str> {
    env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

#[test]
fn routes_anthropic_and_openai_as_plain_env_vars() {
    let (agent_dir, state_dir) = fixture("basic");
    let _guard = EnvGuard::set(&agent_dir);

    let plan = launch::plan("omp", BASE, "omp-basic", &[], &options(&state_dir)).unwrap();

    assert_eq!(env_value(&plan.env, "ANTHROPIC_BASE_URL"), Some(format!("{BASE}/anthropic").as_str()));
    assert_eq!(env_value(&plan.env, "OPENAI_BASE_URL"), Some(format!("{BASE}/openai/v1").as_str()));
    // omp does not read OPENROUTER_BASE_URL at all; setting it would be a silent no-op.
    assert!(env_value(&plan.env, "OPENROUTER_BASE_URL").is_none());

    let _ = std::fs::remove_dir_all(agent_dir.parent().unwrap());
}

#[test]
fn overrides_openrouter_openai_codex_and_ollama_in_models_yml() {
    let (agent_dir, state_dir) = fixture("models-yml");
    let _guard = EnvGuard::set(&agent_dir);

    let plan = launch::plan("omp", BASE, "omp-models", &[], &options(&state_dir)).unwrap();

    let agent_dir_env = env_value(&plan.env, "PI_CODING_AGENT_DIR").unwrap();
    let overlay = plan.overlay_home.as_ref().unwrap();
    assert_eq!(agent_dir_env, overlay.overlay_dir.to_string_lossy());
    assert_eq!(overlay.real_home, agent_dir);
    assert_eq!(overlay.generated_file_name, "models.yml");

    let models_text = std::fs::read_to_string(overlay.overlay_dir.join("models.yml")).unwrap();
    let models: serde_yaml::Value = serde_yaml::from_str(&models_text).unwrap();
    assert_eq!(models["providers"]["openrouter"]["baseUrl"], format!("{BASE}/openrouter/api/v1"));
    assert_eq!(models["providers"]["openai-codex"]["baseUrl"], format!("{BASE}/chatgpt/backend-api"));
    assert_eq!(models["providers"]["ollama"]["baseUrl"], format!("{BASE}/ollama/v1"));

    let _ = std::fs::remove_dir_all(agent_dir.parent().unwrap());
}

#[test]
fn preexisting_models_yml_entries_and_provider_overrides_are_preserved() {
    let (agent_dir, state_dir) = fixture("preserve");
    std::fs::write(
        agent_dir.join("models.yml"),
        "providers:\n  openrouter:\n    compat:\n      replayUnsignedThinking: false\n  my-custom:\n    baseUrl: https://example.test\n",
    )
    .unwrap();
    let _guard = EnvGuard::set(&agent_dir);

    let plan = launch::plan("omp", BASE, "omp-preserve", &[], &options(&state_dir)).unwrap();
    let overlay = plan.overlay_home.as_ref().unwrap();
    let models_text = std::fs::read_to_string(overlay.overlay_dir.join("models.yml")).unwrap();
    let models: serde_yaml::Value = serde_yaml::from_str(&models_text).unwrap();

    // The relay override is added...
    assert_eq!(models["providers"]["openrouter"]["baseUrl"], format!("{BASE}/openrouter/api/v1"));
    // ...without clobbering a sibling key already set on that same provider...
    assert_eq!(models["providers"]["openrouter"]["compat"]["replayUnsignedThinking"], false);
    // ...or an unrelated provider entry.
    assert_eq!(models["providers"]["my-custom"]["baseUrl"], "https://example.test");

    // The real models.yml is untouched — only the overlay copy carries the override.
    let real_text = std::fs::read_to_string(agent_dir.join("models.yml")).unwrap();
    assert_eq!(
        real_text,
        "providers:\n  openrouter:\n    compat:\n      replayUnsignedThinking: false\n  my-custom:\n    baseUrl: https://example.test\n"
    );

    let _ = std::fs::remove_dir_all(agent_dir.parent().unwrap());
}

#[test]
fn preexisting_agent_dir_entries_are_symlinked_not_copied() {
    let (agent_dir, state_dir) = fixture("symlink");
    std::fs::create_dir_all(agent_dir.join("sessions")).unwrap();
    std::fs::write(agent_dir.join("sessions").join("existing.json"), "{}").unwrap();
    let _guard = EnvGuard::set(&agent_dir);

    let plan = launch::plan("omp", BASE, "omp-symlink", &[], &options(&state_dir)).unwrap();
    let overlay = plan.overlay_home.as_ref().unwrap();

    let sessions_link = overlay.overlay_dir.join("sessions");
    assert!(sessions_link.symlink_metadata().unwrap().file_type().is_symlink());
    assert_eq!(std::fs::read_to_string(sessions_link.join("existing.json")).unwrap(), "{}");

    let _ = std::fs::remove_dir_all(agent_dir.parent().unwrap());
}

#[test]
fn from_claude_is_no_longer_refused() {
    let (agent_dir, state_dir) = fixture("from-claude");
    let _guard = EnvGuard::set(&agent_dir);

    let plan = launch::plan("omp", BASE, "omp-from-claude", &["--from-claude".to_string()], &options(&state_dir)).unwrap();
    assert!(plan.args.contains(&"--from-claude".to_string()));

    let _ = std::fs::remove_dir_all(agent_dir.parent().unwrap());
}

#[test]
fn reconcile_moves_back_a_file_omp_materialized_fresh_in_the_overlay() {
    let (agent_dir, state_dir) = fixture("reconcile");

    let overlay_dir = state_dir.join("overlay");
    launch::build_home_overlay(&agent_dir, &overlay_dir, "models.yml").unwrap();
    std::fs::write(overlay_dir.join("models.yml"), "generated: true\n").unwrap();
    std::fs::write(overlay_dir.join("new-model-cache.json"), "{\"fresh\":true}").unwrap();

    let overlay = ashkelon::launch::OverlayHome { overlay_dir, real_home: agent_dir.clone(), generated_file_name: "models.yml".to_string() };
    let moved = launch::reconcile_home_overlay(&overlay).unwrap();

    assert_eq!(moved, vec![agent_dir.join("new-model-cache.json")]);
    assert_eq!(std::fs::read_to_string(agent_dir.join("new-model-cache.json")).unwrap(), "{\"fresh\":true}");
    assert!(!agent_dir.join("models.yml").exists());

    let _ = std::fs::remove_dir_all(agent_dir.parent().unwrap());
}
