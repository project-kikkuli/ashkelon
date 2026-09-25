use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ashkelon::config::{Config, HookConfig, HookEvent};
use ashkelon::hooks::{Engine, PinInjector, WakeFn};
use ashkelon::session::SessionKey;
use ashkelon::transform::PinnedPing;
use ashkelon::usage::Summary;
use ashkelon::wake::WakeTarget;
use ashkelon::wire::Wire;

fn key(session: &str) -> SessionKey {
    SessionKey {
        launch: None,
        harness: Some("test".into()),
        session: session.into(),
    }
}

fn hook(name: &str, on: Vec<HookEvent>, command: &Path) -> HookConfig {
    HookConfig {
        name: name.into(),
        on,
        command: vec![command.to_str().unwrap().to_string()],
        projects: Vec::new(),
        harnesses: Vec::new(),
        timeout_secs: 5,
    }
}

fn write_script(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

fn sandboxed_config(dir: &Path) -> Config {
    Config {
        state_dir: Some(dir.join("state")),
        log_dir: Some(dir.join("logs")),
        ..Config::default()
    }
}

fn noop_waker() -> WakeFn {
    Arc::new(|_target, _session, _text| Box::pin(async { Ok(false) }))
}

async fn wait_for(mut cond: impl FnMut() -> bool) {
    for _ in 0..400 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("condition never became true within the test timeout");
}

fn log_text(cfg: &Config) -> String {
    let path = cfg.log_dir().join(format!("hooks-{}.jsonl", today()));
    std::fs::read_to_string(path).unwrap_or_default()
}

fn today() -> String {
    let now = time::OffsetDateTime::now_utc();
    format!("{:04}-{:02}-{:02}", now.year(), u8::from(now.month()), now.day())
}

fn count_lines(log: &str, hook_name: &str, status: &str) -> usize {
    log.lines()
        .filter(|l| l.contains(&format!("\"hook\":\"{hook_name}\"")) && l.contains(&format!("\"status\":\"{status}\"")))
        .count()
}

/// A `PinInjector` fake: `conversation_len` is fixed, and `inject` succeeds only while `pins`
/// stays at or under `max_ok_pins`, encoding the pin count into the returned body so tests can
/// tell which set of pins the engine actually passed through.
struct ThresholdInjector {
    len: usize,
    max_ok_pins: usize,
}

impl PinInjector for ThresholdInjector {
    fn conversation_len(&self, _wire: Wire, _body: &[u8]) -> Option<usize> {
        Some(self.len)
    }

    fn inject(&self, _wire: Wire, body: &[u8], pins: &[PinnedPing]) -> Option<Vec<u8>> {
        if pins.len() > self.max_ok_pins {
            return None;
        }
        let mut v = body.to_vec();
        v.extend(format!(":{}", pins.len()).into_bytes());
        Some(v)
    }
}

/// Fails the first `fail_calls` invocations of `inject`, then always succeeds. Used to prove a
/// total attach_pings failure doesn't lose the pending pings.
struct FlakyInjector {
    len: usize,
    calls: AtomicUsize,
    fail_calls: usize,
}

impl PinInjector for FlakyInjector {
    fn conversation_len(&self, _wire: Wire, _body: &[u8]) -> Option<usize> {
        Some(self.len)
    }

    fn inject(&self, _wire: Wire, body: &[u8], pins: &[PinnedPing]) -> Option<Vec<u8>> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if n < self.fail_calls {
            return None;
        }
        let mut v = body.to_vec();
        v.extend(format!(":{}", pins.len()).into_bytes());
        Some(v)
    }
}

const ALWAYS_FAIL: &str = "#!/bin/sh\ncat >/dev/null\necho '{\"status\":\"fail\",\"message\":\"boom\"}'\n";

fn always_fail_with(message: &str) -> String {
    format!("#!/bin/sh\ncat >/dev/null\necho '{{\"status\":\"fail\",\"message\":\"{message}\"}}'\n")
}

// --- event derivation, as observed through real hook runs -----------------------------------

#[tokio::test]
async fn session_start_fires_exactly_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = sandboxed_config(dir.path());
    let script = write_script(dir.path(), "onstart.sh", ALWAYS_FAIL);
    c.hooks = vec![hook("onstart", vec![HookEvent::SessionStart], &script)];
    let cfg = Arc::new(c);
    let engine = Engine::new(cfg.clone());
    let k = key("s1");

    engine.observe_request(&k, Wire::Opaque, b"{}");
    wait_for(|| count_lines(&log_text(&cfg), "onstart", "fail") >= 1).await;

    engine.observe_request(&k, Wire::Opaque, b"{}");
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        count_lines(&log_text(&cfg), "onstart", "fail"),
        1,
        "session_start must not refire"
    );
}

