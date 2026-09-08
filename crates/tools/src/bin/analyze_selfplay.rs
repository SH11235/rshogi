/// 自己対局ログの集計ツール
///
/// 使い方:
///   # 明示的なファイルパス指定
///   analyze_selfplay file1.jsonl file2.jsonl
///
///   # glob展開はシェル側で行う
///   analyze_selfplay runs/selfplay/20260206-14*.jsonl
///
///   # JSON出力モード
///   analyze_selfplay --json file1.jsonl file2.jsonl
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Context, Result, bail};
use clap::Parser;
use serde::{Deserialize, Serialize};

use tools::sprt::{Penta, SprtMetaLog, SprtParameters, collect_sprt_penta, judge};

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(about = "自己対局ログの集計")]
struct Cli {
    /// 集計対象のJSONLファイルパス（複数指定可）
    #[arg(required = true)]
    files: Vec<String>,

    /// JSON出力モード
    #[arg(long)]
    json: bool,

    /// SPRT post-hoc 判定表示を有効化。
    /// ラベルは CLI → meta の SPRT 情報 → meta のラベル情報（base_label 記録 /
    /// "base" を含むラベル名等）の順で自動推定し、推定時は根拠を表示する。
    /// 数値パラメータは CLI → meta → ハードコード fallback
    /// (nelo0=0, nelo1=5, alpha=0.05, beta=0.05) の順で解決する。
    #[arg(long, default_value_t = false)]
    sprt: bool,

    /// H1 側（challenger / test）のラベル。未指定時は meta から推定。
    #[arg(long)]
    sprt_test_label: Option<String>,

    /// H0 側（base）のラベル。未指定時は meta から推定。
    #[arg(long)]
    sprt_base_label: Option<String>,

    /// H0 仮説の正規化 Elo。未指定時は meta → ハードコード fallback (0.0) の順で解決。
    #[arg(long)]
    sprt_nelo0: Option<f64>,

    /// H1 仮説の正規化 Elo。未指定時は meta → ハードコード fallback (5.0) の順で解決。
    #[arg(long)]
    sprt_nelo1: Option<f64>,

    /// 第一種過誤率 α。未指定時は meta → ハードコード fallback (0.05) の順で解決。
    #[arg(long)]
    sprt_alpha: Option<f64>,

    /// 第二種過誤率 β。未指定時は meta → ハードコード fallback (0.05) の順で解決。
    #[arg(long)]
    sprt_beta: Option<f64>,
}

// ---------------------------------------------------------------------------
// JSONL読み取り用の構造体（デシリアライズのみ）
// ---------------------------------------------------------------------------

/// 通常JSONLのmeta行
#[derive(Deserialize)]
struct MetaLog {
    settings: MetaSettings,
    engine_cmd: EngineCommandMeta,
    /// tournament.rs が base-vs-N モード（--base-label）時のみ出力。
    /// SPRT post-hoc のラベル役割（base / test）推定に使う。
    #[serde(default)]
    base_label: Option<String>,
    /// tournament.rs が --sprt 実行時のみ出力。未指定時のラベル自動推定に使う。
    #[serde(default)]
    sprt: Option<SprtMetaLog>,
}

#[derive(Deserialize)]
struct MetaSettings {
    games: u32,
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

/// 通常JSONLのresult行
#[derive(Clone, Deserialize)]
struct ResultLog {
    #[serde(default)]
    reason: Option<String>,
    outcome: String,
    /// 勝者のエンジンラベル（tournament.rs が出力、旧形式では None）
    #[serde(default)]
    winner: Option<String>,
    #[serde(default)]
    plies: u32,
    /// SPRT post-hoc 解析用の追加メタ（tournament.rs が出力、旧形式では None）
    #[serde(default)]
    pair_index: Option<u32>,
    #[serde(default)]
    pair_slot: Option<u32>,
    #[serde(default)]
    attempt: u32,
    #[serde(default)]
    error: Option<bool>,
}

/// 通常JSONLのmove行
#[derive(Deserialize)]
struct MoveLog {
    game_id: u32,
    ply: u32,
    side_to_move: String,
    engine: String,
    elapsed_ms: u64,
    think_limit_ms: u64,
    timed_out: bool,
    #[serde(default)]
    eval: Option<MoveEval>,
}

#[derive(Deserialize)]
struct MoveEval {
    #[serde(default)]
    nps: Option<u64>,
    #[serde(default)]
    depth: Option<u32>,
    #[serde(default)]
    seldepth: Option<u32>,
    #[serde(default)]
    nodes: Option<u64>,
}

/// summary JSONLの行
#[derive(Deserialize)]
struct SummaryLog {
    total_games: u32,
    black_wins: u32,
    white_wins: u32,
    draws: u32,
    engine_black: EngineSummary,
    engine_white: EngineSummary,
}

#[derive(Deserialize)]
struct EngineSummary {
    path: String,
}

// ---------------------------------------------------------------------------
// 集計用の構造体
// ---------------------------------------------------------------------------

/// 1ファイルのパース結果
struct FileResult {
    black: String,
    white: String,
    games: u32,
    black_wins: u32,
    white_wins: u32,
    draws: u32,
    done: u32,
    /// meta.black エンジンが先手として対局した数・勝数
    a_sente_games: u32,
    a_sente_wins: u32,
    /// meta.white エンジンが先手として対局した数・勝数
    b_sente_games: u32,
    b_sente_wins: u32,
    retry: RetryAnalysisStats,
    extra: FileExtraStats,
}

#[derive(Clone, Copy, Default)]
struct RetryAnalysisStats {
    error_games: u32,
    error_pairs: u32,
    retried_pairs: u32,
    exhausted_pairs: u32,
}

/// 対戦カード（先手, 後手）ごとの集計
#[derive(Default)]
struct MatchupStats {
    total: u32,
    done: u32,
    black_wins: u32,
    white_wins: u32,
    draws: u32,
    files: u32,
    /// meta.black エンジンの先手対局数・先手勝ち数
    a_sente_games: u32,
    a_sente_wins: u32,
    /// meta.white エンジンの先手対局数・先手勝ち数
    b_sente_games: u32,
    b_sente_wins: u32,
}

/// エンジン別の集計
#[derive(Default)]
struct EngineStats {
    games: u32,
    wins: u32,
    losses: u32,
    draws: u32,
    /// 先手時の対局数・勝数
    sente_games: u32,
    sente_wins: u32,
    /// 後手時の対局数・勝数
    gote_games: u32,
    gote_wins: u32,
}

/// 直接対決の集計（先後合算、正規化済み）
#[derive(Default)]
struct HeadToHeadStats {
    done: u32,
    left_wins: u32,
    right_wins: u32,
    draws: u32,
    /// left エンジンの先手決着局数・先手勝数
    left_sente_games: u32,
    left_sente_wins: u32,
    /// left エンジンの後手決着局数・後手勝数
    left_gote_games: u32,
    left_gote_wins: u32,
}

#[derive(Default)]
struct FileExtraStats {
    reasons: BTreeMap<String, u32>,
    total_plies: u64,
    completed_games: u32,
    black_wins: u32,
    white_wins: u32,
    draws: u32,
    engine_moves: BTreeMap<String, EngineMoveStats>,
}

#[derive(Default)]
struct ParsedGameStats {
    black_wins: u32,
    white_wins: u32,
    draws: u32,
    a_sente_games: u32,
    a_sente_wins: u32,
    b_sente_games: u32,
    b_sente_wins: u32,
    extra: FileExtraStats,
}

fn record_valid_result(
    result: &ResultLog,
    black: &str,
    white: &str,
    meta_parsed: bool,
    stats: &mut ParsedGameStats,
) {
    stats.extra.completed_games += 1;
    stats.extra.total_plies += result.plies as u64;
    if let Some(winner) = result.winner.as_ref() {
        let winner_id = if meta_parsed && (black == winner || white == winner) {
            winner.clone()
        } else {
            extract_engine_id(winner)
        };
        if winner_id == black {
            stats.black_wins += 1;
        } else if winner_id == white {
            stats.white_wins += 1;
        }
        match result.outcome.as_str() {
            "black_win" => {
                stats.extra.black_wins += 1;
                if winner_id == black {
                    stats.a_sente_games += 1;
                    stats.a_sente_wins += 1;
                } else if winner_id == white {
                    stats.b_sente_games += 1;
                    stats.b_sente_wins += 1;
                }
            }
            "white_win" => {
                stats.extra.white_wins += 1;
                if winner_id == black {
                    stats.b_sente_games += 1;
                } else if winner_id == white {
                    stats.a_sente_games += 1;
                }
            }
            "draw" => stats.extra.draws += 1,
            _ => {}
        }
    } else {
        match result.outcome.as_str() {
            "black_win" => {
                stats.black_wins += 1;
                stats.extra.black_wins += 1;
            }
            "white_win" => {
                stats.white_wins += 1;
                stats.extra.white_wins += 1;
            }
            "draw" => {
                stats.draws += 1;
                stats.extra.draws += 1;
            }
            _ => {}
        }
    }
}

#[derive(Default)]
struct EngineMoveStats {
    moves: u64,
    elapsed_ms_sum: u64,
    think_limit_ms_sum: u64,
    timed_out: u32,
    eval_nps_sum: u128,
    eval_nps_count: u64,
    eval_depth_sum: u64,
    eval_depth_count: u64,
    eval_seldepth_sum: u64,
    eval_seldepth_count: u64,
    eval_nodes_sum: u128,
    eval_nodes_count: u64,
    by_side: BTreeMap<String, MoveBucketStats>,
    by_ply_band: BTreeMap<String, MoveBucketStats>,
}

#[derive(Default, Clone)]
struct MoveBucketStats {
    moves: u64,
    elapsed_ms_sum: u64,
}

#[derive(Default)]
struct AggregatedExtraStats {
    reasons: BTreeMap<String, u32>,
    total_plies: u64,
    completed_games: u32,
    black_wins: u32,
    white_wins: u32,
    draws: u32,
    retry: RetryAnalysisStats,
    engine_moves: BTreeMap<String, EngineMoveStats>,
}

/// JSON出力用
#[derive(Serialize)]
struct JsonOutput {
    files: u32,
    progress: Progress,
    matchups: Vec<JsonMatchup>,
    engines: Vec<JsonEngine>,
    head_to_head: Vec<JsonHeadToHead>,
    extra: JsonExtra,
    #[serde(skip_serializing_if = "Option::is_none")]
    sprt: Option<SprtJsonOutput>,
}

#[derive(Serialize)]
struct Progress {
    done: u32,
    total: u32,
    percent: f64,
}

#[derive(Serialize)]
struct JsonMatchup {
    black: String,
    white: String,
    done: u32,
    total: u32,
    black_wins: u32,
    white_wins: u32,
    draws: u32,
    files: u32,
}

#[derive(Serialize)]
struct JsonEngine {
    id: String,
    games: u32,
    wins: u32,
    losses: u32,
    draws: u32,
    win_rate: f64,
}

#[derive(Serialize)]
struct JsonHeadToHead {
    engine_a: String,
    engine_b: String,
    done: u32,
    a_wins: u32,
    b_wins: u32,
    draws: u32,
    a_win_rate: f64,
    elo_diff: Option<f64>,
    elo_ci95: Option<f64>,
}

#[derive(Serialize)]
struct JsonExtra {
    reasons: BTreeMap<String, u32>,
    average_plies: f64,
    black_win_rate_decisive: f64,
    white_win_rate_decisive: f64,
    completed_games: u32,
    draws: u32,
    error_games: u32,
    error_pairs: u32,
    retried_pairs: u32,
    exhausted_pairs: u32,
    invalid: bool,
    engine_timing: Vec<JsonEngineTiming>,
}

#[derive(Serialize)]
struct JsonEngineTiming {
    id: String,
    moves: u64,
    average_elapsed_ms: f64,
    average_think_limit_ms: f64,
    timed_out: u32,
    average_nps: Option<f64>,
    average_depth: Option<f64>,
    average_seldepth: Option<f64>,
    average_nodes: Option<f64>,
    by_side: Vec<JsonTimingBucket>,
    by_ply_band: Vec<JsonTimingBucket>,
}

#[derive(Serialize)]
struct JsonTimingBucket {
    label: String,
    moves: u64,
    average_elapsed_ms: f64,
}

// ---------------------------------------------------------------------------
// エンジンID抽出
// ---------------------------------------------------------------------------

/// パスから `rshogi-usi-HASH` パターンのハッシュ部分（先頭8文字）を抽出する。
/// 該当しない場合はファイル名全体を返す。
fn extract_engine_id(path: &str) -> String {
    let filename = Path::new(path).file_name().and_then(|s| s.to_str()).unwrap_or(path);

    if let Some(rest) = filename.strip_prefix("rshogi-usi-") {
        // ハッシュ部分の先頭8文字を取る
        let hash: String = rest.chars().take(8).collect();
        if !hash.is_empty() {
            return hash;
        }
    }
    filename.to_string()
}

fn normalize_engine_name(name: &str, black: &str, white: &str, meta_parsed: bool) -> String {
    if meta_parsed && (name == black || name == white) {
        name.to_string()
    } else {
        extract_engine_id(name)
    }
}

fn ply_band_label(ply: u32) -> &'static str {
    match ply {
        1..=40 => "1-40",
        41..=80 => "41-80",
        81..=120 => "81-120",
        _ => "121+",
    }
}

