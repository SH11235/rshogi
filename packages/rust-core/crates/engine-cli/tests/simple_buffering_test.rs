//! Simple test to debug buffering issues

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

#[test]
fn test_simple_search() {
    println!("Starting simple search test");

    let mut engine = Command::new(env!("CARGO_BIN_EXE_engine-cli"))
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
        let mut lines = Vec::new();

        println!("Reader thread started");
        for line in reader.lines() {
            match line {
                Ok(line) => {
                    println!("Received: {}", line);
                    lines.push(line.clone());
                    if line.starts_with("bestmove") {
                        println!("Got bestmove: {}", line);
                        let _ = tx_done.send(());
                    }
                }
                Err(e) => {
                    println!("Read error: {:?}", e);
                    break;
                }
            }
        }
        println!("Reader thread exiting");
        lines
    });

    // Send commands
    println!("Sending usi");
    writeln!(stdin, "usi").unwrap();
    stdin.flush().unwrap();

    thread::sleep(Duration::from_millis(100));

    println!("Sending isready");
    writeln!(stdin, "isready").unwrap();
    stdin.flush().unwrap();

    thread::sleep(Duration::from_millis(100));

    println!("Sending position");
    writeln!(stdin, "position startpos").unwrap();
    stdin.flush().unwrap();

    thread::sleep(Duration::from_millis(100));

    println!("Sending go depth 1");
    writeln!(stdin, "go depth 1").unwrap();
    stdin.flush().unwrap();

    // Wait for bestmove or timeout
    println!("Waiting for bestmove...");
    match rx_done.recv_timeout(Duration::from_secs(5)) {
        Ok(()) => println!("Bestmove received, proceeding to shutdown"),
        Err(_) => {
            println!("Timeout waiting for bestmove, sending stop");
            writeln!(stdin, "stop").unwrap();
            stdin.flush().unwrap();
            thread::sleep(Duration::from_millis(200));
        }
    }

    println!("Sending quit");
    writeln!(stdin, "quit").unwrap();
    stdin.flush().unwrap();
    drop(stdin);

    println!("Waiting for engine to exit");
    let _ = engine.wait();

    println!("Joining reader thread");
    let lines = reader_handle.join().unwrap();

    println!("\nReceived {} lines total", lines.len());

    // Check we got expected responses
    assert!(lines.iter().any(|l| l == "usiok"), "Should receive usiok");
    assert!(lines.iter().any(|l| l == "readyok"), "Should receive readyok");
    assert!(lines.iter().any(|l| l.starts_with("bestmove")), "Should receive bestmove");
}
