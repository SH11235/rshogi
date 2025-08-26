//! Reproduction harness for MoveGen hang issue
//!
//! This test is designed to reproduce and collect evidence for the MoveGen hang
//! that occurs when has_legal_moves() is called in a subprocess context.
//!
//! Features:
//! - Watchdog timer with SIGUSR1 signal for stack dump
//! - Evidence collection (stderr, dumps)
//! - Environment variable matrix testing

#![cfg(unix)] // This test uses Unix-specific signals

use std::env;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const TIMEOUT_SECS: u64 = 5;

/// Test configuration for reproduction
struct HangTestConfig {
    skip_legal_moves: &'static str,
    use_any_legal: &'static str,
    force_flush_stderr: &'static str,
    usi_dry_run: &'static str,
    test_name: &'static str,
}

impl HangTestConfig {
    fn to_env(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("SKIP_LEGAL_MOVES", self.skip_legal_moves),
            ("USE_ANY_LEGAL", self.use_any_legal),
            ("FORCE_FLUSH_STDERR", self.force_flush_stderr),
            ("USI_DRY_RUN", self.usi_dry_run),
            ("RUST_LOG", "trace"),
        ]
    }
}

/// Evidence collected during test
#[derive(Debug)]
struct TestEvidence {
    stdout_lines: Vec<String>,
    stderr_lines: Vec<String>,
    hang_detected: bool,
    signal_sent: bool,
    exit_code: Option<i32>,
    duration: Duration,
}

#[test]
#[ignore = "Manual hang reproduction test - run with --ignored"]
fn test_movegen_hang_reproduction() {
    let configs = vec![
        HangTestConfig {
            skip_legal_moves: "0",
            use_any_legal: "0",
            force_flush_stderr: "1",
            usi_dry_run: "0",
            test_name: "baseline_hang",
        },
        HangTestConfig {
            skip_legal_moves: "0",
            use_any_legal: "1",
            force_flush_stderr: "1",
            usi_dry_run: "0",
            test_name: "any_legal_optimization",
        },
        HangTestConfig {
            skip_legal_moves: "1",
            use_any_legal: "0",
            force_flush_stderr: "1",
            usi_dry_run: "0",
            test_name: "skip_legal_moves_workaround",
        },
    ];

    // Create evidence directory
    let evidence_dir = Path::new("hang_evidence");
    fs::create_dir_all(evidence_dir).unwrap();

    for config in configs {
        println!("\n=== Testing configuration: {} ===", config.test_name);
        let evidence = run_hang_test(&config);
        save_evidence(&evidence, &config, evidence_dir);

        println!("Duration: {:?}", evidence.duration);
        println!("Hang detected: {}", evidence.hang_detected);
        println!("Signal sent: {}", evidence.signal_sent);
        if let Some(code) = evidence.exit_code {
            println!("Exit code: {}", code);
        }
    }
}

fn run_hang_test(config: &HangTestConfig) -> TestEvidence {
    let start = Instant::now();
    let evidence = Arc::new(Mutex::new(TestEvidence {
        stdout_lines: Vec::new(),
        stderr_lines: Vec::new(),
        hang_detected: false,
        signal_sent: false,
        exit_code: None,
        duration: Duration::from_secs(0),
    }));

    // Start engine process with environment variables
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_engine-cli"));
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());

    // Set environment variables
    for (key, value) in config.to_env() {
        cmd.env(key, value);
    }

    let mut engine = cmd.spawn().expect("Failed to spawn engine");
    let engine_pid = engine.id();

    let mut stdin = engine.stdin.take().expect("Failed to get stdin");
    let stdout = engine.stdout.take().expect("Failed to get stdout");
    let stderr = engine.stderr.take().expect("Failed to get stderr");

    // Start stderr reader
    let evidence_stderr = evidence.clone();
    let stderr_handle = thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines() {
            if let Ok(line) = line {
                evidence_stderr.lock().unwrap().stderr_lines.push(line.clone());
                eprintln!("STDERR: {}", line);
            }
        }
    });

    // Start stdout reader
    let evidence_stdout = evidence.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let stdout_handle = thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            if let Ok(line) = line {
                evidence_stdout.lock().unwrap().stdout_lines.push(line.clone());
                println!("ENGINE: {}", line);
                let _ = tx.send(line);
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

    writeln!(stdin, "usinewgame").unwrap();
    stdin.flush().unwrap();
    thread::sleep(Duration::from_millis(100));

    writeln!(stdin, "position startpos").unwrap();
    stdin.flush().unwrap();
    thread::sleep(Duration::from_millis(100));

    println!("Sending 'go depth 1' command...");
    writeln!(stdin, "go depth 1").unwrap();
    stdin.flush().unwrap();

    // Watchdog for hang detection
    let watchdog_evidence = evidence.clone();
    let watchdog_handle = thread::spawn(move || {
        let start = Instant::now();
        let mut bestmove_received = false;

        while start.elapsed() < Duration::from_secs(TIMEOUT_SECS) {
            if let Ok(line) = rx.recv_timeout(Duration::from_millis(100)) {
                if line.starts_with("bestmove") {
                    bestmove_received = true;
                    break;
                }
            }
        }

        if !bestmove_received {
            watchdog_evidence.lock().unwrap().hang_detected = true;

            // Send SIGUSR1 for stack dump (Unix only)
            #[cfg(unix)]
            {
                println!("TIMEOUT: Sending SIGUSR1 for stack dump...");
                unsafe {
                    if libc::kill(engine_pid as i32, libc::SIGUSR1) == 0 {
                        watchdog_evidence.lock().unwrap().signal_sent = true;
                        // Give time for signal handler to dump
                        thread::sleep(Duration::from_millis(500));
                    }
                }
            }
        }

        bestmove_received
    });

    // Wait for watchdog result
    let _bestmove_received = watchdog_handle.join().unwrap();

    // Clean shutdown attempt
    writeln!(stdin, "quit").unwrap();
    stdin.flush().unwrap();
    drop(stdin);

    // Wait briefly for graceful exit
    thread::sleep(Duration::from_millis(500));

    // Staged kill process: try graceful termination first
    match engine.try_wait() {
        Ok(Some(status)) => {
            evidence.lock().unwrap().exit_code = status.code();
        }
        Ok(None) => {
            // Process still running - try SIGTERM first
            println!("Sending SIGTERM to engine process...");
            unsafe {
                libc::kill(engine_pid as i32, libc::SIGTERM);
            }

            // Give time for graceful shutdown
            thread::sleep(Duration::from_millis(1000));

            // Check again
            match engine.try_wait() {
                Ok(Some(status)) => {
                    evidence.lock().unwrap().exit_code = status.code();
                }
                Ok(None) => {
                    // Still running - force kill
                    println!("Force killing engine process with SIGKILL...");
                    let _ = engine.kill();
                    if let Ok(status) = engine.wait() {
                        evidence.lock().unwrap().exit_code = status.code();
                    }
                }
                Err(e) => {
                    eprintln!("Error checking process status after SIGTERM: {}", e);
                    let _ = engine.kill();
                }
            }
        }
        Err(e) => {
            eprintln!("Error checking process status: {}", e);
            let _ = engine.kill();
        }
    }

    // Wait for threads to finish
    let _ = stdout_handle.join();
    let _ = stderr_handle.join();

    // Record duration
    evidence.lock().unwrap().duration = start.elapsed();

    Arc::try_unwrap(evidence).unwrap().into_inner().unwrap()
}