fn update_move_bucket(stats: &mut MoveBucketStats, elapsed_ms: u64) {
    stats.moves += 1;
    stats.elapsed_ms_sum += elapsed_ms;
}

fn merge_engine_move_stats(dst: &mut EngineMoveStats, src: &EngineMoveStats) {
    dst.moves += src.moves;
    dst.elapsed_ms_sum += src.elapsed_ms_sum;
    dst.think_limit_ms_sum += src.think_limit_ms_sum;
    dst.timed_out += src.timed_out;
    dst.eval_nps_sum += src.eval_nps_sum;
    dst.eval_nps_count += src.eval_nps_count;
    dst.eval_depth_sum += src.eval_depth_sum;
    dst.eval_depth_count += src.eval_depth_count;
    dst.eval_seldepth_sum += src.eval_seldepth_sum;
    dst.eval_seldepth_count += src.eval_seldepth_count;
    dst.eval_nodes_sum += src.eval_nodes_sum;
    dst.eval_nodes_count += src.eval_nodes_count;
    for (label, bucket) in &src.by_side {
        let dst_bucket = dst.by_side.entry(label.clone()).or_default();
        dst_bucket.moves += bucket.moves;
        dst_bucket.elapsed_ms_sum += bucket.elapsed_ms_sum;
    }
    for (label, bucket) in &src.by_ply_band {
        let dst_bucket = dst.by_ply_band.entry(label.clone()).or_default();
        dst_bucket.moves += bucket.moves;
        dst_bucket.elapsed_ms_sum += bucket.elapsed_ms_sum;
    }
}

fn average(sum: u64, count: u64) -> f64 {
    if count == 0 {
        0.0
    } else {
        sum as f64 / count as f64
    }
}

// ---------------------------------------------------------------------------
// ファイルパース
// ---------------------------------------------------------------------------

fn parse_summary_file(path: &str) -> Result<FileResult> {
    let file =
        std::fs::File::open(path).with_context(|| format!("ファイルを開けません: {path}"))?;
    let reader = BufReader::new(file);

    // summary ファイルは通常1行
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let summary: SummaryLog =
            serde_json::from_str(&line).with_context(|| format!("JSONパースエラー: {path}"))?;
        let done = summary.black_wins + summary.white_wins + summary.draws;
        return Ok(FileResult {
            black: extract_engine_id(&summary.engine_black.path),
            white: extract_engine_id(&summary.engine_white.path),
            games: summary.total_games,
            black_wins: summary.black_wins,
            white_wins: summary.white_wins,
            draws: summary.draws,
            done,
            // summary形式では先後別情報なし
            a_sente_games: 0,
            a_sente_wins: 0,
            b_sente_games: 0,
            b_sente_wins: 0,
            retry: RetryAnalysisStats::default(),
            extra: FileExtraStats::default(),
        });
    }
    bail!("空のsummaryファイル: {path}");
}

fn parse_normal_file(path: &str) -> Result<FileResult> {
    let file =
        std::fs::File::open(path).with_context(|| format!("ファイルを開けません: {path}"))?;
    let reader = BufReader::new(file);

    let mut games: u32 = 0;
    let mut black = String::new();
    let mut white = String::new();
    let mut meta_parsed = false;
    let mut stats = ParsedGameStats::default();
    let mut pair_buffer: BTreeMap<(u32, u32), [Option<ResultLog>; 2]> = BTreeMap::new();
    let mut completed_pairs: HashSet<(u32, u32)> = HashSet::new();
    let mut seq = 0u32;
    let mut error_pairs = HashSet::new();
    let mut retried_pairs = HashSet::new();
    let mut exhausted_pairs = HashSet::new();
    let mut error_games = 0u32;

    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // 高速フィルタ: type フィールドで判別
        if !meta_parsed && trimmed.contains("\"type\":\"meta\"") {
            let meta: MetaLog = serde_json::from_str(trimmed)
                .with_context(|| format!("metaパースエラー: {path}"))?;
            games = meta.settings.games;
            black = meta
                .engine_cmd
                .label_black
                .unwrap_or_else(|| extract_engine_id(&meta.engine_cmd.path_black));
            white = meta
                .engine_cmd
                .label_white
                .unwrap_or_else(|| extract_engine_id(&meta.engine_cmd.path_white));
            meta_parsed = true;
        } else if trimmed.contains("\"type\":\"move\"") {
            let mv: MoveLog = serde_json::from_str(trimmed)
                .with_context(|| format!("moveパースエラー: {path}"))?;
            let _ = mv.game_id;
            let engine_name = normalize_engine_name(&mv.engine, &black, &white, meta_parsed);
            let engine_stats = stats.extra.engine_moves.entry(engine_name).or_default();
            engine_stats.moves += 1;
            engine_stats.elapsed_ms_sum += mv.elapsed_ms;
            engine_stats.think_limit_ms_sum += mv.think_limit_ms;
            if mv.timed_out {
                engine_stats.timed_out += 1;
            }
            if let Some(eval) = mv.eval {
                if let Some(nps) = eval.nps {
                    engine_stats.eval_nps_sum += nps as u128;
                    engine_stats.eval_nps_count += 1;
                }
                if let Some(depth) = eval.depth {
                    engine_stats.eval_depth_sum += depth as u64;
                    engine_stats.eval_depth_count += 1;
                }
                if let Some(seldepth) = eval.seldepth {
                    engine_stats.eval_seldepth_sum += seldepth as u64;
                    engine_stats.eval_seldepth_count += 1;
                }
                if let Some(nodes) = eval.nodes {
                    engine_stats.eval_nodes_sum += nodes as u128;
                    engine_stats.eval_nodes_count += 1;
                }
            }
            update_move_bucket(
                engine_stats.by_side.entry(mv.side_to_move).or_default(),
                mv.elapsed_ms,
            );
            update_move_bucket(
                engine_stats.by_ply_band.entry(ply_band_label(mv.ply).to_string()).or_default(),
                mv.elapsed_ms,
            );
        } else if trimmed.contains("\"type\":\"result\"") {
            let result: ResultLog = serde_json::from_str(trimmed)
                .with_context(|| format!("resultパースエラー: {path}"))?;
            let reason = match result.reason.as_deref() {
                Some(reason) if reason.starts_with("error") => "error",
                Some(reason) => reason,
                None => "unknown",
            };
            *stats.extra.reasons.entry(reason.to_string()).or_default() += 1;
            let pair_index = result.pair_index.unwrap_or(seq / 2);
            let slot = result.pair_slot.unwrap_or(seq % 2).min(1) as usize;
            seq += 1;
            if result.error.unwrap_or(false) {
                error_games += 1;
                error_pairs.insert(pair_index);
            }
            if result.attempt > 0 {
                retried_pairs.insert(pair_index);
            }
            let key = (pair_index, result.attempt);
            if completed_pairs.contains(&key) {
                eprintln!(
                    "警告: {path} — 直接対決の pair_index={pair_index}, attempt={}, slot={slot} は既に集計済みです。重複結果を除外します。",
                    result.attempt
                );
                continue;
            }
            let entry = pair_buffer.entry(key).or_insert([None, None]);
            if entry[slot].is_some() {
                eprintln!(
                    "警告: {path} — 直接対決の pair_index={pair_index}, attempt={}, slot={slot} が重複しています。重複結果を除外します。",
                    result.attempt
                );
                continue;
            }
            entry[slot] = Some(result);
            if entry.iter().all(Option::is_some) {
                let completed = pair_buffer
                    .remove(&key)
                    .with_context(|| format!("ペア集計状態が失われました: {path}"))?;
                let has_error = completed.iter().flatten().any(|game| game.error.unwrap_or(false));
                if has_error {
                    if key.1 >= 2 {
                        exhausted_pairs.insert(pair_index);
                    }
                } else {
                    for game in completed.iter().flatten() {
                        record_valid_result(game, &black, &white, meta_parsed, &mut stats);
                    }
                }
                completed_pairs.insert(key);
            }
        }
        // move行・metrics行等はスキップ
    }

    if !pair_buffer.is_empty() {
        eprintln!(
            "情報: {path} — {} ペアが未完了（片スロット欠け）のため直接対決集計から除外されました",
            pair_buffer.len()
        );
    }

    let done = stats.black_wins + stats.white_wins + stats.draws;
    Ok(FileResult {
        black,
        white,
        games,
        black_wins: stats.black_wins,
        white_wins: stats.white_wins,
        draws: stats.draws,
        done,
        a_sente_games: stats.a_sente_games,
        a_sente_wins: stats.a_sente_wins,
        b_sente_games: stats.b_sente_games,
        b_sente_wins: stats.b_sente_wins,
        retry: RetryAnalysisStats {
            error_games,
            error_pairs: error_pairs.len() as u32,
            retried_pairs: retried_pairs.len() as u32,
            exhausted_pairs: exhausted_pairs.len() as u32,
        },
        extra: stats.extra,
    })
}

fn parse_file(path: &str) -> Result<FileResult> {
    if path.contains(".summary.") {
        parse_summary_file(path)
    } else {
        parse_normal_file(path)
    }
}

// ---------------------------------------------------------------------------
// SPRT post-hoc 集計
// ---------------------------------------------------------------------------

