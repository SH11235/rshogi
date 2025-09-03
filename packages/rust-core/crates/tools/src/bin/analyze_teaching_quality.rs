use std::fs::File;
use std::io::{BufRead, BufReader};

use serde_json::Value;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!(
            "Usage: {} <input.jsonl> --report <exact-rate|gap-distribution|time-distribution>",
            args[0]
        );
        std::process::exit(1);
    }
    let input = &args[1];
    let mut reports: Vec<String> = Vec::new();
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--report" => {
                if let Some(r) = args.get(i + 1) {
                    reports.push(r.clone());
                    i += 2;
                } else {
                    break;
                }
            }
            other => {
                eprintln!("Unknown option: {}", other);
                std::process::exit(1);
            }
        }
    }

    let f = File::open(input)?;
    let reader = BufReader::new(f);

    let mut total = 0usize;
    let mut both_exact = 0usize;
    let mut gaps: Vec<i64> = Vec::new();
    let mut times: Vec<i64> = Vec::new();

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
        total += 1;
        let b1 = v.get("bound1").and_then(|x| x.as_str()).unwrap_or("");
        let b2 = v.get("bound2").and_then(|x| x.as_str()).unwrap_or("");
        if b1 == "Exact" && b2 == "Exact" {
            both_exact += 1;
        }
        if let Some(g) = v.get("best2_gap_cp").and_then(|x| x.as_i64()) {
            gaps.push(g);
        }
        if let Some(t) = v.get("time_ms").and_then(|x| x.as_i64()) {
            times.push(t);
        }
    }

    for r in reports {
        match r.as_str() {
            "exact-rate" => {
                let rate = if total > 0 {
                    both_exact as f64 / total as f64
                } else {
                    0.0
                };
                println!("exact_rate: {:.3} ({}/{})", rate, both_exact, total);
            }
            "gap-distribution" => {
                if gaps.is_empty() {
                    println!("gap_distribution: no-data");
                    continue;
                }
                let avg = gaps.iter().sum::<i64>() as f64 / gaps.len() as f64;
                let max = *gaps.iter().max().unwrap();
                let min = *gaps.iter().min().unwrap();
                println!(
                    "gap_distribution: count={} min={} max={} avg={:.1}",
                    gaps.len(),
                    min,
                    max,
                    avg
                );
            }
            "time-distribution" => {
                if times.is_empty() {
                    println!("time_distribution: no-data");
                    continue;
                }
                let avg = times.iter().sum::<i64>() as f64 / times.len() as f64;
                let max = *times.iter().max().unwrap();
                let min = *times.iter().min().unwrap();
                println!(
                    "time_distribution: count={} min={}ms max={}ms avg={:.1}ms",
                    times.len(),
                    min,
                    max,
                    avg
                );
            }
            other => println!("Unknown report: {}", other),
        }
    }

    Ok(())
}
