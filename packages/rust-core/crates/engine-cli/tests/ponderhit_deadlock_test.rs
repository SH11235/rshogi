use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn spawn_engine() -> std::process::Child {
    Command::new("cargo")
        .args(&["run", "--bin", "engine-cli"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("Failed to spawn engine")
}

fn send_command(stdin: &mut std::process::ChildStdin, cmd: &str) {
    writeln!(stdin, "{}", cmd).expect("Failed to write command");
    stdin.flush().expect("Failed to flush stdin");
}

fn read_until_pattern(
    reader: &mut BufReader<&mut std::process::ChildStdout>,
    pattern: &str,
    timeout: Duration,
) -> Result<String, String> {
    let start = Instant::now();
    let mut buffer = String::new();

    while start.elapsed() < timeout {
        buffer.clear();
        match reader.read_line(&mut buffer) {
            Ok(0) => return Err("EOF reached".to_string()),
            Ok(_) => {
                let line = buffer.trim();
                if !line.is_empty() {
                    println!("Engine: {}", line);
                    if line.contains(pattern) {
                        return Ok(line.to_string());
                    }
                }
            }
            Err(e) => return Err(format!("Read error: {}", e)),
        }
    }

    Err(format!("Timeout waiting for pattern: {}", pattern))
}

#[test]
fn test_ponderhit_no_deadlock() {
    let mut engine = spawn_engine();
    let stdin = engine.stdin.as_mut().expect("Failed to get stdin");
    let stdout = engine.stdout.as_mut().expect("Failed to get stdout");
    let mut reader = BufReader::new(stdout);

    // Initialize
    send_command(stdin, "usi");
    let _ = read_until_pattern(&mut reader, "usiok", Duration::from_secs(2));
    send_command(stdin, "isready");
    let _ = read_until_pattern(&mut reader, "readyok", Duration::from_secs(2));

    // Set position
    send_command(stdin, "position startpos");

    // Start a long search with depth limit
    send_command(stdin, "go depth 20 ponder");

    // Give the search time to start and acquire the engine lock
    thread::sleep(Duration::from_millis(50));

    // Send ponderhit while search is running
    // This should NOT deadlock
    let start = Instant::now();
    send_command(stdin, "ponderhit");

    // The ponderhit should be processed quickly without blocking
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_millis(100),
        "PonderHit took too long to process: {:?}, possible deadlock",
        elapsed
    );

    // Give some time for the search to continue
    thread::sleep(Duration::from_millis(100));

    // Stop the search
    send_command(stdin, "stop");

    // Should get bestmove
    let result = read_until_pattern(&mut reader, "bestmove", Duration::from_secs(2));
    assert!(result.is_ok(), "No bestmove after ponderhit");

    // Cleanup
    send_command(stdin, "quit");
    let _ = engine.wait();
}

#[test]
fn test_rapid_ponderhit_sequence() {
    let mut engine = spawn_engine();
    let stdin = engine.stdin.as_mut().expect("Failed to get stdin");
    let stdout = engine.stdout.as_mut().expect("Failed to get stdout");
    let mut reader = BufReader::new(stdout);

    // Initialize
    send_command(stdin, "usi");
    let _ = read_until_pattern(&mut reader, "usiok", Duration::from_secs(2));
    send_command(stdin, "isready");
    let _ = read_until_pattern(&mut reader, "readyok", Duration::from_secs(2));

    // Test multiple rapid ponderhit scenarios
    for i in 0..3 {
        println!("--- Iteration {} ---", i);

        // Set position
        send_command(stdin, "position startpos");

        // Start ponder search
        send_command(stdin, "go ponder");

        // Very short delay
        thread::sleep(Duration::from_millis(10));

        // Send multiple ponderhits rapidly
        for j in 0..3 {
            let start = Instant::now();
            send_command(stdin, "ponderhit");
            let elapsed = start.elapsed();

            assert!(
                elapsed < Duration::from_millis(50),
                "PonderHit {} in iteration {} took too long: {:?}",
                j,
                i,
                elapsed
            );

            thread::sleep(Duration::from_millis(5));
        }

        // Stop and get result
        send_command(stdin, "stop");
        let result = read_until_pattern(&mut reader, "bestmove", Duration::from_secs(1));
        assert!(result.is_ok(), "No bestmove in iteration {}", i);
    }

    // Cleanup
    send_command(stdin, "quit");
    let _ = engine.wait();
}
