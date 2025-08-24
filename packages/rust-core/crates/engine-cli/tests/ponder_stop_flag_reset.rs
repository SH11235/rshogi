//! Regression tests for ponder stop flag reset behavior

mod common;

use common::*;
use regex::Regex;
use std::io::{BufRead, BufReader};
use std::thread;

#[test]
fn test_ponder_natural_completion_then_new_search() {
    // This test verifies that after a ponder search completes naturally,
    // a new search can be started successfully with a fresh stop flag.
    // The test checks for incrementing search IDs to confirm proper cleanup.

    let (mut child, stderr) = spawn_engine_with_stderr();
    let stdin = child.stdin.as_mut().expect("Failed to get stdin");
    let stdout = child.stdout.as_mut().expect("Failed to get stdout");
    let _stdout_drain = thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for _ in reader.lines().map_while(Result::ok) {
            // stdout は本テストで使わないので捨てる
        }
    });
    let mut reader = BufReader::new(stdout);

    // Initialize engine with proper handshake
    initialize_engine(stdin, &mut reader);

    // Collect stderr in background thread
    let stderr_handle = thread::spawn(move || {
        let reader = BufReader::new(stderr);
        reader.lines().collect::<Result<Vec<_>, _>>().unwrap_or_default()
    });

    send_command(stdin, "position startpos");
    send_command(stdin, "go ponder depth 1"); // Short ponder that completes naturally
    thread::sleep(T_SEARCH); // Wait for natural completion

    send_command(stdin, "go depth 1"); // New search after ponder
    thread::sleep(T_SHORT);

    send_command(stdin, "quit");

    // Wait for completion
    let _ = child.wait();

    // Analyze logs
    let stderr_lines = stderr_handle.join().expect("Failed to join stderr thread");

    let mut ponder_search_id = 0u64;
    let mut new_search_id = 0u64;

    // Use regex for robust log parsing
    let search_re =
        Regex::new(r"Starting new search with ID:\s*(\d+),\s*ponder:\s*(true|false)").unwrap();

    for line in &stderr_lines {
        if let Some(captures) = search_re.captures(line) {
            let id: u64 = captures[1].parse().unwrap_or(0);
            let is_ponder = &captures[2] == "true";

            if is_ponder {
                ponder_search_id = id;
            } else {
                new_search_id = id;
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
    let crit_re = Regex::new(r"CRITICAL:\s*stop_flag is true at search start").unwrap();
    assert!(
        !stderr_lines.iter().any(|l| crit_re.is_match(l)),
        "Regression: stop_flag was true at new search start (see stderr)"
    );
}

#[test]
fn test_ponder_stop_then_new_search() {
    // This test verifies that after stopping a ponder search with the stop command,
    // a new search can be started successfully with a fresh stop flag.
    // The test confirms proper cleanup by checking for the "Stop during ponder" log message
    // and incrementing search IDs.

    let (mut child, stderr) = spawn_engine_with_stderr();
    let stdin = child.stdin.as_mut().expect("Failed to get stdin");
    let stdout = child.stdout.as_mut().expect("Failed to get stdout");
    let mut reader = BufReader::new(stdout);

    // Initialize engine with proper handshake
    initialize_engine(stdin, &mut reader);

    // Collect stderr in background thread
    let stderr_handle = thread::spawn(move || {
        let reader = BufReader::new(stderr);
        reader.lines().collect::<Result<Vec<_>, _>>().unwrap_or_default()
    });

    send_command(stdin, "position startpos");
    send_command(stdin, "go ponder depth 10"); // Long ponder that we'll stop
    thread::sleep(T_SHORT); // Let it start

    send_command(stdin, "stop"); // Stop the ponder
    thread::sleep(T_SEARCH); // Wait for stop to process

    // Wait for bestmove after stop (USI spec compliance check)
    let bestmove = wait_for_bestmove(&mut reader)
        .expect("Should receive bestmove after stop during ponder per USI spec");
    assert_valid_bestmove(&bestmove);

    send_command(stdin, "go depth 1"); // New search after stop
    thread::sleep(T_SHORT);

    send_command(stdin, "quit");

    // Wait for completion
    let _ = child.wait();

    // Analyze logs
    let stderr_lines = stderr_handle.join().expect("Failed to join stderr thread");

    let mut found_stop_during_ponder = false;
    let mut ponder_search_id = 0u64;
    let mut new_search_id = 0u64;

    // Use regex for robust log parsing
    let search_re =
        Regex::new(r"Starting new search with ID:\s*(\d+),\s*ponder:\s*(true|false)").unwrap();
    let stop_ponder_re = Regex::new(r"Stop during ponder.*search_id:\s*(\d+)").unwrap();

    for line in &stderr_lines {
        // Check for stop during ponder
        if let Some(captures) = stop_ponder_re.captures(line) {
            found_stop_during_ponder = true;
            ponder_search_id = captures[1].parse().unwrap_or(0);
        }

        // Extract search IDs
        if let Some(captures) = search_re.captures(line) {
            let id: u64 = captures[1].parse().unwrap_or(0);
            let is_ponder = &captures[2] == "true";

            if is_ponder && ponder_search_id == 0 {
                ponder_search_id = id;
            } else if !is_ponder && found_stop_during_ponder {
                new_search_id = id;
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
    let crit_re = Regex::new(r"CRITICAL:\s*stop_flag is true at search start").unwrap();
    assert!(
        !stderr_lines.iter().any(|l| crit_re.is_match(l)),
        "Regression: stop_flag was true at new search start (see stderr)"
    );
}
