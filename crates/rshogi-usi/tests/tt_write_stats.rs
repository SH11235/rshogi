#![cfg(feature = "tt-write-stats")]

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[test]
fn repeated_searches_report_tt_write_partitions_before_bestmove() {
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
                .filter(|line| line.starts_with("info string tt_write_events "))
                .collect();
            if reports.len() != 1 {
                return Err(format!("expected one TT report: {lines:?}"));
            }
            let fields: std::collections::HashMap<_, _> = reports[0]
                .split_whitespace()
                .skip(3)
                .map(|part| {
                    let (key, value) =
                        part.split_once('=').ok_or_else(|| format!("bad field: {part}"))?;
                    Ok((key, value.parse::<u64>().map_err(|err| err.to_string())?))
                })
                .collect::<Result<_, String>>()?;
            let get = |key| fields.get(key).copied().ok_or_else(|| format!("missing {key}"));
            let attempts = get("attempts")?;
            if attempts == 0
                || get("empty")? + get("same_key16")? + get("different_key16")? != attempts
                || get("payload_accepted")? + get("payload_retained")? != attempts
                || get("move_only_changed")? + get("unchanged")? != get("payload_retained")?
                || get("same_key16_same_depth_accepted")? > get("payload_accepted")?
            {
                return Err(format!("inconsistent TT report: {reports:?}"));
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