/// 入力ファイル群の meta 行から SPRT メタを収集し、単一のラベル組/パラメータに合致するなら返す。
///
/// # 動作
/// - meta 行に SPRT 情報が書かれているのは `tournament.rs --sprt` 実行で生成された
///   base/test ペアの jsonl のみ
/// - `cli_base` / `cli_test` が与えられた場合、一致しない meta は無視する（片方のみの指定でも
///   適用。別 run のログが混在しても CLI 明示ラベルが優先して絞り込めるようにする）
/// - 残った meta が複数あり、`(base_label, test_label, nelo0, nelo1, alpha, beta)` が
///   揃って一致するなら採用。ラベル不一致は `bail!`、Wald パラメータ不一致も `bail!`
///   （LLR 境界が変わるため誤集計防止）
/// - どのファイルにも SPRT 情報が無ければ `None`
///   呼び出し側ではラベルは CLI 明示が必須、Wald パラメータはハードコード fallback あり
/// - 先頭非空行が JSON として壊れている場合は警告を出してそのファイルのみスキップ
///   （破損ファイルと旧形式 jsonl を区別するため）
///
/// # 整形済み JSON との互換性
/// この関数は `serde_json::from_str` で行全体をパースするため、整形済み（スペース入り）
/// jsonl でも動作する。一方 `collect_sprt_penta` は `contains` 高速パス前提のため
/// コンパクト JSON のみを想定している点で非対称。tournament.rs はコンパクト出力なので
/// 現状は問題にならない。
fn collect_sprt_meta(
    files: &[&str],
    cli_base: Option<&str>,
    cli_test: Option<&str>,
) -> Result<Option<SprtMetaLog>> {
    let mut found: Option<(SprtMetaLog, String)> = None;
    for &path in files {
        if path.contains(".summary.") {
            continue;
        }
        let Ok(file) = std::fs::File::open(path) else {
            continue;
        };
        let reader = BufReader::new(file);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            // 先頭非空行を JSON として parse。失敗 = 破損 or jsonl 非互換なので警告して次ファイルへ。
            let value: serde_json::Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("警告: {path} 先頭行の JSON パースに失敗しました: {e}");
                    break;
                }
            };
            if value.get("type").and_then(|v| v.as_str()) != Some("meta") {
                // meta 行は各ファイルの先頭 1 行のみ。非 meta 行が出た時点で打ち切り。
                break;
            }
            let meta: MetaLog = serde_json::from_value(value)
                .with_context(|| format!("metaパースエラー: {path}"))?;
            let Some(sprt) = meta.sprt else { break };

            // CLI で明示されたラベルと一致しない meta を無視する（片方のみの指定でも適用）。
            // これにより、異なる run の jsonl が混在していても CLI 明示で解析対象を絞れる。
            // infer_labels_from_meta のフィルタ規則と揃えてあり、片側指定で別 run が
            // 混在しても不一致 bail せずに補完まで到達できる。
            if let Some(cb) = cli_base
                && sprt.base_label != cb
            {
                break;
            }
            if let Some(ct) = cli_test
                && sprt.test_label != ct
            {
                break;
            }

            match found.as_ref() {
                None => found = Some((sprt, path.to_string())),
                Some((existing, existing_path)) => {
                    if existing.base_label != sprt.base_label
                        || existing.test_label != sprt.test_label
                    {
                        bail!(
                            "入力ファイル間で SPRT ラベルが一致しません: {existing_path} は ({} vs {})、{path} は ({} vs {})。\
                             --sprt-base-label / --sprt-test-label を明示してください。",
                            existing.base_label,
                            existing.test_label,
                            sprt.base_label,
                            sprt.test_label
                        );
                    }
                    if existing != &sprt {
                        bail!(
                            "入力ファイル間で SPRT Wald パラメータが一致しません: \
                             {existing_path} は (nelo0={}, nelo1={}, alpha={}, beta={})、\
                             {path} は (nelo0={}, nelo1={}, alpha={}, beta={})。\
                             --sprt-nelo0 / --sprt-nelo1 / --sprt-alpha / --sprt-beta を明示してください。",
                            existing.nelo0,
                            existing.nelo1,
                            existing.alpha,
                            existing.beta,
                            sprt.nelo0,
                            sprt.nelo1,
                            sprt.alpha,
                            sprt.beta
                        );
                    }
                }
            }
            break;
        }
    }
    Ok(found.map(|(m, _)| m))
}

/// meta 行のラベル情報から推定した SPRT のラベル役割。
#[derive(Debug)]
struct InferredLabels {
    base: String,
    test: String,
    /// 推定根拠（notice 表示用）
    note: String,
    /// 根拠なしの既定（label_black=test）に落ちた場合 true。呼び出し側で警告に格上げする
    assumed: bool,
}

/// SPRT meta を持たないログ向けに、meta 行の `label_black` / `label_white` /
/// `base_label` から base / test の役割を推定する。
///
/// 役割の決定優先順:
/// 1. CLI で片方のみ指定 → もう片方をペアの残りラベルで補完
/// 2. meta の `base_label`（tournament.rs が base-vs-N モードで記録）
/// 3. ラベル名に "base" を含む側が一意なら、それを base
/// 4. label_black を test とみなす既定（`assumed=true`）
///
/// CLI ラベルが与えられた場合、それを含まない meta は別 run とみなして無視する。
/// 残った meta 間でラベル組（順不同）が一致しない場合、または同一ラベル組でも
/// `base_label` 記録が矛盾する場合は bail。
/// meta が読めない / 同一ラベル同士で役割を割り当てられない場合は `None`。
fn infer_labels_from_meta(
    files: &[&str],
    cli_base: Option<&str>,
    cli_test: Option<&str>,
) -> Result<Option<InferredLabels>> {
    // (label_black, label_white, meta の base_label, パス)
    let mut found: Option<(String, String, Option<String>, String)> = None;
    for &path in files {
        if path.contains(".summary.") {
            continue;
        }
        let Ok(file) = std::fs::File::open(path) else {
            continue;
        };
        let reader = BufReader::new(file);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            // 破損行は collect_sprt_meta が同じ走査で警告済みのため、ここでは黙ってスキップ
            let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
                break;
            };
            if value.get("type").and_then(|v| v.as_str()) != Some("meta") {
                break;
            }
            let meta: MetaLog = serde_json::from_value(value)
                .with_context(|| format!("metaパースエラー: {path}"))?;
            let black = meta
                .engine_cmd
                .label_black
                .clone()
                .unwrap_or_else(|| extract_engine_id(&meta.engine_cmd.path_black));
            let white = meta
                .engine_cmd
                .label_white
                .clone()
                .unwrap_or_else(|| extract_engine_id(&meta.engine_cmd.path_white));
            // 同一ラベル同士は役割を割り当てられない
            if black == white {
                break;
            }
            // CLI 指定ラベルを含まない meta は別 run とみなして無視
            if let Some(cb) = cli_base
                && cb != black
                && cb != white
            {
                break;
            }
            if let Some(ct) = cli_test
                && ct != black
                && ct != white
            {
                break;
            }
            match found.as_mut() {
                None => found = Some((black, white, meta.base_label.clone(), path.to_string())),
                Some((eb, ew, ebase, epath)) => {
                    let same_pair =
                        (*eb == black && *ew == white) || (*eb == white && *ew == black);
                    if !same_pair {
                        bail!(
                            "入力ファイル間でラベル組が一致しません: {epath} は ({eb}, {ew})、\
                             {path} は ({black}, {white})。\
                             --sprt-base-label / --sprt-test-label を明示してください"
                        );
                    }
                    // base_label 記録の矛盾は base/test の符号反転に直結するため黙認しない
                    match (ebase.as_ref(), meta.base_label.as_ref()) {
                        (Some(a), Some(b)) if a != b => bail!(
                            "入力ファイル間で meta の base_label が一致しません: \
                             {epath} は {a}、{path} は {b}。\
                             --sprt-base-label を明示してください"
                        ),
                        (None, Some(_)) => *ebase = meta.base_label.clone(),
                        _ => {}
                    }
                }
            }
            // meta 行を処理済み。残り行は不要なので内側ループを抜ける
            break;
        }
    }
    let Some((black, white, meta_base, _)) = found else {
        return Ok(None);
    };
    let other = |one: &str| {
        if one == black {
            white.clone()
        } else {
            black.clone()
        }
    };
    let inferred = if let Some(cb) = cli_base {
        InferredLabels {
            test: other(cb),
            base: cb.to_string(),
            note: "--sprt-base-label 指定からもう片方を test と判断".to_string(),
            assumed: false,
        }
    } else if let Some(ct) = cli_test {
        InferredLabels {
            base: other(ct),
            test: ct.to_string(),
            note: "--sprt-test-label 指定からもう片方を base と判断".to_string(),
            assumed: false,
        }
    } else if let Some(mb) = meta_base.as_ref().filter(|m| **m == black || **m == white) {
        InferredLabels {
            test: other(mb),
            base: mb.clone(),
            note: "meta の base_label 記録（tournament --base-label）から判断".to_string(),
            assumed: false,
        }
    } else {
        // base_label 記録がラベル組に含まれない（別 run の混入や stale な記録）場合、
        // 黙ってヒューリスティックに落とすと推定根拠を誤解させるため通知する
        if let Some(mb) = &meta_base {
            eprintln!(
                "警告: meta の base_label ({mb}) がラベル組 ({black}, {white}) に含まれないため無視します"
            );
        }
        let black_has_base = black.to_ascii_lowercase().contains("base");
        let white_has_base = white.to_ascii_lowercase().contains("base");
        match (black_has_base, white_has_base) {
            (true, false) => InferredLabels {
                test: white.clone(),
                base: black.clone(),
                note: "ラベル名に \"base\" を含む側を base と判断".to_string(),
                assumed: false,
            },
            (false, true) => InferredLabels {
                test: black.clone(),
                base: white.clone(),
                note: "ラベル名に \"base\" を含む側を base と判断".to_string(),
                assumed: false,
            },
            _ => InferredLabels {
                test: black.clone(),
                base: white.clone(),
                note: "役割を示す情報が無いため label_black を test とみなした".to_string(),
                assumed: true,
            },
        }
    };
    Ok(Some(inferred))
}

fn build_sprt_json(
    penta: Penta,
    base_label: &str,
    test_label: &str,
    params: SprtParameters,
) -> SprtJsonOutput {
    let llr = params.llr(penta);
    let (lo, hi) = params.llr_bounds();
    let decision = judge(&params, penta);
    SprtJsonOutput {
        base: base_label.to_string(),
        test: test_label.to_string(),
        nelo0: params.nelo_bounds().0,
        nelo1: params.nelo_bounds().1,
        alpha: params.alpha,
        beta: params.beta,
        pairs: penta.pair_count(),
        llr,
        lower: lo,
        upper: hi,
        decision: decision.as_str().to_string(),
        nelo: penta.normalized_elo().map(|(e, ci)| SprtNelo { value: e, ci95: ci }),
        logistic_elo: penta.logistic_elo().map(|(e, ci)| SprtNelo { value: e, ci95: ci }),
        penta: SprtPentaJson {
            ll: penta.ll,
            dl: penta.dl,
            dd: penta.dd,
            wl: penta.wl,
            wd: penta.wd,
            ww: penta.ww,
        },
    }
}

