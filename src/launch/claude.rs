use anyhow::Context;

use super::{LaunchOptions, LaunchPlan};

/// `--remote-control` requires Anthropic's own subscription-gated bridge relay (see
/// wake-research notes), but Claude Code's MCP "channel" mechanism is a real local alternative:
/// an MCP server that declares `capabilities.experimental: {"claude/channel": {}}` may push
/// `notifications/claude/channel` at will, once admitted via
/// `--dangerously-load-development-channels server:<name>`. All three are verified directly from
/// `strings` on the installed binary:
///
/// - the gate text: `"is not on the approved channels allowlist (use \
///   --dangerously-load-development-channels for local dev)"`, `"server did not declare \
///   claude/channel capability"`, and the flag/help text itself;
/// - the `server:`/`plugin:` tag parser (`fe.startsWith("server:")` -> `{kind:"server",
///   name:fe.slice(7)}`), confirming the exact `server:ashkelon` syntax;
/// - the notification's own schema, `{method: R("notifications/claude/channel"), params:
///   u({content:o(), meta:pe(o(),o()).optional()})}` (minified zod-shaped object/literal/record
///   builders) — i.e. `{content: string, meta?: Record<string,string>}`, with no `source` field
///   (the client attributes a push to whichever MCP connection it arrived on).
///
/// `ashkelon channel` (see `wake::channel`) is that MCP server; Claude spawns it itself via
/// `--mcp-config`, so this plan only has to describe it, not run it — `wake` reaches it over a
/// unix socket at `<state_dir>/launch/<launch>.sock` (see `wake::claude`).
pub fn plan(relay_base: &str, launch: &str, args: &[String], options: &LaunchOptions) -> anyhow::Result<LaunchPlan> {
    let mut plan = LaunchPlan::new("claude", "claude");
    plan.env
        .push(("ANTHROPIC_BASE_URL".to_string(), format!("{relay_base}/anthropic")));

    let no_channel = options.no_channel || args.iter().any(|a| a == "--no-channel");
    let passthrough: Vec<String> = args.iter().filter(|a| a.as_str() != "--no-channel").cloned().collect();

    if no_channel {
        plan.args = passthrough;
        return Ok(plan);
    }

    let exe = std::env::current_exe().context("locating ashkelon's own executable path for --mcp-config")?;
    let socket_dir = options.state_dir.join("launch");
    crate::fsperm::create_dir_private(&socket_dir).context("creating the channel socket directory")?;
    let socket_path = socket_dir.join(format!("{launch}.sock"));

    let mcp_config = serde_json::json!({
        "mcpServers": {
            "ashkelon": {
                "command": exe.to_string_lossy(),
                "args": ["channel", "--socket", socket_path.to_string_lossy()],
            }
        }
    });

    plan.args = vec![
        "--mcp-config".to_string(),
        mcp_config.to_string(),
        "--dangerously-load-development-channels".to_string(),
        "server:ashkelon".to_string(),
    ];
    plan.args.extend(passthrough);
    plan.wake.control = Some(socket_path.to_string_lossy().into_owned());
    Ok(plan)
}
