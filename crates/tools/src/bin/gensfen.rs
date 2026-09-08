use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use chrono::Local;
use clap::Parser;
use crossbeam_channel as chan;
use rand::Rng;
use rand::seq::SliceRandom;
use rshogi_core::movegen::{MoveList, generate_legal, is_legal_with_pass};
use rshogi_core::nnue::{
    LayerStackBucketMode, compute_layer_stack_progresskpabs_bucket_index,
    configure_layer_stack_routing, get_layer_stack_progress_kpabs_weights, get_network,
    init_nnue_from_bytes, layer_stack_progress_coeff_required,
    load_progress_coeff_kpabs_from_bytes, parse_layer_stack_bucket_mode, set_fv_scale_override,
    set_layer_stack_progress_kpabs_weights,
};
use rshogi_core::position::{EnteringKingPointInfo, Position};
use rshogi_core::types::{Color, EnteringKingRule, Move};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;
use std::sync::atomic::AtomicU64;
use tools::packed_sfen::{
    PackedSfenValue, move_to_psv_move16, pack_position, pack_position_hcp, psv_move16_to_hcpe,
};
use tools::selfplay::{
    EngineConfig, EngineProcess, EvalLog, GameEngines, GameOutcome, MultiPvCandidate,
    NativeBackend, ParsedPosition, SearchParams, TimeControl, UsiBackend, build_position,
    load_start_positions, side_label,
};

const DEFAULT_EVAL_HASH_SIZE_MB: usize = 64;
const DEFAULT_HCPE3_EVAL_DROP_THRESHOLD: i32 = 500;
const HCPE3_POLICY_ALGO_VERSION: u32 = 3;
// 消費側の固定長 buffer と将棋の合法手数上限に合わせる。
const MAX_HCPE3_POLICY_CANDIDATES: u32 = 593;
#[cfg(test)]
const TEST_HCPE3_MAX_MOVES_GAME_INFO_CODE_1: u32 = 256;

/// NNUE 学習用の教師局面（PSV/pack）を生成する gensfen ツール。
/// NativeBackend で `--eval-file` 指定の評価関数を使い、対局を回しながら
/// PackedSfenValue を書き出す。棋力評価には `tournament` バイナリを使うこと。
///
/// # よく使うコマンド例
///
/// - 基本（NativeBackend、1000局、nodes=80000）:
///   `cargo run -p tools --bin gensfen -- --eval-file eval/model.bin --games 1000 --nodes 80000`
///
/// - 30 並列で大規模生成:
///   `cargo run -p tools --bin gensfen -- --eval-file eval/model.bin --startpos-file start_sfens.txt --games 100000 --nodes 80000 --concurrency 30`
///
/// - USI モード（外部エンジンで対局させたい場合）:
///   `cargo run -p tools --bin gensfen -- --native=false --engine-path /path/to/usi-engine --usi-option EvalDir=/path/to/eval --usi-option FV_SCALE=24 --games 1000 --nodes 80000`
///
/// `--out-dir` 未指定時は `runs/gensfen/<timestamp>/` に `gensfen.jsonl`（result 行のみ）と
/// `gensfen.psv` を書き出す。
///
fn parse_rate_0_1(s: &str) -> std::result::Result<f64, String> {
    let v: f64 = s.parse().map_err(|e| format!("{e}"))?;
    if !(0.0..=1.0).contains(&v) {
        return Err(format!("value {v} is out of range 0.0..=1.0"));
    }
    Ok(v)
}

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "rshogi gensfen: training data (PSV/pack) generator via engine-vs-engine play"
)]
struct Cli {
    /// Number of games to run
    #[arg(long, default_value_t = 1)]
    games: u32,

    /// Maximum plies per game before declaring a draw
    #[arg(long, default_value_t = 512)]
    max_moves: u32,

    /// Initial time for Black in milliseconds
    #[arg(long, default_value_t = 0)]
    btime: u64,

    /// Initial time for White in milliseconds
    #[arg(long, default_value_t = 0)]
    wtime: u64,

    /// Increment for Black in milliseconds
    #[arg(long, default_value_t = 0)]
    binc: u64,

    /// Increment for White in milliseconds
    #[arg(long, default_value_t = 0)]
    winc: u64,

    /// Byoyomi time per move in milliseconds
    #[arg(long, default_value_t = 0)]
    byoyomi: u64,

    /// Search depth limit (go depth N)
    #[arg(long)]
    depth: Option<u32>,

    /// Search nodes limit (go nodes N)
    #[arg(long)]
    nodes: Option<u64>,

    /// Safety margin used when detecting timeouts
    #[arg(long, default_value_t = 1000)]
    timeout_margin_ms: u64,

    /// NetworkDelay USI option (if available)
    #[arg(long)]
    network_delay: Option<i64>,

    /// NetworkDelay2 USI option (if available)
    #[arg(long)]
    network_delay2: Option<i64>,

    /// MinimumThinkingTime USI option (if available)
    #[arg(long)]
    minimum_thinking_time: Option<i64>,

    /// SlowMover USI option (if available)
    #[arg(long)]
    slowmover: Option<i32>,

    /// Enable USI_Ponder (if available)
    #[arg(long, default_value_t = false)]
    ponder: bool,

    /// Threads USI option (default for both sides)
    #[arg(long, default_value_t = 1)]
    threads: usize,

    /// Threads for Black (overrides --threads)
    #[arg(long)]
    threads_black: Option<usize>,

    /// Threads for White (overrides --threads)
    #[arg(long)]
    threads_white: Option<usize>,

    /// Hash/USI_Hash size (MiB)
    #[arg(long, default_value_t = 1024)]
    hash_mb: u32,

    /// Path to engine-usi binary used when per-side paths are not set
    #[arg(long)]
    engine_path: Option<PathBuf>,

    /// Path to engine-usi binary for Black (overrides engine_path)
    #[arg(long)]
    engine_path_black: Option<PathBuf>,

    /// Path to engine-usi binary for White (overrides engine_path)
    #[arg(long)]
    engine_path_white: Option<PathBuf>,

    /// Common extra arguments passed to engine processes
    #[arg(long, num_args = 1..)]
    engine_args: Option<Vec<String>>,

    /// Extra arguments for Black (overrides engine_args when set)
    #[arg(long, num_args = 1..)]
    engine_args_black: Option<Vec<String>>,

    /// Extra arguments for White (overrides engine_args when set)
    #[arg(long, num_args = 1..)]
    engine_args_white: Option<Vec<String>>,

    /// USI options to set (format: "Name=Value", can be specified multiple times)
    #[arg(long = "usi-option", num_args = 1..)]
    usi_options: Option<Vec<String>>,

    /// USI options for Black (overrides usi_options when set)
    #[arg(long = "usi-option-black", num_args = 1..)]
    usi_options_black: Option<Vec<String>>,

    /// USI options for White (overrides usi_options when set)
    #[arg(long = "usi-option-white", num_args = 1..)]
    usi_options_white: Option<Vec<String>>,

    /// Start position file (USI position lines, one per line)
    #[arg(long)]
    startpos_file: Option<PathBuf>,

    /// Single start position specified as SFEN or full USI position command
    #[arg(long)]
    sfen: Option<String>,

    /// Randomly select start positions instead of sequential selection
    /// (effective when using --startpos-file with multiple positions)
    #[arg(long, default_value_t = false)]
    random_startpos: bool,

    /// 出力ディレクトリ（デフォルト: runs/gensfen/<timestamp>/）
    /// 指定ディレクトリ内に gensfen.jsonl, gensfen.psv 等が出力される
    #[arg(long)]
    out_dir: Option<PathBuf>,

    /// Enable info log output
    #[arg(long, default_value_t = false)]
    log_info: bool,

    /// result 行の diversions 配列を省き、件数のみ記録する (jsonl を大幅削減)。
    /// relabel_psv --deblunder は diversions の ply 位置を必要とするため、
    /// deblunder を掛ける可能性のある run では使わないこと
    #[arg(long, default_value_t = false)]
    omit_diversions: bool,

    /// Flush game log on every move (safer, but slower)
    #[arg(long, default_value_t = false)]
    flush_each_move: bool,

    /// 完了対局を永続化する fsync 間隔。1 は毎対局、0 は fsync 無効。
    #[arg(long, default_value_t = 1)]
    fsync_interval_games: u32,

    /// 評価値行を別ファイルに書き出す（startpos moves 行 + 評価値列）
    #[arg(long, default_value_t = false)]
    emit_eval_file: bool,

    /// ノード数などの簡易メトリクスを各対局ごとに JSONL で出力
    #[arg(long, default_value_t = false)]
    emit_metrics: bool,

    /// 学習データ (PackedSfenValue形式) の出力先パス
    /// 指定しない場合はデフォルトで <output>.psv に出力
    #[arg(long)]
    output_training_data: Option<PathBuf>,

    /// PSV の各レコードに対応する game_id を u32 little-endian で出力する sidecar。
    /// --training-data-format psv でのみ使用できる。
    #[arg(long)]
    emit_game_id_sidecar: Option<PathBuf>,

    /// 学習データ出力時に序盤の手数をスキップする（1手目からN手目まで）
    /// ランダム性確保のため、序盤の定跡手順をスキップする
    #[arg(
        long,
        default_value_t = 0,
        help = "Skip initial N plies (1 to N) for training data"
    )]
    skip_initial_ply: u32,

    /// 学習データ出力時に王手局面をスキップする
    /// 王手局面は応手が限られるため学習価値が低い
    /// 無効化するには --skip-in-check=false を指定
    #[arg(
        long,
        default_value_t = false,
        action = clap::ArgAction::Set,
        help = "Skip positions where king is in check (use --skip-in-check=false to disable)"
    )]
    skip_in_check: bool,

    /// 学習データの出力形式（psv / pack / hcpe3）
    #[arg(long, default_value = "psv")]
    training_data_format: String,

    /// hcpe3 形式の policy 分布に割り当てる visit の総票数
    #[arg(long, default_value_t = 65535)]
    hcpe3_policy_total: u16,

    /// hcpe3 形式の policy softmax の温度（centipawn 単位、大きいほど分布を均す）
    #[arg(long, default_value_t = 100.0)]
    hcpe3_policy_temp: f64,

    /// hcpe3 policy から最善候補との差がこの値を超える候補を除外する（負値で無効）
    #[arg(long, default_value_t = DEFAULT_HCPE3_EVAL_DROP_THRESHOLD)]
    hcpe3_eval_drop_threshold: i32,

    /// Number of concurrent worker threads
    #[arg(long, default_value_t = 1)]
    concurrency: usize,

    /// 前回中断した教師局面生成セッションを再開する。
    /// --out で指定した出力ファイルが存在する場合、完了済み対局数を検出して続きから実行する。
    #[arg(long, default_value_t = false)]
    resume: bool,

    /// 残留した out-dir lock を削除して取得し直す。
    /// lock 内の PID のプロセスが終了済みであることを確認してから指定する。
    #[arg(long, default_value_t = false)]
    force_unlock: bool,

    // =========================================================================
    // gensfen 重複回避オプション
    // =========================================================================
    /// rshogi-core を直接呼び出す NativeBackend を使用する（USI プロセスを起動しない）。
    /// デフォルト: true（`--eval-file` 必須）。USI モードで動かす場合は `--native=false`
    /// と `--engine-path` を指定する。
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    native: Option<bool>,

    /// NNUE 評価関数ファイルのパス（NativeBackend で使用）
    #[arg(long)]
    eval_file: Option<PathBuf>,

    /// progresskpabs 用の進行度係数ファイル（NativeBackend の LayerStacks ネットで使用）
    #[arg(long)]
    progress_file: Option<PathBuf>,

    /// NativeBackend の LayerStacks bucket mode。LayerStacks では必須。
    #[arg(long)]
    bucket_mode: Option<String>,

    /// NativeBackend の progresskpabs が推論に使う bucket 数。
    #[arg(long)]
    progress_buckets: Option<usize>,

    /// FV_SCALE オーバーライド（0=arch 文字列から自動判定、1 以上=指定値。NativeBackend 専用。
    /// arch 文字列の fv_scale が実際の学習スケールと食い違うネットで使用する）
    #[arg(long, default_value_t = 0)]
    fv_scale: i32,

    /// NativeBackend の TT・EvalHash・履歴を対局間で保持する。
    /// USI backend は isready のみ送信し、保持動作は接続先エンジンに依存する。
    /// tanuki- は毎対局クリアするため、デフォルト false。実験用。
    /// --keep-tt=true で有効化、--keep-tt=false で明示的に無効化。
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    keep_tt: Option<bool>,

    /// ハッシュベース重複検出のテーブルサイズ（エントリ数）。0 で無効。
    /// デフォルト: 67108864 (64M entries, 512MB)。
    #[arg(long)]
    dedup_hash_size: Option<u64>,

    /// 開始局面を重複なしで消費する（シャッフル + pop 方式）。
    /// デフォルト: true。`--startpos-no-repeat=false` で無効化。
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    startpos_no_repeat: Option<bool>,

    /// MultiPV ランダム選択の候補数。0 で無効。
    /// デフォルト: 0（無効）。有効にするには --random-multi-pv 4 等を指定。
    #[arg(long)]
    random_multi_pv: Option<u32>,

    /// MultiPV ランダム選択の評価値差閾値（centipawns）。
    /// PV1 のスコアとの差がこの値以内の候補からランダム選択する。
    /// --random-multi-pv が 2 以上のときは必須。
    #[arg(long)]
    random_multi_pv_diff: Option<i32>,

    /// ランダムムーブの回数。0 で無効。
    /// 序盤の指定範囲内で N 回、合法手からランダムに選択する。
    #[arg(long, default_value_t = 0)]
    random_move_count: u32,

    /// ランダムムーブ適用範囲の最小手数
    #[arg(long, default_value_t = 1)]
    random_move_min_ply: u32,

    /// ランダムムーブ適用範囲の最大手数
    #[arg(long, default_value_t = 24)]
    random_move_max_ply: u32,

    /// 開始局面シャッフルの乱数シード（--startpos-no-repeat 用）。
    /// 省略時はランダム生成。resume 時は meta から復元される。
    #[arg(long)]
    shuffle_seed: Option<u64>,

    /// dedup rate チェックの間隔（ゲーム数）。
    /// N ゲームごとに直近区間の重複率を計算し、閾値超過で警告を出力する。
    #[arg(long, default_value_t = 1000)]
    dedup_warn_interval: u32,

    /// dedup rate の警告閾値（0.0-1.0）。
    /// 直近区間の重複率がこの値を超えると stderr に警告を出力する。
    #[arg(long, default_value_t = 0.1, value_parser = parse_rate_0_1)]
    dedup_warn_rate: f64,
}

#[derive(Serialize, Deserialize)]
struct MetaLog {
    #[serde(rename = "type")]
    kind: String,
    timestamp: String,
    settings: MetaSettings,
    engine_cmd: EngineCommandMeta,
    start_positions: Vec<String>,
    output: String,
    info_log: Option<String>,
    /// resume 時に教師データの生成条件が同一であることを検証する。
    fingerprint: Value,
}

#[derive(Serialize, Deserialize)]
struct MetaSettings {
    games: u32,
    max_moves: u32,
    btime: u64,
    wtime: u64,
    binc: u64,
    winc: u64,
    byoyomi: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    depth: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    nodes: Option<u64>,
    timeout_margin_ms: u64,
    threads: usize,
    threads_black: usize,
    threads_white: usize,
    hash_mb: u32,
    network_delay: Option<i64>,
    network_delay2: Option<i64>,
    minimum_thinking_time: Option<i64>,
    slowmover: Option<i32>,
    ponder: bool,
    #[serde(default)]
    flush_each_move: bool,
    #[serde(default)]
    emit_eval_file: bool,
    #[serde(default)]
    emit_metrics: bool,
    startpos_file: Option<String>,
    sfen: Option<String>,
    #[serde(default)]
    random_startpos: bool,
    #[serde(default)]
    output_training_data: Option<String>,
    /// PSV レコードと 1:1・同順の game_id sidecar（u32 little-endian）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    game_id_sidecar: Option<String>,
    #[serde(default)]
    skip_initial_ply: u32,
    #[serde(default = "default_skip_in_check")]
    skip_in_check: bool,
    /// 開始局面シャッフルの乱数シード（--startpos-no-repeat 用）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    shuffle_seed: Option<u64>,
    /// NativeBackend の progresskpabs 進行度係数ファイル
    #[serde(default, skip_serializing_if = "Option::is_none")]
    progress_file: Option<String>,
    /// progress_file 内容の SHA-256（resume 時に同一パスへの係数差し替えを検出する）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    progress_file_sha256: Option<String>,
    /// NativeBackend の LayerStacks bucket mode。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bucket_mode: Option<String>,
    /// NativeBackend の progresskpabs routing bucket 数。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    progress_buckets: Option<usize>,
    /// FV_SCALE オーバーライド（未指定 = arch 文字列から自動判定）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fv_scale: Option<i32>,
    /// result 行の diversions 配列を省略したか（未指定 = 全量記録）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    omit_diversions: Option<bool>,
}

fn default_skip_in_check() -> bool {
    true
}

#[derive(Serialize, Deserialize)]
struct EngineCommandMeta {
    path_black: String,
    path_white: String,
    source_black: String,
    source_white: String,
    args_black: Vec<String>,
    args_white: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    usi_options_black: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    usi_options_white: Vec<String>,
}

/// バイナリの発見元を含む解決結果。
#[derive(Clone)]
struct ResolvedEnginePath {
    path: PathBuf,
    source: &'static str,
}

/// 先手と後手のエンジンバイナリパスの解決結果。
/// 各プレイヤーに異なるエンジンバイナリを使用できるようにする。
struct ResolvedEnginePaths {
    /// 先手（Black）のエンジンバイナリパス
    black: ResolvedEnginePath,
    /// 後手（White）のエンジンバイナリパス
    white: ResolvedEnginePath,
}

#[derive(Serialize)]
struct ResultLog<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    worker_id: usize,
    game_id: u32,
    start_pos_index: usize,
    start_sfen: &'a str,
    outcome: &'a str,
    reason: OutcomeReason,
    adopted: bool,
    plies: u32,
    final_points_black: u32,
    final_points_white: u32,
    king_in_enemy_black: bool,
    king_in_enemy_white: bool,
    enemy_zone_pieces_black: u32,
    enemy_zone_pieces_white: u32,
    /// 乱択来歴。--omit-diversions 時は None (キー自体を省く)。relabel_psv --deblunder が
    /// ply 境界として消費するため、省略の有無は run 内で一貫している必要がある
    #[serde(skip_serializing_if = "Option::is_none")]
    diversions: Option<&'a [DiversionLog]>,
    /// --omit-diversions 時のみ: 省いた multipv 乱択の件数
    #[serde(skip_serializing_if = "Option::is_none")]
    multipv_diversions: Option<u64>,
    /// --omit-diversions 時のみ: 省いたランダムムーブの件数
    #[serde(skip_serializing_if = "Option::is_none")]
    random_moves: Option<u64>,
    /// --omit-diversions 時のみ: 省いた diversion の総数。kind 別件数の合計と
    /// 突き合わせることで、将来 kind が増えた際の集計漏れを検知できる
    #[serde(skip_serializing_if = "Option::is_none")]
    diversions_total: Option<u64>,
    /// この result より先にコミット済みの worker 教師ファイル長。
    training_bytes: u64,
    /// この result より先にコミット済みの worker sidecar 長。
    sidecar_bytes: Option<u64>,
    /// この result より先にコミット済みの worker info log 長。
    info_bytes: Option<u64>,
    /// この result より先にコミット済みの worker eval file 長。
    eval_bytes: Option<u64>,
    /// この result より先にコミット済みの worker metrics 長。
    metrics_bytes: Option<u64>,
    /// ファイル長だけでは電源断前に永続化済みだった世代を識別できないため記録する。
    fsync_boundary: bool,
}

#[derive(Clone, Copy, Debug, Default)]
struct CheckpointLengths {
    training: u64,
    sidecar: Option<u64>,
    info: Option<u64>,
    eval: Option<u64>,
    metrics: Option<u64>,
}

