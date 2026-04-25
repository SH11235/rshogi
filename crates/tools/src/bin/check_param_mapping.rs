//! YO ⇔ rshogi マッピング表の整合性検証
//!
//! 動作:
//! 1. マッピング表自体の整合性（重複・rshogi 名が `tune_params.rs` に存在するか等）
//! 2. 与えられた YO/rshogi `.params` ペアでマッピングを通したときの値一致を検証
//!    （`suisho10.params` と `suisho10_converted.params` を渡せば回帰テストになる）

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use rshogi_core::search::SearchTuneParams;
use tools::spsa_param_mapping::{MappingTable, ParamRow, load_params, yo_to_rshogi_value};

#[derive(Parser, Debug)]
#[command(author, version, about = "YO ⇔ rshogi SPSA mapping integrity checker")]
struct Cli {
    /// マッピング TOML
    #[arg(long, default_value = "tune/yo_rshogi_mapping.toml")]
    mapping: PathBuf,

    /// YO 形式 .params（オプション、ペア検証用）
    #[arg(long)]
    yo_params: Option<PathBuf>,

    /// rshogi 形式 .params（オプション、ペア検証用）
    #[arg(long)]
    rshogi_params: Option<PathBuf>,

    /// 不一致が 1 件でもあれば exit 1
    #[arg(long)]
    strict: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let table = MappingTable::load(&cli.mapping)?;
    println!(
        "mapping: loaded {} entries from {}",
        table.mappings.len(),
        cli.mapping.display()
    );

    // (1) rshogi 名が tune_params に存在するか
    let valid_rshogi_names: HashSet<&str> =
        SearchTuneParams::option_specs().iter().map(|s| s.usi_name).collect();
    let mut bad_rshogi_names: Vec<&str> = Vec::new();
    for m in &table.mappings {
        if !valid_rshogi_names.contains(m.rshogi.as_str()) {
            bad_rshogi_names.push(m.rshogi.as_str());
        }
    }
    for n in &table.unmapped.rshogi {
        if !valid_rshogi_names.contains(n.as_str()) {
            bad_rshogi_names.push(n.as_str());
        }
    }
    if !bad_rshogi_names.is_empty() {
        println!("ERROR: 以下の rshogi 名が SearchTuneParams::option_specs() に存在しません:");
        for n in &bad_rshogi_names {
            println!("  - {n}");
        }
    }

    // (2) tune_params に存在するが mapping にも unmapped にも記載がないものを警告
    let mapped_rshogi: HashSet<&str> = table.mappings.iter().map(|m| m.rshogi.as_str()).collect();
    let unmapped_rshogi: HashSet<&str> = table.unmapped.rshogi.iter().map(|s| s.as_str()).collect();
    let mut uncovered: Vec<&str> = valid_rshogi_names
        .iter()
        .copied()
        .filter(|n| !mapped_rshogi.contains(n) && !unmapped_rshogi.contains(n))
        .collect();
    uncovered.sort();
    if !uncovered.is_empty() {
        println!(
            "WARN: tune_params.rs に存在するが mapping にも unmapped にも記載のない rshogi 名:"
        );
        for n in &uncovered {
            println!("  - {n}");
        }
    }

    // (3) ペア検証
    let mut value_mismatches: Vec<String> = Vec::new();
    if let (Some(yo_path), Some(r_path)) = (&cli.yo_params, &cli.rshogi_params) {
        let yo_rows = load_params(yo_path)?;
        let r_rows = load_params(r_path)?;
        let yo_by_name: HashMap<&str, &ParamRow> =
            yo_rows.iter().map(|r| (r.name.as_str(), r)).collect();
        let r_by_name: HashMap<&str, &ParamRow> =
            r_rows.iter().map(|r| (r.name.as_str(), r)).collect();

        let mut checked = 0;
        for m in &table.mappings {
            let Some(yo) = yo_by_name.get(m.yo.as_str()) else {
                continue;
            };
            let Some(r) = r_by_name.get(m.rshogi.as_str()) else {
                value_mismatches.push(format!(
                    "rshogi 名 {} が rshogi .params にない（YO 名: {})",
                    m.rshogi, m.yo
                ));
                continue;
            };
            let expected = yo_to_rshogi_value(yo.value, m.sign_flip);
            if expected != r.value {
                value_mismatches.push(format!(
                    "{} ↔ {} (sign_flip={}): YO={} → 期待 rshogi={} だが実際は {}",
                    m.yo, m.rshogi, m.sign_flip, yo.value, expected, r.value
                ));
            }
            checked += 1;
        }
        println!("pair check: {} mappings checked", checked);
        if !value_mismatches.is_empty() {
            println!("\nMISMATCHES ({}):", value_mismatches.len());
            for s in &value_mismatches {
                println!("  - {s}");
            }
        }
    } else {
        println!("(--yo-params と --rshogi-params 両方の指定でペア値検証を行います)");
    }

    let has_errors = !bad_rshogi_names.is_empty();
    let has_mismatches = !value_mismatches.is_empty();
    let exit_failure = has_errors || (cli.strict && has_mismatches);

    println!("\n=== summary ===");
    println!("bad rshogi names    : {}", bad_rshogi_names.len());
    println!("uncovered (info)    : {}", uncovered.len());
    println!("value mismatches    : {}", value_mismatches.len());

    if exit_failure {
        std::process::exit(1);
    }
    Ok(())
}
