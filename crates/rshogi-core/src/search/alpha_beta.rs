//! Alpha-Beta探索の実装
//!
//! YaneuraOu準拠のAlpha-Beta探索。
//! - Principal Variation Search (PVS)
//! - 静止探索 (Quiescence Search)
//! - 各種枝刈り: NMP, LMR, Futility, Razoring, SEE, Singular Extension

use std::ptr::NonNull;
use std::sync::Arc;

#[cfg(not(feature = "search-no-pass-rules"))]
use crate::eval::evaluate_pass_rights;
use crate::eval::{get_scaled_pass_move_bonus, EvalHash};
use crate::nnue::{get_network, AccumulatorStackVariant, DirtyPiece};
use crate::position::Position;
use crate::search::PieceToHistory;
use crate::tt::{ProbeResult, TTData, TranspositionTable};
use crate::types::{Bound, Color, Depth, Move, Piece, PieceType, Square, Value, DEPTH_QS, MAX_PLY};

use super::history::{
    continuation_history_bonus_with_offset, continuation_history_weight, low_ply_history_bonus,
    pawn_history_bonus, stat_bonus, stat_malus, HistoryCell, HistoryTables,
    CORRECTION_HISTORY_LIMIT, LOW_PLY_HISTORY_SIZE,
};
use super::movepicker::piece_value;
use super::types::{
    init_stack_array, value_from_tt, value_to_tt, ContHistKey, NodeType, RootMoves,
    SearchedMoveList, StackArray, STACK_SIZE,
};
use super::{LimitsType, MovePicker, SearchTuneParams, TimeManagement};

use super::eval_helpers::{
    compute_eval_context, correction_value, probe_transposition, update_correction_history,
};
use super::pruning::{
    step14_pruning, try_futility_pruning, try_null_move_pruning, try_probcut, try_razoring,
};
use super::qsearch::qsearch;
use super::search_helpers::{
    check_abort, clear_cont_history_for_null, cont_history_ptr, cont_history_tables, nnue_evaluate,
    nnue_pop, nnue_push, set_cont_history_for_move, take_prior_reduction,
};
#[cfg(feature = "tt-trace")]
use super::tt_sanity::{helper_tt_write_enabled_for_depth, maybe_trace_tt_write, TtWriteTrace};

// =============================================================================
// 定数
// =============================================================================

use std::sync::LazyLock;

#[inline]
pub(super) fn draw_jitter(nodes: u64, tune_params: &SearchTuneParams) -> i32 {
    // 千日手盲点を避けるため、VALUE_DRAW(0) を ±1 にばらつかせる。
    let mask = tune_params.draw_jitter_mask.max(0) as u64;
    ((nodes & mask) as i32) + tune_params.draw_jitter_offset
}

#[inline]
/// 補正履歴を適用した静的評価に変換（詰みスコア領域に入り込まないようにクリップ）
pub(super) fn to_corrected_static_eval(unadjusted: Value, correction_value: i32) -> Value {
    let corrected = unadjusted.raw() + correction_value / 131_072;
    Value::new(corrected.clamp(Value::MATED_IN_MAX_PLY.raw() + 1, Value::MATE_IN_MAX_PLY.raw() - 1))
}

// =============================================================================
// ヒストリ更新ヘルパー
// =============================================================================

/// continuation historiesを更新
///
/// `base_ply`手目から見た過去1-6手との continuation history を更新する。
/// 王手中は最初の2手前のみ（YaneuraOu: `if (ss->inCheck && i > 2) break`）。
#[inline]
fn update_continuation_histories(
    h: &mut HistoryTables,
    stack: &StackArray,
    tune_params: &SearchTuneParams,
    base_ply: i32,
    in_check: bool,
    pc: Piece,
    to: Square,
    bonus: i32,
) {
    let max_ply_back = if in_check { 2 } else { 6 };
    for ply_back in 1..=6 {
        if ply_back > max_ply_back {
            continue;
        }
        let weight = continuation_history_weight(tune_params, ply_back);
        let target_ply = base_ply - ply_back as i32;
        if target_ply >= 0 {
            if let Some(key) = stack[target_ply as usize].cont_hist_key {
                let in_check_idx = key.in_check as usize;
                let capture_idx = key.capture as usize;
                let weighted_bonus = continuation_history_bonus_with_offset(
                    bonus * weight / 1024,
                    ply_back,
                    tune_params,
                );
                h.continuation_history[in_check_idx][capture_idx].update(
                    key.piece,
                    key.to,
                    pc,
                    to,
                    weighted_bonus,
                );
            }
        }
    }
}

/// YaneuraOu準拠: quiet手のhistoryを一括更新 (yaneuraou-search.cpp:5054-5068)
///
/// MainHistory, LowPlyHistory, ContinuationHistory, PawnHistoryを更新する。
#[inline]
fn update_quiet_histories(
    h: &mut HistoryTables,
    stack: &StackArray,
    tune_params: &SearchTuneParams,
    pos: &Position,
    ply: i32,
    in_check: bool,
    mv: Move,
    bonus: i32,
) {
    let us = pos.side_to_move();
    h.main_history.update(us, mv, bonus);

    if ply < LOW_PLY_HISTORY_SIZE as i32 {
        h.low_ply_history
            .update(ply as usize, mv, low_ply_history_bonus(bonus, tune_params));
    }

    let moved_pc = pos.moved_piece(mv);
    let cont_pc = if mv.is_promotion() {
        moved_pc.promote().unwrap_or(moved_pc)
    } else {
        moved_pc
    };
    let to = mv.to();

    // YaneuraOu準拠: update_continuation_histories に渡す前に 955/1024 を適用
    // (yaneuraou-search.cpp:5062: update_continuation_histories(ss, pc, to, bonus * 955 / 1024))
    let cont_bonus = bonus * tune_params.continuation_history_multiplier / 1024;
    update_continuation_histories(h, stack, tune_params, ply, in_check, cont_pc, to, cont_bonus);

    let pawn_key_idx = pos.pawn_history_index();
    h.pawn_history
        .update(pawn_key_idx, cont_pc, to, pawn_history_bonus(bonus, tune_params));
}

/// LMR用のreduction配列
///
/// YaneuraOu準拠で `MAX_MOVES` 長を使う。
/// move_count が 64 を超える局面でも reduction が頭打ちにならないようにする。
type Reductions = [i32; crate::movegen::MAX_MOVES];

/// Reduction配列（LazyLockによる遅延初期化）
/// 初回アクセス時に自動初期化されるため、get()呼び出しが不要
static REDUCTIONS: LazyLock<Reductions> = LazyLock::new(|| {
    let mut table: Reductions = [0; crate::movegen::MAX_MOVES];
    for (i, value) in table.iter_mut().enumerate().skip(1) {
        *value = (2809.0 / 128.0 * (i as f64).ln()) as i32;
    }
    table
});

/// Reductionを取得
///
/// LazyLockにより初回アクセス時に自動初期化されるため、panicしない。
#[inline]
pub(crate) fn reduction(
    tune_params: &SearchTuneParams,
    imp: bool,
    depth: i32,
    move_count: i32,
    delta: i32,
    root_delta: i32,
) -> i32 {
    if depth <= 0 || move_count <= 0 {
        return 0;
    }

    let max_idx = (crate::movegen::MAX_MOVES as i32) - 1;
    let d = depth.clamp(1, max_idx) as usize;
    let mc = move_count.clamp(1, max_idx) as usize;
    // LazyLockにより直接アクセス可能（get()不要）
    let reduction_scale = REDUCTIONS[d] * REDUCTIONS[mc];
    let root_delta = root_delta.max(1);
    let delta = delta.max(0);

    // 1024倍スケールで返す。ttPv加算は呼び出し側で行う。
    reduction_scale - delta * tune_params.lmr_reduction_delta_scale / root_delta
        + (!imp as i32) * reduction_scale * tune_params.lmr_reduction_non_improving_mult
            / tune_params.lmr_reduction_non_improving_div.max(1)
        + tune_params.lmr_reduction_base_offset
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
    /// 即時カットオフ値（ヒストリ更新用情報付き）
    Cutoff {
        value: Value,
        tt_move: Move,
        tt_capture: bool,
    },
}

/// 静的評価まわりの情報をまとめたコンテキスト
pub(super) struct EvalContext {
    /// TT境界情報で補正済みの評価値（YOの `eval` 相当）
    pub(super) eval: Value,
    /// 局面の静的評価（YOの `ss->staticEval` 相当）
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
    pub(super) tt_pv: bool,
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
    pub(super) best_move: Move,           // YO: !bestMove 判定用
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
    /// この探索でTT書き込みを許可するか
    pub allow_tt_write: bool,
    /// SPSA向け探索係数
    pub tune_params: &'a SearchTuneParams,
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

    /// このワーカーでTT書き込みを許可するか
    pub allow_tt_write: bool,

    /// SPSA向け探索係数
    pub search_tune_params: SearchTuneParams,

    // =========================================================================
    // 探索状態（SearchState）
    // =========================================================================
    /// 探索中に変化する状態
    pub state: SearchState,
}

impl SearchWorker {
    /// root quiet手の statScore を計算する（YO Step16 準拠）。
    ///
    /// YO の root では `contHist[0/1]` が sentinel を指すため、
    /// `2 * mainHistory + contHist0 + contHist1` は
    /// `2 * mainHistory + 2 * sentinel_contHistory` と等価になる。
    #[inline]
    fn root_quiet_stat_score(&self, mover: Color, mv: Move) -> i32 {
        let moved_piece = mv.moved_piece_after();
        let to_sq = mv.to();
        // SAFETY: 単一スレッド内で使用、可変参照と同時保持しない
        let main_hist =
            unsafe { self.history.as_ref_unchecked() }.main_history.get(mover, mv) as i32;
        // SAFETY:
        // - `cont_history_sentinel` は `SearchWorker::new()` で HistoryCell 内部テーブルを指して初期化される。
        // - HistoryCell の実体は SearchWorker のライフタイム中に再配置されない。
        // - ここでは読み取り専用で参照し、可変参照とは同時に保持しない。
        let cont_hist =
            unsafe { self.cont_history_sentinel.as_ref() }.get(moved_piece, to_sq) as i32;
        2 * main_hist + cont_hist + cont_hist
    }

