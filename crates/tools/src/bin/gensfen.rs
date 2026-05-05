use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use anyhow::{Context, Result, anyhow, bail};
use chrono::Local;
use clap::Parser;
use crossbeam_channel as chan;
use rand::Rng;
use rand::seq::SliceRandom;
use rshogi_core::movegen::{MoveList, generate_legal, is_legal_with_pass};
use rshogi_core::position::Position;
use rshogi_core::types::{Color, Move};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::atomic::AtomicU64;
use tools::packed_sfen::{
    PackedSfenValue, move_to_cshogi_move16, move_to_move16, move16_to_move, pack_position,
    pack_position_hcp,
};
use tools::selfplay::{
    EngineConfig, EngineProcess, EvalLog, GameEngines, GameOutcome, NativeBackend, ParsedPosition,
    SearchParams, TimeControl, UsiBackend, build_position, load_start_positions, side_label,
};

const DEFAULT_EVAL_HASH_SIZE_MB: usize = 64;

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

    /// Flush game log on every move (safer, but slower)
    #[arg(long, default_value_t = false)]
    flush_each_move: bool,

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

    /// 学習データの出力形式（psv または pack）
    #[arg(long, default_value = "psv")]
    training_data_format: String,

    /// Number of concurrent worker threads
    #[arg(long, default_value_t = 1)]
    concurrency: usize,

    /// 前回中断した教師局面生成セッションを再開する。
    /// --out で指定した出力ファイルが存在する場合、完了済み対局数を検出して続きから実行する。
    #[arg(long, default_value_t = false)]
    resume: bool,

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

    /// 置換表を対局間で保持する（TT をクリアしない）。
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
    #[arg(long, default_value_t = 32000)]
    random_multi_pv_diff: i32,

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
    #[serde(default)]
    skip_initial_ply: u32,
    #[serde(default = "default_skip_in_check")]
    skip_in_check: bool,
    /// 先手の初期パス権数（パス権有効時のみ使用）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    initial_pass_count_black: Option<u8>,
    /// 後手の初期パス権数（パス権有効時のみ使用）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    initial_pass_count_white: Option<u8>,
    /// 開始局面シャッフルの乱数シード（--startpos-no-repeat 用）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    shuffle_seed: Option<u64>,
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
    game_id: u32,
    outcome: &'a str,
    reason: &'a str,
    plies: u32,
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
    reason: String,
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
    /// cshogi 可変長対局棋譜形式
    Pack,
}

/// 学習データ出力用のエントリ（game_result未設定の一時データ）
struct TrainingEntry {
    /// PackedSfen (32バイト)
    sfen: [u8; 32],
    /// 探索スコア（手番側から見た評価値）
    score: i16,
    /// 最善手 (Move16形式)
    move16: u16,
    /// 手数
    game_ply: u16,
    /// 手番（game_result計算用）
    side_to_move: Color,
}

/// 学習データ収集器
/// 対局中の局面データを収集し、対局終了後に勝敗を設定して書き出す
struct TrainingDataCollector {
    entries: Vec<TrainingEntry>,
    writer: BufWriter<File>,
    format: TrainingFormat,
    skip_initial_ply: u32,
    skip_in_check: bool,
    total_written: u64,
    skipped_initial: u64,
    skipped_in_check: u64,
    /// InProgress（手数制限/タイムアウト）で終了した対局のスキップ数
    skipped_in_progress: u64,
    /// .pack 形式用: 対局開始局面の HCP バイト列、手数、平手フラグ
    start_hcp: Option<([u8; 32], u16, bool)>,
    /// .pack 形式用: 平手局面の PackedSfen（平手判定の基準）
    hirate_packed_sfen: [u8; 32],
}

