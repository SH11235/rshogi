//! Alpha-Beta探索の実装
//!
//! YaneuraOu準拠のAlpha-Beta探索。
//! - Principal Variation Search (PVS)
//! - 静止探索 (Quiescence Search)
//! - 各種枝刈り: NMP, LMR, Futility, Razoring, SEE, Singular Extension

use crate::nnue::evaluate;
use crate::position::Position;
use crate::search::PieceToHistory;
use crate::tt::TranspositionTable;
use crate::types::{Bound, Color, Depth, Move, Piece, Value, DEPTH_QS, DEPTH_UNSEARCHED, MAX_PLY};

use super::history::{
    capture_malus, continuation_history_bonus_with_offset, low_ply_history_bonus,
    pawn_history_bonus, quiet_malus, stat_bonus, ButterflyHistory, CapturePieceToHistory,
    ContinuationHistory, CorrectionHistory, LowPlyHistory, PawnHistory,
    CONTINUATION_HISTORY_WEIGHTS, CORRECTION_HISTORY_LIMIT, CORRECTION_HISTORY_SIZE,
    LOW_PLY_HISTORY_SIZE, TT_MOVE_HISTORY_BONUS, TT_MOVE_HISTORY_MALUS,
};
use super::movepicker::piece_value;
use super::tt_history::TTMoveHistory;
use super::types::{
    draw_value, init_stack_array, value_from_tt, value_to_tt, ContHistKey, NodeType, RootMoves,
    StackArray,
};
use super::{LimitsType, MovePicker, TimeManagement};

// =============================================================================
// 定数
// =============================================================================

/// Futility margin（depth × 係数）
const FUTILITY_MARGIN_BASE: i32 = 117;

use std::sync::OnceLock;

/// 引き分けスコアに揺らぎを与える（YaneuraOu準拠）
#[inline]
fn draw_jitter(nodes: u64) -> i32 {
    // VALUE_DRAW - 1 + (nodes & 0x2) 相当の±1ゆらぎ
    ((nodes & 0x2) as i32) - 1
}

/// 補正履歴を適用した静的評価に変換（詰みスコア領域に入り込まないようにクリップ）
#[inline]
fn to_corrected_static_eval(unadjusted: Value, correction_value: i32) -> Value {
    let corrected = unadjusted.raw() + correction_value / 131_072;
    Value::new(corrected.clamp(Value::MATED_IN_MAX_PLY.raw() + 1, Value::MATE_IN_MAX_PLY.raw() - 1))
}

/// LMR用のreduction配列
type Reductions = [[[i32; 64]; 64]; 2];

/// Reduction配列（遅延初期化）
static REDUCTIONS: OnceLock<Box<Reductions>> = OnceLock::new();

/// reduction配列を初期化
pub fn init_reductions() {
    REDUCTIONS.get_or_init(|| {
        let mut table: Box<Reductions> = Box::new([[[0; 64]; 64]; 2]);
        for imp_idx in 0..2 {
            for d in 1..64 {
                for mc in 1..64 {
                    let r = (1.56 + (d as f64).ln() * (mc as f64).ln() / 2.17).floor() as i32;
                    table[imp_idx][d][mc] = r - (imp_idx as i32);
                }
            }
        }
        table
    });
}

/// Reductionを取得
///
/// # Panics
/// `init_reductions()`が呼ばれていない場合にpanicする
#[inline]
fn reduction(imp: bool, depth: i32, move_count: i32) -> i32 {
    let imp_idx = if imp { 1 } else { 0 };
    let d = (depth as usize).min(63);
    let mc = (move_count as usize).min(63);

    REDUCTIONS
        .get()
        .expect("REDUCTIONS not initialized. Call init_reductions() at startup.")[imp_idx][d][mc]
}

/// Reductionテーブルが初期化済みかどうかを確認
pub fn is_reductions_initialized() -> bool {
    REDUCTIONS.get().is_some()
}

// =============================================================================
// SearchWorker
// =============================================================================

/// 探索用のワーカー状態
///
/// # Arguments
/// - 引数が多いのはYaneuraOu準拠のため。探索のコアロジックは状態を多く持つ必要がある。
pub struct SearchWorker<'a> {
    /// 置換表への参照
    pub tt: &'a TranspositionTable,

    /// 探索制限
    pub limits: &'a LimitsType,

    /// 時間管理
    pub time_manager: &'a mut TimeManagement,

    /// ルート手
    pub root_moves: RootMoves,

    /// 探索スタック
    pub stack: StackArray,

    // History統計
    pub main_history: ButterflyHistory,
    pub pawn_history: PawnHistory,
    pub capture_history: CapturePieceToHistory,
    pub continuation_history: [[ContinuationHistory; 2]; 2],
    pub low_ply_history: LowPlyHistory,
    pub tt_move_history: TTMoveHistory,
    pub correction_history: CorrectionHistory,

    /// 探索ノード数
    pub nodes: u64,

    /// 選択的深さ
    pub sel_depth: i32,

    /// ルート深さ
    pub root_depth: Depth,

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
}

