use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

#[test]
fn test_ponder_natural_completion_then_new_search() {
    // This test verifies that after a ponder search completes naturally,
    // a new search can be started successfully with a fresh stop flag.
    // The test checks for incrementing search IDs to confirm proper cleanup.

    let mut child = Command::new(env!("CARGO_BIN_EXE_engine-cli"))
        .env("RUST_LOG", "info")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn engine");

    let mut stdin = child.stdin.take().expect("Failed to get stdin");
    let stdout = child.stdout.take().expect("Failed to get stdout");
    let stderr = child.stderr.take().expect("Failed to get stderr");

    // Collect all output in background threads
    let stdout_handle = std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        reader.lines().collect::<Result<Vec<_>, _>>().unwrap_or_default()
    });

    let stderr_handle = std::thread::spawn(move || {
        let reader = BufReader::new(stderr);
        reader.lines().collect::<Result<Vec<_>, _>>().unwrap_or_default()
    });

    // Send commands
    writeln!(stdin, "usi").unwrap();
    writeln!(stdin, "isready").unwrap();
    std::thread::sleep(Duration::from_millis(100));

    writeln!(stdin, "position startpos").unwrap();
    writeln!(stdin, "go ponder depth 1").unwrap(); // Short ponder that completes naturally
    std::thread::sleep(Duration::from_millis(500)); // Wait for natural completion

    writeln!(stdin, "go depth 1").unwrap(); // New search after ponder
    std::thread::sleep(Duration::from_millis(100));

    writeln!(stdin, "quit").unwrap();
    stdin.flush().unwrap();
    drop(stdin);

    // Wait for completion
    let _ = child.wait();

    // Analyze logs
    let stderr_lines = stderr_handle.join().expect("Failed to join stderr thread");
    let _stdout_lines = stdout_handle.join().expect("Failed to join stdout thread");

    let mut ponder_search_id = 0u64;
    let mut new_search_id = 0u64;

    for line in &stderr_lines {
        // Extract ponder search ID
        if line.contains("Starting new search with ID:") && line.contains("ponder: true") {
            if let Some(id_str) = line.split("ID: ").nth(1) {
                if let Some(id_part) = id_str.split(',').next() {
                    ponder_search_id = id_part.parse().unwrap_or(0);
                }
            }
        }

        // Extract new search ID
        if line.contains("Starting new search with ID:") && line.contains("ponder: false") {
            if let Some(id_str) = line.split("ID: ").nth(1) {
                if let Some(id_part) = id_str.split(',').next() {
                    new_search_id = id_part.parse().unwrap_or(0);
                }
            }
        }
    }

    assert!(ponder_search_id > 0, "Should have started a ponder search");
    assert!(new_search_id > 0, "Should have started a new search after ponder");
    assert!(
        new_search_id > ponder_search_id,
        "New search ID ({}) should be greater than ponder search ID ({}), indicating proper cleanup",
        new_search_id,
        ponder_search_id
    );
}

#[test]
fn test_ponder_stop_then_new_search() {
    // This test verifies that after stopping a ponder search with the stop command,
    // a new search can be started successfully with a fresh stop flag.
    // The test confirms proper cleanup by checking for the "Stop during ponder" log message
    // and incrementing search IDs.

    let mut child = Command::new(env!("CARGO_BIN_EXE_engine-cli"))
        .env("RUST_LOG", "info")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn engine");

    let mut stdin = child.stdin.take().expect("Failed to get stdin");
    let stdout = child.stdout.take().expect("Failed to get stdout");
    let stderr = child.stderr.take().expect("Failed to get stderr");

    // Collect all output in background threads
    let stdout_handle = std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        reader.lines().collect::<Result<Vec<_>, _>>().unwrap_or_default()
    });

    let stderr_handle = std::thread::spawn(move || {
        let reader = BufReader::new(stderr);
        reader.lines().collect::<Result<Vec<_>, _>>().unwrap_or_default()
    });

    // Send commands
    writeln!(stdin, "usi").unwrap();
    writeln!(stdin, "isready").unwrap();
    std::thread::sleep(Duration::from_millis(100));

    writeln!(stdin, "position startpos").unwrap();
    writeln!(stdin, "go ponder depth 10").unwrap(); // Long ponder that we'll stop
    std::thread::sleep(Duration::from_millis(100)); // Let it start

    writeln!(stdin, "stop").unwrap(); // Stop the ponder
    std::thread::sleep(Duration::from_millis(200)); // Wait for stop to process

    writeln!(stdin, "go depth 1").unwrap(); // New search after stop
    std::thread::sleep(Duration::from_millis(100));

    writeln!(stdin, "quit").unwrap();
    stdin.flush().unwrap();
    drop(stdin);

    // Wait for completion
    let _ = child.wait();

    // Analyze logs
    let stderr_lines = stderr_handle.join().expect("Failed to join stderr thread");
    let _stdout_lines = stdout_handle.join().expect("Failed to join stdout thread");

    let mut found_stop_during_ponder = false;
    let mut ponder_search_id = 0u64;
    let mut new_search_id = 0u64;

    for line in &stderr_lines {
        // Check for stop during ponder
        if line.contains("Stop during ponder") && line.contains("will send bestmove per USI spec") {
            found_stop_during_ponder = true;
            // Extract search ID from the log
            if let Some(id_str) = line.split("search_id: ").nth(1) {
                if let Some(id_part) = id_str.split(')').next() {
                    ponder_search_id = id_part.parse().unwrap_or(0);
                }
            }
        }

        // Extract new search ID (only after finding stop during ponder)
        if found_stop_during_ponder
            && line.contains("Starting new search with ID:")
            && line.contains("ponder: false")
        {
            if let Some(id_str) = line.split("ID: ").nth(1) {
                if let Some(id_part) = id_str.split(',').next() {
                    new_search_id = id_part.parse().unwrap_or(0);
                }
            }
        }
    }

    assert!(found_stop_during_ponder, "Should see 'Stop during ponder' log message");
    assert!(ponder_search_id > 0, "Should have extracted ponder search ID");
    assert!(new_search_id > 0, "Should have started a new search after stop");
    assert!(
        new_search_id > ponder_search_id,
        "New search ID ({}) should be greater than ponder search ID ({}), indicating proper cleanup",
        new_search_id,
        ponder_search_id
    );
}