impl TrainingDataCollector {
    fn new(
        path: &Path,
        skip_initial_ply: u32,
        skip_in_check: bool,
        format: TrainingFormat,
    ) -> Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create training data directory: {}", parent.display())
            })?;
        }
        let file = File::create(path)
            .with_context(|| format!("failed to create training data file: {}", path.display()))?;

        // 平手判定用の PackedSfen を事前計算
        let mut hirate_pos = Position::new();
        hirate_pos.set_hirate();
        let hirate_packed_sfen = pack_position(&hirate_pos);

        Ok(Self {
            entries: Vec::new(),
            writer: BufWriter::new(file),
            format,
            skip_initial_ply,
            skip_in_check,
            total_written: 0,
            skipped_initial: 0,
            skipped_in_check: 0,
            skipped_in_progress: 0,
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

    /// 局面を記録（game_resultは後で設定）
    /// 注: game_plyとスキップ判定はpos.game_ply()を使用する
    /// （startpos+movesやSFEN手数指定のケースに対応するため）
    fn record_position(
        &mut self,
        pos: &Position,
        score_cp: Option<i32>,
        score_mate: Option<i32>,
        best_move: Option<Move>,
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
            // スコアがない場合は記録しない
            return;
        };

        // 最善手をMove16形式に変換
        let move16 = best_move.map_or(0, move_to_move16);

        // PackedSfenを生成
        let packed_sfen = pack_position(pos);

        // .pack 形式: 最初のエントリで開始局面の HCP を記録
        if self.format == TrainingFormat::Pack && self.start_hcp.is_none() {
            let is_hirate = packed_sfen == self.hirate_packed_sfen;
            let hcp = pack_position_hcp(pos);
            let ply = current_ply.clamp(0, u16::MAX as i32) as u16;
            self.start_hcp = Some((hcp, ply, is_hirate));
        }

        self.entries.push(TrainingEntry {
            sfen: packed_sfen,
            score,
            move16,
            game_ply: current_ply.clamp(0, u16::MAX as i32) as u16,
            side_to_move: pos.side_to_move(),
        });
    }

    /// 対局終了時に勝敗を設定して書き出す
    /// InProgress（手数制限/タイムアウト終了）の対局は学習データに含めない
    fn finish_game(&mut self, outcome: GameOutcome) -> Result<()> {
        // InProgressの対局は学習データとして不適切なので破棄
        if outcome == GameOutcome::InProgress {
            self.skipped_in_progress += self.entries.len() as u64;
            self.entries.clear();
            return Ok(());
        }

        if self.entries.is_empty() {
            return Ok(());
        }

        match self.format {
            TrainingFormat::Psv => self.finish_game_psv(outcome)?,
            TrainingFormat::Pack => self.finish_game_pack(outcome)?,
        }

        self.entries.clear();
        Ok(())
    }

    /// PSV 形式で書き出す（PackedSfenValue 40バイト固定長）
    fn finish_game_psv(&mut self, outcome: GameOutcome) -> Result<()> {
        for (idx, entry) in self.entries.iter().enumerate() {
            // game_result: 手番側から見た勝敗
            // 1 = 勝ち, 0 = 引き分け, -1 = 負け
            let game_result = match outcome {
                GameOutcome::BlackWin => {
                    if entry.side_to_move == Color::Black {
                        1i8
                    } else {
                        -1i8
                    }
                }
                GameOutcome::WhiteWin => {
                    if entry.side_to_move == Color::White {
                        1i8
                    } else {
                        -1i8
                    }
                }
                GameOutcome::Draw => 0i8,
                GameOutcome::InProgress => unreachable!(),
            };

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
            self.total_written += 1;
        }
        Ok(())
    }

    /// .pack 形式で書き出す（cshogi 可変長対局棋譜）
    ///
    /// フォーマット:
    ///   [開始局面フラグ: u8] — 1=平手, 0=任意局面
    ///   0 の場合: [HuffmanCodedPos: 32byte][game_ply: u16 LE]
    ///   繰り返し: [move16(cshogi): u16 LE][score: i16 LE]
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
            // YO move16 → Move → cshogi move16
            let mv = move16_to_move(entry.move16);
            let cshogi_move16 = move_to_cshogi_move16(mv);
            self.writer.write_all(&cshogi_move16.to_le_bytes())?;
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

    fn flush(&mut self) -> Result<()> {
        self.writer.flush()?;
        Ok(())
    }

    fn stats(&self) -> (u64, u64, u64, u64) {
        (
            self.total_written,
            self.skipped_initial,
            self.skipped_in_check,
            self.skipped_in_progress,
        )
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

    /// 重複なら true を返し、新規なら挿入して false を返す
    fn check_and_insert(&self, key: u64) -> bool {
        // key=0 は未使用エントリと区別できないので特殊扱い
        let effective_key = if key == 0 { 1 } else { key };
        let idx = (effective_key & self.mask) as usize;
        let old = self.table[idx].load(Ordering::Relaxed);
        if old == effective_key {
            return true;
        }
        self.table[idx].store(effective_key, Ordering::Relaxed);
        false
    }
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

/// MultiPV 候補からランダムに1手を選択する
///
/// PV1 のスコアとの差が `diff_threshold` 以内の候補からランダムに選択する。
/// 候補がない場合は None を返す。
fn select_multipv_random(
    candidates: &[tools::selfplay::MultiPvCandidate],
    diff_threshold: i32,
    rng: &mut impl Rng,
) -> Option<Move> {
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
    Some(selected.first_move)
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
    fn new(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create info-log directory {}", parent.display())
            })?;
        }
        let file = File::create(path)
            .with_context(|| format!("failed to create info log {}", path.display()))?;
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
    outcome: GameOutcome,
    outcome_reason: String,
}