impl<'a> SearchWorker<'a> {
    /// 新しいSearchWorkerを作成
    ///
    /// REDUCTIONSテーブルが初期化済みであることを前提にする。
    pub fn new(
        tt: &'a TranspositionTable,
        limits: &'a LimitsType,
        time_manager: &'a mut TimeManagement,
        max_moves_to_draw: i32,
    ) -> Self {
        assert!(
            is_reductions_initialized(),
            "REDUCTIONS not initialized. Call init_reductions() at startup."
        );
        Self {
            tt,
            limits,
            time_manager,
            root_moves: RootMoves::new(),
            stack: init_stack_array(),
            main_history: ButterflyHistory::new(),
            pawn_history: PawnHistory::new(),
            capture_history: CapturePieceToHistory::new(),
            continuation_history: Default::default(),
            low_ply_history: LowPlyHistory::new(),
            tt_move_history: TTMoveHistory::new(),
            correction_history: CorrectionHistory::new(),
            nodes: 0,
            sel_depth: 0,
            root_depth: 0,
            completed_depth: 0,
            generate_all_legal_moves: false,
            best_move: Move::NONE,
            abort: false,
            best_move_changes: 0.0,
            max_moves_to_draw,
        }
    }

    /// best_move_changes を半減（世代減衰）
    ///
    /// YaneuraOu準拠: 反復深化の各世代終了時に呼び出して、
    /// 古い情報の重みを低くする
    pub fn decay_best_move_changes(&mut self) {
        self.best_move_changes /= 2.0;
    }

    /// 全合法手生成モードの設定（YaneuraOu互換）
    pub fn set_generate_all_legal_moves(&mut self, flag: bool) {
        self.generate_all_legal_moves = flag;
    }

    /// 中断チェック
    #[inline]
    fn check_abort(&mut self) -> bool {
        if self.abort {
            return true;
        }

        // ノード数制限チェック
        if self.limits.nodes > 0 && self.nodes >= self.limits.nodes {
            self.abort = true;
            return true;
        }

        // 時間制限チェック（1024ノードごと）
        // YaneuraOu準拠の2フェーズロジック
        if self.nodes & 1023 == 0 {
            let elapsed = self.time_manager.elapsed();

            // フェーズ1: search_end 設定済み → 即座に停止
            if self.time_manager.search_end() > 0 && elapsed >= self.time_manager.search_end() {
                self.abort = true;
                return true;
            }

            // フェーズ2: search_end 未設定 → maximum超過時に設定
            if self.time_manager.search_end() == 0
                && self.limits.use_time_management()
                && elapsed > self.time_manager.maximum()
            {
                self.time_manager.set_search_end(elapsed);
                // 注: ここでは停止せず、次のチェックで秒境界で停止
            }
        }

        false
    }

