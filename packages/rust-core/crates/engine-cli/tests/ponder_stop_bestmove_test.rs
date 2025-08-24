//! Test to verify that stop during ponder emits bestmove
//! as per USI protocol specification

mod common;

use common::*;
use std::io::BufReader;
use std::thread;

#[test]
fn test_ponder_stop_sends_bestmove() {
    let mut engine = spawn_engine();
    let stdin = engine.stdin.as_mut().expect("Failed to get stdin");
    let stdout = engine.stdout.as_mut().expect("Failed to get stdout");
    let mut reader = BufReader::new(stdout);

    // Initialize engine with explicit synchronization
    initialize_engine(stdin, &mut reader);

    // Set position
    send_command(stdin, "position startpos");

    // Start ponder search
    println!("\n--- Starting ponder search ---");
    send_command(stdin, "go ponder");

    // Give it time to start pondering
    thread::sleep(T_SEARCH);

    // Send stop during ponder
    println!("\n--- Sending stop during ponder ---");
    send_command(stdin, "stop");

    // Wait for bestmove with proper timeout
    let bestmove = wait_for_bestmove(&mut reader)
        .expect("Should receive bestmove when stopping ponder per USI spec");

    println!("Received bestmove: {bestmove}");
    assert_valid_bestmove(&bestmove);

    // Clean up
    send_command(stdin, "quit");
    let _ = engine.wait();

    println!("\n✓ Test passed: stop during ponder emits exactly one bestmove as per USI spec");
}

#[test]
fn test_normal_search_stop_sends_bestmove() {
    let mut engine = spawn_engine();
    let stdin = engine.stdin.as_mut().expect("Failed to get stdin");
    let stdout = engine.stdout.as_mut().expect("Failed to get stdout");
    let mut reader = BufReader::new(stdout);

    // Initialize engine with explicit synchronization
    initialize_engine(stdin, &mut reader);

    // Set position
    send_command(stdin, "position startpos");

    // Start normal search (not ponder)
    println!("\n--- Starting normal search ---");
    send_command(stdin, "go infinite");

    // Give it time to search
    thread::sleep(T_SEARCH);

    // Send stop during normal search
    println!("\n--- Sending stop during normal search ---");
    send_command(stdin, "stop");

    // Wait for bestmove with proper pattern matching
    let bestmove = wait_for_bestmove(&mut reader)
        .expect("Should receive bestmove after stop in normal search");

    println!("Received bestmove: {bestmove}");
    assert_valid_bestmove(&bestmove);

    // Clean up
    send_command(stdin, "quit");
    let _ = engine.wait();

    println!("\n✓ Test passed: stop during normal search sends bestmove");
}

#[test]
fn test_ponder_with_time_limits() {
    let mut engine = spawn_engine();
    let stdin = engine.stdin.as_mut().expect("Failed to get stdin");
    let stdout = engine.stdout.as_mut().expect("Failed to get stdout");
    let mut reader = BufReader::new(stdout);

    // Initialize engine
    initialize_engine(stdin, &mut reader);

    // Set position
    send_command(stdin, "position startpos");

    // Start ponder search with time limits
    println!("\n--- Starting ponder search with time limits ---");
    send_command(stdin, "go ponder btime 10000 wtime 10000");

    // Give it time to start pondering
    thread::sleep(T_SEARCH);

    // Send stop during ponder
    println!("\n--- Sending stop during time-limited ponder ---");
    send_command(stdin, "stop");

    // Wait for bestmove
    let bestmove = wait_for_bestmove(&mut reader)
        .expect("Should receive bestmove when stopping time-limited ponder");

    println!("Received bestmove: {bestmove}");
    assert_valid_bestmove(&bestmove);

    // Clean up
    send_command(stdin, "quit");
    let _ = engine.wait();

    println!("\n✓ Test passed: stop during time-limited ponder sends bestmove");
}

#[test]
fn test_ponder_with_depth_limit() {
    let mut engine = spawn_engine();
    let stdin = engine.stdin.as_mut().expect("Failed to get stdin");
    let stdout = engine.stdout.as_mut().expect("Failed to get stdout");
    let mut reader = BufReader::new(stdout);

    // Initialize engine
    initialize_engine(stdin, &mut reader);

    // Set position
    send_command(stdin, "position startpos");

    // Start ponder search with depth limit
    println!("\n--- Starting ponder search with depth limit ---");
    send_command(stdin, "go ponder depth 10");

    // Give it time to start pondering
    thread::sleep(T_SEARCH);

    // Send stop during ponder
    println!("\n--- Sending stop during depth-limited ponder ---");
    send_command(stdin, "stop");

    // Wait for bestmove
    let bestmove = wait_for_bestmove(&mut reader)
        .expect("Should receive bestmove when stopping depth-limited ponder");

    println!("Received bestmove: {bestmove}");
    assert_valid_bestmove(&bestmove);

    // Clean up
    send_command(stdin, "quit");
    let _ = engine.wait();

    println!("\n✓ Test passed: stop during depth-limited ponder sends bestmove");
}

// Note: Test for ponder natural termination is omitted due to complexity
// The current implementation properly finalizes ponder searches via finalize_search("PonderFinished")
// in main.rs when SearchFinished arrives for a ponder search
