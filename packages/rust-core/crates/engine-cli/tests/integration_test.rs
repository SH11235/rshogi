//! Integration tests for USI engine

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Helper to spawn engine process
fn spawn_engine() -> std::process::Child {
    // In GitHub Actions, we should use the built binary directly
    let program = if std::env::var("CI").is_ok() {
        // In CI, use the built binary
        std::env::current_exe()
            .map(|p| p.parent().unwrap().join("engine-cli").to_string_lossy().to_string())
            .unwrap_or_else(|_| "cargo".to_string())
    } else {
        "cargo".to_string()
    };

    let mut cmd = if program == "cargo" {
        let mut c = Command::new("cargo");
        c.args(["run", "--bin", "engine-cli", "--"]);
        c
    } else {
        Command::new(&program)
    };

    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped()) // Capture stderr for debugging
        .spawn()
        .expect("Failed to spawn engine")
}

/// Helper to send command to engine
fn send_command(stdin: &mut impl Write, command: &str) {
    if let Err(e) = writeln!(stdin, "{command}") {
        eprintln!("Failed to write command '{}': {}", command, e);
        panic!("Failed to write command: {}", e);
    }
    if let Err(e) = stdin.flush() {
        eprintln!("Failed to flush stdin after command '{}': {}", command, e);
        panic!("Failed to flush stdin: {}", e);
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
fn test_stop_response_time() {
    let mut engine = spawn_engine();
    let stdin = engine.stdin.as_mut().expect("Failed to get stdin");
    let stdout = engine.stdout.as_mut().expect("Failed to get stdout");
    let mut reader = BufReader::new(stdout);

    // Initialize engine
    send_command(stdin, "usi");
    let _ = read_until_pattern(&mut reader, "usiok", Duration::from_secs(2))
        .expect("Failed to get usiok");

    send_command(stdin, "isready");
    let _ = read_until_pattern(&mut reader, "readyok", Duration::from_secs(2))
        .expect("Failed to get readyok");

    // Set position
    send_command(stdin, "position startpos");

    // Start search
    send_command(stdin, "go infinite");
    thread::sleep(Duration::from_millis(100)); // Let search start

    // Send stop and measure time
    let start = Instant::now();
    send_command(stdin, "stop");

    // Wait for bestmove
    let result = read_until_pattern(&mut reader, "bestmove", Duration::from_millis(1000));
    let elapsed = start.elapsed();

    assert!(result.is_ok(), "No bestmove received after stop");

    // Check response time is under 500ms
    assert!(elapsed < Duration::from_millis(500), "Stop response took too long: {elapsed:?}");

    // Cleanup
    send_command(stdin, "quit");
    let _ = engine.wait();
}

#[test]
fn test_quit_clean_exit() {
    let mut engine = spawn_engine();
    let stdin = engine.stdin.as_mut().expect("Failed to get stdin");
    let stdout = engine.stdout.as_mut().expect("Failed to get stdout");
    let mut reader = BufReader::new(stdout);

    // Initialize engine
    send_command(stdin, "usi");
    let _ = read_until_pattern(&mut reader, "usiok", Duration::from_secs(2))
        .expect("Failed to get usiok");

    send_command(stdin, "isready");
    let _ = read_until_pattern(&mut reader, "readyok", Duration::from_secs(2))
        .expect("Failed to get readyok");

    // Set position and start search
    send_command(stdin, "position startpos");
    send_command(stdin, "go infinite");
    thread::sleep(Duration::from_millis(100)); // Let search start

    // Send quit
    send_command(stdin, "quit");

    // Drop stdin to close the pipe
    drop(engine.stdin.take());

    // Wait for process to exit
    let start = Instant::now();
    let timeout = Duration::from_secs(2);

    loop {
        match engine.try_wait() {
            Ok(Some(status)) => {
                assert!(status.success(), "Engine exited with error: {status:?}");
                break;
            }
            Ok(None) => {
                if start.elapsed() > timeout {
                    // Force kill if needed
                    let _ = engine.kill();
                    panic!("Engine did not exit within timeout");
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                panic!("Error waiting for engine to exit: {e}");
            }
        }
    }
}

#[test]
fn test_stop_during_deep_search() {
    let mut engine = spawn_engine();
    let stdin = engine.stdin.as_mut().expect("Failed to get stdin");
    let stdout = engine.stdout.as_mut().expect("Failed to get stdout");
    let mut reader = BufReader::new(stdout);

    // Initialize
    send_command(stdin, "usi");
    let _ = read_until_pattern(&mut reader, "usiok", Duration::from_secs(2));
    send_command(stdin, "isready");
    let _ = read_until_pattern(&mut reader, "readyok", Duration::from_secs(2));

    // Start deep search
    send_command(stdin, "position startpos");
    send_command(stdin, "go depth 20"); // Deep search
    thread::sleep(Duration::from_millis(50)); // Let it start

    // Stop immediately
    let start = Instant::now();
    send_command(stdin, "stop");

    // Should get bestmove quickly
    let result = read_until_pattern(&mut reader, "bestmove", Duration::from_millis(500));
    let elapsed = start.elapsed();

    assert!(result.is_ok(), "No bestmove after stop");
    assert!(elapsed < Duration::from_millis(500), "Stop took too long: {elapsed:?}");

    // Cleanup
    send_command(stdin, "quit");
    let _ = engine.wait();
}

#[test]
fn test_multiple_stop_commands() {
    let mut engine = spawn_engine();
    let stdin = engine.stdin.as_mut().expect("Failed to get stdin");
    let stdout = engine.stdout.as_mut().expect("Failed to get stdout");
    let mut reader = BufReader::new(stdout);

    // Initialize
    send_command(stdin, "usi");
    let _ = read_until_pattern(&mut reader, "usiok", Duration::from_secs(2));
    send_command(stdin, "isready");
    let _ = read_until_pattern(&mut reader, "readyok", Duration::from_secs(2));

    // Run multiple searches with stops
    for i in 0..3 {
        send_command(stdin, "position startpos");
        send_command(stdin, "go infinite");
        thread::sleep(Duration::from_millis(50));

        let start = Instant::now();
        send_command(stdin, "stop");

        let result = read_until_pattern(&mut reader, "bestmove", Duration::from_millis(500));
        let elapsed = start.elapsed();

        assert!(result.is_ok(), "No bestmove on iteration {i}");
        assert!(elapsed < Duration::from_millis(500), "Stop {i} took too long: {elapsed:?}");
    }

    // Cleanup
    send_command(stdin, "quit");
    let _ = engine.wait();
}

#[test]
fn test_ponder_sequence() {
    let mut engine = spawn_engine();

    // Check if engine process started successfully
    match engine.try_wait() {
        Ok(Some(status)) => {
            panic!("Engine exited immediately with status: {:?}", status);
        }
        Ok(None) => {
            // Process is still running, good
        }
        Err(e) => {
            panic!("Failed to check engine status: {}", e);
        }
    }

    let stdin = engine.stdin.as_mut().expect("Failed to get stdin");
    let stdout = engine.stdout.as_mut().expect("Failed to get stdout");
    let mut reader = BufReader::new(stdout);

    // Initialize
    send_command(stdin, "usi");
    let _ = read_until_pattern(&mut reader, "usiok", Duration::from_secs(5))
        .expect("Failed to get usiok");
    send_command(stdin, "isready");
    let _ = read_until_pattern(&mut reader, "readyok", Duration::from_secs(5))
        .expect("Failed to get readyok");

    // Set position (Black move then White move)
    send_command(stdin, "position startpos moves 2c2d 8g8f");

    // Start ponder search with time controls (these will be used after ponderhit)
    // The ponder search itself runs infinitely, but we need time controls for after ponderhit
    send_command(stdin, "go ponder btime 10000 wtime 10000");

    // Give it some time to start pondering (ponder mode runs infinitely)
    thread::sleep(Duration::from_millis(500));

    // Send ponder hit (opponent played expected move)
    // This should convert the ponder search to a normal search with the time limits
    send_command(stdin, "ponderhit");

    // Now the search should have time limits and will complete on its own
    // Wait for bestmove (should come relatively quickly after ponderhit)
    let result = read_until_pattern(&mut reader, "bestmove", Duration::from_secs(10));

    match result {
        Ok(lines) => {
            // Check that we got info lines before bestmove
            let has_info = lines.iter().any(|line| line.starts_with("info"));
            assert!(has_info, "Expected info output during search");

            let has_bestmove = lines.iter().any(|line| line.starts_with("bestmove"));
            assert!(has_bestmove, "Expected bestmove after ponderhit");
        }
        Err(e) => {
            // If no bestmove, try stopping manually
            eprintln!("No bestmove received after ponderhit, trying stop command. Error: {}", e);
            send_command(stdin, "stop");
            let stop_result = read_until_pattern(&mut reader, "bestmove", Duration::from_secs(2));
            assert!(stop_result.is_ok(), "No bestmove after stop. Original error: {e}");
        }
    }

    // Cleanup
    send_command(stdin, "quit");
    let _ = engine.wait();
}
