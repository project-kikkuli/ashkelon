//! `ashkelon run` used to exit the whole process the instant the harness process exited,
//! aborting the relay's own response-forwarding task (`Tracker` in `src/relay/tracker.rs`)
//! mid-flight whenever the harness was satisfied and quit before that background task had
//! finished draining a slow upstream and writing the call's telemetry. A real repro (ollama
//! through `omp -p --model ...`) answered correctly every time but never showed up in the call
//! log. This spins up a fake upstream that finishes 300ms after its first bytes, and a fake
//! harness that reads only those first bytes and exits immediately — reproducing the shape of
//! the race deterministically instead of depending on a real harness's timing.
#![cfg(unix)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

const FIRST_CHUNK: &[u8] = b"OK-FIRST-CHUNK-1234\n";
const SECOND_CHUNK: &[u8] = b"OK-SECOND-CHUNK-5678\n";
const UPSTREAM_DELAY: Duration = Duration::from_millis(300);

/// A raw upstream that sends `FIRST_CHUNK` immediately, sleeps `UPSTREAM_DELAY`, then sends
/// `SECOND_CHUNK` and closes. Declares its true total `Content-Length` up front, so the relay's
/// body reader keeps waiting for the second chunk instead of considering the response complete
/// after the first.
fn spawn_slow_upstream() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let total_len = FIRST_CHUNK.len() + SECOND_CHUNK.len();
                let headers = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {total_len}\r\nConnection: close\r\n\r\n"
                );
                if stream.write_all(headers.as_bytes()).is_err() {
                    return;
                }
                if stream.write_all(FIRST_CHUNK).is_err() {
                    return;
                }
                let _ = stream.flush();
                std::thread::sleep(UPSTREAM_DELAY);
                let _ = stream.write_all(SECOND_CHUNK);
                let _ = stream.flush();
            });
        }
    });
    port
}

#[test]
fn exit_waits_for_the_background_forwarder_so_the_call_is_still_logged() {
    let dir = std::env::temp_dir().join(format!("ashkelon-run-drain-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let log_dir = dir.join("logs");
    std::fs::create_dir_all(&log_dir).unwrap();

    let upstream_port = spawn_slow_upstream();

    // Route the built-in "anthropic" name at the fake upstream instead of the real API, so the
    // fake harness below can pick it up from the same ANTHROPIC_BASE_URL env var `run` always
    // sets for the "claude" harness.
    let config_path = dir.join("config.toml");
    std::fs::write(
        &config_path,
        format!(
            "log_dir = {log_dir:?}\n[[routes]]\nname = \"anthropic\"\nupstream = \"http://127.0.0.1:{upstream_port}\"\n"
        ),
    )
    .unwrap();

    // Connects itself over `/dev/tcp` (bash builtin — no curl needed) and reads one byte at a
    // time until it has seen the first chunk's marker text, then exits immediately. `curl` was
    // tried first and rejected: piped through `head -c N`, curl still blocks on the network
    // read for the *whole* body before it ever notices the downstream pipe closed, so the
    // harness process never actually exits early — defeating the point of this test. Reading
    // byte-by-byte for a marker (rather than a fixed count) is robust to the relay's exact
    // header bytes, which differ from the raw upstream's.
    let fake_claude = dir.join("claude");
    let marker = std::str::from_utf8(FIRST_CHUNK).unwrap().trim_end();
    let mut script = std::fs::File::create(&fake_claude).unwrap();
    writeln!(script, "#!/bin/bash").unwrap();
    writeln!(script, "url=\"$ANTHROPIC_BASE_URL/v1/messages\"").unwrap();
    writeln!(script, "rest=\"${{url#http://}}\"").unwrap();
    writeln!(script, "host_port=\"${{rest%%/*}}\"").unwrap();
    writeln!(script, "path=\"/${{rest#*/}}\"").unwrap();
    writeln!(script, "host=\"${{host_port%%:*}}\"").unwrap();
    writeln!(script, "port=\"${{host_port##*:}}\"").unwrap();
    writeln!(script, "exec 3<>\"/dev/tcp/$host/$port\"").unwrap();
    writeln!(
        script,
        "printf 'GET %s HTTP/1.1\\r\\nHost: %s\\r\\nConnection: close\\r\\n\\r\\n' \"$path\" \"$host_port\" >&3"
    )
    .unwrap();
    writeln!(script, "buf=\"\"").unwrap();
    writeln!(script, "for ((i = 0; i < 4000; i++)); do").unwrap();
    writeln!(script, "  IFS= read -r -n 1 -t 2 ch <&3 || break").unwrap();
    writeln!(script, "  buf=\"$buf$ch\"").unwrap();
    writeln!(script, "  case \"$buf\" in *{marker}*) break ;; esac").unwrap();
    writeln!(script, "done").unwrap();
    writeln!(script, "exit 0").unwrap();
    drop(script);
    std::fs::set_permissions(&fake_claude, std::fs::Permissions::from_mode(0o755)).unwrap();

    let started = Instant::now();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ashkelon"))
        .args(["--config", config_path.to_str().unwrap(), "run", "claude", "--"])
        .env("PATH", &dir)
        .output()
        .expect("spawning ashkelon run");
    let elapsed = started.elapsed();

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // The harness itself was satisfied in milliseconds; `ashkelon run` exiting only after
    // ~UPSTREAM_DELAY proves it waited for the background forwarder rather than tearing the
    // process down the instant the harness quit.
    assert!(
        elapsed >= UPSTREAM_DELAY - Duration::from_millis(50),
        "ashkelon run exited after {elapsed:?}, expected it to wait out the slow upstream (~{UPSTREAM_DELAY:?})"
    );

    let mut records = Vec::new();
    for entry in std::fs::read_dir(&log_dir).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("calls-") && name.ends_with(".jsonl") {
            let text = std::fs::read_to_string(entry.path()).unwrap();
            for line in text.lines() {
                records.push(serde_json::from_str::<serde_json::Value>(line).unwrap());
            }
        }
    }

    assert_eq!(records.len(), 1, "expected exactly one call record, got {records:#?}");
    assert_eq!(
        records[0]["response_bytes"],
        (FIRST_CHUNK.len() + SECOND_CHUNK.len()) as u64,
        "the record should reflect the full (slow) upstream body, not just what the harness read: {:#?}",
        records[0]
    );

    let _ = std::fs::remove_dir_all(&dir);
}
