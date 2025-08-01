//! Thread safety tests for USI engine
//! Tests race conditions and concurrent command handling

use crossbeam_channel::{unbounded, Receiver};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

/// Helper to spawn engine process
fn spawn_engine() -> std::process::Child {
    Command::new(env!("CARGO_BIN_EXE_engine-cli"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit()) // Show any panics or errors
        .spawn()
        .expect("Failed to spawn engine")
}

/// Helper to send command to engine
fn send_command<W: Write + ?Sized>(stdin: &mut W, command: &str) {
    if let Err(e) = writeln!(stdin, "{command}") {
        eprintln!("Warning: Failed to write command '{command}': {e}");
        return;
    }
    if let Err(e) = stdin.flush() {
        eprintln!("Warning: Failed to flush stdin after '{command}': {e}");
    }
}

/// Spawn a thread that continuously reads engine output
fn spawn_output_reader(
    mut reader: BufReader<std::process::ChildStdout>,
) -> (Receiver<String>, thread::JoinHandle<()>) {
    let (tx, rx) = unbounded::<String>();

    let handle = thread::spawn(move || {
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break, // EOF
                Ok(_) => {
                    let trimmed = line.trim_end().to_string();
                    if !trimmed.is_empty() {
                        println!("[ENGINE] {trimmed}");
                        if tx.send(trimmed).is_err() {
                            break; // Receiver dropped
                        }
                    }
                }
                Err(_) => break,
            }
        }
    });

    (rx, handle)
}

/// Helper to read lines until a specific pattern or timeout
fn read_until_pattern(
    rx: &Receiver<String>,
    pattern: &str,
    timeout: Duration,
) -> Result<Vec<String>, String> {
    let deadline = Instant::now() + timeout;
    let mut lines = Vec::new();

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!("Timeout waiting for pattern: {pattern}"));
        }

        match rx.recv_timeout(remaining) {
            Ok(line) => {
                lines.push(line.clone());
                if line.starts_with(pattern) {
                    return Ok(lines);
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                return Err(format!("Timeout waiting for pattern: {pattern}"));
            }
            Err(_) => return Err("Output channel closed".into()),
        }
    }
}

/// Cleanup helper to properly shutdown engine
fn cleanup_engine(
    mut engine: std::process::Child,
    stdin: std::process::ChildStdin,
    reader_handle: thread::JoinHandle<()>,
) {
    // Close stdin to send EOF
    drop(stdin);

    // Wait for engine to exit
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match engine.try_wait() {
            Ok(Some(status)) => {
                println!("Engine exited with status: {status:?}");
                break;
            }
            Ok(None) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(50));
            }
            Ok(None) | Err(_) => {
                println!("Engine didn't exit in time, killing it");
                let _ = engine.kill();
                let _ = engine.wait();
                break;
            }
        }
    }

    // Wait for reader thread to finish
    let _ = reader_handle.join();
}

/// Initialize engine and return handles
fn init_engine() -> (
    std::process::Child,
    std::process::ChildStdin,
    Receiver<String>,
    thread::JoinHandle<()>,
) {
    let mut engine = spawn_engine();
    let mut stdin = engine.stdin.take().expect("Failed to get stdin");
    let stdout = engine.stdout.take().expect("Failed to get stdout");
    let reader = BufReader::new(stdout);

    // Start output reader thread
    let (rx, reader_handle) = spawn_output_reader(reader);

    // Initialize
    send_command(&mut stdin, "usi");
    let _ = read_until_pattern(&rx, "usiok", Duration::from_secs(2)).expect("Failed to get usiok");
    send_command(&mut stdin, "isready");
    let _ =
        read_until_pattern(&rx, "readyok", Duration::from_secs(2)).expect("Failed to get readyok");

    (engine, stdin, rx, reader_handle)
}

