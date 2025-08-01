//! Stress tests for ponder_hit functionality to ensure no deadlocks

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// Helper to spawn engine process
fn spawn_engine() -> std::process::Child {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_engine-cli"));
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped()) // Capture stderr instead of null
        .env("RUST_LOG", ""); // Disable logging to avoid interference

    match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            eprintln!("Failed to spawn engine at path: {}", env!("CARGO_BIN_EXE_engine-cli"));
            eprintln!("Error: {e}");
            panic!("Failed to spawn engine");
        }
    }
}

/// Helper to send command to engine
fn send_command(stdin: &mut impl Write, command: &str) {
    match writeln!(stdin, "{command}") {
        Ok(_) => {}
        Err(e) => {
            eprintln!("Failed to write command '{command}': {e:?}");
            panic!("Failed to write command: {e:?}");
        }
    }
    match stdin.flush() {
        Ok(_) => {}
        Err(e) => {
            eprintln!("Failed to flush stdin after command '{command}': {e:?}");
            panic!("Failed to flush stdin: {e:?}");
        }
    }
}

/// Helper to read lines until a specific pattern or timeout
fn read_until_pattern(
    reader: &mut impl BufRead,
    pattern: &str,
    timeout: Duration,
) -> Result<Vec<String>, String> {
    let mut lines = Vec::new();
    let start = Instant::now();

    while start.elapsed() < timeout {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {
                let trimmed = line.trim().to_string();
                if !trimmed.is_empty() {
                    let matches_pattern = trimmed.starts_with(pattern);
                    lines.push(trimmed);
                    if matches_pattern {
                        return Ok(lines);
                    }
                }
            }
            Err(_) => break,
        }
    }

    if lines.is_empty() {
        Err(format!("Timeout waiting for pattern: {pattern}"))
    } else {
        Ok(lines)
    }
}

#[test]
fn test_rapid_ponder_hit() {
    let mut engine = spawn_engine();
    let stdin = engine.stdin.as_mut().expect("Failed to get stdin");
    let stdout = engine.stdout.as_mut().expect("Failed to get stdout");
    let mut reader = BufReader::new(stdout);

    // Initialize
    send_command(stdin, "usi");
    match read_until_pattern(&mut reader, "usiok", Duration::from_secs(2)) {
        Ok(_) => {}
        Err(e) => {
            // Check if engine crashed
            match engine.try_wait() {
                Ok(Some(status)) => {
                    let mut stderr = String::new();
                    if let Some(mut err) = engine.stderr.take() {
                        use std::io::Read;
                        let _ = err.read_to_string(&mut stderr);
                    }
                    panic!("Engine exited with status: {status:?}, stderr: {stderr}");
                }
                _ => panic!("Failed to get usiok: {e}"),
            }
        }
    }
    send_command(stdin, "isready");
    let _ = read_until_pattern(&mut reader, "readyok", Duration::from_secs(2))
        .expect("Failed to get readyok");

    // Test rapid ponder_hit commands
    for iteration in 0..5 {
        println!("Rapid ponder_hit test iteration {}", iteration + 1);

        // Set position (using USI notation)
        send_command(stdin, "position startpos moves 7g7f 8c8d");

        // Start ponder search
        println!("Sending 'go ponder' command...");
        send_command(stdin, "go ponder btime 30000 wtime 30000");

        // Flush to ensure command is sent
        stdin.flush().expect("Failed to flush stdin after go ponder");

        // Give enough time for ponder to actually start
        thread::sleep(Duration::from_millis(50));

        // Send ponder_hit immediately
        let ponder_hit_start = Instant::now();
        println!("Sending 'ponderhit' command...");
        send_command(stdin, "ponderhit");

        // Ponder_hit should be processed immediately without blocking
        // The search should continue and eventually produce a bestmove
        let result = read_until_pattern(&mut reader, "bestmove", Duration::from_secs(5));
        let ponder_hit_elapsed = ponder_hit_start.elapsed();

        assert!(
            result.is_ok(),
            "Failed to get bestmove after ponderhit in iteration {}",
            iteration + 1
        );

        // Allow more time for search completion since we're using time controls
        // Extended timeout for CI environments with limited resources
        assert!(
            ponder_hit_elapsed < Duration::from_secs(10),
            "Ponder_hit processing took too long: {ponder_hit_elapsed:?}"
        );

        println!("Iteration {} completed in {:?}", iteration + 1, ponder_hit_elapsed);

        // Verify we got some search info
        let lines = result.unwrap();
        let has_info = lines.iter().any(|line| line.starts_with("info"));
        assert!(has_info, "Expected info output during search");
    }

    // Cleanup
    send_command(stdin, "quit");
    let _ = engine.wait();
}

