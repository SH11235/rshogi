//! Error handling tests for USI engine
//! Tests pipe errors, disconnections, and error recovery

use std::io::Write;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Helper to spawn engine process
fn spawn_engine() -> std::process::Child {
    Command::new(env!("CARGO_BIN_EXE_engine-cli"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("Failed to spawn engine")
}

/// Helper to spawn engine with stdout drain thread
fn spawn_engine_with_drain() -> (std::process::Child, thread::JoinHandle<()>) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_engine-cli"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("Failed to spawn engine");

    // Drain stdout in background to prevent pipe buffer full
    let stdout = child.stdout.take().expect("Failed to get stdout");
    let drain_handle = thread::spawn(move || {
        let _ = std::io::copy(&mut std::io::BufReader::new(stdout), &mut std::io::sink());
    });

    (child, drain_handle)
}

/// Wait for process with timeout
fn wait_with_timeout(
    mut child: std::process::Child,
    timeout: Duration,
) -> std::process::ExitStatus {
    let start = Instant::now();

    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status,
            Ok(None) => {
                if start.elapsed() > timeout {
                    // Timeout - kill the process
                    let _ = child.kill();
                    return child.wait().expect("Failed to wait after kill");
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => panic!("Error waiting for child: {e}"),
        }
    }
}

#[test]
fn test_graceful_shutdown_on_stdin_close() {
    let mut engine = spawn_engine();
    let mut stdin = engine.stdin.take().expect("Failed to get stdin");

    // Send initial commands
    writeln!(stdin, "usi").expect("Failed to write usi");
    stdin.flush().expect("Failed to flush");

    // Give engine time to process
    thread::sleep(Duration::from_millis(100));

    // Close stdin (simulating GUI disconnect)
    drop(stdin);

    // Engine should exit gracefully within timeout
    let exit_status = wait_with_timeout(engine, Duration::from_secs(2));

    // Should exit with status 0 (graceful shutdown)
    assert!(exit_status.success(), "Engine didn't exit gracefully: {exit_status:?}");
}

#[test]
#[cfg(unix)]
fn test_broken_pipe_handling() {
    let mut engine = spawn_engine();
    let mut stdin = engine.stdin.take().expect("Failed to get stdin");
    let _stdout = engine.stdout.take(); // Take but immediately drop to close pipe

    // Send commands
    writeln!(stdin, "usi").expect("Failed to write usi");
    writeln!(stdin, "isready").expect("Failed to write isready");
    stdin.flush().expect("Failed to flush");

    // Give engine time to try writing to broken pipe
    thread::sleep(Duration::from_millis(200));

    // Send quit to trigger response writes
    writeln!(stdin, "quit").expect("Failed to write quit");
    stdin.flush().expect("Failed to flush");

    // Engine should exit (portable exit code 1 instead of Unix-specific 141)
    let exit_status = wait_with_timeout(engine, Duration::from_secs(2));

    // Check exit code - should be 1 (our error exit) or signal termination
    let code = exit_status.code();

    match code {
        Some(0) => {
            // Graceful shutdown is acceptable
            println!("Engine shut down gracefully");
        }
        Some(1) => {
            // Expected error exit (broken pipe)
            println!("Engine exited with error code as expected");
        }
        #[cfg(unix)]
        None => {
            // Terminated by signal
            use std::os::unix::process::ExitStatusExt;
            if let Some(sig) = exit_status.signal() {
                println!("Engine terminated by signal: {sig}");
            }
        }
        other => {
            panic!("Unexpected exit code: {other:?}");
        }
    }
}

#[test]
fn test_engine_handles_invalid_commands() {
    let (mut engine, drain_handle) = spawn_engine_with_drain();
    let mut stdin = engine.stdin.take().expect("Failed to get stdin");

    // Send valid command first
    writeln!(stdin, "usi").expect("Failed to write usi");
    stdin.flush().expect("Failed to flush");

    // Send invalid commands
    writeln!(stdin, "invalid_command").expect("Failed to write invalid");
    writeln!(stdin).expect("Failed to write empty line");
    writeln!(stdin, "   ").expect("Failed to write whitespace");
    writeln!(stdin, "go bananas").expect("Failed to write invalid go");
    stdin.flush().expect("Failed to flush");

    // Engine should still be responsive
    writeln!(stdin, "isready").expect("Failed to write isready");
    stdin.flush().expect("Failed to flush");

    // Give time to process
    thread::sleep(Duration::from_millis(100));

    // Graceful shutdown
    writeln!(stdin, "quit").expect("Failed to write quit");
    stdin.flush().expect("Failed to flush");
    drop(stdin); // Close stdin to trigger EOF

    let exit_status = wait_with_timeout(engine, Duration::from_secs(3));
    assert!(exit_status.success(), "Engine crashed on invalid commands: {exit_status:?}");

    // Wait for drain thread
    let _ = drain_handle.join();
}

#[test]
fn test_engine_survives_rapid_commands() {
    let (mut engine, drain_handle) = spawn_engine_with_drain();
    let mut stdin = engine.stdin.take().expect("Failed to get stdin");

    // Initialize
    writeln!(stdin, "usi").expect("Failed to write usi");
    stdin.flush().expect("Failed to flush");
    thread::sleep(Duration::from_millis(100));

    // Send many commands rapidly
    for i in 0..100 {
        if i % 10 == 0 {
            writeln!(stdin, "isready").expect("Failed to write isready");
        } else {
            writeln!(stdin, "position startpos").expect("Failed to write position");
        }
    }
    stdin.flush().expect("Failed to flush");

    // Engine should handle command flood
    thread::sleep(Duration::from_millis(500));

    // Quit
    writeln!(stdin, "quit").expect("Failed to write quit");
    stdin.flush().expect("Failed to flush");
    drop(stdin); // Close stdin to trigger EOF

    let exit_status = wait_with_timeout(engine, Duration::from_secs(3));
    assert!(exit_status.success(), "Engine crashed under command flood: {exit_status:?}");

    // Wait for drain thread
    let _ = drain_handle.join();
}
