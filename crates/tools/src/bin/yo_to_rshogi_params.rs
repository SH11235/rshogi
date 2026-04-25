//! YaneuraOu 形式の SPSA `.params` を rshogi 形式に変換する。
//!
//! 動作:
//! - `--mapping` で指定した TOML（既定 `tune/yo_rshogi_mapping.toml`）を引きつつ
//!   YO 値を rshogi 名のスロットに転記する（必要なら符号反転）。
//! - rshogi 側の min/max/step/alpha は `--base` で指定した rshogi `.params` の値を保持する。
//!   `--base` 省略時は `SearchTuneParams::option_specs()` のデフォルト range を使用する。
//! - YO に対応がない rshogi パラメータは `--base` の値（あるいはデフォルト）のまま。

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::Parser;
use rshogi_core::search::SearchTuneParams;
use tools::spsa_param_mapping::{
    MappingTable, ParamRow, load_params, write_params, yo_to_rshogi_value,
};

#[derive(Parser, Debug)]
#[command(author, version, about = "YaneuraOu .params → rshogi .params 変換")]
struct Cli {
    /// YO 形式の入力 .params
    #[arg(long)]
    yo_params: PathBuf,

    /// マッピング TOML
    #[arg(long, default_value = "tune/yo_rshogi_mapping.toml")]
    mapping: PathBuf,

    /// ベースとなる rshogi .params（min/max/step を流用）。省略時は SearchTuneParams から生成
    #[arg(long)]
    base: Option<PathBuf>,

    /// 出力先 rshogi .params
    #[arg(long)]
    output: PathBuf,

    /// 範囲外値を検出した時に warning に留めず error にする
    #[arg(long)]
    strict_range: bool,
}

fn rshogi_default_rows() -> Vec<ParamRow> {
    SearchTuneParams::option_specs()
        .iter()
        .map(|spec| {
            let span = (spec.max - spec.min).max(1);
            let step = ((span as f64) / 200.0).max(1.0).round();
            let alpha = (span as f64) / 20.0;
            ParamRow {
                name: spec.usi_name.to_owned(),
                kind: "int".to_owned(),
                value: spec.default,
                min: spec.min,
                max: spec.max,
                step,
                alpha,
                not_used: false,
            }
        })
        .collect()
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let table = MappingTable::load(&cli.mapping)?;
    let yo_rows = load_params(&cli.yo_params)?;
    let yo_by_name: HashMap<&str, &ParamRow> =
        yo_rows.iter().map(|r| (r.name.as_str(), r)).collect();

    let mut rshogi_rows = match &cli.base {
        Some(path) => load_params(path)?,
        None => rshogi_default_rows(),
    };

    let index = table.index();

    let mut applied = 0usize;
    let mut out_of_range: Vec<(String, i32, i32, i32)> = Vec::new();

    for r in rshogi_rows.iter_mut() {
        let Some(mapping) = index.by_rshogi(r.name.as_str()) else {
            continue;
        };
        let Some(yo_row) = yo_by_name.get(mapping.yo.as_str()) else {
            // YO 入力に該当パラメータがない（古い tune.params など）→ base 値維持
            continue;
        };
        let new_value = yo_to_rshogi_value(yo_row.value, mapping.sign_flip);
        if new_value < r.min || new_value > r.max {
            out_of_range.push((r.name.clone(), new_value, r.min, r.max));
        }
        r.value = new_value;
        applied += 1;
    }

    // 入力 YO に存在するが rshogi に対応がないパラメータの検出
    let mut yo_unmapped_in_input: Vec<&str> = yo_rows
        .iter()
        .map(|r| r.name.as_str())
        .filter(|n| !index.contains_yo(n) && !table.unmapped.yo.iter().any(|u| u == n))
        .collect();
    yo_unmapped_in_input.sort();

    if !yo_unmapped_in_input.is_empty() {
        eprintln!(
            "warn: YO 入力に rshogi 側対応がないパラメータが {} 件あります（無視されました）:",
            yo_unmapped_in_input.len()
        );
        for n in &yo_unmapped_in_input {
            eprintln!("  - {n}");
        }
    }

    if !out_of_range.is_empty() {
        eprintln!("warn: 変換結果が rshogi 側 min/max を超えるパラメータ:");
        for (n, v, mn, mx) in &out_of_range {
            eprintln!("  - {n} = {v} (range = [{mn}, {mx}])");
        }
        if cli.strict_range {
            bail!("strict-range: out of range values detected");
        }
    }

    write_params(&cli.output, &rshogi_rows)?;
    eprintln!(
        "applied {applied} mappings, {} out-of-range, {} YO entries unmapped",
        out_of_range.len(),
        yo_unmapped_in_input.len()
    );
    eprintln!("wrote {}", cli.output.display());

    Ok(())
}
