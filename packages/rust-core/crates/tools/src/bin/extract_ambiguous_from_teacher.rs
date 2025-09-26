use clap::Parser;
use serde_json::Value as JsonValue;
use std::io::BufRead;
use tools::io_detect::open_maybe_compressed_reader;

#[derive(Parser, Debug)]
#[command(
    name = "extract_ambiguous_from_teacher",
    about = "Extract SFENs flagged as ambiguous using teacher_* fields"
)]
struct Cli {
    /// Input JSONL (.jsonl[.gz|.zst])
    #[arg(long = "in", value_name = "FILE", required = true)]
    input: String,
    /// Output SFEN lines (plain text)
    #[arg(long = "out", value_name = "FILE", required = true)]
    out: String,
    /// Max teacher_depth to include (inclusive). Set 0 to disable depth criterion.
    #[arg(long = "max-depth", default_value_t = 10)]
    max_depth: i32,
    /// Include when teacher_bound is upper/lower
    #[arg(long = "include-non-exact", default_value_t = true)]
    include_non_exact: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let mut r = open_maybe_compressed_reader(&cli.input, 4 * 1024 * 1024)?;
    let mut w = std::fs::File::create(&cli.out)?;
    let mut buf = String::new();
    let mut n_out = 0usize;
    loop {
        buf.clear();
        let n = r.read_line(&mut buf)?;
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
        let t_depth = v.get("teacher_depth").and_then(|x| x.as_i64()).map(|x| x as i32);
        let t_bound = v.get("teacher_bound").and_then(|x| x.as_str()).unwrap_or("");
        let mut flag = false;
        if cli.include_non_exact && !t_bound.eq_ignore_ascii_case("exact") && !t_bound.is_empty() {
            flag = true;
        }
        if !flag {
            if let Some(d) = t_depth {
                if cli.max_depth > 0 && d >= 0 && d <= cli.max_depth {
                    flag = true;
                }
            }
        }
        if flag {
            use std::io::Write;
            writeln!(w, "sfen {}", sfen)?;
            n_out += 1;
        }
    }
    eprintln!("extracted {} ambiguous positions", n_out);
    Ok(())
}