#[tokio::test]
async fn identical_repeated_prompt_does_not_refire() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = sandboxed_config(dir.path());
    let script = write_script(dir.path(), "onprompt.sh", ALWAYS_FAIL);
    c.hooks = vec![hook("onprompt", vec![HookEvent::Prompt], &script)];
    let cfg = Arc::new(c);
    let engine = Engine::new(cfg.clone());
    let k = key("s1");
    let body = br#"{"messages":[{"role":"user","content":"same text"}]}"#;

    engine.observe_request(&k, Wire::AnthropicMessages, body);
    wait_for(|| count_lines(&log_text(&cfg), "onprompt", "fail") >= 1).await;

    engine.observe_request(&k, Wire::AnthropicMessages, body);
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        count_lines(&log_text(&cfg), "onprompt", "fail"),
        1,
        "an unchanged prompt must not refire"
    );
}

#[tokio::test]
async fn compaction_fires_and_clears_previously_delivered_pins() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = sandboxed_config(dir.path());
    let onprompt = write_script(dir.path(), "onprompt.sh", &always_fail_with("boom"));
    let oncompaction = write_script(dir.path(), "oncompaction.sh", &always_fail_with("compacted"));
    c.hooks = vec![
        hook("onprompt", vec![HookEvent::Prompt], &onprompt),
        hook("oncompaction", vec![HookEvent::Compaction], &oncompaction),
    ];
    let cfg = Arc::new(c);
    let engine = Engine::new_with_injector(
        cfg.clone(),
        noop_waker(),
        Box::new(ThresholdInjector {
            len: 3,
            max_ok_pins: 99,
        }),
    );
    let k = key("s1");

    let long_body = br#"{"messages":[{"role":"user","content":"a"},{"role":"assistant","content":"b"},{"role":"user","content":"first"}]}"#;
    engine.observe_request(&k, Wire::AnthropicMessages, long_body);
    wait_for(|| count_lines(&log_text(&cfg), "onprompt", "fail") >= 1).await;

    // Deliver (pin) the pending ping so it moves into `delivered`.
    let (_, ids) = engine.attach_pings(&k, Wire::AnthropicMessages, long_body).unwrap();
    assert_eq!(ids.len(), 1);

    // A shorter conversation, same last prompt text (so `onprompt` does not refire): this must
    // read as compaction only.
    let short_body = br#"{"messages":[{"role":"user","content":"first"}]}"#;
    engine.observe_request(&k, Wire::AnthropicMessages, short_body);
    wait_for(|| count_lines(&log_text(&cfg), "oncompaction", "fail") >= 1).await;
    assert_eq!(
        count_lines(&log_text(&cfg), "onprompt", "fail"),
        1,
        "same prompt text must not have refired"
    );

    // The only pin `attach_pings` should have left to combine with the new one is the
    // compaction hook's own pending ping: if the old delivered pin had NOT been cleared, this
    // would carry 2 pins instead of 1.
    let (body, ids) = engine.attach_pings(&k, Wire::AnthropicMessages, short_body).unwrap();
    assert_eq!(ids.len(), 1);
    assert!(
        body.ends_with(b":1"),
        "stale delivered pin should have been dropped by compaction"
    );
}

// --- hook execution: timeouts, bad output, pass/fail, coalescing -----------------------------

#[tokio::test]
async fn timeout_kills_the_hook_and_never_becomes_a_ping() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = sandboxed_config(dir.path());
    let script = write_script(
        dir.path(),
        "hang.sh",
        "#!/bin/sh\ncat >/dev/null\nsleep 5\necho '{\"status\":\"pass\"}'\n",
    );
    let mut h = hook("hang", vec![HookEvent::Prompt], &script);
    h.timeout_secs = 1;
    c.hooks = vec![h];
    let cfg = Arc::new(c);
    let engine = Engine::new(cfg.clone());
    let k = key("s1");

    engine.observe_request(
        &k,
        Wire::AnthropicMessages,
        br#"{"messages":[{"role":"user","content":"hi"}]}"#,
    );
    wait_for(|| count_lines(&log_text(&cfg), "hang", "timeout") >= 1).await;

    assert!(
        engine.attach_pings(&k, Wire::AnthropicMessages, b"{}").is_none(),
        "a timeout must never become a ping"
    );
}

#[tokio::test]
async fn non_json_stdout_is_logged_as_error_without_a_ping() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = sandboxed_config(dir.path());
    let script = write_script(
        dir.path(),
        "garbage.sh",
        "#!/bin/sh\ncat >/dev/null\necho 'not json at all'\n",
    );
    c.hooks = vec![hook("garbage", vec![HookEvent::Prompt], &script)];
    let cfg = Arc::new(c);
    let engine = Engine::new(cfg.clone());
    let k = key("s1");

    engine.observe_request(
        &k,
        Wire::AnthropicMessages,
        br#"{"messages":[{"role":"user","content":"hi"}]}"#,
    );
    wait_for(|| count_lines(&log_text(&cfg), "garbage", "error") >= 1).await;

    assert!(
        engine.attach_pings(&k, Wire::AnthropicMessages, b"{}").is_none(),
        "malformed output must never become a ping"
    );
}

