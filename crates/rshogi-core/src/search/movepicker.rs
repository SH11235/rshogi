//! MovePicker（指し手オーダリング）
//!
//! 探索中に指し手を効率的に順序付けして返すコンポーネント。
//! Alpha-Beta探索の効率を最大化するため、カットオフを起こしやすい手を先に返す。
//!
//! ## Lazy Generation
//!
//! MovePickerは指し手を段階的に生成する（lazy generation）。
//! LMP等の枝刈り条件が成立したら、`skip_quiets()`を呼び出すことで
//! 残りのquiet手の生成をスキップできる。
//!
//! ## History参照を保持しない設計
//!
//! 再帰呼び出し時の参照エイリアス問題を避けるため、MovePickerはHistory参照を
//! フィールドとして保持しない。代わりに、`next_move()`メソッドでHistoryTables
//! への参照を受け取る。
//!
//! ## Stage
//!
//! 指し手生成は複数の段階（Stage）に分けて行われる：
//!
//! ### 通常探索（王手なし）
//! 1. MainTT - 置換表の指し手
//! 2. CaptureInit - 捕獲手の生成
//! 3. GoodCapture - 良い捕獲手（SEE >= threshold）
//! 4. QuietInit - 静かな手の生成
//! 5. GoodQuiet - 良い静かな手
//! 6. BadCapture - 悪い捕獲手
//! 7. BadQuiet - 悪い静かな手
//!
//! ### 王手回避
//! 1. EvasionTT - 置換表の指し手
//! 2. EvasionInit - 回避手の生成
//! 3. Evasion - 回避手
//!
//! ### 静止探索
//! 1. QSearchTT - 置換表の指し手
//! 2. QCaptureInit - 捕獲手の生成
//! 3. QCapture - 捕獲手

use super::{HistoryTables, PieceToHistory, LOW_PLY_HISTORY_SIZE};
use crate::movegen::{ExtMove, ExtMoveBuffer};
use crate::position::Position;
use crate::types::{Color, Depth, Move, Piece, PieceType, Value, DEPTH_QS};

// =============================================================================
// Stage（指し手生成の段階）
// =============================================================================

/// 指し手生成の段階
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Stage {
    // 通常探索（王手なし）
    /// 置換表の指し手
    MainTT,
    /// ProbCut: 置換表の指し手
    ProbCutTT,
    /// 捕獲手の生成
    CaptureInit,
    /// ProbCut: 捕獲手生成
    ProbCutInit,
    /// ProbCut: SEEしきい値付き捕獲
    ProbCut,
    /// 良い捕獲手（SEE >= threshold）
    GoodCapture,
    /// 静かな手の生成
    QuietInit,
    /// 良い静かな手
    GoodQuiet,
    /// 悪い捕獲手
    BadCapture,
    /// 悪い静かな手
    BadQuiet,

    // 王手回避
    /// 置換表の指し手（回避）
    EvasionTT,
    /// 回避手の生成
    EvasionInit,
    /// 回避手
    Evasion,

    // 静止探索
    /// 置換表の指し手（静止探索）
    QSearchTT,
    /// 静止探索用捕獲手の生成
    QCaptureInit,
    /// 静止探索用捕獲手
    QCapture,
}

impl Stage {
    /// 次のステージを取得
    pub fn next(self) -> Self {
        match self {
            Stage::MainTT => Stage::CaptureInit,
            Stage::CaptureInit => Stage::GoodCapture,
            Stage::ProbCutTT => Stage::ProbCutInit,
            Stage::ProbCutInit => Stage::ProbCut,
            Stage::ProbCut => Stage::ProbCut, // 終端
            Stage::GoodCapture => Stage::QuietInit,
            Stage::QuietInit => Stage::GoodQuiet,
            Stage::GoodQuiet => Stage::BadCapture,
            Stage::BadCapture => Stage::BadQuiet,
            Stage::BadQuiet => Stage::BadQuiet, // 終端

            Stage::EvasionTT => Stage::EvasionInit,
            Stage::EvasionInit => Stage::Evasion,
            Stage::Evasion => Stage::Evasion, // 終端

            Stage::QSearchTT => Stage::QCaptureInit,
            Stage::QCaptureInit => Stage::QCapture,
            Stage::QCapture => Stage::QCapture, // 終端
        }
    }
}

// =============================================================================
// MovePicker
// =============================================================================

/// 指し手オーダリング器（History参照を保持しない設計）
///
/// 探索中に指し手を効率的に順序付けして返す。
/// History統計を参照してスコアリングを行う。
///
/// ## 借用問題の解決
///
/// `MovePicker`は`Position`や`HistoryTables`への参照をフィールドとして保持しない。
/// 代わりに、`next_move()`メソッドでこれらの参照を受け取る。
/// これにより、探索ループ内で`pos.do_move()`を呼び出す際や、
/// 再帰呼び出し時のHistory参照エイリアス問題を回避できる。
///
/// ## 使用パターン
///
/// ```ignore
/// let mut mp = MovePicker::new(pos, tt_move, depth, ply, cont_hist, generate_all);
/// loop {
///     let mv = ctx.history.with_read(|h| mp.next_move(pos, h));
///     if mv == Move::NONE { break; }
///     // LMPチェック
///     if lmp_condition && mp.is_quiet_stage() && !is_capture {
///         mp.skip_quiets();
///         continue;
///     }
///     // 再帰（この時点で &HistoryTables は存在しない）
///     let value = search_node(...);
/// }
/// ```
pub struct MovePicker {
    // History参照は保持しない（next_move時に渡す）

