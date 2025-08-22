//! Test to verify that ponderhit is ignored when not in ponder state

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
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
fn test_non_ponder_ponderhit_ignored() {
    let mut engine = spawn_engine();
    let mut stdin = engine.stdin.take().expect("Failed to get stdin");
    let stdout = engine.stdout.take().expect("Failed to get stdout");

    // Capture stdout in background thread
    let stdout_handle = thread::spawn(move || {
        let reader = BufReader::new(stdout);
        let mut lines = Vec::new();
        for line in reader.lines().map_while(Result::ok) {
            println!("<<< {line}");
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
    println!("\n--- Starting normal search (not ponder) ---");
    send_command(&mut stdin, "go depth 2");

    // Wait for bestmove
    thread::sleep(Duration::from_millis(300));

    // Send ponderhit while in idle state (should be ignored)
    println!("\n--- Sending ponderhit in idle state (should be ignored) ---");
    send_command(&mut stdin, "ponderhit");

    thread::sleep(Duration::from_millis(100));

    // Start another search to verify engine is still functional
    println!("\n--- Starting another search ---");
    send_command(&mut stdin, "go depth 1");
    thread::sleep(Duration::from_millis(300));

    // Clean up
    send_command(&mut stdin, "quit");
    drop(stdin);
    let _ = engine.wait();
    let lines = stdout_handle.join().unwrap();

    // Verify we got exactly 2 bestmoves
    let bestmove_count = lines.iter().filter(|l| l.starts_with("bestmove")).count();
    assert_eq!(bestmove_count, 2, "Should have exactly 2 bestmoves, got {bestmove_count}");

    println!("\n✓ Test passed: ponderhit in non-ponder state is correctly ignored");
}

#[test]
fn test_ponderhit_during_normal_search() {
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

    // Set position
    send_command(&mut stdin, "position startpos");

    // Start normal search (not ponder)
    println!("\n--- Starting normal search ---");
    send_command(&mut stdin, "go infinite");

    // Let it search for a bit
    thread::sleep(Duration::from_millis(200));

    // Send ponderhit during normal search (should be ignored)
    println!("\n--- Sending ponderhit during normal search (should be ignored) ---");
    send_command(&mut stdin, "ponderhit");

    thread::sleep(Duration::from_millis(100));

    // Stop the search
    println!("\n--- Stopping search ---");
    send_command(&mut stdin, "stop");

    thread::sleep(Duration::from_millis(200));

    // Clean up
    send_command(&mut stdin, "quit");
    drop(stdin);
    let _ = engine.wait();
    let lines = stdout_handle.join().unwrap();

    // Verify we got exactly 1 bestmove (from stop, not from ponderhit)
    let bestmove_count = lines.iter().filter(|l| l.starts_with("bestmove")).count();
    assert_eq!(
        bestmove_count, 1,
        "Should have exactly 1 bestmove from stop, got {bestmove_count}"
    );

    println!("\n✓ Test passed: ponderhit during normal search is correctly ignored");
}