#[tokio::test]
async fn a_later_pass_resolves_a_still_pending_failure() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = sandboxed_config(dir.path());
    let flag = dir.path().join("ready");
    let script = write_script(
        dir.path(),
        "flip.sh",
        &format!(
            "#!/bin/sh\ncat >/dev/null\nif [ -f {flag} ]; then\n  echo '{{\"status\":\"pass\"}}'\nelse\n  echo '{{\"status\":\"fail\",\"message\":\"not ready\"}}'\nfi\n",
            flag = flag.display()
        ),
    );
    c.hooks = vec![hook("flip", vec![HookEvent::Prompt], &script)];
    let cfg = Arc::new(c);
    let engine = Engine::new(cfg.clone());
    let k = key("s1");

    engine.observe_request(
        &k,
        Wire::AnthropicMessages,
        br#"{"messages":[{"role":"user","content":"attempt one"}]}"#,
    );
    wait_for(|| count_lines(&log_text(&cfg), "flip", "fail") >= 1).await;

    std::fs::write(&flag, b"").unwrap();
    engine.observe_request(
        &k,
        Wire::AnthropicMessages,
        br#"{"messages":[{"role":"user","content":"attempt two"}]}"#,
    );
    wait_for(|| count_lines(&log_text(&cfg), "flip", "pass") >= 1).await;

    assert!(
        engine.attach_pings(&k, Wire::AnthropicMessages, b"{}").is_none(),
        "the resolved failure must not have anything left to deliver"
    );
}

#[tokio::test]
async fn identical_failure_is_deduplicated_not_queued_twice() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = sandboxed_config(dir.path());
    let script = write_script(dir.path(), "lint.sh", &always_fail_with("same message"));
    c.hooks = vec![hook("lint", vec![HookEvent::Prompt], &script)];
    let cfg = Arc::new(c);
    let engine = Engine::new_with_injector(
        cfg.clone(),
        noop_waker(),
        Box::new(ThresholdInjector {
            len: 1,
            max_ok_pins: 99,
        }),
    );
    let k = key("s1");

    engine.observe_request(
        &k,
        Wire::AnthropicMessages,
        br#"{"messages":[{"role":"user","content":"one"}]}"#,
    );
    wait_for(|| count_lines(&log_text(&cfg), "lint", "fail") >= 1).await;
    engine.observe_request(
        &k,
        Wire::AnthropicMessages,
        br#"{"messages":[{"role":"user","content":"two"}]}"#,
    );
    wait_for(|| count_lines(&log_text(&cfg), "lint", "fail") >= 2).await;

    let (_, ids) = engine.attach_pings(&k, Wire::AnthropicMessages, b"{}").unwrap();
    assert_eq!(ids.len(), 1, "two identical failures must collapse into a single ping");
    // The pin is now delivered, and stays re-inserted on every later request; nothing NEW is
    // left to deliver, but the call itself keeps succeeding.
    let (_, ids2) = engine.attach_pings(&k, Wire::AnthropicMessages, b"{}").unwrap();
    assert!(ids2.is_empty());
}

#[tokio::test]
async fn max_per_session_caps_total_deliveries() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = sandboxed_config(dir.path());
    c.pings.max_per_session = 1;
    let script_a = write_script(dir.path(), "a.sh", &always_fail_with("one"));
    let script_b = write_script(dir.path(), "b.sh", &always_fail_with("two"));
    c.hooks = vec![
        hook("a", vec![HookEvent::Prompt], &script_a),
        hook("b", vec![HookEvent::Prompt], &script_b),
    ];
    let cfg = Arc::new(c);
    let engine = Engine::new_with_injector(
        cfg.clone(),
        noop_waker(),
        Box::new(ThresholdInjector {
            len: 1,
            max_ok_pins: 99,
        }),
    );
    let k = key("s1");

    engine.observe_request(
        &k,
        Wire::AnthropicMessages,
        br#"{"messages":[{"role":"user","content":"one shot"}]}"#,
    );
    wait_for(|| count_lines(&log_text(&cfg), "a", "fail") >= 1 && count_lines(&log_text(&cfg), "b", "fail") >= 1).await;

    let (_, ids) = engine.attach_pings(&k, Wire::AnthropicMessages, b"{}").unwrap();
    assert_eq!(ids.len(), 1, "the cap must admit exactly one of the two failures");
    let (_, ids2) = engine.attach_pings(&k, Wire::AnthropicMessages, b"{}").unwrap();
    assert!(ids2.is_empty(), "nothing new left to deliver, the cap holds");
}

