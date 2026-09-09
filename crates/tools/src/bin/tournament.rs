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
///   --engine /path/to/YaneuraOu/source/YaneuraOu-halfkp_256x2-32-32 --engine-label yaneuraou \
///   --games 50 --byoyomi 500 --threads 2 \
///   --usi-option "FV_SCALE=24" \
///   --strict-engine-usi-option \
///   --engine-usi-option "0:EvalFile=eval/halfkp_256x2-32-32_crelu/suisho5.bin" \
///   --engine-usi-option "1:EvalDir=$SHOGI_DATA/nnue/halfkp_256x2-32-32_crelu" \
///   --engine-usi-option "1:NetworkDelay2=0" \
///   --engine-usi-option "1:RoundUpToFullSecond=false" \
///   --out-dir "runs/selfplay/$(date +%Y%m%d_%H%M%S)-rshogi-vs-yaneuraou-suisho5"
/// ```
///
/// # 実行中の動的制御（再起動不要）
///
/// `<out-dir>/control.json` を書き換えると、再起動せず対局境界で以下を変更できる:
///
/// ```json
/// { "target_games": 300, "concurrency": 16 }
/// ```
///
/// - `target_games`: 各方向・各ペアあたりの目標対局数（CLI `--games` と同じ単位）。
///   増やすと既存ペアに追加チケットを供給し、減らすと in-flight を drain して停止する。
/// - `concurrency`: ワーカー数。増やすと即座に追加 spawn（NNUE ロード相当のコストあり）、
///   減らすと対象ワーカーが現局面完了後に退役する。
///
/// 変更は `<out-dir>/control_history.jsonl` に追記され、`pair_index` 整合は維持されるため
/// `analyze_selfplay` の集計と矛盾しない。
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use chrono::Local;
use clap::Parser as _;
use crossbeam_channel as chan;
use rand::Rng as _;
use serde::{Deserialize, Serialize};

use tools::selfplay::game::{GameConfig, MoveEvent, run_game};
use tools::selfplay::time_control::TimeControl;
use tools::selfplay::types::{EvalLog, side_label};
use tools::selfplay::{DrawRule, ResignRule};
use tools::selfplay::{
    EngineConfig, EngineProcess, GameOutcome, ParsedPosition, load_start_positions,
};
use tools::sprt::{Decision, GameSide, Penta, SprtMetaLog, SprtParameters, judge};

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
    /// Merged on top of --usi-option by default; per-engine values win.
    #[arg(long = "engine-usi-option", num_args = 1..)]
    engine_usi_options: Option<Vec<String>>,

    /// Make --engine-usi-option replace common --usi-option for that engine.
    #[arg(long = "strict-engine-usi-option")]
    strict_engine_usi_option: bool,

    /// SPSA .params ファイルからUSIオプションを読み込む (format: "INDEX:path/to/file.params")。
    /// ファイル内の各パラメータが Name=Value として該当エンジンに設定される。
    /// --engine-usi-option と併用可。
    #[arg(long = "engine-params-file", num_args = 1..)]
    engine_params_files: Option<Vec<String>>,

    /// Maximum plies per game
    #[arg(long, default_value_t = 512)]
    max_moves: u32,
    /// 評価値による投了裁定 (movecount=3,score=600)。既定 off。
    #[arg(long, value_parser = clap::value_parser!(ResignRule))]
    adjudicate_resign: Option<ResignRule>,
    /// 評価値による引分裁定 (movenumber=34,movecount=8,score=20)。手数は ply。
    #[arg(long, value_parser = clap::value_parser!(DrawRule))]
    adjudicate_draw: Option<DrawRule>,

    /// Output directory (required)
    #[arg(long)]
    out_dir: PathBuf,

    /// Start position file (USI position lines, one per line)
    #[arg(long)]
    startpos_file: Option<PathBuf>,

    /// 開始局面選択用の乱数 seed。省略時は entropy から生成して起動ログに表示する。
    #[arg(long)]
    seed: Option<u64>,

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

    /// Per-engine fixed node budget (overrides global --nodes for that engine).
    /// Format: INDEX:NODES (0-based), can be repeated.
    #[arg(long = "engine-nodes", num_args = 1..)]
    engine_nodes: Option<Vec<String>>,

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
    /// 同じ論理ペアを再対局した世代。初回は 0。
    attempt: u32,
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
    /// この結果送信後に worker が終了する場合 true。
    worker_retired: bool,
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
    attempt: u32,
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
    /// base-vs-N モード（`--base-label`）時のみ付与される基準エンジンのラベル。
    /// analyze_selfplay 側で SPRT post-hoc のラベル役割（base / test）推定に利用する。
    #[serde(skip_serializing_if = "Option::is_none")]
    base_label: Option<String>,
    /// SPRT 実行時のみ付与される。base / test ラベルと Wald パラメータを含み、
    /// analyze_selfplay 側でラベル自動推定に利用する。
    #[serde(skip_serializing_if = "Option::is_none")]
    sprt: Option<SprtMetaLog>,
}

#[derive(Serialize)]
struct MetaSettings {
    games: u32,
    seed: u64,
    max_moves: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    adjudicate_resign: Option<ResignRule>,
    #[serde(skip_serializing_if = "Option::is_none")]
    adjudicate_draw: Option<DrawRule>,
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
    /// 解決済みの固定ノード予算（手番別）。`--engine-nodes` / global `--nodes` で
    /// エンジンに割り当てた値で、None はノード制限なし（時間 or depth で停止）。
    #[serde(skip_serializing_if = "Option::is_none")]
    nodes_black: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    nodes_white: Option<u64>,
}

