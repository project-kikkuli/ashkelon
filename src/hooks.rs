use std::path::PathBuf;
use std::sync::Arc;

use crate::config::Config;
use crate::session::SessionKey;
use crate::usage::Summary;
use crate::wake::WakeTarget;
use crate::wire::Wire;

pub struct Engine {
    #[allow(dead_code)]
    cfg: Arc<Config>,
}

impl Engine {
    pub fn new(cfg: Arc<Config>) -> Arc<Engine> {
        Arc::new(Engine { cfg })
    }

    /// Records how to reach a launched agent and where it runs.
    pub fn register_launch(&self, _launch: &str, _target: WakeTarget, _cwd: PathBuf) {}

    /// Called with every request the agent sent (before transforms). Fires session_start / prompt /
    /// tool_result / compaction hooks in the background; never blocks.
    pub fn observe_request(&self, _key: &SessionKey, _wire: Wire, _body: &[u8]) {}

    /// Called when a response finishes. Fires tool_call / turn_end hooks in the background; never blocks.
    pub fn observe_response(&self, _key: &SessionKey, _wire: Wire, _summary: &Summary) {}

    /// Returns the request body with pending and previously delivered pings inserted, plus the ids newly delivered.
    pub fn attach_pings(&self, _key: &SessionKey, _wire: Wire, _body: &[u8]) -> Option<(Vec<u8>, Vec<String>)> {
        None
    }
}