#[tokio::test]
async fn overlapping_events_coalesce_into_a_single_rerun() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = sandboxed_config(dir.path());
    let script = write_script(
        dir.path(),
        "slow.sh",
        "#!/bin/sh\ncat >/dev/null\nsleep 0.3\necho '{\"status\":\"pass\"}'\n",
    );
    c.hooks = vec![hook("slow", vec![HookEvent::Prompt], &script)];
    let cfg = Arc::new(c);
    let engine = Engine::new(cfg.clone());
    let k = key("s1");

    engine.observe_request(
        &k,
        Wire::AnthropicMessages,
        br#"{"messages":[{"role":"user","content":"one"}]}"#,
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    engine.observe_request(
        &k,
        Wire::AnthropicMessages,
        br#"{"messages":[{"role":"user","content":"one"},{"role":"assistant","content":"a"},{"role":"user","content":"two"}]}"#,
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    engine.observe_request(
        &k,
        Wire::AnthropicMessages,
        br#"{"messages":[{"role":"user","content":"one"},{"role":"assistant","content":"a"},{"role":"user","content":"two"},{"role":"assistant","content":"b"},{"role":"user","content":"three"}]}"#,
    );

    wait_for(|| count_lines(&log_text(&cfg), "slow", "pass") >= 2).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        count_lines(&log_text(&cfg), "slow", "pass"),
        2,
        "three overlapping triggers must coalesce to one run plus a single rerun, never three"
    );
}

#[tokio::test]
async fn hook_receives_the_documented_stdin_and_env() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = sandboxed_config(dir.path());
    let captured = dir.path().join("captured.json");
    let env_captured = dir.path().join("env.txt");
    let script = write_script(
        dir.path(),
        "capture.sh",
        &format!(
            "#!/bin/sh\ncat > {stdin}\nprintf '%s\\n%s\\n%s\\n' \"$ASHKELON_EVENT\" \"$ASHKELON_SESSION_DIR\" \"$ASHKELON_BIN\" > {env}\necho '{{\"status\":\"pass\"}}'\n",
            stdin = captured.display(),
            env = env_captured.display(),
        ),
    );
    c.hooks = vec![hook("capture", vec![HookEvent::Prompt], &script)];
    let cfg = Arc::new(c);
    let engine = Engine::new(cfg.clone());
    let k = key("s1");

    engine.observe_request(
        &k,
        Wire::AnthropicMessages,
        br#"{"messages":[{"role":"user","content":"hello world"}]}"#,
    );
    wait_for(|| captured.exists() && env_captured.exists()).await;
    wait_for(|| count_lines(&log_text(&cfg), "capture", "pass") >= 1).await;

    let payload: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&captured).unwrap()).unwrap();
    assert_eq!(payload["event"], "prompt");
    assert_eq!(payload["hook"], "capture");
    assert_eq!(payload["harness"], "test");
    assert_eq!(payload["session"], "s1");
    assert_eq!(payload["prompt"], "hello world");
    assert!(payload["state_dir"].as_str().unwrap().contains("sessions"));
    assert!(payload["request_path"].as_str().unwrap().ends_with("request.json"));
    assert!(payload["response_path"].as_str().unwrap().ends_with("response.txt"));
    assert!(payload["ts"].as_str().is_some());

    let env_text = std::fs::read_to_string(&env_captured).unwrap();
    let mut lines = env_text.lines();
    assert_eq!(lines.next(), Some("prompt"));
    assert!(lines.next().unwrap().contains("sessions"));
    assert!(
        !lines.next().unwrap().is_empty(),
        "ASHKELON_BIN should be set to the current executable"
    );
}

#[tokio::test]
async fn turn_end_carries_the_prompt_that_started_the_turn() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = sandboxed_config(dir.path());
    let captured = dir.path().join("turn.json");
    let script = write_script(
        dir.path(),
        "turn.sh",
        &format!(
            "#!/bin/sh\ncat > {}\necho '{{\"status\":\"pass\"}}'\n",
            captured.display()
        ),
    );
    c.hooks = vec![hook("turn", vec![HookEvent::TurnEnd], &script)];
    let cfg = Arc::new(c);
    let engine = Engine::new(cfg.clone());
    let k = key("turn-session");
    engine.observe_request(
        &k,
        Wire::AnthropicMessages,
        br#"{"messages":[{"role":"user","content":"the actual task"}]}"#,
    );
    engine.observe_response(
        &k,
        Wire::AnthropicMessages,
        &Summary {
            turn_end: true,
            text: "the answer".into(),
            ..Summary::default()
        },
    );
    wait_for(|| count_lines(&log_text(&cfg), "turn", "pass") >= 1).await;
    let payload: serde_json::Value = serde_json::from_slice(&std::fs::read(captured).unwrap()).unwrap();
    assert_eq!(payload["prompt"], "the actual task");
    assert_eq!(payload["text"], "the answer");
}

