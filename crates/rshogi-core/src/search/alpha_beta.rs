//! Alpha-Beta探索の実装
//!
//! YaneuraOu準拠のAlpha-Beta探索。
//! - Principal Variation Search (PVS)
//! - 静止探索 (Quiescence Search)
//! - 各種枝刈り: NMP, LMR, Futility, Razoring, SEE, Singular Extension

use std::ptr::NonNull;
use std::sync::Arc;

use crate::eval::{get_scaled_pass_move_bonus, EvalHash};
use crate::nnue::{get_network, AccumulatorStackVariant, DirtyPiece};
use crate::position::Position;
use crate::search::PieceToHistory;
use crate::tt::{ProbeResult, TTData, TranspositionTable};
use crate::types::{Bound, Color, Depth, Move, Piece, PieceType, Square, Value, DEPTH_QS, MAX_PLY};

use super::history::{
    capture_malus, continuation_history_bonus_with_offset, low_ply_history_bonus,
    pawn_history_bonus, quiet_malus, stat_bonus, HistoryCell, CONTINUATION_HISTORY_NEAR_PLY_OFFSET,
    CONTINUATION_HISTORY_WEIGHTS, CORRECTION_HISTORY_LIMIT, LOW_PLY_HISTORY_SIZE,
    PRIOR_CAPTURE_COUNTERMOVE_BONUS, TT_MOVE_HISTORY_BONUS, TT_MOVE_HISTORY_MALUS,
};
use super::movepicker::piece_value;
use super::types::{
    init_stack_array, value_to_tt, ContHistKey, NodeType, RootMoves, SearchedMoveList, StackArray,
    STACK_SIZE,
};
use super::{LimitsType, MovePicker, TimeManagement};

use super::eval_helpers::{compute_eval_context, probe_transposition, update_correction_history};
use super::pruning::{
    step14_pruning, try_futility_pruning, try_null_move_pruning, try_probcut, try_razoring,
    try_small_probcut,
};
use super::qsearch::qsearch;
use super::search_helpers::{
    check_abort, clear_cont_history_for_null, cont_history_ref, cont_history_tables, nnue_evaluate,
    nnue_pop, nnue_push, set_cont_history_for_move, take_prior_reduction,
};

// =============================================================================
// 定数
// =============================================================================

/// IIR関連のしきい値（yaneuraou-search.cpp:2769-2774 由来）
const IIR_PRIOR_REDUCTION_THRESHOLD_SHALLOW: i32 = 1;
const IIR_PRIOR_REDUCTION_THRESHOLD_DEEP: i32 = 3;
const IIR_DEPTH_BOUNDARY: Depth = 10;
const IIR_EVAL_SUM_THRESHOLD: i32 = 177;

use std::sync::LazyLock;

/// 引き分けスコアに揺らぎを与える（YaneuraOu準拠）
const DRAW_JITTER_MASK: u64 = 0x2;
const DRAW_JITTER_OFFSET: i32 = -1; // VALUE_DRAW(0) 周辺に ±1 の揺らぎを入れる

#[inline]
pub(super) fn draw_jitter(nodes: u64) -> i32 {
    // YaneuraOu: value_draw(nodes) = VALUE_DRAW - 1 + (nodes & 0x2)
    // 千日手盲点を避けるため、VALUE_DRAW(0) を ±1 にばらつかせる。
    ((nodes & DRAW_JITTER_MASK) as i32) + DRAW_JITTER_OFFSET
}

/// depthに対するmsb（YaneuraOuのlm r補正用）
#[inline]
fn msb(x: i32) -> i32 {
    if x <= 0 {
        return 0;
    }
    31 - x.leading_zeros() as i32
}

/// 補正履歴を適用した静的評価に変換（詰みスコア領域に入り込まないようにクリップ）
#[inline]
pub(super) fn to_corrected_static_eval(unadjusted: Value, correction_value: i32) -> Value {
    let corrected = unadjusted.raw() + correction_value / 131_072;
    Value::new(corrected.clamp(Value::MATED_IN_MAX_PLY.raw() + 1, Value::MATE_IN_MAX_PLY.raw() - 1))
}

/// LMR用のreduction配列
type Reductions = [i32; 64];

const REDUCTION_DELTA_SCALE: i32 = 757;
const REDUCTION_NON_IMPROVING_MULT: i32 = 218;
const REDUCTION_NON_IMPROVING_DIV: i32 = 512;
const REDUCTION_BASE_OFFSET: i32 = 1200;

/// Reduction配列（LazyLockによる遅延初期化）
/// 初回アクセス時に自動初期化されるため、get()呼び出しが不要
static REDUCTIONS: LazyLock<Reductions> = LazyLock::new(|| {
    let mut table: Reductions = [0; 64];
    for (i, value) in table.iter_mut().enumerate().skip(1) {
        *value = (2782.0 / 128.0 * (i as f64).ln()) as i32;
    }
    table
});

/// Reductionを取得
///
/// LazyLockにより初回アクセス時に自動初期化されるため、panicしない。
#[inline]
pub(crate) fn reduction(
    imp: bool,
    depth: i32,
    move_count: i32,
    delta: i32,
    root_delta: i32,
) -> i32 {
    if depth <= 0 || move_count <= 0 {
        return 0;
    }

    let d = depth.clamp(1, 63) as usize;
    let mc = move_count.clamp(1, 63) as usize;
    // LazyLockにより直接アクセス可能（get()不要）
    let reduction_scale = REDUCTIONS[d] * REDUCTIONS[mc];
    let root_delta = root_delta.max(1);
    let delta = delta.max(0);

    // 1024倍スケールで返す。ttPv加算は呼び出し側で行う。
    reduction_scale - delta * REDUCTION_DELTA_SCALE / root_delta
        + (!imp as i32) * reduction_scale * REDUCTION_NON_IMPROVING_MULT
            / REDUCTION_NON_IMPROVING_DIV
        + REDUCTION_BASE_OFFSET
}

// stats モジュールからマクロをインポート
use super::stats::{inc_stat, inc_stat_by_depth};
#[cfg(feature = "search-stats")]
use super::stats::{SearchStats, STATS_MAX_DEPTH};

/// 置換表プローブの結果をまとめたコンテキスト
///
/// TTプローブ後の即時カットオフ判定や、後続の枝刈りロジックで使用される。
pub(super) struct TTContext {
    pub(super) key: u64,
    pub(super) result: ProbeResult,
    pub(super) data: TTData,
    pub(super) hit: bool,
    pub(super) mv: Move,
    pub(super) value: Value,
    pub(super) capture: bool,
}

/// 置換表プローブの結果（続行 or カットオフ）
pub(super) enum ProbeOutcome {
    /// 探索続行（TTContext付き）
    Continue(TTContext),
    /// 即時カットオフ値
    Cutoff(Value),
}

/// 静的評価まわりの情報をまとめたコンテキスト
pub(super) struct EvalContext {
    pub(super) static_eval: Value,
    pub(super) unadjusted_static_eval: Value,
    pub(super) correction_value: i32,
    /// 2手前と比較して局面が改善しているか
    pub(super) improving: bool,
    /// 相手側の局面が悪化しているか
    pub(super) opponent_worsening: bool,
}

/// Step14の枝刈り判定結果
pub(super) enum Step14Outcome {
    /// 枝刈りする（best_value を更新する場合のみ付随）
    Skip { best_value: Option<Value> },
    /// 続行し、lmr_depth を返す
    Continue,
}

/// Futility判定に必要な情報をまとめたパラメータ
#[derive(Clone, Copy)]
pub(super) struct FutilityParams {
    pub(super) depth: Depth,
    pub(super) beta: Value,
    pub(super) static_eval: Value,
    pub(super) correction_value: i32,
    pub(super) improving: bool,
    pub(super) opponent_worsening: bool,
    pub(super) tt_hit: bool,
    pub(super) tt_move_exists: bool, // TT に手が保存されているか
    pub(super) tt_capture: bool,     // TT の手が駒取りか
    pub(super) pv_node: bool,
    pub(super) in_check: bool,
}

/// Step14 の枝刈りに必要な文脈
pub(super) struct Step14Context<'a> {
    pub(super) pos: &'a Position,
    pub(super) mv: Move,
    pub(super) depth: Depth,
    pub(super) ply: i32,
    pub(super) best_value: Value,
    pub(super) in_check: bool,
    pub(super) gives_check: bool,
    pub(super) is_capture: bool,
    pub(super) lmr_depth: i32,
    pub(super) mover: Color,
    pub(super) cont_history_1: &'a PieceToHistory,
    pub(super) cont_history_2: &'a PieceToHistory,
    pub(super) static_eval: Value,
    pub(super) alpha: Value,
    pub(super) tt_move: Move,             // bestMove判定用
    pub(super) pawn_history_index: usize, // pawnHistory用インデックス
}

// =============================================================================
// SearchContext / SearchState
// =============================================================================

/// 探索中に変化しない共有データ
///
/// 探索の各ノードで共有される不変の参照群。
/// TimeManagement と LimitsType は可変アクセスが必要なため、別途引数として渡す。
pub struct SearchContext<'a> {
    /// 置換表への参照
    pub tt: &'a TranspositionTable,
    /// 評価ハッシュへの参照
    pub eval_hash: &'a EvalHash,
    /// 履歴テーブルへの参照（HistoryCell 経由でアクセス）
    pub history: &'a HistoryCell,
    /// ContinuationHistoryのsentinel
    pub cont_history_sentinel: NonNull<PieceToHistory>,
    /// 全合法手生成フラグ
    pub generate_all_legal_moves: bool,
    /// 引き分けまでの最大手数
    pub max_moves_to_draw: i32,
    /// スレッドID（0=main）
    pub thread_id: usize,
}