fn write_committed_result<W, F>(
    writer: &mut W,
    result: ResultLog<'_>,
    fsync_boundary: bool,
    persist_training: F,
) -> Result<()>
where
    W: Write,
    F: FnOnce() -> Result<CheckpointLengths>,
{
    let lengths = persist_training()?;
    if injected_fault("before_result") {
        bail!("injected failure before result write");
    }
    if injected_fault("result_partial") {
        writer.write_all(b"{\"type\":\"result\"")?;
        writer.flush()?;
        bail!("injected partial result write");
    }
    let committed = ResultLog {
        training_bytes: lengths.training,
        sidecar_bytes: lengths.sidecar,
        info_bytes: lengths.info,
        eval_bytes: lengths.eval,
        metrics_bytes: lengths.metrics,
        fsync_boundary,
        ..result
    };
    serde_json::to_writer(&mut *writer, &committed)?;
    writer.write_all(b"\n")?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AbnormalEndReason {
    Timeout,
    IllegalMove,
    NoBestmove,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutcomeReason {
    Mate,
    Resign,
    Win,
    Sennichite,
    PerpetualCheck,
    MaxMoves,
    Timeout,
    IllegalMove,
    NoBestmove,
}

impl Serialize for OutcomeReason {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl OutcomeReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::Mate => "mate",
            Self::Resign => "resign",
            Self::Win => "win",
            Self::Sennichite => "sennichite",
            Self::PerpetualCheck => "perpetual_check",
            Self::MaxMoves => "max_moves",
            Self::Timeout => "timeout",
            Self::IllegalMove => "illegal_move",
            Self::NoBestmove => "no_bestmove",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrainingDisposition {
    Adopt,
    Discard(AbnormalEndReason),
}

impl TrainingDisposition {
    fn from_outcome_reason(reason: OutcomeReason) -> Self {
        match reason {
            OutcomeReason::Mate
            | OutcomeReason::Resign
            | OutcomeReason::Win
            | OutcomeReason::Sennichite
            | OutcomeReason::PerpetualCheck
            | OutcomeReason::MaxMoves => Self::Adopt,
            OutcomeReason::Timeout => Self::Discard(AbnormalEndReason::Timeout),
            OutcomeReason::IllegalMove => Self::Discard(AbnormalEndReason::IllegalMove),
            OutcomeReason::NoBestmove => Self::Discard(AbnormalEndReason::NoBestmove),
        }
    }

    fn is_adopted(self) -> bool {
        self == Self::Adopt
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct TrainingStats {
    total_written: u64,
    skipped_initial: u64,
    skipped_in_check: u64,
    discarded_positions: u64,
    discarded_gap_positions: u64,
    discarded_timeout_games: u64,
    discarded_illegal_move_games: u64,
    discarded_no_bestmove_games: u64,
    declaration_win_dedup_skipped_games: u64,
}

impl TrainingStats {
    fn merge(&mut self, other: Self) {
        self.total_written += other.total_written;
        self.skipped_initial += other.skipped_initial;
        self.skipped_in_check += other.skipped_in_check;
        self.discarded_positions += other.discarded_positions;
        self.discarded_gap_positions += other.discarded_gap_positions;
        self.discarded_timeout_games += other.discarded_timeout_games;
        self.discarded_illegal_move_games += other.discarded_illegal_move_games;
        self.discarded_no_bestmove_games += other.discarded_no_bestmove_games;
        self.declaration_win_dedup_skipped_games += other.declaration_win_dedup_skipped_games;
    }
}

fn game_result_for_side(outcome: GameOutcome, side_to_move: Color) -> i8 {
    match outcome {
        GameOutcome::BlackWin => match side_to_move {
            Color::Black => 1,
            Color::White => -1,
        },
        GameOutcome::WhiteWin => match side_to_move {
            Color::Black => -1,
            Color::White => 1,
        },
        GameOutcome::Draw => 0,
        GameOutcome::InProgress => unreachable!(),
    }
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct DiversionLog {
    ply: u32,
    kind: &'static str,
    chosen_move: String,
    best_move: Option<String>,
    score_gap_cp: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FinalEnteringKingMeta {
    black: EnteringKingPointInfo,
    white: EnteringKingPointInfo,
}

fn final_entering_king_meta(pos: &Position) -> FinalEnteringKingMeta {
    FinalEnteringKingMeta {
        black: pos.entering_king_point_info(Color::Black),
        white: pos.entering_king_point_info(Color::White),
    }
}

#[derive(Serialize)]
struct MetricsLog {
    #[serde(rename = "type")]
    kind: &'static str,
    game_id: u32,
    plies: u32,
    nodes_black: u64,
    nodes_white: u64,
    nodes_first60: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_cp_black: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_cp_white: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_mate_black: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_mate_white: Option<i32>,
    outcome: String,
    reason: OutcomeReason,
}

#[derive(Default)]
struct MetricsCollector {
    nodes_black: u64,
    nodes_white: u64,
    nodes_first60: u64,
    last_cp_black: Option<i32>,
    last_cp_white: Option<i32>,
    last_mate_black: Option<i32>,
    last_mate_white: Option<i32>,
}

impl MetricsCollector {
    fn update(&mut self, side: Color, eval: Option<&EvalLog>, ply: u32) {
        let Some(eval) = eval else { return };
        if let Some(nodes) = eval.nodes {
            if side == Color::Black {
                self.nodes_black = self.nodes_black.saturating_add(nodes);
            } else {
                self.nodes_white = self.nodes_white.saturating_add(nodes);
            }
            if ply <= 60 {
                self.nodes_first60 = self.nodes_first60.saturating_add(nodes);
            }
        }
        if let Some(mate) = eval.score_mate {
            if side == Color::Black {
                self.last_mate_black = Some(mate);
                self.last_cp_black = None;
            } else {
                self.last_mate_white = Some(mate);
                self.last_cp_white = None;
            }
        } else if let Some(cp) = eval.score_cp {
            if side == Color::Black {
                self.last_cp_black = Some(cp);
                self.last_mate_black = None;
            } else {
                self.last_cp_white = Some(cp);
                self.last_mate_white = None;
            }
        }
    }
}

/// 学習データの出力形式
#[derive(Clone, Copy, PartialEq, Eq)]
enum TrainingFormat {
    /// PackedSfenValue 40バイト固定長形式
    Psv,
    /// 可変長対局棋譜形式
    Pack,
    /// 可変長対局棋譜 + 各手の MultiPV policy 分布を持つ hcpe3 形式
    Hcpe3,
}

/// hcpe3 形式でのみ使う追加データ（局面 replay 用の実着手と policy 分布）
struct Hcpe3EntryData {
    /// 実際に着手した手（rshogi move16）。replay でこの手を辿って局面を再構成する
    selected_move16: u16,
    /// 手番側視点の eval。詰みは 32000-ply 符号化
    eval: i16,
    /// policy 分布 (rshogi move16, visit)。multipv 昇順
    policy: Vec<(u16, u16)>,
}

/// 学習データ出力用のエントリ（game_result未設定の一時データ）
struct TrainingEntry {
    /// PackedSfen (32バイト)
    sfen: [u8; 32],
    /// 探索スコア（手番側から見た評価値）
    score: i16,
    /// 出力形式に応じた手ラベル（PSV は最善手、Pack は実着手）
    move16: u16,
    /// 手数
    game_ply: u16,
    /// 手番（game_result計算用）
    side_to_move: Color,
    /// hcpe3 形式のときのみ Some（Psv/Pack では None で確保なし）
    hcpe3: Option<Hcpe3EntryData>,
}

/// hcpe3 policy の生成設定。
#[derive(Clone, Copy)]
struct Hcpe3PolicyConfig {
    total: u16,
    temp: f64,
    multi_pv: u32,
    eval_drop_threshold: i32,
}

/// 学習データ収集器
/// 対局中の局面データを収集し、対局終了後に勝敗を設定して書き出す
struct TrainingDataCollector {
    entries: Vec<TrainingEntry>,
    writer: BufWriter<File>,
    /// PSV と同じループで game_id を書く。PSV 以外では常に None。
    game_id_writer: Option<BufWriter<File>>,
    format: TrainingFormat,
    /// hcpe3 policy の生成設定
    policy: Hcpe3PolicyConfig,
    /// hcpe3 gameInfo に符号化する最大手数
    max_moves: u32,
    skip_initial_ply: u32,
    skip_in_check: bool,
    total_written: u64,
    skipped_initial: u64,
    skipped_in_check: u64,
    discarded_positions: u64,
    discarded_gap_positions: u64,
    discarded_timeout_games: u64,
    discarded_illegal_move_games: u64,
    discarded_no_bestmove_games: u64,
    declaration_win_dedup_skipped_games: u64,
    /// .pack 形式用: 対局開始局面の HCP バイト列、手数、平手フラグ
    start_hcp: Option<([u8; 32], u16, bool)>,
    /// .pack 形式用: 平手局面の PackedSfen（平手判定の基準）
    hirate_packed_sfen: [u8; 32],
}

impl TrainingDataCollector {
    #[cfg(test)]
    fn new(
        path: &Path,
        skip_initial_ply: u32,
        skip_in_check: bool,
        format: TrainingFormat,
        policy_total: u16,
        policy_temp: f64,
        game_id_path: Option<&Path>,
    ) -> Result<Self> {
        Self::open(
            path,
            skip_initial_ply,
            skip_in_check,
            format,
            Hcpe3PolicyConfig {
                total: policy_total,
                temp: policy_temp,
                multi_pv: u32::MAX,
                eval_drop_threshold: DEFAULT_HCPE3_EVAL_DROP_THRESHOLD,
            },
            TEST_HCPE3_MAX_MOVES_GAME_INFO_CODE_1,
            game_id_path,
            false,
        )
    }

    fn open(
        path: &Path,
        skip_initial_ply: u32,
        skip_in_check: bool,
        format: TrainingFormat,
        policy: Hcpe3PolicyConfig,
        max_moves: u32,
        game_id_path: Option<&Path>,
        append: bool,
    ) -> Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create training data directory: {}", parent.display())
            })?;
        }
        let file = open_worker_checkpoint(path, append)
            .with_context(|| format!("failed to open training data file: {}", path.display()))?;
        let game_id_writer = game_id_path
            .map(|game_id_path| {
                open_worker_checkpoint(game_id_path, append).map(BufWriter::new).with_context(
                    || format!("failed to open game_id sidecar: {}", game_id_path.display()),
                )
            })
            .transpose()?;

        // 平手判定用の PackedSfen を事前計算
        let mut hirate_pos = Position::new();
        hirate_pos.set_hirate();
        let hirate_packed_sfen = pack_position(&hirate_pos);

        Ok(Self {
            entries: Vec::new(),
            writer: BufWriter::new(file),
            game_id_writer,
            format,
            policy,
            max_moves,
            skip_initial_ply,
            skip_in_check,
            total_written: 0,
            skipped_initial: 0,
            skipped_in_check: 0,
            discarded_positions: 0,
            discarded_gap_positions: 0,
            discarded_timeout_games: 0,
            discarded_illegal_move_games: 0,
            discarded_no_bestmove_games: 0,
            declaration_win_dedup_skipped_games: 0,
            start_hcp: None,
            hirate_packed_sfen,
        })
    }

    /// 新しい対局を開始（エントリをクリア）
    fn start_game(&mut self) {
        self.entries.clear();
        self.start_hcp = None;
    }

    /// 現在蓄積中のエントリ数
    fn entries_len(&self) -> usize {
        self.entries.len()
    }

    fn record_declaration_win_dedup_skip(&mut self) {
        self.declaration_win_dedup_skipped_games += 1;
    }

    /// 記録局面が 1 手飛んだとき、replay が必要な形式では蓄積中のセグメントを
    /// 破棄して次の記録局面から取り直す（PSV は各局面が独立なので影響を受けない）。
    fn discard_segment_on_gap(&mut self) {
        if matches!(self.format, TrainingFormat::Pack | TrainingFormat::Hcpe3) {
            self.discarded_gap_positions += self.entries.len() as u64;
            self.entries.clear();
            self.start_hcp = None;
        }
    }

    /// 局面を記録（game_resultは後で設定）
    /// 注: game_plyとスキップ判定はpos.game_ply()を使用する
    /// （startpos+movesやSFEN手数指定のケースに対応するため）
    fn record_position(
        &mut self,
        pos: &Position,
        score_cp: Option<i32>,
        score_mate: Option<i32>,
        best_move: Option<Move>,
        played_move: Move,
        candidates: &[MultiPvCandidate],
    ) {
        let current_ply = pos.game_ply();

        // 序盤をスキップ（1手目から skip_initial_ply 手目まで）
        if current_ply <= self.skip_initial_ply as i32 {
            self.skipped_initial += 1;
            return;
        }

        // 王手局面をスキップ
        if self.skip_in_check && pos.in_check() {
            self.skipped_in_check += 1;
            return;
        }

        // スコアを決定（mate > cp の優先順位）
        let score = if let Some(mate) = score_mate {
            // 詰みスコアは大きな値にクリップ
            if mate >= 0 {
                10000i16 // 勝ちの詰み（即詰みを含む）
            } else {
                -10000i16 // 負けの詰み
            }
        } else if let Some(cp) = score_cp {
            // 通常のセンチポーンスコア
            cp.clamp(-10000, 10000) as i16
        } else {
            // スコアがない場合は記録しない。Pack/hcpe3 は手列を連続させて replay するため、
            // 蓄積途中で 1 手飛ぶと復元不能になる。途中のセグメントを破棄して取り直す。
            self.discard_segment_on_gap();
            return;
        };

        // PSV と hcpe3 の move16 は最善手ラベル、Pack は replay 用の実着手を記録する。
        // hcpe3 の replay 手は下の Hcpe3EntryData に別途保持する。
        let move16 = match self.format {
            TrainingFormat::Pack => move_to_psv_move16(played_move),
            TrainingFormat::Psv | TrainingFormat::Hcpe3 => best_move.map_or(0, move_to_psv_move16),
        };

        // PackedSfenを生成
        let packed_sfen = pack_position(pos);

        // Pack/hcpe3 形式: 最初のエントリで開始局面の HCP を記録（以降を replay する起点）
        if matches!(self.format, TrainingFormat::Pack | TrainingFormat::Hcpe3)
            && self.start_hcp.is_none()
        {
            // 1 byte の平手ヘッダは、配置だけでなく replay 開始手数も初期値のときに限る。
            let is_hirate = packed_sfen == self.hirate_packed_sfen && current_ply == 1;
            let hcp = pack_position_hcp(pos);
            let ply = current_ply.clamp(0, u16::MAX as i32) as u16;
            self.start_hcp = Some((hcp, ply, is_hirate));
        }

        // hcpe3 形式は replay 整合のため selectedMove16 を実着手にし、各手に MultiPV policy を持たせる
        let hcpe3 = if self.format == TrainingFormat::Hcpe3 {
            let selected_move16 = move_to_psv_move16(played_move);
            let eval = score_mate.map_or(score, mate_to_eval);
            let policy = if candidates.is_empty() {
                vec![(selected_move16, 1u16)]
            } else {
                multipv_to_policy(
                    candidates,
                    played_move,
                    self.policy.total,
                    self.policy.temp,
                    self.policy.multi_pv,
                    self.policy.eval_drop_threshold,
                )
            };
            Some(Hcpe3EntryData {
                selected_move16,
                eval,
                policy,
            })
        } else {
            None
        };

        self.entries.push(TrainingEntry {
            sfen: packed_sfen,
            score,
            move16,
            game_ply: current_ply.clamp(0, u16::MAX as i32) as u16,
            side_to_move: pos.side_to_move(),
            hcpe3,
        });
    }

    /// 宣言勝ちが成立した現在局面を PSV の終端局面として記録する。
    fn record_declaration_win_position(&mut self, pos: &Position) {
        if self.format != TrainingFormat::Psv {
            return;
        }

        let current_ply = pos.game_ply();
        if current_ply <= self.skip_initial_ply as i32 {
            self.skipped_initial += 1;
            return;
        }

        // 宣言成立は探索値より強い勝ち確定情報なので、評価の符号や有無に依存させない。
        self.entries.push(TrainingEntry {
            sfen: pack_position(pos),
            score: 10000,
            move16: 0,
            game_ply: current_ply.clamp(0, u16::MAX as i32) as u16,
            side_to_move: pos.side_to_move(),
            hcpe3: None,
        });
    }

    /// 対局終了時に勝敗を設定して書き出す
    #[cfg(test)]
    fn finish_game(
        &mut self,
        outcome: GameOutcome,
        disposition: TrainingDisposition,
        game_id: u32,
    ) -> Result<()> {
        self.finish_game_with_reason(outcome, OutcomeReason::Mate, disposition, game_id)
    }

    fn finish_game_with_reason(
        &mut self,
        outcome: GameOutcome,
        outcome_reason: OutcomeReason,
        disposition: TrainingDisposition,
        game_id: u32,
    ) -> Result<()> {
        if let TrainingDisposition::Discard(reason) = disposition {
            self.discarded_positions += self.entries.len() as u64;
            match reason {
                AbnormalEndReason::Timeout => self.discarded_timeout_games += 1,
                AbnormalEndReason::IllegalMove => self.discarded_illegal_move_games += 1,
                AbnormalEndReason::NoBestmove => self.discarded_no_bestmove_games += 1,
            }
            self.entries.clear();
            return Ok(());
        }

        if outcome == GameOutcome::InProgress {
            return Err(anyhow!("cannot adopt an in-progress game as training data"));
        }

        if self.entries.is_empty() {
            return Ok(());
        }

        match self.format {
            TrainingFormat::Psv => self.finish_game_psv(outcome, game_id)?,
            TrainingFormat::Pack => self.finish_game_pack(outcome)?,
            TrainingFormat::Hcpe3 => self.finish_game_hcpe3(outcome, outcome_reason)?,
        }

        self.entries.clear();
        Ok(())
    }

    /// PSV 形式で書き出す（PackedSfenValue 40バイト固定長）
    fn finish_game_psv(&mut self, outcome: GameOutcome, game_id: u32) -> Result<()> {
        for (idx, entry) in self.entries.iter().enumerate() {
            // game_result: 手番側から見た勝敗
            // 1 = 勝ち, 0 = 引き分け, -1 = 負け
            let game_result = game_result_for_side(outcome, entry.side_to_move);

            let psv = PackedSfenValue {
                sfen: entry.sfen,
                score: entry.score,
                move16: entry.move16,
                game_ply: entry.game_ply,
                game_result,
                padding: 0,
            };

            self.writer
                .write_all(&psv.to_bytes())
                .with_context(|| format!("failed to write position {idx} of game"))?;
            // PSV と sidecar の 1:1・同順を構造的に保つため、同じループで連続して書く。
            if let Some(writer) = self.game_id_writer.as_mut() {
                if injected_fault("sidecar_partial") {
                    writer.write_all(&game_id.to_le_bytes()[..2])?;
                    bail!("injected partial sidecar write");
                }
                writer.write_all(&game_id.to_le_bytes()).with_context(|| {
                    format!("failed to write game_id for position {idx} of game")
                })?;
            }
            self.total_written += 1;
        }
        Ok(())
    }

    /// .pack 形式で書き出す（cshogi 可変長対局棋譜）
    ///
    /// フォーマット:
    ///   [開始局面フラグ: u8] — 1=平手, 0=任意局面
    ///   0 の場合: [HuffmanCodedPos: 32byte][game_ply: u16 LE]
    ///   繰り返し: [move16(hcpe): u16 LE][score: i16 LE]
    ///   [終局マーカー: u16 LE (from==to)] [終局理由: u8]
    fn finish_game_pack(&mut self, outcome: GameOutcome) -> Result<()> {
        let (hcp, start_ply, is_hirate) =
            self.start_hcp.ok_or_else(|| anyhow!("pack format: start_hcp not set"))?;

        // 1. 開始局面ヘッダ
        if is_hirate {
            self.writer.write_all(&[1u8])?;
        } else {
            self.writer.write_all(&[0u8])?;
            self.writer.write_all(&hcp)?;
            self.writer.write_all(&start_ply.to_le_bytes())?;
        }

        // 2. 各エントリの指し手とスコア
        for entry in &self.entries {
            let hcpe_move16 = psv_move16_to_hcpe(entry.move16);
            self.writer.write_all(&hcpe_move16.to_le_bytes())?;
            self.writer.write_all(&entry.score.to_le_bytes())?;
            self.total_written += 1;
        }

        // 3. 終局マーカー: game_result を絶対値エンコード
        //    0=draw, 1=black_win, 2=white_win
        let result_val: u16 = match outcome {
            GameOutcome::BlackWin => 1,
            GameOutcome::WhiteWin => 2,
            GameOutcome::Draw => 0,
            GameOutcome::InProgress => unreachable!(),
        };
        // 終局マーカー: from==to となる u16 (result_val | (result_val << 7))
        let end_marker = result_val | (result_val << 7);
        self.writer.write_all(&end_marker.to_le_bytes())?;

        // 4. 終局理由: 1 = 通常終了
        self.writer.write_all(&[1u8])?;

        Ok(())
    }

    /// hcpe3 形式で書き出す（可変長対局棋譜 + 各手の policy 分布）。
    ///
    /// レイアウト:
    ///   [hcp: 32byte][moveNum: u16 LE][result: u8][gameInfo: u8]
    ///   moveNum 回: [selectedMove16: u16 LE][eval: i16 LE][candidateNum: u16 LE]
    ///               candidateNum 回: [move16: u16 LE][visitNum: u16 LE]
    /// result は 0=draw / 1=black_win / 2=white_win。move16 は hcpe 形式。
    /// 局面は hcp から selectedMove16 を順に辿って再構成するため手列が連続している必要がある。
    fn finish_game_hcpe3(
        &mut self,
        outcome: GameOutcome,
        outcome_reason: OutcomeReason,
    ) -> Result<()> {
        let (hcp, _start_ply, _is_hirate) =
            self.start_hcp.ok_or_else(|| anyhow!("hcpe3 format: start_hcp not set"))?;
        let move_num: u16 = self.entries.len().try_into().map_err(|_| {
            anyhow!("hcpe3 format: too many moves in one game ({})", self.entries.len())
        })?;
        let result = hcpe3_result(outcome, outcome_reason);
        let game_info = hcpe3_game_info(self.max_moves);

        self.writer.write_all(&hcp)?;
        self.writer.write_all(&move_num.to_le_bytes())?;
        self.writer.write_all(&[result, game_info])?;

        for entry in &self.entries {
            let h = entry
                .hcpe3
                .as_ref()
                .ok_or_else(|| anyhow!("hcpe3 format: entry has no policy data"))?;
            let selected = psv_move16_to_hcpe(h.selected_move16);
            let candidate_num: u16 =
                h.policy.len().try_into().map_err(|_| {
                    anyhow!("hcpe3 format: too many candidates ({})", h.policy.len())
                })?;
            self.writer.write_all(&selected.to_le_bytes())?;
            self.writer.write_all(&h.eval.to_le_bytes())?;
            self.writer.write_all(&candidate_num.to_le_bytes())?;
            for (move16, visit) in &h.policy {
                let hcpe_move16 = psv_move16_to_hcpe(*move16);
                self.writer.write_all(&hcpe_move16.to_le_bytes())?;
                self.writer.write_all(&visit.to_le_bytes())?;
            }
            self.total_written += 1;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        self.writer.flush()?;
        if let Some(writer) = self.game_id_writer.as_mut() {
            writer.flush()?;
        }
        Ok(())
    }

    fn committed_lengths(&mut self) -> Result<(u64, Option<u64>)> {
        self.flush()?;
        let training = self.writer.get_ref().metadata()?.len();
        let sidecar = self
            .game_id_writer
            .as_ref()
            .map(|writer| writer.get_ref().metadata().map(|meta| meta.len()))
            .transpose()?;
        Ok((training, sidecar))
    }

    fn sync_all(&mut self) -> Result<()> {
        self.flush()?;
        self.writer.get_ref().sync_all()?;
        if let Some(writer) = self.game_id_writer.as_mut() {
            writer.get_ref().sync_all()?;
        }
        Ok(())
    }

    fn stats(&self) -> TrainingStats {
        TrainingStats {
            total_written: self.total_written,
            skipped_initial: self.skipped_initial,
            skipped_in_check: self.skipped_in_check,
            discarded_positions: self.discarded_positions,
            discarded_gap_positions: self.discarded_gap_positions,
            discarded_timeout_games: self.discarded_timeout_games,
            discarded_illegal_move_games: self.discarded_illegal_move_games,
            discarded_no_bestmove_games: self.discarded_no_bestmove_games,
            declaration_win_dedup_skipped_games: self.declaration_win_dedup_skipped_games,
        }
    }
}

fn hcpe3_result(outcome: GameOutcome, outcome_reason: OutcomeReason) -> u8 {
    let result = match outcome {
        GameOutcome::BlackWin => 1,
        GameOutcome::WhiteWin => 2,
        GameOutcome::Draw => 0,
        GameOutcome::InProgress => unreachable!(),
    };
    result
        | match outcome_reason {
            OutcomeReason::Sennichite | OutcomeReason::PerpetualCheck => 4,
            OutcomeReason::Win => 8,
            OutcomeReason::MaxMoves => 16,
            _ => 0,
        }
}

fn hcpe3_game_info(max_moves: u32) -> u8 {
    match max_moves {
        256 => 1 << 2,
        320 => 2 << 2,
        512 => 3 << 2,
        _ => 0,
    }
}

// =============================================================================
// gensfen ユーティリティ型
// =============================================================================

/// ハッシュベース重複検出テーブル（tanuki- section 2-1 と同方式）
///
/// Zobrist ハッシュを衝突時上書き方式で記録する。
/// 重複検出時はそれまでの蓄積エントリをクリアし、対局は続行する。
///
/// tanuki- と同じく全スレッドで1つのテーブルを共有する（`Arc` で配布）。
/// `AtomicU64` + `Relaxed` ordering でロックフリーアクセス。
/// レース条件は許容: 最悪ケースは重複の見逃しだが、上書き方式なので致命的ではない。
struct SharedDedupHash {
    table: Vec<std::sync::atomic::AtomicU64>,
    mask: u64,
}

impl SharedDedupHash {
    fn new(size: u64) -> Self {
        let size = size.next_power_of_two();
        let table: Vec<_> = (0..size).map(|_| AtomicU64::new(0)).collect();
        Self {
            table,
            mask: size - 1,
        }
    }

    fn effective_key(key: u64) -> u64 {
        // key=0 は未使用エントリと区別できないので特殊扱い
        if key == 0 { 1 } else { key }
    }

    fn contains(&self, key: u64) -> bool {
        let effective_key = Self::effective_key(key);
        let idx = (effective_key & self.mask) as usize;
        self.table[idx].load(Ordering::Relaxed) == effective_key
    }

    /// 重複なら true を返し、新規なら挿入して false を返す
    fn check_and_insert(&self, key: u64) -> bool {
        let effective_key = Self::effective_key(key);
        let idx = (effective_key & self.mask) as usize;
        if self.table[idx].load(Ordering::Relaxed) == effective_key {
            return true;
        }
        self.table[idx].store(effective_key, Ordering::Relaxed);
        false
    }
}

#[derive(Default)]
struct PendingDedupKeys {
    ordered: Vec<u64>,
    unique: HashSet<u64>,
}

impl PendingDedupKeys {
    fn check_and_stage(&mut self, dedup_hash: &SharedDedupHash, key: u64) -> bool {
        let effective_key = SharedDedupHash::effective_key(key);
        let shared_hit = dedup_hash.contains(effective_key);
        let first_encounter = self.unique.insert(effective_key);
        if first_encounter {
            self.ordered.push(effective_key);
        }
        shared_hit || !first_encounter
    }

    fn publish(&self, dedup_hash: &SharedDedupHash) {
        // 同じ対局からの反映順を初回遭遇順に固定するため、ordered をそのまま走査する。
        for &key in &self.ordered {
            dedup_hash.check_and_insert(key);
        }
    }
}

fn check_training_position_dedup(
    dedup_hash: Option<&SharedDedupHash>,
    pending_keys: &mut PendingDedupKeys,
    key: u64,
    collector: Option<&mut TrainingDataCollector>,
    dedup_hits: &mut u64,
    dedup_discarded: &mut u64,
    interval_dedup_hits: &mut u64,
    interval_positions_checked: &mut u64,
) -> bool {
    let Some(dedup_hash) = dedup_hash else {
        return false;
    };

    *interval_positions_checked += 1;
    if !pending_keys.check_and_stage(dedup_hash, key) {
        return false;
    }

    *dedup_hits += 1;
    *interval_dedup_hits += 1;
    if let Some(collector) = collector {
        *dedup_discarded += collector.entries_len() as u64;
        collector.start_game();
    }
    true
}

fn check_declaration_win_position_dedup(
    format: TrainingFormat,
    dedup_hash: Option<&SharedDedupHash>,
    pending_keys: &mut PendingDedupKeys,
    key: u64,
    collector: Option<&mut TrainingDataCollector>,
    dedup_hits: &mut u64,
    interval_dedup_hits: &mut u64,
    interval_positions_checked: &mut u64,
) -> bool {
    if format != TrainingFormat::Psv {
        return false;
    }
    let Some(dedup_hash) = dedup_hash else {
        return false;
    };

    *interval_positions_checked += 1;
    if !pending_keys.check_and_stage(dedup_hash, key) {
        return false;
    }

    *dedup_hits += 1;
    *interval_dedup_hits += 1;
    if let Some(collector) = collector {
        // 終端後には再収集できないため、重複した終端だけを除外して収集済み局面は残す。
        collector.record_declaration_win_dedup_skip();
    }
    true
}

/// 開始局面を重複なしで消費するためのシャッフル済みインデックス列
///
/// 専用の `StdRng`（seed 固定）を使うため、同じ seed + count から
/// 同一の順列を再構築でき、resume 時に completed_games 分だけ
/// `next()` を呼び進めれば正確な位置を復元できる。
struct ShuffledStartpos {
    indices: Vec<usize>,
    cursor: usize,
    rng: rand::rngs::StdRng,
}

impl ShuffledStartpos {
    fn new(count: usize, seed: u64) -> Self {
        use rand::SeedableRng;
        let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
        let mut indices: Vec<usize> = (0..count).collect();
        indices.shuffle(&mut rng);
        Self {
            indices,
            cursor: 0,
            rng,
        }
    }

    fn next(&mut self) -> usize {
        if self.cursor >= self.indices.len() {
            self.indices.shuffle(&mut self.rng);
            self.cursor = 0;
        }
        let idx = self.indices[self.cursor];
        self.cursor += 1;
        idx
    }
}

/// replay 形式と hcpe3 policy に固有の制約を検証する。
///
/// Pack/hcpe3 は hcp から各手を replay して局面を再構成するため手列が連続している必要がある。
/// 中間局面を間引く `--skip-in-check` は replay を壊すので拒否する（序盤 prefix の
/// `--skip-initial-ply` は連続性を保つため許可）。hcpe3 では policy のパラメータも検証する。
fn validate_training_format_opts(
    format: TrainingFormat,
    skip_in_check: bool,
    policy_total: u16,
    policy_temp: f64,
) -> Result<()> {
    if matches!(format, TrainingFormat::Pack | TrainingFormat::Hcpe3) && skip_in_check {
        bail!(
            "pack and hcpe3 formats do not support --skip-in-check (skipping mid-game positions breaks move replay)"
        );
    }
    if format != TrainingFormat::Hcpe3 {
        return Ok(());
    }
    if policy_total == 0 {
        bail!("--hcpe3-policy-total must be >= 1");
    }
    if !policy_temp.is_finite() {
        bail!("--hcpe3-policy-temp must be finite");
    }
    Ok(())
}

fn validate_cli(cli: &Cli) -> Result<()> {
    if cli.concurrency == 0 {
        bail!("--concurrency must be >= 1");
    }
    if cli.random_multi_pv.unwrap_or(0) > 1 && cli.random_multi_pv_diff.is_none() {
        bail!("--random-multi-pv-diff is required when --random-multi-pv is greater than 1");
    }
    if cli.random_multi_pv_diff.is_some_and(|diff| diff < 0) {
        bail!("--random-multi-pv-diff must be >= 0");
    }
    Ok(())
}

fn entering_king_rule_from_options(options: &[String]) -> Result<EnteringKingRule> {
    let mut rule = EnteringKingRule::default();
    for option in options {
        let Some((name, value)) = option.split_once('=') else {
            continue;
        };
        if name.trim() != "EnteringKingRule" {
            continue;
        }
        rule = EnteringKingRule::from_usi(value.trim())
            .ok_or_else(|| anyhow!("unknown EnteringKingRule value: {}", value.trim()))?;
    }
    Ok(rule)
}

fn has_entering_king_rule_option(options: &[String]) -> bool {
    options.iter().any(|option| {
        option
            .split_once('=')
            .is_some_and(|(name, _)| name.trim() == "EnteringKingRule")
    })
}

fn has_explicit_usi_model_option(options: &[String]) -> bool {
    options.iter().any(|option| {
        option.split_once('=').is_some_and(|(name, value)| {
            let name = name.trim().to_ascii_lowercase();
            !value.trim().is_empty()
                && (name.contains("evalfile")
                    || name.contains("evaldir")
                    || name.contains("nnue")
                    || name.contains("modelfile")
                    || name.contains("modeldir"))
        })
    })
}

fn winner_for_side(side: Color) -> GameOutcome {
    match side {
        Color::Black => GameOutcome::BlackWin,
        Color::White => GameOutcome::WhiteWin,
    }
}

#[derive(Debug, Clone, Copy)]
struct PlayedMoveHistory {
    mover: Color,
    gives_check: bool,
}

struct GameRepetitionHistory {
    position_keys: Vec<u64>,
    played_moves: Vec<PlayedMoveHistory>,
}

impl GameRepetitionHistory {
    fn new(pos: &Position) -> Self {
        Self {
            position_keys: vec![pos.key()],
            played_moves: Vec::new(),
        }
    }

    fn record_move(
        &mut self,
        pos: &Position,
        mover: Color,
        gives_check: bool,
    ) -> Option<(GameOutcome, OutcomeReason)> {
        self.played_moves.push(PlayedMoveHistory { mover, gives_check });
        self.position_keys.push(pos.key());

        let current_index = self.position_keys.len() - 1;
        let current_key = self.position_keys[current_index];
        let first_of_four = self.position_keys[..current_index]
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, key)| **key == current_key)
            .nth(2)
            .map(|(index, _)| index)?;

        let continuously_checks = |side| {
            let mut moves =
                self.played_moves[first_of_four..].iter().filter(|played| played.mover == side);
            moves
                .next()
                .is_some_and(|played| played.gives_check && moves.all(|played| played.gives_check))
        };
        let side_to_move = pos.side_to_move();

        // 両者が連続王手となる局面でも core と同じく現在手番側の負けを優先する。
        if continuously_checks(side_to_move) {
            Some((winner_for_side(side_to_move.opponent()), OutcomeReason::PerpetualCheck))
        } else if continuously_checks(side_to_move.opponent()) {
            Some((winner_for_side(side_to_move), OutcomeReason::PerpetualCheck))
        } else {
            Some((GameOutcome::Draw, OutcomeReason::Sennichite))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeclarationWinAction {
    Unavailable,
    PseudoWin,
    PlayMove(Move),
}

fn declaration_win_action(pos: &Position, rule: EnteringKingRule) -> DeclarationWinAction {
    let action = pos.declaration_win(rule);
    if action == Move::NONE {
        DeclarationWinAction::Unavailable
    } else if action == Move::WIN {
        DeclarationWinAction::PseudoWin
    } else {
        DeclarationWinAction::PlayMove(action)
    }
}

fn is_valid_bestmove_win(pos: &Position, rule: EnteringKingRule) -> bool {
    declaration_win_action(pos, rule) == DeclarationWinAction::PseudoWin
}

fn is_try_rule_win_move(pos: &Position, rule: EnteringKingRule, played_move: Move) -> bool {
    if rule != EnteringKingRule::TryRule {
        return false;
    }
    matches!(
        declaration_win_action(pos, rule),
        DeclarationWinAction::PlayMove(expected) if expected.raw() == played_move.raw()
    )
}

/// 詰みスコア（手番側視点・手数）を hcpe3 eval の 32000-ply 符号化へ。
/// `|eval| >= 30001` に収め、学習器が詰み帯（`|eval| >= 30000`）を勝率回帰から
/// 除外する規約と整合させる。
fn mate_to_eval(score_mate: i32) -> i16 {
    if score_mate >= 0 {
        (32000 - score_mate).clamp(30001, 32767) as i16
    } else {
        (-32000 - score_mate).clamp(-32767, -30001) as i16
    }
}

fn candidate_eval(candidate: &MultiPvCandidate) -> i32 {
    candidate.score_mate.map_or(candidate.score_cp, |mate| {
        // native backend の mate_ply は負け詰みでも手数が非負になり得るため、
        // 勝敗符号が残る score_cp も使って詰み評価値の符号を復元する。
        let signed_mate = if mate < 0 || candidate.score_cp < 0 {
            -mate.abs()
        } else {
            mate.abs()
        };
        i32::from(mate_to_eval(signed_mate))
    })
}

/// MultiPV 候補を hcpe3 の policy 分布 `(move16, visit)` へ変換する。
///
/// 各候補に最低 1 票を与え、残りを温度 `temp` の softmax と largest-remainder 法で
/// 厳密配分する。決定性のため候補と同端数の配分順は multipv 昇順にする。
fn multipv_to_policy(
    candidates: &[MultiPvCandidate],
    played_move: Move,
    total: u16,
    temp: f64,
    multi_pv: u32,
    eval_drop_threshold: i32,
) -> Vec<(u16, u16)> {
    let mut sorted = candidates.to_vec();
    sorted.sort_by_key(|c| c.multipv);

    let mut seen_move16 = Vec::with_capacity(sorted.len());
    sorted.retain(|c| {
        let move16 = move_to_psv_move16(c.first_move);
        if seen_move16.contains(&move16) {
            false
        } else {
            seen_move16.push(move16);
            true
        }
    });

    let selected_move16 = move_to_psv_move16(played_move);
    if !seen_move16.contains(&selected_move16) {
        let (score_cp, score_mate) =
            sorted.first().map_or((0, None), |c| (c.score_cp, c.score_mate));
        sorted.insert(
            0,
            MultiPvCandidate {
                // 通常の MultiPV は 1-indexed。0 なら合成候補を常に先頭に固定できる。
                multipv: 0,
                score_cp,
                score_mate,
                first_move: played_move,
            },
        );
    }

    // 合成候補は先頭に挿入済みなので、末尾を切っても実着手は必ず残る。
    sorted.truncate(multi_pv.clamp(1, MAX_HCPE3_POLICY_CANDIDATES) as usize);

    if eval_drop_threshold >= 0 && !sorted.is_empty() {
        let best_score = sorted.iter().map(candidate_eval).max().unwrap_or(i32::MIN);
        sorted.retain(|c| {
            best_score.saturating_sub(candidate_eval(c)) <= eval_drop_threshold
                || move_to_psv_move16(c.first_move) == selected_move16
        });
    }

    let total = u32::from(total).max(sorted.len() as u32).min(u32::from(u16::MAX));
    let mut visits = vec![1u32; sorted.len()];
    let remaining = total - sorted.len() as u32;
    if remaining == 0 {
        return sorted.iter().map(|c| (move_to_psv_move16(c.first_move), 1)).collect();
    }

    if temp <= 0.0 {
        let best_score = sorted.iter().map(candidate_eval).max().unwrap_or(i32::MIN);
        let best = sorted.iter().position(|c| candidate_eval(c) == best_score).unwrap_or(0);
        visits[best] += remaining;
        return sorted
            .iter()
            .zip(visits)
            .map(|(c, visit)| (move_to_psv_move16(c.first_move), visit as u16))
            .collect();
    }

    let max_s = sorted
        .iter()
        .map(|c| f64::from(candidate_eval(c)))
        .fold(f64::NEG_INFINITY, f64::max);
    let weights: Vec<f64> = sorted
        .iter()
        .map(|c| ((f64::from(candidate_eval(c)) - max_s) / temp).clamp(-700.0, 700.0).exp())
        .collect();
    let sum: f64 = weights.iter().sum();

    let ideals: Vec<f64> = weights.iter().map(|w| w / sum * f64::from(remaining)).collect();
    for (visit, ideal) in visits.iter_mut().zip(&ideals) {
        *visit += ideal.floor() as u32;
    }
    let mut leftover = total - visits.iter().sum::<u32>();
    let mut order: Vec<usize> = (0..sorted.len()).collect();
    order.sort_by(|&a, &b| {
        let ra = ideals[a] - ideals[a].floor();
        let rb = ideals[b] - ideals[b].floor();
        rb.partial_cmp(&ra).unwrap_or(std::cmp::Ordering::Equal).then(a.cmp(&b))
    });
    for &i in &order {
        if leftover == 0 {
            break;
        }
        visits[i] += 1;
        leftover -= 1;
    }

    sorted
        .iter()
        .zip(visits)
        .map(|(c, visit)| (move_to_psv_move16(c.first_move), visit as u16))
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SelectedMultiPvMove {
    mv: Move,
    score_gap_cp: i32,
}

/// MultiPV 候補からランダムに1手を選択する
///
/// PV1 のスコアとの差が `diff_threshold` 以内の候補からランダムに選択する。
/// 候補がない場合は None を返す。
fn select_multipv_random(
    candidates: &[tools::selfplay::MultiPvCandidate],
    diff_threshold: i32,
    rng: &mut impl Rng,
) -> Option<SelectedMultiPvMove> {
    if candidates.is_empty() {
        return None;
    }
    let best = candidates.iter().find(|c| c.multipv == 1)?;
    let best_score = best.score_cp;
    let eligible: Vec<_> = candidates
        .iter()
        .filter(|c| (best_score - c.score_cp).abs() <= diff_threshold)
        .collect();
    debug_assert!(!eligible.is_empty(), "eligible must contain at least PV1 (diff_threshold >= 0)");
    let selected = eligible[rng.random_range(0..eligible.len())];
    Some(SelectedMultiPvMove {
        mv: selected.first_move,
        score_gap_cp: best_score - selected.score_cp,
    })
}

/// 指定範囲から N 個の手数をサンプリングする（重複なし）
fn sample_random_move_plies(
    min_ply: u32,
    max_ply: u32,
    count: u32,
    rng: &mut impl Rng,
) -> std::collections::HashSet<u32> {
    use std::collections::HashSet;
    let range_size = max_ply.saturating_sub(min_ply) + 1;
    let count = count.min(range_size);
    let mut plies = HashSet::with_capacity(count as usize);
    if range_size <= count * 2 {
        // 範囲が小さい場合はシャッフルしてから先頭 N 個
        let mut all: Vec<u32> = (min_ply..=max_ply).collect();
        all.shuffle(rng);
        for &p in all.iter().take(count as usize) {
            plies.insert(p);
        }
    } else {
        while plies.len() < count as usize {
            plies.insert(rng.random_range(min_ply..=max_ply));
        }
    }
    plies
}

#[derive(Serialize)]
struct InfoLogEntry<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    game_id: u32,
    ply: u32,
    side_to_move: char,
    engine: &'a str,
    line: &'a str,
}

struct InfoLogger {
    writer: BufWriter<File>,
}

impl InfoLogger {
    fn new(path: &Path, append: bool) -> Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create info-log directory {}", parent.display())
            })?;
        }
        let file = open_worker_checkpoint(path, append)
            .with_context(|| format!("failed to open info log {}", path.display()))?;
        Ok(Self {
            writer: BufWriter::new(file),
        })
    }

    fn log(&mut self, entry: InfoLogEntry<'_>) -> Result<()> {
        serde_json::to_writer(&mut self.writer, &entry)?;
        self.writer.write_all(b"\n")?;
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        self.writer.flush()?;
        Ok(())
    }

    fn committed_len(&mut self, sync: bool) -> Result<u64> {
        self.writer.flush()?;
        if sync {
            self.writer.get_ref().sync_all()?;
        }
        Ok(self.writer.get_ref().metadata()?.len())
    }
}

fn committed_writer_len(writer: &mut BufWriter<File>, sync: bool) -> Result<u64> {
    writer.flush()?;
    if sync {
        writer.get_ref().sync_all()?;
    }
    Ok(writer.get_ref().metadata()?.len())
}

struct FaultSpec {
    point: String,
    occurrence: Option<u64>,
}

static FAULT_SPEC: OnceLock<Option<FaultSpec>> = OnceLock::new();
static FAULT_MATCHES: AtomicU64 = AtomicU64::new(0);

fn fault_spec() -> Option<&'static FaultSpec> {
    FAULT_SPEC
        .get_or_init(|| {
            let value = std::env::var("RSHOGI_GENSFEN_FAULT").ok()?;
            let (point, occurrence) = match value.rsplit_once(':') {
                Some((point, occurrence)) => {
                    let occurrence = occurrence.parse::<u64>().ok().filter(|value| *value > 0)?;
                    (point.to_string(), Some(occurrence))
                }
                None => (value, None),
            };
            Some(FaultSpec { point, occurrence })
        })
        .as_ref()
}

fn injected_fault(point: &str) -> bool {
    let Some(spec) = fault_spec().filter(|spec| spec.point == point) else {
        return false;
    };
    match spec.occurrence {
        Some(target) => FAULT_MATCHES.fetch_add(1, Ordering::Relaxed) + 1 == target,
        None => true,
    }
}

// ---------------------------------------------------------------------------
// Concurrency support
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct GameTicket {
    game_idx: u32,
    startpos_idx: usize,
}

fn make_game_ticket<R: Rng + ?Sized>(
    game_idx: u32,
    random_startpos: bool,
    startpos_count: usize,
    rng: &mut R,
) -> GameTicket {
    let startpos_idx = if random_startpos {
        rng.random_range(0..startpos_count)
    } else {
        (game_idx as usize) % startpos_count
    };
    GameTicket {
        game_idx,
        startpos_idx,
    }
}

struct WorkerGameResult {
    game_id: u32,
    outcome: GameOutcome,
    outcome_reason: OutcomeReason,
}

struct WorkerOutput {
    training_stats: TrainingStats,
}

struct WorkerConfig {
    worker_id: usize,
    // Engine
    engine_path_black: PathBuf,
    engine_path_white: PathBuf,
    black_args: Vec<String>,
    white_args: Vec<String>,
    threads_black: usize,
    threads_white: usize,
    hash_mb: u32,
    network_delay: Option<i64>,
    network_delay2: Option<i64>,
    minimum_thinking_time: Option<i64>,
    slowmover: Option<i32>,
    ponder: bool,
    black_usi_opts: Vec<String>,
    white_usi_opts: Vec<String>,
    entering_king_rule_black: EnteringKingRule,
    entering_king_rule_white: EnteringKingRule,
    // Game
    max_moves: u32,
    timeout_margin_ms: u64,
    btime: u64,
    wtime: u64,
    binc: u64,
    winc: u64,
    byoyomi: u64,
    // Depth/nodes limits
    go_depth: Option<u32>,
    go_nodes: Option<u64>,
    // Positions (shared across workers)
    start_defs: Arc<Vec<ParsedPosition>>,
    start_commands: Arc<Vec<String>>,
    // Output (temp paths)
    jsonl_path: PathBuf,
    info_path: Option<PathBuf>,
    eval_path: Option<PathBuf>,
    metrics_path: Option<PathBuf>,
    training_data_path: Option<PathBuf>,
    game_id_sidecar_path: Option<PathBuf>,
    // Output flags
    flush_each_move: bool,
    fsync_interval_games: u32,
    append_checkpoints: bool,
    run_seed: u64,
    // Training
    skip_initial_ply: u32,
    skip_in_check: bool,
    training_format: TrainingFormat,
    hcpe3_policy_total: u16,
    hcpe3_policy_temp: f64,
    hcpe3_eval_drop_threshold: i32,
    // gensfen: NativeBackend モード
    native_mode: bool,
    /// USI 単一エンジン最適化（先後同一エンジン時に 1 プロセスで兼用）。
    /// TT/履歴が先後で共有されるため、Elo 評価に影響する。
    /// --for-train 時のみ有効（棋力評価用途では使用しない）。
    usi_single: bool,
    eval_hash_size_mb: usize,
    progress_routing_buckets: Option<usize>,
    // gensfen: 重複回避
    keep_tt: bool,
    dedup_hash: Option<Arc<SharedDedupHash>>,
    random_multi_pv: u32,
    random_multi_pv_diff: i32,
    random_move_count: u32,
    random_move_min_ply: u32,
    random_move_max_ply: u32,
    /// ワーカーあたりの dedup rate チェック間隔（interval / concurrency で調整済み）
    dedup_warn_interval_per_worker: u32,
    dedup_warn_rate: f64,
    /// 直近 interval で既に警告済みかを示すフラグ（全ワーカー共有）。
    /// 同一タイミングで複数ワーカーが重複警告を出すのを抑制する。
    dedup_warn_emitted: Arc<AtomicBool>,
    omit_diversions: bool,
}

