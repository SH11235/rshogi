/// round-robin 並列トーナメント。
///
/// crossbeam-channel ワーカーモデルで複数エンジン間の総当たり対局を並列実行する。
/// 出力は analyze_selfplay 互換の JSONL 形式。
///
/// # 使用例
///
/// 同一バイナリで異なる評価関数を比較（--engine-label 必須）:
/// ```shell
/// cargo run -p tools --release --bin tournament -- \
///   --engine target/release/rshogi-usi --engine-label nnue-v60 \
///   --engine target/release/rshogi-usi --engine-label material \
///   --games 50 --byoyomi 500 --threads 2 \
///   --engine-usi-option "0:EvalFile=eval/halfka_hm_512x2-8-64_crelu/v60.bin" \
///   --out-dir "runs/selfplay/$(date +%Y%m%d_%H%M%S)-nnue-v60-vs-material9"
/// ```
///
/// rshogi vs YaneuraOu（suisho5, HalfKP 256x2-32-32, FV_SCALE=24）:
/// ```shell
/// cargo build --release -p rshogi-usi && \
/// cargo run -p tools --release --bin tournament -- \
///   --concurrency 8 \
///   --engine target/release/rshogi-usi --engine-label rshogi \
///   --engine /mnt/nvme1/development/YaneuraOu/source/YaneuraOu-halfkp_256x2-32-32 --engine-label yaneuraou \
///   --games 50 --byoyomi 500 --threads 2 \
///   --usi-option "FV_SCALE=24" \
///   --engine-usi-option "0:EvalFile=eval/halfkp_256x2-32-32_crelu/suisho5.bin" \
///   --engine-usi-option "1:EvalDir=/mnt/nvme1/development/rshogi/eval/halfkp_256x2-32-32_crelu" \
///   --engine-usi-option "1:NetworkDelay2=0" \
///   --engine-usi-option "1:RoundUpToFullSecond=false" \
///   --out-dir "runs/selfplay/$(date +%Y%m%d_%H%M%S)-rshogi-vs-yaneuraou-suisho5"
/// ```
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use chrono::Local;
use clap::Parser as _;
use crossbeam_channel as chan;
use rand::prelude::IndexedRandom;
use serde::Serialize;

use tools::selfplay::game::{GameConfig, MoveEvent, run_game};
use tools::selfplay::time_control::TimeControl;
use tools::selfplay::types::{EvalLog, side_label};
use tools::selfplay::{
    EngineConfig, EngineProcess, GameOutcome, ParsedPosition, load_start_positions,
};
use tools::sprt::{Decision, GameSide, Penta, SprtParameters, judge};

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(clap::Parser, Debug)]
#[command(about = "round-robin parallel tournament for rshogi-usi engines")]
struct Cli {
    /// Engine binary paths (2 or more required)
    #[arg(long = "engine", required = true, num_args = 1)]
    engines: Vec<PathBuf>,

    /// Engine labels (must match --engine count if specified).
    /// Required when the same binary path appears more than once.
    #[arg(long = "engine-label", num_args = 1)]
    engine_labels: Vec<String>,

    /// Number of games per direction for each pair
    #[arg(long, default_value_t = 100)]
    games: u32,

    /// Number of concurrent workers
    #[arg(long, default_value_t = 1)]
    concurrency: usize,

    /// Byoyomi time per move in milliseconds (mutually exclusive with --btime/--binc)
    #[arg(long, default_value_t = 0)]
    byoyomi: u64,

    /// Initial time per side in milliseconds (Fischer clock, mutually exclusive with --byoyomi)
    #[arg(long, default_value_t = 0)]
    btime: u64,

    /// Increment per move in milliseconds (Fischer clock, mutually exclusive with --byoyomi)
    #[arg(long, default_value_t = 0)]
    binc: u64,

    /// Threads per engine
    #[arg(long, default_value_t = 1)]
    threads: usize,

    /// Hash/USI_Hash size (MiB) per engine
    #[arg(long, default_value_t = 256)]
    hash_mb: u32,

    /// Additional USI options (format: "Name=Value", can be repeated)
    #[arg(long = "usi-option", num_args = 1..)]
    usi_options: Option<Vec<String>>,

    /// Per-engine USI options (format: "INDEX:Name=Value", can be repeated).
    /// Overrides --usi-option for that engine (not merged).
    #[arg(long = "engine-usi-option", num_args = 1..)]
    engine_usi_options: Option<Vec<String>>,

    /// SPSA .params ファイルからUSIオプションを読み込む (format: "INDEX:path/to/file.params")。
    /// ファイル内の各パラメータが Name=Value として該当エンジンに設定される。
    /// --engine-usi-option と併用可。
    #[arg(long = "engine-params-file", num_args = 1..)]
    engine_params_files: Option<Vec<String>>,

    /// Maximum plies per game
    #[arg(long, default_value_t = 512)]
    max_moves: u32,

    /// Output directory (required)
    #[arg(long)]
    out_dir: PathBuf,

    /// Start position file (USI position lines, one per line)
    #[arg(long)]
    startpos_file: Option<PathBuf>,

    /// Report progress every N games
    #[arg(long, default_value_t = 10)]
    report_interval: u32,

    /// Safety margin for timeout detection (ms)
    #[arg(long, default_value_t = 1000)]
    timeout_margin_ms: u64,

    /// Depth limit per move. If specified, sends `go depth N` instead of `go byoyomi`.
    #[arg(long)]
    depth: Option<u32>,

    /// Nodes limit per move. If specified, sends `go nodes N`.
    #[arg(long)]
    nodes: Option<u64>,

    /// Base engine label for "base-vs-N" mode.
    /// Only pairs that include this engine are scheduled; non-base pairings are skipped.
    /// The label must match one of the `--engine-label` (or path-derived) values.
    #[arg(long)]
    base_label: Option<String>,

    /// SPRT 逐次確率比検定を有効化（--sprt-test-label, --sprt-base-label 必須）。
    /// 境界到達で新規チケット供給を停止し、進行中ゲームは完了待ち。
    #[arg(long, default_value_t = false)]
    sprt: bool,

