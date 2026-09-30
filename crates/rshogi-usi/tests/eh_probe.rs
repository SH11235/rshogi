use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[test]
fn hidden_eval_hash_probe_option_switches_between_searches() {
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
        let receive_until = |prefix: &str| -> Result<Vec<String>, String> {
            let deadline = Instant::now() + Duration::from_secs(30);
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
        writeln!(child.stdin.as_mut().unwrap(), "usi").map_err(|err| err.to_string())?;
        if receive_until("usiok")?.iter().any(|line| line.contains("EhProbeOnTtMiss")) {
            return Err("screening option must be hidden".into());
        }
        writeln!(
            child.stdin.as_mut().unwrap(),
            "setoption name USI_Hash value 1\nsetoption name EvalHash value 1\n\
             setoption name MaterialLevel value 1\nsetoption name UseEvalHash value true\nisready"
        )
        .map_err(|err| err.to_string())?;
        receive_until("readyok")?;
        let mut baseline = None;
        // 既定値、同一プロセス内での切替、ワーカー再利用を確認する。
        for option in [None, Some(1), Some(0)] {
            if let Some(option) = option {
                writeln!(
                    child.stdin.as_mut().unwrap(),
                    "setoption name EhProbeOnTtMiss value {option}"
                )
                .map_err(|err| err.to_string())?;
            }
            writeln!(child.stdin.as_mut().unwrap(), "usinewgame\nisready")
                .map_err(|err| err.to_string())?;
            receive_until("readyok")?;
            writeln!(child.stdin.as_mut().unwrap(), "position startpos\ngo depth 3")
                .map_err(|err| err.to_string())?;
            let lines = receive_until("bestmove ")?;
            let bestmove = lines.last().unwrap().clone();
            let info = lines
                .iter()
                .rev()
                .find(|line| line.starts_with("info depth 3 "))
                .ok_or_else(|| format!("depth 3 did not finish: {lines:?}"))?;
            let fields: Vec<_> = info.split_whitespace().collect();
            let value_after = |name| {
                fields
                    .iter()
                    .position(|&field| field == name)
                    .and_then(|index| fields.get(index + 1))
                    .copied()
                    .ok_or_else(|| format!("missing {name}: {info}"))
            };
            let outcome =
                (bestmove, value_after("cp")?.to_string(), value_after("nodes")?.to_string());
            if let Some(ref baseline) = baseline {
                if baseline != &outcome {
                    return Err(format!("search changed: {baseline:?} != {outcome:?}"));
                }
            } else {
                baseline = Some(outcome);
            }
            #[cfg(feature = "search-stats")]
            for site in ["search", "qsearch"] {
                for tt in ["miss", "hit"] {
                    let prefix = format!("info string EvalHash {site} tt_{tt}: ");
                    let line = lines
                        .iter()
                        .find_map(|line| line.strip_prefix(&prefix))
                        .ok_or_else(|| format!("missing {prefix}"))?;
                    let counts: Vec<u64> = line
                        .split_whitespace()
                        .map(|field| field.split_once('=').unwrap().1.parse().unwrap())
                        .collect();
                    let [probes, hits, skipped] = counts[..] else {
                        return Err(format!("invalid counters: {line}"));
                    };
                    if hits > probes || (tt == "hit" && skipped != 0) {
                        return Err(format!("invalid counters: {line}"));
                    }
                    if tt == "miss"
                        && if option == Some(1) {
                            probes == 0 || skipped != 0
                        } else {
                            probes != 0 || skipped == 0
                        }
                    {
                        return Err(format!("wrong probe policy: {line}"));
                    }
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
