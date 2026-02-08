use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use chrono::Local;
use clap::Parser;
use rand::prelude::IndexedRandom;
use rshogi_core::movegen::is_legal_with_pass;
use rshogi_core::position::Position;
use rshogi_core::types::{Color, Move, PieceType, Square};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tools::packed_sfen::{move_to_move16, pack_position, PackedSfenValue};
use tools::selfplay::{
    build_position, load_start_positions, parse_position_line, side_label, EngineConfig,
    EngineProcess, EvalLog, GameOutcome, SearchRequest, TimeControl,
};

/// engine-usi 同士の自己対局ハーネス。時間管理と info ログ収集を最小限に実装する。
///
/// # よく使うコマンド例
///
/// - 1秒秒読みで数をこなす（infoログなし、デフォルト出力先）:
///   `cargo run -p tools --bin engine_selfplay -- --games 10 --max-moves 300 --byoyomi 1000`
///
/// - 5秒秒読み + network-delay2=1120、infoログ付きで指定パスに出力:
///   `cargo run -p tools --bin engine_selfplay -- --games 2 --max-moves 300 --byoyomi 5000 --network-delay2 1120 --log-info --out runs/selfplay/byoyomi5s.jsonl`
///
/// - 特定SFENの再現（startposファイルを用意して1局だけ）:
///   `cargo run -p tools --bin engine_selfplay -- --games 1 --max-moves 300 --byoyomi 5000 --startpos-file sfen.txt --log-info`
///
/// - 学習データを生成しながら対局:
///   `cargo run -p tools --bin engine_selfplay -- --games 100 --byoyomi 1000 --output-training-data output.pack`
///
/// `--out` 未指定時は `runs/selfplay/<timestamp>-selfplay.jsonl` に書き出し、infoは同名 `.info.jsonl` を生成する。
///
#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "rshogi-usi selfplay harness (engine vs engine)"
)]
struct Cli {
    /// Number of games to run
    #[arg(long, default_value_t = 1)]
    games: u32,

    /// Maximum plies per game before declaring a draw
    #[arg(long, default_value_t = 512)]
    max_moves: u32,

    /// Enable pass rights (finite pass mode) with specified number of passes for Black
    #[arg(long)]
    pass_rights_black: Option<u8>,

    /// Enable pass rights (finite pass mode) with specified number of passes for White
    #[arg(long)]
    pass_rights_white: Option<u8>,

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

    /// Output path (defaults to runs/selfplay/<timestamp>-selfplay.jsonl)
    #[arg(long)]
    out: Option<PathBuf>,

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
    /// 指定しない場合はデフォルトで <output>.pack に出力
    #[arg(long)]
    output_training_data: Option<PathBuf>,

    /// 学習データ出力を無効化
    #[arg(long, default_value_t = false)]
    no_training_data: bool,

    /// 学習データ出力時に序盤の手数をスキップする（1手目からN手目まで）
    /// ランダム性確保のため、序盤の定跡手順をスキップする
    #[arg(
        long,
        default_value_t = 8,
        help = "Skip initial N plies (1 to N) for training data"
    )]
    skip_initial_ply: u32,

    /// 学習データ出力時に王手局面をスキップする
    /// 王手局面は応手が限られるため学習価値が低い
    /// 無効化するには --skip-in-check=false を指定
    #[arg(
        long,
        default_value_t = true,
        action = clap::ArgAction::Set,
        help = "Skip positions where king is in check (use --skip-in-check=false to disable)"
    )]
    skip_in_check: bool,
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
struct MoveLog {
    #[serde(rename = "type")]
    kind: &'static str,
    game_id: u32,
    ply: u32,
    side_to_move: char,
    sfen_before: String,
    move_usi: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    raw_move_usi: Option<String>,
    engine: String,
    elapsed_ms: u64,
    think_limit_ms: u64,
    timed_out: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    eval: Option<EvalLog>,
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

/// 対局セッション全体のサマリ
#[derive(Serialize)]
struct SummaryLog {
    #[serde(rename = "type")]
    kind: &'static str,
    timestamp: String,
    total_games: u32,
    black_wins: u32,
    white_wins: u32,
    draws: u32,
    black_win_rate: f64,
    white_win_rate: f64,
    draw_rate: f64,
    engine_black: EngineSummary,
    engine_white: EngineSummary,
    time_control: TimeControlSummary,
}

#[derive(Serialize)]
struct EngineSummary {
    path: String,
    name: String,
    usi_options: Vec<String>,
    threads: usize,
}

#[derive(Serialize)]
struct TimeControlSummary {
    btime: u64,
    wtime: u64,
    binc: u64,
    winc: u64,
    byoyomi: u64,
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
    skip_initial_ply: u32,
    skip_in_check: bool,
    total_written: u64,
    skipped_initial: u64,
    skipped_in_check: u64,
    /// InProgress（手数制限/タイムアウト）で終了した対局のスキップ数
    skipped_in_progress: u64,
}

impl TrainingDataCollector {
    fn new(path: &Path, skip_initial_ply: u32, skip_in_check: bool) -> Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!("failed to create training data directory: {}", parent.display())
                })?;
            }
        }
        let file = File::create(path)
            .with_context(|| format!("failed to create training data file: {}", path.display()))?;
        Ok(Self {
            entries: Vec::new(),
            writer: BufWriter::new(file),
            skip_initial_ply,
            skip_in_check,
            total_written: 0,
            skipped_initial: 0,
            skipped_in_check: 0,
            skipped_in_progress: 0,
        })
    }

    /// 新しい対局を開始（エントリをクリア）
    fn start_game(&mut self) {
        self.entries.clear();
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
                GameOutcome::InProgress => unreachable!(), // 上でreturn済み
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
        self.entries.clear();
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
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!("failed to create info-log directory {}", parent.display())
                })?;
            }
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

