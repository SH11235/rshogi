//! rshogi 形式の SPSA `.params` を YaneuraOu 形式に変換する。
//!
//! 動作:
//! - `--mapping` で指定した TOML を引きつつ rshogi 値を YO 名のスロットに転記する
//!   （必要なら符号反転）。
//! - YO 側の min/max/step/alpha は `--base` で指定した YO `.params`（例: tune/suisho10.params）
//!   の値を保持する。`--base` 省略時は rshogi 値を中心に簡易 range を生成する。
//! - rshogi 独自パラメータ（`unmapped.rshogi`）は YO 出力には含まれない（warn 出力）。
//!
//! ## rshogi default 値の検知
//!
//! 入力 rshogi `.params` の値列が `SearchTuneParams::option_specs()` の default と
//! 95% 以上一致した場合、警告を出す。`generate_spsa_params` の出力を canonical の
//! 代わりに誤投入するのを防ぐためのチェック。意図的に default 値から開始したい
//! 場合は `--allow-rshogi-defaults` で警告抑制、CI で混入を完全に防ぐ場合は
//! `--strict-rshogi-defaults` で error 化。

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Parser;
use rshogi_core::search::SearchTuneParams;
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

    /// 入力 rshogi `.params` の値列が rshogi 内部 default と高一致した場合の警告を抑制する。
    ///
    /// 意図的に default 値から SPSA を始めたい場合 (新規探索ベースライン作り等) に指定。
    /// 指定しない場合、95% 以上の一致率で警告 (続行はする)、`--strict-rshogi-defaults`
    /// 指定時は error で停止。
    #[arg(long, default_value_t = false)]
    allow_rshogi_defaults: bool,

    /// rshogi default 値の混入検知を error 化する (CI 用)。
    ///
    /// `--allow-rshogi-defaults` と同時指定するとエラーになります (両フラグは意味が矛盾します)。
    #[arg(long, default_value_t = false)]
    strict_rshogi_defaults: bool,
}

/// rshogi default 一致が閾値超過と判定する一致率 (95%)。
///
/// `generate_spsa_params` の出力をそのまま入力にすると 100% 一致するので
/// 確実に発火。本物の SPSA tuned params は数十パーセント単位で値が動くため
/// 通常は 95% 未満に収まる。意図的に default 開始したい少数派は
/// `--allow-rshogi-defaults` で警告抑制可能。
const DEFAULT_MATCH_WARN_RATE: f64 = 0.95;

/// rshogi default 一致検知の結果。
#[derive(Debug, Clone)]
struct DefaultMatchReport {
    /// rshogi default を持つパラメータの総数 (option_specs と入力の交差)
    checked: usize,
    /// 上記のうち入力値が default と完全一致したもの
    matched: usize,
}

impl DefaultMatchReport {
    fn match_rate(&self) -> f64 {
        if self.checked == 0 {
            0.0
        } else {
            self.matched as f64 / self.checked as f64
        }
    }
}

/// 入力 rshogi `.params` の値列が `SearchTuneParams::option_specs()` の default と
/// どの程度一致しているかを集計する。
///
/// 一致率の閾値判定 (95%) は呼び出し側で行う。ここではカウントだけ返す。
///
/// **重複名の扱い**: 入力に同名 row が複数ある場合、後段の変換ロジック
/// (`rshogi_by_name: HashMap<&str, &ParamRow>`) は last-write-wins で 1 entry に
/// 集約する。検知側もそれと一致させるため、HashMap で重複排除してから集計する。
/// (旧実装は全行カウントで、重複入力時に検知と実動作が乖離するバグがあった)
fn detect_rshogi_default_match(rshogi_rows: &[ParamRow]) -> DefaultMatchReport {
    let defaults: HashMap<&str, i32> = SearchTuneParams::option_specs()
        .iter()
        .map(|s| (s.usi_name, s.default))
        .collect();
    // 後段の変換と同じ per-name view を作る (last-write-wins)
    let by_name: HashMap<&str, &ParamRow> =
        rshogi_rows.iter().map(|r| (r.name.as_str(), r)).collect();
    let mut checked = 0usize;
    let mut matched = 0usize;
    for (name, row) in &by_name {
        if let Some(&def) = defaults.get(name) {
            checked += 1;
            if row.value == def {
                matched += 1;
            }
        }
    }
    DefaultMatchReport { checked, matched }
}

