use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};

use serde_json::Value;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!(
            "Usage: {} <input1.jsonl> <...> <output.jsonl> [--dedup-by-sfen] [--prefer-deeper]",
            args[0]
        );
        std::process::exit(1);
    }

    let mut input_paths = Vec::new();
    let mut dedup_by_sfen = false;
    let mut prefer_deeper = false;

    // Collect until we hit an option; last non-option is output
    let mut i = 1;
    while i < args.len() && !args[i].starts_with('-') {
        input_paths.push(args[i].clone());
        i += 1;
    }
    if input_paths.len() < 2 {
        eprintln!("Need at least one input and one output");
        std::process::exit(1);
    }
    let output_path = input_paths.pop();

    while i < args.len() {
        match args[i].as_str() {
            "--dedup-by-sfen" => {
                dedup_by_sfen = true;
                i += 1;
            }
            "--prefer-deeper" => {
                prefer_deeper = true;
                i += 1;
            }
            other => {
                eprintln!("Unknown option: {}", other);
                std::process::exit(1);
            }
        }
    }

    let mut map: HashMap<String, Value> = HashMap::new();

    for path in input_paths {
        let f = File::open(&path)?;
        let reader = BufReader::new(f);
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
            if !dedup_by_sfen {
                // Just append later; store with unique key
                let key = format!("{}#{}", path, map.len());
                map.insert(key, v);
                continue;
            }
            if let Some(sfen) = v.get("sfen").and_then(|x| x.as_str()) {
                if let Some(prev) = map.get(sfen) {
                    if prefer_deeper {
                        let d_prev = prev.get("depth").and_then(|x| x.as_i64()).unwrap_or(0);
                        let d_new = v.get("depth").and_then(|x| x.as_i64()).unwrap_or(0);
                        if d_new > d_prev {
                            map.insert(sfen.to_string(), v);
                        }
                    } else {
                        // Prefer latter occurrence by default
                        map.insert(sfen.to_string(), v);
                    }
                } else {
                    map.insert(sfen.to_string(), v);
                }
            }
        }
    }

    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(output_path.unwrap())?;
    if dedup_by_sfen {
        for (_k, v) in map {
            writeln!(out, "{}", v)?;
        }
    } else {
        // If not deduping, the map contains synthetic keys; just dump values
        for (_k, v) in map {
            writeln!(out, "{}", v)?;
        }
    }

    Ok(())
}