    /// H1 側（challenger / test）のエンジンラベル。Penta はこの視点で集計される。
    #[arg(long)]
    sprt_test_label: Option<String>,

    /// H0 側（base）のエンジンラベル。`--base-label` と別指定したい場合に使う。
    /// 未指定時は `--base-label` を流用する。
    #[arg(long)]
    sprt_base_label: Option<String>,

    /// H0 仮説の正規化 Elo（default: 0.0）
    #[arg(long, default_value_t = 0.0)]
    sprt_nelo0: f64,

    /// H1 仮説の正規化 Elo（default: 5.0）
    #[arg(long, default_value_t = 5.0)]
    sprt_nelo1: f64,

    /// 第一種過誤率 α（default: 0.05）
    #[arg(long, default_value_t = 0.05)]
    sprt_alpha: f64,

    /// 第二種過誤率 β（default: 0.05）
    #[arg(long, default_value_t = 0.05)]
    sprt_beta: f64,

    /// SPRT レポートをペア何単位ごとに出すか（default: 10）
    #[arg(long, default_value_t = 10)]
    sprt_report_interval: u32,
}

// ---------------------------------------------------------------------------
// チケットと結果
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct MatchTicket {
    /// グローバル一意 ID
    id: u64,
    /// engines[black_idx] が先手
    black_idx: usize,
    /// engines[white_idx] が後手
    white_idx: usize,
    /// 開始局面インデックス
    startpos_idx: usize,
    /// 同じ startpos の先後入替ペアの通し番号（0, 1, 2, ... = ペアの index）
    pair_index: u32,
    /// ペア内のスロット: 0 = 1 局目, 1 = 2 局目（先後入替）
    pair_slot: u32,
}

struct MatchResult {
    ticket: MatchTicket,
    outcome: GameOutcome,
    reason: String,
    plies: u32,
    move_logs: Vec<MoveLogEntry>,
    /// エンジン起動失敗・通信エラー等で対局が成立しなかった場合 true。
    /// SPRT 集計からは除外される。
    error: bool,
}

#[derive(Clone, Serialize)]
struct MoveLogEntry {
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
struct ResultLogEntry<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    game_id: u32,
    outcome: &'a str,
    reason: &'a str,
    plies: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    winner: Option<String>,
    /// SPRT post-hoc 解析用の追加メタ（非破壊）。
    #[serde(skip_serializing_if = "Option::is_none")]
    ticket_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pair_index: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pair_slot: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    startpos_idx: Option<u32>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    error: bool,
}

#[derive(Serialize)]
struct MetaLogEntry {
    #[serde(rename = "type")]
    kind: String,
    timestamp: String,
    settings: MetaSettings,
    engine_cmd: EngineCommandMeta,
    start_positions: Vec<String>,
    output: String,
    /// SPRT 実行時のみ付与される。base / test ラベルと Wald パラメータを含み、
    /// analyze_selfplay 側でラベル自動推定に利用する。
    #[serde(skip_serializing_if = "Option::is_none")]
    sprt: Option<SprtMetaLog>,
}

#[derive(Serialize, Clone)]
struct SprtMetaLog {
    base_label: String,
    test_label: String,
    nelo0: f64,
    nelo1: f64,
    alpha: f64,
    beta: f64,
}

#[derive(Serialize)]
struct MetaSettings {
    games: u32,
    max_moves: u32,
    byoyomi: u64,
    #[serde(skip_serializing_if = "is_zero_u64")]
    btime: u64,
    #[serde(skip_serializing_if = "is_zero_u64")]
    binc: u64,
    timeout_margin_ms: u64,
    threads: usize,
    hash_mb: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    depth: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    nodes: Option<u64>,
}

fn is_zero_u64(v: &u64) -> bool {
    *v == 0
}

#[derive(Serialize)]
struct EngineCommandMeta {
    path_black: String,
    path_white: String,
    label_black: String,
    label_white: String,
    usi_options_black: Vec<String>,
    usi_options_white: Vec<String>,
}

#[derive(Serialize)]
struct TournamentMeta {
    timestamp: String,
    settings: MetaSettings,
    engines: Vec<EngineMetaEntry>,
    start_positions: Vec<String>,
    output_dir: String,
}

#[derive(Serialize)]
struct EngineMetaEntry {
    index: usize,
    label: String,
    path: String,
    usi_options: Vec<String>,
}

// ---------------------------------------------------------------------------
// SPRT 状態
// ---------------------------------------------------------------------------

/// SPRT 逐次判定のランタイム状態。
///
/// 視点は常に challenger (test) 側で固定される。pair 単位で集計するため、
/// 並列ワーカーから結果が順不同に到着しても `pair_index` 経由でバッファして
/// 2 ゲームが揃った時点で Penta に反映する。
struct SprtState {
    params: SprtParameters,
    base_idx: usize,
    test_idx: usize,
    /// pair_index → そのペアの 2 スロット分の結果（test 視点）
    buffer: HashMap<u32, [Option<GameSide>; 2]>,
    /// 集計済みの Pentanomial
    penta: Penta,
    report_interval: u32,
    /// 直前に `maybe_report` が出力した pair_count。
    /// 非 SPRT ペアの結果が流れてきたり、同一 pair_index が再観測されたりしても
    /// ペア数が変わらない限り再出力しない。
    last_reported_pairs: u64,
    /// 判定確定時点のスナップショット（後追いで変化しないように凍結）
    stopped_at: Option<SprtSnapshot>,
    test_label: String,
    base_label: String,
}

#[derive(Clone)]
struct SprtSnapshot {
    decision: Decision,
    pairs: u64,
    llr: f64,
    nelo: Option<(f64, f64)>,
    penta: Penta,
}

impl SprtState {
    fn new(
        params: SprtParameters,
        base_idx: usize,
        test_idx: usize,
        report_interval: u32,
        base_label: String,
        test_label: String,
    ) -> Self {
        SprtState {
            params,
            base_idx,
            test_idx,
            buffer: HashMap::new(),
            penta: Penta::ZERO,
            report_interval: report_interval.max(1),
            last_reported_pairs: 0,
            stopped_at: None,
            test_label,
            base_label,
        }
    }

