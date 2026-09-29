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

use super::{ContHistKey, HistoryTables, LOW_PLY_HISTORY_SIZE};
use crate::movegen::{ExtMove, ExtMoveBuffer};
use crate::position::Position;
use crate::types::{Color, DEPTH_QS, Depth, Move, Piece, PieceType, Value};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

static MP_LAZY_QUIET: AtomicBool = AtomicBool::new(false);
static MP_LAZY_MIN_K: AtomicUsize = AtomicUsize::new(9);

/// quiet の遅延ソートを切り替える（既定 false）。
///
/// 全探索を停止した状態で設定し、探索中は変更しないこと。
/// 各ノードでは QuietInit で一度だけ読み、その後は picker の境界で管理する。
pub fn set_mp_lazy_quiet(enabled: bool) {
    MP_LAZY_QUIET.store(enabled, Ordering::Relaxed);
}

/// quiet の遅延選択を使う高値域の最小手数を設定する（既定 9、0 は 1 に補正）。
///
/// 全探索を停止した状態で設定し、探索中は変更しないこと。
pub fn set_mp_lazy_min_k(min_k: usize) {
    MP_LAZY_MIN_K.store(min_k.max(1), Ordering::Relaxed);
}

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
    /// 遅延選択で良い静かな手を返す
    LazyGoodQuiet,
    /// 遅延選択で悪い静かな手を返す
    LazyBadQuiet,

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
            Stage::LazyGoodQuiet => Stage::BadCapture,
            Stage::LazyBadQuiet => Stage::LazyBadQuiet, // 終端

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

    // キーだけを保持し、スコアリング時の生存中 HistoryTables から解決する。
    continuation_history: [ContHistKey; 6],

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
    /// 遅延経路のソート対象領域の末尾（exclusive）。先頭手は limit 未満でも含む。
    /// 通常経路では end_captures とし、BadCapture 後の遷移先の識別に使う。
    end_good_quiets: usize,
    /// 降順が確定した接頭辞の末尾。遅延選択なしなら usize::MAX。
    lazy_quiet_upto: usize,
    /// 残りを挿入ソートで仕上げるまでの選択予算。
    lazy_quiet_budget: usize,
}

