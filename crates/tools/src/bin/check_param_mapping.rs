//! YO ⇔ rshogi マッピング表の整合性検証
//!
//! 動作:
//! 1. マッピング表自体の整合性（重複・rshogi 名が `tune_params.rs` に存在するか等）
//! 2. 与えられた YO/rshogi `.params` ペアでマッピングを通したときの値一致を検証
//!    （`suisho10.params` と `suisho10_converted.params` を渡せば回帰テストになる）
//! 3. `--yo-binary <path>` 指定時、YO バイナリの USI option 一覧と
//!    マッピング表の整合性を検証する（tune.py 注入の陳腐化検出）

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
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

    /// YO バイナリのパス。指定時は `usi` を送って `option name ...` 一覧を取得し
    /// マッピング表 (`mappings.yo` ∪ `unmapped.yo`) との不整合を検出する。
    /// tune.py 注入が変わって mapping 表が陳腐化したことを CI で拾う用途。
    #[arg(long)]
    yo_binary: Option<PathBuf>,

    /// `--yo-binary` で `usi` 応答を待つタイムアウト秒（既定 5 秒）
    #[arg(long, default_value_t = 5)]
    yo_timeout_secs: u64,
}

/// `option name <NAME> type <TYPE> ...` から `<NAME>` を抜き出す
fn parse_usi_option_name(line: &str) -> Option<&str> {
    let rest = line.trim().strip_prefix("option ")?;
    let after_name = rest.strip_prefix("name ")?;
    // `type` までが name 部
    let upto = after_name.find(" type ")?;
    Some(after_name[..upto].trim())
}

/// YO バイナリを起動し `usi` を送って公開する USI option 名一覧を取得する。
///
/// `usiok` 応答 or タイムアウト到達で読み取りを終了し、`quit` を送って終了させる。
fn fetch_yo_usi_options(binary: &Path, timeout: Duration) -> Result<HashSet<String>> {
    let mut child = Command::new(binary)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to spawn YO binary {}", binary.display()))?;

    {
        let stdin = child.stdin.as_mut().context("YO stdin not available")?;
        stdin.write_all(b"usi\n").context("failed to write 'usi' to YO")?;
        stdin.flush().ok();
    }

    let stdout = child.stdout.take().context("YO stdout not available")?;
    let mut reader = BufReader::new(stdout);
    let deadline = Instant::now() + timeout;
    let mut options = HashSet::new();
    let mut got_usiok = false;

    // BufReader は blocking なので timeout は厳密ではないが、`usiok` で必ず抜けるため
    // 通常は数百ms で完了する。万一 hang した場合に kill するための保険。
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {
                if let Some(name) = parse_usi_option_name(&line) {
                    options.insert(name.to_owned());
                }
                if line.trim() == "usiok" {
                    got_usiok = true;
                    break;
                }
            }
            Err(e) => {
                eprintln!("warn: YO stdout read error: {e}");
                break;
            }
        }
        if Instant::now() >= deadline {
            eprintln!("warn: YO `usi` 応答が {} 秒以内に完了しませんでした", timeout.as_secs());
            break;
        }
    }

    // 後始末: quit 送って待つ。失敗しても child は drop で kill される。
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(b"quit\n");
    }
    let _ = child.wait();

    if !got_usiok && options.is_empty() {
        bail!("YO バイナリから option を 1 件も取得できませんでした ({})", binary.display());
    }
    Ok(options)
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

    // (4) YO バイナリ整合性検証
    let mut yo_only_in_binary: Vec<String> = Vec::new();
    let mut yo_only_in_table: Vec<String> = Vec::new();
    if let Some(yo_bin) = &cli.yo_binary {
        let timeout = Duration::from_secs(cli.yo_timeout_secs);
        let yo_options = fetch_yo_usi_options(yo_bin, timeout)?;
        println!(
            "yo-binary check: {} USI options (timeout {}s) from {}",
            yo_options.len(),
            cli.yo_timeout_secs,
            yo_bin.display()
        );

        let table_yo: HashSet<&str> = table
            .mappings
            .iter()
            .map(|m| m.yo.as_str())
            .chain(table.unmapped.yo.iter().map(|s| s.as_str()))
            .collect();

        for name in &yo_options {
            if !table_yo.contains(name.as_str()) {
                yo_only_in_binary.push(name.clone());
            }
        }
        yo_only_in_binary.sort();
        for name in &table_yo {
            if !yo_options.contains(*name) {
                yo_only_in_table.push((*name).to_owned());
            }
        }
        yo_only_in_table.sort();

        if !yo_only_in_binary.is_empty() {
            println!(
                "WARN: YO で公開されているが mapping にも unmapped.yo にも記載のない option \
                 ({} 件、tune.py 注入が増えた可能性):",
                yo_only_in_binary.len()
            );
            for n in &yo_only_in_binary {
                println!("  - {n}");
            }
        }
        if !yo_only_in_table.is_empty() {
            println!(
                "WARN: mapping/unmapped.yo にあるが YO バイナリの USI option に存在しない \
                 ({} 件、旧 mapping の残骸の可能性):",
                yo_only_in_table.len()
            );
            for n in &yo_only_in_table {
                println!("  - {n}");
            }
        }
    }

    let has_errors = !bad_rshogi_names.is_empty();
    let has_mismatches = !value_mismatches.is_empty();
    let has_yo_drift = !yo_only_in_binary.is_empty() || !yo_only_in_table.is_empty();
    let exit_failure = has_errors || (cli.strict && (has_mismatches || has_yo_drift));

    println!("\n=== summary ===");
    println!("bad rshogi names    : {}", bad_rshogi_names.len());
    println!("uncovered (info)    : {}", uncovered.len());
    println!("value mismatches    : {}", value_mismatches.len());
    if cli.yo_binary.is_some() {
        println!("yo only in binary   : {}", yo_only_in_binary.len());
        println!("yo only in table    : {}", yo_only_in_table.len());
    }

    if exit_failure {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_usi_option_name_extracts_simple() {
        assert_eq!(
            parse_usi_option_name(
                "option name correction_value_1 type spin default 9536 min 0 max 17734"
            ),
            Some("correction_value_1")
        );
    }

    #[test]
    fn parse_usi_option_name_handles_multiword_name() {
        assert_eq!(
            parse_usi_option_name("option name USI_Hash type spin default 16"),
            Some("USI_Hash")
        );
    }

    #[test]
    fn parse_usi_option_name_rejects_non_option() {
        assert_eq!(parse_usi_option_name("id name YaneuraOu"), None);
        assert_eq!(parse_usi_option_name("usiok"), None);
        // type 句を欠く不正形式
        assert_eq!(parse_usi_option_name("option name foo bar"), None);
    }
}