fn main() -> Result<()> {
    let mut cli = Cli::parse();

    // 時間制限のバリデーション: すべて0の場合は無限思考モードになりタイムアウト問題が発生するため警告
    if cli.btime == 0 && cli.wtime == 0 && cli.byoyomi == 0 && cli.binc == 0 && cli.winc == 0 {
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

    let pass_rights_cli_specified =
        cli.pass_rights_black.is_some() || cli.pass_rights_white.is_some();
    let load_pass_black = if pass_rights_cli_specified || pass_rights_via_usi_early {
        Some(cli.pass_rights_black.unwrap_or(usi_initial_pass_count_early))
    } else {
        None
    };
    let load_pass_white = if pass_rights_cli_specified || pass_rights_via_usi_early {
        Some(cli.pass_rights_white.unwrap_or(usi_initial_pass_count_early))
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
    let output_path = resolve_output_path(cli.out.as_deref(), &timestamp);
    let info_path = output_path.with_extension("info.jsonl");

    if let Some(parent) = output_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
    }
    let mut writer = BufWriter::new(
        File::create(&output_path)
            .with_context(|| format!("failed to open {}", output_path.display()))?,
    );
    let mut info_logger = if cli.log_info {
        Some(InfoLogger::new(&info_path)?)
    } else {
        None
    };
    let mut eval_writer = if cli.emit_eval_file {
        let eval_path = default_eval_path(&output_path);
        if let Some(parent) = eval_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }
        }
        Some(BufWriter::new(
            File::create(&eval_path)
                .with_context(|| format!("failed to create {}", eval_path.display()))?,
        ))
    } else {
        None
    };
    let mut metrics_writer = if cli.emit_metrics {
        let metrics_path = default_metrics_path(&output_path);
        if let Some(parent) = metrics_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }
        }
        Some(BufWriter::new(
            File::create(&metrics_path)
                .with_context(|| format!("failed to create {}", metrics_path.display()))?,
        ))
    } else {
        None
    };

    // 学習データ出力の初期化（デフォルトで有効、--no-training-data で無効化）
    let training_data_path = match (cli.no_training_data, &cli.output_training_data) {
        (true, _) => None,
        (false, Some(path)) => Some(path.clone()),
        (false, None) => Some(default_training_data_path(&output_path)),
    };
    // パス権有効時は学習データ収集を抑止（PackedSfen形式がパス権をサポートしていないため）
    let pass_rights_active = pass_rights_cli_specified || pass_rights_via_usi_early;
    let mut training_data_collector = if let Some(ref path) = training_data_path {
        if pass_rights_active {
            eprintln!(
                "Warning: Training data collection is disabled when pass rights are enabled \
                 (PackedSfen format does not support pass rights)"
            );
            None
        } else {
            Some(TrainingDataCollector::new(path, cli.skip_initial_ply, cli.skip_in_check)?)
        }
    } else {
        None
    };

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
    let common_args = cli.engine_args.clone().unwrap_or_default();
    let black_args = cli.engine_args_black.clone().unwrap_or_else(|| common_args.clone());
    let white_args = cli.engine_args_white.clone().unwrap_or(common_args.clone());

    let common_usi_opts = cli.usi_options.clone().unwrap_or_default();
    let mut black_usi_opts =
        cli.usi_options_black.clone().unwrap_or_else(|| common_usi_opts.clone());
    let mut white_usi_opts =
        cli.usi_options_white.clone().unwrap_or_else(|| common_usi_opts.clone());

    // パス権オプションが指定されている場合、PassRights=true を自動追加
    if cli.pass_rights_black.is_some() || cli.pass_rights_white.is_some() {
        let pass_rights_opt = "PassRights=true".to_string();
        if !black_usi_opts.iter().any(|o| o.starts_with("PassRights")) {
            black_usi_opts.push(pass_rights_opt.clone());
        }
        if !white_usi_opts.iter().any(|o| o.starts_with("PassRights")) {
            white_usi_opts.push(pass_rights_opt);
        }
    }

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

    let pass_rights_enabled =
        cli.pass_rights_black.is_some() || cli.pass_rights_white.is_some() || pass_rights_via_usi;

    let mut black = EngineProcess::spawn(
        &EngineConfig {
            path: engine_paths.black.path.clone(),
            args: black_args.clone(),
            threads: threads_black,
            hash_mb: cli.hash_mb,
            network_delay: cli.network_delay,
            network_delay2: cli.network_delay2,
            minimum_thinking_time: cli.minimum_thinking_time,
            slowmover: cli.slowmover,
            ponder: cli.ponder,
            usi_options: black_usi_opts.clone(),
        },
        "black".to_string(),
    )?;
    let mut white = EngineProcess::spawn(
        &EngineConfig {
            path: engine_paths.white.path.clone(),
            args: white_args.clone(),
            threads: threads_white,
            hash_mb: cli.hash_mb,
            network_delay: cli.network_delay,
            network_delay2: cli.network_delay2,
            minimum_thinking_time: cli.minimum_thinking_time,
            slowmover: cli.slowmover,
            ponder: cli.ponder,
            usi_options: white_usi_opts.clone(),
        },
        "white".to_string(),
    )?;

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
                Some(cli.pass_rights_black.unwrap_or(usi_initial_pass_count))
            } else {
                None
            },
            initial_pass_count_white: if pass_rights_enabled {
                Some(cli.pass_rights_white.unwrap_or(usi_initial_pass_count))
            } else {
                None
            },
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

    // 勝敗カウンター
    let mut black_wins = 0u32;
    let mut white_wins = 0u32;
    let mut draws = 0u32;

    // 開始局面選択用のRNG
    let mut rng = rand::rng();

    for game_idx in 0..cli.games {
        black.new_game()?;
        white.new_game()?;
        // 開始局面を選択（ランダムまたは順繰り）
        let parsed = if cli.random_startpos {
            start_defs.choose(&mut rng).unwrap()
        } else {
            &start_defs[(game_idx as usize) % start_defs.len()]
        };
        let pass_black = if pass_rights_enabled {
            Some(cli.pass_rights_black.unwrap_or(usi_initial_pass_count))
        } else {
            None
        };
        let pass_white = if pass_rights_enabled {
            Some(cli.pass_rights_white.unwrap_or(usi_initial_pass_count))
        } else {
            None
        };
        let mut pos = build_position(parsed, pass_black, pass_white)?;
        let mut tc = TimeControl::new(cli.btime, cli.wtime, cli.binc, cli.winc, cli.byoyomi);
        let mut outcome = GameOutcome::InProgress;
        let mut outcome_reason = "max_moves";
        let mut plies_played = 0u32;
        let mut move_list: Vec<String> = Vec::new();
        let mut eval_list: Vec<String> = Vec::new();
        let mut metrics = MetricsCollector::default();

        // 学習データ収集を開始
        if let Some(ref mut collector) = training_data_collector {
            collector.start_game();
        }

        for ply_idx in 0..cli.max_moves {
            plies_played = ply_idx + 1;
            let side = pos.side_to_move();
            let engine = if side == Color::Black {
                &mut black
            } else {
                &mut white
            };
            let engine_label = if side == Color::Black {
                "black"
            } else {
                "white"
            };
            let sfen_before = pos.to_sfen();
            let think_limit_ms = tc.think_limit_ms(side);
            let pass_rights = if pass_rights_enabled {
                Some((pos.pass_rights(Color::Black), pos.pass_rights(Color::White)))
            } else {
                None
            };
            let req = SearchRequest {
                sfen: &sfen_before,
                time_args: tc.time_args(),
                think_limit_ms,
                timeout_margin_ms: cli.timeout_margin_ms,
                game_id: game_idx + 1,
                ply: plies_played,
                side,
                engine_label: engine_label.to_string(),
                pass_rights,
            };
            // InfoLogger をクロージャ経由で渡す
            type InfoCb<'a> = Box<dyn FnMut(&str, &SearchRequest<'_>) + 'a>;
            let mut info_cb: Option<InfoCb<'_>> = if info_logger.is_some() {
                Some(Box::new(|line: &str, req: &SearchRequest<'_>| {
                    if let Some(ref mut logger) = info_logger {
                        let _ = logger.log(InfoLogEntry {
                            kind: "info",
                            game_id: req.game_id,
                            ply: req.ply,
                            side_to_move: side_label(req.side),
                            engine: &req.engine_label,
                            line,
                        });
                    }
                }))
            } else {
                None
            };
            let search = engine.search(
                &req,
                info_cb.as_mut().map(|b| b.as_mut() as &mut dyn FnMut(&str, &SearchRequest<'_>)),
            )?;

            let timed_out = search.timed_out;
            let mut move_usi = search.bestmove.clone().unwrap_or_else(|| "none".to_string());
            let mut raw_move_usi = None;
            let mut terminal = false;
            let elapsed_ms = search.elapsed_ms;
            let eval_log = search.eval.clone();

            if timed_out {
                outcome = if side == Color::Black {
                    GameOutcome::WhiteWin
                } else {
                    GameOutcome::BlackWin
                };
                outcome_reason = "timeout";
                terminal = true;
                if search.bestmove.is_none() {
                    move_usi = "timeout".to_string();
                }
            } else if let Some(ref mv_str) = search.bestmove {
                raw_move_usi = Some(mv_str.clone());
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
                    _ => match Move::from_usi(mv_str) {
                        Some(mv) if is_legal_with_pass(&pos, mv) => {
                            // 学習データを記録（do_move前に記録することが重要）
                            if let Some(ref mut collector) = training_data_collector {
                                collector.record_position(
                                    &pos,
                                    eval_log.as_ref().and_then(|e| e.score_cp),
                                    eval_log.as_ref().and_then(|e| e.score_mate),
                                    Some(mv),
                                );
                            }
                            // パス手は王手にならない
                            let gives_check = if mv.is_pass() {
                                false
                            } else {
                                pos.gives_check(mv)
                            };
                            pos.do_move(mv, gives_check);
                            tc.update_after_move(side, search.elapsed_ms);
                            move_usi = mv_str.clone();
                            raw_move_usi = None;
                        }
                        _ => {
                            outcome = if side == Color::Black {
                                GameOutcome::WhiteWin
                            } else {
                                GameOutcome::BlackWin
                            };
                            outcome_reason = "illegal_move";
                            terminal = true;
                            move_usi = "illegal".to_string();
                        }
                    },
                }
            } else {
                outcome = if side == Color::Black {
                    GameOutcome::WhiteWin
                } else {
                    GameOutcome::BlackWin
                };
                outcome_reason = "no_bestmove";
                terminal = true;
            }

            if cli.emit_eval_file {
                eval_list.push(eval_label(eval_log.as_ref()));
                move_list.push(move_usi.clone());
            }

            if cli.emit_metrics {
                metrics.update(side, eval_log.as_ref(), plies_played);
            }

            let move_log = MoveLog {
                kind: "move",
                game_id: game_idx + 1,
                ply: plies_played,
                side_to_move: side_label(side),
                sfen_before,
                move_usi,
                raw_move_usi,
                engine: engine_label.to_string(),
                elapsed_ms,
                think_limit_ms,
                timed_out,
                eval: eval_log,
            };
            serde_json::to_writer(&mut writer, &move_log)?;
            writer.write_all(b"\n")?;
            if cli.flush_each_move {
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
        if cli.emit_eval_file {
            if let Some(w) = eval_writer.as_mut() {
                let start_cmd = &start_commands[(game_idx as usize) % start_commands.len()];
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
        }
        if cli.emit_metrics {
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
        }
        // 学習データの書き出し（勝敗を設定してから書き出す）
        if let Some(ref mut collector) = training_data_collector {
            collector.finish_game(outcome)?;
        }
        writer.flush()?;

        // 勝敗カウント更新
        match outcome {
            GameOutcome::BlackWin => black_wins += 1,
            GameOutcome::WhiteWin => white_wins += 1,
            GameOutcome::Draw => draws += 1,
            GameOutcome::InProgress => {}
        }

        // 進捗表示
        println!(
            "game {}/{}: {} ({}) - black {} / white {} / draw {}",
            game_idx + 1,
            cli.games,
            outcome.label(),
            outcome_reason,
            black_wins,
            white_wins,
            draws
        );
    }

    // 最終サマリー
    println!();
    println!("=== Result Summary ===");
    println!(
        "Total: {} games | Black wins: {} | White wins: {} | Draws: {}",
        cli.games, black_wins, white_wins, draws
    );
    if cli.games > 0 {
        let black_rate = (black_wins as f64 / cli.games as f64) * 100.0;
        let white_rate = (white_wins as f64 / cli.games as f64) * 100.0;
        let draw_rate = (draws as f64 / cli.games as f64) * 100.0;
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

    // サマリファイル出力
    let summary_path = default_summary_path(&output_path);
    {
        let black_rate = if cli.games > 0 {
            (black_wins as f64 / cli.games as f64) * 100.0
        } else {
            0.0
        };
        let white_rate = if cli.games > 0 {
            (white_wins as f64 / cli.games as f64) * 100.0
        } else {
            0.0
        };
        let draw_rate = if cli.games > 0 {
            (draws as f64 / cli.games as f64) * 100.0
        } else {
            0.0
        };

        let summary = SummaryLog {
            kind: "summary",
            timestamp: timestamp.to_rfc3339(),
            total_games: cli.games,
            black_wins,
            white_wins,
            draws,
            black_win_rate: black_rate,
            white_win_rate: white_rate,
            draw_rate,
            engine_black: EngineSummary {
                path: engine_paths.black.path.display().to_string(),
                name: engine_paths
                    .black
                    .path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("rshogi-usi")
                    .to_string(),
                usi_options: black_usi_opts.clone(),
                threads: threads_black,
            },
            engine_white: EngineSummary {
                path: engine_paths.white.path.display().to_string(),
                name: engine_paths
                    .white
                    .path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("rshogi-usi")
                    .to_string(),
                usi_options: white_usi_opts.clone(),
                threads: threads_white,
            },
            time_control: TimeControlSummary {
                btime: cli.btime,
                wtime: cli.wtime,
                binc: cli.binc,
                winc: cli.winc,
                byoyomi: cli.byoyomi,
            },
        };

        let mut summary_writer = BufWriter::new(
            File::create(&summary_path)
                .with_context(|| format!("failed to create {}", summary_path.display()))?,
        );
        serde_json::to_writer(&mut summary_writer, &summary)?;
        summary_writer.write_all(b"\n")?;
        summary_writer.flush()?;
    }

    if let Some(logger) = info_logger.as_mut() {
        logger.flush()?;
    }
    if let Some(w) = eval_writer.as_mut() {
        w.flush()?;
    }
    if let Some(w) = metrics_writer.as_mut() {
        w.flush()?;
    }
    // 学習データのflushとサマリー出力
    if let Some(ref mut collector) = training_data_collector {
        collector.flush()?;
        let (total, skipped_initial, skipped_in_check, skipped_in_progress) = collector.stats();
        println!();
        println!("--- Training Data ---");
        println!("Total positions written: {total}");
        println!("Skipped (initial ply 1-{}): {skipped_initial}", cli.skip_initial_ply);
        if cli.skip_in_check {
            println!("Skipped (in check): {skipped_in_check}");
        }
        if skipped_in_progress > 0 {
            println!("Skipped (in progress games): {skipped_in_progress}");
        }
        println!(
            "Output: {}",
            training_data_path.as_ref().map_or("-".to_string(), |p| p.display().to_string())
        );
        println!("---------------------");
    }
    writer.flush()?;
    println!("selfplay log written to {}", output_path.display());
    println!("summary written to {}", summary_path.display());
    if cli.log_info {
        println!("info log written to {}", info_path.display());
    }
    let kif_path = default_kif_path(&output_path);
    match convert_jsonl_to_kif(&output_path, &kif_path) {
        Ok(paths) if paths.is_empty() => eprintln!("failed to create KIF: no games found"),
        Ok(paths) if paths.len() == 1 => println!("kif written to {}", paths[0].display()),
        Ok(paths) => {
            println!("kif written (per game):");
            for p in paths {
                println!("  {}", p.display());
            }
        }
        Err(err) => eprintln!("failed to create KIF: {}", err),
    }
    Ok(())
}

fn resolve_output_path(out: Option<&Path>, timestamp: &chrono::DateTime<Local>) -> PathBuf {
    if let Some(path) = out {
        return path.to_path_buf();
    }
    let dir = PathBuf::from("runs/selfplay");
    let name = format!("{}-selfplay.jsonl", timestamp.format("%Y%m%d-%H%M%S"));
    dir.join(name)
}

fn default_kif_path(jsonl: &Path) -> PathBuf {
    let parent = jsonl.parent().unwrap_or_else(|| Path::new("."));
    let stem = jsonl.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
    parent.join(format!("{stem}.kif"))
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

fn default_summary_path(jsonl: &Path) -> PathBuf {
    let parent = jsonl.parent().unwrap_or_else(|| Path::new("."));
    let stem = jsonl.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
    parent.join(format!("{stem}.summary.jsonl"))
}

fn default_training_data_path(jsonl: &Path) -> PathBuf {
    let parent = jsonl.parent().unwrap_or_else(|| Path::new("."));
    let stem = jsonl.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
    parent.join(format!("{stem}.pack"))
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
    if let Ok(exec) = std::env::current_exe() {
        if let Some(dir) = exec.parent() {
            if let Some(found) = find_engine_in_dir(dir) {
                return found;
            }
        }
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

// ---------------------------------------------------------------------------
// KIF 変換
// ---------------------------------------------------------------------------

#[derive(Default)]
struct GameLog {
    moves: Vec<MoveEntry>,
    result: Option<ResultEntry>,
}

#[derive(Deserialize, Clone)]
struct MoveEntry {
    game_id: u32,
    ply: u32,
    sfen_before: String,
    move_usi: String,
    #[serde(default)]
    elapsed_ms: Option<u64>,
    #[serde(default)]
    eval: Option<EvalLog>,
}

#[derive(Deserialize)]
struct ResultEntry {
    game_id: u32,
    outcome: String,
    reason: String,
    plies: u32,
}

fn convert_jsonl_to_kif(input: &Path, output: &Path) -> Result<Vec<PathBuf>> {
    let file =
        File::open(input).with_context(|| format!("failed to open input {}", input.display()))?;
    let reader = BufReader::new(file);

    let mut meta: Option<MetaLog> = None;
    let mut games: BTreeMap<u32, GameLog> = BTreeMap::new();

    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(trimmed)
            .with_context(|| format!("failed to parse JSON line: {}", trimmed))?;
        match value.get("type").and_then(|v| v.as_str()) {
            Some("meta") => {
                meta = Some(serde_json::from_value(value)?);
            }
            Some("move") => {
                let entry: MoveEntry = serde_json::from_value(value)?;
                games.entry(entry.game_id).or_default().moves.push(entry);
            }
            Some("result") => {
                let entry: ResultEntry = serde_json::from_value(value)?;
                let gid = entry.game_id;
                games.entry(gid).or_default().result = Some(entry);
            }
            _ => {}
        }
    }

    if games.is_empty() {
        bail!("no games found in {}", input.display());
    }

    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    let stem = output.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
    let ext = output.extension().and_then(|s| s.to_str()).unwrap_or("kif");

    let multi = games.len() > 1;
    let mut written = Vec::new();
    for (game_id, game) in games {
        let path = if multi {
            parent.join(format!("{stem}_g{game_id:02}.{ext}"))
        } else {
            output.to_path_buf()
        };
        let mut writer = BufWriter::new(
            File::create(&path).with_context(|| format!("failed to create {}", path.display()))?,
        );
        export_game_to_kif(&mut writer, meta.as_ref(), game_id, &game)?;
        writer.flush()?;
        written.push(path);
    }
    Ok(written)
}

fn export_game_to_kif<W: Write>(
    writer: &mut W,
    meta: Option<&MetaLog>,
    game_id: u32,
    game: &GameLog,
) -> Result<()> {
    let (mut pos, start_sfen) = start_position_for_game(meta, game_id, &game.moves)
        .ok_or_else(|| anyhow!("could not determine start position for game {}", game_id))?;

    let timestamp = meta.map(|m| m.timestamp.clone()).unwrap_or_else(|| "-".to_string());
    let (black_name, white_name) = engine_names_for(meta);
    let (btime, wtime) = meta.map(|m| (m.settings.btime, m.settings.wtime)).unwrap_or((0, 0));
    writeln!(writer, "開始日時：{}", timestamp)?;
    writeln!(writer, "手合割：平手")?;
    writeln!(writer, "先手：{}", black_name)?;
    writeln!(writer, "後手：{}", white_name)?;
    writeln!(writer, "持ち時間：先手{}ms / 後手{}ms", btime, wtime)?;
    writeln!(writer, "開始局面：{}", start_sfen)?;
    writeln!(writer, "手数----指手---------消費時間--")?;

    let mut moves = game.moves.clone();
    moves.sort_by_key(|m| m.ply);
    let mut total_black = 0u64;
    let mut total_white = 0u64;

    for entry in moves {
        if entry.move_usi == "resign" || entry.move_usi == "win" || entry.move_usi == "timeout" {
            break;
        }
        let side = pos.side_to_move();
        let mv = Move::from_usi(&entry.move_usi)
            .ok_or_else(|| anyhow!("invalid move in log: {}", entry.move_usi))?;
        if !is_legal_with_pass(&pos, mv) {
            bail!("illegal move '{}' in log for game {}", entry.move_usi, game_id);
        }
        let elapsed_ms = entry.elapsed_ms.unwrap_or(0);
        let total_time = if side == Color::Black {
            total_black + elapsed_ms
        } else {
            total_white + elapsed_ms
        };
        let line = format_move_kif(entry.ply, &pos, mv, elapsed_ms, total_time);
        writeln!(writer, "{line}")?;
        let gives_check = if mv.is_pass() {
            false
        } else {
            pos.gives_check(mv)
        };
        pos.do_move(mv, gives_check);
        if side == Color::Black {
            total_black = total_time;
        } else {
            total_white = total_time;
        }
        write_eval_comments(writer, entry.eval.as_ref())?;
    }

    let final_plies = game
        .result
        .as_ref()
        .map(|r| r.plies)
        .or_else(|| game.moves.last().map(|m| m.ply))
        .unwrap_or(0);
    if let Some(res) = game.result.as_ref() {
        if res.reason != "max_moves" {
            writeln!(writer, "**終了理由={}", res.reason)?;
        }
    }
    let summary = match game.result.as_ref().map(|r| r.outcome.as_str()).unwrap_or("draw") {
        "black_win" => format!("まで{}手で先手の勝ち", final_plies),
        "white_win" => format!("まで{}手で後手の勝ち", final_plies),
        _ => format!("まで{}手で引き分け", final_plies),
    };
    writeln!(writer, "\n{}", summary)?;
    Ok(())
}

fn start_position_for_game(
    meta: Option<&MetaLog>,
    game_id: u32,
    moves: &[MoveEntry],
) -> Option<(Position, String)> {
    // パス手がログに含まれているか確認
    let has_pass = moves.iter().any(|m| m.move_usi == "pass");

    // metaから初期パス権数を取得（未記録の場合は後方互換のため15を使用）
    let (pass_black, pass_white) = if has_pass {
        let black = meta.and_then(|m| m.settings.initial_pass_count_black).unwrap_or(15);
        let white = meta.and_then(|m| m.settings.initial_pass_count_white).unwrap_or(15);
        (black, white)
    } else {
        (0, 0)
    };

    // random_startpos の場合は moves[0].sfen_before を優先
    let use_moves_first = meta.map(|m| m.settings.random_startpos).unwrap_or(false);

    if !use_moves_first {
        if let Some(meta) = meta {
            if !meta.start_positions.is_empty() {
                let idx = ((game_id - 1) as usize) % meta.start_positions.len();
                if let Ok((mut pos, _)) = start_position_from_command(&meta.start_positions[idx]) {
                    if has_pass {
                        pos.enable_pass_rights(pass_black, pass_white);
                    }
                    let sfen = pos.to_sfen();
                    return Some((pos, sfen));
                }
            }
        }
    }
    moves.first().and_then(|m| {
        let mut pos = Position::new();
        pos.set_sfen(&m.sfen_before).ok()?;
        if has_pass {
            pos.enable_pass_rights(pass_black, pass_white);
        }
        let sfen = pos.to_sfen();
        Some((pos, sfen))
    })
}

fn start_position_from_command(cmd: &str) -> Result<(Position, String)> {
    let parsed = parse_position_line(cmd)?;
    let has_pass = parsed.moves.iter().any(|m| m == "pass");
    let (pass_black, pass_white) = if has_pass {
        (Some(15), Some(15))
    } else {
        (None, None)
    };
    let pos = build_position(&parsed, pass_black, pass_white)?;
    let sfen = pos.to_sfen();
    Ok((pos, sfen))
}

fn engine_names_for(meta: Option<&MetaLog>) -> (String, String) {
    let default = ("black".to_string(), "white".to_string());
    let Some(meta) = meta else { return default };
    let black_name = Path::new(&meta.engine_cmd.path_black)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&meta.engine_cmd.path_black);
    let white_name = Path::new(&meta.engine_cmd.path_white)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&meta.engine_cmd.path_white);

    let black_opts = &meta.engine_cmd.usi_options_black;
    let white_opts = &meta.engine_cmd.usi_options_white;

    let black_display = if black_opts.is_empty() {
        black_name.to_string()
    } else {
        format!("{} [{}]", black_name, black_opts.join(", "))
    };
    let white_display = if white_opts.is_empty() {
        white_name.to_string()
    } else {
        format!("{} [{}]", white_name, white_opts.join(", "))
    };

    (black_display, white_display)
}

fn format_move_kif(ply: u32, pos: &Position, mv: Move, elapsed_ms: u64, total_ms: u64) -> String {
    let prefix = if pos.side_to_move() == Color::Black {
        "▲"
    } else {
        "△"
    };

    // パス手は特別に処理
    if mv.is_pass() {
        let per_move = format_mm_ss(elapsed_ms);
        let total = format_hh_mm_ss(total_ms);
        return format!("{:>4} {}パス   ({:>5}/{})", ply, prefix, per_move, total);
    }

    let dest = square_label_kanji(mv.to());
    let (label, from_suffix) = if mv.is_drop() {
        (format!("{}打", piece_label(mv.drop_piece_type(), false)), String::new())
    } else {
        let from = mv.from();
        let piece = pos.piece_on(from);
        let promoted = piece.piece_type().is_promoted() || mv.is_promote();
        let suffix = format!("({}{})", square_file_digit(from), square_rank_digit(from));
        (piece_label(piece.piece_type(), promoted).to_string(), suffix)
    };
    let per_move = format_mm_ss(elapsed_ms);
    let total = format_hh_mm_ss(total_ms);
    format!(
        "{:>4} {}{}{}{}   ({:>5}/{})",
        ply, prefix, dest, label, from_suffix, per_move, total
    )
}

fn square_label_kanji(sq: Square) -> String {
    format!("{}{}", file_kanji(sq), rank_kanji(sq))
}

fn file_kanji(sq: Square) -> &'static str {
    const FILES: [&str; 10] = ["", "１", "２", "３", "４", "５", "６", "７", "８", "９"];
    let idx = sq.file().to_usi_char().to_digit(10).unwrap_or(1) as usize;
    FILES[idx]
}

fn rank_kanji(sq: Square) -> &'static str {
    const RANKS: [&str; 9] = ["一", "二", "三", "四", "五", "六", "七", "八", "九"];
    let rank = sq.rank().to_usi_char() as u8;
    let idx = (rank - b'a') as usize;
    RANKS.get(idx).copied().unwrap_or("一")
}

