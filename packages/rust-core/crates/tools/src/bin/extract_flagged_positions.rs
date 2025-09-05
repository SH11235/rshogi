use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde_json::Value;

#[cfg(feature = "zstd")]
use zstd::stream::read::Decoder as ZstdDecoder;

fn open_reader<P: AsRef<Path>>(path: P) -> io::Result<Box<dyn BufRead>> {
    let p = path.as_ref();
    if p.to_string_lossy() == "-" {
        return Ok(Box::new(BufReader::new(io::stdin())));
    }
    let f = File::open(p)?;
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    if ext == "gz" {
        let dec = flate2::read::GzDecoder::new(f);
        Ok(Box::new(BufReader::new(dec)))
    } else {
        #[cfg(feature = "zstd")]
        if ext == "zst" {
            let dec = ZstdDecoder::new(f)?;
            return Ok(Box::new(BufReader::new(dec)));
        }
        Ok(Box::new(BufReader::new(f)))
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <input.jsonl|-> [<output.sfens|->] [--gap-threshold <cp>] [--include-non-exact] [--include-aspiration-failures <n>] [--include-mate-boundary]", args[0]);
        std::process::exit(1);
    }

    let input = PathBuf::from(&args[1]);
    // Optional output (stdout if omitted or '-')
    let mut arg_index = 2;
    let output_opt = if args.len() > 2 && !args[2].starts_with("--") {
        arg_index = 3;
        Some(PathBuf::from(&args[2]))
    } else {
        None
    };

    let mut gap_threshold: Option<i64> = None;
    let mut include_non_exact = false;
    let mut include_asp_fail: Option<i64> = None;
    let mut include_mate_boundary = false;

    let mut i = arg_index;
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

    let reader = open_reader(&input)?;
    // Choose output: file or stdout
    let mut out: Box<dyn Write> = match output_opt {
        Some(path) => {
            if path.as_os_str() == "-" {
                Box::new(std::io::stdout())
            } else {
                Box::new(
                    std::fs::OpenOptions::new()
                        .create(true)
                        .truncate(true)
                        .write(true)
                        .open(path)?,
                )
            }
        }
        None => Box::new(std::io::stdout()),
    };

    for (line_idx, line) in reader.lines().enumerate() {
        let line = match line {
            Ok(l) => l,
            Err(e) => { eprintln!("[warn] read error at {}:{} -> {}", input.display(), line_idx + 1, e); continue },
        };
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => { eprintln!("[warn] json parse error at {}:{} -> {}", input.display(), line_idx + 1, e); continue },
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
