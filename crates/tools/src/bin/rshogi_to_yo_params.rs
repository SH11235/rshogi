//! rshogi 形式の SPSA `.params` を YaneuraOu 形式に変換する。
//!
//! 動作:
//! - `--mapping` で指定した TOML を引きつつ rshogi 値を YO 名のスロットに転記する
//!   （必要なら符号反転）。
//! - YO 側の min/max/step/alpha は `--base` で指定した YO `.params`（例: tune/suisho10.params）
//!   の値を保持する。`--base` 省略時は rshogi 値を中心に簡易 range を生成する。
//! - rshogi 独自パラメータ（`unmapped.rshogi`）は YO 出力には含まれない（warn 出力）。

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Parser;
use tools::spsa_param_mapping::{
    MappingTable, ParamRow, load_params, rshogi_to_yo_value, write_params,
};

#[derive(Parser, Debug)]
#[command(author, version, about = "rshogi .params → YaneuraOu .params 変換")]
struct Cli {
    /// rshogi 形式の入力 .params
    #[arg(long)]
    rshogi_params: PathBuf,

    /// マッピング TOML
    #[arg(long, default_value = "tune/yo_rshogi_mapping.toml")]
    mapping: PathBuf,

    /// ベースとなる YO .params（min/max/step を流用）。省略時は値ベースで簡易生成
    #[arg(long)]
    base: Option<PathBuf>,

    /// 出力先 YO .params
    #[arg(long)]
    output: PathBuf,

    /// 範囲外値を検出した時に warning に留めず error にする
    #[arg(long)]
    strict_range: bool,
}

/// `--base` がない場合に YO 行を簡易生成する
fn synthesize_yo_row(yo_name: &str, value: i32) -> ParamRow {
    let abs_value = value.unsigned_abs() as i32;
    let span = (abs_value * 2).max(8);
    let (min, max) = if value >= 0 { (0, span) } else { (-span, 0) };
    let step = ((span as f64) / 20.0).max(1.0);
    ParamRow {
        name: yo_name.to_owned(),
        kind: "int".to_owned(),
        value,
        min,
        max,
        step,
        alpha: 0.002,
        not_used: false,
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let table = MappingTable::load(&cli.mapping)?;
    let rshogi_rows = load_params(&cli.rshogi_params)?;
    let rshogi_by_name: HashMap<&str, &ParamRow> =
        rshogi_rows.iter().map(|r| (r.name.as_str(), r)).collect();

    let base_rows: Option<Vec<ParamRow>> = match &cli.base {
        Some(path) => Some(load_params(path)?),
        None => None,
    };
    let base_by_name: HashMap<&str, &ParamRow> = base_rows
        .as_ref()
        .map(|rows| rows.iter().map(|r| (r.name.as_str(), r)).collect())
        .unwrap_or_default();

    let mut out_rows: Vec<ParamRow> = Vec::new();
    let mut applied = 0usize;
    let mut missing_rshogi: Vec<&str> = Vec::new();
    let mut out_of_range: Vec<(String, i32, i32, i32)> = Vec::new();

    // base ファイルがあれば、その順序を保つ
    let iter_order: Vec<&str> = if let Some(rows) = base_rows.as_ref() {
        rows.iter().map(|r| r.name.as_str()).collect()
    } else {
        // base がない場合はマッピング表の順序
        table.mappings.iter().map(|m| m.yo.as_str()).collect()
    };

    let yo_to_r = table.by_yo_name();

    for yo_name in iter_order {
        let Some(mapping) = yo_to_r.get(yo_name) else {
            // base にあるが mapping 表にない YO パラメータ → base のまま出力（rshogi 由来データなし）
            if let Some(base_row) = base_by_name.get(yo_name) {
                out_rows.push((*base_row).clone());
            }
            continue;
        };
        let Some(r_row) = rshogi_by_name.get(mapping.rshogi.as_str()) else {
            missing_rshogi.push(mapping.rshogi.as_str());
            // base 値があればそれを使う、なければ skip
            if let Some(base_row) = base_by_name.get(yo_name) {
                out_rows.push((*base_row).clone());
            }
            continue;
        };
        let new_value = rshogi_to_yo_value(r_row.value, mapping.sign_flip);
        let mut yo_row = base_by_name
            .get(yo_name)
            .map(|r| (*r).clone())
            .unwrap_or_else(|| synthesize_yo_row(yo_name, new_value));
        if new_value < yo_row.min || new_value > yo_row.max {
            out_of_range.push((yo_row.name.clone(), new_value, yo_row.min, yo_row.max));
        }
        yo_row.value = new_value;
        out_rows.push(yo_row);
        applied += 1;
    }

    // rshogi 独自パラメータの警告
    let mut rshogi_only: Vec<&str> = rshogi_rows
        .iter()
        .map(|r| r.name.as_str())
        .filter(|n| !table.by_rshogi_name().contains_key(n))
        .collect();
    rshogi_only.sort();
    if !rshogi_only.is_empty() {
        eprintln!("info: YO 出力に含まれない rshogi 独自パラメータ {} 件:", rshogi_only.len());
        for n in &rshogi_only {
            eprintln!("  - {n}");
        }
    }

    if !missing_rshogi.is_empty() {
        eprintln!("warn: マッピング先 rshogi パラメータが入力にない {} 件:", missing_rshogi.len());
        for n in &missing_rshogi {
            eprintln!("  - {n}");
        }
    }

    if !out_of_range.is_empty() {
        eprintln!("warn: 変換結果が YO 側 min/max を超えるパラメータ:");
        for (n, v, mn, mx) in &out_of_range {
            eprintln!("  - {n} = {v} (range = [{mn}, {mx}])");
        }
        if cli.strict_range {
            bail!("strict-range: out of range values detected");
        }
    }

    write_params(&cli.output, &out_rows)
        .with_context(|| format!("failed to write {}", cli.output.display()))?;
    eprintln!(
        "applied {applied} mappings, {} missing rshogi inputs, {} out-of-range",
        missing_rshogi.len(),
        out_of_range.len()
    );
    eprintln!("wrote {}", cli.output.display());

    Ok(())
}