fn save_evidence(evidence: &TestEvidence, config: &HangTestConfig, evidence_dir: &Path) {
    let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let test_dir = evidence_dir.join(format!("{}_{}", config.test_name, timestamp));
    fs::create_dir_all(&test_dir).unwrap();

    // Save configuration with timestamp
    let config_path = test_dir.join(format!("config_{}.txt", timestamp));
    let mut config_file = File::create(config_path).unwrap();
    writeln!(config_file, "Test Name: {}", config.test_name).unwrap();
    writeln!(config_file, "Timestamp: {}", timestamp).unwrap();
    writeln!(config_file, "SKIP_LEGAL_MOVES: {}", config.skip_legal_moves).unwrap();
    writeln!(config_file, "USE_ANY_LEGAL: {}", config.use_any_legal).unwrap();
    writeln!(config_file, "FORCE_FLUSH_STDERR: {}", config.force_flush_stderr).unwrap();
    writeln!(config_file, "USI_DRY_RUN: {}", config.usi_dry_run).unwrap();
    writeln!(config_file, "Duration: {:?}", evidence.duration).unwrap();
    writeln!(config_file, "Hang Detected: {}", evidence.hang_detected).unwrap();
    writeln!(config_file, "Signal Sent: {}", evidence.signal_sent).unwrap();
    if let Some(code) = evidence.exit_code {
        writeln!(config_file, "Exit Code: {}", code).unwrap();
    }

    // Save stdout with timestamp
    let stdout_path = test_dir.join(format!("stdout_{}.log", timestamp));
    let mut stdout_file = File::create(stdout_path).unwrap();
    for line in &evidence.stdout_lines {
        writeln!(stdout_file, "{}", line).unwrap();
    }

    // Save stderr with timestamp
    let stderr_path = test_dir.join(format!("stderr_{}.log", timestamp));
    let mut stderr_file = File::create(stderr_path).unwrap();
    for line in &evidence.stderr_lines {
        writeln!(stderr_file, "{}", line).unwrap();
    }

    println!("Evidence saved to: {}", test_dir.display());
}

#[test]
#[ignore = "Interactive test for SIGUSR1 signal verification"]
fn test_signal_handler_verification() {
    // This test verifies that SIGUSR1 signal handling is working
    println!("Starting engine with SIGUSR1 handler test...");

    let mut engine = Command::new(env!("CARGO_BIN_EXE_engine-cli"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("RUST_LOG", "trace")
        .env("FORCE_FLUSH_STDERR", "1")
        .spawn()
        .expect("Failed to spawn engine");

    let engine_pid = engine.id();
    let mut stdin = engine.stdin.take().unwrap();
    let stderr = engine.stderr.take().unwrap();

    // Capture stderr
    let stderr_handle = thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines() {
            if let Ok(line) = line {
                eprintln!("STDERR: {}", line);
            }
        }
    });

    // Initialize engine
    writeln!(stdin, "usi").unwrap();
    stdin.flush().unwrap();
    thread::sleep(Duration::from_millis(100));

    writeln!(stdin, "isready").unwrap();
    stdin.flush().unwrap();
    thread::sleep(Duration::from_millis(500));

    // Send SIGUSR1
    #[cfg(unix)]
    {
        println!("Sending SIGUSR1 to PID {}...", engine_pid);
        unsafe {
            libc::kill(engine_pid as i32, libc::SIGUSR1);
        }
        thread::sleep(Duration::from_millis(500));
    }

    // Cleanup
    writeln!(stdin, "quit").unwrap();
    stdin.flush().unwrap();
    drop(stdin);

    let _ = engine.wait();
    let _ = stderr_handle.join();
}