// --- persisted per-session context -----------------------------------------------------------

#[tokio::test]
async fn request_and_response_bodies_are_persisted_per_session() {
    let dir = tempfile::tempdir().unwrap();
    let c = sandboxed_config(dir.path());
    let cfg = Arc::new(c);
    let engine = Engine::new(cfg.clone());
    let k = key("s1");

    let body = br#"{"messages":[{"role":"user","content":"hello"}]}"#;
    engine.observe_request(&k, Wire::AnthropicMessages, body);

    let sessions_dir = cfg.state_dir().join("sessions");
    wait_for(|| {
        sessions_dir.is_dir()
            && std::fs::read_dir(&sessions_dir)
                .map(|mut d| d.next().is_some())
                .unwrap_or(false)
    })
    .await;
    let session_dir = std::fs::read_dir(&sessions_dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();

    wait_for(|| session_dir.join("request.json").exists()).await;
    let saved = std::fs::read(session_dir.join("request.json")).unwrap();
    assert_eq!(saved, body);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let dir_mode = std::fs::metadata(&session_dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700);
        let file_mode = std::fs::metadata(session_dir.join("request.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(file_mode, 0o600);
    }
}

// --- pin bookkeeping (attach_pings), via an injectable PinInjector ---------------------------

#[tokio::test]
async fn attach_pings_is_none_when_nothing_is_queued() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = Arc::new(sandboxed_config(dir.path()));
    let engine = Engine::new_with_injector(
        cfg,
        noop_waker(),
        Box::new(ThresholdInjector {
            len: 1,
            max_ok_pins: 99,
        }),
    );
    let k = key("s1");
    engine.observe_request(&k, Wire::Opaque, b"{}");
    assert!(engine.attach_pings(&k, Wire::AnthropicMessages, b"{}").is_none());
}

#[tokio::test]
async fn attach_pings_delivers_a_pending_failure() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = sandboxed_config(dir.path());
    let script = write_script(dir.path(), "lint.sh", &always_fail_with("bad"));
    c.hooks = vec![hook("lint", vec![HookEvent::Prompt], &script)];
    let cfg = Arc::new(c);
    let engine = Engine::new_with_injector(
        cfg.clone(),
        noop_waker(),
        Box::new(ThresholdInjector {
            len: 2,
            max_ok_pins: 99,
        }),
    );
    let k = key("s1");

    engine.observe_request(
        &k,
        Wire::AnthropicMessages,
        br#"{"messages":[{"role":"user","content":"hi"}]}"#,
    );
    wait_for(|| count_lines(&log_text(&cfg), "lint", "fail") >= 1).await;

    let (body, ids) = engine.attach_pings(&k, Wire::AnthropicMessages, b"{}").unwrap();
    assert_eq!(ids.len(), 1);
    assert!(body.ends_with(b":1"));
    // Already delivered: still re-inserted (per the pinning contract), but nothing NEW.
    let (body2, ids2) = engine.attach_pings(&k, Wire::AnthropicMessages, b"{}").unwrap();
    assert!(ids2.is_empty());
    assert!(body2.ends_with(b":1"));
}

#[tokio::test]
async fn signal_is_delivered_once_without_pinning() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = sandboxed_config(dir.path());
    let script = write_script(
        dir.path(),
        "signal.sh",
        "#!/bin/sh\ncat >/dev/null\necho '{\"status\":\"signal\",\"message\":\"feedback\"}'\n",
    );
    c.hooks = vec![hook("feedback", vec![HookEvent::Prompt], &script)];
    let cfg = Arc::new(c);
    let engine = Engine::new_with_injector(
        cfg.clone(),
        noop_waker(),
        Box::new(ThresholdInjector {
            len: 2,
            max_ok_pins: 99,
        }),
    );
    let k = key("signal-session");
    engine.observe_request(
        &k,
        Wire::AnthropicMessages,
        br#"{"messages":[{"role":"user","content":"hi"}]}"#,
    );
    wait_for(|| count_lines(&log_text(&cfg), "feedback", "signal") >= 1).await;
    let (_, ids) = engine.attach_pings(&k, Wire::AnthropicMessages, b"{}").unwrap();
    assert_eq!(ids.len(), 1);
    assert!(engine.attach_pings(&k, Wire::AnthropicMessages, b"{}").is_none());
}