impl MovePicker {
    /// 通常探索・静止探索用コンストラクタ（History参照を保持しない）
    ///
    /// `pos`は初期化時のみ使用し、フィールドとして保持しない。
    /// `continuation_history` は 1〜6 手前のキーであり、参照を保持しない。
    /// 履歴は `next_move` に渡された owner から、その段階のスコアリング時に読む。
    ///
    /// 旧 API の `[&PieceToHistory; 6]` は `[ContHistKey; 6]` に置き換える。
    /// 履歴のない手前の ply には `ContHistKey::null_sentinel()` を指定する。
    /// 単独の `PieceToHistory` を渡す代わりに、選択したキーに対応する
    /// `HistoryTables::continuation_history` 内のテーブルへ値を設定する。
    ///
    pub fn new(
        pos: &Position,
        tt_move: Move,
        depth: Depth,
        ply: i32,
        continuation_history: [ContHistKey; 6],
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
            continuation_history,
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
            lazy_quiet_upto: usize::MAX,
            lazy_quiet_budget: 0,
        }
    }

    /// 王手回避専用コンストラクタ（シンプル版）
    pub fn new_evasions(
        pos: &Position,
        tt_move: Move,
        ply: i32,
        continuation_history: [ContHistKey; 6],
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
            continuation_history,
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
            lazy_quiet_upto: usize::MAX,
            lazy_quiet_budget: 0,
        }
    }

    /// ProbCut専用コンストラクタ
    pub fn new_probcut(
        pos: &Position,
        tt_move: Move,
        threshold: Value,
        ply: i32,
        continuation_history: [ContHistKey; 6],
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
            continuation_history,
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
            lazy_quiet_upto: usize::MAX,
            lazy_quiet_budget: 0,
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
    /// LMP発火条件の判定に使用する。quiet段階（QuietInit, GoodQuiet, BadQuiet）
    /// および遅延選択の quiet 段階でのみLMPを発火させ、captureは残す。
    #[inline]
    pub fn is_quiet_stage(&self) -> bool {
        matches!(
            self.stage,
            Stage::QuietInit
                | Stage::GoodQuiet
                | Stage::BadQuiet
                | Stage::LazyGoodQuiet
                | Stage::LazyBadQuiet
        )
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
    /// `Move::NONE` を除き、返す手は常に現局面で pseudo-legal である。
    /// TT 手はコンストラクタで `pseudo_legal_with_all` による検査済みであり、
    /// それ以外の手は指し手生成器の出力である。
    /// 通常探索・root・qsearch・probcut の各ループはこの契約に依存し、`is_legal` だけを検査する。
    /// 将来 killer 段など生成器以外から手を返す段を追加する場合は、TT 手と同じ検査が必要である。
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
                            crate::movegen::generate_with_type(
                                pos,
                                crate::movegen::GenType::QuietsAll,
                                &mut self.moves,
                                None,
                            );
                            self.moves.len() - self.end_captures
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
                        let min_k = MP_LAZY_QUIET
                            .load(Ordering::Relaxed)
                            .then(|| MP_LAZY_MIN_K.load(Ordering::Relaxed));
                        self.stage = self.init_quiet_order(limit, min_k);
                    } else {
                        self.end_good_quiets = self.end_captures;
                        self.stage = Stage::GoodQuiet;
                    }
                }

                // ==============================
                // 良い静かな手を返す（YaneuraOu準拠: value > GOOD_QUIET_THRESHOLD）
                // partial_insertion_sortで高スコア手が先頭に来るが、
                // YaneuraOu同様に全quiet手を走査してthreshold超の手を返す
                // ==============================
                Stage::GoodQuiet => {
                    if let Some(m) = self.next_good_quiet::<false>() {
                        return m;
                    }
                }
                Stage::LazyGoodQuiet => {
                    if let Some(m) = self.next_good_quiet::<true>() {
                        return m;
                    }
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
                    self.stage = if self.end_good_quiets > self.end_captures {
                        Stage::LazyBadQuiet
                    } else {
                        Stage::BadQuiet
                    };
                }

                // ==============================
                // 悪い静かな手を返す（YaneuraOu準拠: value <= GOOD_QUIET_THRESHOLD）
                // ==============================
                Stage::BadQuiet => {
                    if !self.skip_quiets {
                        // YaneuraOu準拠: endCaptures から全quiet手を再走査し、
                        // value <= GOOD_QUIET_THRESHOLD の手のみ返す
                        if let Some(m) = self.select_bad_quiet::<false>() {
                            return m;
                        }
                    }
                    return Move::NONE;
                }
                Stage::LazyBadQuiet => {
                    if !self.skip_quiets
                        && let Some(m) = self.select_bad_quiet::<true>()
                    {
                        return m;
                    }
                    return Move::NONE;
                }

                // ==============================
                // 回避手の生成
                // ==============================
                Stage::EvasionInit => {
                    // 回避手を生成
                    let count = if self.generate_all_legal_moves {
                        self.moves.clear();
                        crate::movegen::generate_with_type(
                            pos,
                            crate::movegen::GenType::EvasionsAll,
                            &mut self.moves,
                            None,
                        );
                        self.moves.len()
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

    /// 先頭の手と limit 以上の手をソート対象にし、低値域の swap 順序を維持する。
    fn init_quiet_order(&mut self, limit: i32, min_k: Option<usize>) -> Stage {
        let moves = &mut self.moves.as_mut_slice()[self.end_captures..self.end_generated];
        let count = moves.len();
        self.lazy_quiet_upto = usize::MAX;
        self.lazy_quiet_budget = 0;
        // 先頭手は limit 未満でもソート対象。partition 前に数え、閾値未満なら
        // 入力バッファを変更せず、通常経路と同じ部分挿入ソートへ渡す。
        let use_lazy = min_k.is_some_and(|min_k| {
            count > 0 && 1 + moves.iter().skip(1).filter(|m| m.value >= limit).count() >= min_k
        });
        if !use_lazy {
            partial_insertion_sort(moves, count, limit);
            self.end_good_quiets = self.end_captures;
            return Stage::GoodQuiet;
        }
        let high_count = partition_quiets(moves, limit);
        self.end_good_quiets = self.end_captures + high_count;
        self.lazy_quiet_upto = self.end_captures;
        // 小さい領域ほど早く挿入ソートに戻す。
        self.lazy_quiet_budget = (high_count / 4).clamp(6, 8);
        Stage::LazyGoodQuiet
    }

    /// 未確定の高値域から 1 手だけ確定する。BadQuiet の再走査では確定済みを触らない。
    #[inline]
    fn ensure_quiet_order(&mut self) {
        if self.cur >= self.lazy_quiet_upto && self.cur < self.end_good_quiets {
            self.select_next_quiet();
        }
    }

    fn select_next_quiet(&mut self) {
        debug_assert_eq!(self.cur, self.lazy_quiet_upto);
        let moves = &mut self.moves.as_mut_slice()[self.cur..self.end_good_quiets];
        if self.lazy_quiet_budget == 0 {
            partial_insertion_sort(moves, moves.len(), i32::MIN);
            self.lazy_quiet_upto = usize::MAX;
            return;
        }
        let mut best = 0;
        for i in 1..moves.len() {
            if moves[i].value > moves[best].value {
                best = i;
            }
        }
        let selected = moves[best];
        // 回転で未選択手の相対順を保ち、同値の先勝ちを安定ソートと一致させる。
        for i in (1..=best).rev() {
            moves[i] = moves[i - 1];
        }
        moves[0] = selected;
        self.lazy_quiet_upto = self.cur + 1;
        self.lazy_quiet_budget -= 1;
    }

    // =========================================================================
    // スコアリング
    // =========================================================================

    /// 捕獲手のスコアを計算
    fn score_captures(&mut self, pos: &Position, history: &HistoryTables) {
        debug_assert!(self.cur <= self.end_cur && self.end_cur <= self.moves.len());
        // SAFETY: cur <= end_cur <= moves.len() は MovePicker の不変条件。
        for ext in unsafe { self.moves.as_mut_slice().get_unchecked_mut(self.cur..self.end_cur) } {
            let m = ext.mv;
            let to = m.to();
            let pc = m.moved_piece_after();
            let captured = pos.piece_on(to);
            let captured_pt = captured.piece_type();

            // MVV + CaptureHistory + 王手ボーナス
            let mut value = history.capture_history.get(pc, to, captured_pt) as i32;
            value += 7 * piece_value(captured);

            ext.value = value;
        }
    }

    /// 静かな手のスコアを計算
    fn score_quiets(&mut self, pos: &Position, history: &HistoryTables) {
        let us = self.side_to_move;
        let pawn_idx = self.pawn_history_index;
        debug_assert!(self.cur <= self.end_cur && self.end_cur <= self.moves.len());
        // SAFETY: cur <= end_cur <= moves.len() は MovePicker の不変条件。
        let moves = unsafe { self.moves.as_mut_slice().get_unchecked_mut(self.cur..self.end_cur) };
        let tables = self.continuation_history.map(|key| {
            history.continuation_history[key.in_check as usize][key.capture as usize]
                .get_table(key.piece, key.to)
        });
        let [ch0, ch1, ch2, ch3, _, ch5] = tables;

        if self.ply < LOW_PLY_HISTORY_SIZE as i32 {
            debug_assert!(self.ply >= 0, "ply must be non-negative: {}", self.ply);
            let low_ply_idx = self.ply as usize;
            let low_ply_div = 1 + self.ply;

            for ext in moves {
                let m = ext.mv;
                let to = m.to();
                let pc = m.moved_piece_after();
                let pt = pc.piece_type();

                let mut value = 2 * history.main_history.get(us, m) as i32;
                value += 2 * history.pawn_history.get(pawn_idx, pc, to) as i32;
                value += ch0.get(pc, to) as i32;
                value += ch1.get(pc, to) as i32;
                value += ch2.get(pc, to) as i32;
                value += ch3.get(pc, to) as i32;
                value += ch5.get(pc, to) as i32;

                if pos.check_squares(pt).contains(to) && pos.see_ge(m, Value::new(-75)) {
                    value += 16384;
                }

                // ply >= 0 (debug_assert 済み) なので low_ply_div >= 1 だが、
                // コンパイラが除算ゼロチェックを除去できないため .max(1) で明示。
                value +=
                    8 * history.low_ply_history.get(low_ply_idx, m) as i32 / low_ply_div.max(1);
                ext.value = value;
            }
        } else {
            for ext in moves {
                let m = ext.mv;
                let to = m.to();
                let pc = m.moved_piece_after();
                let pt = pc.piece_type();

                let mut value = 2 * history.main_history.get(us, m) as i32;
                value += 2 * history.pawn_history.get(pawn_idx, pc, to) as i32;
                value += ch0.get(pc, to) as i32;
                value += ch1.get(pc, to) as i32;
                value += ch2.get(pc, to) as i32;
                value += ch3.get(pc, to) as i32;
                value += ch5.get(pc, to) as i32;

                if pos.check_squares(pt).contains(to) && pos.see_ge(m, Value::new(-75)) {
                    value += 16384;
                }

                ext.value = value;
            }
        }
    }

    /// 回避手のスコアを計算
    fn score_evasions(&mut self, pos: &Position, history: &HistoryTables) {
        let us = self.side_to_move;
        let key = self.continuation_history[0];
        let ch = history.continuation_history[key.in_check as usize][key.capture as usize]
            .get_table(key.piece, key.to);

        debug_assert!(self.cur <= self.end_cur && self.end_cur <= self.moves.len());
        // SAFETY: cur <= end_cur <= moves.len() は MovePicker の不変条件。
        for ext in unsafe { self.moves.as_mut_slice().get_unchecked_mut(self.cur..self.end_cur) } {
            let m = ext.mv;
            let to = m.to();
            let pc = m.moved_piece_after();

            if pos.capture_stage(m) {
                // 捕獲手は駒価値 + 大きなボーナス
                let captured = pos.piece_on(to);
                ext.value = piece_value(captured) + (1 << 28);
            } else {
                // 静かな手はHistory
                let mut value = history.main_history.get(us, m) as i32;
                value += ch.get(pc, to) as i32;
                ext.value = value;
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

    /// quiet の選択方法は Stage で決め、通常経路には遅延境界の判定を生成しない。
    #[inline]
    fn next_good_quiet<const LAZY: bool>(&mut self) -> Option<Move> {
        if !self.skip_quiets {
            self.end_cur = self.end_generated;
            if let Some(m) = self.select_good_quiet::<LAZY>() {
                return Some(m);
            }
        }
        self.cur = 0;
        self.end_cur = self.end_bad_captures;
        self.stage = Stage::BadCapture;
        None
    }

    /// GoodQuiet用: value > GOOD_QUIET_THRESHOLD の手のみ返す（YaneuraOu準拠）
    fn select_good_quiet<const LAZY: bool>(&mut self) -> Option<Move> {
        const GOOD_QUIET_THRESHOLD: i32 = -14000;
        while self.cur < self.end_cur {
            if LAZY {
                self.ensure_quiet_order();
            }
            let ext = self.moves.get(self.cur);
            self.cur += 1;

            if LAZY && self.cur <= self.end_good_quiets && ext.value <= GOOD_QUIET_THRESHOLD {
                // 残りの高値域にも good quiet はない。未確定分は BadQuiet で再開する。
                self.cur = self.end_good_quiets;
                continue;
            }

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
    fn select_bad_quiet<const LAZY: bool>(&mut self) -> Option<Move> {
        const GOOD_QUIET_THRESHOLD: i32 = -14000;
        while self.cur < self.end_cur {
            if LAZY {
                self.ensure_quiet_order();
            }
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
#[inline]
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
    if end <= 1 {
        return 0;
    }

    let mut sorted_end: usize = 0;
    let base = moves.as_mut_ptr();
    for p in 1..end {
        // SAFETY:
        // - p は常に 1..end の範囲で、end は呼び出し元が `moves.len()` 以下で渡す。
        // - sorted_end は 0..p の範囲を保つため、add() でアクセスする要素は常に同一 slice 内。
        // - ExtMove は Copy なので、要素の読み書き・シフトで drop 問題は発生しない。
        unsafe {
            let tmp = *base.add(p);
            if tmp.value >= limit {
                sorted_end += 1;
                if p != sorted_end {
                    *base.add(p) = *base.add(sorted_end);
                }
                let mut q = sorted_end;
                while q > 0 && (*base.add(q - 1)).value < tmp.value {
                    *base.add(q) = *base.add(q - 1);
                    q -= 1;
                }
                *base.add(q) = tmp;
            }
        }
    }
    sorted_end
}

/// 挿入ソートと同じ低値域を作り、先頭の手 + limit 以上の手の個数を返す。
#[inline]
fn partition_quiets(moves: &mut [ExtMove], limit: i32) -> usize {
    if moves.is_empty() {
        return 0;
    }
    let mut sorted_end = 0;
    for p in 1..moves.len() {
        let candidate = moves[p];
        let high = candidate.value >= limit;
        // 低値なら自身との swap。書き込む値でなく添字を選び、cmov にする。
        // 高値なら sorted_end + 1 と swap し、挿入ソートと同じ末尾を作る。
        let next = sorted_end + 1;
        let destination = if high { next } else { p };
        moves[p] = moves[destination];
        moves[destination] = candidate;
        sorted_end += usize::from(high);
    }
    sorted_end + 1
}

/// 駒の価値（MVV用）
#[inline]
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
    use crate::types::Square;
    use rand::{Rng, SeedableRng};
    use rand_xoshiro::Xoshiro256PlusPlus;

    fn scored_quiet_picker(
        pos: &Position,
        scores: &[i32],
        limit: i32,
        tt_move: Move,
        min_k: Option<usize>,
    ) -> MovePicker {
        let mut picker =
            MovePicker::new(pos, Move::NONE, 1, 0, [ContHistKey::null_sentinel(); 6], false);
        // 消費済み capture の領域と、後で返す bad capture を含む境界を再現する。
        for raw in 1..=3 {
            picker.moves.push(ExtMove::new(Move::from_u16(raw), 0));
        }
        picker.end_bad_captures = 2;
        picker.end_captures = 3;
        for (i, &value) in scores.iter().enumerate() {
            picker.moves.push(ExtMove::new(Move::from_u16(i as u16 + 100), value));
        }
        picker.end_generated = picker.moves.len();
        picker.end_cur = picker.end_generated;
        picker.cur = picker.end_captures;
        picker.tt_move = tt_move;
        picker.stage = picker.init_quiet_order(limit, min_k);
        picker
    }

    #[test]
    fn lazy_quiets_match_legacy_for_random_scores_and_skips() {
        let mut rng = Xoshiro256PlusPlus::seed_from_u64(0x004d_504c_415a_5932);
        let mut pos = Position::new();
        pos.set_hirate();
        let history = HistoryTables::new_boxed();
        let lengths = [
            0,
            1,
            2,
            8,
            9,
            10,
            15,
            16,
            17,
            23,
            24,
            25,
            31,
            32,
            33,
            65,
            crate::movegen::MAX_MOVES - 3,
        ];
        for case in 0..1500 {
            let n = if case < lengths.len() * 10 {
                lengths[case % lengths.len()]
            } else {
                rng.random_range(0..180)
            };
            let limit = [-3560, -7120, -14240, -14000, i32::MIN, i32::MAX][case % 6];
            let scores: Vec<i32> = (0..n)
                .map(|_| match case % 5 {
                    0 => limit.saturating_sub(1),
                    1 => limit,
                    2 => [-14001, -14000, -13999, limit][rng.random_range(0..4)],
                    3 => rng.random_range(-40000..=40000),
                    _ => [i32::MIN, i32::MAX, 0][rng.random_range(0..3)],
                })
                .collect();
            let tt = match case % 4 {
                0 if n > 0 => Move::from_u16(rng.random_range(0..n) as u16 + 100),
                1 => Move::from_u16(1),
                _ => Move::NONE,
            };

            // ExtMove の Eq は value だけを見るため、末尾も Move の全ビットで比較する。
            let mut expected: Vec<_> = scores
                .iter()
                .enumerate()
                .map(|(i, &v)| ExtMove::new(Move::from_u16(i as u16 + 100), v))
                .collect();
            partial_insertion_sort(&mut expected, n, limit);
            let mut partitioned: Vec<_> = scores
                .iter()
                .enumerate()
                .map(|(i, &v)| ExtMove::new(Move::from_u16(i as u16 + 100), v))
                .collect();
            let high_count = partition_quiets(&mut partitioned, limit);
            for (actual, reference) in partitioned[high_count..].iter().zip(&expected[high_count..])
            {
                assert_eq!(
                    (actual.mv.raw32(), actual.value),
                    (reference.mv.raw32(), reference.value)
                );
            }
            partial_insertion_sort(&mut partitioned, high_count, i32::MIN);
            for (actual, reference) in partitioned.iter().zip(&expected) {
                assert_eq!(
                    (actual.mv.raw32(), actual.value),
                    (reference.mv.raw32(), reference.value)
                );
            }

            let skip_points: Vec<_> = if n <= 9 {
                (0..=n + 3).chain(std::iter::once(usize::MAX)).collect()
            } else {
                vec![
                    0,
                    1,
                    6,
                    8,
                    n / 2,
                    n,
                    rng.random_range(0..=n + 2),
                    usize::MAX,
                ]
            };
            for skip_after in skip_points {
                // picker の走査実装とは独立に、旧バッファのフィルタ結果を oracle にする。
                let good = expected
                    .iter()
                    .filter(|m| m.value > -14000)
                    .map(|m| (m.mv.raw32(), Stage::GoodQuiet));
                let captures = [(1, Stage::BadCapture), (2, Stage::BadCapture)];
                let bad = expected
                    .iter()
                    .filter(|m| m.value <= -14000)
                    .map(|m| (m.mv.raw32(), Stage::BadQuiet));
                let mut reference = Vec::new();
                for (raw, stage) in good.chain(captures).chain(bad) {
                    if raw == tt.raw32() {
                        continue;
                    }
                    // skip は捕獲手を返した後も有効。
                    if reference.len() >= skip_after && stage != Stage::BadCapture {
                        continue;
                    }
                    reference.push((raw, stage));
                }
                reference.push((Move::NONE.raw32(), Stage::BadQuiet));
                for min_k in [1, 9, 16, 24, 32, crate::movegen::MAX_MOVES + 1] {
                    let mut old = scored_quiet_picker(&pos, &scores, limit, tt, None);
                    let mut new = scored_quiet_picker(&pos, &scores, limit, tt, Some(min_k));
                    let mut finished = false;
                    for (consumed, &reference_move) in reference.iter().enumerate() {
                        if consumed == skip_after {
                            old.skip_quiets();
                            new.skip_quiets();
                        }
                        let expected = old.next_move(&pos, &history);
                        let actual = new.next_move(&pos, &history);
                        assert_eq!(new.is_quiet_stage(), old.is_quiet_stage());
                        let stage = match new.stage() {
                            Stage::LazyGoodQuiet => Stage::GoodQuiet,
                            Stage::LazyBadQuiet => Stage::BadQuiet,
                            stage => stage,
                        };
                        assert_eq!((expected.raw32(), old.stage()), reference_move);
                        assert_eq!(
                            (actual.raw32(), stage),
                            (expected.raw32(), old.stage()),
                            "min_k={min_k}, case={case}, n={n}, limit={limit}, skip={skip_after}, consumed={consumed}"
                        );
                        if actual.is_none() {
                            finished = true;
                            break;
                        }
                    }
                    assert!(finished);
                }
            }
        }
    }

    #[test]
    fn lazy_quiets_screen_by_high_count_before_partition() {
        let mut pos = Position::new();
        pos.set_hirate();
        for min_k in [1, 9, 16, 24, 32] {
            for k in [min_k - 1, min_k, min_k + 1] {
                // 先頭手は低値でも k に含む。低値域と同値を混在させる。
                let mut scores = Vec::new();
                if k > 0 {
                    scores.push(-20000);
                    for i in 1..k {
                        scores.extend([-30000, (i % 3) as i32]);
                    }
                }
                let old = scored_quiet_picker(&pos, &scores, -10000, Move::NONE, None);
                let new = scored_quiet_picker(&pos, &scores, -10000, Move::NONE, Some(min_k));
                if k > 0 && k >= min_k {
                    assert_eq!(new.stage(), Stage::LazyGoodQuiet);
                    assert_eq!(new.end_good_quiets - new.end_captures, k);
                    assert_eq!(new.lazy_quiet_upto, new.end_captures);
                } else {
                    assert_eq!(new.stage(), Stage::GoodQuiet);
                    assert_eq!(new.end_good_quiets, new.end_captures);
                    assert_eq!(new.lazy_quiet_upto, usize::MAX);
                    for (actual, expected) in new.moves.as_slice().iter().zip(old.moves.as_slice())
                    {
                        assert_eq!(
                            (actual.mv.raw32(), actual.value),
                            (expected.mv.raw32(), expected.value)
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn lazy_quiets_resume_in_bad_quiet_and_fall_back() {
        let mut pos = Position::new();
        pos.set_hirate();
        let history = HistoryTables::new_boxed();
        let scores = vec![-14000; 40];
        let mut picker = scored_quiet_picker(&pos, &scores, -20000, Move::from_u16(100), Some(9));
        // 最大値が threshold 以下で、かつ TT 手でも、未確定の残りは保存する。
        assert_eq!(picker.next_move(&pos, &history).raw32(), 1);
        assert_eq!(picker.lazy_quiet_upto, picker.end_captures + 1);
        assert_eq!(picker.lazy_quiet_budget, 7);
        assert_eq!(picker.next_move(&pos, &history).raw32(), 2);
        for raw in 101..=139 {
            assert_eq!(picker.next_move(&pos, &history).raw32(), raw);
        }
        assert_eq!(picker.lazy_quiet_upto, usize::MAX);
        assert!(picker.next_move(&pos, &history).is_none());
    }

    fn all_moves(mut picker: MovePicker, pos: &Position, history: &HistoryTables) -> Vec<Move> {
        std::iter::from_fn(|| {
            let mv = picker.next_move(pos, history);
            mv.is_some().then_some(mv)
        })
        .collect()
    }

    #[test]
    fn continuation_keys_do_not_retain_owner() {
        let mut pos = Position::new();
        pos.set_hirate();
        let keys = [ContHistKey::null_sentinel(); 6];
        let picker = {
            let history = HistoryTables::new_boxed();
            let picker = MovePicker::new(&pos, Move::NONE, 1, 0, keys, false);
            drop(history);
            picker
        };
        let history = HistoryTables::new_boxed();
        let actual = all_moves(picker, &pos, &history);
        let control =
            all_moves(MovePicker::new(&pos, Move::NONE, 1, 0, keys, false), &pos, &history);
        assert_eq!(actual, control);
        assert_eq!(actual.len(), 30);
        let mut unique = actual.clone();
        unique.sort_by_key(|mv| mv.to_usi());
        unique.dedup();
        assert_eq!(unique.len(), 30);
    }

    #[test]
    fn continuation_keys_score_lazily_after_tt() {
        let mut pos = Position::new();
        pos.set_hirate();
        let tt = pos.to_move(Move::from_usi("7g7f").unwrap()).unwrap();
        let target = pos.to_move(Move::from_usi("2g2f").unwrap()).unwrap();
        let key =
            ContHistKey::new(true, true, Piece::new(Color::White, PieceType::Rook), Square::SQ_55);
        let mut keys = [ContHistKey::null_sentinel(); 6];
        keys[0] = key;
        let mut history = HistoryTables::new_boxed();
        let mut picker = MovePicker::new(&pos, tt, 1, 0, keys, false);
        assert_eq!(picker.next_move(&pos, &history), tt);
        // TT 手の探索から戻るまで quiet のスコアはまだ確定しない。
        let control = all_moves(MovePicker::new(&pos, tt, 1, 0, keys, false), &pos, &history);
        assert_ne!(control[1], target);
        history.continuation_history[1][1].get_table_mut(key.piece, key.to).update(
            target.moved_piece_after(),
            target.to(),
            10000,
        );
        assert_eq!(picker.next_move(&pos, &history), target);
        let mut expected = all_moves(MovePicker::new(&pos, tt, 1, 0, keys, false), &pos, &history);
        expected.drain(..2);
        assert_eq!(all_moves(picker, &pos, &history), expected);
    }

    #[test]
    fn continuation_keys_evasion_and_probcut() {
        let mut pos = Position::new();
        pos.set_sfen("k8/9/9/9/9/9/9/4r4/4K4 b - 1").unwrap();
        assert!(pos.in_check());
        let tt = pos.to_move(Move::from_usi("5i6i").unwrap()).unwrap();
        let target = pos.to_move(Move::from_usi("5i4i").unwrap()).unwrap();
        let key =
            ContHistKey::new(false, true, Piece::new(Color::Black, PieceType::Gold), Square::SQ_99);
        let keys = [key; 6];
        let mut history = HistoryTables::new_boxed();
        let mut picker = MovePicker::new_evasions(&pos, tt, 0, keys, false);
        assert_eq!(picker.next_move(&pos, &history), tt);
        history.continuation_history[0][1].get_table_mut(key.piece, key.to).update(
            target.moved_piece_after(),
            target.to(),
            10000,
        );
        let rest = all_moves(picker, &pos, &history);
        let quiet = rest.iter().find(|mv| !pos.capture_stage(**mv)).copied();
        assert_eq!(quiet, Some(target));
        let ordinary = all_moves(MovePicker::new(&pos, tt, 1, 0, keys, false), &pos, &history);
        assert_eq!(ordinary[1..], rest);

        pos.set_sfen("K8/9/9/4r4/4R4/9/9/9/8k b - 1").unwrap();
        let capture = pos.to_move(Move::from_usi("5e5d").unwrap()).unwrap();
        let picker = MovePicker::new_probcut(&pos, capture, Value::ZERO, 0, keys, false);
        drop(history);
        let live = HistoryTables::new_boxed();
        assert_eq!(all_moves(picker, &pos, &live), vec![capture]);
    }

    #[test]
    fn test_tt_stage_rejects_dead_pieces_but_keeps_legal_non_promotions() {
        let keys = [ContHistKey::null_sentinel(); 6];
        for (sfen, usi, accepted) in [
            ("4k4/9/9/9/9/9/9/9/4K4 b P 1", "P*4a", false),
            ("4k4/5P3/9/9/9/9/9/9/4K4 b - 1", "4b4a", false),
            ("4k4/9/9/5P3/9/9/9/9/4K4 b - 1", "4d4c", true),
        ] {
            let mut pos = Position::new();
            pos.set_sfen(sfen).unwrap();
            let tt_move = pos.to_move(Move::from_usi(usi).unwrap()).unwrap();
            for all in [false, true] {
                let main = MovePicker::new(&pos, tt_move, 1, 0, keys, all);
                assert_eq!(
                    main.stage,
                    if accepted {
                        Stage::MainTT
                    } else {
                        Stage::CaptureInit
                    }
                );
                let qsearch = MovePicker::new(&pos, tt_move, DEPTH_QS, 0, keys, all);
                assert_eq!(
                    qsearch.stage,
                    if accepted {
                        Stage::QSearchTT
                    } else {
                        Stage::QCaptureInit
                    }
                );
            }
        }
    }

    fn assert_picker_moves_pseudo_legal(
        mut picker: MovePicker,
        pos: &Position,
        history: &HistoryTables,
    ) {
        let initial_stage = picker.stage;
        loop {
            let mv = picker.next_move(pos, history);
            if mv.is_none() {
                break;
            }
            assert!(
                pos.pseudo_legal(mv),
                "pseudo-legal でない手 {}: stage={initial_stage:?} tt={} all={} sfen={}",
                mv.to_usi(),
                picker.tt_move.to_usi(),
                picker.generate_all_legal_moves,
                pos.to_sfen()
            );
        }
    }

    #[test]
    fn random_playouts_only_yield_pseudo_legal_moves() {
        use crate::movegen::{MoveList, generate_legal_all};
        use rand::{Rng, SeedableRng};
        use rand_xoshiro::Xoshiro256PlusPlus;

        let history = HistoryTables::new_boxed();
        let keys = [ContHistKey::null_sentinel(); 6];
        // 平手、合駒可能な王手、両王手、不成、後手番の駒打ちから手順を広げる。
        for sfen in [
            None,
            Some("k8/9/9/9/4r4/9/9/9/4K4 b RBGSNLP 1"),
            Some("k8/9/9/9/4r4/9/2b6/9/4K4 b GSNLP 1"),
            Some("k8/5P3/3NL4/9/9/9/9/9/4K4 b RBGSNLP 1"),
            Some("4k4/9/9/4p4/4P4/9/9/9/4K4 w RBGSNLPrbgsnlp 1"),
        ] {
            for seed in [0x5EED_u64, 0xCAFE] {
                let mut rng = Xoshiro256PlusPlus::seed_from_u64(seed);
                let mut pos = Position::new();
                if let Some(sfen) = sfen {
                    pos.set_sfen(sfen).unwrap();
                } else {
                    pos.set_hirate();
                }

                for ply in 0..128 {
                    let mut legal = MoveList::new();
                    generate_legal_all(&pos, &mut legal);
                    let next = if legal.is_empty() {
                        Move::NONE
                    } else {
                        legal.at(rng.random_range(0..legal.len()))
                    };
                    // 捕獲がある局面では ProbCut の TT 段階も通す。
                    let valid_tt =
                        legal.iter().copied().find(|&mv| pos.capture_stage(mv)).unwrap_or(next);
                    // 自玉の同一升への移動は、升と駒情報が有効でも必ず不正になる。
                    let king = pos.king_square(pos.side_to_move());
                    let garbage_tt =
                        Move::new_move_with_piece(king, king, false, pos.piece_on(king));
                    assert!(!pos.pseudo_legal(garbage_tt));

                    for all in [false, true] {
                        for tt in [Move::NONE, valid_tt, garbage_tt] {
                            for depth in [6, DEPTH_QS] {
                                assert_picker_moves_pseudo_legal(
                                    MovePicker::new(&pos, tt, depth, ply, keys, all),
                                    &pos,
                                    &history,
                                );
                            }
                            let picker = if pos.in_check() {
                                MovePicker::new_evasions(&pos, tt, ply, keys, all)
                            } else {
                                MovePicker::new_probcut(&pos, tt, Value::ZERO, ply, keys, all)
                            };
                            assert_picker_moves_pseudo_legal(picker, &pos, &history);
                        }
                    }

                    if next.is_none() {
                        break;
                    }
                    let gives_check = pos.gives_check(next);
                    pos.do_move(next, gives_check);
                }
            }
        }
    }

    #[test]
    fn partial_sort_partition_and_permutation() {
        let cases: &[(&[i32], i32, &[i32])] = &[
            (&[], 100, &[]),
            (&[50], 100, &[50]),
            (&[100, 50, 200, 10, 150], 100, &[200, 150, 100]),
            (&[99, 100, 101], 100, &[101, 100, 99]),
            (&[10, 20, 30], 100, &[10]),
            (&[50, -100, 200, 0], i32::MIN, &[200, 50, 0, -100]),
        ];
        for &(values, limit, prefix) in cases {
            let mut moves: Vec<_> = values.iter().map(|&v| ExtMove::new(Move::NONE, v)).collect();
            let len = moves.len();
            let end = partial_insertion_sort(&mut moves, len, limit);
            assert_eq!(end, prefix.len().saturating_sub(1));
            let mut actual: Vec<_> = moves.iter().map(|m| m.value).collect();
            assert_eq!(&actual[..prefix.len()], prefix);
            assert!(actual[prefix.len()..].iter().all(|&v| v < limit));
            let mut expected = values.to_vec();
            expected.sort_unstable();
            actual.sort_unstable();
            assert_eq!(actual, expected);
        }
    }
}
