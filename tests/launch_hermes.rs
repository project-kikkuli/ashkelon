//! Hermes routes through a temporary `HERMES_HOME` overlay (see `launch::hermes`), never the
//! user's real one — every fixture here is a fake home under a tempdir. `HERMES_HOME` and the
//! two credential env vars `launch::hermes::plan` reads are process-global, so these tests share
//! one lock and always set every var they depend on (never relying on ambient absence).
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ashkelon::launch::{self, LaunchOptions};

static ENV_LOCK: Mutex<()> = Mutex::new(());

const BASE: &str = "http://127.0.0.1:9999/s/deadbeef";

struct EnvGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl EnvGuard {
    fn set(home: &Path, anthropic_key: Option<&str>, openrouter_key: Option<&str>) -> EnvGuard {
        let lock = ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        std::env::set_var("HERMES_HOME", home);
        set_or_remove("ANTHROPIC_API_KEY", anthropic_key);
        set_or_remove("OPENROUTER_API_KEY", openrouter_key);
        EnvGuard { _lock: lock }
    }
}

fn set_or_remove(name: &str, value: Option<&str>) {
    match value {
        Some(v) => std::env::set_var(name, v),
        None => std::env::remove_var(name),
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        std::env::remove_var("HERMES_HOME");
        std::env::remove_var("ANTHROPIC_API_KEY");
        std::env::remove_var("OPENROUTER_API_KEY");
    }
}

