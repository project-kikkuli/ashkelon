//! Runs the real `ashkelon demo` subcommand as a subprocess and asserts each of its six steps
//! actually happened, from its own stdout — the same walkthrough a person would read.

#[test]
fn demo_runs_every_step_and_reports_success() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ashkelon"))
        .args(["demo"])
        .output()
        .expect("spawning ashkelon demo");

    assert!(
        output.status.success(),
        "ashkelon demo exited non-zero.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Step 1: the call-log line, with real fields off the actual telemetry record.
    assert!(stdout.contains("step 1: an ordinary turn"));
    assert!(stdout.contains("call log:"));
    assert!(stdout.contains("status=200"));
    assert!(stdout.contains(r#"wire="anthropic_messages""#));
    assert!(stdout.contains("turn_end=true"));
    assert!(stdout.contains("reasoning_tokens"));

    // Step 2: the hook failing in the background.
    assert!(stdout.contains("step 2: the demo hook fails"));
    assert!(stdout.contains(r#""status":"fail""#));
    assert!(!stdout.contains("never reported fail"));

    // Step 3: the ping injected into the next request, with its actual text shown.
    assert!(stdout.contains("step 3: the ping is injected"));
    assert!(stdout.contains("<ashkelon-ping hook=\"demo-hook\""));
    assert!(stdout.contains("Demo hook: this always fails on the first turn."));
    assert!(stdout.contains("</ashkelon-ping>"));
    assert!(!stdout.contains("no ping text found"));

    // Step 4: the hook passing and the ping being cleared from a later request.
    assert!(stdout.contains("step 4: the hook passes"));
    assert!(stdout.contains(r#""status":"pass""#));
    assert!(stdout.contains("third request carries a ping: false"));

    // Step 5: a rule cutting the stream — the forbidden trailing text never arrives.
    assert!(stdout.contains("step 5: a rule cuts"));
    assert!(stdout.contains("ashkelon_rule"));
    assert!(stdout.contains("leaked_secret"));
    assert!(stdout.contains("trailing sentence reached the agent: false"));
    assert!(!stdout.contains("trailing sentence that must never arrive either"));

    // Step 6: a large tool output trimmed before the provider ever saw it.
    assert!(stdout.contains("step 6: a large tool output is trimmed"));
    assert!(stdout.contains("tool_result sent by the agent: 500 chars"));
    assert!(stdout.contains("[ashkelon: trimmed"));

    assert!(stdout.contains("demo complete."));
}