    // ContinuationHistory参照（スコアリング用、ply毎に異なるため保持）
    continuation_history: [*const PieceToHistory; 6],

    // 状態
    stage: Stage,
    tt_move: Move,
    probcut_threshold: Option<Value>,
    /// 探索の深さ（部分ソートの閾値計算に使用）
    depth: Depth,
    ply: i32,
    skip_quiets: bool,
    generate_all_legal_moves: bool,

    // 初期化時にキャッシュする情報
    side_to_move: Color,
    pawn_history_index: usize,

    // 指し手バッファ（MaybeUninitにより初期化コストゼロ）
    moves: ExtMoveBuffer,
    cur: usize,
    end_cur: usize,
    end_bad_captures: usize,
    end_captures: usize,
    end_generated: usize,
    /// 良い静かな手の終了位置（partial_insertion_sortの閾値以上の手の数）
    /// QuietInitで設定し、GoodQuiet/BadQuietで使用
    end_good_quiets: usize,
}

impl MovePicker {
    /// 通常探索・静止探索用コンストラクタ（History参照を保持しない）
    ///
    /// `pos`は初期化時のみ使用し、フィールドとして保持しない。
    /// `continuation_history`はスコアリング時に使用するため、ポインタとして保持する。
    ///
    /// # Safety
    ///
    /// `continuation_history`のポインタは、MovePickerのライフタイム中有効でなければならない。
    /// これは探索ノード内でのみMovePickerを使用することで保証される。
    pub fn new(
        pos: &Position,
        tt_move: Move,
        depth: Depth,
        ply: i32,
        continuation_history: [&PieceToHistory; 6],
        generate_all_legal_moves: bool,
    ) -> Self {
        let stage = if pos.in_check() {
            // 王手回避
            if tt_move.is_some() && pos.pseudo_legal_with_all(tt_move, generate_all_legal_moves) {
                Stage::EvasionTT
            } else {
                Stage::EvasionInit
            }
        } else if depth > DEPTH_QS {
            // 通常探索
            if tt_move.is_some() && pos.pseudo_legal_with_all(tt_move, generate_all_legal_moves) {
                Stage::MainTT
            } else {
                Stage::CaptureInit
            }
        } else {
            // 静止探索
            if tt_move.is_some() && pos.pseudo_legal_with_all(tt_move, generate_all_legal_moves) {
                Stage::QSearchTT
            } else {
                Stage::QCaptureInit
            }
        };

        Self {
            continuation_history: [
                continuation_history[0] as *const _,
                continuation_history[1] as *const _,
                continuation_history[2] as *const _,
                continuation_history[3] as *const _,
                continuation_history[4] as *const _,
                continuation_history[5] as *const _,
            ],
            stage,
            tt_move,
            probcut_threshold: None,
            depth,
            ply,
            skip_quiets: false,
            generate_all_legal_moves,
            side_to_move: pos.side_to_move(),
            pawn_history_index: pos.pawn_history_index(),
            moves: ExtMoveBuffer::new(),
            cur: 0,
            end_cur: 0,
            end_bad_captures: 0,
            end_captures: 0,
            end_generated: 0,
            end_good_quiets: 0,
        }
    }

    /// 王手回避専用コンストラクタ（シンプル版）
    pub fn new_evasions(
        pos: &Position,
        tt_move: Move,
        ply: i32,
        continuation_history: [&PieceToHistory; 6],
        generate_all_legal_moves: bool,
    ) -> Self {
        debug_assert!(pos.in_check());

        let stage =
            if tt_move.is_some() && pos.pseudo_legal_with_all(tt_move, generate_all_legal_moves) {
                Stage::EvasionTT
            } else {
                Stage::EvasionInit
            };

        Self {
            continuation_history: [
                continuation_history[0] as *const _,
                continuation_history[1] as *const _,
                continuation_history[2] as *const _,
                continuation_history[3] as *const _,
                continuation_history[4] as *const _,
                continuation_history[5] as *const _,
            ],
            stage,
            tt_move,
            probcut_threshold: None,
            depth: DEPTH_QS,
            ply,
            skip_quiets: false,
            generate_all_legal_moves,
            side_to_move: pos.side_to_move(),
            pawn_history_index: pos.pawn_history_index(),
            moves: ExtMoveBuffer::new(),
            cur: 0,
            end_cur: 0,
            end_bad_captures: 0,
            end_captures: 0,
            end_generated: 0,
            end_good_quiets: 0,
        }
    }