/// 探索中に変化する状態
///
/// 各探索スレッドが持つ可変状態。
pub struct SearchState {
    /// 探索ノード数
    pub nodes: u64,
    /// 探索スタック
    pub stack: StackArray,
    /// ルートでのウィンドウ幅（beta - alpha）。LMRスケール用。
    pub root_delta: i32,
    /// 中断フラグ
    pub abort: bool,
    /// 選択的深さ
    pub sel_depth: i32,
    /// ルート深さ
    pub root_depth: Depth,
    /// 完了済み深さ
    pub completed_depth: Depth,
    /// 最善手
    pub best_move: Move,
    /// 最善手変更カウンター（PV安定性判断用）
    pub best_move_changes: f64,
    /// Null Move Pruning の Verification Search 用フラグ
    pub nmp_min_ply: i32,
    /// ルート手
    pub root_moves: RootMoves,
    /// NNUE Accumulator スタック
    pub nnue_stack: AccumulatorStackVariant,
    /// check_abort呼び出しカウンター
    pub calls_cnt: i32,
    /// 探索統計（search-stats feature有効時のみ）
    #[cfg(feature = "search-stats")]
    pub stats: SearchStats,
}

impl SearchState {
    /// 新しい SearchState を作成
    pub fn new() -> Self {
        Self {
            nodes: 0,
            stack: init_stack_array(),
            root_delta: 1,
            abort: false,
            sel_depth: 0,
            root_depth: 0,
            completed_depth: 0,
            best_move: Move::NONE,
            best_move_changes: 0.0,
            nmp_min_ply: 0,
            root_moves: RootMoves::new(),
            nnue_stack: AccumulatorStackVariant::new_default(),
            calls_cnt: 0,
            #[cfg(feature = "search-stats")]
            stats: SearchStats::default(),
        }
    }
}

impl Default for SearchState {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// SearchWorker
// =============================================================================

/// 探索用のワーカー状態
///
/// Workerはゲーム全体で再利用される。
/// 履歴統計は直接メンバとして保持し、usinewgameでクリア、goでは保持。
///
/// SearchContext（不変データ）と SearchState（可変状態）に分離された設計。
/// - Context用フィールド: tt, eval_hash, history, cont_history_sentinel, generate_all_legal_moves, max_moves_to_draw, thread_id
/// - State: 探索中に変化するフィールドを SearchState として保持
pub struct SearchWorker {
    // =========================================================================
    // Context用フィールド（探索中に変化しない）
    // =========================================================================
    /// 置換表への共有参照（Arc）
    pub tt: Arc<TranspositionTable>,

    /// 評価ハッシュへの共有参照（Arc）
    pub eval_hash: Arc<EvalHash>,

    /// 履歴/統計テーブル群（HistoryCell 経由でアクセス）
    pub history: Box<HistoryCell>,

    /// ContinuationHistoryのsentinel
    pub cont_history_sentinel: NonNull<PieceToHistory>,

    /// 全合法手生成フラグ
    pub generate_all_legal_moves: bool,

    /// 引き分けまでの最大手数
    pub max_moves_to_draw: i32,

    /// スレッドID（0=main）
    pub thread_id: usize,

    // =========================================================================
    // 探索状態（SearchState）
    // =========================================================================
    /// 探索中に変化する状態
    pub state: SearchState,
}

impl SearchWorker {
    /// 新しいSearchWorkerを作成（YaneuraOu準拠: isreadyまたは最初のgo時）
    ///
    /// Box化してヒープに配置し、スタックオーバーフローを防ぐ。
    pub fn new(
        tt: Arc<TranspositionTable>,
        eval_hash: Arc<EvalHash>,
        max_moves_to_draw: i32,
        thread_id: usize,
    ) -> Box<Self> {
        let history = HistoryCell::new_boxed();
        // HistoryCell経由でsentinelポインタを取得
        let cont_history_sentinel = history.with_read(|h| {
            NonNull::from(h.continuation_history[0][0].get_table(Piece::NONE, Square::SQ_11))
        });

        let mut worker = Box::new(Self {
            tt,
            eval_hash,
            history,
            cont_history_sentinel,
            generate_all_legal_moves: false,
            max_moves_to_draw,
            thread_id,
            state: SearchState::new(),
        });
        worker.reset_cont_history_ptrs();
        worker
    }

    /// SearchContext を作成
    ///
    /// 探索中に変化しない共有データへの参照をまとめる。
    #[inline]
    pub fn create_context(&self) -> SearchContext<'_> {
        SearchContext {
            tt: &self.tt,
            eval_hash: &self.eval_hash,
            history: &self.history,
            cont_history_sentinel: self.cont_history_sentinel,
            generate_all_legal_moves: self.generate_all_legal_moves,
            max_moves_to_draw: self.max_moves_to_draw,
            thread_id: self.thread_id,
        }
    }

    /// SearchState への可変参照を取得
    #[inline]
    pub fn state_mut(&mut self) -> &mut SearchState {
        &mut self.state
    }

    /// SearchState への参照を取得
    #[inline]
    pub fn state(&self) -> &SearchState {
        &self.state
    }

    /// 探索統計をリセット（search-stats feature有効時のみ）
    #[cfg(feature = "search-stats")]
    pub fn reset_stats(&mut self) {
        self.state.stats.reset();
    }

    /// 探索統計をリセット（search-stats feature無効時はno-op）
    #[cfg(not(feature = "search-stats"))]
    pub fn reset_stats(&mut self) {}

    /// 探索統計のレポートを取得（search-stats feature有効時のみ）
    #[cfg(feature = "search-stats")]
    pub fn get_stats_report(&self) -> String {
        self.state.stats.format_report()
    }

    /// 探索統計のレポートを取得（search-stats feature無効時は空文字列）
    #[cfg(not(feature = "search-stats"))]
    pub fn get_stats_report(&self) -> String {
        String::new()
    }

    fn reset_cont_history_ptrs(&mut self) {
        let sentinel = self.cont_history_sentinel;
        for stack in self.state.stack.iter_mut() {
            stack.cont_history_ptr = sentinel;
        }
    }

    #[inline]
    pub(super) fn set_cont_history_for_move(
        &mut self,
        ply: i32,
        in_check: bool,
        capture: bool,
        piece: Piece,
        to: Square,
    ) {
        debug_assert!(ply >= 0 && (ply as usize) < STACK_SIZE, "ply out of bounds: {ply}");
        let in_check_idx = in_check as usize;
        let capture_idx = capture as usize;
        let table = self.history.with_read(|h| {
            NonNull::from(h.continuation_history[in_check_idx][capture_idx].get_table(piece, to))
        });
        self.state.stack[ply as usize].cont_history_ptr = table;
        self.state.stack[ply as usize].cont_hist_key =
            Some(ContHistKey::new(in_check, capture, piece, to));
    }

    #[inline]
    pub(super) fn clear_cont_history_for_null(&mut self, ply: i32) {
        self.state.stack[ply as usize].cont_history_ptr = self.cont_history_sentinel;
        self.state.stack[ply as usize].cont_hist_key = None;
    }

    /// usinewgameで呼び出し：全履歴をクリア（YaneuraOu Worker::clear()相当）
    pub fn clear(&mut self) {
        self.history.clear();
    }

    /// goで呼び出し：探索状態のリセット（履歴はクリアしない、YaneuraOu準拠）
    pub fn prepare_search(&mut self) {
        self.state.nodes = 0;
        self.state.sel_depth = 0;
        self.state.root_depth = 0;
        self.state.root_delta = 1;
        self.state.completed_depth = 0;
        self.state.best_move = Move::NONE;
        self.state.abort = false;
        self.state.best_move_changes = 0.0;
        self.state.nmp_min_ply = 0;
        self.state.root_moves.clear();
        // 探索統計をリセット（1回のgo毎にリセット）
        self.reset_stats();
        // YaneuraOu準拠: low_ply_historyのみクリア
        self.history.with_write(|h| h.low_ply_history.clear());
        // NNUE AccumulatorStack: ネットワークに応じたバリアントに更新・リセット
        if let Some(network) = get_network() {
            // バリアントがネットワークと一致しない場合は再作成
            if !self.state.nnue_stack.matches_network(network) {
                self.state.nnue_stack = AccumulatorStackVariant::from_network(network);
            } else {
                self.state.nnue_stack.reset();
            }
        } else {
            // NNUE未初期化の場合はデフォルト（HalfKP）でリセット
            self.state.nnue_stack.reset();
        }
        // check_abort頻度制御カウンターをリセット
        // これにより新しい探索開始時に即座に停止チェックが行われる
        self.state.calls_cnt = 0;
    }

    /// best_move_changes を半減（世代減衰）
    ///
    /// YaneuraOu準拠: 反復深化の各世代終了時に呼び出して、
    /// 古い情報の重みを低くする
    pub fn decay_best_move_changes(&mut self) {
        self.state.best_move_changes /= 2.0;
    }

    /// 全合法手生成モードの設定（YaneuraOu互換）
    pub fn set_generate_all_legal_moves(&mut self, flag: bool) {
        self.generate_all_legal_moves = flag;
    }

    // =========================================================================
    // NNUE ヘルパーメソッド（LayerStacks / HalfKP・HalfKA_hm の分岐を隠蔽）
    // =========================================================================

    /// NNUE アキュムレータスタックを push
    #[inline]
    pub(super) fn nnue_push(&mut self, dirty_piece: DirtyPiece) {
        self.state.nnue_stack.push(dirty_piece);
    }

    /// NNUE アキュムレータスタックを pop
    #[inline]
    pub(super) fn nnue_pop(&mut self) {
        self.state.nnue_stack.pop();
    }

