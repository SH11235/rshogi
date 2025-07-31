use std::process::{Command, Stdio};
use std::io::{Write, BufRead, BufReader};
use std::time::Duration;
use std::thread;

fn main() {
    println!("Starting test...");
    
    // Start the engine
    let mut child = Command::new("cargo")
        .args(&["run", "--bin", "engine-cli", "--", "--debug"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to start engine");
    
    let mut stdin = child.stdin.take().expect("Failed to open stdin");
    let stdout = child.stdout.take().expect("Failed to open stdout");
    let stderr = child.stderr.take().expect("Failed to open stderr");
    
    // Read stderr in a separate thread
    let stderr_handle = thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines() {
            if let Ok(line) = line {
                eprintln!("[STDERR] {}", line);
            }
        }
    });
    
    // Read stdout in a separate thread
    let stdout_handle = thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            if let Ok(line) = line {
                println!("[ENGINE] {}", line);
            }
        }
    });
    
    // Send USI commands
    println!("Sending: usi");
    writeln!(stdin, "usi").unwrap();
    thread::sleep(Duration::from_millis(500));
    
    println!("Sending: isready");
    writeln!(stdin, "isready").unwrap();
    thread::sleep(Duration::from_millis(500));
    
    println!("Sending: position startpos");
    writeln!(stdin, "position startpos").unwrap();
    thread::sleep(Duration::from_millis(500));
    
    println!("Sending: go depth 5");
    writeln!(stdin, "go depth 5").unwrap();
    
    // Wait for response
    thread::sleep(Duration::from_secs(3));
    
    println!("Sending: quit");
    writeln!(stdin, "quit").unwrap();
    
    // Wait a bit more
    thread::sleep(Duration::from_millis(500));
    
    // Clean up
    drop(stdin);
    let _ = child.wait();
    
    println!("Test completed.");
}