fn square_file_digit(sq: Square) -> char {
    sq.file().to_usi_char()
}

fn square_rank_digit(sq: Square) -> char {
    let rank = sq.rank().to_usi_char();
    let idx = (rank as u8 - b'a') + 1;
    char::from_digit(idx as u32, 10).unwrap_or('1')
}

fn piece_label(piece_type: PieceType, promoted: bool) -> &'static str {
    match (piece_type, promoted) {
        (PieceType::Pawn, false) => "歩",
        (PieceType::Pawn, true) => "と",
        (PieceType::Lance, false) => "香",
        (PieceType::Lance, true) => "成香",
        (PieceType::Knight, false) => "桂",
        (PieceType::Knight, true) => "成桂",
        (PieceType::Silver, false) => "銀",
        (PieceType::Silver, true) => "成銀",
        (PieceType::Gold, _) => "金",
        (PieceType::Bishop, false) => "角",
        (PieceType::Bishop, true) => "馬",
        (PieceType::Rook, false) => "飛",
        (PieceType::Rook, true) => "龍",
        (PieceType::King, _) => "玉",
        (PieceType::ProPawn, _) => "と",
        (PieceType::ProLance, _) => "成香",
        (PieceType::ProKnight, _) => "成桂",
        (PieceType::ProSilver, _) => "成銀",
        (PieceType::Horse, _) => "馬",
        (PieceType::Dragon, _) => "龍",
    }
}

