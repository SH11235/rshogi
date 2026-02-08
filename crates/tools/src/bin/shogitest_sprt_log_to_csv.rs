use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::LazyLock;

use anyhow::{bail, Context, Result};
use clap::Parser;
use regex::Regex;

static RESULTS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^Results of (.+?) vs (.+?) \(").expect("invalid RESULTS_RE pattern")
});
static ELO_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^Elo:\s*([+-]?\d+(?:\.\d+)?)\s*\+/-\s*([+-]?\d+(?:\.\d+)?),\s*nElo:\s*([+-]?\d+(?:\.\d+)?)\s*\+/-\s*([+-]?\d+(?:\.\d+)?)",
    )
    .expect("invalid ELO_RE pattern")
});
static GAMES_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^Games:\s*(\d+),\s*Wins:\s*(\d+),\s*Draws:\s*(\d+),\s*Losses:\s*(\d+)\s*\(Score:\s*([+-]?\d+(?:\.\d+)?)%\)",
    )
    .expect("invalid GAMES_RE pattern")
});
static LLR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^LLR:\s*([+-]?\d+(?:\.\d+)?)\s*\(([+-]?\d+(?:\.\d+)?),\s*([+-]?\d+(?:\.\d+)?)\)\s*\[([+-]?\d+(?:\.\d+)?),\s*([+-]?\d+(?:\.\d+)?)\]",
    )
    .expect("invalid LLR_RE pattern")
});

#[derive(Parser, Debug)]
#[command(author, version, about = "Parse shogitest SPRT logs into summary CSV")]
struct Cli {
    /// shogitest 実行ログ
    input_log: PathBuf,

    /// 出力CSV（省略時: <input>.summary.csv）
    #[arg(long)]
    output_csv: Option<PathBuf>,
}

#[derive(Clone, Debug)]
struct SnapshotRow {
    snapshot: usize,
    line_no: usize,
    engine_dev: String,
    engine_base: String,
    games: u32,
    wins: u32,
    draws: u32,
    losses: u32,
    score_pct: f64,
    elo: Option<f64>,
    elo_err: Option<f64>,
    nelo: Option<f64>,
    nelo_err: Option<f64>,
    llr: Option<f64>,
    llr_lower: Option<f64>,
    llr_upper: Option<f64>,
    nelo_lower: Option<f64>,
    nelo_upper: Option<f64>,
    sprt_state: String,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let output_csv = cli
        .output_csv
        .clone()
        .unwrap_or_else(|| PathBuf::from(format!("{}.summary.csv", cli.input_log.display())));

    let bytes = fs::read(&cli.input_log)
        .with_context(|| format!("failed to read {}", cli.input_log.display()))?;
    let content = String::from_utf8_lossy(&bytes);

    let mut current_dev = String::new();
    let mut current_base = String::new();
    let mut current_elo: Option<(f64, f64, f64, f64)> = None;
    let mut rows: Vec<SnapshotRow> = Vec::new();
    let mut pending_idx: Option<usize> = None;

    for (idx, raw_line) in content.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw_line.trim();

        if let Some(caps) = RESULTS_RE.captures(line) {
            if let (Some(dev), Some(base)) = (caps.get(1), caps.get(2)) {
                current_dev = dev.as_str().to_owned();
                current_base = base.as_str().to_owned();
                current_elo = None;
                pending_idx = None;
            }
            continue;
        }

        if let Some(caps) = ELO_RE.captures(line) {
            current_elo = Some((
                parse_capture_f64(&caps, 1, "elo")?,
                parse_capture_f64(&caps, 2, "elo_err")?,
                parse_capture_f64(&caps, 3, "nelo")?,
                parse_capture_f64(&caps, 4, "nelo_err")?,
            ));
            continue;
        }

        if let Some(caps) = GAMES_RE.captures(line) {
            let snapshot = rows.len() + 1;
            let (elo, elo_err, nelo, nelo_err) = current_elo
                .map(|v| (Some(v.0), Some(v.1), Some(v.2), Some(v.3)))
                .unwrap_or((None, None, None, None));
            rows.push(SnapshotRow {
                snapshot,
                line_no,
                engine_dev: current_dev.clone(),
                engine_base: current_base.clone(),
                games: parse_capture_u32(&caps, 1, "games")?,
                wins: parse_capture_u32(&caps, 2, "wins")?,
                draws: parse_capture_u32(&caps, 3, "draws")?,
                losses: parse_capture_u32(&caps, 4, "losses")?,
                score_pct: parse_capture_f64(&caps, 5, "score_pct")?,
                elo,
                elo_err,
                nelo,
                nelo_err,
                llr: None,
                llr_lower: None,
                llr_upper: None,
                nelo_lower: None,
                nelo_upper: None,
                sprt_state: "running".to_owned(),
            });
            pending_idx = Some(rows.len() - 1);
            continue;
        }

