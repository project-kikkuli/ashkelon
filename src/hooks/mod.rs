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
            .unwrap()
            .insert(launch.to_string(), LaunchInfo { target, cwd });
    }

    fn session_dir(&self, key: &SessionKey) -> PathBuf {
        self.cfg.state_dir().join("sessions").join(session_hash(key))
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
        let mut sessions = self.sessions.lock().unwrap();
        let is_new = !sessions.contains_key(key);
        if is_new {
            let launch_info = key.launch.as_deref().and_then(|id| {
                self.launches
                    .lock()
                    .unwrap()
                    .get(id)
                    .map(|i| (i.cwd.clone(), i.target.clone()))
            });
            let (cwd, wake_target) = match launch_info {
                Some((cwd, target)) => (Some(cwd), Some(target)),
                None => (None, None),
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
                    triggers.push(HookTrigger::with_prompt(HookEvent::Prompt, prompt));
                }
            }
            if extracted.has_tool_result {
                triggers.push(HookTrigger::new(HookEvent::ToolResult));
            }
        }

        triggers
    }

    fn record_response(&self, key: &SessionKey, summary: &Summary) -> Vec<HookTrigger> {
        let mut sessions = self.sessions.lock().unwrap();
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
            triggers.push(HookTrigger::with_text(HookEvent::TurnEnd, summary.text.clone()));
        }
        triggers
    }

    /// Runs every configured hook that matches `trigger`'s event for this session, each as its
    /// own background task so unrelated hooks never wait on one another.
    async fn dispatch(&self, key: &SessionKey, trigger: HookTrigger) {
        let cwd = {
            let sessions = self.sessions.lock().unwrap();
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
            let mut sessions = self.sessions.lock().unwrap();
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
                let sessions = self.sessions.lock().unwrap();
                sessions.get(&key).and_then(|s| s.cwd.clone())
            };
            let dir = self.session_dir(&key);
            let result = runner::run_hook(&self.hook_semaphore, &hook, &key, &trigger, cwd.as_deref(), &dir).await;
            self.apply_outcome(&key, &hook, trigger.event, result).await;

            let next = {
                let mut sessions = self.sessions.lock().unwrap();
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
            Outcome::Fail { .. } => "fail",
            Outcome::Timeout => "timeout",
            Outcome::Error(_) => "error",
        };
        self.append_hook_log(&hook.name, event, &key.session, status, result.duration_ms)
            .await;

        match result.outcome {
            Outcome::Pass => {
                let mut sessions = self.sessions.lock().unwrap();
                if let Some(state) = sessions.get_mut(key) {
                    state.outbox.retain(|p| p.hook != hook.name);
                }
            }
            Outcome::Fail { message, fix } => {
                let candidate = Ping::new(&hook.name, &message, fix.as_deref());
                let mut sessions = self.sessions.lock().unwrap();
                if let Some(state) = sessions.get_mut(key) {
                    let duplicate = state.outbox.iter().any(|p| p.id == candidate.id)
                        || state.delivered.iter().any(|p| p.id == candidate.id);
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
        if let Err(e) = tokio::fs::create_dir_all(&dir).await {
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
        let mut sessions = self.sessions.lock().unwrap();
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
            state.delivered.extend(new_pins.iter().cloned());
            state.delivered_count += new_pins.len() as u32;
            let ids = new_pins.into_iter().map(|p| p.id).collect();
            return Some((new_body, ids));
        }

        // The old anchors no longer line up with this body (the conversation moved under us):
        // they're stale. Drop them and retry with only the pins we're adding now.
        state.delivered.clear();
        if let Some(new_body) = self.injector.inject(wire, body, &new_pins) {
            state.delivered.extend(new_pins.iter().cloned());
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
            let sessions = self.sessions.lock().unwrap();
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
                let mut sessions = self.sessions.lock().unwrap();
                if let Some(state) = sessions.get_mut(&key) {
                    // The harness will carry this as a real user turn, so it needs no anchor:
                    // mark delivered without pinning.
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
    if let Err(e) = tokio::fs::create_dir_all(dir).await {
        tracing::warn!(error = %e, "failed to create session state dir");
        return;
    }
    #[cfg(unix)]
    set_mode(dir, 0o700).await;

    let path = dir.join(name);
    if let Err(e) = tokio::fs::write(&path, bytes).await {
        tracing::warn!(error = %e, "failed to write session state file");
        return;
    }
    #[cfg(unix)]
    set_mode(&path, 0o600).await;
}

#[cfg(unix)]
async fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).await;
}