fn print_sprt_text_report(penta: Penta, output: &SprtJsonOutput) {
    println!();
    println!("=== SPRT (post-hoc): {} (test) vs {} (base) ===", output.test, output.base);
    println!(
        "hypotheses: H0 = nelo0={:+.1}  H1 = nelo1={:+.1}  (alpha={}, beta={})",
        output.nelo0, output.nelo1, output.alpha, output.beta
    );
    println!("bounds:     LLR ∈ [{:+.3}, {:+.3}]", output.lower, output.upper);
    println!("pairs:      {}", output.pairs);
    println!("LLR:        {:+.3}", output.llr);
    // accept_h0/h1 はラベル役割の取り違えに弱いため、どちらが強い判定なのかを
    // ラベル実名で言語化して併記する。
    let decision_note = match output.decision.as_str() {
        "accept_h1" => format!(
            "H1 採択: {} は {} より強い (nelo {:+.1} 以上)",
            output.test, output.base, output.nelo1
        ),
        "accept_h0" => format!(
            "H0 採択: {} が {} より nelo {:+.1} 以上強いとは言えない",
            output.test, output.base, output.nelo1
        ),
        "running" => "境界未到達 (判定保留)".to_string(),
        other => format!("不明な decision: {other}"),
    };
    println!("decision:   {} — {}", output.decision, decision_note);
    match &output.nelo {
        Some(n) => println!("nelo:       {:+.2} ± {:.2} ({} 視点)", n.value, n.ci95, output.test),
        None => println!("nelo:       n/a (variance 0)"),
    }
    match &output.logistic_elo {
        Some(n) => println!("elo:        {:+.2} ± {:.2} ({} 視点)", n.value, n.ci95, output.test),
        None => println!("elo:        n/a"),
    }
    println!("penta:      {} ({} 視点)", penta, output.test);
    println!("=================================");
}

#[derive(Serialize, Clone)]
struct SprtJsonOutput {
    base: String,
    test: String,
    nelo0: f64,
    nelo1: f64,
    alpha: f64,
    beta: f64,
    pairs: u64,
    llr: f64,
    lower: f64,
    upper: f64,
    decision: String,
    nelo: Option<SprtNelo>,
    logistic_elo: Option<SprtNelo>,
    penta: SprtPentaJson,
}

#[derive(Serialize, Clone)]
struct SprtNelo {
    value: f64,
    ci95: f64,
}

#[derive(Serialize, Clone)]
struct SprtPentaJson {
    ll: u64,
    dl: u64,
    dd: u64,
    wl: u64,
    wd: u64,
    ww: u64,
}

// ---------------------------------------------------------------------------
// Elo計算
// ---------------------------------------------------------------------------

/// スコア（勝率）からEloレーティング差を計算する。
/// `score = (wins + draws * 0.5) / total`
/// `Elo = -400 * log10(1/score - 1)`
fn elo_diff(wins: u32, losses: u32, draws: u32) -> Option<f64> {
    let total = wins + losses + draws;
    if total == 0 {
        return None;
    }
    let score = (wins as f64 + draws as f64 * 0.5) / total as f64;
    if score <= 0.0 || score >= 1.0 {
        return None;
    }
    Some(-400.0 * (1.0 / score - 1.0).log10())
}

/// Elo差の95%信頼区間を計算する（正規近似）。
/// 標準誤差: SE = sqrt(score * (1 - score) / n)
/// Elo の SE ≈ dElo/dscore * SE_score
///   dElo/dscore = 400 / (ln(10) * score * (1 - score))
fn elo_ci95(wins: u32, losses: u32, draws: u32) -> Option<f64> {
    let total = wins + losses + draws;
    if total == 0 {
        return None;
    }
    let n = total as f64;
    let score = (wins as f64 + draws as f64 * 0.5) / n;
    if score <= 0.0 || score >= 1.0 {
        return None;
    }
    let se_score = (score * (1.0 - score) / n).sqrt();
    let delo_dscore = 400.0 / (std::f64::consts::LN_10 * score * (1.0 - score));
    let se_elo = (delo_dscore * se_score).abs();
    Some(1.96 * se_elo)
}

/// Pentanomial 集計を相手視点に反転する。
fn reverse_penta(penta: Penta) -> Penta {
    Penta {
        ll: penta.ww,
        dl: penta.wd,
        dd: penta.dd,
        wl: penta.wl,
        wd: penta.dl,
        ww: penta.ll,
    }
}

/// SPRT の test/base と一致する直接対決を test 視点で表示するか判定する。
fn should_show_right_first(
    left: &str,
    right: &str,
    sprt_test: Option<&str>,
    sprt_base: Option<&str>,
) -> bool {
    sprt_test == Some(right) && sprt_base == Some(left)
}

// ---------------------------------------------------------------------------
// メイン処理
// ---------------------------------------------------------------------------