fn fixture(name: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("ashkelon-hermes-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let state_dir = root.join("state");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&state_dir).unwrap();
    (home, state_dir)
}

fn options(state_dir: &Path) -> LaunchOptions {
    LaunchOptions {
        state_dir: state_dir.to_path_buf(),
        no_channel: false,
    }
}

#[test]
fn anthropic_with_api_key_routes_through_the_isolated_named_provider() {
    let (home, state_dir) = fixture("anthropic-ok");
    std::fs::write(home.join("config.yaml"), "model:\n  provider: anthropic\n").unwrap();
    std::fs::create_dir_all(home.join("sessions")).unwrap();
    std::fs::write(home.join("sessions").join("existing.json"), "{}").unwrap();

    let _guard = EnvGuard::set(&home, Some("sk-ant-test"), None);
    let plan = launch::plan("hermes", BASE, "hermes-anthropic", &[], &options(&state_dir)).unwrap();

    assert_eq!(plan.program, "hermes");
    assert_eq!(plan.args, vec!["--provider".to_string(), "ashkelon-relay".to_string()]);

    let overlay = plan.overlay_home.as_ref().unwrap();
    assert_eq!(overlay.real_home, home);
    assert_eq!(overlay.generated_file_name, "config.yaml");

    let overlaid_text = std::fs::read_to_string(overlay.overlay_dir.join("config.yaml")).unwrap();
    let overlaid: serde_yaml::Value = serde_yaml::from_str(&overlaid_text).unwrap();
    assert_eq!(
        overlaid["providers"]["ashkelon-relay"]["api"],
        format!("{BASE}/anthropic")
    );
    assert_eq!(overlaid["providers"]["ashkelon-relay"]["key_env"], "ANTHROPIC_API_KEY");
    assert_eq!(
        overlaid["providers"]["ashkelon-relay"]["api_mode"],
        "anthropic_messages"
    );

    // The real config.yaml is untouched — only the overlay copy carries the override.
    assert_eq!(
        std::fs::read_to_string(home.join("config.yaml")).unwrap(),
        "model:\n  provider: anthropic\n"
    );

    // Pre-existing entries are symlinked straight through, not copied.
    let sessions_link = overlay.overlay_dir.join("sessions");
    assert!(sessions_link.symlink_metadata().unwrap().file_type().is_symlink());
    assert_eq!(
        std::fs::read_to_string(sessions_link.join("existing.json")).unwrap(),
        "{}"
    );

    let _ = std::fs::remove_dir_all(home.parent().unwrap());
}

#[test]
fn anthropic_without_any_portable_key_refuses() {
    let (home, state_dir) = fixture("anthropic-refuse");
    std::fs::write(home.join("config.yaml"), "model:\n  provider: anthropic\n").unwrap();

    let _guard = EnvGuard::set(&home, None, None);
    let err = launch::plan("hermes", BASE, "hermes-anthropic-refuse", &[], &options(&state_dir)).unwrap_err();
    assert!(err.to_string().contains("ANTHROPIC_API_KEY"));
    assert!(err.to_string().to_lowercase().contains("keychain"));

    let _ = std::fs::remove_dir_all(home.parent().unwrap());
}

#[test]
fn anthropic_key_in_the_real_dotenv_file_is_also_honored() {
    let (home, state_dir) = fixture("anthropic-dotenv");
    std::fs::write(home.join("config.yaml"), "model:\n  provider: anthropic\n").unwrap();
    std::fs::write(home.join(".env"), "ANTHROPIC_API_KEY=sk-ant-from-dotenv\n").unwrap();

    let _guard = EnvGuard::set(&home, None, None);
    let plan = launch::plan("hermes", BASE, "hermes-dotenv", &[], &options(&state_dir)).unwrap();
    let overlay = plan.overlay_home.as_ref().unwrap();
    let overlaid_text = std::fs::read_to_string(overlay.overlay_dir.join("config.yaml")).unwrap();
    let overlaid: serde_yaml::Value = serde_yaml::from_str(&overlaid_text).unwrap();
    assert_eq!(
        overlaid["providers"]["ashkelon-relay"]["api"],
        format!("{BASE}/anthropic")
    );

    // The real .env is symlinked through untouched, so Hermes's other env-derived behavior
    // (unrelated to the relayed provider) keeps working.
    assert!(overlay
        .overlay_dir
        .join(".env")
        .symlink_metadata()
        .unwrap()
        .file_type()
        .is_symlink());

    let _ = std::fs::remove_dir_all(home.parent().unwrap());
}

#[test]
fn openrouter_with_api_key_routes_through_the_isolated_named_provider() {
    let (home, state_dir) = fixture("openrouter-ok");
    std::fs::write(home.join("config.yaml"), "model:\n  provider: openrouter\n").unwrap();

    let _guard = EnvGuard::set(&home, None, Some("sk-or-test"));
    let plan = launch::plan("hermes", BASE, "hermes-openrouter", &[], &options(&state_dir)).unwrap();
    let overlay = plan.overlay_home.as_ref().unwrap();
    let overlaid_text = std::fs::read_to_string(overlay.overlay_dir.join("config.yaml")).unwrap();
    let overlaid: serde_yaml::Value = serde_yaml::from_str(&overlaid_text).unwrap();
    assert_eq!(
        overlaid["providers"]["ashkelon-relay"]["api"],
        format!("{BASE}/openrouter/api/v1")
    );
    assert_eq!(overlaid["providers"]["ashkelon-relay"]["key_env"], "OPENROUTER_API_KEY");
    assert_eq!(overlaid["providers"]["ashkelon-relay"]["api_mode"], "chat_completions");

    let _ = std::fs::remove_dir_all(home.parent().unwrap());
}

#[test]
fn a_custom_provider_refuses_with_a_clear_message() {
    let (home, state_dir) = fixture("custom-refuse");
    std::fs::write(
        home.join("config.yaml"),
        "model:\n  provider: custom\n  base_url: https://my-llm.example\n",
    )
    .unwrap();

    let _guard = EnvGuard::set(&home, Some("sk-ant-test"), Some("sk-or-test"));
    let err = launch::plan("hermes", BASE, "hermes-custom-refuse", &[], &options(&state_dir)).unwrap_err();
    assert!(err.to_string().contains("custom"));

    let _ = std::fs::remove_dir_all(home.parent().unwrap());
}

#[test]
fn preexisting_providers_entry_is_preserved_alongside_the_new_one() {
    let (home, state_dir) = fixture("preserve-providers");
    std::fs::write(
        home.join("config.yaml"),
        "model:\n  provider: anthropic\nproviders:\n  my-other:\n    api: https://example.test\n    key_env: OTHER_KEY\n",
    )
    .unwrap();

    let _guard = EnvGuard::set(&home, Some("sk-ant-test"), None);
    let plan = launch::plan("hermes", BASE, "hermes-preserve", &[], &options(&state_dir)).unwrap();
    let overlay = plan.overlay_home.as_ref().unwrap();
    let overlaid_text = std::fs::read_to_string(overlay.overlay_dir.join("config.yaml")).unwrap();
    let overlaid: serde_yaml::Value = serde_yaml::from_str(&overlaid_text).unwrap();
    assert_eq!(overlaid["providers"]["my-other"]["api"], "https://example.test");
    assert_eq!(overlaid["providers"]["ashkelon-relay"]["key_env"], "ANTHROPIC_API_KEY");

    let _ = std::fs::remove_dir_all(home.parent().unwrap());
}

#[test]
fn reconcile_moves_back_a_file_hermes_materialized_fresh_in_the_overlay() {
    let (home, state_dir) = fixture("reconcile");
    std::fs::write(home.join("config.yaml"), "model:\n  provider: anthropic\n").unwrap();
    std::fs::create_dir_all(home.join("sessions")).unwrap();

    let overlay_dir = state_dir.join("overlay");
    launch::build_home_overlay(&home, &overlay_dir, "config.yaml").unwrap();
    std::fs::write(overlay_dir.join("config.yaml"), "generated: true\n").unwrap();

    // Simulate Hermes creating a brand-new top-level file no symlink anticipated.
    std::fs::write(overlay_dir.join("new-cache-file.json"), "{\"fresh\":true}").unwrap();

    let overlay = ashkelon::launch::OverlayHome {
        overlay_dir: overlay_dir.clone(),
        real_home: home.clone(),
        generated_file_name: "config.yaml".to_string(),
    };
    let moved = launch::reconcile_home_overlay(&overlay).unwrap();

    assert_eq!(moved, vec![home.join("new-cache-file.json")]);
    assert_eq!(
        std::fs::read_to_string(home.join("new-cache-file.json")).unwrap(),
        "{\"fresh\":true}"
    );
    // ashkelon's own generated config was never copied back over the real one.
    assert_eq!(
        std::fs::read_to_string(home.join("config.yaml")).unwrap(),
        "model:\n  provider: anthropic\n"
    );

    let _ = std::fs::remove_dir_all(home.parent().unwrap());
}
