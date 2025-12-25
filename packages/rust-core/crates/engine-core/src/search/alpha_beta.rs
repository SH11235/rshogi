//! Alpha-Beta探索の実装
//!
//! YaneuraOu準拠のAlpha-Beta探索。
//! - Principal Variation Search (PVS)
//! - 静止探索 (Quiescence Search)
//! - 各種枝刈り: NMP, LMR, Futility, Razoring, SEE, Singular Extension

use std::ptr::NonNull;
use std::sync::Arc;

use crate::nnue::{evaluate, AccumulatorStack, DirtyPiece};
use crate::position::Position;
use crate::search::PieceToHistory;
use crate::tt::{ProbeResult, TTData, TranspositionTable};
use crate::types::{
    Bound, Color, Depth, Move, Piece, PieceType, Square, Value, DEPTH_QS, DEPTH_UNSEARCHED, MAX_PLY,
};

use super::history::{
    capture_malus, continuation_history_bonus_with_offset, low_ply_history_bonus,
    pawn_history_bonus, quiet_malus, stat_bonus, HistoryTables,
    CONTINUATION_HISTORY_NEAR_PLY_OFFSET, CONTINUATION_HISTORY_WEIGHTS, CORRECTION_HISTORY_LIMIT,
    CORRECTION_HISTORY_SIZE, LOW_PLY_HISTORY_SIZE, PRIOR_CAPTURE_COUNTERMOVE_BONUS,
    TT_MOVE_HISTORY_BONUS, TT_MOVE_HISTORY_MALUS,
};
use super::movepicker::piece_value;
use super::types::{
    draw_value, init_stack_array, value_from_tt, value_to_tt, ContHistKey, NodeType,
    OrderedMovesBuffer, RootMoves, SearchedMoveList, StackArray, STACK_SIZE,
};
use super::{LimitsType, MovePicker, TimeManagement};

// =============================================================================
// 定数
// =============================================================================

/// Futility margin（depth × 係数）
///
/// YaneuraOu準拠での基準係数。枝刈りでtt未ヒットのカットノードは係数を少し下げる。
const FUTILITY_MARGIN_BASE: i32 = 90;

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
fn draw_jitter(nodes: u64) -> i32 {
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
fn to_corrected_static_eval(unadjusted: Value, correction_value: i32) -> Value {
    let corrected = unadjusted.raw() + correction_value / 131_072;
    Value::new(corrected.clamp(Value::MATED_IN_MAX_PLY.raw() + 1, Value::MATE_IN_MAX_PLY.raw() - 1))
}

/// LMR用のreduction配列（YaneuraOu準拠の1次元テーブル）
type Reductions = [i32; 64];

// YaneuraOuのreduction式で用いる定数（yaneuraou-search.cpp:3163-3170,4759）
const REDUCTION_DELTA_SCALE: i32 = 731;
const REDUCTION_NON_IMPROVING_MULT: i32 = 216;
const REDUCTION_NON_IMPROVING_DIV: i32 = 512;
const REDUCTION_BASE_OFFSET: i32 = 1089;

/// Reduction配列（LazyLockによる遅延初期化）
/// 初回アクセス時に自動初期化されるため、get()呼び出しが不要
static REDUCTIONS: LazyLock<Reductions> = LazyLock::new(|| {
    let mut table: Reductions = [0; 64];
    // YaneuraOu: reductions[i] = int(2782 / 128.0 * log(i)) （yaneuraou-search.cpp:1818）
    for (i, value) in table.iter_mut().enumerate().skip(1) {
        *value = (2782.0 / 128.0 * (i as f64).ln()) as i32;
    }
    table
});

/// Reductionを取得
///
/// LazyLockにより初回アクセス時に自動初期化されるため、panicしない。
#[inline]
fn reduction(imp: bool, depth: i32, move_count: i32, delta: i32, root_delta: i32) -> i32 {
    if depth <= 0 || move_count <= 0 {
        return 0;
    }

    let d = depth.clamp(1, 63) as usize;
    let mc = move_count.clamp(1, 63) as usize;
    // LazyLockにより直接アクセス可能（get()不要）
    let reduction_scale = REDUCTIONS[d] * REDUCTIONS[mc];
    let root_delta = root_delta.max(1);
    let delta = delta.max(0);

    // YaneuraOuのreduction式（yaneuraou-search.cpp:3163-3170,4759）
    // 1024倍スケールで返す。ttPv加算は呼び出し側で行う。
    reduction_scale - delta * REDUCTION_DELTA_SCALE / root_delta
        + (!imp as i32) * reduction_scale * REDUCTION_NON_IMPROVING_MULT
            / REDUCTION_NON_IMPROVING_DIV
        + REDUCTION_BASE_OFFSET
}

/// 置換表プローブの結果をまとめたコンテキスト
///
/// TTプローブ後の即時カットオフ判定や、後続の枝刈りロジックで使用される。
struct TTContext {
    key: u64,
    result: ProbeResult,
    data: TTData,
    hit: bool,
    mv: Move,
    value: Value,
    capture: bool,
}

/// 置換表プローブの結果（続行 or カットオフ）
enum ProbeOutcome {
    /// 探索続行（TTContext付き）
    Continue(TTContext),
    /// 即時カットオフ値
    Cutoff(Value),
}

/// 静的評価まわりの情報をまとめたコンテキスト
struct EvalContext {
    static_eval: Value,
    unadjusted_static_eval: Value,
    correction_value: i32,
    /// 2手前と比較して局面が改善しているか
    improving: bool,
    /// 相手側の局面が悪化しているか
    opponent_worsening: bool,
}

/// Step14の枝刈り判定結果
enum Step14Outcome {
    /// 枝刈りする（best_value を更新する場合のみ付随）
    Skip { best_value: Option<Value> },
    /// 続行し、lmr_depth を返す
    Continue,
}

/// Futility判定に必要な情報をまとめたパラメータ
#[derive(Clone, Copy)]
struct FutilityParams {
    depth: Depth,
    beta: Value,
    static_eval: Value,
    correction_value: i32,
    improving: bool,
    opponent_worsening: bool,
    cut_node: bool,
    tt_hit: bool,
    pv_node: bool,
    in_check: bool,
}

/// Step14 の枝刈りに必要な文脈
struct Step14Context<'a> {
    pos: &'a Position,
    mv: Move,
    depth: Depth,
    ply: i32,
    improving: bool,
    best_move: Move,
    best_value: Value,
    alpha: Value,
    in_check: bool,
    gives_check: bool,
    is_capture: bool,
    lmr_depth: i32,
    mover: Color,
    move_count: i32,
    cont_history_1: &'a PieceToHistory,
    cont_history_2: &'a PieceToHistory,
}

// =============================================================================
// SearchWorker
// =============================================================================

/// 探索用のワーカー状態
///
/// YaneuraOu準拠: Workerはゲーム全体で再利用される。
/// 履歴統計は直接メンバとして保持し、usinewgameでクリア、goでは保持。
pub struct SearchWorker {
    /// 置換表への共有参照（Arc）
    pub tt: Arc<TranspositionTable>,

    /// スレッドID（0=main）
    pub thread_id: usize,

    /// スレッドごとの探索スキップ設定（Lazy SMP用）
    skip_size: usize,
    skip_phase: usize,

    // =========================================================================
    // 履歴統計（YaneuraOu準拠: 直接メンバとして保持）
    // =========================================================================
    // HistoryTablesを単一のヒープ領域として確保し、履歴テーブルを連続配置する。
    // `Box::new_zeroed` で一括確保することで、巨大配列のスタック確保を避ける。
    // =========================================================================
    /// 履歴/統計テーブル群
    pub history: Box<HistoryTables>,

    /// ContinuationHistoryのsentinel（YaneuraOu方式）
    pub cont_history_sentinel: NonNull<PieceToHistory>,

    // =========================================================================
    // 探索状態（毎回リセット）
    // =========================================================================
    /// ルート手
    pub root_moves: RootMoves,

    /// 探索スタック
    pub stack: StackArray,

    /// 探索ノード数
    pub nodes: u64,

    /// 選択的深さ
    pub sel_depth: i32,

    /// ルート深さ
    pub root_depth: Depth,

    /// ルートでのウィンドウ幅（beta - alpha）。YaneuraOuのLMRスケール用。
    pub root_delta: i32,

    /// 探索完了済み深さ
    pub completed_depth: Depth,

    /// 全合法手生成フラグ（YaneuraOu互換）
    pub generate_all_legal_moves: bool,

    /// 最善手
    pub best_move: Move,

    /// 中断フラグ
    pub abort: bool,

    /// 最善手変更カウンター（PV安定性判断用）
    ///
    /// YaneuraOu準拠: move_count > 1 && !pvIdx の時にインクリメント
    /// 反復深化の各世代で /= 2 して減衰させる
    pub best_move_changes: f64,

    /// 引き分けまでの最大手数（YaneuraOu準拠）
    pub max_moves_to_draw: i32,

    /// Null Move Pruning の Verification Search 用フラグ
    ///
    /// Stockfish/YaneuraOu準拠: Verification Search中は、
    /// `ply >= nmp_min_ply` の条件でNMPを制限する。
    /// 深い探索（depth >= 16）でZugzwangを検出して再探索する仕組み。
    pub nmp_min_ply: i32,

    /// NNUE Accumulator スタック
    ///
    /// 探索時のNNUE差分更新用。Position.do_move/undo_moveと同期してpush/popする。
    /// StateInfoからAccumulatorを分離することで、do_moveでの初期化コストを削減。
    pub nnue_stack: AccumulatorStack,

    // =========================================================================
    // 頻度制御（YaneuraOu準拠）
    // =========================================================================
    /// check_abort呼び出しカウンター（512回に1回チェック）
    calls_cnt: i32,
}

impl SearchWorker {
    fn skip_params(thread_id: usize) -> (usize, usize) {
        if thread_id == 0 {
            return (0, 0);
        }

        let idx = thread_id - 1;
        let mut size = 1;
        let mut base = 0;

        loop {
            let block = size * 2;
            if idx < base + block {
                return (size, idx - base);
            }
            base += block;
            size += 1;
        }
    }

    /// 新しいSearchWorkerを作成（YaneuraOu準拠: isreadyまたは最初のgo時）
    ///
    /// Box化してヒープに配置し、スタックオーバーフローを防ぐ。
    pub fn new(tt: Arc<TranspositionTable>, max_moves_to_draw: i32, thread_id: usize) -> Box<Self> {
        let (skip_size, skip_phase) = Self::skip_params(thread_id);
        let history = HistoryTables::new_boxed();
        let cont_history_sentinel =
            NonNull::from(history.continuation_history[0][0].get_table(Piece::NONE, Square::SQ_11));

        let mut worker = Box::new(Self {
            tt,
            thread_id,
            skip_size,
            skip_phase,
            // 履歴統計の初期化
            history,
            cont_history_sentinel,
            // 探索状態の初期化
            root_moves: RootMoves::new(),
            stack: init_stack_array(),
            nodes: 0,
            sel_depth: 0,
            root_depth: 0,
            root_delta: 1,
            completed_depth: 0,
            generate_all_legal_moves: false,
            best_move: Move::NONE,
            abort: false,
            best_move_changes: 0.0,
            max_moves_to_draw,
            nmp_min_ply: 0,
            nnue_stack: AccumulatorStack::new(),
            // 頻度制御
            calls_cnt: 0,
        });
        worker.reset_cont_history_ptrs();
        worker
    }

