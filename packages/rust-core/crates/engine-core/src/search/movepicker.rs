//! MovePicker（指し手オーダリング）
//!
//! 探索中に指し手を効率的に順序付けして返すコンポーネント。
//! Alpha-Beta探索の効率を最大化するため、カットオフを起こしやすい手を先に返す。
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

use super::{
    ButterflyHistory, CapturePieceToHistory, LowPlyHistory, PawnHistory, PieceToHistory,
    LOW_PLY_HISTORY_SIZE,
};
use crate::movegen::{ExtMove, ExtMoveBuffer};
use crate::position::Position;
use crate::types::{Depth, Move, Piece, PieceType, Value, DEPTH_QS};

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

/// 指し手オーダリング器
///
/// 探索中に指し手を効率的に順序付けして返す。
/// History統計を参照してスコアリングを行う。
pub struct MovePicker<'a> {
    // 参照
    pos: &'a Position,
    main_history: &'a ButterflyHistory,
    low_ply_history: &'a LowPlyHistory,
    capture_history: &'a CapturePieceToHistory,
    continuation_history: [&'a PieceToHistory; 6],
    pawn_history: &'a PawnHistory,

    // 状態
    stage: Stage,
    tt_move: Move,
    probcut_threshold: Option<Value>,
    #[allow(dead_code)] // 将来的にYaneuraOu方式の部分ソートで使用予定
    depth: Depth,
    ply: i32,
    skip_quiets: bool,
    generate_all_legal_moves: bool,

    // 指し手バッファ（MaybeUninitにより初期化コストゼロ）
    moves: ExtMoveBuffer,
    cur: usize,
    end_cur: usize,
    end_bad_captures: usize,
    end_captures: usize,
    end_generated: usize,
    /// 悪い静かな手の開始位置（GoodQuietステージ終了時に設定）
    end_bad_quiets: usize,
}

impl<'a> MovePicker<'a> {
    /// 通常探索・静止探索用コンストラクタ
    pub fn new(
        pos: &'a Position,
        tt_move: Move,
        depth: Depth,
        main_history: &'a ButterflyHistory,
        low_ply_history: &'a LowPlyHistory,
        capture_history: &'a CapturePieceToHistory,
        continuation_history: [&'a PieceToHistory; 6],
        pawn_history: &'a PawnHistory,
        ply: i32,
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
            pos,
            main_history,
            low_ply_history,
            capture_history,
            continuation_history,
            pawn_history,
            stage,
            tt_move,
            probcut_threshold: None,
            depth,
            ply,
            skip_quiets: false,
            generate_all_legal_moves,
            moves: ExtMoveBuffer::new(),
            cur: 0,
            end_cur: 0,
            end_bad_captures: 0,
            end_captures: 0,
            end_generated: 0,
            end_bad_quiets: 0,
        }
    }

    /// 王手回避専用コンストラクタ（シンプル版）
    pub fn new_evasions(
        pos: &'a Position,
        tt_move: Move,
        main_history: &'a ButterflyHistory,
        low_ply_history: &'a LowPlyHistory,
        capture_history: &'a CapturePieceToHistory,
        continuation_history: [&'a PieceToHistory; 6],
        pawn_history: &'a PawnHistory,
        ply: i32,
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
            pos,
            main_history,
            low_ply_history,
            capture_history,
            continuation_history,
            pawn_history,
            stage,
            tt_move,
            probcut_threshold: None,
            depth: DEPTH_QS,
            ply,
            skip_quiets: false,
            generate_all_legal_moves,
            moves: ExtMoveBuffer::new(),
            cur: 0,
            end_cur: 0,
            end_bad_captures: 0,
            end_captures: 0,
            end_generated: 0,
            end_bad_quiets: 0,
        }
    }

    /// ProbCut専用コンストラクタ
    pub fn new_probcut(
        pos: &'a Position,
        tt_move: Move,
        threshold: Value,
        main_history: &'a ButterflyHistory,
        low_ply_history: &'a LowPlyHistory,
        capture_history: &'a CapturePieceToHistory,
        continuation_history: [&'a PieceToHistory; 6],
        pawn_history: &'a PawnHistory,
        ply: i32,
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
            pos,
            main_history,
            low_ply_history,
            capture_history,
            continuation_history,
            pawn_history,
            stage,
            tt_move,
            probcut_threshold: Some(threshold),
            depth: DEPTH_QS,
            ply,
            skip_quiets: false,
            generate_all_legal_moves,
            moves: ExtMoveBuffer::new(),
            cur: 0,
            end_cur: 0,
            end_bad_captures: 0,
            end_captures: 0,
            end_generated: 0,
            end_bad_quiets: 0,
        }
    }