    /// 中断チェック
    /// YaneuraOu準拠: 512回に1回だけ実際のチェックを行う
    #[inline]
    pub(super) fn check_abort(
        &mut self,
        limits: &LimitsType,
        time_manager: &mut TimeManagement,
    ) -> bool {
        // すでにabortフラグが立っている場合は即座に返す
        if self.state.abort {
            #[cfg(debug_assertions)]
            eprintln!("check_abort: abort flag already set");
            return true;
        }

        // 頻度制御：512回に1回だけ実際のチェックを行う（YaneuraOu準拠）
        self.state.calls_cnt -= 1;
        if self.state.calls_cnt > 0 {
            return false;
        }
        // カウンターをリセット
        self.state.calls_cnt = if limits.nodes > 0 {
            std::cmp::min(512, (limits.nodes / 1024) as i32).max(1)
        } else {
            512
        };

        // 外部からの停止要求
        if time_manager.stop_requested() {
            #[cfg(debug_assertions)]
            eprintln!("check_abort: stop requested");
            self.state.abort = true;
            return true;
        }

        // ノード数制限チェック
        if limits.nodes > 0 && self.state.nodes >= limits.nodes {
            #[cfg(debug_assertions)]
            eprintln!(
                "check_abort: node limit reached nodes={} limit={}",
                self.state.nodes, limits.nodes
            );
            self.state.abort = true;
            return true;
        }

        // 時間制限チェック（main threadのみ）
        // YaneuraOu準拠の2フェーズロジック
        if self.thread_id == 0 {
            // ponderhit フラグをポーリングし、検知したら通常探索へ切り替える
            if time_manager.take_ponderhit() {
                time_manager.on_ponderhit();
            }

            let elapsed = time_manager.elapsed();
            let elapsed_effective = time_manager.elapsed_from_ponderhit();

            // フェーズ1: search_end 設定済み → 即座に停止
            if time_manager.search_end() > 0 && elapsed >= time_manager.search_end() {
                #[cfg(debug_assertions)]
                eprintln!(
                    "check_abort: search_end reached elapsed={} search_end={}",
                    elapsed,
                    time_manager.search_end()
                );
                self.state.abort = true;
                return true;
            }

            // フェーズ2: search_end 未設定 → maximum超過 or stop_on_ponderhit で設定
            // ただし ponder 中は停止判定を行わない（YO準拠）
            if !time_manager.is_pondering()
                && time_manager.search_end() == 0
                && limits.use_time_management()
                && (elapsed_effective > time_manager.maximum() || time_manager.stop_on_ponderhit())
            {
                time_manager.set_search_end(elapsed);
                // 注: ここでは停止せず、次のチェックで秒境界で停止
            }
        }

        false
    }

    /// 探索のメインエントリーポイント
    ///
    /// 反復深化で指定された深さまで探索する。
    pub fn search(
        &mut self,
        pos: &mut Position,
        depth: Depth,
        limits: &LimitsType,
        time_manager: &mut TimeManagement,
    ) {
        // ルート手を初期化
        self.state.root_moves = RootMoves::from_legal_moves(pos, &limits.search_moves);

        if self.state.root_moves.is_empty() {
            // 合法手がない場合
            self.state.best_move = Move::NONE;
            return;
        }

        // 反復深化
        for d in 1..=depth {
            if self.state.abort {
                break;
            }

            // イテレーション開始時にeffortをリセット
            for rm in self.state.root_moves.iter_mut() {
                rm.effort = 0.0;
            }

            self.state.root_depth = d;
            self.state.sel_depth = 0;

            // Aspiration Window
            let prev_score = if d > 1 {
                self.state.root_moves[0].score
            } else {
                Value::new(-32001)
            };

            let mut delta = Value::new(10);
            let mut alpha = if d >= 4 {
                Value::new(prev_score.raw().saturating_sub(delta.raw()).max(-32001))
            } else {
                Value::new(-32001)
            };
            let mut beta = if d >= 4 {
                Value::new(prev_score.raw().saturating_add(delta.raw()).min(32001))
            } else {
                Value::new(32001)
            };

            loop {
                let score = self.search_root(pos, d, alpha, beta, limits, time_manager);

                if self.state.abort {
                    break;
                }

                // Window調整
                if score <= alpha {
                    beta = Value::new((alpha.raw() + beta.raw()) / 2);
                    alpha = Value::new(score.raw().saturating_sub(delta.raw()).max(-32001));
                } else if score >= beta {
                    beta = Value::new(score.raw().saturating_add(delta.raw()).min(32001));
                } else {
                    break;
                }

                delta = Value::new(
                    delta.raw().saturating_add(delta.raw() / 3).min(Value::INFINITE.raw()),
                );
            }

            if !self.state.abort {
                self.state.completed_depth = d;
                self.state.best_move = self.state.root_moves[0].mv();
            }
        }
    }

    /// ルート探索
    pub(crate) fn search_root(
        &mut self,
        pos: &mut Position,
        depth: Depth,
        alpha: Value,
        beta: Value,
        limits: &LimitsType,
        time_manager: &mut TimeManagement,
    ) -> Value {
        self.state.root_delta = (beta.raw() - alpha.raw()).abs().max(1);

        let mut alpha = alpha;
        let mut best_value = Value::new(-32001);
        let mut pv_idx = 0;
        let root_in_check = pos.in_check();

        self.state.stack[0].in_check = root_in_check;
        self.state.stack[0].cont_history_ptr = self.cont_history_sentinel;
        self.state.stack[0].cont_hist_key = None;

        // PVをクリアして前回探索の残留を防ぐ
        // NOTE: YaneuraOuでは (ss+1)->pv = pv でポインタを新配列に向け、ss->pv[0] = Move::none() でクリア
        //       Vecベースの実装では明示的なclear()で同等の効果を得る
        self.state.stack[0].pv.clear();
        self.state.stack[1].pv.clear();

        for rm_idx in 0..self.state.root_moves.len() {
            if self.check_abort(limits, time_manager) {
                return Value::ZERO;
            }

            // 各手ごとにsel_depthをリセット（YaneuraOu準拠）
            self.state.sel_depth = 0;

            let mv = self.state.root_moves[rm_idx].mv();
            let gives_check = pos.gives_check(mv);
            let is_capture = pos.is_capture(mv);

            let nodes_before = self.state.nodes;

            // 探索
            let dirty_piece = pos.do_move_with_prefetch(mv, gives_check, self.tt.as_ref());
            self.nnue_push(dirty_piece);
            self.state.nodes += 1;
            self.state.stack[0].current_move = mv;

            // PASS は to()/moved_piece_after() が未定義のため、null move と同様に扱う
            if mv.is_pass() {
                self.clear_cont_history_for_null(0);
            } else {
                let cont_hist_piece = mv.moved_piece_after();
                let cont_hist_to = mv.to();
                self.set_cont_history_for_move(
                    0,
                    root_in_check,
                    is_capture,
                    cont_hist_piece,
                    cont_hist_to,
                );
            }

            // PVS
            let value = if rm_idx == 0 {
                -self.search_node_wrapper::<{ NodeType::PV as u8 }>(
                    pos,
                    depth - 1,
                    -beta,
                    -alpha,
                    1,
                    false,
                    limits,
                    time_manager,
                )
            } else {
                // Zero Window Search
                let mut value = -self.search_node_wrapper::<{ NodeType::NonPV as u8 }>(
                    pos,
                    depth - 1,
                    -alpha - Value::new(1),
                    -alpha,
                    1,
                    true,
                    limits,
                    time_manager,
                );

                // Re-search if needed
                if value > alpha && value < beta {
                    value = -self.search_node_wrapper::<{ NodeType::PV as u8 }>(
                        pos,
                        depth - 1,
                        -beta,
                        -alpha,
                        1,
                        false,
                        limits,
                        time_manager,
                    );
                }

                value
            };

            self.nnue_pop();
            pos.undo_move(mv);

            // この手に費やしたノード数をeffortに積算
            let nodes_delta = self.state.nodes.saturating_sub(nodes_before);
            self.state.root_moves[rm_idx].effort += nodes_delta as f64;

            if self.state.abort {
                return Value::ZERO;
            }

            // スコア更新（この手の探索で到達したsel_depthを記録）
            let mut updated_alpha = rm_idx == 0; // 先頭手は維持（YO準拠）
            {
                let rm = &mut self.state.root_moves[rm_idx];
                rm.score = value;
                rm.sel_depth = self.state.sel_depth;
                rm.accumulate_score_stats(value);
            }

            if value > best_value {
                best_value = value;

                if value > alpha {
                    // YaneuraOu準拠: 2番目以降の手がalphaを更新した場合にカウント
                    // moveCount > 1 && !pvIdx の条件
                    // (Multi-PV未実装なので pvIdx は常に0)
                    if rm_idx > 0 {
                        self.state.best_move_changes += 1.0;
                    }

                    alpha = value;
                    pv_idx = rm_idx;
                    updated_alpha = true;

                    // PVを更新
                    self.state.root_moves[rm_idx].pv.truncate(1);
                    self.state.root_moves[rm_idx].pv.extend_from_slice(&self.state.stack[1].pv);

                    if value >= beta {
                        break;
                    }
                }
            }

            // α未更新の手はスコアを -INFINITE に落として順序維持（YO準拠）
            if !updated_alpha {
                self.state.root_moves[rm_idx].score = Value::new(-Value::INFINITE.raw());
            }
        }

        // 最善手を先頭に移動
        self.state.root_moves.move_to_front(pv_idx);
        self.state.root_moves.sort();

        best_value
    }