fn worker_main(
    cfg: WorkerConfig,
    rx: chan::Receiver<Option<GameTicket>>,
    tx: chan::Sender<WorkerGameResult>,
    shutdown: Arc<AtomicBool>,
) -> Result<WorkerOutput> {
    let run = || -> Result<WorkerOutput> {
        // Create game engines (NativeBackend or UsiBackend)
        let mut engines = if cfg.native_mode {
            GameEngines::Native(Box::new(NativeBackend::new(
                cfg.hash_mb as usize,
                cfg.eval_hash_size_mb,
            )))
        } else {
            let spawn_usi = |side: &str, args: &[String], usi_opts: &[String], threads: usize| {
                let mut engine = EngineProcess::spawn(
                    &EngineConfig {
                        path: if side == "black" {
                            cfg.engine_path_black.clone()
                        } else {
                            cfg.engine_path_white.clone()
                        },
                        args: args.to_vec(),
                        threads,
                        hash_mb: cfg.hash_mb,
                        network_delay: cfg.network_delay,
                        network_delay2: cfg.network_delay2,
                        minimum_thinking_time: cfg.minimum_thinking_time,
                        slowmover: cfg.slowmover,
                        ponder: cfg.ponder,
                        usi_options: usi_opts.to_vec(),
                    },
                    format!("w{}-{}", cfg.worker_id, side),
                )?;
                // MultiPV 設定
                if cfg.random_multi_pv > 1 {
                    engine.set_option_if_available("MultiPV", &cfg.random_multi_pv.to_string())?;
                }
                Ok::<_, anyhow::Error>(engine)
            };

            // 先後同一エンジンかつ usi_single が有効なら 1 プロセスで兼用。
            // TT/履歴が先後で共有されるため、--for-train 以外では無効。
            if cfg.usi_single {
                let engine =
                    spawn_usi("single", &cfg.black_args, &cfg.black_usi_opts, cfg.threads_black)?;
                GameEngines::UsiSingle(Box::new(UsiBackend::new(engine)))
            } else {
                let black =
                    spawn_usi("black", &cfg.black_args, &cfg.black_usi_opts, cfg.threads_black)?;
                let white =
                    spawn_usi("white", &cfg.white_args, &cfg.white_usi_opts, cfg.threads_white)?;
                GameEngines::Usi(Box::new(tools::selfplay::UsiEngines {
                    black: UsiBackend::new(black),
                    white: UsiBackend::new(white),
                }))
            }
        };

        // Open temp output files
        let mut writer = BufWriter::new(
            open_worker_checkpoint(&cfg.jsonl_path, cfg.append_checkpoints).with_context(|| {
                format!("worker {}: failed to open {}", cfg.worker_id, cfg.jsonl_path.display())
            })?,
        );
        let mut info_logger = if let Some(ref path) = cfg.info_path {
            Some(InfoLogger::new(path, cfg.append_checkpoints)?)
        } else {
            None
        };
        let mut eval_writer = if let Some(ref path) = cfg.eval_path {
            Some(BufWriter::new(open_checkpoint(path, cfg.append_checkpoints)?))
        } else {
            None
        };
        let mut metrics_writer = if let Some(ref path) = cfg.metrics_path {
            Some(BufWriter::new(open_checkpoint(path, cfg.append_checkpoints)?))
        } else {
            None
        };
        let mut training_data_collector = if let Some(ref path) = cfg.training_data_path {
            Some(TrainingDataCollector::open(
                path,
                cfg.skip_initial_ply,
                cfg.skip_in_check,
                cfg.training_format,
                Hcpe3PolicyConfig {
                    total: cfg.hcpe3_policy_total,
                    temp: cfg.hcpe3_policy_temp,
                    multi_pv: cfg.random_multi_pv,
                    eval_drop_threshold: cfg.hcpe3_eval_drop_threshold,
                },
                cfg.max_moves,
                cfg.game_id_sidecar_path.as_deref(),
                cfg.append_checkpoints,
            )?)
        } else {
            None
        };
        for path in [
            Some(cfg.jsonl_path.as_path()),
            cfg.info_path.as_deref(),
            cfg.eval_path.as_deref(),
            cfg.metrics_path.as_deref(),
            cfg.training_data_path.as_deref(),
            cfg.game_id_sidecar_path.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            sync_parent(path)?;
        }

        let dedup_hash = cfg.dedup_hash.clone();
        let mut dedup_hits = 0u64;
        let mut dedup_discarded = 0u64;
        let mut multipv_diversions = 0u64;
        let mut random_moves_played = 0u64;
        // dedup rate 監視用（interval ごとにリセット）
        let mut interval_games = 0u32;
        let mut interval_dedup_hits = 0u64;
        let mut interval_positions_checked = 0u64;
        let mut committed_games = 0u32;
        let progress_weights =
            cfg.progress_routing_buckets.map(|_| get_layer_stack_progress_kpabs_weights());
        let mut progress_bucket_counts = cfg
            .progress_routing_buckets
            .map(|num_buckets| vec![0u64; num_buckets])
            .unwrap_or_default();

        // Game loop
        while let Ok(Some(ticket)) = rx.recv() {
            if shutdown.load(Ordering::Relaxed) {
                break;
            }

            let game_idx = ticket.game_idx;
            use rand::SeedableRng;
            let mut rng = rand::rngs::StdRng::seed_from_u64(
                cfg.run_seed ^ u64::from(game_idx + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15),
            );
            engines.prepare_game(cfg.keep_tt)?;

            let parsed = &cfg.start_defs[ticket.startpos_idx];
            let mut pos = build_position(parsed, None, None)?;
            let mut repetition_history = GameRepetitionHistory::new(&pos);
            let start_sfen = pos.to_sfen();
            let start_pos_index = parsed.source_line.unwrap_or(ticket.startpos_idx + 1);
            let mut tc = TimeControl::new(cfg.btime, cfg.wtime, cfg.binc, cfg.winc, cfg.byoyomi);
            let mut outcome = GameOutcome::InProgress;
            let mut outcome_reason = OutcomeReason::MaxMoves;
            let mut plies_played = 0u32;
            let mut move_list: Vec<String> = Vec::new();
            let mut eval_list: Vec<String> = Vec::new();
            let mut diversions: Vec<DiversionLog> = Vec::new();
            let mut metrics = MetricsCollector::default();
            let mut pending_dedup_keys = PendingDedupKeys::default();

            if let Some(ref mut collector) = training_data_collector {
                collector.start_game();
            }

            // gensfen: ランダムムーブ対象手数を決定
            let random_move_plies = if cfg.random_move_count > 0 {
                sample_random_move_plies(
                    cfg.random_move_min_ply,
                    cfg.random_move_max_ply,
                    cfg.random_move_count,
                    &mut rng,
                )
            } else {
                std::collections::HashSet::new()
            };

            for ply_idx in 0..cfg.max_moves {
                plies_played = ply_idx + 1;
                let side = pos.side_to_move();
                let entering_king_rule = if side == Color::Black {
                    cfg.entering_king_rule_black
                } else {
                    cfg.entering_king_rule_white
                };
                let engine_label = if side == Color::Black {
                    "black"
                } else {
                    "white"
                };
                if let (Some(weights), Some(num_buckets)) =
                    (progress_weights, cfg.progress_routing_buckets)
                {
                    let bucket = compute_layer_stack_progresskpabs_bucket_index(
                        &pos,
                        side,
                        weights,
                        num_buckets,
                    );
                    progress_bucket_counts[bucket] += 1;
                }
                let sfen_before = pos.to_sfen();

                // --- gensfen: ランダムムーブ ---
                if random_move_plies.contains(&plies_played) {
                    let mut legal_moves = MoveList::new();
                    generate_legal(&pos, &mut legal_moves);
                    if legal_moves.is_empty() {
                        outcome = if side == Color::Black {
                            GameOutcome::WhiteWin
                        } else {
                            GameOutcome::BlackWin
                        };
                        outcome_reason = OutcomeReason::Mate;
                        break;
                    }
                    let mv = legal_moves[rng.random_range(0..legal_moves.len())];
                    let rm_usi = mv.to_usi();
                    diversions.push(DiversionLog {
                        ply: plies_played,
                        kind: "random",
                        chosen_move: rm_usi.clone(),
                        best_move: None,
                        score_gap_cp: None,
                    });
                    // ランダムムーブ前のエントリをクリア（tanuki- 方式）
                    if let Some(ref mut collector) = training_data_collector {
                        collector.start_game();
                    }
                    random_moves_played += 1;
                    let gives_check = if mv.is_pass() {
                        false
                    } else {
                        pos.gives_check(mv)
                    };
                    let try_rule_win = is_try_rule_win_move(&pos, entering_king_rule, mv);
                    pos.do_move(mv, gives_check);
                    if try_rule_win {
                        outcome = winner_for_side(side);
                        outcome_reason = OutcomeReason::Win;
                    } else if let Some((repetition_result, reason)) =
                        repetition_history.record_move(&pos, side, gives_check)
                    {
                        outcome = repetition_result;
                        outcome_reason = reason;
                    }
                    if eval_writer.is_some() {
                        eval_list.push("R".to_string());
                        move_list.push(rm_usi.clone());
                    }
                    if outcome != GameOutcome::InProgress {
                        break;
                    }
                    continue;
                }

                // --- 通常探索 ---
                let think_limit_ms = tc.think_limit_ms(side);
                let params = SearchParams {
                    sfen: sfen_before.clone(),
                    time_args: tc.time_args(),
                    think_limit_ms,
                    timeout_margin_ms: cfg.timeout_margin_ms,
                    go_depth: cfg.go_depth,
                    go_nodes: cfg.go_nodes,
                    multi_pv: cfg.random_multi_pv.max(1),
                    pass_rights: None,
                    side,
                    game_id: game_idx + 1,
                    ply: plies_played,
                    collect_info_lines: info_logger.is_some(),
                };
                let search = engines.search(side, &pos, &params)?;

                // info ログ
                if let Some(ref mut logger) = info_logger {
                    for line in &search.info_lines {
                        let _ = logger.log(InfoLogEntry {
                            kind: "info",
                            game_id: game_idx + 1,
                            ply: plies_played,
                            side_to_move: side_label(side),
                            engine: engine_label,
                            line,
                        });
                    }
                }

                let timed_out = search.timed_out;
                let mut move_usi =
                    search.best_move_usi.clone().unwrap_or_else(|| "none".to_string());
                let mut terminal = false;
                let eval_log = search.eval.clone();

                if timed_out {
                    outcome = if side == Color::Black {
                        GameOutcome::WhiteWin
                    } else {
                        GameOutcome::BlackWin
                    };
                    outcome_reason = OutcomeReason::Timeout;
                    terminal = true;
                    if search.best_move_usi.is_none() {
                        move_usi = "timeout".to_string();
                    }
                } else if let Some(ref mv_str) = search.best_move_usi
                    && mv_str != "none"
                {
                    match mv_str.as_str() {
                        "resign" => {
                            move_usi = mv_str.clone();
                            outcome = if side == Color::Black {
                                GameOutcome::WhiteWin
                            } else {
                                GameOutcome::BlackWin
                            };
                            outcome_reason = OutcomeReason::Resign;
                            terminal = true;
                        }
                        "win" => {
                            move_usi = mv_str.clone();
                            if is_valid_bestmove_win(&pos, entering_king_rule) {
                                let skip_record = check_declaration_win_position_dedup(
                                    cfg.training_format,
                                    dedup_hash.as_deref(),
                                    &mut pending_dedup_keys,
                                    pos.key(),
                                    training_data_collector.as_mut(),
                                    &mut dedup_hits,
                                    &mut interval_dedup_hits,
                                    &mut interval_positions_checked,
                                );
                                if !skip_record
                                    && let Some(ref mut collector) = training_data_collector
                                {
                                    collector.record_declaration_win_position(&pos);
                                }
                                outcome = if side == Color::Black {
                                    GameOutcome::BlackWin
                                } else {
                                    GameOutcome::WhiteWin
                                };
                                outcome_reason = OutcomeReason::Win;
                            } else {
                                outcome = if side == Color::Black {
                                    GameOutcome::WhiteWin
                                } else {
                                    GameOutcome::BlackWin
                                };
                                outcome_reason = OutcomeReason::IllegalMove;
                                move_usi = "illegal".to_string();
                            }
                            terminal = true;
                        }
                        _ => {
                            // バックエンドがパース済み Move を返す場合はそれを使う
                            let mv_opt = search
                                .best_move
                                .filter(|mv| is_legal_with_pass(&pos, *mv))
                                .or_else(|| {
                                    Move::from_usi(mv_str)
                                        .filter(|mv| is_legal_with_pass(&pos, *mv))
                                });
                            match mv_opt {
                                Some(mv) => {
                                    // --- gensfen: ハッシュ重複検出 ---
                                    // 全ワーカーで共有するテーブルで重複チェック（tanuki-と同じ構成）
                                    let skip_record = check_training_position_dedup(
                                        dedup_hash.as_deref(),
                                        &mut pending_dedup_keys,
                                        pos.key(),
                                        training_data_collector.as_mut(),
                                        &mut dedup_hits,
                                        &mut dedup_discarded,
                                        &mut interval_dedup_hits,
                                        &mut interval_positions_checked,
                                    );

                                    // --- gensfen: MultiPV ランダム選択 ---
                                    let played_mv = if cfg.random_multi_pv > 1 {
                                        if let Some(selected) = select_multipv_random(
                                            &search.multipv_candidates,
                                            cfg.random_multi_pv_diff,
                                            &mut rng,
                                        ) {
                                            if selected.mv != mv {
                                                multipv_diversions += 1;
                                                diversions.push(DiversionLog {
                                                    ply: plies_played,
                                                    kind: "multipv",
                                                    chosen_move: selected.mv.to_usi(),
                                                    best_move: Some(mv.to_usi()),
                                                    score_gap_cp: Some(selected.score_gap_cp),
                                                });
                                            }
                                            selected.mv
                                        } else {
                                            mv
                                        }
                                    } else {
                                        mv
                                    };

                                    // PSV は最善手 PV1 をラベルにする。Pack/hcpe3 は replay 整合のため
                                    // played_mv を実着手として記録する（hcpe3 policy は MultiPV 候補）。
                                    if !skip_record
                                        && let Some(ref mut collector) = training_data_collector
                                    {
                                        collector.record_position(
                                            &pos,
                                            eval_log.as_ref().and_then(|e| e.score_cp),
                                            eval_log.as_ref().and_then(|e| e.score_mate),
                                            Some(mv),
                                            played_mv,
                                            &search.multipv_candidates,
                                        );
                                    }

                                    let gives_check = if played_mv.is_pass() {
                                        false
                                    } else {
                                        pos.gives_check(played_mv)
                                    };
                                    let try_rule_win =
                                        is_try_rule_win_move(&pos, entering_king_rule, played_mv);
                                    pos.do_move(played_mv, gives_check);
                                    tc.update_after_move(side, search.elapsed_ms);
                                    move_usi = played_mv.to_usi();
                                    if try_rule_win {
                                        outcome = winner_for_side(side);
                                        outcome_reason = OutcomeReason::Win;
                                        terminal = true;
                                    } else if let Some((repetition_result, reason)) =
                                        repetition_history.record_move(&pos, side, gives_check)
                                    {
                                        outcome = repetition_result;
                                        outcome_reason = reason;
                                        terminal = true;
                                    }
                                }
                                None => {
                                    outcome = if side == Color::Black {
                                        GameOutcome::WhiteWin
                                    } else {
                                        GameOutcome::BlackWin
                                    };
                                    outcome_reason = OutcomeReason::IllegalMove;
                                    terminal = true;
                                    move_usi = "illegal".to_string();
                                }
                            }
                        }
                    }
                } else {
                    outcome = if side == Color::Black {
                        GameOutcome::WhiteWin
                    } else {
                        GameOutcome::BlackWin
                    };
                    // 合法手ゼロなら mate、それ以外は no_bestmove
                    let mut legal_moves = MoveList::new();
                    generate_legal(&pos, &mut legal_moves);
                    outcome_reason = if legal_moves.is_empty() {
                        OutcomeReason::Mate
                    } else {
                        OutcomeReason::NoBestmove
                    };
                    terminal = true;
                }

                if eval_writer.is_some() {
                    eval_list.push(eval_label(eval_log.as_ref()));
                    move_list.push(move_usi.clone());
                }

                if metrics_writer.is_some() {
                    metrics.update(side, eval_log.as_ref(), plies_played);
                }

                if cfg.flush_each_move {
                    writer.flush()?;
                    if let Some(logger) = info_logger.as_mut() {
                        logger.flush()?;
                    }
                }

                if terminal || outcome != GameOutcome::InProgress {
                    break;
                }
            }

            if outcome == GameOutcome::InProgress {
                outcome = GameOutcome::Draw;
                outcome_reason = OutcomeReason::MaxMoves;
            }
            let training_disposition = TrainingDisposition::from_outcome_reason(outcome_reason);
            let final_meta = final_entering_king_meta(&pos);
            let result = ResultLog {
                kind: "result",
                worker_id: cfg.worker_id,
                game_id: game_idx + 1,
                start_pos_index,
                start_sfen: &start_sfen,
                outcome: outcome.label(),
                reason: outcome_reason,
                adopted: training_disposition.is_adopted(),
                plies: plies_played,
                final_points_black: final_meta.black.points,
                final_points_white: final_meta.white.points,
                king_in_enemy_black: final_meta.black.king_in_enemy,
                king_in_enemy_white: final_meta.white.king_in_enemy,
                enemy_zone_pieces_black: final_meta.black.enemy_zone_pieces,
                enemy_zone_pieces_white: final_meta.white.enemy_zone_pieces,
                diversions: (!cfg.omit_diversions).then_some(diversions.as_slice()),
                multipv_diversions: cfg
                    .omit_diversions
                    .then(|| diversions.iter().filter(|d| d.kind == "multipv").count() as u64),
                random_moves: cfg
                    .omit_diversions
                    .then(|| diversions.iter().filter(|d| d.kind == "random").count() as u64),
                diversions_total: cfg.omit_diversions.then_some(diversions.len() as u64),
                training_bytes: 0,
                sidecar_bytes: None,
                info_bytes: None,
                eval_bytes: None,
                metrics_bytes: None,
                fsync_boundary: false,
            };
            if let Some(w) = eval_writer.as_mut() {
                let start_cmd = &cfg.start_commands[ticket.startpos_idx];
                let moves_text = if move_list.is_empty() {
                    String::new()
                } else {
                    format!(" moves {}", move_list.join(" "))
                };
                writeln!(w, "game {}: {}{}", game_idx + 1, start_cmd, moves_text)?;
                if !eval_list.is_empty() {
                    writeln!(w, "eval {}", eval_list.join(" "))?;
                } else {
                    writeln!(w, "eval")?;
                }
                writeln!(w)?;
            }

            if let Some(w) = metrics_writer.as_mut() {
                let metrics_log = MetricsLog {
                    kind: "metrics",
                    game_id: game_idx + 1,
                    plies: plies_played,
                    nodes_black: metrics.nodes_black,
                    nodes_white: metrics.nodes_white,
                    nodes_first60: metrics.nodes_first60,
                    last_cp_black: metrics.last_cp_black,
                    last_cp_white: metrics.last_cp_white,
                    last_mate_black: metrics.last_mate_black,
                    last_mate_white: metrics.last_mate_white,
                    outcome: outcome.label().to_string(),
                    reason: outcome_reason,
                };
                serde_json::to_writer(&mut *w, &metrics_log)?;
                w.write_all(b"\n")?;
            }

            if let Some(ref mut collector) = training_data_collector {
                collector.finish_game_with_reason(
                    outcome,
                    outcome_reason,
                    training_disposition,
                    game_idx + 1,
                )?;
            }
            if training_disposition.is_adopted()
                && let Some(dedup_hash) = dedup_hash.as_deref()
            {
                pending_dedup_keys.publish(dedup_hash);
            }
            committed_games += 1;
            let sync_due = cfg.fsync_interval_games > 0
                && committed_games.is_multiple_of(cfg.fsync_interval_games);
            write_committed_result(&mut writer, result, sync_due, || {
                let (training, sidecar) = if let Some(collector) = training_data_collector.as_mut()
                {
                    if sync_due {
                        collector.sync_all()?;
                    }
                    collector.committed_lengths()?
                } else {
                    (0, None)
                };
                let info = info_logger
                    .as_mut()
                    .map(|logger| logger.committed_len(sync_due))
                    .transpose()?;
                let eval = eval_writer
                    .as_mut()
                    .map(|writer| committed_writer_len(writer, sync_due))
                    .transpose()?;
                let metrics = metrics_writer
                    .as_mut()
                    .map(|writer| committed_writer_len(writer, sync_due))
                    .transpose()?;
                Ok(CheckpointLengths {
                    training,
                    sidecar,
                    info,
                    eval,
                    metrics,
                })
            })?;
            writer.flush()?;

            if sync_due {
                writer.get_ref().sync_all()?;
            }

            let _ = tx.send(WorkerGameResult {
                game_id: game_idx + 1,
                outcome,
                outcome_reason,
            });

            // dedup rate 監視（dedup 有効時のみカウント・チェック）
            if dedup_hash.is_some() && cfg.dedup_warn_interval_per_worker > 0 {
                interval_games += 1;
                if interval_games >= cfg.dedup_warn_interval_per_worker {
                    if interval_positions_checked > 0 {
                        let rate = interval_dedup_hits as f64 / interval_positions_checked as f64;
                        if rate > cfg.dedup_warn_rate {
                            // 同一 interval で複数ワーカーが重複警告を出すのを抑制。
                            // compare_exchange で「まだ誰も出していなければ自分が出す」。
                            // Relaxed で十分（厳密な排他は不要、レースで 2-3 行出ても許容）。
                            if cfg
                                .dedup_warn_emitted
                                .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
                                .is_ok()
                            {
                                eprintln!(
                                    "warning: dedup rate {:.1}% in last ~{} games \
                                     ({} hits / {} checked, worker {}). \
                                     Consider increasing --random-multi-pv or adding --random-move-count",
                                    rate * 100.0,
                                    interval_games,
                                    interval_dedup_hits,
                                    interval_positions_checked,
                                    cfg.worker_id,
                                );
                            }
                        } else {
                            // rate が閾値以下に戻った: 次の interval で再度警告可能にする
                            cfg.dedup_warn_emitted.store(false, Ordering::Relaxed);
                        }
                    }
                    interval_games = 0;
                    interval_dedup_hits = 0;
                    interval_positions_checked = 0;
                }
            }
        }

        // Flush all temp files
        writer.flush()?;
        if let Some(logger) = info_logger.as_mut() {
            logger.flush()?;
        }
        if let Some(w) = eval_writer.as_mut() {
            w.flush()?;
        }
        if let Some(w) = metrics_writer.as_mut() {
            w.flush()?;
        }
        if cfg.fsync_interval_games > 0 {
            if let Some(collector) = training_data_collector.as_mut() {
                collector.sync_all()?;
            }
            writer.get_ref().sync_all()?;
        }

        // gensfen 統計
        if dedup_hits > 0 || random_moves_played > 0 || multipv_diversions > 0 {
            eprintln!(
                "worker {}: gensfen stats: dedup_hits={}, dedup_discarded={}, multipv_diversions={}, random_moves={}",
                cfg.worker_id, dedup_hits, dedup_discarded, multipv_diversions, random_moves_played
            );
        }
        if let Some(num_buckets) = cfg.progress_routing_buckets {
            let used = progress_bucket_counts.iter().filter(|&&count| count > 0).count();
            eprintln!(
                "worker {}: progress bucket distribution: {:?} (used {}/{})",
                cfg.worker_id, progress_bucket_counts, used, num_buckets
            );
        }

        let training_stats = if let Some(ref mut collector) = training_data_collector {
            collector.flush()?;
            collector.stats()
        } else {
            TrainingStats::default()
        };

        Ok(WorkerOutput { training_stats })
    };

    let output = run().with_context(|| format!("worker {} failed", cfg.worker_id));
    if output.is_err() {
        shutdown.store(true, Ordering::Relaxed);
    }
    output
}

fn open_checkpoint(path: &Path, append: bool) -> Result<File> {
    open_worker_checkpoint(path, append)
        .with_context(|| format!("failed to open checkpoint {}", path.display()))
}

// ---------------------------------------------------------------------------
// Resume support
// ---------------------------------------------------------------------------

/// 前回中断した教師局面生成セッションの進捗状態
#[derive(Clone, Debug, Default)]
struct CompletedGames {
    words: Vec<u64>,
    len: u32,
}

impl CompletedGames {
    fn insert(&mut self, game_id: u32) -> bool {
        if game_id == 0 {
            return false;
        }
        let index = (game_id - 1) as usize;
        let word = index / 64;
        if self.words.len() <= word {
            self.words.resize(word + 1, 0);
        }
        let mask = 1u64 << (index % 64);
        if self.words[word] & mask != 0 {
            return false;
        }
        self.words[word] |= mask;
        self.len += 1;
        true
    }

    fn contains(&self, game_id: u32) -> bool {
        if game_id == 0 {
            return false;
        }
        let index = (game_id - 1) as usize;
        self.words
            .get(index / 64)
            .is_some_and(|word| word & (1u64 << (index % 64)) != 0)
    }

    fn len(&self) -> u32 {
        self.len
    }

    fn max_id(&self) -> Option<u32> {
        self.words.iter().rposition(|word| *word != 0).map(|word_index| {
            let word = self.words[word_index];
            (word_index * 64 + (64 - word.leading_zeros() as usize)) as u32
        })
    }

    fn merge(&mut self, other: &Self) -> Result<()> {
        for (word_index, &word) in other.words.iter().enumerate() {
            let mut remaining = word;
            while remaining != 0 {
                let bit = remaining.trailing_zeros() as usize;
                let game_id = (word_index * 64 + bit + 1) as u32;
                if !self.insert(game_id) {
                    bail!("duplicate completed game_id {game_id} across resume checkpoints");
                }
                remaining &= remaining - 1;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct ResumeState {
    completed_games: CompletedGames,
    black_wins: u32,
    white_wins: u32,
    draws: u32,
    /// meta 行に保存された shuffle_seed（存在しない場合は None）
    shuffle_seed: Option<u64>,
    /// meta 行に保存された progress_file（存在しない場合は None）
    progress_file: Option<String>,
    /// meta 行に保存された progress_file 内容の SHA-256（存在しない場合は None）
    progress_file_sha256: Option<String>,
    fingerprint: Option<Value>,
}

/// 既存の最終 JSONL を解析し、完了済み game_id の集合と勝敗を取得する。
fn parse_resume_state(path: &Path, max_game_id: u32) -> Result<ResumeState> {
    let file = open_regular_file_nofollow(path)
        .with_context(|| format!("failed to open {} for resume", path.display()))?;
    let reader = BufReader::new(file);

    let mut completed_games = CompletedGames::default();
    let mut black_wins: u32 = 0;
    let mut white_wins: u32 = 0;
    let mut draws: u32 = 0;
    let mut shuffle_seed: Option<u64> = None;
    let mut progress_file: Option<String> = None;
    let mut progress_file_sha256: Option<String> = None;
    let mut fingerprint = None;

    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(trimmed)
            .with_context(|| format!("invalid JSONL in resume file {}", path.display()))?;
        match value.get("type").and_then(|v| v.as_str()) {
            Some("meta") => {
                // meta 行から resume に必要な設定を復元
                if let Some(settings) = value.get("settings") {
                    shuffle_seed = settings.get("shuffle_seed").and_then(|v| v.as_u64());
                    progress_file = settings
                        .get("progress_file")
                        .and_then(|v| v.as_str())
                        .map(ToString::to_string);
                    progress_file_sha256 = settings
                        .get("progress_file_sha256")
                        .and_then(|v| v.as_str())
                        .map(ToString::to_string);
                }
                fingerprint = value.get("fingerprint").cloned();
            }
            Some("result") => {
                let gid: u32 = value
                    .get("game_id")
                    .and_then(Value::as_u64)
                    .context("result without game_id")?
                    .try_into()
                    .context("game_id exceeds u32")?;
                if gid == 0 || gid > max_game_id {
                    bail!(
                        "result game_id {gid} is outside 1..={max_game_id} in {} \
                         (control.json で target_games を引き上げた run は、引き上げ後の値以上を \
                         --games に指定して resume する)",
                        path.display()
                    );
                }
                match value.get("outcome").and_then(|v| v.as_str()) {
                    Some("black_win") => black_wins += 1,
                    Some("white_win") => white_wins += 1,
                    Some("draw") => draws += 1,
                    _ => bail!("invalid result outcome in {}", path.display()),
                }
                if !completed_games.insert(gid) {
                    bail!("duplicate result game_id {gid} in {}", path.display());
                }
            }
            _ => {}
        }
    }

    Ok(ResumeState {
        completed_games,
        black_wins,
        white_wins,
        draws,
        shuffle_seed,
        progress_file,
        progress_file_sha256,
        fingerprint,
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

fn sha256_file(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut file =
        File::open(path).with_context(|| format!("failed to hash {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn sha256_path_content(path: &Path) -> Result<Option<String>> {
    use sha2::{Digest, Sha256};
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => return sha256_file(path).map(Some),
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            bail!("USI option path is neither a file nor a directory: {}", path.display())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect USI option path {}", path.display()));
        }
    }
    let mut hasher = Sha256::new();
    let entries = walkdir::WalkDir::new(path).follow_links(true).sort_by_file_name().into_iter();
    for entry in entries {
        let entry = entry.with_context(|| format!("failed to walk {}", path.display()))?;
        if entry.path() == path || entry.file_type().is_dir() {
            continue;
        }
        let relative = entry.path().strip_prefix(path)?;
        let relative = relative.to_string_lossy();
        hasher.update((relative.len() as u64).to_le_bytes());
        hasher.update(relative.as_bytes());
        let mut file = File::open(entry.path())?;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
    }
    Ok(Some(format!("{:x}", hasher.finalize())))
}

fn usi_option_path_fingerprints(options: &[String]) -> Result<Vec<Value>> {
    let engine_cwd =
        std::env::current_dir().context("failed to resolve engine working directory")?;
    let mut fingerprints = Vec::new();
    for option in options {
        let Some((name, value)) = option.split_once('=') else {
            continue;
        };
        let name = name.trim();
        let value = value.trim();
        let normalized: String = name
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
            .flat_map(char::to_lowercase)
            .collect();
        if !(normalized.ends_with("file")
            || normalized.ends_with("dir")
            || normalized.ends_with("path")
            || normalized == "ls_progress_coeff")
        {
            continue;
        }
        let path = Path::new(value);
        let resolved_path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            engine_cwd.join(path)
        };
        fingerprints.push(serde_json::json!({
            "name": name,
            "value": value,
            "resolved_path": resolved_path,
            "content_sha256": sha256_path_content(&resolved_path).with_context(|| {
                format!("failed to fingerprint path-valued USI option {name}={value}")
            })?,
        }));
    }
    Ok(fingerprints)
}

/// fingerprint の model 節を構築する。
///
/// `fv_scale` と LayerStacks routing は未指定時にキー自体を出さない。旧バージョンで
/// 生成した無関係な run の fingerprint と bit 一致させ、resume を壊さないための互換要件。
fn build_model_fingerprint(
    native_mode: bool,
    eval_file: Option<String>,
    eval_file_sha256: Option<String>,
    progress_file: Option<String>,
    progress_file_sha256: Option<String>,
    bucket_mode: Option<String>,
    progress_buckets: Option<usize>,
    fv_scale: i32,
) -> Value {
    let mut model = serde_json::json!({
        "native": native_mode,
        "eval_file": eval_file,
        "eval_file_sha256": eval_file_sha256,
        "progress_file": progress_file,
        "progress_file_sha256": progress_file_sha256,
    });
    if let Some(bucket_mode) = bucket_mode {
        model["bucket_mode"] = serde_json::json!(bucket_mode);
    }
    if let Some(progress_buckets) = progress_buckets {
        model["progress_buckets"] = serde_json::json!(progress_buckets);
    }
    if fv_scale > 0 {
        model["fv_scale"] = serde_json::json!(fv_scale);
    }
    model
}

/// target_games の動的変更後に保留 ticket と退避 ticket を整合させる。
///
/// - 供給対象外 (game_id > target) になった保留 ticket は破棄せず退避する。ticket の
///   startpos は game_idx 順の乱数消費で決まるため、破棄・再生成すると resume の再現と
///   食い違う。
/// - target が戻ったら退避 ticket を新規生成より優先して供給に戻す (game_idx 順を保つ)。
///
/// 戻り値: 保留が空のままで、呼び出し側が新規 ticket 生成を試みるべきなら true。
fn reconcile_pending_ticket(
    next_ticket: &mut Option<GameTicket>,
    parked_ticket: &mut Option<GameTicket>,
    target_games: u32,
) -> bool {
    if next_ticket.as_ref().is_some_and(|t| t.game_idx + 1 > target_games) {
        *parked_ticket = next_ticket.take();
    }
    if next_ticket.is_none() && parked_ticket.as_ref().is_some_and(|t| t.game_idx < target_games) {
        *next_ticket = parked_ticket.take();
    }
    next_ticket.is_none()
}

/// `control.json` をポーリングする間隔。対局は秒オーダーなので 500ms で十分応答できる。
const CONTROL_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// `control.json` の受理スキーマ。存在するフィールドだけ反映する。
#[derive(Deserialize)]
struct ControlFile {
    concurrency: Option<usize>,
    target_games: Option<u32>,
}

/// `control_history.jsonl` の 1 レコード。再現性のため変更を時系列で残す。
#[derive(Serialize)]
struct ControlHistoryEntry {
    #[serde(rename = "type")]
    kind: &'static str,
    timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    concurrency: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_games: Option<u32>,
    /// 変更を適用した時点で完了していた対局数。
    completed: u32,
}

/// `control.json` を読み、同時 in-flight 対局数の上限を更新する。
///
/// worker スレッド数（= `--concurrency`、per-worker checkpoint 数と fingerprint に固定）
/// は変えられないため、上限を超える指定は `--concurrency` に clamp する。
/// 長時間 background 運用での堅牢性を優先し、ファイル不在 / 読込失敗 / パース失敗 /
/// history 追記失敗はいずれも警告のみで実行を継続する（対局を落とさない）。
fn apply_control(
    control_path: &Path,
    history_path: &Path,
    control_baseline: std::time::SystemTime,
    stale_control_warned: &mut bool,
    effective_concurrency: &mut usize,
    max_concurrency: usize,
    target_games: &mut u32,
    min_target_games: u32,
    completed: u32,
) {
    // 前回 run の drain 指定などが残った control.json を resume が拾って即終了しないよう、
    // 本プロセス開始より古い mtime のファイルは無視する (反映したければ書き直す)。
    match std::fs::metadata(control_path).and_then(|m| m.modified()) {
        // >= : baseline は秒に切り捨て済みなので、mtime が秒粒度でもプロセス開始と
        // 同一秒に書かれた指定を握り潰さない
        Ok(mtime) if mtime >= control_baseline => {}
        Ok(_) => {
            if !*stale_control_warned {
                *stale_control_warned = true;
                eprintln!(
                    "[control] {} は本プロセス開始前の内容のため無視します (反映するには書き直してください)",
                    control_path.display()
                );
            }
            return;
        }
        Err(_) => return,
    }
    let Ok(text) = std::fs::read_to_string(control_path) else {
        return;
    };
    let parsed: ControlFile = match serde_json::from_str(&text) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[control] {} のパースに失敗したため無視します: {e}", control_path.display());
            return;
        }
    };
    let mut changed_conc: Option<usize> = None;
    if let Some(requested) = parsed.concurrency {
        if requested == 0 {
            eprintln!("[control] concurrency=0 は不正のため無視します");
        } else {
            let clamped = requested.min(max_concurrency);
            if clamped != *effective_concurrency {
                if requested > max_concurrency {
                    eprintln!(
                        "[control] concurrency={requested} は worker 数の上限 --concurrency {max_concurrency} に clamp します"
                    );
                }
                *effective_concurrency = clamped;
                changed_conc = Some(clamped);
            }
        }
    }
    let mut changed_target: Option<u32> = None;
    if let Some(requested) = parsed.target_games {
        // 発行済み game_id は取り消せないため、下げる場合は発行済み範囲へ clamp する
        // (= 供給停止 + in-flight 完走の安全な drain になる)。
        let clamped = requested.max(min_target_games);
        if clamped != *target_games {
            if requested < min_target_games {
                eprintln!(
                    "[control] target_games={requested} は送信済み game_id の最大値 {min_target_games} に clamp します (in-flight 完走後に finalize)"
                );
            }
            *target_games = clamped;
            changed_target = Some(clamped);
        }
    }
    if changed_conc.is_none() && changed_target.is_none() {
        return;
    }
    let mut applied_parts = Vec::new();
    if let Some(c) = changed_conc {
        applied_parts.push(format!("concurrency={c}"));
    }
    if let Some(t) = changed_target {
        applied_parts.push(format!("target_games={t}"));
    }
    println!("[control] applied: {} (completed={completed})", applied_parts.join(" "));
    let entry = ControlHistoryEntry {
        kind: "control",
        timestamp: Local::now().to_rfc3339(),
        concurrency: changed_conc,
        target_games: changed_target,
        completed,
    };
    let result = serde_json::to_string(&entry).map_err(anyhow::Error::from).and_then(|line| {
        let mut file = OpenOptions::new().create(true).append(true).open(history_path)?;
        writeln!(file, "{line}")?;
        Ok(())
    });
    if let Err(e) = result {
        eprintln!(
            "[control] {} への履歴追記に失敗しました（実行は継続）: {e}",
            history_path.display()
        );
    }
}

fn validate_resume_fingerprint(meta: Option<&Value>, current: &Value) -> Result<()> {
    let meta = meta.context(
        "--resume: meta has no generation fingerprint; move existing outputs aside and start a new run",
    )?;
    let mut saved = meta.clone();
    let mut current = current.clone();
    // hcpe3 policy のキーは psv / pack の fingerprint にも書かれるが、これらの形式では
    // 生成物に影響しない。既定値を変えたときに無関係な差分で resume を拒否しないよう、
    // hcpe3 以外では両側から対称に除外して比較する (片側だけだとキー欠落が差分になる)。
    if current.pointer("/training/format").and_then(Value::as_str) != Some("hcpe3") {
        remove_hcpe3_policy_fingerprint(&mut saved);
        remove_hcpe3_policy_fingerprint(&mut current);
    }
    if saved == current {
        return Ok(());
    }
    let mut differences = Vec::new();
    collect_json_differences("", &saved, &current, &mut differences);
    differences.sort();
    bail!(
        "--resume: generation fingerprint mismatch in fields: {}",
        differences.join(", ")
    )
}

fn remove_hcpe3_policy_fingerprint(fingerprint: &mut Value) {
    let Some(training) = fingerprint.get_mut("training").and_then(Value::as_object_mut) else {
        return;
    };
    for key in [
        "hcpe3_policy_total",
        "hcpe3_policy_temp",
        "hcpe3_eval_drop_threshold",
        "hcpe3_policy_algo",
    ] {
        training.remove(key);
    }
}

fn add_hcpe3_policy_fingerprint(
    fingerprint: &mut Value,
    format: TrainingFormat,
    eval_drop_threshold: i32,
) {
    if format == TrainingFormat::Hcpe3 {
        // psv / pack の既存 run を巻き込まず、hcpe3 の policy 生成規約が変わった
        // checkpoint からの resume を拒否するため、hcpe3 のときだけ常に記録する。
        fingerprint["training"]["hcpe3_eval_drop_threshold"] =
            serde_json::json!(eval_drop_threshold);
        fingerprint["training"]["hcpe3_policy_algo"] = serde_json::json!(HCPE3_POLICY_ALGO_VERSION);
    }
}

fn collect_json_differences(prefix: &str, left: &Value, right: &Value, out: &mut Vec<String>) {
    match (left.as_object(), right.as_object()) {
        (Some(left), Some(right)) => {
            for key in left.keys().chain(right.keys()) {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                if out.iter().any(|existing| existing == &path) {
                    continue;
                }
                match (left.get(key), right.get(key)) {
                    (Some(a), Some(b)) if a != b => collect_json_differences(&path, a, b, out),
                    (Some(_), Some(_)) => {}
                    _ => out.push(path),
                }
            }
        }
        _ => out.push(prefix.to_string()),
    }
}

fn recover_worker_checkpoint(
    jsonl_path: &Path,
    training_path: Option<&Path>,
    sidecar_path: Option<&Path>,
    info_path: Option<&Path>,
    eval_path: Option<&Path>,
    metrics_path: Option<&Path>,
    format: TrainingFormat,
    worker_id: usize,
    max_game_id: u32,
) -> Result<ResumeState> {
    let jsonl_file = checkpoint_file_state(Some(jsonl_path))?;
    let training_file = checkpoint_file_state(training_path)?;
    let sidecar_file = checkpoint_file_state(sidecar_path)?;
    let info_file = checkpoint_file_state(info_path)?;
    let eval_file = checkpoint_file_state(eval_path)?;
    let metrics_file = checkpoint_file_state(metrics_path)?;
    if !jsonl_file.exists {
        if [
            training_file,
            sidecar_file,
            info_file,
            eval_file,
            metrics_file,
        ]
        .into_iter()
        .any(|file| file.len > 0)
        {
            bail!(
                "resume checkpoint {} is missing while training data remains; move the worker temp files aside before retrying",
                jsonl_path.display()
            );
        }
        return Ok(empty_resume_state());
    }

    let mut state = empty_resume_state();
    let mut parsed_state = empty_resume_state();
    let mut committed = CheckpointLengths {
        training: 0,
        sidecar: sidecar_path.map(|_| 0),
        info: info_path.map(|_| 0),
        eval: eval_path.map(|_| 0),
        metrics: metrics_path.map(|_| 0),
    };
    let mut reader = BufReader::new(open_regular_file_nofollow(jsonl_path)?);
    let mut line = Vec::new();
    let mut parsed = committed;
    let mut committed_jsonl_len = 0u64;
    let mut complete_len = 0u64;
    let mut line_index = 0usize;
    let mut durable_prefix_ended = false;
    let mut results_after_boundary = 0u64;
    let mut seen_games = CompletedGames::default();
    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line)?;
        if read == 0 {
            break;
        }
        if !line.ends_with(b"\n") {
            break;
        }
        complete_len += read as u64;
        line_index += 1;
        let line = &line[..line.len() - 1];
        if line.is_empty() {
            continue;
        }
        let value: Value = serde_json::from_slice(line).with_context(|| {
            format!("invalid JSON in {} at line {}", jsonl_path.display(), line_index)
        })?;
        if value.get("type").and_then(Value::as_str) != Some("result") {
            bail!("unexpected non-result line in worker checkpoint {}", jsonl_path.display());
        }
        let game_id: u32 = value
            .get("game_id")
            .and_then(Value::as_u64)
            .context("worker result has no game_id")?
            .try_into()
            .context("worker game_id exceeds u32")?;
        if game_id == 0 || game_id > max_game_id {
            bail!(
                "worker game_id {game_id} is outside 1..={max_game_id} in {} \
                 (control.json で target_games を引き上げた run は、引き上げ後の値以上を \
                 --games に指定して resume する)",
                jsonl_path.display()
            );
        }
        let row_worker_id = value
            .get("worker_id")
            .and_then(Value::as_u64)
            .context("worker result has no worker_id")? as usize;
        if row_worker_id != worker_id {
            bail!("worker_id mismatch in {}", jsonl_path.display());
        }
        let next_training = value
            .get("training_bytes")
            .and_then(Value::as_u64)
            .with_context(|| {
                format!(
                    "{} has legacy result without training_bytes; move temp files aside rather than overwriting them",
                    jsonl_path.display()
                )
            })?;
        let read_optional_offset =
            |field: &str, enabled: bool| -> Result<Option<u64>> {
                if enabled {
                    Ok(Some(value.get(field).and_then(Value::as_u64).with_context(|| {
                        format!("{} result has no {field}", jsonl_path.display())
                    })?))
                } else if value.get(field).is_some_and(|offset| !offset.is_null()) {
                    bail!("{} unexpectedly has {field}", jsonl_path.display())
                } else {
                    Ok(None)
                }
            };
        let next = CheckpointLengths {
            training: next_training,
            sidecar: read_optional_offset("sidecar_bytes", sidecar_path.is_some())?,
            info: read_optional_offset("info_bytes", info_path.is_some())?,
            eval: read_optional_offset("eval_bytes", eval_path.is_some())?,
            metrics: read_optional_offset("metrics_bytes", metrics_path.is_some())?,
        };
        validate_referenced_files_exist(
            next,
            [
                ("training data", training_path, training_file),
                ("game_id sidecar", sidecar_path, sidecar_file),
                ("info log", info_path, info_file),
                ("eval file", eval_path, eval_file),
                ("metrics", metrics_path, metrics_file),
            ],
            jsonl_path,
        )?;
        validate_checkpoint_monotonic(parsed, next, jsonl_path)?;
        validate_checkpoint_record_boundaries(next, format, jsonl_path)?;
        parsed = next;
        let outcome = value
            .get("outcome")
            .and_then(Value::as_str)
            .with_context(|| format!("invalid result outcome in {}", jsonl_path.display()))?;
        if !matches!(outcome, "black_win" | "white_win" | "draw") {
            bail!("invalid result outcome in {}", jsonl_path.display());
        }
        if !seen_games.insert(game_id) {
            bail!("duplicate game_id {game_id} in {}", jsonl_path.display());
        }
        let fsync_boundary = value
            .get("fsync_boundary")
            .and_then(Value::as_bool)
            .with_context(|| {
                format!(
                    "{} has legacy result without fsync_boundary; move temp files aside rather than overwriting them",
                    jsonl_path.display()
                )
            })?;
        match outcome {
            "black_win" => parsed_state.black_wins += 1,
            "white_win" => parsed_state.white_wins += 1,
            "draw" => parsed_state.draws += 1,
            _ => unreachable!(),
        }
        parsed_state.completed_games.insert(game_id);
        results_after_boundary += 1;
        if durable_prefix_ended || !fsync_boundary {
            continue;
        }
        if !checkpoint_fits_files(
            next,
            training_file.len,
            sidecar_file.len,
            info_file.len,
            eval_file.len,
            metrics_file.len,
        ) {
            durable_prefix_ended = true;
            continue;
        }
        state = parsed_state.clone();
        committed = next;
        committed_jsonl_len = complete_len;
        results_after_boundary = 0;
    }

    // ここまでの検証で全成果物を同じ result 境界へ戻せることが確定している。
    truncate_if_needed(jsonl_path, committed_jsonl_len, jsonl_file.len)?;
    if let Some(path) = training_path {
        truncate_if_needed(path, committed.training, training_file.len)?;
    }
    if let (Some(path), Some(len)) = (sidecar_path, committed.sidecar) {
        truncate_if_needed(path, len, sidecar_file.len)?;
    }
    if let (Some(path), Some(len)) = (info_path, committed.info) {
        truncate_if_needed(path, len, info_file.len)?;
    }
    if let (Some(path), Some(len)) = (eval_path, committed.eval) {
        truncate_if_needed(path, len, eval_file.len)?;
    }
    if let (Some(path), Some(len)) = (metrics_path, committed.metrics) {
        truncate_if_needed(path, len, metrics_file.len)?;
    }
    if results_after_boundary > 0 {
        eprintln!(
            "warning: recovered {} by discarding {results_after_boundary} result row(s) after the last proven fsync boundary",
            jsonl_path.display()
        );
    }
    Ok(state)
}

fn checkpoint_fits_files(
    lengths: CheckpointLengths,
    training_len: u64,
    sidecar_len: u64,
    info_len: u64,
    eval_len: u64,
    metrics_len: u64,
) -> bool {
    lengths.training <= training_len
        && lengths.sidecar.is_none_or(|len| len <= sidecar_len)
        && lengths.info.is_none_or(|len| len <= info_len)
        && lengths.eval.is_none_or(|len| len <= eval_len)
        && lengths.metrics.is_none_or(|len| len <= metrics_len)
}

fn validate_checkpoint_record_boundaries(
    lengths: CheckpointLengths,
    format: TrainingFormat,
    path: &Path,
) -> Result<()> {
    if format != TrainingFormat::Psv {
        return Ok(());
    }
    if !lengths.training.is_multiple_of(PackedSfenValue::SIZE as u64) {
        bail!(
            "committed PSV length in {} is not on a {}-byte record boundary",
            path.display(),
            PackedSfenValue::SIZE
        );
    }
    if let Some(sidecar) = lengths.sidecar {
        if !sidecar.is_multiple_of(4) {
            bail!(
                "committed game_id sidecar length in {} is not on a 4-byte record boundary",
                path.display()
            );
        }
        if sidecar / 4 != lengths.training / PackedSfenValue::SIZE as u64 {
            bail!("PSV and game_id sidecar committed record counts differ in {}", path.display());
        }
    }
    Ok(())
}

fn validate_checkpoint_monotonic(
    previous: CheckpointLengths,
    next: CheckpointLengths,
    path: &Path,
) -> Result<()> {
    let fields = [
        ("training_bytes", Some(previous.training), Some(next.training)),
        ("sidecar_bytes", previous.sidecar, next.sidecar),
        ("info_bytes", previous.info, next.info),
        ("eval_bytes", previous.eval, next.eval),
        ("metrics_bytes", previous.metrics, next.metrics),
    ];
    for (name, previous, next) in fields {
        if let (Some(previous), Some(next)) = (previous, next)
            && next < previous
        {
            bail!("non-monotonic {name} in {}: {next} < {previous}", path.display());
        }
    }
    Ok(())
}

fn truncate_if_needed(path: &Path, committed: u64, actual: u64) -> Result<()> {
    if committed != actual {
        truncate_file(path, committed)?;
    }
    Ok(())
}

fn empty_resume_state() -> ResumeState {
    ResumeState {
        completed_games: CompletedGames::default(),
        black_wins: 0,
        white_wins: 0,
        draws: 0,
        shuffle_seed: None,
        progress_file: None,
        progress_file_sha256: None,
        fingerprint: None,
    }
}

#[derive(Clone, Copy)]
struct CheckpointFileState {
    exists: bool,
    len: u64,
}

fn checkpoint_file_state(path: Option<&Path>) -> Result<CheckpointFileState> {
    match path {
        Some(path) => match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.is_file() => Ok(CheckpointFileState {
                exists: true,
                len: meta.len(),
            }),
            Ok(_) => bail!("worker checkpoint {} is not a regular file", path.display()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(CheckpointFileState {
                exists: false,
                len: 0,
            }),
            Err(error) => Err(error).with_context(|| format!("failed to stat {}", path.display())),
        },
        None => Ok(CheckpointFileState {
            exists: false,
            len: 0,
        }),
    }
}

fn validate_referenced_files_exist(
    lengths: CheckpointLengths,
    files: [(&str, Option<&Path>, CheckpointFileState); 5],
    jsonl_path: &Path,
) -> Result<()> {
    let offsets = [
        Some(lengths.training),
        lengths.sidecar,
        lengths.info,
        lengths.eval,
        lengths.metrics,
    ];
    for ((label, path, file), offset) in files.into_iter().zip(offsets) {
        if offset.is_some_and(|offset| offset > 0) && !file.exists {
            bail!(
                "resume checkpoint {} references missing {label} {} at non-zero offset; no files were truncated",
                jsonl_path.display(),
                path.context("enabled checkpoint path missing")?.display()
            );
        }
    }
    Ok(())
}

fn file_len_or_zero(path: Option<&Path>) -> Result<u64> {
    Ok(checkpoint_file_state(path)?.len)
}

fn truncate_file(path: &Path, len: u64) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        bail!("worker checkpoint {} is not a regular file", path.display());
    }
    file.set_len(len)?;
    file.sync_all()?;
    Ok(())
}

