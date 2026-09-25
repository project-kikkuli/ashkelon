pub mod context;
pub mod extract;
pub mod matching;
mod ping;
mod runner;
mod state;
mod types;

pub use ping::{is_ping_text, Ping};
use types::HookTrigger;

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use tokio::io::AsyncWriteExt;
use tokio::sync::Semaphore;

use crate::config::{Config, HookConfig, HookEvent};
use crate::session::SessionKey;
use crate::transform::{self, PinnedPing};
use crate::usage::Summary;
use crate::wake::{self, WakeTarget};
use crate::wire::Wire;

use runner::Outcome;
use state::SessionState;
use types::event_name;

/// Delivers a ping to an idle agent. Boxed so tests can substitute a fake without ever spawning
/// a harness or touching real credentials; production uses [`wake::wake`].
pub type WakeFn = Arc<
    dyn Fn(WakeTarget, SessionKey, String) -> Pin<Box<dyn Future<Output = anyhow::Result<bool>> + Send>> + Send + Sync,
>;

fn default_waker() -> WakeFn {
    Arc::new(|target, session, text| Box::pin(async move { wake::wake(&target, &session, &text).await }))
}

/// The two shared, harness-facing pinning primitives (`transform::conversation_len` /
/// `transform::inject_pings`), abstracted so the engine's own pin bookkeeping — what gets
/// queued, delivered, retried, or restored — can be tested without depending on the real JSON
/// insertion those functions perform.
pub trait PinInjector: Send + Sync {
    fn conversation_len(&self, wire: Wire, body: &[u8]) -> Option<usize>;
    fn inject(&self, wire: Wire, body: &[u8], pins: &[PinnedPing]) -> Option<Vec<u8>>;
}

struct RealInjector;

impl PinInjector for RealInjector {
    fn conversation_len(&self, wire: Wire, body: &[u8]) -> Option<usize> {
        transform::conversation_len(wire, body)
    }

    fn inject(&self, wire: Wire, body: &[u8], pins: &[PinnedPing]) -> Option<Vec<u8>> {
        transform::inject_pings(wire, body, pins)
    }
}

struct LaunchInfo {
    target: WakeTarget,
    cwd: PathBuf,
}

pub struct Engine {
    cfg: Arc<Config>,
    launches: Mutex<HashMap<String, LaunchInfo>>,
    /// Claude Code channel MCP servers that have self-registered (serve mode; `run` mode reaches
    /// its channel through `launches` instead), keyed by `CLAUDE_CODE_SESSION_ID` — the same id
    /// Claude Code puts in `metadata.user_id.session_id` on every request, so it is exactly
    /// `SessionKey::session` for that session (confirmed by direct comparison against a live
    /// Claude Code process's own environment, not assumed from documentation).
    claude_channels: Mutex<HashMap<String, PathBuf>>,
    sessions: Mutex<HashMap<SessionKey, SessionState>>,
    hook_semaphore: Semaphore,
    waker: WakeFn,
    injector: Box<dyn PinInjector>,
    self_weak: OnceLock<Weak<Engine>>,
}

impl Engine {
    pub fn new(cfg: Arc<Config>) -> Arc<Engine> {
        Engine::new_with_injector(cfg, default_waker(), Box::new(RealInjector))
    }

    /// Same as [`Engine::new`], but with the idle-wake delivery mechanism replaced. For tests
    /// that need to observe (or refuse) a wake without starting any real harness.
    pub fn new_with_waker(cfg: Arc<Config>, waker: WakeFn) -> Arc<Engine> {
        Engine::new_with_injector(cfg, waker, Box::new(RealInjector))
    }