    /// ゲーム結果を 1 つ取り込む。
    ///
    /// - base と test のペアでない、または error なら無視
    /// - 両スロットが揃ったら Penta に反映し、判定を更新
    ///
    /// 戻り値: 今回の取り込みで初めて terminal に到達したら `Some(Decision)`。
    fn observe(&mut self, result: &MatchResult) -> Option<Decision> {
        if result.error {
            return None;
        }
        let bi = result.ticket.black_idx;
        let wi = result.ticket.white_idx;
        let (a, b) = if bi < wi { (bi, wi) } else { (wi, bi) };
        let base = self.base_idx.min(self.test_idx);
        let test = self.base_idx.max(self.test_idx);
        if (a, b) != (base, test) {
            return None;
        }

        let test_side = match (result.outcome, self.test_idx == bi, self.test_idx == wi) {
            (GameOutcome::BlackWin, true, _) | (GameOutcome::WhiteWin, _, true) => GameSide::Win,
            (GameOutcome::BlackWin, _, true) | (GameOutcome::WhiteWin, true, _) => GameSide::Loss,
            (GameOutcome::Draw, _, _) => GameSide::Draw,
            (GameOutcome::InProgress, _, _) => return None,
            _ => return None,
        };

        let slot = result.ticket.pair_slot.min(1) as usize;
        let entry = self.buffer.entry(result.ticket.pair_index).or_insert([None, None]);
        entry[slot] = Some(test_side);

        if let ([Some(a), Some(b)], _) = (*entry, ()) {
            let delta = Penta::from_pair(a, b);
            self.penta += delta;
            self.buffer.remove(&result.ticket.pair_index);

            if self.stopped_at.is_none() {
                let decision = judge(&self.params, self.penta);
                if decision.is_terminal() {
                    self.stopped_at = Some(self.snapshot(decision));
                    return Some(decision);
                }
            }
        }

        None
    }

    fn pair_count(&self) -> u64 {
        self.penta.pair_count()
    }

    fn snapshot(&self, decision: Decision) -> SprtSnapshot {
        SprtSnapshot {
            decision,
            pairs: self.penta.pair_count(),
            llr: self.params.llr(self.penta),
            nelo: self.penta.normalized_elo(),
            penta: self.penta,
        }
    }

    /// レポート間隔ごとに現在の LLR / nelo / penta を表示する。
    ///
    /// 非 SPRT ペアの結果でも `handle_sprt_observation` から呼ばれるため、
    /// 「ペア数が変わっていない」間は何度呼ばれても再出力しないことで
    /// 進捗行の連打を避ける。
    fn maybe_report(&mut self, force: bool) {
        let pairs = self.pair_count();
        if pairs == 0 {
            return;
        }
        if pairs == self.last_reported_pairs {
            return;
        }
        if !force && !pairs.is_multiple_of(self.report_interval as u64) {
            return;
        }
        self.last_reported_pairs = pairs;
        let llr = self.params.llr(self.penta);
        let (lo, hi) = self.params.llr_bounds();
        let nelo_txt = match self.penta.normalized_elo() {
            Some((e, ci)) => format!("{:+.2} ± {:.2}", e, ci),
            None => "n/a".to_string(),
        };
        println!(
            "[SPRT pair={} | {} vs {}] LLR={:+.3} (bounds {:+.2}..{:+.2})  nelo={}  penta={}  state={}",
            pairs,
            self.test_label,
            self.base_label,
            llr,
            lo,
            hi,
            nelo_txt,
            self.penta,
            match (self.stopped_at.as_ref(), judge(&self.params, self.penta)) {
                (Some(snap), _) => snap.decision.as_str(),
                (None, d) => d.as_str(),
            }
        );
    }
}

fn print_sprt_final(state: &SprtState) {
    let (lo, hi) = state.params.llr_bounds();
    let current_llr = state.params.llr(state.penta);
    let current_decision = judge(&state.params, state.penta);
    println!();
    println!("=== SPRT Summary ({} vs {}) ===", state.test_label, state.base_label);
    println!(
        "bounds: LLR ∈ [{:+.3}, {:+.3}]  (alpha={}, beta={})",
        lo, hi, state.params.alpha, state.params.beta,
    );
    println!(
        "nelo hypotheses: H0={:+.1}  H1={:+.1}",
        state.params.nelo_bounds().0,
        state.params.nelo_bounds().1,
    );
    if let Some(snap) = state.stopped_at.as_ref() {
        println!(
            "stopped_at:  pairs={}, LLR={:+.3}, decision={}",
            snap.pairs,
            snap.llr,
            snap.decision.as_str(),
        );
        if let Some((e, ci)) = snap.nelo {
            println!("             nelo={:+.2} ± {:.2}  penta={}", e, ci, snap.penta);
        } else {
            println!("             nelo=n/a  penta={}", snap.penta);
        }
    }
    println!(
        "final:       pairs={}, LLR={:+.3}, decision={}",
        state.penta.pair_count(),
        current_llr,
        current_decision.as_str(),
    );
    if let Some((e, ci)) = state.penta.normalized_elo() {
        println!("             nelo={:+.2} ± {:.2}  penta={}", e, ci, state.penta);
    } else {
        println!("             nelo=n/a  penta={}", state.penta);
    }
    println!("================================");
}

// ---------------------------------------------------------------------------
// ペア別ライター
// ---------------------------------------------------------------------------

/// (black_idx, white_idx) → ファイルへの BufWriter
struct PairWriter {
    writer: BufWriter<File>,
}

impl PairWriter {
    fn new(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let file =
            File::create(path).with_context(|| format!("failed to create {}", path.display()))?;
        Ok(Self {
            writer: BufWriter::new(file),
        })
    }