    /// 新しいSearchWorkerを作成（YaneuraOu準拠: isreadyまたは最初のgo時）
    ///
    /// Box化してヒープに配置し、スタックオーバーフローを防ぐ。
    pub fn new(
        tt: Arc<TranspositionTable>,
        eval_hash: Arc<EvalHash>,
        max_moves_to_draw: i32,
        thread_id: usize,
        search_tune_params: SearchTuneParams,
    ) -> Box<Self> {
        let history = HistoryCell::new_boxed();
        // HistoryCell経由でsentinelポインタを取得
        // SAFETY: 初期化時のみ使用、他の参照と同時保持しない
        let cont_history_sentinel = {
            let h = unsafe { history.as_ref_unchecked() };
            NonNull::from(h.continuation_history[0][0].get_table(Piece::NONE, Square::SQ_11))
        };

        let mut worker = Box::new(Self {
            tt,
            eval_hash,
            history,
            cont_history_sentinel,
            generate_all_legal_moves: false,
            max_moves_to_draw,
            thread_id,
            allow_tt_write: true,
            search_tune_params,
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
            allow_tt_write: self.allow_tt_write,
            tune_params: &self.search_tune_params,
        }
    }

    /// ルート局面の static_eval を初期化し、未補正評価値を返す。
    ///
    /// in-check ノードで参照される `(ss-2)->staticEval` 相当を安定化するため、
    /// root 探索開始前に `stack[0].static_eval` を埋めておく。
    #[inline]
    /// ルート局面の static_eval を初期化し、(未補正評価値, correction_value) を返す。
    fn init_root_static_eval(&mut self, pos: &Position, root_in_check: bool) -> (Value, i32) {
        let unadjusted_static_eval = if root_in_check {
            Value::NONE
        } else {
            nnue_evaluate(&mut self.state, pos)
        };

        let (static_eval, corr) = if root_in_check || unadjusted_static_eval == Value::NONE {
            (Value::NONE, 0)
        } else {
            let ctx = self.create_context();
            let corr = correction_value(&self.state, &ctx, pos, 0);
            #[cfg(feature = "search-no-pass-rules")]
            let pass_rights_eval = Value::ZERO;
            #[cfg(not(feature = "search-no-pass-rules"))]
            let pass_rights_eval = evaluate_pass_rights(pos, pos.game_ply() as u16);

            let eval = to_corrected_static_eval(unadjusted_static_eval, corr) + pass_rights_eval;
            (eval, corr)
        };
        self.state.stack[0].static_eval = static_eval;
        (unadjusted_static_eval, corr)
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
        // SAFETY: 単一スレッド内で使用、可変参照と同時保持しない
        let table = {
            let h = unsafe { self.history.as_ref_unchecked() };
            NonNull::from(h.continuation_history[in_check_idx][capture_idx].get_table(piece, to))
        };
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
        // SAFETY: 探索開始前の初期化、他の参照と同時保持しない
        unsafe { self.history.as_mut_unchecked() }.low_ply_history.clear();
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
        mut depth: Depth,
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
        // YaneuraOu準拠: ss->statScore = 0 (yaneuraou-search.cpp:2193)
        self.state.stack[0].stat_score = 0;
        // YaneuraOu準拠: (ss+2)->cutoffCnt = 0 (yaneuraou-search.cpp:2194)
        self.state.stack[2].cutoff_cnt = 0;
        let (root_unadjusted_static_eval, root_correction_value) =
            self.init_root_static_eval(pos, root_in_check);

        // YaneuraOu準拠: ルートでもTTプローブを行う (yaneuraou-search.cpp:2257)
        let key = pos.key();
        let tt_result = self.tt.probe(key, pos);
        let tt_hit = tt_result.found;
        let tt_data = tt_result.data;
        // rootNode では ttMove = rootMoves[0] (yaneuraou-search.cpp:2279)
        let tt_move_root = self.state.root_moves[0].mv();
        let tt_value_root = if tt_hit {
            value_from_tt(tt_data.value, 0)
        } else {
            Value::NONE
        };
        // ttPv: PvNode(常にtrue) || (ttHit && is_pv)
        self.state.stack[0].tt_pv = true;
        self.state.stack[0].tt_hit = tt_hit;
        // ttCapture (root LMRで使用)
        let tt_capture_root = tt_move_root.is_some() && pos.capture_stage(tt_move_root);

        // PVをクリアして前回探索の残留を防ぐ
        // NOTE: YaneuraOuでは (ss+1)->pv = pv でポインタを新配列に向け、ss->pv[0] = Move::none() でクリア
        //       Vecベースの実装では明示的なclear()で同等の効果を得る
        self.state.stack[0].pv.clear();
        self.state.stack[1].pv.clear();

        // YaneuraOu準拠: quietsSearched, capturesSearched のトラッキング
        let mut quiets_tried = SearchedMoveList::new();
        let mut captures_tried = SearchedMoveList::new();
        let mut root_move_count = 0i32;

        for rm_idx in 0..self.state.root_moves.len() {
            if self.check_abort(limits, time_manager) {
                return Value::ZERO;
            }

            // 各手ごとにsel_depthをリセット（YaneuraOu準拠）
            self.state.sel_depth = 0;
            root_move_count = (rm_idx + 1) as i32;

            let mv = self.state.root_moves[rm_idx].mv();
            let gives_check = pos.gives_check(mv);
            let is_capture = pos.is_capture(mv);

            let nodes_before = self.state.nodes;

            // 探索
            let dirty_piece = pos.do_move_with_prefetch(mv, gives_check, self.tt.as_ref());
            self.nnue_push(dirty_piece);
            self.state.nodes += 1;
            self.state.stack[0].current_move = mv;
            // YaneuraOu準拠: ss->moveCount = ++moveCount (yaneuraou-search.cpp:3160)
            // 子ノード(ply 1)が(ss-1)->moveCountを参照するため設定必須
            self.state.stack[0].move_count = (rm_idx + 1) as i32;

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

            // PVS + LMR（YaneuraOu準拠: yaneuraou-search.cpp Step 17-19）
            // YOではsearch<Root>内でLMRが適用される。rootのPvNode+ttPvでは
            // reductionが大きく負になるため、zero-window検証がnewDepthより深くなる。
            let mut new_depth = depth - 1;
            // YaneuraOu準拠: rootでも全手で statScore を設定してから子ノードを探索する。
            // (moveCount==1 ではLMR補正には使われないが、子ノード側の統計参照で使われる)
            // YaneuraOu準拠: statScore（yaneuraou-search.cpp:3564-3572）
            let root_stat_score = if mv.is_pass() {
                0
            } else if is_capture {
                let captured = pos.captured_piece();
                let captured_pt = captured.piece_type();
                let moved_piece = mv.moved_piece_after();
                // SAFETY: 単一スレッド内で使用、可変参照と同時保持しない
                let hist = unsafe { self.history.as_ref_unchecked() }.capture_history.get(
                    moved_piece,
                    mv.to(),
                    captured_pt,
                ) as i32;
                self.search_tune_params.lmr_step16_capture_stat_scale_num * piece_value(captured)
                    / 128
                    + hist
            } else {
                let mover = !pos.side_to_move();
                self.root_quiet_stat_score(mover, mv)
            };
            self.state.stack[0].stat_score = root_stat_score;
            let value = if rm_idx == 0 {
                // 第1手: full depth PV search (Step 19)
                -self.search_node_wrapper::<{ NodeType::PV as u8 }>(
                    pos,
                    new_depth,
                    -beta,
                    -alpha,
                    1,
                    false,
                    limits,
                    time_manager,
                )
            } else if depth >= 2 && rm_idx >= 2 {
                // YaneuraOu準拠: depth >= 2 && moveCount > 1 + rootNode (rootNode=true → moveCount > 2)
                // 第3手以降(depth>=2時): LMR (Step 17) + PV re-search (Step 19)
                let (d, deeper_base, deeper_mul, shallower_thr) = {
                    let tune = &self.search_tune_params;
                    let delta = (beta.raw() - alpha.raw()).abs().max(1);
                    let root_delta = self.state.root_delta.max(1);
                    // root (ply 0) では improving = false (ply < 2)
                    let mut r =
                        reduction(tune, false, depth, (rm_idx + 1) as i32, delta, root_delta);

                    // ttPv調整（rootでは常にttPv=true, PvNode=true, cutNode=false）
                    let tt_value_higher = (tt_value_root > alpha) as i32;
                    let tt_depth_ge = (tt_data.depth >= depth) as i32;
                    r -= tune.lmr_step16_ttpv_sub_base
                        + tune.lmr_step16_ttpv_sub_pv_node
                        + tt_value_higher * tune.lmr_step16_ttpv_sub_tt_value
                        + tt_depth_ge * tune.lmr_step16_ttpv_sub_tt_depth;

                    // 基本調整
                    r += tune.lmr_step16_base_add;
                    r -= (rm_idx + 1) as i32 * tune.lmr_step16_move_count_mul;
                    r -= root_correction_value.abs() / tune.lmr_step16_correction_div.max(1);

                    if tt_capture_root {
                        r += tune.lmr_step16_tt_capture_add;
                    }
                    if self.state.stack[1].cutoff_cnt > 2 {
                        r += tune.lmr_step16_cutoff_count_add;
                    }
                    if mv == tt_move_root {
                        r -= tune.lmr_step16_tt_move_penalty;
                    }

                    let stat_score = self.state.stack[0].stat_score;
                    r -= stat_score * tune.lmr_step16_stat_score_scale_num / 8192;

                    // d計算 (YO: max(1, min(newDepth - r/1024, newDepth + 2)) + PvNode)
                    let d =
                        std::cmp::max(1, std::cmp::min(new_depth - r / 1024, new_depth + 2)) + 1; // +1 for PvNode
                    (
                        d,
                        tune.lmr_research_deeper_base,
                        tune.lmr_research_deeper_depth_mul,
                        tune.lmr_research_shallower_threshold,
                    )
                };

                // Step 17: LMR zero-window search
                self.state.stack[0].reduction = new_depth - d;
                let mut value = -self.search_node_wrapper::<{ NodeType::NonPV as u8 }>(
                    pos,
                    d,
                    -alpha - Value::new(1),
                    -alpha,
                    1,
                    true,
                    limits,
                    time_manager,
                );
                self.state.stack[0].reduction = 0;

                // LMR fail high後の deeper/shallower 調整
                if value > alpha {
                    let deeper_threshold = deeper_base + deeper_mul * new_depth;
                    let do_deeper =
                        d < new_depth && value > (best_value + Value::new(deeper_threshold));
                    let do_shallower = value < best_value + Value::new(shallower_thr);
                    new_depth += do_deeper as i32 - do_shallower as i32;

                    if new_depth > d {
                        value = -self.search_node_wrapper::<{ NodeType::NonPV as u8 }>(
                            pos,
                            new_depth,
                            -alpha - Value::new(1),
                            -alpha,
                            1,
                            false,
                            limits,
                            time_manager,
                        );
                    }
                }

                // Post LMR continuation history updates (yaneuraou-search.cpp:3644)
                // YO: update_continuation_histories(ss, movedPiece, move.to_sq(), 1365)
                // ply=0では (ss-i)->currentMove が全て無効のため全イテレーションがスキップされる（no-op）

                // Step 19: PV re-search (value > alpha の場合のみ)
                if value > alpha {
                    value = -self.search_node_wrapper::<{ NodeType::PV as u8 }>(
                        pos,
                        new_depth,
                        -beta,
                        -alpha,
                        1,
                        false,
                        limits,
                        time_manager,
                    );
                }

                value
            } else {
                // YaneuraOu準拠: LMR対象外の非第一手（第2手 or depth < 2）
                // PVS: zero-window search → PV re-search
                let mut value = -self.search_node_wrapper::<{ NodeType::NonPV as u8 }>(
                    pos,
                    new_depth,
                    -alpha - Value::new(1),
                    -alpha,
                    1,
                    true,
                    limits,
                    time_manager,
                );
                if value > alpha {
                    value = -self.search_node_wrapper::<{ NodeType::PV as u8 }>(
                        pos,
                        new_depth,
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

            // YaneuraOu準拠: 非best手のトラッキング (yaneuraou-search.cpp:3886-3893)
            // PASSはhistory未定義のためスキップ
            if !mv.is_pass() {
                if is_capture {
                    captures_tried.push(mv);
                } else {
                    quiets_tried.push(mv);
                }
            }

            // YaneuraOu準拠: Root move score/PV handling (yaneuraou-search.cpp:3727-3810)
            // moveCount == 1 (第1手) || value > alpha の場合にスコアとPVを更新
            let move_count = (rm_idx + 1) as i32;
            if move_count == 1 || value > alpha {
                let rm = &mut self.state.root_moves[rm_idx];
                rm.score = value;
                rm.sel_depth = self.state.sel_depth;
                rm.accumulate_score_stats(value);
                // PVを更新（第1手はfail lowでも常に更新 — YO準拠）
                rm.pv.truncate(1);
                rm.pv.extend_from_slice(&self.state.stack[1].pv);
                // 2番目以降の手がalphaを更新した場合にカウント
                if move_count > 1 {
                    self.state.best_move_changes += 1.0;
                }
            } else {
                // PV以外のすべての手は最低値に設定（YO準拠: yaneuraou-search.cpp:3804-3807）
                self.state.root_moves[rm_idx].score = Value::new(-Value::INFINITE.raw());
            }

            // bestValue/alpha更新（YaneuraOu準拠: yaneuraou-search.cpp:3835-3877）
            if value > best_value {
                best_value = value;

                if value > alpha {
                    alpha = value;
                    pv_idx = rm_idx;

                    if value >= beta {
                        break;
                    }

                    // YaneuraOu準拠 (yaneuraou-search.cpp:3866-3867):
                    // alpha改善後、以降の手の探索深さを削減する
                    if depth > 2 && depth < 14 && !value.is_mate_score() {
                        depth -= 2;
                    }
                }
            }
        }

        // YaneuraOu準拠 (yaneuraou-search.cpp:3917-3918):
        // fail highの場合に最良値を調整する
        if best_value >= beta && !best_value.is_mate_score() && !alpha.is_mate_score() {
            best_value = Value::new((best_value.raw() * depth + beta.raw()) / (depth + 1).max(1));
        }

        // =================================================================
        // History更新（YaneuraOu準拠: update_all_stats, yaneuraou-search.cpp:3939-5012）
        // =================================================================
        // rootでもbest moveが存在する場合にhistoryを更新する
        // 注: ply=0のためcontinuation historyの更新はスキップされる（ply < ply_back）
        // 注: prevSq == SQ_NONEのためfail-low countermove bonus/early refutation penaltyもスキップ
        {
            let best_move_for_stats = self.state.root_moves[pv_idx].mv();
            if best_move_for_stats.is_some() && !best_move_for_stats.is_pass() {
                let is_best_capture = pos.capture_stage(best_move_for_stats);
                let is_tt_move = best_move_for_stats == tt_move_root;
                let tune = self.search_tune_params;
                let bonus = stat_bonus(depth, is_tt_move, &tune);
                let malus = stat_malus(depth, root_move_count, &tune);
                let us = pos.side_to_move();
                let pawn_key_idx = pos.pawn_history_index();

                let best_moved_pc = pos.moved_piece(best_move_for_stats);
                let best_cont_pc = if best_move_for_stats.is_promotion() {
                    best_moved_pc.promote().unwrap_or(best_moved_pc)
                } else {
                    best_moved_pc
                };
                let best_to = best_move_for_stats.to();

                if !is_best_capture {
                    // Quiet best: update_quiet_histories (yaneuraou-search.cpp:4965-4973)
                    let scaled_bonus = bonus * tune.update_all_stats_quiet_bonus_scale_num / 1024;
                    let scaled_malus = malus * tune.update_all_stats_quiet_malus_scale_num / 1024;

                    {
                        // SAFETY: 単一スレッド内で使用、他の参照と同時保持しない
                        let h = unsafe { self.history.as_mut_unchecked() };
                        // MainHistory
                        h.main_history.update(us, best_move_for_stats, scaled_bonus);

                        // LowPlyHistory (ply=0 < LOW_PLY_HISTORY_SIZE)
                        let low_ply_bonus = low_ply_history_bonus(scaled_bonus, &tune);
                        h.low_ply_history.update(0, best_move_for_stats, low_ply_bonus);

                        // ContinuationHistory: ply=0では全てスキップ（ply >= ply_back が成立しない）

                        // PawnHistory
                        let pawn_bonus = pawn_history_bonus(scaled_bonus, &tune);
                        h.pawn_history.update(pawn_key_idx, best_cont_pc, best_to, pawn_bonus);

                        // 他のquiet手にペナルティ (yaneuraou-search.cpp:4972-4973)
                        for &m in quiets_tried.iter() {
                            if m != best_move_for_stats {
                                h.main_history.update(us, m, -scaled_malus);

                                let low_ply_malus = low_ply_history_bonus(-scaled_malus, &tune);
                                h.low_ply_history.update(0, m, low_ply_malus);

                                // ContinuationHistory: ply=0ではスキップ

                                let moved_pc = pos.moved_piece(m);
                                let cont_pc = if m.is_promotion() {
                                    moved_pc.promote().unwrap_or(moved_pc)
                                } else {
                                    moved_pc
                                };
                                let to = m.to();

                                let pawn_malus = pawn_history_bonus(-scaled_malus, &tune);
                                h.pawn_history.update(pawn_key_idx, cont_pc, to, pawn_malus);
                            }
                        }
                    }
                } else {
                    // Capture best: captureHistory更新 (yaneuraou-search.cpp:4980-4981)
                    let captured_pt = pos.piece_on(best_to).piece_type();
                    {
                        // SAFETY: 単一スレッド内で使用、他の参照と同時保持しない
                        let h = unsafe { self.history.as_mut_unchecked() };
                        h.capture_history.update(
                            best_cont_pc,
                            best_to,
                            captured_pt,
                            bonus * tune.update_all_stats_capture_bonus_scale_num / 1024,
                        );
                    }
                }

                // 他の捕獲手にペナルティ (yaneuraou-search.cpp:4999-5011)
                if !captures_tried.is_empty() {
                    {
                        // SAFETY: 単一スレッド内で使用、他の参照と同時保持しない
                        let h = unsafe { self.history.as_mut_unchecked() };
                        for &m in captures_tried.iter() {
                            if m != best_move_for_stats {
                                let moved_pc = pos.moved_piece(m);
                                let cont_pc = if m.is_promotion() {
                                    moved_pc.promote().unwrap_or(moved_pc)
                                } else {
                                    moved_pc
                                };
                                let to = m.to();
                                let captured_pt = pos.piece_on(to).piece_type();
                                h.capture_history.update(
                                    cont_pc,
                                    to,
                                    captured_pt,
                                    -malus * tune.update_all_stats_capture_malus_scale_num / 1024,
                                );
                            }
                        }
                    }
                }
            }
        }

        // 最善手を先頭に移動
        self.state.root_moves.move_to_front(pv_idx);
        self.state.root_moves.sort();

        // YaneuraOu準拠: ルートでもTTに保存する (yaneuraou-search.cpp:4034)
        // rootNode && !pvIdx (single-PV) かつ excludedMove なし → 常に save
        let best_move = self.state.root_moves[0].mv();
        if self.allow_tt_write {
            let bound = if best_value >= beta {
                Bound::Lower
            } else if best_move.is_some() {
                Bound::Exact
            } else {
                Bound::Upper
            };
            let stored_depth = if root_move_count != 0 {
                depth
            } else {
                (depth + 6).min(MAX_PLY - 1)
            };
            tt_result.write(
                key,
                value_to_tt(best_value, 0),
                true, // PvNode
                bound,
                stored_depth,
                best_move,
                root_unadjusted_static_eval,
                self.tt.generation(),
            );
        }

        // YaneuraOu準拠: rootでもCorrectionHistoryを更新する (yaneuraou-search.cpp:4048-4055)
        if !(root_in_check || best_move.is_some() && pos.is_capture(best_move))
            && (best_value > self.state.stack[0].static_eval) == best_move.is_some()
        {
            let static_eval = self.state.stack[0].static_eval;
            let divisor = if best_move.is_some() { 10 } else { 8 };
            let bonus = ((best_value.raw() - static_eval.raw()) * depth / divisor)
                .clamp(-CORRECTION_HISTORY_LIMIT / 4, CORRECTION_HISTORY_LIMIT / 4);
            let ctx = SearchContext {
                tt: &self.tt,
                eval_hash: &self.eval_hash,
                history: &self.history,
                cont_history_sentinel: self.cont_history_sentinel,
                generate_all_legal_moves: self.generate_all_legal_moves,
                max_moves_to_draw: self.max_moves_to_draw,
                thread_id: self.thread_id,
                allow_tt_write: self.allow_tt_write,
                tune_params: &self.search_tune_params,
            };
            update_correction_history(&self.state, &ctx, pos, 0, bonus);
        }

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
        // YaneuraOu準拠: rootNode && pvIdx の経路のみこの関数が担当する。
        // pv_idx == 0 は search_root() を使い、root TT save はそちらでのみ実行する。
        debug_assert!(pv_idx > 0);

        self.state.root_delta = (beta.raw() - alpha.raw()).abs().max(1);

        let mut alpha = alpha;
        let mut best_value = Value::new(-32001);
        let mut best_rm_idx = pv_idx;
        let root_in_check = pos.in_check();

        self.state.stack[0].in_check = root_in_check;
        self.state.stack[0].cont_history_ptr = self.cont_history_sentinel;
        self.state.stack[0].cont_hist_key = None;
        // YaneuraOu準拠: root探索開始時の初期化
        self.state.stack[0].stat_score = 0;
        self.state.stack[2].cutoff_cnt = 0;
        let (_root_unadjusted_static_eval, root_correction_value) =
            self.init_root_static_eval(pos, root_in_check);
        // YaneuraOu準拠: rootでもTT probeを行い、ttHit/ttPvを更新
        let key = pos.key();
        let tt_result = self.tt.probe(key, pos);
        let tt_hit = tt_result.found;
        let tt_data = tt_result.data;
        // rootNode && pvIdx 経路では rootMoves[pv_idx] を ttMove 相当として扱う。
        let tt_move_root = self.state.root_moves[pv_idx].mv();
        let tt_value_root = if tt_hit {
            value_from_tt(tt_data.value, 0)
        } else {
            Value::NONE
        };
        let tt_capture_root = tt_move_root.is_some() && pos.capture_stage(tt_move_root);
        self.state.stack[0].tt_hit = tt_hit;
        self.state.stack[0].tt_pv = true;

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
            // YaneuraOu準拠: ss->moveCount = ++moveCount (yaneuraou-search.cpp:3160)
            self.state.stack[0].move_count = (rm_idx + 1) as i32;

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

            // YaneuraOu準拠: statScore（yaneuraou-search.cpp:3564-3572）
            // do_move 後に計算する（captured_piece() / side_to_move() は do_move 後の状態を参照）
            self.state.stack[0].stat_score = if mv.is_pass() {
                0
            } else if is_capture {
                let captured = pos.captured_piece();
                let captured_pt = captured.piece_type();
                let moved_piece = mv.moved_piece_after();
                // SAFETY: 単一スレッド内で使用、可変参照と同時保持しない
                let hist = unsafe { self.history.as_ref_unchecked() }.capture_history.get(
                    moved_piece,
                    mv.to(),
                    captured_pt,
                ) as i32;
                self.search_tune_params.lmr_step16_capture_stat_scale_num * piece_value(captured)
                    / 128
                    + hist
            } else {
                let mover = !pos.side_to_move();
                self.root_quiet_stat_score(mover, mv)
            };

            let mut new_depth = depth - 1;

            // PVS: 最初の手（このPVラインの候補）はPV探索
            let value = if rm_idx == pv_idx {
                -self.search_node_wrapper::<{ NodeType::PV as u8 }>(
                    pos,
                    new_depth,
                    -beta,
                    -alpha,
                    1,
                    false,
                    limits,
                    time_manager,
                )
            } else {
                // YaneuraOu準拠: rootNode && pvIdx 経路でも Step17/18 の LMR を適用する。
                let (d, deeper_base, deeper_mul, shallower_thr) = {
                    let tune = &self.search_tune_params;
                    let delta = (beta.raw() - alpha.raw()).abs().max(1);
                    let root_delta = self.state.root_delta.max(1);
                    // root (ply 0) では improving = false (ply < 2)
                    let mut r =
                        reduction(tune, false, depth, (rm_idx + 1) as i32, delta, root_delta);

                    // ttPv調整（rootでは常にttPv=true, PvNode=true, cutNode=false）
                    let tt_value_higher = (tt_value_root > alpha) as i32;
                    let tt_depth_ge = (tt_data.depth >= depth) as i32;
                    r -= tune.lmr_step16_ttpv_sub_base
                        + tune.lmr_step16_ttpv_sub_pv_node
                        + tt_value_higher * tune.lmr_step16_ttpv_sub_tt_value
                        + tt_depth_ge * tune.lmr_step16_ttpv_sub_tt_depth;

                    // 基本調整
                    r += tune.lmr_step16_base_add;
                    r -= (rm_idx + 1) as i32 * tune.lmr_step16_move_count_mul;
                    r -= root_correction_value.abs() / tune.lmr_step16_correction_div.max(1);

                    if tt_capture_root {
                        r += tune.lmr_step16_tt_capture_add;
                    }
                    if self.state.stack[1].cutoff_cnt > 2 {
                        r += tune.lmr_step16_cutoff_count_add;
                    }
                    if mv == tt_move_root {
                        r -= tune.lmr_step16_tt_move_penalty;
                    }

                    let stat_score = self.state.stack[0].stat_score;
                    r -= stat_score * tune.lmr_step16_stat_score_scale_num / 8192;

                    // d計算 (YO: max(1, min(newDepth - r/1024, newDepth + 2)) + PvNode)
                    let d =
                        std::cmp::max(1, std::cmp::min(new_depth - r / 1024, new_depth + 2)) + 1; // +1 for PvNode
                    (
                        d,
                        tune.lmr_research_deeper_base,
                        tune.lmr_research_deeper_depth_mul,
                        tune.lmr_research_shallower_threshold,
                    )
                };

                // Step 17: LMR zero-window search
                self.state.stack[0].reduction = new_depth - d;
                let mut value = -self.search_node_wrapper::<{ NodeType::NonPV as u8 }>(
                    pos,
                    d,
                    -alpha - Value::new(1),
                    -alpha,
                    1,
                    true,
                    limits,
                    time_manager,
                );
                self.state.stack[0].reduction = 0;

                // LMR fail high後の deeper/shallower 調整
                if value > alpha {
                    let deeper_threshold = deeper_base + deeper_mul * new_depth;
                    let do_deeper =
                        d < new_depth && value > (best_value + Value::new(deeper_threshold));
                    let do_shallower = value < best_value + Value::new(shallower_thr);
                    new_depth += do_deeper as i32 - do_shallower as i32;

                    if new_depth > d {
                        value = -self.search_node_wrapper::<{ NodeType::NonPV as u8 }>(
                            pos,
                            new_depth,
                            -alpha - Value::new(1),
                            -alpha,
                            1,
                            false,
                            limits,
                            time_manager,
                        );
                    }
                }

                // YaneuraOu準拠: PvNodeでalphaを超えたら再探索（value<beta条件は付けない）
                if value > alpha {
                    value = -self.search_node_wrapper::<{ NodeType::PV as u8 }>(
                        pos,
                        new_depth,
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

        // YaneuraOu準拠: rootNode && pvIdx では TT 保存しない。
        // save条件は search<Root>() 側の `!excludedMove && !(rootNode && pvIdx)` と等価。
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
            allow_tt_write: self.allow_tt_write,
            tune_params: &self.search_tune_params,
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

        // スタック設定（YaneuraOu準拠: yaneuraou-search.cpp:2190-2194）
        st.stack[ply as usize].in_check = in_check;
        st.stack[ply as usize].move_count = 0;
        st.stack[ply as usize].stat_score = 0;
        // YaneuraOu準拠: (ss+2)->cutoffCnt = 0（祖父ノードがリセット）
        // 兄弟ノード間で cutoff_cnt が蓄積されるように ply+2 を初期化する
        st.stack[(ply + 2) as usize].cutoff_cnt = 0;

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
        // YaneuraOu準拠: priorCapture は「1手前が捕獲手か」を局面状態から判定
        let prior_capture = pos.captured_piece().is_some();

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
            cut_node,
        ) {
            ProbeOutcome::Continue(c) => c,
            ProbeOutcome::Cutoff {
                value,
                tt_move: cutoff_tt_move,
                tt_capture: cutoff_tt_capture,
            } => {
                inc_stat!(st, tt_cutoff);
                inc_stat_by_depth!(st, tt_cutoff_by_depth, depth);

                // YaneuraOu準拠: TTカットオフ時のヒストリ更新（yaneuraou-search.cpp:2376-2389）
                if cutoff_tt_move.is_some() && value.raw() >= beta.raw() {
                    // quiet ttMoveがfail-highした場合、quiet_historiesを更新
                    if !cutoff_tt_capture {
                        let bonus = (130 * depth - 71).min(1043);
                        {
                            // SAFETY: 単一スレッド内で使用、他の参照と同時保持しない
                            let h = unsafe { ctx.history.as_mut_unchecked() };
                            update_quiet_histories(
                                h,
                                &st.stack,
                                ctx.tune_params,
                                pos,
                                ply,
                                in_check,
                                cutoff_tt_move,
                                bonus,
                            );
                        }
                    }

                    // 1手前の早期quiet手へのペナルティ
                    // YaneuraOu: prevSq != SQ_NONE && (ss-1)->moveCount <= 4 && !priorCapture
                    if ply >= 1 {
                        let prev_ply = (ply - 1) as usize;
                        let prev_move_count = st.stack[prev_ply].move_count;
                        let prev_move = st.stack[prev_ply].current_move;
                        if prev_move.is_normal() && prev_move_count <= 4 && !prior_capture {
                            let prev_sq = prev_move.to();
                            let prev_piece = pos.piece_on(prev_sq);
                            let prev_in_check = st.stack[prev_ply].in_check;
                            {
                                // SAFETY: 単一スレッド内で使用、他の参照と同時保持しない
                                let h = unsafe { ctx.history.as_mut_unchecked() };
                                update_continuation_histories(
                                    h,
                                    &st.stack,
                                    ctx.tune_params,
                                    ply - 1,
                                    prev_in_check,
                                    prev_piece,
                                    prev_sq,
                                    -2142,
                                );
                            }
                        }
                    }
                }

                return value;
            }
        };
        let tt_move = tt_ctx.mv;
        let tt_value = tt_ctx.value;
        let tt_hit = tt_ctx.hit;
        let tt_data = tt_ctx.data;
        let _tt_capture = tt_ctx.capture;

        // 静的評価
        let eval_ctx =
            compute_eval_context(st, ctx, pos, ply, in_check, pv_node, &tt_ctx, excluded_move);
        let mut improving = eval_ctx.improving;
        let opponent_worsening = eval_ctx.opponent_worsening;

        // evalDiff によるヒストリ更新（YaneuraOu準拠: yaneuraou-search.cpp:2752-2758）
        // 条件: (ss-1)->currentMove が有効 && !(ss-1)->inCheck && !priorCapture
        if ply >= 1 {
            let prev_ply = (ply - 1) as usize;
            let prev_move = st.stack[prev_ply].current_move;
            let prev_in_check = st.stack[prev_ply].in_check;

            if prev_move.is_normal()
                && !prev_in_check
                && !prior_capture
                && eval_ctx.static_eval != Value::NONE
                && st.stack[prev_ply].static_eval != Value::NONE
            {
                let prev_eval = st.stack[prev_ply].static_eval.raw();
                let curr_eval = eval_ctx.static_eval.raw();
                // YaneuraOu/Stockfish準拠: 評価値の変化に基づくヒストリ更新
                // -(prev + curr): 相手の手で自分の評価が良くなったかを測定
                // clamp(-200, 156) + 58: 学習済みパラメータで正規化
                let eval_diff = (-(prev_eval + curr_eval)).clamp(-200, 156) + 58;
                let opponent = !pos.side_to_move();
                let prev_sq = prev_move.to();

                {
                    // SAFETY: 単一スレッド内で使用、他の参照と同時保持しない
                    let h = unsafe { ctx.history.as_mut_unchecked() };
                    // mainHistory 更新: evalDiff * 9 (YaneuraOu準拠)
                    h.main_history.update(opponent, prev_move, eval_diff * 9);

                    // pawnHistory 更新 (YaneuraOu準拠: yaneuraou-search.cpp:2754-2757)
                    // 条件:
                    // - !ttHit: TTヒット時はスキップ（既に十分な情報がある）
                    // - piece != Pawn: 「pawnHistory」は歩の配置に対する駒の評価履歴
                    //   （歩自体の手は対象外、駒を動かしたときの評価）
                    // - !promotion: 成り手は駒種が変わるため対象外
                    if !tt_hit {
                        let prev_piece = pos.piece_on(prev_sq);
                        if prev_piece.piece_type() != PieceType::Pawn && !prev_move.is_promotion() {
                            let pawn_idx = pos.pawn_history_index();
                            h.pawn_history.update(pawn_idx, prev_piece, prev_sq, eval_diff * 13);
                        }
                    }
                }
            }
        }

        // priorReduction に応じた深さ調整（yaneuraou-search.cpp:2769-2774）
        if prior_reduction
            >= if depth < ctx.tune_params.iir_depth_boundary {
                ctx.tune_params.iir_prior_reduction_threshold_shallow
            } else {
                ctx.tune_params.iir_prior_reduction_threshold_deep
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
                > Value::new(ctx.tune_params.iir_eval_sum_threshold)
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
            eval_ctx.eval,
            limits,
            time_manager,
        ) {
            return v;
        }

        // TT の手が駒取りかどうか判定
        let tt_capture = tt_move.is_some() && pos.capture_stage(tt_move);

        if let Some(v) = try_futility_pruning(
            FutilityParams {
                depth,
                beta,
                static_eval: eval_ctx.eval,
                correction_value: eval_ctx.correction_value,
                improving,
                opponent_worsening,
                tt_hit,
                tt_move_exists: tt_move.is_some(),
                tt_capture,
                tt_pv: st.stack[ply as usize].tt_pv,
                in_check,
            },
            ctx.tune_params,
        ) {
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
            cut_node,
            excluded_move,
            limits,
            time_manager,
            Self::search_node::<{ NodeType::NonPV as u8 }>,
        ) {
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
        // YaneuraOu準拠: contHist[0], contHist[1] の参照元はノード先頭で固定する。
        let cont_hist_ptr_1 = cont_history_ptr(st, ctx, ply, 1);
        let cont_hist_ptr_2 = cont_history_ptr(st, ctx, ply, 2);
        let mut mp =
            MovePicker::new(pos, tt_move, depth, ply, cont_tables, ctx.generate_all_legal_moves);

        // Singular Extension用の変数
        let tt_pv = st.stack[ply as usize].tt_pv;
        let root_node = NT == NodeType::Root as u8;

        // LMPが発火したかどうか
        let mut lmp_triggered = false;

        loop {
            // 次の手を取得（lazy generation）
            // SAFETY: 単一スレッド内で使用、可変参照と同時保持しない
            let mv = {
                let h = unsafe { ctx.history.as_ref_unchecked() };
                mp.next_move(pos, h)
            };
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

            let mut new_depth = depth - 1;
            let mut extension = 0i32;

            // =============================================================
            // Singular Extension（YaneuraOu準拠）
            // =============================================================
            // singular延長をするnodeであるか判定
            // 条件: !rootNode && move == ttMove && !excludedMove
            //       && is_valid(ttValue) && !is_decisive(ttValue) && (ttBound & BOUND_LOWER)
            //       && depth/ttDepth 条件（係数は tune_params 参照）
            if !root_node
                && mv == tt_move
                && excluded_move.is_none()
                && depth
                    >= ctx.tune_params.singular_min_depth_base
                        + ctx.tune_params.singular_min_depth_tt_pv_add * tt_pv as i32
                && tt_value != Value::NONE
                && !tt_value.is_mate_score()
                && tt_data.bound.is_lower_or_exact()
                && tt_data.depth >= depth - ctx.tune_params.singular_tt_depth_margin
            {
                let singular_beta_margin = (ctx.tune_params.singular_beta_margin_base
                    + ctx.tune_params.singular_beta_margin_tt_pv_non_pv_add
                        * (tt_pv && !pv_node) as i32)
                    * depth
                    / ctx.tune_params.singular_beta_margin_div;
                let singular_beta = tt_value - Value::new(singular_beta_margin);
                let singular_depth = new_depth / ctx.tune_params.singular_depth_div;

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
                    // YaneuraOu/Stockfish学習済みパラメータ（yaneuraou-search.cpp:3404-3411）
                    // YaneuraOu準拠パラメータ (yaneuraou-search.cpp:3419-3423)
                    let corr_val_adj =
                        eval_ctx.correction_value.abs() / ctx.tune_params.singular_corr_val_adj_div;
                    // SAFETY: 単一スレッド内で使用、可変参照と同時保持しない
                    let tt_move_hist =
                        unsafe { ctx.history.as_ref_unchecked() }.tt_move_history.get() as i32;
                    let double_margin = ctx.tune_params.singular_double_margin_base
                        + ctx.tune_params.singular_double_margin_pv_node * pv_node as i32
                        + ctx.tune_params.singular_double_margin_non_tt_capture
                            * !tt_capture as i32
                        - corr_val_adj
                        + ctx.tune_params.singular_double_margin_tt_move_hist_mult * tt_move_hist
                            / ctx.tune_params.singular_double_margin_tt_move_hist_div
                        - (ply > st.root_depth) as i32
                            * ctx.tune_params.singular_double_margin_late_ply_penalty;
                    let triple_margin = ctx.tune_params.singular_triple_margin_base
                        + ctx.tune_params.singular_triple_margin_pv_node * pv_node as i32
                        + ctx.tune_params.singular_triple_margin_non_tt_capture
                            * !tt_capture as i32
                        + ctx.tune_params.singular_triple_margin_tt_pv * tt_pv as i32
                        - corr_val_adj
                        - (ply * 2 > st.root_depth * 3) as i32
                            * ctx.tune_params.singular_triple_margin_late_ply_penalty;

                    extension = 1
                        + (singular_value < singular_beta - Value::new(double_margin)) as i32
                        + (singular_value < singular_beta - Value::new(triple_margin)) as i32;

                    // YaneuraOu準拠: singular確定時にdepthを+1（yaneuraou-search.cpp:3401）
                    depth += 1;
                } else if singular_value >= beta && !singular_value.is_mate_score() {
                    // Multi-Cut: 他の手もfail highする場合は枝刈り
                    // YaneuraOu準拠: TTMoveHistoryを更新
                    {
                        // SAFETY: 単一スレッド内で使用、他の参照と同時保持しない
                        let h = unsafe { ctx.history.as_mut_unchecked() };
                        h.tt_move_history
                            .update(super::tt_history::TTMoveHistory::multi_cut_bonus(depth));
                    }
                    inc_stat!(st, multi_cut);
                    return singular_value;
                } else if tt_value >= beta {
                    // Negative Extension: ttMoveが特別でない場合
                    extension = ctx.tune_params.singular_negative_extension_tt_fail_high;
                } else if cut_node {
                    extension = ctx.tune_params.singular_negative_extension_cut_node;
                }
            }

            // 注: YaneuraOu準拠で、new_depth += extension はdo_moveの後で行う（3482行目）
            // ここではextensionを保持しておき、枝刈りはextension反映前のnew_depthで判定

            // =============================================================
            // Reduction計算とStep14の枝刈り
            // =============================================================
            let delta = (beta.raw() - alpha.raw()).max(0);
            let mut r = reduction(
                ctx.tune_params,
                improving,
                depth,
                move_count,
                delta,
                st.root_delta.max(1),
            );

            // YaneuraOu準拠: ttPv時にreductionを増やす (yaneuraou-search.cpp:3215-3216)
            if st.stack[ply as usize].tt_pv {
                r += ctx.tune_params.lmr_ttpv_add;
            }

            let lmr_depth = new_depth - r / 1024;

            // =============================================================
            // LMP（Step14の前）
            // =============================================================
            // moveCount >= limitのとき、quiet手の生成をスキップ
            // YaneuraOu準拠: skip_quiet_moves()のみ、continueしない。
            // 現在の手はStep14の枝刈りで判定される。
            if !root_node && !best_value.is_loss() {
                let lmp_limit = (3 + depth * depth) / (2 - improving as i32);
                if move_count >= lmp_limit && !lmp_triggered {
                    mp.skip_quiets();
                    lmp_triggered = true;
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
                // SAFETY: cont_history_ptr() が返すポインタは探索中有効。
                cont_history_1: unsafe { cont_hist_ptr_1.as_ref() },
                // SAFETY: cont_history_ptr() が返すポインタは探索中有効。
                cont_history_2: unsafe { cont_hist_ptr_2.as_ref() },
                static_eval: eval_ctx.static_eval,
                alpha,
                best_move,
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
            // YaneuraOu準拠: ttPv大型補正（yaneuraou-search.cpp:3519-3521）
            // !ttHit時はtt_value=VALUE_NONE(32002)でほぼtrue、tt_data.depthはスロット残値
            let tt_value_higher = tt_value > alpha;
            let tt_depth_ge = tt_data.depth >= depth;

            if st.stack[ply as usize].tt_pv {
                r -= ctx.tune_params.lmr_step16_ttpv_sub_base
                    + (pv_node as i32) * ctx.tune_params.lmr_step16_ttpv_sub_pv_node
                    + (tt_value_higher as i32) * ctx.tune_params.lmr_step16_ttpv_sub_tt_value
                    + (tt_depth_ge as i32)
                        * (ctx.tune_params.lmr_step16_ttpv_sub_tt_depth
                            + (cut_node as i32) * ctx.tune_params.lmr_step16_ttpv_sub_cut_node);
            }

            // YaneuraOu準拠: 基本調整群（yaneuraou-search.cpp:3528-3532）
            r += ctx.tune_params.lmr_step16_base_add;
            r -= move_count * ctx.tune_params.lmr_step16_move_count_mul;
            r -= eval_ctx.correction_value.abs() / ctx.tune_params.lmr_step16_correction_div.max(1);

            // YaneuraOu準拠: cut_node（yaneuraou-search.cpp:3545-3546）
            if cut_node {
                let no_tt_move = !tt_hit || tt_move.is_none();
                r += ctx.tune_params.lmr_step16_cut_node_add
                    + ctx.tune_params.lmr_step16_cut_node_no_tt_add * (no_tt_move as i32);
            }

            // YaneuraOu準拠: ttCapture（yaneuraou-search.cpp:3551-3552）
            if tt_capture {
                r += ctx.tune_params.lmr_step16_tt_capture_add;
            }

            // YaneuraOu準拠: cutoffCnt（yaneuraou-search.cpp:3557-3558）
            if st.stack[(ply + 1) as usize].cutoff_cnt > 2 {
                r += ctx.tune_params.lmr_step16_cutoff_count_add
                    + (all_node as i32) * ctx.tune_params.lmr_step16_cutoff_count_all_node_add;
            }

            // YaneuraOu準拠: ttMove（yaneuraou-search.cpp:3563-3564）
            if mv == tt_move {
                r -= ctx.tune_params.lmr_step16_tt_move_penalty;
            }

            // YaneuraOu準拠: statScore（yaneuraou-search.cpp:3566-3579）
            let stat_score = if mv.is_pass() {
                0 // PASS は history がないので還元補正なし
            } else if is_capture {
                let captured = pos.captured_piece();
                let captured_pt = captured.piece_type();
                let moved_piece = mv.moved_piece_after();
                // SAFETY: 単一スレッド内で使用、可変参照と同時保持しない
                let hist = unsafe { ctx.history.as_ref_unchecked() }.capture_history.get(
                    moved_piece,
                    mv.to(),
                    captured_pt,
                ) as i32;
                ctx.tune_params.lmr_step16_capture_stat_scale_num * piece_value(captured) / 128
                    + hist
            } else {
                let moved_piece = mv.moved_piece_after();
                // SAFETY: 単一スレッド内で使用、可変参照と同時保持しない
                let main_hist =
                    unsafe { ctx.history.as_ref_unchecked() }.main_history.get(mover, mv) as i32;
                // SAFETY: cont_history_ptr() が返すポインタは探索中有効。
                let cont0 = unsafe { cont_hist_ptr_1.as_ref() }.get(moved_piece, mv.to()) as i32;
                // SAFETY: cont_history_ptr() が返すポインタは探索中有効。
                let cont1 = unsafe { cont_hist_ptr_2.as_ref() }.get(moved_piece, mv.to()) as i32;
                2 * main_hist + cont0 + cont1
            };
            st.stack[ply as usize].stat_score = stat_score;
            r -= stat_score * ctx.tune_params.lmr_step16_stat_score_scale_num / 8192;

            // =============================================================
            // 探索
            // =============================================================
            let mut value = if depth >= 2 && move_count > 1 {
                inc_stat!(st, lmr_applied);
                // YaneuraOu準拠: d = max(1, min(newDepth - r/1024, newDepth + 2)) + PvNode
                // 内側のmax(1, ...)で1以上が保証され、pv_node(0or1)加算で減ることはない
                let d = std::cmp::max(1, std::cmp::min(new_depth - r / 1024, new_depth + 2))
                    + pv_node as i32;

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

                let reduction_from_parent = new_depth - d;
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
                    let deeper_threshold = ctx.tune_params.lmr_research_deeper_base
                        + ctx.tune_params.lmr_research_deeper_depth_mul * new_depth;
                    let do_deeper =
                        d < new_depth && value > (best_value + Value::new(deeper_threshold));
                    let do_shallower = value
                        < best_value + Value::new(ctx.tune_params.lmr_research_shallower_threshold);
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
                        for offset in 1..=6 {
                            if st.stack[ply as usize].in_check && offset > 2 {
                                break;
                            }
                            let weight = match offset {
                                1 => ctx.tune_params.fail_high_continuation_weight_1,
                                2 => ctx.tune_params.fail_high_continuation_weight_2,
                                3 => ctx.tune_params.fail_high_continuation_weight_3,
                                4 => ctx.tune_params.fail_high_continuation_weight_4,
                                5 => ctx.tune_params.fail_high_continuation_weight_5,
                                6 => ctx.tune_params.fail_high_continuation_weight_6,
                                _ => 0,
                            };
                            let idx = ply - offset;
                            if idx < 0 {
                                break;
                            }
                            if let Some(key) = st.stack[idx as usize].cont_hist_key {
                                let in_check_idx = key.in_check as usize;
                                let capture_idx = key.capture as usize;
                                let bonus =
                                    ctx.tune_params.fail_high_continuation_base_num * weight / 1024
                                        + if offset < 2 {
                                            ctx.tune_params.fail_high_continuation_near_ply_offset
                                        } else {
                                            0
                                        };
                                {
                                    // SAFETY: 単一スレッド内で使用、他の参照と同時保持しない
                                    let h = unsafe { ctx.history.as_mut_unchecked() };
                                    h.continuation_history[in_check_idx][capture_idx].update(
                                        key.piece,
                                        key.to,
                                        moved_piece,
                                        to_sq,
                                        bonus,
                                    );
                                }
                            }
                        }
                    }
                    // beta cutoff 時の cutoff_cnt 更新は Step 20 のスコア更新で実施
                } else if value > alpha && value < best_value + Value::new(9) {
                    new_depth -= 1;
                }

                if pv_node && (move_count == 1 || value > alpha) {
                    // YaneuraOu準拠: ttMove由来のnewDepth下限補正 (yaneuraou-search.cpp:3692-3695)
                    if mv == tt_move
                        && ((tt_value != Value::NONE
                            && tt_value.is_mate_score()
                            && tt_data.depth > 0)
                            || (tt_data.depth > 1 && st.root_depth > 8))
                    {
                        new_depth = new_depth.max(1);
                    }
                    st.stack[ply as usize].reduction = 0;
                    -Self::search_node::<{ NodeType::PV as u8 }>(
                        st,
                        ctx,
                        pos,
                        new_depth,
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
                let mut non_lmr_depth = new_depth;
                if tt_move.is_none() {
                    r += 1118;
                }
                non_lmr_depth -= (r > 3212) as i32;
                non_lmr_depth -= (r > 4784 && new_depth > 2) as i32;

                st.stack[ply as usize].reduction = 0;
                let mut value = -Self::search_node::<{ NodeType::NonPV as u8 }>(
                    st,
                    ctx,
                    pos,
                    non_lmr_depth,
                    -alpha - Value::new(1),
                    -alpha,
                    ply + 1,
                    !cut_node,
                    limits,
                    time_manager,
                );
                st.stack[ply as usize].reduction = 0;

                // YaneuraOu Step18準拠:
                // PvNodeでは zero-window で alpha を超えたら full PV再探索する。
                if pv_node && value > alpha {
                    // YaneuraOu準拠: ttMove由来のnewDepth下限補正 (yaneuraou-search.cpp:3692-3695)
                    if mv == tt_move
                        && ((tt_value != Value::NONE
                            && tt_value.is_mate_score()
                            && tt_data.depth > 0)
                            || (tt_data.depth > 1 && st.root_depth > 8))
                    {
                        new_depth = new_depth.max(1);
                    }
                    st.stack[ply as usize].reduction = 0;
                    value = -Self::search_node::<{ NodeType::PV as u8 }>(
                        st,
                        ctx,
                        pos,
                        new_depth,
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
                // YaneuraOu準拠: ttMove由来のnewDepth下限補正 (yaneuraou-search.cpp:3692-3695)
                if mv == tt_move
                    && ((tt_value != Value::NONE && tt_value.is_mate_score() && tt_data.depth > 0)
                        || (tt_data.depth > 1 && st.root_depth > 8))
                {
                    new_depth = new_depth.max(1);
                }
                st.stack[ply as usize].reduction = 0;
                -Self::search_node::<{ NodeType::PV as u8 }>(
                    st,
                    ctx,
                    pos,
                    new_depth,
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
            // YaneuraOu準拠 (yaneuraou-search.cpp:3832-3833):
            // Lazy SMP多様化のため、リーフ付近で同点の手を確率的に昇格させる
            let inc = Value::new(
                if value == best_value
                    && ply + 2 >= st.root_depth
                    && (st.nodes as i32 & 14) == 0
                    && !Value::new(value.raw().abs() + 1).is_win()
                {
                    1
                } else {
                    0
                },
            );

            if value + inc > best_value {
                best_value = value;

                if value + inc > alpha {
                    best_move = mv;
                    // PV更新
                    if pv_node {
                        // 借用チェッカーの制約を避けるためクローン
                        let child_pv = st.stack[(ply + 1) as usize].pv.clone();
                        st.stack[ply as usize].update_pv(mv, &child_pv);
                    }

                    if value >= beta {
                        // extension < 2 または PvNode の場合のみインクリメント
                        st.stack[ply as usize].cutoff_cnt += (extension < 2 || pv_node) as i32;
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
                    // YaneuraOu準拠: fail-high しなかったとき、後続手に対して探索深さをやや下げる。
                    if depth > 2 && depth < 14 && !value.is_mate_score() {
                        depth -= 2;
                    }
                    // YaneuraOu準拠: fail-high しなかった場合のみ alpha を更新する。
                    alpha = value;
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

        // YaneuraOu準拠: fail-high のときは lower bound の過大化を抑えるため
        // best_value を beta 側へ寄せてから後段の更新に渡す。
        if best_value >= beta && !best_value.is_mate_score() && !alpha.is_mate_score() {
            best_value = Value::new((best_value.raw() * depth + beta.raw()) / (depth + 1).max(1));
        }

        // =================================================================
        // History更新（YaneuraOu準拠: update_all_stats）
        // =================================================================
        // YaneuraOu: bestMoveがある場合は常にupdate_all_statsを呼ぶ
        // PASS は history_index() が未定義のためスキップ
        if best_move.is_some() && !best_move.is_pass() {
            let is_best_capture = pos.is_capture(best_move);
            let is_tt_move = best_move == tt_move;
            // YaneuraOu準拠: bonus = min(121*depth-77, 1633) + 375*(bestMove==ttMove)
            let bonus = stat_bonus(depth, is_tt_move, ctx.tune_params);
            // YaneuraOu準拠: malus = min(825*depth-196, 2159) - 16*moveCount
            let malus = stat_malus(depth, move_count, ctx.tune_params);
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
                // Quiet手がbest: update_quiet_histories(bestMove, bonus * 881 / 1024)相当
                // YaneuraOu準拠: bonus * 881 / 1024をベースに各historyを更新
                let scaled_bonus =
                    bonus * ctx.tune_params.update_all_stats_quiet_bonus_scale_num / 1024;

                // 他のquiet手にはペナルティ
                // YaneuraOu: update_quiet_histories(move, -quietMalus * 1083 / 1024)
                let scaled_malus =
                    malus * ctx.tune_params.update_all_stats_quiet_malus_scale_num / 1024;

                // History更新をまとめて実行
                {
                    // SAFETY: 単一スレッド内で使用、他の参照と同時保持しない
                    let h = unsafe { ctx.history.as_mut_unchecked() };
                    // MainHistory: そのまま渡す
                    h.main_history.update(us, best_move, scaled_bonus);

                    // LowPlyHistory: bonus * 761 / 1024
                    if ply < LOW_PLY_HISTORY_SIZE as i32 {
                        let low_ply_bonus = low_ply_history_bonus(scaled_bonus, ctx.tune_params);
                        h.low_ply_history.update(ply as usize, best_move, low_ply_bonus);
                    }

                    // ContinuationHistory: (bonus * 955 / 1024) * weight / 1024 + 88*(i<2)
                    // YaneuraOu準拠: update_quiet_histories → update_continuation_histories
                    let cont_scaled_bonus =
                        scaled_bonus * ctx.tune_params.continuation_history_multiplier / 1024;
                    for ply_back in 1..=6 {
                        if ply_back > max_ply_back {
                            continue;
                        }
                        let weight = continuation_history_weight(ctx.tune_params, ply_back);
                        if ply >= ply_back as i32 {
                            let prev_ply = (ply - ply_back as i32) as usize;
                            if let Some(key) = st.stack[prev_ply].cont_hist_key {
                                let in_check_idx = key.in_check as usize;
                                let capture_idx = key.capture as usize;
                                let weighted_bonus = continuation_history_bonus_with_offset(
                                    cont_scaled_bonus * weight / 1024,
                                    ply_back,
                                    ctx.tune_params,
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

                    // PawnHistory: bonus * (pos ? 850 : 550) / 1024
                    let pawn_bonus = pawn_history_bonus(scaled_bonus, ctx.tune_params);
                    h.pawn_history.update(pawn_key_idx, best_cont_pc, best_to, pawn_bonus);

                    // 他のquiet手にはペナルティ
                    for &m in quiets_tried.iter() {
                        if m != best_move {
                            // MainHistory
                            h.main_history.update(us, m, -scaled_malus);

                            // LowPlyHistory（現行欠落していたので追加）
                            if ply < LOW_PLY_HISTORY_SIZE as i32 {
                                let low_ply_malus =
                                    low_ply_history_bonus(-scaled_malus, ctx.tune_params);
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
                            // YaneuraOu準拠: -malus * 1083/1024 * 955/1024 * weight/1024 + 88*(i<2)
                            let cont_scaled_malus = -scaled_malus
                                * ctx.tune_params.continuation_history_multiplier
                                / 1024;
                            for ply_back in 1..=6 {
                                if ply_back > max_ply_back {
                                    continue;
                                }
                                let weight = continuation_history_weight(ctx.tune_params, ply_back);
                                if ply >= ply_back as i32 {
                                    let prev_ply = (ply - ply_back as i32) as usize;
                                    if let Some(key) = st.stack[prev_ply].cont_hist_key {
                                        let in_check_idx = key.in_check as usize;
                                        let capture_idx = key.capture as usize;
                                        let weighted_malus = continuation_history_bonus_with_offset(
                                            cont_scaled_malus * weight / 1024,
                                            ply_back,
                                            ctx.tune_params,
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
                            let pawn_malus = pawn_history_bonus(-scaled_malus, ctx.tune_params);
                            h.pawn_history.update(pawn_key_idx, cont_pc, to, pawn_malus);
                        }
                    }
                }
            } else {
                // 捕獲手がbest: captureHistoryを更新
                let captured_pt = pos.piece_on(best_to).piece_type();
                // YaneuraOu準拠: captureHistory best moveにスケーリング適用（yaneuraou-search.cpp:4983）
                {
                    // SAFETY: 単一スレッド内で使用、他の参照と同時保持しない
                    let h = unsafe { ctx.history.as_mut_unchecked() };
                    h.capture_history.update(
                        best_cont_pc,
                        best_to,
                        captured_pt,
                        bonus * ctx.tune_params.update_all_stats_capture_bonus_scale_num / 1024,
                    );
                }
            }

            // YaneuraOu準拠: 他の捕獲手へのペナルティもmalusを共通使用
            {
                // SAFETY: 単一スレッド内で使用、他の参照と同時保持しない
                let h = unsafe { ctx.history.as_mut_unchecked() };
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
                            -malus * ctx.tune_params.update_all_stats_capture_malus_scale_num
                                / 1024,
                        );
                    }
                }
            }

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
                        let penalty_base = -malus
                            * ctx.tune_params.update_all_stats_early_refutation_penalty_scale_num
                            / 1024;
                        // YaneuraOu: update_continuation_histories(ss - 1, ...) で (ss - 1)->inCheck を参照
                        let prev_in_check = st.stack[prev_ply].in_check;
                        let prev_max_ply_back = if prev_in_check { 2 } else { 6 };

                        {
                            // SAFETY: 単一スレッド内で使用、他の参照と同時保持しない
                            let h = unsafe { ctx.history.as_mut_unchecked() };
                            for ply_back in 1..=6 {
                                if ply_back > prev_max_ply_back {
                                    continue;
                                }
                                let weight = continuation_history_weight(ctx.tune_params, ply_back);
                                // ss - 1 からさらに ply_back 手前 = ply - 1 - ply_back
                                let target_ply = ply - 1 - ply_back as i32;
                                if target_ply >= 0 {
                                    if let Some(target_key) =
                                        st.stack[target_ply as usize].cont_hist_key
                                    {
                                        let in_check_idx = target_key.in_check as usize;
                                        let capture_idx = target_key.capture as usize;
                                        // YaneuraOu準拠: 88 * (i < 2) → ply_back=1 のみ
                                        let weighted_penalty = penalty_base * weight / 1024
                                            + if ply_back < 2 {
                                                ctx.tune_params.continuation_history_near_ply_offset
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
                        }
                    }
                }
            }

            // TTMoveHistory更新（非PVノードのみ、YaneuraOu準拠）
            // YaneuraOu: ttMoveHistory << (bestMove == ttData.move ? 809 : -865)
            if !pv_node && tt_move.is_some() {
                let bonus = if best_move == tt_move {
                    ctx.tune_params.tt_move_history_bonus
                } else {
                    ctx.tune_params.tt_move_history_malus
                };
                // SAFETY: 単一スレッド内で使用、他の参照と同時保持しない
                unsafe { ctx.history.as_mut_unchecked() }.tt_move_history.update(bonus);
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
                    let mut bonus_scale: i32 =
                        ctx.tune_params.prior_quiet_countermove_bonus_scale_base;
                    bonus_scale -= parent_stat_score
                        / ctx.tune_params.prior_quiet_countermove_parent_stat_div.max(1);
                    bonus_scale += (ctx.tune_params.prior_quiet_countermove_depth_mul * depth)
                        .min(ctx.tune_params.prior_quiet_countermove_depth_cap);
                    bonus_scale += ctx.tune_params.prior_quiet_countermove_move_count_bonus
                        * (parent_move_count > 8) as i32;
                    bonus_scale += ctx.tune_params.prior_quiet_countermove_eval_bonus
                        * (!in_check
                            && static_eval != Value::NONE
                            && best_value
                                <= static_eval
                                    - Value::new(
                                        ctx.tune_params.prior_quiet_countermove_eval_margin,
                                    )) as i32;
                    bonus_scale += ctx.tune_params.prior_quiet_countermove_parent_eval_bonus
                        * (!parent_in_check
                            && parent_static_eval != Value::NONE
                            && best_value
                                <= -parent_static_eval
                                    - Value::new(
                                        ctx.tune_params.prior_quiet_countermove_parent_eval_margin,
                                    )) as i32;
                    bonus_scale = bonus_scale.max(0);

                    // 値域: bonus_scale ≥ 0, min(...) ∈ [52, 1365] (depth>=1)
                    // i64で計算してオーバーフローを防止
                    let scaled_bonus = (ctx.tune_params.prior_quiet_countermove_scaled_depth_mul
                        * depth
                        + ctx.tune_params.prior_quiet_countermove_scaled_offset)
                        .min(ctx.tune_params.prior_quiet_countermove_scaled_cap)
                        as i64
                        * bonus_scale as i64;

                    // continuation history更新
                    // YaneuraOu: update_continuation_histories(ss - 1, pos.piece_on(prevSq), prevSq, scaledBonus * 400 / 32768)
                    // 注: prev_sq は cont_hist_key.to（do_move後に設定）なので、
                    //     この時点で prev_piece != NONE が保証される
                    let prev_piece = pos.piece_on(prev_sq);
                    let prev_max_ply_back = if parent_in_check { 2 } else { 6 };
                    let cont_bonus = (scaled_bonus
                        * ctx.tune_params.prior_quiet_countermove_cont_scale_num as i64
                        / 32768) as i32;

                    // main history更新
                    // YaneuraOu: mainHistory[~us][((ss - 1)->currentMove).raw()] << scaledBonus * 220 / 32768
                    let prev_move = st.stack[prev_ply].current_move;
                    let main_bonus = (scaled_bonus
                        * ctx.tune_params.prior_quiet_countermove_main_scale_num as i64
                        / 32768) as i32;
                    // 注: 前の手なので手番は!pos.side_to_move()
                    let opponent = !pos.side_to_move();

                    // pawn history更新（歩以外かつ成りでない場合）
                    // YaneuraOu: if (type_of(pos.piece_on(prevSq)) != PAWN && ((ss - 1)->currentMove).type_of() != PROMOTION)
                    let pawn_key_idx = pos.pawn_history_index();
                    let pawn_bonus = (scaled_bonus
                        * ctx.tune_params.prior_quiet_countermove_pawn_scale_num as i64
                        / 32768) as i32;
                    let update_pawn =
                        prev_piece.piece_type() != PieceType::Pawn && !prev_move.is_promotion();

                    {
                        // SAFETY: 単一スレッド内で使用、他の参照と同時保持しない
                        let h = unsafe { ctx.history.as_mut_unchecked() };
                        for ply_back in 1..=6 {
                            if ply_back > prev_max_ply_back {
                                continue;
                            }
                            let weight = continuation_history_weight(ctx.tune_params, ply_back);
                            // ss - 1 からさらに ply_back 手前 = ply - 1 - ply_back
                            let target_ply = ply - 1 - ply_back as i32;
                            if target_ply >= 0 {
                                if let Some(target_key) =
                                    st.stack[target_ply as usize].cont_hist_key
                                {
                                    let in_check_idx = target_key.in_check as usize;
                                    let capture_idx = target_key.capture as usize;
                                    // YaneuraOu準拠: 88 * (i < 2) → ply_back=1 のみ
                                    let weighted_bonus = cont_bonus * weight / 1024
                                        + if ply_back < 2 {
                                            ctx.tune_params.continuation_history_near_ply_offset
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
                    }
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
                        // SAFETY: 単一スレッド内で使用、他の参照と同時保持しない
                        unsafe { ctx.history.as_mut_unchecked() }.capture_history.update(
                            prev_piece,
                            prev_sq,
                            captured_piece.piece_type(),
                            ctx.tune_params.prior_capture_countermove_bonus,
                        );
                    }
                }
            }
        }

        // =================================================================
        // ttPv の上方伝播（YaneuraOu準拠: yaneuraou-search.cpp:4007-4008）
        // =================================================================
        // fail low 時、親ノードが PV ライン上だったなら現ノードも PV ライン上として扱う。
        // これにより LMR の reduction 調整や Futility Pruning の抑制が適切に機能する。
        if best_value <= alpha {
            st.stack[ply as usize].tt_pv =
                st.stack[ply as usize].tt_pv || st.stack[(ply - 1) as usize].tt_pv;
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
            let stored_depth = if move_count != 0 {
                depth
            } else {
                (depth + 6).min(MAX_PLY - 1)
            };

            #[cfg(feature = "tt-trace")]
            let allow_write = ctx.allow_tt_write
                && helper_tt_write_enabled_for_depth(ctx.thread_id, bound, stored_depth);
            #[cfg(not(feature = "tt-trace"))]
            let allow_write = ctx.allow_tt_write;
            if allow_write {
                #[cfg(feature = "tt-trace")]
                maybe_trace_tt_write(TtWriteTrace {
                    stage: "ab_store",
                    thread_id: ctx.thread_id,
                    ply,
                    key: tt_ctx.key,
                    depth: stored_depth,
                    bound,
                    is_pv: st.stack[ply as usize].tt_pv,
                    tt_move: best_move,
                    stored_value: value_to_tt(best_value, ply),
                    eval: eval_ctx.unadjusted_static_eval,
                    root_move: if ply >= 1 {
                        st.stack[0].current_move
                    } else {
                        Move::NONE
                    },
                });
                tt_ctx.result.write(
                    tt_ctx.key,
                    value_to_tt(best_value, ply),
                    st.stack[ply as usize].tt_pv,
                    bound,
                    stored_depth,
                    best_move,
                    eval_ctx.unadjusted_static_eval,
                    ctx.tt.generation(),
                );
                inc_stat_by_depth!(st, tt_write_by_depth, stored_depth);
            }
        }

        // CorrectionHistoryの更新（YaneuraOu準拠: yaneuraou-search.cpp:4048-4055）
        // 条件: !inCheck && !(bestMove && capture(bestMove))
        //        && (bestValue > staticEval) == bool(bestMove)
        // bestMove有無で除数が異なる（有: /10, 無: /8）
        // YOと同順序で、ttPv伝播・TT保存の後に更新する。
        if !(in_check || best_move.is_some() && pos.is_capture(best_move))
            && (best_value > st.stack[ply as usize].static_eval) == best_move.is_some()
        {
            let static_eval = st.stack[ply as usize].static_eval;
            let divisor = if best_move.is_some() { 10 } else { 8 };
            let bonus = ((best_value.raw() - static_eval.raw()) * depth / divisor)
                .clamp(-CORRECTION_HISTORY_LIMIT / 4, CORRECTION_HISTORY_LIMIT / 4);
            update_correction_history(st, ctx, pos, ply, bonus);
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