fn validate_resume_progress_file(
    meta_progress_file: Option<&str>,
    cli_progress_file: Option<&Path>,
) -> Result<()> {
    let cli_progress_file = cli_progress_file.map(|p| p.display().to_string());
    if meta_progress_file != cli_progress_file.as_deref() {
        bail!(
            "--resume: --progress-file does not match meta settings.progress_file \
             (meta={}, cli={})",
            meta_progress_file.unwrap_or("<none>"),
            cli_progress_file.as_deref().unwrap_or("<none>"),
        );
    }
    Ok(())
}

/// resume 時に、実際にロードした progress 係数の内容が meta 記録時と同一かを照合する。
/// パス一致だけでは同一パスへの係数差し替えを検出できない。
/// meta に SHA-256 が無い場合（記録前の run からの再開）は照合をスキップして警告する。
fn validate_resume_progress_content(
    meta_progress_sha256: Option<&str>,
    loaded_sha256: &str,
) -> Result<()> {
    match meta_progress_sha256 {
        Some(meta) if meta != loaded_sha256 => bail!(
            "--resume: --progress-file content does not match meta settings.progress_file_sha256 \
             (meta={meta}, loaded={loaded_sha256})",
        ),
        None => {
            eprintln!(
                "warning: meta has no settings.progress_file_sha256; \
                 skipping --progress-file content verification"
            );
            Ok(())
        }
        _ => Ok(()),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct FinalizationOutput {
    final_path: PathBuf,
    staged_path: PathBuf,
    len: u64,
    sha256: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct FinalizationJournal {
    schema: u32,
    outputs: Vec<FinalizationOutput>,
    worker_temps: Vec<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct FinalizedState {
    schema: u32,
    outputs: Vec<FinalizedOutput>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct FinalizedOutput {
    path: PathBuf,
    len: u64,
    sha256: String,
}

#[derive(Serialize)]
struct RunLockInfo {
    pid: u32,
}

#[derive(Debug)]
struct RunDirLock {
    path: PathBuf,
    body: Vec<u8>,
}

impl RunDirLock {
    fn acquire(output_path: &Path, force_unlock: bool) -> Result<Self> {
        let path = output_path.with_file_name(".gensfen.lock");
        if force_unlock {
            match std::fs::remove_file(&path) {
                Ok(()) => eprintln!("--force-unlock: 残留 lock {} を削除しました", path.display()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("failed to remove stale lock {}", path.display())
                    });
                }
            }
        }

        let body = serde_json::to_vec(&RunLockInfo {
            pid: std::process::id(),
        })?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.custom_flags(libc::O_NOFOLLOW);
        let mut file = match options.open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let owner = read_regular_file_nofollow(&path)
                    .and_then(|bytes| String::from_utf8(bytes).map_err(Into::into))
                    .unwrap_or_else(|_| "<read error>".into());
                bail!(
                    "out-dir is locked by another gensfen process: {}\n  lock: {}\n  process が終了済みなら --force-unlock を指定してください",
                    path.display(),
                    owner.trim()
                );
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to create lock {}", path.display()));
            }
        };
        file.write_all(&body)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        sync_parent(&path)?;
        let mut stored_body = body;
        stored_body.push(b'\n');
        Ok(Self {
            path,
            body: stored_body,
        })
    }
}

impl Drop for RunDirLock {
    fn drop(&mut self) {
        if read_regular_file_nofollow(&self.path).ok().as_deref() == Some(self.body.as_slice()) {
            let _ = std::fs::remove_file(&self.path);
            let _ = sync_parent(&self.path);
        }
    }
}

fn finalization_journal_path(output_path: &Path) -> PathBuf {
    output_path.with_file_name(".gensfen.finalization.json")
}

fn finalized_state_path(output_path: &Path) -> PathBuf {
    output_path.with_file_name("gensfen.finalized.json")
}

fn atomic_temp_path(path: &Path) -> PathBuf {
    path.with_extension("json.tmp")
}

fn sync_parent(path: &Path) -> Result<()> {
    // Windows は CreateFile でディレクトリを開けず access denied になり、
    // directory fsync に相当する安全な std API も無いためスキップする
    // (電源断時のディレクトリエントリ永続化保証が Unix より弱くなるのみ)。
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        File::open(parent)
            .and_then(|dir| dir.sync_all())
            .with_context(|| format!("failed to fsync parent dir of {}", path.display()))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn open_regular_file_nofollow(path: &Path) -> Result<File> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    if !metadata.is_file() {
        bail!("{} is not a regular file", path.display());
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    let file = options
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    if !file.metadata()?.is_file() {
        bail!("{} is not a regular file", path.display());
    }
    Ok(file)
}

fn create_new_file_nofollow(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    options
        .open(path)
        .with_context(|| format!("failed to create {}", path.display()))
}

fn open_worker_checkpoint(path: &Path, append: bool) -> Result<File> {
    if append {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_file() => {}
            Ok(_) => bail!("worker checkpoint {} is not a regular file", path.display()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    open_worker_checkpoint_after_type_check(path, append)
}

fn open_worker_checkpoint_after_type_check(path: &Path, append: bool) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true);
    if append {
        options.create(true).append(true);
    } else {
        options.create_new(true);
    }
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    let file = options
        .open(path)
        .with_context(|| format!("failed to open worker checkpoint {}", path.display()))?;
    if !file.metadata()?.is_file() {
        bail!("worker checkpoint {} is not a regular file", path.display());
    }
    Ok(file)
}

fn read_regular_file_nofollow(path: &Path) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    open_regular_file_nofollow(path)?.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn is_not_found(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let tmp = atomic_temp_path(path);
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => bail!("{} is not a regular file", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let mut file = create_new_file_nofollow(&tmp)?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    drop(file);
    let is_finalization_journal =
        path.file_name().is_some_and(|name| name == ".gensfen.finalization.json");
    if is_finalization_journal && injected_fault("before_journal_rename") {
        bail!("injected failure before finalization journal rename");
    }
    std::fs::rename(&tmp, path)?;
    if is_finalization_journal && injected_fault("after_journal_rename") {
        std::process::abort();
    }
    sync_parent(path)
}

fn finalized_state(journal: &FinalizationJournal) -> FinalizedState {
    FinalizedState {
        schema: 1,
        outputs: journal
            .outputs
            .iter()
            .map(|output| FinalizedOutput {
                path: output.final_path.clone(),
                len: output.len,
                sha256: output.sha256.clone(),
            })
            .collect(),
    }
}

fn validate_file_identity(path: &Path, expected_len: u64, expected_sha256: &str) -> Result<()> {
    use sha2::{Digest, Sha256};
    let mut file = open_regular_file_nofollow(path)
        .with_context(|| format!("missing finalized output {}", path.display()))?;
    let actual_len = file.metadata()?.len();
    if actual_len != expected_len {
        bail!(
            "finalized output {} has unexpected length: {actual_len} != {expected_len}",
            path.display()
        );
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let actual_sha256 = format!("{:x}", hasher.finalize());
    if actual_sha256 != expected_sha256 {
        bail!("finalized output {} has unexpected content hash", path.display());
    }
    Ok(())
}

fn complete_finalization(
    output_path: &Path,
    journal: &FinalizationJournal,
    verify_staged: bool,
) -> Result<()> {
    for (index, output) in journal.outputs.iter().enumerate() {
        match std::fs::symlink_metadata(&output.staged_path) {
            Ok(metadata) if metadata.is_file() => {
                if verify_staged {
                    validate_file_identity(&output.staged_path, output.len, &output.sha256)?;
                }
                std::fs::rename(&output.staged_path, &output.final_path).with_context(|| {
                    format!("failed to atomically replace {}", output.final_path.display())
                })?;
                sync_parent(&output.final_path)?;
                if injected_fault(&format!("after_final_rename_{}", index + 1)) {
                    std::process::abort();
                }
            }
            Ok(_) => bail!("{} is not a regular file", output.staged_path.display()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                validate_file_identity(&output.final_path, output.len, &output.sha256)?;
            }
            Err(error) => return Err(error.into()),
        }
    }

    write_json_atomic(&finalized_state_path(output_path), &finalized_state(journal))?;
    for (index, temp) in journal.worker_temps.iter().enumerate() {
        match std::fs::remove_file(temp) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        if injected_fault(&format!("after_worker_temp_delete_{}", index + 1)) {
            std::process::abort();
        }
    }
    sync_parent(output_path)?;
    let journal_path = finalization_journal_path(output_path);
    std::fs::remove_file(&journal_path)?;
    sync_parent(&journal_path)
}

fn recover_pending_finalization(output_path: &Path) -> Result<()> {
    let path = finalization_journal_path(output_path);
    let tmp_path = atomic_temp_path(&path);
    let bytes = match read_regular_file_nofollow(&path) {
        Ok(bytes) => bytes,
        Err(error) if is_not_found(&error) => match read_regular_file_nofollow(&tmp_path) {
            Ok(bytes) => {
                match serde_json::from_slice::<FinalizationJournal>(&bytes) {
                    Ok(journal) if journal.schema == 1 => {}
                    Ok(journal) => {
                        eprintln!(
                            "warning: discarding finalization journal temporary file {} with unsupported schema {}",
                            tmp_path.display(),
                            journal.schema
                        );
                        std::fs::remove_file(&tmp_path)?;
                        sync_parent(&tmp_path)?;
                        return Ok(());
                    }
                    Err(parse_error) => {
                        eprintln!(
                            "warning: discarding invalid finalization journal temporary file {}: {parse_error}",
                            tmp_path.display()
                        );
                        std::fs::remove_file(&tmp_path)?;
                        sync_parent(&tmp_path)?;
                        return Ok(());
                    }
                }
                std::fs::rename(&tmp_path, &path)?;
                sync_parent(&path)?;
                bytes
            }
            Err(error) if is_not_found(&error) => {
                return Ok(());
            }
            Err(error) => return Err(error),
        },
        Err(error) => return Err(error),
    };
    let journal: FinalizationJournal = serde_json::from_slice(&bytes)
        .with_context(|| format!("invalid finalization journal {}", path.display()))?;
    if journal.schema != 1 {
        bail!("unsupported finalization journal schema {}", journal.schema);
    }
    let state_tmp = atomic_temp_path(&finalized_state_path(output_path));
    match std::fs::symlink_metadata(&state_tmp) {
        Ok(metadata) if metadata.is_file() => std::fs::remove_file(&state_tmp)?,
        Ok(_) => bail!("{} is not a regular file", state_tmp.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    eprintln!("recovering interrupted gensfen finalization");
    complete_finalization(output_path, &journal, true)
}

fn validate_finalized_outputs(
    output_path: &Path,
    required_paths: &[PathBuf],
    training_path: Option<&Path>,
    sidecar_path: Option<&Path>,
    format: TrainingFormat,
    has_results: bool,
) -> Result<Option<FinalizedState>> {
    let state_path = finalized_state_path(output_path);
    let bytes = match read_regular_file_nofollow(&state_path) {
        Ok(bytes) => bytes,
        Err(error) if is_not_found(&error) && !has_results => {
            for path in [training_path, sidecar_path].into_iter().flatten() {
                if std::fs::metadata(path).is_ok_and(|meta| meta.len() > 0) {
                    bail!("final output {} exists without finalized state", path.display());
                }
            }
            return Ok(None);
        }
        Err(error) if is_not_found(&error) => {
            bail!("resume results exist but {} is missing", state_path.display())
        }
        Err(error) => return Err(error),
    };
    let state: FinalizedState = serde_json::from_slice(&bytes)?;
    if state.schema != 1 {
        bail!("unsupported finalized state schema {}", state.schema);
    }
    for output in &state.outputs {
        let actual = open_regular_file_nofollow(&output.path)
            .with_context(|| format!("missing final output {}", output.path.display()))?
            .metadata()?
            .len();
        if actual != output.len {
            bail!(
                "final output {} length differs from finalized state: {actual} != {}",
                output.path.display(),
                output.len
            );
        }
    }
    for required in required_paths {
        if !state.outputs.iter().any(|output| output.path == required.as_path()) {
            bail!("finalized state has no entry for {}", required.display());
        }
    }
    if format == TrainingFormat::Psv {
        let training_len = file_len_or_zero(training_path)?;
        if !training_len.is_multiple_of(PackedSfenValue::SIZE as u64) {
            bail!("final PSV is not on a {}-byte record boundary", PackedSfenValue::SIZE);
        }
        if let Some(sidecar_path) = sidecar_path {
            let sidecar_len = file_len_or_zero(Some(sidecar_path))?;
            if !sidecar_len.is_multiple_of(4) {
                bail!("final game_id sidecar is not on a 4-byte record boundary");
            }
            if training_len / PackedSfenValue::SIZE as u64 != sidecar_len / 4 {
                bail!("final PSV and game_id sidecar record counts differ");
            }
        }
    }
    Ok(Some(state))
}

/// worker temp を同一ディレクトリの一時ファイルへ連結し、rename 前の状態まで永続化する。
fn stage_concatenated_file(
    final_path: &Path,
    temp_paths: &[PathBuf],
    append: bool,
    regenerate_existing: bool,
    expected_prefix: Option<&FinalizedOutput>,
) -> Result<FinalizationOutput> {
    use sha2::{Digest, Sha256};
    let merge_path = merge_temp_path(final_path);
    if regenerate_existing {
        match std::fs::symlink_metadata(&merge_path) {
            Ok(metadata) if metadata.is_file() => std::fs::remove_file(&merge_path)?,
            Ok(_) => bail!("{} is not a regular file", merge_path.display()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    let mut out = create_new_file_nofollow(&merge_path)
        .with_context(|| format!("failed to create merge temp {}", merge_path.display()))?;
    let mut hasher = Sha256::new();
    let mut len = 0u64;
    if append {
        match std::fs::symlink_metadata(final_path) {
            Ok(_) => {
                append_file_to_stage(final_path, &mut out, &mut hasher, &mut len)?;
                if let Some(expected) = expected_prefix
                    && (len != expected.len
                        || format!("{:x}", hasher.clone().finalize()) != expected.sha256)
                {
                    bail!(
                        "final output {} content differs from finalized state while staging",
                        final_path.display()
                    );
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    for tmp in temp_paths {
        match std::fs::symlink_metadata(tmp) {
            Ok(_) => append_file_to_stage(tmp, &mut out, &mut hasher, &mut len)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    out.sync_all()?;
    drop(out);
    sync_parent(&merge_path)?;
    Ok(FinalizationOutput {
        final_path: final_path.to_path_buf(),
        staged_path: merge_path,
        len,
        sha256: format!("{:x}", hasher.finalize()),
    })
}

fn finalized_output_for<'a>(
    state: Option<&'a FinalizedState>,
    path: &Path,
) -> Option<&'a FinalizedOutput> {
    state?.outputs.iter().find(|output| output.path == path)
}

fn validate_worker_outputs_exist(paths: &[(&str, &Path)]) -> Result<()> {
    for (label, path) in paths {
        let metadata = std::fs::symlink_metadata(path)
            .with_context(|| format!("worker {label} output {} is missing", path.display()))?;
        if !metadata.is_file() {
            bail!("worker {label} output {} is not a regular file", path.display());
        }
    }
    Ok(())
}

fn validate_completed_worker_outputs(
    jsonl_path: &Path,
    training_path: &Path,
    sidecar_path: Option<&Path>,
    info_path: Option<&Path>,
    eval_path: Option<&Path>,
    metrics_path: Option<&Path>,
    format: TrainingFormat,
    worker_id: usize,
    max_game_id: u32,
) -> Result<()> {
    let mut required = vec![("JSONL", jsonl_path), ("training data", training_path)];
    required.extend(
        [
            sidecar_path.map(|path| ("game_id sidecar", path)),
            info_path.map(|path| ("info log", path)),
            eval_path.map(|path| ("eval file", path)),
            metrics_path.map(|path| ("metrics", path)),
        ]
        .into_iter()
        .flatten(),
    );
    validate_worker_outputs_exist(&required)?;

    let mut lengths = CheckpointLengths {
        sidecar: sidecar_path.map(|_| 0),
        info: info_path.map(|_| 0),
        eval: eval_path.map(|_| 0),
        metrics: metrics_path.map(|_| 0),
        ..CheckpointLengths::default()
    };
    let mut reader = BufReader::new(open_regular_file_nofollow(jsonl_path)?);
    let mut line = Vec::new();
    let mut line_index = 0usize;
    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line)?;
        if read == 0 {
            break;
        }
        line_index += 1;
        if !line.ends_with(b"\n") {
            bail!("incomplete JSON in {} at line {line_index}", jsonl_path.display());
        }
        let value: Value = serde_json::from_slice(&line[..line.len() - 1]).with_context(|| {
            format!("invalid JSON in {} at line {line_index}", jsonl_path.display())
        })?;
        if value.get("type").and_then(Value::as_str) != Some("result") {
            bail!("unexpected non-result line in worker checkpoint {}", jsonl_path.display());
        }
        if value.get("worker_id").and_then(Value::as_u64) != Some(worker_id as u64) {
            bail!("worker_id mismatch in {}", jsonl_path.display());
        }
        let game_id = value
            .get("game_id")
            .and_then(Value::as_u64)
            .context("worker result has no game_id")?;
        if game_id == 0 || game_id > u64::from(max_game_id) {
            bail!("worker game_id {game_id} is outside 1..={max_game_id}");
        }
        let read_offset =
            |field: &str, enabled: bool| -> Result<Option<u64>> {
                if enabled {
                    Ok(Some(value.get(field).and_then(Value::as_u64).with_context(|| {
                        format!("{} result has no {field}", jsonl_path.display())
                    })?))
                } else if value.get(field).is_some_and(|offset| !offset.is_null()) {
                    bail!("{} unexpectedly has {field}", jsonl_path.display())
                } else {
                    Ok(None)
                }
            };
        let next = CheckpointLengths {
            training: value.get("training_bytes").and_then(Value::as_u64).with_context(|| {
                format!("{} result has no training_bytes", jsonl_path.display())
            })?,
            sidecar: read_offset("sidecar_bytes", sidecar_path.is_some())?,
            info: read_offset("info_bytes", info_path.is_some())?,
            eval: read_offset("eval_bytes", eval_path.is_some())?,
            metrics: read_offset("metrics_bytes", metrics_path.is_some())?,
        };
        validate_checkpoint_monotonic(lengths, next, jsonl_path)?;
        validate_checkpoint_record_boundaries(next, format, jsonl_path)?;
        lengths = next;
    }

    let actual = CheckpointLengths {
        training: checkpoint_file_state(Some(training_path))?.len,
        sidecar: sidecar_path
            .map(|path| checkpoint_file_state(Some(path)).map(|file| file.len))
            .transpose()?,
        info: info_path
            .map(|path| checkpoint_file_state(Some(path)).map(|file| file.len))
            .transpose()?,
        eval: eval_path
            .map(|path| checkpoint_file_state(Some(path)).map(|file| file.len))
            .transpose()?,
        metrics: metrics_path
            .map(|path| checkpoint_file_state(Some(path)).map(|file| file.len))
            .transpose()?,
    };
    if lengths.training != actual.training
        || lengths.sidecar != actual.sidecar
        || lengths.info != actual.info
        || lengths.eval != actual.eval
        || lengths.metrics != actual.metrics
    {
        bail!(
            "worker checkpoint {} output lengths do not match its final result",
            jsonl_path.display()
        );
    }
    Ok(())
}

fn merge_temp_path(final_path: &Path) -> PathBuf {
    let file_name = final_path.file_name().and_then(|name| name.to_str()).unwrap_or("output");
    final_path.with_file_name(format!(".{file_name}.merge.tmp"))
}

fn append_file_to_stage(
    source: &Path,
    out: &mut File,
    hasher: &mut sha2::Sha256,
    len: &mut u64,
) -> Result<()> {
    use sha2::Digest;
    let mut input = open_regular_file_nofollow(source)?;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        out.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
        *len += read as u64;
    }
    Ok(())
}

fn main() -> Result<()> {
    // control.json の鮮度判定の基準点。NNUE ロードや resume 解析より前 (= プロセス開始
    // 直後) に取らないと、初期化中にオペレータが書いた指定が「古い」と誤判定される。
    // 秒粒度 mtime の FS では書き込み時刻が秒に切り捨てられるため、基準点も秒に切り捨てて
    // 「開始と同一秒の書き込みを無視しない」側に倒す (stale ファイルは分単位で古いので
    // 1 秒未満の窓で誤適用する実害はない)。
    let control_baseline = std::time::UNIX_EPOCH
        + std::time::Duration::from_secs(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        );
    let mut cli = Cli::parse();
    validate_cli(&cli)?;
    let _ = fault_spec();

    // 時間制限のバリデーション: depth/nodes 指定がなく時間制御もない場合はデフォルト byoyomi を設定
    let has_limit = cli.depth.is_some() || cli.nodes.is_some();
    if !has_limit
        && cli.btime == 0
        && cli.wtime == 0
        && cli.byoyomi == 0
        && cli.binc == 0
        && cli.winc == 0
    {
        eprintln!(
            "Warning: No time control specified. Using default byoyomi=1000ms to prevent infinite thinking."
        );
        cli.byoyomi = 1000;
    }

    // gensfen は PassRights を全くサポートしない（PSV/pack 形式が pass 手をエンコード
    // できないため）。USI options で検出した時点で副作用前に即 bail する。
    let common_usi_opts_early = cli.usi_options.clone().unwrap_or_default();
    let black_usi_opts_early =
        cli.usi_options_black.clone().unwrap_or_else(|| common_usi_opts_early.clone());
    let white_usi_opts_early =
        cli.usi_options_white.clone().unwrap_or_else(|| common_usi_opts_early.clone());
    let is_pass_rights_opt = |o: &str| {
        o == "PassRights=true"
            || o == "PassRights = true"
            || o == "PassRights=1"
            || o == "PassRights = 1"
    };
    if black_usi_opts_early.iter().any(|o| is_pass_rights_opt(o))
        || white_usi_opts_early.iter().any(|o| is_pass_rights_opt(o))
    {
        bail!(
            "PassRights USI option is not supported by gensfen (PackedSfen format cannot encode pass moves)"
        );
    }

    // --engine-path* が指定されているのに --native=false が明示されていない場合は
    // ユーザの意図が曖昧（NativeBackend は外部エンジンを起動しないため指定が無視される）。
    // explicit > magical の方針で副作用前に bail し、誤解を防ぐ。
    if (cli.engine_path.is_some()
        || cli.engine_path_black.is_some()
        || cli.engine_path_white.is_some())
        && cli.native != Some(false)
    {
        bail!(
            "--engine-path* requires --native=false. NativeBackend does not spawn external USI engines."
        );
    }
    if cli.native == Some(false)
        && (!has_explicit_usi_model_option(&black_usi_opts_early)
            || !has_explicit_usi_model_option(&white_usi_opts_early))
    {
        eprintln!(
            "warning: USI engine model is not explicit for both sides; specify EvalFile/EvalDir/NNUE/ModelFile options so resume can fingerprint model contents"
        );
    }

    let (start_defs, start_commands) =
        load_start_positions(cli.startpos_file.as_deref(), cli.sfen.as_deref(), None, None)?;
    let timestamp = Local::now();
    let output_path = resolve_output_path(cli.out_dir.as_deref(), &timestamp);
    let info_path = output_path.with_extension("info.jsonl");
    let training_format = match cli.training_data_format.as_str() {
        "psv" => TrainingFormat::Psv,
        "pack" => TrainingFormat::Pack,
        "hcpe3" => TrainingFormat::Hcpe3,
        other => {
            bail!("unknown training data format: '{}' (expected 'psv', 'pack', or 'hcpe3')", other)
        }
    };
    if cli.emit_game_id_sidecar.is_some() && training_format != TrainingFormat::Psv {
        bail!("--emit-game-id-sidecar requires --training-data-format psv");
    }
    validate_training_format_opts(
        training_format,
        cli.skip_in_check,
        cli.hcpe3_policy_total,
        cli.hcpe3_policy_temp,
    )?;
    let training_data_ext = match training_format {
        TrainingFormat::Psv => "psv",
        TrainingFormat::Pack => "pack",
        TrainingFormat::Hcpe3 => "hcpe3",
    };
    let training_data_path = cli
        .output_training_data
        .clone()
        .unwrap_or_else(|| default_training_data_path(&output_path, training_data_ext));
    let game_id_sidecar_path = cli.emit_game_id_sidecar.clone();
    validate_output_paths_unique(
        &output_path,
        &training_data_path,
        game_id_sidecar_path.as_deref(),
        training_data_ext,
        cli.concurrency,
        cli.log_info,
        cli.emit_eval_file,
        cli.emit_metrics,
    )?;
    let mut final_paths = vec![output_path.clone(), training_data_path.clone()];
    if cli.log_info {
        final_paths.push(info_path.clone());
    }
    if cli.emit_eval_file {
        final_paths.push(default_eval_path(&output_path));
    }
    if cli.emit_metrics {
        final_paths.push(default_metrics_path(&output_path));
    }
    final_paths.extend(game_id_sidecar_path.iter().cloned());
    validate_output_entry_types(
        &output_path,
        &final_paths,
        training_data_ext,
        cli.concurrency,
        cli.log_info,
        cli.emit_eval_file,
        cli.emit_metrics,
        game_id_sidecar_path.is_some(),
        cli.resume,
    )?;
    if !cli.resume {
        validate_fresh_output_paths(&final_paths)?;
    }
    if let Some(parent) = output_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let _run_dir_lock = RunDirLock::acquire(&output_path, cli.force_unlock)?;
    recover_pending_finalization(&output_path)?;
    // --resume バリデーションと進捗読み取り
    let mut resume_state = if cli.resume {
        if cli.out_dir.is_none() {
            bail!(
                "--resume には --out-dir の指定が必要です（自動生成パスでは前回のディレクトリを特定できません）"
            );
        }
        if !output_path.exists() {
            bail!("--resume: 出力ファイルが見つかりません: {}", output_path.display());
        }
        let state = parse_resume_state(&output_path, cli.games)?;
        validate_resume_progress_file(
            state.progress_file.as_deref(),
            cli.progress_file.as_deref(),
        )?;
        println!(
            "Resuming: {}/{}局完了済み（black {} / white {} / draw {}）",
            state.completed_games.len(),
            cli.games,
            state.black_wins,
            state.white_wins,
            state.draws,
        );
        Some(state)
    } else {
        None
    };
    let prior_finalized_state = if cli.resume {
        validate_finalized_outputs(
            &output_path,
            &final_paths,
            Some(&training_data_path),
            game_id_sidecar_path.as_deref(),
            training_format,
            resume_state.as_ref().is_some_and(|state| state.completed_games.len() > 0),
        )?
    } else {
        None
    };
    if let Some(parent) = game_id_sidecar_path.as_deref().and_then(Path::parent)
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let engine_paths = resolve_engine_paths(&cli);
    let threads_black = cli.threads_black.unwrap_or(cli.threads);
    let threads_white = cli.threads_white.unwrap_or(cli.threads);

    if engine_paths.black.path == engine_paths.white.path
        && engine_paths.black.source == engine_paths.white.source
    {
        let engine_path_display = engine_paths.black.path.display();
        let engine_path_source = engine_paths.black.source;
        println!("using engine binary: {engine_path_display} ({engine_path_source})");
    } else {
        println!(
            "using engine binaries: black={} ({}), white={} ({})",
            engine_paths.black.path.display(),
            engine_paths.black.source,
            engine_paths.white.path.display(),
            engine_paths.white.source
        );
    }
    if threads_black == threads_white {
        println!("threads: {threads_black}");
    } else {
        println!("threads: black={threads_black}, white={threads_white}");
    }
    if cli.concurrency > 1 {
        println!("concurrency: {}", cli.concurrency);
    }
    let common_args = cli.engine_args.clone().unwrap_or_default();
    let black_args = cli.engine_args_black.clone().unwrap_or_else(|| common_args.clone());
    let white_args = cli.engine_args_white.clone().unwrap_or(common_args.clone());

    let common_usi_opts = cli.usi_options.clone().unwrap_or_default();
    let black_usi_opts = cli.usi_options_black.clone().unwrap_or_else(|| common_usi_opts.clone());
    let white_usi_opts = cli.usi_options_white.clone().unwrap_or_else(|| common_usi_opts.clone());

    let native_mode = cli.native.unwrap_or(true);
    if native_mode
        && (has_entering_king_rule_option(&black_usi_opts)
            || has_entering_king_rule_option(&white_usi_opts))
    {
        eprintln!(
            "warning: NativeBackend ignores the EnteringKingRule USI option and uses CSARule27"
        );
    }
    if !native_mode
        && (cli.progress_file.is_some()
            || cli.bucket_mode.is_some()
            || cli.progress_buckets.is_some())
    {
        bail!(
            "--progress-file, --bucket-mode, and --progress-buckets are only supported with \
             --native=true. In USI mode, pass LS_BUCKET_MODE, LS_PROGRESS_BUCKETS, and (for \
             progresskpabs) LS_PROGRESS_COEFF with --usi-option."
        );
    }
    if cli.fv_scale < 0 {
        bail!("--fv-scale must be 0 (auto) or a positive value");
    }
    if !native_mode && cli.fv_scale != 0 {
        bail!(
            "--fv-scale is only supported with --native=true. \
             In USI mode, pass the engine option directly with --usi-option FV_SCALE=<value>."
        );
    }
    if native_mode && cli.fv_scale > 0 {
        set_fv_scale_override(cli.fv_scale);
        eprintln!("NativeBackend: FV_SCALE override {}", cli.fv_scale);
    }
    let entering_king_rule_black = if native_mode {
        EnteringKingRule::default()
    } else {
        entering_king_rule_from_options(&black_usi_opts)?
    };
    let entering_king_rule_white = if native_mode {
        EnteringKingRule::default()
    } else {
        entering_king_rule_from_options(&white_usi_opts)?
    };
    // USI モードかつ先後同一エンジンなら 1 プロセスで兼用する最適化。
    // TT/履歴が先後で共有されるため棋力評価対局（tournament）では不可だが、
    // gensfen は教師局面生成専用のため常に有効化して問題ない。
    let usi_single = !native_mode
        && engine_paths.black.path == engine_paths.white.path
        && black_args == white_args
        && black_usi_opts == white_usi_opts
        && threads_black == threads_white;
    if usi_single {
        eprintln!(
            "USI single-engine mode: {} process{} (instead of {})",
            cli.concurrency,
            if cli.concurrency == 1 { "" } else { "es" },
            cli.concurrency * 2
        );
    }
    let startpos_no_repeat_resolved = cli.startpos_no_repeat.unwrap_or(true);

    if startpos_no_repeat_resolved && cli.random_startpos {
        eprintln!("warning: --random-startpos is ignored when --startpos-no-repeat is active");
    }

    // depth/nodes 指定時は時間管理パラメータをデフォルト 0 にする。
    // YO 等の USI エンジンでは MinimumThinkingTime/NetworkDelay のデフォルト値が
    // nodes モードでも探索に影響するため、明示指定がない場合は干渉を防ぐ。
    let has_fixed_limit = cli.depth.is_some() || cli.nodes.is_some();
    if has_fixed_limit {
        if cli.network_delay.is_none() {
            cli.network_delay = Some(0);
        }
        if cli.network_delay2.is_none() {
            cli.network_delay2 = Some(0);
        }
        if cli.minimum_thinking_time.is_none() {
            cli.minimum_thinking_time = Some(0);
        }
    }

    // seed は開始局面選択と各 game_id の乱択を再現するため、全モードで固定する。
    let shuffle_seed_resolved: Option<u64> = {
        if let Some(ref state) = resume_state {
            // resume: meta から seed を復元
            let meta_seed = state.shuffle_seed;
            if let Some(cli_seed) = cli.shuffle_seed
                && meta_seed != Some(cli_seed)
            {
                bail!(
                    "--shuffle-seed {} does not match meta seed {:?}. \
                         Resume requires the same seed to restore the startpos order.",
                    cli_seed,
                    meta_seed
                );
            }
            Some(meta_seed.context("resume meta has no shuffle_seed; start a new run")?)
        } else if let Some(seed) = cli.shuffle_seed {
            Some(seed)
        } else {
            Some(rand::random::<u64>())
        }
    };
    let keep_tt_resolved = cli.keep_tt.unwrap_or(false);
    let dedup_hash_size_resolved = cli.dedup_hash_size.unwrap_or(64 * 1024 * 1024);
    let random_multi_pv_resolved = cli.random_multi_pv.unwrap_or(0);
    let random_multi_pv_diff_resolved = cli.random_multi_pv_diff.unwrap_or(0);

    let mut native_progress_routing_buckets = None;
    let mut native_eval_file_sha256 = None;
    let mut native_progress_file_sha256 = None;
    let mut native_eval_bytes = None;
    let mut native_progress_bytes = None;
    if native_mode {
        let eval_file =
            cli.eval_file.as_ref().ok_or_else(|| anyhow!("--native requires --eval-file"))?;
        let eval_bytes = std::fs::read(eval_file)
            .with_context(|| format!("failed to read --eval-file {}", eval_file.display()))?;
        native_eval_file_sha256 = Some(sha256_hex(&eval_bytes));
        native_eval_bytes = Some(eval_bytes);
        if let Some(path) = &cli.progress_file {
            let bytes = std::fs::read(path)
                .with_context(|| format!("failed to read --progress-file {}", path.display()))?;
            let loaded_sha256 = sha256_hex(&bytes);
            if let Some(state) = &resume_state {
                validate_resume_progress_content(
                    state.progress_file_sha256.as_deref(),
                    &loaded_sha256,
                )?;
            }
            native_progress_file_sha256 = Some(loaded_sha256);
            native_progress_bytes = Some(bytes);
        }
        if !cli.resume {
            // 新規 run は初期化失敗で不完全な meta を残さないよう、meta 永続化より先に検証する。
            native_progress_routing_buckets = initialize_native_backend(
                eval_file,
                native_eval_bytes.as_deref().context("native eval bytes missing")?,
                cli.progress_file.as_deref().zip(native_progress_bytes.as_deref()),
                cli.bucket_mode.as_deref(),
                cli.progress_buckets,
            )?;
        }
    }

    let eval_file_sha256 = if native_mode {
        native_eval_file_sha256
    } else {
        cli.eval_file.as_deref().map(sha256_file).transpose()?
    };
    let startpos_file_sha256 = cli.startpos_file.as_deref().map(sha256_file).transpose()?;
    let start_positions_sha256 = sha256_hex(&serde_json::to_vec(&start_commands)?);
    let native_executable_sha256 = native_mode
        .then(|| std::env::current_exe().context("failed to resolve current executable"))
        .transpose()?
        .map(|path| sha256_file(&path))
        .transpose()?;
    let engine_black_sha256 = if native_mode {
        native_executable_sha256.clone()
    } else {
        Some(sha256_file(&engine_paths.black.path)?)
    };
    let engine_white_sha256 = if native_mode {
        native_executable_sha256
    } else {
        Some(sha256_file(&engine_paths.white.path)?)
    };
    let usi_path_options_black = if native_mode {
        Vec::new()
    } else {
        usi_option_path_fingerprints(&black_usi_opts)?
    };
    let usi_path_options_white = if native_mode {
        Vec::new()
    } else {
        usi_option_path_fingerprints(&white_usi_opts)?
    };
    let model_fingerprint = build_model_fingerprint(
        native_mode,
        cli.eval_file.as_ref().map(|path| path.display().to_string()),
        eval_file_sha256,
        cli.progress_file.as_ref().map(|path| path.display().to_string()),
        native_progress_file_sha256.clone(),
        cli.bucket_mode.clone(),
        cli.progress_buckets,
        cli.fv_scale,
    );
    let mut generation_fingerprint = serde_json::json!({
        "schema": 2,
        "model": model_fingerprint,
        "engine": serde_json::json!({
            "path_black": engine_paths.black.path.display().to_string(),
            "path_white": engine_paths.white.path.display().to_string(),
            "sha256_black": engine_black_sha256,
            "sha256_white": engine_white_sha256,
            "args_black": black_args,
            "args_white": white_args,
            "usi_options_black": black_usi_opts,
            "usi_options_white": white_usi_opts,
            "usi_path_options_black": usi_path_options_black,
            "usi_path_options_white": usi_path_options_white,
            "threads_black": threads_black,
            "threads_white": threads_white,
            "hash_mb": cli.hash_mb,
            "keep_tt": keep_tt_resolved,
        }),
        "search": serde_json::json!({
            "max_moves": cli.max_moves,
            "btime": cli.btime,
            "wtime": cli.wtime,
            "binc": cli.binc,
            "winc": cli.winc,
            "byoyomi": cli.byoyomi,
            "depth": cli.depth,
            "nodes": cli.nodes,
            "timeout_margin_ms": cli.timeout_margin_ms,
            "network_delay": cli.network_delay,
            "network_delay2": cli.network_delay2,
            "minimum_thinking_time": cli.minimum_thinking_time,
            "slowmover": cli.slowmover,
            "ponder": cli.ponder,
            "concurrency": cli.concurrency,
        }),
        "start": serde_json::json!({
            "file": cli.startpos_file.as_ref().map(|path| path.display().to_string()),
            "file_sha256": startpos_file_sha256,
            "sfen": cli.sfen,
            "positions_sha256": start_positions_sha256,
            "random_startpos": cli.random_startpos,
            "no_repeat": startpos_no_repeat_resolved,
            "seed": shuffle_seed_resolved,
        }),
        "training": serde_json::json!({
            "skip_initial_ply": cli.skip_initial_ply,
            "skip_in_check": cli.skip_in_check,
            "format": cli.training_data_format,
            "hcpe3_policy_total": cli.hcpe3_policy_total,
            "hcpe3_policy_temp": cli.hcpe3_policy_temp,
            "game_id_sidecar": game_id_sidecar_path.is_some(),
            "dedup_hash_size": dedup_hash_size_resolved,
        }),
        "auxiliary_outputs": serde_json::json!({
            "info": cli.log_info,
            "eval": cli.emit_eval_file,
            "metrics": cli.emit_metrics,
        }),
        "randomization": serde_json::json!({
            "multi_pv": random_multi_pv_resolved,
            "multi_pv_diff": random_multi_pv_diff_resolved,
            "move_count": cli.random_move_count,
            "move_min_ply": cli.random_move_min_ply,
            "move_max_ply": cli.random_move_max_ply,
        }),
    });
    if cli.omit_diversions {
        // 未指定時はキーを出さない (既存 run の fingerprint 互換)。full/slim の混在は
        // relabel_psv --deblunder の境界判定を silent に壊すため resume で拒否する。
        generation_fingerprint["auxiliary_outputs"]["omit_diversions"] = serde_json::json!(true);
    }
    add_hcpe3_policy_fingerprint(
        &mut generation_fingerprint,
        training_format,
        cli.hcpe3_eval_drop_threshold,
    );
    if let Some(state) = &resume_state {
        validate_resume_fingerprint(state.fingerprint.as_ref(), &generation_fingerprint)?;
    }

    if !cli.resume {
        let output_stem =
            output_path.file_stem().and_then(|stem| stem.to_str()).unwrap_or("output");
        let output_parent = output_path.parent().unwrap_or_else(|| Path::new("."));
        let prefix = format!("{output_stem}.w");
        for entry in std::fs::read_dir(output_parent)? {
            let entry = entry?;
            if entry.file_name().to_string_lossy().starts_with(&prefix)
                && std::fs::symlink_metadata(entry.path())?.len() > 0
            {
                bail!(
                    "worker temp {} already exists and is non-empty; use --resume or move it aside",
                    entry.path().display()
                );
            }
        }
    }

    // Write meta line to final JSONL (resume時はスキップ: 既にメタ行が存在する)
    if !cli.resume {
        let mut writer = BufWriter::new(
            create_new_file_nofollow(&output_path)
                .with_context(|| format!("failed to open {}", output_path.display()))?,
        );
        let meta = MetaLog {
            kind: "meta".to_string(),
            timestamp: timestamp.to_rfc3339(),
            settings: MetaSettings {
                games: cli.games,
                max_moves: cli.max_moves,
                btime: cli.btime,
                wtime: cli.wtime,
                binc: cli.binc,
                winc: cli.winc,
                byoyomi: cli.byoyomi,
                depth: cli.depth,
                nodes: cli.nodes,
                timeout_margin_ms: cli.timeout_margin_ms,
                threads: cli.threads,
                threads_black,
                threads_white,
                hash_mb: cli.hash_mb,
                network_delay: cli.network_delay,
                network_delay2: cli.network_delay2,
                minimum_thinking_time: cli.minimum_thinking_time,
                slowmover: cli.slowmover,
                ponder: cli.ponder,
                flush_each_move: cli.flush_each_move,
                emit_eval_file: cli.emit_eval_file,
                emit_metrics: cli.emit_metrics,
                startpos_file: cli.startpos_file.as_ref().map(|p| p.display().to_string()),
                sfen: cli.sfen.clone(),
                random_startpos: cli.random_startpos,
                output_training_data: Some(training_data_path.display().to_string()),
                game_id_sidecar: game_id_sidecar_path.as_ref().map(|p| p.display().to_string()),
                skip_initial_ply: cli.skip_initial_ply,
                skip_in_check: cli.skip_in_check,
                shuffle_seed: shuffle_seed_resolved,
                progress_file: cli.progress_file.as_ref().map(|p| p.display().to_string()),
                progress_file_sha256: native_progress_file_sha256.clone(),
                bucket_mode: cli.bucket_mode.clone(),
                progress_buckets: cli.progress_buckets,
                fv_scale: (cli.fv_scale > 0).then_some(cli.fv_scale),
                omit_diversions: cli.omit_diversions.then_some(true),
            },
            engine_cmd: EngineCommandMeta {
                path_black: engine_paths.black.path.display().to_string(),
                path_white: engine_paths.white.path.display().to_string(),
                source_black: engine_paths.black.source.to_string(),
                source_white: engine_paths.white.source.to_string(),
                args_black: black_args.clone(),
                args_white: white_args.clone(),
                usi_options_black: black_usi_opts.clone(),
                usi_options_white: white_usi_opts.clone(),
            },
            start_positions: start_commands.clone(),
            output: output_path.display().to_string(),
            info_log: cli.log_info.then(|| info_path.display().to_string()),
            fingerprint: generation_fingerprint,
        };
        serde_json::to_writer(&mut writer, &meta)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
    }

    // gensfen: 共有ハッシュ重複検出テーブル（全ワーカーで1つ共有、tanuki-と同じ構成）
    let shared_dedup_hash = if dedup_hash_size_resolved > 0 {
        eprintln!(
            "DedupHash: {} entries ({} MB)",
            dedup_hash_size_resolved,
            dedup_hash_size_resolved * 8 / (1024 * 1024)
        );
        Some(Arc::new(SharedDedupHash::new(dedup_hash_size_resolved)))
    } else {
        None
    };
    if cli.resume && shared_dedup_hash.is_some() {
        eprintln!(
            "warning: --resume starts with an empty dedup table; duplicates against data committed before the restart are not detected"
        );
    }

    // dedup 警告の重複抑制フラグ（全ワーカー共有）
    let dedup_warn_emitted = Arc::new(AtomicBool::new(false));

    // ゲームチケットは逐次生成する。
    // `--games` が極端に大きい場合でも O(1) メモリで dispatch できるようにする。
    use rand::SeedableRng;
    let mut rng = rand::rngs::StdRng::seed_from_u64(
        shuffle_seed_resolved.context("run seed missing")? ^ 0x5354_4152_5450_4f53,
    );
    let startpos_count = start_defs.len();

    // Compute temp file paths per worker
    let output_stem = output_path.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
    let output_parent = output_path.parent().unwrap_or_else(|| Path::new("."));

    // Create channels (small buffer to decouple dispatch from result collection)
    let (ticket_tx, ticket_rx) = chan::bounded::<Option<GameTicket>>(cli.concurrency);
    let (result_tx, result_rx) = chan::bounded::<WorkerGameResult>(cli.concurrency);

    // Shutdown flag
    let shutdown = Arc::new(AtomicBool::new(false));
    {
        let sd = shutdown.clone();
        ctrlc::set_handler(move || {
            if sd.load(Ordering::Relaxed) {
                // 2回目以降: 強制終了
                eprintln!("\nForce exit.");
                std::process::exit(1);
            }
            eprintln!("\nShutting down gracefully... (press Ctrl-C again to force exit)");
            sd.store(true, Ordering::Relaxed);
        })
        .ok();
    }

    // Wrap shared data in Arc to avoid per-worker cloning
    let shared_start_defs = Arc::new(start_defs);
    let shared_start_commands = Arc::new(start_commands);

    // Spawn worker threads
    let mut handles = Vec::new();
    let mut temp_jsonl_paths = Vec::new();
    let mut temp_info_paths = Vec::new();
    let mut temp_eval_paths = Vec::new();
    let mut temp_metrics_paths = Vec::new();
    let mut temp_pack_paths = Vec::new();
    let mut temp_game_id_paths = Vec::new();

    for w in 0..cli.concurrency {
        let jsonl_path = output_parent.join(format!("{output_stem}.w{w}.jsonl"));
        let w_info_path = if cli.log_info {
            Some(output_parent.join(format!("{output_stem}.w{w}.info.jsonl")))
        } else {
            None
        };
        let w_eval_path = if cli.emit_eval_file {
            Some(output_parent.join(format!("{output_stem}.w{w}.eval.txt")))
        } else {
            None
        };
        let w_metrics_path = if cli.emit_metrics {
            Some(output_parent.join(format!("{output_stem}.w{w}.metrics.jsonl")))
        } else {
            None
        };
        let w_training_path =
            Some(output_parent.join(format!("{output_stem}.w{w}.{training_data_ext}")));
        let w_game_id_path = game_id_sidecar_path
            .as_ref()
            .map(|_| output_parent.join(format!("{output_stem}.w{w}.game_ids.bin")));

        temp_jsonl_paths.push(jsonl_path.clone());
        if let Some(ref p) = w_info_path {
            temp_info_paths.push(p.clone());
        }
        if let Some(ref p) = w_eval_path {
            temp_eval_paths.push(p.clone());
        }
        if let Some(ref p) = w_metrics_path {
            temp_metrics_paths.push(p.clone());
        }
        if let Some(ref p) = w_training_path {
            temp_pack_paths.push(p.clone());
        }
        if let Some(ref p) = w_game_id_path {
            temp_game_id_paths.push(p.clone());
        }

        if cli.resume {
            let recovered = recover_worker_checkpoint(
                &jsonl_path,
                w_training_path.as_deref(),
                w_game_id_path.as_deref(),
                w_info_path.as_deref(),
                w_eval_path.as_deref(),
                w_metrics_path.as_deref(),
                training_format,
                w,
                cli.games,
            )?;
            let state = resume_state.as_mut().context("resume state missing")?;
            state.completed_games.merge(&recovered.completed_games)?;
            state.black_wins += recovered.black_wins;
            state.white_wins += recovered.white_wins;
            state.draws += recovered.draws;
        }
    }

    let all_games_completed = resume_state.as_ref().is_some_and(|state| {
        state.completed_games.len() >= cli.games
            && (1..=cli.games).all(|game_id| state.completed_games.contains(game_id))
    });
    if let Some(state) = &resume_state {
        if all_games_completed {
            println!("全{}局が checkpoint 上で完了済みです。成果物を連結します。", cli.games);
        }
        println!(
            "resume checkpoints restored: {}/{} games (black {} / white {} / draw {})",
            state.completed_games.len(),
            cli.games,
            state.black_wins,
            state.white_wins,
            state.draws
        );
    }

    if !all_games_completed {
        if native_mode && cli.resume {
            // checkpoint だけで完了できる resume では大きな NNUE をロードする必要がない。
            native_progress_routing_buckets = initialize_native_backend(
                cli.eval_file.as_deref().context("native eval path missing")?,
                native_eval_bytes.as_deref().context("native eval bytes missing")?,
                cli.progress_file.as_deref().zip(native_progress_bytes.as_deref()),
                cli.bucket_mode.as_deref(),
                cli.progress_buckets,
            )?;
        }
        for w in 0..cli.concurrency {
            let cfg = WorkerConfig {
                worker_id: w,
                engine_path_black: engine_paths.black.path.clone(),
                engine_path_white: engine_paths.white.path.clone(),
                black_args: black_args.clone(),
                white_args: white_args.clone(),
                threads_black,
                threads_white,
                hash_mb: cli.hash_mb,
                network_delay: cli.network_delay,
                network_delay2: cli.network_delay2,
                minimum_thinking_time: cli.minimum_thinking_time,
                slowmover: cli.slowmover,
                ponder: cli.ponder,
                black_usi_opts: black_usi_opts.clone(),
                white_usi_opts: white_usi_opts.clone(),
                entering_king_rule_black,
                entering_king_rule_white,
                max_moves: cli.max_moves,
                timeout_margin_ms: cli.timeout_margin_ms,
                btime: cli.btime,
                wtime: cli.wtime,
                binc: cli.binc,
                winc: cli.winc,
                byoyomi: cli.byoyomi,
                go_depth: cli.depth,
                go_nodes: cli.nodes,
                start_defs: Arc::clone(&shared_start_defs),
                start_commands: Arc::clone(&shared_start_commands),
                jsonl_path: temp_jsonl_paths[w].clone(),
                info_path: cli.log_info.then(|| temp_info_paths[w].clone()),
                eval_path: cli.emit_eval_file.then(|| temp_eval_paths[w].clone()),
                metrics_path: cli.emit_metrics.then(|| temp_metrics_paths[w].clone()),
                training_data_path: Some(temp_pack_paths[w].clone()),
                game_id_sidecar_path: game_id_sidecar_path
                    .as_ref()
                    .map(|_| temp_game_id_paths[w].clone()),
                flush_each_move: cli.flush_each_move,
                fsync_interval_games: cli.fsync_interval_games,
                append_checkpoints: cli.resume,
                run_seed: shuffle_seed_resolved.context("run seed missing")?,
                skip_initial_ply: cli.skip_initial_ply,
                skip_in_check: cli.skip_in_check,
                training_format,
                hcpe3_policy_total: cli.hcpe3_policy_total,
                hcpe3_policy_temp: cli.hcpe3_policy_temp,
                hcpe3_eval_drop_threshold: cli.hcpe3_eval_drop_threshold,
                native_mode,
                usi_single,
                eval_hash_size_mb: DEFAULT_EVAL_HASH_SIZE_MB,
                progress_routing_buckets: native_progress_routing_buckets,
                keep_tt: keep_tt_resolved,
                dedup_hash: shared_dedup_hash.clone(),
                random_multi_pv: random_multi_pv_resolved,
                random_multi_pv_diff: random_multi_pv_diff_resolved,
                random_move_count: cli.random_move_count,
                random_move_min_ply: cli.random_move_min_ply,
                random_move_max_ply: cli.random_move_max_ply,
                dedup_warn_interval_per_worker: (cli.dedup_warn_interval / cli.concurrency as u32)
                    .max(1),
                dedup_warn_rate: cli.dedup_warn_rate,
                dedup_warn_emitted: Arc::clone(&dedup_warn_emitted),
                omit_diversions: cli.omit_diversions,
            };
            let rx = ticket_rx.clone();
            let tx = result_tx.clone();
            let sd = shutdown.clone();
            if native_mode {
                // NativeBackend は SearchWorker の再帰的 alpha-beta 探索で大きなスタックを使うため
                // 64MB スタックが必要（rshogi-usi の SEARCH_STACK_SIZE と同じ値）
                let builder = thread::Builder::new()
                    .name(format!("gensfen-worker-{w}"))
                    .stack_size(64 * 1024 * 1024);
                handles.push(
                    builder
                        .spawn(move || worker_main(cfg, rx, tx, sd))
                        .expect("failed to spawn worker thread"),
                );
            } else {
                handles.push(thread::spawn(move || worker_main(cfg, rx, tx, sd)));
            }
        }
    }
    // Main thread doesn't send results
    drop(result_tx);

    // Main loop: dispatch tickets and collect results
    //
    // game_id ごとに開始局面を決定し、完了済み ID も乱数列だけは消費する。
    let mut shuffled_startpos = if startpos_no_repeat_resolved {
        let seed = if let Some(s) = shuffle_seed_resolved {
            s
        } else {
            // 新規セッションなのに seed が無い = バグ（上流で必ず設定される）
            bail!("internal error: shuffle_seed not set for --startpos-no-repeat");
        };
        Some(ShuffledStartpos::new(startpos_count, seed))
    } else {
        None
    };
    let mut completed_games = resume_state
        .as_ref()
        .map_or_else(CompletedGames::default, |state| state.completed_games.clone());
    if completed_games.max_id().is_some_and(|max_id| max_id > cli.games) {
        bail!("--games is smaller than an already completed game_id");
    }
    let mut next_game_idx = 0u32;
    let next_incomplete_ticket = |next_game_idx: &mut u32,
                                  rng: &mut rand::rngs::StdRng,
                                  shuffled: &mut Option<ShuffledStartpos>,
                                  completed_games: &CompletedGames,
                                  target_games: u32| {
        while *next_game_idx < target_games {
            let game_idx = *next_game_idx;
            *next_game_idx += 1;
            let ticket = if let Some(s) = shuffled.as_mut() {
                GameTicket {
                    game_idx,
                    startpos_idx: s.next(),
                }
            } else {
                make_game_ticket(game_idx, cli.random_startpos, startpos_count, rng)
            };
            if !completed_games.contains(game_idx + 1) {
                return Some(ticket);
            }
        }
        None
    };
    let mut next_ticket = next_incomplete_ticket(
        &mut next_game_idx,
        &mut rng,
        &mut shuffled_startpos,
        &completed_games,
        cli.games,
    );
    let mut completed = completed_games.len();
    let mut black_wins = resume_state.as_ref().map_or(0, |s| s.black_wins);
    let mut white_wins = resume_state.as_ref().map_or(0, |s| s.white_wins);
    let mut draws = resume_state.as_ref().map_or(0, |s| s.draws);

    let handle_result = |result: WorkerGameResult,
                         completed_games: &mut CompletedGames,
                         black_wins: &mut u32,
                         white_wins: &mut u32,
                         draws: &mut u32,
                         completed: &mut u32,
                         target_games: u32|
     -> Result<()> {
        if !completed_games.insert(result.game_id) {
            bail!("worker returned duplicate completed game_id {}", result.game_id);
        }
        match result.outcome {
            GameOutcome::BlackWin => *black_wins += 1,
            GameOutcome::WhiteWin => *white_wins += 1,
            GameOutcome::Draw => *draws += 1,
            GameOutcome::InProgress => {}
        }
        *completed += 1;
        println!(
            "game {}/{}: {} ({}) - black {} / white {} / draw {}",
            completed,
            target_games,
            result.outcome.label(),
            result.outcome_reason.as_str(),
            black_wins,
            white_wins,
            draws
        );
        Ok(())
    };

    // 実行中の動的制御: <out-dir>/control.json の concurrency を対局境界で反映する。
    // worker スレッド数は固定のまま、同時 in-flight 対局数を絞る方式
    // (供給を止められた worker は ticket recv でブロックし CPU を消費しない)。
    let control_dir = output_path.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
    let control_path = control_dir.join("control.json");
    let control_history_path = control_dir.join("control_history.jsonl");
    let mut effective_concurrency = cli.concurrency;
    let mut target_games = cli.games;
    let mut last_control_poll: Option<Instant> = None;
    let mut stale_control_warned = false;
    println!(
        "[control] 実行中の動的制御: {} を {}ms 間隔でポーリング (例: echo '{{\"concurrency\":M,\"target_games\":N}}' > {}、concurrency 上限は --concurrency {}。target_games を発行済み対局数まで下げると安全に drain して finalize する)",
        control_path.display(),
        CONTROL_POLL_INTERVAL.as_millis(),
        control_path.display(),
        cli.concurrency,
    );
    let mut dispatched: u32 = 0;
    let mut session_completed: u32 = 0;
    // 送信済み game_id の最大値。drain 時の target 下限は「実際に worker へ送った範囲」
    // だけを含める (生成済みでも未送信の保留 ticket は取り消せるため含めない)。
    let mut max_dispatched_id: u32 = 0;
    // target 引き下げで供給対象外になった未送信 ticket の退避先。ticket の startpos は
    // game_idx 順の乱数消費で決まるため、破棄して再生成すると resume の再現と食い違う。
    // target が再度引き上げられたらここから供給に戻す。
    let mut parked_ticket: Option<GameTicket> = None;
    while completed < target_games && !shutdown.load(Ordering::Relaxed) {
        if last_control_poll.is_none_or(|t| t.elapsed() >= CONTROL_POLL_INTERVAL) {
            last_control_poll = Some(Instant::now());
            // 送信済み game_id は無効化できない (per-worker checkpoint と resume 検証が
            // game_id ≤ games を前提とする) ため、target の下限は送信済み範囲。
            let min_target = max_dispatched_id.max(completed_games.max_id().unwrap_or(0));
            apply_control(
                &control_path,
                &control_history_path,
                control_baseline,
                &mut stale_control_warned,
                &mut effective_concurrency,
                cli.concurrency,
                &mut target_games,
                min_target,
                completed,
            );
            if reconcile_pending_ticket(&mut next_ticket, &mut parked_ticket, target_games) {
                // target 引き上げ直後は供給を再開する。
                next_ticket = next_incomplete_ticket(
                    &mut next_game_idx,
                    &mut rng,
                    &mut shuffled_startpos,
                    &completed_games,
                    target_games,
                );
            }
        }
        let in_flight = dispatched.saturating_sub(session_completed) as usize;
        match next_ticket.take() {
            Some(t) if in_flight < effective_concurrency => {
                chan::select! {
                    send(ticket_tx, Some(t.clone())) -> res => {
                        if res.is_ok() {
                            dispatched += 1;
                            max_dispatched_id = max_dispatched_id.max(t.game_idx + 1);
                            next_ticket = next_incomplete_ticket(
                                &mut next_game_idx,
                                &mut rng,
                                &mut shuffled_startpos,
                                &completed_games,
                                target_games,
                            );
                        }
                    }
                    recv(result_rx) -> result => {
                        // Put the ticket back since we received a result instead of sending
                        next_ticket = Some(t);
                        if let Ok(result) = result {
                            session_completed += 1;
                            handle_result(result, &mut completed_games, &mut black_wins, &mut white_wins, &mut draws, &mut completed, target_games)?;
                        }
                    }
                }
            }
            // 供給するものが無い、または in-flight が制御値まで達している。
            // control.json の引き上げを取りこぼさないよう timeout 付きで結果を待つ。
            other => {
                next_ticket = other;
                match result_rx.recv_timeout(CONTROL_POLL_INTERVAL) {
                    Ok(result) => {
                        session_completed += 1;
                        handle_result(
                            result,
                            &mut completed_games,
                            &mut black_wins,
                            &mut white_wins,
                            &mut draws,
                            &mut completed,
                            target_games,
                        )?;
                    }
                    Err(chan::RecvTimeoutError::Timeout) => {}
                    Err(chan::RecvTimeoutError::Disconnected) => break,
                }
            }
        }
    }

    // Signal workers to stop
    for _ in 0..cli.concurrency {
        let _ = ticket_tx.try_send(None);
    }
    drop(ticket_tx); // チャネル閉鎖でワーカーの recv が終了する

    // グレースフルシャットダウン後、ワーカーが完了したゲームの結果を回収する。
    // Ctrl-C 後もワーカーは進行中のゲームを完了させるため、
    // メインスレッドのカウンタがずれないようここで drain する。
    while let Ok(result) = result_rx.recv() {
        handle_result(
            result,
            &mut completed_games,
            &mut black_wins,
            &mut white_wins,
            &mut draws,
            &mut completed,
            target_games,
        )?;
    }

    // Join workers and collect training stats
    let mut training_stats = TrainingStats::default();
    let mut worker_errors = Vec::new();
    for (worker_id, handle) in handles.into_iter().enumerate() {
        match handle.join() {
            Ok(Ok(output)) => training_stats.merge(output.training_stats),
            Ok(Err(error)) => worker_errors.push(format!("worker {worker_id}: {error:#}")),
            Err(_) => worker_errors.push(format!("worker {worker_id}: thread panicked")),
        }
    }

    if worker_errors.is_empty() && !all_games_completed {
        for worker_id in 0..cli.concurrency {
            validate_completed_worker_outputs(
                &temp_jsonl_paths[worker_id],
                &temp_pack_paths[worker_id],
                game_id_sidecar_path.as_ref().map(|_| temp_game_id_paths[worker_id].as_path()),
                cli.log_info.then(|| temp_info_paths[worker_id].as_path()),
                cli.emit_eval_file.then(|| temp_eval_paths[worker_id].as_path()),
                cli.emit_metrics.then(|| temp_metrics_paths[worker_id].as_path()),
                training_format,
                worker_id,
                // 実行中に target を引き上げた場合は cli.games を超える game_id が正当に存在する
                cli.games.max(target_games),
            )?;
        }
    } else if !worker_errors.is_empty() {
        for worker_id in 0..cli.concurrency {
            recover_worker_checkpoint(
                &temp_jsonl_paths[worker_id],
                Some(temp_pack_paths[worker_id].as_path()),
                game_id_sidecar_path.as_ref().map(|_| temp_game_id_paths[worker_id].as_path()),
                cli.log_info.then(|| temp_info_paths[worker_id].as_path()),
                cli.emit_eval_file.then(|| temp_eval_paths[worker_id].as_path()),
                cli.emit_metrics.then(|| temp_metrics_paths[worker_id].as_path()),
                training_format,
                worker_id,
                cli.games.max(target_games),
            )?;
        }
    }

    // worker checkpoint から全成果物を staging し、journal に固定してから置換する。
    let append_mode = cli.resume;
    let mut staged_outputs = Vec::new();
    if cli.log_info && !temp_info_paths.is_empty() {
        let staged = stage_concatenated_file(
            &info_path,
            &temp_info_paths,
            append_mode,
            cli.resume,
            finalized_output_for(prior_finalized_state.as_ref(), &info_path),
        )?;
        staged_outputs.push(staged);
    }
    if cli.emit_eval_file && !temp_eval_paths.is_empty() {
        let eval_path = default_eval_path(&output_path);
        let staged = stage_concatenated_file(
            &eval_path,
            &temp_eval_paths,
            append_mode,
            cli.resume,
            finalized_output_for(prior_finalized_state.as_ref(), &eval_path),
        )?;
        staged_outputs.push(staged);
    }
    if cli.emit_metrics && !temp_metrics_paths.is_empty() {
        let metrics_path = default_metrics_path(&output_path);
        let staged = stage_concatenated_file(
            &metrics_path,
            &temp_metrics_paths,
            append_mode,
            cli.resume,
            finalized_output_for(prior_finalized_state.as_ref(), &metrics_path),
        )?;
        staged_outputs.push(staged);
    }
    if !temp_pack_paths.is_empty() {
        let staged = stage_concatenated_file(
            &training_data_path,
            &temp_pack_paths,
            append_mode,
            cli.resume,
            finalized_output_for(prior_finalized_state.as_ref(), &training_data_path),
        )?;
        staged_outputs.push(staged);
    }
    if let Some(sidecar_path) = game_id_sidecar_path.as_deref() {
        let staged = stage_concatenated_file(
            sidecar_path,
            &temp_game_id_paths,
            append_mode,
            cli.resume,
            finalized_output_for(prior_finalized_state.as_ref(), sidecar_path),
        )?;
        staged_outputs.push(staged);
    }
    let staged_jsonl = stage_concatenated_file(
        &output_path,
        &temp_jsonl_paths,
        true,
        cli.resume,
        finalized_output_for(prior_finalized_state.as_ref(), &output_path),
    )?;
    staged_outputs.push(staged_jsonl);
    let worker_temps = temp_jsonl_paths
        .iter()
        .chain(&temp_info_paths)
        .chain(&temp_eval_paths)
        .chain(&temp_metrics_paths)
        .chain(&temp_pack_paths)
        .chain(&temp_game_id_paths)
        .cloned()
        .collect();
    let journal = FinalizationJournal {
        schema: 1,
        outputs: staged_outputs,
        worker_temps,
    };
    if injected_fault("before_journal_write") {
        bail!("injected failure before finalization journal write");
    }
    write_json_atomic(&finalization_journal_path(&output_path), &journal)?;
    complete_finalization(&output_path, &journal, false)?;

    if !worker_errors.is_empty() {
        eprintln!("gensfen stopped because worker errors occurred:");
        for error in &worker_errors {
            eprintln!("  - {error}");
        }
        bail!("{} worker(s) failed; committed game data was preserved", worker_errors.len());
    }

    // 最終サマリー
    let actual_games = black_wins + white_wins + draws;
    println!();
    println!("=== Result Summary ===");
    println!(
        "Total: {} games | Black wins: {} | White wins: {} | Draws: {}",
        actual_games, black_wins, white_wins, draws
    );
    if actual_games > 0 {
        let black_rate = (black_wins as f64 / actual_games as f64) * 100.0;
        let white_rate = (white_wins as f64 / actual_games as f64) * 100.0;
        let draw_rate = (draws as f64 / actual_games as f64) * 100.0;
        println!(
            "Win rate: Black {:.1}% | White {:.1}% | Draw {:.1}%",
            black_rate, white_rate, draw_rate
        );
    }
    println!();
    println!("--- Engine Settings ---");
    println!("Black: {}", format_engine_settings(&engine_paths.black, &black_usi_opts));
    println!("White: {}", format_engine_settings(&engine_paths.white, &white_usi_opts));
    println!("=======================");
    println!();

    // 学習データサマリー出力
    {
        println!();
        println!("--- Training Data ---");
        println!("Positions written in this invocation: {}", training_stats.total_written);
        if target_games != cli.games {
            println!(
                "target_games (final, via control.json): {} (--games {}; resume には {} 以上を指定)",
                target_games,
                cli.games,
                target_games.max(cli.games)
            );
        }
        println!(
            "Skipped (initial ply 1-{}): {}",
            cli.skip_initial_ply, training_stats.skipped_initial
        );
        if cli.skip_in_check {
            println!("Skipped (in check): {}", training_stats.skipped_in_check);
        }
        if training_stats.discarded_timeout_games > 0
            || training_stats.discarded_illegal_move_games > 0
            || training_stats.discarded_no_bestmove_games > 0
        {
            println!(
                "Discarded abnormal games: timeout={} illegal_move={} no_bestmove={} ({} collected positions discarded at game end)",
                training_stats.discarded_timeout_games,
                training_stats.discarded_illegal_move_games,
                training_stats.discarded_no_bestmove_games,
                training_stats.discarded_positions
            );
        }
        if training_stats.discarded_gap_positions > 0 {
            println!(
                "Discarded positions at replay gaps: {}",
                training_stats.discarded_gap_positions
            );
        }
        if training_stats.declaration_win_dedup_skipped_games > 0 {
            println!(
                "Declaration-win terminals skipped by dedup: {} games",
                training_stats.declaration_win_dedup_skipped_games
            );
        }
        println!("Output: {}", training_data_path.display());
        if let Some(path) = game_id_sidecar_path.as_deref() {
            println!("Game ID sidecar: {}", path.display());
        }
        println!("---------------------");
    }
    println!("gensfen log written to {}", output_path.display());
    if cli.log_info {
        println!("info log written to {}", info_path.display());
    }
    Ok(())
}

/// 出力ディレクトリを確定し、その中の gensfen.jsonl パスを返す。
fn resolve_output_path(out_dir: Option<&Path>, timestamp: &chrono::DateTime<Local>) -> PathBuf {
    let dir = match out_dir {
        Some(d) => d.to_path_buf(),
        None => PathBuf::from("runs/gensfen").join(timestamp.format("%Y%m%d-%H%M%S").to_string()),
    };
    dir.join("gensfen.jsonl")
}

fn default_eval_path(jsonl: &Path) -> PathBuf {
    let parent = jsonl.parent().unwrap_or_else(|| Path::new("."));
    let stem = jsonl.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
    parent.join(format!("{stem}.eval.txt"))
}

fn default_metrics_path(jsonl: &Path) -> PathBuf {
    let parent = jsonl.parent().unwrap_or_else(|| Path::new("."));
    let stem = jsonl.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
    parent.join(format!("{stem}.metrics.jsonl"))
}

fn default_training_data_path(jsonl: &Path, ext: &str) -> PathBuf {
    let parent = jsonl.parent().unwrap_or_else(|| Path::new("."));
    let stem = jsonl.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
    parent.join(format!("{stem}.{ext}"))
}

fn validate_fresh_output_paths(paths: &[PathBuf]) -> Result<()> {
    for path in paths {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) => {
                let kind = if metadata.file_type().is_symlink() {
                    "symbolic link"
                } else if metadata.is_dir() {
                    "directory"
                } else if metadata.is_file() {
                    "file"
                } else {
                    "filesystem entry"
                };
                bail!(
                    "final output {} already exists as a {kind}; move it aside before starting a new run",
                    path.display()
                );
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to inspect final output {}", path.display()));
            }
        }
    }
    Ok(())
}

fn validate_output_entry_types(
    output_jsonl: &Path,
    final_paths: &[PathBuf],
    training_data_ext: &str,
    concurrency: usize,
    log_info: bool,
    emit_eval_file: bool,
    emit_metrics: bool,
    emit_sidecar: bool,
    resume: bool,
) -> Result<()> {
    let journal = finalization_journal_path(output_jsonl);
    let journal_tmp = atomic_temp_path(&journal);
    let finalized = finalized_state_path(output_jsonl);
    let finalized_tmp = atomic_temp_path(&finalized);
    let journal_exists = std::fs::symlink_metadata(&journal).is_ok_and(|meta| meta.is_file());
    for path in final_paths {
        validate_existing_output_entry(path, resume, "final output")?;
    }
    for final_path in final_paths {
        validate_existing_output_entry(
            &merge_temp_path(final_path),
            resume,
            "merge temporary file",
        )?;
    }
    for path in worker_checkpoint_paths(
        output_jsonl,
        training_data_ext,
        concurrency,
        log_info,
        emit_eval_file,
        emit_metrics,
        emit_sidecar,
    ) {
        validate_existing_output_entry(&path, resume, "worker checkpoint")?;
    }
    validate_existing_output_entry(&journal, resume, "finalization journal")?;
    validate_existing_output_entry(
        &journal_tmp,
        resume && !journal_exists,
        "finalization journal temporary file",
    )?;
    validate_existing_output_entry(&finalized, resume, "finalized state")?;
    validate_existing_output_entry(
        &finalized_tmp,
        resume && journal_exists,
        "finalized state temporary file",
    )?;
    validate_existing_output_entry(
        &output_jsonl.with_file_name(".gensfen.lock"),
        true,
        "run lock",
    )?;
    Ok(())
}

fn worker_checkpoint_paths(
    output_jsonl: &Path,
    training_data_ext: &str,
    concurrency: usize,
    log_info: bool,
    emit_eval_file: bool,
    emit_metrics: bool,
    emit_sidecar: bool,
) -> Vec<PathBuf> {
    let parent = output_jsonl.parent().unwrap_or_else(|| Path::new("."));
    let stem = output_jsonl.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
    let mut paths = Vec::new();
    for worker in 0..concurrency {
        paths.push(parent.join(format!("{stem}.w{worker}.jsonl")));
        paths.push(parent.join(format!("{stem}.w{worker}.{training_data_ext}")));
        if emit_sidecar {
            paths.push(parent.join(format!("{stem}.w{worker}.game_ids.bin")));
        }
        if log_info {
            paths.push(parent.join(format!("{stem}.w{worker}.info.jsonl")));
        }
        if emit_eval_file {
            paths.push(parent.join(format!("{stem}.w{worker}.eval.txt")));
        }
        if emit_metrics {
            paths.push(parent.join(format!("{stem}.w{worker}.metrics.jsonl")));
        }
    }
    paths
}

fn validate_existing_output_entry(path: &Path, allow_regular: bool, label: &str) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && allow_regular => Ok(()),
        Ok(metadata) => {
            let kind = if metadata.file_type().is_symlink() {
                "symbolic link"
            } else if metadata.is_dir() {
                "directory"
            } else if metadata.is_file() {
                "file"
            } else {
                "special filesystem entry"
            };
            bail!("{label} {} already exists as a {kind}", path.display())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("failed to inspect {label} {}", path.display()))
        }
    }
}

fn validate_output_paths_unique(
    output_jsonl: &Path,
    training_data: &Path,
    sidecar: Option<&Path>,
    training_data_ext: &str,
    concurrency: usize,
    log_info: bool,
    emit_eval_file: bool,
    emit_metrics: bool,
) -> Result<()> {
    let mut paths = vec![("final JSONL".to_string(), output_jsonl.to_path_buf())];
    paths.push(("final training data".to_string(), training_data.to_path_buf()));
    if log_info {
        paths.push(("final info log".to_string(), output_jsonl.with_extension("info.jsonl")));
    }
    if emit_eval_file {
        paths.push(("final eval file".to_string(), default_eval_path(output_jsonl)));
    }
    if emit_metrics {
        paths.push(("final metrics".to_string(), default_metrics_path(output_jsonl)));
    }
    if let Some(sidecar) = sidecar {
        paths.push(("final game_id sidecar".to_string(), sidecar.to_path_buf()));
    }

    let parent = output_jsonl.parent().unwrap_or_else(|| Path::new("."));
    let stem = output_jsonl.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
    for worker in 0..concurrency {
        paths.push((
            format!("worker {worker} JSONL"),
            parent.join(format!("{stem}.w{worker}.jsonl")),
        ));
        paths.push((
            format!("worker {worker} training data"),
            parent.join(format!("{stem}.w{worker}.{training_data_ext}")),
        ));
        if sidecar.is_some() {
            paths.push((
                format!("worker {worker} game_id sidecar"),
                parent.join(format!("{stem}.w{worker}.game_ids.bin")),
            ));
        }
        if log_info {
            paths.push((
                format!("worker {worker} info log"),
                parent.join(format!("{stem}.w{worker}.info.jsonl")),
            ));
        }
        if emit_eval_file {
            paths.push((
                format!("worker {worker} eval file"),
                parent.join(format!("{stem}.w{worker}.eval.txt")),
            ));
        }
        if emit_metrics {
            paths.push((
                format!("worker {worker} metrics"),
                parent.join(format!("{stem}.w{worker}.metrics.jsonl")),
            ));
        }
    }

    let final_paths: Vec<PathBuf> = paths
        .iter()
        .filter(|(label, _)| label.starts_with("final "))
        .map(|(_, path)| path.clone())
        .collect();
    for (index, path) in final_paths.iter().enumerate() {
        paths.push((format!("staging file {index}"), merge_temp_path(path)));
    }
    let journal = finalization_journal_path(output_jsonl);
    let finalized = finalized_state_path(output_jsonl);
    paths.extend([
        ("finalization journal".to_string(), journal.clone()),
        ("finalization journal temporary file".to_string(), atomic_temp_path(&journal)),
        ("finalized state".to_string(), finalized.clone()),
        ("finalized state temporary file".to_string(), atomic_temp_path(&finalized)),
        ("run lock".to_string(), output_jsonl.with_file_name(".gensfen.lock")),
    ]);

    let mut normalized = std::collections::HashMap::<PathBuf, (String, PathBuf)>::new();
    #[cfg(unix)]
    let mut file_ids = std::collections::HashMap::<(u64, u64), (String, PathBuf)>::new();
    for (label, path) in paths {
        let identity = normalize_output_path(&path)?;
        if let Some((other_label, other_path)) =
            normalized.insert(identity, (label.clone(), path.clone()))
        {
            bail!(
                "output path collision: {label} {} conflicts with {other_label} {}",
                path.display(),
                other_path.display()
            );
        }
        #[cfg(unix)]
        if let Ok(metadata) = std::fs::metadata(&path) {
            use std::os::unix::fs::MetadataExt;
            if let Some((other_label, other_path)) =
                file_ids.insert((metadata.dev(), metadata.ino()), (label.clone(), path.clone()))
            {
                bail!(
                    "output file collision: {label} {} is the same file as {other_label} {}",
                    path.display(),
                    other_path.display()
                );
            }
        }
    }
    Ok(())
}

/// 未生成の出力ファイル同士を比較できるよう、存在する最深の祖先を canonicalize し、
/// 残りの未生成コンポーネントを実体パスに対して正規化する。
fn normalize_output_path(path: &Path) -> Result<PathBuf> {
    normalize_output_path_inner(path, 0)
}

fn normalize_output_path_inner(path: &Path, symlink_depth: usize) -> Result<PathBuf> {
    use std::path::Component;

    if symlink_depth > 40 {
        bail!("too many symlinks while normalizing output path {}", path.display());
    }

    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };

    let components: Vec<_> = absolute.components().collect();
    let mut ancestor_len = components.len();
    let ancestor = loop {
        let candidate: PathBuf = components[..ancestor_len].iter().collect();
        match std::fs::symlink_metadata(&candidate) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let target = std::fs::read_link(&candidate)?;
                let target = if target.is_absolute() {
                    target
                } else {
                    candidate.parent().unwrap_or_else(|| Path::new(".")).join(target)
                };
                break normalize_output_path_inner(&target, symlink_depth + 1)?;
            }
            Ok(_) => break candidate.canonicalize()?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        ancestor_len -= 1;
    };

    let mut normalized = ancestor;
    for component in &components[ancestor_len..] {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    Ok(normalized)
}

fn initialize_native_backend(
    eval_path: &Path,
    eval_bytes: &[u8],
    progress: Option<(&Path, &[u8])>,
    bucket_mode: Option<&str>,
    progress_buckets: Option<usize>,
) -> Result<Option<usize>> {
    init_nnue_from_bytes(eval_bytes).map_err(|e| anyhow!("NNUE init failed: {e}"))?;
    eprintln!("NativeBackend: NNUE loaded from {}", eval_path.display());
    let layer_stack_buckets =
        get_network().as_deref().and_then(|network| network.layer_stack_num_buckets());
    let progress_routing_buckets = match layer_stack_buckets {
        Some(stored_buckets) => {
            let mode_str =
                bucket_mode.context("--native LayerStacks NNUE requires --bucket-mode")?;
            let mode = parse_layer_stack_bucket_mode(mode_str).with_context(|| {
                format!("invalid --bucket-mode '{mode_str}' (expected progresskpabs or kingrank9)")
            })?;
            match (mode, progress) {
                (LayerStackBucketMode::ProgressKPAbs, Some((path, bytes))) => {
                    let weights = load_progress_coeff_kpabs_from_bytes(bytes).map_err(|e| {
                        anyhow!("failed to load --progress-file {}: {e}", path.display())
                    })?;
                    set_layer_stack_progress_kpabs_weights(weights)
                        .map_err(|e| anyhow!("failed to set --progress-file weights: {e}"))?;
                    eprintln!("NativeBackend: progress file loaded from {}", path.display());
                }
                (LayerStackBucketMode::ProgressKPAbs, None) => {
                    if layer_stack_progress_coeff_required(progress_buckets) {
                        bail!("--bucket-mode progresskpabs requires --progress-file")
                    }
                }
                (LayerStackBucketMode::KingRank9, Some(_)) => {
                    bail!("--bucket-mode kingrank9 does not use --progress-file")
                }
                (LayerStackBucketMode::KingRank9, None) => {}
                _ => unreachable!("known LayerStack bucket mode"),
            }
            configure_layer_stack_routing(mode, stored_buckets, progress_buckets)
                .map_err(anyhow::Error::msg)?;
            eprintln!(
                "NativeBackend: LayerStacks mode={} stored_buckets={} routing_buckets={}",
                mode.as_str(),
                stored_buckets,
                progress_buckets.unwrap_or(9)
            );
            (mode == LayerStackBucketMode::ProgressKPAbs)
                .then_some(progress_buckets.expect("validated progress bucket count"))
        }
        None if bucket_mode.is_some() || progress_buckets.is_some() || progress.is_some() => {
            bail!("LayerStacks routing options were provided for a non-LayerStacks network")
        }
        None => None,
    };
    Ok(progress_routing_buckets)
}

fn resolve_engine_paths(cli: &Cli) -> ResolvedEnginePaths {
    let shared = resolve_engine_path(cli);
    let black = cli
        .engine_path_black
        .as_ref()
        .map(|path| ResolvedEnginePath {
            path: path.clone(),
            source: "cli:black",
        })
        .unwrap_or_else(|| shared.clone());
    let white = cli
        .engine_path_white
        .as_ref()
        .map(|path| ResolvedEnginePath {
            path: path.clone(),
            source: "cli:white",
        })
        .unwrap_or_else(|| shared.clone());
    ResolvedEnginePaths { black, white }
}

/// エンジンバイナリを探す。明示指定 > 環境変数 > 同ディレクトリの release > debug > フォールバックの優先順位。
fn resolve_engine_path(cli: &Cli) -> ResolvedEnginePath {
    if let Some(path) = &cli.engine_path {
        return ResolvedEnginePath {
            path: path.clone(),
            source: "cli",
        };
    }
    if let Ok(p) = std::env::var("CARGO_BIN_EXE_engine-usi") {
        return ResolvedEnginePath {
            path: PathBuf::from(p),
            source: "cargo-env",
        };
    }
    if let Ok(exec) = std::env::current_exe()
        && let Some(dir) = exec.parent()
        && let Some(found) = find_engine_in_dir(dir)
    {
        return found;
    }
    ResolvedEnginePath {
        path: PathBuf::from("rshogi-usi"),
        source: "fallback",
    }
}

fn find_engine_in_dir(dir: &Path) -> Option<ResolvedEnginePath> {
    #[cfg(windows)]
    let release_names = ["rshogi-usi.exe"];
    #[cfg(not(windows))]
    let release_names = ["rshogi-usi"];
    #[cfg(windows)]
    let debug_names = ["rshogi-usi-debug.exe"];
    #[cfg(not(windows))]
    let debug_names = ["rshogi-usi-debug"];

    for name in release_names {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(ResolvedEnginePath {
                path: candidate,
                source: "auto:release",
            });
        }
    }
    for name in debug_names {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(ResolvedEnginePath {
                path: candidate,
                source: "auto:debug",
            });
        }
    }
    None
}

fn eval_label(eval: Option<&EvalLog>) -> String {
    let Some(eval) = eval else {
        return "?".to_string();
    };
    if let Some(mate) = eval.score_mate {
        return format!("mate{mate}");
    }
    if let Some(cp) = eval.score_cp {
        return format!("{cp:+}");
    }
    "?".to_string()
}

/// エンジン設定を人間可読な形式でフォーマットする
fn format_engine_settings(engine: &ResolvedEnginePath, usi_options: &[String]) -> String {
    let engine_name = engine.path.file_name().and_then(|s| s.to_str()).unwrap_or("rshogi-usi");

    if usi_options.is_empty() {
        format!("{engine_name} (default)")
    } else {
        format!("{engine_name} [{}]", usi_options.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use clap::Parser;
    use rand::{SeedableRng, rngs::StdRng};
    use rshogi_core::types::RepetitionState;
    use std::path::PathBuf;

    #[test]
    fn resolve_engine_paths_uses_per_side_when_provided() {
        let cli = Cli::parse_from([
            "gensfen",
            "--engine-path-black",
            "/path/to/black",
            "--engine-path-white",
            "/path/to/white",
        ]);
        let paths = resolve_engine_paths(&cli);
        assert_eq!(paths.black.path, PathBuf::from("/path/to/black"));
        assert_eq!(paths.white.path, PathBuf::from("/path/to/white"));
        assert_eq!(paths.black.source, "cli:black");
        assert_eq!(paths.white.source, "cli:white");
    }

    #[test]
    fn resolve_engine_paths_uses_shared_when_per_side_missing() {
        let cli = Cli::parse_from(["gensfen", "--engine-path", "/shared/path/engine-usi"]);
        let paths = resolve_engine_paths(&cli);
        assert_eq!(paths.black.path, PathBuf::from("/shared/path/engine-usi"));
        assert_eq!(paths.white.path, PathBuf::from("/shared/path/engine-usi"));
        assert_eq!(paths.black.source, "cli");
        assert_eq!(paths.white.source, "cli");
    }

    #[test]
    fn make_game_ticket_cycles_startpos_indices_when_not_random() {
        let mut rng = StdRng::seed_from_u64(1);
        let tickets: Vec<_> = (0..6)
            .map(|game_idx| make_game_ticket(game_idx, false, 4, &mut rng).startpos_idx)
            .collect();
        assert_eq!(tickets, vec![0, 1, 2, 3, 0, 1]);
    }

    #[test]
    fn make_game_ticket_random_startpos_stays_in_range() {
        let mut rng = StdRng::seed_from_u64(1);
        for game_idx in 0..128 {
            let ticket = make_game_ticket(game_idx, true, 5, &mut rng);
            assert!(ticket.startpos_idx < 5);
        }
    }

    #[test]
    fn final_entering_king_meta_startpos_has_no_points() {
        let mut pos = Position::new();
        pos.set_hirate();
        let meta = final_entering_king_meta(&pos);

        assert_eq!(meta.black.points, 0);
        assert_eq!(meta.white.points, 0);
        assert!(!meta.black.king_in_enemy);
        assert!(!meta.white.king_in_enemy);
        assert_eq!(meta.black.enemy_zone_pieces, 0);
        assert_eq!(meta.white.enemy_zone_pieces, 0);
    }

    #[test]
    fn final_entering_king_meta_counts_enemy_zone_and_hand() {
        let mut pos = Position::new();
        pos.set_sfen("KGG6/SS7/PPPPPP3/9/9/9/2pppppp1/1ss1gg1nl/4k2nl b 2R2B3p 1")
            .expect("sfen");
        let meta = final_entering_king_meta(&pos);

        assert_eq!(meta.black.points, 30);
        assert!(meta.black.king_in_enemy);
        assert_eq!(meta.black.enemy_zone_pieces, 10);
        assert_eq!(meta.white.points, 17);
        assert!(meta.white.king_in_enemy);
        assert_eq!(meta.white.enemy_zone_pieces, 14);
    }

    fn play_moves(
        pos: &mut Position,
        history: &mut GameRepetitionHistory,
        moves: &[&str],
    ) -> Option<(GameOutcome, OutcomeReason)> {
        let mut repetition = None;
        for usi in moves {
            let mv = Move::from_usi(usi).expect("USI move");
            assert!(is_legal_with_pass(pos, mv), "illegal fixture move: {usi}");
            let mover = pos.side_to_move();
            let gives_check = pos.gives_check(mv);
            pos.do_move(mv, gives_check);
            repetition = history.record_move(pos, mover, gives_check);
        }
        repetition
    }

    #[test]
    fn repetition_draw_is_adjudicated_only_on_fourth_occurrence() {
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/9/9/9/9/9/9/4K4 b - 1").unwrap();
        let mut history = GameRepetitionHistory::new(&pos);
        let cycle = ["5i4i", "5a4a", "4i5i", "4a5a"];
        assert!(play_moves(&mut pos, &mut history, &cycle).is_none());
        assert!(play_moves(&mut pos, &mut history, &cycle).is_none());
        let repetition = play_moves(&mut pos, &mut history, &cycle);
        assert_eq!(pos.repetition_state(0), RepetitionState::Draw);
        let (outcome, reason) = repetition.unwrap();
        assert!(outcome == GameOutcome::Draw);
        assert_eq!(reason, OutcomeReason::Sennichite);
    }

    #[test]
    fn repetition_history_adjudicates_a_cycle_longer_than_core_window() {
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/9/9/9/9/9/9/4K4 b - 1").unwrap();
        let mut history = GameRepetitionHistory::new(&pos);
        let cycle = [
            "5i4i", "5a4a", "4i3i", "4a3a", "3i3h", "3a3b", "3h3g", "3b3c", "3g4g", "3c4c", "4g5g",
            "4c5c", "5g6g", "5c6c", "6g6h", "6c6b", "6h6i", "6b6a", "6i5i", "6a5a",
        ];

        assert!(play_moves(&mut pos, &mut history, &cycle).is_none());
        assert!(play_moves(&mut pos, &mut history, &cycle).is_none());
        let repetition = play_moves(&mut pos, &mut history, &cycle);

        assert_eq!(pos.repetition_state(0), RepetitionState::None);
        let (outcome, reason) = repetition.unwrap();
        assert!(outcome == GameOutcome::Draw);
        assert_eq!(reason, OutcomeReason::Sennichite);
    }

    #[test]
    fn perpetual_check_win_is_current_side_perspective() {
        let mut pos = Position::new();
        pos.set_sfen("4k4/4R4/9/9/9/9/9/9/K8 w - 1").unwrap();
        let mut history = GameRepetitionHistory::new(&pos);
        let cycle = ["5a4a", "5b4b", "4a5a", "4b5b"];
        let mut repetition = None;
        for _ in 0..3 {
            repetition = play_moves(&mut pos, &mut history, &cycle);
        }
        assert_eq!(pos.side_to_move(), Color::White);
        assert_eq!(pos.repetition_state(0), RepetitionState::Win);
        let (outcome, reason) = repetition.unwrap();
        assert!(outcome == GameOutcome::WhiteWin);
        assert_eq!(reason, OutcomeReason::PerpetualCheck);
        assert_eq!(game_result_for_side(outcome, Color::White), 1);
        assert_eq!(game_result_for_side(outcome, Color::Black), -1);
    }

    #[test]
    fn perpetual_check_lose_is_current_side_perspective() {
        let mut pos = Position::new();
        pos.set_sfen("5k3/4R4/9/9/9/9/9/9/K8 b - 1").unwrap();
        let mut history = GameRepetitionHistory::new(&pos);
        let cycle = ["5b4b", "4a5a", "4b5b", "5a4a"];
        let mut repetition = None;
        for _ in 0..3 {
            repetition = play_moves(&mut pos, &mut history, &cycle);
        }
        assert_eq!(pos.side_to_move(), Color::Black);
        assert_eq!(pos.repetition_state(0), RepetitionState::Lose);
        let (outcome, reason) = repetition.unwrap();
        assert!(outcome == GameOutcome::WhiteWin);
        assert_eq!(reason, OutcomeReason::PerpetualCheck);
        assert_eq!(game_result_for_side(outcome, Color::White), 1);
        assert_eq!(game_result_for_side(outcome, Color::Black), -1);
    }

    #[test]
    fn invalid_external_bestmove_win_is_rejected() {
        let mut pos = Position::new();
        pos.set_hirate();
        assert!(!is_valid_bestmove_win(&pos, EnteringKingRule::Point27));

        pos.set_sfen("KGG6/SS7/PPPPPP3/9/9/9/2pppppp1/1ss1gg1nl/4k2nl b 2R2B3p 1")
            .unwrap();
        assert!(is_valid_bestmove_win(&pos, EnteringKingRule::Point27));
    }

    #[test]
    fn try_rule_requires_the_returned_king_move_instead_of_bestmove_win() {
        let mut pos = Position::new();
        pos.set_sfen("3K5/9/9/9/9/9/9/9/4k4 b 2r2b4g4s4n4l18p 1").unwrap();
        let action = declaration_win_action(&pos, EnteringKingRule::TryRule);
        let DeclarationWinAction::PlayMove(mv) = action else {
            panic!("TryRule must return the king move");
        };
        assert_eq!(mv.to_usi(), "6a5a");
        assert!(!is_valid_bestmove_win(&pos, EnteringKingRule::TryRule));
        assert!(!is_valid_bestmove_win(&pos, EnteringKingRule::None));
    }

    #[test]
    fn entering_king_rule_follows_usi_option() {
        assert_eq!(entering_king_rule_from_options(&[]).unwrap(), EnteringKingRule::Point27);
        assert_eq!(
            entering_king_rule_from_options(&["EnteringKingRule=CSARule24".to_string()]).unwrap(),
            EnteringKingRule::Point24
        );
        assert!(
            entering_king_rule_from_options(&["EnteringKingRule=invalid".to_string()]).is_err()
        );
        assert_eq!(
            entering_king_rule_from_options(&["EnteringKingRule=NoEnteringKing".to_string()])
                .unwrap(),
            EnteringKingRule::None
        );
        assert_eq!(
            entering_king_rule_from_options(&["EnteringKingRule=TryRule".to_string()]).unwrap(),
            EnteringKingRule::TryRule
        );
        assert!(has_entering_king_rule_option(&["EnteringKingRule = TryRule".to_string()]));
        assert!(!has_entering_king_rule_option(&["Threads=2".to_string()]));
    }

    #[test]
    fn cli_rejects_zero_concurrency() {
        let cli = Cli::parse_from(["gensfen", "--concurrency", "0"]);
        assert!(validate_cli(&cli).is_err());
    }

    #[test]
    fn cli_requires_multipv_diff_when_random_multipv_is_enabled() {
        let missing = Cli::parse_from(["gensfen", "--random-multi-pv", "4"]);
        assert!(validate_cli(&missing).is_err());
        let explicit = Cli::parse_from([
            "gensfen",
            "--random-multi-pv",
            "4",
            "--random-multi-pv-diff",
            "100",
        ]);
        assert!(validate_cli(&explicit).is_ok());
        let disabled = Cli::parse_from(["gensfen"]);
        assert!(validate_cli(&disabled).is_ok());
    }

    #[test]
    fn training_disposition_discards_only_abnormal_reasons() {
        for reason in [
            OutcomeReason::Timeout,
            OutcomeReason::IllegalMove,
            OutcomeReason::NoBestmove,
        ] {
            assert!(!TrainingDisposition::from_outcome_reason(reason).is_adopted());
        }
        for reason in [
            OutcomeReason::Mate,
            OutcomeReason::Resign,
            OutcomeReason::Win,
            OutcomeReason::MaxMoves,
            OutcomeReason::Sennichite,
            OutcomeReason::PerpetualCheck,
        ] {
            assert!(TrainingDisposition::from_outcome_reason(reason).is_adopted());
        }
    }

    #[test]
    fn outcome_reason_serialization_uses_as_str() {
        for reason in [
            OutcomeReason::Mate,
            OutcomeReason::Resign,
            OutcomeReason::Win,
            OutcomeReason::Sennichite,
            OutcomeReason::PerpetualCheck,
            OutcomeReason::MaxMoves,
            OutcomeReason::Timeout,
            OutcomeReason::IllegalMove,
            OutcomeReason::NoBestmove,
        ] {
            assert_eq!(serde_json::to_value(reason).unwrap(), reason.as_str());
        }
    }

    #[test]
    fn shared_dedup_hash_detects_duplicates() {
        let dh = SharedDedupHash::new(1024);
        // 初回挿入は false
        assert!(!dh.check_and_insert(12345));
        // 2回目は重複検出で true
        assert!(dh.check_and_insert(12345));
        // 別のキーは false
        assert!(!dh.check_and_insert(67890));
        // key=0 の特殊扱い（内部で 1 に変換）
        assert!(!dh.check_and_insert(0));
        assert!(dh.check_and_insert(0));
    }

    #[test]
    fn shared_dedup_hash_overwrites_on_collision() {
        // サイズ 2 のテーブル（mask=1）で衝突を強制
        let dh = SharedDedupHash::new(2);
        // key=2 と key=4 は同じスロット（idx = key & 1 = 0）
        assert!(!dh.check_and_insert(2));
        // key=4 が上書き
        assert!(!dh.check_and_insert(4));
        // key=2 は上書きされているので false（新規扱い）
        assert!(!dh.check_and_insert(2));
    }

    #[test]
    fn abnormal_games_do_not_publish_pending_dedup_keys() {
        for reason in [
            AbnormalEndReason::Timeout,
            AbnormalEndReason::IllegalMove,
            AbnormalEndReason::NoBestmove,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("abnormal.psv");
            let mut collector =
                TrainingDataCollector::new(&path, 0, false, TrainingFormat::Psv, 1000, 600.0, None)
                    .unwrap();
            let dedup = SharedDedupHash::new(1024);
            let mut pending = PendingDedupKeys::default();
            let mut hits = 0;
            let mut discarded = 0;
            let mut interval_hits = 0;
            let mut interval_checked = 0;
            let mut pos = Position::new();
            pos.set_hirate();
            let mv = Move::from_usi("7g7f").unwrap();

            assert!(!check_training_position_dedup(
                Some(&dedup),
                &mut pending,
                pos.key(),
                Some(&mut collector),
                &mut hits,
                &mut discarded,
                &mut interval_hits,
                &mut interval_checked,
            ));
            collector.record_position(&pos, Some(10), None, Some(mv), mv, &[]);
            collector
                .finish_game(GameOutcome::WhiteWin, TrainingDisposition::Discard(reason), 1)
                .unwrap();

            let mut next_game_pending = PendingDedupKeys::default();
            assert!(!check_training_position_dedup(
                Some(&dedup),
                &mut next_game_pending,
                pos.key(),
                Some(&mut collector),
                &mut hits,
                &mut discarded,
                &mut interval_hits,
                &mut interval_checked,
            ));
        }
    }

    #[test]
    fn pending_dedup_keys_detect_duplicates_within_game() {
        let dedup = SharedDedupHash::new(1024);
        let mut pending = PendingDedupKeys::default();
        let mut hits = 0;
        let mut discarded = 0;
        let mut interval_hits = 0;
        let mut interval_checked = 0;

        assert!(!check_training_position_dedup(
            Some(&dedup),
            &mut pending,
            12345,
            None,
            &mut hits,
            &mut discarded,
            &mut interval_hits,
            &mut interval_checked,
        ));
        assert!(check_training_position_dedup(
            Some(&dedup),
            &mut pending,
            12345,
            None,
            &mut hits,
            &mut discarded,
            &mut interval_hits,
            &mut interval_checked,
        ));
        assert_eq!((hits, discarded, interval_hits, interval_checked), (1, 0, 1, 2));
        assert!(!dedup.contains(12345));
    }

    #[test]
    fn pending_dedup_collision_keeps_exact_set_semantics_through_publish() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("collision.psv");
        let mut collector =
            TrainingDataCollector::new(&path, 0, false, TrainingFormat::Psv, 1000, 600.0, None)
                .unwrap();
        let dedup = SharedDedupHash::new(2);
        let mut pending = PendingDedupKeys::default();
        let mut hits = 0;
        let mut discarded = 0;
        let mut interval_hits = 0;
        let mut interval_checked = 0;
        let mut pos = Position::new();
        pos.set_hirate();
        let mv = Move::from_usi("7g7f").unwrap();

        assert!(!check_training_position_dedup(
            Some(&dedup),
            &mut pending,
            2,
            Some(&mut collector),
            &mut hits,
            &mut discarded,
            &mut interval_hits,
            &mut interval_checked,
        ));
        collector.record_position(&pos, Some(10), None, Some(mv), mv, &[]);
        assert!(!check_training_position_dedup(
            Some(&dedup),
            &mut pending,
            4,
            Some(&mut collector),
            &mut hits,
            &mut discarded,
            &mut interval_hits,
            &mut interval_checked,
        ));
        collector.record_position(&pos, Some(20), None, Some(mv), mv, &[]);
        assert!(check_training_position_dedup(
            Some(&dedup),
            &mut pending,
            2,
            Some(&mut collector),
            &mut hits,
            &mut discarded,
            &mut interval_hits,
            &mut interval_checked,
        ));
        assert_eq!((hits, discarded, interval_hits, interval_checked), (1, 2, 1, 3));

        pending.publish(&dedup);
        assert!(!dedup.contains(2));
        assert!(dedup.contains(4));
    }

    #[test]
    fn shared_hit_remains_pending_after_colliding_publish() {
        let dedup = SharedDedupHash::new(2);
        assert!(!dedup.check_and_insert(2));

        let mut game_pending = PendingDedupKeys::default();
        assert!(game_pending.check_and_stage(&dedup, 2));
        assert_eq!(game_pending.ordered, vec![2]);

        let mut colliding_pending = PendingDedupKeys::default();
        assert!(!colliding_pending.check_and_stage(&dedup, 4));
        colliding_pending.publish(&dedup);
        assert!(!dedup.contains(2));
        assert!(dedup.contains(4));

        assert!(game_pending.check_and_stage(&dedup, 2));
        assert_eq!(game_pending.ordered, vec![2]);
    }

    #[test]
    fn adopted_game_publishes_pending_dedup_keys() {
        let dedup = SharedDedupHash::new(1024);
        let mut first_game_pending = PendingDedupKeys::default();
        assert!(!first_game_pending.check_and_stage(&dedup, 12345));
        first_game_pending.publish(&dedup);

        let mut next_game_pending = PendingDedupKeys::default();
        assert!(next_game_pending.check_and_stage(&dedup, 12345));
    }

    #[test]
    fn shuffled_startpos_covers_all_indices() {
        let mut s = ShuffledStartpos::new(5, 42);
        let mut seen = std::collections::HashSet::new();
        // 5 回取得すれば全インデックスが出る
        for _ in 0..5 {
            seen.insert(s.next());
        }
        assert_eq!(seen.len(), 5);
        for i in 0..5 {
            assert!(seen.contains(&i));
        }
    }

    #[test]
    fn shuffled_startpos_reshuffles_after_exhaustion() {
        let mut s = ShuffledStartpos::new(3, 42);
        // 1周目
        let first_round: Vec<_> = (0..3).map(|_| s.next()).collect();
        assert_eq!(first_round.iter().collect::<std::collections::HashSet<_>>().len(), 3);
        // 2周目（リシャッフル後も全インデックスが出る）
        let second_round: Vec<_> = (0..3).map(|_| s.next()).collect();
        assert_eq!(second_round.iter().collect::<std::collections::HashSet<_>>().len(), 3);
    }

    #[test]
    fn shuffled_startpos_is_reproducible_with_same_seed() {
        // 同じ seed + count なら同一の順列が再構築できる
        let mut s1 = ShuffledStartpos::new(100, 12345);
        let mut s2 = ShuffledStartpos::new(100, 12345);
        let seq1: Vec<_> = (0..200).map(|_| s1.next()).collect(); // 2周分
        let seq2: Vec<_> = (0..200).map(|_| s2.next()).collect();
        assert_eq!(seq1, seq2);
    }

    #[test]
    fn shuffled_startpos_resume_skips_correctly() {
        // resume: 同じ seed で構築し、completed 分だけ next() を呼び進めて
        // 残りが元の続きと一致することを確認
        let mut full = ShuffledStartpos::new(50, 99);
        let first_30: Vec<_> = (0..30).map(|_| full.next()).collect();
        let remaining_20: Vec<_> = (0..20).map(|_| full.next()).collect();

        // resume: seed=99 で再構築、30 回スキップ
        let mut resumed = ShuffledStartpos::new(50, 99);
        for _ in 0..30 {
            resumed.next();
        }
        let resumed_20: Vec<_> = (0..20).map(|_| resumed.next()).collect();
        assert_eq!(remaining_20, resumed_20);

        // first_30 に重複がないことも確認
        let unique: std::collections::HashSet<_> = first_30.iter().collect();
        assert_eq!(unique.len(), 30);
    }

    #[test]
    fn select_multipv_random_filters_by_threshold() {
        use rshogi_core::types::Move;

        let mv1 = Move::from_usi("7g7f").unwrap();
        let mv2 = Move::from_usi("2g2f").unwrap();
        let mv3 = Move::from_usi("3g3f").unwrap();

        let candidates = vec![
            MultiPvCandidate {
                multipv: 1,
                score_cp: 100,
                score_mate: None,
                first_move: mv1,
            },
            MultiPvCandidate {
                multipv: 2,
                score_cp: 80,
                score_mate: None,
                first_move: mv2,
            },
            MultiPvCandidate {
                multipv: 3,
                score_cp: -200,
                score_mate: None,
                first_move: mv3,
            },
        ];

        let mut rng = StdRng::seed_from_u64(42);
        // 閾値 50 なら PV1(100) と PV2(80) のみ対象、PV3(-200) は除外
        for _ in 0..20 {
            let selected = select_multipv_random(&candidates, 50, &mut rng);
            assert!(selected.is_some());
            let mv = selected.unwrap().mv;
            assert!(mv == mv1 || mv == mv2);
        }
    }

    #[test]
    fn select_multipv_random_returns_none_for_empty() {
        let mut rng = StdRng::seed_from_u64(42);
        assert!(select_multipv_random(&[], 100, &mut rng).is_none());
    }

    #[test]
    fn select_multipv_random_reports_score_gap() {
        let candidates = vec![
            legal_candidate(1, 100, "7g7f"),
            legal_candidate(2, 75, "2g2f"),
        ];
        let mut rng = StdRng::seed_from_u64(0);
        let mut saw_gap = false;
        for _ in 0..64 {
            let selected = select_multipv_random(&candidates, 100, &mut rng).unwrap();
            if selected.mv == candidates[1].first_move {
                assert_eq!(selected.score_gap_cp, 25);
                saw_gap = true;
                break;
            }
        }
        assert!(saw_gap);
    }

    #[test]
    fn apply_control_clamps_to_max_and_ignores_invalid() {
        let dir = tempfile::tempdir().expect("tempdir");
        let control = dir.path().join("control.json");
        let history = dir.path().join("control_history.jsonl");
        let mut effective = 8usize;
        let mut target = 1000u32;
        let mut warned = false;

        // ファイル不在は無視
        apply_control(
            &control,
            &history,
            std::time::SystemTime::UNIX_EPOCH,
            &mut warned,
            &mut effective,
            8,
            &mut target,
            0,
            0,
        );
        assert_eq!(effective, 8);

        std::fs::write(&control, r#"{"concurrency":3}"#).unwrap();
        apply_control(
            &control,
            &history,
            std::time::SystemTime::UNIX_EPOCH,
            &mut warned,
            &mut effective,
            8,
            &mut target,
            0,
            10,
        );
        assert_eq!(effective, 3);

        // 上限超過は --concurrency に clamp
        std::fs::write(&control, r#"{"concurrency":100}"#).unwrap();
        apply_control(
            &control,
            &history,
            std::time::SystemTime::UNIX_EPOCH,
            &mut warned,
            &mut effective,
            8,
            &mut target,
            0,
            20,
        );
        assert_eq!(effective, 8);

        // 0 とパース不能は無視して現状維持
        std::fs::write(&control, r#"{"concurrency":0}"#).unwrap();
        apply_control(
            &control,
            &history,
            std::time::SystemTime::UNIX_EPOCH,
            &mut warned,
            &mut effective,
            8,
            &mut target,
            0,
            30,
        );
        assert_eq!(effective, 8);
        std::fs::write(&control, "not json").unwrap();
        apply_control(
            &control,
            &history,
            std::time::SystemTime::UNIX_EPOCH,
            &mut warned,
            &mut effective,
            8,
            &mut target,
            0,
            40,
        );
        assert_eq!(effective, 8);
        assert_eq!(target, 1000);

        // 変更 2 回分だけ履歴が残る
        let lines = std::fs::read_to_string(&history).unwrap();
        assert_eq!(lines.lines().count(), 2);
    }

    #[test]
    fn apply_control_target_games_clamps_to_issued_floor() {
        let dir = tempfile::tempdir().expect("tempdir");
        let control = dir.path().join("control.json");
        let history = dir.path().join("control_history.jsonl");
        let mut effective = 4usize;
        let mut target = 1000u32;
        let mut warned = false;

        // 引き上げは無制限
        std::fs::write(&control, r#"{"target_games":2000}"#).unwrap();
        apply_control(
            &control,
            &history,
            std::time::SystemTime::UNIX_EPOCH,
            &mut warned,
            &mut effective,
            4,
            &mut target,
            50,
            40,
        );
        assert_eq!(target, 2000);

        // 発行済み範囲より下へは clamp (= drain)
        std::fs::write(&control, r#"{"target_games":0}"#).unwrap();
        apply_control(
            &control,
            &history,
            std::time::SystemTime::UNIX_EPOCH,
            &mut warned,
            &mut effective,
            4,
            &mut target,
            50,
            45,
        );
        assert_eq!(target, 50);
        assert_eq!(effective, 4);

        // 同時指定も反映される
        std::fs::write(&control, r#"{"concurrency":2,"target_games":60}"#).unwrap();
        apply_control(
            &control,
            &history,
            std::time::SystemTime::UNIX_EPOCH,
            &mut warned,
            &mut effective,
            4,
            &mut target,
            50,
            50,
        );
        assert_eq!(effective, 2);
        assert_eq!(target, 60);

        let lines = std::fs::read_to_string(&history).unwrap();
        assert_eq!(lines.lines().count(), 3);
    }

    #[test]
    fn apply_control_ignores_file_older_than_baseline() {
        let dir = tempfile::tempdir().expect("tempdir");
        let control = dir.path().join("control.json");
        let history = dir.path().join("control_history.jsonl");
        let mut effective = 8usize;
        let mut target = 1000u32;
        let mut warned = false;

        std::fs::write(&control, r#"{"concurrency":2,"target_games":0}"#).unwrap();
        let baseline = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
        apply_control(
            &control,
            &history,
            baseline,
            &mut warned,
            &mut effective,
            8,
            &mut target,
            0,
            0,
        );
        assert_eq!(effective, 8);
        assert_eq!(target, 1000);
        assert!(warned);
        assert!(!history.exists());
    }

    #[test]
    fn model_fingerprint_omits_fv_scale_key_when_auto() {
        let auto = build_model_fingerprint(
            true,
            Some("eval.bin".into()),
            Some("evalsha".into()),
            Some("progress.bin".into()),
            Some("progsha".into()),
            Some("progresskpabs".into()),
            Some(8),
            0,
        );
        assert!(auto.get("fv_scale").is_none());
        // 同型 4 引数の位置対応を pin する (取り違えると既存 run の resume が全滅する)
        assert_eq!(auto.get("eval_file").and_then(|v| v.as_str()), Some("eval.bin"));
        assert_eq!(auto.get("eval_file_sha256").and_then(|v| v.as_str()), Some("evalsha"));
        assert_eq!(auto.get("progress_file").and_then(|v| v.as_str()), Some("progress.bin"));
        assert_eq!(auto.get("progress_file_sha256").and_then(|v| v.as_str()), Some("progsha"));
        assert_eq!(auto.get("bucket_mode").and_then(|v| v.as_str()), Some("progresskpabs"));
        assert_eq!(auto.get("progress_buckets").and_then(|v| v.as_u64()), Some(8));
        let overridden = build_model_fingerprint(true, None, None, None, None, None, None, 14);
        assert_eq!(overridden.get("fv_scale").and_then(|v| v.as_i64()), Some(14));
        assert!(overridden.get("bucket_mode").is_none());
        assert!(overridden.get("progress_buckets").is_none());
    }

    #[test]
    fn reconcile_pending_ticket_parks_and_restores_same_ticket() {
        let ticket = GameTicket {
            game_idx: 50,
            startpos_idx: 7,
        };
        let mut next = Some(ticket.clone());
        let mut parked = None;

        // target 引き下げ (game_id 51 > 50) → park。戻り値 true で呼び出し側は新規生成を
        // 試みるが、next_game_idx が target に達しているため None が返るだけ
        assert!(reconcile_pending_ticket(&mut next, &mut parked, 50));
        assert!(next.is_none());
        assert_eq!(parked.as_ref().map(|t| t.game_idx), Some(50));

        // target 据え置きの間は park されたまま
        assert!(reconcile_pending_ticket(&mut next, &mut parked, 50));
        assert!(parked.is_some());

        // target 引き上げ → 同じ ticket (同じ startpos_idx) が供給に戻る
        assert!(!reconcile_pending_ticket(&mut next, &mut parked, 51));
        let restored = next.expect("restored");
        assert_eq!(restored.game_idx, ticket.game_idx);
        assert_eq!(restored.startpos_idx, ticket.startpos_idx);
        assert!(parked.is_none());
    }

    #[test]
    fn validate_resume_progress_file_requires_exact_path_match() {
        assert!(validate_resume_progress_file(None, None).is_ok());
        assert!(
            validate_resume_progress_file(
                Some("/tmp/progress.bin"),
                Some(Path::new("/tmp/progress.bin"))
            )
            .is_ok()
        );
        assert!(
            validate_resume_progress_file(
                Some("/tmp/progress.bin"),
                Some(Path::new("/tmp/./progress.bin"))
            )
            .is_err()
        );
        assert!(validate_resume_progress_file(Some("/tmp/progress.bin"), None).is_err());
        assert!(validate_resume_progress_file(None, Some(Path::new("/tmp/progress.bin"))).is_err());
    }

    #[test]
    fn validate_resume_progress_content_checks_sha256() {
        assert!(validate_resume_progress_content(Some("ab"), "ab").is_ok());
        // パスが同じでも内容 SHA-256 が異なる（係数差し替え）なら拒否する
        assert!(validate_resume_progress_content(Some("ab"), "cd").is_err());
        // SHA-256 記録が無い meta からの再開は照合をスキップして許容する
        assert!(validate_resume_progress_content(None, "ab").is_ok());
    }

    #[test]
    fn parse_resume_state_restores_progress_file_from_meta() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("gensfen.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"meta","settings":{"shuffle_seed":7,"progress_file":"/tmp/progress.bin","progress_file_sha256":"abcd"}}"#,
                "\n",
                r#"{"type":"result","game_id":3,"outcome":"black_win"}"#,
                "\n"
            ),
        )
        .expect("write resume jsonl");

        let state = parse_resume_state(&path, 10).expect("parse resume state");
        assert_eq!(state.completed_games.len(), 1);
        assert!(state.completed_games.contains(3));
        assert_eq!(state.shuffle_seed, Some(7));
        assert_eq!(state.progress_file.as_deref(), Some("/tmp/progress.bin"));
        assert_eq!(state.progress_file_sha256.as_deref(), Some("abcd"));
    }

    #[test]
    fn resume_completed_games_preserves_gaps() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gaps.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"meta","settings":{}}"#,
                "\n",
                r#"{"type":"result","game_id":1,"outcome":"draw"}"#,
                "\n",
                r#"{"type":"result","game_id":3,"outcome":"draw"}"#,
                "\n"
            ),
        )
        .unwrap();
        let state = parse_resume_state(&path, 10).unwrap();
        assert!(state.completed_games.contains(1));
        assert!(!state.completed_games.contains(2));
        assert!(state.completed_games.contains(3));
        assert_eq!(state.completed_games.len(), 2);
    }

    #[test]
    fn final_resume_rejects_invalid_outcome_before_completing_game() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("invalid-outcome.jsonl");
        std::fs::write(
            &path,
            "{\"type\":\"meta\",\"settings\":{}}\n{\"type\":\"result\",\"game_id\":1,\"outcome\":\"unknown\"}\n",
        )
        .unwrap();
        assert!(parse_resume_state(&path, 1).unwrap_err().to_string().contains("outcome"));
    }

    #[test]
    fn final_resume_rejects_out_of_range_game_id_before_bitset_growth() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge-id.jsonl");
        std::fs::write(
            &path,
            format!(
                "{{\"type\":\"meta\",\"settings\":{{}}}}\n{{\"type\":\"result\",\"game_id\":{},\"outcome\":\"draw\"}}\n",
                u32::MAX
            ),
        )
        .unwrap();
        let error = parse_resume_state(&path, 10).unwrap_err().to_string();
        assert!(error.contains("outside 1..=10"));
    }

    #[test]
    fn resume_gap_keeps_start_position_bound_to_original_game_id() {
        let expected: Vec<usize> = {
            let mut shuffled = ShuffledStartpos::new(8, 99);
            (0..4).map(|_| shuffled.next()).collect()
        };
        let mut completed = CompletedGames::default();
        completed.insert(1);
        completed.insert(3);
        let mut resumed = ShuffledStartpos::new(8, 99);
        let missing: Vec<(u32, usize)> = (1..=4)
            .filter_map(|game_id| {
                let startpos = resumed.next();
                (!completed.contains(game_id)).then_some((game_id, startpos))
            })
            .collect();
        assert_eq!(missing, vec![(2, expected[1]), (4, expected[3])]);
    }

    #[test]
    fn worker_checkpoint_truncates_uncommitted_psv_sidecar_and_json_tail() {
        let dir = tempfile::tempdir().unwrap();
        let jsonl = dir.path().join("gensfen.w0.jsonl");
        let psv = dir.path().join("gensfen.w0.psv");
        let sidecar = dir.path().join("gensfen.w0.game_ids.bin");
        let committed = serde_json::json!({
            "type": "result",
            "worker_id": 0,
            "game_id": 2,
            "outcome": "draw",
            "training_bytes": 40,
            "sidecar_bytes": 4,
            "fsync_boundary": true,
        });
        std::fs::write(&jsonl, format!("{committed}\n{{\"type\":\"res")).unwrap();
        std::fs::write(&psv, vec![0u8; 47]).unwrap();
        std::fs::write(&sidecar, vec![0u8; 7]).unwrap();

        let state = recover_worker_checkpoint(
            &jsonl,
            Some(&psv),
            Some(&sidecar),
            None,
            None,
            None,
            TrainingFormat::Psv,
            0,
            10,
        )
        .unwrap();
        assert!(state.completed_games.contains(2));
        assert_eq!(std::fs::metadata(psv).unwrap().len(), 40);
        assert_eq!(std::fs::metadata(sidecar).unwrap().len(), 4);
        assert!(std::fs::read_to_string(jsonl).unwrap().ends_with('\n'));
    }

    #[test]
    fn worker_checkpoint_without_commit_discards_only_uncommitted_tail() {
        let dir = tempfile::tempdir().unwrap();
        let jsonl = dir.path().join("gensfen.w0.jsonl");
        let psv = dir.path().join("gensfen.w0.psv");
        std::fs::write(&jsonl, b"").unwrap();
        std::fs::write(&psv, vec![0u8; 40]).unwrap();
        let state = recover_worker_checkpoint(
            &jsonl,
            Some(&psv),
            None,
            None,
            None,
            None,
            TrainingFormat::Psv,
            0,
            10,
        )
        .unwrap();
        assert_eq!(state.completed_games.len(), 0);
        assert_eq!(std::fs::metadata(psv).unwrap().len(), 0);
    }

    #[test]
    fn worker_checkpoint_discards_results_ahead_of_durable_teacher_data() {
        let dir = tempfile::tempdir().unwrap();
        let jsonl = dir.path().join("gensfen.w0.jsonl");
        let psv = dir.path().join("gensfen.w0.psv");
        let sidecar = dir.path().join("gensfen.w0.game_ids.bin");
        let first = "{\"type\":\"result\",\"worker_id\":0,\"game_id\":1,\"outcome\":\"draw\",\"training_bytes\":40,\"sidecar_bytes\":4,\"fsync_boundary\":true}\n";
        let second = "{\"type\":\"result\",\"worker_id\":0,\"game_id\":2,\"outcome\":\"black_win\",\"training_bytes\":80,\"sidecar_bytes\":8,\"fsync_boundary\":true}\n";
        std::fs::write(&jsonl, format!("{first}{second}")).unwrap();
        std::fs::write(&psv, vec![0u8; PackedSfenValue::SIZE]).unwrap();
        std::fs::write(&sidecar, vec![0u8; 8]).unwrap();

        let state = recover_worker_checkpoint(
            &jsonl,
            Some(&psv),
            Some(&sidecar),
            None,
            None,
            None,
            TrainingFormat::Psv,
            0,
            2,
        )
        .unwrap();

        assert!(state.completed_games.contains(1));
        assert!(!state.completed_games.contains(2));
        assert_eq!(state.draws, 1);
        assert_eq!(state.black_wins, 0);
        assert_eq!(std::fs::read_to_string(jsonl).unwrap(), first);
        assert_eq!(std::fs::metadata(psv).unwrap().len(), PackedSfenValue::SIZE as u64);
        assert_eq!(std::fs::metadata(sidecar).unwrap().len(), 4);
    }

    #[test]
    fn worker_checkpoint_rejects_invalid_result_record_boundary_before_truncating() {
        let dir = tempfile::tempdir().unwrap();
        let jsonl = dir.path().join("gensfen.w0.jsonl");
        let psv = dir.path().join("gensfen.w0.psv");
        let row = "{\"type\":\"result\",\"worker_id\":0,\"game_id\":1,\"outcome\":\"draw\",\"training_bytes\":41,\"fsync_boundary\":true}\n";
        std::fs::write(&jsonl, row).unwrap();
        std::fs::write(&psv, vec![0u8; PackedSfenValue::SIZE]).unwrap();

        let error = recover_worker_checkpoint(
            &jsonl,
            Some(&psv),
            None,
            None,
            None,
            None,
            TrainingFormat::Psv,
            0,
            1,
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("record boundary"));
        assert_eq!(std::fs::read_to_string(jsonl).unwrap(), row);
        assert_eq!(std::fs::metadata(psv).unwrap().len(), PackedSfenValue::SIZE as u64);
    }

    #[test]
    fn legacy_worker_checkpoint_fails_closed_without_truncating() {
        let dir = tempfile::tempdir().unwrap();
        let jsonl = dir.path().join("gensfen.w0.jsonl");
        let psv = dir.path().join("gensfen.w0.psv");
        std::fs::write(
            &jsonl,
            b"{\"type\":\"result\",\"worker_id\":0,\"game_id\":1,\"outcome\":\"draw\"}\n",
        )
        .unwrap();
        std::fs::write(&psv, vec![0u8; 40]).unwrap();
        let error = recover_worker_checkpoint(
            &jsonl,
            Some(&psv),
            None,
            None,
            None,
            None,
            TrainingFormat::Psv,
            0,
            10,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("move temp files aside"));
        assert_eq!(std::fs::metadata(psv).unwrap().len(), 40);
    }

    #[test]
    fn worker_checkpoint_validates_all_offsets_before_truncating() {
        let dir = tempfile::tempdir().unwrap();
        let jsonl = dir.path().join("gensfen.w0.jsonl");
        let psv = dir.path().join("gensfen.w0.psv");
        let rows = concat!(
            "{\"type\":\"result\",\"worker_id\":0,\"game_id\":1,\"outcome\":\"draw\",\"training_bytes\":80,\"fsync_boundary\":true}\n",
            "{\"type\":\"result\",\"worker_id\":0,\"game_id\":2,\"outcome\":\"draw\",\"training_bytes\":40,\"fsync_boundary\":true}\n"
        );
        std::fs::write(&jsonl, rows).unwrap();
        std::fs::write(&psv, vec![0u8; 120]).unwrap();
        let error = recover_worker_checkpoint(
            &jsonl,
            Some(&psv),
            None,
            None,
            None,
            None,
            TrainingFormat::Psv,
            0,
            2,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("non-monotonic training_bytes"));
        assert_eq!(std::fs::metadata(&psv).unwrap().len(), 120);
    }

    #[test]
    fn path_valued_usi_option_hashes_file_and_directory_contents() {
        let dir = tempfile::tempdir().unwrap();
        let eval_file = dir.path().join("eval.bin");
        let eval_dir = dir.path().join("eval-dir");
        std::fs::create_dir(&eval_dir).unwrap();
        std::fs::write(&eval_file, b"first").unwrap();
        std::fs::write(eval_dir.join("model.bin"), b"model-a").unwrap();
        let options = vec![
            format!("EvalFile={}", eval_file.display()),
            format!("EvalDir={}", eval_dir.display()),
            "Hash=16".to_string(),
        ];
        let first = usi_option_path_fingerprints(&options).unwrap();
        assert_eq!(first.len(), 2);
        std::fs::write(&eval_file, b"second").unwrap();
        std::fs::write(eval_dir.join("model.bin"), b"model-b").unwrap();
        let second = usi_option_path_fingerprints(&options).unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn fingerprint_reports_each_changed_field() {
        let meta = serde_json::json!({"search": {"nodes": 10}, "training": {"format": "psv"}});
        let current = serde_json::json!({"search": {"nodes": 20}, "training": {"format": "pack"}});
        let error = validate_resume_fingerprint(Some(&meta), &current).unwrap_err().to_string();
        assert!(error.contains("search.nodes"));
        assert!(error.contains("training.format"));
    }

    #[test]
    fn hcpe3_policy_fingerprint_is_format_specific() {
        let mut psv = serde_json::json!({"training": {}});
        add_hcpe3_policy_fingerprint(
            &mut psv,
            TrainingFormat::Psv,
            DEFAULT_HCPE3_EVAL_DROP_THRESHOLD,
        );
        assert!(psv["training"].get("hcpe3_eval_drop_threshold").is_none());
        assert!(psv["training"].get("hcpe3_policy_algo").is_none());

        let mut hcpe3 = serde_json::json!({"training": {}});
        add_hcpe3_policy_fingerprint(
            &mut hcpe3,
            TrainingFormat::Hcpe3,
            DEFAULT_HCPE3_EVAL_DROP_THRESHOLD,
        );
        assert_eq!(
            hcpe3["training"]["hcpe3_eval_drop_threshold"],
            DEFAULT_HCPE3_EVAL_DROP_THRESHOLD
        );
        assert_eq!(hcpe3["training"]["hcpe3_policy_algo"], HCPE3_POLICY_ALGO_VERSION);
    }

    #[test]
    fn psv_resume_ignores_legacy_hcpe3_policy_fields() {
        let saved = serde_json::json!({
            "training": {
                "format": "psv",
                "hcpe3_policy_total": 1000,
                "hcpe3_policy_temp": 600.0,
                "hcpe3_eval_drop_threshold": 300,
                "hcpe3_policy_algo": 1
            }
        });
        let current = serde_json::json!({
            "training": {
                "format": "psv",
                "hcpe3_policy_total": 65535,
                "hcpe3_policy_temp": 100.0
            }
        });

        validate_resume_fingerprint(Some(&saved), &current).unwrap();
    }

    #[test]
    fn hcpe3_resume_compares_policy_fields() {
        let saved = serde_json::json!({
            "training": {
                "format": "hcpe3",
                "hcpe3_policy_total": 1000,
                "hcpe3_policy_temp": 100.0,
                "hcpe3_eval_drop_threshold": 500,
                "hcpe3_policy_algo": HCPE3_POLICY_ALGO_VERSION
            }
        });
        let current = serde_json::json!({
            "training": {
                "format": "hcpe3",
                "hcpe3_policy_total": 65535,
                "hcpe3_policy_temp": 100.0,
                "hcpe3_eval_drop_threshold": 500,
                "hcpe3_policy_algo": HCPE3_POLICY_ALGO_VERSION
            }
        });

        let error = validate_resume_fingerprint(Some(&saved), &current).unwrap_err().to_string();
        assert!(error.contains("training.hcpe3_policy_total"));
    }

    #[test]
    fn hcpe3_checkpoint_cannot_resume_as_psv() {
        let saved = serde_json::json!({
            "training": {
                "format": "hcpe3",
                "hcpe3_policy_total": 65535,
                "hcpe3_policy_temp": 100.0,
                "hcpe3_eval_drop_threshold": 500,
                "hcpe3_policy_algo": HCPE3_POLICY_ALGO_VERSION
            }
        });
        let current = serde_json::json!({"training": {"format": "psv"}});

        let error = validate_resume_fingerprint(Some(&saved), &current).unwrap_err().to_string();
        assert!(error.contains("training.format"));
    }

    #[test]
    fn failed_training_commit_does_not_write_result() {
        let result = ResultLog {
            kind: "result",
            worker_id: 0,
            game_id: 1,
            start_pos_index: 1,
            start_sfen: "startpos",
            outcome: "draw",
            reason: OutcomeReason::MaxMoves,
            adopted: true,
            plies: 0,
            final_points_black: 0,
            final_points_white: 0,
            king_in_enemy_black: false,
            king_in_enemy_white: false,
            enemy_zone_pieces_black: 0,
            enemy_zone_pieces_white: 0,
            diversions: Some(&[]),
            multipv_diversions: None,
            random_moves: None,
            diversions_total: None,
            training_bytes: 0,
            sidecar_bytes: None,
            info_bytes: None,
            eval_bytes: None,
            metrics_bytes: None,
            fsync_boundary: false,
        };
        let mut jsonl = Vec::new();
        assert!(write_committed_result(&mut jsonl, result, false, || bail!("disk full")).is_err());
        assert!(jsonl.is_empty());
    }

    #[test]
    fn result_log_serializes_empty_diversions() {
        let result = ResultLog {
            kind: "result",
            worker_id: 0,
            game_id: 1,
            start_pos_index: 1,
            start_sfen: "lnsgkgsnl/1r5b1/p1ppppppp/9/1p7/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL b - 1",
            outcome: "draw",
            reason: OutcomeReason::MaxMoves,
            adopted: true,
            plies: 1,
            final_points_black: 0,
            final_points_white: 0,
            king_in_enemy_black: false,
            king_in_enemy_white: false,
            enemy_zone_pieces_black: 0,
            enemy_zone_pieces_white: 0,
            diversions: Some(&[]),
            multipv_diversions: None,
            random_moves: None,
            diversions_total: None,
            training_bytes: 0,
            sidecar_bytes: None,
            info_bytes: None,
            eval_bytes: None,
            metrics_bytes: None,
            fsync_boundary: false,
        };
        let value = serde_json::to_value(result).unwrap();
        assert_eq!(value["diversions"], serde_json::json!([]));
        assert_eq!(value["adopted"], true);
        assert_eq!(value["reason"], "max_moves");
        // 全量記録モードでは件数フィールドを出さない
        assert!(value.get("multipv_diversions").is_none());
        assert!(value.get("random_moves").is_none());
        assert!(value.get("diversions_total").is_none());
    }

    #[test]
    fn result_log_omits_diversions_and_keeps_counts() {
        let result = ResultLog {
            kind: "result",
            worker_id: 0,
            game_id: 1,
            start_pos_index: 1,
            start_sfen: "lnsgkgsnl/1r5b1/p1ppppppp/9/1p7/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL b - 1",
            outcome: "draw",
            reason: OutcomeReason::MaxMoves,
            adopted: true,
            plies: 1,
            final_points_black: 0,
            final_points_white: 0,
            king_in_enemy_black: false,
            king_in_enemy_white: false,
            enemy_zone_pieces_black: 0,
            enemy_zone_pieces_white: 0,
            diversions: None,
            multipv_diversions: Some(12),
            random_moves: Some(4),
            diversions_total: Some(16),
            training_bytes: 0,
            sidecar_bytes: None,
            info_bytes: None,
            eval_bytes: None,
            metrics_bytes: None,
            fsync_boundary: false,
        };
        let value = serde_json::to_value(result).unwrap();
        assert!(value.get("diversions").is_none());
        assert_eq!(value["multipv_diversions"], 12);
        assert_eq!(value["random_moves"], 4);
        assert_eq!(value["diversions_total"], 16);
    }

    #[test]
    fn diversion_log_serializes_score_gap() {
        let diversion = DiversionLog {
            ply: 7,
            kind: "multipv",
            chosen_move: "2g2f".to_string(),
            best_move: Some("7g7f".to_string()),
            score_gap_cp: Some(25),
        };
        let value = serde_json::to_value(diversion).unwrap();
        assert_eq!(value["ply"], 7);
        assert_eq!(value["kind"], "multipv");
        assert_eq!(value["chosen_move"], "2g2f");
        assert_eq!(value["best_move"], "7g7f");
        assert_eq!(value["score_gap_cp"], 25);
    }

    #[test]
    fn sample_random_move_plies_no_duplicates() {
        let mut rng = StdRng::seed_from_u64(42);
        let plies = sample_random_move_plies(5, 20, 10, &mut rng);
        assert_eq!(plies.len(), 10);
        for &p in &plies {
            assert!((5..=20).contains(&p));
        }
    }

    #[test]
    fn sample_random_move_plies_capped_by_range() {
        let mut rng = StdRng::seed_from_u64(42);
        // 範囲 3 に対して count 10 → 3 個に制限される
        let plies = sample_random_move_plies(1, 3, 10, &mut rng);
        assert_eq!(plies.len(), 3);
    }

    #[test]
    fn mate_to_eval_encodes_32000_minus_ply() {
        assert_eq!(mate_to_eval(1), 31999);
        assert_eq!(mate_to_eval(39), 31961);
        assert_eq!(mate_to_eval(-1), -31999);
        assert_eq!(mate_to_eval(-39), -31961);
        // 境界: 手数 0 でも詰み帯に収まる
        assert_eq!(mate_to_eval(0), 32000);
        // 巨大 ply でも詰み帯（|eval| >= 30000）に収まる
        assert!(mate_to_eval(5000) >= 30001);
        assert!(mate_to_eval(-5000) <= -30001);
    }

    #[test]
    fn multipv_to_policy_sorts_and_keeps_pv1() {
        use rshogi_core::types::Move;
        use tools::packed_sfen::move_to_psv_move16;
        use tools::selfplay::MultiPvCandidate;

        let m1 = Move::from_usi("7g7f").unwrap();
        let m2 = Move::from_usi("2g2f").unwrap();
        let m3 = Move::from_usi("3g3f").unwrap();
        // 到着順は逆でも multipv 昇順に整列する
        let candidates = vec![
            MultiPvCandidate {
                multipv: 3,
                score_cp: -500,
                score_mate: None,
                first_move: m3,
            },
            MultiPvCandidate {
                multipv: 1,
                score_cp: 100,
                score_mate: None,
                first_move: m1,
            },
            MultiPvCandidate {
                multipv: 2,
                score_cp: 90,
                score_mate: None,
                first_move: m2,
            },
        ];
        let policy = multipv_to_policy(&candidates, m1, 1000, 600.0, 3, -1);
        assert!(!policy.is_empty());
        // PV1 (m1) が先頭で 1 票以上
        assert_eq!(policy[0].0, move_to_psv_move16(m1));
        assert!(policy[0].1 >= 1);
        let sum: i32 = policy.iter().map(|(_, v)| *v as i32).sum();
        // largest-remainder で総票数は total に厳密一致する
        assert_eq!(sum, 1000);
    }

    #[test]
    fn multipv_to_policy_ties_are_equal() {
        use rshogi_core::types::Move;
        use tools::selfplay::MultiPvCandidate;

        let m1 = Move::from_usi("7g7f").unwrap();
        let m2 = Move::from_usi("2g2f").unwrap();
        let candidates = vec![
            MultiPvCandidate {
                multipv: 1,
                score_cp: 50,
                score_mate: None,
                first_move: m1,
            },
            MultiPvCandidate {
                multipv: 2,
                score_cp: 50,
                score_mate: None,
                first_move: m2,
            },
        ];
        let policy = multipv_to_policy(&candidates, m1, 1000, 600.0, 2, -1);
        assert_eq!(policy.len(), 2);
        assert_eq!(policy[0].1, policy[1].1);
    }

    #[test]
    fn validate_training_format_opts_enforces_constraints() {
        // replay 形式は中間スキップを拒否する
        assert!(validate_training_format_opts(TrainingFormat::Pack, true, 0, 0.0).is_err());
        assert!(validate_training_format_opts(TrainingFormat::Hcpe3, true, 1000, 600.0).is_err());
        // hcpe3 は不正 policy パラメータも拒否する
        assert!(validate_training_format_opts(TrainingFormat::Hcpe3, false, 0, 600.0).is_err());
        assert!(validate_training_format_opts(TrainingFormat::Hcpe3, false, 1000, 0.0).is_ok());
        assert!(
            validate_training_format_opts(TrainingFormat::Hcpe3, false, 1000, f64::NAN).is_err()
        );
        assert!(validate_training_format_opts(TrainingFormat::Hcpe3, false, 1000, 600.0).is_ok());
        // PSV には replay/policy の制約を課さない
        assert!(validate_training_format_opts(TrainingFormat::Psv, true, 0, f64::NAN).is_ok());
    }

    #[test]
    fn game_id_sidecar_rejects_final_and_worker_path_collisions() {
        let output = Path::new("run/gensfen.jsonl");
        let training = Path::new("run/gensfen.psv");
        for collision in [
            "run/gensfen.jsonl",
            "run/gensfen.psv",
            "run/gensfen.info.jsonl",
            "run/gensfen.eval.txt",
            "run/gensfen.metrics.jsonl",
            "run/gensfen.w0.jsonl",
            "run/gensfen.w1.psv",
            "run/gensfen.w0.info.jsonl",
            "run/gensfen.w1.eval.txt",
            "run/gensfen.w0.metrics.jsonl",
            "run/gensfen.w1.game_ids.bin",
            "run/./gensfen.psv",
            "run/nested/../gensfen.psv",
        ] {
            let error = validate_output_paths_unique(
                output,
                training,
                Some(Path::new(collision)),
                "psv",
                2,
                true,
                true,
                true,
            )
            .unwrap_err();
            assert!(error.to_string().contains("collision"), "{error:#}");
        }
        validate_output_paths_unique(
            output,
            training,
            Some(Path::new("run/ids.bin")),
            "psv",
            2,
            true,
            true,
            true,
        )
        .unwrap();
    }

    #[test]
    fn abnormal_endings_discard_every_collected_position() {
        for (name, reason) in [
            ("timeout", AbnormalEndReason::Timeout),
            ("illegal", AbnormalEndReason::IllegalMove),
            ("no_bestmove", AbnormalEndReason::NoBestmove),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(format!("{name}.psv"));
            let mut pos = Position::new();
            pos.set_hirate();
            let mv = Move::from_usi("7g7f").unwrap();
            let stats;
            {
                let mut collector = TrainingDataCollector::new(
                    &path,
                    0,
                    false,
                    TrainingFormat::Psv,
                    1000,
                    600.0,
                    None,
                )
                .unwrap();
                collector.record_position(&pos, Some(20), None, Some(mv), mv, &[]);
                collector
                    .finish_game(GameOutcome::WhiteWin, TrainingDisposition::Discard(reason), 1)
                    .unwrap();
                collector.flush().unwrap();
                stats = collector.stats();
            }
            assert_eq!(std::fs::metadata(path).unwrap().len(), 0);
            assert_eq!(stats.total_written, 0);
            assert_eq!(stats.discarded_positions, 1);
            assert_eq!(stats.discarded_timeout_games, u64::from(name == "timeout"));
            assert_eq!(stats.discarded_illegal_move_games, u64::from(name == "illegal"));
            assert_eq!(stats.discarded_no_bestmove_games, u64::from(name == "no_bestmove"));
        }
    }

    #[test]
    fn max_moves_draw_adopts_all_collected_positions_with_draw_result() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("max_moves.psv");
        let mut pos = Position::new();
        pos.set_hirate();
        let first = Move::from_usi("7g7f").unwrap();
        let second = Move::from_usi("3c3d").unwrap();
        {
            let mut collector =
                TrainingDataCollector::new(&path, 0, false, TrainingFormat::Psv, 1000, 600.0, None)
                    .unwrap();
            collector.record_position(&pos, Some(20), None, Some(first), first, &[]);
            let gives_check = pos.gives_check(first);
            pos.do_move(first, gives_check);
            collector.record_position(&pos, Some(-10), None, Some(second), second, &[]);
            collector.finish_game(GameOutcome::Draw, TrainingDisposition::Adopt, 1).unwrap();
            collector.flush().unwrap();
        }
        let bytes = std::fs::read(path).unwrap();
        let records: Vec<_> = bytes
            .chunks_exact(PackedSfenValue::SIZE)
            .map(|record| PackedSfenValue::from_bytes(record).unwrap())
            .collect();
        assert_eq!(records.len(), 2);
        assert!(records.iter().all(|record| record.game_result == 0));
    }

    #[test]
    fn declaration_win_position_is_recorded_as_psv_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("declaration.psv");
        let mut pos = Position::new();
        pos.set_sfen("KGG6/SS7/PPPPPP3/9/9/9/2pppppp1/1ss1gg1nl/4k2nl b 2R2B3p 1")
            .unwrap();
        assert!(is_valid_bestmove_win(&pos, EnteringKingRule::Point27));
        {
            let mut collector =
                TrainingDataCollector::new(&path, 0, false, TrainingFormat::Psv, 1000, 600.0, None)
                    .unwrap();
            collector.record_declaration_win_position(&pos);
            collector
                .finish_game(GameOutcome::BlackWin, TrainingDisposition::Adopt, 1)
                .unwrap();
            collector.flush().unwrap();
        }
        let bytes = std::fs::read(path).unwrap();
        let record = PackedSfenValue::from_bytes(&bytes).unwrap();
        assert_eq!(record.score, 10000);
        assert_eq!(record.move16, 0);
        assert_eq!(record.game_result, 1);
    }

    #[test]
    fn declaration_win_position_score_is_fixed_to_saturated_win() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("declaration_score.psv");
        let mut pos = Position::new();
        pos.set_sfen("KGG6/SS7/PPPPPP3/9/9/9/2pppppp1/1ss1gg1nl/4k2nl b 2R2B3p 1")
            .unwrap();
        let mut collector =
            TrainingDataCollector::new(&path, 0, false, TrainingFormat::Psv, 1000, 600.0, None)
                .unwrap();

        collector.record_declaration_win_position(&pos);
        assert_eq!(collector.entries[0].score, 10000);
    }

    #[test]
    fn declaration_win_dedup_keeps_second_games_non_terminal_positions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("declaration_dedup.psv");
        let mut declaration_pos = Position::new();
        declaration_pos
            .set_sfen("KGG6/SS7/PPPPPP3/9/9/9/2pppppp1/1ss1gg1nl/4k2nl b 2R2B3p 1")
            .unwrap();
        let mut collector =
            TrainingDataCollector::new(&path, 0, false, TrainingFormat::Psv, 1000, 600.0, None)
                .unwrap();
        let dedup = SharedDedupHash::new(8);
        let mut hits = 0;
        let mut interval_hits = 0;
        let mut interval_checked = 0;
        let mut first_game_pending = PendingDedupKeys::default();

        let first_is_duplicate = check_declaration_win_position_dedup(
            TrainingFormat::Psv,
            Some(&dedup),
            &mut first_game_pending,
            declaration_pos.key(),
            Some(&mut collector),
            &mut hits,
            &mut interval_hits,
            &mut interval_checked,
        );
        assert!(!first_is_duplicate);
        collector.record_declaration_win_position(&declaration_pos);
        collector
            .finish_game(GameOutcome::BlackWin, TrainingDisposition::Adopt, 1)
            .unwrap();
        first_game_pending.publish(&dedup);

        collector.start_game();
        let mut second_game_pending = PendingDedupKeys::default();
        let mut normal_pos = Position::new();
        normal_pos.set_hirate();
        let normal_move = Move::from_usi("7g7f").unwrap();
        collector.record_position(
            &normal_pos,
            Some(100),
            None,
            Some(normal_move),
            normal_move,
            &[],
        );
        let second_is_duplicate = check_declaration_win_position_dedup(
            TrainingFormat::Psv,
            Some(&dedup),
            &mut second_game_pending,
            declaration_pos.key(),
            Some(&mut collector),
            &mut hits,
            &mut interval_hits,
            &mut interval_checked,
        );
        assert!(second_is_duplicate);
        assert_eq!(collector.entries_len(), 1);
        assert_eq!(hits, 1);
        assert_eq!(interval_hits, 1);
        assert_eq!(interval_checked, 2);
        collector
            .finish_game(GameOutcome::BlackWin, TrainingDisposition::Adopt, 2)
            .unwrap();
        collector.flush().unwrap();
        assert_eq!(std::fs::metadata(path).unwrap().len(), (PackedSfenValue::SIZE * 2) as u64);
        assert_eq!(collector.stats().declaration_win_dedup_skipped_games, 1);
    }

    #[test]
    fn declaration_win_does_not_append_fake_replay_move() {
        for format in [TrainingFormat::Pack, TrainingFormat::Hcpe3] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("declaration.bin");
            let mut pos = Position::new();
            pos.set_sfen("KGG6/SS7/PPPPPP3/9/9/9/2pppppp1/1ss1gg1nl/4k2nl b 2R2B3p 1")
                .unwrap();
            let collector =
                TrainingDataCollector::new(&path, 0, false, format, 1000, 600.0, None).unwrap();
            let mut collector = collector;
            collector.record_declaration_win_position(&pos);
            assert_eq!(collector.entries_len(), 0);
        }
    }

    #[test]
    fn unrecorded_declaration_win_does_not_dedup_or_discard_pack_and_hcpe3_games() {
        for format in [TrainingFormat::Pack, TrainingFormat::Hcpe3] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("declaration.bin");
            let mut recorded_pos = Position::new();
            recorded_pos.set_hirate();
            let mv = Move::from_usi("7g7f").unwrap();
            let mut collector =
                TrainingDataCollector::new(&path, 0, false, format, 1000, 600.0, None).unwrap();
            collector.record_position(&recorded_pos, Some(10), None, Some(mv), mv, &[]);

            let mut declaration_pos = Position::new();
            declaration_pos
                .set_sfen("KGG6/SS7/PPPPPP3/9/9/9/2pppppp1/1ss1gg1nl/4k2nl b 2R2B3p 1")
                .unwrap();
            let dedup = SharedDedupHash::new(8);
            let mut hits = 0;
            let mut interval_hits = 0;
            let mut interval_checked = 0;
            let mut pending = PendingDedupKeys::default();

            assert!(!check_declaration_win_position_dedup(
                format,
                Some(&dedup),
                &mut pending,
                declaration_pos.key(),
                Some(&mut collector),
                &mut hits,
                &mut interval_hits,
                &mut interval_checked,
            ));
            assert_eq!(collector.entries_len(), 1);
            assert_eq!((hits, interval_hits, interval_checked), (0, 0, 0));
            assert!(!dedup.check_and_insert(declaration_pos.key()));
        }
    }

    #[test]
    fn psv_game_id_sidecar_matches_result_game_ids() {
        let dir = tempfile::tempdir().unwrap();
        let psv_path = dir.path().join("short.psv");
        let sidecar_path = dir.path().join("short.game_ids.bin");
        let jsonl_path = dir.path().join("short.jsonl");

        let mut pos = Position::new();
        pos.set_hirate();
        let packed = pack_position(&pos);
        let entry = |side_to_move| TrainingEntry {
            sfen: packed,
            score: 10,
            move16: 0,
            game_ply: 1,
            side_to_move,
            hcpe3: None,
        };

        {
            let mut collector = TrainingDataCollector::new(
                &psv_path,
                0,
                false,
                TrainingFormat::Psv,
                1000,
                600.0,
                Some(&sidecar_path),
            )
            .unwrap();
            collector.entries.push(entry(Color::Black));
            collector.entries.push(entry(Color::White));
            collector
                .finish_game(GameOutcome::BlackWin, TrainingDisposition::Adopt, 7)
                .unwrap();
            collector.entries.push(entry(Color::Black));
            collector.finish_game(GameOutcome::Draw, TrainingDisposition::Adopt, 9).unwrap();
            collector.flush().unwrap();
        }

        let mut jsonl = BufWriter::new(File::create(&jsonl_path).unwrap());
        for game_id in [7, 9] {
            serde_json::to_writer(
                &mut jsonl,
                &serde_json::json!({"type": "result", "game_id": game_id}),
            )
            .unwrap();
            jsonl.write_all(b"\n").unwrap();
        }
        jsonl.flush().unwrap();

        let psv_records =
            std::fs::metadata(&psv_path).unwrap().len() as usize / PackedSfenValue::SIZE;
        let sidecar = std::fs::read(&sidecar_path).unwrap();
        let game_ids: Vec<u32> = sidecar
            .chunks_exact(4)
            .map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()))
            .collect();
        assert_eq!(psv_records, game_ids.len());
        assert_eq!(game_ids, [7, 7, 9]);

        let result_ids: Vec<u32> = BufReader::new(File::open(jsonl_path).unwrap())
            .lines()
            .map(|line| {
                serde_json::from_str::<Value>(&line.unwrap()).unwrap()["game_id"]
                    .as_u64()
                    .unwrap() as u32
            })
            .collect();
        assert!(game_ids.iter().all(|game_id| result_ids.contains(game_id)));
    }

    #[test]
    fn finish_game_hcpe3_byte_layout() {
        use rshogi_core::position::Position;
        use rshogi_core::types::Move;
        use tools::selfplay::MultiPvCandidate;

        let path =
            std::env::temp_dir().join(format!("gensfen_hcpe3_layout_{}.hcpe3", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let mut pos = Position::new();
        pos.set_hirate();
        let mv = Move::from_usi("7g7f").unwrap();
        let candidates = vec![MultiPvCandidate {
            multipv: 1,
            score_cp: 123,
            score_mate: None,
            first_move: mv,
        }];

        {
            let mut col = TrainingDataCollector::new(
                &path,
                0,
                false,
                TrainingFormat::Hcpe3,
                1000,
                600.0,
                None,
            )
            .unwrap();
            col.start_game();
            col.record_position(&pos, Some(123), None, Some(mv), mv, &candidates);
            col.finish_game(GameOutcome::BlackWin, TrainingDisposition::Adopt, 1).unwrap();
            col.flush().unwrap();
        }

        let bytes = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        // 1 局面 1 候補 = hcp(32)+moveNum(2)+result(1)+opponent(1)
        //   + selectedMove16(2)+eval(2)+candidateNum(2) + 1*(move16(2)+visit(2)) = 46
        assert_eq!(bytes.len(), 46);
        assert_eq!(u16::from_le_bytes([bytes[32], bytes[33]]), 1); // moveNum
        assert_eq!(bytes[34], 1); // result = BLACK_WIN
        assert_eq!(bytes[35], 4); // gameInfo: max_moves=256
        assert_eq!(i16::from_le_bytes([bytes[38], bytes[39]]), 123); // eval
        assert_eq!(u16::from_le_bytes([bytes[40], bytes[41]]), 1); // candidateNum
        assert_eq!(bytes[36..38], bytes[42..44]); // selectedMove16 == 候補 move16
        assert_eq!(u16::from_le_bytes([bytes[44], bytes[45]]), 1000); // visit（PV1 単独で全票）
    }

    #[test]
    fn finish_game_hcpe3_multi_candidate_layout() {
        use rshogi_core::position::Position;
        use rshogi_core::types::Move;
        use tools::selfplay::MultiPvCandidate;

        let path =
            std::env::temp_dir().join(format!("gensfen_hcpe3_multi_{}.hcpe3", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let mut pos = Position::new();
        pos.set_hirate();
        let m1 = Move::from_usi("7g7f").unwrap();
        let m2 = Move::from_usi("2g2f").unwrap();
        let m3 = Move::from_usi("3g3f").unwrap();
        let candidates = vec![
            MultiPvCandidate {
                multipv: 1,
                score_cp: 100,
                score_mate: None,
                first_move: m1,
            },
            MultiPvCandidate {
                multipv: 2,
                score_cp: 80,
                score_mate: None,
                first_move: m2,
            },
            MultiPvCandidate {
                multipv: 3,
                score_cp: 60,
                score_mate: None,
                first_move: m3,
            },
        ];

        {
            let mut col = TrainingDataCollector::new(
                &path,
                0,
                false,
                TrainingFormat::Hcpe3,
                1000,
                600.0,
                None,
            )
            .unwrap();
            col.start_game();
            col.record_position(&pos, Some(100), None, Some(m1), m1, &candidates);
            col.finish_game(GameOutcome::WhiteWin, TrainingDisposition::Adopt, 1).unwrap();
            col.flush().unwrap();
        }

        let bytes = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        let candidate_num = u16::from_le_bytes([bytes[40], bytes[41]]);
        assert_eq!(candidate_num, 3);
        // hcp(32)+moveNum(2)+result(1)+opponent(1) + selectedMove16(2)+eval(2)+candidateNum(2)
        //   + 3*(move16(2)+visit(2)) = 54
        assert_eq!(bytes.len(), 54);
        assert_eq!(bytes[34], 2); // result = WHITE_WIN
    }

    fn legal_candidate(rank: u32, cp: i32, usi: &str) -> tools::selfplay::MultiPvCandidate {
        use rshogi_core::types::Move;
        tools::selfplay::MultiPvCandidate {
            multipv: rank,
            score_cp: cp,
            score_mate: None,
            first_move: Move::from_usi(usi).unwrap(),
        }
    }

    #[test]
    fn multipv_to_policy_visits_sum_to_total() {
        use tools::packed_sfen::move_to_psv_move16;
        let candidates = vec![
            legal_candidate(1, 100, "7g7f"),
            legal_candidate(2, 63, "2g2f"),
            legal_candidate(3, 26, "3g3f"),
            legal_candidate(4, -40, "6g6f"),
            legal_candidate(5, -120, "5g5f"),
        ];
        for total in [1u16, 7, 1000, 1001, 65535] {
            let policy =
                multipv_to_policy(&candidates, candidates[0].first_move, total, 600.0, 5, -1);
            let sum: u32 = policy.iter().map(|(_, v)| *v as u32).sum();
            assert_eq!(sum, u32::from(total).max(candidates.len() as u32), "total={total}");
            assert_eq!(policy[0].0, move_to_psv_move16(candidates[0].first_move));
            assert!(policy.iter().all(|(_, visit)| *visit >= 1));
        }
    }

    #[test]
    fn multipv_to_policy_filters_eval_drop_but_keeps_selected_move() {
        use tools::packed_sfen::move_to_psv_move16;

        let candidates = vec![
            legal_candidate(1, 1000, "7g7f"),
            legal_candidate(2, 499, "2g2f"),
            legal_candidate(3, 498, "3g3f"),
        ];
        let selected_move = candidates[2].first_move;
        let selected = move_to_psv_move16(selected_move);
        let policy = multipv_to_policy(&candidates, selected_move, 100, 100.0, 3, 500);

        assert_eq!(policy.len(), 2);
        assert_eq!(policy[0].0, move_to_psv_move16(candidates[0].first_move));
        assert_eq!(policy[1].0, selected);
        assert_eq!(policy.iter().map(|(_, visit)| u32::from(*visit)).sum::<u32>(), 100);
        assert!(policy.iter().all(|(_, visit)| *visit >= 1));
    }

    #[test]
    fn multipv_to_policy_inserts_missing_played_move_first() {
        use rshogi_core::types::Move;
        use tools::packed_sfen::move_to_psv_move16;

        let candidates = vec![
            legal_candidate(2, 80, "2g2f"),
            legal_candidate(1, 100, "7g7f"),
        ];
        let played = Move::from_usi("3g3f").unwrap();
        let policy = multipv_to_policy(&candidates, played, 100, 100.0, 2, 0);

        assert_eq!(policy.len(), 2);
        assert_eq!(policy[0].0, move_to_psv_move16(played));
        assert!(policy[0].1 >= 1);
        assert_eq!(policy.iter().map(|(_, visit)| u32::from(*visit)).sum::<u32>(), 100);
    }

    #[test]
    fn multipv_to_policy_trims_synthetic_candidate_to_multi_pv_width() {
        use rshogi_core::types::Move;
        use tools::packed_sfen::move_to_psv_move16;

        let candidates = vec![
            legal_candidate(1, 100, "7g7f"),
            legal_candidate(2, 80, "2g2f"),
            legal_candidate(3, 60, "3g3f"),
        ];
        let played = Move::from_usi("6g6f").unwrap();
        let policy = multipv_to_policy(&candidates, played, 100, 100.0, 3, -1);

        assert_eq!(policy.len(), 3);
        assert_eq!(policy[0].0, move_to_psv_move16(played));
        assert_eq!(policy[1].0, move_to_psv_move16(candidates[0].first_move));
        assert_eq!(policy[2].0, move_to_psv_move16(candidates[1].first_move));
        assert!(
            !policy
                .iter()
                .any(|(move16, _)| *move16 == move_to_psv_move16(candidates[2].first_move))
        );
        assert_eq!(policy.iter().map(|(_, visit)| u32::from(*visit)).sum::<u32>(), 100);
    }

    #[test]
    fn multipv_to_policy_keeps_all_candidates_when_played_move_is_within_exact_width() {
        use tools::packed_sfen::move_to_psv_move16;

        let candidates = vec![
            legal_candidate(1, 100, "7g7f"),
            legal_candidate(2, 80, "2g2f"),
            legal_candidate(3, 60, "3g3f"),
        ];
        let played = candidates[1].first_move;
        let policy = multipv_to_policy(&candidates, played, 100, 100.0, 3, -1);

        assert_eq!(policy.len(), candidates.len());
        assert_eq!(
            policy.iter().map(|(move16, _)| *move16).collect::<Vec<_>>(),
            candidates
                .iter()
                .map(|candidate| move_to_psv_move16(candidate.first_move))
                .collect::<Vec<_>>()
        );
        assert_eq!(policy.iter().map(|(_, visit)| u32::from(*visit)).sum::<u32>(), 100);
    }

    #[test]
    fn multipv_to_policy_caps_width_at_hcpe3_candidate_limit() {
        use rshogi_core::types::{Move, Square};

        let candidates = (0..=MAX_HCPE3_POLICY_CANDIDATES)
            .map(|index| MultiPvCandidate {
                multipv: index + 1,
                score_cp: 0,
                score_mate: None,
                first_move: Move::new_move(
                    Square::from_u8((index / 81) as u8).unwrap(),
                    Square::from_u8((index % 81) as u8).unwrap(),
                    false,
                ),
            })
            .collect::<Vec<_>>();
        let policy = multipv_to_policy(
            &candidates,
            candidates[0].first_move,
            1000,
            100.0,
            MAX_HCPE3_POLICY_CANDIDATES + 1,
            -1,
        );

        assert_eq!(policy.len(), MAX_HCPE3_POLICY_CANDIDATES as usize);
    }

    #[test]
    fn multipv_to_policy_clamps_multi_pv_width_to_one() {
        use rshogi_core::types::Move;
        use tools::packed_sfen::move_to_psv_move16;

        let candidates = vec![
            legal_candidate(1, 100, "7g7f"),
            legal_candidate(2, 80, "2g2f"),
        ];
        let played = Move::from_usi("3g3f").unwrap();

        for multi_pv in [0, 1] {
            let policy = multipv_to_policy(&candidates, played, 100, 100.0, multi_pv, -1);
            assert_eq!(policy, vec![(move_to_psv_move16(played), 100)]);
        }
    }

    #[test]
    fn multipv_to_policy_deduplicates_move16_by_lowest_multipv() {
        use tools::packed_sfen::move_to_psv_move16;

        let candidates = vec![
            legal_candidate(3, -500, "7g7f"),
            legal_candidate(2, 50, "2g2f"),
            legal_candidate(1, 100, "7g7f"),
        ];
        let policy = multipv_to_policy(&candidates, candidates[0].first_move, 100, 100.0, 3, -1);

        assert_eq!(policy.len(), 2);
        assert_eq!(policy[0].0, move_to_psv_move16(candidates[0].first_move));
        assert_eq!(policy.iter().map(|(_, visit)| u32::from(*visit)).sum::<u32>(), 100);
        assert_eq!(
            policy
                .iter()
                .filter(|(move16, _)| { *move16 == move_to_psv_move16(candidates[0].first_move) })
                .count(),
            1
        );
        assert!(policy[0].1 > policy[1].1);
    }

    #[test]
    fn multipv_to_policy_is_deterministic() {
        let candidates = vec![
            legal_candidate(3, 26, "3g3f"),
            legal_candidate(1, 100, "7g7f"),
            legal_candidate(2, 63, "2g2f"),
        ];
        let a = multipv_to_policy(&candidates, candidates[1].first_move, 1000, 600.0, 3, -1);
        let b = multipv_to_policy(&candidates, candidates[1].first_move, 1000, 600.0, 3, -1);
        assert_eq!(a, b);
    }

    #[test]
    fn multipv_to_policy_greedy_selects_first_best_candidate() {
        use tools::packed_sfen::move_to_psv_move16;

        let candidates = vec![
            legal_candidate(3, 50, "3g3f"),
            legal_candidate(2, 100, "2g2f"),
            legal_candidate(1, 100, "7g7f"),
        ];

        for temp in [0.0, -1.0] {
            let policy = multipv_to_policy(&candidates, candidates[2].first_move, 100, temp, 3, -1);
            assert_eq!(policy[0], (move_to_psv_move16(candidates[2].first_move), 98));
            assert_eq!(policy[1], (move_to_psv_move16(candidates[1].first_move), 1));
            assert_eq!(policy[2], (move_to_psv_move16(candidates[0].first_move), 1));
        }
    }

    #[test]
    fn hcpe3_game_info_and_result_flags_match_dlshogi_layout() {
        assert_eq!(hcpe3_game_info(256), 4);
        assert_eq!(hcpe3_game_info(320), 8);
        assert_eq!(hcpe3_game_info(512), 12);
        assert_eq!(hcpe3_game_info(400), 0);

        assert_eq!(hcpe3_result(GameOutcome::Draw, OutcomeReason::Sennichite), 4);
        assert_eq!(hcpe3_result(GameOutcome::WhiteWin, OutcomeReason::PerpetualCheck), 6);
        assert_eq!(hcpe3_result(GameOutcome::BlackWin, OutcomeReason::Win), 9);
        assert_eq!(hcpe3_result(GameOutcome::Draw, OutcomeReason::MaxMoves), 16);
        assert_eq!(hcpe3_result(GameOutcome::BlackWin, OutcomeReason::Mate), 1);
    }

    #[test]
    fn multipv_to_policy_downweights_losing_mate() {
        use rshogi_core::types::Move;
        use tools::packed_sfen::move_to_psv_move16;
        use tools::selfplay::MultiPvCandidate;

        let good = Move::from_usi("7g7f").unwrap();
        let losing = Move::from_usi("2g2f").unwrap();
        // PV2 は負け詰み
        let candidates = vec![
            MultiPvCandidate {
                multipv: 1,
                score_cp: 120,
                score_mate: None,
                first_move: good,
            },
            MultiPvCandidate {
                multipv: 2,
                score_cp: -30000,
                score_mate: Some(5),
                first_move: losing,
            },
        ];
        assert_eq!(candidate_eval(&candidates[1]), -31995);
        let policy = multipv_to_policy(&candidates, good, 1000, 600.0, 2, -1);
        let good_v = policy
            .iter()
            .find(|(m, _)| *m == move_to_psv_move16(good))
            .map_or(0, |(_, v)| *v);
        let losing_v = policy
            .iter()
            .find(|(m, _)| *m == move_to_psv_move16(losing))
            .map_or(0, |(_, v)| *v);
        // 符号付き score_cp で負け詰みは大きく減点され、PV1 を上回らない
        assert!(good_v > losing_v);
    }

    #[test]
    fn candidate_eval_handles_signed_usi_mate_and_cp_fallback() {
        use rshogi_core::types::Move;
        use tools::selfplay::MultiPvCandidate;

        let candidate = |score_cp, score_mate| MultiPvCandidate {
            multipv: 1,
            score_cp,
            score_mate,
            first_move: Move::from_usi("7g7f").unwrap(),
        };

        assert_eq!(candidate_eval(&candidate(30000, Some(-5))), -31995);
        assert_eq!(candidate_eval(&candidate(30000, Some(5))), 31995);
        assert_eq!(candidate_eval(&candidate(30000, None)), 30000);
        assert_eq!(candidate_eval(&candidate(-30000, None)), -30000);
    }

    #[test]
    fn finish_game_hcpe3_records_played_move_not_pv1() {
        use rshogi_core::position::Position;
        use rshogi_core::types::Move;

        let path =
            std::env::temp_dir().join(format!("gensfen_hcpe3_played_{}.hcpe3", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let mut pos = Position::new();
        pos.set_hirate();
        let best = Move::from_usi("7g7f").unwrap();
        let played = Move::from_usi("2g2f").unwrap();
        let candidates = vec![legal_candidate(1, 100, "7g7f")];

        {
            let mut col = TrainingDataCollector::new(
                &path,
                0,
                false,
                TrainingFormat::Hcpe3,
                1000,
                600.0,
                None,
            )
            .unwrap();
            col.start_game();
            // 実着手 played != 最善手 best。selectedMove16 は replay 用に played を記録する
            col.record_position(&pos, Some(100), None, Some(best), played, &candidates);
            col.finish_game(GameOutcome::BlackWin, TrainingDisposition::Adopt, 1).unwrap();
            col.flush().unwrap();
        }
        let bytes = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        // 実着手が MultiPV に無くても合成候補として先頭に入り、policy に 1 票以上を持つ
        assert_eq!(u16::from_le_bytes([bytes[40], bytes[41]]), 2);
        assert_eq!(bytes[36..38], bytes[42..44]);
        assert!(u16::from_le_bytes([bytes[44], bytes[45]]) >= 1);
    }

    #[test]
    fn pack_records_played_move_but_psv_keeps_best_move_label() {
        use tools::packed_sfen::{move_to_psv_move16, psv_move16_to_hcpe};

        let dir = tempfile::tempdir().unwrap();
        let mut pos = Position::new();
        pos.set_hirate();
        let best = Move::from_usi("7g7f").unwrap();
        let played = Move::from_usi("2g2f").unwrap();

        let pack_path = dir.path().join("played.pack");
        {
            let mut collector = TrainingDataCollector::new(
                &pack_path,
                0,
                false,
                TrainingFormat::Pack,
                1000,
                600.0,
                None,
            )
            .unwrap();
            collector.record_position(&pos, Some(100), None, Some(best), played, &[]);
            collector
                .finish_game(GameOutcome::BlackWin, TrainingDisposition::Adopt, 1)
                .unwrap();
            collector.flush().unwrap();
        }
        let pack = std::fs::read(pack_path).unwrap();
        assert_eq!(pack[0], 1); // 平手開始
        assert_eq!(
            u16::from_le_bytes([pack[1], pack[2]]),
            psv_move16_to_hcpe(move_to_psv_move16(played))
        );

        let psv_path = dir.path().join("best.psv");
        {
            let mut collector = TrainingDataCollector::new(
                &psv_path,
                0,
                false,
                TrainingFormat::Psv,
                1000,
                600.0,
                None,
            )
            .unwrap();
            collector.record_position(&pos, Some(100), None, Some(best), played, &[]);
            collector
                .finish_game(GameOutcome::BlackWin, TrainingDisposition::Adopt, 1)
                .unwrap();
            collector.flush().unwrap();
        }
        let psv = std::fs::read(psv_path).unwrap();
        let record = PackedSfenValue::from_bytes(&psv).unwrap();
        assert_eq!(record.move16, move_to_psv_move16(best));
    }

    #[test]
    fn finish_game_hcpe3_one_hot_without_candidates() {
        use rshogi_core::position::Position;
        use rshogi_core::types::Move;

        let path =
            std::env::temp_dir().join(format!("gensfen_hcpe3_onehot_{}.hcpe3", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let mut pos = Position::new();
        pos.set_hirate();
        let mv = Move::from_usi("7g7f").unwrap();
        {
            let mut col = TrainingDataCollector::new(
                &path,
                0,
                false,
                TrainingFormat::Hcpe3,
                1000,
                600.0,
                None,
            )
            .unwrap();
            col.start_game();
            // --random-multi-pv 未指定相当: 候補なし → 実着手の one-hot (visit=1)
            col.record_position(&pos, Some(50), None, Some(mv), mv, &[]);
            col.finish_game(GameOutcome::BlackWin, TrainingDisposition::Adopt, 1).unwrap();
            col.flush().unwrap();
        }
        let bytes = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(bytes.len(), 46);
        assert_eq!(u16::from_le_bytes([bytes[40], bytes[41]]), 1); // candidateNum
        assert_eq!(u16::from_le_bytes([bytes[44], bytes[45]]), 1); // visit
    }

    #[test]
    fn finish_game_hcpe3_multi_move_is_contiguous() {
        use rshogi_core::position::Position;
        use rshogi_core::types::Move;

        let path = std::env::temp_dir()
            .join(format!("gensfen_hcpe3_multimove_{}.hcpe3", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let mut pos = Position::new();
        pos.set_hirate();
        let m1 = Move::from_usi("7g7f").unwrap();
        {
            let mut col = TrainingDataCollector::new(
                &path,
                0,
                false,
                TrainingFormat::Hcpe3,
                1000,
                600.0,
                None,
            )
            .unwrap();
            col.start_game();
            col.record_position(
                &pos,
                Some(20),
                None,
                Some(m1),
                m1,
                &[legal_candidate(1, 20, "7g7f")],
            );
            let gc = pos.gives_check(m1);
            pos.do_move(m1, gc);
            let m2 = Move::from_usi("3c3d").unwrap();
            col.record_position(
                &pos,
                Some(-15),
                None,
                Some(m2),
                m2,
                &[legal_candidate(1, -15, "3c3d")],
            );
            col.finish_game(GameOutcome::Draw, TrainingDisposition::Adopt, 1).unwrap();
            col.flush().unwrap();
        }
        let bytes = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(u16::from_le_bytes([bytes[32], bytes[33]]), 2); // moveNum
        // hcp(32)+moveNum(2)+result(1)+opponent(1) + 2*(selectedMove16(2)+eval(2)+candidateNum(2)+1*(2+2)) = 56
        assert_eq!(bytes.len(), 56);
    }

    #[test]
    fn hcpe3_no_score_discards_partial_segment() {
        use rshogi_core::position::Position;
        use rshogi_core::types::Move;
        use tools::packed_sfen::pack_position_hcp;

        let path =
            std::env::temp_dir().join(format!("gensfen_hcpe3_gap_{}.hcpe3", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let mut pos = Position::new();
        pos.set_hirate();
        let m1 = Move::from_usi("7g7f").unwrap();
        // 欠落後に着手を進め、取り直しの起点が「次の有効局面」になることを確認する
        let gc = pos.gives_check(m1);
        let mut after = pos.clone();
        after.do_move(m1, gc);
        let m2 = Move::from_usi("3c3d").unwrap();
        let expected_hcp = pack_position_hcp(&after);
        {
            let mut col = TrainingDataCollector::new(
                &path,
                0,
                false,
                TrainingFormat::Hcpe3,
                1000,
                600.0,
                None,
            )
            .unwrap();
            col.start_game();
            // 局面A を記録 → 評価値欠落でセグメント破棄 → 局面C（着手後）で取り直す
            col.record_position(
                &pos,
                Some(50),
                None,
                Some(m1),
                m1,
                &[legal_candidate(1, 50, "7g7f")],
            );
            col.record_position(&pos, None, None, Some(m1), m1, &[]);
            col.record_position(
                &after,
                Some(40),
                None,
                Some(m2),
                m2,
                &[legal_candidate(1, 40, "3c3d")],
            );
            assert_eq!(col.stats().discarded_gap_positions, 1);
            col.finish_game(GameOutcome::BlackWin, TrainingDisposition::Adopt, 1).unwrap();
            col.flush().unwrap();
        }
        let bytes = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        // 破棄により書き出されるのは欠落後の 1 手だけで、起点 HCP は局面C（取り直し局面）
        assert_eq!(u16::from_le_bytes([bytes[32], bytes[33]]), 1);
        assert_eq!(&bytes[0..32], &expected_hcp);
    }

    #[test]
    fn pack_no_score_discards_partial_segment_and_restarts_replay() {
        use tools::packed_sfen::{move_to_psv_move16, pack_position_hcp, psv_move16_to_hcpe};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gap.pack");
        let mut pos = Position::new();
        pos.set_hirate();
        let first = Move::from_usi("7g7f").unwrap();
        let gives_check = pos.gives_check(first);
        let mut after = pos.clone();
        after.do_move(first, gives_check);
        let second = Move::from_usi("3c3d").unwrap();
        let expected_hcp = pack_position_hcp(&after);

        {
            let mut collector = TrainingDataCollector::new(
                &path,
                0,
                false,
                TrainingFormat::Pack,
                1000,
                600.0,
                None,
            )
            .unwrap();
            collector.record_position(&pos, Some(50), None, Some(first), first, &[]);
            collector.record_position(&pos, None, None, Some(first), first, &[]);
            collector.record_position(&after, Some(40), None, Some(second), second, &[]);
            assert_eq!(collector.stats().discarded_gap_positions, 1);
            collector
                .finish_game(GameOutcome::BlackWin, TrainingDisposition::Adopt, 1)
                .unwrap();
            collector.flush().unwrap();
        }

        let bytes = std::fs::read(path).unwrap();
        assert_eq!(bytes[0], 0); // 欠落後の局面は平手開始ではない
        assert_eq!(&bytes[1..33], &expected_hcp);
        assert_eq!(
            u16::from_le_bytes([bytes[35], bytes[36]]),
            psv_move16_to_hcpe(move_to_psv_move16(second))
        );
        assert_eq!(bytes.len(), 42); // 任意局面 header + 1手 + 終局 marker/reason
    }

    #[test]
    fn pack_hirate_position_with_non_initial_ply_writes_explicit_start_ply() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hirate_non_initial_ply.pack");
        let mut pos = Position::new();
        pos.set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 5")
            .unwrap();
        let mv = Move::from_usi("7g7f").unwrap();

        {
            let mut collector = TrainingDataCollector::new(
                &path,
                0,
                false,
                TrainingFormat::Pack,
                1000,
                600.0,
                None,
            )
            .unwrap();
            collector.record_position(&pos, Some(50), None, Some(mv), mv, &[]);
            collector
                .finish_game(GameOutcome::BlackWin, TrainingDisposition::Adopt, 1)
                .unwrap();
            collector.flush().unwrap();
        }

        let bytes = std::fs::read(path).unwrap();
        assert_eq!(bytes[0], 0);
        assert_eq!(u16::from_le_bytes([bytes[33], bytes[34]]), 5);
    }

    #[test]
    fn run_dir_lock_rejects_second_owner_and_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("gensfen.jsonl");
        let lock = RunDirLock::acquire(&output, false).unwrap();
        let error = RunDirLock::acquire(&output, false).unwrap_err().to_string();
        assert!(error.contains("out-dir is locked"));
        assert!(error.contains(&std::process::id().to_string()));
        drop(lock);
        assert!(!dir.path().join(".gensfen.lock").exists());
    }

    #[test]
    fn run_dir_lock_force_unlock_replaces_stale_file() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("gensfen.jsonl");
        std::fs::write(dir.path().join(".gensfen.lock"), b"{\"pid\":999999}\n").unwrap();
        let _lock = RunDirLock::acquire(&output, true).unwrap();
        let body = std::fs::read_to_string(dir.path().join(".gensfen.lock")).unwrap();
        assert!(body.contains(&std::process::id().to_string()));
    }

    #[cfg(unix)]
    #[test]
    fn worker_checkpoint_open_after_type_check_does_not_follow_symlink() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        let checkpoint = dir.path().join("checkpoint");
        std::fs::write(&target, b"preserve").unwrap();
        symlink(&target, &checkpoint).unwrap();

        assert!(open_worker_checkpoint_after_type_check(&checkpoint, true).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"preserve");
    }

    #[cfg(unix)]
    #[test]
    fn worker_checkpoint_truncate_does_not_follow_symlink() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        let checkpoint = dir.path().join("checkpoint");
        std::fs::write(&target, b"preserve").unwrap();
        symlink(&target, &checkpoint).unwrap();

        assert!(truncate_file(&checkpoint, 0).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"preserve");
    }
}