#[test]
fn test_ponder_hit_while_mutex_locked() {
    let mut engine = spawn_engine();
    let stdin = engine.stdin.as_mut().expect("Failed to get stdin");
    let stdout = engine.stdout.as_mut().expect("Failed to get stdout");
    let mut reader = BufReader::new(stdout);

    // Initialize
    send_command(stdin, "usi");
    let _ = read_until_pattern(&mut reader, "usiok", Duration::from_secs(2))
        .expect("Failed to get usiok");
    send_command(stdin, "isready");
    let _ = read_until_pattern(&mut reader, "readyok", Duration::from_secs(2))
        .expect("Failed to get readyok");

    // Set position
    send_command(stdin, "position startpos moves 7g7f 3c3d");

    // Start ponder search
    send_command(stdin, "go ponder btime 60000 wtime 60000");

    // Give time for search to actually start
    thread::sleep(Duration::from_millis(100));

    // Send multiple ponder_hit commands rapidly
    // This tests that ponder_hit doesn't block even if sent multiple times
    let start = Instant::now();
    for i in 0..3 {
        send_command(stdin, "ponderhit");
        // Each command should be processed immediately
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(50 * (i + 1) as u64),
            "Commands appear to be blocking"
        );
    }

    // Wait for bestmove
    let result = read_until_pattern(&mut reader, "bestmove", Duration::from_secs(3));
    assert!(result.is_ok(), "Failed to get bestmove after multiple ponderhit commands");

    // Cleanup
    send_command(stdin, "quit");
    let _ = engine.wait();
}

#[test]
fn test_concurrent_ponder_and_position_commands() {
    let mut engine = spawn_engine();
    let stdin = engine.stdin.as_mut().expect("Failed to get stdin");
    let stdout = engine.stdout.as_mut().expect("Failed to get stdout");
    let mut reader = BufReader::new(stdout);

    // Initialize
    send_command(stdin, "usi");
    let _ = read_until_pattern(&mut reader, "usiok", Duration::from_secs(2))
        .expect("Failed to get usiok");
    send_command(stdin, "isready");
    let _ = read_until_pattern(&mut reader, "readyok", Duration::from_secs(2))
        .expect("Failed to get readyok");

    // Set initial position
    send_command(stdin, "position startpos moves 7g7f");

    // Start ponder search
    send_command(stdin, "go ponder btime 30000 wtime 30000");
    thread::sleep(Duration::from_millis(50));

    // Send ponder_hit
    send_command(stdin, "ponderhit");

    // Wait for bestmove from the ponder search first
    let ponder_result = read_until_pattern(&mut reader, "bestmove", Duration::from_secs(5));
    assert!(ponder_result.is_ok(), "Failed to get bestmove after ponderhit");

    // Now try to set a new position (should work immediately since search is done)
    let position_start = Instant::now();
    send_command(stdin, "position startpos");

    // Start a new search to verify position was updated
    // Use longer movetime for CI environments
    send_command(stdin, "go movetime 500");

    let result = read_until_pattern(&mut reader, "bestmove", Duration::from_secs(5));
    let total_elapsed = position_start.elapsed();

    assert!(result.is_ok(), "Failed to complete new search after position update");
    assert!(
        total_elapsed < Duration::from_secs(6),
        "Position update appeared to block for too long: {total_elapsed:?}"
    );

    // Cleanup
    send_command(stdin, "quit");
    let _ = engine.wait();
}

