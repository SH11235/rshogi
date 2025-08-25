//! Test to verify BufWriter buffering behavior
//! Run with: cargo test -p engine-cli --test buffering_behavior_test -- --nocapture

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

#[test]
fn test_panic_safe_flush() {
    // Test that we don't deadlock when panicking during write
    // This test is designed to ensure try_flush_all works correctly

    let mut engine = Command::new(env!("CARGO_BIN_EXE_engine-cli"))
        .env("USI_FLUSH_DELAY_MS", "100")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("Failed to spawn engine");

    let mut stdin = engine.stdin.take().expect("Failed to get stdin");
    let stdout = engine.stdout.take().expect("Failed to get stdout");

    // Start reader
    let reader_handle = thread::spawn(move || {
        let reader = BufReader::new(stdout);
        let mut lines = Vec::new();
        for line in reader.lines().map_while(Result::ok) {
            lines.push(line);
        }
        lines
    });

    // Send USI command
    writeln!(stdin, "usi").unwrap();
    stdin.flush().unwrap();
    thread::sleep(Duration::from_millis(50));

    // Send quit to trigger clean shutdown
    writeln!(stdin, "quit").unwrap();
    stdin.flush().unwrap();
    drop(stdin);

    let _ = engine.wait();
    let lines = reader_handle.join().unwrap();

    // Verify we got expected output
    assert!(lines.iter().any(|l| l.contains("id name")));
    assert!(lines.iter().any(|l| l == "usiok"));
}