#[derive(Serialize)]
struct TournamentMeta {
    timestamp: String,
    settings: MetaSettings,
    engines: Vec<EngineMetaEntry>,
    start_positions: Vec<String>,
    output_dir: String,
    #[serde(flatten)]
    retry: RetrySummary,
    run_status: RunStatus,
    /// 発行した通常ペアのうち、有効な 2 局が揃っていない数。
    incomplete_pairs: u32,
    /// 送信済みだが worker から結果を回収できなかったチケット数。
    unreturned_games: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum RunStatus {
    Running,
    Completed,
    Interrupted,
    WorkerFailed,
}

#[derive(Clone, Copy, Default, Serialize)]
struct RetrySummary {
    error_pairs: u32,
    retried_pairs: u32,
    exhausted_pairs: u32,
    invalid: bool,
}

#[derive(Serialize)]
struct EngineMetaEntry {
    index: usize,
    label: String,
    path: String,
    usi_options: Vec<String>,
    /// 解決済みの固定ノード予算。`--engine-nodes` / global `--nodes` でこの index に
    /// 割り当てた値で、None はノード制限なし（時間 or depth で停止）。
    #[serde(skip_serializing_if = "Option::is_none")]
    nodes: Option<u64>,
}

// ---------------------------------------------------------------------------
// SPRT 状態
// ---------------------------------------------------------------------------

/// SPRT 逐次判定のランタイム状態。
///
/// 視点は常に challenger (test) 側で固定される。pair 単位で集計するため、
/// 並列ワーカーから結果が順不同に到着しても `(pair_index, attempt)` でバッファして
/// 2 ゲームが揃った時点で Penta に反映する。
struct SprtState {
    params: SprtParameters,
    base_idx: usize,
    test_idx: usize,
    /// (pair_index, attempt) → 同世代 2 スロット分の結果（test 視点）
    buffer: HashMap<(u32, u32), PairObservation>,
    /// 集計済みの (pair_index, attempt)。完成後の重複到着を除外する。
    completed_pairs: HashSet<(u32, u32)>,
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

#[derive(Default)]
struct PairObservation {
    sides: [Option<GameSide>; 2],
    received: [bool; 2],
    error: bool,
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
            completed_pairs: HashSet::new(),
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
    /// - base と test のペアでなければ無視
    /// - 同世代の両スロットが揃い、error が無ければ Penta に反映して判定を更新
    ///
    /// 戻り値: 今回の取り込みで初めて terminal に到達したら `Some(Decision)`。
    fn observe(&mut self, result: &MatchResult) -> Option<Decision> {
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

        let key = (result.ticket.pair_index, result.ticket.attempt);
        let slot = result.ticket.pair_slot.min(1) as usize;
        if self.completed_pairs.contains(&key) {
            eprintln!(
                "警告: live SPRT — pair_index={}, attempt={}, slot={slot} は既に集計済みです。重複結果を除外します。",
                key.0, key.1
            );
            return None;
        }
        let entry = self.buffer.entry(key).or_default();
        if entry.received[slot] {
            eprintln!(
                "警告: live SPRT — pair_index={}, attempt={}, slot={slot} が重複しています。重複結果を除外します。",
                key.0, key.1
            );
            return None;
        }
        entry.received[slot] = true;
        entry.error |= result.error;
        if !result.error {
            entry.sides[slot] = Some(test_side);
        }

        if entry.received.into_iter().all(|received| received) {
            let completed = self.buffer.remove(&key)?;
            self.completed_pairs.insert(key);
            let (a, b) = (!completed.error)
                .then(|| completed.sides[0].zip(completed.sides[1]))
                .flatten()?;
            self.penta += Penta::from_pair(a, b);

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

fn print_sprt_final(state: &SprtState, retry: RetrySummary) {
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
    if retry.invalid {
        println!("WARNING: invalid test — exhausted error pairs: {}", retry.exhausted_pairs);
    }
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
    adjudicate_resign: Option<ResignRule>,
    adjudicate_draw: Option<DrawRule>,
    timeout_margin_ms: u64,
    byoyomi: u64,
    btime: u64,
    binc: u64,
    go_depth: Option<u32>,
    /// エンジン index ごとの固定ノード数。`go_nodes[i]` が `engine_paths[i]` に対応する。
    go_nodes: Vec<Option<u64>>,
    start_positions: Vec<ParsedPosition>,
}

fn worker_main(
    cfg: WorkerConfig,
    rx: chan::Receiver<Option<MatchTicket>>,
    tx: chan::Sender<MatchResult>,
    shutdown: Arc<AtomicBool>,
    worker_failed: Arc<AtomicBool>,
) {
    let WorkerConfig {
        engine_paths,
        engine_labels,
        engine_usi_options,
        threads,
        hash_mb,
        max_moves,
        adjudicate_resign,
        adjudicate_draw,
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
                worker_failed.store(true, Ordering::Relaxed);
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

        // usinewgame/isready が失敗した EngineProcess は同期状態を保証できない。
        // error 結果を 1 件返して worker ごと退役し、再試行は新しいプロセスへ渡す。
        if let Err(e) = black.new_game().and_then(|_| white.new_game()) {
            eprintln!("worker: new game error: {e}; retiring worker");
            let _ = tx.send(MatchResult {
                ticket,
                outcome: GameOutcome::Draw,
                reason: format!("error: new game: {e}"),
                plies: 0,
                move_logs: Vec::new(),
                error: true,
                worker_retired: true,
            });
            return;
        }

        let start_pos = &start_positions[ticket.startpos_idx];
        let tc = TimeControl::new(btime, btime, binc, binc, byoyomi);
        let config = GameConfig {
            limit_only_timeout_ms: None,
            cancel: None,
            max_moves,
            resign_rule: adjudicate_resign,
            draw_rule: adjudicate_draw,
            timeout_margin_ms,
            pass_rights: None,
            go_depth,
            go_nodes_black: go_nodes[ticket.black_idx],
            go_nodes_white: go_nodes[ticket.white_idx],
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

        // run_game の panic を捕捉する。捕捉しないと worker が結果を送らず終了し、
        // メインの `result_rx.recv()` が in-flight 分を永久に待ってハングする。
        // panic 時はエラー結果を 1 件送り、状態が不確かなエンジンを使い続けないよう
        // この worker を退役させる（チケットごとに必ず結果 1 件を保証）。
        let run_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_game(black, white, start_pos, tc, &config, game_id, &mut on_move, None)
        }));
        match run_result {
            Ok(Ok(result)) => {
                let _ = tx.send(MatchResult {
                    ticket,
                    outcome: result.outcome,
                    reason: result.reason,
                    plies: result.plies,
                    move_logs,
                    error: false,
                    worker_retired: false,
                });
            }
            Ok(Err(e)) => {
                eprintln!("worker: game error: {e}; retiring worker");
                let _ = tx.send(MatchResult {
                    ticket,
                    outcome: GameOutcome::Draw,
                    reason: format!("error: {e}"),
                    plies: 0,
                    move_logs,
                    error: true,
                    worker_retired: true,
                });
                return;
            }
            Err(_) => {
                eprintln!("worker: game {game_id} panicked; retiring worker");
                let _ = tx.send(MatchResult {
                    ticket,
                    outcome: GameOutcome::Draw,
                    reason: "error: worker panic".to_string(),
                    plies: 0,
                    move_logs,
                    error: true,
                    worker_retired: true,
                });
                return;
            }
        }
    }
}

/// ワーカー spawn に必要な不変設定への参照束。チャネル送受信は保持しない。
///
/// これにより、終了処理でチャネル送信元を自由に drop してチャネルを閉じられる
/// （送信元を握り続ける長命なローカルが無いため、借用が終端まで残らない）。
struct SpawnCtx<'a> {
    engine_paths: &'a [PathBuf],
    engine_labels: &'a [String],
    engine_usi_options: &'a [Vec<String>],
    threads: usize,
    hash_mb: u32,
    max_moves: u32,
    adjudicate_resign: Option<ResignRule>,
    adjudicate_draw: Option<DrawRule>,
    timeout_margin_ms: u64,
    byoyomi: u64,
    btime: u64,
    binc: u64,
    go_depth: Option<u32>,
    /// エンジン index ごとの固定ノード数。`go_nodes[i]` が `engine_paths[i]` に対応する。
    go_nodes: &'a [Option<u64>],
    start_defs: &'a [ParsedPosition],
}

/// ワーカースレッドを 1 つ起動して `handles` に追加する。初期起動と動的増員で共用する。
fn spawn_worker(
    ctx: &SpawnCtx,
    ticket_rx: &chan::Receiver<Option<MatchTicket>>,
    result_tx: &chan::Sender<MatchResult>,
    shutdown: &Arc<AtomicBool>,
    worker_failed: &Arc<AtomicBool>,
    handles: &mut Vec<thread::JoinHandle<()>>,
) {
    let cfg = WorkerConfig {
        engine_paths: ctx.engine_paths.to_vec(),
        engine_labels: ctx.engine_labels.to_vec(),
        engine_usi_options: ctx.engine_usi_options.to_vec(),
        threads: ctx.threads,
        hash_mb: ctx.hash_mb,
        max_moves: ctx.max_moves,
        adjudicate_resign: ctx.adjudicate_resign,
        adjudicate_draw: ctx.adjudicate_draw,
        timeout_margin_ms: ctx.timeout_margin_ms,
        byoyomi: ctx.byoyomi,
        btime: ctx.btime,
        binc: ctx.binc,
        go_depth: ctx.go_depth,
        go_nodes: ctx.go_nodes.to_vec(),
        start_positions: ctx
            .start_defs
            .iter()
            .map(|p| ParsedPosition {
                startpos: p.startpos,
                sfen: p.sfen.clone(),
                moves: p.moves.clone(),
                source_line: p.source_line,
            })
            .collect(),
    };
    let rx = ticket_rx.clone();
    let tx = result_tx.clone();
    let sd = shutdown.clone();
    let failed = worker_failed.clone();
    handles.push(thread::spawn(move || {
        // panic で結果送信元だけが消えても、main が channel 待ちを続けないよう通知する。
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            worker_main(cfg, rx, tx, sd.clone(), failed.clone());
        }))
        .is_err()
        {
            failed.store(true, Ordering::Relaxed);
            sd.store(true, Ordering::Relaxed);
        }
    }));
}

// ---------------------------------------------------------------------------
// メイン
// ---------------------------------------------------------------------------

