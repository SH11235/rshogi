//! Test to ensure bestmove is sent exactly once even in race conditions

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

fn spawn_engine() -> std::process::Child {
    Command::new(env!("CARGO_BIN_EXE_engine-cli"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("Failed to spawn engine")
}

fn send_command(stdin: &mut std::process::ChildStdin, cmd: &str) {
    println!(">>> {cmd}");
    writeln!(stdin, "{cmd}").expect("Failed to write command");
    stdin.flush().expect("Failed to flush stdin");
}

#[test]
fn test_stop_and_search_finished_race() {
    let mut engine = spawn_engine();
    let mut stdin = engine.stdin.take().expect("Failed to get stdin");
    let stdout = engine.stdout.take().expect("Failed to get stdout");

    // Channel to collect output
    let (tx, rx) = mpsc::channel();

    // Capture stdout in background thread
    let stdout_handle = thread::spawn(move || {
        let reader = BufReader::new(stdout);
        let mut lines = Vec::new();
        for line in reader.lines().map_while(Result::ok) {
            println!("<<< {line}");
            if line.starts_with("bestmove") {
                tx.send(line.clone()).ok();
            }
            lines.push(line);
        }
        lines
    });

    // Initialize engine
    send_command(&mut stdin, "usi");
    thread::sleep(Duration::from_millis(100));

    send_command(&mut stdin, "isready");
    thread::sleep(Duration::from_millis(500));

    // Set position
    send_command(&mut stdin, "position startpos");

    // Start normal search (not ponder, to ensure SearchFinished will try to send bestmove)
    println!("\n--- Starting normal search ---");
    send_command(&mut stdin, "go infinite");

    // Give it time to search deeply
    thread::sleep(Duration::from_millis(300));

    // Send stop command which will trigger bestmove
    println!("\n--- Sending stop command ---");
    send_command(&mut stdin, "stop");

    // Wait for bestmove
    let bestmove = rx.recv_timeout(Duration::from_secs(2)).expect("Should receive bestmove");

    println!("Received bestmove: {bestmove}");

    // Wait a bit more to see if any duplicate bestmove arrives
    thread::sleep(Duration::from_millis(500));

    // Check that no additional bestmove was sent
    if let Ok(duplicate) = rx.try_recv() {
        panic!("Received duplicate bestmove: {duplicate}");
    }

    // Clean up
    send_command(&mut stdin, "quit");
    drop(stdin);
    let _ = engine.wait();
    let lines = stdout_handle.join().unwrap();

    // Count bestmoves - should be exactly 1
    let bestmove_count = lines.iter().filter(|l| l.starts_with("bestmove")).count();
    assert_eq!(bestmove_count, 1, "Should have exactly 1 bestmove, got {bestmove_count}");

    println!("\n✓ Test passed: bestmove sent exactly once despite potential race");
}

#[test]
fn test_rapid_stop_go_cycles() {
    let mut engine = spawn_engine();
    let mut stdin = engine.stdin.take().expect("Failed to get stdin");
    let stdout = engine.stdout.take().expect("Failed to get stdout");

    // Capture stdout
    let stdout_handle = thread::spawn(move || {
        let reader = BufReader::new(stdout);
        let mut lines = Vec::new();
        for line in reader.lines().map_while(Result::ok) {
            println!("<<< {line}");
            lines.push(line);
        }
        lines
    });

    // Initialize
    send_command(&mut stdin, "usi");
    thread::sleep(Duration::from_millis(100));
    send_command(&mut stdin, "isready");
    thread::sleep(Duration::from_millis(500));

    // Run rapid stop/go cycles to stress test race conditions
    for i in 0..5 {
        println!("\n--- Rapid cycle {} ---", i + 1);

        send_command(&mut stdin, "position startpos");
        send_command(&mut stdin, "go infinite");

        // Very short search time
        thread::sleep(Duration::from_millis(50));

        send_command(&mut stdin, "stop");

        // Minimal wait before next cycle
        thread::sleep(Duration::from_millis(10));
    }

    // Final position and search
    send_command(&mut stdin, "position startpos");
    send_command(&mut stdin, "go depth 1");
    thread::sleep(Duration::from_millis(200));

    // Clean up
    send_command(&mut stdin, "quit");
    drop(stdin);
    let _ = engine.wait();
    let lines = stdout_handle.join().unwrap();

    // Count bestmoves - should be 6 (5 stop cycles + 1 final)
    let bestmove_count = lines.iter().filter(|l| l.starts_with("bestmove")).count();
    assert_eq!(
        bestmove_count, 6,
        "Should have exactly 6 bestmoves (5 stops + 1 final), got {bestmove_count}"
    );

    // Verify no "already sent" messages in info strings
    let duplicate_attempts = lines.iter().filter(|l| l.contains("Bestmove already sent")).count();

    println!("Duplicate send attempts blocked: {duplicate_attempts}");

    println!("\n✓ Test passed: rapid stop/go cycles handled correctly");
}

#[test]
fn test_ponder_stop_immediate_search_finished() {
    let mut engine = spawn_engine();
    let mut stdin = engine.stdin.take().expect("Failed to get stdin");
    let stdout = engine.stdout.take().expect("Failed to get stdout");

    // Channel to collect output
    let (tx, rx) = mpsc::channel();

    // Capture stdout in background thread
    let stdout_handle = thread::spawn(move || {
        let reader = BufReader::new(stdout);
        let mut lines = Vec::new();
        for line in reader.lines().map_while(Result::ok) {
            println!("<<< {line}");
            if line.starts_with("bestmove") {
                tx.send(line.clone()).ok();
            }
            lines.push(line);
        }
        lines
    });

    // Initialize engine
    send_command(&mut stdin, "usi");
    thread::sleep(Duration::from_millis(100));

    send_command(&mut stdin, "isready");
    thread::sleep(Duration::from_millis(500));

    // Set position
    send_command(&mut stdin, "position startpos");

    // Start ponder search
    println!("\n--- Starting ponder search ---");
    send_command(&mut stdin, "go ponder");

    // Give it time to search
    thread::sleep(Duration::from_millis(300));

    // Send stop command (should send bestmove)
    println!("\n--- Sending stop during ponder ---");
    send_command(&mut stdin, "stop");

    // Wait for bestmove from stop
    let bestmove = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("Should receive bestmove from stop");

    println!("Received bestmove from stop: {bestmove}");

    // Wait to ensure SearchFinished doesn't send another bestmove
    thread::sleep(Duration::from_millis(500));

    // Check that no additional bestmove was sent
    if let Ok(duplicate) = rx.try_recv() {
        panic!("Received duplicate bestmove after ponder stop: {duplicate}");
    }

    // Clean up
    send_command(&mut stdin, "quit");
    drop(stdin);
    let _ = engine.wait();
    let lines = stdout_handle.join().unwrap();

    // Count bestmoves - should be exactly 1
    let bestmove_count = lines.iter().filter(|l| l.starts_with("bestmove")).count();
    assert_eq!(
        bestmove_count, 1,
        "Should have exactly 1 bestmove from ponder stop, got {bestmove_count}"
    );

    // Check for the expected log message
    let has_stop_requested_log = lines.iter().any(|l| {
        l.contains("SearchFinished") && l.contains("ignored") && l.contains("StopRequested")
    });

    if has_stop_requested_log {
        println!("✓ Found expected log: SearchFinished ignored in StopRequested state");
    }

    println!("\n✓ Test passed: ponder stop prevents duplicate bestmove from SearchFinished");
}