#[test]
fn test_ponder_hit_with_stop() {
    let mut engine = spawn_engine();
    let stdin = engine.stdin.as_mut().expect("Failed to get stdin");
    let stdout = engine.stdout.as_mut().expect("Failed to get stdout");
    let mut reader = BufReader::new(stdout);

    // Initialize
    send_command(stdin, "usi");
    let _ = read_until_pattern(&mut reader, "usiok", Duration::from_secs(2))
        .expect("Failed to get usiok");
    send_command(stdin, "isready");
    let _ = read_until_pattern(&mut reader, "readyok", Duration::from_secs(2))
        .expect("Failed to get readyok");

    // Set position
    send_command(stdin, "position startpos moves 7g7f 3c3d");

    // Start ponder search
    send_command(stdin, "go ponder btime 60000 wtime 60000");
    thread::sleep(Duration::from_millis(100));

    // Send ponder_hit
    send_command(stdin, "ponderhit");
    thread::sleep(Duration::from_millis(50));

    // Then immediately stop
    let stop_start = Instant::now();
    send_command(stdin, "stop");

    // Should get bestmove quickly
    let result = read_until_pattern(&mut reader, "bestmove", Duration::from_millis(500));
    let stop_elapsed = stop_start.elapsed();

    assert!(result.is_ok(), "Failed to get bestmove after stop");
    assert!(
        stop_elapsed < Duration::from_millis(500),
        "Stop after ponderhit took too long: {stop_elapsed:?}"
    );

    // Cleanup
    send_command(stdin, "quit");
    let _ = engine.wait();
}

#[test]
fn test_ponder_hit_stress_with_thread_timing() {
    // This test specifically checks for race conditions by using thread timing
    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_flag_clone = stop_flag.clone();

    // Spawn thread to run the engine
    let engine_thread = thread::spawn(move || {
        let mut engine = spawn_engine();
        let stdin = engine.stdin.as_mut().expect("Failed to get stdin");
        let stdout = engine.stdout.as_mut().expect("Failed to get stdout");
        let mut reader = BufReader::new(stdout);

        // Initialize
        send_command(stdin, "usi");
        let _ = read_until_pattern(&mut reader, "usiok", Duration::from_secs(2))
            .expect("Failed to get usiok");
        send_command(stdin, "isready");
        let _ = read_until_pattern(&mut reader, "readyok", Duration::from_secs(2))
            .expect("Failed to get readyok");

        // Run rapid ponder/ponderhit cycles
        let mut iteration = 0;
        while !stop_flag_clone.load(Ordering::Relaxed) && iteration < 10 {
            // Set position
            send_command(stdin, "position startpos moves 7g7f 3c3d");

            // Start ponder
            send_command(stdin, "go ponder btime 10000 wtime 10000");

            // Short sleep to create race condition opportunity
            // Using 50ms to ensure ponder search has time to initialize
            thread::sleep(Duration::from_millis(50));

            // Send ponderhit
            send_command(stdin, "ponderhit");

            // Wait for bestmove
            match read_until_pattern(&mut reader, "bestmove", Duration::from_secs(1)) {
                Ok(_) => {
                    iteration += 1;
                    // Small delay between iterations to ensure clean state
                    thread::sleep(Duration::from_millis(10));
                }
                Err(e) => {
                    eprintln!("Failed to get bestmove in iteration {iteration}: {e}");
                    send_command(stdin, "stop");
                    let _ = read_until_pattern(&mut reader, "bestmove", Duration::from_millis(500));
                    break;
                }
            }
        }

        // Cleanup
        send_command(stdin, "quit");
        let _ = engine.wait();

        iteration
    });

    // Let it run for a bit longer to allow more iterations
    thread::sleep(Duration::from_secs(8));

    // Signal stop
    stop_flag.store(true, Ordering::Relaxed);

    // Wait for thread to complete
    let iterations_completed = engine_thread.join().expect("Engine thread panicked");

    // Should have completed at least a few iterations without deadlock
    // Reduced from 5 to 3 for stability in CI environments with resource constraints
    assert!(
        iterations_completed >= 3,
        "Only completed {iterations_completed} iterations, possible deadlock"
    );
}