fn main() -> Result<()> {
    let cli = Cli::parse();
    let seed = cli.seed.unwrap_or_else(|| rand::rng().random());
    eprintln!("startpos seed: {seed}");

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

    // エンジンごとの固定ノード数を解決する。
    // --engine-nodes が指定されたエンジンはその値を、それ以外は global --nodes を使う。
    let engine_nodes = resolve_engine_nodes(n, cli.nodes, cli.engine_nodes.as_deref())?;

    // 時間管理のバリデーション
    let use_byoyomi = cli.byoyomi > 0;
    let use_fischer = cli.btime > 0 || cli.binc > 0;
    // engine_nodes は cli.nodes も織り込んだ解決済み Vec なので、resolve 後の状態で判定する。
    let use_fixed = cli.depth.is_some() || engine_nodes.iter().any(|n| n.is_some());
    if use_byoyomi && use_fischer {
        bail!("--byoyomi と --btime/--binc は同時に指定できません");
    }
    if !use_byoyomi && !use_fischer && !use_fixed {
        bail!(
            "時間管理を指定してください: --byoyomi, --btime/--binc, --depth, --nodes, --engine-nodes のいずれか"
        );
    }
    if use_fischer && cli.btime == 0 && cli.binc == 0 {
        bail!("フィッシャー時計: --btime または --binc の少なくとも一方を正の値で指定してください");
    }
    ensure_node_coverage(&engine_nodes, use_byoyomi || use_fischer, cli.depth.is_some())?;

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
            let mut opts = build_engine_usi_options(
                &common_usi_options,
                per_engine_usi.remove(&i),
                cli.strict_engine_usi_option,
            );
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
    let worker_failed = Arc::new(AtomicBool::new(false));

    // Ctrl-C ハンドラ
    {
        let shutdown_clone = shutdown.clone();
        ctrlc::set_handler(move || {
            eprintln!("\nShutting down gracefully...");
            shutdown_clone.store(true, Ordering::Relaxed);
        })
        .ok();
    }

    let mut tournament_meta = TournamentMeta {
        timestamp: timestamp.to_rfc3339(),
        settings: MetaSettings {
            games: cli.games * 2,
            seed,
            max_moves: cli.max_moves,
            adjudicate_resign: cli.adjudicate_resign,
            adjudicate_draw: cli.adjudicate_draw,
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
                nodes: engine_nodes[i],
            })
            .collect(),
        start_positions: start_commands.clone(),
        output_dir: cli.out_dir.display().to_string(),
        retry: RetrySummary::default(),
        run_status: RunStatus::Running,
        incomplete_pairs: 0,
        unreturned_games: 0,
    };
    // meta.json 書き出し
    {
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

    // チケットは事前生成せず、共有 atomic な目標値を参照する TicketSource から逐次発行する。
    // これにより実行中に control.json で target_games を増減でき、対局境界で追従する。
    // cli.games は「各方向の対局数」。1 ペアあたり cli.games * 2 局。
    let target_per_dir = Arc::new(AtomicU32::new(cli.games));
    let mut source =
        TicketSource::new(pair_indices.clone(), start_defs.len(), target_per_dir.clone(), seed);

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
        cli.games * 2,
        source.current_target_total(),
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
                    seed,
                    max_moves: cli.max_moves,
                    adjudicate_resign: cli.adjudicate_resign,
                    adjudicate_draw: cli.adjudicate_draw,
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
                    nodes_black: engine_nodes[i],
                    nodes_white: engine_nodes[j],
                },
                start_positions: start_commands.clone(),
                output: path.display().to_string(),
                base_label: cli.base_label.clone(),
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

    // ワーカー spawn 用の不変設定（チャネル送受信は持たない）。
    // チャネルを保持しないことで、終了時に送信元を drop してチャネルを閉じ、
    // ワーカー数を数えずに安全に全ワーカーを退役させられる。
    let spawn_ctx = SpawnCtx {
        engine_paths: &cli.engines,
        engine_labels: &engine_labels,
        engine_usi_options: &engine_usi_options,
        threads: cli.threads,
        hash_mb: cli.hash_mb,
        max_moves: cli.max_moves,
        adjudicate_resign: cli.adjudicate_resign,
        adjudicate_draw: cli.adjudicate_draw,
        timeout_margin_ms: cli.timeout_margin_ms,
        byoyomi: cli.byoyomi,
        btime: cli.btime,
        binc: cli.binc,
        go_depth: cli.depth,
        go_nodes: &engine_nodes,
        start_defs: &start_defs,
    };

    // ワーカースレッドの起動
    let mut handles = Vec::new();
    for _ in 0..cli.concurrency {
        spawn_worker(&spawn_ctx, &ticket_rx, &result_tx, &shutdown, &worker_failed, &mut handles);
    }

    // 勝敗カウンターと出力をまとめる集計器。
    let mut pair_stats: HashMap<(usize, usize), (u32, u32, u32)> = HashMap::new();
    for &(i, j) in &pair_indices {
        pair_stats.insert((i, j), (0, 0, 0));
    }
    let start_time = Instant::now();
    let mut agg = Aggregator {
        engine_labels: &engine_labels,
        pair_writers,
        pair_stats,
        pair_game_count,
        direct_buffer: HashMap::new(),
        direct_completed_pairs: HashSet::new(),
        completed: 0,
        valid_completed: 0,
        sprt_state,
        stop_feeding: false,
        // 0 だと進捗が一切出ないため最低 1 に丸める。
        report_interval: cli.report_interval.max(1),
        start_time,
    };

    // 制御プレーンの状態。
    let control_path = cli.out_dir.join("control.json");
    let history_path = cli.out_dir.join("control_history.jsonl");
    let mut applied = ControlState {
        target_games: cli.games,
        concurrency: cli.concurrency,
    };
    println!(
        "[control] 実行中の動的制御: {} を {}ms 間隔でポーリング (例: echo '{{\"target_games\":N,\"concurrency\":M}}' > {})",
        control_path.display(),
        CONTROL_POLL_INTERVAL.as_millis(),
        control_path.display(),
    );

    let mut tickets_sent: u32 = 0;
    let mut live_workers = cli.concurrency;
    let mut desired_workers = cli.concurrency;
    let mut last_poll: Option<Instant> = None;
    // 発行済みだがまだ送信できていないチケットを保持する（peek 相当）。
    // `select!` で recv 側が選ばれてもチケットを失わないよう、送信成功時のみ消費する。
    let mut pending: Option<MatchTicket> = None;

    // メインイベントループ。
    // - 供給: TicketSource から逐次チケットを発行（target_games に追従）
    // - 退役: live_workers > desired_workers なら poison(None) を 1 つ送る
    // - 増員: live_workers < desired_workers なら worker を spawn
    // - 終了: 供給するものが無く、in-flight も 0 になったら抜ける
    while !shutdown.load(Ordering::Relaxed) {
        // control.json を throttle ポーリング。
        if last_poll.is_none_or(|t| t.elapsed() >= CONTROL_POLL_INTERVAL) {
            last_poll = Some(Instant::now());
            apply_control(
                &control_path,
                &history_path,
                &mut applied,
                &target_per_dir,
                &mut desired_workers,
                agg.completed,
            );
        }

        // 増員（即時 spawn）。
        while live_workers < desired_workers {
            spawn_worker(
                &spawn_ctx,
                &ticket_rx,
                &result_tx,
                &shutdown,
                &worker_failed,
                &mut handles,
            );
            live_workers += 1;
        }

        // 供給ペイロード: poison(worker 退役) を最優先、次に通常チケット。
        // None = 今は供給するものが無い。チケットは送信成功まで `pending` に peek 保持し、
        // 送信前に target が下がった場合は `still_wanted` で再評価して破棄する。
        let offer: Option<Option<MatchTicket>> = if live_workers > desired_workers {
            Some(None)
        } else if agg.stop_feeding && source.retry_queue.is_empty() {
            None
        } else {
            if agg.stop_feeding && pending.as_ref().is_some_and(|ticket| ticket.attempt == 0) {
                pending = None;
            }
            if pending.as_ref().is_some_and(|t| !source.still_wanted(t)) {
                pending = None; // target 減少で不要になった未送信チケットを破棄
            }
            if pending.is_none() {
                pending = source.peek_ticket();
            }
            pending.clone().map(Some)
        };
        let target_total = source.current_target_total();

        match offer {
            Some(payload) => {
                let is_poison = payload.is_none();
                chan::select! {
                    send(ticket_tx, payload.clone()) -> res => {
                        if res.is_ok() {
                            if is_poison {
                                live_workers -= 1;
                            } else if let Some(ticket) = pending.take() {
                                // 送信成功時のみ状態を確定して消費。
                                source.commit_sent(&ticket);
                                tickets_sent += 1;
                            }
                        }
                    }
                    recv(result_rx) -> result => {
                        if let Ok(result) = result {
                            agg.on_result(&result, tickets_sent, target_total)?;
                            source.observe_result(&result);
                            if result.worker_retired {
                                live_workers = live_workers.saturating_sub(1);
                            }
                        }
                    }
                    // 全 worker の初期化失敗でも送信待ちへ残らず shutdown を再確認する。
                    default(Duration::from_millis(100)) => {}
                }
            }
            None => {
                let inflight = tickets_sent.saturating_sub(agg.completed);
                if inflight == 0 {
                    // 終了直前に最後の制御変更（target 増加）を取りこぼさないよう強制ポーリング。
                    last_poll = Some(Instant::now());
                    apply_control(
                        &control_path,
                        &history_path,
                        &mut applied,
                        &target_per_dir,
                        &mut desired_workers,
                        agg.completed,
                    );
                    if source.has_next() && (!agg.stop_feeding || !source.retry_queue.is_empty())
                        || live_workers > desired_workers
                    {
                        continue;
                    }
                    break;
                }
                // in-flight を drain。
                match result_rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(result) => {
                        agg.on_result(&result, tickets_sent, target_total)?;
                        source.observe_result(&result);
                        if result.worker_retired {
                            live_workers = live_workers.saturating_sub(1);
                        }
                    }
                    Err(chan::RecvTimeoutError::Timeout) => continue,
                    Err(chan::RecvTimeoutError::Disconnected) => break,
                }
            }
        }
    }

    // ワーカーの停止。送信元をすべて drop してチャネルを閉じることで、実在ワーカー数を
    // 数えずに idle ワーカーの `rx.recv()` を Err にして退役させる（worker 起動失敗・
    // poison 退役・shutdown など、どの経路でワーカーが抜けても live_workers の数え違いで
    // hang しない）。drop 後は残った in-flight 結果を drain し、全ワーカーが result 送信元を
    // 手放したら join する。
    drop(ticket_tx);
    drop(result_tx);
    while let Ok(result) = result_rx.recv() {
        agg.on_result(&result, tickets_sent, source.current_target_total())?;
        source.observe_result(&result);
    }
    for h in handles {
        if h.join().is_err() {
            worker_failed.store(true, Ordering::Relaxed);
        }
    }
    let run_status = if worker_failed.load(Ordering::Relaxed) {
        RunStatus::WorkerFailed
    } else if shutdown.load(Ordering::Relaxed) {
        RunStatus::Interrupted
    } else {
        RunStatus::Completed
    };

    // 集計器を分解して以降の表示に使う。
    let Aggregator {
        mut pair_writers,
        pair_stats,
        sprt_state,
        completed,
        valid_completed,
        ..
    } = agg;

    // ライターをフラッシュ
    for (_, pw) in pair_writers.iter_mut() {
        pw.flush()?;
    }

    // 完了直前の進捗を 1 行出す（report_interval で割り切れない端数対策）。
    // 直近のループで既に同じ行を出している場合（completed が interval の倍数）は重複を避ける。
    if valid_completed > 0 && !valid_completed.is_multiple_of(cli.report_interval.max(1)) {
        print_progress(
            valid_completed,
            source.current_target_total().max(valid_completed),
            &pair_stats,
            &engine_labels,
            start_time,
        );
    }

    let mut retry_summary = source.retry_summary();
    retry_summary.invalid |= run_status != RunStatus::Completed;
    tournament_meta.retry = retry_summary;
    tournament_meta.run_status = run_status;
    tournament_meta.incomplete_pairs = source
        .emitted
        .iter()
        .sum::<u32>()
        .div_ceil(2)
        .saturating_sub(valid_completed / 2);
    tournament_meta.unreturned_games = tickets_sent.saturating_sub(completed);
    let meta_file = File::create(cli.out_dir.join("meta.json"))?;
    serde_json::to_writer_pretty(BufWriter::new(meta_file), &tournament_meta)?;

    println!();
    println!(
        "{}",
        match run_status {
            RunStatus::Completed => "=== Tournament Complete ===",
            RunStatus::Interrupted => "=== Tournament Interrupted ===",
            RunStatus::WorkerFailed => "=== Tournament Worker Failed ===",
            RunStatus::Running => unreachable!(),
        }
    );
    println!("Total: {} games in {:.1}s", completed, start_time.elapsed().as_secs_f64());
    print_final_table(&pair_stats, &engine_labels);
    println!(
        "Error pairs: {}  Retried pairs: {}  Exhausted pairs: {}  Invalid: {}",
        retry_summary.error_pairs,
        retry_summary.retried_pairs,
        retry_summary.exhausted_pairs,
        retry_summary.invalid,
    );
    println!("Output: {}", cli.out_dir.display());
    println!("===========================");

    if run_status == RunStatus::Completed {
        if let Some(state) = sprt_state.as_ref() {
            print_sprt_final(state, retry_summary);
        }
    } else {
        println!("SPRT decision withheld: run did not complete.");
        bail!("tournament did not complete; recovered results and run status were saved");
    }

    Ok(())
}

fn handle_sprt_observation(
    sprt_state: &mut Option<SprtState>,
    result: &MatchResult,
    stop_feeding: &mut bool,
    completed: u32,
    tickets_sent: u32,
) {
    let Some(state) = sprt_state.as_mut() else {
        return;
    };
    let was_terminal = state.stopped_at.is_some();
    let _ = state.observe(result);
    state.maybe_report(false);
    if !was_terminal && state.stopped_at.is_some() && !*stop_feeding {
        // 境界到達: 新規チケット供給を止め、在庫（in-flight）だけ drain する。
        *stop_feeding = true;
        let inflight = tickets_sent.saturating_sub(completed);
        println!("[SPRT] terminal decision reached; draining {inflight} in-flight game(s)...");
    }
}