        if let Some(caps) = LLR_RE.captures(line) {
            if let Some(row_idx) = pending_idx.take() {
                let llr = parse_capture_f64(&caps, 1, "llr")?;
                let llr_lower = parse_capture_f64(&caps, 2, "llr_lower")?;
                let llr_upper = parse_capture_f64(&caps, 3, "llr_upper")?;
                let nelo_lower = parse_capture_f64(&caps, 4, "nelo_lower")?;
                let nelo_upper = parse_capture_f64(&caps, 5, "nelo_upper")?;
                let state = classify_sprt(llr, llr_lower, llr_upper).to_owned();

                if let Some(row) = rows.get_mut(row_idx) {
                    row.llr = Some(llr);
                    row.llr_lower = Some(llr_lower);
                    row.llr_upper = Some(llr_upper);
                    row.nelo_lower = Some(nelo_lower);
                    row.nelo_upper = Some(nelo_upper);
                    row.sprt_state = state;
                }
            }
        }
    }

    if rows.is_empty() {
        bail!("no SPRT summary rows found in log: {}", cli.input_log.display());
    }

    if let Some(parent) = output_csv.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
    }

    let out = File::create(&output_csv)
        .with_context(|| format!("failed to create {}", output_csv.display()))?;
    let mut writer = BufWriter::new(out);
    let fieldnames = [
        "snapshot",
        "line_no",
        "engine_dev",
        "engine_base",
        "games",
        "wins",
        "draws",
        "losses",
        "score_pct",
        "elo",
        "elo_err",
        "nelo",
        "nelo_err",
        "llr",
        "llr_lower",
        "llr_upper",
        "nelo_lower",
        "nelo_upper",
        "sprt_state",
    ];
    write_csv_row(&mut writer, &fieldnames)?;
    for row in &rows {
        let fields = vec![
            row.snapshot.to_string(),
            row.line_no.to_string(),
            row.engine_dev.clone(),
            row.engine_base.clone(),
            row.games.to_string(),
            row.wins.to_string(),
            row.draws.to_string(),
            row.losses.to_string(),
            fmt_float(row.score_pct),
            fmt_opt_float(row.elo),
            fmt_opt_float(row.elo_err),
            fmt_opt_float(row.nelo),
            fmt_opt_float(row.nelo_err),
            fmt_opt_float(row.llr),
            fmt_opt_float(row.llr_lower),
            fmt_opt_float(row.llr_upper),
            fmt_opt_float(row.nelo_lower),
            fmt_opt_float(row.nelo_upper),
            row.sprt_state.clone(),
        ];
        write_csv_row(&mut writer, &fields)?;
    }
    writer.flush()?;

    let final_row = rows.last().context("missing final row")?;
    println!("wrote summary CSV: {}", output_csv.display());
    println!(
        "final: games={games} score={score:.2}% elo={elo} nelo={nelo} llr={llr} state={state}",
        games = final_row.games,
        score = final_row.score_pct,
        elo = fmt_opt_float(final_row.elo),
        nelo = fmt_opt_float(final_row.nelo),
        llr = fmt_opt_float(final_row.llr),
        state = final_row.sprt_state,
    );

    Ok(())
}

fn parse_capture_f64(caps: &regex::Captures<'_>, idx: usize, label: &str) -> Result<f64> {
    let raw = caps
        .get(idx)
        .map(|m| m.as_str())
        .with_context(|| format!("missing capture for {label}"))?;
    raw.parse::<f64>().with_context(|| format!("failed to parse {label}: {raw}"))
}

fn parse_capture_u32(caps: &regex::Captures<'_>, idx: usize, label: &str) -> Result<u32> {
    let raw = caps
        .get(idx)
        .map(|m| m.as_str())
        .with_context(|| format!("missing capture for {label}"))?;
    raw.parse::<u32>().with_context(|| format!("failed to parse {label}: {raw}"))
}

fn classify_sprt(llr: f64, lo: f64, hi: f64) -> &'static str {
    if llr <= lo {
        return "accept_h0";
    }
    if llr >= hi {
        return "accept_h1";
    }
    "running"
}

fn fmt_float(value: f64) -> String {
    format!("{value:.6}")
}

fn fmt_opt_float(value: Option<f64>) -> String {
    value.map(fmt_float).unwrap_or_default()
}

fn write_csv_row(writer: &mut BufWriter<File>, row: &[impl AsRef<str>]) -> Result<()> {
    for (idx, value) in row.iter().enumerate() {
        if idx > 0 {
            writer.write_all(b",")?;
        }
        write_csv_value(writer, value.as_ref())?;
    }
    writer.write_all(b"\n")?;
    Ok(())
}

fn write_csv_value(writer: &mut BufWriter<File>, value: &str) -> Result<()> {
    let needs_quote = value.contains(',') || value.contains('"') || value.contains('\n');
    if !needs_quote {
        writer.write_all(value.as_bytes())?;
        return Ok(());
    }
    writer.write_all(b"\"")?;
    for ch in value.chars() {
        if ch == '"' {
            writer.write_all(b"\"\"")?;
        } else {
            let mut buf = [0_u8; 4];
            writer.write_all(ch.encode_utf8(&mut buf).as_bytes())?;
        }
    }
    writer.write_all(b"\"")?;
    Ok(())
}