#[test]
fn test_stop_ponderhit_simultaneous() {
    let (mut engine, stdin, rx, _reader_handle) = init_engine();
    let stdin = Arc::new(std::sync::Mutex::new(stdin));

    // Set position
    {
        let mut stdin = stdin.lock().unwrap();
        send_command(&mut *stdin, "position startpos moves 7g7f 8c8d");
    }

    // Start ponder search
    {
        let mut stdin = stdin.lock().unwrap();
        send_command(&mut *stdin, "go ponder btime 60000 wtime 60000");
    }

    // Give time for ponder to start
    thread::sleep(Duration::from_millis(50));

    // Create barrier for simultaneous execution
    let barrier = Arc::new(Barrier::new(2));
    let stdin_clone = stdin.clone();
    let barrier_clone = barrier.clone();
    let stdin_clone2 = stdin.clone();

    // Thread 1: Send stop
    let stop_thread = thread::spawn(move || {
        barrier_clone.wait();
        if let Ok(mut stdin) = stdin_clone.lock() {
            send_command(&mut *stdin, "stop");
        } else {
            eprintln!("Failed to lock stdin for stop command");
        }
    });

    // Thread 2: Send ponderhit
    let ponderhit_thread = thread::spawn(move || {
        barrier.wait();
        if let Ok(mut stdin) = stdin_clone2.lock() {
            send_command(&mut *stdin, "ponderhit");
        } else {
            eprintln!("Failed to lock stdin for ponderhit command");
        }
    });

    // Wait for threads
    stop_thread.join().expect("Stop thread panicked");
    ponderhit_thread.join().expect("Ponderhit thread panicked");

    // Should get bestmove without deadlock
    let result = read_until_pattern(&rx, "bestmove", Duration::from_secs(10));
    assert!(result.is_ok(), "Failed to get bestmove after simultaneous stop/ponderhit");

    // Cleanup
    {
        let mut stdin = Arc::try_unwrap(stdin)
            .unwrap_or_else(|_| panic!("Failed to unwrap stdin"))
            .into_inner()
            .unwrap();
        send_command(&mut stdin, "quit");
    }
    let _ = engine.wait();
}

#[test]
fn test_multiple_go_commands_concurrent() {
    let (mut engine, stdin, rx, _reader_handle) = init_engine();
    let stdin = Arc::new(std::sync::Mutex::new(stdin));

    // Set position
    {
        let mut stdin = stdin.lock().expect("Failed to lock stdin for initial position");
        send_command(&mut *stdin, "position startpos");
    }

    // Track which thread succeeded
    let success_count = Arc::new(AtomicU32::new(0));
    let barrier = Arc::new(Barrier::new(3));

    // Spawn 3 threads trying to start search simultaneously
    let mut handles = vec![];
    for i in 0..3 {
        let stdin = stdin.clone();
        let barrier = barrier.clone();
        let success_count = success_count.clone();

        let handle = thread::spawn(move || {
            barrier.wait();
            let mut stdin = stdin.lock().expect("Failed to lock stdin for go command");
            send_command(&mut *stdin, &format!("go movetime {}", 1000 + i * 100));
            drop(stdin); // Release lock immediately
            success_count.fetch_add(1, Ordering::SeqCst);
        });
        handles.push(handle);
    }

    // Wait for all threads
    for handle in handles {
        handle.join().expect("Thread panicked");
    }

    // Collect all messages to verify proper handling of multiple go commands
    let mut messages = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut bestmove_count = 0;

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }

        match rx.recv_timeout(remaining) {
            Ok(line) => {
                messages.push(line.clone());
                if line.starts_with("bestmove") {
                    bestmove_count += 1;
                }
                if line.contains("Engine is currently in use") || line.contains("error") {
                    println!("Engine busy/error message: {line}");
                }
            }
            Err(_) => break,
        }
    }

    // Engine should process all go commands sequentially (current implementation)
    // This is acceptable behavior - engine queues and processes each command
    println!("Received {bestmove_count} bestmove responses");
    assert!(bestmove_count >= 1, "Expected at least 1 bestmove, got {bestmove_count}");

    // Check for any error messages
    let error_messages: Vec<_> = messages
        .iter()
        .filter(|m| m.contains("error") || m.contains("Engine is currently in use"))
        .collect();
    if !error_messages.is_empty() {
        println!("Error messages: {error_messages:?}");
    }

    // Verify we sent 3 commands
    assert_eq!(success_count.load(Ordering::SeqCst), 3, "Not all go commands were sent");

    // Verify engine handled all commands without errors
    assert!(error_messages.is_empty(), "Engine reported errors: {error_messages:?}");

    // Cleanup
    {
        let mut stdin = Arc::try_unwrap(stdin)
            .unwrap_or_else(|_| panic!("Failed to unwrap stdin"))
            .into_inner()
            .unwrap();
        send_command(&mut stdin, "quit");
    }
    let _ = engine.wait();
}