// ---------------------------------------------------------------------------
// 制御プレーン（実行中の動的制御）
// ---------------------------------------------------------------------------

/// `control.json` をポーリングする間隔。対局は秒オーダーなので 500ms で十分応答できる。
const CONTROL_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// 現在有効な制御値。`control.json` と差分があったフィールドだけ適用する。
#[derive(Clone, Copy)]
struct ControlState {
    /// 各方向・各ペアあたりの対局数（CLI `--games` と同じ単位）。
    target_games: u32,
    concurrency: usize,
}

/// `control.json` の受理スキーマ。各フィールドは任意で、存在するものだけ反映する。
#[derive(Deserialize)]
struct ControlFile {
    target_games: Option<u32>,
    concurrency: Option<usize>,
}

/// `control_history.jsonl` の 1 レコード。再現性のため変更を時系列で残す。
#[derive(Serialize)]
struct ControlHistoryEntry {
    #[serde(rename = "type")]
    kind: &'static str,
    timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_games: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    concurrency: Option<usize>,
    /// 変更を適用した時点で完了していた対局数。
    completed: u32,
}

/// `control.json` を読み、前回適用値との差分を反映する。
///
/// - ファイルが無い / 読めない / パースできない場合は無視（実行は継続）。
/// - `target_games` は共有 atomic を書き換えるだけで、`TicketSource` が次回参照時に追従する。
/// - `concurrency` は `desired_workers` を更新し、メインループが worker を追加 spawn / 退役させる。
/// - 差分があれば `control_history.jsonl` に追記する。
///
/// 長時間 background 運用での堅牢性を優先し、ファイル不在 / 読込失敗 / パース失敗 /
/// history 追記失敗はいずれも警告のみで実行を継続する（対局を落とさない）。
fn apply_control(
    control_path: &Path,
    history_path: &Path,
    applied: &mut ControlState,
    target_per_dir: &AtomicU32,
    desired_workers: &mut usize,
    completed: u32,
) {
    let Ok(text) = fs::read_to_string(control_path) else {
        return;
    };
    let parsed: ControlFile = match serde_json::from_str(&text) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[control] {} のパースに失敗したため無視します: {e}", control_path.display());
            return;
        }
    };

    let mut changed_target: Option<u32> = None;
    let mut changed_conc: Option<usize> = None;

    if let Some(t) = parsed.target_games
        && t != applied.target_games
    {
        applied.target_games = t;
        target_per_dir.store(t, Ordering::Relaxed);
        changed_target = Some(t);
    }
    if let Some(c) = parsed.concurrency {
        if c == 0 {
            eprintln!("[control] concurrency=0 は不正のため無視します");
        } else if c != applied.concurrency {
            applied.concurrency = c;
            *desired_workers = c;
            changed_conc = Some(c);
        }
    }

    if changed_target.is_some() || changed_conc.is_some() {
        println!(
            "[control] applied: target_games={changed_target:?} concurrency={changed_conc:?} (completed={completed})"
        );
        if let Err(e) = append_control_history(
            history_path,
            &ControlHistoryEntry {
                kind: "control",
                timestamp: Local::now().to_rfc3339(),
                target_games: changed_target,
                concurrency: changed_conc,
                completed,
            },
        ) {
            eprintln!(
                "[control] {} への履歴追記に失敗しました（実行は継続）: {e}",
                history_path.display()
            );
        }
    }
}

fn append_control_history(path: &Path, entry: &ControlHistoryEntry) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    serde_json::to_writer(&mut file, entry)?;
    file.write_all(b"\n")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// チケット供給源（動的 target 対応）
// ---------------------------------------------------------------------------

/// ペアごとに対局チケットを逐次生成する供給源。
///
/// 事前生成した `Vec` の代わりに、各ペアの発行済み数と共有 `target_per_dir`（atomic）を
/// 突き合わせて「まだ目標に達していない最初のペア」へ次のチケットを割り当てる。
/// これにより実行中の `target_games` 増減に対局境界で追従できる。
///
/// 通常チケットの `pair_index` / `pair_slot` は再試行を除く発行数から導出する。
/// 再試行チケットは新しい `id` を得る一方、元の `pair_index` / `pair_slot` を引き継ぐ。
/// 各ペアの通常発行数は常に偶数境界で次ペアへ移るため、2 局ペアが境界をまたがない。
struct TicketSource {
    pair_indices: Vec<(usize, usize)>,
    start_defs_len: usize,
    target_per_dir: Arc<AtomicU32>,
    seed: u64,
    /// ペアごとの発行済みゲーム数。
    emitted: Vec<u32>,
    retry_queue: VecDeque<MatchTicket>,
    retry_observations: HashMap<(u32, u32), RetryObservation>,
    error_pairs: HashSet<u32>,
    retried_pairs: HashSet<u32>,
    exhausted_pairs: HashSet<u32>,
    next_id: u64,
}

#[derive(Default)]
struct RetryObservation {
    tickets: [Option<MatchTicket>; 2],
    received: [bool; 2],
    error: bool,
}

impl TicketSource {
    fn new(
        pair_indices: Vec<(usize, usize)>,
        start_defs_len: usize,
        target_per_dir: Arc<AtomicU32>,
        seed: u64,
    ) -> Self {
        let pair_count = pair_indices.len();
        TicketSource {
            pair_indices,
            start_defs_len,
            target_per_dir,
            seed,
            emitted: vec![0; pair_count],
            retry_queue: VecDeque::new(),
            retry_observations: HashMap::new(),
            error_pairs: HashSet::new(),
            retried_pairs: HashSet::new(),
            exhausted_pairs: HashSet::new(),
            next_id: 0,
        }
    }

    /// 各ペアあたりの目標ゲーム数（双方向なので `target_games * 2`）。
    fn target_per_pair(&self) -> u32 {
        self.target_per_dir.load(Ordering::Relaxed).saturating_mul(2)
    }

    /// 現在の目標値での全ペア合計ゲーム数。進捗表示の分母に使う。
    fn current_target_total(&self) -> u32 {
        self.target_per_pair().saturating_mul(self.pair_indices.len() as u32)
    }

    /// ペア `k` がまだチケットを発行すべきか。
    ///
    /// `emitted < target` に加え、`emitted` が奇数（= ペアの 1 局目だけ発行済み）なら
    /// target を下回っていても 2 局目を発行してペアを完結させる。これにより実行中に
    /// `target_games` を下げても pentanomial の 2 局ペアが片側だけで残らない。
    fn pair_needs_more(emitted: u32, target_per_pair: u32) -> bool {
        emitted < target_per_pair || !emitted.is_multiple_of(2)
    }

    /// 現在の目標値で、まだ発行すべきチケットが残っているか。
    fn has_next(&self) -> bool {
        if !self.retry_queue.is_empty() {
            return true;
        }
        let target = self.target_per_pair();
        self.emitted.iter().any(|&e| Self::pair_needs_more(e, target))
    }

    /// 次に発行すべきチケットを計算する（状態は進めない）。全ペアが目標到達なら `None`。
    ///
    /// 状態を進めないため、`peek_ticket` で得たチケットを送信前に保持している間に
    /// `target_games` が変化しても、`still_wanted` で現在値に対して再評価できる。
    /// 送信が確定したら `commit_sent` で状態を進める。
    fn peek_ticket(&self) -> Option<MatchTicket> {
        if let Some(retry) = self.retry_queue.front() {
            let mut ticket = retry.clone();
            ticket.id = self.next_id;
            return Some(ticket);
        }
        let target = self.target_per_pair();
        let pair_pos = self.emitted.iter().position(|&e| Self::pair_needs_more(e, target))?;
        let (i, j) = self.pair_indices[pair_pos];
        let game_idx = self.emitted[pair_pos];

        // 偶数 game_idx が 1 局目 (i 先手)、奇数が 2 局目（先後入替）。
        let (black_idx, white_idx) = if game_idx.is_multiple_of(2) {
            (i, j)
        } else {
            (j, i)
        };
        let id = self.next_id;
        // retry ticket の id は増えても、通常発行した論理ペアの index は変えない。
        let normal_emitted: u32 = self.emitted.iter().copied().sum();
        let pair_index = normal_emitted / 2;
        let local_pair_index = game_idx / 2;
        Some(MatchTicket {
            id,
            black_idx,
            white_idx,
            startpos_idx: deterministic_startpos_index(
                self.seed,
                (i, j),
                local_pair_index,
                self.start_defs_len,
            ),
            pair_slot: game_idx % 2,
            pair_index,
            attempt: 0,
        })
    }

    /// `peek_ticket` が返したチケットの送信が確定したので状態を進める。
    ///
    /// `pair_indices` は `(min, max)` 正規化済み前提。チケットの先後 idx を同じく
    /// `(min, max)` に正規化して該当ペアを引き、その発行数を 1 進める。
    fn commit_sent(&mut self, ticket: &MatchTicket) {
        self.next_id = ticket.id + 1;
        if ticket.attempt > 0 {
            self.retry_queue.pop_front();
            return;
        }
        let pair = (ticket.black_idx.min(ticket.white_idx), ticket.black_idx.max(ticket.white_idx));
        if let Some(pos) = self.pair_indices.iter().position(|&p| p == pair) {
            self.emitted[pos] += 1;
        }
    }

    /// 保持中の peek 済みチケットが、現在の `target_games` でもまだ送信対象か。
    ///
    /// 送信前に target を下げた場合に、未送信のチケットを破棄して取りこぼさず反映するために使う。
    /// 2 局目（奇数 slot）はペアを完結させるため常に送る。1 局目（偶数 slot）は対象ペアが
    /// まだ target 未達のときだけ送る。
    fn still_wanted(&self, ticket: &MatchTicket) -> bool {
        if ticket.attempt > 0 {
            return true;
        }
        if !ticket.pair_slot.is_multiple_of(2) {
            return true;
        }
        let pair = (ticket.black_idx.min(ticket.white_idx), ticket.black_idx.max(ticket.white_idx));
        match self.pair_indices.iter().position(|&p| p == pair) {
            Some(pos) => Self::pair_needs_more(self.emitted[pos], self.target_per_pair()),
            None => false,
        }
    }

