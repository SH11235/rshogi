use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use serde_json::Value;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: {} <input.jsonl> <output.sfens> [--gap-threshold <cp>] [--include-non-exact] [--include-aspiration-failures <n>] [--include-mate-boundary]", args[0]);
        std::process::exit(1);
    }

    let input = PathBuf::from(&args[1]);
    let output = PathBuf::from(&args[2]);

    let mut gap_threshold: Option<i64> = None;
    let mut include_non_exact = false;
    let mut include_asp_fail: Option<i64> = None;
    let mut include_mate_boundary = false;

    let mut i = 3;
    while i < args.len() {
        match args[i].as_str() {
            "--gap-threshold" => {
                gap_threshold = args.get(i + 1).and_then(|s| s.parse::<i64>().ok());
                i += 2;
            }
            "--include-non-exact" => {
                include_non_exact = true;
                i += 1;
            }
            "--include-aspiration-failures" => {
                include_asp_fail = args.get(i + 1).and_then(|s| s.parse::<i64>().ok());
                i += 2;
            }
            "--include-mate-boundary" => {
                include_mate_boundary = true;
                i += 1;
            }
            other => {
                eprintln!("Unknown option: {}", other);
                std::process::exit(1);
            }
        }
    }

    let f = File::open(input)?;
    let reader = BufReader::new(f);
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(output)?;

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let sfen = v.get("sfen").and_then(|x| x.as_str());
        if sfen.is_none() {
            continue;
        }
        let sfen = sfen.unwrap();

        let mut flag = false;
        // Gap threshold condition
        if let Some(th) = gap_threshold {
            if let Some(g) = v.get("best2_gap_cp").and_then(|x| x.as_i64()) {
                if g <= th {
                    flag = true;
                }
            }
        }
        // Non-exact condition
        if include_non_exact {
            let b1 = v.get("bound1").and_then(|x| x.as_str());
            let b2 = v.get("bound2").and_then(|x| x.as_str());
            if matches!(b1, Some(b) if b != "Exact") || matches!(b2, Some(b) if b != "Exact") {
                flag = true;
            }
        }
        // Aspiration failures
        if let Some(min_fail) = include_asp_fail {
            if let Some(retries) = v.get("aspiration_retries").and_then(|x| x.as_i64()) {
                if retries >= min_fail {
                    flag = true;
                }
            }
        }
        // Mate boundary
        if include_mate_boundary {
            if let Some(lines) = v.get("lines").and_then(|x| x.as_array()) {
                if lines.iter().any(|l| l.get("mate_distance").and_then(|m| m.as_i64()).is_some()) {
                    flag = true;
                }
            }
        }

        if flag {
            writeln!(out, "sfen {}", sfen)?;
        }
    }

    Ok(())
}