    /// 静かな手をスキップ
    pub fn skip_quiet_moves(&mut self) {
        self.skip_quiets = true;
    }

    /// 次の指し手を返す
    ///
    /// 指し手が尽きたら `Move::NONE` を返す。
    pub fn next_move(&mut self) -> Move {
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
                        crate::movegen::generate_with_type(self.pos, gen_type, &mut buf, None);
                        let mut c = 0;
                        for ext in buf.iter() {
                            if self.pos.is_capture(ext.mv) {
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
                            self.pos,
                            crate::movegen::GenType::CapturesProPlus,
                            &mut buf,
                            None,
                        );
                        // 捕獲手のみフィルタ
                        let mut tmp_count = 0usize;
                        for ext in buf.iter() {
                            if self.pos.is_capture(ext.mv) {
                                self.moves.set(tmp_count, ExtMove::new(ext.mv, 0));
                                tmp_count += 1;
                            }
                        }
                        self.moves.set_len(tmp_count);
                        tmp_count
                    } else {
                        self.pos.generate_captures(&mut self.moves)
                    };
                    self.end_cur = count;
                    self.end_captures = count;

                    self.score_captures();
                    partial_insertion_sort(self.moves.as_mut_slice(), self.end_cur, i32::MIN);

                    self.stage = self.stage.next();
                }