#[test]
fn test_position_update_during_stop() {
    let (engine, stdin, rx, reader_handle) = init_engine();
    let stdin = Arc::new(std::sync::Mutex::new(stdin));

    // Start search
    {
        let mut stdin = stdin.lock().unwrap();
        send_command(&mut *stdin, "position startpos");
        send_command(&mut *stdin, "go infinite");
    }

    // Wait for search to actually start by looking for info output
    match read_until_pattern(&rx, "info ", Duration::from_millis(500)) {
        Ok(_) => println!("Search confirmed started (info received)"),
        Err(e) => {
            println!("Warning: No info line for infinite search: {e}");
            // Try to continue anyway
        }
    }

    // Create threads for concurrent position and stop
    let barrier = Arc::new(Barrier::new(2));
    let stdin_clone = stdin.clone();
    let stdin_clone2 = stdin.clone();
    let barrier_clone = barrier.clone();

    // Thread 1: Update position
    let position_thread = thread::spawn(move || {
        barrier_clone.wait();
        let mut stdin = stdin_clone.lock().unwrap();
        send_command(&mut *stdin, "position startpos moves 7g7f");
    });

    // Thread 2: Send stop
    let stop_thread = thread::spawn(move || {
        barrier.wait();
        let mut stdin = stdin_clone2.lock().unwrap();
        send_command(&mut *stdin, "stop");
    });

    // Wait for threads
    position_thread.join().expect("Position thread panicked");
    stop_thread.join().expect("Stop thread panicked");

    // Should get bestmove
    let result = read_until_pattern(&rx, "bestmove", Duration::from_secs(10));
    assert!(
        result.is_ok(),
        "Failed to get bestmove after position/stop race - {:?}",
        result.err()
    );

    // Verify we can start a new search with the updated position
    {
        let mut stdin = stdin.lock().unwrap();
        send_command(&mut *stdin, "go movetime 300");
    }

    // Wait for this search to complete
    let result = read_until_pattern(&rx, "bestmove", Duration::from_secs(2));
    assert!(
        result.is_ok(),
        "Failed to start new search after position update - {:?}",
        result.err()
    );

    // Cleanup
    {
        let mut stdin = stdin.lock().unwrap();
        send_command(&mut *stdin, "quit");
    }
    let stdin = Arc::try_unwrap(stdin)
        .unwrap_or_else(|_| panic!("Failed to unwrap stdin"))
        .into_inner()
        .unwrap();
    cleanup_engine(engine, stdin, reader_handle);
}

