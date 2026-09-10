use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[test]
fn node_limited_ponder_waits_for_hit_or_stop() {
    for threads in [1, 4] {
        for release in ["ponderhit", "stop"] {
            let mut child = Command::new(assert_cmd::cargo::cargo_bin!("rshogi-usi"))
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            let stdout = child.stdout.take().unwrap();
            let (sender, receiver) = mpsc::channel();
            let reader = std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    if sender.send(line.unwrap()).is_err() {
                        break;
                    }
                }
            });
            let result = (|| -> Result<(), String> {
                let wait_for = |prefix: &str, duration: Duration| -> Result<String, String> {
                    let deadline = Instant::now() + duration;
                    loop {
                        let line = receiver
                            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                            .map_err(|err| err.to_string())?;
                        if line.starts_with(prefix) {
                            return Ok(line);
                        }
                        if line.starts_with("bestmove ") {
                            return Err(format!("early {line}"));
                        }
                    }
                };
                writeln!(child.stdin.as_mut().unwrap(), "usi\nsetoption name MaterialLevel value 9\nsetoption name Threads value {threads}\nsetoption name USI_Hash value 16\nisready").map_err(|err| err.to_string())?;
                wait_for("readyok", Duration::from_secs(10))?;
                writeln!(child.stdin.as_mut().unwrap(), "position startpos\ngo ponder nodes 100")
                    .map_err(|err| err.to_string())?;
                // 探索が起動したことを確認してから無出力期間を検査する。
                wait_for("info depth ", Duration::from_secs(10))?;
                let deadline = Instant::now() + Duration::from_millis(300);
                loop {
                    match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    {
                        Ok(line) if line.starts_with("bestmove ") => {
                            return Err(format!("before {release}: {line}"));
                        }
                        Ok(_) => {}
                        Err(mpsc::RecvTimeoutError::Timeout) => break,
                        Err(err) => return Err(err.to_string()),
                    }
                }
                writeln!(child.stdin.as_mut().unwrap(), "{release}")
                    .map_err(|err| err.to_string())?;
                let bestmove = wait_for("bestmove ", Duration::from_secs(3))?;
                if bestmove.starts_with("bestmove resign") {
                    return Err(bestmove);
                }
                Ok(())
            })();
            if result.is_ok() {
                writeln!(child.stdin.as_mut().unwrap(), "quit").unwrap();
            } else {
                let _ = child.kill();
            }
            let status = child.wait().unwrap();
            reader.join().unwrap();
            result.unwrap_or_else(|err| panic!("threads={threads} release={release}: {err}"));
            assert!(status.success());
        }
    }
}
