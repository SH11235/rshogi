use clap::Parser;
use std::io::{BufRead, Write};
use std::path::PathBuf;

use serde_json::Value;

use tools::common::io::{open_reader, open_writer};

// open_reader moved to tools::common::io

#[derive(Parser, Debug)]
#[command(
    name = "extract_flagged_positions",
    about = "Extract SFENs from JSONL by flags/thresholds.",
    disable_help_subcommand = true
)]
struct Cli {
    /// Input JSONL path or '-' for STDIN
    input: String,
    /// Optional output path for SFENs or '-' for STDOUT (default: STDOUT)
    output: Option<String>,
    /// Include when best2_gap_cp <= threshold (alias: --max-gap-cp)
    #[arg(long = "gap-threshold", alias = "max-gap-cp")]
    gap_threshold: Option<i64>,
    /// Include non-exact records
    #[arg(long)]
    include_non_exact: bool,
    /// Include when aspiration_retries >= N
    #[arg(long = "include-aspiration-failures", value_name = "N")]
    include_aspiration_failures: Option<i64>,
    /// Include when any line has mate_distance
    #[arg(long)]
    include_mate_boundary: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let input = PathBuf::from(&cli.input);
    let output_opt = cli.output.as_ref().map(PathBuf::from);
    let gap_threshold = cli.gap_threshold;
    let include_non_exact = cli.include_non_exact;
    let include_asp_fail = cli.include_aspiration_failures;
    let include_mate_boundary = cli.include_mate_boundary;

    let reader = open_reader(&input)?;
    // Choose output: file or stdout (compressed if extension suggests)
    let mut out: Box<dyn Write> = match output_opt {
        Some(path) => open_writer(&path)?,
        None => Box::new(std::io::stdout()),
    };

    for (line_idx, line) in reader.lines().enumerate() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[warn] read error at {}:{} -> {}", input.display(), line_idx + 1, e);
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!(
                    "[warn] json parse error at {}:{} -> {}",
                    input.display(),
                    line_idx + 1,
                    e
                );
                continue;
            }
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
            let is_exact = |s: &str| s.eq_ignore_ascii_case("exact");
            if matches!(b1, Some(b) if !is_exact(b)) || matches!(b2, Some(b) if !is_exact(b)) {
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