    /// ProbCut専用コンストラクタ
    pub fn new_probcut(
        pos: &Position,
        tt_move: Move,
        threshold: Value,
        ply: i32,
        continuation_history: [&PieceToHistory; 6],
        generate_all_legal_moves: bool,
    ) -> Self {
        debug_assert!(!pos.in_check());

        let stage = if tt_move.is_some()
            && pos.is_capture(tt_move)
            && pos.pseudo_legal_with_all(tt_move, generate_all_legal_moves)
            && pos.see_ge(tt_move, threshold)
        {
            Stage::ProbCutTT
        } else {
            Stage::ProbCutInit
        };

        Self {
            continuation_history: [
                continuation_history[0] as *const _,
                continuation_history[1] as *const _,
                continuation_history[2] as *const _,
                continuation_history[3] as *const _,
                continuation_history[4] as *const _,
                continuation_history[5] as *const _,
            ],
            stage,
            tt_move,
            probcut_threshold: Some(threshold),
            depth: DEPTH_QS,
            ply,
            skip_quiets: false,
            generate_all_legal_moves,
            side_to_move: pos.side_to_move(),
            pawn_history_index: pos.pawn_history_index(),
            moves: ExtMoveBuffer::new(),
            cur: 0,
            end_cur: 0,
            end_bad_captures: 0,
            end_captures: 0,
            end_generated: 0,
            end_good_quiets: 0,
        }
    }

    /// quiet手の生成をスキップ（LMP条件成立時に呼び出す）
    ///
    /// skip_quietsフラグを設定し、現在のステージに応じて遷移先を調整する。
    /// QuietInit/GoodQuietステージにいる場合はBadCaptureに即座に遷移する。
    /// **bad capturesは残す**（quietのみスキップ）
    pub fn skip_quiets(&mut self) {
        self.skip_quiets = true;
        // 現在のステージに応じて遷移先を調整
        match self.stage {
            Stage::QuietInit | Stage::GoodQuiet => {
                // BadCapture範囲にcur/end_curを再初期化してから遷移
                // これがないと、GoodQuiet中で発火した場合に残りのquietが返されてしまう
                self.cur = 0;
                self.end_cur = self.end_bad_captures;
                self.stage = Stage::BadCapture;
            }
            _ => {}
        }
    }

    /// 現在のステージがquiet段階かどうかを返す
    ///
    /// LMP発火条件の判定に使用する。quiet段階（QuietInit, GoodQuiet, BadQuiet）で
    /// のみLMPを発火させ、captureは残す。
    #[inline]
    pub fn is_quiet_stage(&self) -> bool {
        matches!(self.stage, Stage::QuietInit | Stage::GoodQuiet | Stage::BadQuiet)
    }

    /// 現在のステージを取得（デバッグ用）
    #[inline]
    pub fn stage(&self) -> Stage {
        self.stage
    }

