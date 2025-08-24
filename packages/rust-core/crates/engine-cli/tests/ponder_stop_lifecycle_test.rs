//! Test to verify proper worker lifecycle management during ponder stop
//! Ensures that stopping ponder doesn't leave zombie workers

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
fn test_ponder_stop_then_immediate_go() {
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
    thread::sleep(Duration::from_millis(300));

    // Stop ponder
    println!("\n--- Stopping ponder ---");
    send_command(&mut stdin, "stop");

    // Wait for bestmove from ponder stop
    let ponder_bestmove = rx
        .recv_timeout(Duration::from_secs(3))
        .expect("Should receive bestmove from ponder stop");

    println!("Received bestmove from ponder stop: {ponder_bestmove}");

    // Very short wait - not enough for full cleanup
    thread::sleep(Duration::from_millis(50));

    // Immediately start new search
    println!("\n--- Starting new search immediately ---");
    send_command(&mut stdin, "go depth 3");

    // Wait for bestmove from the new search
    let bestmove = rx
        .recv_timeout(Duration::from_secs(3))
        .expect("Should receive bestmove from new search");

    println!("Received bestmove from new search: {bestmove}");

    // Set new position and search again to verify no interference
    println!("\n--- Testing with new position ---");
    send_command(&mut stdin, "position startpos moves 7g7f");
    send_command(&mut stdin, "go depth 3");

    let second_bestmove = rx
        .recv_timeout(Duration::from_secs(3))
        .expect("Should receive bestmove from second search");

    println!("Received bestmove from second search: {second_bestmove}");

    // Clean up
    send_command(&mut stdin, "quit");
    drop(stdin);
    let _ = engine.wait();
    let lines = stdout_handle.join().unwrap();

    // Verify we got exactly 3 bestmoves (1 from ponder stop + 2 from normal searches)
    let bestmove_count = lines.iter().filter(|l| l.starts_with("bestmove")).count();
    assert_eq!(
        bestmove_count, 3,
        "Should have exactly 3 bestmoves (1 ponder stop + 2 searches), got {bestmove_count}"
    );

    println!("\n✓ Test passed: ponder stop followed by immediate go works correctly");
}

#[test]
fn test_ponder_stop_delayed_message_handling() {
    let mut engine = spawn_engine();
    let mut stdin = engine.stdin.take().expect("Failed to get stdin");
    let stdout = engine.stdout.take().expect("Failed to get stdout");

    // Channel to collect all messages
    let (tx, rx) = mpsc::channel();

    // Capture stdout in background thread
    let stdout_handle = thread::spawn(move || {
        let reader = BufReader::new(stdout);
        let mut lines = Vec::new();
        for line in reader.lines().map_while(Result::ok) {
            println!("<<< {line}");
            tx.send(line.clone()).ok();
            lines.push(line);
        }
        lines
    });

    // Initialize engine
    send_command(&mut stdin, "usi");
    thread::sleep(Duration::from_millis(100));

    send_command(&mut stdin, "isready");
    thread::sleep(Duration::from_millis(500));

    // Clear any initialization messages
    while rx.try_recv().is_ok() {}

    // Set position
    send_command(&mut stdin, "position startpos");

    // Start ponder search
    println!("\n--- Starting ponder search ---");
    send_command(&mut stdin, "go ponder");

    // Let it search for a bit to build up some depth
    thread::sleep(Duration::from_millis(500));

    // Stop ponder
    println!("\n--- Stopping ponder ---");
    send_command(&mut stdin, "stop");

    // Wait a bit to see if any bestmove arrives
    thread::sleep(Duration::from_millis(500));

    // Check that bestmove was sent (per USI spec, stop during ponder sends bestmove)
    let mut bestmove_found = false;
    while let Ok(msg) = rx.try_recv() {
        if msg.starts_with("bestmove") {
            bestmove_found = true;
            println!("Got expected bestmove from ponder stop: {msg}");
        }
    }

    assert!(bestmove_found, "Bestmove should be sent when stopping ponder (USI spec)");

    // Now do a normal search to ensure engine still works
    println!("\n--- Starting normal search ---");
    send_command(&mut stdin, "go depth 2");

    // This time we should get a bestmove
    let mut got_bestmove = false;
    let timeout = std::time::Instant::now() + Duration::from_secs(3);

    while std::time::Instant::now() < timeout && !got_bestmove {
        if let Ok(msg) = rx.recv_timeout(Duration::from_millis(100)) {
            if msg.starts_with("bestmove") {
                got_bestmove = true;
                println!("Got expected bestmove: {msg}");
            }
        }
    }

    assert!(got_bestmove, "Should receive bestmove from normal search");

    // Clean up
    send_command(&mut stdin, "quit");
    drop(stdin);
    let _ = engine.wait();
    let lines = stdout_handle.join().unwrap();

    // Verify we got exactly 2 bestmoves (1 from ponder stop + 1 from normal search)
    let total_bestmoves = lines.iter().filter(|l| l.starts_with("bestmove")).count();
    assert_eq!(
        total_bestmoves, 2,
        "Should have exactly 2 bestmoves (ponder stop + normal search), got {total_bestmoves}"
    );

    println!("\n✓ Test passed: delayed messages handled correctly");
}

