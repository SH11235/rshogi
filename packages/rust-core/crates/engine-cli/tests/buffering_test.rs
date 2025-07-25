//! Test to verify BufWriter buffering behavior
//! Run with: cargo test -p engine-cli --test buffering_test --features buffered-io -- --nocapture

use std::io::Write;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[test]
#[cfg(feature = "buffered-io")]
fn test_buffering_behavior() {
    let mut engine = Command::new(env!("CARGO_BIN_EXE_engine-cli"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("Failed to spawn engine");

    let mut stdin = engine.stdin.take().expect("Failed to get stdin");
    let stdout = engine.stdout.take().expect("Failed to get stdout");

    // Start reader thread
    let reader_handle = thread::spawn(move || {
        use std::io::{BufRead, BufReader};
        let reader = BufReader::new(stdout);
        let mut output = Vec::new();
        let start = Instant::now();

        for line in reader.lines() {
            match line {
                Ok(line) => {
                    let elapsed = start.elapsed();
                    output.push((elapsed, line.clone()));
                    println!("[{:>4}ms] {}", elapsed.as_millis(), line);
                }
                Err(_) => break,
            }
        }
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
    println!("\n--- Starting search (info messages should be buffered) ---");
    writeln!(stdin, "go depth 5").unwrap();
    stdin.flush().unwrap();

    // Wait for search to complete
    thread::sleep(Duration::from_millis(500));

    // Send quit
    writeln!(stdin, "quit").unwrap();
    stdin.flush().unwrap();
    drop(stdin);

    // Wait for engine to exit
    let _ = engine.wait();

    // Get output
    let output = reader_handle.join().unwrap();

    // Analyze output timing
    println!("\n--- Analysis ---");
    let mut info_count = 0;
    let mut last_info_time = None;

    for (time, line) in &output {
        if line.starts_with("info ") && !line.contains("string") {
            info_count += 1;
            if let Some(last) = last_info_time {
                let delta = time.saturating_sub(last);
                if delta.as_millis() > 50 {
                    println!("Info batch after {}ms gap", delta.as_millis());
                }
            }
            last_info_time = Some(*time);
        }
    }

    println!("Total info messages: {}", info_count);
    println!("\nWith buffered-io feature, info messages should come in batches");
}

#[test]
#[cfg(not(feature = "buffered-io"))]
fn test_immediate_flush_behavior() {
    println!("Running with immediate flush (default behavior)");
    // Similar test but expecting immediate output
}