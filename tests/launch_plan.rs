use ashkelon::launch::{self, COMPANION_URL_PLACEHOLDER};

const BASE: &str = "http://127.0.0.1:9999/s/deadbeef";
const LAUNCH: &str = "deadbeef";

fn env_value<'a>(env: &'a [(String, String)], key: &str) -> Option<&'a str> {
    env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

#[test]
fn claude_routes_anthropic_base_url() {
    let plan = launch::plan("claude", BASE, LAUNCH, &[]).unwrap();
    assert_eq!(plan.program, "claude");
    assert_eq!(env_value(&plan.env, "ANTHROPIC_BASE_URL"), Some(format!("{BASE}/anthropic").as_str()));
    assert!(plan.args.is_empty());
    assert_eq!(plan.wake.harness, "claude");
    assert!(plan.wake.control.is_none());
}

#[test]
fn claude_passes_through_args() {
    let plan = launch::plan("claude", BASE, LAUNCH, &["--continue".to_string()]).unwrap();
    assert_eq!(plan.args, vec!["--continue".to_string()]);
}

#[test]
fn codex_default_uses_chatgpt_login_provider() {
    let plan = launch::plan("codex", BASE, LAUNCH, &["extra-arg".to_string()]).unwrap();
    assert_eq!(plan.program, "codex");
    let joined = plan.args.join(" ");
    assert!(joined.contains(&format!(r#"base_url="{BASE}/chatgpt/backend-api/codex""#)));
    assert!(joined.contains("requires_openai_auth=true"));
    assert!(joined.contains(r#"model_provider="ashkelon""#));
    assert!(plan.args.contains(&"extra-arg".to_string()));
    assert!(!joined.contains("--openai-api"));
}

#[test]
fn codex_openai_api_flag_switches_base_and_strips_itself() {
    let plan = launch::plan("codex", BASE, LAUNCH, &["--openai-api".to_string(), "extra-arg".to_string()]).unwrap();
    let joined = plan.args.join(" ");
    assert!(joined.contains(&format!(r#"base_url="{BASE}/openai/v1""#)));
    assert!(joined.contains(r#"env_key="OPENAI_API_KEY""#));
    assert!(!joined.contains("requires_openai_auth"));
    assert!(!plan.args.contains(&"--openai-api".to_string()));
    assert!(plan.args.contains(&"extra-arg".to_string()));
}

#[test]
fn opencode_writes_temp_config_and_defers_control_to_companion() {
    let plan = launch::plan("opencode", BASE, "opencode-test-1", &["-s".to_string(), "abc".to_string()]).unwrap();
    assert_eq!(plan.program, "opencode");
    assert_eq!(plan.args[0], "attach");
    assert_eq!(plan.args[1], COMPANION_URL_PLACEHOLDER);
    assert_eq!(plan.args[2], "-s");
    assert_eq!(plan.args[3], "abc");
    assert_eq!(plan.wake.control.as_deref(), Some(COMPANION_URL_PLACEHOLDER));
    assert_eq!(env_value(&plan.env, "ANTHROPIC_BASE_URL"), Some(format!("{BASE}/anthropic").as_str()));

    assert_eq!(plan.temp_files.len(), 1);
    let config_text = std::fs::read_to_string(&plan.temp_files[0]).unwrap();
    let config: serde_json::Value = serde_json::from_str(&config_text).unwrap();
    assert_eq!(config["provider"]["anthropic"]["options"]["baseURL"], format!("{BASE}/anthropic/v1"));
    assert_eq!(config["provider"]["openai"]["options"]["baseURL"], format!("{BASE}/openai/v1"));
    assert_eq!(config["provider"]["openrouter"]["options"]["baseURL"], format!("{BASE}/openrouter/api/v1"));
    assert_eq!(config["provider"]["opencode"]["options"]["baseURL"], format!("{BASE}/opencode/zen/v1"));
    std::fs::remove_file(&plan.temp_files[0]).unwrap();

    let companion = plan.companion.as_ref().unwrap();
    assert_eq!(companion.program, "opencode");
    assert!(companion.args.contains(&"serve".to_string()));
    let caps = companion.ready_pattern.captures("opencode server listening on http://127.0.0.1:54321").unwrap();
    assert_eq!(&caps["host"], "127.0.0.1");
    assert_eq!(&caps["port"], "54321");
}

#[test]
fn opencode_companion_url_resolves_into_args_and_wake_control() {
    let mut plan = launch::plan("opencode", BASE, "opencode-test-2", &[]).unwrap();
    std::fs::remove_file(&plan.temp_files[0]).unwrap();
    plan.resolve_companion_url("http://127.0.0.1:54321");
    assert_eq!(plan.args, vec!["attach".to_string(), "http://127.0.0.1:54321".to_string()]);
    assert_eq!(plan.wake.control.as_deref(), Some("http://127.0.0.1:54321"));
}

#[test]
fn omp_routes_all_three_base_urls() {
    let plan = launch::plan("omp", BASE, LAUNCH, &[]).unwrap();
    assert_eq!(env_value(&plan.env, "ANTHROPIC_BASE_URL"), Some(format!("{BASE}/anthropic").as_str()));
    assert_eq!(env_value(&plan.env, "OPENAI_BASE_URL"), Some(format!("{BASE}/openai").as_str()));
    assert_eq!(env_value(&plan.env, "OPENROUTER_BASE_URL"), Some(format!("{BASE}/openrouter").as_str()));
}

#[test]
fn omp_refuses_from_claude_import() {
    let err = launch::plan("omp", BASE, LAUNCH, &["--from-claude".to_string()]).unwrap_err();
    assert!(err.to_string().contains("--from-claude"));
}

#[test]
fn hermes_always_refuses() {
    let err = launch::plan("hermes", BASE, LAUNCH, &[]).unwrap_err();
    assert!(err.to_string().to_lowercase().contains("keychain"));
}

#[test]
fn ori_routes_openrouter_without_api_v1_suffix() {
    let plan = launch::plan("ori", BASE, LAUNCH, &[]).unwrap();
    assert_eq!(env_value(&plan.env, "ORI_OPENROUTER_BASE_URL"), Some(format!("{BASE}/openrouter").as_str()));
}

#[test]
fn cursor_is_explicitly_unsupported() {
    let err = launch::plan("cursor", BASE, LAUNCH, &[]).unwrap_err();
    assert!(err.to_string().to_lowercase().contains("cursor"));
}

#[test]
fn unknown_harness_lists_supported_ones() {
    let err = launch::plan("nonexistent", BASE, LAUNCH, &[]).unwrap_err();
    let message = err.to_string();
    assert!(message.contains("claude"));
    assert!(message.contains("codex"));
    assert!(message.contains("opencode"));
}

#[test]
fn opencode_headless_run_attaches_with_flag() {
    let plan = launch::plan("opencode", BASE, "opencode-test-2", &["run".to_string(), "hello".to_string()]).unwrap();
    assert_eq!(plan.args, vec!["run", "--attach", COMPANION_URL_PLACEHOLDER, "hello"]);
    for f in &plan.temp_files {
        let _ = std::fs::remove_file(f);
    }
}