#[tokio::test]
async fn media_only_signal_is_queued_for_the_next_request() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = sandboxed_config(dir.path());
    let script = write_script(
        dir.path(),
        "media-signal.sh",
        "#!/bin/sh\ncat >/dev/null\necho '{\"status\":\"signal\",\"attachments\":[{\"mime_type\":\"image/png\",\"data_base64\":\"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==\",\"alt_text\":\"tiny image\"}]}'\n",
    );
    c.hooks = vec![hook("media", vec![HookEvent::Prompt], &script)];
    let cfg = Arc::new(c);
    let engine = Engine::new_with_injector(
        cfg.clone(),
        noop_waker(),
        Box::new(ThresholdInjector {
            len: 2,
            max_ok_pins: 99,
        }),
    );
    let k = key("media-only-session");
    engine.observe_request(
        &k,
        Wire::AnthropicMessages,
        br#"{"messages":[{"role":"user","content":"hi"}]}"#,
    );
    wait_for(|| count_lines(&log_text(&cfg), "media", "signal") >= 1).await;
    let (_, ids) = engine.attach_pings(&k, Wire::AnthropicMessages, b"{}").unwrap();
    assert_eq!(ids.len(), 1, "an empty-text image signal must not be discarded");
}

#[tokio::test]
async fn attach_pings_reinserts_previously_delivered_pins_alongside_new_ones() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = sandboxed_config(dir.path());
    let first = write_script(dir.path(), "first.sh", &always_fail_with("first failure"));
    let second = write_script(dir.path(), "second.sh", &always_fail_with("second failure"));
    c.hooks = vec![
        hook("first", vec![HookEvent::Prompt], &first),
        hook("second", vec![HookEvent::ToolResult], &second),
    ];
    let cfg = Arc::new(c);
    let engine = Engine::new_with_injector(
        cfg.clone(),
        noop_waker(),
        Box::new(ThresholdInjector {
            len: 2,
            max_ok_pins: 99,
        }),
    );
    let k = key("s1");

    engine.observe_request(
        &k,
        Wire::AnthropicMessages,
        br#"{"messages":[{"role":"user","content":"hi"}]}"#,
    );
    wait_for(|| count_lines(&log_text(&cfg), "first", "fail") >= 1).await;
    let (_, ids1) = engine.attach_pings(&k, Wire::AnthropicMessages, b"{}").unwrap();
    assert_eq!(ids1.len(), 1);

    let tool_result_body =
        br#"{"messages":[{"role":"user","content":[{"type":"tool_result","tool_use_id":"1","content":"ok"}]}]}"#;
    engine.observe_request(&k, Wire::AnthropicMessages, tool_result_body);
    wait_for(|| count_lines(&log_text(&cfg), "second", "fail") >= 1).await;

    let (body, ids2) = engine.attach_pings(&k, Wire::AnthropicMessages, b"{}").unwrap();
    assert_eq!(ids2.len(), 1, "only the newly delivered id is reported");
    assert_ne!(ids1[0], ids2[0]);
    assert!(
        body.ends_with(b":2"),
        "both the old and new pin must have been passed to inject"
    );
}

#[tokio::test]
async fn attach_pings_drops_stale_delivered_pins_and_retries_with_new_only() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = sandboxed_config(dir.path());
    let first = write_script(dir.path(), "first.sh", &always_fail_with("first failure"));
    let second = write_script(dir.path(), "second.sh", &always_fail_with("second failure"));
    c.hooks = vec![
        hook("first", vec![HookEvent::Prompt], &first),
        hook("second", vec![HookEvent::ToolResult], &second),
    ];
    let cfg = Arc::new(c);
    // Succeeds for a single pin (the ordinary deliver, and the retry-with-new-only), fails once
    // a second pin is combined in (the stale "all pins" attempt).
    let engine = Engine::new_with_injector(
        cfg.clone(),
        noop_waker(),
        Box::new(ThresholdInjector { len: 2, max_ok_pins: 1 }),
    );
    let k = key("s1");

    engine.observe_request(
        &k,
        Wire::AnthropicMessages,
        br#"{"messages":[{"role":"user","content":"hi"}]}"#,
    );
    wait_for(|| count_lines(&log_text(&cfg), "first", "fail") >= 1).await;
    let (_, ids1) = engine.attach_pings(&k, Wire::AnthropicMessages, b"{}").unwrap();
    assert_eq!(ids1.len(), 1);

    let tool_result_body =
        br#"{"messages":[{"role":"user","content":[{"type":"tool_result","tool_use_id":"1","content":"ok"}]}]}"#;
    engine.observe_request(&k, Wire::AnthropicMessages, tool_result_body);
    wait_for(|| count_lines(&log_text(&cfg), "second", "fail") >= 1).await;

    let (body, ids2) = engine.attach_pings(&k, Wire::AnthropicMessages, b"{}").unwrap();
    assert_eq!(ids2.len(), 1);
    assert_ne!(
        ids1[0], ids2[0],
        "the stale pin was dropped, the new one is what got delivered"
    );
    assert!(body.ends_with(b":1"), "the retry carried only the new pin");
}

