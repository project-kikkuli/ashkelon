use std::path::Path;

use ashkelon::config::{HookConfig, HookEvent};
use ashkelon::hooks::matching::{expand_tilde, matches};

fn hook(on: Vec<HookEvent>, harnesses: Vec<&str>, projects: Vec<&str>) -> HookConfig {
    HookConfig {
        name: "h".into(),
        on,
        command: vec!["true".into()],
        projects: projects.into_iter().map(String::from).collect(),
        harnesses: harnesses.into_iter().map(String::from).collect(),
        timeout_secs: 5,
    }
}

#[test]
fn event_must_be_in_on_list() {
    let h = hook(vec![HookEvent::Prompt], vec![], vec![]);
    assert!(matches(&h, HookEvent::Prompt, None, None));
    assert!(!matches(&h, HookEvent::ToolCall, None, None));
}

#[test]
fn empty_harnesses_matches_any() {
    let h = hook(vec![HookEvent::Prompt], vec![], vec![]);
    assert!(matches(&h, HookEvent::Prompt, Some("claude"), None));
    assert!(matches(&h, HookEvent::Prompt, None, None));
}

#[test]
fn nonempty_harnesses_requires_match() {
    let h = hook(vec![HookEvent::Prompt], vec!["claude"], vec![]);
    assert!(matches(&h, HookEvent::Prompt, Some("claude"), None));
    assert!(!matches(&h, HookEvent::Prompt, Some("codex"), None));
    assert!(!matches(&h, HookEvent::Prompt, None, None));
}

#[test]
fn nonempty_projects_requires_cwd_under_one() {
    let h = hook(vec![HookEvent::Prompt], vec![], vec!["/work/one"]);
    assert!(matches(&h, HookEvent::Prompt, None, Some(Path::new("/work/one/sub"))));
    assert!(!matches(&h, HookEvent::Prompt, None, Some(Path::new("/work/two"))));
    assert!(!matches(&h, HookEvent::Prompt, None, None));
}

#[test]
fn empty_projects_matches_sessions_with_no_cwd() {
    let h = hook(vec![HookEvent::Prompt], vec![], vec![]);
    assert!(matches(&h, HookEvent::Prompt, None, None));
}

#[test]
fn both_harness_and_project_constraints_must_hold() {
    let h = hook(vec![HookEvent::Prompt], vec!["claude"], vec!["/work/one"]);
    assert!(matches(
        &h,
        HookEvent::Prompt,
        Some("claude"),
        Some(Path::new("/work/one"))
    ));
    assert!(!matches(
        &h,
        HookEvent::Prompt,
        Some("codex"),
        Some(Path::new("/work/one"))
    ));
    assert!(!matches(
        &h,
        HookEvent::Prompt,
        Some("claude"),
        Some(Path::new("/work/two"))
    ));
}

#[test]
fn tilde_expands_to_home() {
    let home = dirs::home_dir().unwrap();
    assert_eq!(expand_tilde("~/proj"), home.join("proj"));
    assert_eq!(expand_tilde("~"), home);
    assert_eq!(expand_tilde("/abs/path"), Path::new("/abs/path"));
}