    /// 特定のPVライン（pv_idx）のみを探索
    ///
    /// YaneuraOuのMultiPVループに相当。
    /// pv_idx以降の手のみを探索対象とし、0..pv_idxの手は固定とみなす。
    ///
    /// # Arguments
    /// * `pos` - 現在の局面
    /// * `depth` - 探索深さ
    /// * `alpha` - アルファ値
    /// * `beta` - ベータ値
    /// * `pv_idx` - 探索対象のPVインデックス（0-indexed）
    /// * `limits` - 探索制限
    /// * `time_manager` - 時間管理
    pub(crate) fn search_root_for_pv(
        &mut self,
        pos: &mut Position,
        depth: Depth,
        alpha: Value,
        beta: Value,
        pv_idx: usize,
        limits: &LimitsType,
        time_manager: &mut TimeManagement,
    ) -> Value {
        self.state.root_delta = (beta.raw() - alpha.raw()).abs().max(1);

        let mut alpha = alpha;
        let mut best_value = Value::new(-32001);
        let mut best_rm_idx = pv_idx;
        let root_in_check = pos.in_check();

        self.state.stack[0].in_check = root_in_check;
        self.state.stack[0].cont_history_ptr = self.cont_history_sentinel;
        self.state.stack[0].cont_hist_key = None;

        // PVをクリアして前回探索の残留を防ぐ
        // NOTE: YaneuraOuでは (ss+1)->pv = pv でポインタを新配列に向け、ss->pv[0] = Move::none() でクリア
        //       Vecベースの実装では明示的なclear()で同等の効果を得る
        self.state.stack[0].pv.clear();
        self.state.stack[1].pv.clear();

        // pv_idx以降の手のみを探索
        for rm_idx in pv_idx..self.state.root_moves.len() {
            if self.check_abort(limits, time_manager) {
                return Value::ZERO;
            }

            // 各手ごとにsel_depthをリセット
            self.state.sel_depth = 0;

            let mv = self.state.root_moves[rm_idx].mv();
            let gives_check = pos.gives_check(mv);
            let is_capture = pos.is_capture(mv);

            let nodes_before = self.state.nodes;

            // 探索
            let dirty_piece = pos.do_move_with_prefetch(mv, gives_check, self.tt.as_ref());
            self.nnue_push(dirty_piece);
            self.state.nodes += 1;
            self.state.stack[0].current_move = mv;

            // PASS は to()/moved_piece_after() が未定義のため、null move と同様に扱う
            if mv.is_pass() {
                self.clear_cont_history_for_null(0);
            } else {
                let cont_hist_piece = mv.moved_piece_after();
                let cont_hist_to = mv.to();
                self.set_cont_history_for_move(
                    0,
                    root_in_check,
                    is_capture,
                    cont_hist_piece,
                    cont_hist_to,
                );
            }

            // PVS: 最初の手（このPVラインの候補）はPV探索
            let value = if rm_idx == pv_idx {
                -self.search_node_wrapper::<{ NodeType::PV as u8 }>(
                    pos,
                    depth - 1,
                    -beta,
                    -alpha,
                    1,
                    false,
                    limits,
                    time_manager,
                )
            } else {
                // それ以降はZero Window Search
                let mut value = -self.search_node_wrapper::<{ NodeType::NonPV as u8 }>(
                    pos,
                    depth - 1,
                    -alpha - Value::new(1),
                    -alpha,
                    1,
                    true,
                    limits,
                    time_manager,
                );

                // Re-search if needed
                if value > alpha && value < beta {
                    value = -self.search_node_wrapper::<{ NodeType::PV as u8 }>(
                        pos,
                        depth - 1,
                        -beta,
                        -alpha,
                        1,
                        false,
                        limits,
                        time_manager,
                    );
                }

                value
            };

            self.nnue_pop();
            pos.undo_move(mv);

            // この手に費やしたノード数をeffortに積算
            let nodes_delta = self.state.nodes.saturating_sub(nodes_before);
            self.state.root_moves[rm_idx].effort += nodes_delta as f64;

            if self.state.abort {
                return Value::ZERO;
            }

            // スコア更新
            let mut updated_alpha = rm_idx == pv_idx; // PVラインの先頭は維持
            {
                let rm = &mut self.state.root_moves[rm_idx];
                rm.score = value;
                rm.sel_depth = self.state.sel_depth;
                rm.accumulate_score_stats(value);
            }

            if value > best_value {
                best_value = value;

                if value > alpha {
                    // best_move_changesのカウント（2番目以降の手で更新された場合）
                    // MultiPVでは pv_idx == 0（第1PVライン）のみカウントする
                    if pv_idx == 0 && rm_idx > pv_idx {
                        self.state.best_move_changes += 1.0;
                    }

                    alpha = value;
                    best_rm_idx = rm_idx;
                    updated_alpha = true;

                    // PVを更新
                    self.state.root_moves[rm_idx].pv.truncate(1);
                    self.state.root_moves[rm_idx].pv.extend_from_slice(&self.state.stack[1].pv);

                    if value >= beta {
                        break;
                    }
                }
            }

            // α未更新の手は -INFINITE で前回順序を保持（YO準拠）
            if !updated_alpha {
                self.state.root_moves[rm_idx].score = Value::new(-Value::INFINITE.raw());
            }
        }

        // 最善手をpv_idxの位置に移動
        self.state.root_moves.move_to_index(best_rm_idx, pv_idx);

        best_value
    }

    /// 通常探索ノード（ラッパー）
    ///
    /// search_node 関連関数へのエイリアス。既存の呼び出し元との互換性のため維持。
    #[inline]
    pub(super) fn search_node_wrapper<const NT: u8>(
        &mut self,
        pos: &mut Position,
        depth: Depth,
        alpha: Value,
        beta: Value,
        ply: i32,
        cut_node: bool,
        limits: &LimitsType,
        time_manager: &mut TimeManagement,
    ) -> Value {
        // SearchContext を直接構築して借用の競合を避ける
        let ctx = SearchContext {
            tt: &self.tt,
            eval_hash: &self.eval_hash,
            history: &self.history,
            cont_history_sentinel: self.cont_history_sentinel,
            generate_all_legal_moves: self.generate_all_legal_moves,
            max_moves_to_draw: self.max_moves_to_draw,
            thread_id: self.thread_id,
        };
        Self::search_node::<NT>(
            &mut self.state,
            &ctx,
            pos,
            depth,
            alpha,
            beta,
            ply,
            cut_node,
            limits,
            time_manager,
        )
    }

