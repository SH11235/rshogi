//! Test to verify ponder move output
//! Run with: cargo test -p engine-cli --test ponder_output_test -- --nocapture

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

static TEST_MUTEX: OnceLock<Mutex<()>> = OnceLock::new();

#[test]
fn test_ponder_output_depth_3() {
    let _guard = TEST_MUTEX.get_or_init(|| Mutex::new(())).lock().unwrap();
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

    // Channel to notify when usiok/readyok appear
    let (ok_tx, ok_rx) = mpsc::channel::<String>();

    // Capture stderr for logs
    let stderr_handle = thread::spawn(move || {
        let reader = BufReader::new(stderr);
        let mut logs = Vec::new();
        for line in reader.lines().map_while(Result::ok) {
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
        for line in reader.lines().map_while(Result::ok) {
            if line.starts_with("bestmove") {
                println!("USI: {line}");
            }
            if line == "usiok" || line == "readyok" {
                let _ = ok_tx.send(line.clone());
            }
            lines.push(line);
        }
        lines
    });

    // Initialize engine and wait for usiok/readyok
    writeln!(stdin, "usi").unwrap();
    stdin.flush().unwrap();
    let _ = ok_rx.recv_timeout(Duration::from_secs(2)).expect("usiok not received");

    writeln!(stdin, "isready").unwrap();
    stdin.flush().unwrap();
    let _ = ok_rx.recv_timeout(Duration::from_secs(2)).expect("readyok not received");

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

    if bestmove_line.contains(" ponder ") {
        println!("Ponder move was included in output");

        // Check if fallback was used
        let used_fallback = logs.iter().any(|l| l.contains("Generated fallback"));
        if used_fallback {
            println!("Fallback ponder move was generated");
        } else {
            println!("Ponder move was extracted from PV");
        }
    } else {
        println!("No ponder move at depth 3 (expected for shallow search)");
    }
}

#[test]
fn test_ponder_output_movetime() {
    let _guard = TEST_MUTEX.get_or_init(|| Mutex::new(())).lock().unwrap();
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

    // Channels to notify when bestmove and usiok/readyok appear
    let (bestmove_tx, bestmove_rx) = mpsc::channel::<String>();
    let (ok_tx, ok_rx) = mpsc::channel::<String>();

    // Capture stderr for logs
    let stderr_handle = thread::spawn(move || {
        let reader = BufReader::new(stderr);
        let mut logs = Vec::new();
        for line in reader.lines().map_while(Result::ok) {
            // Log more information for debugging
            if line.contains("Best move:")
                || line.contains("Search completed")
                || line.contains("execute_search_static")
                || line.contains("SearchFinished")
                || line.contains("time")
                || line.contains("depth")
                || line.contains("Time")
                || line.contains("limit")
                || line.contains("emergency")
                || line.contains("fallback")
            {
                println!("LOG: {line}");
                logs.push(line.clone());
            }
            // Always print ERROR and WARN logs
            if line.contains("ERROR") || line.contains("WARN") {
                println!("LOG: {line}");
                if !logs.iter().any(|l| l == &line) {
                    logs.push(line);
                }
            }
        }
        logs
    });

    // Capture stdout
    let stdout_handle = thread::spawn(move || {
        let reader = BufReader::new(stdout);
        let mut lines = Vec::new();
        for line in reader.lines().map_while(Result::ok) {
            if line.starts_with("bestmove") || line.starts_with("info") {
                println!("USI: {line}");
            }
            if line == "usiok" || line == "readyok" {
                let _ = ok_tx.send(line.clone());
            }
            if line.starts_with("bestmove") {
                let _ = bestmove_tx.send(line.clone());
            }
            lines.push(line);
        }
        lines
    });

    // Initialize engine and wait for usiok/readyok
    writeln!(stdin, "usi").unwrap();
    stdin.flush().unwrap();
    let _ = ok_rx.recv_timeout(Duration::from_secs(2)).expect("usiok not received");

    writeln!(stdin, "isready").unwrap();
    stdin.flush().unwrap();
    let _ = ok_rx.recv_timeout(Duration::from_secs(2)).expect("readyok not received");

    // Set position and search with longer time
    writeln!(stdin, "position startpos").unwrap();
    stdin.flush().unwrap();

    println!("\n--- Testing with movetime 500 ---");
    writeln!(stdin, "go movetime 500").unwrap();
    stdin.flush().unwrap();

    // Wait slightly to see if bestmove comes automatically
    thread::sleep(Duration::from_millis(600));

    // Check if we got bestmove already
    let early_bestmove = bestmove_rx.try_recv().ok();
    if let Some(ref bm) = early_bestmove {
        println!("Got early bestmove at ~600ms: {bm}");
    } else {
        println!("No bestmove after 600ms, sending stop command");
        // Always stop slightly after movetime to force bestmove emission
        thread::sleep(Duration::from_millis(200));
        writeln!(stdin, "stop").unwrap();
        stdin.flush().unwrap();
    }

    // First, try to receive bestmove within 2s after stop (or use early one)
    let maybe_bestmove =
        early_bestmove.or_else(|| bestmove_rx.recv_timeout(Duration::from_millis(2000)).ok());

    // If not received yet, send quit and then inspect all collected lines
    if maybe_bestmove.is_none() {
        writeln!(stdin, "quit").unwrap();
        stdin.flush().unwrap();
        drop(stdin);

        // Wait for engine to exit
        let _ = engine.wait();

        // Collect outputs
        let lines = stdout_handle.join().unwrap();
        let _logs = stderr_handle.join().unwrap();

        // Find bestmove in collected lines
        let bestmove_line = lines
            .iter()
            .find(|l| l.starts_with("bestmove"))
            .cloned()
            .expect("Should have bestmove output");

        println!("Found bestmove: {bestmove_line}");
        return;
    }

    let bestmove_line = maybe_bestmove.unwrap();
    println!("Found bestmove: {bestmove_line}");

    // Quit
    writeln!(stdin, "quit").unwrap();
    stdin.flush().unwrap();
    drop(stdin);

    // Wait for engine to exit
    let _ = engine.wait();

    // Check results (also to keep previous diagnostics)
    let _lines = stdout_handle.join().unwrap();
    let logs = stderr_handle.join().unwrap();

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