    #[inline]
    fn cont_history_ptr(&self, ply: i32, back: i32) -> NonNull<PieceToHistory> {
        debug_assert!(ply >= 0 && (ply as usize) < STACK_SIZE, "ply out of bounds: {ply}");
        debug_assert!(back >= 0, "back must be non-negative: {back}");
        if ply >= back {
            self.stack[(ply - back) as usize].cont_history_ptr
        } else {
            self.cont_history_sentinel
        }
    }

    #[inline]
    fn cont_history_ref(&self, ply: i32, back: i32) -> &PieceToHistory {
        let ptr = self.cont_history_ptr(ply, back);
        unsafe { ptr.as_ref() }
    }

    #[inline]
    fn cont_history_tables(&self, ply: i32) -> [&PieceToHistory; 6] {
        [
            self.cont_history_ref(ply, 1),
            self.cont_history_ref(ply, 2),
            self.cont_history_ref(ply, 3),
            self.cont_history_ref(ply, 4),
            self.cont_history_ref(ply, 5),
            self.cont_history_ref(ply, 6),
        ]
    }

    fn reset_cont_history_ptrs(&mut self) {
        let sentinel = self.cont_history_sentinel;
        for stack in self.stack.iter_mut() {
            stack.cont_history_ptr = sentinel;
        }
    }

    #[inline]
    fn set_cont_history_for_move(
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
        let table =
            self.history.continuation_history[in_check_idx][capture_idx].get_table(piece, to);
        self.stack[ply as usize].cont_history_ptr = NonNull::from(table);
        self.stack[ply as usize].cont_hist_key =
            Some(ContHistKey::new(in_check, capture, piece, to));
    }

    #[inline]
    fn clear_cont_history_for_null(&mut self, ply: i32) {
        self.stack[ply as usize].cont_history_ptr = self.cont_history_sentinel;
        self.stack[ply as usize].cont_hist_key = None;
    }

    /// usinewgameで呼び出し：全履歴をクリア（YaneuraOu Worker::clear()相当）
    pub fn clear(&mut self) {
        self.history.clear();
    }

    /// goで呼び出し：探索状態のリセット（履歴はクリアしない、YaneuraOu準拠）
    pub fn prepare_search(&mut self) {
        self.nodes = 0;
        self.sel_depth = 0;
        self.root_depth = 0;
        self.root_delta = 1;
        self.completed_depth = 0;
        self.best_move = Move::NONE;
        self.abort = false;
        self.best_move_changes = 0.0;
        self.nmp_min_ply = 0;
        self.root_moves.clear();
        // YaneuraOu準拠: low_ply_historyのみクリア
        self.history.low_ply_history.clear();
        // NNUE AccumulatorStackをリセット
        self.nnue_stack.reset();
        // check_abort頻度制御カウンターをリセット
        // これにより新しい探索開始時に即座に停止チェックが行われる
        self.calls_cnt = 0;
    }

    /// best_move_changes を半減（世代減衰）
    ///
    /// YaneuraOu準拠: 反復深化の各世代終了時に呼び出して、
    /// 古い情報の重みを低くする
    pub fn decay_best_move_changes(&mut self) {
        self.best_move_changes /= 2.0;
    }

    /// スレッドIDに基づいて探索深さを調整
    pub fn adjusted_depth(&self, base_depth: Depth) -> Depth {
        if self.thread_id == 0 || self.skip_size == 0 {
            return base_depth;
        }

        let skip = (self.skip_size + 1) as i32;
        let phase = self.skip_phase as i32;
        if (base_depth + phase) % skip != 0 {
            return (base_depth - 1).max(1);
        }

        base_depth
    }

    /// 全合法手生成モードの設定（YaneuraOu互換）
    pub fn set_generate_all_legal_moves(&mut self, flag: bool) {
        self.generate_all_legal_moves = flag;
    }