                // ==============================
                // 良い捕獲手を返す
                // ==============================
                Stage::GoodCapture => {
                    if let Some(m) = self.select_good_capture() {
                        return m;
                    }
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
                                self.pos,
                                crate::movegen::GenType::QuietsAll,
                                &mut buf,
                                None,
                            );
                            let mut c = 0;
                            for ext in buf.iter() {
                                if !self.pos.is_capture(ext.mv) {
                                    self.moves.set(self.end_captures + c, ExtMove::new(ext.mv, 0));
                                    c += 1;
                                }
                            }
                            c
                        } else {
                            self.pos.generate_quiets(&mut self.moves, self.end_captures)
                        };
                        self.end_cur = self.end_captures + count;
                        self.end_generated = self.end_cur;
                        self.moves.set_len(self.end_cur);

                        self.cur = self.end_captures;
                        self.score_quiets();

                        // PDQSortで静かな手の範囲だけをソート（YaneuraOu方式）
                        // その後は線形で返すだけなのでO(n log n) + O(n)
                        let quiet_slice =
                            &mut self.moves.as_mut_slice()[self.end_captures..self.end_cur];
                        if quiet_slice.len() > SORT_SWITCH_THRESHOLD {
                            // 大きい配列はPDQSort
                            quiet_slice.sort_unstable_by(|a, b| b.value.cmp(&a.value));
                        } else if quiet_slice.len() > 1 {
                            // 小さい配列は挿入ソート
                            for i in 1..quiet_slice.len() {
                                let tmp = quiet_slice[i];
                                let mut j = i;
                                while j > 0 && quiet_slice[j - 1].value < tmp.value {
                                    quiet_slice[j] = quiet_slice[j - 1];
                                    j -= 1;
                                }
                                quiet_slice[j] = tmp;
                            }
                        }
                    }
                    self.stage = Stage::GoodQuiet;
                }

                // ==============================
                // 静かな手を返す（YaneuraOu方式: ソート済みを線形返却）
                // ==============================
                Stage::GoodQuiet => {
                    if !self.skip_quiets {
                        // 閾値より大きいスコアの手のみ返す（YaneuraOu準拠）
                        if let Some(m) = self.select(|_, ext| ext.value > GOOD_QUIET_THRESHOLD) {
                            return m;
                        }
                    }

                    // 悪い捕獲手の準備（悪い静かな手の開始位置を保存）
                    self.end_bad_quiets = self.cur;
                    self.cur = 0;
                    self.end_cur = self.end_bad_captures;
                    self.stage = Stage::BadCapture;
                }

                // ==============================
                // 悪い捕獲手を返す
                // ==============================
                Stage::BadCapture => {
                    if let Some(m) = self.select(|_, _| true) {
                        return m;
                    }

                    // 悪い静かな手の準備（GoodQuietで止まった位置から再開）
                    self.cur = self.end_bad_quiets;
                    self.end_cur = self.end_generated;
                    self.stage = Stage::BadQuiet;
                }

                // ==============================
                // 悪い静かな手を返す（閾値以下のスコアの手）
                // ==============================
                Stage::BadQuiet => {
                    if !self.skip_quiets {
                        // 閾値以下のスコアの手を返す（YaneuraOu準拠）
                        if let Some(m) = self.select(|_, ext| ext.value <= GOOD_QUIET_THRESHOLD) {
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
                            self.pos,
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
                        self.pos.generate_evasions_ext(&mut self.moves)
                    };
                    self.cur = 0;
                    self.end_cur = count;
                    self.end_generated = count;

                    self.score_evasions();
                    partial_insertion_sort(self.moves.as_mut_slice(), self.end_cur, i32::MIN);

                    self.stage = Stage::Evasion;
                }

                // ==============================
                // 回避手を返す
                // ==============================
                Stage::Evasion => {
                    return self.select(|_, _| true).unwrap_or(Move::NONE);
                }

                // ==============================
                // 静止探索用捕獲手を返す
                // ==============================
                Stage::QCapture => {
                    return self.select(|_, _| true).unwrap_or(Move::NONE);
                }

                // ==============================
                // ProbCut: SEEが閾値以上の捕獲のみ
                // ==============================
                Stage::ProbCut => {
                    if let Some(th) = self.probcut_threshold {
                        return self
                            .select(|s, ext| s.pos.see_ge(ext.mv, th))
                            .unwrap_or(Move::NONE);
                    }
                    return Move::NONE;
                }
            }
        }
    }

    // =========================================================================
    // スコアリング
    // =========================================================================

    /// 捕獲手のスコアを計算
    fn score_captures(&mut self) {
        for i in self.cur..self.end_cur {
            let ext = self.moves.get(i);
            let m = ext.mv;
            let to = m.to();
            let pc = m.moved_piece_after();
            let pt = pc.piece_type();
            let captured = self.pos.piece_on(to);
            let captured_pt = captured.piece_type();

            // MVV + CaptureHistory + 王手ボーナス
            let mut value = self.capture_history.get(pc, to, captured_pt) as i32;
            value += 7 * piece_value(captured);

            // 王手になるマスへの移動にボーナス
            if self.pos.check_squares(pt).contains(to) {
                value += 1024;
            }

            self.moves.set_value(i, value);
        }
    }

    /// 静かな手のスコアを計算
    fn score_quiets(&mut self) {
        let us = self.pos.side_to_move();
        let pawn_idx = self.pos.pawn_history_index();

        for i in self.cur..self.end_cur {
            let ext = self.moves.get(i);
            let m = ext.mv;
            let to = m.to();
            let pc = m.moved_piece_after();
            let pt = pc.piece_type();
            let mut value = 0i32;

            // ButterflyHistory (×2)
            value += 2 * self.main_history.get(us, m) as i32;

            // PawnHistory (×2)
            value += 2 * self.pawn_history.get(pawn_idx, pc, to) as i32;

            // ContinuationHistory (6個のうち5個を使用)
            // インデックス4 (ply-5) はスキップ: YaneuraOu準拠で、ply-5は統計的に有効性が低いため除外
            // 参照: yaneuraou-search.cpp の continuationHistory 配列の使用箇所
            for (idx, weight) in [(0, 1), (1, 1), (2, 1), (3, 1), (5, 1)] {
                let ch = self.continuation_history[idx];
                value += weight * ch.get(pc, to) as i32;
            }

            // 王手ボーナス
            if self.pos.check_squares(pt).contains(to) && self.pos.see_ge(m, Value::new(-75)) {
                value += 16384;
            }

            // LowPlyHistory
            if self.ply < LOW_PLY_HISTORY_SIZE as i32 {
                let ply_idx = self.ply as usize;
                value += 8 * self.low_ply_history.get(ply_idx, m) as i32 / (1 + self.ply);
            }

            self.moves.set_value(i, value);
        }
    }

    /// 回避手のスコアを計算
    fn score_evasions(&mut self) {
        let us = self.pos.side_to_move();

        for i in self.cur..self.end_cur {
            let ext = self.moves.get(i);
            let m = ext.mv;
            let to = m.to();
            let pc = m.moved_piece_after();

            if self.pos.is_capture(m) {
                // 捕獲手は駒価値 + 大きなボーナス
                let captured = self.pos.piece_on(to);
                self.moves.set_value(i, piece_value(captured) + (1 << 28));
            } else {
                // 静かな手はHistory
                let mut value = self.main_history.get(us, m) as i32;

                let ch = self.continuation_history[0];
                value += ch.get(pc, to) as i32;

                if self.ply < LOW_PLY_HISTORY_SIZE as i32 {
                    let ply_idx = self.ply as usize;
                    value += 2 * self.low_ply_history.get(ply_idx, m) as i32 / (1 + self.ply);
                }

                self.moves.set_value(i, value);
            }
        }
    }

    // =========================================================================
    // ヘルパー
    // =========================================================================

    /// 良い捕獲手を選択（SEE >= threshold）
    fn select_good_capture(&mut self) -> Option<Move> {
        while self.cur < self.end_cur {
            let ext = self.moves.get(self.cur);
            self.cur += 1;

            // TT手は既に返したのでスキップ
            if ext.mv == self.tt_move {
                continue;
            }

            // SEEで閾値以上の手のみ
            let threshold = Value::new(-ext.value / 18);
            if self.pos.see_ge(ext.mv, threshold) {
                return Some(ext.mv);
            } else {
                // 悪い捕獲手は後回し
                self.moves.swap(self.end_bad_captures, self.cur - 1);
                self.end_bad_captures += 1;
            }
        }
        None
    }

    /// 条件を満たす次の手を選択
    fn select<F>(&mut self, filter: F) -> Option<Move>
    where
        F: Fn(&Self, &ExtMove) -> bool,
    {
        while self.cur < self.end_cur {
            let ext = self.moves.get(self.cur);
            self.cur += 1;

            // TT手は既に返したのでスキップ
            if ext.mv == self.tt_move {
                continue;
            }

            if filter(self, &ext) {
                return Some(ext.mv);
            }
        }
        None
    }
}

