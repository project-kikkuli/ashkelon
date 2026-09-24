mod claude;
mod codex;
mod hermes;
mod omp;
mod opencode;
mod ori;

use std::path::{Path, PathBuf};

use crate::wake::WakeTarget;

/// Substituted in a [`LaunchPlan`]'s args/env once a [`CompanionProcess`] this plan depends on
/// has actually started and reported the address it bound. Only opencode uses this today (the
/// `serve` process's readiness banner is what tells the launcher it's safe to point the
/// `attach`/`run` client and the wake control endpoint at it).
pub const COMPANION_URL_PLACEHOLDER: &str = "{{ASHKELON_COMPANION_URL}}";

/// A second local process a [`LaunchPlan`] needs running alongside its main program, started
/// first and torn down when the main program exits.
#[derive(Debug)]
pub struct CompanionProcess {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Matched against each line of the companion's stdout/stderr to learn the address it bound
    /// (named capture groups `host` and `port`). Once matched, [`COMPANION_URL_PLACEHOLDER`] in
    /// the owning [`LaunchPlan`]'s args/env/wake target is replaced with `http://<host>:<port>`.
    pub ready_pattern: regex::Regex,
}

/// A `HERMES_HOME`-shaped temp directory built by symlinking every entry of a harness's real
/// home except one (its config file, replaced with a relay-pointed copy). See
/// `launch::build_home_overlay` / `launch::reconcile_home_overlay`.
#[derive(Debug)]
pub struct OverlayHome {
    pub overlay_dir: PathBuf,
    pub real_home: PathBuf,
    /// The one entry that is a real (non-symlink) file in `overlay_dir`, written by the plan
    /// itself — never symlinked in, and never copied back on reconcile.
    pub generated_file_name: String,
}

/// Extra knobs that apply to only some harnesses, so they don't need to be threaded through
/// every plan function's positional argument list.
pub struct LaunchOptions {
    pub state_dir: PathBuf,
    /// `ashkelon run claude --no-channel`: skip wiring the MCP channel wake mechanism.
    pub no_channel: bool,
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
    pub overlay_home: Option<OverlayHome>,
}

impl LaunchPlan {
    fn new(program: impl Into<String>, harness: &str) -> LaunchPlan {
        LaunchPlan {
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
            env_remove: Vec::new(),
            temp_files: Vec::new(),
            wake: WakeTarget {
                harness: harness.to_string(),
                tmux_pane: tmux_pane_from_env(),
                control: None,
                control_auth: None,
                harness_session_id: None,
            },
            companion: None,
            overlay_home: None,
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
pub fn plan(
    harness: &str,
    relay_base: &str,
    launch: &str,
    args: &[String],
    options: &LaunchOptions,
) -> anyhow::Result<LaunchPlan> {
    match harness {
        "claude" => claude::plan(relay_base, launch, args, options),
        "codex" => codex::plan(relay_base, launch, args, options),
        "opencode" => opencode::plan(relay_base, launch, args, options),
        "omp" => omp::plan(relay_base, launch, args, options),
        "hermes" => hermes::plan(relay_base, launch, args, options),
        "ori" => ori::plan(relay_base, launch, args, options),
        "cursor" => anyhow::bail!(
            "cursor is not supported: neither the cursor-agent CLI nor the Cursor IDE has a base-URL/endpoint override, \
             so there is nothing ashkelon can route through the relay (see routing report)"
        ),
        other => anyhow::bail!(
            "unsupported harness: {other} (supported: claude, codex, opencode, omp, hermes, ori)"
        ),
    }
}

/// Symlinks every entry of `real_home` into `overlay_dir`, except `skip_name` (the caller writes
/// that one itself, as a real modified file — never a symlink to the user's original). Creates
/// both directories if missing, so a harness that has never run yet still gets a usable overlay.
pub fn build_home_overlay(real_home: &Path, overlay_dir: &Path, skip_name: &str) -> anyhow::Result<()> {
    crate::fsperm::create_dir_private(real_home)?;
    crate::fsperm::create_dir_private(overlay_dir)?;
    for entry in std::fs::read_dir(real_home)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == std::ffi::OsStr::new(skip_name) {
            continue;
        }
        std::os::unix::fs::symlink(real_home.join(&name), overlay_dir.join(&name))?;
    }
    Ok(())
}

/// Moves back into `real_home` anything the harness materialized directly inside
/// `overlay.overlay_dir` during the run: a symlink always lived in `real_home` already (nothing
/// to do), so only a real file or directory — something the overlay didn't anticipate — needs
/// reconciling. `overlay.generated_file_name` (ashkelon's own written config) is never copied
/// back. Returns the destination paths it moved anything to.
pub fn reconcile_home_overlay(overlay: &OverlayHome) -> anyhow::Result<Vec<PathBuf>> {
    let mut moved = Vec::new();
    let entries = match std::fs::read_dir(&overlay.overlay_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(moved),
        Err(e) => return Err(e.into()),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        if name == std::ffi::OsStr::new(&overlay.generated_file_name) {
            continue;
        }
        if entry.file_type()?.is_symlink() {
            continue;
        }
        let dest = overlay.real_home.join(&name);
        move_merge(&entry.path(), &dest)?;
        moved.push(dest);
    }
    Ok(moved)
}

/// Moves `src` to `dest`. When `dest` already exists and both are directories, merges entry by
/// entry instead of clobbering; an existing non-directory collision is set aside next to `dest`
/// with a distinguishing suffix rather than either overwritten or dropped.
fn move_merge(src: &Path, dest: &Path) -> anyhow::Result<()> {
    if !dest.exists() {
        std::fs::rename(src, dest)?;
        return Ok(());
    }
    if src.is_dir() && dest.is_dir() {
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            move_merge(&entry.path(), &dest.join(entry.file_name()))?;
        }
        std::fs::remove_dir_all(src).ok();
        return Ok(());
    }
    let conflict_name = format!(
        "{}.ashkelon-overlay-conflict",
        dest.file_name().and_then(|n| n.to_str()).unwrap_or("entry")
    );
    std::fs::rename(src, dest.with_file_name(conflict_name))?;
    Ok(())
}
