//! Test to verify that stop during ponder emits bestmove
//! as per USI protocol specification

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
fn test_ponder_stop_sends_bestmove() {
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

    // Give it time to start pondering
    thread::sleep(Duration::from_millis(500));

    // Send stop during ponder
    println!("\n--- Sending stop during ponder ---");
    send_command(&mut stdin, "stop");

    // Wait to see if bestmove is emitted
    thread::sleep(Duration::from_millis(500));

    // Check if we received any bestmove
    let bestmove_received = rx.try_recv().is_ok();

    // Clean up
    send_command(&mut stdin, "quit");
    drop(stdin);
    let _ = engine.wait();
    let lines = stdout_handle.join().unwrap();

    // Verify bestmove was sent
    assert!(bestmove_received, "Bestmove should be sent when stopping ponder per USI spec");

    // Double-check in collected lines
    let has_bestmove = lines.iter().any(|l| l.starts_with("bestmove"));
    assert!(has_bestmove, "Bestmove line should exist in output when stopping ponder");

    println!("\n✓ Test passed: stop during ponder emits bestmove as per USI spec");
}

#[test]
fn test_normal_search_stop_sends_bestmove() {
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

    // Start normal search (not ponder)
    println!("\n--- Starting normal search ---");
    send_command(&mut stdin, "go infinite");

    // Give it time to search
    thread::sleep(Duration::from_millis(500));

    // Send stop during normal search
    println!("\n--- Sending stop during normal search ---");
    send_command(&mut stdin, "stop");

    // Wait for bestmove
    let bestmove = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("Should receive bestmove after stop in normal search");

    println!("Received bestmove: {bestmove}");

    // Clean up
    send_command(&mut stdin, "quit");
    drop(stdin);
    let _ = engine.wait();
    let _ = stdout_handle.join();

    // Verify bestmove was sent
    assert!(
        bestmove.starts_with("bestmove"),
        "Should receive bestmove when stopping normal search"
    );

    println!("\n✓ Test passed: stop during normal search sends bestmove");
}
