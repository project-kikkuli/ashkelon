//! Smoke test for `ashkelon run <harness> -- <args>`: a fake "claude" script stands in for the
//! real harness (found by putting a tiny temp dir first on PATH), and only inspects its own
//! environment/exit code — the relay is still a stub in this branch, so nothing asserts on an
//! actual HTTP response.
#![cfg(unix)]

use std::io::Write;
use std::os::unix::fs::PermissionsExt;

#[test]
fn run_sets_anthropic_base_url_and_propagates_exit_code() {
    let dir = std::env::temp_dir().join(format!("ashkelon-run-smoke-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out_file = dir.join("captured-base-url");
    let fake_claude = dir.join("claude");

    let mut script = std::fs::File::create(&fake_claude).unwrap();
    writeln!(script, "#!/bin/sh").unwrap();
    writeln!(script, "printf '%s' \"$ANTHROPIC_BASE_URL\" > \"$OUT_FILE\"").unwrap();
    writeln!(script, "exit 7").unwrap();
    drop(script);
    std::fs::set_permissions(&fake_claude, std::fs::Permissions::from_mode(0o755)).unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ashkelon"))
        .args(["run", "claude", "--"])
        .env("PATH", &dir)
        .env("OUT_FILE", &out_file)
        .output()
        .expect("spawning ashkelon run");

    assert_eq!(
        output.status.code(),
        Some(7),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let captured = std::fs::read_to_string(&out_file).expect("fake claude should have written the base url");
    assert!(captured.starts_with("http://127.0.0.1:"), "captured: {captured}");
    assert!(captured.ends_with("/anthropic"), "captured: {captured}");
    assert!(captured.contains("/s/"), "captured: {captured}");

    let _ = std::fs::remove_dir_all(&dir);
}