#[tokio::test]
async fn attach_pings_restores_the_outbox_on_total_failure() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = sandboxed_config(dir.path());
    let script = write_script(dir.path(), "lint.sh", &always_fail_with("bad"));
    c.hooks = vec![hook("lint", vec![HookEvent::Prompt], &script)];
    let cfg = Arc::new(c);
    // Fails the first two calls (the "all pins" attempt and the "new only" retry inside one
    // attach_pings call), then succeeds forever after.
    let engine = Engine::new_with_injector(
        cfg.clone(),
        noop_waker(),
        Box::new(FlakyInjector {
            len: 1,
            calls: AtomicUsize::new(0),
            fail_calls: 2,
        }),
    );
    let k = key("s1");

    engine.observe_request(
        &k,
        Wire::AnthropicMessages,
        br#"{"messages":[{"role":"user","content":"hi"}]}"#,
    );
    wait_for(|| count_lines(&log_text(&cfg), "lint", "fail") >= 1).await;

    assert!(
        engine.attach_pings(&k, Wire::AnthropicMessages, b"{}").is_none(),
        "both attempts were made to fail"
    );
    let (_, ids) = engine
        .attach_pings(&k, Wire::AnthropicMessages, b"{}")
        .expect("the pending ping must not have been lost by the earlier failure");
    assert_eq!(ids.len(), 1);
}

// --- idle wake --------------------------------------------------------------------------------

#[tokio::test]
async fn idle_sweep_wakes_and_marks_delivered_without_pinning() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = sandboxed_config(dir.path());
    c.pings.wake_idle = true;
    c.pings.idle_after_secs = 0;
    let script = write_script(dir.path(), "lint.sh", &always_fail_with("bad"));
    c.hooks = vec![hook("lint", vec![HookEvent::Prompt], &script)];
    let cfg = Arc::new(c);

    let woken: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let woken2 = woken.clone();
    let waker: WakeFn = Arc::new(move |_target, _session, text| {
        let woken2 = woken2.clone();
        Box::pin(async move {
            woken2.lock().unwrap().push(text);
            Ok(true)
        })
    });
    let engine = Engine::new_with_injector(
        cfg.clone(),
        waker,
        Box::new(ThresholdInjector {
            len: 1,
            max_ok_pins: 99,
        }),
    );
    let k = key("s1");
    engine.register_launch("l1", WakeTarget::default(), dir.path().to_path_buf());
    let k = SessionKey {
        launch: Some("l1".into()),
        ..k
    };

    engine.observe_request(
        &k,
        Wire::AnthropicMessages,
        br#"{"messages":[{"role":"user","content":"hi"}]}"#,
    );
    wait_for(|| count_lines(&log_text(&cfg), "lint", "fail") >= 1).await;

    engine.idle_sweep().await;
    assert_eq!(woken.lock().unwrap().len(), 1);
    assert!(woken.lock().unwrap()[0].contains("bad"));
    assert!(
        engine.attach_pings(&k, Wire::AnthropicMessages, b"{}").is_none(),
        "a woken ping must not still be pending or become a pinned delivery"
    );
}

#[tokio::test]
async fn idle_sweep_does_not_rewake_an_identical_failure_already_delivered() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = sandboxed_config(dir.path());
    c.pings.wake_idle = true;
    c.pings.idle_after_secs = 0;
    let script = write_script(dir.path(), "lint.sh", &always_fail_with("bad"));
    c.hooks = vec![hook("lint", vec![HookEvent::Prompt], &script)];
    let cfg = Arc::new(c);

    let woken: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let woken2 = woken.clone();
    let waker: WakeFn = Arc::new(move |_target, _session, text| {
        let woken2 = woken2.clone();
        Box::pin(async move {
            woken2.lock().unwrap().push(text);
            Ok(true)
        })
    });
    let engine = Engine::new_with_injector(
        cfg.clone(),
        waker,
        Box::new(ThresholdInjector {
            len: 1,
            max_ok_pins: 99,
        }),
    );
    let k = key("s1");
    engine.register_launch("l1", WakeTarget::default(), dir.path().to_path_buf());
    let k = SessionKey {
        launch: Some("l1".into()),
        ..k
    };

    engine.observe_request(
        &k,
        Wire::AnthropicMessages,
        br#"{"messages":[{"role":"user","content":"first"}]}"#,
    );
    wait_for(|| count_lines(&log_text(&cfg), "lint", "fail") >= 1).await;
    engine.idle_sweep().await;
    assert_eq!(woken.lock().unwrap().len(), 1, "first failure must wake once");

    // A second, distinct prompt refires the same always-failing hook; it produces the identical
    // message (hence the identical ping id) as the one already woken above.
    engine.observe_request(
        &k,
        Wire::AnthropicMessages,
        br#"{"messages":[{"role":"user","content":"second"}]}"#,
    );
    wait_for(|| count_lines(&log_text(&cfg), "lint", "fail") >= 2).await;
    engine.idle_sweep().await;
    assert_eq!(
        woken.lock().unwrap().len(),
        1,
        "an identical failure already delivered by a wake must not be pinged again"
    );
}