struct WorkerOutput {
    training_stats: (u64, u64, u64, u64),
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
    // Pass rights
    pass_rights_enabled: bool,
    pass_black: Option<u8>,
    pass_white: Option<u8>,
    // Positions (shared across workers)
    start_defs: Arc<Vec<ParsedPosition>>,
    start_commands: Arc<Vec<String>>,
    // Output (temp paths)
    jsonl_path: PathBuf,
    info_path: Option<PathBuf>,
    eval_path: Option<PathBuf>,
    metrics_path: Option<PathBuf>,
    training_data_path: Option<PathBuf>,
    // Output flags
    flush_each_move: bool,
    // Training
    skip_initial_ply: u32,
    skip_in_check: bool,
    training_format: TrainingFormat,
    // gensfen: NativeBackend モード
    native_mode: bool,
    /// USI 単一エンジン最適化（先後同一エンジン時に 1 プロセスで兼用）。
    /// TT/履歴が先後で共有されるため、Elo 評価に影響する。
    /// --for-train 時のみ有効（棋力評価用途では使用しない）。
    usi_single: bool,
    eval_hash_size_mb: usize,
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
}

fn worker_main(
    cfg: WorkerConfig,
    rx: chan::Receiver<Option<GameTicket>>,
    tx: chan::Sender<WorkerGameResult>,
    shutdown: Arc<AtomicBool>,
) -> WorkerOutput {
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
        let mut writer = BufWriter::new(File::create(&cfg.jsonl_path).with_context(|| {
            format!("worker {}: failed to create {}", cfg.worker_id, cfg.jsonl_path.display())
        })?);
        let mut info_logger = if let Some(ref path) = cfg.info_path {
            Some(InfoLogger::new(path)?)
        } else {
            None
        };
        let mut eval_writer = if let Some(ref path) = cfg.eval_path {
            Some(BufWriter::new(File::create(path)?))
        } else {
            None
        };
        let mut metrics_writer = if let Some(ref path) = cfg.metrics_path {
            Some(BufWriter::new(File::create(path)?))
        } else {
            None
        };
        let mut training_data_collector = if let Some(ref path) = cfg.training_data_path {
            Some(TrainingDataCollector::new(
                path,
                cfg.skip_initial_ply,
                cfg.skip_in_check,
                cfg.training_format,
            )?)
        } else {
            None
        };

        let usi_initial_pass_count = {
            let parse = |opts: &[String]| -> Option<u8> {
                for opt in opts {
                    if let Some(val) = opt.strip_prefix("InitialPassCount=") {
                        return val.trim().parse().ok();
                    }
                    if let Some(val) = opt.strip_prefix("InitialPassCount = ") {
                        return val.trim().parse().ok();
                    }
                }
                None
            };
            parse(&cfg.black_usi_opts).or_else(|| parse(&cfg.white_usi_opts)).unwrap_or(2)
        };

        let dedup_hash = cfg.dedup_hash.clone();
        let mut rng = rand::rng();
        let mut dedup_hits = 0u64;
        let mut dedup_discarded = 0u64;
        let mut multipv_diversions = 0u64;
        let mut random_moves_played = 0u64;
        // dedup rate 監視用（interval ごとにリセット）
        let mut interval_games = 0u32;
        let mut interval_dedup_hits = 0u64;
        let mut interval_positions_checked = 0u64;

        // Game loop
        while let Ok(Some(ticket)) = rx.recv() {
            if shutdown.load(Ordering::Relaxed) {
                break;
            }

            let game_idx = ticket.game_idx;
            engines.prepare_game(cfg.keep_tt)?;

            let parsed = &cfg.start_defs[ticket.startpos_idx];
            let pass_b = if cfg.pass_rights_enabled {
                Some(cfg.pass_black.unwrap_or(usi_initial_pass_count))
            } else {
                None
            };
            let pass_w = if cfg.pass_rights_enabled {
                Some(cfg.pass_white.unwrap_or(usi_initial_pass_count))
            } else {
                None
            };
            let mut pos = build_position(parsed, pass_b, pass_w)?;
            let mut tc = TimeControl::new(cfg.btime, cfg.wtime, cfg.binc, cfg.winc, cfg.byoyomi);
            let mut outcome = GameOutcome::InProgress;
            let mut outcome_reason = "max_moves";
            let mut plies_played = 0u32;
            let mut move_list: Vec<String> = Vec::new();
            let mut eval_list: Vec<String> = Vec::new();
            let mut metrics = MetricsCollector::default();

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
                let engine_label = if side == Color::Black {
                    "black"
                } else {
                    "white"
                };
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
                        outcome_reason = "mate";
                        break;
                    }
                    let mv = legal_moves[rng.random_range(0..legal_moves.len())];
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
                    pos.do_move(mv, gives_check);
                    let rm_usi = mv.to_usi();
                    if eval_writer.is_some() {
                        eval_list.push("R".to_string());
                        move_list.push(rm_usi.clone());
                    }
                    continue;
                }

                // --- 通常探索 ---
                let think_limit_ms = tc.think_limit_ms(side);
                let pass_rights = if cfg.pass_rights_enabled {
                    Some((pos.pass_rights(Color::Black), pos.pass_rights(Color::White)))
                } else {
                    None
                };
                let params = SearchParams {
                    sfen: sfen_before.clone(),
                    time_args: tc.time_args(),
                    think_limit_ms,
                    timeout_margin_ms: cfg.timeout_margin_ms,
                    go_depth: cfg.go_depth,
                    go_nodes: cfg.go_nodes,
                    multi_pv: cfg.random_multi_pv.max(1),
                    pass_rights,
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
                    outcome_reason = "timeout";
                    terminal = true;
                    if search.best_move_usi.is_none() {
                        move_usi = "timeout".to_string();
                    }
                } else if let Some(ref mv_str) = search.best_move_usi {
                    match mv_str.as_str() {
                        "resign" => {
                            move_usi = mv_str.clone();
                            outcome = if side == Color::Black {
                                GameOutcome::WhiteWin
                            } else {
                                GameOutcome::BlackWin
                            };
                            outcome_reason = "resign";
                            terminal = true;
                        }
                        "win" => {
                            move_usi = mv_str.clone();
                            outcome = if side == Color::Black {
                                GameOutcome::BlackWin
                            } else {
                                GameOutcome::WhiteWin
                            };
                            outcome_reason = "win";
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
                                    let skip_record = if let Some(ref dh) = dedup_hash {
                                        interval_positions_checked += 1;
                                        if dh.check_and_insert(pos.key()) {
                                            dedup_hits += 1;
                                            interval_dedup_hits += 1;
                                            let discarded = training_data_collector
                                                .as_ref()
                                                .map_or(0, |c| c.entries_len() as u64);
                                            dedup_discarded += discarded;
                                            if let Some(ref mut collector) = training_data_collector
                                            {
                                                collector.start_game();
                                            }
                                            true
                                        } else {
                                            false
                                        }
                                    } else {
                                        false
                                    };

                                    // 学習データには PV1 のスコアと PV1 の手を記録する。
                                    // MultiPV ランダム選択で別の手がプレイされても、
                                    // 教師ラベルとしては「この局面での最善手 = PV1」が正しい。
                                    // （tanuki- の gensfen と同じ方式）
                                    if !skip_record
                                        && let Some(ref mut collector) = training_data_collector
                                    {
                                        collector.record_position(
                                            &pos,
                                            eval_log.as_ref().and_then(|e| e.score_cp),
                                            eval_log.as_ref().and_then(|e| e.score_mate),
                                            Some(mv),
                                        );
                                    }

                                    // --- gensfen: MultiPV ランダム選択 ---
                                    let played_mv = if cfg.random_multi_pv > 1 {
                                        if let Some(selected) = select_multipv_random(
                                            &search.multipv_candidates,
                                            cfg.random_multi_pv_diff,
                                            &mut rng,
                                        ) {
                                            if selected != mv {
                                                multipv_diversions += 1;
                                            }
                                            selected
                                        } else {
                                            mv
                                        }
                                    } else {
                                        mv
                                    };

                                    let gives_check = if played_mv.is_pass() {
                                        false
                                    } else {
                                        pos.gives_check(played_mv)
                                    };
                                    pos.do_move(played_mv, gives_check);
                                    tc.update_after_move(side, search.elapsed_ms);
                                    move_usi = played_mv.to_usi();
                                }
                                None => {
                                    outcome = if side == Color::Black {
                                        GameOutcome::WhiteWin
                                    } else {
                                        GameOutcome::BlackWin
                                    };
                                    outcome_reason = "illegal_move";
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
                        "mate"
                    } else {
                        "no_bestmove"
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
                }

                if terminal || outcome != GameOutcome::InProgress {
                    break;
                }
            }

            if outcome == GameOutcome::InProgress {
                outcome = GameOutcome::Draw;
                outcome_reason = "max_moves";
            }
            let result = ResultLog {
                kind: "result",
                game_id: game_idx + 1,
                outcome: outcome.label(),
                reason: outcome_reason,
                plies: plies_played,
            };
            serde_json::to_writer(&mut writer, &result)?;
            writer.write_all(b"\n")?;

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
                    reason: outcome_reason.to_string(),
                };
                serde_json::to_writer(&mut *w, &metrics_log)?;
                w.write_all(b"\n")?;
            }

            if let Some(ref mut collector) = training_data_collector {
                collector.finish_game(outcome)?;
            }
            writer.flush()?;

            let _ = tx.send(WorkerGameResult {
                outcome,
                outcome_reason: outcome_reason.to_string(),
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

        // gensfen 統計
        if dedup_hits > 0 || random_moves_played > 0 || multipv_diversions > 0 {
            eprintln!(
                "worker {}: gensfen stats: dedup_hits={}, dedup_discarded={}, multipv_diversions={}, random_moves={}",
                cfg.worker_id, dedup_hits, dedup_discarded, multipv_diversions, random_moves_played
            );
        }

        let training_stats = if let Some(ref mut collector) = training_data_collector {
            collector.flush()?;
            collector.stats()
        } else {
            (0, 0, 0, 0)
        };

        Ok(WorkerOutput { training_stats })
    };

    match run() {
        Ok(output) => output,
        Err(e) => {
            eprintln!("worker {}: error: {e}", cfg.worker_id);
            shutdown.store(true, Ordering::Relaxed);
            WorkerOutput {
                training_stats: (0, 0, 0, 0),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Resume support
// ---------------------------------------------------------------------------

/// 前回中断した教師局面生成セッションの進捗状態
struct ResumeState {
    /// 完了済み対局数（max game_id ベース）
    completed_games: u32,
    black_wins: u32,
    white_wins: u32,
    draws: u32,
    /// meta 行に保存された shuffle_seed（存在しない場合は None）
    shuffle_seed: Option<u64>,
}

/// 既存の JSONL 出力ファイルを解析し、完了済み対局数と勝敗を取得する。
/// ワーカー並行実行で result 行の順序が保証されないため、game_id の最大値を
/// completed_games とする。
fn parse_resume_state(path: &Path) -> Result<ResumeState> {
    let file = File::open(path)
        .with_context(|| format!("failed to open {} for resume", path.display()))?;
    let reader = BufReader::new(file);

    let mut max_game_id: u32 = 0;
    let mut black_wins: u32 = 0;
    let mut white_wins: u32 = 0;
    let mut draws: u32 = 0;
    let mut shuffle_seed: Option<u64> = None;
    let mut last_parse_error = false;

    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: Value = match serde_json::from_str(trimmed) {
            Ok(v) => {
                last_parse_error = false;
                v
            }
            Err(_) => {
                // 最終行の不完全な書き込みを許容
                last_parse_error = true;
                continue;
            }
        };
        match value.get("type").and_then(|v| v.as_str()) {
            Some("meta") => {
                // meta 行から shuffle_seed を復元
                shuffle_seed = value
                    .get("settings")
                    .and_then(|s| s.get("shuffle_seed"))
                    .and_then(|v| v.as_u64());
            }
            Some("result") => {
                if let Some(gid) = value.get("game_id").and_then(|v| v.as_u64()) {
                    max_game_id = max_game_id.max(gid as u32);
                }
                match value.get("outcome").and_then(|v| v.as_str()) {
                    Some("black_win") => black_wins += 1,
                    Some("white_win") => white_wins += 1,
                    Some("draw") => draws += 1,
                    _ => {}
                }
            }
            _ => {}
        }
    }

    // 最終行以外でパースエラーが起きた場合の警告は不要（last_parse_error は最終行のみ）
    let _ = last_parse_error;

    Ok(ResumeState {
        completed_games: max_game_id,
        black_wins,
        white_wins,
        draws,
        shuffle_seed,
    })
}

/// Concatenate worker temp files.
/// `append=true`: append to existing file (for JSONL with meta line).
/// `append=false`: create new file.
fn concatenate_temp_files(final_path: &Path, temp_paths: &[PathBuf], append: bool) -> Result<()> {
    let mut out: File = if append {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(final_path)
            .with_context(|| format!("failed to open {} for appending", final_path.display()))?
    } else {
        File::create(final_path)
            .with_context(|| format!("failed to create {}", final_path.display()))?
    };
    for tmp in temp_paths {
        match File::open(tmp) {
            Ok(mut f) => {
                std::io::copy(&mut f, &mut out)?;
                std::fs::remove_file(tmp)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let mut cli = Cli::parse();

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

    // USIオプションからパス権情報を早期に解析（load_start_positions で使用するため）
    let common_usi_opts_early = cli.usi_options.clone().unwrap_or_default();
    let black_usi_opts_early =
        cli.usi_options_black.clone().unwrap_or_else(|| common_usi_opts_early.clone());
    let white_usi_opts_early =
        cli.usi_options_white.clone().unwrap_or_else(|| common_usi_opts_early.clone());
    let is_pass_rights_enabled_early = |o: &str| {
        o == "PassRights=true"
            || o == "PassRights = true"
            || o == "PassRights=1"
            || o == "PassRights = 1"
    };
    let pass_rights_via_usi_early =
        black_usi_opts_early.iter().any(|o| is_pass_rights_enabled_early(o))
            || white_usi_opts_early.iter().any(|o| is_pass_rights_enabled_early(o));

    let parse_initial_pass_count_early = |opts: &[String]| -> Option<u8> {
        for opt in opts {
            if let Some(val) = opt.strip_prefix("InitialPassCount=") {
                return val.trim().parse().ok();
            }
            if let Some(val) = opt.strip_prefix("InitialPassCount = ") {
                return val.trim().parse().ok();
            }
        }
        None
    };
    let usi_initial_pass_count_early = parse_initial_pass_count_early(&black_usi_opts_early)
        .or_else(|| parse_initial_pass_count_early(&white_usi_opts_early))
        .unwrap_or(2);

    let load_pass_black = if pass_rights_via_usi_early {
        Some(usi_initial_pass_count_early)
    } else {
        None
    };
    let load_pass_white = if pass_rights_via_usi_early {
        Some(usi_initial_pass_count_early)
    } else {
        None
    };

    let (start_defs, start_commands) = load_start_positions(
        cli.startpos_file.as_deref(),
        cli.sfen.as_deref(),
        load_pass_black,
        load_pass_white,
    )?;
    let timestamp = Local::now();
    let output_path = resolve_output_path(cli.out_dir.as_deref(), &timestamp);
    let info_path = output_path.with_extension("info.jsonl");

    // --resume バリデーションと進捗読み取り
    let resume_state = if cli.resume {
        if cli.out_dir.is_none() {
            bail!(
                "--resume には --out-dir の指定が必要です（自動生成パスでは前回のディレクトリを特定できません）"
            );
        }
        if !output_path.exists() {
            bail!("--resume: 出力ファイルが見つかりません: {}", output_path.display());
        }
        let state = parse_resume_state(&output_path)?;
        if state.completed_games >= cli.games {
            println!(
                "全{}局が完了済みです（black {} / white {} / draw {}）。再開は不要です。",
                state.completed_games, state.black_wins, state.white_wins, state.draws,
            );
            return Ok(());
        }
        println!(
            "Resuming: {}/{}局完了済み（black {} / white {} / draw {}）",
            state.completed_games, cli.games, state.black_wins, state.white_wins, state.draws,
        );
        Some(state)
    } else {
        None
    };
    let resume_offset = resume_state.as_ref().map_or(0, |s| s.completed_games);

    if let Some(parent) = output_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    // 学習データ出力形式のパース
    let training_format = match cli.training_data_format.as_str() {
        "psv" => TrainingFormat::Psv,
        "pack" => TrainingFormat::Pack,
        other => bail!("unknown training data format: '{}' (expected 'psv' or 'pack')", other),
    };

    // 学習データ出力の初期化（デフォルトで有効、--no-training-data で無効化）
    let training_data_ext = match training_format {
        TrainingFormat::Psv => "psv",
        TrainingFormat::Pack => "pack",
    };
    let training_data_path = Some(
        cli.output_training_data
            .clone()
            .unwrap_or_else(|| default_training_data_path(&output_path, training_data_ext)),
    );
    // パス権は PSV 形式でエンコードできないため gensfen ではサポートしない。
    // USI options で PassRights=true が渡された場合のみ早期 bail する。
    if pass_rights_via_usi_early {
        bail!(
            "PassRights USI option is not supported by gensfen (PackedSfen format cannot encode pass moves)"
        );
    }
    let training_data_enabled = training_data_path.is_some();

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

    let is_pass_rights_enabled = |o: &str| {
        o == "PassRights=true"
            || o == "PassRights = true"
            || o == "PassRights=1"
            || o == "PassRights = 1"
    };
    let pass_rights_via_usi = black_usi_opts.iter().any(|o| is_pass_rights_enabled(o))
        || white_usi_opts.iter().any(|o| is_pass_rights_enabled(o));

    let parse_initial_pass_count = |opts: &[String]| -> Option<u8> {
        for opt in opts {
            if let Some(val) = opt.strip_prefix("InitialPassCount=") {
                return val.trim().parse().ok();
            }
            if let Some(val) = opt.strip_prefix("InitialPassCount = ") {
                return val.trim().parse().ok();
            }
        }
        None
    };
    let usi_initial_pass_count = parse_initial_pass_count(&black_usi_opts)
        .or_else(|| parse_initial_pass_count(&white_usi_opts))
        .unwrap_or(2);

    let pass_rights_enabled = pass_rights_via_usi;

    // gensfen オプションのデフォルト解決（meta 書き込みより前に解決する必要がある）
    let native_mode = cli.native.unwrap_or(true);

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

    // shuffle_seed の解決: CLI 指定 > meta から復元 > ランダム生成
    // resume 時は meta の seed と CLI の seed が不一致ならエラー（順列が変わるため）
    let shuffle_seed_resolved: Option<u64> = if startpos_no_repeat_resolved {
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
            if meta_seed.is_none() {
                bail!(
                    "Cannot resume with --startpos-no-repeat: \
                     the original session did not save shuffle_seed in meta. \
                     Re-run without --startpos-no-repeat or start a new session."
                );
            }
            meta_seed
        } else if let Some(seed) = cli.shuffle_seed {
            Some(seed)
        } else {
            Some(rand::random::<u64>())
        }
    } else {
        None
    };

    // Write meta line to final JSONL (resume時はスキップ: 既にメタ行が存在する)
    if !cli.resume {
        let mut writer = BufWriter::new(
            File::create(&output_path)
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
                output_training_data: training_data_path.as_ref().map(|p| p.display().to_string()),
                skip_initial_ply: cli.skip_initial_ply,
                skip_in_check: cli.skip_in_check,
                initial_pass_count_black: if pass_rights_enabled {
                    Some(usi_initial_pass_count)
                } else {
                    None
                },
                initial_pass_count_white: if pass_rights_enabled {
                    Some(usi_initial_pass_count)
                } else {
                    None
                },
                shuffle_seed: shuffle_seed_resolved,
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
        };
        serde_json::to_writer(&mut writer, &meta)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
    }

    let keep_tt_resolved = cli.keep_tt.unwrap_or(false);
    let dedup_hash_size_resolved = cli.dedup_hash_size.unwrap_or(64 * 1024 * 1024);
    let random_multi_pv_resolved = cli.random_multi_pv.unwrap_or(0);

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

    // dedup 警告の重複抑制フラグ（全ワーカー共有）
    let dedup_warn_emitted = Arc::new(AtomicBool::new(false));

    // NativeBackend 使用時に NNUE 評価関数の初期化
    if native_mode {
        let eval_file =
            cli.eval_file.as_ref().ok_or_else(|| anyhow!("--native requires --eval-file"))?;
        rshogi_core::nnue::init_nnue(eval_file).map_err(|e| anyhow!("NNUE init failed: {e}"))?;
        eprintln!("NativeBackend: NNUE loaded from {}", eval_file.display());
    }

    // ゲームチケットは逐次生成する。
    // `--games` が極端に大きい場合でも O(1) メモリで dispatch できるようにする。
    let mut rng = rand::rng();
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
        let w_training_path = if training_data_enabled {
            Some(output_parent.join(format!("{output_stem}.w{w}.{training_data_ext}")))
        } else {
            None
        };

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
            max_moves: cli.max_moves,
            timeout_margin_ms: cli.timeout_margin_ms,
            btime: cli.btime,
            wtime: cli.wtime,
            binc: cli.binc,
            winc: cli.winc,
            byoyomi: cli.byoyomi,
            go_depth: cli.depth,
            go_nodes: cli.nodes,
            pass_rights_enabled,
            pass_black: if pass_rights_enabled {
                Some(usi_initial_pass_count)
            } else {
                None
            },
            pass_white: if pass_rights_enabled {
                Some(usi_initial_pass_count)
            } else {
                None
            },
            start_defs: Arc::clone(&shared_start_defs),
            start_commands: Arc::clone(&shared_start_commands),
            jsonl_path,
            info_path: w_info_path,
            eval_path: w_eval_path,
            metrics_path: w_metrics_path,
            training_data_path: w_training_path,
            flush_each_move: cli.flush_each_move,
            skip_initial_ply: cli.skip_initial_ply,
            skip_in_check: cli.skip_in_check,
            training_format,
            native_mode,
            usi_single,
            eval_hash_size_mb: DEFAULT_EVAL_HASH_SIZE_MB,
            keep_tt: keep_tt_resolved,
            dedup_hash: shared_dedup_hash.clone(),
            random_multi_pv: random_multi_pv_resolved,
            random_multi_pv_diff: cli.random_multi_pv_diff,
            random_move_count: cli.random_move_count,
            random_move_min_ply: cli.random_move_min_ply,
            random_move_max_ply: cli.random_move_max_ply,
            dedup_warn_interval_per_worker: (cli.dedup_warn_interval / cli.concurrency as u32)
                .max(1),
            dedup_warn_rate: cli.dedup_warn_rate,
            dedup_warn_emitted: Arc::clone(&dedup_warn_emitted),
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
    // Main thread doesn't send results
    drop(result_tx);

    // Main loop: dispatch tickets and collect results
    //
    // --startpos-no-repeat: seed 固定の StdRng でシャッフルし、resume 時は
    // 同じ seed + completed_games 回の next() で順列位置を復元する。
    // seed は meta 行に shuffle_seed として保存される。
    let mut shuffled_startpos = if startpos_no_repeat_resolved {
        let seed = if let Some(s) = shuffle_seed_resolved {
            s
        } else {
            // 新規セッションなのに seed が無い = バグ（上流で必ず設定される）
            bail!("internal error: shuffle_seed not set for --startpos-no-repeat");
        };
        let mut s = ShuffledStartpos::new(startpos_count, seed);
        // resume 時は完了済み対局分だけ消費して同一位置まで進める
        for _ in 0..resume_offset {
            s.next();
        }
        if resume_offset > 0 {
            eprintln!(
                "resume: restored startpos-no-repeat position (seed={}, skip={})",
                seed, resume_offset
            );
        }
        Some(s)
    } else {
        None
    };
    let mut next_game_idx = resume_offset;
    let make_ticket = |game_idx: u32,
                       rng: &mut rand::rngs::ThreadRng,
                       shuffled: &mut Option<ShuffledStartpos>| {
        if let Some(s) = shuffled.as_mut() {
            GameTicket {
                game_idx,
                startpos_idx: s.next(),
            }
        } else {
            make_game_ticket(game_idx, cli.random_startpos, startpos_count, rng)
        }
    };
    let mut next_ticket = (next_game_idx < cli.games)
        .then(|| make_ticket(next_game_idx, &mut rng, &mut shuffled_startpos));
    let mut completed = resume_offset;
    let mut black_wins = resume_state.as_ref().map_or(0, |s| s.black_wins);
    let mut white_wins = resume_state.as_ref().map_or(0, |s| s.white_wins);
    let mut draws = resume_state.as_ref().map_or(0, |s| s.draws);

    let handle_result = |result: WorkerGameResult,
                         black_wins: &mut u32,
                         white_wins: &mut u32,
                         draws: &mut u32,
                         completed: &mut u32| {
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
            cli.games,
            result.outcome.label(),
            result.outcome_reason,
            black_wins,
            white_wins,
            draws
        );
    };

    while completed < cli.games && !shutdown.load(Ordering::Relaxed) {
        match next_ticket.take() {
            None => {
                // All tickets dispatched, just wait for results
                match result_rx.recv() {
                    Ok(result) => {
                        handle_result(
                            result,
                            &mut black_wins,
                            &mut white_wins,
                            &mut draws,
                            &mut completed,
                        );
                    }
                    Err(_) => break,
                }
            }
            Some(t) => {
                chan::select! {
                    send(ticket_tx, Some(t.clone())) -> res => {
                        if res.is_ok() {
                            next_game_idx += 1;
                            next_ticket = (next_game_idx < cli.games).then(|| {
                                make_ticket(next_game_idx, &mut rng, &mut shuffled_startpos)
                            });
                        }
                    }
                    recv(result_rx) -> result => {
                        // Put the ticket back since we received a result instead of sending
                        next_ticket = Some(t);
                        if let Ok(result) = result {
                            handle_result(result, &mut black_wins, &mut white_wins, &mut draws, &mut completed);
                        }
                    }
                }
            }
        }
    }

    // Signal workers to stop
    for _ in 0..cli.concurrency {
        let _ = ticket_tx.send(None);
    }
    drop(ticket_tx); // チャネル閉鎖でワーカーの recv が終了する

    // グレースフルシャットダウン後、ワーカーが完了したゲームの結果を回収する。
    // Ctrl-C 後もワーカーは進行中のゲームを完了させるため、
    // メインスレッドのカウンタがずれないようここで drain する。
    while let Ok(result) = result_rx.recv() {
        handle_result(result, &mut black_wins, &mut white_wins, &mut draws, &mut completed);
    }

    // Join workers and collect training stats
    let mut total_written = 0u64;
    let mut total_skipped_initial = 0u64;
    let mut total_skipped_in_check = 0u64;
    let mut total_skipped_in_progress = 0u64;
    for h in handles {
        if let Ok(output) = h.join() {
            let (tw, si, sic, sip) = output.training_stats;
            total_written += tw;
            total_skipped_initial += si;
            total_skipped_in_check += sic;
            total_skipped_in_progress += sip;
        }
    }

    // Concatenate temp files into final outputs
    // resume時は既存ファイルに追記する
    let append_mode = cli.resume;
    concatenate_temp_files(&output_path, &temp_jsonl_paths, true)?;

    if cli.log_info && !temp_info_paths.is_empty() {
        concatenate_temp_files(&info_path, &temp_info_paths, append_mode)?;
    }
    if cli.emit_eval_file && !temp_eval_paths.is_empty() {
        let eval_path = default_eval_path(&output_path);
        concatenate_temp_files(&eval_path, &temp_eval_paths, append_mode)?;
    }
    if cli.emit_metrics && !temp_metrics_paths.is_empty() {
        let metrics_path = default_metrics_path(&output_path);
        concatenate_temp_files(&metrics_path, &temp_metrics_paths, append_mode)?;
    }
    if training_data_enabled && !temp_pack_paths.is_empty() {
        let pack_path = training_data_path
            .as_ref()
            .cloned()
            .unwrap_or_else(|| default_training_data_path(&output_path, training_data_ext));
        concatenate_temp_files(&pack_path, &temp_pack_paths, append_mode)?;
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
    if training_data_enabled {
        println!();
        println!("--- Training Data ---");
        println!("Total positions written: {total_written}");
        println!("Skipped (initial ply 1-{}): {total_skipped_initial}", cli.skip_initial_ply);
        if cli.skip_in_check {
            println!("Skipped (in check): {total_skipped_in_check}");
        }
        if total_skipped_in_progress > 0 {
            println!("Skipped (in progress games): {total_skipped_in_progress}");
        }
        println!(
            "Output: {}",
            training_data_path.as_ref().map_or("-".to_string(), |p| p.display().to_string())
        );
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
        use tools::selfplay::MultiPvCandidate;

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
            let mv = selected.unwrap();
            assert!(mv == mv1 || mv == mv2);
        }
    }

    #[test]
    fn select_multipv_random_returns_none_for_empty() {
        let mut rng = StdRng::seed_from_u64(42);
        assert!(select_multipv_random(&[], 100, &mut rng).is_none());
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
}
