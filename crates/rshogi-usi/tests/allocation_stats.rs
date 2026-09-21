#![cfg(feature = "allocation-stats")]

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[test]
fn repeated_searches_report_all_phases_before_bestmove() {
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
        writeln!(
            child.stdin.as_mut().unwrap(),
            "usi\nsetoption name MaterialLevel value 9\nsetoption name Threads value 2\nisready"
        )
        .map_err(|err| err.to_string())?;
        let receive_until = |prefix: &str| -> Result<Vec<String>, String> {
            let deadline = Instant::now() + Duration::from_secs(60);
            let mut lines = Vec::new();
            loop {
                let line = receiver
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .map_err(|err| err.to_string())?;
                let done = line.starts_with(prefix);
                lines.push(line);
                if done {
                    return Ok(lines);
                }
            }
        };
        receive_until("readyok")?;
        for _ in 0..2 {
            writeln!(child.stdin.as_mut().unwrap(), "position startpos\ngo depth 3")
                .map_err(|err| err.to_string())?;
            let lines = receive_until("bestmove ")?;
            let reports: Vec<_> = lines
                .iter()
                .filter(|line| line.starts_with("info string allocation_events "))
                .collect();
            if reports.len() != 5 {
                return Err(format!("expected five phases: {lines:?}"));
            }
            for (line, phase) in
                reports.iter().zip(["Other", "Setup", "Iteration", "Tree", "Output"])
            {
                if !line.contains(&format!("phase={phase} ")) {
                    return Err(format!("wrong phase: {line}"));
                }
                for field in ["allocated", "deallocated", "object_bytes"] {
                    let value = line
                        .split_whitespace()
                        .find_map(|word| word.strip_prefix(&format!("{field}=")))
                        .ok_or_else(|| format!("missing {field}: {line}"))?;
                    value.parse::<u64>().map_err(|err| err.to_string())?;
                }
            }
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
    result.unwrap();
    assert!(status.success());
}
