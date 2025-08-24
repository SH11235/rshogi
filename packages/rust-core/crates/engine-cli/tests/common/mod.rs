//! Common test utilities for engine-cli tests

#![allow(dead_code)] // These utilities may be used by various test files

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

// Timeout constants for CI stability
pub const T_INIT: Duration = Duration::from_secs(5); // Initial setup timeout
pub const T_BESTMOVE: Duration = Duration::from_secs(3); // Bestmove receive timeout
pub const T_SEARCH: Duration = Duration::from_millis(300); // Search duration
pub const T_SHORT: Duration = Duration::from_millis(100); // Short wait

/// Spawn the engine process
pub fn spawn_engine() -> Child {
    Command::new(env!("CARGO_BIN_EXE_engine-cli"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("Failed to spawn engine")
}

/// Send a command to the engine
pub fn send_command(stdin: &mut ChildStdin, cmd: &str) {
    println!(">>> {cmd}");
    writeln!(stdin, "{cmd}").expect("Failed to write command");
    stdin.flush().expect("Failed to flush stdin");
}

/// Read lines until a specific prefix is found or timeout
///
/// Uses `starts_with` for strict matching to avoid false positives
/// (e.g., "bestmove " won't match "info string kind=bestmove_sent")
pub fn read_until_prefix(
    reader: &mut BufReader<&mut ChildStdout>,
    prefix: &str,
    timeout: Duration,
) -> Result<String, String> {
    let start = Instant::now();
    let mut buffer = String::new();

    while start.elapsed() < timeout {
        buffer.clear();
        match reader.read_line(&mut buffer) {
            Ok(0) => return Err("EOF reached".to_string()),
            Ok(_) => {
                let line = buffer.trim();
                if !line.is_empty() {
                    println!("<<< {line}");
                    if line.starts_with(prefix) {
                        return Ok(line.to_string());
                    }
                }
            }
            Err(e) => return Err(format!("Read error: {e}")),
        }
    }

    Err(format!("Timeout waiting for prefix: {prefix}"))
}

/// Initialize engine with USI protocol
pub fn initialize_engine(stdin: &mut ChildStdin, reader: &mut BufReader<&mut ChildStdout>) {
    // Send usi command
    send_command(stdin, "usi");

    // Wait for usiok
    read_until_prefix(reader, "usiok", T_INIT).expect("Failed to receive usiok");

    // Send isready
    send_command(stdin, "isready");

    // Wait for readyok
    read_until_prefix(reader, "readyok", T_INIT).expect("Failed to receive readyok");
}

/// Wait for bestmove with strict prefix matching
pub fn wait_for_bestmove(reader: &mut BufReader<&mut ChildStdout>) -> Result<String, String> {
    // Use "bestmove " with space to avoid matching info strings
    read_until_prefix(reader, "bestmove ", T_BESTMOVE)
}

/// Assert that a bestmove string is valid
pub fn assert_valid_bestmove(bestmove: &str) {
    assert!(bestmove.starts_with("bestmove "), "Invalid bestmove format: {bestmove}");

    let parts: Vec<&str> = bestmove.split_whitespace().collect();
    assert!(parts.len() >= 2, "Bestmove must contain at least a move: {bestmove}");

    let move_str = parts[1];
    assert!(
        move_str != "0000" || move_str == "resign",
        "Unexpected null move or resign: {bestmove}"
    );
}
