mod claude;
mod codex;
mod hermes;
mod omp;
mod opencode;
mod ori;

use std::path::PathBuf;

use crate::wake::WakeTarget;

/// Substituted in a [`LaunchPlan`]'s args/env once a [`CompanionProcess`] this plan depends on
/// has actually started and reported the address it bound. Only opencode uses this today (the
/// `serve` process binds an ephemeral port the `attach` TUI and the wake control endpoint both
/// need to agree on).
pub const COMPANION_URL_PLACEHOLDER: &str = "{{ASHKELON_COMPANION_URL}}";

/// A second local process a [`LaunchPlan`] needs running alongside its main program, started
/// first and torn down when the main program exits.
#[derive(Debug)]
pub struct CompanionProcess {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Matched against each line of the companion's stderr to learn the address it bound
    /// (named capture groups `host` and `port`). Once matched, [`COMPANION_URL_PLACEHOLDER`] in
    /// the owning [`LaunchPlan`]'s args/env/wake target is replaced with `http://<host>:<port>`.
    pub ready_pattern: regex::Regex,
}

/// Everything needed to start one harness pointed at the relay.
#[derive(Debug)]
pub struct LaunchPlan {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub env_remove: Vec<String>,
    /// Temporary files (configs) that live as long as the launch.
    pub temp_files: Vec<PathBuf>,
    pub wake: WakeTarget,
    pub companion: Option<CompanionProcess>,
}

impl LaunchPlan {
    fn new(program: impl Into<String>, harness: &str) -> LaunchPlan {
        LaunchPlan {
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
            env_remove: Vec::new(),
            temp_files: Vec::new(),
            wake: WakeTarget { harness: harness.to_string(), tmux_pane: tmux_pane_from_env(), control: None, harness_session_id: None },
            companion: None,
        }
    }

    /// Replaces every occurrence of [`COMPANION_URL_PLACEHOLDER`] in args, env values, and the
    /// wake target's control endpoint with `url`. Called once the companion process (if any) has
    /// reported the address it actually bound.
    pub fn resolve_companion_url(&mut self, url: &str) {
        for arg in &mut self.args {
            if arg.contains(COMPANION_URL_PLACEHOLDER) {
                *arg = arg.replace(COMPANION_URL_PLACEHOLDER, url);
            }
        }
        for (_, value) in &mut self.env {
            if value.contains(COMPANION_URL_PLACEHOLDER) {
                *value = value.replace(COMPANION_URL_PLACEHOLDER, url);
            }
        }
        if self.wake.control.as_deref() == Some(COMPANION_URL_PLACEHOLDER) {
            self.wake.control = Some(url.to_string());
        }
    }
}

fn tmux_pane_from_env() -> Option<String> {
    std::env::var("TMUX_PANE").ok().filter(|pane| !pane.is_empty())
}

/// `relay_base` is `http://127.0.0.1:<port>/s/<launch>`; routes hang off it (`/anthropic`, `/openai`, ...).
pub fn plan(harness: &str, relay_base: &str, launch: &str, args: &[String]) -> anyhow::Result<LaunchPlan> {
    match harness {
        "claude" => claude::plan(relay_base, launch, args),
        "codex" => codex::plan(relay_base, launch, args),
        "opencode" => opencode::plan(relay_base, launch, args),
        "omp" => omp::plan(relay_base, launch, args),
        "hermes" => hermes::plan(relay_base, launch, args),
        "ori" => ori::plan(relay_base, launch, args),
        "cursor" => anyhow::bail!(
            "cursor is not supported: neither the cursor-agent CLI nor the Cursor IDE has a base-URL/endpoint override, \
             so there is nothing ashkelon can route through the relay (see routing report)"
        ),
        other => anyhow::bail!(
            "unsupported harness: {other} (supported: claude, codex, opencode, omp, hermes, ori)"
        ),
    }
}