    /// 次の指し手を返す
    ///
    /// 指し手が尽きたら `Move::NONE` を返す。
    ///
    /// ## 引数
    /// - `pos`: 現在の局面への参照（各呼び出しで一時的に借用）
    /// - `history`: HistoryTablesへの参照（スコアリング時に使用）
    ///
    /// ## 使用パターン
    ///
    /// ```ignore
    /// let mv = ctx.history.with_read(|h| mp.next_move(pos, h));
    /// ```
    pub fn next_move(&mut self, pos: &Position, history: &HistoryTables) -> Move {
        loop {
            match self.stage {
                // ==============================
                // TT手を返す
                // ==============================
                Stage::MainTT | Stage::EvasionTT | Stage::QSearchTT | Stage::ProbCutTT => {
                    self.stage = self.stage.next();
                    return self.tt_move;
                }

                // ==============================
                // 捕獲手の生成
                // ==============================
                Stage::CaptureInit | Stage::QCaptureInit | Stage::ProbCutInit => {
                    self.cur = 0;
                    self.end_bad_captures = 0;

                    // 捕獲手を生成
                    let count = if self.generate_all_legal_moves {
                        // All指定時は生成タイプを切り替え
                        let gen_type = if matches!(self.stage, Stage::ProbCutInit) {
                            crate::movegen::GenType::CapturesAll
                        } else {
                            crate::movegen::GenType::CapturesProPlusAll
                        };
                        let mut buf = ExtMoveBuffer::new();
                        crate::movegen::generate_with_type(pos, gen_type, &mut buf, None);
                        let mut c = 0;
                        for ext in buf.iter() {
                            if pos.is_capture(ext.mv) {
                                self.moves.set(c, ExtMove::new(ext.mv, 0));
                                c += 1;
                            }
                        }
                        self.moves.set_len(c);
                        c
                    } else if matches!(self.stage, Stage::ProbCutInit) {
                        // ProbCut: 捕獲手生成
                        let mut buf = ExtMoveBuffer::new();
                        crate::movegen::generate_with_type(
                            pos,
                            crate::movegen::GenType::CapturesProPlus,
                            &mut buf,
                            None,
                        );
                        // 捕獲手のみフィルタ
                        let mut tmp_count = 0usize;
                        for ext in buf.iter() {
                            if pos.is_capture(ext.mv) {
                                self.moves.set(tmp_count, ExtMove::new(ext.mv, 0));
                                tmp_count += 1;
                            }
                        }
                        self.moves.set_len(tmp_count);
                        tmp_count
                    } else {
                        pos.generate_captures(&mut self.moves)
                    };
                    self.end_cur = count;
                    self.end_captures = count;

                    self.score_captures(pos, history);
                    partial_insertion_sort(self.moves.as_mut_slice(), self.end_cur, i32::MIN);

                    self.stage = self.stage.next();
                }

                // ==============================
                // 良い捕獲手を返す
                // ==============================
                Stage::GoodCapture => {
                    if let Some(m) = self.select_good_capture(pos) {
                        return m;
                    }
                    // skip_quietsフラグがある場合はquiet手をスキップしてBadCaptureへ
                    if self.skip_quiets {
                        self.stage = Stage::BadCapture;
                    } else {
                        self.stage = Stage::QuietInit;
                    }
                }

                // ==============================
                // 静かな手の生成
                // ==============================
                Stage::QuietInit => {
                    if !self.skip_quiets {
                        // 静かな手を生成
                        let count = if self.generate_all_legal_moves {
                            let mut buf = ExtMoveBuffer::new();
                            crate::movegen::generate_with_type(
                                pos,
                                crate::movegen::GenType::QuietsAll,
                                &mut buf,
                                None,
                            );
                            let mut c = 0;
                            for ext in buf.iter() {
                                if !pos.is_capture(ext.mv) {
                                    self.moves.set(self.end_captures + c, ExtMove::new(ext.mv, 0));
                                    c += 1;
                                }
                            }
                            c
                        } else {
                            pos.generate_quiets(&mut self.moves, self.end_captures)
                        };
                        self.end_cur = self.end_captures + count;
                        self.end_generated = self.end_cur;
                        self.moves.set_len(self.end_cur);

                        self.cur = self.end_captures;
                        self.score_quiets(pos, history);

                        // ハイブリッド方式: 深さベースの閾値で部分ソート
                        // -3560はYaneuraOuの経験的な定数で、depth=1で-3560、depth=10で-35600となる
                        // 深さが浅いほど多くの手をソート（閾値が高い）、深いほど少ない手のみソート（閾値が低い）
                        // partial_insertion_sortは閾値以上の手を先頭に集めてソートし、その数を返す
                        let limit = -3560 * self.depth;
                        let quiet_count = self.end_cur - self.end_captures;
                        let good_count = partial_insertion_sort(
                            &mut self.moves.as_mut_slice()[self.end_captures..],
                            quiet_count,
                            limit,
                        );
                        // 閾値以上の手の終了位置を保存（GoodQuiet/BadQuietで使用）
                        self.end_good_quiets = self.end_captures + good_count;
                    } else {
                        self.end_good_quiets = self.end_captures;
                    }
                    self.stage = Stage::GoodQuiet;
                }

                // ==============================
                // 良い静かな手を返す（YaneuraOu準拠: value > GOOD_QUIET_THRESHOLD）
                // ==============================
                Stage::GoodQuiet => {
                    if !self.skip_quiets {
                        self.end_cur = self.end_good_quiets;
                        if let Some(m) = self.select_good_quiet() {
                            return m;
                        }
                    }

                    // 悪い捕獲手の準備
                    self.cur = 0;
                    self.end_cur = self.end_bad_captures;
                    self.stage = Stage::BadCapture;
                }

                // ==============================
                // 悪い捕獲手を返す
                // ==============================
                Stage::BadCapture => {
                    if let Some(m) = self.select_simple() {
                        return m;
                    }

                    // YaneuraOu準拠: endCapturesからquiet手全体を再走査
                    self.cur = self.end_captures;
                    self.end_cur = self.end_generated;
                    self.stage = Stage::BadQuiet;
                }

                // ==============================
                // 悪い静かな手を返す（YaneuraOu準拠: value <= GOOD_QUIET_THRESHOLD）
                // ==============================
                Stage::BadQuiet => {
                    if !self.skip_quiets {
                        // YaneuraOu準拠: endCaptures から全quiet手を再走査し、
                        // value <= GOOD_QUIET_THRESHOLD の手のみ返す
                        if let Some(m) = self.select_bad_quiet() {
                            return m;
                        }
                    }
                    return Move::NONE;
                }

                // ==============================
                // 回避手の生成
                // ==============================
                Stage::EvasionInit => {
                    // 回避手を生成
                    let count = if self.generate_all_legal_moves {
                        let mut buf = ExtMoveBuffer::new();
                        crate::movegen::generate_with_type(
                            pos,
                            crate::movegen::GenType::EvasionsAll,
                            &mut buf,
                            None,
                        );
                        let gen_count = buf.len();
                        for (i, ext) in buf.iter().enumerate() {
                            self.moves.set(i, ExtMove::new(ext.mv, 0));
                        }
                        self.moves.set_len(gen_count);
                        gen_count
                    } else {
                        pos.generate_evasions_ext(&mut self.moves)
                    };
                    self.cur = 0;
                    self.end_cur = count;
                    self.end_generated = count;

                    self.score_evasions(pos, history);
                    partial_insertion_sort(self.moves.as_mut_slice(), self.end_cur, i32::MIN);

                    self.stage = Stage::Evasion;
                }

                // ==============================
                // 回避手を返す
                // ==============================
                Stage::Evasion => {
                    return self.select_simple().unwrap_or(Move::NONE);
                }

                // ==============================
                // 静止探索用捕獲手を返す
                // ==============================
                Stage::QCapture => {
                    return self.select_simple().unwrap_or(Move::NONE);
                }

                // ==============================
                // ProbCut: SEEが閾値以上の捕獲のみ
                // ==============================
                Stage::ProbCut => {
                    if let Some(th) = self.probcut_threshold {
                        return self.select_probcut(pos, th).unwrap_or(Move::NONE);
                    }
                    return Move::NONE;
                }
            }
        }
    }

