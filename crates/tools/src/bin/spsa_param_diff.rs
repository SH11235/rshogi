use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::Parser;
use regex::Regex;
use rshogi_core::search::SearchTuneParams;

const NOT_USED_MARKER: &str = "[[NOT USED]]";

#[derive(Parser, Debug)]
#[command(author, version, about = "SPSA .params の最終差分と履歴差分を集計する")]
struct Cli {
    /// 比較対象の tuned.params
    #[arg(long)]
    current: PathBuf,

    /// ベース .params（未指定時は SearchTuneParams::option_specs() を使用）
    #[arg(long)]
    base: Option<PathBuf>,

    /// 対象パラメータ名フィルタ（正規表現）
    #[arg(long)]
    regex: Option<String>,

    /// 反復ごとの param_values.csv（指定時は touched を集計）
    #[arg(long)]
    param_values_csv: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug)]
struct DiffEntry {
    default: i32,
    current: i32,
    delta: i32,
}

fn parse_param_value_i32(text: &str) -> Result<i32> {
    if let Ok(v) = text.parse::<i32>() {
        return Ok(v);
    }
    let v = text.parse::<f64>().with_context(|| format!("invalid numeric value: {text}"))?;
    Ok(v.round() as i32)
}

fn parse_params_line(line: &str, line_no: usize) -> Result<Option<(String, i32)>> {
    let mut raw = line.trim().to_owned();
    if raw.is_empty() || raw.starts_with('#') {
        return Ok(None);
    }
    if raw.contains(NOT_USED_MARKER) {
        raw = raw.replace(NOT_USED_MARKER, "");
    }
    if let Some((head, _)) = raw.split_once("//") {
        raw = head.to_owned();
    }
    let cols: Vec<&str> = raw.split(',').map(str::trim).collect();
    if cols.len() < 3 {
        bail!("line {line_no}: invalid params format");
    }
    let name = cols[0].to_owned();
    let value = parse_param_value_i32(cols[2])
        .with_context(|| format!("line {line_no}: invalid value '{}'", cols[2]))?;
    Ok(Some((name, value)))
}

fn load_params_values(path: &PathBuf) -> Result<HashMap<String, i32>> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut map = HashMap::new();
    for (line_no, line) in reader.lines().enumerate() {
        let line_no = line_no + 1;
        let line = line.with_context(|| format!("line {line_no}: read failed"))?;
        if let Some((name, value)) = parse_params_line(&line, line_no)? {
            map.insert(name, value);
        }
    }
    Ok(map)
}

fn default_params_values() -> HashMap<String, i32> {
    SearchTuneParams::option_specs()
        .iter()
        .map(|spec| (spec.usi_name.to_owned(), spec.default))
        .collect()
}

fn load_param_values_touched(
    path: &PathBuf,
    defaults: &HashMap<String, i32>,
    target_names: &HashSet<String>,
) -> Result<HashSet<String>> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut lines = reader.lines();
    let header = lines
        .next()
        .transpose()?
        .with_context(|| format!("empty csv: {}", path.display()))?;
    let columns: Vec<&str> = header.split(',').collect();
    let mut indices = Vec::new();
    for (idx, col) in columns.iter().enumerate() {
        if target_names.contains(*col) {
            indices.push((idx, (*col).to_owned()));
        }
    }
    let mut touched = HashSet::new();
    for (line_no, line) in lines.enumerate() {
        let row = line.with_context(|| format!("csv read failed line {}", line_no + 2))?;
        let cols: Vec<&str> = row.split(',').collect();
        for (idx, name) in &indices {
            let Some(value_text) = cols.get(*idx) else {
                continue;
            };
            let Ok(value) = parse_param_value_i32(value_text) else {
                continue;
            };
            if let Some(default_value) = defaults.get(name) {
                if value != *default_value {
                    touched.insert(name.clone());
                }
            }
        }
    }
    Ok(touched)
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let name_re = cli.regex.as_deref().map(Regex::new).transpose().context("invalid --regex")?;

    let defaults = if let Some(base_path) = &cli.base {
        load_params_values(base_path)?
    } else {
        default_params_values()
    };
    let current = load_params_values(&cli.current)?;

    let mut entries = Vec::<(String, DiffEntry)>::new();
    for (name, default) in &defaults {
        if let Some(re) = &name_re {
            if !re.is_match(name) {
                continue;
            }
        }
        let current_value = *current.get(name).unwrap_or(default);
        entries.push((
            name.clone(),
            DiffEntry {
                default: *default,
                current: current_value,
                delta: current_value - default,
            },
        ));
    }
    if entries.is_empty() {
        bail!("no parameters matched filter");
    }

    let changed_final = entries.iter().filter(|(_, d)| d.delta != 0).count();
    let total = entries.len();
    let unchanged = total - changed_final;
    println!(
        "final_changed={changed_final}/{total} unchanged={} ({:.1}%)",
        unchanged,
        changed_final as f64 * 100.0 / total as f64
    );

    let target_names: HashSet<String> = entries.iter().map(|(name, _)| name.clone()).collect();
    if let Some(path) = &cli.param_values_csv {
        let touched = load_param_values_touched(path, &defaults, &target_names)?;
        let touched_not_final = touched.iter().filter(|name| {
            entries
                .iter()
                .find(|(entry_name, _)| entry_name == *name)
                .is_some_and(|(_, diff)| diff.delta == 0)
        });
        let touched_not_final_count = touched_not_final.count();
        println!(
            "touched={}/{} never_touched={} touched_not_final={}",
            touched.len(),
            total,
            total - touched.len(),
            touched_not_final_count
        );
    }

    entries.sort_by(|(name_a, diff_a), (name_b, diff_b)| {
        diff_b.delta.abs().cmp(&diff_a.delta.abs()).then_with(|| name_a.cmp(name_b))
    });
    println!();
    println!("{:<50} {:>8} {:>8} {:>8}", "Parameter", "Default", "Current", "Delta");
    println!("{}", "-".repeat(80));
    for (name, diff) in entries.iter().filter(|(_, diff)| diff.delta != 0) {
        println!("{:<50} {:>8} {:>8} {:+8}", name, diff.default, diff.current, diff.delta);
    }
    let unchanged_names: Vec<String> = entries
        .iter()
        .filter(|(_, diff)| diff.delta == 0)
        .map(|(name, _)| name.clone())
        .collect();
    if !unchanged_names.is_empty() {
        println!();
        println!("unchanged: {}", unchanged_names.join(", "));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_params_line_handles_comment_and_marker() {
        let line = "SPSA_FOO,int,12,0,100,1,1 // c [[NOT USED]]";
        let parsed = parse_params_line(line, 1).expect("parse failed").expect("none");
        assert_eq!(parsed.0, "SPSA_FOO");
        assert_eq!(parsed.1, 12);
    }

    #[test]
    fn parse_params_line_skips_comment_and_empty() {
        assert!(parse_params_line("  ", 1).expect("parse failed").is_none());
        assert!(parse_params_line("# x", 2).expect("parse failed").is_none());
    }

    #[test]
    fn parse_param_value_i32_supports_float() {
        assert_eq!(parse_param_value_i32("12.4").expect("parse failed"), 12);
        assert_eq!(parse_param_value_i32("12.5").expect("parse failed"), 13);
    }
}