    /// Same as [`Engine::new`], but with the pin-injection mechanism replaced. For tests that
    /// need to exercise `attach_pings`'s bookkeeping without depending on the real (and, at
    /// this point in the build, stubbed) JSON insertion in `transform`.
    pub fn new_with_injector(cfg: Arc<Config>, waker: WakeFn, injector: Box<dyn PinInjector>) -> Arc<Engine> {
        let permits = cfg.pings.max_concurrent_hooks.max(1);
        let engine = Arc::new(Engine {
            cfg,
            launches: Mutex::new(HashMap::new()),
            claude_channels: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
            hook_semaphore: Semaphore::new(permits),
            waker,
            injector,
            self_weak: OnceLock::new(),
        });
        let _ = engine.self_weak.set(Arc::downgrade(&engine));
        engine
    }

    fn arc(&self) -> Arc<Engine> {
        self.self_weak
            .get()
            .and_then(Weak::upgrade)
            .expect("Engine used after its Arc was dropped")
    }

    /// Launches the idle-wake sweep. The constructor stays sync; call this once the engine is
    /// wired into a running Tokio runtime.
    pub fn start(self: &Arc<Self>) {
        let engine = Arc::clone(self);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(5));
            loop {
                ticker.tick().await;
                engine.idle_sweep().await;
            }
        });
    }

    /// Records how to reach a launched agent and where it runs.
    pub fn register_launch(&self, launch: &str, target: WakeTarget, cwd: PathBuf) {
        self.launches
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(launch.to_string(), LaunchInfo { target, cwd });
    }

    fn session_dir(&self, key: &SessionKey) -> PathBuf {
        self.cfg.state_dir().join("sessions").join(session_hash(key))
    }

    /// Records a serve-mode Claude Code channel's control socket against the Claude Code session
    /// id it reported (its own `CLAUDE_CODE_SESSION_ID`). Updates any session already tracked
    /// under that id immediately, in case its first request arrived before this registration did;
    /// a session created afterward picks the socket up in `record_request` instead.
    pub fn register_claude_channel(&self, session_id: &str, socket: PathBuf) {
        self.claude_channels
            .lock()
            .unwrap()
            .insert(session_id.to_string(), socket.clone());
        let mut sessions = self.sessions.lock().unwrap();
        for (key, state) in sessions.iter_mut() {
            if key.harness.as_deref() == Some("claude") && key.session == session_id {
                state.wake_target = Some(WakeTarget {
                    harness: "claude".to_string(),
                    tmux_pane: None,
                    control: Some(socket.to_string_lossy().into_owned()),
                    control_auth: None,
                    harness_session_id: Some(session_id.to_string()),
                });
            }
        }
    }

    /// Called with every request the agent sent (before transforms). Fires session_start /
    /// prompt / tool_result / compaction hooks in the background; never blocks.
    pub fn observe_request(&self, key: &SessionKey, wire: Wire, body: &[u8]) {
        let triggers = self.record_request(key, wire, body);
        let engine = self.arc();
        let key = key.clone();
        let body = body.to_vec();
        let dir = self.session_dir(&key);
        tokio::spawn(async move {
            persist_file(&dir, "request.json", &body).await;
            for trigger in triggers {
                engine.dispatch(&key, trigger).await;
            }
        });
    }

    /// Called when a response finishes. Fires tool_call / turn_end hooks in the background;
    /// never blocks.
    pub fn observe_response(&self, key: &SessionKey, _wire: Wire, summary: &Summary) {
        let triggers = self.record_response(key, summary);
        let engine = self.arc();
        let key = key.clone();
        let text = summary.text.clone();
        let dir = self.session_dir(&key);
        tokio::spawn(async move {
            if !text.is_empty() {
                persist_file(&dir, "response.txt", text.as_bytes()).await;
            }
            for trigger in triggers {
                engine.dispatch(&key, trigger).await;
            }
        });
    }

    /// The synchronous half of `observe_request`: updates session state and decides which
    /// events fired, without doing any I/O or process spawning itself.
    fn record_request(&self, key: &SessionKey, wire: Wire, body: &[u8]) -> Vec<HookTrigger> {
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        let is_new = !sessions.contains_key(key);
        if is_new {
            let launch_info = key.launch.as_deref().and_then(|id| {
                self.launches
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(id)
                    .map(|i| (i.cwd.clone(), i.target.clone()))
            });
            let (cwd, wake_target) = match launch_info {
                Some((cwd, target)) => (Some(cwd), Some(target)),
                // `run` always registers a launch; a session with none was pointed at this relay
                // directly (`ashkelon serve`), so cwd and the wake channel have to come from
                // whatever the harness itself told the model, or from a serve-mode registration
                // keyed by the harness's own session id (`claude_channels`, `register_launch`'s
                // serve-mode counterpart for Claude Code's MCP channel).
                None => (context::derive_cwd(wire, body), self.serve_mode_wake_target(key)),
            };
            sessions.insert(key.clone(), SessionState::new(cwd, wake_target));
        }
        let state = sessions.get_mut(key).expect("just inserted or already present");
        state.last_request_at = Instant::now();

        let mut triggers = Vec::new();
        if is_new {
            triggers.push(HookTrigger::new(HookEvent::SessionStart));
        }

        let new_len = extract::conversation_length(wire, body);
        if let (Some(prev), Some(new)) = (state.prev_conversation_len, new_len) {
            if new < prev {
                state.delivered.clear();
                triggers.push(HookTrigger::new(HookEvent::Compaction));
            }
        }
        if new_len.is_some() {
            state.prev_conversation_len = new_len;
        }

        if let Some(extracted) = extract::last_item(wire, body) {
            if let Some(prompt) = extracted.prompt {
                let fp = ping::fingerprint(&prompt);
                if state.last_prompt_fingerprint.as_deref() != Some(fp.as_str()) {
                    state.last_prompt_fingerprint = Some(fp);
                    state.last_prompt = Some(prompt.clone());
                    triggers.push(HookTrigger::with_prompt(HookEvent::Prompt, prompt));
                }
            }
            if extracted.has_tool_result {
                triggers.push(HookTrigger::new(HookEvent::ToolResult));
            }
        }

        triggers
    }

    /// Builds a [`WakeTarget`] for a session that arrived with no registered launch (serve mode),
    /// from whatever this harness lets the daemon reach it by without one:
    ///
    /// - Codex always reports its own thread id as `SessionKey::session` (see
    ///   `wake::try_verified_adapter`'s codex arm), so a bare `WakeTarget{harness: "codex"}` is
    ///   enough; no separate registration is needed.
    /// - Claude Code needs its channel MCP server to have self-registered under this session's id
    ///   first (`register_claude_channel`); until then there is nothing to wake it with.
    /// - opencode, omp, hermes, ori: no serve-mode channel exists (opencode's `serve` isn't one
    ///   this daemon started, so its control URL and password are never known here; the others
    ///   have no verified local channel at all — see `wake::try_verified_adapter`), and a tmux
    ///   pane observed from the daemon's own environment would not reliably be the harness's
    ///   pane, so none of these get a wake target in serve mode.
    fn serve_mode_wake_target(&self, key: &SessionKey) -> Option<WakeTarget> {
        match key.harness.as_deref() {
            Some("codex") => Some(WakeTarget {
                harness: "codex".to_string(),
                tmux_pane: None,
                control: None,
                control_auth: None,
                harness_session_id: None,
            }),
            Some("claude") => {
                let socket = self.claude_channels.lock().unwrap().get(&key.session).cloned()?;
                Some(WakeTarget {
                    harness: "claude".to_string(),
                    tmux_pane: None,
                    control: Some(socket.to_string_lossy().into_owned()),
                    control_auth: None,
                    harness_session_id: Some(key.session.clone()),
                })
            }
            _ => None,
        }
    }

    fn record_response(&self, key: &SessionKey, summary: &Summary) -> Vec<HookTrigger> {
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        let Some(state) = sessions.get_mut(key) else {
            // A response with no matching request-derived session is nothing this engine can
            // attribute background work to; drop it rather than inventing state for it.
            return Vec::new();
        };
        state.last_request_at = Instant::now();

        let mut triggers = Vec::new();
        if !summary.tool_calls.is_empty() {
            let names = summary.tool_calls.iter().map(|t| t.name.clone()).collect();
            triggers.push(HookTrigger::with_tool_calls(HookEvent::ToolCall, names));
        }
        if summary.turn_end {
            let mut trigger = HookTrigger::with_text(HookEvent::TurnEnd, summary.text.clone());
            trigger.prompt = state.last_prompt.clone();
            triggers.push(trigger);
        }
        triggers
    }

    /// Runs every configured hook that matches `trigger`'s event for this session, each as its
    /// own background task so unrelated hooks never wait on one another.
    async fn dispatch(&self, key: &SessionKey, trigger: HookTrigger) {
        let cwd = {
            let sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            sessions.get(key).and_then(|s| s.cwd.clone())
        };
        for hook in &self.cfg.hooks {
            if !matching::matches(hook, trigger.event, key.harness.as_deref(), cwd.as_deref()) {
                continue;
            }
            let engine = self.arc();
            let key = key.clone();
            let hook = hook.clone();
            let trigger = trigger.clone();
            tokio::spawn(async move {
                engine.trigger_hook(key, hook, trigger).await;
            });
        }
    }

    /// Runs `hook` for `trigger`, honoring the per-(session, hook) busy flag: a hook already
    /// running for this session queues this trigger as a single coalesced rerun instead of
    /// running concurrently with itself.
    async fn trigger_hook(&self, key: SessionKey, hook: HookConfig, mut trigger: HookTrigger) {
        // Become the runner for (session, hook), or queue this trigger as the coalesced rerun
        // if someone already owns that slot. This check-and-acquire happens exactly once: once
        // we own the slot, later loop iterations (running the queued rerun) must not re-check
        // it, or they would mistake their own ownership for someone else's and queue forever.
        {
            let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            let Some(state) = sessions.get_mut(&key) else { return };
            let run = state.hook_runs.entry(hook.name.clone()).or_default();
            if run.running {
                run.rerun = Some(trigger);
                return;
            }
            run.running = true;
        }

        loop {
            let cwd = {
                let sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
                sessions.get(&key).and_then(|s| s.cwd.clone())
            };
            let dir = self.session_dir(&key);
            let result = runner::run_hook(&self.hook_semaphore, &hook, &key, &trigger, cwd.as_deref(), &dir).await;
            self.apply_outcome(&key, &hook, trigger.event, result).await;

            let next = {
                let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
                match sessions.get_mut(&key) {
                    Some(state) => {
                        let run = state.hook_runs.entry(hook.name.clone()).or_default();
                        match run.rerun.take() {
                            Some(t) => Some(t),
                            None => {
                                run.running = false;
                                None
                            }
                        }
                    }
                    None => None,
                }
            };
            match next {
                Some(t) => trigger = t,
                None => return,
            }
        }
    }

    async fn apply_outcome(&self, key: &SessionKey, hook: &HookConfig, event: HookEvent, result: runner::RunResult) {
        let status = match &result.outcome {
            Outcome::Pass => "pass",
            Outcome::Noop => "noop",
            Outcome::Signal { .. } => "signal",
            Outcome::Fail { .. } => "fail",
            Outcome::Timeout => "timeout",
            Outcome::Error(_) => "error",
        };
        self.append_hook_log(&hook.name, event, &key.session, status, result.duration_ms)
            .await;

        match result.outcome {
            Outcome::Noop => {}
            Outcome::Signal { message } => {
                if message.is_empty() {
                    return;
                }
                let candidate = Ping::signal(&hook.name, &message);
                let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(state) = sessions.get_mut(key) {
                    // Keep only the latest not-yet-delivered signal from this hook.
                    state.outbox.retain(|p| p.hook != hook.name || !p.transient);
                    let cap = self.cfg.pings.max_per_session;
                    if state.delivered_count + (state.outbox.len() as u32) < cap {
                        state.outbox.push(candidate);
                    }
                }
            }
            Outcome::Pass => {
                let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(state) = sessions.get_mut(key) {
                    state.outbox.retain(|p| p.hook != hook.name);
                }
            }
            Outcome::Fail { message, fix } => {
                let candidate = Ping::new(&hook.name, &message, fix.as_deref());
                let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(state) = sessions.get_mut(key) {
                    let duplicate = state.outbox.iter().any(|p| p.id == candidate.id)
                        || state.delivered_ids.contains(&candidate.id);
                    let cap = self.cfg.pings.max_per_session;
                    let at_cap = state.delivered_count + state.outbox.len() as u32 >= cap;
                    if !duplicate && !at_cap {
                        state.outbox.push(candidate);
                    }
                }
            }
            Outcome::Timeout => {
                tracing::warn!(hook = %hook.name, "hook timed out");
            }
            Outcome::Error(reason) => {
                tracing::warn!(hook = %hook.name, reason = %reason, "hook error");
            }
        }
    }

    async fn append_hook_log(&self, hook: &str, event: HookEvent, session: &str, status: &str, duration_ms: u64) {
        let dir = self.cfg.log_dir();
        if let Err(e) = crate::fsperm::create_dir_private_async(&dir).await {
            tracing::warn!(error = %e, "failed to create hook log dir");
            return;
        }
        let path = dir.join(format!("hooks-{}.jsonl", runner::today_date()));
        let mut line = serde_json::json!({
            "ts": runner::now_rfc3339(),
            "hook": hook,
            "event": event_name(event),
            "session": session,
            "status": status,
            "duration_ms": duration_ms,
        })
        .to_string();
        line.push('\n');

        match tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
        {
            Ok(mut f) => {
                // Narrowed before any content is appended, not after.
                if let Err(e) = crate::fsperm::set_private_file_async(&path).await {
                    tracing::warn!(error = %e, "failed to set hook log permissions");
                }
                if let Err(e) = f.write_all(line.as_bytes()).await {
                    tracing::warn!(error = %e, "failed to write hook log line");
                }
            }
            Err(e) => tracing::warn!(error = %e, "failed to open hook log"),
        }
    }

    /// Returns the request body with pending and previously delivered pings inserted, plus the
    /// ids newly delivered.
    pub fn attach_pings(&self, key: &SessionKey, wire: Wire, body: &[u8]) -> Option<(Vec<u8>, Vec<String>)> {
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        let state = sessions.get_mut(key)?;
        if state.outbox.is_empty() && state.delivered.is_empty() {
            return None;
        }

        let len = self.injector.conversation_len(wire, body)?;
        let anchor = len.checked_sub(1)?;

        let new_pings: Vec<Ping> = state.outbox.drain(..).collect();
        let new_pins: Vec<PinnedPing> = new_pings.iter().map(|p| ping::into_pinned(p, anchor)).collect();

        let mut all_pins = state.delivered.clone();
        all_pins.extend(new_pins.iter().cloned());

        if let Some(new_body) = self.injector.inject(wire, body, &all_pins) {
            state.delivered_ids.extend(new_pins.iter().map(|p| p.id.clone()));
            state.delivered.extend(
                new_pings
                    .iter()
                    .zip(new_pins.iter())
                    .filter_map(|(p, pin)| (!p.transient).then_some(pin.clone())),
            );
            state.delivered_count += new_pins.len() as u32;
            let ids = new_pins.into_iter().map(|p| p.id).collect();
            return Some((new_body, ids));
        }

        // The old anchors no longer line up with this body (the conversation moved under us):
        // they're stale. Drop them and retry with only the pins we're adding now. `delivered_ids`
        // is untouched: those pings were genuinely delivered and stay deduped for the rest of the
        // session even though their anchors are gone.
        state.delivered.clear();
        if let Some(new_body) = self.injector.inject(wire, body, &new_pins) {
            state.delivered_ids.extend(new_pins.iter().map(|p| p.id.clone()));
            state.delivered.extend(
                new_pings
                    .iter()
                    .zip(new_pins.iter())
                    .filter_map(|(p, pin)| (!p.transient).then_some(pin.clone())),
            );
            state.delivered_count += new_pins.len() as u32;
            let ids = new_pins.into_iter().map(|p| p.id).collect();
            return Some((new_body, ids));
        }

        // Total failure: put the new pings back so they aren't lost.
        state.outbox.extend(new_pings);
        None
    }

    /// Wakes idle sessions that have pending pings and a registered launch. Runs on a 5s tick
    /// via [`Engine::start`]; also callable directly (as tests do) to sweep once on demand.
    pub async fn idle_sweep(&self) {
        if !self.cfg.pings.wake_idle {
            return;
        }
        let idle_after = Duration::from_secs(self.cfg.pings.idle_after_secs);
        let now = Instant::now();

        let candidates: Vec<(SessionKey, WakeTarget, String)> = {
            let sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            sessions
                .iter()
                .filter_map(|(key, state)| {
                    if state.outbox.is_empty() {
                        return None;
                    }
                    if now.duration_since(state.last_request_at) < idle_after {
                        return None;
                    }
                    let target = state.wake_target.clone()?;
                    let text = state
                        .outbox
                        .iter()
                        .map(|p| p.text.clone())
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    Some((key.clone(), target, text))
                })
                .collect()
        };

        for (key, target, text) in candidates {
            if let Ok(true) = (self.waker)(target, key.clone(), text).await {
                let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(state) = sessions.get_mut(&key) {
                    // The harness will carry this as a real user turn, so it needs no anchor:
                    // mark delivered without pinning. `delivered_ids` still has to record these
                    // ids, or an identical future failure would neither be seen as a duplicate
                    // nor be reflected in `delivered_count`'s dedup, and would wake the agent
                    // again for the same thing every idle-sweep tick.
                    state.delivered_ids.extend(state.outbox.iter().map(|p| p.id.clone()));
                    state.delivered_count += state.outbox.len() as u32;
                    state.outbox.clear();
                    state.last_request_at = Instant::now();
                }
            }
        }
    }
}