    // =========================================================================
    // スコアリング
    // =========================================================================

    /// ContinuationHistory参照を取得（unsafeだがMovePickerライフタイム中は有効）
    #[inline]
    fn cont_history(&self, idx: usize) -> &PieceToHistory {
        // SAFETY: MovePickerのライフタイム中、continuation_historyポインタは有効
        unsafe { &*self.continuation_history[idx] }
    }

    /// 捕獲手のスコアを計算
    fn score_captures(&mut self, pos: &Position, history: &HistoryTables) {
        for i in self.cur..self.end_cur {
            let ext = self.moves.get(i);
            let m = ext.mv;
            let to = m.to();
            let pc = m.moved_piece_after();
            let pt = pc.piece_type();
            let captured = pos.piece_on(to);
            let captured_pt = captured.piece_type();

            // MVV + CaptureHistory + 王手ボーナス
            let mut value = history.capture_history.get(pc, to, captured_pt) as i32;
            value += 7 * piece_value(captured);

            // 王手になるマスへの移動にボーナス
            if pos.check_squares(pt).contains(to) {
                value += 1024;
            }

            self.moves.set_value(i, value);
        }
    }

    /// 静かな手のスコアを計算
    fn score_quiets(&mut self, pos: &Position, history: &HistoryTables) {
        let us = self.side_to_move;
        let pawn_idx = self.pawn_history_index;

        for i in self.cur..self.end_cur {
            let ext = self.moves.get(i);
            let m = ext.mv;
            let to = m.to();
            let pc = m.moved_piece_after();
            let pt = pc.piece_type();
            let mut value = 0i32;

            // ButterflyHistory (×2)
            value += 2 * history.main_history.get(us, m) as i32;

            // PawnHistory (×2)
            value += 2 * history.pawn_history.get(pawn_idx, pc, to) as i32;

            // ContinuationHistory (6個のうち5個を使用)
            // インデックス4 (ply-5) はスキップ: YaneuraOu準拠で、ply-5は統計的に有効性が低いため除外
            // 参照: yaneuraou-search.cpp の continuationHistory 配列の使用箇所
            for (idx, weight) in [(0, 1), (1, 1), (2, 1), (3, 1), (5, 1)] {
                let ch = self.cont_history(idx);
                value += weight * ch.get(pc, to) as i32;
            }

            // 王手ボーナス
            if pos.check_squares(pt).contains(to) && pos.see_ge(m, Value::new(-75)) {
                value += 16384;
            }

            // LowPlyHistory
            if self.ply < LOW_PLY_HISTORY_SIZE as i32 {
                let ply_idx = self.ply as usize;
                value += 8 * history.low_ply_history.get(ply_idx, m) as i32 / (1 + self.ply);
            }

            self.moves.set_value(i, value);
        }
    }

    /// 回避手のスコアを計算
    fn score_evasions(&mut self, pos: &Position, history: &HistoryTables) {
        let us = self.side_to_move;

        for i in self.cur..self.end_cur {
            let ext = self.moves.get(i);
            let m = ext.mv;
            let to = m.to();
            let pc = m.moved_piece_after();

            if pos.is_capture(m) {
                // 捕獲手は駒価値 + 大きなボーナス
                let captured = pos.piece_on(to);
                self.moves.set_value(i, piece_value(captured) + (1 << 28));
            } else {
                // 静かな手はHistory
                let mut value = history.main_history.get(us, m) as i32;

                let ch = self.cont_history(0);
                value += ch.get(pc, to) as i32;

                if self.ply < LOW_PLY_HISTORY_SIZE as i32 {
                    let ply_idx = self.ply as usize;
                    value += 2 * history.low_ply_history.get(ply_idx, m) as i32 / (1 + self.ply);
                }

                self.moves.set_value(i, value);
            }
        }
    }

    // =========================================================================
    // ヘルパー
    // =========================================================================