fn write_eval_comments<W: Write>(writer: &mut W, eval: Option<&EvalLog>) -> Result<()> {
    let Some(eval) = eval else {
        return Ok(());
    };
    writeln!(writer, "*info")?;
    if let Some(mate) = eval.score_mate {
        writeln!(writer, "**詰み={}", mate)?;
    } else if let Some(cp) = eval.score_cp {
        writeln!(writer, "**評価値={:+}", cp)?;
    }
    if let Some(depth) = eval.depth {
        writeln!(writer, "**深さ={}", depth)?;
    }
    if let Some(seldepth) = eval.seldepth {
        writeln!(writer, "**選択深さ={}", seldepth)?;
    }
    if let Some(nodes) = eval.nodes {
        writeln!(writer, "**ノード数={}", nodes)?;
    }
    if let Some(time_ms) = eval.time_ms {
        writeln!(writer, "**探索時間={}ms", time_ms)?;
    }
    if let Some(nps) = eval.nps {
        writeln!(writer, "**NPS={}", nps)?;
    }
    if let Some(pv) = eval.pv.as_ref() {
        if !pv.is_empty() {
            writeln!(writer, "**読み筋={}", pv.join(" "))?;
        }
    }
    Ok(())
}

fn format_mm_ss(ms: u64) -> String {
    let secs = ms / 1000;
    let m = secs / 60;
    let s = secs % 60;
    format!("{:>2}:{:02}", m, s)
}

fn format_hh_mm_ss(ms: u64) -> String {
    let secs = ms / 1000;
    let h = secs / 3600;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    format!("{:02}:{:02}:{:02}", h, m, s)
}

#[cfg(test)]
mod tests {
    use super::*;

    use clap::Parser;
    use std::path::PathBuf;

    #[test]
    fn resolve_engine_paths_uses_per_side_when_provided() {
        let cli = Cli::parse_from([
            "engine_selfplay",
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
        let cli = Cli::parse_from([
            "engine_selfplay",
            "--engine-path",
            "/shared/path/engine-usi",
        ]);
        let paths = resolve_engine_paths(&cli);
        assert_eq!(paths.black.path, PathBuf::from("/shared/path/engine-usi"));
        assert_eq!(paths.white.path, PathBuf::from("/shared/path/engine-usi"));
        assert_eq!(paths.black.source, "cli");
        assert_eq!(paths.white.source, "cli");
    }
}
