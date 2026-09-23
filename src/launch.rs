use std::path::PathBuf;

use crate::wake::WakeTarget;

/// Everything needed to start one harness pointed at the relay.
pub struct LaunchPlan {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub env_remove: Vec<String>,
    /// Temporary files (configs) that live as long as the launch.
    pub temp_files: Vec<PathBuf>,
    pub wake: WakeTarget,
}

/// `relay_base` is `http://127.0.0.1:<port>/s/<launch>`; routes hang off it (`/anthropic`, `/openai`, ...).
pub fn plan(harness: &str, _relay_base: &str, _launch: &str, _args: &[String]) -> anyhow::Result<LaunchPlan> {
    anyhow::bail!("unsupported harness: {harness}")
}