/// `--base` がない場合に YO 行を簡易生成する
///
/// SPSA は両側に摂動するため、min/max は `value` を中心に対称な range にする。
/// 片側だけ (例 `(0, span)`) にすると正方向の摂動が clamp されて勾配推定が壊れる。
fn synthesize_yo_row(yo_name: &str, value: i32) -> ParamRow {
    let abs_value = value.unsigned_abs() as i32;
    let span = (abs_value * 2).max(8);
    let min = value.saturating_sub(span);
    let max = value.saturating_add(span);
    // YO 側 .params の step は整数前提で扱われるため round する
    // (yo_to_rshogi_params の rshogi_default_rows と整合)
    let step = (((max - min) as f64) / 20.0).max(1.0).round();
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
    if cli.allow_rshogi_defaults && cli.strict_rshogi_defaults {
        bail!(
            "--allow-rshogi-defaults と --strict-rshogi-defaults は同時指定できません \
             (前者は警告抑制、後者は warn → error 昇格で意味が矛盾します)"
        );
    }
    let table = MappingTable::load(&cli.mapping)?;
    let rshogi_rows = load_params(&cli.rshogi_params)?;

    // rshogi default 一致検知: --allow-rshogi-defaults 指定時はスキップ。
    // 95% 以上一致なら警告 (strict 指定時は error)。
    if !cli.allow_rshogi_defaults {
        let report = detect_rshogi_default_match(&rshogi_rows);
        // checked == 0 (= 入力に rshogi 名が 1 件も含まれない) は素通し。
        // YO 名混在ファイル等を「default 値混入」と誤検知しないため。
        if report.checked > 0 && report.match_rate() >= DEFAULT_MATCH_WARN_RATE {
            let msg = format!(
                "入力 rshogi params の値列が rshogi 内部 default と {}/{} ({:.1}%) 一致しています ({}).\n\
                 \n  意図的に default 値から始める場合: --allow-rshogi-defaults を追加してください\n\
                 \n  そうでない場合: 入力ファイルを再確認し、suisho10 等の canonical を渡してください\n      \
                 (`generate_spsa_params` の出力をそのまま入力にしている可能性があります)",
                report.matched,
                report.checked,
                report.match_rate() * 100.0,
                cli.rshogi_params.display()
            );
            if cli.strict_rshogi_defaults {
                bail!("--strict-rshogi-defaults: {msg}");
            } else {
                eprintln!("warn: {msg}");
            }
        }
    }

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

    let index = table.index();

    for yo_name in iter_order {
        let Some(mapping) = index.by_yo(yo_name) else {
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
        .filter(|n| !index.contains_rshogi(n))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_row(name: &str, value: i32) -> ParamRow {
        ParamRow {
            name: name.to_string(),
            kind: "int".to_string(),
            value,
            min: 0,
            max: 1000,
            step: 50.0,
            alpha: 0.002,
            not_used: false,
        }
    }

    #[test]
    fn detect_returns_zero_for_unknown_names() {
        // mapping にも option_specs にもない名前は checked にカウントされない
        let rows = vec![
            make_row("totally_unknown_param", 42),
            make_row("another_made_up", 99),
        ];
        let report = detect_rshogi_default_match(&rows);
        assert_eq!(report.checked, 0);
        assert_eq!(report.matched, 0);
        // match_rate は 0/0 で 0.0 を返す (NaN にならない)
        assert!((report.match_rate() - 0.0).abs() < 1e-9);
    }

    #[test]
    fn detect_counts_only_known_specs() {
        // SearchTuneParams::option_specs() にある最初のパラメータの default を取り、
        // 一致 / 不一致を構築して checked/matched を確認する
        let specs = SearchTuneParams::option_specs();
        assert!(!specs.is_empty(), "option_specs must be non-empty");
        let first = &specs[0];
        let rows = vec![
            make_row(first.usi_name, first.default), // 一致
            make_row("totally_unknown_param", 42),   // unknown (count されない)
        ];
        let report = detect_rshogi_default_match(&rows);
        assert_eq!(report.checked, 1);
        assert_eq!(report.matched, 1);
        assert!((report.match_rate() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn detect_partial_match_below_threshold() {
        // option_specs の先頭 4 件で 1 つだけ一致 → 25% < 95% 閾値
        let specs = SearchTuneParams::option_specs();
        assert!(specs.len() >= 4, "need >=4 specs for this test");
        let rows = vec![
            make_row(specs[0].usi_name, specs[0].default), // 一致
            make_row(specs[1].usi_name, specs[1].default + 12345), // 不一致 (default と確実に異なる値 (+12345 offset))
            make_row(specs[2].usi_name, specs[2].default + 12345),
            make_row(specs[3].usi_name, specs[3].default + 12345),
        ];
        let report = detect_rshogi_default_match(&rows);
        assert_eq!(report.checked, 4);
        assert_eq!(report.matched, 1);
        assert!(report.match_rate() < DEFAULT_MATCH_WARN_RATE);
    }

    #[test]
    fn detect_match_rate_at_threshold_triggers() {
        // 20 件中 19 件一致 = 95.0% (DEFAULT_MATCH_WARN_RATE ちょうど)
        // `>= 0.95` の境界条件で発火することを担保
        let specs = SearchTuneParams::option_specs();
        assert!(specs.len() >= 20, "need >=20 specs for boundary test");
        let mut rows: Vec<ParamRow> =
            specs.iter().take(20).map(|s| make_row(s.usi_name, s.default)).collect();
        // 1 件だけ default からズラす (default + 12345 で衝突回避)
        rows[0].value = specs[0].default + 12345;
        let report = detect_rshogi_default_match(&rows);
        assert_eq!(report.checked, 20);
        assert_eq!(report.matched, 19);
        let rate = report.match_rate();
        assert!((rate - 0.95).abs() < 1e-9, "match_rate should be exactly 0.95, got {rate}");
        assert!(rate >= DEFAULT_MATCH_WARN_RATE);
    }

    #[test]
    fn detect_full_default_match_triggers_threshold() {
        // option_specs の全件を default 値で構築 → 100% 一致 → 閾値超過
        let specs = SearchTuneParams::option_specs();
        let rows: Vec<ParamRow> = specs.iter().map(|s| make_row(s.usi_name, s.default)).collect();
        let report = detect_rshogi_default_match(&rows);
        assert_eq!(report.checked, specs.len());
        assert_eq!(report.matched, specs.len());
        assert!(report.match_rate() >= DEFAULT_MATCH_WARN_RATE);
    }

    #[test]
    fn detect_handles_duplicate_names_with_last_write_wins() {
        // 同名 row が複数あるとき、後段の rshogi_to_yo_params 変換は HashMap
        // (last-write-wins) で 1 entry に集約する。検知側も同じ per-name view で
        // 数え上げないと、--strict-rshogi-defaults で false positive を起こす。
        let specs = SearchTuneParams::option_specs();
        assert!(!specs.is_empty(), "option_specs must be non-empty");
        let first = &specs[0];
        // 同名で 3 つの row。Vec 順は default → default → default+12345 (last は不一致)
        let rows = vec![
            make_row(first.usi_name, first.default),
            make_row(first.usi_name, first.default),
            make_row(first.usi_name, first.default + 12345),
        ];
        let report = detect_rshogi_default_match(&rows);
        // by_name 集約後は 1 entry のみ。HashMap の last-write-wins で値は不一致になる
        // 可能性があるが、HashMap iteration 順は不定なのでどの value が採用されるかは
        // 実装依存。検知件数 (checked) が 3 ではなく 1 であることだけ保証する。
        assert_eq!(
            report.checked, 1,
            "duplicates should be deduplicated to match conversion logic"
        );
    }

    // 注: --allow-rshogi-defaults と --strict-rshogi-defaults の同時指定 bail は
    // main() レベルでしか検証できない (clap parse 自体は成功するが main 内で弾く)。
    // unit test では検証困難なため、PR description / runbook の smoke test 4
    // シナリオで担保している。
}
