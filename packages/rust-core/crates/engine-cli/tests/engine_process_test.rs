//! Test through actual engine process

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

#[test]
fn test_engine_process_with_has_legal_moves() {
    // Start engine process
    let mut engine = Command::new(env!("CARGO_BIN_EXE_engine-cli"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn engine");

    let mut stdin = engine.stdin.take().expect("Failed to get stdin");
    let stdout = engine.stdout.take().expect("Failed to get stdout");
    let stderr = engine.stderr.take().expect("Failed to get stderr");

    // Start stderr reader to capture error logs
    let _stderr_handle = thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines() {
            if let Ok(line) = line {
                eprintln!("STDERR: {}", line);
            }
        }
    });

    // Start stdout reader
    let (tx, rx) = std::sync::mpsc::channel();
    let _stdout_handle = thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            if let Ok(line) = line {
                println!("ENGINE: {}", line);
                if tx.send(line).is_err() {
                    break; // Receiver dropped
                }
            }
        }
    });

    // Send USI commands
    writeln!(stdin, "usi").unwrap();
    stdin.flush().unwrap();
    thread::sleep(Duration::from_millis(100));

    writeln!(stdin, "isready").unwrap();
    stdin.flush().unwrap();
    thread::sleep(Duration::from_millis(100));

    writeln!(stdin, "position startpos").unwrap();
    stdin.flush().unwrap();
    thread::sleep(Duration::from_millis(100));

    // This is where the hang occurs
    println!("Sending 'go depth 1' command...");
    writeln!(stdin, "go depth 1").unwrap();
    stdin.flush().unwrap();

    // Wait for bestmove with timeout
    let start = std::time::Instant::now();
    let mut bestmove_received = false;

    while start.elapsed() < Duration::from_secs(3) {
        if let Ok(line) = rx.recv_timeout(Duration::from_millis(100)) {
            if line.starts_with("bestmove") {
                bestmove_received = true;
                break;
            }
        }
    }

    if !bestmove_received {
        println!("TIMEOUT: No bestmove received within 3 seconds");

        // Try sending stop
        println!("Sending stop command...");
        writeln!(stdin, "stop").unwrap();
        stdin.flush().unwrap();
        thread::sleep(Duration::from_millis(500));
    }

    // Send quit
    writeln!(stdin, "quit").unwrap();
    stdin.flush().unwrap();
    drop(stdin);

    // Wait for process to exit
    thread::sleep(Duration::from_secs(1));

    // Kill if still running
    let _ = engine.kill();

    assert!(bestmove_received, "Engine should return bestmove");
}
