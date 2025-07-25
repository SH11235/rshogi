//! Test to verify ponder move output
//! Run with: cargo test -p engine-cli --test ponder_output_test -- --nocapture

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

#[test]
fn test_ponder_output_depth_3() {
    let mut engine = Command::new(env!("CARGO_BIN_EXE_engine-cli"))
        .env("RUST_LOG", "info")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn engine");

    let mut stdin = engine.stdin.take().expect("Failed to get stdin");
    let stdout = engine.stdout.take().expect("Failed to get stdout");
    let stderr = engine.stderr.take().expect("Failed to get stderr");

    // Capture stderr for logs
    let stderr_handle = thread::spawn(move || {
        let reader = BufReader::new(stderr);
        let mut logs = Vec::new();
        for line in reader.lines().flatten() {
            if line.contains("Best move:") {
                println!("LOG: {line}");
                logs.push(line);
            }
        }
        logs
    });

    // Capture stdout
    let stdout_handle = thread::spawn(move || {
        let reader = BufReader::new(stdout);
        let mut lines = Vec::new();
        for line in reader.lines().flatten() {
            if line.starts_with("bestmove") {
                println!("USI: {line}");
            }
            lines.push(line);
        }
        lines
    });

    // Initialize engine
    writeln!(stdin, "usi").unwrap();
    stdin.flush().unwrap();
    thread::sleep(Duration::from_millis(100));

    writeln!(stdin, "isready").unwrap();
    stdin.flush().unwrap();
    thread::sleep(Duration::from_millis(100));

    // Set position and search with depth 3
    writeln!(stdin, "position startpos").unwrap();
    stdin.flush().unwrap();

    println!("\n--- Testing with depth 3 ---");
    writeln!(stdin, "go depth 3").unwrap();
    stdin.flush().unwrap();

    // Wait for search to complete
    thread::sleep(Duration::from_millis(1000));

    // Quit
    writeln!(stdin, "quit").unwrap();
    stdin.flush().unwrap();
    drop(stdin);

    // Wait for engine to exit
    let _ = engine.wait();

    // Check results
    let lines = stdout_handle.join().unwrap();
    let logs = stderr_handle.join().unwrap();

    // Find bestmove line
    let bestmove_line = lines
        .iter()
        .find(|l| l.starts_with("bestmove"))
        .expect("Should have bestmove output");

    println!("Found bestmove: {bestmove_line}");

    // Check if we have ponder in logs
    let has_ponder_in_log = logs.iter().any(|l| l.contains("ponder: Some"));

    if has_ponder_in_log {
        println!("PV was long enough to extract ponder move");
        // Should have ponder in USI output
        assert!(
            bestmove_line.contains(" ponder "),
            "Expected ponder in USI output when PV has 2+ moves"
        );
    } else {
        println!("PV was too short for ponder move");
        // Should not have ponder in USI output
        assert!(
            !bestmove_line.contains(" ponder "),
            "Should not have ponder in USI output when PV is short"
        );
    }
}

#[test]
fn test_ponder_output_movetime() {
    let mut engine = Command::new(env!("CARGO_BIN_EXE_engine-cli"))
        .env("RUST_LOG", "info")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn engine");

    let mut stdin = engine.stdin.take().expect("Failed to get stdin");
    let stdout = engine.stdout.take().expect("Failed to get stdout");
    let stderr = engine.stderr.take().expect("Failed to get stderr");

    // Capture stderr for logs
    let stderr_handle = thread::spawn(move || {
        let reader = BufReader::new(stderr);
        let mut logs = Vec::new();
        for line in reader.lines().flatten() {
            if line.contains("Best move:") || line.contains("Search completed") {
                println!("LOG: {line}");
                logs.push(line);
            }
        }
        logs
    });

    // Capture stdout
    let stdout_handle = thread::spawn(move || {
        let reader = BufReader::new(stdout);
        let mut lines = Vec::new();
        for line in reader.lines().flatten() {
            if line.starts_with("bestmove") || line.starts_with("info") {
                println!("USI: {line}");
            }
            lines.push(line);
        }
        lines
    });

    // Initialize engine
    writeln!(stdin, "usi").unwrap();
    stdin.flush().unwrap();
    thread::sleep(Duration::from_millis(100));

    writeln!(stdin, "isready").unwrap();
    stdin.flush().unwrap();
    thread::sleep(Duration::from_millis(100));

    // Set position and search with longer time
    writeln!(stdin, "position startpos").unwrap();
    stdin.flush().unwrap();

    println!("\n--- Testing with movetime 500 ---");
    writeln!(stdin, "go movetime 500").unwrap();
    stdin.flush().unwrap();

    // Wait for search to complete
    thread::sleep(Duration::from_millis(700));

    // Quit
    writeln!(stdin, "quit").unwrap();
    stdin.flush().unwrap();
    drop(stdin);

    // Wait for engine to exit
    let _ = engine.wait();

    // Check results
    let lines = stdout_handle.join().unwrap();
    let logs = stderr_handle.join().unwrap();

    // Find bestmove line
    let bestmove_line = lines
        .iter()
        .find(|l| l.starts_with("bestmove"))
        .expect("Should have bestmove output");

    println!("Found bestmove: {bestmove_line}");

    // Check depth reached
    let depth_reached = logs
        .iter()
        .find(|l| l.contains("Search completed"))
        .and_then(|l| {
            l.split("depth:")
                .nth(1)
                .and_then(|s| s.split_whitespace().next())
                .and_then(|s| s.parse::<u32>().ok())
        })
        .unwrap_or(0);

    println!("Search reached depth: {depth_reached}");

    if depth_reached >= 2 {
        // With depth 2+, we might have ponder moves
        println!("Depth >= 2, checking for ponder moves");
    }
}