    /// 良い捕獲手を選択（SEE >= threshold）
    fn select_good_capture(&mut self, pos: &Position) -> Option<Move> {
        while self.cur < self.end_cur {
            let ext = self.moves.get(self.cur);
            self.cur += 1;

            // TT手は既に返したのでスキップ
            if ext.mv == self.tt_move {
                continue;
            }

            // SEEで閾値以上の手のみ
            let threshold = Value::new(-ext.value / 18);
            if pos.see_ge(ext.mv, threshold) {
                return Some(ext.mv);
            } else {
                // 悪い捕獲手は後回し
                self.moves.swap(self.end_bad_captures, self.cur - 1);
                self.end_bad_captures += 1;
            }
        }
        None
    }

    /// GoodQuiet用: value > GOOD_QUIET_THRESHOLD の手のみ返す（YaneuraOu準拠）
    fn select_good_quiet(&mut self) -> Option<Move> {
        const GOOD_QUIET_THRESHOLD: i32 = -14000;
        while self.cur < self.end_cur {
            let ext = self.moves.get(self.cur);
            self.cur += 1;

            if ext.mv == self.tt_move {
                continue;
            }

            if ext.value > GOOD_QUIET_THRESHOLD {
                return Some(ext.mv);
            }
        }
        None
    }

    /// BadQuiet用: value <= GOOD_QUIET_THRESHOLD の手のみ返す（YaneuraOu準拠）
    fn select_bad_quiet(&mut self) -> Option<Move> {
        const GOOD_QUIET_THRESHOLD: i32 = -14000;
        while self.cur < self.end_cur {
            let ext = self.moves.get(self.cur);
            self.cur += 1;

            if ext.mv == self.tt_move {
                continue;
            }

            if ext.value <= GOOD_QUIET_THRESHOLD {
                return Some(ext.mv);
            }
        }
        None
    }

    /// シンプルな手の選択（TT手スキップのみ）
    fn select_simple(&mut self) -> Option<Move> {
        while self.cur < self.end_cur {
            let ext = self.moves.get(self.cur);
            self.cur += 1;

            // TT手は既に返したのでスキップ
            if ext.mv == self.tt_move {
                continue;
            }

            return Some(ext.mv);
        }
        None
    }

    /// ProbCut用の手の選択（SEE閾値チェック）
    fn select_probcut(&mut self, pos: &Position, threshold: Value) -> Option<Move> {
        while self.cur < self.end_cur {
            let ext = self.moves.get(self.cur);
            self.cur += 1;

            // TT手は既に返したのでスキップ
            if ext.mv == self.tt_move {
                continue;
            }

            if pos.see_ge(ext.mv, threshold) {
                return Some(ext.mv);
            }
        }
        None
    }
}

// MovePickerIter は History 参照を引数で渡す新しい設計では使用できないため削除
// 代わりに、探索ループ内で直接 mp.next_move(pos, history) を呼び出す

// =============================================================================
// ユーティリティ関数
// =============================================================================

/// 挿入ソートからPDQSortに切り替えるしきい値
///
/// 小さい配列では挿入ソートが高速だが、大きい配列ではPDQSort（O(n log n)）が
/// 挿入ソート（O(n²)）より高速。将棋の平均合法手数は約100手のため、
/// 大半のケースでPDQSortが適用される。
///
/// 16という値は、一般的なソートアルゴリズムの実装（std::sort等）で
/// 挿入ソートへのフォールバック閾値として広く使われている値。
/// Rust標準ライブラリのPDQSortも内部で同様の閾値を使用している。
const SORT_SWITCH_THRESHOLD: usize = 16;

/// 部分ソート（ハイブリッド方式）
///
/// `limit` 以上のスコアの手だけを降順でソートする。
/// 閾値以上の手を先頭に集めてから、その部分だけをソートする。
///
/// ## 戻り値
/// 閾値以上の手の数（good_count）を返す。
/// - `limit == i32::MIN`の場合は全要素ソートされ、`end`を返す
/// - それ以外の場合は閾値以上の手の数を返す
///
/// ## 境界条件
/// - `value >= limit`: 閾値以上の手として先頭に配置される
/// - `value < limit`: 閾値未満の手として後方に配置される
///
/// ## 計算量
/// - 閾値以上の手が多い場合: PDQSort O(k log k)
/// - 閾値以上の手が少ない場合: 挿入ソート O(k²)
/// - 全体の計算量: O(n) + O(k log k) または O(n) + O(k²)
fn partial_insertion_sort(moves: &mut [ExtMove], end: usize, limit: i32) -> usize {
    if end == 0 {
        return 0;
    }
    // 1手の場合もlimit判定を行う
    if end == 1 {
        return if moves[0].value >= limit { 1 } else { 0 };
    }

    let slice = &mut moves[..end];

    // limit = i32::MIN の場合は全要素ソート
    if limit == i32::MIN {
        if end > SORT_SWITCH_THRESHOLD {
            slice.sort_unstable_by(|a, b| b.value.cmp(&a.value));
        } else {
            // 小さい配列は挿入ソート
            for i in 1..end {
                let tmp = slice[i];
                let mut j = i;
                while j > 0 && slice[j - 1].value < tmp.value {
                    slice[j] = slice[j - 1];
                    j -= 1;
                }
                slice[j] = tmp;
            }
        }
        return end;
    }

    // ハイブリッド方式: 閾値以上の手を先頭に集めてからソート

    // 1. 閾値以上の手を先頭に集める（O(n)）
    let mut good_count = 0;
    for i in 0..end {
        if slice[i].value >= limit {
            slice.swap(i, good_count);
            good_count += 1;
        }
    }

    // 閾値以上の手がない場合は終了
    if good_count == 0 {
        return 0;
    }

    // 2. 閾値以上の手の部分だけをソート
    let good_slice = &mut slice[..good_count];
    if good_count > SORT_SWITCH_THRESHOLD {
        // 多い場合はPDQSort O(k log k)
        good_slice.sort_unstable_by(|a, b| b.value.cmp(&a.value));
    } else {
        // 少ない場合は挿入ソート O(k²)
        for i in 1..good_count {
            let tmp = good_slice[i];
            let mut j = i;
            while j > 0 && good_slice[j - 1].value < tmp.value {
                good_slice[j] = good_slice[j - 1];
                j -= 1;
            }
            good_slice[j] = tmp;
        }
    }

    good_count
}