    fn observe_result(&mut self, result: &MatchResult) {
        let key = (result.ticket.pair_index, result.ticket.attempt);
        let slot = result.ticket.pair_slot.min(1) as usize;
        let entry = self.retry_observations.entry(key).or_default();
        entry.tickets[slot] = Some(result.ticket.clone());
        entry.received[slot] = true;
        entry.error |= result.error;
        if result.error {
            self.error_pairs.insert(result.ticket.pair_index);
        }
        if !entry.received.into_iter().all(|received| received) {
            return;
        }

        let Some(completed) = self.retry_observations.remove(&key) else {
            return;
        };
        if !completed.error {
            return;
        }
        if result.ticket.attempt >= 2 {
            self.exhausted_pairs.insert(result.ticket.pair_index);
            return;
        }

        self.retried_pairs.insert(result.ticket.pair_index);
        for ticket in completed.tickets.into_iter().flatten() {
            self.retry_queue.push_back(MatchTicket {
                id: 0,
                attempt: ticket.attempt + 1,
                ..ticket
            });
        }
    }

    fn retry_summary(&self) -> RetrySummary {
        RetrySummary {
            error_pairs: self.error_pairs.len() as u32,
            retried_pairs: self.retried_pairs.len() as u32,
            exhausted_pairs: self.exhausted_pairs.len() as u32,
            invalid: !self.exhausted_pairs.is_empty(),
        }
    }
}

fn deterministic_startpos_index(
    seed: u64,
    matchup: (usize, usize),
    local_pair_index: u32,
    startpos_count: usize,
) -> usize {
    if startpos_count <= 1 {
        return 0;
    }

    // matchup ごとに独立したストリームを作り、その matchup 内のペア通番で進める。
    // global ticket id を使わないため、control.json の target_games をいつ増やしても、
    // 同じ matchup / local_pair_index には同じ開始局面が割り当てられる。
    let first_engine_seed =
        splitmix64(seed.wrapping_add((matchup.0 as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15)));
    let matchup_seed = splitmix64(
        first_engine_seed.wrapping_add((matchup.1 as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15)),
    );
    let stream_value = splitmix64(matchup_seed)
        .wrapping_add((u64::from(local_pair_index) + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    (splitmix64(stream_value) % startpos_count as u64) as usize
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9E37_79B9_7F4A_7C15);
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
}

// ---------------------------------------------------------------------------
// ヘルパー
// ---------------------------------------------------------------------------

/// 対局結果の集計とファイル出力をまとめる。メインループの各 recv 地点から共通で呼ぶ。
struct Aggregator<'a> {
    engine_labels: &'a [String],
    pair_writers: HashMap<(usize, usize), PairWriter>,
    pair_stats: HashMap<(usize, usize), (u32, u32, u32)>,
    pair_game_count: HashMap<(usize, usize), u32>,
    direct_buffer: HashMap<(u32, u32), [Option<DirectResult>; 2]>,
    direct_completed_pairs: HashSet<(u32, u32)>,
    completed: u32,
    valid_completed: u32,
    sprt_state: Option<SprtState>,
    /// SPRT 境界到達後は新規供給を止めて drain する。
    stop_feeding: bool,
    report_interval: u32,
    start_time: Instant,
}

#[derive(Clone, Copy)]
struct DirectResult {
    black_idx: usize,
    white_idx: usize,
    outcome: GameOutcome,
    error: bool,
}

impl Aggregator<'_> {
    /// 1 件の対局結果を取り込む。集計・JSONL 出力・SPRT 観測・進捗表示まで行う。
    fn on_result(
        &mut self,
        result: &MatchResult,
        tickets_sent: u32,
        target_total: u32,
    ) -> Result<()> {
        process_result(
            result,
            self.engine_labels,
            &mut self.pair_writers,
            &mut self.pair_game_count,
        )?;
        if self.observe_direct_result(result) {
            self.valid_completed += 2;
        }
        self.completed += 1;
        handle_sprt_observation(
            &mut self.sprt_state,
            result,
            &mut self.stop_feeding,
            self.completed,
            tickets_sent,
        );
        if self.completed.is_multiple_of(self.report_interval) {
            print_progress(
                self.valid_completed,
                target_total.max(self.valid_completed),
                &self.pair_stats,
                self.engine_labels,
                self.start_time,
            );
        }
        Ok(())
    }

    fn observe_direct_result(&mut self, result: &MatchResult) -> bool {
        let key = (result.ticket.pair_index, result.ticket.attempt);
        let slot = result.ticket.pair_slot.min(1) as usize;
        if self.direct_completed_pairs.contains(&key) {
            eprintln!(
                "警告: 直接対決集計 — pair_index={}, attempt={}, slot={slot} は既に集計済みです。重複結果を除外します。",
                key.0, key.1
            );
            return false;
        }
        let entry = self.direct_buffer.entry(key).or_insert([None, None]);
        if entry[slot].is_some() {
            eprintln!(
                "警告: 直接対決集計 — pair_index={}, attempt={}, slot={slot} が重複しています。重複結果を除外します。",
                key.0, key.1
            );
            return false;
        }
        entry[slot] = Some(DirectResult {
            black_idx: result.ticket.black_idx,
            white_idx: result.ticket.white_idx,
            outcome: result.outcome,
            error: result.error,
        });
        let [Some(a), Some(b)] = *entry else {
            return false;
        };
        self.direct_buffer.remove(&key);
        self.direct_completed_pairs.insert(key);
        if a.error || b.error {
            return false;
        }
        for game in [a, b] {
            let pair_key = (game.black_idx.min(game.white_idx), game.black_idx.max(game.white_idx));
            if let Some(stats) = self.pair_stats.get_mut(&pair_key) {
                match game.outcome {
                    GameOutcome::BlackWin => {
                        if game.black_idx == pair_key.0 {
                            stats.0 += 1;
                        } else {
                            stats.1 += 1;
                        }
                    }
                    GameOutcome::WhiteWin => {
                        if game.white_idx == pair_key.0 {
                            stats.0 += 1;
                        } else {
                            stats.1 += 1;
                        }
                    }
                    GameOutcome::Draw => stats.2 += 1,
                    GameOutcome::InProgress => {}
                }
            }
        }
        true
    }
}

fn process_result(
    result: &MatchResult,
    engine_labels: &[String],
    pair_writers: &mut HashMap<(usize, usize), PairWriter>,
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
            attempt: result.ticket.attempt,
            startpos_idx: Some(result.ticket.startpos_idx as u32),
            error: result.error,
        };
        pw.write_json(&result_entry)?;
        pw.flush()?;
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

fn build_engine_usi_options(
    common: &[String],
    per_engine: Option<Vec<String>>,
    strict_engine_usi_option: bool,
) -> Vec<String> {
    let Some(per_engine) = per_engine else {
        return common.to_vec();
    };
    if strict_engine_usi_option {
        return per_engine;
    }

    let mut merged = common.to_vec();
    for option in per_engine {
        push_usi_option_overwrite(&mut merged, option);
    }
    merged
}

fn push_usi_option_overwrite(options: &mut Vec<String>, new_option: String) {
    let new_name = usi_option_name(&new_option).to_string();
    options.retain(|option| usi_option_name(option) != new_name);
    options.push(new_option);
}

fn usi_option_name(option: &str) -> &str {
    // 値側に '=' が含まれる USI option を壊さないよう、最初の '=' だけを区切りにする。
    option.split_once('=').map_or(option, |(name, _)| name).trim()
}

/// エンジン index ごとの固定ノード数を解決する。
///
/// `specs` は `"INDEX:NODES"` 形式（0 始まり）。指定のないエンジンは `global` に
/// フォールバックする。`global` も `None` なら、そのエンジンにはノード制限がない。
fn resolve_engine_nodes(
    n: usize,
    global: Option<u64>,
    specs: Option<&[String]>,
) -> Result<Vec<Option<u64>>> {
    let mut nodes = vec![global; n];
    if let Some(specs) = specs {
        for spec in specs {
            let (idx_str, nodes_str) = spec
                .split_once(':')
                .with_context(|| format!("invalid --engine-nodes format: {spec}"))?;
            let idx: usize =
                idx_str.parse().with_context(|| format!("invalid engine index: {idx_str}"))?;
            if idx >= n {
                bail!("--engine-nodes index {idx} out of range (0..{n})");
            }
            let value: u64 =
                nodes_str.parse().with_context(|| format!("invalid node count: {nodes_str}"))?;
            if value == 0 {
                bail!(
                    "--engine-nodes: ノード数は 1 以上が必要 (index {idx} に 0 が指定された)。\
                     `go nodes 0` は USI 仕様上未定義で停止条件にならない"
                );
            }
            nodes[idx] = Some(value);
        }
    }
    Ok(nodes)
}