    fn write_json(&mut self, value: &impl Serialize) -> Result<()> {
        serde_json::to_writer(&mut self.writer, value)?;
        self.writer.write_all(b"\n")?;
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        self.writer.flush()?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ワーカースレッド
// ---------------------------------------------------------------------------

struct WorkerConfig {
    engine_paths: Vec<PathBuf>,
    engine_labels: Vec<String>,
    engine_usi_options: Vec<Vec<String>>,
    threads: usize,
    hash_mb: u32,
    max_moves: u32,
    timeout_margin_ms: u64,
    byoyomi: u64,
    btime: u64,
    binc: u64,
    go_depth: Option<u32>,
    go_nodes: Option<u64>,
    start_positions: Vec<ParsedPosition>,
}

fn worker_main(
    cfg: WorkerConfig,
    rx: chan::Receiver<Option<MatchTicket>>,
    tx: chan::Sender<MatchResult>,
    shutdown: Arc<AtomicBool>,
) {
    let WorkerConfig {
        engine_paths,
        engine_labels,
        engine_usi_options,
        threads,
        hash_mb,
        max_moves,
        timeout_margin_ms,
        byoyomi,
        btime,
        binc,
        go_depth,
        go_nodes,
        start_positions,
    } = cfg;
    // ワーカー内で全エンジンを起動
    let mut engines: Vec<EngineProcess> = Vec::new();
    for (i, path) in engine_paths.iter().enumerate() {
        let label = engine_labels[i].clone();
        let cfg = EngineConfig {
            path: path.clone(),
            args: Vec::new(),
            threads,
            hash_mb,
            network_delay: None,
            network_delay2: None,
            minimum_thinking_time: None,
            slowmover: None,
            ponder: false,
            usi_options: engine_usi_options[i].clone(),
        };
        match EngineProcess::spawn(&cfg, label) {
            Ok(ep) => {
                engines.push(ep);
            }
            Err(e) => {
                eprintln!("worker: failed to spawn engine {i} ({}): {e}", path.display());
                shutdown.store(true, Ordering::Relaxed);
                return;
            }
        }
    }

    while let Ok(Some(ticket)) = rx.recv() {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        // 2つの異なるインデックスへの同時可変借用のため split_at_mut を使用
        let (black, white) = if ticket.black_idx < ticket.white_idx {
            let (left, right) = engines.split_at_mut(ticket.white_idx);
            (&mut left[ticket.black_idx], &mut right[0])
        } else {
            let (left, right) = engines.split_at_mut(ticket.black_idx);
            (&mut right[0], &mut left[ticket.white_idx])
        };

        let _ = black.new_game();
        let _ = white.new_game();

        let start_pos = &start_positions[ticket.startpos_idx];
        let tc = TimeControl::new(btime, btime, binc, binc, byoyomi);
        let config = GameConfig {
            max_moves,
            timeout_margin_ms,
            pass_rights: None,
            go_depth,
            go_nodes,
        };
        let game_id = (ticket.id as u32) + 1;

        let mut move_logs: Vec<MoveLogEntry> = Vec::new();
        let mut on_move = |event: &MoveEvent| {
            move_logs.push(MoveLogEntry {
                kind: "move",
                game_id,
                ply: event.ply,
                side_to_move: side_label(event.side),
                sfen_before: event.sfen_before.clone(),
                move_usi: event.move_usi.clone(),
                raw_move_usi: event.raw_move_usi.clone(),
                engine: event.engine_label.clone(),
                elapsed_ms: event.elapsed_ms,
                think_limit_ms: event.think_limit_ms,
                timed_out: event.timed_out,
                eval: event.eval.clone(),
            });
        };

        match run_game(black, white, start_pos, tc, &config, game_id, &mut on_move, None) {
            Ok(result) => {
                let _ = tx.send(MatchResult {
                    ticket,
                    outcome: result.outcome,
                    reason: result.reason,
                    plies: result.plies,
                    move_logs,
                    error: false,
                });
            }
            Err(e) => {
                eprintln!("worker: game error: {e}");
                let _ = tx.send(MatchResult {
                    ticket,
                    outcome: GameOutcome::Draw,
                    reason: format!("error: {e}"),
                    plies: 0,
                    move_logs,
                    error: true,
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// メイン
// ---------------------------------------------------------------------------

fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.engines.len() < 2 {
        bail!("at least 2 engines are required");
    }
    if cli.concurrency == 0 {
        bail!("--concurrency must be at least 1");
    }

    // バイナリ存在確認
    for path in &cli.engines {
        if !path.is_file() {
            bail!("engine binary not found: {}", path.display());
        }
    }

    let n = cli.engines.len();

    // エンジンラベルの解決
    let engine_labels: Vec<String> = if cli.engine_labels.is_empty() {
        // ラベル未指定: 同一パスが重複していないか確認
        let mut seen: HashMap<&Path, usize> = HashMap::new();
        for (i, p) in cli.engines.iter().enumerate() {
            if let Some(prev) = seen.insert(p.as_path(), i) {
                bail!(
                    "同一バイナリが複数指定されています (engines[{prev}] と engines[{i}]: {})。\n\
                     --engine-label で各エンジンにラベルを付けてください。",
                    p.display()
                );
            }
        }
        cli.engines.iter().map(|p| engine_label_from_path(p)).collect()
    } else {
        if cli.engine_labels.len() != n {
            bail!(
                "--engine-label の数 ({}) が --engine の数 ({n}) と一致しません",
                cli.engine_labels.len()
            );
        }
        cli.engine_labels.clone()
    };

    // ラベルの重複チェック
    {
        let mut seen: HashMap<&str, usize> = HashMap::new();
        for (i, label) in engine_labels.iter().enumerate() {
            if let Some(prev) = seen.insert(label.as_str(), i) {
                bail!(
                    "ラベル '{}' が重複しています (engines[{prev}] と engines[{i}])。\n\
                     各エンジンには一意のラベルを指定してください。",
                    label
                );
            }
        }
    }

    // 時間管理のバリデーション
    let use_byoyomi = cli.byoyomi > 0;
    let use_fischer = cli.btime > 0 || cli.binc > 0;
    let use_fixed = cli.depth.is_some() || cli.nodes.is_some();
    if use_byoyomi && use_fischer {
        bail!("--byoyomi と --btime/--binc は同時に指定できません");
    }
    if !use_byoyomi && !use_fischer && !use_fixed {
        bail!("時間管理を指定してください: --byoyomi, --btime/--binc, --depth, --nodes のいずれか");
    }
    if use_fischer && cli.btime == 0 && cli.binc == 0 {
        bail!("フィッシャー時計: --btime または --binc の少なくとも一方を正の値で指定してください");
    }

    // 開始局面のロード
    let (start_defs, start_commands) =
        load_start_positions(cli.startpos_file.as_deref(), None, None, None)?;

    // 出力ディレクトリの作成
    fs::create_dir_all(&cli.out_dir)
        .with_context(|| format!("failed to create {}", cli.out_dir.display()))?;

    let common_usi_options = cli.usi_options.clone().unwrap_or_default();

    // per-engine オプションを解析: HashMap<usize, Vec<String>>
    let mut per_engine_usi: HashMap<usize, Vec<String>> = HashMap::new();

    // --engine-params-file: SPSA .params ファイルから Name=Value を読み込む
    if let Some(pfiles) = &cli.engine_params_files {
        for entry in pfiles {
            let (idx_str, path_str) = entry
                .split_once(':')
                .with_context(|| format!("invalid --engine-params-file format: {entry}"))?;
            let idx: usize =
                idx_str.parse().with_context(|| format!("invalid engine index: {idx_str}"))?;
            if idx >= n {
                bail!("--engine-params-file index {idx} out of range (0..{n})");
            }
            let path = std::path::Path::new(path_str);
            let content = std::fs::read_to_string(path)
                .with_context(|| format!("failed to read params file: {}", path.display()))?;
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                // .params format: name,type,value,min,max,c_end,r_end [// comment] [[[NOT USED]]]
                let val_part = trimmed.split("//").next().unwrap_or(trimmed);
                let val_part = val_part.replace("[[NOT USED]]", "");
                let cols: Vec<&str> = val_part.split(',').map(str::trim).collect();
                if cols.len() >= 3 {
                    let name = cols[0];
                    let type_name = cols[1];
                    let value = cols[2];
                    // 整数パラメータは小数点を除去
                    let formatted = if type_name.eq_ignore_ascii_case("int") {
                        if let Ok(v) = value.parse::<f64>() {
                            format!("{name}={}", v.round() as i64)
                        } else {
                            format!("{name}={value}")
                        }
                    } else {
                        format!("{name}={value}")
                    };
                    per_engine_usi.entry(idx).or_default().push(formatted);
                }
            }
        }
    }

    if let Some(opts) = &cli.engine_usi_options {
        for opt in opts {
            let (idx_str, kv) = opt
                .split_once(':')
                .with_context(|| format!("invalid --engine-usi-option format: {opt}"))?;
            let idx: usize =
                idx_str.parse().with_context(|| format!("invalid engine index: {idx_str}"))?;
            if idx >= n {
                bail!("--engine-usi-option index {idx} out of range (0..{n})");
            }
            per_engine_usi.entry(idx).or_default().push(kv.to_string());
        }
    }

    // エンジンごとの最終オプションリストを構築
    //
    // 公平な対局条件のため、NetworkDelay=0 と MinimumThinkingTime を
    // デフォルトで注入する。ユーザーが明示的に指定した場合はそちらを優先。
    // - NetworkDelay: 0 以外だと秒境界切り上げで思考時間が短縮され、
    //   エンジン間で実質的な思考時間が不平等になる。
    // - MinimumThinkingTime: byoyomi 時は byoyomi と一致させることで秒読み全体を使い切れる。
    //   フィッシャー時は 0（エンジンの時間管理に委ねる）。
    let min_think = if cli.byoyomi > 0 {
        cli.byoyomi.to_string()
    } else {
        "0".to_string()
    };
    let time_defaults = [
        ("NetworkDelay", "0"),
        ("NetworkDelay2", "0"),
        ("MinimumThinkingTime", min_think.as_str()),
    ];
    let engine_usi_options: Vec<Vec<String>> = (0..n)
        .map(|i| {
            let mut opts = per_engine_usi.remove(&i).unwrap_or_else(|| common_usi_options.clone());
            for (name, default_value) in &time_defaults {
                let already_set =
                    opts.iter().any(|o| o.split_once('=').is_some_and(|(k, _)| k == *name));
                if !already_set {
                    opts.push(format!("{name}={default_value}"));
                }
            }
            opts
        })
        .collect();
    let timestamp = Local::now();
    let shutdown = Arc::new(AtomicBool::new(false));

    // Ctrl-C ハンドラ
    {
        let shutdown_clone = shutdown.clone();
        ctrlc::set_handler(move || {
            eprintln!("\nShutting down gracefully...");
            shutdown_clone.store(true, Ordering::Relaxed);
        })
        .ok();
    }

    // meta.json 書き出し
    {
        let tournament_meta = TournamentMeta {
            timestamp: timestamp.to_rfc3339(),
            settings: MetaSettings {
                games: cli.games * 2,
                max_moves: cli.max_moves,
                byoyomi: cli.byoyomi,
                btime: cli.btime,
                binc: cli.binc,
                timeout_margin_ms: cli.timeout_margin_ms,
                threads: cli.threads,
                hash_mb: cli.hash_mb,
                depth: cli.depth,
                nodes: cli.nodes,
            },
            engines: (0..n)
                .map(|i| EngineMetaEntry {
                    index: i,
                    label: engine_labels[i].clone(),
                    path: cli.engines[i].display().to_string(),
                    usi_options: engine_usi_options[i].clone(),
                })
                .collect(),
            start_positions: start_commands.clone(),
            output_dir: cli.out_dir.display().to_string(),
        };
        let meta_file = File::create(cli.out_dir.join("meta.json"))?;
        serde_json::to_writer_pretty(BufWriter::new(meta_file), &tournament_meta)?;
    }

    // SPRT 有効化時のラベル解決（ticket 生成前に検証しておきたい）
    let mut sprt_state: Option<SprtState> = None;
    if cli.sprt {
        let base_label =
            cli.sprt_base_label.as_deref().or(cli.base_label.as_deref()).ok_or_else(|| {
                anyhow::anyhow!(
                    "--sprt 有効時は --sprt-base-label か --base-label のいずれかが必須です"
                )
            })?;
        let test_label = cli
            .sprt_test_label
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("--sprt 有効時は --sprt-test-label が必須です"))?;
        let base_i = engine_labels.iter().position(|l| l == base_label).ok_or_else(|| {
            anyhow::anyhow!("SPRT base label '{}' が engines に存在しません", base_label)
        })?;
        let test_i = engine_labels.iter().position(|l| l == test_label).ok_or_else(|| {
            anyhow::anyhow!("SPRT test label '{}' が engines に存在しません", test_label)
        })?;
        if base_i == test_i {
            bail!("--sprt-base-label と --sprt-test-label は異なるエンジンである必要があります");
        }
        let params =
            SprtParameters::new(cli.sprt_nelo0, cli.sprt_nelo1, cli.sprt_alpha, cli.sprt_beta)
                .map_err(|e| anyhow::anyhow!(e))?;
        sprt_state = Some(SprtState::new(
            params,
            base_i,
            test_i,
            cli.sprt_report_interval,
            base_label.to_string(),
            test_label.to_string(),
        ));
    }

    // --base-label が指定されていればその index を resolve、それ以外は None（総当たり）
    let base_idx: Option<usize> = if let Some(ref label) = cli.base_label {
        match engine_labels.iter().position(|l| l == label) {
            Some(idx) => Some(idx),
            None => bail!("--base-label '{label}' が --engine / --engine-label に存在しません"),
        }
    } else {
        None
    };

    // ペア一覧を生成。base-vs-N モードなら base と他エンジンの組のみ。
    // いずれのモードでもキーは (min, max) で正規化。
    let pair_indices: Vec<(usize, usize)> = match base_idx {
        Some(bi) => (0..n)
            .filter(|&j| j != bi)
            .map(|j| if bi < j { (bi, j) } else { (j, bi) })
            .collect(),
        None => (0..n).flat_map(|i| ((i + 1)..n).map(move |j| (i, j))).collect(),
    };

    // 全ペア × games × 2方向のチケット生成
    // cli.games は「各方向の対局数」なので、1ペアあたり cli.games * 2 局
    let total_per_pair = cli.games * 2;
    let mut tickets: Vec<MatchTicket> = Vec::new();
    let mut rng = rand::rng();
    {
        let mut ticket_id = 0u64;
        for &(i, j) in &pair_indices {
            // ペア内の通し index (0, 1, 2, ...)。
            // game_idx % 2 == 0 が pair の 1 局目、== 1 が 2 局目（先後入替）。
            for game_idx in 0..total_per_pair {
                // base-vs-N モードでは base (bi) を先にする向きから始めたいので、
                // i < j の前提のもとで base が i 側にあるとは限らない点に注意する必要はない。
                // 任意の (i, j) に対し、偶数 game_idx で (i, j)、奇数で (j, i) という扱いで
                // pair_slot が 0 の手番と 1 の手番が必ず 1 ペアを成す。
                let (black_idx, white_idx) = if game_idx % 2 == 0 { (i, j) } else { (j, i) };
                let startpos_idx = if start_defs.len() == 1 {
                    0
                } else if game_idx % 2 == 0 {
                    *((0..start_defs.len()).collect::<Vec<_>>()).choose(&mut rng).unwrap()
                } else {
                    tickets.last().unwrap().startpos_idx
                };
                // `pair_index` / `pair_slot` は `ticket_id` 基準で一意化する。
                // ペア (偶数 ticket_id, 奇数 ticket_id) が常に 1 組になるよう
                // `pair_index = ticket_id / 2`, `pair_slot = ticket_id % 2`。
                let pair_slot = (ticket_id % 2) as u32;
                let pair_index = (ticket_id / 2) as u32;
                tickets.push(MatchTicket {
                    id: ticket_id,
                    black_idx,
                    white_idx,
                    startpos_idx,
                    pair_slot,
                    pair_index,
                });
                ticket_id += 1;
            }
        }
    }

    let total_games = tickets.len() as u32;
    let mode_label = if base_idx.is_some() {
        "base-vs-N"
    } else {
        "round-robin"
    };
    println!(
        "tournament: {} engines, {} pairs ({}), {} games/direction, {} games/pair, {} total games, concurrency={}",
        n,
        pair_indices.len(),
        mode_label,
        cli.games,
        total_per_pair,
        total_games,
        cli.concurrency
    );

    // ペア別のファイルライターを準備し、meta行を書き出す
    let mut pair_writers: HashMap<(usize, usize), PairWriter> = HashMap::new();
    // ペアごとのゲームカウンター
    let mut pair_game_count: HashMap<(usize, usize), u32> = HashMap::new();

    for &(i, j) in &pair_indices {
        {
            let filename = format!("{}-vs-{}.jsonl", engine_labels[i], engine_labels[j]);
            let path = cli.out_dir.join(&filename);
            let mut pw = PairWriter::new(&path)?;

            // SPRT 有効かつこのペアが base/test の組み合わせなら SPRT meta を付与する。
            // pair_indices は (min, max) 正規化済みなので両順序をカバーする。
            let sprt_meta = sprt_state.as_ref().and_then(|state| {
                let pair = (state.base_idx.min(state.test_idx), state.base_idx.max(state.test_idx));
                if pair == (i, j) {
                    Some(SprtMetaLog {
                        base_label: state.base_label.clone(),
                        test_label: state.test_label.clone(),
                        nelo0: state.params.nelo0,
                        nelo1: state.params.nelo1,
                        alpha: state.params.alpha,
                        beta: state.params.beta,
                    })
                } else {
                    None
                }
            });

            // meta行を書く
            let meta = MetaLogEntry {
                kind: "meta".to_string(),
                timestamp: timestamp.to_rfc3339(),
                settings: MetaSettings {
                    games: cli.games * 2, // 各方向 cli.games 局、双方向で合計
                    max_moves: cli.max_moves,
                    byoyomi: cli.byoyomi,
                    btime: cli.btime,
                    binc: cli.binc,
                    timeout_margin_ms: cli.timeout_margin_ms,
                    threads: cli.threads,
                    hash_mb: cli.hash_mb,
                    depth: cli.depth,
                    nodes: cli.nodes,
                },
                engine_cmd: EngineCommandMeta {
                    path_black: cli.engines[i].display().to_string(),
                    path_white: cli.engines[j].display().to_string(),
                    label_black: engine_labels[i].clone(),
                    label_white: engine_labels[j].clone(),
                    usi_options_black: engine_usi_options[i].clone(),
                    usi_options_white: engine_usi_options[j].clone(),
                },
                start_positions: start_commands.clone(),
                output: path.display().to_string(),
                sprt: sprt_meta,
            };
            pw.write_json(&meta)?;
            pw.flush()?;

            pair_writers.insert((i, j), pw);
            pair_game_count.insert((i, j), 0);
        }
    }

    // チャネルの作成（ランデブー）
    let (ticket_tx, ticket_rx) = chan::bounded::<Option<MatchTicket>>(0);
    let (result_tx, result_rx) = chan::bounded::<MatchResult>(0);

    // ワーカースレッドの起動
    let mut handles = Vec::new();
    for _ in 0..cli.concurrency {
        let engine_paths = cli.engines.clone();
        let labels = engine_labels.clone();
        let usi_opts = engine_usi_options.clone();
        let threads = cli.threads;
        let hash_mb = cli.hash_mb;
        let max_moves = cli.max_moves;
        let timeout_margin_ms = cli.timeout_margin_ms;
        let byoyomi = cli.byoyomi;
        let btime = cli.btime;
        let binc = cli.binc;
        let go_depth = cli.depth;
        let go_nodes = cli.nodes;
        let start_positions: Vec<ParsedPosition> = start_defs
            .iter()
            .map(|p| ParsedPosition {
                startpos: p.startpos,
                sfen: p.sfen.clone(),
                moves: p.moves.clone(),
            })
            .collect();
        let rx = ticket_rx.clone();
        let tx = result_tx.clone();
        let sd = shutdown.clone();
        handles.push(thread::spawn(move || {
            worker_main(
                WorkerConfig {
                    engine_paths,
                    engine_labels: labels,
                    engine_usi_options: usi_opts,
                    threads,
                    hash_mb,
                    max_moves,
                    timeout_margin_ms,
                    byoyomi,
                    btime,
                    binc,
                    go_depth,
                    go_nodes,
                    start_positions,
                },
                rx,
                tx,
                sd,
            );
        }));
    }
    // メインスレッドは result_tx を持たないので drop
    drop(result_tx);

    // 勝敗カウンター: pair_key → (wins_i, wins_j, draws) (i < j)
    let mut pair_stats: HashMap<(usize, usize), (u32, u32, u32)> = HashMap::new();
    for &(i, j) in &pair_indices {
        pair_stats.insert((i, j), (0, 0, 0));
    }

    let start_time = Instant::now();
    let mut completed = 0u32;
    let mut ticket_iter = tickets.into_iter();
    let mut next_ticket: Option<MatchTicket> = ticket_iter.next();
    // SPRT が境界到達した時点で feeding を停止する。
    // 停止後は total_to_complete = sent に落として、
    // 在庫（ワーカーが今受け取っているチケット）だけを drain する。
    let mut sent: u32 = 0;
    let mut total_to_complete: u32 = total_games;
    let mut sprt_stopped = false;

    // メインイベントループ
    while completed < total_to_complete && !shutdown.load(Ordering::Relaxed) {
        if sprt_stopped {
            next_ticket = None;
        }
        match &next_ticket {
            None => {
                // チケットは全送信済み、結果を待つ
                match result_rx.recv() {
                    Ok(result) => {
                        process_result(
                            &result,
                            &engine_labels,
                            &mut pair_writers,
                            &mut pair_stats,
                            &mut pair_game_count,
                        )?;
                        completed += 1;
                        handle_sprt_observation(
                            &mut sprt_state,
                            &result,
                            &mut sprt_stopped,
                            &mut total_to_complete,
                            sent,
                            completed,
                        );
                        if completed.is_multiple_of(cli.report_interval)
                            || completed == total_to_complete
                        {
                            print_progress(
                                completed,
                                total_to_complete,
                                &pair_stats,
                                &engine_labels,
                                start_time,
                            );
                        }
                    }
                    Err(_) => break,
                }
            }
            Some(t) => {
                chan::select! {
                    send(ticket_tx, Some(t.clone())) -> res => {
                        if res.is_ok() {
                            sent += 1;
                            next_ticket = ticket_iter.next();
                        }
                    }
                    recv(result_rx) -> result => {
                        if let Ok(result) = result {
                            process_result(
                                &result,
                                &engine_labels,
                                &mut pair_writers,
                                &mut pair_stats,
                                &mut pair_game_count,
                            )?;
                            completed += 1;
                            handle_sprt_observation(
                                &mut sprt_state,
                                &result,
                                &mut sprt_stopped,
                                &mut total_to_complete,
                                sent,
                                completed,
                            );
                            if completed.is_multiple_of(cli.report_interval)
                                || completed == total_to_complete
                            {
                                print_progress(
                                    completed,
                                    total_to_complete,
                                    &pair_stats,
                                    &engine_labels,
                                    start_time,
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    // ワーカーを停止
    for _ in 0..cli.concurrency {
        let _ = ticket_tx.send(None);
    }
    for h in handles {
        let _ = h.join();
    }

    // ライターをフラッシュ
    for (_, pw) in pair_writers.iter_mut() {
        pw.flush()?;
    }

    println!();
    println!("=== Tournament Complete ===");
    println!("Total: {} games in {:.1}s", completed, start_time.elapsed().as_secs_f64());
    print_final_table(&pair_stats, &engine_labels);
    println!("Output: {}", cli.out_dir.display());
    println!("===========================");

    if let Some(state) = sprt_state.as_ref() {
        print_sprt_final(state);
    }

    Ok(())
}

fn handle_sprt_observation(
    sprt_state: &mut Option<SprtState>,
    result: &MatchResult,
    sprt_stopped: &mut bool,
    total_to_complete: &mut u32,
    sent: u32,
    completed: u32,
) {
    let Some(state) = sprt_state.as_mut() else {
        return;
    };
    let was_terminal = state.stopped_at.is_some();
    let _ = state.observe(result);
    state.maybe_report(false);
    if !was_terminal && state.stopped_at.is_some() && !*sprt_stopped {
        *sprt_stopped = true;
        *total_to_complete = sent;
        let remaining = total_to_complete.saturating_sub(completed);
        println!("[SPRT] terminal decision reached; draining {} in-flight game(s)...", remaining);
    }
}

// ---------------------------------------------------------------------------
// ヘルパー
// ---------------------------------------------------------------------------

fn process_result(
    result: &MatchResult,
    engine_labels: &[String],
    pair_writers: &mut HashMap<(usize, usize), PairWriter>,
    pair_stats: &mut HashMap<(usize, usize), (u32, u32, u32)>,
    pair_game_count: &mut HashMap<(usize, usize), u32>,
) -> Result<()> {
    let bi = result.ticket.black_idx;
    let wi = result.ticket.white_idx;
    let pair_key = if bi < wi { (bi, wi) } else { (wi, bi) };

    // ゲーム番号をペアごとに採番
    let game_num = pair_game_count.entry(pair_key).or_insert(0);
    *game_num += 1;
    let game_id = *game_num;

    // ファイルに書き出し
    if let Some(pw) = pair_writers.get_mut(&pair_key) {
        for ml in &result.move_logs {
            // game_id をペアローカルのものに書き換え
            let entry = MoveLogEntry {
                kind: ml.kind,
                game_id,
                ply: ml.ply,
                side_to_move: ml.side_to_move,
                sfen_before: ml.sfen_before.clone(),
                move_usi: ml.move_usi.clone(),
                raw_move_usi: ml.raw_move_usi.clone(),
                engine: ml.engine.clone(),
                elapsed_ms: ml.elapsed_ms,
                think_limit_ms: ml.think_limit_ms,
                timed_out: ml.timed_out,
                eval: ml.eval.clone(),
            };
            pw.write_json(&entry)?;
        }
        let winner = match result.outcome {
            GameOutcome::BlackWin => Some(engine_labels[result.ticket.black_idx].clone()),
            GameOutcome::WhiteWin => Some(engine_labels[result.ticket.white_idx].clone()),
            GameOutcome::Draw | GameOutcome::InProgress => None,
        };
        let result_entry = ResultLogEntry {
            kind: "result",
            game_id,
            outcome: result.outcome.label(),
            reason: &result.reason,
            plies: result.plies,
            winner,
            ticket_id: Some(result.ticket.id),
            pair_index: Some(result.ticket.pair_index),
            pair_slot: Some(result.ticket.pair_slot),
            startpos_idx: Some(result.ticket.startpos_idx as u32),
            error: result.error,
        };
        pw.write_json(&result_entry)?;
        pw.flush()?;
    }

    // 統計更新
    if let Some(stats) = pair_stats.get_mut(&pair_key) {
        match result.outcome {
            GameOutcome::BlackWin => {
                if bi == pair_key.0 {
                    stats.0 += 1; // i wins
                } else {
                    stats.1 += 1; // j wins
                }
            }
            GameOutcome::WhiteWin => {
                if wi == pair_key.0 {
                    stats.0 += 1;
                } else {
                    stats.1 += 1;
                }
            }
            GameOutcome::Draw | GameOutcome::InProgress => {
                stats.2 += 1;
            }
        }
    }

    Ok(())
}

fn print_progress(
    completed: u32,
    total: u32,
    pair_stats: &HashMap<(usize, usize), (u32, u32, u32)>,
    engine_labels: &[String],
    start_time: Instant,
) {
    let elapsed = start_time.elapsed().as_secs_f64();
    let gps = if elapsed > 0.0 {
        completed as f64 / elapsed
    } else {
        0.0
    };
    println!(
        "\n--- Progress: {}/{} ({:.1}%) | {:.1} games/sec ---",
        completed,
        total,
        completed as f64 / total as f64 * 100.0,
        gps
    );
    for (&(i, j), &(wi, wj, d)) in pair_stats {
        let total_pair = wi + wj + d;
        if total_pair == 0 {
            continue;
        }
        let li = &engine_labels[i];
        let lj = &engine_labels[j];
        let wr = if total_pair > 0 {
            (wi as f64 + d as f64 * 0.5) / total_pair as f64 * 100.0
        } else {
            0.0
        };
        println!(
            "  {} vs {}: {}W-{}L-{}D ({} games, {} win rate: {:.1}%)",
            li, lj, wi, wj, d, total_pair, li, wr
        );
    }
}

fn print_final_table(
    pair_stats: &HashMap<(usize, usize), (u32, u32, u32)>,
    engine_labels: &[String],
) {
    println!();
    for (&(i, j), &(wi, wj, d)) in pair_stats {
        let total_pair = wi + wj + d;
        if total_pair == 0 {
            continue;
        }
        let li = &engine_labels[i];
        let lj = &engine_labels[j];
        let score_i = wi as f64 + d as f64 * 0.5;
        let wr = score_i / total_pair as f64;
        let elo = if wr > 0.0 && wr < 1.0 {
            Some(-400.0 * (1.0 / wr - 1.0).log10())
        } else {
            None
        };
        let elo_str = elo.map_or("N/A".to_string(), |e| format!("{:+.0}", e));
        println!(
            "  {} vs {}: {}W-{}L-{}D | {} win rate: {:.1}% | Elo: {}",
            li,
            lj,
            wi,
            wj,
            d,
            li,
            wr * 100.0,
            elo_str
        );
    }
}

fn engine_label_from_path(path: &Path) -> String {
    let filename = path.file_name().and_then(|s| s.to_str()).unwrap_or("unknown");
    // rshogi-usi-HASH パターンからハッシュ部分を抽出
    if let Some(rest) = filename.strip_prefix("rshogi-usi-") {
        let hash: String = rest.chars().take(8).collect();
        if !hash.is_empty() {
            return hash;
        }
    }
    filename.to_string()
}
