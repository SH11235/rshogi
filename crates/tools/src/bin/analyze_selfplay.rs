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
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Context, Result, bail};
use clap::Parser;
use serde::{Deserialize, Serialize};

use tools::sprt::{GameSide, Penta, SprtMetaLog, SprtParameters, judge};

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
    /// ラベル / パラメータは tournament.rs が meta 行に書き出した SPRT 情報から自動推定する。
    /// ラベルは meta にも CLI にも無い場合エラー。数値パラメータは meta にも CLI にも無い
    /// 場合のみハードコード fallback (nelo0=0, nelo1=5, alpha=0.05, beta=0.05) を使う。
    #[arg(long, default_value_t = false)]
    sprt: bool,

    /// H1 側（challenger / test）のラベル。未指定時は meta から推定（meta にも無ければエラー）。
    #[arg(long)]
    sprt_test_label: Option<String>,

    /// H0 側（base）のラベル。未指定時は meta から推定（meta にも無ければエラー）。
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
#[derive(Deserialize)]
struct ResultLog {
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
    extra: FileExtraStats,
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
    total_plies: u64,
    completed_games: u32,
    black_wins: u32,
    white_wins: u32,
    draws: u32,
    engine_moves: BTreeMap<String, EngineMoveStats>,
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
    total_plies: u64,
    completed_games: u32,
    black_wins: u32,
    white_wins: u32,
    draws: u32,
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
    average_plies: f64,
    black_win_rate_decisive: f64,
    white_win_rate_decisive: f64,
    completed_games: u32,
    draws: u32,
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
    let mut black_wins: u32 = 0;
    let mut white_wins: u32 = 0;
    let mut draws: u32 = 0;
    let mut meta_parsed = false;
    // per-engine sente/gote stats (a = meta.black engine, b = meta.white engine)
    let mut a_sente_games: u32 = 0;
    let mut a_sente_wins: u32 = 0;
    let mut b_sente_games: u32 = 0;
    let mut b_sente_wins: u32 = 0;
    let mut extra = FileExtraStats::default();

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
            let engine_stats = extra.engine_moves.entry(engine_name).or_default();
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
            extra.completed_games += 1;
            extra.total_plies += result.plies as u64;
            if let Some(ref winner) = result.winner {
                // winner フィールドあり: エンジン名で集計（tournament.rs 形式）
                // meta にラベルがある場合は winner もラベルそのままなので正規化不要。
                // 旧形式（ラベルなし）では winner がパス由来なので extract_engine_id で正規化。
                let winner_id = if meta_parsed && (black == *winner || white == *winner) {
                    winner.clone()
                } else {
                    extract_engine_id(winner)
                };
                if winner_id == black {
                    black_wins += 1;
                } else if winner_id == white {
                    white_wins += 1;
                }
                // outcome + winner から各対局の先手/後手を判定
                // outcome="black_win" → 先手が勝ち → winner が先手だった
                // outcome="white_win" → 後手が勝ち → winner が後手だった
                match result.outcome.as_str() {
                    "black_win" => {
                        extra.black_wins += 1;
                        // winner が先手
                        if winner_id == black {
                            a_sente_games += 1;
                            a_sente_wins += 1;
                        } else if winner_id == white {
                            b_sente_games += 1;
                            b_sente_wins += 1;
                        }
                        // 敗者は後手
                        // (後手 games は done - sente_games で算出)
                    }
                    "white_win" => {
                        extra.white_wins += 1;
                        // winner が後手 → 敗者が先手
                        if winner_id == black {
                            // black engine が後手で勝ち → white engine が先手で負け
                            b_sente_games += 1;
                        } else if winner_id == white {
                            // white engine が後手で勝ち → black engine が先手で負け
                            a_sente_games += 1;
                        }
                    }
                    "draw" => {
                        extra.draws += 1;
                    }
                    _ => {}
                }
            } else {
                // winner なし: 旧形式または引分
                match result.outcome.as_str() {
                    "black_win" => {
                        black_wins += 1;
                        extra.black_wins += 1;
                    }
                    "white_win" => {
                        white_wins += 1;
                        extra.white_wins += 1;
                    }
                    "draw" => {
                        draws += 1;
                        extra.draws += 1;
                    }
                    _ => {}
                }
            }
        }
        // move行・metrics行等はスキップ
    }

    let done = black_wins + white_wins + draws;
    Ok(FileResult {
        black,
        white,
        games,
        black_wins,
        white_wins,
        draws,
        done,
        a_sente_games,
        a_sente_wins,
        b_sente_games,
        b_sente_wins,
        extra,
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

/// JSONL 1 行の `type` フィールドを parse して返す。JSON でない / `type` が無い
/// / 文字列でない場合は `None`。`contains("\"type\":\"...\"")` の部分一致よりも
/// 整形スタイル差に対して堅牢。
fn log_line_type(trimmed: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    value.get("type").and_then(|t| t.as_str()).map(str::to_owned)
}

/// 入力ファイル群の meta 行から SPRT メタを収集し、単一のラベル組/パラメータに合致するなら返す。
///
/// # 動作
/// - meta 行に SPRT 情報が書かれているのは `tournament.rs --sprt` 実行で生成された
///   base/test ペアの jsonl のみ
/// - `cli_base` / `cli_test` が共に与えられた場合、一致しない meta は無視する（別 run のログが
///   混在しても CLI 明示ラベルが優先して絞り込めるようにする）
/// - 残った meta が複数あり、`(base_label, test_label, nelo0, nelo1, alpha, beta)` が
///   揃って一致するなら採用。ラベル不一致は `bail!`、Wald パラメータ不一致も `bail!`
///   （LLR 境界が変わるため誤集計防止）
/// - どのファイルにも SPRT 情報が無ければ `None`
///   呼び出し側ではラベルは CLI 明示が必須、Wald パラメータはハードコード fallback あり
/// - 先頭非空行が JSON として壊れている場合は警告を出してそのファイルのみスキップ
///   （破損ファイルと旧形式 jsonl を区別するため）
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

            // CLI でラベルが両方明示されている場合は、ラベル不一致 meta を無視する。
            // これにより、異なる run の jsonl が混在していても CLI 明示で解析対象を絞れる。
            if let (Some(cb), Some(ct)) = (cli_base, cli_test)
                && (sprt.base_label != cb || sprt.test_label != ct)
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

/// 単一 JSONL ファイルから base/test ペアに該当する Penta を集計する。
///
/// - ファイルの meta が base/test 両方のラベルを含まなければ `Penta::ZERO`
/// - `pair_index` が無い旧ログは `seq / 2` / `seq % 2` でペアリング
/// - `error=true` の結果は除外
fn collect_sprt_penta(path: &str, base: &str, test: &str) -> Result<Penta> {
    let file =
        std::fs::File::open(path).with_context(|| format!("ファイルを開けません: {path}"))?;
    let reader = BufReader::new(file);

    let mut meta_labels: Option<(String, String)> = None;
    let mut pair_buffer: BTreeMap<u32, [Option<GameSide>; 2]> = BTreeMap::new();
    // ペア完成後にバッファから remove するので、`pair_buffer` だけでは
    // 「その pair_index は既に集計済み」かどうか判定できない。
    // 3 件目以降の重複到着を正しく検出するため、処理済み pair_index を別に保持する。
    let mut completed_pairs: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut total = Penta::ZERO;
    let mut seq: u32 = 0;
    let mut warned_missing_pair_index = false;

    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let kind = log_line_type(trimmed);
        if meta_labels.is_none() && kind.as_deref() == Some("meta") {
            let meta: MetaLog = serde_json::from_str(trimmed)
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
            let match_pair = (black == base && white == test) || (black == test && white == base);
            if !match_pair {
                return Ok(Penta::ZERO);
            }
            meta_labels = Some((black, white));
        } else if kind.as_deref() == Some("result") {
            let Some((label_black_meta, label_white_meta)) = meta_labels.as_ref() else {
                continue;
            };
            let result: ResultLog = serde_json::from_str(trimmed)
                .with_context(|| format!("resultパースエラー: {path}"))?;
            if result.error.unwrap_or(false) {
                continue;
            }
            if result.pair_index.is_none() && !warned_missing_pair_index {
                eprintln!(
                    "警告: {path} は pair_index を含まない旧形式ログです。\n\
                     SPRT ペアリングは result の出現順 (seq / 2, seq % 2) でフォールバックしますが、\n\
                     並列対局ログでは完了順がずれている可能性があるため結果は正確でない場合があります。"
                );
                warned_missing_pair_index = true;
            }
            let pair_idx = result.pair_index.unwrap_or(seq / 2);
            let slot_hint = result.pair_slot.unwrap_or(seq % 2);
            seq += 1;
            let slot = slot_hint.min(1) as usize;

            // 既に集計済みの pair_index に 3 件目以降が到着した場合は除外する。
            // test_side 決定より前に診断することで、ログ破損 (winner/outcome 不正) で
            // test_side の match が `continue` に落ちても警告は確実に出る。
            if completed_pairs.contains(&pair_idx) {
                eprintln!(
                    "警告: {path} — pair_index={pair_idx} は既に集計済みです。\
                     余剰データを除外します。"
                );
                continue;
            }

            // test 視点の Win/Draw/Loss を決定する。
            //
            // 優先: tournament.rs が書く `winner` フィールド (エンジンラベルそのもの)。
            // 旧ログ (winner なし) は pair_slot から実黒を推定:
            //   slot == 0 → meta.label_black が実際の黒
            //   slot == 1 → meta.label_white が実際の黒（先後入替）
            let test_side = if let Some(winner) = result.winner.as_deref() {
                match result.outcome.as_str() {
                    "black_win" | "white_win" => {
                        if winner == test {
                            GameSide::Win
                        } else if winner == base {
                            GameSide::Loss
                        } else {
                            continue;
                        }
                    }
                    "draw" => GameSide::Draw,
                    _ => continue,
                }
            } else {
                let actual_black = if slot == 0 {
                    label_black_meta.as_str()
                } else {
                    label_white_meta.as_str()
                };
                let test_is_black = actual_black == test;
                match result.outcome.as_str() {
                    "black_win" => {
                        if test_is_black {
                            GameSide::Win
                        } else {
                            GameSide::Loss
                        }
                    }
                    "white_win" => {
                        if test_is_black {
                            GameSide::Loss
                        } else {
                            GameSide::Win
                        }
                    }
                    "draw" => GameSide::Draw,
                    _ => continue,
                }
            };

            let entry = pair_buffer.entry(pair_idx).or_insert([None, None]);
            if entry[slot].is_none() {
                entry[slot] = Some(test_side);
            } else if entry[1 - slot].is_none() {
                // 同 slot が 2 度到着するのは通常の tournament 出力では起き得ない。
                // 空きスロットに入れつつ警告する。
                eprintln!(
                    "警告: {path} — pair_index={pair_idx} の slot={slot} が重複しています。\
                     空きスロットに配置しますが、結果は正確でない可能性があります。"
                );
                entry[1 - slot] = Some(test_side);
            }
            // else: entry[slot] も entry[1-slot] も埋まっているケースは
            // 上の `completed_pairs` チェックで弾かれるため到達しない。

            if let [Some(a), Some(b)] = *entry {
                total += Penta::from_pair(a, b);
                pair_buffer.remove(&pair_idx);
                completed_pairs.insert(pair_idx);
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
    println!("=== SPRT (post-hoc): {} vs {} ===", output.test, output.base);
    println!(
        "hypotheses: H0 = nelo0={:+.1}  H1 = nelo1={:+.1}  (alpha={}, beta={})",
        output.nelo0, output.nelo1, output.alpha, output.beta
    );
    println!("bounds:     LLR ∈ [{:+.3}, {:+.3}]", output.lower, output.upper);
    println!("pairs:      {}", output.pairs);
    println!("LLR:        {:+.3}", output.llr);
    println!("decision:   {}", output.decision);
    match &output.nelo {
        Some(n) => println!("nelo:       {:+.2} ± {:.2}", n.value, n.ci95),
        None => println!("nelo:       n/a (variance 0)"),
    }
    match &output.logistic_elo {
        Some(n) => println!("elo:        {:+.2} ± {:.2}", n.value, n.ci95),
        None => println!("elo:        n/a"),
    }
    println!("penta:      {}", penta);
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
                extra.total_plies += result.extra.total_plies;
                extra.completed_games += result.extra.completed_games;
                extra.black_wins += result.extra.black_wins;
                extra.white_wins += result.extra.white_wins;
                extra.draws += result.extra.draws;
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

    // 直接対決ペアごとの pentanomial 集計（nElo 表示用、テキスト出力時のみ）
    let h2h_penta: BTreeMap<(String, String), Penta> = if !cli.json {
        let mut map = BTreeMap::new();
        for (left, right) in head_to_head.keys() {
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
            map.insert((left.clone(), right.clone()), penta);
        }
        map
    } else {
        BTreeMap::new()
    };

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

        let base_label = cli
            .sprt_base_label
            .clone()
            .or_else(|| meta_sprt.as_ref().map(|m| m.base_label.clone()))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "--sprt 有効時は --sprt-base-label が必須です（meta 行に SPRT 情報が無いため自動推定できませんでした）"
                )
            })?;
        let test_label = cli
            .sprt_test_label
            .clone()
            .or_else(|| meta_sprt.as_ref().map(|m| m.test_label.clone()))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "--sprt 有効時は --sprt-test-label が必須です（meta 行に SPRT 情報が無いため自動推定できませんでした）"
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
        let an = labels.get(a).map_or(a.as_str(), |s| s.as_str());
        let bn = labels.get(b).map_or(b.as_str(), |s| s.as_str());
        let total = v.left_wins + v.right_wins + v.draws;
        let wr_a = if total > 0 {
            v.left_wins as f64 / total as f64 * 100.0
        } else {
            0.0
        };

        let elo = elo_diff(v.left_wins, v.right_wins, v.draws);
        let ci = elo_ci95(v.left_wins, v.right_wins, v.draws);

        // pentanomial nElo（right=test 視点で集計されているため、left 視点に変換）
        let nelo_str = h2h_penta
            .get(&(a.clone(), b.clone()))
            .and_then(|p| p.normalized_elo())
            .map(|(e, c)| format!(" | nElo:{:+.0} ±{:.0}", -e, c))
            .unwrap_or_default();

        let elo_str = match (elo, ci) {
            (Some(e), Some(c)) => format!(" | Elo差:{:+.0} ±{:.0}{}", e, c, nelo_str),
            _ => nelo_str,
        };

        println!(
            "  {:16} vs {:16} | {:3}局 | {}:{:3}勝 {}:{:3}勝 引分:{} | {}勝率:{:.1}%{}",
            an, bn, v.done, an, v.left_wins, bn, v.right_wins, v.draws, an, wr_a, elo_str
        );

        // 先手/後手別勝率
        if v.left_sente_games > 0 || v.left_gote_games > 0 {
            let half = v.done / 2;
            let half_up = half + v.done % 2;

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

            let a_sente = fmt_wr("先手", v.left_sente_wins, v.left_sente_games, half_up);
            let a_gote = fmt_wr("後手", v.left_gote_wins, v.left_gote_games, half);
            // right の先手 = left の後手局、right の後手 = left の先手局
            let r_sente_decisive = v.left_gote_games;
            let r_sente_wins = r_sente_decisive - v.left_gote_wins;
            let r_gote_decisive = v.left_sente_games;
            let r_gote_wins = r_gote_decisive - v.left_sente_wins;
            let b_sente = fmt_wr("先手", r_sente_wins, r_sente_decisive, half);
            let b_gote = fmt_wr("後手", r_gote_wins, r_gote_decisive, half_up);
            println!("    {} {} {}", an, a_sente, a_gote);
            println!("    {} {} {}", bn, b_sente, b_gote);
        }
    }

    if extra.completed_games > 0 {
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
            extra.total_plies as f64 / extra.completed_games as f64,
            extra.completed_games
        );
        println!(
            "  先手勝率: {:.1}% ({}/{} 決着局), 後手勝率: {:.1}% ({}/{} 決着局), 引分: {}",
            black_wr, extra.black_wins, decisive, white_wr, extra.white_wins, decisive, extra.draws
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
            sides.sort_by(|(a, _), (b, _)| a.cmp(b));
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

    /// 破損 JSON の先頭行は警告を出して当該ファイルのみスキップし、他ファイルから収集できる。
    #[test]
    fn broken_json_is_skipped_with_warning() {
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

    /// `log_line_type` が JSON の type フィールドを正しく返す。
    #[test]
    fn log_line_type_returns_type_string() {
        assert_eq!(log_line_type("{\"type\":\"meta\",\"x\":1}").as_deref(), Some("meta"));
        // 空白を挟んだ整形でも検出できる（旧 contains ベースではスキップされていたケース）
        assert_eq!(log_line_type("{ \"type\" : \"result\" }").as_deref(), Some("result"));
        // type が無い / 非 JSON / 非文字列
        assert_eq!(log_line_type("{\"foo\":1}"), None);
        assert_eq!(log_line_type("not json"), None);
        assert_eq!(log_line_type("{\"type\":42}"), None);
    }
}
