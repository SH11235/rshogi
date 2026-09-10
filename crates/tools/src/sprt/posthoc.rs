//! tournament JSONL からの post-hoc Pentanomial 集計。

use std::collections::{BTreeMap, HashSet};
use std::io::{BufRead, BufReader};

use anyhow::{Context as _, Result};
use serde::Deserialize;

use super::{GameSide, Penta};

#[derive(Deserialize)]
struct MetaLog {
    engine_cmd: EngineCommandMeta,
}

#[derive(Deserialize)]
struct EngineCommandMeta {
    path_black: String,
    path_white: String,
    #[serde(default)]
    label_black: Option<String>,
    #[serde(default)]
    label_white: Option<String>,
}

#[derive(Deserialize)]
struct ResultLog {
    outcome: String,
    #[serde(default)]
    winner: Option<String>,
    #[serde(default)]
    pair_index: Option<u32>,
    #[serde(default)]
    pair_slot: Option<u32>,
    #[serde(default)]
    attempt: u32,
    #[serde(default)]
    error: Option<bool>,
}

#[derive(Default)]
struct PairObservation {
    sides: [Option<GameSide>; 2],
    received: [bool; 2],
    error: bool,
}

fn extract_engine_id(path: &str) -> String {
    let filename = std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path);
    if let Some(rest) = filename.strip_prefix("rshogi-usi-") {
        let hash: String = rest.chars().take(8).collect();
        if !hash.is_empty() {
            return hash;
        }
    }
    filename.to_string()
}

/// outcome と先後交換 slot から勝者ラベルを解決する。
///
/// 明示 winner を優先する。省略時は slot 0 が meta の先後、slot 1 が逆順。
/// 引分・未完了・不明な outcome / slot は None。
pub fn result_winner_label<'a>(
    outcome: &str,
    winner: Option<&'a str>,
    slot: usize,
    label_black_meta: &'a str,
    label_white_meta: &'a str,
) -> Option<&'a str> {
    if !matches!(outcome, "black_win" | "white_win") {
        return None;
    }
    if let Some(winner) = winner {
        return Some(winner);
    }
    match (outcome, slot) {
        ("black_win", 0) | ("white_win", 1) => Some(label_black_meta),
        ("white_win", 0) | ("black_win", 1) => Some(label_white_meta),
        _ => None,
    }
}

fn result_test_side(
    result: &ResultLog,
    slot: usize,
    label_black_meta: &str,
    label_white_meta: &str,
    base: &str,
    test: &str,
) -> Option<GameSide> {
    if result.outcome == "draw" {
        return Some(GameSide::Draw);
    }
    match result_winner_label(
        &result.outcome,
        result.winner.as_deref(),
        slot,
        label_black_meta,
        label_white_meta,
    ) {
        Some(winner) if winner == test => Some(GameSide::Win),
        Some(winner) if winner == base => Some(GameSide::Loss),
        _ => None,
    }
}

/// 単一 tournament JSONL から base/test ペアの Pentanomial を集計する。
///
/// 同じ `(pair_index, attempt, slot)` の重複行は警告して除外し、error を含む世代と
/// 片スロットしかない未完了世代は集計しない。
pub fn collect_sprt_penta(path: &str, base: &str, test: &str) -> Result<Penta> {
    let file =
        std::fs::File::open(path).with_context(|| format!("ファイルを開けません: {path}"))?;
    let reader = BufReader::new(file);
    let mut meta_labels: Option<(String, String)> = None;
    let mut pair_buffer: BTreeMap<(u32, u32), PairObservation> = BTreeMap::new();
    let mut completed_pairs = HashSet::new();
    let mut total = Penta::ZERO;
    let mut seq = 0u32;
    let mut warned_missing_pair_index = false;

    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: serde_json::Value =
            serde_json::from_str(trimmed).with_context(|| format!("JSONパースエラー: {path}"))?;
        if meta_labels.is_none() && value["type"] == "meta" {
            let meta: MetaLog = serde_json::from_value(value)
                .with_context(|| format!("metaパースエラー: {path}"))?;
            let black = meta
                .engine_cmd
                .label_black
                .unwrap_or_else(|| extract_engine_id(&meta.engine_cmd.path_black));
            let white = meta
                .engine_cmd
                .label_white
                .unwrap_or_else(|| extract_engine_id(&meta.engine_cmd.path_white));
            if !((black == base && white == test) || (black == test && white == base)) {
                return Ok(Penta::ZERO);
            }
            meta_labels = Some((black, white));
        } else if value["type"] == "result" {
            let Some((label_black_meta, label_white_meta)) = meta_labels.as_ref() else {
                continue;
            };
            let result: ResultLog = serde_json::from_value(value)
                .with_context(|| format!("resultパースエラー: {path}"))?;
            if result.pair_index.is_none() && !warned_missing_pair_index {
                eprintln!(
                    "警告: {path} は pair_index を含まない旧形式ログです。\n\
                     SPRT ペアリングは result の出現順 (seq / 2, seq % 2) でフォールバックしますが、\n\
                     並列対局ログでは完了順がずれている可能性があるため結果は正確でない場合があります。"
                );
                warned_missing_pair_index = true;
            }
            let pair_index = result.pair_index.unwrap_or(seq / 2);
            let slot = result.pair_slot.unwrap_or(seq % 2).min(1) as usize;
            seq += 1;
            let key = (pair_index, result.attempt);
            if completed_pairs.contains(&key) {
                eprintln!(
                    "警告: {path} — pair_index={pair_index}, attempt={} は既に集計済みです。重複結果を除外します。",
                    result.attempt
                );
                continue;
            }
            let entry = pair_buffer.entry(key).or_default();
            if entry.received[slot] {
                eprintln!(
                    "警告: {path} — pair_index={pair_index}, attempt={} の slot={slot} が重複しています。重複結果を除外します。",
                    result.attempt
                );
                continue;
            }
            entry.received[slot] = true;
            entry.error |= result.error.unwrap_or(false);
            if !result.error.unwrap_or(false) {
                let Some(side) =
                    result_test_side(&result, slot, label_black_meta, label_white_meta, base, test)
                else {
                    continue;
                };
                entry.sides[slot] = Some(side);
            }
            if entry.received.into_iter().all(|received| received) {
                let completed = pair_buffer
                    .remove(&key)
                    .with_context(|| format!("SPRT ペア集計状態が失われました: {path}"))?;
                if !completed.error
                    && let (Some(a), Some(b)) = (completed.sides[0], completed.sides[1])
                {
                    total += Penta::from_pair(a, b);
                }
                completed_pairs.insert(key);
            }
        }
    }
    if !pair_buffer.is_empty() {
        eprintln!(
            "情報: {path} — {} ペアが未完了（片スロット欠け）のため SPRT 集計から除外されました",
            pair_buffer.len()
        );
    }
    Ok(total)
}