// Iteratorトレイトの実装
impl Iterator for MovePicker<'_> {
    type Item = Move;

    fn next(&mut self) -> Option<Self::Item> {
        let m = self.next_move();
        if m == Move::NONE {
            None
        } else {
            Some(m)
        }
    }
}

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

/// 静かな手の良し悪しを分ける閾値（YaneuraOu準拠）
///
/// この閾値より大きいスコアの手はGoodQuietステージで返され、
/// 閾値以下の手はBadQuietステージで返される。
const GOOD_QUIET_THRESHOLD: i32 = -14000;

/// 部分挿入ソート（配列の先頭からend まで）
///
/// `limit` より大きいスコアの手だけを降順でソートする。
/// `limit = i32::MIN`の場合は全要素をソートする（この場合、大きい配列ではPDQSortを使用）。
fn partial_insertion_sort(moves: &mut [ExtMove], end: usize, limit: i32) {
    if end <= 1 {
        return;
    }

    // limit = i32::MIN の場合は全要素ソート
    // 大きい配列では標準ライブラリのPDQSortを使用（O(n log n)）
    if limit == i32::MIN && end > SORT_SWITCH_THRESHOLD {
        let slice = &mut moves[..end];
        // 降順ソート
        slice.sort_unstable_by(|a, b| b.value.cmp(&a.value));
        return;
    }

    // 小さい配列または部分ソートの場合は挿入ソート
    let mut sorted_end = 0;

    for p in 1..end {
        if moves[p].value >= limit {
            let tmp = moves[p];
            moves[p] = moves[sorted_end + 1];
            sorted_end += 1;

            // 挿入位置を探す
            let mut q = sorted_end;
            while q > 0 && moves[q - 1].value < tmp.value {
                moves[q] = moves[q - 1];
                q -= 1;
            }
            moves[q] = tmp;
        }
    }
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
        partial_insertion_sort(&mut moves, len, 100);

        // 100以上の手が先頭にソートされている
        assert_eq!(moves[0].value, 200);
        assert_eq!(moves[1].value, 150);
        assert_eq!(moves[2].value, 100);
    }

    #[test]
    fn test_piece_value() {
        assert_eq!(piece_value(Piece::B_PAWN), 90);
        assert_eq!(piece_value(Piece::W_GOLD), 540);
        assert_eq!(piece_value(Piece::B_ROOK), 990);
        assert_eq!(piece_value(Piece::W_DRAGON), 1224);
    }
}
