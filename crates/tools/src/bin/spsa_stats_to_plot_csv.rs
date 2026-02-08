use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::Parser;

const MODE_SEED: &str = "seed";
const MODE_AGGREGATE: &str = "aggregate";

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "tools spsa の --stats-csv / --stats-aggregate-csv 出力を可視化向けCSVへ整形する"
)]
struct Cli {
    /// 入力CSV
    input_csv: PathBuf,

    /// 出力CSV（省略時: <input>.plot.csv）
    #[arg(long)]
    output_csv: Option<PathBuf>,

    /// score_rate の移動平均ウィンドウ幅（既定: 8）
    #[arg(long, default_value_t = 8)]
    window: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Seed,
    Aggregate,
}

#[derive(Clone, Debug)]
struct RowValues {
    seeds: i32,
    games_per_seed: i32,
    games: i32,
    plus_win_rate: f64,
    minus_win_rate: f64,
    draw_rate: f64,
    score_rate: f64,
    score_rate_std: f64,
    grad_scale: f64,
    grad_scale_std: f64,
    a_t: String,
    c_t: String,
    avg_abs_shift: String,
    avg_abs_update: String,
    max_abs_update: String,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.window == 0 {
        bail!("--window must be >= 1");
    }

    let output_csv = cli
        .output_csv
        .clone()
        .unwrap_or_else(|| PathBuf::from(format!("{}.plot.csv", cli.input_csv.display())));

    let src = File::open(&cli.input_csv)
        .with_context(|| format!("failed to open {}", cli.input_csv.display()))?;
    let mut reader = BufReader::new(src);

    let mut header_line = String::new();
    if reader
        .read_line(&mut header_line)
        .with_context(|| format!("failed to read {}", cli.input_csv.display()))?
        == 0
    {
        bail!("input CSV header is empty");
    }
    let header_line = header_line.trim_end_matches(['\n', '\r']);
    let headers = parse_csv_line(header_line);
    if headers.is_empty() {
        bail!("input CSV header is empty");
    }

    let mode = detect_mode(&headers)?;
    let index = build_index(&headers);

    let dst = File::create(&output_csv)
        .with_context(|| format!("failed to create {}", output_csv.display()))?;
    let mut writer = BufWriter::new(dst);
    let output_headers = [
        "iteration",
        "mode",
        "seeds",
        "games_per_seed",
        "games",
        "plus_win_rate",
        "minus_win_rate",
        "draw_rate",
        "score_rate",
        "score_rate_std",
        "cumulative_score_rate",
        "rolling_score_rate",
        "grad_scale",
        "grad_scale_std",
        "a_t",
        "c_t",
        "avg_abs_shift",
        "avg_abs_update",
        "max_abs_update",
        "total_games",
    ];
    write_csv_row(&mut writer, &output_headers)?;

    let mut cumulative_games = 0_u64;
    let mut cumulative_step_sum = 0.0_f64;
    let mut rolling_scores: VecDeque<f64> = VecDeque::with_capacity(cli.window);

    let mut line = String::new();
    loop {
        line.clear();
        if reader
            .read_line(&mut line)
            .with_context(|| format!("failed to read {}", cli.input_csv.display()))?
            == 0
        {
            break;
        }
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed.is_empty() {
            continue;
        }
        let cols = parse_csv_line(trimmed);
        if cols.len() != headers.len() {
            bail!("invalid CSV row: expected {} columns but got {}", headers.len(), cols.len());
        }
        let row = CsvRow {
            cols: &cols,
            index: &index,
        };

        let iteration = row.get("iteration")?;
        let total_games = row.get("total_games")?;

        let values = match mode {
            Mode::Seed => row_values_seed(&row)?,
            Mode::Aggregate => row_values_aggregate(&row)?,
        };

        cumulative_games += u64::try_from(values.games).context("games must be >= 0")?;
        cumulative_step_sum += values.score_rate * f64::from(values.games);
        let cumulative_score_rate = if cumulative_games > 0 {
            cumulative_step_sum / cumulative_games as f64
        } else {
            0.0
        };

        if rolling_scores.len() == cli.window {
            rolling_scores.pop_front();
        }
        rolling_scores.push_back(values.score_rate);
        let rolling_sum: f64 = rolling_scores.iter().copied().sum();
        let rolling_score_rate = rolling_sum / rolling_scores.len() as f64;

        let record = vec![
            iteration.to_owned(),
            mode.as_str().to_owned(),
            values.seeds.to_string(),
            values.games_per_seed.to_string(),
            values.games.to_string(),
            values.plus_win_rate.to_string(),
            values.minus_win_rate.to_string(),
            values.draw_rate.to_string(),
            values.score_rate.to_string(),
            values.score_rate_std.to_string(),
            cumulative_score_rate.to_string(),
            rolling_score_rate.to_string(),
            values.grad_scale.to_string(),
            values.grad_scale_std.to_string(),
            values.a_t,
            values.c_t,
            values.avg_abs_shift,
            values.avg_abs_update,
            values.max_abs_update,
            total_games.to_owned(),
        ];
        write_csv_row(&mut writer, &record)?;
    }

    writer.flush()?;
    Ok(())
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Seed => MODE_SEED,
            Self::Aggregate => MODE_AGGREGATE,
        }
    }
}

struct CsvRow<'a> {
    cols: &'a [String],
    index: &'a HashMap<String, usize>,
}