    /// ContHistKeyからContinuationHistoryテーブルへの参照を構築（YaneuraOu方式）
    ///
    /// 過去1,2,3,4,5,6手分のContinuationHistoryテーブルへの参照を配列で返す。
    /// plyが足りない場合やContHistKeyがない場合はNoneになる。
    #[inline]
    fn build_cont_tables(&self, ply: i32) -> [Option<&PieceToHistory>; 6] {
        let mut tables: [Option<&PieceToHistory>; 6] = [None; 6];
        for (idx, ply_back) in [1, 2, 3, 4, 5, 6].iter().enumerate() {
            if ply >= *ply_back {
                let prev_ply = (ply - *ply_back) as usize;
                if let Some(key) = self.stack[prev_ply].cont_hist_key {
                    let in_check_idx = key.in_check as usize;
                    let capture_idx = key.capture as usize;
                    tables[idx] = Some(
                        self.continuation_history[in_check_idx][capture_idx]
                            .get_table(key.piece, key.to),
                    );
                }
            }
        }
        tables
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

        let pcv = self.correction_history.pawn_value(pawn_idx, us) as i32;
        let micv = self.correction_history.minor_value(minor_idx, us) as i32;
        let wnpcv = self.correction_history.non_pawn_value(non_pawn_idx_w, Color::White, us) as i32;
        let bnpcv = self.correction_history.non_pawn_value(non_pawn_idx_b, Color::Black, us) as i32;

        let mut cntcv = 0;
        if ply >= 2 {
            let prev_move = self.stack[(ply - 1) as usize].current_move;
            if prev_move.is_some() {
                if let Some(prev2_key) = self.stack[(ply - 2) as usize].cont_hist_key {
                    let pc = pos.piece_on(prev_move.to());
                    cntcv = self.correction_history.continuation_value(
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

        self.correction_history.update_pawn(pawn_idx, us, bonus);
        self.correction_history.update_minor(minor_idx, us, bonus * 153 / 128);
        self.correction_history.update_non_pawn(
            non_pawn_idx_w,
            Color::White,
            us,
            bonus * NON_PAWN_WEIGHT / 128,
        );
        self.correction_history.update_non_pawn(
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
                    self.correction_history.update_continuation(
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

    /// 探索のメインエントリーポイント
    ///
    /// 反復深化で指定された深さまで探索する。
    pub fn search(&mut self, pos: &mut Position, depth: Depth) {
        // ルート手を初期化
        self.root_moves = RootMoves::from_legal_moves(pos, &self.limits.search_moves);

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
                let score = self.search_root(pos, d, alpha, beta);

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
    ) -> Value {
        let mut alpha = alpha;
        let mut best_value = Value::new(-32001);
        let mut pv_idx = 0;

        for rm_idx in 0..self.root_moves.len() {
            if self.check_abort() {
                return Value::ZERO;
            }

            // 各手ごとにsel_depthをリセット（YaneuraOu準拠）
            self.sel_depth = 0;

            let mv = self.root_moves[rm_idx].mv();
            let gives_check = pos.gives_check(mv);

            let nodes_before = self.nodes;

            // 探索
            pos.do_move(mv, gives_check);
            self.nodes += 1;

            // PVS
            let value = if rm_idx == 0 {
                -self.search_node::<{ NodeType::PV as u8 }>(pos, depth - 1, -beta, -alpha, 1)
            } else {
                // Zero Window Search
                let mut value = -self.search_node::<{ NodeType::NonPV as u8 }>(
                    pos,
                    depth - 1,
                    -alpha - Value::new(1),
                    -alpha,
                    1,
                );

                // Re-search if needed
                if value > alpha && value < beta {
                    value = -self.search_node::<{ NodeType::PV as u8 }>(
                        pos,
                        depth - 1,
                        -beta,
                        -alpha,
                        1,
                    );
                }

                value
            };

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
    pub(crate) fn search_root_for_pv(
        &mut self,
        pos: &mut Position,
        depth: Depth,
        alpha: Value,
        beta: Value,
        pv_idx: usize,
    ) -> Value {
        let mut alpha = alpha;
        let mut best_value = Value::new(-32001);
        let mut best_rm_idx = pv_idx;

        // pv_idx以降の手のみを探索
        for rm_idx in pv_idx..self.root_moves.len() {
            if self.check_abort() {
                return Value::ZERO;
            }

            // 各手ごとにsel_depthをリセット
            self.sel_depth = 0;

            let mv = self.root_moves[rm_idx].mv();
            let gives_check = pos.gives_check(mv);

            let nodes_before = self.nodes;

            // 探索
            pos.do_move(mv, gives_check);
            self.nodes += 1;

            // PVS: 最初の手（このPVラインの候補）はPV探索
            let value = if rm_idx == pv_idx {
                -self.search_node::<{ NodeType::PV as u8 }>(pos, depth - 1, -beta, -alpha, 1)
            } else {
                // それ以降はZero Window Search
                let mut value = -self.search_node::<{ NodeType::NonPV as u8 }>(
                    pos,
                    depth - 1,
                    -alpha - Value::new(1),
                    -alpha,
                    1,
                );

                // Re-search if needed
                if value > alpha && value < beta {
                    value = -self.search_node::<{ NodeType::PV as u8 }>(
                        pos,
                        depth - 1,
                        -beta,
                        -alpha,
                        1,
                    );
                }

                value
            };

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
    fn search_node<const NT: u8>(
        &mut self,
        pos: &mut Position,
        depth: Depth,
        alpha: Value,
        beta: Value,
        ply: i32,
    ) -> Value {
        let pv_node = NT == NodeType::PV as u8 || NT == NodeType::Root as u8;
        let in_check = pos.in_check();

        // 深さが0以下なら静止探索へ
        if depth <= DEPTH_QS {
            return self.qsearch::<NT>(pos, depth, alpha, beta, ply);
        }

        // 最大深さチェック
        if ply >= MAX_PLY {
            return if in_check { Value::ZERO } else { evaluate(pos) };
        }

        // 選択的深さを更新
        if pv_node && self.sel_depth < ply + 1 {
            self.sel_depth = ply + 1;
        }

        // 中断チェック
        if self.check_abort() {
            return Value::ZERO;
        }

        // スタック設定
        self.stack[ply as usize].in_check = in_check;
        self.stack[ply as usize].move_count = 0;

        // =================================================================
        // 置換表プローブ
        // =================================================================
        let key = pos.key();
        let tt_result = self.tt.probe(key, pos);
        let tt_hit = tt_result.found;
        let tt_data = tt_result.data;

        // YaneuraOu準拠: tt_hit/tt_pvをスタックに記録
        self.stack[ply as usize].tt_hit = tt_hit;
        self.stack[ply as usize].tt_pv = pv_node || (tt_hit && tt_data.is_pv);
        let mut tt_move = if tt_hit { tt_data.mv } else { Move::NONE };
        let tt_value = if tt_hit {
            value_from_tt(tt_data.value, ply)
        } else {
            Value::NONE
        };

        // TTカットオフ（非PVノード）
        if !pv_node
            && tt_hit
            && tt_data.depth >= depth
            && tt_value != Value::NONE
            && tt_data.bound.can_cutoff(tt_value, beta)
        {
            return tt_value;
        }

        // 1手詰め判定（置換表未ヒット時のみ、Rootでは実施しない）
        if NT != NodeType::Root as u8 && !in_check && !tt_hit {
            let mate_move = pos.mate_1ply();
            if mate_move.is_some() {
                let value = Value::mate_in(ply + 1);
                let stored_depth = (depth + 6).min(MAX_PLY - 1);
                // YaneuraOu準拠: mate_in は root からの手数込みなので value_to_tt は通さずそのまま保存
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
                return value;
            }
        }

        // =================================================================
        // 静的評価
        // =================================================================
        let correction_value = self.correction_value(pos, ply);
        let mut unadjusted_static_eval = Value::NONE;
        let mut static_eval = if in_check {
            Value::NONE
        } else if tt_hit && tt_data.eval != Value::NONE {
            unadjusted_static_eval = tt_data.eval;
            unadjusted_static_eval
        } else {
            unadjusted_static_eval = evaluate(pos);
            unadjusted_static_eval
        };

        if !in_check && unadjusted_static_eval != Value::NONE {
            static_eval = to_corrected_static_eval(unadjusted_static_eval, correction_value);
        }

        if !in_check
            && tt_hit
            && tt_value != Value::NONE
            && !tt_value.is_mate_score()
            && ((tt_value > static_eval && tt_data.bound == Bound::Lower)
                || (tt_value < static_eval && tt_data.bound == Bound::Upper))
        {
            static_eval = tt_value;
        }

        self.stack[ply as usize].static_eval = static_eval;

        // improving判定
        let mut improving = if ply >= 2 && !in_check {
            static_eval > self.stack[(ply - 2) as usize].static_eval
        } else {
            false
        };

        // =================================================================
        // Razoring（非PV、浅い深さで評価値が低い場合に静止探索）
        // =================================================================
        if !pv_node && !in_check && depth <= 3 {
            let razoring_threshold = alpha - Value::new(200 * depth);
            if static_eval < razoring_threshold {
                let value =
                    self.qsearch::<{ NodeType::NonPV as u8 }>(pos, DEPTH_QS, alpha, beta, ply);
                if value <= alpha {
                    return value;
                }
            }
        }

        // =================================================================
        // Futility Pruning（非PV、静的評価が十分高い場合）
        // =================================================================
        if !pv_node && !in_check && depth <= 8 && static_eval != Value::NONE {
            let futility_margin =
                Value::new(FUTILITY_MARGIN_BASE * depth + (correction_value.abs() / 171_290));
            if static_eval - futility_margin >= beta {
                return static_eval;
            }
        }

        // =================================================================
        // ProbCut（YaneuraOu準拠の簡易版）
        // =================================================================
        if !in_check && depth >= 3 && static_eval != Value::NONE {
            // YaneuraOu: improving再計算（static_eval >= betaなら改善とみなす）
            let improving_prob = improving || static_eval >= beta;
            let prob_beta = beta + Value::new(215 - 60 * improving_prob as i32);
            if !(beta.is_mate_score()
                || (tt_hit && tt_value != Value::NONE && tt_value < prob_beta))
            {
                let threshold = prob_beta - static_eval;
                if threshold > Value::ZERO {
                    let probcut_moves = {
                        let cont_tables = self.build_cont_tables(ply);
                        let mp = MovePicker::new_probcut(
                            pos,
                            tt_move,
                            threshold,
                            &self.main_history,
                            &self.low_ply_history,
                            &self.capture_history,
                            cont_tables,
                            &self.pawn_history,
                            ply,
                            self.generate_all_legal_moves,
                        );
                        let mut buf = Vec::new();
                        for mv in mp {
                            buf.push(mv);
                        }
                        buf
                    };

                    let dynamic_reduction = (static_eval - beta).raw() / 300;
                    let probcut_depth = (depth - 5 - dynamic_reduction).max(0);

                    // YaneuraOu準拠: 全捕獲手を試す（PV:2手/NonPV:4手の制限を撤廃）
                    for mv in probcut_moves {
                        if !pos.is_legal(mv) {
                            continue;
                        }

                        pos.do_move(mv, pos.gives_check(mv));
                        self.nodes += 1;
                        let mut value = -self.qsearch::<{ NodeType::NonPV as u8 }>(
                            pos,
                            DEPTH_QS,
                            -prob_beta,
                            -prob_beta + Value::new(1),
                            ply + 1,
                        );

                        if value >= prob_beta && probcut_depth > 0 {
                            value = -self.search_node::<{ NodeType::NonPV as u8 }>(
                                pos,
                                probcut_depth,
                                -prob_beta,
                                -prob_beta + Value::new(1),
                                ply + 1,
                            );
                        }
                        pos.undo_move(mv);

                        if value >= prob_beta {
                            let stored_depth = (probcut_depth + 1).max(1);
                            // YaneuraOu: ss->ttPvを使用
                            tt_result.write(
                                key,
                                value_to_tt(value, ply),
                                self.stack[ply as usize].tt_pv,
                                Bound::Lower,
                                stored_depth,
                                mv,
                                unadjusted_static_eval,
                                self.tt.generation(),
                            );

                            if value.raw().abs() < Value::INFINITE.raw() {
                                return value - (prob_beta - beta);
                            }
                            return value;
                        }
                    }
                }
            }
        }

        // small probcut
        if depth >= 1 {
            let sp_beta = beta + Value::new(417);
            if tt_hit
                && tt_data.bound == Bound::Lower
                && tt_data.depth >= depth - 4
                && tt_value != Value::NONE
                && tt_value >= sp_beta
                && !tt_value.is_mate_score()
                && !beta.is_mate_score()
            {
                return sp_beta;
            }
        }

        // =================================================================
        // Null Move Pruning
        // =================================================================
        if !pv_node
            && !in_check
            && static_eval >= beta
            && depth >= 3
            && ply >= 1
            && !self.stack[(ply - 1) as usize].current_move.is_none()
        {
            let r = 3 + depth / 3;
            pos.do_null_move();
            let null_value = -self.search_node::<{ NodeType::NonPV as u8 }>(
                pos,
                depth - r,
                -beta,
                -beta + Value::new(1),
                ply + 1,
            );
            pos.undo_null_move();

            if null_value >= beta {
                // 詰みスコアは信頼しない
                if null_value.is_win() {
                    return beta;
                }
                return null_value;
            }
        }

        // Null move 後のimproving再計算（YaneuraOu準拠）
        if !in_check && static_eval != Value::NONE {
            improving |= static_eval >= beta;
        }

        // =================================================================
        // 指し手ループ
        // =================================================================
        let mut alpha = alpha;
        let mut best_value = Value::new(-32001);
        let mut best_move = Move::NONE;
        let mut move_count = 0;
        let mut quiets_tried: Vec<Move> = Vec::new();
        let mut captures_tried: Vec<Move> = Vec::new();

        // 合法手を生成（簡易実装）
        let mut ordered_moves = Vec::new();

        // qsearch/ProbCut互換: 捕獲フェーズではTT手もcapture_stageで制約
        if depth <= DEPTH_QS
            && tt_move.is_some()
            && (!pos.capture_stage(tt_move) && !pos.gives_check(tt_move) || depth < -16)
        {
            tt_move = Move::NONE;
        }

        {
            let cont_tables = self.build_cont_tables(ply);
            let mut mp = MovePicker::new(
                pos,
                tt_move,
                depth,
                &self.main_history,
                &self.low_ply_history,
                &self.capture_history,
                cont_tables,
                &self.pawn_history,
                ply,
                self.generate_all_legal_moves,
            );

            while let Some(mv) = {
                let m = mp.next_move();
                if m.is_none() {
                    None
                } else {
                    Some(m)
                }
            } {
                ordered_moves.push(mv);
            }

            // qsearchでは捕獲以外のチェックも生成（YaneuraOu準拠）
            if !in_check && depth == DEPTH_QS {
                let mut buf = [Move::NONE; crate::movegen::MAX_MOVES];
                let gen_type = if self.generate_all_legal_moves {
                    crate::movegen::GenType::QuietChecksAll
                } else {
                    crate::movegen::GenType::QuietChecks
                };
                let count = crate::movegen::generate_with_type(pos, gen_type, &mut buf, None);
                for mv in buf.iter().take(count) {
                    if ordered_moves.contains(mv) {
                        continue;
                    }
                    ordered_moves.push(*mv);
                }
            }

            // depth <= -5 なら recaptures のみに絞る
            if depth <= -5 && ply >= 1 {
                let mut buf = [Move::NONE; crate::movegen::MAX_MOVES];
                let rec_sq = self.stack[(ply - 1) as usize].current_move.to();
                let gen_type = if self.generate_all_legal_moves {
                    crate::movegen::GenType::RecapturesAll
                } else {
                    crate::movegen::GenType::Recaptures
                };
                let count =
                    crate::movegen::generate_with_type(pos, gen_type, &mut buf, Some(rec_sq));
                ordered_moves.clear();
                for mv in buf.iter().take(count) {
                    ordered_moves.push(*mv);
                }
            }
        }

        // TODO: singular extension（YaneuraOu準拠）は未実装。
        // 追加時は補正履歴の寄与（abs(correctionValue)/249096 を margin に加算）も含める。
        for mv in ordered_moves {
            if !pos.pseudo_legal(mv) {
                continue;
            }
            if !pos.is_legal(mv) {
                continue;
            }
            if self.check_abort() {
                return Value::ZERO;
            }

            move_count += 1;
            self.stack[ply as usize].move_count = move_count;

            let is_capture = pos.is_capture(mv);
            let gives_check = pos.gives_check(mv);

            // =============================================================
            // Late Move Pruning
            // =============================================================
            if !pv_node && !in_check && !is_capture && move_count >= 3 + depth * depth {
                continue;
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

            pos.do_move(mv, gives_check);
            self.nodes += 1;

            // YaneuraOu方式: ContHistKeyを設定
            // ⚠ in_checkは親ノードの王手状態を使用（gives_checkではない）
            self.stack[ply as usize].cont_hist_key =
                Some(ContHistKey::new(in_check, is_capture, cont_hist_piece, cont_hist_to));

            // 手の記録（YaneuraOu準拠: quietsSearched, capturesSearched）
            if is_capture {
                captures_tried.push(mv);
            } else {
                quiets_tried.push(mv);
            }

            // =============================================================
            // Late Move Reduction (LMR)
            // =============================================================
            let mut new_depth = depth - 1;

            if depth >= 2 && move_count > 1 + (pv_node as i32) {
                let r = reduction(improving, depth, move_count);

                // 王手には reduction しない
                let r = if gives_check { 0 } else { r };

                // capture にはあまり reduction しない
                let r = if is_capture { r / 2 } else { r };

                // YaneuraOu: 補正履歴が大きいときは過度なLMRを抑制する
                let corr_reduction = correction_value.abs() / 27_160;
                let r = (r - corr_reduction).max(0);

                new_depth = (depth - 1 - r).max(1);
            }

            // =============================================================
            // 探索
            // =============================================================
            let value = if new_depth < depth - 1 {
                // Reduced search
                let mut value = -self.search_node::<{ NodeType::NonPV as u8 }>(
                    pos,
                    new_depth,
                    -alpha - Value::new(1),
                    -alpha,
                    ply + 1,
                );

                // Re-search if reduced search beats alpha
                if value > alpha && new_depth < depth - 1 {
                    value = -self.search_node::<{ NodeType::NonPV as u8 }>(
                        pos,
                        depth - 1,
                        -alpha - Value::new(1),
                        -alpha,
                        ply + 1,
                    );
                }

                // Full window re-search for PV nodes
                if pv_node && value > alpha && value < beta {
                    value = -self.search_node::<{ NodeType::PV as u8 }>(
                        pos,
                        depth - 1,
                        -beta,
                        -alpha,
                        ply + 1,
                    );
                }

                value
            } else if !pv_node || move_count > 1 {
                // Zero window search
                let mut value = -self.search_node::<{ NodeType::NonPV as u8 }>(
                    pos,
                    depth - 1,
                    -alpha - Value::new(1),
                    -alpha,
                    ply + 1,
                );

                if pv_node && value > alpha && value < beta {
                    value = -self.search_node::<{ NodeType::PV as u8 }>(
                        pos,
                        depth - 1,
                        -beta,
                        -alpha,
                        ply + 1,
                    );
                }

                value
            } else {
                // Full window search
                -self.search_node::<{ NodeType::PV as u8 }>(pos, depth - 1, -beta, -alpha, ply + 1)
            };

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
                        break;
                    }
                }
            }
        }

        // =================================================================
        // 詰み/ステイルメイト判定
        // =================================================================
        if move_count == 0 {
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
                self.main_history.update(us, best_move, scaled_bonus);

                // LowPlyHistory: bonus * 771 / 1024 + 40
                if ply < LOW_PLY_HISTORY_SIZE as i32 {
                    let low_ply_bonus = low_ply_history_bonus(scaled_bonus);
                    self.low_ply_history.update(ply as usize, best_move, low_ply_bonus);
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
                            self.continuation_history[in_check_idx][capture_idx].update(
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
                self.pawn_history.update(pawn_key_idx, best_cont_pc, best_to, pawn_bonus);

                // 他のquiet手にはペナルティ
                // YaneuraOu: update_quiet_histories(move, -quietMalus * 1115 / 1024)
                let scaled_malus = malus * 1115 / 1024;
                for &m in &quiets_tried {
                    if m != best_move {
                        // MainHistory
                        self.main_history.update(us, m, -scaled_malus);

                        // LowPlyHistory（現行欠落していたので追加）
                        if ply < LOW_PLY_HISTORY_SIZE as i32 {
                            let low_ply_malus = low_ply_history_bonus(-scaled_malus);
                            self.low_ply_history.update(ply as usize, m, low_ply_malus);
                        }

                        // ContinuationHistoryへのペナルティ
                        let moved_pc = pos.moved_piece(m);
                        let cont_pc = if m.is_promotion() {
                            moved_pc.promote().unwrap_or(moved_pc)
                        } else {
                            moved_pc
                        };
                        let to = m.to();

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
                                    self.continuation_history[in_check_idx][capture_idx].update(
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
                        self.pawn_history.update(pawn_key_idx, cont_pc, to, pawn_malus);
                    }
                }
            } else {
                // 捕獲手がbest: captureHistoryを更新
                let captured_pt = pos.piece_on(best_to).piece_type();
                self.capture_history.update(best_cont_pc, best_to, captured_pt, bonus);
            }

            // YaneuraOu: 他の捕獲手へのペナルティ（capture best以外の全捕獲手）
            // captureMalus = min(708*depth-148, 2287) - 29*capturesSearched.size()
            let cap_malus = capture_malus(depth, captures_tried.len());
            for &m in &captures_tried {
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
                    self.capture_history.update(cont_pc, to, captured_pt, -cap_malus * 1431 / 1024);
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
                                    // YaneuraOu: (bonus * weight / 1024) + 80 * (i < 2)
                                    let weighted_penalty = penalty_base * weight / 1024
                                        + if ply_back <= 2 { 80 } else { 0 };
                                    self.continuation_history[in_check_idx][capture_idx].update(
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
                    self.tt_move_history.update(ply as usize, TT_MOVE_HISTORY_BONUS);
                } else {
                    self.tt_move_history.update(ply as usize, TT_MOVE_HISTORY_MALUS);
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
        let bound = if best_value >= beta {
            Bound::Lower
        } else if pv_node && best_move.is_some() {
            Bound::Exact
        } else {
            Bound::Upper
        };

        tt_result.write(
            key,
            value_to_tt(best_value, ply),
            pv_node,
            bound,
            depth,
            best_move,
            unadjusted_static_eval,
            self.tt.generation(),
        );

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
    ) -> Value {
        let pv_node = NT == NodeType::PV as u8;
        let in_check = pos.in_check();

        if ply >= MAX_PLY {
            return if in_check { Value::ZERO } else { evaluate(pos) };
        }

        if pv_node && self.sel_depth < ply + 1 {
            self.sel_depth = ply + 1;
        }

        if self.check_abort() {
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
            unadjusted_static_eval = evaluate(pos);
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
            let cont_tables = self.build_cont_tables(ply);
            let mut buf_moves = Vec::new();

            {
                let mp = if in_check {
                    MovePicker::new_evasions(
                        pos,
                        tt_move,
                        &self.main_history,
                        &self.low_ply_history,
                        &self.capture_history,
                        cont_tables,
                        &self.pawn_history,
                        ply,
                        self.generate_all_legal_moves,
                    )
                } else {
                    MovePicker::new(
                        pos,
                        tt_move,
                        DEPTH_QS,
                        &self.main_history,
                        &self.low_ply_history,
                        &self.capture_history,
                        cont_tables,
                        &self.pawn_history,
                        ply,
                        self.generate_all_legal_moves,
                    )
                };

                for mv in mp {
                    buf_moves.push(mv);
                }
            }

            if !in_check && depth == DEPTH_QS {
                let mut buf = [Move::NONE; crate::movegen::MAX_MOVES];
                let gen_type = if self.generate_all_legal_moves {
                    crate::movegen::GenType::QuietChecksAll
                } else {
                    crate::movegen::GenType::QuietChecks
                };
                let count = crate::movegen::generate_with_type(pos, gen_type, &mut buf, None);
                for mv in buf.iter().take(count) {
                    if buf_moves.contains(mv) {
                        continue;
                    }
                    buf_moves.push(*mv);
                }
            }

            if !in_check && depth <= -5 && ply >= 1 && !prev_move.is_none() {
                let mut buf = [Move::NONE; crate::movegen::MAX_MOVES];
                let rec_sq = prev_move.to();
                let gen_type = if self.generate_all_legal_moves {
                    crate::movegen::GenType::RecapturesAll
                } else {
                    crate::movegen::GenType::Recaptures
                };
                let count =
                    crate::movegen::generate_with_type(pos, gen_type, &mut buf, Some(rec_sq));
                buf_moves.clear();
                for mv in buf.iter().take(count) {
                    buf_moves.push(*mv);
                }
            }

            buf_moves
        };

        let mut move_count = 0;

        for mv in ordered_moves {
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

                    // ss-1の参照
                    if ply >= 1 {
                        if let Some(key) = self.stack[(ply - 1) as usize].cont_hist_key {
                            let in_check_idx = key.in_check as usize;
                            let capture_idx = key.capture as usize;
                            cont_score += self.continuation_history[in_check_idx][capture_idx].get(
                                key.piece,
                                key.to,
                                mv.moved_piece_after(),
                                mv.to(),
                            ) as i32;
                        }
                    }

                    let pawn_idx = pos.pawn_history_index();
                    cont_score +=
                        self.pawn_history.get(pawn_idx, pos.moved_piece(mv), mv.to()) as i32;
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

            pos.do_move(mv, gives_check);
            self.nodes += 1;

            self.stack[ply as usize].cont_hist_key =
                Some(ContHistKey::new(in_check, capture, cont_hist_pc, cont_hist_to));

            let value = -self.qsearch::<NT>(pos, depth - 1, -beta, -alpha, ply + 1);

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

// =============================================================================
// テスト
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_init_reductions() {
        init_reductions();
        // reduction(true, 10, 5) などが正の値を返すことを確認
        assert!(reduction(true, 10, 5) >= 0);
        assert!(reduction(false, 10, 5) >= reduction(true, 10, 5));
    }

    #[test]
    fn test_reduction_bounds() {
        init_reductions();
        // 境界値テスト
        assert_eq!(reduction(true, 0, 0), 0); // depth=0, mc=0 は計算外
        assert!(reduction(true, 63, 63) < 64);
        assert!(reduction(false, 63, 63) < 64);
    }

    /// LMRテーブルが初期化済みかどうかを確認できることをテスト
    #[test]
    fn test_is_reductions_initialized() {
        // 他のテストで既に初期化されている可能性があるので、
        // 初期化後に true を返すことを確認
        init_reductions();
        assert!(
            is_reductions_initialized(),
            "REDUCTIONS should be initialized after init_reductions()"
        );
    }

    /// depth/move_countが大きい場合にreductionが正の値を返すことを確認
    /// バグ修正前: REDUCTIONSが初期化されずに常に0を返していた
    #[test]
    fn test_reduction_returns_nonzero_for_large_values() {
        init_reductions();

        // 深い探索で多くの手を試した場合、reductionは正の値であるべき
        let r = reduction(false, 10, 10);
        assert!(
            r > 0,
            "reduction should return positive value for depth=10, move_count=10, got {r}"
        );

        // improving=trueの場合は若干小さい値になる
        let r_imp = reduction(true, 10, 10);
        assert!(r >= r_imp, "non-improving should have >= reduction than improving");
    }

    /// 境界ケース: depth=1, move_count=1でもreduction関数が動作することを確認
    #[test]
    fn test_reduction_small_values() {
        init_reductions();

        // 小さな値でもpanicしないことを確認
        let r = reduction(true, 1, 1);
        assert!(r >= 0, "reduction should not be negative");
    }

    // 注: SearchWorkerのスタック使用量が大きいため、Box化などの最適化が必要
    // sel_depthのリセットは iterate_root の各手のループ内で行われる（alpha_beta.rs:XXX行目）
    // バグ修正: self.sel_depth = 0; を各ルート手の処理開始時に追加
    //
    // SearchWorkerのユニットテストは統合テストで行う
}
