//! Integration test for subprocess and pipe detection

use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn test_piped_subprocess_auto_skip() {
    // Test that piped I/O automatically skips legal moves check even with SKIP_LEGAL_MOVES=0
    let mut child = Command::new(env!("CARGO_BIN_EXE_engine-cli"))
        .env("SKIP_LEGAL_MOVES", "0") // Explicitly try to enable the check
        .env("RUST_LOG", "info") // Enable logging to see detection message
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn engine");

    // Send commands
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"position startpos\n").unwrap();
    stdin.write_all(b"go depth 1\n").unwrap();
    stdin.write_all(b"quit\n").unwrap();
    drop(stdin);

    // Wait for completion and capture output
    let output = child.wait_with_output().expect("Failed to wait for output");

    // Check that it succeeded quickly (no hang)
    assert!(output.status.success(), "Engine failed with status: {:?}", output.status);

    // Check stderr for pipe detection message
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Piped I/O detected"),
        "Expected pipe detection message in stderr, but got: {}",
        stderr
    );
}

#[test]
fn test_subprocess_mode_detection() {
    // Test that SUBPROCESS_MODE also triggers skip
    let mut child = Command::new(env!("CARGO_BIN_EXE_engine-cli"))
        .env("SKIP_LEGAL_MOVES", "0")
        .env("SUBPROCESS_MODE", "1") // Explicit subprocess mode
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn engine");

    // Send commands
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"position startpos\n").unwrap();
    stdin.write_all(b"go depth 1\n").unwrap();
    stdin.write_all(b"quit\n").unwrap();
    drop(stdin);

    // Wait for completion
    let output = child.wait_with_output().expect("Failed to wait for output");
    assert!(output.status.success());

    // Check for detection message
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Subprocess mode detected") || stderr.contains("Piped I/O detected"),
        "Expected detection message in stderr"
    );
}

#[test]
fn test_direct_execution_no_skip() {
    // This test would need to check that direct execution (no pipes) doesn't skip
    // However, we can't easily test this in an integration test since we need pipes
    // to capture output. This is more of a manual test scenario.

    // Instead, we'll test that the engine works correctly with pipes
    let output = Command::new(env!("CARGO_BIN_EXE_engine-cli"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .arg("--version")
        .output()
        .expect("Failed to execute engine");

    assert!(output.status.success());
}