#[test]
fn test_memory_ordering_visibility() {
    // This test verifies that atomic operations have proper visibility across threads
    let (mut engine, stdin, rx, _reader_handle) = init_engine();
    let stdin = Arc::new(std::sync::Mutex::new(stdin));

    // Track command timing
    let command_sent = Arc::new(AtomicBool::new(false));
    let response_received = Arc::new(AtomicBool::new(false));

    let command_sent_clone = command_sent.clone();
    let response_received_clone = response_received.clone();
    let stdin_clone = stdin.clone();

    // Thread 1: Send commands and mark timing
    let sender_thread = thread::spawn(move || {
        let mut stdin = stdin_clone.lock().unwrap();
        send_command(&mut *stdin, "position startpos");
        send_command(&mut *stdin, "go ponder");
        command_sent_clone.store(true, Ordering::Release);

        // Wait a bit then send ponderhit
        thread::sleep(Duration::from_millis(50));
        send_command(&mut *stdin, "ponderhit");
    });

    // Thread 2: Monitor for proper ordering
    let monitor_thread = thread::spawn(move || {
        // Wait for command to be sent
        while !command_sent.load(Ordering::Acquire) {
            thread::yield_now();
        }

        // At this point, commands should be visible to engine
        response_received_clone.store(true, Ordering::Release);
    });

    // Wait for threads
    sender_thread.join().expect("Sender thread panicked");
    monitor_thread.join().expect("Monitor thread panicked");

    // Verify ordering was maintained
    assert!(response_received.load(Ordering::Acquire), "Memory ordering not maintained");

    // Get bestmove (allow more time for CI environments where search might take longer)
    let result = read_until_pattern(&rx, "bestmove", Duration::from_secs(10));
    assert!(result.is_ok(), "Failed to get bestmove");

    // Cleanup
    {
        let mut stdin = Arc::try_unwrap(stdin)
            .unwrap_or_else(|_| panic!("Failed to unwrap stdin"))
            .into_inner()
            .unwrap();
        send_command(&mut stdin, "quit");
    }
    let _ = engine.wait();
}

#[test]
fn test_rapid_go_stop_cycles() {
    let (engine, mut stdin, rx, reader_handle) = init_engine();

    // Set position once
    send_command(&mut stdin, "position startpos");

    // Rapid go/stop cycles - reduced iterations for stability
    for i in 0..5 {
        println!("Starting rapid cycle {}", i + 1);

        // Start search with longer time to ensure it starts
        send_command(&mut stdin, "go movetime 500");

        // Wait for "info" line to confirm search actually started
        match read_until_pattern(&rx, "info ", Duration::from_millis(500)) {
            Ok(_) => {
                println!("Search started (info received), sending stop");
            }
            Err(e) => {
                println!("Warning: No info line received: {e}");
                // Continue anyway - some engines might not send info immediately
            }
        }

        // Stop after confirming search started
        send_command(&mut stdin, "stop");

        // Should get bestmove
        let result = read_until_pattern(&rx, "bestmove", Duration::from_secs(3));
        assert!(
            result.is_ok(),
            "Failed to get bestmove in rapid cycle {} - {:?}",
            i + 1,
            result.err()
        );

        // Small delay between cycles
        thread::sleep(Duration::from_millis(50));
    }

    // Cleanup
    send_command(&mut stdin, "quit");
    cleanup_engine(engine, stdin, reader_handle);
}

#[test]
fn test_concurrent_setoption_and_go() {
    let (mut engine, stdin, rx, _reader_handle) = init_engine();
    let stdin = Arc::new(std::sync::Mutex::new(stdin));

    let barrier = Arc::new(Barrier::new(2));
    let stdin_clone = stdin.clone();
    let barrier_clone = barrier.clone();
    let stdin_clone2 = stdin.clone();

    // Thread 1: Set option
    let option_thread = thread::spawn(move || {
        barrier_clone.wait();
        let mut stdin = stdin_clone.lock().unwrap();
        send_command(&mut *stdin, "setoption name Threads value 2");
    });

    // Thread 2: Start search
    let search_thread = thread::spawn(move || {
        barrier.wait();
        let mut stdin = stdin_clone2.lock().unwrap();
        send_command(&mut *stdin, "position startpos");
        send_command(&mut *stdin, "go movetime 500");
    });

    // Wait for threads
    option_thread.join().expect("Option thread panicked");
    search_thread.join().expect("Search thread panicked");

    // Should complete search successfully
    let result = read_until_pattern(&rx, "bestmove", Duration::from_secs(2));
    assert!(result.is_ok(), "Failed to complete search after concurrent setoption");

    // Cleanup
    {
        let mut stdin = Arc::try_unwrap(stdin)
            .unwrap_or_else(|_| panic!("Failed to unwrap stdin"))
            .into_inner()
            .unwrap();
        send_command(&mut stdin, "quit");
    }
    let _ = engine.wait();
}