fn session_hash(key: &SessionKey) -> String {
    use sha2::{Digest, Sha256};
    let canon = format!(
        "{}\u{0}{}\u{0}{}",
        key.launch.as_deref().unwrap_or(""),
        key.harness.as_deref().unwrap_or(""),
        key.session
    );
    hex::encode(Sha256::digest(canon.as_bytes()))[..16].to_string()
}

async fn persist_file(dir: &Path, name: &str, bytes: &[u8]) {
    if let Err(e) = crate::fsperm::create_dir_private_async(dir).await {
        tracing::warn!(error = %e, "failed to create session state dir");
        return;
    }
    let path = dir.join(name);
    if let Err(e) = crate::fsperm::write_private_file_async(&path, bytes).await {
        tracing::warn!(error = %e, "failed to write session state file");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> Arc<Engine> {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Arc::new(Config {
            state_dir: Some(dir.path().join("state")),
            log_dir: Some(dir.path().join("logs")),
            ..Config::default()
        });
        // Leaked on purpose: the tempdir only needs to outlive this test process, not be cleaned
        // up, since these tests never assert on-disk session artifacts.
        std::mem::forget(dir);
        Engine::new_with_waker(cfg, Arc::new(|_target, _session, _text| Box::pin(async { Ok(false) })))
    }

    fn key(harness: &str, session: &str) -> SessionKey {
        SessionKey {
            launch: None,
            harness: Some(harness.to_string()),
            session: session.to_string(),
        }
    }

    #[test]
    fn serve_mode_session_gets_cwd_from_claude_body_and_no_registered_launch() {
        let engine = engine();
        let body = br#"{"system":"be terse","messages":[{"role":"user","content":[{"type":"text","text":"<system-reminder>\n# Environment\n - Primary working directory: /work/proj\n</system-reminder>"}]}]}"#;
        engine.record_request(&key("claude", "sess-1"), Wire::AnthropicMessages, body);
        let sessions = engine.sessions.lock().unwrap();
        let state = sessions.get(&key("claude", "sess-1")).unwrap();
        assert_eq!(state.cwd, Some(PathBuf::from("/work/proj")));
    }

    #[test]
    fn serve_mode_codex_session_gets_a_bare_wake_target_without_any_registration() {
        let engine = engine();
        let body = br#"{"input":[]}"#;
        engine.record_request(&key("codex", "thread-1"), Wire::OpenAiResponses, body);
        let sessions = engine.sessions.lock().unwrap();
        let state = sessions.get(&key("codex", "thread-1")).unwrap();
        assert_eq!(state.wake_target.as_ref().map(|t| t.harness.as_str()), Some("codex"));
    }

    #[test]
    fn serve_mode_claude_session_has_no_wake_target_before_channel_registration() {
        let engine = engine();
        engine.record_request(&key("claude", "sess-2"), Wire::AnthropicMessages, b"{}");
        let sessions = engine.sessions.lock().unwrap();
        let state = sessions.get(&key("claude", "sess-2")).unwrap();
        assert!(state.wake_target.is_none());
    }

    #[test]
    fn channel_registration_before_first_request_is_picked_up_on_session_creation() {
        let engine = engine();
        engine.register_claude_channel("sess-3", PathBuf::from("/tmp/sess-3.sock"));
        engine.record_request(&key("claude", "sess-3"), Wire::AnthropicMessages, b"{}");
        let sessions = engine.sessions.lock().unwrap();
        let state = sessions.get(&key("claude", "sess-3")).unwrap();
        let target = state.wake_target.as_ref().expect("wake target from prior registration");
        assert_eq!(target.control.as_deref(), Some("/tmp/sess-3.sock"));
    }

    #[test]
    fn channel_registration_after_first_request_updates_the_live_session() {
        let engine = engine();
        engine.record_request(&key("claude", "sess-4"), Wire::AnthropicMessages, b"{}");
        engine.register_claude_channel("sess-4", PathBuf::from("/tmp/sess-4.sock"));
        let sessions = engine.sessions.lock().unwrap();
        let state = sessions.get(&key("claude", "sess-4")).unwrap();
        let target = state.wake_target.as_ref().expect("wake target from late registration");
        assert_eq!(target.control.as_deref(), Some("/tmp/sess-4.sock"));
    }

    #[test]
    fn run_mode_launch_info_still_takes_priority_over_body_derived_cwd() {
        let engine = engine();
        engine.register_launch(
            "launch-1",
            WakeTarget {
                harness: "claude".to_string(),
                tmux_pane: None,
                control: Some("/tmp/launch-1.sock".to_string()),
                control_auth: None,
                harness_session_id: None,
            },
            PathBuf::from("/exact/launch/cwd"),
        );
        let body = br#"{"messages":[{"role":"user","content":"Primary working directory: /wrong/from/body"}]}"#;
        let k = SessionKey {
            launch: Some("launch-1".to_string()),
            harness: Some("claude".to_string()),
            session: "sess-5".to_string(),
        };
        engine.record_request(&k, Wire::AnthropicMessages, body);
        let sessions = engine.sessions.lock().unwrap();
        let state = sessions.get(&k).unwrap();
        assert_eq!(state.cwd, Some(PathBuf::from("/exact/launch/cwd")));
    }
}