#[tokio::test]
async fn idle_sweep_skips_sessions_that_are_still_recent() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = sandboxed_config(dir.path());
    c.pings.wake_idle = true;
    c.pings.idle_after_secs = 3600;
    let script = write_script(dir.path(), "lint.sh", &always_fail_with("bad"));
    c.hooks = vec![hook("lint", vec![HookEvent::Prompt], &script)];
    let cfg = Arc::new(c);

    let woken: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let woken2 = woken.clone();
    let waker: WakeFn = Arc::new(move |_t, _s, text| {
        let woken2 = woken2.clone();
        Box::pin(async move {
            woken2.lock().unwrap().push(text);
            Ok(true)
        })
    });
    let engine = Engine::new_with_injector(
        cfg.clone(),
        waker,
        Box::new(ThresholdInjector {
            len: 1,
            max_ok_pins: 99,
        }),
    );
    let k = key("s1");
    engine.register_launch("l1", WakeTarget::default(), dir.path().to_path_buf());
    let k = SessionKey {
        launch: Some("l1".into()),
        ..k
    };

    engine.observe_request(
        &k,
        Wire::AnthropicMessages,
        br#"{"messages":[{"role":"user","content":"hi"}]}"#,
    );
    wait_for(|| count_lines(&log_text(&cfg), "lint", "fail") >= 1).await;

    engine.idle_sweep().await;
    assert!(
        woken.lock().unwrap().is_empty(),
        "the session is not idle long enough yet"
    );
    let (_, ids) = engine.attach_pings(&k, Wire::AnthropicMessages, b"{}").unwrap();
    assert_eq!(
        ids.len(),
        1,
        "the ping must still be pending, not consumed by the sweep"
    );
}

#[tokio::test]
async fn idle_sweep_ignores_sessions_without_a_registered_launch() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = sandboxed_config(dir.path());
    c.pings.wake_idle = true;
    c.pings.idle_after_secs = 0;
    let script = write_script(dir.path(), "lint.sh", &always_fail_with("bad"));
    c.hooks = vec![hook("lint", vec![HookEvent::Prompt], &script)];
    let cfg = Arc::new(c);

    let engine = Engine::new_with_injector(
        cfg.clone(),
        noop_waker(),
        Box::new(ThresholdInjector {
            len: 1,
            max_ok_pins: 99,
        }),
    );
    let k = key("s1"); // no register_launch call: no wake target

    engine.observe_request(
        &k,
        Wire::AnthropicMessages,
        br#"{"messages":[{"role":"user","content":"hi"}]}"#,
    );
    wait_for(|| count_lines(&log_text(&cfg), "lint", "fail") >= 1).await;

    engine.idle_sweep().await;
    let (_, ids) = engine.attach_pings(&k, Wire::AnthropicMessages, b"{}").unwrap();
    assert_eq!(
        ids.len(),
        1,
        "with no wake target the sweep must leave the pending ping alone"
    );
}

#[tokio::test]
async fn idle_sweep_is_a_noop_when_wake_idle_is_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = sandboxed_config(dir.path());
    c.pings.wake_idle = false;
    c.pings.idle_after_secs = 0;
    let script = write_script(dir.path(), "lint.sh", &always_fail_with("bad"));
    c.hooks = vec![hook("lint", vec![HookEvent::Prompt], &script)];
    let cfg = Arc::new(c);

    let woken: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let woken2 = woken.clone();
    let waker: WakeFn = Arc::new(move |_t, _s, text| {
        let woken2 = woken2.clone();
        Box::pin(async move {
            woken2.lock().unwrap().push(text);
            Ok(true)
        })
    });
    let engine = Engine::new_with_injector(
        cfg.clone(),
        waker,
        Box::new(ThresholdInjector {
            len: 1,
            max_ok_pins: 99,
        }),
    );
    let k = key("s1");
    engine.register_launch("l1", WakeTarget::default(), dir.path().to_path_buf());
    let k = SessionKey {
        launch: Some("l1".into()),
        ..k
    };

    engine.observe_request(
        &k,
        Wire::AnthropicMessages,
        br#"{"messages":[{"role":"user","content":"hi"}]}"#,
    );
    wait_for(|| count_lines(&log_text(&cfg), "lint", "fail") >= 1).await;

    engine.idle_sweep().await;
    assert!(woken.lock().unwrap().is_empty());
}
