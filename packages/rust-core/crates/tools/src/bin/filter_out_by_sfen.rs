use clap::Parser;
use serde_json::Value as JsonValue;
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use tools::io_detect::open_maybe_compressed_reader;

#[derive(Parser, Debug)]
#[command(
    name = "filter_out_by_sfen",
    about = "Filter out JSONL lines whose sfen is in the provided SFEN list"
)]
struct Cli {
    /// Input JSONL (.jsonl[.gz|.zst])
    #[arg(long = "in", value_name = "FILE", required = true)]
    input: String,
    /// SFEN list file (text lines starting with 'sfen ' or raw 4-token SFEN)
    #[arg(long = "sfens", value_name = "FILE", required = true)]
    sfens: String,
    /// Output JSONL (.jsonl[.gz|.zst])
    #[arg(long = "out", value_name = "FILE", required = true)]
    out: String,
}

fn normalize_sfen_line(line: &str) -> Option<String> {
    let s = line.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(rest) = s.strip_prefix("sfen ") {
        Some(rest.trim().to_string())
    } else {
        Some(s.to_string())
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    // Load SFEN set
    let mut set: HashSet<String> = HashSet::new();
    let f = File::open(&cli.sfens)?;
    let mut r = BufReader::new(f);
    let mut line = String::new();
    loop {
        line.clear();
        if r.read_line(&mut line)? == 0 {
            break;
        }
        if let Some(s) = normalize_sfen_line(&line) {
            set.insert(s);
        }
    }
    eprintln!("loaded {} sfens to filter", set.len());

    // Stream input and write filtered output
    let mut inr = open_maybe_compressed_reader(&cli.input, 4 * 1024 * 1024)?;
    let mut outw: Box<dyn Write> = if cli.out.ends_with(".gz") {
        Box::new(flate2::write::GzEncoder::new(
            File::create(&cli.out)?,
            flate2::Compression::default(),
        ))
    } else if cli.out.ends_with(".zst") {
        #[cfg(feature = "zstd")]
        {
            Box::new(zstd::Encoder::new(File::create(&cli.out)?, 0)?.auto_finish())
        }
        #[cfg(not(feature = "zstd"))]
        {
            return Err("output ends with .zst but built without zstd feature".into());
        }
    } else {
        Box::new(std::io::BufWriter::new(File::create(&cli.out)?))
    };

    let mut buf = String::new();
    let mut kept: usize = 0;
    let mut skipped: usize = 0;
    loop {
        buf.clear();
        let n = inr.read_line(&mut buf)?;
        if n == 0 {
            break;
        }
        if buf.trim().is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<JsonValue>(&buf) else {
            continue;
        };
        let Some(sfen) = v.get("sfen").and_then(|x| x.as_str()) else {
            continue;
        };
        if set.contains(sfen) {
            skipped += 1;
            continue;
        }
        outw.write_all(buf.as_bytes())?;
        kept += 1;
    }
    eprintln!("kept {} lines, skipped {}", kept, skipped);
    Ok(())
}
