use crate::config::Config;

/// A resolved `/s/<launch>/<route>/<rest>` or `/<route>/<rest>` request path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    pub launch: Option<String>,
    pub route: String,
    /// Upstream base, e.g. `https://api.anthropic.com` (no trailing slash).
    pub upstream: String,
    /// Path forwarded to the upstream, always starting with `/`.
    pub rest: String,
}

fn builtin_upstream(name: &str) -> Option<&'static str> {
    match name {
        "anthropic" => Some("https://api.anthropic.com"),
        "openai" => Some("https://api.openai.com"),
        "chatgpt" => Some("https://chatgpt.com"),
        "openrouter" => Some("https://openrouter.ai"),
        "opencode" => Some("https://opencode.ai"),
        _ => None,
    }
}

fn lookup(route: &str, cfg: &Config) -> Option<String> {
    if let Some(rc) = cfg.routes.iter().find(|r| r.name == route) {
        return Some(rc.upstream.trim_end_matches('/').to_string());
    }
    builtin_upstream(route).map(str::to_string)
}

fn rest_of(remainder: Option<&str>) -> String {
    match remainder {
        Some(r) if !r.is_empty() => format!("/{r}"),
        _ => "/".to_string(),
    }
}

/// Resolves a request path (no query string) against the built-in and configured routes.
pub fn resolve(path: &str, cfg: &Config) -> Option<Parsed> {
    let trimmed = path.trim_start_matches('/');
    let mut top = trimmed.splitn(2, '/');
    let first = top.next().unwrap_or("");
    let after_first = top.next();

    if first == "s" {
        let mut launch_and_rest = after_first.unwrap_or("").splitn(2, '/');
        let launch = launch_and_rest.next().unwrap_or("").to_string();
        let mut route_and_rest = launch_and_rest.next().unwrap_or("").splitn(2, '/');
        let route = route_and_rest.next().unwrap_or("").to_string();
        let rest = rest_of(route_and_rest.next());
        let upstream = lookup(&route, cfg)?;
        Some(Parsed { launch: Some(launch), route, upstream, rest })
    } else {
        let route = first.to_string();
        let rest = rest_of(after_first);
        let upstream = lookup(&route, cfg)?;
        Some(Parsed { launch: None, route, upstream, rest })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RouteConfig;

    fn cfg_with_route(name: &str, upstream: &str) -> Config {
        let mut cfg = Config::default();
        cfg.routes.push(RouteConfig { name: name.to_string(), upstream: upstream.to_string() });
        cfg
    }

    #[test]
    fn resolves_builtin_route_without_launch() {
        let cfg = Config::default();
        let parsed = resolve("/anthropic/v1/messages", &cfg).unwrap();
        assert_eq!(parsed.launch, None);
        assert_eq!(parsed.route, "anthropic");
        assert_eq!(parsed.upstream, "https://api.anthropic.com");
        assert_eq!(parsed.rest, "/v1/messages");
    }

    #[test]
    fn resolves_launch_prefixed_route() {
        let cfg = Config::default();
        let parsed = resolve("/s/launch-1/openai/v1/responses", &cfg).unwrap();
        assert_eq!(parsed.launch, Some("launch-1".to_string()));
        assert_eq!(parsed.route, "openai");
        assert_eq!(parsed.upstream, "https://api.openai.com");
        assert_eq!(parsed.rest, "/v1/responses");
    }

    #[test]
    fn resolves_configured_route() {
        let cfg = cfg_with_route("fake", "http://127.0.0.1:9999");
        let parsed = resolve("/fake/v1/messages", &cfg).unwrap();
        assert_eq!(parsed.upstream, "http://127.0.0.1:9999");
        assert_eq!(parsed.rest, "/v1/messages");
    }

    #[test]
    fn configured_route_overrides_builtin_name() {
        let cfg = cfg_with_route("anthropic", "http://127.0.0.1:1234");
        let parsed = resolve("/anthropic/v1/messages", &cfg).unwrap();
        assert_eq!(parsed.upstream, "http://127.0.0.1:1234");
    }

    #[test]
    fn unknown_route_is_none() {
        let cfg = Config::default();
        assert!(resolve("/not-a-route/x", &cfg).is_none());
    }

    #[test]
    fn route_with_no_rest_defaults_to_root() {
        let cfg = Config::default();
        let parsed = resolve("/anthropic", &cfg).unwrap();
        assert_eq!(parsed.rest, "/");
    }
}