    /// 中断チェック
    /// YaneuraOu準拠: 512回に1回だけ実際のチェックを行う
    #[inline]
    fn check_abort(&mut self, limits: &LimitsType, time_manager: &mut TimeManagement) -> bool {
        // すでにabortフラグが立っている場合は即座に返す
        if self.abort {
            #[cfg(debug_assertions)]
            eprintln!("check_abort: abort flag already set");
            return true;
        }

        // 頻度制御：512回に1回だけ実際のチェックを行う（YaneuraOu準拠）
        self.calls_cnt -= 1;
        if self.calls_cnt > 0 {
            return false;
        }
        // カウンターをリセット
        self.calls_cnt = if limits.nodes > 0 {
            std::cmp::min(512, (limits.nodes / 1024) as i32).max(1)
        } else {
            512
        };

        // 外部からの停止要求
        if time_manager.stop_requested() {
            #[cfg(debug_assertions)]
            eprintln!("check_abort: stop requested");
            self.abort = true;
            return true;
        }

        // ノード数制限チェック
        if limits.nodes > 0 && self.nodes >= limits.nodes {
            #[cfg(debug_assertions)]
            eprintln!(
                "check_abort: node limit reached nodes={} limit={}",
                self.nodes, limits.nodes
            );
            self.abort = true;
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
                self.abort = true;
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

    /// 補正履歴から静的評価の補正値を算出（YaneuraOu準拠）
    #[inline]
    fn correction_value(&self, pos: &Position, ply: i32) -> i32 {
        let us = pos.side_to_move();
        let pawn_idx = (pos.pawn_key() as usize) & (CORRECTION_HISTORY_SIZE - 1);
        let minor_idx = (pos.minor_piece_key() as usize) & (CORRECTION_HISTORY_SIZE - 1);
        let non_pawn_idx_w =
            (pos.non_pawn_key(Color::White) as usize) & (CORRECTION_HISTORY_SIZE - 1);
        let non_pawn_idx_b =
            (pos.non_pawn_key(Color::Black) as usize) & (CORRECTION_HISTORY_SIZE - 1);

        let pcv = self.history.correction_history.pawn_value(pawn_idx, us) as i32;
        let micv = self.history.correction_history.minor_value(minor_idx, us) as i32;
        let wnpcv =
            self.history.correction_history.non_pawn_value(non_pawn_idx_w, Color::White, us) as i32;
        let bnpcv =
            self.history.correction_history.non_pawn_value(non_pawn_idx_b, Color::Black, us) as i32;

        let mut cntcv = 0;
        if ply >= 2 {
            let prev_move = self.stack[(ply - 1) as usize].current_move;
            if prev_move.is_some() {
                if let Some(prev2_key) = self.stack[(ply - 2) as usize].cont_hist_key {
                    let pc = pos.piece_on(prev_move.to());
                    cntcv = self.history.correction_history.continuation_value(
                        prev2_key.piece,
                        prev2_key.to,
                        pc,
                        prev_move.to(),
                    ) as i32;
                }
            }
        }

        8867 * pcv + 8136 * micv + 10_757 * (wnpcv + bnpcv) + 7232 * cntcv
    }

    /// 補正履歴の更新（YaneuraOu準拠）
    #[inline]
    fn update_correction_history(&mut self, pos: &Position, ply: i32, bonus: i32) {
        let us = pos.side_to_move();
        let pawn_idx = (pos.pawn_key() as usize) & (CORRECTION_HISTORY_SIZE - 1);
        let minor_idx = (pos.minor_piece_key() as usize) & (CORRECTION_HISTORY_SIZE - 1);
        let non_pawn_idx_w =
            (pos.non_pawn_key(Color::White) as usize) & (CORRECTION_HISTORY_SIZE - 1);
        let non_pawn_idx_b =
            (pos.non_pawn_key(Color::Black) as usize) & (CORRECTION_HISTORY_SIZE - 1);

        const NON_PAWN_WEIGHT: i32 = 165;

        self.history.correction_history.update_pawn(pawn_idx, us, bonus);
        self.history.correction_history.update_minor(minor_idx, us, bonus * 153 / 128);
        self.history.correction_history.update_non_pawn(
            non_pawn_idx_w,
            Color::White,
            us,
            bonus * NON_PAWN_WEIGHT / 128,
        );
        self.history.correction_history.update_non_pawn(
            non_pawn_idx_b,
            Color::Black,
            us,
            bonus * NON_PAWN_WEIGHT / 128,
        );

        if ply >= 2 {
            let prev_move = self.stack[(ply - 1) as usize].current_move;
            if prev_move.is_some() {
                if let Some(prev2_key) = self.stack[(ply - 2) as usize].cont_hist_key {
                    let pc = pos.piece_on(prev_move.to());
                    self.history.correction_history.update_continuation(
                        prev2_key.piece,
                        prev2_key.to,
                        pc,
                        prev_move.to(),
                        bonus * 153 / 128,
                    );
                }
            }
        }
    }

    /// 親ノードのreductionを取得してクリア
    #[inline]
    fn take_prior_reduction(&mut self, ply: i32) -> i32 {
        if ply >= 1 {
            let parent_idx = (ply - 1) as usize;
            let pr = self.stack[parent_idx].reduction;
            self.stack[parent_idx].reduction = 0;
            pr
        } else {
            0
        }
    }

    /// 置換表プローブと即時カットオフ（root以外のmate_1ply含む）
    ///
    /// `excluded_move`がある場合（Singular Extension中）は置換表カットオフを回避する。
    fn probe_transposition<const NT: u8>(
        &mut self,
        pos: &mut Position,
        depth: Depth,
        beta: Value,
        ply: i32,
        pv_node: bool,
        in_check: bool,
        excluded_move: Move,
    ) -> ProbeOutcome {
        let key = pos.key();
        let tt_result = self.tt.probe(key, pos);
        let tt_hit = tt_result.found;
        let tt_data = tt_result.data;

        self.stack[ply as usize].tt_hit = tt_hit;
        // excludedMoveがある場合は前回のttPvを維持（YaneuraOu準拠）
        self.stack[ply as usize].tt_pv = if excluded_move.is_some() {
            self.stack[ply as usize].tt_pv
        } else {
            pv_node || (tt_hit && tt_data.is_pv)
        };

        let tt_move = if tt_hit { tt_data.mv } else { Move::NONE };
        let tt_value = if tt_hit {
            value_from_tt(tt_data.value, ply)
        } else {
            Value::NONE
        };
        let tt_capture = tt_move.is_some() && pos.is_capture(tt_move);

        // excludedMoveがある場合はカットオフしない（YaneuraOu準拠）
        if !pv_node
            && excluded_move.is_none()
            && tt_hit
            && tt_data.depth >= depth
            && tt_value != Value::NONE
            && tt_data.bound.can_cutoff(tt_value, beta)
        {
            return ProbeOutcome::Cutoff(tt_value);
        }

        // 1手詰め判定（置換表未ヒット時のみ、Rootでは実施しない）
        // excludedMoveがある場合も実施しない（詰みがあればsingular前にbeta cutするため）
        if NT != NodeType::Root as u8 && !in_check && !tt_hit && excluded_move.is_none() {
            let mate_move = pos.mate_1ply();
            if mate_move.is_some() {
                let value = Value::mate_in(ply + 1);
                let stored_depth = (depth + 6).min(MAX_PLY - 1);
                tt_result.write(
                    key,
                    value,
                    self.stack[ply as usize].tt_pv,
                    Bound::Exact,
                    stored_depth,
                    mate_move,
                    Value::NONE,
                    self.tt.generation(),
                );
                return ProbeOutcome::Cutoff(value);
            }
        }

        ProbeOutcome::Continue(TTContext {
            key,
            result: tt_result,
            data: tt_data,
            hit: tt_hit,
            mv: tt_move,
            value: tt_value,
            capture: tt_capture,
        })
    }

    /// 静的評価と補正値の計算
    ///
    /// `excluded_move`がある場合（Singular Extension中）は既存のstatic_evalを維持する。
    fn compute_eval_context(
        &mut self,
        pos: &mut Position,
        ply: i32,
        in_check: bool,
        tt_ctx: &TTContext,
        excluded_move: Move,
    ) -> EvalContext {
        let correction_value = self.correction_value(pos, ply);

        // excludedMoveがある場合は、前回のstatic_evalをそのまま使用（YaneuraOu準拠）
        if excluded_move.is_some() {
            let static_eval = self.stack[ply as usize].static_eval;
            let improving = if ply >= 2 && !in_check && static_eval != Value::NONE {
                static_eval > self.stack[(ply - 2) as usize].static_eval
            } else {
                false
            };
            let opponent_worsening = if ply >= 1 && static_eval != Value::NONE {
                let prev_eval = self.stack[(ply - 1) as usize].static_eval;
                prev_eval != Value::NONE && static_eval > -prev_eval
            } else {
                false
            };
            return EvalContext {
                static_eval,
                unadjusted_static_eval: static_eval, // excludedMove時は未補正値も同じ
                correction_value,
                improving,
                opponent_worsening,
            };
        }

        let mut unadjusted_static_eval = Value::NONE;
        let mut static_eval = if in_check {
            Value::NONE
        } else if tt_ctx.hit && tt_ctx.data.eval != Value::NONE {
            unadjusted_static_eval = tt_ctx.data.eval;
            unadjusted_static_eval
        } else {
            unadjusted_static_eval = evaluate(pos, &mut self.nnue_stack);
            unadjusted_static_eval
        };

        if !in_check && unadjusted_static_eval != Value::NONE {
            static_eval = to_corrected_static_eval(unadjusted_static_eval, correction_value);
        }

        if !in_check
            && tt_ctx.hit
            && tt_ctx.value != Value::NONE
            && !tt_ctx.value.is_mate_score()
            && ((tt_ctx.value > static_eval && tt_ctx.data.bound == Bound::Lower)
                || (tt_ctx.value < static_eval && tt_ctx.data.bound == Bound::Upper))
        {
            static_eval = tt_ctx.value;
        }

        self.stack[ply as usize].static_eval = static_eval;

        let improving = if ply >= 2 && !in_check {
            static_eval > self.stack[(ply - 2) as usize].static_eval
        } else {
            false
        };
        let opponent_worsening = if ply >= 1 && static_eval != Value::NONE {
            let prev_eval = self.stack[(ply - 1) as usize].static_eval;
            prev_eval != Value::NONE && static_eval > -prev_eval
        } else {
            false
        };

        EvalContext {
            static_eval,
            unadjusted_static_eval,
            correction_value,
            improving,
            opponent_worsening,
        }
    }

    /// Razoring
    #[allow(clippy::too_many_arguments)]
    fn try_razoring<const NT: u8>(
        &mut self,
        pos: &mut Position,
        depth: Depth,
        alpha: Value,
        beta: Value,
        ply: i32,
        pv_node: bool,
        in_check: bool,
        static_eval: Value,
        limits: &LimitsType,
        time_manager: &mut TimeManagement,
    ) -> Option<Value> {
        if !pv_node && !in_check && depth <= 3 {
            let razoring_threshold = alpha - Value::new(200 * depth);
            if static_eval < razoring_threshold {
                let value = self.qsearch::<{ NodeType::NonPV as u8 }>(
                    pos,
                    DEPTH_QS,
                    alpha,
                    beta,
                    ply,
                    limits,
                    time_manager,
                );
                if value <= alpha {
                    return Some(value);
                }
            }
        }
        None
    }

    /// Futility pruning
    fn try_futility_pruning(&self, params: FutilityParams) -> Option<Value> {
        if !params.pv_node
            && !params.in_check
            && params.depth <= 8
            && params.static_eval != Value::NONE
        {
            let futility_mult =
                FUTILITY_MARGIN_BASE - 20 * (params.cut_node && !params.tt_hit) as i32;
            let futility_margin = Value::new(
                futility_mult * params.depth
                    - (params.improving as i32) * futility_mult * 2
                    - (params.opponent_worsening as i32) * futility_mult / 3
                    + (params.correction_value.abs() / 171_290),
            );

            if params.static_eval - futility_margin >= params.beta {
                return Some(params.static_eval);
            }
        }
        None
    }

    /// Null move pruning with Verification Search（Stockfish/YaneuraOu準拠）
    ///
    /// NMPは、自分の手番で「何もしない（パス）」という仮想的な手を打ち、
    /// それでも優勢なら探索を打ち切る枝刈り技術。
    ///
    /// Verification Search（深度 >= 16 の場合）:
    /// - NMPの結果が正しいかを検証するため、NMPを無効化して再探索
    /// - Zugzwang（動かなければならないこと自体が不利な局面）対策
    #[allow(clippy::too_many_arguments)]
    fn try_null_move_pruning<const NT: u8>(
        &mut self,
        pos: &mut Position,
        depth: Depth,
        beta: Value,
        ply: i32,
        cut_node: bool,
        in_check: bool,
        static_eval: Value,
        mut improving: bool,
        excluded_move: Move,
        limits: &LimitsType,
        time_manager: &mut TimeManagement,
    ) -> (Option<Value>, bool) {
        // Stockfish/YaneuraOu準拠のNMP条件:
        // - ply >= 1（スタックアクセスの安全性）
        // - Singular Extension中でない（excludedMoveが設定されていない）
        // - cutNodeである（!pv_nodeではなく、より厳密な条件）
        // - 王手されていない
        // - 評価値がマージン付きでbetaを超える（beta - 18 * depth + 390）
        // - ply >= nmp_min_ply（Verification Search中のNMP制限）
        // - betaが負けスコアでない
        // - 前回の手が有効な手（NONE/NULLでない）（連続NullMove禁止）
        //
        // 注: Stockfishでは non_pawn_material(us) チェックがあるが、
        //     YaneuraOuでは将棋で使用していない（#if STOCKFISHで囲まれている）。
        //     チェスでは Pawn endgame の Zugzwang 対策だが、
        //     将棋は持ち駒があるため Pawn だけの終盤は存在しない。
        //     将来検証する場合: && pos.non_pawn_material(pos.side_to_move())

        // ply >= 1 のガード（防御的プログラミング）
        // 実際には search_root から search_node を呼ぶ際に常に ply=1 から開始するため、
        // ply=0 でこの関数が呼ばれることはないが、将来の変更に備えて明示的にガードする。
        if ply < 1 {
            return (None, improving);
        }

        let margin = 18 * depth - 390;
        let prev_move = self.stack[(ply - 1) as usize].current_move;
        // YaneuraOu準拠: prev_move != Move::null() のみをチェック
        // Move::NONEのチェックは不要（root直下でNMPを無効化してしまう）
        // YaneuraOuではASSERT_LV3で検証しているのみ
        if excluded_move.is_none()
            && cut_node
            && !in_check
            && static_eval >= beta - Value::new(margin)
            && ply >= self.nmp_min_ply
            && !beta.is_loss()
            && !prev_move.is_null()
        {
            // Null move dynamic reduction based on depth（YaneuraOu準拠）
            let r = 7 + depth / 3;

            // YaneuraOu準拠: NullMove探索前にcurrent_moveとcont_hist_keyを設定
            // これにより再帰呼び出し先で連続NullMoveが禁止され、
            // continuation historyの不正参照を防ぐ
            self.stack[ply as usize].current_move = Move::NULL;
            self.clear_cont_history_for_null(ply);

            pos.do_null_move_with_prefetch(self.tt.as_ref());
            self.nnue_stack.push(DirtyPiece::new()); // null moveでは駒の移動なし
            let null_value = -self.search_node::<{ NodeType::NonPV as u8 }>(
                pos,
                depth - r,
                -beta,
                -beta + Value::new(1),
                ply + 1,
                false, // cutNode = false（NullMove後は相手番なので）
                limits,
                time_manager,
            );
            self.nnue_stack.pop();
            pos.undo_null_move();

            // Do not return unproven mate scores（勝ちスコアは信頼しない）
            if null_value >= beta && !null_value.is_win() {
                // 浅い探索 or 既にVerification Search中 → そのままreturn
                if self.nmp_min_ply != 0 || depth < 16 {
                    return (Some(null_value), improving);
                }

                // Verification Search: 深い探索でZugzwangを検出
                // NMPを無効化（nmp_min_plyを設定）して再探索
                self.nmp_min_ply = ply + 3 * (depth - r) / 4;

                let v = self.search_node::<{ NodeType::NonPV as u8 }>(
                    pos,
                    depth - r,
                    beta - Value::new(1),
                    beta,
                    ply,
                    false, // cutNode = false（Verification SearchはcutNodeにしない）
                    limits,
                    time_manager,
                );

                self.nmp_min_ply = 0;

                if v >= beta {
                    return (Some(null_value), improving);
                }
            }
        }

        // improving の更新（Stockfish/YaneuraOu: NMPの後で更新）
        if !in_check && static_eval != Value::NONE {
            improving |= static_eval >= beta;
        }

        (None, improving)
    }

    /// ProbCut（YaneuraOu準拠）
    #[allow(clippy::too_many_arguments)]
    fn try_probcut(
        &mut self,
        pos: &mut Position,
        depth: Depth,
        beta: Value,
        improving: bool,
        tt_ctx: &TTContext,
        ply: i32,
        static_eval: Value,
        unadjusted_static_eval: Value,
        in_check: bool,
        limits: &LimitsType,
        time_manager: &mut TimeManagement,
    ) -> Option<Value> {
        if in_check || depth < 3 || static_eval == Value::NONE {
            return None;
        }

        let prob_beta = beta + Value::new(215 - 60 * improving as i32);
        if beta.is_mate_score()
            || (tt_ctx.hit
                && tt_ctx.value != Value::NONE
                && tt_ctx.value < prob_beta
                && !tt_ctx.value.is_mate_score())
        {
            return None;
        }

        let threshold = prob_beta - static_eval;
        if threshold <= Value::ZERO {
            return None;
        }

        let dynamic_reduction = (static_eval - beta).raw() / 300;
        let probcut_depth = (depth - 5 - dynamic_reduction).max(0);

        // 固定長バッファに収集（借用チェッカーの制約回避）
        let probcut_moves = {
            let cont_tables = self.cont_history_tables(ply);
            let mp = MovePicker::new_probcut(
                pos,
                tt_ctx.mv,
                threshold,
                &self.history.main_history,
                &self.history.low_ply_history,
                &self.history.capture_history,
                cont_tables,
                &self.history.pawn_history,
                ply,
                self.generate_all_legal_moves,
            );

            let mut buf = [Move::NONE; crate::movegen::MAX_MOVES];
            let mut len = 0;
            for mv in mp {
                buf[len] = mv;
                len += 1;
            }
            (buf, len)
        };
        let (buf, len) = probcut_moves;

        // YaneuraOu準拠: 全捕獲手を試す
        for &mv in buf[..len].iter() {
            if !pos.is_legal(mv) {
                continue;
            }

            let gives_check = pos.gives_check(mv);
            let is_capture = pos.is_capture(mv);
            let cont_hist_piece = mv.moved_piece_after();
            let cont_hist_to = mv.to();

            self.stack[ply as usize].current_move = mv;
            let dirty_piece = pos.do_move_with_prefetch(mv, gives_check, self.tt.as_ref());
            self.nnue_stack.push(dirty_piece);
            self.nodes += 1;
            self.set_cont_history_for_move(
                ply,
                in_check,
                is_capture,
                cont_hist_piece,
                cont_hist_to,
            );
            let mut value = -self.qsearch::<{ NodeType::NonPV as u8 }>(
                pos,
                DEPTH_QS,
                -prob_beta,
                -prob_beta + Value::new(1),
                ply + 1,
                limits,
                time_manager,
            );

            if value >= prob_beta && probcut_depth > 0 {
                value = -self.search_node::<{ NodeType::NonPV as u8 }>(
                    pos,
                    probcut_depth,
                    -prob_beta,
                    -prob_beta + Value::new(1),
                    ply + 1,
                    true,
                    limits,
                    time_manager,
                );
            }
            self.nnue_stack.pop();
            pos.undo_move(mv);

            if value >= prob_beta {
                let stored_depth = (probcut_depth + 1).max(1);
                tt_ctx.result.write(
                    tt_ctx.key,
                    value_to_tt(value, ply),
                    self.stack[ply as usize].tt_pv,
                    Bound::Lower,
                    stored_depth,
                    mv,
                    unadjusted_static_eval,
                    self.tt.generation(),
                );

                if value.raw().abs() < Value::INFINITE.raw() {
                    return Some(value - (prob_beta - beta));
                }
                return Some(value);
            }
        }

        None
    }

    /// Small ProbCut
    #[inline]
    fn try_small_probcut(&self, depth: Depth, beta: Value, tt_ctx: &TTContext) -> Option<Value> {
        if depth >= 1 {
            let sp_beta = beta + Value::new(417);
            if tt_ctx.hit
                && tt_ctx.data.bound == Bound::Lower
                && tt_ctx.data.depth >= depth - 4
                && tt_ctx.value != Value::NONE
                && tt_ctx.value >= sp_beta
                && !tt_ctx.value.is_mate_score()
                && !beta.is_mate_score()
            {
                return Some(sp_beta);
            }
        }
        None
    }

    /// 指し手生成（TT手優先、qsearch用チェック生成など含む）
    ///
    /// 固定長バッファを使用してヒープ割り当てを回避する。
    fn generate_ordered_moves(
        &self,
        pos: &mut Position,
        tt_move: Move,
        depth: Depth,
        in_check: bool,
        ply: i32,
    ) -> (OrderedMovesBuffer, Move) {
        let mut ordered_moves = OrderedMovesBuffer::new();
        let mut tt_move = tt_move;

        // qsearch/ProbCut互換: 捕獲フェーズではTT手もcapture_stageで制約
        if depth <= DEPTH_QS
            && tt_move.is_some()
            && (!pos.capture_stage(tt_move) && !pos.gives_check(tt_move) || depth < -16)
        {
            tt_move = Move::NONE;
        }

        let cont_tables = self.cont_history_tables(ply);
        let mp = MovePicker::new(
            pos,
            tt_move,
            depth,
            &self.history.main_history,
            &self.history.low_ply_history,
            &self.history.capture_history,
            cont_tables,
            &self.history.pawn_history,
            ply,
            self.generate_all_legal_moves,
        );

        for mv in mp {
            if mv.is_some() {
                ordered_moves.push(mv);
            }
        }

        // qsearchでは捕獲以外のチェックも生成（YaneuraOu準拠）
        if !in_check && depth == DEPTH_QS {
            let mut buf = crate::movegen::ExtMoveBuffer::new();
            let gen_type = if self.generate_all_legal_moves {
                crate::movegen::GenType::QuietChecksAll
            } else {
                crate::movegen::GenType::QuietChecks
            };
            crate::movegen::generate_with_type(pos, gen_type, &mut buf, None);
            for ext in buf.iter() {
                if ordered_moves.contains(&ext.mv) {
                    continue;
                }
                ordered_moves.push(ext.mv);
            }
        }

        // depth <= -5 なら recaptures のみに絞る
        if depth <= -5 && ply >= 1 {
            let mut buf = crate::movegen::ExtMoveBuffer::new();
            let rec_sq = self.stack[(ply - 1) as usize].current_move.to();
            let gen_type = if self.generate_all_legal_moves {
                crate::movegen::GenType::RecapturesAll
            } else {
                crate::movegen::GenType::Recaptures
            };
            crate::movegen::generate_with_type(pos, gen_type, &mut buf, Some(rec_sq));
            ordered_moves.clear();
            for ext in buf.iter() {
                ordered_moves.push(ext.mv);
            }
        }

        (ordered_moves, tt_move)
    }

    /// Step14 の枝刈り（進行可否を返す）
    fn step14_pruning(&self, ctx: Step14Context<'_>) -> Step14Outcome {
        let mut lmr_depth = ctx.lmr_depth;

        if ctx.ply != 0 && !ctx.best_value.is_loss() {
            let lmp_denominator = 2 - ctx.improving as i32;
            debug_assert!(lmp_denominator > 0, "LMP denominator must be positive");
            let lmp_limit = (3 + ctx.depth * ctx.depth) / lmp_denominator;
            if ctx.move_count >= lmp_limit && !ctx.is_capture && !ctx.gives_check {
                return Step14Outcome::Skip { best_value: None };
            }

            if ctx.is_capture || ctx.gives_check {
                let captured = ctx.pos.piece_on(ctx.mv.to());
                let capt_hist = self.history.capture_history.get_with_captured_piece(
                    ctx.mv.moved_piece_after(),
                    ctx.mv.to(),
                    captured,
                ) as i32;

                if !ctx.gives_check && lmr_depth < 7 && !ctx.in_check {
                    let futility_value = self.stack[ctx.ply as usize].static_eval
                        + Value::new(232 + 224 * lmr_depth)
                        + Value::new(piece_value(captured))
                        + Value::new(131 * capt_hist / 1024);
                    if futility_value <= ctx.alpha {
                        return Step14Outcome::Skip { best_value: None };
                    }
                }

                let margin = (158 * ctx.depth + capt_hist / 31).clamp(0, 283 * ctx.depth);
                if !ctx.pos.see_ge(ctx.mv, Value::new(-margin)) {
                    return Step14Outcome::Skip { best_value: None };
                }
            } else {
                let mut history = 0;
                history += ctx.cont_history_1.get(ctx.mv.moved_piece_after(), ctx.mv.to()) as i32;
                history += ctx.cont_history_2.get(ctx.mv.moved_piece_after(), ctx.mv.to()) as i32;
                history += self.history.pawn_history.get(
                    ctx.pos.pawn_history_index(),
                    ctx.mv.moved_piece_after(),
                    ctx.mv.to(),
                ) as i32;

                if history < -4361 * ctx.depth {
                    return Step14Outcome::Skip { best_value: None };
                }

                history += 71 * self.history.main_history.get(ctx.mover, ctx.mv) as i32 / 32;
                lmr_depth += history / 3233;

                let base_futility = if ctx.best_move.is_some() { 46 } else { 230 };
                let futility_value = self.stack[ctx.ply as usize].static_eval
                    + Value::new(base_futility + 131 * lmr_depth)
                    + Value::new(
                        91 * (self.stack[ctx.ply as usize].static_eval > ctx.alpha) as i32,
                    );

                if !ctx.in_check && lmr_depth < 11 && futility_value <= ctx.alpha {
                    if ctx.best_value <= futility_value
                        && !ctx.best_value.is_mate_score()
                        && !futility_value.is_win()
                    {
                        return Step14Outcome::Skip {
                            best_value: Some(futility_value),
                        };
                    }
                    return Step14Outcome::Skip { best_value: None };
                }

                lmr_depth = lmr_depth.max(0);
                // lmr_depth は探索深さ由来で十数〜数十程度に収まるため i32 乗算でオーバーフローしない
                if !ctx.pos.see_ge(ctx.mv, Value::new(-26 * lmr_depth * lmr_depth)) {
                    return Step14Outcome::Skip { best_value: None };
                }
            }
        }

        Step14Outcome::Continue
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
        self.root_moves = RootMoves::from_legal_moves(pos, &limits.search_moves);

        if self.root_moves.is_empty() {
            // 合法手がない場合
            self.best_move = Move::NONE;
            return;
        }

        // 反復深化
        for d in 1..=depth {
            if self.abort {
                break;
            }

            // イテレーション開始時にeffortをリセット
            for rm in self.root_moves.iter_mut() {
                rm.effort = 0.0;
            }

            self.root_depth = d;
            self.sel_depth = 0;

            // Aspiration Window
            let prev_score = if d > 1 {
                self.root_moves[0].score
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

                if self.abort {
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

                delta = Value::new(delta.raw() + delta.raw() / 3);
            }

            if !self.abort {
                self.completed_depth = d;
                self.best_move = self.root_moves[0].mv();
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
        self.root_delta = (beta.raw() - alpha.raw()).abs().max(1);

        let mut alpha = alpha;
        let mut best_value = Value::new(-32001);
        let mut pv_idx = 0;
        let root_in_check = pos.in_check();

        self.stack[0].in_check = root_in_check;
        self.stack[0].cont_history_ptr = self.cont_history_sentinel;
        self.stack[0].cont_hist_key = None;

        for rm_idx in 0..self.root_moves.len() {
            if self.check_abort(limits, time_manager) {
                return Value::ZERO;
            }

            // 各手ごとにsel_depthをリセット（YaneuraOu準拠）
            self.sel_depth = 0;

            let mv = self.root_moves[rm_idx].mv();
            let gives_check = pos.gives_check(mv);
            let is_capture = pos.is_capture(mv);
            let cont_hist_piece = mv.moved_piece_after();
            let cont_hist_to = mv.to();

            let nodes_before = self.nodes;

            // 探索
            let dirty_piece = pos.do_move_with_prefetch(mv, gives_check, self.tt.as_ref());
            self.nnue_stack.push(dirty_piece);
            self.nodes += 1;
            self.stack[0].current_move = mv;
            self.set_cont_history_for_move(
                0,
                root_in_check,
                is_capture,
                cont_hist_piece,
                cont_hist_to,
            );

            // PVS
            let value = if rm_idx == 0 {
                -self.search_node::<{ NodeType::PV as u8 }>(
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
                let mut value = -self.search_node::<{ NodeType::NonPV as u8 }>(
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
                    value = -self.search_node::<{ NodeType::PV as u8 }>(
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

            self.nnue_stack.pop();
            pos.undo_move(mv);

            // この手に費やしたノード数をeffortに積算
            let nodes_delta = self.nodes.saturating_sub(nodes_before);
            self.root_moves[rm_idx].effort += nodes_delta as f64;

            if self.abort {
                return Value::ZERO;
            }

            // スコア更新（この手の探索で到達したsel_depthを記録）
            let mut updated_alpha = rm_idx == 0; // 先頭手は維持（YO準拠）
            {
                let rm = &mut self.root_moves[rm_idx];
                rm.score = value;
                rm.sel_depth = self.sel_depth;
                rm.accumulate_score_stats(value);
            }

            if value > best_value {
                best_value = value;

                if value > alpha {
                    // YaneuraOu準拠: 2番目以降の手がalphaを更新した場合にカウント
                    // moveCount > 1 && !pvIdx の条件
                    // (Multi-PV未実装なので pvIdx は常に0)
                    if rm_idx > 0 {
                        self.best_move_changes += 1.0;
                    }

                    alpha = value;
                    pv_idx = rm_idx;
                    updated_alpha = true;

                    // PVを更新
                    self.root_moves[rm_idx].pv.truncate(1);
                    self.root_moves[rm_idx].pv.extend_from_slice(&self.stack[1].pv);

                    if value >= beta {
                        break;
                    }
                }
            }

            // α未更新の手はスコアを -INFINITE に落として順序維持（YO準拠）
            if !updated_alpha {
                self.root_moves[rm_idx].score = Value::new(-Value::INFINITE.raw());
            }
        }

        // 最善手を先頭に移動
        self.root_moves.move_to_front(pv_idx);
        self.root_moves.sort();

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
        self.root_delta = (beta.raw() - alpha.raw()).abs().max(1);

        let mut alpha = alpha;
        let mut best_value = Value::new(-32001);
        let mut best_rm_idx = pv_idx;
        let root_in_check = pos.in_check();

        self.stack[0].in_check = root_in_check;
        self.stack[0].cont_history_ptr = self.cont_history_sentinel;
        self.stack[0].cont_hist_key = None;

        // pv_idx以降の手のみを探索
        for rm_idx in pv_idx..self.root_moves.len() {
            if self.check_abort(limits, time_manager) {
                return Value::ZERO;
            }

            // 各手ごとにsel_depthをリセット
            self.sel_depth = 0;

            let mv = self.root_moves[rm_idx].mv();
            let gives_check = pos.gives_check(mv);
            let is_capture = pos.is_capture(mv);
            let cont_hist_piece = mv.moved_piece_after();
            let cont_hist_to = mv.to();

            let nodes_before = self.nodes;

            // 探索
            let dirty_piece = pos.do_move_with_prefetch(mv, gives_check, self.tt.as_ref());
            self.nnue_stack.push(dirty_piece);
            self.nodes += 1;
            self.stack[0].current_move = mv;
            self.set_cont_history_for_move(
                0,
                root_in_check,
                is_capture,
                cont_hist_piece,
                cont_hist_to,
            );

            // PVS: 最初の手（このPVラインの候補）はPV探索
            let value = if rm_idx == pv_idx {
                -self.search_node::<{ NodeType::PV as u8 }>(
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
                let mut value = -self.search_node::<{ NodeType::NonPV as u8 }>(
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
                    value = -self.search_node::<{ NodeType::PV as u8 }>(
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

            self.nnue_stack.pop();
            pos.undo_move(mv);

            // この手に費やしたノード数をeffortに積算
            let nodes_delta = self.nodes.saturating_sub(nodes_before);
            self.root_moves[rm_idx].effort += nodes_delta as f64;

            if self.abort {
                return Value::ZERO;
            }

            // スコア更新
            let mut updated_alpha = rm_idx == pv_idx; // PVラインの先頭は維持
            {
                let rm = &mut self.root_moves[rm_idx];
                rm.score = value;
                rm.sel_depth = self.sel_depth;
                rm.accumulate_score_stats(value);
            }

            if value > best_value {
                best_value = value;

                if value > alpha {
                    // best_move_changesのカウント（2番目以降の手で更新された場合）
                    // MultiPVでは pv_idx == 0（第1PVライン）のみカウントする
                    if pv_idx == 0 && rm_idx > pv_idx {
                        self.best_move_changes += 1.0;
                    }

                    alpha = value;
                    best_rm_idx = rm_idx;
                    updated_alpha = true;

                    // PVを更新
                    self.root_moves[rm_idx].pv.truncate(1);
                    self.root_moves[rm_idx].pv.extend_from_slice(&self.stack[1].pv);

                    if value >= beta {
                        break;
                    }
                }
            }

            // α未更新の手は -INFINITE で前回順序を保持（YO準拠）
            if !updated_alpha {
                self.root_moves[rm_idx].score = Value::new(-Value::INFINITE.raw());
            }
        }

        // 最善手をpv_idxの位置に移動
        self.root_moves.move_to_index(best_rm_idx, pv_idx);

        best_value
    }

    /// 通常探索ノード
    ///
    /// NTは NodeType を const genericで受け取る（コンパイル時最適化）
    /// cut_node は「βカットが期待される（ゼロウィンドウの非PVなど）」ときに true を渡す。
    /// 再探索やPV探索では all_node 扱いにするため false を渡す（YaneuraOuのcutNode引き渡しと対応）。
    fn search_node<const NT: u8>(
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
        let pv_node = NT == NodeType::PV as u8 || NT == NodeType::Root as u8;
        let mut depth = depth;
        let in_check = pos.in_check();
        // YaneuraOuのallNode定義: !(PvNode || cutNode)（yaneuraou-search.cpp:1854付近）
        let all_node = !(pv_node || cut_node);
        let mut alpha = alpha;

        // 深さが0以下なら静止探索へ
        if depth <= DEPTH_QS {
            return self.qsearch::<NT>(pos, depth, alpha, beta, ply, limits, time_manager);
        }

        // 最大深さチェック
        if ply >= MAX_PLY {
            return if in_check {
                Value::ZERO
            } else {
                evaluate(pos, &mut self.nnue_stack)
            };
        }

        // 選択的深さを更新
        if pv_node && self.sel_depth < ply + 1 {
            self.sel_depth = ply + 1;
        }

        // 中断チェック
        if self.check_abort(limits, time_manager) {
            return Value::ZERO;
        }

        // スタック設定
        self.stack[ply as usize].in_check = in_check;
        self.stack[ply as usize].move_count = 0;
        self.stack[(ply + 1) as usize].cutoff_cnt = 0;
        let prior_reduction = self.take_prior_reduction(ply);
        self.stack[ply as usize].reduction = 0;

        // Singular Extension用の除外手を取得
        let excluded_move = self.stack[ply as usize].excluded_move;

        // 置換表プローブ（即時カットオフ含む）
        let tt_ctx = match self.probe_transposition::<NT>(
            pos,
            depth,
            beta,
            ply,
            pv_node,
            in_check,
            excluded_move,
        ) {
            ProbeOutcome::Continue(ctx) => ctx,
            ProbeOutcome::Cutoff(value) => return value,
        };
        let tt_move = tt_ctx.mv;
        let tt_value = tt_ctx.value;
        let tt_hit = tt_ctx.hit;
        let tt_data = tt_ctx.data;
        let tt_capture = tt_ctx.capture;

        // 静的評価
        let eval_ctx = self.compute_eval_context(pos, ply, in_check, &tt_ctx, excluded_move);
        let mut improving = eval_ctx.improving;
        let opponent_worsening = eval_ctx.opponent_worsening;

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
            && self.stack[(ply - 1) as usize].static_eval != Value::NONE
            // Value は ±32002 程度なので i32 加算でオーバーフローしない
            && eval_ctx.static_eval + self.stack[(ply - 1) as usize].static_eval
                > Value::new(IIR_EVAL_SUM_THRESHOLD)
        {
            depth -= 1;
        }

        if let Some(v) = self.try_razoring::<NT>(
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

        if let Some(v) = self.try_futility_pruning(FutilityParams {
            depth,
            beta,
            static_eval: eval_ctx.static_eval,
            correction_value: eval_ctx.correction_value,
            improving,
            opponent_worsening,
            cut_node,
            tt_hit,
            pv_node,
            in_check,
        }) {
            return v;
        }

        let (null_value, improving_after_null) = self.try_null_move_pruning::<NT>(
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
        );
        if let Some(v) = null_value {
            return v;
        }
        improving = improving_after_null;

        // Internal Iterative Reductions（improving再計算後に実施）
        // YaneuraOu準拠: !allNode && depth>=6 && !ttMove && priorReduction<=3
        // （yaneuraou-search.cpp:2912-2919）
        if !all_node && depth >= 6 && tt_move.is_none() && prior_reduction <= 3 {
            depth -= 1;
        }

        if let Some(v) = self.try_probcut(
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
        ) {
            return v;
        }

        if let Some(v) = self.try_small_probcut(depth, beta, &tt_ctx) {
            return v;
        }

        // =================================================================
        // 指し手ループ
        // =================================================================
        let mut best_value = Value::new(-32001);
        let mut best_move = Move::NONE;
        let mut move_count = 0;
        let mut quiets_tried = SearchedMoveList::new();
        let mut captures_tried = SearchedMoveList::new();
        let mover = pos.side_to_move();

        let (ordered_moves, tt_move) =
            self.generate_ordered_moves(pos, tt_move, depth, in_check, ply);

        // Singular Extension用の変数
        let tt_pv = self.stack[ply as usize].tt_pv;
        let root_node = NT == NodeType::Root as u8;

        for mv in ordered_moves.iter() {
            // Singular Extension用の除外手をスキップ（YaneuraOu準拠）
            if mv == excluded_move {
                continue;
            }
            if !pos.pseudo_legal(mv) {
                continue;
            }
            if !pos.is_legal(mv) {
                continue;
            }
            if self.check_abort(limits, time_manager) {
                return Value::ZERO;
            }

            move_count += 1;
            self.stack[ply as usize].move_count = move_count;

            let is_capture = pos.is_capture(mv);
            let gives_check = pos.gives_check(mv);

            // quietの指し手の連続回数（YaneuraOu: quietMoveStreak）
            self.stack[(ply + 1) as usize].quiet_move_streak = if !is_capture && !gives_check {
                self.stack[ply as usize].quiet_move_streak + 1
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
                self.stack[ply as usize].excluded_move = mv;
                let singular_value = self.search_node::<{ NodeType::NonPV as u8 }>(
                    pos,
                    singular_depth,
                    singular_beta - Value::new(1),
                    singular_beta,
                    ply,
                    cut_node,
                    limits,
                    time_manager,
                );
                self.stack[ply as usize].excluded_move = Move::NONE;

                if singular_value < singular_beta {
                    // Singular確定 → 延長量を計算
                    // 補正履歴の寄与（abs(correctionValue)/249096）を margin に加算
                    let corr_val_adj = eval_ctx.correction_value.abs() / 249_096;
                    let double_margin =
                        4 + 205 * pv_node as i32 - 223 * !tt_capture as i32 - corr_val_adj;
                    let triple_margin = 80 + 276 * pv_node as i32 - 249 * !tt_capture as i32
                        + 86 * tt_pv as i32
                        - corr_val_adj;

                    extension = 1
                        + (singular_value < singular_beta - Value::new(double_margin)) as i32
                        + (singular_value < singular_beta - Value::new(triple_margin)) as i32;

                    // YaneuraOu準拠: singular確定時にdepthを+1（yaneuraou-search.cpp:3401）
                    depth += 1;
                } else if singular_value >= beta && !singular_value.is_mate_score() {
                    // Multi-Cut: 他の手もfail highする場合は枝刈り
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
            let mut r = reduction(improving, depth, move_count, delta, self.root_delta.max(1));

            // YaneuraOu: ttPvなら reduction を少し増やす（yaneuraou-search.cpp:3168-3170）
            if self.stack[ply as usize].tt_pv {
                r += 931;
            }

            let lmr_depth = new_depth - r / 1024;

            let step14_ctx = Step14Context {
                pos,
                mv,
                depth,
                ply,
                improving,
                best_move,
                best_value,
                alpha,
                in_check,
                gives_check,
                is_capture,
                lmr_depth,
                mover,
                move_count,
                cont_history_1: self.cont_history_ref(ply, 1),
                cont_history_2: self.cont_history_ref(ply, 2),
            };

            match self.step14_pruning(step14_ctx) {
                Step14Outcome::Skip {
                    best_value: updated,
                } => {
                    if let Some(v) = updated {
                        best_value = v;
                    }
                    continue;
                }
                Step14Outcome::Continue => {}
            }

            // =============================================================
            // Late Move Pruning
            // =============================================================
            if !pv_node && !in_check && !is_capture {
                let lmp_limit = (3 + depth * depth) / (2 - improving as i32);
                if move_count >= lmp_limit {
                    continue;
                }
            }

            // =============================================================
            // SEE Pruning
            // =============================================================
            if !pv_node && depth <= 8 && !in_check {
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
            self.stack[ply as usize].current_move = mv;

            // ContHistKey用の情報をMoveから取得（YaneuraOu方式）
            let cont_hist_piece = mv.moved_piece_after();
            let cont_hist_to = mv.to();

            let dirty_piece = pos.do_move_with_prefetch(mv, gives_check, self.tt.as_ref());
            self.nnue_stack.push(dirty_piece);
            self.nodes += 1;
            // YaneuraOu方式: ContHistKey/ContinuationHistoryを設定
            // ⚠ in_checkは親ノードの王手状態を使用（gives_checkではない）
            self.set_cont_history_for_move(
                ply,
                in_check,
                is_capture,
                cont_hist_piece,
                cont_hist_to,
            );

            // 手の記録（YaneuraOu準拠: quietsSearched, capturesSearched）
            if is_capture {
                captures_tried.push(mv);
            } else {
                quiets_tried.push(mv);
            }

            // 延長量をnew_depthに加算（YaneuraOu準拠: do_moveの後、yaneuraou-search.cpp:3482）
            new_depth += extension;

            // =============================================================
            // Late Move Reduction (LMR)
            // =============================================================
            let msb_depth = msb(depth);
            let tt_value_higher = tt_hit && tt_value != Value::NONE && tt_value > alpha;
            let tt_depth_ge = tt_hit && tt_data.depth >= depth;

            if self.stack[ply as usize].tt_pv {
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

            if self.stack[(ply + 1) as usize].cutoff_cnt > 2 {
                r += 925 + 33 * msb_depth + (all_node as i32) * (701 + 224 * msb_depth);
            }

            r += self.stack[(ply + 1) as usize].quiet_move_streak * 51;

            if mv == tt_move {
                r -= 2121 + 28 * msb_depth;
            }

            // statScoreによる補正
            let stat_score = if is_capture {
                let captured = pos.captured_piece();
                let captured_pt = captured.piece_type();
                let moved_piece = mv.moved_piece_after();
                let hist =
                    self.history.capture_history.get(moved_piece, mv.to(), captured_pt) as i32;
                782 * piece_value(captured) / 128 + hist
            } else {
                let moved_piece = mv.moved_piece_after();
                let main_hist = self.history.main_history.get(mover, mv) as i32;
                let cont0 = self.cont_history_ref(ply, 1).get(moved_piece, mv.to()) as i32;
                let cont1 = self.cont_history_ref(ply, 2).get(moved_piece, mv.to()) as i32;
                2 * main_hist + cont0 + cont1
            };
            self.stack[ply as usize].stat_score = stat_score;
            r -= stat_score * (729 - 12 * msb_depth) / 8192;

            // =============================================================
            // 探索
            // =============================================================
            let value =
                if depth >= 2 && move_count > 1 {
                    let d = (std::cmp::max(
                        1,
                        std::cmp::min(new_depth - r / 1024, new_depth + 1 + pv_node as i32),
                    ) + pv_node as i32)
                        .max(1);

                    let reduction_from_parent = (depth - 1) - d;
                    self.stack[ply as usize].reduction = reduction_from_parent;
                    let mut value = -self.search_node::<{ NodeType::NonPV as u8 }>(
                        pos,
                        d,
                        -alpha - Value::new(1),
                        -alpha,
                        ply + 1,
                        true,
                        limits,
                        time_manager,
                    );
                    self.stack[ply as usize].reduction = 0;

                    if value > alpha {
                        let do_deeper =
                            d < new_depth && value > (best_value + Value::new(43 + 2 * new_depth));
                        let do_shallower = value < best_value + Value::new(9);
                        new_depth += do_deeper as i32 - do_shallower as i32;

                        if new_depth > d {
                            value = -self.search_node::<{ NodeType::NonPV as u8 }>(
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
                        let moved_piece = mv.moved_piece_after();
                        let to_sq = mv.to();
                        const CONTHIST_BONUSES: &[(i32, i32)] =
                            &[(1, 1108), (2, 652), (3, 273), (4, 572), (5, 126), (6, 449)];
                        for &(offset, weight) in CONTHIST_BONUSES {
                            if self.stack[ply as usize].in_check && offset > 2 {
                                break;
                            }
                            let idx = ply - offset;
                            if idx < 0 {
                                break;
                            }
                            if let Some(key) = self.stack[idx as usize].cont_hist_key {
                                let in_check_idx = key.in_check as usize;
                                let capture_idx = key.capture as usize;
                                let bonus = 1412 * weight / 1024 + if offset < 2 { 80 } else { 0 };
                                self.history.continuation_history[in_check_idx][capture_idx]
                                    .update(key.piece, key.to, moved_piece, to_sq, bonus);
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
                        self.stack[ply as usize].reduction = 0;
                        -self.search_node::<{ NodeType::PV as u8 }>(
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
                    self.stack[ply as usize].reduction = 0;
                    let mut value = -self.search_node::<{ NodeType::NonPV as u8 }>(
                        pos,
                        depth - 1,
                        -alpha - Value::new(1),
                        -alpha,
                        ply + 1,
                        !cut_node,
                        limits,
                        time_manager,
                    );
                    self.stack[ply as usize].reduction = 0;

                    if pv_node && value > alpha && value < beta {
                        self.stack[ply as usize].reduction = 0;
                        value = -self.search_node::<{ NodeType::PV as u8 }>(
                            pos,
                            depth - 1,
                            -beta,
                            -alpha,
                            ply + 1,
                            false,
                            limits,
                            time_manager,
                        );
                        self.stack[ply as usize].reduction = 0;
                    }

                    value
                } else {
                    // Full window search
                    self.stack[ply as usize].reduction = 0;
                    -self.search_node::<{ NodeType::PV as u8 }>(
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
            self.nnue_stack.pop();
            pos.undo_move(mv);

            if self.abort {
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
                        let child_pv = self.stack[(ply + 1) as usize].pv.clone();
                        self.stack[ply as usize].update_pv(mv, &child_pv);
                    }

                    if value >= beta {
                        // cutoffCntインクリメント条件 (extension<2 || PvNode) をベータカット時に加算で近似。
                        self.stack[ply as usize].cutoff_cnt += 1;
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
        if best_move.is_some() {
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

                // MainHistory: そのまま渡す
                self.history.main_history.update(us, best_move, scaled_bonus);

                // LowPlyHistory: bonus * 771 / 1024 + 40
                if ply < LOW_PLY_HISTORY_SIZE as i32 {
                    let low_ply_bonus = low_ply_history_bonus(scaled_bonus);
                    self.history.low_ply_history.update(ply as usize, best_move, low_ply_bonus);
                }

                // ContinuationHistory: bonus * (bonus > 0 ? 979 : 842) / 1024 + weight + 80*(i<2)
                for &(ply_back, weight) in CONTINUATION_HISTORY_WEIGHTS.iter() {
                    if ply_back > max_ply_back {
                        continue;
                    }
                    if ply >= ply_back as i32 {
                        let prev_ply = (ply - ply_back as i32) as usize;
                        if let Some(key) = self.stack[prev_ply].cont_hist_key {
                            let in_check_idx = key.in_check as usize;
                            let capture_idx = key.capture as usize;
                            let weighted_bonus = continuation_history_bonus_with_offset(
                                scaled_bonus * weight / 1024,
                                ply_back,
                            );
                            self.history.continuation_history[in_check_idx][capture_idx].update(
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
                self.history
                    .pawn_history
                    .update(pawn_key_idx, best_cont_pc, best_to, pawn_bonus);

                // 他のquiet手にはペナルティ
                // YaneuraOu: update_quiet_histories(move, -quietMalus * 1115 / 1024)
                let scaled_malus = malus * 1115 / 1024;
                for &m in quiets_tried.iter() {
                    if m != best_move {
                        // MainHistory
                        self.history.main_history.update(us, m, -scaled_malus);

                        // LowPlyHistory（現行欠落していたので追加）
                        if ply < LOW_PLY_HISTORY_SIZE as i32 {
                            let low_ply_malus = low_ply_history_bonus(-scaled_malus);
                            self.history.low_ply_history.update(ply as usize, m, low_ply_malus);
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
                                if let Some(key) = self.stack[prev_ply].cont_hist_key {
                                    let in_check_idx = key.in_check as usize;
                                    let capture_idx = key.capture as usize;
                                    let weighted_malus = continuation_history_bonus_with_offset(
                                        -scaled_malus * weight / 1024,
                                        ply_back,
                                    );
                                    self.history.continuation_history[in_check_idx][capture_idx]
                                        .update(key.piece, key.to, cont_pc, to, weighted_malus);
                                }
                            }
                        }

                        // PawnHistoryへのペナルティ
                        let pawn_malus = pawn_history_bonus(-scaled_malus);
                        self.history.pawn_history.update(pawn_key_idx, cont_pc, to, pawn_malus);
                    }
                }
            } else {
                // 捕獲手がbest: captureHistoryを更新
                let captured_pt = pos.piece_on(best_to).piece_type();
                self.history.capture_history.update(best_cont_pc, best_to, captured_pt, bonus);
            }

            // YaneuraOu: 他の捕獲手へのペナルティ（capture best以外の全捕獲手）
            // captureMalus = min(708*depth-148, 2287) - 29*capturesSearched.size()
            let cap_malus = capture_malus(depth, captures_tried.len());
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
                    self.history.capture_history.update(
                        cont_pc,
                        to,
                        captured_pt,
                        -cap_malus * 1431 / 1024,
                    );
                }
            }

            // YaneuraOu: quiet early refutationペナルティ
            // 条件: prevSq != SQ_NONE && (ss-1)->moveCount == 1 + (ss-1)->ttHit && !pos.captured_piece()
            // 処理: update_continuation_histories(ss - 1, pos.piece_on(prevSq), prevSq, -captureMalus * 622 / 1024)
            if ply >= 1 {
                let prev_ply = (ply - 1) as usize;
                let prev_move_count = self.stack[prev_ply].move_count;
                let prev_tt_hit = self.stack[prev_ply].tt_hit;
                // YaneuraOu: !pos.captured_piece() = 現在の局面で駒が取られていない
                if prev_move_count == 1 + (prev_tt_hit as i32)
                    && pos.captured_piece() == Piece::NONE
                {
                    if let Some(key) = self.stack[prev_ply].cont_hist_key {
                        let prev_sq = key.to;
                        let prev_piece = pos.piece_on(prev_sq);
                        // YaneuraOu: update_continuation_histories(ss - 1, ...)を呼ぶ
                        // = 過去1-6手分全てに weight と +80 オフセット付きで更新
                        let penalty_base = -cap_malus * 622 / 1024;
                        // YaneuraOu: update_continuation_histories(ss - 1, ...) で (ss - 1)->inCheck を参照
                        let prev_in_check = self.stack[prev_ply].in_check;
                        let prev_max_ply_back = if prev_in_check { 2 } else { 6 };

                        for &(ply_back, weight) in CONTINUATION_HISTORY_WEIGHTS.iter() {
                            if ply_back > prev_max_ply_back {
                                continue;
                            }
                            // ss - 1 からさらに ply_back 手前 = ply - 1 - ply_back
                            let target_ply = ply - 1 - ply_back as i32;
                            if target_ply >= 0 {
                                if let Some(target_key) =
                                    self.stack[target_ply as usize].cont_hist_key
                                {
                                    let in_check_idx = target_key.in_check as usize;
                                    let capture_idx = target_key.capture as usize;
                                    let weighted_penalty = penalty_base * weight / 1024
                                        + if ply_back <= 2 {
                                            CONTINUATION_HISTORY_NEAR_PLY_OFFSET
                                        } else {
                                            0
                                        };
                                    self.history.continuation_history[in_check_idx][capture_idx]
                                        .update(
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

            // TTMoveHistory更新（非PVノードのみ）
            if !pv_node && tt_move.is_some() {
                if best_move == tt_move {
                    self.history.tt_move_history.update(ply as usize, TT_MOVE_HISTORY_BONUS);
                } else {
                    self.history.tt_move_history.update(ply as usize, TT_MOVE_HISTORY_MALUS);
                }
            }
        }
        // =================================================================
        // Prior Countermove Bonus（fail low時の前の手にボーナス）
        // YaneuraOu準拠: yaneuraou-search.cpp:3936-3977
        // =================================================================
        else if ply >= 1 {
            let prev_ply = (ply - 1) as usize;
            if let Some(prev_key) = self.stack[prev_ply].cont_hist_key {
                let prior_capture = prev_key.capture;
                let prev_sq = prev_key.to;

                if !prior_capture {
                    // Prior quiet countermove bonus
                    // YaneuraOu: yaneuraou-search.cpp:3945-3966
                    let parent_stat_score = self.stack[prev_ply].stat_score;
                    let parent_move_count = self.stack[prev_ply].move_count;
                    let parent_in_check = self.stack[prev_ply].in_check;
                    let parent_static_eval = self.stack[prev_ply].static_eval;
                    let static_eval = self.stack[ply as usize].static_eval;

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

                    // 値域: bonus_scale ∈ [0, ~913], min(...) ∈ [52, 1365] (depth>=1)
                    // 最大値 1365 * 913 ≈ 1.2M << i32::MAX なのでオーバーフローなし
                    let scaled_bonus = (144 * depth - 92).min(1365) * bonus_scale;

                    // continuation history更新
                    // YaneuraOu: update_continuation_histories(ss - 1, pos.piece_on(prevSq), prevSq, scaledBonus * 400 / 32768)
                    // 注: prev_sq は cont_hist_key.to（do_move後に設定）なので、
                    //     この時点で prev_piece != NONE が保証される
                    let prev_piece = pos.piece_on(prev_sq);
                    let prev_max_ply_back = if parent_in_check { 2 } else { 6 };
                    let cont_bonus = scaled_bonus * 400 / 32768;

                    for &(ply_back, weight) in CONTINUATION_HISTORY_WEIGHTS.iter() {
                        if ply_back > prev_max_ply_back {
                            continue;
                        }
                        // ss - 1 からさらに ply_back 手前 = ply - 1 - ply_back
                        let target_ply = ply - 1 - ply_back as i32;
                        if target_ply >= 0 {
                            if let Some(target_key) = self.stack[target_ply as usize].cont_hist_key
                            {
                                let in_check_idx = target_key.in_check as usize;
                                let capture_idx = target_key.capture as usize;
                                let weighted_bonus = cont_bonus * weight / 1024
                                    + if ply_back <= 2 {
                                        CONTINUATION_HISTORY_NEAR_PLY_OFFSET
                                    } else {
                                        0
                                    };
                                self.history.continuation_history[in_check_idx][capture_idx]
                                    .update(
                                        target_key.piece,
                                        target_key.to,
                                        prev_piece,
                                        prev_sq,
                                        weighted_bonus,
                                    );
                            }
                        }
                    }

                    // main history更新
                    // YaneuraOu: mainHistory[~us][((ss - 1)->currentMove).raw()] << scaledBonus * 220 / 32768
                    let prev_move = self.stack[prev_ply].current_move;
                    let main_bonus = scaled_bonus * 220 / 32768;
                    // 注: 前の手なので手番は!pos.side_to_move()
                    let opponent = !pos.side_to_move();
                    self.history.main_history.update(opponent, prev_move, main_bonus);

                    // pawn history更新（歩以外かつ成りでない場合）
                    // YaneuraOu: if (type_of(pos.piece_on(prevSq)) != PAWN && ((ss - 1)->currentMove).type_of() != PROMOTION)
                    if prev_piece.piece_type() != PieceType::Pawn && !prev_move.is_promotion() {
                        let pawn_key_idx = pos.pawn_history_index();
                        let pawn_bonus = scaled_bonus * 1164 / 32768;
                        self.history.pawn_history.update(
                            pawn_key_idx,
                            prev_piece,
                            prev_sq,
                            pawn_bonus,
                        );
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
                        self.history.capture_history.update(
                            prev_piece,
                            prev_sq,
                            captured_piece.piece_type(),
                            PRIOR_CAPTURE_COUNTERMOVE_BONUS,
                        );
                    }
                }
            }
        }

        // CorrectionHistoryの更新（YaneuraOu準拠）
        if !in_check && best_move.is_some() && !pos.is_capture(best_move) {
            let static_eval = self.stack[ply as usize].static_eval;
            if static_eval != Value::NONE
                && ((best_value < static_eval && best_value < beta) || best_value > static_eval)
            {
                let bonus = ((best_value.raw() - static_eval.raw()) * depth / 8)
                    .clamp(-CORRECTION_HISTORY_LIMIT / 4, CORRECTION_HISTORY_LIMIT / 4);
                self.update_correction_history(pos, ply, bonus);
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
                self.tt.generation(),
            );
        }

        best_value
    }

    /// 静止探索
    fn qsearch<const NT: u8>(
        &mut self,
        pos: &mut Position,
        depth: Depth,
        alpha: Value,
        beta: Value,
        ply: i32,
        limits: &LimitsType,
        time_manager: &mut TimeManagement,
    ) -> Value {
        let pv_node = NT == NodeType::PV as u8;
        let in_check = pos.in_check();

        if ply >= MAX_PLY {
            return if in_check {
                Value::ZERO
            } else {
                evaluate(pos, &mut self.nnue_stack)
            };
        }

        if pv_node && self.sel_depth < ply + 1 {
            self.sel_depth = ply + 1;
        }

        if self.check_abort(limits, time_manager) {
            return Value::ZERO;
        }

        let rep_state = pos.repetition_state(ply);
        if rep_state.is_repetition() || rep_state.is_superior_inferior() {
            let v = draw_value(rep_state, pos.side_to_move());
            if v != Value::NONE {
                if v == Value::DRAW {
                    let jittered = Value::new(v.raw() + draw_jitter(self.nodes));
                    return value_from_tt(jittered, ply);
                }
                return value_from_tt(v, ply);
            }
        }

        // 引き分け手数ルール（YaneuraOu準拠、MaxMovesToDrawオプション）
        if self.max_moves_to_draw > 0 && pos.game_ply() > self.max_moves_to_draw {
            return Value::new(Value::DRAW.raw() + draw_jitter(self.nodes));
        }

        let key = pos.key();
        let tt_result = self.tt.probe(key, pos);
        let tt_hit = tt_result.found;
        let tt_data = tt_result.data;
        let pv_hit = tt_hit && tt_data.is_pv;
        self.stack[ply as usize].tt_hit = tt_hit;
        self.stack[ply as usize].tt_pv = pv_hit;
        let mut tt_move = if tt_hit { tt_data.mv } else { Move::NONE };
        let tt_value = if tt_hit {
            value_from_tt(tt_data.value, ply)
        } else {
            Value::NONE
        };

        if !pv_node
            && tt_hit
            && tt_data.depth >= DEPTH_QS
            && tt_value != Value::NONE
            && tt_data.bound.can_cutoff(tt_value, beta)
        {
            return tt_value;
        }

        let mut best_move = Move::NONE;

        let correction_value = self.correction_value(pos, ply);
        let mut unadjusted_static_eval = Value::NONE;
        let mut static_eval = if in_check {
            Value::NONE
        } else if tt_hit && tt_data.eval != Value::NONE {
            unadjusted_static_eval = tt_data.eval;
            unadjusted_static_eval
        } else {
            // 置換表に無いときだけ簡易1手詰め判定を行う
            if !tt_hit {
                let mate_move = pos.mate_1ply();
                if mate_move.is_some() {
                    return Value::mate_in(ply + 1);
                }
            }
            unadjusted_static_eval = evaluate(pos, &mut self.nnue_stack);
            unadjusted_static_eval
        };

        if !in_check && unadjusted_static_eval != Value::NONE {
            static_eval = to_corrected_static_eval(unadjusted_static_eval, correction_value);
        }

        self.stack[ply as usize].static_eval = static_eval;

        let mut alpha = alpha;
        let mut best_value = if in_check {
            Value::mated_in(ply)
        } else {
            static_eval
        };

        if !in_check && tt_hit && tt_value != Value::NONE && !tt_value.is_mate_score() {
            let improves = (tt_value > best_value && tt_data.bound == Bound::Lower)
                || (tt_value < best_value && tt_data.bound == Bound::Upper);
            if improves {
                best_value = tt_value;
                static_eval = tt_value;
                self.stack[ply as usize].static_eval = static_eval;
            }
        }

        if !in_check && best_value >= beta {
            let mut v = best_value;
            if !v.is_mate_score() {
                v = Value::new((v.raw() + beta.raw()) / 2);
            }
            if !tt_hit {
                // YaneuraOu: pvHitを使用
                tt_result.write(
                    key,
                    value_to_tt(v, ply),
                    pv_hit,
                    Bound::Lower,
                    DEPTH_UNSEARCHED,
                    Move::NONE,
                    unadjusted_static_eval,
                    self.tt.generation(),
                );
            }
            return v;
        }

        if !in_check && best_value > alpha {
            alpha = best_value;
        }

        let futility_base = if in_check {
            Value::NONE
        } else {
            static_eval + Value::new(352)
        };

        if depth <= DEPTH_QS
            && tt_move.is_some()
            && ((!pos.capture_stage(tt_move) && !pos.gives_check(tt_move)) || depth < -16)
        {
            tt_move = Move::NONE;
        }

        let prev_move = if ply >= 1 {
            self.stack[(ply - 1) as usize].current_move
        } else {
            Move::NONE
        };

        let ordered_moves = {
            let cont_tables = self.cont_history_tables(ply);
            let mut buf_moves = OrderedMovesBuffer::new();

            {
                let mp = if in_check {
                    MovePicker::new_evasions(
                        pos,
                        tt_move,
                        &self.history.main_history,
                        &self.history.low_ply_history,
                        &self.history.capture_history,
                        cont_tables,
                        &self.history.pawn_history,
                        ply,
                        self.generate_all_legal_moves,
                    )
                } else {
                    MovePicker::new(
                        pos,
                        tt_move,
                        DEPTH_QS,
                        &self.history.main_history,
                        &self.history.low_ply_history,
                        &self.history.capture_history,
                        cont_tables,
                        &self.history.pawn_history,
                        ply,
                        self.generate_all_legal_moves,
                    )
                };

                for mv in mp {
                    buf_moves.push(mv);
                }
            }

            if !in_check && depth == DEPTH_QS {
                let mut buf = crate::movegen::ExtMoveBuffer::new();
                let gen_type = if self.generate_all_legal_moves {
                    crate::movegen::GenType::QuietChecksAll
                } else {
                    crate::movegen::GenType::QuietChecks
                };
                crate::movegen::generate_with_type(pos, gen_type, &mut buf, None);
                for ext in buf.iter() {
                    if buf_moves.contains(&ext.mv) {
                        continue;
                    }
                    buf_moves.push(ext.mv);
                }
            }

            if !in_check && depth <= -5 && ply >= 1 && !prev_move.is_none() {
                let mut buf = crate::movegen::ExtMoveBuffer::new();
                let rec_sq = prev_move.to();
                let gen_type = if self.generate_all_legal_moves {
                    crate::movegen::GenType::RecapturesAll
                } else {
                    crate::movegen::GenType::Recaptures
                };
                crate::movegen::generate_with_type(pos, gen_type, &mut buf, Some(rec_sq));
                buf_moves.clear();
                for ext in buf.iter() {
                    buf_moves.push(ext.mv);
                }
            }

            buf_moves
        };

        let mut move_count = 0;

        for mv in ordered_moves.iter() {
            if !pos.is_legal(mv) {
                continue;
            }

            let gives_check = pos.gives_check(mv);
            let capture = pos.capture_stage(mv);

            if !in_check && depth <= DEPTH_QS && !capture && !gives_check {
                continue;
            }

            if !in_check && capture && !pos.see_ge(mv, Value::ZERO) {
                continue;
            }

            move_count += 1;

            if !best_value.is_loss() {
                if !gives_check
                    && (prev_move.is_none() || mv.to() != prev_move.to())
                    && futility_base != Value::NONE
                {
                    if move_count > 2 {
                        continue;
                    }

                    let futility_value =
                        futility_base + Value::new(piece_value(pos.piece_on(mv.to())));

                    if futility_value <= alpha {
                        best_value = best_value.max(futility_value);
                        continue;
                    }

                    if !pos.see_ge(mv, alpha - futility_base) {
                        best_value = best_value.min(alpha.min(futility_base));
                        continue;
                    }
                }
                if !capture {
                    let mut cont_score = 0;

                    // ss-1の参照（ContinuationHistory直結）
                    cont_score +=
                        self.cont_history_ref(ply, 1).get(mv.moved_piece_after(), mv.to()) as i32;

                    let pawn_idx = pos.pawn_history_index();
                    cont_score +=
                        self.history.pawn_history.get(pawn_idx, pos.moved_piece(mv), mv.to())
                            as i32;
                    if cont_score <= 5868 {
                        continue;
                    }
                }

                if !pos.see_ge(mv, Value::new(-74)) {
                    continue;
                }
            }

            self.stack[ply as usize].current_move = mv;
            let cont_hist_pc = mv.moved_piece_after();
            let cont_hist_to = mv.to();

            let dirty_piece = pos.do_move_with_prefetch(mv, gives_check, self.tt.as_ref());
            self.nnue_stack.push(dirty_piece);
            self.nodes += 1;

            self.set_cont_history_for_move(ply, in_check, capture, cont_hist_pc, cont_hist_to);

            let value =
                -self.qsearch::<NT>(pos, depth - 1, -beta, -alpha, ply + 1, limits, time_manager);

            self.nnue_stack.pop();
            pos.undo_move(mv);

            if self.abort {
                return Value::ZERO;
            }

            if value > best_value {
                best_value = value;
                best_move = mv;

                if value > alpha {
                    alpha = value;

                    if value >= beta {
                        break;
                    }
                }
            }
        }

        if in_check && move_count == 0 {
            return Value::mated_in(ply);
        }

        if !best_value.is_mate_score() && best_value > beta {
            best_value = Value::new((best_value.raw() + beta.raw()) / 2);
        }

        let bound = if best_value >= beta {
            Bound::Lower
        } else if pv_node && best_move.is_some() {
            Bound::Exact
        } else {
            Bound::Upper
        };

        // YaneuraOu: pvHitを使用
        tt_result.write(
            key,
            value_to_tt(best_value, ply),
            pv_hit,
            bound,
            DEPTH_QS,
            best_move,
            unadjusted_static_eval,
            self.tt.generation(),
        );

        best_value
    }
}

// SAFETY: SearchWorkerは単一スレッドで使用される前提。
// StackArray内の各Stackが持つ `cont_history_ptr: NonNull<PieceToHistory>` は
// `self.history.continuation_history` 内のテーブルへの参照である。
// SearchWorkerがスレッド間でmoveされても、history フィールドも一緒にmoveされるため、
// ポインタの参照先は常に有効であり、データ競合も発生しない。
unsafe impl Send for SearchWorker {}

// =============================================================================
// テスト
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reduction_values() {
        // reduction(true, 10, 5) などが正の値を返すことを確認
        // LazyLockにより初回アクセス時に自動初期化される
        let root_delta = 64;
        let delta = 32;
        assert!(reduction(true, 10, 5, delta, root_delta) / 1024 >= 0);
        assert!(
            reduction(false, 10, 5, delta, root_delta) / 1024
                >= reduction(true, 10, 5, delta, root_delta) / 1024
        );
    }

    #[test]
    fn test_reduction_bounds() {
        // 境界値テスト
        let root_delta = 64;
        let delta = 32;
        assert_eq!(reduction(true, 0, 0, delta, root_delta), 0); // depth=0, mc=0 は計算外
        assert!(reduction(true, 63, 63, delta, root_delta) / 1024 < 64);
        assert!(reduction(false, 63, 63, delta, root_delta) / 1024 < 64);
    }

    /// depth/move_countが大きい場合にreductionが正の値を返すことを確認
    #[test]
    fn test_reduction_returns_nonzero_for_large_values() {
        let root_delta = 64;
        let delta = 32;
        // 深い探索で多くの手を試した場合、reductionは正の値であるべき
        let r = reduction(false, 10, 10, delta, root_delta) / 1024;
        assert!(
            r > 0,
            "reduction should return positive value for depth=10, move_count=10, got {r}"
        );

        // improving=trueの場合は若干小さい値になる
        let r_imp = reduction(true, 10, 10, delta, root_delta) / 1024;
        assert!(r >= r_imp, "non-improving should have >= reduction than improving");
    }

    /// 境界ケース: depth=1, move_count=1でもreduction関数が動作することを確認
    #[test]
    fn test_reduction_small_values() {
        let root_delta = 64;
        let delta = 32;
        // 小さな値でもpanicしないことを確認
        let r = reduction(true, 1, 1, delta, root_delta) / 1024;
        assert!(r >= 0, "reduction should not be negative");
    }

    #[test]
    fn test_reduction_extremes_no_overflow() {
        // 最大depth/mcでもオーバーフローせずに値が得られることを確認
        let delta = 0;
        let root_delta = 1;
        let r = reduction(false, 63, 63, delta, root_delta);
        assert!(
            (0..i32::MAX / 2).contains(&r),
            "reduction extreme should be in safe range, got {r}"
        );
    }

    #[test]
    fn test_reduction_zero_root_delta_clamped() {
        // root_delta=0 を渡しても内部で1にクランプされることを確認
        let r = reduction(false, 10, 10, 0, 0) / 1024;
        assert!(r >= 0, "reduction should clamp root_delta to >=1 even when 0 is passed");
    }

    #[test]
    fn test_sentinel_initialization() {
        use std::sync::Arc;

        // SearchWorker作成時にsentinelが正しく初期化されることを確認
        let tt = Arc::new(TranspositionTable::new(16));
        let worker = SearchWorker::new(tt, 0, 0);

        // sentinelポインタがdanglingではなく、実際のテーブルを指していることを確認
        let sentinel = worker.cont_history_sentinel;
        // NonNullはnullにならないことが保証されているので、
        // 代わりにsafeにderefできることを確認（ポインタが有効なメモリを指していること）
        let sentinel_ref = unsafe { sentinel.as_ref() };
        // PieceToHistoryテーブルはゼロ初期化されているはず
        assert_eq!(
            sentinel_ref.get(crate::types::Piece::B_PAWN, crate::types::Square::SQ_11),
            0,
            "sentinel table should be zero-initialized"
        );

        // 全てのスタックエントリがsentinelで初期化されていることを確認
        for (i, stack) in worker.stack.iter().enumerate() {
            assert_eq!(
                stack.cont_history_ptr, sentinel,
                "stack[{i}].cont_history_ptr should be initialized to sentinel"
            );
        }
    }

    #[test]
    fn test_cont_history_ptr_returns_sentinel_for_negative_offset() {
        use std::sync::Arc;

        let tt = Arc::new(TranspositionTable::new(16));
        let worker = SearchWorker::new(tt, 0, 0);

        // ply < back の場合はsentinelを返すことを確認
        let ptr = worker.cont_history_ptr(0, 1);
        assert_eq!(ptr, worker.cont_history_sentinel);

        let ptr = worker.cont_history_ptr(3, 5);
        assert_eq!(ptr, worker.cont_history_sentinel);
    }

    #[test]
    fn test_skip_params_matches_legacy_pattern() {
        let expected_sizes = [
            1usize, 1, 2, 2, 2, 2, 3, 3, 3, 3, 3, 3, 4, 4, 4, 4, 4, 4, 4, 4,
        ];
        let expected_phases = [
            0usize, 1, 0, 1, 2, 3, 0, 1, 2, 3, 4, 5, 0, 1, 2, 3, 4, 5, 6, 7,
        ];

        assert_eq!(SearchWorker::skip_params(0), (0, 0));

        for (idx, (&size, &phase)) in expected_sizes.iter().zip(expected_phases.iter()).enumerate()
        {
            let thread_id = idx + 1;
            let (skip_size, skip_phase) = SearchWorker::skip_params(thread_id);
            assert_eq!(skip_size, size, "thread_id={thread_id} skip_size mismatch");
            assert_eq!(skip_phase, phase, "thread_id={thread_id} skip_phase mismatch");
        }
    }
}