    /// 通常探索ノード（関連関数版）
    ///
    /// NTは NodeType を const genericで受け取る（コンパイル時最適化）
    /// cut_node は「βカットが期待される（ゼロウィンドウの非PVなど）」ときに true を渡す。
    /// 再探索やPV探索では all_node 扱いにするため false を渡す（YaneuraOuのcutNode引き渡しと対応）。
    pub(super) fn search_node<const NT: u8>(
        st: &mut SearchState,
        ctx: &SearchContext<'_>,
        pos: &mut Position,
        depth: Depth,
        alpha: Value,
        beta: Value,
        ply: i32,
        cut_node: bool,
        limits: &LimitsType,
        time_manager: &mut TimeManagement,
    ) -> Value {
        inc_stat!(st, nodes_searched);
        inc_stat_by_depth!(st, nodes_by_depth, depth);
        let pv_node = NT == NodeType::PV as u8 || NT == NodeType::Root as u8;
        let mut depth = depth;
        let in_check = pos.in_check();
        // YaneuraOuのallNode定義: !(PvNode || cutNode)（yaneuraou-search.cpp:1854付近）
        let all_node = !(pv_node || cut_node);
        let mut alpha = alpha;
        let mut beta = beta;

        // 深さが0以下なら静止探索へ
        if depth <= DEPTH_QS {
            return qsearch::<NT>(st, ctx, pos, depth, alpha, beta, ply, limits, time_manager);
        }

        // 最大深さチェック
        if ply >= MAX_PLY {
            return if in_check {
                Value::ZERO
            } else {
                nnue_evaluate(st, pos)
            };
        }

        // 選択的深さを更新
        if pv_node && st.sel_depth < ply + 1 {
            st.sel_depth = ply + 1;
        }

        // 中断チェック
        if check_abort(st, ctx, limits, time_manager) {
            return Value::ZERO;
        }

        // =====================================================================
        // Step 3. Mate Distance Pruning
        // =====================================================================
        // 詰みまでの手数による枝刈り。
        // - 現在のplyで詰まされる場合のスコア(mated_in(ply))より低いalphaは意味がない
        // - 次の手で詰ます場合のスコア(mate_in(ply+1))より高いbetaは意味がない
        // - 補正後にalpha >= betaなら即座にカット
        if NT != NodeType::Root as u8 {
            alpha = alpha.max(Value::mated_in(ply));
            beta = beta.min(Value::mate_in(ply + 1));
            if alpha >= beta {
                return alpha;
            }
        }

        // スタック設定
        st.stack[ply as usize].in_check = in_check;
        st.stack[ply as usize].move_count = 0;
        st.stack[(ply + 1) as usize].cutoff_cnt = 0;

        // PVノードの場合、PVをクリアして前回探索の残留を防ぐ
        // NOTE: YaneuraOuでは (ss+1)->pv = pv でポインタを新配列に向け、ss->pv[0] = Move::none() でクリア
        //       Vecベースの実装では明示的なclear()で同等の効果を得る
        if pv_node {
            st.stack[ply as usize].pv.clear();
            st.stack[(ply + 1) as usize].pv.clear();
        }

        let prior_reduction = take_prior_reduction(st, ply);
        st.stack[ply as usize].reduction = 0;

        // Singular Extension用の除外手を取得
        let excluded_move = st.stack[ply as usize].excluded_move;

        // 置換表プローブ（即時カットオフ含む）
        let tt_ctx = match probe_transposition::<NT>(
            st,
            ctx,
            pos,
            depth,
            beta,
            ply,
            pv_node,
            in_check,
            excluded_move,
        ) {
            ProbeOutcome::Continue(c) => c,
            ProbeOutcome::Cutoff(value) => {
                inc_stat!(st, tt_cutoff);
                inc_stat_by_depth!(st, tt_cutoff_by_depth, depth);
                return value;
            }
        };
        let tt_move = tt_ctx.mv;
        let tt_value = tt_ctx.value;
        let tt_hit = tt_ctx.hit;
        let tt_data = tt_ctx.data;
        let _tt_capture = tt_ctx.capture;

        // 静的評価
        let eval_ctx = compute_eval_context(st, ctx, pos, ply, in_check, &tt_ctx, excluded_move);
        let mut improving = eval_ctx.improving;
        let opponent_worsening = eval_ctx.opponent_worsening;

        // evalDiff によるヒストリ更新（YaneuraOu準拠: yaneuraou-search.cpp:2752-2758）
        // 条件: (ss-1)->currentMove が有効 && !(ss-1)->inCheck && !priorCapture
        if ply >= 1 {
            let prev_ply = (ply - 1) as usize;
            let prev_move = st.stack[prev_ply].current_move;
            let prev_in_check = st.stack[prev_ply].in_check;
            let prior_capture = st.stack[prev_ply].cont_hist_key.is_some_and(|k| k.capture);

            if prev_move.is_normal()
                && !prev_in_check
                && !prior_capture
                && eval_ctx.static_eval != Value::NONE
                && st.stack[prev_ply].static_eval != Value::NONE
            {
                let prev_eval = st.stack[prev_ply].static_eval.raw();
                let curr_eval = eval_ctx.static_eval.raw();
                let eval_diff = (-(prev_eval + curr_eval)).clamp(-200, 156) + 58;
                let opponent = !pos.side_to_move();
                let prev_sq = prev_move.to();

                ctx.history.with_write(|h| {
                    // mainHistory 更新: evalDiff * 9
                    h.main_history.update(opponent, prev_move, eval_diff * 9);

                    // pawnHistory 更新（追加条件: !ttHit && 駒種 != 歩 && 成りでない）
                    if !tt_hit {
                        let prev_piece = pos.piece_on(prev_sq);
                        if prev_piece.piece_type() != PieceType::Pawn && !prev_move.is_promotion() {
                            let pawn_idx = pos.pawn_history_index();
                            h.pawn_history.update(pawn_idx, prev_piece, prev_sq, eval_diff * 14);
                        }
                    }
                });
            }
        }

        // priorReduction に応じた深さ調整（yaneuraou-search.cpp:2769-2774）
        if prior_reduction
            >= if depth < IIR_DEPTH_BOUNDARY {
                IIR_PRIOR_REDUCTION_THRESHOLD_SHALLOW
            } else {
                IIR_PRIOR_REDUCTION_THRESHOLD_DEEP
            }
            && !opponent_worsening
        {
            depth += 1;
        }
        if prior_reduction >= 2
            && depth >= 2
            && ply >= 1
            && eval_ctx.static_eval != Value::NONE
            && st.stack[(ply - 1) as usize].static_eval != Value::NONE
            // Value は ±32002 程度なので i32 加算でオーバーフローしない
            && eval_ctx.static_eval + st.stack[(ply - 1) as usize].static_eval
                > Value::new(IIR_EVAL_SUM_THRESHOLD)
        {
            depth -= 1;
        }

        if let Some(v) = try_razoring::<NT>(
            st,
            ctx,
            pos,
            depth,
            alpha,
            beta,
            ply,
            pv_node,
            in_check,
            eval_ctx.static_eval,
            limits,
            time_manager,
        ) {
            return v;
        }

        // TT の手が駒取りかどうか判定
        let tt_capture = tt_move.is_some() && pos.is_capture(tt_move);

        if let Some(v) = try_futility_pruning(FutilityParams {
            depth,
            beta,
            static_eval: eval_ctx.static_eval,
            correction_value: eval_ctx.correction_value,
            improving,
            opponent_worsening,
            tt_hit,
            tt_move_exists: tt_move.is_some(),
            tt_capture,
            pv_node,
            in_check,
        }) {
            inc_stat!(st, futility_pruned);
            inc_stat_by_depth!(st, futility_by_depth, depth);
            return v;
        }

        let (null_value, improving_after_null) = try_null_move_pruning::<NT, _>(
            st,
            ctx,
            pos,
            depth,
            beta,
            ply,
            cut_node,
            in_check,
            eval_ctx.static_eval,
            improving,
            excluded_move,
            limits,
            time_manager,
            Self::search_node::<{ NodeType::NonPV as u8 }>,
        );
        if let Some(v) = null_value {
            return v;
        }
        improving = improving_after_null;

        // Internal Iterative Reductions（improving再計算後に実施）
        if !all_node && depth >= 6 && tt_move.is_none() && prior_reduction <= 3 {
            depth -= 1;
        }

        if let Some(v) = try_probcut(
            st,
            ctx,
            pos,
            depth,
            beta,
            improving,
            &tt_ctx,
            ply,
            eval_ctx.static_eval,
            eval_ctx.unadjusted_static_eval,
            in_check,
            limits,
            time_manager,
            Self::search_node::<{ NodeType::NonPV as u8 }>,
        ) {
            return v;
        }

        if let Some(v) = try_small_probcut(depth, beta, &tt_ctx) {
            return v;
        }

        // =================================================================
        // 指し手ループ（lazy generation）
        // =================================================================
        let mut best_value = Value::new(-32001);
        let mut best_move = Move::NONE;
        let mut move_count = 0;
        let mut quiets_tried = SearchedMoveList::new();
        let mut captures_tried = SearchedMoveList::new();
        let mover = pos.side_to_move();

        // qsearch/ProbCut互換: 捕獲フェーズではTT手もcapture_stageで制約
        let tt_move = if depth <= DEPTH_QS
            && tt_move.is_some()
            && (!pos.capture_stage(tt_move) && !pos.gives_check(tt_move) || depth < -16)
        {
            Move::NONE
        } else {
            tt_move
        };

        // MovePickerを作成（lazy generation）
        let cont_tables = cont_history_tables(st, ctx, ply);
        let mut mp =
            MovePicker::new(pos, tt_move, depth, ply, cont_tables, ctx.generate_all_legal_moves);

        // Singular Extension用の変数
        let tt_pv = st.stack[ply as usize].tt_pv;
        let root_node = NT == NodeType::Root as u8;

        // LMPが発火したかどうか
        let mut lmp_triggered = false;

        loop {
            // 次の手を取得（lazy generation）
            let mv = ctx.history.with_read(|h| mp.next_move(pos, h));
            if mv == Move::NONE {
                break;
            }
            // Singular Extension用の除外手をスキップ
            if mv == excluded_move {
                continue;
            }
            if !pos.pseudo_legal(mv) {
                continue;
            }
            if !pos.is_legal(mv) {
                continue;
            }
            if check_abort(st, ctx, limits, time_manager) {
                return Value::ZERO;
            }

            move_count += 1;
            st.stack[ply as usize].move_count = move_count;

            let is_capture = pos.is_capture(mv);
            let gives_check = pos.gives_check(mv);

            // quietの指し手の連続回数（YaneuraOu: quietMoveStreak）
            st.stack[(ply + 1) as usize].quiet_move_streak = if !is_capture && !gives_check {
                st.stack[ply as usize].quiet_move_streak + 1
            } else {
                0
            };

            let mut new_depth = depth - 1;
            let mut extension = 0i32;

            // =============================================================
            // Singular Extension（YaneuraOu準拠）
            // =============================================================
            // singular延長をするnodeであるか判定
            // 条件: !rootNode && move == ttMove && !excludedMove && depth >= 6 + ttPv
            //       && is_valid(ttValue) && !is_decisive(ttValue) && (ttBound & BOUND_LOWER)
            //       && ttDepth >= depth - 3
            if !root_node
                && mv == tt_move
                && excluded_move.is_none()
                && depth >= 6 + tt_pv as i32
                && tt_value != Value::NONE
                && !tt_value.is_mate_score()
                && tt_data.bound.is_lower_or_exact()
                && tt_data.depth >= depth - 3
            {
                // singularBeta = ttValue - (56 + 79 * (ttPv && !PvNode)) * depth / 58
                let singular_beta_margin = (56 + 79 * (tt_pv && !pv_node) as i32) * depth / 58;
                let singular_beta = tt_value - Value::new(singular_beta_margin);
                let singular_depth = new_depth / 2;

                // ttMoveを除外して浅い探索を実行
                // 注: YaneuraOu準拠で同じplyで再帰呼び出しを行う（do_moveせず同一局面で探索）
                // これによりstack[ply]の一部フィールド（tt_hit, move_count等）が上書きされるが：
                // - tt_pv: excludedMoveがある場合は保持される（probe_transposition内）
                // - tt_hit: 同じ局面なので同じ値になる
                // - move_count: ローカル変数で管理しているため影響なし
                // - その他: ヒューリスティック用途のため多少の誤差は許容される
                st.stack[ply as usize].excluded_move = mv;
                let singular_value = Self::search_node::<{ NodeType::NonPV as u8 }>(
                    st,
                    ctx,
                    pos,
                    singular_depth,
                    singular_beta - Value::new(1),
                    singular_beta,
                    ply,
                    cut_node,
                    limits,
                    time_manager,
                );
                st.stack[ply as usize].excluded_move = Move::NONE;

                if singular_value < singular_beta {
                    inc_stat!(st, singular_extension);
                    // Singular確定 → 延長量を計算
                    // 補正履歴の寄与（abs(correctionValue)/249096）を margin に加算
                    let corr_val_adj = eval_ctx.correction_value.abs() / 249_096;
                    // YaneuraOu準拠: TTMoveHistoryをdoubleMarginに組み込み
                    let tt_move_hist = ctx.history.with_read(|h| h.tt_move_history.get() as i32);
                    // YaneuraOu準拠: plyがrootDepthを超える場合はマージンを減らす
                    let double_margin = 4 + 205 * pv_node as i32
                        - 223 * !tt_capture as i32
                        - corr_val_adj
                        - 921 * tt_move_hist / 127649
                        - (ply > st.root_depth) as i32 * 45;
                    let triple_margin = 80 + 276 * pv_node as i32 - 249 * !tt_capture as i32
                        + 86 * tt_pv as i32
                        - corr_val_adj
                        - (ply * 2 > st.root_depth * 3) as i32 * 52;

                    extension = 1
                        + (singular_value < singular_beta - Value::new(double_margin)) as i32
                        + (singular_value < singular_beta - Value::new(triple_margin)) as i32;

                    // YaneuraOu準拠: singular確定時にdepthを+1（yaneuraou-search.cpp:3401）
                    depth += 1;
                } else if singular_value >= beta && !singular_value.is_mate_score() {
                    // Multi-Cut: 他の手もfail highする場合は枝刈り
                    // YaneuraOu準拠: TTMoveHistoryを更新
                    ctx.history.with_write(|h| {
                        h.tt_move_history
                            .update(super::tt_history::TTMoveHistory::multi_cut_bonus(depth));
                    });
                    inc_stat!(st, multi_cut);
                    return singular_value;
                } else if tt_value >= beta {
                    // Negative Extension: ttMoveが特別でない場合
                    extension = -3;
                } else if cut_node {
                    extension = -2;
                }
            }

            // 注: YaneuraOu準拠で、new_depth += extension はdo_moveの後で行う（3482行目）
            // ここではextensionを保持しておき、枝刈りはextension反映前のnew_depthで判定

            // =============================================================
            // Reduction計算とStep14の枝刈り
            // =============================================================
            let delta = (beta.raw() - alpha.raw()).max(0);
            let mut r = reduction(improving, depth, move_count, delta, st.root_delta.max(1));

            // YaneuraOu: ttPvなら reduction を少し増やす
            if st.stack[ply as usize].tt_pv {
                r += 931;
            }

            let lmr_depth = new_depth - r / 1024;

            // =============================================================
            // LMP（Step14の前）
            // =============================================================
            // moveCount >= limitのとき、quiet手をスキップ
            // YaneuraOu: skip_quiet_moves()のみでcontinueしないが、
            // rshogiはStep14の条件が緩いため、continueも追加
            if !pv_node && !in_check && !is_capture && !best_value.is_loss() && !mv.is_pass() {
                let lmp_limit = (3 + depth * depth) / (2 - improving as i32);
                if move_count >= lmp_limit {
                    if !lmp_triggered && mp.is_quiet_stage() {
                        mp.skip_quiets();
                        lmp_triggered = true;
                    }
                    continue;
                }
            }

            let step14_ctx = Step14Context {
                pos,
                mv,
                depth,
                ply,
                best_value,
                in_check,
                gives_check,
                is_capture,
                lmr_depth,
                mover,
                cont_history_1: cont_history_ref(st, ctx, ply, 1),
                cont_history_2: cont_history_ref(st, ctx, ply, 2),
                static_eval: eval_ctx.static_eval,
                alpha,
                tt_move,
                pawn_history_index: pos.pawn_history_index(),
            };

            match step14_pruning(ctx, step14_ctx) {
                Step14Outcome::Skip {
                    best_value: updated,
                } => {
                    inc_stat!(st, move_loop_pruned);
                    if let Some(v) = updated {
                        best_value = v;
                    }
                    continue;
                }
                Step14Outcome::Continue => {}
            }

            // =============================================================
            // SEE Pruning
            // =============================================================
            // パス手はSEE Pruningの対象外（SEEは駒交換の評価でありパスには適用不可）
            if !pv_node && depth <= 8 && !in_check && !mv.is_pass() {
                let see_threshold = if is_capture {
                    Value::new(-20 * depth * depth)
                } else {
                    Value::new(-50 * depth)
                };
                if !pos.see_ge(mv, see_threshold) {
                    continue;
                }
            }

            // 指し手を実行
            st.stack[ply as usize].current_move = mv;

            let dirty_piece = pos.do_move_with_prefetch(mv, gives_check, ctx.tt);
            nnue_push(st, dirty_piece);
            st.nodes += 1;

            // YaneuraOu方式: ContHistKey/ContinuationHistoryを設定
            // ⚠ in_checkは親ノードの王手状態を使用（gives_checkではない）
            // PASS は to()/moved_piece_after() が未定義のため、null move と同様に扱う
            if mv.is_pass() {
                clear_cont_history_for_null(st, ctx, ply);
            } else {
                let cont_hist_piece = mv.moved_piece_after();
                let cont_hist_to = mv.to();
                set_cont_history_for_move(
                    st,
                    ctx,
                    ply,
                    in_check,
                    is_capture,
                    cont_hist_piece,
                    cont_hist_to,
                );
            }

            // 手の記録（YaneuraOu準拠: quietsSearched, capturesSearched）
            // PASS は history_index() が未定義のため記録しない
            if !mv.is_pass() {
                if is_capture {
                    captures_tried.push(mv);
                } else {
                    quiets_tried.push(mv);
                }
            }

            // 延長量をnew_depthに加算（YaneuraOu準拠: do_moveの後、yaneuraou-search.cpp:3482）
            new_depth += extension;

            // =============================================================
            // Late Move Reduction (LMR)
            // =============================================================
            let msb_depth = msb(depth);
            let tt_value_higher = tt_hit && tt_value != Value::NONE && tt_value > alpha;
            let tt_depth_ge = tt_hit && tt_data.depth >= depth;

            if st.stack[ply as usize].tt_pv {
                r -= 2510
                    + (pv_node as i32) * 963
                    + (tt_value_higher as i32) * 916
                    + (tt_depth_ge as i32) * (943 + (cut_node as i32) * 1180);
            }

            r += 679 - 6 * msb_depth;
            r -= move_count * (67 - 2 * msb_depth);
            r -= eval_ctx.correction_value.abs() / 27_160;

            if cut_node {
                let no_tt_move = !tt_hit || tt_move.is_none();
                r += 2998 + 2 * msb_depth + (948 + 14 * msb_depth) * (no_tt_move as i32);
            }

            if tt_capture {
                r += 1402 - 39 * msb_depth;
            }

            if st.stack[(ply + 1) as usize].cutoff_cnt > 2 {
                r += 925 + 33 * msb_depth + (all_node as i32) * (701 + 224 * msb_depth);
            }

            r += st.stack[(ply + 1) as usize].quiet_move_streak * 51;

            if mv == tt_move {
                r -= 2121 + 28 * msb_depth;
            }

            // statScoreによる補正
            // PASS は history が未定義のため stat_score = 0 とし、還元を控えめに
            let stat_score = if mv.is_pass() {
                0 // PASS は history がないので還元補正なし
            } else if is_capture {
                let captured = pos.captured_piece();
                let captured_pt = captured.piece_type();
                let moved_piece = mv.moved_piece_after();
                let hist = ctx
                    .history
                    .with_read(|h| h.capture_history.get(moved_piece, mv.to(), captured_pt) as i32);
                782 * piece_value(captured) / 128 + hist
            } else {
                let moved_piece = mv.moved_piece_after();
                let main_hist = ctx.history.with_read(|h| h.main_history.get(mover, mv) as i32);
                let cont0 = cont_history_ref(st, ctx, ply, 1).get(moved_piece, mv.to()) as i32;
                let cont1 = cont_history_ref(st, ctx, ply, 2).get(moved_piece, mv.to()) as i32;
                2 * main_hist + cont0 + cont1
            };
            st.stack[ply as usize].stat_score = stat_score;
            r -= stat_score * (729 - 12 * msb_depth) / 8192;

            // =============================================================
            // 探索
            // =============================================================
            let mut value = if depth >= 2 && move_count > 1 {
                inc_stat!(st, lmr_applied);
                // YaneuraOu準拠: d = max(1, min(newDepth - r/1024, newDepth + 2)) + PvNode
                let d = (std::cmp::max(1, std::cmp::min(new_depth - r / 1024, new_depth + 2))
                    + pv_node as i32)
                    .max(1);

                // LMR統計: 削減量と新深度を記録
                #[cfg(feature = "search-stats")]
                {
                    // r/1024のヒストグラム（15以上は15+にまとめる）
                    let reduction = (r / 1024).max(0) as usize;
                    let reduction_idx = reduction.min(15);
                    st.stats.lmr_reduction_histogram[reduction_idx] += 1;
                    // 新深度のヒストグラム
                    let new_depth_idx = (d as usize).min(STATS_MAX_DEPTH - 1);
                    st.stats.lmr_new_depth_histogram[new_depth_idx] += 1;
                }

                // depth 1への遷移を追跡
                #[cfg(feature = "search-stats")]
                if d == 1 {
                    let parent_depth_idx = (depth as usize).min(STATS_MAX_DEPTH - 1);
                    st.stats.lmr_to_depth1_from[parent_depth_idx] += 1;
                }

                // cut_node 分析
                #[cfg(feature = "search-stats")]
                {
                    if cut_node {
                        st.stats.lmr_cut_node_applied += 1;
                        if d == 1 {
                            st.stats.lmr_cut_node_to_depth1 += 1;
                        }
                    } else {
                        st.stats.lmr_non_cut_node_applied += 1;
                        if d == 1 {
                            st.stats.lmr_non_cut_node_to_depth1 += 1;
                        }
                    }
                }

                let reduction_from_parent = (depth - 1) - d;
                st.stack[ply as usize].reduction = reduction_from_parent;
                let mut value = -Self::search_node::<{ NodeType::NonPV as u8 }>(
                    st,
                    ctx,
                    pos,
                    d,
                    -alpha - Value::new(1),
                    -alpha,
                    ply + 1,
                    true,
                    limits,
                    time_manager,
                );
                st.stack[ply as usize].reduction = 0;

                if value > alpha {
                    let do_deeper =
                        d < new_depth && value > (best_value + Value::new(43 + 2 * new_depth));
                    let do_shallower = value < best_value + Value::new(9);
                    new_depth += do_deeper as i32 - do_shallower as i32;

                    if new_depth > d {
                        inc_stat!(st, lmr_research);
                        value = -Self::search_node::<{ NodeType::NonPV as u8 }>(
                            st,
                            ctx,
                            pos,
                            new_depth,
                            -alpha - Value::new(1),
                            -alpha,
                            ply + 1,
                            !cut_node,
                            limits,
                            time_manager,
                        );
                    }

                    // YaneuraOu: fail high後にcontHistを更新 (yaneuraou-search.cpp:3614-3618)
                    // PASS は履歴の対象外なのでスキップ
                    if !mv.is_pass() {
                        let moved_piece = mv.moved_piece_after();
                        let to_sq = mv.to();
                        const CONTHIST_BONUSES: &[(i32, i32)] =
                            &[(1, 1108), (2, 652), (3, 273), (4, 572), (5, 126), (6, 449)];
                        for &(offset, weight) in CONTHIST_BONUSES {
                            if st.stack[ply as usize].in_check && offset > 2 {
                                break;
                            }
                            let idx = ply - offset;
                            if idx < 0 {
                                break;
                            }
                            if let Some(key) = st.stack[idx as usize].cont_hist_key {
                                let in_check_idx = key.in_check as usize;
                                let capture_idx = key.capture as usize;
                                let bonus = 1412 * weight / 1024 + if offset < 2 { 80 } else { 0 };
                                ctx.history.with_write(|h| {
                                    h.continuation_history[in_check_idx][capture_idx].update(
                                        key.piece,
                                        key.to,
                                        moved_piece,
                                        to_sq,
                                        bonus,
                                    );
                                });
                            }
                        }
                    }
                    // cutoffCntインクリメント条件 (extension<2 || PvNode) をベータカット時に加算で近似。
                    // ※ Extension導入後は extension<2 を実際の延長量で判定する形に差し替えること。
                } else if value > alpha && value < best_value + Value::new(9) {
                    #[allow(unused_assignments)]
                    {
                        new_depth -= 1;
                    }
                }

                if pv_node && (move_count == 1 || value > alpha) {
                    st.stack[ply as usize].reduction = 0;
                    -Self::search_node::<{ NodeType::PV as u8 }>(
                        st,
                        ctx,
                        pos,
                        depth - 1,
                        -beta,
                        -alpha,
                        ply + 1,
                        false,
                        limits,
                        time_manager,
                    )
                } else {
                    value
                }
            } else if !pv_node || move_count > 1 {
                // Zero window search
                st.stack[ply as usize].reduction = 0;
                let mut value = -Self::search_node::<{ NodeType::NonPV as u8 }>(
                    st,
                    ctx,
                    pos,
                    depth - 1,
                    -alpha - Value::new(1),
                    -alpha,
                    ply + 1,
                    !cut_node,
                    limits,
                    time_manager,
                );
                st.stack[ply as usize].reduction = 0;

                if pv_node && value > alpha && value < beta {
                    st.stack[ply as usize].reduction = 0;
                    value = -Self::search_node::<{ NodeType::PV as u8 }>(
                        st,
                        ctx,
                        pos,
                        depth - 1,
                        -beta,
                        -alpha,
                        ply + 1,
                        false,
                        limits,
                        time_manager,
                    );
                    st.stack[ply as usize].reduction = 0;
                }

                value
            } else {
                // Full window search
                st.stack[ply as usize].reduction = 0;
                -Self::search_node::<{ NodeType::PV as u8 }>(
                    st,
                    ctx,
                    pos,
                    depth - 1,
                    -beta,
                    -alpha,
                    ply + 1,
                    false,
                    limits,
                    time_manager,
                )
            };
            nnue_pop(st);
            pos.undo_move(mv);

            // パス手評価ボーナス: パス手を実行した場合、評価値にボーナスを加算
            // スケーリングなし（常に設定値の100%を適用）
            // 負のボーナスも適用（パス抑制用途）
            // 注意: 詰みスコアには加算しない（mate距離が壊れるため）
            if mv.is_pass() && !value.is_mate_score() {
                let bonus = get_scaled_pass_move_bonus(pos.game_ply());
                if bonus != 0 {
                    value += Value::new(bonus);
                }
            }

            if st.abort {
                return Value::ZERO;
            }

            // =============================================================
            // スコア更新
            // =============================================================
            if value > best_value {
                best_value = value;

                if value > alpha {
                    best_move = mv;
                    alpha = value;

                    // PV更新
                    if pv_node {
                        // 借用チェッカーの制約を避けるためクローン
                        let child_pv = st.stack[(ply + 1) as usize].pv.clone();
                        st.stack[ply as usize].update_pv(mv, &child_pv);
                    }

                    if value >= beta {
                        // cutoffCntインクリメント条件 (extension<2 || PvNode) をベータカット時に加算で近似。
                        st.stack[ply as usize].cutoff_cnt += 1;
                        // Move Ordering品質統計
                        inc_stat_by_depth!(st, cutoff_by_depth, depth);
                        if move_count == 1 {
                            inc_stat_by_depth!(st, first_move_cutoff_by_depth, depth);
                        }
                        // カットオフ時のmove_count統計
                        #[cfg(feature = "search-stats")]
                        {
                            let d = (depth as usize).min(STATS_MAX_DEPTH - 1);
                            st.stats.move_count_sum_by_depth[d] += move_count as u64;
                        }
                        break;
                    }
                }
            }
        }

        // =================================================================
        // 詰み/ステイルメイト判定
        // =================================================================
        // YaneuraOu準拠: excludedMoveがある場合は、ttMoveが除外されているので
        // 単にalphaを返す（詰みとは判定しない）
        if move_count == 0 {
            if excluded_move.is_some() {
                return alpha;
            }
            // 合法手なし
            if in_check {
                // 詰み
                return Value::mated_in(ply);
            } else {
                // ステイルメイト（将棋では通常発生しないがパスがない場合）
                return Value::ZERO;
            }
        }

        // =================================================================
        // History更新（YaneuraOu準拠: update_all_stats）
        // =================================================================
        // YaneuraOu: bestMoveがある場合は常にupdate_all_statsを呼ぶ
        // PASS は history_index() が未定義のためスキップ
        if best_move.is_some() && !best_move.is_pass() {
            let is_best_capture = pos.is_capture(best_move);
            let is_tt_move = best_move == tt_move;
            // YaneuraOu準拠: bonus = min(170*depth-87, 1598) + 332*(bestMove==ttMove)
            let bonus = stat_bonus(depth, is_tt_move);
            // YaneuraOu準拠: quietMalus = min(743*depth-180, 2287) - 33*quietsSearched.size()
            let malus = quiet_malus(depth, quiets_tried.len());
            let us = pos.side_to_move();
            let pawn_key_idx = pos.pawn_history_index();

            // best_moveの駒情報を取得
            let best_moved_pc = pos.moved_piece(best_move);
            let best_cont_pc = if best_move.is_promotion() {
                best_moved_pc.promote().unwrap_or(best_moved_pc)
            } else {
                best_moved_pc
            };
            let best_to = best_move.to();

            // 王手中は1,2手前のみ
            let max_ply_back = if in_check { 2 } else { 6 };

            if !is_best_capture {
                // Quiet手がbest: update_quiet_histories(bestMove, bonus * 978 / 1024)相当
                // YaneuraOu準拠: bonus * 978 / 1024をベースに各historyを更新
                let scaled_bonus = bonus * 978 / 1024;

                // 他のquiet手にはペナルティ
                // YaneuraOu: update_quiet_histories(move, -quietMalus * 1115 / 1024)
                let scaled_malus = malus * 1115 / 1024;

                // History更新をまとめて実行
                ctx.history.with_write(|h| {
                    // MainHistory: そのまま渡す
                    h.main_history.update(us, best_move, scaled_bonus);

                    // LowPlyHistory: bonus * 771 / 1024 + 40
                    if ply < LOW_PLY_HISTORY_SIZE as i32 {
                        let low_ply_bonus = low_ply_history_bonus(scaled_bonus);
                        h.low_ply_history.update(ply as usize, best_move, low_ply_bonus);
                    }

                    // ContinuationHistory: bonus * (bonus > 0 ? 979 : 842) / 1024 + weight + 80*(i<2)
                    for &(ply_back, weight) in CONTINUATION_HISTORY_WEIGHTS.iter() {
                        if ply_back > max_ply_back {
                            continue;
                        }
                        if ply >= ply_back as i32 {
                            let prev_ply = (ply - ply_back as i32) as usize;
                            if let Some(key) = st.stack[prev_ply].cont_hist_key {
                                let in_check_idx = key.in_check as usize;
                                let capture_idx = key.capture as usize;
                                let weighted_bonus = continuation_history_bonus_with_offset(
                                    scaled_bonus * weight / 1024,
                                    ply_back,
                                );
                                h.continuation_history[in_check_idx][capture_idx].update(
                                    key.piece,
                                    key.to,
                                    best_cont_pc,
                                    best_to,
                                    weighted_bonus,
                                );
                            }
                        }
                    }

                    // PawnHistory: bonus * 704 / 1024 + 70
                    let pawn_bonus = pawn_history_bonus(scaled_bonus);
                    h.pawn_history.update(pawn_key_idx, best_cont_pc, best_to, pawn_bonus);

                    // 他のquiet手にはペナルティ
                    for &m in quiets_tried.iter() {
                        if m != best_move {
                            // MainHistory
                            h.main_history.update(us, m, -scaled_malus);

                            // LowPlyHistory（現行欠落していたので追加）
                            if ply < LOW_PLY_HISTORY_SIZE as i32 {
                                let low_ply_malus = low_ply_history_bonus(-scaled_malus);
                                h.low_ply_history.update(ply as usize, m, low_ply_malus);
                            }

                            // ContinuationHistory/PawnHistoryへのペナルティで必要な情報
                            let moved_pc = pos.moved_piece(m);
                            let cont_pc = if m.is_promotion() {
                                moved_pc.promote().unwrap_or(moved_pc)
                            } else {
                                moved_pc
                            };
                            let to = m.to();

                            // ContinuationHistoryへのペナルティ
                            for &(ply_back, weight) in CONTINUATION_HISTORY_WEIGHTS.iter() {
                                if ply_back > max_ply_back {
                                    continue;
                                }
                                if ply >= ply_back as i32 {
                                    let prev_ply = (ply - ply_back as i32) as usize;
                                    if let Some(key) = st.stack[prev_ply].cont_hist_key {
                                        let in_check_idx = key.in_check as usize;
                                        let capture_idx = key.capture as usize;
                                        let weighted_malus = continuation_history_bonus_with_offset(
                                            -scaled_malus * weight / 1024,
                                            ply_back,
                                        );
                                        h.continuation_history[in_check_idx][capture_idx].update(
                                            key.piece,
                                            key.to,
                                            cont_pc,
                                            to,
                                            weighted_malus,
                                        );
                                    }
                                }
                            }

                            // PawnHistoryへのペナルティ
                            let pawn_malus = pawn_history_bonus(-scaled_malus);
                            h.pawn_history.update(pawn_key_idx, cont_pc, to, pawn_malus);
                        }
                    }
                });
            } else {
                // 捕獲手がbest: captureHistoryを更新
                let captured_pt = pos.piece_on(best_to).piece_type();
                ctx.history.with_write(|h| {
                    h.capture_history.update(best_cont_pc, best_to, captured_pt, bonus)
                });
            }

            // YaneuraOu: 他の捕獲手へのペナルティ（capture best以外の全捕獲手）
            // captureMalus = min(708*depth-148, 2287) - 29*capturesSearched.size()
            let cap_malus = capture_malus(depth, captures_tried.len());
            ctx.history.with_write(|h| {
                for &m in captures_tried.iter() {
                    if m != best_move {
                        let moved_pc = pos.moved_piece(m);
                        let cont_pc = if m.is_promotion() {
                            moved_pc.promote().unwrap_or(moved_pc)
                        } else {
                            moved_pc
                        };
                        let to = m.to();
                        let captured_pt = pos.piece_on(to).piece_type();
                        // YaneuraOu: captureHistory << -captureMalus * 1431 / 1024
                        h.capture_history.update(
                            cont_pc,
                            to,
                            captured_pt,
                            -cap_malus * 1431 / 1024,
                        );
                    }
                }
            });

            // YaneuraOu: quiet early refutationペナルティ
            // 条件: prevSq != SQ_NONE && (ss-1)->moveCount == 1 + (ss-1)->ttHit && !pos.captured_piece()
            // 処理: update_continuation_histories(ss - 1, pos.piece_on(prevSq), prevSq, -captureMalus * 622 / 1024)
            if ply >= 1 {
                let prev_ply = (ply - 1) as usize;
                let prev_move_count = st.stack[prev_ply].move_count;
                let prev_tt_hit = st.stack[prev_ply].tt_hit;
                // YaneuraOu: !pos.captured_piece() = 現在の局面で駒が取られていない
                if prev_move_count == 1 + (prev_tt_hit as i32)
                    && pos.captured_piece() == Piece::NONE
                {
                    if let Some(key) = st.stack[prev_ply].cont_hist_key {
                        let prev_sq = key.to;
                        let prev_piece = pos.piece_on(prev_sq);
                        // YaneuraOu: update_continuation_histories(ss - 1, ...)を呼ぶ
                        // = 過去1-6手分全てに weight と +80 オフセット付きで更新
                        let penalty_base = -cap_malus * 622 / 1024;
                        // YaneuraOu: update_continuation_histories(ss - 1, ...) で (ss - 1)->inCheck を参照
                        let prev_in_check = st.stack[prev_ply].in_check;
                        let prev_max_ply_back = if prev_in_check { 2 } else { 6 };

                        ctx.history.with_write(|h| {
                            for &(ply_back, weight) in CONTINUATION_HISTORY_WEIGHTS.iter() {
                                if ply_back > prev_max_ply_back {
                                    continue;
                                }
                                // ss - 1 からさらに ply_back 手前 = ply - 1 - ply_back
                                let target_ply = ply - 1 - ply_back as i32;
                                if target_ply >= 0 {
                                    if let Some(target_key) =
                                        st.stack[target_ply as usize].cont_hist_key
                                    {
                                        let in_check_idx = target_key.in_check as usize;
                                        let capture_idx = target_key.capture as usize;
                                        let weighted_penalty = penalty_base * weight / 1024
                                            + if ply_back <= 2 {
                                                CONTINUATION_HISTORY_NEAR_PLY_OFFSET
                                            } else {
                                                0
                                            };
                                        h.continuation_history[in_check_idx][capture_idx].update(
                                            target_key.piece,
                                            target_key.to,
                                            prev_piece,
                                            prev_sq,
                                            weighted_penalty,
                                        );
                                    }
                                }
                            }
                        });
                    }
                }
            }

            // TTMoveHistory更新（非PVノードのみ、YaneuraOu準拠）
            // YaneuraOu: ttMoveHistory << (bestMove == ttData.move ? 809 : -865)
            if !pv_node && tt_move.is_some() {
                let bonus = if best_move == tt_move {
                    TT_MOVE_HISTORY_BONUS
                } else {
                    TT_MOVE_HISTORY_MALUS
                };
                ctx.history.with_write(|h| h.tt_move_history.update(bonus));
            }
        }
        // =================================================================
        // Prior Countermove Bonus（fail low時の前の手にボーナス）
        // YaneuraOu準拠: yaneuraou-search.cpp:3936-3977
        // =================================================================
        else if ply >= 1 {
            let prev_ply = (ply - 1) as usize;
            if let Some(prev_key) = st.stack[prev_ply].cont_hist_key {
                let prior_capture = prev_key.capture;
                let prev_sq = prev_key.to;

                if !prior_capture {
                    // Prior quiet countermove bonus
                    // YaneuraOu: yaneuraou-search.cpp:3945-3966
                    let parent_stat_score = st.stack[prev_ply].stat_score;
                    let parent_move_count = st.stack[prev_ply].move_count;
                    let parent_in_check = st.stack[prev_ply].in_check;
                    let parent_static_eval = st.stack[prev_ply].static_eval;
                    let static_eval = st.stack[ply as usize].static_eval;

                    // bonusScale計算（YaneuraOu準拠）
                    let mut bonus_scale: i32 = -228;
                    bonus_scale -= parent_stat_score / 104;
                    bonus_scale += (63 * depth).min(508);
                    bonus_scale += 184 * (parent_move_count > 8) as i32;
                    bonus_scale += 143
                        * (!in_check
                            && static_eval != Value::NONE
                            && best_value <= static_eval - Value::new(92))
                            as i32;
                    bonus_scale += 149
                        * (!parent_in_check
                            && parent_static_eval != Value::NONE
                            && best_value <= -parent_static_eval - Value::new(70))
                            as i32;
                    bonus_scale = bonus_scale.max(0);

                    // 値域: bonus_scale ≥ 0, min(...) ∈ [52, 1365] (depth>=1)
                    // i64で計算してオーバーフローを防止
                    let scaled_bonus = (144 * depth - 92).min(1365) as i64 * bonus_scale as i64;

                    // continuation history更新
                    // YaneuraOu: update_continuation_histories(ss - 1, pos.piece_on(prevSq), prevSq, scaledBonus * 400 / 32768)
                    // 注: prev_sq は cont_hist_key.to（do_move後に設定）なので、
                    //     この時点で prev_piece != NONE が保証される
                    let prev_piece = pos.piece_on(prev_sq);
                    let prev_max_ply_back = if parent_in_check { 2 } else { 6 };
                    let cont_bonus = (scaled_bonus * 400 / 32768) as i32;

                    // main history更新
                    // YaneuraOu: mainHistory[~us][((ss - 1)->currentMove).raw()] << scaledBonus * 220 / 32768
                    let prev_move = st.stack[prev_ply].current_move;
                    let main_bonus = (scaled_bonus * 220 / 32768) as i32;
                    // 注: 前の手なので手番は!pos.side_to_move()
                    let opponent = !pos.side_to_move();

                    // pawn history更新（歩以外かつ成りでない場合）
                    // YaneuraOu: if (type_of(pos.piece_on(prevSq)) != PAWN && ((ss - 1)->currentMove).type_of() != PROMOTION)
                    let pawn_key_idx = pos.pawn_history_index();
                    let pawn_bonus = (scaled_bonus * 1164 / 32768) as i32;
                    let update_pawn =
                        prev_piece.piece_type() != PieceType::Pawn && !prev_move.is_promotion();

                    ctx.history.with_write(|h| {
                        for &(ply_back, weight) in CONTINUATION_HISTORY_WEIGHTS.iter() {
                            if ply_back > prev_max_ply_back {
                                continue;
                            }
                            // ss - 1 からさらに ply_back 手前 = ply - 1 - ply_back
                            let target_ply = ply - 1 - ply_back as i32;
                            if target_ply >= 0 {
                                if let Some(target_key) =
                                    st.stack[target_ply as usize].cont_hist_key
                                {
                                    let in_check_idx = target_key.in_check as usize;
                                    let capture_idx = target_key.capture as usize;
                                    let weighted_bonus = cont_bonus * weight / 1024
                                        + if ply_back <= 2 {
                                            CONTINUATION_HISTORY_NEAR_PLY_OFFSET
                                        } else {
                                            0
                                        };
                                    h.continuation_history[in_check_idx][capture_idx].update(
                                        target_key.piece,
                                        target_key.to,
                                        prev_piece,
                                        prev_sq,
                                        weighted_bonus,
                                    );
                                }
                            }
                        }

                        h.main_history.update(opponent, prev_move, main_bonus);

                        if update_pawn {
                            h.pawn_history.update(pawn_key_idx, prev_piece, prev_sq, pawn_bonus);
                        }
                    });
                } else {
                    // Prior capture countermove bonus
                    // YaneuraOu: yaneuraou-search.cpp:3972-3977
                    // 注: prev_sq は cont_hist_key.to（do_move後に設定）なので prev_piece は有効
                    let prev_piece = pos.piece_on(prev_sq);
                    let captured_piece = pos.captured_piece();
                    // YaneuraOu: assert(capturedPiece != NO_PIECE)
                    debug_assert!(
                        captured_piece != Piece::NONE,
                        "prior_capture is true but captured_piece is NONE"
                    );
                    if captured_piece != Piece::NONE {
                        ctx.history.with_write(|h| {
                            h.capture_history.update(
                                prev_piece,
                                prev_sq,
                                captured_piece.piece_type(),
                                PRIOR_CAPTURE_COUNTERMOVE_BONUS,
                            );
                        });
                    }
                }
            }
        }

        // CorrectionHistoryの更新（YaneuraOu準拠）
        if !in_check && best_move.is_some() && !pos.is_capture(best_move) {
            let static_eval = st.stack[ply as usize].static_eval;
            if static_eval != Value::NONE
                && ((best_value < static_eval && best_value < beta) || best_value > static_eval)
            {
                let bonus = ((best_value.raw() - static_eval.raw()) * depth / 8)
                    .clamp(-CORRECTION_HISTORY_LIMIT / 4, CORRECTION_HISTORY_LIMIT / 4);
                update_correction_history(st, ctx, pos, ply, bonus);
            }
        }

        // =================================================================
        // 置換表更新
        // =================================================================
        // excludedMoveがある場合は置換表に書き込まない（YaneuraOu準拠）
        // 同一局面で異なるexcludedMoveを持つ局面が同じhashkeyを持つため
        if excluded_move.is_none() {
            let bound = if best_value >= beta {
                Bound::Lower
            } else if pv_node && best_move.is_some() {
                Bound::Exact
            } else {
                Bound::Upper
            };

            tt_ctx.result.write(
                tt_ctx.key,
                value_to_tt(best_value, ply),
                pv_node,
                bound,
                depth,
                best_move,
                eval_ctx.unadjusted_static_eval,
                ctx.tt.generation(),
            );
            inc_stat_by_depth!(st, tt_write_by_depth, depth);
        }

        best_value
    }
}

// SAFETY: SearchWorkerは単一スレッドで使用される前提。
// StackArray内の各Stackが持つ `cont_history_ptr: NonNull<PieceToHistory>` は
// `self.history.continuation_history` 内のテーブルへの参照である。
// SearchWorkerがスレッド間でmoveされても、history フィールドも一緒にmoveされるため、
// ポインタの参照先は常に有効であり、データ競合も発生しない。
unsafe impl Send for SearchWorker {}