impl<'a> CsvRow<'a> {
    fn get(&self, key: &str) -> Result<&str> {
        let idx = self.index.get(key).copied().with_context(|| format!("missing column: {key}"))?;
        self.cols
            .get(idx)
            .map(String::as_str)
            .with_context(|| format!("column index out of range: {key}"))
    }

    fn parse_i32(&self, key: &str) -> Result<i32> {
        self.get(key)?
            .parse::<i32>()
            .with_context(|| format!("failed to parse integer column: {key}"))
    }

    fn parse_f64(&self, key: &str) -> Result<f64> {
        self.get(key)?
            .parse::<f64>()
            .with_context(|| format!("failed to parse float column: {key}"))
    }

    fn parse_f64_or_default(&self, key: &str, default: f64) -> Result<f64> {
        let value = self.get(key)?;
        if value.is_empty() {
            return Ok(default);
        }
        value
            .parse::<f64>()
            .with_context(|| format!("failed to parse float column: {key}"))
    }
}

fn parse_csv_line(line: &str) -> Vec<String> {
    // tools spsa の統計CSVは引用符なしで生成されるため、単純splitで扱う。
    line.split(',').map(ToOwned::to_owned).collect()
}

fn build_index(headers: &[String]) -> HashMap<String, usize> {
    headers.iter().enumerate().map(|(idx, name)| (name.clone(), idx)).collect()
}

fn detect_mode(headers: &[String]) -> Result<Mode> {
    let names: HashSet<&str> = headers.iter().map(String::as_str).collect();

    let aggregate_required = [
        "iteration",
        "seeds",
        "games_per_seed",
        "plus_wins_mean",
        "minus_wins_mean",
        "draws_mean",
        "step_sum_mean",
        "grad_scale_mean",
        "total_games",
    ];
    if aggregate_required.iter().all(|name| names.contains(name)) {
        return Ok(Mode::Aggregate);
    }

    let seed_required = [
        "iteration",
        "games",
        "plus_wins",
        "minus_wins",
        "draws",
        "step_sum",
        "grad_scale",
        "a_t",
        "c_t",
        "avg_abs_shift",
        "avg_abs_update",
        "max_abs_update",
        "total_games",
    ];
    if seed_required.iter().all(|name| names.contains(name)) {
        return Ok(Mode::Seed);
    }

    bail!("input CSV format is not recognized. expected seed stats CSV or aggregate stats CSV")
}

fn row_values_seed(row: &CsvRow<'_>) -> Result<RowValues> {
    let games = row.parse_i32("games")?;
    let plus_wins = row.parse_i32("plus_wins")?;
    let minus_wins = row.parse_i32("minus_wins")?;
    let draws = row.parse_i32("draws")?;
    let step_sum = row.parse_f64("step_sum")?;
    let grad_scale = row.parse_f64("grad_scale")?;

    let denom = if games > 0 { f64::from(games) } else { 1.0 };
    let plus_win_rate = f64::from(plus_wins) / denom;
    let minus_win_rate = f64::from(minus_wins) / denom;
    let draw_rate = f64::from(draws) / denom;
    let score_rate = step_sum / denom;

    Ok(RowValues {
        seeds: 1,
        games_per_seed: games,
        games,
        plus_win_rate,
        minus_win_rate,
        draw_rate,
        score_rate,
        score_rate_std: 0.0,
        grad_scale,
        grad_scale_std: 0.0,
        a_t: row.get("a_t")?.to_owned(),
        c_t: row.get("c_t")?.to_owned(),
        avg_abs_shift: row.get("avg_abs_shift")?.to_owned(),
        avg_abs_update: row.get("avg_abs_update")?.to_owned(),
        max_abs_update: row.get("max_abs_update")?.to_owned(),
    })
}

fn row_values_aggregate(row: &CsvRow<'_>) -> Result<RowValues> {
    let seeds = row.parse_i32("seeds")?;
    let games_per_seed = row.parse_i32("games_per_seed")?;
    let games = seeds.checked_mul(games_per_seed).context("games overflow")?;

    let plus_wins_mean = row.parse_f64("plus_wins_mean")?;
    let minus_wins_mean = row.parse_f64("minus_wins_mean")?;
    let draws_mean = row.parse_f64("draws_mean")?;
    let step_sum_mean = row.parse_f64("step_sum_mean")?;
    let grad_scale_mean = row.parse_f64("grad_scale_mean")?;

    let step_sum_variance = row.parse_f64_or_default("step_sum_variance", 0.0)?;
    let grad_scale_variance = row.parse_f64_or_default("grad_scale_variance", 0.0)?;

    let denom = if games_per_seed > 0 {
        f64::from(games_per_seed)
    } else {
        1.0
    };
    let plus_win_rate = plus_wins_mean / denom;
    let minus_win_rate = minus_wins_mean / denom;
    let draw_rate = draws_mean / denom;
    let score_rate = step_sum_mean / denom;
    let score_rate_std = step_sum_variance.max(0.0).sqrt() / denom;
    let grad_scale_std = grad_scale_variance.max(0.0).sqrt();

    Ok(RowValues {
        seeds,
        games_per_seed,
        games,
        plus_win_rate,
        minus_win_rate,
        draw_rate,
        score_rate,
        score_rate_std,
        grad_scale: grad_scale_mean,
        grad_scale_std,
        a_t: String::new(),
        c_t: String::new(),
        avg_abs_shift: String::new(),
        avg_abs_update: String::new(),
        max_abs_update: String::new(),
    })
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