/// 時間制御 (`--byoyomi`/`--btime`) も `--depth` も無い固定ノード探索では、全エンジンに
/// ノード数が要る。ノードも時間も無いエンジンは停止条件が無く `go` が即停止扱いになるため。
fn ensure_node_coverage(
    engine_nodes: &[Option<u64>],
    has_time: bool,
    has_depth: bool,
) -> Result<()> {
    if has_time || has_depth {
        return Ok(());
    }
    if let Some(idx) = engine_nodes.iter().position(|n| n.is_none()) {
        bail!(
            "時間制御 (--byoyomi/--btime) も --depth も無い場合、全エンジンにノード数が必要です \
             (engine index {idx} に予算がありません)。全エンジンを --engine-nodes \"IDX:NODES\" で \
             指定するか、global --nodes も併せて指定してください"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    #[test]
    fn adjudication_flags_parse_and_default_off() {
        let args = [
            "tournament",
            "--engine",
            "base",
            "--engine",
            "test",
            "--out-dir",
            "out",
        ];
        let cli = super::Cli::try_parse_from(args).unwrap();
        assert_eq!(cli.adjudicate_resign, None);
        assert_eq!(cli.adjudicate_draw, None);
        for separator in [",", " "] {
            let resign = ["movecount=3", "score=600"].join(separator);
            let draw = ["movenumber=34", "movecount=8", "score=20"].join(separator);
            let cli = super::Cli::try_parse_from(args.into_iter().chain([
                "--adjudicate-resign",
                resign.as_str(),
                "--adjudicate-draw",
                draw.as_str(),
            ]))
            .unwrap();
            assert_eq!(
                cli.adjudicate_resign,
                Some(super::ResignRule {
                    movecount: 3,
                    score: 600
                })
            );
            assert_eq!(
                cli.adjudicate_draw,
                Some(super::DrawRule {
                    movenumber: 34,
                    movecount: 8,
                    score: 20
                })
            );
        }
    }

    #[test]
    fn adjudication_flags_reject_invalid_fields() {
        for (flag, value) in [
            ("--adjudicate-resign", "movecount=3,score=600,unknown=1"),
            ("--adjudicate-resign", "movecount=3"),
            ("--adjudicate-resign", "score=600"),
            ("--adjudicate-resign", "movecount=3,score=600,score=20"),
            ("--adjudicate-resign", "movecount=0,score=600"),
            ("--adjudicate-resign", "movecount=3,score=-1"),
            ("--adjudicate-resign", "movecount=3,score=2147483648"),
            ("--adjudicate-resign", "movecount=x,score=600"),
            ("--adjudicate-resign", "movecount=3,score"),
            ("--adjudicate-draw", "movenumber=34,movecount=8,score=20,unknown=1"),
            ("--adjudicate-draw", "movecount=8,score=20"),
            ("--adjudicate-draw", "movenumber=34,score=20"),
            ("--adjudicate-draw", "movenumber=34,movecount=8"),
            ("--adjudicate-draw", "movenumber=34,movecount=8,score=20,movenumber=40"),
            ("--adjudicate-draw", "movenumber=34,movecount=0,score=20"),
        ] {
            assert!(
                super::Cli::try_parse_from([
                    "tournament",
                    "--engine",
                    "base",
                    "--engine",
                    "test",
                    "--out-dir",
                    "out",
                    flag,
                    value,
                ])
                .is_err(),
                "{flag} {value}"
            );
        }
    }

    #[test]
    fn adjudication_meta_omits_disabled_rules() {
        let mut settings = super::MetaSettings {
            games: 2,
            seed: 0,
            max_moves: 512,
            adjudicate_resign: None,
            adjudicate_draw: None,
            byoyomi: 100,
            btime: 0,
            binc: 0,
            timeout_margin_ms: 0,
            threads: 1,
            hash_mb: 16,
            depth: None,
            nodes: None,
        };
        let json = serde_json::to_value(&settings).unwrap();
        assert!(json.get("adjudicate_resign").is_none());
        assert!(json.get("adjudicate_draw").is_none());
        settings.adjudicate_resign = Some("movecount=3,score=600".parse().unwrap());
        settings.adjudicate_draw = Some("movenumber=34,movecount=8,score=20".parse().unwrap());
        let json = serde_json::to_value(&settings).unwrap();
        assert_eq!(json["adjudicate_resign"], serde_json::json!({"movecount":3,"score":600}));
        assert_eq!(
            json["adjudicate_draw"],
            serde_json::json!({"movenumber":34,"movecount":8,"score":20})
        );
    }

    use super::{
        ControlFile, MatchResult, SprtState, TicketSource, build_engine_usi_options,
        deterministic_startpos_index, ensure_node_coverage, resolve_engine_nodes, splitmix64,
    };
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    /// peek + commit でチケットを 1 つ発行する（メインループと同じ消費手順）。
    fn pull(source: &mut TicketSource) -> Option<super::MatchTicket> {
        let t = source.peek_ticket()?;
        source.commit_sent(&t);
        Some(t)
    }

    /// 現在の目標値で発行できる分をすべて取り出す。
    fn drain_source(source: &mut TicketSource) -> Vec<super::MatchTicket> {
        let mut out = Vec::new();
        while let Some(t) = pull(source) {
            out.push(t);
        }
        out
    }

    fn result(ticket: super::MatchTicket, outcome: super::GameOutcome, error: bool) -> MatchResult {
        MatchResult {
            ticket,
            outcome,
            reason: String::new(),
            plies: 0,
            move_logs: Vec::new(),
            error,
            worker_retired: false,
        }
    }

    /// pentanomial 整合: 各 pair_index がちょうど 2 回・slot 0/1 で現れ、
    /// その 2 局は同じエンジンペア (min,max) に属すること。
    fn assert_pentanomial_integrity(tickets: &[super::MatchTicket]) {
        use std::collections::HashMap;
        let mut by_pair_index: HashMap<u32, Vec<&super::MatchTicket>> = HashMap::new();
        for t in tickets {
            by_pair_index.entry(t.pair_index).or_default().push(t);
        }
        for (pi, group) in &by_pair_index {
            assert_eq!(group.len(), 2, "pair_index {pi} は 2 局で構成されるべき");
            let mut slots: Vec<u32> = group.iter().map(|t| t.pair_slot).collect();
            slots.sort_unstable();
            assert_eq!(slots, vec![0, 1], "pair_index {pi} の slot は 0,1");
            let norm = |t: &super::MatchTicket| {
                (t.black_idx.min(t.white_idx), t.black_idx.max(t.white_idx))
            };
            assert_eq!(norm(group[0]), norm(group[1]), "pair_index {pi} は同一エンジンペア");
            assert_eq!(
                group[0].startpos_idx, group[1].startpos_idx,
                "pair_index {pi} は同一開始局面"
            );
        }
    }

    #[test]
    fn startpos_selection_is_deterministic_for_seed_matchup_and_local_pair() {
        let sequence = |seed| {
            (0..256)
                .map(|local_pair_index| {
                    deterministic_startpos_index(seed, (0, 1), local_pair_index, 65_521)
                })
                .collect::<Vec<_>>()
        };

        assert_eq!(sequence(42), sequence(42));

        let mut seed_42 = sequence(42);
        let mut seed_43 = sequence(43);
        seed_42.sort_unstable();
        seed_43.sort_unstable();
        assert_ne!(seed_42, seed_43, "隣接 seed の選択多重集合は一致しない");
    }

    #[test]
    fn splitmix64_matches_reference_vectors() {
        assert_eq!(splitmix64(0), 0xE220_A839_7B1D_CDAF);
        assert_eq!(splitmix64(0x9E37_79B9_7F4A_7C15), 0x6E78_9E6A_A1B9_65F4);
    }

    #[test]
    fn worker_panic_notifies_shutdown_before_main_join() {
        let ctx = super::SpawnCtx {
            engine_paths: &[],
            engine_labels: &[],
            engine_usi_options: &[],
            threads: 1,
            hash_mb: 1,
            max_moves: 1,
            adjudicate_resign: None,
            adjudicate_draw: None,
            timeout_margin_ms: 1,
            byoyomi: 0,
            btime: 0,
            binc: 0,
            go_depth: None,
            go_nodes: &[],
            start_defs: &[],
        };
        let (ticket_tx, ticket_rx) = super::chan::bounded(1);
        let (result_tx, _result_rx) = super::chan::bounded(1);
        let shutdown = Arc::new(super::AtomicBool::new(false));
        let failed = Arc::new(super::AtomicBool::new(false));
        let mut handles = Vec::new();
        super::spawn_worker(&ctx, &ticket_rx, &result_tx, &shutdown, &failed, &mut handles);
        // 内部の不正 fixture で安全な slice 境界 panic を起こし、外側の監督経路を検証する。
        ticket_tx
            .send(Some(super::MatchTicket {
                id: 0,
                black_idx: 0,
                white_idx: 1,
                startpos_idx: 0,
                pair_index: 0,
                pair_slot: 0,
                attempt: 0,
            }))
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !shutdown.load(Ordering::Relaxed) && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(shutdown.load(Ordering::Relaxed));
        assert!(failed.load(Ordering::Relaxed));
        handles.pop().unwrap().join().unwrap();
    }

    #[test]
    fn ticket_source_single_pair_emits_expected_pairs() {
        let target = Arc::new(AtomicU32::new(2)); // 各方向 2 局 = 1 ペアあたり 4 局
        let mut source = TicketSource::new(vec![(0, 1)], 1, target.clone(), 0);
        assert_eq!(source.current_target_total(), 4);

        let tickets = drain_source(&mut source);
        assert_eq!(tickets.len(), 4);
        assert!(!source.has_next());
        // id 連番 0..4、pair_index = id/2、pair_slot = id%2
        let ids: Vec<u64> = tickets.iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![0, 1, 2, 3]);
        assert_pentanomial_integrity(&tickets);
        // 偶数 slot は (0,1)、奇数 slot は先後入替 (1,0)
        assert_eq!((tickets[0].black_idx, tickets[0].white_idx), (0, 1));
        assert_eq!((tickets[1].black_idx, tickets[1].white_idx), (1, 0));
    }

    #[test]
    fn ticket_source_target_increase_continues_consistently() {
        let target = Arc::new(AtomicU32::new(1)); // 1 ペアあたり 2 局
        let mut source = TicketSource::new(vec![(0, 1)], 1, target.clone(), 0);
        let first = drain_source(&mut source);
        assert_eq!(first.len(), 2);
        assert!(!source.has_next());

        // 実行中に target を増やす → 追加分を発行できる
        target.store(3, Ordering::Relaxed); // 1 ペアあたり 6 局
        assert!(source.has_next());
        let more = drain_source(&mut source);
        assert_eq!(more.len(), 4); // 6 - 2

        let mut all = first;
        all.extend(more);
        // id は連番のまま、pair_index/slot 整合も維持
        let ids: Vec<u64> = all.iter().map(|t| t.id).collect();
        assert_eq!(ids, (0..6).collect::<Vec<_>>());
        assert_pentanomial_integrity(&all);
    }

    #[test]
    fn ticket_source_target_decrease_stops_feeding() {
        let target = Arc::new(AtomicU32::new(3)); // 1 ペアあたり 6 局
        let mut source = TicketSource::new(vec![(0, 1)], 1, target.clone(), 0);
        // 2 局だけ発行
        assert!(pull(&mut source).is_some());
        assert!(pull(&mut source).is_some());
        // target を発行済みより小さくする → それ以上は発行しない
        target.store(1, Ordering::Relaxed); // 1 ペアあたり 2 局（既に到達）
        assert!(!source.has_next());
        assert!(pull(&mut source).is_none());
    }

    #[test]
    fn ticket_source_round_robin_keeps_pairs_within_engine_pair() {
        let target = Arc::new(AtomicU32::new(1)); // 各ペア 2 局
        let pairs = vec![(0, 1), (0, 2), (1, 2)];
        let mut source = TicketSource::new(pairs.clone(), 1, target.clone(), 0);
        assert_eq!(source.current_target_total(), 6);
        let tickets = drain_source(&mut source);
        assert_eq!(tickets.len(), 6);
        assert_pentanomial_integrity(&tickets);
    }

    #[test]
    fn ticket_source_round_robin_target_increase_backfills_all_pairs() {
        use std::collections::HashMap;
        let target = Arc::new(AtomicU32::new(1)); // 各ペア 2 局
        let pairs = vec![(0, 1), (0, 2), (1, 2)];
        let mut source = TicketSource::new(pairs.clone(), 1, target.clone(), 0);
        let first = drain_source(&mut source);
        assert_eq!(first.len(), 6);

        // 実行中に target を増加 → 全ペアに均等に backfill される。
        target.store(2, Ordering::Relaxed); // 各ペア 4 局
        let more = drain_source(&mut source);
        assert_eq!(more.len(), 6); // 12 - 6

        let mut all = first;
        all.extend(more);
        assert_eq!(all.len(), 12);
        assert_pentanomial_integrity(&all);
        // 各エンジンペアが等しく 4 局ずつ
        let mut per_pair: HashMap<(usize, usize), usize> = HashMap::new();
        for t in &all {
            *per_pair
                .entry((t.black_idx.min(t.white_idx), t.black_idx.max(t.white_idx)))
                .or_default() += 1;
        }
        for p in &pairs {
            assert_eq!(per_pair.get(p), Some(&4), "ペア {p:?} は 4 局");
        }
    }

    #[test]
    fn target_increase_keeps_startpos_sequence_per_matchup() {
        use std::collections::HashMap;

        let pairs = vec![(0, 1), (0, 2), (1, 2)];
        let collect_startpos = |tickets: Vec<super::MatchTicket>| {
            let mut by_matchup: HashMap<(usize, usize), Vec<usize>> = HashMap::new();
            for ticket in tickets {
                if ticket.pair_slot == 0 {
                    let matchup = (
                        ticket.black_idx.min(ticket.white_idx),
                        ticket.black_idx.max(ticket.white_idx),
                    );
                    by_matchup.entry(matchup).or_default().push(ticket.startpos_idx);
                }
            }
            by_matchup
        };

        // まず各方向 1 局を発行し、その後 3 局へ増やす。
        let dynamic_target = Arc::new(AtomicU32::new(1));
        let mut dynamic = TicketSource::new(pairs.clone(), 65_521, dynamic_target.clone(), 42);
        let mut dynamic_tickets = drain_source(&mut dynamic);
        dynamic_target.store(3, Ordering::Relaxed);
        dynamic_tickets.extend(drain_source(&mut dynamic));

        // 最初から最終 target を指定した場合と、matchup ごとの局面列が一致する。
        let fixed_target = Arc::new(AtomicU32::new(3));
        let mut fixed = TicketSource::new(pairs, 65_521, fixed_target, 42);
        let fixed_tickets = drain_source(&mut fixed);

        assert_eq!(collect_startpos(dynamic_tickets), collect_startpos(fixed_tickets));
    }

    #[test]
    fn ticket_source_decrease_mid_pair_completes_pair() {
        // ペアの 1 局目だけ発行した状態で target を下げても、2 局目を発行してペアを完結させる。
        let target = Arc::new(AtomicU32::new(3)); // 1 ペアあたり 6 局
        let mut source = TicketSource::new(vec![(0, 1)], 1, target.clone(), 0);
        let slot0 = pull(&mut source).unwrap();
        assert_eq!(slot0.pair_slot, 0);

        // 発行済み (=1, 奇数) より小さい target に下げる。
        target.store(0, Ordering::Relaxed);
        // 奇数 emitted のため 2 局目はまだ発行できる。
        assert!(source.has_next());
        let slot1 = pull(&mut source).unwrap();
        assert_eq!(slot1.pair_slot, 1);
        assert_eq!(slot0.pair_index, slot1.pair_index);
        // ペアが完結したので以降は発行しない。
        assert!(!source.has_next());
        assert!(pull(&mut source).is_none());
        assert_pentanomial_integrity(&[slot0, slot1]);
    }

    #[test]
    fn ticket_source_peek_does_not_advance_state() {
        let target = Arc::new(AtomicU32::new(2));
        let mut source = TicketSource::new(vec![(0, 1)], 1, target.clone(), 0);
        let a = source.peek_ticket().unwrap();
        let b = source.peek_ticket().unwrap();
        assert_eq!(a.id, b.id, "peek は状態を進めない");
        source.commit_sent(&a);
        let c = source.peek_ticket().unwrap();
        assert_eq!(c.id, a.id + 1, "commit 後は次の id");
    }

    #[test]
    fn ticket_source_still_wanted_reflects_target_decrease() {
        let target = Arc::new(AtomicU32::new(2)); // 各ペア 4 局
        let mut source = TicketSource::new(vec![(0, 1)], 1, target.clone(), 0);

        // 未コミットの 1 局目（偶数 slot）は、target を下げると不要になる。
        let fresh = source.peek_ticket().unwrap();
        assert_eq!(fresh.pair_slot, 0);
        assert!(source.still_wanted(&fresh));
        target.store(0, Ordering::Relaxed);
        assert!(!source.still_wanted(&fresh), "target=0 では未送信の 1 局目は破棄対象");

        // 1 局目 commit 済みで 2 局目（奇数 slot）は target=0 でも常に送る（ペア完結）。
        target.store(2, Ordering::Relaxed);
        let slot0 = pull(&mut source).unwrap();
        let slot1 = source.peek_ticket().unwrap();
        assert_eq!(slot1.pair_slot, 1);
        assert_eq!(slot0.pair_index, slot1.pair_index);
        target.store(0, Ordering::Relaxed);
        assert!(source.still_wanted(&slot1), "2 局目はペア完結のため常に送る");
    }

    #[test]
    fn apply_control_applies_changes_and_records_history() {
        use super::{ControlState, apply_control};
        let dir = tempfile::tempdir().unwrap();
        let control_path = dir.path().join("control.json");
        let history_path = dir.path().join("control_history.jsonl");

        let target = AtomicU32::new(100);
        let mut desired = 4usize;
        let mut applied = ControlState {
            target_games: 100,
            concurrency: 4,
        };

        // ファイル不在 → 何もしない（履歴も作られない）。
        apply_control(&control_path, &history_path, &mut applied, &target, &mut desired, 0);
        assert!(!history_path.exists());

        // target と concurrency を変更。
        std::fs::write(&control_path, r#"{"target_games": 150, "concurrency": 8}"#).unwrap();
        apply_control(&control_path, &history_path, &mut applied, &target, &mut desired, 10);
        assert_eq!(target.load(Ordering::Relaxed), 150);
        assert_eq!(desired, 8);
        assert_eq!(applied.target_games, 150);
        assert_eq!(applied.concurrency, 8);
        let history = std::fs::read_to_string(&history_path).unwrap();
        assert_eq!(history.lines().count(), 1, "変更 1 件で履歴 1 行");

        // 同じ内容の再ポーリングは no-op（履歴が増えない）。
        apply_control(&control_path, &history_path, &mut applied, &target, &mut desired, 20);
        let history = std::fs::read_to_string(&history_path).unwrap();
        assert_eq!(history.lines().count(), 1, "差分なしでは履歴が増えない");

        // concurrency=0 は無視され、target だけ反映される。
        std::fs::write(&control_path, r#"{"target_games": 200, "concurrency": 0}"#).unwrap();
        apply_control(&control_path, &history_path, &mut applied, &target, &mut desired, 30);
        assert_eq!(target.load(Ordering::Relaxed), 200);
        assert_eq!(desired, 8, "concurrency=0 は無視");

        // 壊れた JSON は無視され、実行継続（既存値も維持）。
        std::fs::write(&control_path, "{ not json").unwrap();
        apply_control(&control_path, &history_path, &mut applied, &target, &mut desired, 40);
        assert_eq!(target.load(Ordering::Relaxed), 200);
    }

    #[test]
    fn ticket_source_swap_game_reuses_start_position() {
        // 複数開始局面でも、ペアの 2 局目は 1 局目と同じ開始局面を使う。
        let target = Arc::new(AtomicU32::new(1));
        let mut source = TicketSource::new(vec![(0, 1)], 8, target.clone(), 0);
        let tickets = drain_source(&mut source);
        assert_eq!(tickets.len(), 2);
        assert_eq!(
            tickets[0].startpos_idx, tickets[1].startpos_idx,
            "先後入替の 2 局目は同一開始局面"
        );
    }

    #[test]
    fn error_pair_retries_twice_without_consuming_target() {
        let target = Arc::new(AtomicU32::new(1));
        let mut source = TicketSource::new(vec![(0, 1)], 8, target, 42);
        let initial = drain_source(&mut source);
        assert_eq!(initial.len(), 2);
        let startpos_idx = initial[0].startpos_idx;

        source.observe_result(&result(initial[0].clone(), super::GameOutcome::Draw, true));
        source.observe_result(&result(initial[1].clone(), super::GameOutcome::Draw, false));
        let attempt_1 = vec![pull(&mut source).unwrap(), pull(&mut source).unwrap()];
        assert_eq!(attempt_1.iter().map(|t| t.attempt).collect::<Vec<_>>(), vec![1, 1]);
        assert_eq!(attempt_1.iter().map(|t| t.pair_slot).collect::<Vec<_>>(), vec![0, 1]);
        assert!(attempt_1.iter().all(|t| t.pair_index == initial[0].pair_index));
        assert!(attempt_1.iter().all(|t| t.startpos_idx == startpos_idx));
        assert_eq!(
            attempt_1.iter().map(|t| (t.black_idx, t.white_idx)).collect::<Vec<_>>(),
            initial.iter().map(|t| (t.black_idx, t.white_idx)).collect::<Vec<_>>()
        );

        for ticket in attempt_1 {
            source.observe_result(&result(ticket, super::GameOutcome::Draw, true));
        }
        let attempt_2 = vec![pull(&mut source).unwrap(), pull(&mut source).unwrap()];
        assert!(attempt_2.iter().all(|t| t.attempt == 2));
        for ticket in attempt_2 {
            source.observe_result(&result(ticket, super::GameOutcome::Draw, true));
        }

        assert!(!source.has_next());
        assert_eq!(source.current_target_total(), 2);
        let summary = source.retry_summary();
        assert_eq!(summary.error_pairs, 1);
        assert_eq!(summary.retried_pairs, 1);
        assert_eq!(summary.exhausted_pairs, 1);
        assert!(summary.invalid);
    }

    #[test]
    fn live_sprt_pairs_only_within_attempt() {
        let params = super::SprtParameters::new(0.0, 5.0, 0.05, 0.05).unwrap();
        let mut state = SprtState::new(params, 0, 1, 10, "base".into(), "test".into());
        let target = Arc::new(AtomicU32::new(1));
        let mut source = TicketSource::new(vec![(0, 1)], 1, target, 0);
        let initial = drain_source(&mut source);
        state.observe(&result(initial[0].clone(), super::GameOutcome::WhiteWin, false));
        state.observe(&result(initial[1].clone(), super::GameOutcome::Draw, true));
        source.observe_result(&result(initial[0].clone(), super::GameOutcome::WhiteWin, false));
        source.observe_result(&result(initial[1].clone(), super::GameOutcome::Draw, true));
        assert_eq!(state.penta, super::Penta::ZERO);

        let retry = [pull(&mut source).unwrap(), pull(&mut source).unwrap()];
        state.observe(&result(retry[0].clone(), super::GameOutcome::WhiteWin, false));
        state.observe(&result(retry[1].clone(), super::GameOutcome::BlackWin, false));
        assert_eq!(
            state.penta,
            super::Penta {
                ww: 1,
                ..super::Penta::ZERO
            }
        );
    }

    /// 同一結果列を live と JSONL post-hoc に流し、ペアリング規則の一致を直接検証する。
    #[test]
    fn live_and_posthoc_penta_match_for_retry_duplicate_and_incomplete_input() {
        use std::io::Write as _;

        fn ticket(pair_index: u32, pair_slot: u32, attempt: u32) -> super::MatchTicket {
            let (black_idx, white_idx) = if pair_slot == 0 { (0, 1) } else { (1, 0) };
            super::MatchTicket {
                id: 0,
                black_idx,
                white_idx,
                startpos_idx: pair_index as usize,
                pair_index,
                pair_slot,
                attempt,
            }
        }

        let cases = [
            (0, 0, 0, super::GameOutcome::WhiteWin, false),
            (0, 1, 0, super::GameOutcome::BlackWin, false),
            (1, 0, 0, super::GameOutcome::WhiteWin, false),
            (1, 1, 0, super::GameOutcome::Draw, true),
            (1, 0, 1, super::GameOutcome::Draw, false),
            (1, 1, 1, super::GameOutcome::Draw, false),
            (2, 0, 0, super::GameOutcome::BlackWin, false),
            // 同じ slot の 2 行目は、先の Loss を上書きせず除外する。
            (2, 0, 0, super::GameOutcome::WhiteWin, false),
            (2, 1, 0, super::GameOutcome::WhiteWin, false),
            // 未完了ペア。
            (3, 0, 0, super::GameOutcome::WhiteWin, false),
        ];

        let params = super::SprtParameters::new(0.0, 5.0, 0.05, 0.05).unwrap();
        let mut live = SprtState::new(params, 0, 1, 10, "base".into(), "test".into());
        for &(pair_index, pair_slot, attempt, outcome, error) in &cases {
            live.observe(&result(ticket(pair_index, pair_slot, attempt), outcome, error));
        }

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shared.jsonl");
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(
            file,
            r#"{{"type":"meta","engine_cmd":{{"path_black":"/base","path_white":"/test","label_black":"base","label_white":"test"}}}}"#
        )
        .unwrap();
        let rows = [
            r#"{"type":"result","outcome":"white_win","winner":"test","pair_index":0,"pair_slot":0}"#,
            r#"{"type":"result","outcome":"black_win","winner":"test","pair_index":0,"pair_slot":1}"#,
            r#"{"type":"result","outcome":"white_win","winner":"test","pair_index":1,"pair_slot":0}"#,
            r#"{"type":"result","outcome":"draw","pair_index":1,"pair_slot":1,"error":true}"#,
            r#"{"type":"result","outcome":"draw","pair_index":1,"pair_slot":0,"attempt":1}"#,
            r#"{"type":"result","outcome":"draw","pair_index":1,"pair_slot":1,"attempt":1}"#,
            r#"{"type":"result","outcome":"black_win","winner":"base","pair_index":2,"pair_slot":0}"#,
            r#"{"type":"result","outcome":"white_win","winner":"test","pair_index":2,"pair_slot":0}"#,
            r#"{"type":"result","outcome":"white_win","winner":"base","pair_index":2,"pair_slot":1}"#,
            r#"{"type":"result","outcome":"white_win","winner":"test","pair_index":3,"pair_slot":0}"#,
        ];
        for row in rows {
            writeln!(file, "{row}").unwrap();
        }
        drop(file);

        let posthoc =
            tools::sprt::collect_sprt_penta(path.to_str().unwrap(), "base", "test").unwrap();
        assert_eq!(live.penta, posthoc);
        assert_eq!(
            posthoc,
            super::Penta {
                ll: 1,
                dd: 1,
                ww: 1,
                ..super::Penta::ZERO
            }
        );
    }

    #[test]
    fn control_file_parses_partial_fields() {
        let only_target: ControlFile = serde_json::from_str(r#"{"target_games": 5}"#).unwrap();
        assert_eq!(only_target.target_games, Some(5));
        assert_eq!(only_target.concurrency, None);

        let both: ControlFile =
            serde_json::from_str(r#"{"target_games": 12, "concurrency": 8}"#).unwrap();
        assert_eq!(both.target_games, Some(12));
        assert_eq!(both.concurrency, Some(8));

        let empty: ControlFile = serde_json::from_str("{}").unwrap();
        assert_eq!(empty.target_games, None);
        assert_eq!(empty.concurrency, None);
    }

    #[test]
    fn engine_nodes_overrides_global_per_engine_with_fallback() {
        let specs = strings(&["0:2000", "2:8000"]);
        let resolved = resolve_engine_nodes(3, Some(5000), Some(&specs)).unwrap();
        // index 0/2 は個別指定、index 1 は global --nodes にフォールバック。
        assert_eq!(resolved, vec![Some(2000), Some(5000), Some(8000)]);
    }

    #[test]
    fn engine_nodes_without_global_leaves_unspecified_engines_unlimited() {
        let specs = strings(&["1:4000"]);
        let resolved = resolve_engine_nodes(2, None, Some(&specs)).unwrap();
        assert_eq!(resolved, vec![None, Some(4000)]);
    }

    #[test]
    fn engine_nodes_rejects_out_of_range_index() {
        let specs = strings(&["2:1000"]);
        assert!(resolve_engine_nodes(2, None, Some(&specs)).is_err());
    }

    #[test]
    fn engine_nodes_rejects_malformed_spec() {
        assert!(resolve_engine_nodes(2, None, Some(&strings(&["0-1000"]))).is_err());
        assert!(resolve_engine_nodes(2, None, Some(&strings(&["x:1000"]))).is_err());
        assert!(resolve_engine_nodes(2, None, Some(&strings(&["0:notanumber"]))).is_err());
        // ノード数 0 は停止条件にならないため弾く。
        assert!(resolve_engine_nodes(2, None, Some(&strings(&["0:0"]))).is_err());
    }

    #[test]
    fn engine_nodes_duplicate_index_last_wins() {
        let specs = strings(&["0:1000", "0:2000"]);
        let resolved = resolve_engine_nodes(2, None, Some(&specs)).unwrap();
        assert_eq!(resolved, vec![Some(2000), None]);
    }

    #[test]
    fn ensure_node_coverage_requires_budget_only_when_no_time_or_depth() {
        // 時間制御も depth も無く、予算のないエンジン (None) があるとエラー。
        assert!(ensure_node_coverage(&[Some(1000), None], false, false).is_err());
        // 全エンジンに予算があれば OK。
        assert!(ensure_node_coverage(&[Some(1000), Some(2000)], false, false).is_ok());
        // 時間制御 or depth があれば None でも OK (未指定エンジンはそちらで停止する)。
        assert!(ensure_node_coverage(&[Some(1000), None], true, false).is_ok());
        assert!(ensure_node_coverage(&[None, None], false, true).is_ok());
    }

    #[test]
    fn engine_usi_option_merges_with_common_and_overwrites_same_key() {
        let common = strings(&["EvalFile=common.bin", "Threads=2", "BookFile=no_book"]);
        let per_engine = strings(&["EvalFile=engine.bin", "NetworkDelay=0"]);

        let merged = build_engine_usi_options(&common, Some(per_engine), false);

        assert_eq!(
            merged,
            strings(&[
                "Threads=2",
                "BookFile=no_book",
                "EvalFile=engine.bin",
                "NetworkDelay=0",
            ])
        );
    }

    #[test]
    fn strict_engine_usi_option_replaces_common_options() {
        let common = strings(&["EvalFile=common.bin", "Threads=2"]);
        let per_engine = strings(&["EvalDir=/path/to/eval"]);

        let merged = build_engine_usi_options(&common, Some(per_engine), true);

        assert_eq!(merged, strings(&["EvalDir=/path/to/eval"]));
    }

    #[test]
    fn duplicated_per_engine_option_uses_last_value() {
        let common = strings(&["EvalFile=common.bin", "Threads=2"]);
        let per_engine = strings(&["EvalFile=first.bin", "EvalFile=last.bin"]);

        let merged = build_engine_usi_options(&common, Some(per_engine), false);

        assert_eq!(merged, strings(&["Threads=2", "EvalFile=last.bin"]));
    }
}
