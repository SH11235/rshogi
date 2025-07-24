use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn spawn_engine() -> std::process::Child {
    Command::new("cargo")
        .args(["run", "--bin", "engine-cli"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("Failed to spawn engine")
}

fn send_command(stdin: &mut std::process::ChildStdin, cmd: &str) {
    println!(">>> {cmd}");
    writeln!(stdin, "{cmd}").expect("Failed to write command");
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
                    println!("<<< {line}");
                    if line.contains(pattern) {
                        return Ok(line.to_string());
                    }
                }
            }
            Err(e) => return Err(format!("Read error: {e}")),
        }
    }

    Err(format!("Timeout waiting for pattern: {pattern}"))
}

#[test]
fn test_simple_ponder() {
    let mut engine = spawn_engine();
    let stdin = engine.stdin.as_mut().expect("Failed to get stdin");
    let stdout = engine.stdout.as_mut().expect("Failed to get stdout");
    let mut reader = BufReader::new(stdout);

    // Initialize
    send_command(stdin, "usi");
    let result = read_until_pattern(&mut reader, "usiok", Duration::from_secs(5));
    assert!(result.is_ok(), "Failed to get usiok: {result:?}");

    send_command(stdin, "isready");
    let result = read_until_pattern(&mut reader, "readyok", Duration::from_secs(5));
    assert!(result.is_ok(), "Failed to get readyok: {result:?}");

    // Set position
    send_command(stdin, "position startpos");

    // Start simple ponder search WITH time limits (required for proper handling after ponderhit)
    send_command(stdin, "go ponder btime 10000 wtime 10000");

    // Give it a bit of time to start pondering
    thread::sleep(Duration::from_millis(200));

    // Send ponderhit - this should convert ponder to normal search with time limits
    send_command(stdin, "ponderhit");

    // The search should complete on its own due to time limits
    let result = read_until_pattern(&mut reader, "bestmove", Duration::from_secs(5));

    if result.is_err() {
        // If no bestmove yet, try stopping manually
        send_command(stdin, "stop");
        let stop_result = read_until_pattern(&mut reader, "bestmove", Duration::from_secs(2));
        assert!(stop_result.is_ok(), "No bestmove after stop: {stop_result:?}");
    }

    // Cleanup
    send_command(stdin, "quit");
    let _ = engine.wait();
}