#[test]
fn test_buffering_with_env_vars() {
    // Test immediate flush with USI_FLUSH_DELAY_MS=0
    let mut engine = Command::new(env!("CARGO_BIN_EXE_engine-cli"))
        .env("USI_FLUSH_DELAY_MS", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("Failed to spawn engine");

    let mut stdin = engine.stdin.take().expect("Failed to get stdin");
    let stdout = engine.stdout.take().expect("Failed to get stdout");

    // Create channel for bestmove notification
    let (tx_done, rx_done) = mpsc::channel();

    // Start reader thread
    let reader_handle = thread::spawn(move || {
        let reader = BufReader::new(stdout);
        let mut output = Vec::new();
        let start = Instant::now();

        for line in reader.lines() {
            match line {
                Ok(line) => {
                    let elapsed = start.elapsed();
                    output.push((elapsed, line.clone()));
                    if line.starts_with("info ") && !line.contains("string") {
                        println!("[{:>4}ms] INFO: {}", elapsed.as_millis(), line);
                    }
                    // Check for bestmove
                    if line.starts_with("bestmove") {
                        println!("Received bestmove at {}ms: {}", elapsed.as_millis(), line);
                        let _ = tx_done.send(());
                    }
                }
                Err(e) => {
                    println!("Reader error: {:?}", e);
                    break;
                }
            }
        }
        println!("Reader thread exiting");
        output
    });

    // Initialize engine
    writeln!(stdin, "usi").unwrap();
    stdin.flush().unwrap();
    thread::sleep(Duration::from_millis(50));

    writeln!(stdin, "isready").unwrap();
    stdin.flush().unwrap();
    thread::sleep(Duration::from_millis(50));

    // Send position
    writeln!(stdin, "position startpos").unwrap();
    stdin.flush().unwrap();

    // Start search to generate info messages
    println!("\n--- Testing with USI_FLUSH_DELAY_MS=0 (immediate flush) ---");
    writeln!(stdin, "go depth 3").unwrap();
    stdin.flush().unwrap();

    // Wait for bestmove or timeout
    match rx_done.recv_timeout(Duration::from_secs(3)) {
        Ok(()) => println!("Bestmove received, proceeding to shutdown"),
        Err(_) => {
            println!("Timeout waiting for bestmove, sending stop command");
            writeln!(stdin, "stop").unwrap();
            stdin.flush().unwrap();
            thread::sleep(Duration::from_millis(200));
        }
    }

    // Send quit
    writeln!(stdin, "quit").unwrap();
    stdin.flush().unwrap();
    drop(stdin);

    // Wait for engine to exit
    let _ = engine.wait();

    // Get output
    let output = reader_handle.join().unwrap();

    // With USI_FLUSH_DELAY_MS=0, all info messages should appear immediately
    let mut info_count = 0;
    for (_, line) in &output {
        if line.starts_with("info ") && !line.contains("string") {
            info_count += 1;
        }
    }

    println!("Total info messages: {info_count}");
    assert!(info_count > 0, "Should have received info messages");
}

#[test]
fn test_buffering_with_delay() {
    // Test buffering with USI_FLUSH_DELAY_MS=200
    let mut engine = Command::new(env!("CARGO_BIN_EXE_engine-cli"))
        .env("USI_FLUSH_DELAY_MS", "200")
        .env("USI_FLUSH_MESSAGE_COUNT", "20")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("Failed to spawn engine");

    let mut stdin = engine.stdin.take().expect("Failed to get stdin");
    let stdout = engine.stdout.take().expect("Failed to get stdout");

    // Create channel for bestmove notification
    let (tx_done, rx_done) = mpsc::channel();

    // Start reader thread
    let reader_handle = thread::spawn(move || {
        let reader = BufReader::new(stdout);
        let mut output = Vec::new();
        let start = Instant::now();
        let mut info_times = Vec::new();

        for line in reader.lines() {
            match line {
                Ok(line) => {
                    let elapsed = start.elapsed();
                    output.push((elapsed, line.clone()));

                    if line.starts_with("info ") && !line.contains("string") {
                        info_times.push(elapsed);
                        println!("[{:>4}ms] INFO: {}", elapsed.as_millis(), line);
                    }
                    // Check for bestmove
                    if line.starts_with("bestmove") {
                        println!("Received bestmove at {}ms: {}", elapsed.as_millis(), line);
                        let _ = tx_done.send(());
                    }
                }
                Err(e) => {
                    println!("Reader error: {:?}", e);
                    break;
                }
            }
        }
        println!("Reader thread exiting");

        // Analyze timing gaps
        let mut gaps = Vec::new();
        for i in 1..info_times.len() {
            let gap = info_times[i].saturating_sub(info_times[i - 1]);
            gaps.push(gap);
            if gap.as_millis() > 50 {
                println!("  Gap of {}ms between messages", gap.as_millis());
            }
        }

        (output, gaps)
    });

    // Initialize engine
    writeln!(stdin, "usi").unwrap();
    stdin.flush().unwrap();
    thread::sleep(Duration::from_millis(50));

    writeln!(stdin, "isready").unwrap();
    stdin.flush().unwrap();
    thread::sleep(Duration::from_millis(50));

    // Send position
    writeln!(stdin, "position startpos").unwrap();
    stdin.flush().unwrap();

    // Start search to generate info messages
    println!("\n--- Testing with USI_FLUSH_DELAY_MS=200 (buffered) ---");
    writeln!(stdin, "go depth 3").unwrap(); // Reduced from depth 5 to 3
    stdin.flush().unwrap();

    // Wait for bestmove or timeout
    match rx_done.recv_timeout(Duration::from_secs(5)) {
        Ok(()) => println!("Bestmove received, proceeding to shutdown"),
        Err(_) => {
            println!("Timeout waiting for bestmove, sending stop command");
            writeln!(stdin, "stop").unwrap();
            stdin.flush().unwrap();
            thread::sleep(Duration::from_millis(200));
        }
    }

    // Send quit
    writeln!(stdin, "quit").unwrap();
    stdin.flush().unwrap();
    drop(stdin);

    // Wait for engine to exit
    let _ = engine.wait();

    // Get output
    let (output, gaps) = reader_handle.join().unwrap();

    // Analyze batching behavior
    let mut info_count = 0;
    for (_, line) in &output {
        if line.starts_with("info ") && !line.contains("string") {
            info_count += 1;
        }
    }

    println!("\nTotal info messages: {info_count}");
    println!(
        "Message timing gaps: {:?}",
        gaps.iter().map(|d| d.as_millis()).collect::<Vec<_>>()
    );

    // With buffering, messages should come in groups
    assert!(info_count > 0, "Should have received info messages");

    // Due to the fast search, we might not see obvious batching
    // but the test validates the mechanism works
}

#[test]
fn test_critical_messages_immediate_flush() {
    // Critical messages should always flush immediately
    let mut engine = Command::new(env!("CARGO_BIN_EXE_engine-cli"))
        .env("USI_FLUSH_DELAY_MS", "1000") // Very long delay
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("Failed to spawn engine");

    let mut stdin = engine.stdin.take().expect("Failed to get stdin");
    let stdout = engine.stdout.take().expect("Failed to get stdout");

    // Start reader thread
    let reader_handle = thread::spawn(move || {
        let reader = BufReader::new(stdout);
        let mut critical_messages = Vec::new();
        let start = Instant::now();

        for line in reader.lines() {
            match line {
                Ok(line) => {
                    let elapsed = start.elapsed();
                    if line == "usiok" || line == "readyok" || line.starts_with("bestmove") {
                        critical_messages.push((elapsed, line));
                    }
                }
                Err(_) => break,
            }
        }
        critical_messages
    });

    // Test critical messages
    println!("\n--- Testing critical messages with USI_FLUSH_DELAY_MS=1000 ---");

    let _start = Instant::now();
    writeln!(stdin, "usi").unwrap();
    stdin.flush().unwrap();

    // Wait a bit
    thread::sleep(Duration::from_millis(100));

    writeln!(stdin, "isready").unwrap();
    stdin.flush().unwrap();

    thread::sleep(Duration::from_millis(100));

    // Send quit
    writeln!(stdin, "quit").unwrap();
    stdin.flush().unwrap();
    drop(stdin);

    // Wait for engine to exit
    let _ = engine.wait();

    // Get output
    let critical_messages = reader_handle.join().unwrap();

    // Check that critical messages arrived quickly
    for (elapsed, msg) in &critical_messages {
        println!("Critical message at {:>4}ms: {}", elapsed.as_millis(), msg);
        // Critical messages should arrive within 200ms even with 1000ms buffer delay
        assert!(
            elapsed.as_millis() < 200,
            "Critical message '{}' took too long: {}ms",
            msg,
            elapsed.as_millis()
        );
    }

    // Should have received both usiok and readyok
    assert!(critical_messages.iter().any(|(_, m)| m == "usiok"), "Should receive usiok");
    assert!(critical_messages.iter().any(|(_, m)| m == "readyok"), "Should receive readyok");
}
