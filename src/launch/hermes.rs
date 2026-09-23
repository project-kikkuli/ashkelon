use super::LaunchPlan;

/// Hermes has exactly two provider-selection paths (`hermes_cli/runtime_provider.py`):
///
/// - the built-in `anthropic` provider, which resolves credentials via
///   `resolve_anthropic_token()` — env vars first, but falling back to the macOS Keychain entry
///   "Claude Code-credentials" (refreshing it if expired) ahead of `ANTHROPIC_API_KEY`. A custom
///   `base_url` on this provider changes only where the request goes, not which credential is
///   used, so routing it through the relay does not avoid the keychain read. Never used here.
/// - a named `providers:` config entry (`_get_named_custom_provider`), which never imports the
///   anthropic adapter and never touches the keychain — the safe path.
///
/// The named-provider path is only reachable via `config.yaml` under `HERMES_HOME`, and Hermes
/// has no config-only override — only the whole-home `HERMES_HOME` env var
/// (`hermes_constants.get_hermes_home`). Pointing that at a fresh temp directory would avoid the
/// keychain, but it would also silently start Hermes with none of the user's real skills,
/// plugins, or persona, which is a different, still-unacceptable surprise. So there is no safe
/// way to launch Hermes through ashkelon today, and `plan` says so instead of guessing.
pub fn plan(_relay_base: &str, _launch: &str, _args: &[String]) -> anyhow::Result<LaunchPlan> {
    anyhow::bail!(
        "hermes is not supported: its only provider path that avoids the macOS Keychain \"Claude Code-credentials\" \
         entry is a named `providers:` config-file entry, and the only way to point Hermes at an alternate config \
         is redirecting its entire HERMES_HOME (losing the user's real skills/plugins/persona), not a config-only \
         override; there is no mechanism that safely relays Hermes traffic without either risk"
    )
}
