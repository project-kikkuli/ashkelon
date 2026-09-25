use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Instant;

use crate::transform::PinnedPing;
use crate::wake::WakeTarget;

use super::ping::Ping;
use super::types::HookTrigger;

/// Per-hook-name run bookkeeping: never run the same hook concurrently for one session, and
/// coalesce any event that arrives mid-run into a single rerun afterward.
#[derive(Default)]
pub struct HookRun {
    pub running: bool,
    pub rerun: Option<HookTrigger>,
}

pub struct SessionState {
    /// When this session was first observed. Tracked as part of the session's bookkeeping for
    /// future introspection (e.g. session age); not consumed by any decision in this engine yet.
    #[allow(dead_code)]
    pub first_seen: Instant,
    pub last_request_at: Instant,
    pub prev_conversation_len: Option<usize>,
    pub last_prompt_fingerprint: Option<String>,
    /// Latest user prompt, carried into turn_end for criterion attribution.
    pub last_prompt: Option<String>,
    pub cwd: Option<PathBuf>,
    pub wake_target: Option<WakeTarget>,
    /// Hook failures not yet attached to any outgoing request.
    pub outbox: Vec<Ping>,
    /// Pings already pinned into a request, re-inserted at the same anchor on every later one.
    pub delivered: Vec<PinnedPing>,
    /// Every ping id ever delivered by any path (pinned into a request or woken), for lifetime
    /// dedup: an identical failure (same hook + message, hence same `Ping::id`) is pinged at
    /// most once per session, whether delivery happens through injection or through a wake.
    pub delivered_ids: HashSet<String>,
    /// Total pings ever delivered (pinned or woken), for `pings.max_per_session`.
    pub delivered_count: u32,
    pub hook_runs: HashMap<String, HookRun>,
}

impl SessionState {
    pub fn new(cwd: Option<PathBuf>, wake_target: Option<WakeTarget>) -> SessionState {
        let now = Instant::now();
        SessionState {
            first_seen: now,
            last_request_at: now,
            prev_conversation_len: None,
            last_prompt_fingerprint: None,
            last_prompt: None,
            cwd,
            wake_target,
            outbox: Vec::new(),
            delivered: Vec::new(),
            delivered_ids: HashSet::new(),
            delivered_count: 0,
            hook_runs: HashMap::new(),
        }
    }
}