#[test]
fn test_multiple_ponder_stop_cycles() {
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

    // Run multiple ponder/stop cycles
    for i in 0..3 {
        println!("\n--- Cycle {} ---", i + 1);

        // Set position
        send_command(&mut stdin, "position startpos");

        // Start ponder
        send_command(&mut stdin, "go ponder");
        thread::sleep(Duration::from_millis(200));

        // Stop ponder
        send_command(&mut stdin, "stop");
        thread::sleep(Duration::from_millis(100));
    }

    // Final normal search to verify engine is still responsive
    println!("\n--- Final search ---");
    send_command(&mut stdin, "position startpos");
    send_command(&mut stdin, "go depth 1");
    thread::sleep(Duration::from_millis(500));

    // Clean up
    send_command(&mut stdin, "quit");
    drop(stdin);
    let _ = engine.wait();
    let lines = stdout_handle.join().unwrap();

    // Count bestmoves - should have 3 from ponder stops + 1 from final search = 4 total
    let bestmove_count = lines.iter().filter(|l| l.starts_with("bestmove")).count();
    assert_eq!(
        bestmove_count, 4,
        "Should have exactly 4 bestmoves (3 ponder stops + 1 final search), got {bestmove_count}"
    );

    println!("\n✓ Test passed: multiple ponder/stop cycles handled correctly");
}

#[test]
fn test_ponder_stop_then_gameover() {
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
    thread::sleep(Duration::from_millis(300));

    // Stop ponder
    println!("\n--- Stopping ponder ---");
    send_command(&mut stdin, "stop");

    // Very short wait
    thread::sleep(Duration::from_millis(50));

    // Send gameover while worker might still be running
    println!("\n--- Sending gameover ---");
    send_command(&mut stdin, "gameover win");

    // Wait a bit to ensure no bestmove is sent
    thread::sleep(Duration::from_millis(300));

    // Verify bestmove was sent from ponder stop
    let ponder_bestmove = rx
        .recv_timeout(Duration::from_millis(500))
        .expect("Should receive bestmove from ponder stop");
    println!("Got expected bestmove from ponder stop: {ponder_bestmove}");

    // Now start a new game and search to verify engine is still functional
    println!("\n--- Starting new game ---");
    send_command(&mut stdin, "usinewgame");
    send_command(&mut stdin, "position startpos");
    send_command(&mut stdin, "go depth 2");

    // This should produce a bestmove
    let bestmove = rx
        .recv_timeout(Duration::from_secs(3))
        .expect("Should receive bestmove from new game search");

    println!("Got expected bestmove from new game: {bestmove}");

    // Clean up
    send_command(&mut stdin, "quit");
    drop(stdin);
    let _ = engine.wait();
    let lines = stdout_handle.join().unwrap();

    // Count bestmoves - should have 1 from ponder stop + 1 from new game = 2 total
    let bestmove_count = lines.iter().filter(|l| l.starts_with("bestmove")).count();
    assert_eq!(
        bestmove_count, 2,
        "Should have exactly 2 bestmoves (1 ponder stop + 1 new game), got {bestmove_count}"
    );

    println!("\n✓ Test passed: ponder stop followed by gameover handled correctly");
}

#[test]
fn test_ponder_stop_ponderhit_race() {
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

    // Start ponder
    println!("\n--- Starting ponder search ---");
    send_command(&mut stdin, "go ponder");
    thread::sleep(Duration::from_millis(200));

    // Stop ponder
    println!("\n--- Stopping ponder ---");
    send_command(&mut stdin, "stop");

    // Immediately send ponderhit (should be ignored)
    println!("\n--- Sending ponderhit (should be ignored) ---");
    send_command(&mut stdin, "ponderhit");

    thread::sleep(Duration::from_millis(100));

    // Start a new search
    println!("\n--- Starting new search ---");
    send_command(&mut stdin, "go depth 1");
    thread::sleep(Duration::from_millis(500));

    // Clean up
    send_command(&mut stdin, "quit");
    drop(stdin);
    let _ = engine.wait();
    let lines = stdout_handle.join().unwrap();

    // Verify we got exactly 2 bestmoves (1 from ponder stop + 1 from final search)
    let bestmove_count = lines.iter().filter(|l| l.starts_with("bestmove")).count();
    assert_eq!(
        bestmove_count, 2,
        "Should have exactly 2 bestmoves (1 ponder stop + 1 final search), got {bestmove_count}"
    );

    // Check logs for ponderhit being ignored
    let _ignored_ponderhit = lines
        .iter()
        .any(|l| l.contains("Ponder hit ignored") || l.contains("not in active search state"));

    // Note: This log might not appear in the output since it's a debug log
    // The important thing is that no extra bestmove was sent

    println!("\n✓ Test passed: ponderhit during stop state handled correctly");
}