/// 駒の価値（MVV用）
pub(crate) fn piece_value(pc: Piece) -> i32 {
    if pc.is_none() {
        return 0;
    }
    use PieceType::*;
    match pc.piece_type() {
        Pawn => 90,
        Lance => 315,
        Knight => 405,
        Silver => 495,
        Gold | ProPawn | ProLance | ProKnight | ProSilver => 540,
        Bishop => 855,
        Rook => 990,
        Horse => 1089,  // Bishop + King
        Dragon => 1224, // Rook + King
        King => 15000,
    }
}

// =============================================================================
// テスト
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stage_next() {
        assert_eq!(Stage::MainTT.next(), Stage::CaptureInit);
        assert_eq!(Stage::CaptureInit.next(), Stage::GoodCapture);
        assert_eq!(Stage::GoodCapture.next(), Stage::QuietInit);
        assert_eq!(Stage::QuietInit.next(), Stage::GoodQuiet);
        assert_eq!(Stage::GoodQuiet.next(), Stage::BadCapture);
        assert_eq!(Stage::BadCapture.next(), Stage::BadQuiet);
        assert_eq!(Stage::BadQuiet.next(), Stage::BadQuiet);

        assert_eq!(Stage::EvasionTT.next(), Stage::EvasionInit);
        assert_eq!(Stage::EvasionInit.next(), Stage::Evasion);
        assert_eq!(Stage::Evasion.next(), Stage::Evasion);

        assert_eq!(Stage::QSearchTT.next(), Stage::QCaptureInit);
        assert_eq!(Stage::QCaptureInit.next(), Stage::QCapture);
        assert_eq!(Stage::QCapture.next(), Stage::QCapture);
    }

    #[test]
    fn test_partial_insertion_sort() {
        let mut moves = vec![
            ExtMove::new(Move::NONE, 100),
            ExtMove::new(Move::NONE, 50),
            ExtMove::new(Move::NONE, 200),
            ExtMove::new(Move::NONE, 10),
            ExtMove::new(Move::NONE, 150),
        ];

        // limit=100でソート
        let len = moves.len();
        let good_count = partial_insertion_sort(&mut moves, len, 100);

        // 100以上の手が先頭にソートされている
        assert_eq!(good_count, 3); // 100, 150, 200 の3つ
        assert_eq!(moves[0].value, 200);
        assert_eq!(moves[1].value, 150);
        assert_eq!(moves[2].value, 100);
    }

    #[test]
    fn test_partial_insertion_sort_boundary_value() {
        // 境界値テスト: value == limit の手は閾値以上として扱われる
        let mut moves = vec![
            ExtMove::new(Move::NONE, 99),
            ExtMove::new(Move::NONE, 100), // ちょうど閾値
            ExtMove::new(Move::NONE, 101),
        ];

        let len = moves.len();
        let good_count = partial_insertion_sort(&mut moves, len, 100);

        // limit=100 の場合、value >= 100 の手が閾値以上
        assert_eq!(good_count, 2); // 100と101の2つ
                                   // 閾値以上の手がソートされて先頭に
        assert_eq!(moves[0].value, 101);
        assert_eq!(moves[1].value, 100);
        // 閾値未満の手は後方に
        assert_eq!(moves[2].value, 99);
    }

    #[test]
    fn test_partial_insertion_sort_large_array() {
        // SORT_SWITCH_THRESHOLD(16)を超える配列
        let mut moves: Vec<ExtMove> = (0..20).map(|i| ExtMove::new(Move::NONE, i * 10)).collect();

        let len = moves.len();
        // limit=100: 100以上の手は 100, 110, 120, ..., 190 の10個
        let good_count = partial_insertion_sort(&mut moves, len, 100);

        assert_eq!(good_count, 10);
        // 閾値以上の手がソートされて先頭に（降順）
        assert_eq!(moves[0].value, 190);
        assert_eq!(moves[1].value, 180);
        assert_eq!(moves[9].value, 100);
    }

    #[test]
    fn test_partial_insertion_sort_no_good_moves() {
        // 閾値を満たす手が0個の場合
        let mut moves = vec![
            ExtMove::new(Move::NONE, 10),
            ExtMove::new(Move::NONE, 20),
            ExtMove::new(Move::NONE, 30),
        ];

        let len = moves.len();
        let good_count = partial_insertion_sort(&mut moves, len, 100);

        assert_eq!(good_count, 0);
        // 順序は変わらない（swapは発生しない）
    }

    #[test]
    fn test_partial_insertion_sort_all_good_moves() {
        // 全ての手が閾値以上の場合
        let mut moves = vec![
            ExtMove::new(Move::NONE, 100),
            ExtMove::new(Move::NONE, 200),
            ExtMove::new(Move::NONE, 150),
        ];

        let len = moves.len();
        let good_count = partial_insertion_sort(&mut moves, len, 50);

        assert_eq!(good_count, 3);
        // 全てソートされる（降順）
        assert_eq!(moves[0].value, 200);
        assert_eq!(moves[1].value, 150);
        assert_eq!(moves[2].value, 100);
    }

    #[test]
    fn test_partial_insertion_sort_full_sort() {
        // limit = i32::MIN の場合は全要素ソート
        let mut moves = vec![
            ExtMove::new(Move::NONE, 50),
            ExtMove::new(Move::NONE, -100),
            ExtMove::new(Move::NONE, 200),
            ExtMove::new(Move::NONE, 0),
        ];

        let len = moves.len();
        let good_count = partial_insertion_sort(&mut moves, len, i32::MIN);

        assert_eq!(good_count, 4); // 全要素が「閾値以上」
                                   // 全体がソートされる（降順）
        assert_eq!(moves[0].value, 200);
        assert_eq!(moves[1].value, 50);
        assert_eq!(moves[2].value, 0);
        assert_eq!(moves[3].value, -100);
    }

    #[test]
    fn test_partial_insertion_sort_empty() {
        // 空配列
        let mut moves: Vec<ExtMove> = vec![];
        let good_count = partial_insertion_sort(&mut moves, 0, 100);
        assert_eq!(good_count, 0);
    }

    #[test]
    fn test_partial_insertion_sort_single_element() {
        // 1要素の配列
        let mut moves = vec![ExtMove::new(Move::NONE, 150)];
        let good_count = partial_insertion_sort(&mut moves, 1, 100);
        assert_eq!(good_count, 1);
        assert_eq!(moves[0].value, 150);

        // 閾値未満の1要素: limit判定を正しく行う
        let mut moves2 = vec![ExtMove::new(Move::NONE, 50)];
        let good_count2 = partial_insertion_sort(&mut moves2, 1, 100);
        assert_eq!(good_count2, 0); // 50 < 100 なので閾値未満、good_count=0
    }

    #[test]
    fn test_piece_value() {
        assert_eq!(piece_value(Piece::B_PAWN), 90);
        assert_eq!(piece_value(Piece::W_GOLD), 540);
        assert_eq!(piece_value(Piece::B_ROOK), 990);
        assert_eq!(piece_value(Piece::W_DRAGON), 1224);
    }

    /// end_good_quietsの境界が正しく設定されることを検証
    ///
    /// バグ修正の検証: partial_insertion_sortで閾値以上の手を前に集めた後、
    /// good_countが正しく返され、GoodQuiet/BadQuietの境界が正確に設定される
    #[test]
    fn test_end_good_quiets_boundary() {
        // partial_insertion_sortで閾値以上の手を前に集めた後、
        // end_good_quietsが正しく設定されることを検証
        let mut moves = vec![
            ExtMove::new(Move::NONE, 100),  // 閾値以上 (100 >= 0)
            ExtMove::new(Move::NONE, -200), // 閾値未満 (-200 < 0)
            ExtMove::new(Move::NONE, 50),   // 閾値以上 (50 >= 0)
            ExtMove::new(Move::NONE, 200),  // 閾値以上 (200 >= 0)
            ExtMove::new(Move::NONE, -100), // 閾値未満 (-100 < 0)
        ];

        // limit=0でソート
        let len = moves.len();
        let good_count = partial_insertion_sort(&mut moves, len, 0);

        // 閾値(0)以上の手は100, 50, 200の3つ
        assert_eq!(good_count, 3);

        // 最初のgood_count個が閾値以上
        for (i, m) in moves.iter().enumerate().take(good_count) {
            assert!(m.value >= 0, "Move at index {i} should have value >= 0, got {}", m.value);
        }

        // good_count以降が閾値未満
        for (i, m) in moves.iter().enumerate().skip(good_count) {
            assert!(m.value < 0, "Move at index {i} should have value < 0, got {}", m.value);
        }
    }
}