fn main() -> Result<()> {
    let cli = Cli::parse();

    // 通常の .jsonl が1つでもあれば .summary.jsonl を自動除外（二重カウント防止）
    let has_normal = cli.files.iter().any(|f| !f.contains(".summary."));
    let files: Vec<&str> = cli
        .files
        .iter()
        .filter(|f| {
            if has_normal && f.contains(".summary.") {
                eprintln!("スキップ（summaryは通常ファイルと重複）: {f}");
                false
            } else {
                true
            }
        })
        .map(|s| s.as_str())
        .collect();

    // 全ファイルをパースして集計
    let mut matchups: BTreeMap<(String, String), MatchupStats> = BTreeMap::new();
    let mut engine_ids: BTreeSet<String> = BTreeSet::new();
    let mut valid_files = 0u32;
    let mut extra = AggregatedExtraStats::default();

    for path in &files {
        match parse_file(path) {
            Ok(result) => {
                if result.black.is_empty() || result.white.is_empty() || result.games == 0 {
                    eprintln!("警告: 有効なデータなし: {path}");
                    continue;
                }
                let key = (result.black.clone(), result.white.clone());
                let stats = matchups.entry(key).or_default();
                stats.total += result.games;
                stats.done += result.done;
                stats.black_wins += result.black_wins;
                stats.white_wins += result.white_wins;
                stats.draws += result.draws;
                stats.files += 1;
                stats.a_sente_games += result.a_sente_games;
                stats.a_sente_wins += result.a_sente_wins;
                stats.b_sente_games += result.b_sente_games;
                stats.b_sente_wins += result.b_sente_wins;
                engine_ids.insert(result.black);
                engine_ids.insert(result.white);
                for (reason, count) in result.extra.reasons {
                    *extra.reasons.entry(reason).or_default() += count;
                }
                extra.total_plies += result.extra.total_plies;
                extra.completed_games += result.extra.completed_games;
                extra.black_wins += result.extra.black_wins;
                extra.white_wins += result.extra.white_wins;
                extra.draws += result.extra.draws;
                extra.retry.error_games += result.retry.error_games;
                extra.retry.error_pairs += result.retry.error_pairs;
                extra.retry.retried_pairs += result.retry.retried_pairs;
                extra.retry.exhausted_pairs += result.retry.exhausted_pairs;
                for (engine, move_stats) in result.extra.engine_moves {
                    merge_engine_move_stats(
                        extra.engine_moves.entry(engine).or_default(),
                        &move_stats,
                    );
                }
                valid_files += 1;
            }
            Err(e) => {
                eprintln!("警告: {path}: {e}");
            }
        }
    }

    if matchups.is_empty() {
        bail!("有効な対局データがありません");
    }

    // エンジン名ラベル（A, B, C, ...）を短いハッシュ順に自動割当
    let labels: BTreeMap<String, String> = engine_ids
        .iter()
        .enumerate()
        .map(|(i, id)| {
            let label = format!("{}({})", (b'A' + i as u8) as char, id);
            (id.clone(), label)
        })
        .collect();

    let total_done: u32 = matchups.values().map(|v| v.done).sum();
    let total_all: u32 = matchups.values().map(|v| v.total).sum();

    // エンジン別集計
    let mut engines: BTreeMap<String, EngineStats> = BTreeMap::new();
    for ((b, w), v) in &matchups {
        // b = meta.black engine (= "a"), w = meta.white engine (= "b")
        let be = engines.entry(b.clone()).or_default();
        be.wins += v.black_wins;
        be.losses += v.white_wins;
        be.draws += v.draws;
        be.games += v.done;
        be.sente_games += v.a_sente_games;
        be.sente_wins += v.a_sente_wins;
        // 相手(w)が先手の局数 = 自分(b)が後手の局数
        be.gote_games += v.b_sente_games;
        be.gote_wins += v.black_wins - v.a_sente_wins;

        let we = engines.entry(w.clone()).or_default();
        we.wins += v.white_wins;
        we.losses += v.black_wins;
        we.draws += v.draws;
        we.games += v.done;
        we.sente_games += v.b_sente_games;
        we.sente_wins += v.b_sente_wins;
        // 相手(b)が先手の局数 = 自分(w)が後手の局数
        we.gote_games += v.a_sente_games;
        we.gote_wins += v.white_wins - v.b_sente_wins;
    }

    // 直接対決集計（先後合算、正規化キー: 辞書順で小さい方がleft）
    let mut head_to_head: BTreeMap<(String, String), HeadToHeadStats> = BTreeMap::new();
    for ((b, w), v) in &matchups {
        let (left, right) = if b <= w {
            (b.clone(), w.clone())
        } else {
            (w.clone(), b.clone())
        };
        let h = head_to_head.entry((left, right)).or_default();
        h.done += v.done;
        h.draws += v.draws;
        if b <= w {
            // b=left, w=right
            h.left_wins += v.black_wins;
            h.right_wins += v.white_wins;
            // a(=b=left)の先手データ
            h.left_sente_games += v.a_sente_games;
            h.left_sente_wins += v.a_sente_wins;
            // a(=b=left)の後手データ: 相手(w)が先手の局
            h.left_gote_games += v.b_sente_games;
            h.left_gote_wins += v.black_wins - v.a_sente_wins;
        } else {
            // b=right, w=left
            h.right_wins += v.black_wins;
            h.left_wins += v.white_wins;
            // w(=left)の先手データ: b_sente は meta.white が先手の局
            h.left_sente_games += v.b_sente_games;
            h.left_sente_wins += v.b_sente_wins;
            // w(=left)の後手データ: a(=b=right)が先手の局
            h.left_gote_games += v.a_sente_games;
            h.left_gote_wins += v.white_wins - v.b_sente_wins;
        }
    }

    // SPRT post-hoc 集計（JSON モードでは最終 JSON にフィールドとして埋め込むため事前に計算する）
    let sprt_payload: Option<(Penta, SprtJsonOutput)> = if cli.sprt {
        // CLI が全項目（ラベル+パラメータ）を明示している場合は meta 参照を完全スキップ。
        // 部分明示の場合は未解決項目の補完のため meta を収集するが、CLI でラベルが明示されて
        // いる場合はそれを `collect_sprt_meta` に渡して別 run の meta を無視させる。
        let needs_meta = cli.sprt_base_label.is_none()
            || cli.sprt_test_label.is_none()
            || cli.sprt_nelo0.is_none()
            || cli.sprt_nelo1.is_none()
            || cli.sprt_alpha.is_none()
            || cli.sprt_beta.is_none();
        let meta_sprt = if needs_meta {
            collect_sprt_meta(
                &files,
                cli.sprt_base_label.as_deref(),
                cli.sprt_test_label.as_deref(),
            )?
        } else {
            None
        };

        // SPRT meta が無いログ（通常 run）では meta のラベル情報から base/test を推定する。
        // CLI で両ラベルが明示済みなら推定不要。
        let inferred = if meta_sprt.is_none()
            && (cli.sprt_base_label.is_none() || cli.sprt_test_label.is_none())
        {
            infer_labels_from_meta(
                &files,
                cli.sprt_base_label.as_deref(),
                cli.sprt_test_label.as_deref(),
            )?
        } else {
            None
        };
        if let Some(inf) = &inferred {
            let prefix = if inf.assumed { "警告" } else { "情報" };
            eprintln!(
                "{prefix}: SPRT ラベルを meta から推定: test={} / base={}（{}）。\
                 役割が逆の場合は --sprt-base-label / --sprt-test-label を明示してください",
                inf.test, inf.base, inf.note
            );
        }

        let base_label = cli
            .sprt_base_label
            .clone()
            .or_else(|| meta_sprt.as_ref().map(|m| m.base_label.clone()))
            .or_else(|| inferred.as_ref().map(|i| i.base.clone()))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "--sprt 有効時は base ラベルが必要です（meta からの推定もできませんでした）。--sprt-base-label を明示してください"
                )
            })?;
        let test_label = cli
            .sprt_test_label
            .clone()
            .or_else(|| meta_sprt.as_ref().map(|m| m.test_label.clone()))
            .or_else(|| inferred.as_ref().map(|i| i.test.clone()))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "--sprt 有効時は test ラベルが必要です（meta からの推定もできませんでした）。--sprt-test-label を明示してください"
                )
            })?;
        if base_label == test_label {
            bail!("--sprt-base-label と --sprt-test-label は異なる必要があります");
        }

        // nelo / alpha / beta は CLI → meta → ハードコード fallback の順で解決する。
        let nelo0 = cli.sprt_nelo0.or_else(|| meta_sprt.as_ref().map(|m| m.nelo0)).unwrap_or(0.0);
        let nelo1 = cli.sprt_nelo1.or_else(|| meta_sprt.as_ref().map(|m| m.nelo1)).unwrap_or(5.0);
        let alpha = cli.sprt_alpha.or_else(|| meta_sprt.as_ref().map(|m| m.alpha)).unwrap_or(0.05);
        let beta = cli.sprt_beta.or_else(|| meta_sprt.as_ref().map(|m| m.beta)).unwrap_or(0.05);

        let mut total = Penta::ZERO;
        for path in &files {
            if path.contains(".summary.") {
                continue;
            }
            match collect_sprt_penta(path, &base_label, &test_label) {
                Ok(p) => total += p,
                Err(e) => eprintln!("警告: SPRT 集計失敗 {path}: {e}"),
            }
        }
        let params =
            SprtParameters::new(nelo0, nelo1, alpha, beta).map_err(|e| anyhow::anyhow!(e))?;
        let json = build_sprt_json(total, &base_label, &test_label, params);
        Some((total, json))
    } else {
        None
    };

    // 直接対決ペアごとの pentanomial 集計（nElo 表示用、テキスト出力時のみ）。
    // SPRT 対象ペアは集計済みの値を視点変換して再利用し、未完了ペアの通知と
    // JSONL の走査が重複するのを避ける。
    let h2h_penta: BTreeMap<(String, String), Penta> = if !cli.json {
        let mut map = BTreeMap::new();
        for (left, right) in head_to_head.keys() {
            let reused = sprt_payload.as_ref().and_then(|(penta, output)| {
                if output.base == *left && output.test == *right {
                    Some(*penta)
                } else if output.base == *right && output.test == *left {
                    Some(reverse_penta(*penta))
                } else {
                    None
                }
            });
            let penta = if let Some(penta) = reused {
                penta
            } else {
                let mut penta = Penta::ZERO;
                for path in &files {
                    if path.contains(".summary.") {
                        continue;
                    }
                    // left=base, right=test で集計 → normalized_elo() は right 視点
                    match collect_sprt_penta(path, left, right) {
                        Ok(p) => penta += p,
                        Err(e) => eprintln!("警告: h2h penta 集計失敗 {path}: {e}"),
                    }
                }
                penta
            };
            map.insert((left.clone(), right.clone()), penta);
        }
        map
    } else {
        BTreeMap::new()
    };

    if cli.json {
        print_json(
            valid_files,
            total_done,
            total_all,
            &matchups,
            &engines,
            &head_to_head,
            &labels,
            &extra,
            sprt_payload.as_ref().map(|(_, j)| j.clone()),
        )?;
    } else {
        print_text(
            valid_files,
            total_done,
            total_all,
            &engines,
            &head_to_head,
            &h2h_penta,
            &labels,
            &extra,
            sprt_payload.as_ref().map(|(_, output)| output),
        );
        if let Some((penta, json)) = sprt_payload.as_ref() {
            print_sprt_text_report(*penta, json);
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// テキスト出力
// ---------------------------------------------------------------------------

fn print_text(
    file_count: u32,
    total_done: u32,
    total_all: u32,
    engines: &BTreeMap<String, EngineStats>,
    head_to_head: &BTreeMap<(String, String), HeadToHeadStats>,
    h2h_penta: &BTreeMap<(String, String), Penta>,
    labels: &BTreeMap<String, String>,
    extra: &AggregatedExtraStats,
    sprt: Option<&SprtJsonOutput>,
) {
    let pct = if total_all > 0 {
        total_done as f64 / total_all as f64 * 100.0
    } else {
        0.0
    };
    println!(
        "ファイル数: {}  進捗: {}/{}局完了 ({:.1}%)",
        file_count, total_done, total_all, pct
    );
    println!();

    // エンジン別（勝率降順でソート）
    println!();
    println!("エンジン別 勝敗（先後合算）");
    println!("{}", "=".repeat(75));
    let mut engine_list: Vec<_> = engines.iter().collect();
    engine_list.sort_by(|(_, a), (_, b)| {
        let rate_a = win_rate(a.wins, a.losses, a.draws);
        let rate_b = win_rate(b.wins, b.losses, b.draws);
        rate_b.partial_cmp(&rate_a).unwrap_or(std::cmp::Ordering::Equal)
    });
    for (id, s) in &engine_list {
        let name = labels.get(*id).map_or(id.as_str(), |s| s.as_str());
        let wr = win_rate(s.wins, s.losses, s.draws);
        let sente_wr = if s.sente_games > 0 {
            s.sente_wins as f64 / s.sente_games as f64 * 100.0
        } else {
            0.0
        };
        let gote_wr = if s.gote_games > 0 {
            s.gote_wins as f64 / s.gote_games as f64 * 100.0
        } else {
            0.0
        };
        let sente_str = if s.sente_games > 0 {
            format!("先手:{:.1}%({}/{})", sente_wr, s.sente_wins, s.sente_games)
        } else {
            "先手:-".to_string()
        };
        let gote_str = if s.gote_games > 0 {
            format!("後手:{:.1}%({}/{})", gote_wr, s.gote_wins, s.gote_games)
        } else {
            "後手:-".to_string()
        };
        println!(
            "  {:16} | {:3}局完了 | 勝:{:3} 負:{:3} 引分:{:2} | 勝率:{:.1}% ({} {})",
            name, s.games, s.wins, s.losses, s.draws, wr, sente_str, gote_str
        );
    }

    // 直接対決
    println!();
    println!("直接対決");
    println!("{}", "=".repeat(75));
    for ((a, b), v) in head_to_head {
        // SPRT 対象ペアは test を左に表示し、直後の SPRT レポートと視点を揃える。
        // それ以外は従来どおり辞書順の left 視点を保つ。
        let show_right_first = should_show_right_first(
            a,
            b,
            sprt.map(|output| output.test.as_str()),
            sprt.map(|output| output.base.as_str()),
        );
        let total = v.left_wins + v.right_wins + v.draws;
        let half = v.done / 2;
        let half_up = half + v.done % 2;
        let (
            primary,
            secondary,
            primary_wins,
            secondary_wins,
            primary_sente_games,
            primary_sente_wins,
            primary_gote_games,
            primary_gote_wins,
            primary_sente_total,
            primary_gote_total,
            nelo_sign,
        ) = if show_right_first {
            (
                b,
                a,
                v.right_wins,
                v.left_wins,
                v.left_gote_games,
                v.left_gote_games - v.left_gote_wins,
                v.left_sente_games,
                v.left_sente_games - v.left_sente_wins,
                half,
                half_up,
                1.0,
            )
        } else {
            (
                a,
                b,
                v.left_wins,
                v.right_wins,
                v.left_sente_games,
                v.left_sente_wins,
                v.left_gote_games,
                v.left_gote_wins,
                half_up,
                half,
                -1.0,
            )
        };
        let primary_name = labels.get(primary).map_or(primary.as_str(), |s| s.as_str());
        let secondary_name = labels.get(secondary).map_or(secondary.as_str(), |s| s.as_str());
        let primary_wr = if total > 0 {
            primary_wins as f64 / total as f64 * 100.0
        } else {
            0.0
        };

        let elo = elo_diff(primary_wins, secondary_wins, v.draws);
        let ci = elo_ci95(primary_wins, secondary_wins, v.draws);

        // pentanomial nElo は正規化キーの right 視点で集計されている。
        let nelo_str = h2h_penta
            .get(&(a.clone(), b.clone()))
            .and_then(|p| p.normalized_elo())
            .map(|(e, c)| format!(" | nElo:{:+.0} ±{:.0}", nelo_sign * e, c))
            .unwrap_or_default();

        // 符号の視点を明示する。
        let elo_str = match (elo, ci) {
            (Some(e), Some(c)) => {
                format!(" | Elo差:{:+.0} ±{:.0}{} ({primary_name}視点)", e, c, nelo_str)
            }
            _ if !nelo_str.is_empty() => format!("{nelo_str} ({primary_name}視点)"),
            _ => nelo_str,
        };

        println!(
            "  {:16} vs {:16} | {:3}局 | {}:{:3}勝 {}:{:3}勝 引分:{} | {}勝率:{:.1}%{}",
            primary_name,
            secondary_name,
            v.done,
            primary_name,
            primary_wins,
            secondary_name,
            secondary_wins,
            v.draws,
            primary_name,
            primary_wr,
            elo_str
        );

        // 先手/後手別勝率
        if primary_sente_games > 0 || primary_gote_games > 0 {
            let fmt_wr = |label: &str, wins: u32, decisive: u32, total_games: u32| -> String {
                if decisive > 0 {
                    format!(
                        "{}:{:.1}%({}/{}局)",
                        label,
                        wins as f64 / decisive as f64 * 100.0,
                        wins,
                        total_games
                    )
                } else {
                    format!("{}:-", label)
                }
            };

            let primary_sente =
                fmt_wr("先手", primary_sente_wins, primary_sente_games, primary_sente_total);
            let primary_gote =
                fmt_wr("後手", primary_gote_wins, primary_gote_games, primary_gote_total);
            // secondary の先手 = primary の後手局、secondary の後手 = primary の先手局
            let secondary_sente_wins = primary_gote_games - primary_gote_wins;
            let secondary_gote_wins = primary_sente_games - primary_sente_wins;
            let secondary_sente =
                fmt_wr("先手", secondary_sente_wins, primary_gote_games, primary_gote_total);
            let secondary_gote =
                fmt_wr("後手", secondary_gote_wins, primary_sente_games, primary_sente_total);
            println!("    {primary_name} {primary_sente} {primary_gote}");
            println!("    {secondary_name} {secondary_sente} {secondary_gote}");
        }
    }

    print_reasons(&extra.reasons);

    if extra.completed_games > 0 || extra.retry.error_games > 0 {
        println!();
        println!("追加統計");
        println!("{}", "=".repeat(75));
        let decisive = extra.black_wins + extra.white_wins;
        let black_wr = if decisive > 0 {
            extra.black_wins as f64 / decisive as f64 * 100.0
        } else {
            0.0
        };
        let white_wr = if decisive > 0 {
            extra.white_wins as f64 / decisive as f64 * 100.0
        } else {
            0.0
        };
        println!(
            "  平均手数: {:.1} plies ({}局)",
            if extra.completed_games > 0 {
                extra.total_plies as f64 / extra.completed_games as f64
            } else {
                0.0
            },
            extra.completed_games
        );
        println!(
            "  先手勝率: {:.1}% ({}/{} 決着局), 後手勝率: {:.1}% ({}/{} 決着局), 引分: {}",
            black_wr, extra.black_wins, decisive, white_wr, extra.white_wins, decisive, extra.draws
        );
        println!(
            "  error局: {}, errorペア: {}, 再試行ペア: {}, 枯渇ペア: {}{}",
            extra.retry.error_games,
            extra.retry.error_pairs,
            extra.retry.retried_pairs,
            extra.retry.exhausted_pairs,
            if extra.retry.exhausted_pairs > 0 {
                " (invalid)"
            } else {
                ""
            },
        );
        let mut move_stats: Vec<_> = extra.engine_moves.iter().collect();
        move_stats.sort_by(|(id_a, _), (id_b, _)| {
            let name_a = labels.get(*id_a).map_or(id_a.as_str(), |s| s.as_str());
            let name_b = labels.get(*id_b).map_or(id_b.as_str(), |s| s.as_str());
            name_a.cmp(name_b)
        });
        for (id, stats) in move_stats {
            let name = labels.get(id).map_or(id.as_str(), |s| s.as_str());
            let avg_elapsed = average(stats.elapsed_ms_sum, stats.moves);
            let avg_limit = average(stats.think_limit_ms_sum, stats.moves);
            let avg_nps = if stats.eval_nps_count > 0 {
                Some(stats.eval_nps_sum as f64 / stats.eval_nps_count as f64)
            } else {
                None
            };
            let avg_depth = if stats.eval_depth_count > 0 {
                Some(stats.eval_depth_sum as f64 / stats.eval_depth_count as f64)
            } else {
                None
            };
            let avg_seldepth = if stats.eval_seldepth_count > 0 {
                Some(stats.eval_seldepth_sum as f64 / stats.eval_seldepth_count as f64)
            } else {
                None
            };
            let avg_nodes = if stats.eval_nodes_count > 0 {
                Some(stats.eval_nodes_sum as f64 / stats.eval_nodes_count as f64)
            } else {
                None
            };
            print!(
                "  {}: moves={} avg_elapsed={:.1}ms avg_limit={:.1}ms timed_out={}",
                name, stats.moves, avg_elapsed, avg_limit, stats.timed_out
            );
            if let Some(avg_nps) = avg_nps {
                print!(" avg_nps={:.0}", avg_nps);
            }
            if let Some(avg_depth) = avg_depth {
                print!(" avg_depth={:.2}", avg_depth);
            }
            if let Some(avg_seldepth) = avg_seldepth {
                print!(" avg_seldepth={:.2}", avg_seldepth);
            }
            if let Some(avg_nodes) = avg_nodes {
                print!(" avg_nodes={:.0}", avg_nodes);
            }
            println!();
            let mut sides: Vec<_> = stats.by_side.iter().collect();
            sides.sort_by_key(|(a, _)| *a);
            for (side, bucket) in sides {
                println!(
                    "    side {}: moves={} avg_elapsed={:.1}ms",
                    side,
                    bucket.moves,
                    average(bucket.elapsed_ms_sum, bucket.moves)
                );
            }
            for band in ["1-40", "41-80", "81-120", "121+"] {
                if let Some(bucket) = stats.by_ply_band.get(band) {
                    println!(
                        "    ply {}: moves={} avg_elapsed={:.1}ms",
                        band,
                        bucket.moves,
                        average(bucket.elapsed_ms_sum, bucket.moves)
                    );
                }
            }
        }
    }
}

const REASON_ORDER: [&str; 11] = [
    "resign",
    "win",
    "sennichite",
    "sennichite_perpetual_check",
    "adjudication_resign",
    "adjudication_draw",
    "max_moves",
    "timeout",
    "illegal_move",
    "no_bestmove",
    "error",
];

fn print_reasons(reasons: &BTreeMap<String, u32>) {
    let total: u64 = reasons.values().map(|&count| u64::from(count)).sum();
    if total == 0 {
        return;
    }
    println!("\n終局理由（全 result 行: {total}、error 局・重複行を含む）");
    let ordered = REASON_ORDER.into_iter().chain(
        reasons
            .keys()
            .map(String::as_str)
            .filter(|reason| !REASON_ORDER.contains(reason)),
    );
    for reason in ordered {
        if let Some(&count) = reasons.get(reason).filter(|&&count| count > 0) {
            println!("  {reason}: {count} ({:.2}%)", f64::from(count) / total as f64 * 100.0);
        }
    }
    let max_moves = reasons.get("max_moves").copied().unwrap_or(0);
    println!(
        "  max_moves 到達率: {:.2}% ({max_moves}/{total})",
        f64::from(max_moves) / total as f64 * 100.0
    );
}

fn win_rate(wins: u32, losses: u32, draws: u32) -> f64 {
    let total = wins + losses + draws;
    if total == 0 {
        return 0.0;
    }
    wins as f64 / total as f64 * 100.0
}

// ---------------------------------------------------------------------------
// JSON出力
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn print_json(
    file_count: u32,
    total_done: u32,
    total_all: u32,
    matchups: &BTreeMap<(String, String), MatchupStats>,
    engines: &BTreeMap<String, EngineStats>,
    head_to_head: &BTreeMap<(String, String), HeadToHeadStats>,
    labels: &BTreeMap<String, String>,
    extra: &AggregatedExtraStats,
    sprt: Option<SprtJsonOutput>,
) -> Result<()> {
    let pct = if total_all > 0 {
        total_done as f64 / total_all as f64 * 100.0
    } else {
        0.0
    };

    let json_matchups: Vec<JsonMatchup> = matchups
        .iter()
        .map(|((b, w), v)| JsonMatchup {
            black: labels.get(b).cloned().unwrap_or_else(|| b.clone()),
            white: labels.get(w).cloned().unwrap_or_else(|| w.clone()),
            done: v.done,
            total: v.total,
            black_wins: v.black_wins,
            white_wins: v.white_wins,
            draws: v.draws,
            files: v.files,
        })
        .collect();

    let mut engine_list: Vec<_> = engines.iter().collect();
    engine_list.sort_by(|(_, a), (_, b)| {
        let rate_a = win_rate(a.wins, a.losses, a.draws);
        let rate_b = win_rate(b.wins, b.losses, b.draws);
        rate_b.partial_cmp(&rate_a).unwrap_or(std::cmp::Ordering::Equal)
    });
    let json_engines: Vec<JsonEngine> = engine_list
        .iter()
        .map(|(id, s)| JsonEngine {
            id: labels.get(*id).cloned().unwrap_or_else(|| (*id).clone()),
            games: s.games,
            wins: s.wins,
            losses: s.losses,
            draws: s.draws,
            win_rate: win_rate(s.wins, s.losses, s.draws),
        })
        .collect();

    let json_h2h: Vec<JsonHeadToHead> = head_to_head
        .iter()
        .map(|((a, b), v)| JsonHeadToHead {
            engine_a: labels.get(a).cloned().unwrap_or_else(|| a.clone()),
            engine_b: labels.get(b).cloned().unwrap_or_else(|| b.clone()),
            done: v.done,
            a_wins: v.left_wins,
            b_wins: v.right_wins,
            draws: v.draws,
            a_win_rate: {
                let total = v.left_wins + v.right_wins + v.draws;
                if total > 0 {
                    v.left_wins as f64 / total as f64 * 100.0
                } else {
                    0.0
                }
            },
            elo_diff: elo_diff(v.left_wins, v.right_wins, v.draws),
            elo_ci95: elo_ci95(v.left_wins, v.right_wins, v.draws),
        })
        .collect();

    let mut engine_timing: Vec<_> = extra.engine_moves.iter().collect();
    engine_timing.sort_by(|(id_a, _), (id_b, _)| {
        let name_a = labels.get(*id_a).map_or(id_a.as_str(), |s| s.as_str());
        let name_b = labels.get(*id_b).map_or(id_b.as_str(), |s| s.as_str());
        name_a.cmp(name_b)
    });
    let json_engine_timing: Vec<JsonEngineTiming> = engine_timing
        .into_iter()
        .map(|(id, stats)| JsonEngineTiming {
            id: labels.get(id).cloned().unwrap_or_else(|| id.clone()),
            moves: stats.moves,
            average_elapsed_ms: average(stats.elapsed_ms_sum, stats.moves),
            average_think_limit_ms: average(stats.think_limit_ms_sum, stats.moves),
            timed_out: stats.timed_out,
            average_nps: if stats.eval_nps_count > 0 {
                Some(stats.eval_nps_sum as f64 / stats.eval_nps_count as f64)
            } else {
                None
            },
            average_depth: if stats.eval_depth_count > 0 {
                Some(stats.eval_depth_sum as f64 / stats.eval_depth_count as f64)
            } else {
                None
            },
            average_seldepth: if stats.eval_seldepth_count > 0 {
                Some(stats.eval_seldepth_sum as f64 / stats.eval_seldepth_count as f64)
            } else {
                None
            },
            average_nodes: if stats.eval_nodes_count > 0 {
                Some(stats.eval_nodes_sum as f64 / stats.eval_nodes_count as f64)
            } else {
                None
            },
            by_side: stats
                .by_side
                .iter()
                .map(|(label, bucket)| JsonTimingBucket {
                    label: label.clone(),
                    moves: bucket.moves,
                    average_elapsed_ms: average(bucket.elapsed_ms_sum, bucket.moves),
                })
                .collect(),
            by_ply_band: ["1-40", "41-80", "81-120", "121+"]
                .into_iter()
                .filter_map(|label| {
                    stats.by_ply_band.get(label).map(|bucket| JsonTimingBucket {
                        label: label.to_string(),
                        moves: bucket.moves,
                        average_elapsed_ms: average(bucket.elapsed_ms_sum, bucket.moves),
                    })
                })
                .collect(),
        })
        .collect();
    let decisive = extra.black_wins + extra.white_wins;

    let output = JsonOutput {
        files: file_count,
        progress: Progress {
            done: total_done,
            total: total_all,
            percent: pct,
        },
        matchups: json_matchups,
        engines: json_engines,
        head_to_head: json_h2h,
        extra: JsonExtra {
            reasons: extra.reasons.clone(),
            average_plies: if extra.completed_games > 0 {
                extra.total_plies as f64 / extra.completed_games as f64
            } else {
                0.0
            },
            black_win_rate_decisive: if decisive > 0 {
                extra.black_wins as f64 / decisive as f64 * 100.0
            } else {
                0.0
            },
            white_win_rate_decisive: if decisive > 0 {
                extra.white_wins as f64 / decisive as f64 * 100.0
            } else {
                0.0
            },
            completed_games: extra.completed_games,
            draws: extra.draws,
            error_games: extra.retry.error_games,
            error_pairs: extra.retry.error_pairs,
            retried_pairs: extra.retry.retried_pairs,
            exhausted_pairs: extra.retry.exhausted_pairs,
            invalid: extra.retry.exhausted_pairs > 0,
            engine_timing: json_engine_timing,
        },
        sprt,
    };

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn meta_accepts_settings_with_and_without_seed() {
        let meta_json = |settings| {
            format!(
                r#"{{"settings":{settings},"engine_cmd":{{"path_black":"/b","path_white":"/w"}}}}"#
            )
        };

        for settings in [r#"{"games":2}"#, r#"{"games":2,"seed":42}"#] {
            let meta: MetaLog = serde_json::from_str(&meta_json(settings)).unwrap();
            assert_eq!(meta.settings.games, 2);
        }
    }

    #[test]
    fn h2h_places_sprt_test_on_the_left() {
        assert!(should_show_right_first("base", "test", Some("test"), Some("base")));
        assert!(!should_show_right_first("test", "base", Some("test"), Some("base")));
        assert!(!should_show_right_first("a", "b", Some("test"), Some("base")));
        assert!(!should_show_right_first("base", "test", None, None));
    }

    #[test]
    fn reverse_penta_swaps_win_and_loss_categories() {
        let penta = Penta {
            ll: 1,
            dl: 2,
            dd: 3,
            wl: 4,
            wd: 5,
            ww: 6,
        };
        let reversed = reverse_penta(penta);
        assert_eq!(
            reversed,
            Penta {
                ll: 6,
                dl: 5,
                dd: 3,
                wl: 4,
                wd: 2,
                ww: 1
            }
        );
        assert_eq!(reverse_penta(reversed), penta);
    }

    fn write_meta_jsonl(dir: &std::path::Path, name: &str, sprt_json: Option<&str>) -> String {
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        let sprt_field = match sprt_json {
            Some(s) => format!(",\"sprt\":{s}"),
            None => String::new(),
        };
        writeln!(
            f,
            "{{\"type\":\"meta\",\"timestamp\":\"t\",\"settings\":{{\"games\":2}},\
             \"engine_cmd\":{{\"path_black\":\"/b\",\"path_white\":\"/w\",\
             \"label_black\":\"x\",\"label_white\":\"y\",\
             \"usi_options_black\":[],\"usi_options_white\":[]}}{sprt_field}}}"
        )
        .unwrap();
        path.display().to_string()
    }

    /// CLI でラベルが両方明示されていれば、CLI と合わない meta は無視される。
    /// 別 run 由来の異ラベル jsonl が混在しても bail! せず、CLI と合う meta を採用する。
    #[test]
    fn cli_labels_filter_unrelated_meta() {
        let dir = tempfile::tempdir().unwrap();
        let matching_sprt = "{\"base_label\":\"v100\",\"test_label\":\"v101\",\"nelo0\":0.0,\"nelo1\":4.0,\"alpha\":0.05,\"beta\":0.05}";
        let unrelated_sprt = "{\"base_label\":\"v200\",\"test_label\":\"v201\",\"nelo0\":0.0,\"nelo1\":5.0,\"alpha\":0.01,\"beta\":0.01}";
        let a = write_meta_jsonl(dir.path(), "a.jsonl", Some(matching_sprt));
        let b = write_meta_jsonl(dir.path(), "b.jsonl", Some(unrelated_sprt));
        let files: Vec<&str> = vec![a.as_str(), b.as_str()];

        let res = collect_sprt_meta(&files, Some("v100"), Some("v101")).unwrap();
        let got = res.expect("matching meta should be picked up");
        assert_eq!(got.base_label, "v100");
        assert_eq!(got.test_label, "v101");
        assert_eq!(got.nelo1, 4.0);
    }

    /// CLI で片方のみ指定された場合も、一致しない SPRT meta は無視される
    /// （infer_labels_from_meta のフィルタ規則と対称）。
    #[test]
    fn one_sided_cli_label_filters_unrelated_sprt_meta() {
        let dir = tempfile::tempdir().unwrap();
        let matching_sprt = "{\"base_label\":\"v100\",\"test_label\":\"v101\",\"nelo0\":0.0,\"nelo1\":4.0,\"alpha\":0.05,\"beta\":0.05}";
        let unrelated_sprt = "{\"base_label\":\"v200\",\"test_label\":\"v201\",\"nelo0\":0.0,\"nelo1\":5.0,\"alpha\":0.01,\"beta\":0.01}";
        let a = write_meta_jsonl(dir.path(), "a.jsonl", Some(matching_sprt));
        let b = write_meta_jsonl(dir.path(), "b.jsonl", Some(unrelated_sprt));
        let files: Vec<&str> = vec![a.as_str(), b.as_str()];

        let res = collect_sprt_meta(&files, Some("v100"), None).unwrap();
        let got = res.expect("matching meta should be picked up");
        assert_eq!(got.base_label, "v100");
        assert_eq!(got.test_label, "v101");

        let res = collect_sprt_meta(&files, None, Some("v201")).unwrap();
        let got = res.expect("matching meta should be picked up");
        assert_eq!(got.base_label, "v200");
        assert_eq!(got.test_label, "v201");
    }

    /// CLI ラベル未指定で異ラベルの meta が混在する場合は従来通り bail! する。
    #[test]
    fn without_cli_labels_conflicting_meta_bails() {
        let dir = tempfile::tempdir().unwrap();
        let a = write_meta_jsonl(
            dir.path(),
            "a.jsonl",
            Some(
                "{\"base_label\":\"v100\",\"test_label\":\"v101\",\"nelo0\":0.0,\"nelo1\":4.0,\"alpha\":0.05,\"beta\":0.05}",
            ),
        );
        let b = write_meta_jsonl(
            dir.path(),
            "b.jsonl",
            Some(
                "{\"base_label\":\"v200\",\"test_label\":\"v201\",\"nelo0\":0.0,\"nelo1\":5.0,\"alpha\":0.01,\"beta\":0.01}",
            ),
        );
        let files: Vec<&str> = vec![a.as_str(), b.as_str()];
        let err = collect_sprt_meta(&files, None, None).unwrap_err();
        assert!(err.to_string().contains("SPRT ラベル"));
    }

    /// ラベル一致でもパラメータが違う場合は bail!。
    #[test]
    fn same_labels_different_params_bails() {
        let dir = tempfile::tempdir().unwrap();
        let a = write_meta_jsonl(
            dir.path(),
            "a.jsonl",
            Some(
                "{\"base_label\":\"v100\",\"test_label\":\"v101\",\"nelo0\":0.0,\"nelo1\":4.0,\"alpha\":0.05,\"beta\":0.05}",
            ),
        );
        let b = write_meta_jsonl(
            dir.path(),
            "b.jsonl",
            Some(
                "{\"base_label\":\"v100\",\"test_label\":\"v101\",\"nelo0\":0.0,\"nelo1\":5.0,\"alpha\":0.01,\"beta\":0.01}",
            ),
        );
        let files: Vec<&str> = vec![a.as_str(), b.as_str()];
        let err = collect_sprt_meta(&files, None, None).unwrap_err();
        assert!(err.to_string().contains("Wald パラメータ"));
    }

    /// sprt meta を含まない旧形式 jsonl は None を返す（呼び出し側で CLI 必須を要求）。
    #[test]
    fn legacy_jsonl_without_sprt_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let a = write_meta_jsonl(dir.path(), "legacy.jsonl", None);
        let files: Vec<&str> = vec![a.as_str()];
        let res = collect_sprt_meta(&files, None, None).unwrap();
        assert!(res.is_none());
    }

    /// collect_sprt_penta は破損 result 行で bail する（サイレントにスキップしない）。
    /// JSONL が途中で壊れたケースで Penta/LLR が過小集計されるのを防止。
    #[test]
    fn collect_sprt_penta_bails_on_broken_result_line() {
        use std::io::Write as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken_result.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        // 先頭に有効な meta（base=a, test=b）、その後に壊れた result 行を入れる
        writeln!(
            f,
            "{{\"type\":\"meta\",\"timestamp\":\"t\",\"settings\":{{\"games\":2}},\
             \"engine_cmd\":{{\"path_black\":\"/a\",\"path_white\":\"/b\",\
             \"label_black\":\"a\",\"label_white\":\"b\",\
             \"usi_options_black\":[],\"usi_options_white\":[]}}}}"
        )
        .unwrap();
        // 壊れた result 行（outcome フィールドが数値で ResultLog パース失敗）
        writeln!(f, "{{\"type\":\"result\",\"outcome\":123}}").unwrap();
        drop(f);

        let err = collect_sprt_penta(&path.display().to_string(), "a", "b").unwrap_err();
        assert!(err.to_string().contains("resultパースエラー"));
    }

    fn write_retry_log(lines: &[&str]) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("retry.jsonl");
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(
            file,
            "{{\"type\":\"meta\",\"settings\":{{\"games\":2}},\
             \"engine_cmd\":{{\"path_black\":\"/base\",\"path_white\":\"/test\",\
             \"label_black\":\"base\",\"label_white\":\"test\"}}}}"
        )
        .unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
        (dir, path.display().to_string())
    }

    #[test]
    fn retry_generation_replaces_error_generation_for_direct_and_penta() {
        let (_dir, path) = write_retry_log(&[
            r#"{"type":"result","outcome":"white_win","winner":"test","plies":20,"pair_index":0,"pair_slot":0}"#,
            r#"{"type":"result","outcome":"draw","plies":0,"pair_index":0,"pair_slot":1,"error":true}"#,
            r#"{"type":"result","outcome":"white_win","winner":"test","plies":30,"pair_index":0,"pair_slot":0,"attempt":1}"#,
            r#"{"type":"result","outcome":"black_win","winner":"test","plies":40,"pair_index":0,"pair_slot":1,"attempt":1}"#,
        ]);

        let parsed = parse_normal_file(&path).unwrap();
        assert_eq!(parsed.done, 2);
        assert_eq!(parsed.black_wins, 0);
        assert_eq!(parsed.white_wins, 2);
        assert_eq!(parsed.draws, 0);
        assert_eq!(parsed.retry.error_games, 1);
        assert_eq!(parsed.retry.error_pairs, 1);
        assert_eq!(parsed.retry.retried_pairs, 1);
        assert_eq!(parsed.retry.exhausted_pairs, 0);

        let penta = collect_sprt_penta(&path, "base", "test").unwrap();
        assert_eq!(
            penta,
            Penta {
                ww: 1,
                ..Penta::ZERO
            },
            "live SPRT と同じ世代入力は retry 世代の WW 1 ペアだけになる"
        );
    }

    #[test]
    fn reasons_count_all_results_without_changing_wld_or_penta() {
        let (dir, path) = write_retry_log(&[
            r#"{"type":"result","outcome":"draw","reason":"sennichite"}"#,
            r#"{"type":"result","outcome":"white_win","winner":"test","reason":"sennichite_perpetual_check"}"#,
            r#"{"type":"result","outcome":"draw","reason":"max_moves"}"#,
            r#"{"type":"result","outcome":"draw","reason":"adjudication_draw"}"#,
            r#"{"type":"result","outcome":"black_win","winner":"base","reason":"adjudication_resign"}"#,
            r#"{"type":"result","outcome":"draw"}"#,
            r#"{"type":"result","outcome":"draw","reason":"error: worker panic","error":true}"#,
            r#"{"type":"result","outcome":"draw","reason":"error: engine exit","error":true}"#,
            r#"{"type":"result","outcome":"draw","reason":"max_moves","pair_index":0,"pair_slot":0}"#,
        ]);
        let parsed = parse_normal_file(&path).unwrap();
        assert_eq!((parsed.black_wins, parsed.white_wins, parsed.draws), (1, 1, 4));
        assert_eq!(parsed.done, 6);
        assert_eq!(
            parsed.extra.reasons,
            BTreeMap::from([
                ("sennichite".to_string(), 1),
                ("sennichite_perpetual_check".to_string(), 1),
                ("max_moves".to_string(), 2),
                ("adjudication_draw".to_string(), 1),
                ("adjudication_resign".to_string(), 1),
                ("unknown".to_string(), 1),
                ("error".to_string(), 2),
            ])
        );
        assert_eq!(
            collect_sprt_penta(&path, "base", "test").unwrap(),
            Penta {
                wd: 1,
                dd: 1,
                dl: 1,
                ..Penta::ZERO
            }
        );
        dir.close().unwrap();
    }

    #[test]
    fn direct_aggregation_keeps_first_result_for_duplicate_slot() {
        let (_dir, path) = write_retry_log(&[
            r#"{"type":"result","outcome":"black_win","winner":"base","plies":20,"pair_index":0,"pair_slot":0}"#,
            r#"{"type":"result","outcome":"white_win","winner":"test","plies":30,"pair_index":0,"pair_slot":0}"#,
            r#"{"type":"result","outcome":"white_win","winner":"base","plies":40,"pair_index":0,"pair_slot":1}"#,
        ]);

        let parsed = parse_normal_file(&path).unwrap();
        assert_eq!(parsed.done, 2);
        assert_eq!(parsed.black_wins, 2);
        assert_eq!(parsed.white_wins, 0);
    }

    #[test]
    fn attempt_defaults_to_zero_and_exhausted_pair_is_excluded() {
        let legacy: ResultLog =
            serde_json::from_str(r#"{"outcome":"draw","pair_index":7,"pair_slot":0}"#).unwrap();
        assert_eq!(legacy.attempt, 0);

        let (_dir, path) = write_retry_log(&[
            r#"{"type":"result","outcome":"draw","pair_index":0,"pair_slot":0,"attempt":2,"error":true}"#,
            r#"{"type":"result","outcome":"white_win","winner":"base","pair_index":0,"pair_slot":1,"attempt":2}"#,
        ]);
        let parsed = parse_normal_file(&path).unwrap();
        assert_eq!(parsed.done, 0);
        assert_eq!(parsed.retry.error_pairs, 1);
        assert_eq!(parsed.retry.retried_pairs, 1);
        assert_eq!(parsed.retry.exhausted_pairs, 1);
        assert_eq!(collect_sprt_penta(&path, "base", "test").unwrap(), Penta::ZERO);
    }

    fn write_meta_jsonl_labels(
        dir: &std::path::Path,
        name: &str,
        label_black: &str,
        label_white: &str,
        base_label: Option<&str>,
    ) -> String {
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        let base_field = match base_label {
            Some(s) => format!(",\"base_label\":\"{s}\""),
            None => String::new(),
        };
        writeln!(
            f,
            "{{\"type\":\"meta\",\"timestamp\":\"t\",\"settings\":{{\"games\":2}},\
             \"engine_cmd\":{{\"path_black\":\"/b\",\"path_white\":\"/w\",\
             \"label_black\":\"{label_black}\",\"label_white\":\"{label_white}\",\
             \"usi_options_black\":[],\"usi_options_white\":[]}}{base_field}}}"
        )
        .unwrap();
        path.display().to_string()
    }

    /// meta の base_label 記録（tournament --base-label）があればそれを base とする。
    #[test]
    fn infer_uses_meta_base_label() {
        let dir = tempfile::tempdir().unwrap();
        let a = write_meta_jsonl_labels(dir.path(), "a.jsonl", "cand", "anchor", Some("anchor"));
        let files: Vec<&str> = vec![a.as_str()];
        let inf = infer_labels_from_meta(&files, None, None).unwrap().unwrap();
        assert_eq!(inf.base, "anchor");
        assert_eq!(inf.test, "cand");
        assert!(!inf.assumed);
    }

    /// CLI で片方のみ指定された場合、もう片方をペアの残りラベルで補完する。
    /// CLI 指定は meta の base_label 記録より優先される。
    #[test]
    fn infer_completes_one_sided_cli_label() {
        let dir = tempfile::tempdir().unwrap();
        let a = write_meta_jsonl_labels(dir.path(), "a.jsonl", "x", "y", Some("x"));
        let files: Vec<&str> = vec![a.as_str()];

        let inf = infer_labels_from_meta(&files, Some("y"), None).unwrap().unwrap();
        assert_eq!(inf.base, "y");
        assert_eq!(inf.test, "x");

        let inf = infer_labels_from_meta(&files, None, Some("y")).unwrap().unwrap();
        assert_eq!(inf.base, "x");
        assert_eq!(inf.test, "y");
        assert!(!inf.assumed);
    }

    /// 片方のラベルだけが "base" を含むなら、それを base と推定する。
    #[test]
    fn infer_base_name_heuristic() {
        let dir = tempfile::tempdir().unwrap();
        let a = write_meta_jsonl_labels(dir.path(), "a.jsonl", "ftfact-100", "base-100", None);
        let files: Vec<&str> = vec![a.as_str()];
        let inf = infer_labels_from_meta(&files, None, None).unwrap().unwrap();
        assert_eq!(inf.base, "base-100");
        assert_eq!(inf.test, "ftfact-100");
        assert!(!inf.assumed);
    }

    /// base_label 記録がラベル組に含まれない場合は無視して後続の推定にフォールバックする。
    #[test]
    fn infer_ignores_base_label_not_in_pair() {
        let dir = tempfile::tempdir().unwrap();
        let a =
            write_meta_jsonl_labels(dir.path(), "a.jsonl", "ftfact-100", "base-100", Some("v999"));
        let files: Vec<&str> = vec![a.as_str()];
        let inf = infer_labels_from_meta(&files, None, None).unwrap().unwrap();
        assert_eq!(inf.base, "base-100");
        assert_eq!(inf.test, "ftfact-100");
        assert!(!inf.assumed);
    }

    /// 役割を示す情報が皆無なら label_black=test の既定に落ち、assumed=true になる。
    #[test]
    fn infer_falls_back_to_label_black_as_test() {
        let dir = tempfile::tempdir().unwrap();
        let a = write_meta_jsonl_labels(dir.path(), "a.jsonl", "v100", "v101", None);
        let files: Vec<&str> = vec![a.as_str()];
        let inf = infer_labels_from_meta(&files, None, None).unwrap().unwrap();
        assert_eq!(inf.test, "v100");
        assert_eq!(inf.base, "v101");
        assert!(inf.assumed);
    }

    /// 両ラベルが "base" を含む場合はヒューリスティックを使わず既定に落ちる。
    #[test]
    fn infer_ambiguous_base_names_fall_back() {
        let dir = tempfile::tempdir().unwrap();
        let a = write_meta_jsonl_labels(dir.path(), "a.jsonl", "base-a", "base-b", None);
        let files: Vec<&str> = vec![a.as_str()];
        let inf = infer_labels_from_meta(&files, None, None).unwrap().unwrap();
        assert!(inf.assumed);
    }

    /// 同一ラベル組でもファイル間で base_label 記録が矛盾する場合は bail する
    /// （入力順で base/test の符号が反転するのを防ぐ）。
    #[test]
    fn infer_conflicting_base_label_records_bail() {
        let dir = tempfile::tempdir().unwrap();
        let a = write_meta_jsonl_labels(dir.path(), "a.jsonl", "cand", "anchor", Some("anchor"));
        let b = write_meta_jsonl_labels(dir.path(), "b.jsonl", "cand", "anchor", Some("cand"));
        let files: Vec<&str> = vec![a.as_str(), b.as_str()];
        let err = infer_labels_from_meta(&files, None, None).unwrap_err();
        assert!(err.to_string().contains("base_label"));
    }

    /// base_label 記録を持つファイルと持たないファイルの混在は矛盾ではなく、
    /// 記録を持つ側から base を解決する。
    #[test]
    fn infer_merges_base_label_from_later_file() {
        let dir = tempfile::tempdir().unwrap();
        let a = write_meta_jsonl_labels(dir.path(), "a.jsonl", "cand", "anchor", None);
        let b = write_meta_jsonl_labels(dir.path(), "b.jsonl", "cand", "anchor", Some("anchor"));
        let files: Vec<&str> = vec![a.as_str(), b.as_str()];
        let inf = infer_labels_from_meta(&files, None, None).unwrap().unwrap();
        assert_eq!(inf.base, "anchor");
        assert_eq!(inf.test, "cand");
        assert!(!inf.assumed);
    }

    /// 入力ファイル間でラベル組が一致しない場合は bail する。
    #[test]
    fn infer_conflicting_label_pairs_bails() {
        let dir = tempfile::tempdir().unwrap();
        let a = write_meta_jsonl_labels(dir.path(), "a.jsonl", "x", "y", None);
        let b = write_meta_jsonl_labels(dir.path(), "b.jsonl", "p", "q", None);
        let files: Vec<&str> = vec![a.as_str(), b.as_str()];
        let err = infer_labels_from_meta(&files, None, None).unwrap_err();
        assert!(err.to_string().contains("ラベル組"));
    }

    /// CLI ラベル指定があれば、それを含まない別 run の meta は無視して bail しない。
    #[test]
    fn infer_cli_label_filters_unrelated_meta() {
        let dir = tempfile::tempdir().unwrap();
        let a = write_meta_jsonl_labels(dir.path(), "a.jsonl", "x", "y", None);
        let b = write_meta_jsonl_labels(dir.path(), "b.jsonl", "p", "q", None);
        let files: Vec<&str> = vec![a.as_str(), b.as_str()];
        let inf = infer_labels_from_meta(&files, Some("y"), None).unwrap().unwrap();
        assert_eq!(inf.base, "y");
        assert_eq!(inf.test, "x");
    }

    /// 同一ラベル同士（自己対局）は役割を割り当てられないため None を返す。
    #[test]
    fn infer_identical_labels_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let a = write_meta_jsonl_labels(dir.path(), "a.jsonl", "same", "same", None);
        let files: Vec<&str> = vec![a.as_str()];
        let res = infer_labels_from_meta(&files, None, None).unwrap();
        assert!(res.is_none());
    }

    /// 破損 JSON の先頭行は当該ファイルのみスキップし、他ファイルから収集できる。
    /// （警告の eprintln! 出力はテストでは捕捉していない）
    #[test]
    fn broken_json_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let broken = dir.path().join("broken.jsonl");
        std::fs::write(&broken, "{not json\n").unwrap();
        let good = write_meta_jsonl(
            dir.path(),
            "good.jsonl",
            Some(
                "{\"base_label\":\"v100\",\"test_label\":\"v101\",\"nelo0\":0.0,\"nelo1\":4.0,\"alpha\":0.05,\"beta\":0.05}",
            ),
        );
        let broken_str = broken.display().to_string();
        let files: Vec<&str> = vec![broken_str.as_str(), good.as_str()];
        let res = collect_sprt_meta(&files, None, None).unwrap();
        let got = res.expect("good file should still provide meta");
        assert_eq!(got.base_label, "v100");
    }
}
