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
///     let mv = { let h = unsafe { ctx.history.as_ref_unchecked() }; mp.next_move(pos, h) };
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
    /// partial_insertion_sort の sorted 領域末尾位置（未使用だが YO 構造保持用）
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
            && pos.capture_stage(tt_move)
            && pos.pseudo_legal_with_all(tt_move, generate_all_legal_moves)
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
    /// YaneuraOu準拠: フラグのみ設定する。実際のステージ遷移は next_move() 側で処理される。
    pub fn skip_quiets(&mut self) {
        self.skip_quiets = true;
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
                    self.moves.clear();

                    // YaneuraOu準拠: CaptureInit/QCaptureInit/ProbCutInit は同じ capture 生成を使う
                    let count = if self.generate_all_legal_moves {
                        crate::movegen::generate_with_type(
                            pos,
                            crate::movegen::GenType::CapturesAll,
                            &mut self.moves,
                            None,
                        );
                        self.moves.len()
                    } else {
                        crate::movegen::generate_with_type(
                            pos,
                            crate::movegen::GenType::Captures,
                            &mut self.moves,
                            None,
                        );
                        self.moves.len()
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
                    // YaneuraOu準拠: 常に QuietInit へ遷移。skip_quiets は
                    // QuietInit/GoodQuiet/BadQuiet 側で判定する。
                    self.stage = Stage::QuietInit;
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
                                self.moves.set(self.end_captures + c, ExtMove::new(ext.mv, 0));
                                c += 1;
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

                        // YaneuraOu準拠: 深さベースの閾値で部分ソート
                        // -3560 * depth で depth が浅いほど閾値が高く、多くの手がソート対象
                        let limit = -3560 * self.depth;
                        let quiet_count = self.end_cur - self.end_captures;
                        let sorted_end = partial_insertion_sort(
                            &mut self.moves.as_mut_slice()[self.end_captures..],
                            quiet_count,
                            limit,
                        );
                        // sorted 領域末尾位置（現在は実運用で未参照）
                        self.end_good_quiets = self.end_captures + sorted_end;
                    } else {
                        self.end_good_quiets = self.end_captures;
                    }
                    self.stage = Stage::GoodQuiet;
                }

                // ==============================
                // 良い静かな手を返す（YaneuraOu準拠: value > GOOD_QUIET_THRESHOLD）
                // partial_insertion_sortで高スコア手が先頭に来るが、
                // YaneuraOu同様に全quiet手を走査してthreshold超の手を返す
                // ==============================
                Stage::GoodQuiet => {
                    if !self.skip_quiets {
                        self.end_cur = self.end_generated;
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
            let captured = pos.piece_on(to);
            let captured_pt = captured.piece_type();

            // MVV + CaptureHistory + 王手ボーナス
            let mut value = history.capture_history.get(pc, to, captured_pt) as i32;
            value += 7 * piece_value(captured);

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
            let main_h = 2 * history.main_history.get(us, m) as i32;
            value += main_h;

            // PawnHistory (×2)
            let pawn_h = 2 * history.pawn_history.get(pawn_idx, pc, to) as i32;
            value += pawn_h;

            // ContinuationHistory (6個のうち5個を使用)
            // インデックス4 (ply-5) はスキップ: YaneuraOu準拠で、ply-5は統計的に有効性が低いため除外
            // 参照: yaneuraou-search.cpp の continuationHistory 配列の使用箇所
            let mut cont_h = 0i32;
            for (idx, weight) in [(0, 1), (1, 1), (2, 1), (3, 1), (5, 1)] {
                let ch = self.cont_history(idx);
                cont_h += weight * ch.get(pc, to) as i32;
            }
            value += cont_h;

            // 王手ボーナス
            let check_bonus =
                if pos.check_squares(pt).contains(to) && pos.see_ge(m, Value::new(-75)) {
                    16384
                } else {
                    0
                };
            value += check_bonus;

            // LowPlyHistory
            let low_ply_h = if self.ply < LOW_PLY_HISTORY_SIZE as i32 {
                let ply_idx = self.ply as usize;
                8 * history.low_ply_history.get(ply_idx, m) as i32 / (1 + self.ply)
            } else {
                0
            };
            value += low_ply_h;

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

            if pos.capture_stage(m) {
                // 捕獲手は駒価値 + 大きなボーナス
                let captured = pos.piece_on(to);
                self.moves.set_value(i, piece_value(captured) + (1 << 28));
            } else {
                // 静かな手はHistory
                let mut value = history.main_history.get(us, m) as i32;

                let ch = self.cont_history(0);
                value += ch.get(pc, to) as i32;

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

/// 部分ソート（ハイブリッド方式）
///
/// YaneuraOu準拠の部分挿入ソート。
///
/// `limit` 以上のスコアの手を先頭に集め、降順でソートする。
/// 先頭要素（index 0）を初期 sorted 領域とみなし、index 1 から走査する。
///
/// ## 戻り値
/// sorted 領域の末尾インデックス（sorted_end）を返す。
/// sorted 領域は `[0, sorted_end]`（inclusive）。
/// - `end <= 1` の場合は 0 を返す（ソート不要）
///
/// ## 計算量
/// - 挿入ソート相当
fn partial_insertion_sort(moves: &mut [ExtMove], end: usize, limit: i32) -> usize {
    // YaneuraOu: movepick.cpp partial_insertion_sort()
    //   for (ExtMove *sortedEnd = begin, *p = begin + 1; p < end; ++p)
    //       if (p->value >= limit) {
    //           ExtMove tmp = *p, *q;
    //           *p = *++sortedEnd;
    //           for (q = sortedEnd; q != begin && *(q - 1) < tmp; --q)
    //               *q = *(q - 1);
    //           *q = tmp;
    //       }
    let mut sorted_end: usize = 0;
    for p in 1..end {
        if moves[p].value >= limit {
            let tmp = moves[p];
            sorted_end += 1;
            moves[p] = moves[sorted_end];
            let mut q = sorted_end;
            while q > 0 && moves[q - 1].value < tmp.value {
                moves[q] = moves[q - 1];
                q -= 1;
            }
            moves[q] = tmp;
        }
    }
    sorted_end
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
        // YaneuraOu Eval::PieceValue 準拠
        Horse => 945,
        Dragon => 1395,
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
        // YaneuraOu準拠: index 0 を初期 sorted 領域とし、index 1 から走査
        let mut moves = vec![
            ExtMove::new(Move::NONE, 100),
            ExtMove::new(Move::NONE, 50),
            ExtMove::new(Move::NONE, 200),
            ExtMove::new(Move::NONE, 10),
            ExtMove::new(Move::NONE, 150),
        ];

        let len = moves.len();
        let sorted_end = partial_insertion_sort(&mut moves, len, 100);

        // index 1 以降で >= 100 の手は 200, 150 の2つ → sorted_end=2
        assert_eq!(sorted_end, 2);
        // sorted 領域 [0..=2] が降順
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
        let sorted_end = partial_insertion_sort(&mut moves, len, 100);

        // index 1 以降で >= 100: 100, 101 の2つ → sorted_end=2
        assert_eq!(sorted_end, 2);
        assert_eq!(moves[0].value, 101);
        assert_eq!(moves[1].value, 100);
        assert_eq!(moves[2].value, 99);
    }

    #[test]
    fn test_partial_insertion_sort_large_array() {
        // 20要素の配列
        let mut moves: Vec<ExtMove> = (0..20).map(|i| ExtMove::new(Move::NONE, i * 10)).collect();

        let len = moves.len();
        // limit=100: index 1 以降で >= 100 の手は 100, 110, ..., 190 の10個
        let sorted_end = partial_insertion_sort(&mut moves, len, 100);

        assert_eq!(sorted_end, 10);
        // sorted 領域先頭に降順配置
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
        let sorted_end = partial_insertion_sort(&mut moves, len, 100);

        assert_eq!(sorted_end, 0);
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
        let sorted_end = partial_insertion_sort(&mut moves, len, 50);

        // index 1 以降で >= 50: 200, 150 の2つ → sorted_end=2
        assert_eq!(sorted_end, 2);
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
        let sorted_end = partial_insertion_sort(&mut moves, len, i32::MIN);

        // index 1 以降の全3要素が >= i32::MIN → sorted_end=3
        assert_eq!(sorted_end, 3);
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
        let sorted_end = partial_insertion_sort(&mut moves, 0, 100);
        assert_eq!(sorted_end, 0);
    }

    #[test]
    fn test_partial_insertion_sort_single_element() {
        // 1要素の配列: ループ 1..1 は実行されない → sorted_end=0
        let mut moves = vec![ExtMove::new(Move::NONE, 150)];
        let sorted_end = partial_insertion_sort(&mut moves, 1, 100);
        assert_eq!(sorted_end, 0);
        assert_eq!(moves[0].value, 150);

        let mut moves2 = vec![ExtMove::new(Move::NONE, 50)];
        let sorted_end2 = partial_insertion_sort(&mut moves2, 1, 100);
        assert_eq!(sorted_end2, 0);
    }

    #[test]
    fn test_piece_value() {
        assert_eq!(piece_value(Piece::B_PAWN), 90);
        assert_eq!(piece_value(Piece::W_GOLD), 540);
        assert_eq!(piece_value(Piece::B_ROOK), 990);
        assert_eq!(piece_value(Piece::W_HORSE), 945);
        assert_eq!(piece_value(Piece::W_DRAGON), 1395);
    }

    /// partial_insertion_sort のソート順が YO と同型であることを検証
    #[test]
    fn test_partial_insertion_sort_order() {
        // YaneuraOu準拠: index 0 は初期 sorted 領域、index 1 以降を走査
        let mut moves = vec![
            ExtMove::new(Move::NONE, 100),  // index 0: 初期 sorted 領域
            ExtMove::new(Move::NONE, -200), // index 1: < 0, skip
            ExtMove::new(Move::NONE, 50),   // index 2: >= 0, sorted_end=1
            ExtMove::new(Move::NONE, 200),  // index 3: >= 0, sorted_end=2
            ExtMove::new(Move::NONE, -100), // index 4: < 0, skip
        ];

        let len = moves.len();
        let sorted_end = partial_insertion_sort(&mut moves, len, 0);

        // index 1 以降で >= 0 の手は 50, 200 の2つ
        assert_eq!(sorted_end, 2);

        // sorted 領域 [0..=2] は降順
        assert_eq!(moves[0].value, 200);
        assert_eq!(moves[1].value, 100);
        assert_eq!(moves[2].value, 50);
        // 残りは閾値未満
        assert!(moves[3].value < 0);
        assert!(moves[4].value < 0);
    }
}
