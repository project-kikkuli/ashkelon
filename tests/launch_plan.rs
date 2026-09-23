use std::path::{Path, PathBuf};

use ashkelon::launch::{self, LaunchOptions, COMPANION_URL_PLACEHOLDER};

const BASE: &str = "http://127.0.0.1:9999/s/deadbeef";
const LAUNCH: &str = "deadbeef";

fn env_value<'a>(env: &'a [(String, String)], key: &str) -> Option<&'a str> {
    env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn state_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ashkelon-launch-plan-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn options(dir: &Path) -> LaunchOptions {
    LaunchOptions { state_dir: dir.to_path_buf(), no_channel: false }
}

fn options_no_channel(dir: &Path) -> LaunchOptions {
    LaunchOptions { state_dir: dir.to_path_buf(), no_channel: true }
}

#[test]
fn claude_routes_anthropic_base_url() {
    let dir = state_dir("claude-base");
    let plan = launch::plan("claude", BASE, LAUNCH, &[], &options(&dir)).unwrap();
    assert_eq!(plan.program, "claude");
    assert_eq!(env_value(&plan.env, "ANTHROPIC_BASE_URL"), Some(format!("{BASE}/anthropic").as_str()));
    assert_eq!(plan.wake.harness, "claude");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn claude_passes_through_args() {
    let dir = state_dir("claude-passthrough");
    let plan = launch::plan("claude", BASE, LAUNCH, &["--continue".to_string()], &options(&dir)).unwrap();
    assert!(plan.args.contains(&"--continue".to_string()));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn claude_wires_the_mcp_channel_by_default() {
    let dir = state_dir("claude-channel");
    let plan = launch::plan("claude", BASE, "chan1", &[], &options(&dir)).unwrap();

    let mcp_flag_index = plan.args.iter().position(|a| a == "--mcp-config").unwrap();
    let mcp_json = &plan.args[mcp_flag_index + 1];
    let config: serde_json::Value = serde_json::from_str(mcp_json).unwrap();
    let server = &config["mcpServers"]["ashkelon"];
    assert_eq!(server["args"], serde_json::json!(["channel", "--socket", dir.join("launch").join("chan1.sock").to_string_lossy()]));
    assert!(server["command"].as_str().unwrap().ends_with("ashkelon") || server["command"].as_str().unwrap().contains("ashkelon"));

    assert!(plan.args.contains(&"--dangerously-load-development-channels".to_string()));
    assert!(plan.args.contains(&"server:ashkelon".to_string()));

    assert_eq!(plan.wake.control.as_deref(), Some(dir.join("launch").join("chan1.sock").to_string_lossy().as_ref()));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn claude_no_channel_flag_skips_the_mcp_wiring() {
    let dir = state_dir("claude-no-channel-flag");
    let plan = launch::plan("claude", BASE, "chan2", &["--no-channel".to_string(), "--continue".to_string()], &options(&dir)).unwrap();
    assert!(!plan.args.iter().any(|a| a == "--mcp-config" || a == "--no-channel"));
    assert!(plan.wake.control.is_none());
    assert_eq!(plan.args, vec!["--continue".to_string()]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn claude_no_channel_option_skips_the_mcp_wiring() {
    let dir = state_dir("claude-no-channel-option");
    let plan = launch::plan("claude", BASE, "chan3", &[], &options_no_channel(&dir)).unwrap();
    assert!(!plan.args.iter().any(|a| a == "--mcp-config"));
    assert!(plan.wake.control.is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn codex_default_uses_chatgpt_login_provider() {
    let dir = state_dir("codex-default");
    let plan = launch::plan("codex", BASE, LAUNCH, &["extra-arg".to_string()], &options(&dir)).unwrap();
    assert_eq!(plan.program, "codex");
    let joined = plan.args.join(" ");
    assert!(joined.contains(&format!(r#"base_url="{BASE}/chatgpt/backend-api/codex""#)));
    assert!(joined.contains("requires_openai_auth=true"));
    assert!(joined.contains(r#"model_provider="ashkelon""#));
    assert!(plan.args.contains(&"extra-arg".to_string()));
    assert!(!joined.contains("--openai-api"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn codex_openai_api_flag_switches_base_and_strips_itself() {
    let dir = state_dir("codex-openai-api");
    let plan = launch::plan("codex", BASE, LAUNCH, &["--openai-api".to_string(), "extra-arg".to_string()], &options(&dir)).unwrap();
    let joined = plan.args.join(" ");
    assert!(joined.contains(&format!(r#"base_url="{BASE}/openai/v1""#)));
    assert!(joined.contains(r#"env_key="OPENAI_API_KEY""#));
    assert!(!joined.contains("requires_openai_auth"));
    assert!(!plan.args.contains(&"--openai-api".to_string()));
    assert!(plan.args.contains(&"extra-arg".to_string()));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn opencode_writes_temp_config_and_defers_control_to_companion() {
    let dir = state_dir("opencode-1");
    let plan = launch::plan("opencode", BASE, "opencode-test-1", &["-s".to_string(), "abc".to_string()], &options(&dir)).unwrap();
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
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn opencode_picks_its_own_port_instead_of_passing_zero() {
    let dir = state_dir("opencode-port");
    let plan = launch::plan("opencode", BASE, "opencode-test-port", &[], &options(&dir)).unwrap();
    let companion = plan.companion.as_ref().unwrap();
    let port_index = companion.args.iter().position(|a| a == "--port").unwrap();
    let port: u16 = companion.args[port_index + 1].parse().unwrap();
    assert_ne!(port, 0);
    for f in &plan.temp_files {
        let _ = std::fs::remove_file(f);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn opencode_generates_a_server_password_and_matching_wake_auth() {
    let dir = state_dir("opencode-password");
    let plan = launch::plan("opencode", BASE, "opencode-test-password", &[], &options(&dir)).unwrap();

    let companion = plan.companion.as_ref().unwrap();
    let companion_password = env_value(&companion.env, "OPENCODE_SERVER_PASSWORD").unwrap();
    let main_password = env_value(&plan.env, "OPENCODE_SERVER_PASSWORD").unwrap();
    assert_eq!(companion_password, main_password);
    assert!(!companion_password.is_empty());

    let auth = plan.wake.control_auth.as_deref().unwrap();
    let expected = format!("Basic {}", {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(format!("opencode:{companion_password}"))
    });
    assert_eq!(auth, expected);

    for f in &plan.temp_files {
        let _ = std::fs::remove_file(f);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn opencode_companion_url_resolves_into_args_and_wake_control() {
    let dir = state_dir("opencode-2");
    let mut plan = launch::plan("opencode", BASE, "opencode-test-2", &[], &options(&dir)).unwrap();
    std::fs::remove_file(&plan.temp_files[0]).unwrap();
    plan.resolve_companion_url("http://127.0.0.1:54321");
    assert_eq!(plan.args, vec!["attach".to_string(), "http://127.0.0.1:54321".to_string()]);
    assert_eq!(plan.wake.control.as_deref(), Some("http://127.0.0.1:54321"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn omp_routes_all_three_base_urls() {
    let dir = state_dir("omp-base");
    let plan = launch::plan("omp", BASE, LAUNCH, &[], &options(&dir)).unwrap();
    assert_eq!(env_value(&plan.env, "ANTHROPIC_BASE_URL"), Some(format!("{BASE}/anthropic").as_str()));
    assert_eq!(env_value(&plan.env, "OPENAI_BASE_URL"), Some(format!("{BASE}/openai").as_str()));
    assert_eq!(env_value(&plan.env, "OPENROUTER_BASE_URL"), Some(format!("{BASE}/openrouter").as_str()));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn omp_refuses_from_claude_import() {
    let dir = state_dir("omp-refuse");
    let err = launch::plan("omp", BASE, LAUNCH, &["--from-claude".to_string()], &options(&dir)).unwrap_err();
    assert!(err.to_string().contains("--from-claude"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ori_routes_openrouter_without_api_v1_suffix() {
    let dir = state_dir("ori-base");
    let plan = launch::plan("ori", BASE, LAUNCH, &[], &options(&dir)).unwrap();
    assert_eq!(env_value(&plan.env, "ORI_OPENROUTER_BASE_URL"), Some(format!("{BASE}/openrouter").as_str()));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cursor_is_explicitly_unsupported() {
    let dir = state_dir("cursor");
    let err = launch::plan("cursor", BASE, LAUNCH, &[], &options(&dir)).unwrap_err();
    assert!(err.to_string().to_lowercase().contains("cursor"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unknown_harness_lists_supported_ones() {
    let dir = state_dir("unknown");
    let err = launch::plan("nonexistent", BASE, LAUNCH, &[], &options(&dir)).unwrap_err();
    let message = err.to_string();
    assert!(message.contains("claude"));
    assert!(message.contains("codex"));
    assert!(message.contains("opencode"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn opencode_headless_run_attaches_with_flag() {
    let dir = state_dir("opencode-3");
    let plan = launch::plan("opencode", BASE, "opencode-test-3", &["run".to_string(), "hello".to_string()], &options(&dir)).unwrap();
    assert_eq!(plan.args, vec!["run", "--attach", COMPANION_URL_PLACEHOLDER, "hello"]);
    for f in &plan.temp_files {
        let _ = std::fs::remove_file(f);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
