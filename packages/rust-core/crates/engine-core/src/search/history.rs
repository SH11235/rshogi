//! History統計
//!
//! 探索中の手の成功/失敗を記録し、手の順序付けに利用する。
//!
//! - `StatsEntry`: 範囲制限付き履歴エントリ
//! - `ButterflyHistory`: [Color][from_to] -> score
//! - `LowPlyHistory`: [ply][from_to] -> score
//! - `CapturePieceToHistory`: [piece][to][captured_pt] -> score
//! - `PieceToHistory`: [piece][to] -> score
//! - `ContinuationHistory`: [prev_pc][prev_to][pc][to] -> score
//! - `PawnHistory`: [pawn_key_idx][piece][to] -> score
//! - `CounterMoveHistory`: [piece][square] -> Move

use crate::types::{Color, Move, Piece, PieceType, Square};

// =============================================================================
// 定数
// =============================================================================

/// PawnHistoryのサイズ（2のべき乗）
pub const PAWN_HISTORY_SIZE: usize = 512;

/// CorrectionHistoryのサイズ（2のべき乗）
pub const CORRECTION_HISTORY_SIZE: usize = 32768;

/// CorrectionHistoryの値の制限
pub const CORRECTION_HISTORY_LIMIT: i32 = 1024;

/// LowPlyHistoryのサイズ（ルート付近のply数）
pub const LOW_PLY_HISTORY_SIZE: usize = 5;

/// from_toインデックスのサイズ
/// 将棋では from = SQUARE_NB + 0..6 で駒打ちを表す
/// (81マス + 7種の駒打ち) × 81マス
pub const FROM_TO_SIZE: usize = (Square::NUM + 7) * Square::NUM;

/// 駒種の数（PieceType::NUM相当）
const PIECE_TYPE_NUM: usize = PieceType::NUM + 1; // None含む

/// 駒の数（Piece::NUM相当、先後含む）
const PIECE_NUM: usize = Piece::NUM; // NONE含む

// =============================================================================
// YaneuraOu準拠定数
// =============================================================================

/// TTMoveHistory更新ボーナス（TT手がbest moveだった場合）
pub const TT_MOVE_HISTORY_BONUS: i32 = 811;

/// TTMoveHistory更新ペナルティ（TT手がbest moveでなかった場合）
pub const TT_MOVE_HISTORY_MALUS: i32 = -848;

/// ContinuationHistory更新の重み [(ply_back, weight)]
///
/// YaneuraOu準拠: 1,2,3,4,5,6手前の指し手と現在の指し手のペアを更新。
/// 王手中は1,2手前のみ更新。
pub const CONTINUATION_HISTORY_WEIGHTS: [(usize, i32); 6] =
    [(1, 1108), (2, 652), (3, 273), (4, 572), (5, 126), (6, 449)];

/// update_quiet_histories用のlowPlyHistory倍率
pub const LOW_PLY_HISTORY_MULTIPLIER: i32 = 771;
pub const LOW_PLY_HISTORY_OFFSET: i32 = 40;

/// update_quiet_histories用のcontinuationHistory倍率（正のボーナス時）
pub const CONTINUATION_HISTORY_POS_MULTIPLIER: i32 = 979;
/// update_quiet_histories用のcontinuationHistory倍率（負のボーナス時）
pub const CONTINUATION_HISTORY_NEG_MULTIPLIER: i32 = 842;

/// update_quiet_histories用のpawnHistory倍率（正のボーナス時）
pub const PAWN_HISTORY_POS_MULTIPLIER: i32 = 704;
/// update_quiet_histories用のpawnHistory倍率（負のボーナス時）
pub const PAWN_HISTORY_NEG_MULTIPLIER: i32 = 439;
/// update_quiet_histories用のpawnHistoryオフセット
pub const PAWN_HISTORY_OFFSET: i32 = 70;

/// ContinuationHistory近接ply（1,2手前）へのオフセット
pub const CONTINUATION_HISTORY_NEAR_PLY_OFFSET: i32 = 80;

// =============================================================================
// StatsEntry
// =============================================================================

/// 履歴統計の1エントリ
///
/// 値の範囲を [-D, D] に制限しながら更新できる。
#[derive(Clone, Copy)]
pub struct StatsEntry<const D: i32> {
    value: i16,
}

impl<const D: i32> Default for StatsEntry<D> {
    fn default() -> Self {
        Self { value: 0 }
    }
}

impl<const D: i32> StatsEntry<D> {
    /// 値を取得
    #[inline]
    pub fn get(&self) -> i16 {
        self.value
    }

    /// 値を設定
    #[inline]
    pub fn set(&mut self, v: i16) {
        self.value = v;
    }

    /// ボーナス値を加算（範囲制限付き）
    ///
    /// 更新式: entry += clamp(bonus, -D, D) - entry * |clamp(bonus, -D, D)| / D
    ///
    /// この式の性質:
    /// - bonus == D のとき、entry が D に収束
    /// - bonus が小さいとき、ほぼそのまま加算
    /// - 値が D を超えないよう自動調整
    /// - 自然にゼロ方向に引っ張られる
    #[inline]
    pub fn update(&mut self, bonus: i32) {
        let clamped = bonus.clamp(-D, D);
        let delta = clamped - (self.value as i32) * clamped.abs() / D;
        self.value = (self.value as i32 + delta) as i16;
        debug_assert!(
            self.value.abs() <= D as i16,
            "StatsEntry out of range: {} (D={})",
            self.value,
            D
        );
    }
}

// =============================================================================
// ButterflyHistory
// =============================================================================

/// ButterflyHistory: [Color][from_to] -> score
///
/// 静かな手（quiet moves）の成功/失敗を記録。
/// 手の移動元と移動先でインデックス。
pub struct ButterflyHistory {
    table: [[StatsEntry<7183>; FROM_TO_SIZE]; Color::NUM],
}

impl ButterflyHistory {
    /// 新しいButterflyHistoryを作成
    pub fn new() -> Self {
        Self {
            table: [[StatsEntry::default(); FROM_TO_SIZE]; Color::NUM],
        }
    }

    /// 値を取得
    #[inline]
    pub fn get(&self, color: Color, mv: Move) -> i16 {
        self.table[color.index()][mv.history_index()].get()
    }

    /// 値を更新
    #[inline]
    pub fn update(&mut self, color: Color, mv: Move, bonus: i32) {
        self.table[color.index()][mv.history_index()].update(bonus);
    }

    /// クリア
    pub fn clear(&mut self) {
        for color_table in &mut self.table {
            for entry in color_table.iter_mut() {
                entry.set(0);
            }
        }
    }
}

impl Default for ButterflyHistory {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// LowPlyHistory
// =============================================================================

/// LowPlyHistory: [ply][from_to] -> score
///
/// ルート付近での手の順序を改善するための履歴。
pub struct LowPlyHistory {
    table: [[StatsEntry<7183>; FROM_TO_SIZE]; LOW_PLY_HISTORY_SIZE],
}

impl LowPlyHistory {
    /// 新しいLowPlyHistoryを作成
    pub fn new() -> Self {
        Self {
            table: [[StatsEntry::default(); FROM_TO_SIZE]; LOW_PLY_HISTORY_SIZE],
        }
    }

    /// 値を取得
    #[inline]
    pub fn get(&self, ply: usize, mv: Move) -> i16 {
        if ply < LOW_PLY_HISTORY_SIZE {
            self.table[ply][mv.history_index()].get()
        } else {
            0
        }
    }

    /// 値を更新
    #[inline]
    pub fn update(&mut self, ply: usize, mv: Move, bonus: i32) {
        if ply < LOW_PLY_HISTORY_SIZE {
            self.table[ply][mv.history_index()].update(bonus);
        }
    }

    /// クリア
    pub fn clear(&mut self) {
        for ply_table in &mut self.table {
            for entry in ply_table.iter_mut() {
                entry.set(0);
            }
        }
    }
}

impl Default for LowPlyHistory {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// CapturePieceToHistory
// =============================================================================

/// CapturePieceToHistory: [piece][to][captured_piece_type] -> score
///
/// 捕獲する手の履歴。
pub struct CapturePieceToHistory {
    table: Box<[[[StatsEntry<10692>; PIECE_TYPE_NUM]; Square::NUM]; PIECE_NUM]>,
}

impl CapturePieceToHistory {
    /// 新しいCapturePieceToHistoryを作成
    pub fn new() -> Self {
        Self {
            table: Box::new([[[StatsEntry::default(); PIECE_TYPE_NUM]; Square::NUM]; PIECE_NUM]),
        }
    }

    /// 値を取得
    #[inline]
    pub fn get(&self, pc: Piece, to: Square, captured_pt: PieceType) -> i16 {
        self.table[pc.index()][to.index()][captured_pt as usize].get()
    }

    /// 値を更新
    #[inline]
    pub fn update(&mut self, pc: Piece, to: Square, captured_pt: PieceType, bonus: i32) {
        self.table[pc.index()][to.index()][captured_pt as usize].update(bonus);
    }

    /// クリア
    pub fn clear(&mut self) {
        for pc_table in self.table.iter_mut() {
            for sq_table in pc_table.iter_mut() {
                for entry in sq_table.iter_mut() {
                    entry.set(0);
                }
            }
        }
    }
}

impl Default for CapturePieceToHistory {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// PieceToHistory
// =============================================================================

/// PieceToHistory: [piece][to] -> score
///
/// 駒と移動先でインデックスする履歴。
#[derive(Clone)]
pub struct PieceToHistory {
    table: [[StatsEntry<30000>; Square::NUM]; PIECE_NUM],
}

impl PieceToHistory {
    /// 新しいPieceToHistoryを作成
    pub fn new() -> Self {
        Self {
            table: [[StatsEntry::default(); Square::NUM]; PIECE_NUM],
        }
    }

    /// 値を取得
    #[inline]
    pub fn get(&self, pc: Piece, to: Square) -> i16 {
        self.table[pc.index()][to.index()].get()
    }

    /// 値を更新
    #[inline]
    pub fn update(&mut self, pc: Piece, to: Square, bonus: i32) {
        self.table[pc.index()][to.index()].update(bonus);
    }

    /// クリア
    pub fn clear(&mut self) {
        for pc_table in &mut self.table {
            for entry in pc_table.iter_mut() {
                entry.set(0);
            }
        }
    }
}

impl Default for PieceToHistory {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// ContinuationHistory
// =============================================================================

/// ContinuationHistory: [prev_piece][prev_to][piece][to] -> score
///
/// 連続する2手の組み合わせ履歴。
/// 1手前の駒と移動先から、現在の駒と移動先へのスコア。
pub struct ContinuationHistory {
    table: Vec<PieceToHistory>,
}

impl ContinuationHistory {
    /// 新しいContinuationHistoryを作成
    pub fn new() -> Self {
        Self {
            table: vec![PieceToHistory::new(); PIECE_NUM * Square::NUM],
        }
    }

    #[inline]
    fn index(prev_pc: Piece, prev_to: Square) -> usize {
        prev_pc.index() * Square::NUM + prev_to.index()
    }

    /// 内部テーブルへの参照を取得
    #[inline]
    pub fn get_table(&self, prev_pc: Piece, prev_to: Square) -> &PieceToHistory {
        &self.table[Self::index(prev_pc, prev_to)]
    }

    /// 内部テーブルへの可変参照を取得
    #[inline]
    pub fn get_table_mut(&mut self, prev_pc: Piece, prev_to: Square) -> &mut PieceToHistory {
        let idx = Self::index(prev_pc, prev_to);
        &mut self.table[idx]
    }

    /// 1手前・2手前などの継続手を更新
    pub fn update_from_prev(
        &mut self,
        prev_pc: Piece,
        prev_to: Square,
        pc: Piece,
        to: Square,
        bonus: i32,
    ) {
        self.get_table_mut(prev_pc, prev_to).update(pc, to, bonus);
    }

    /// 値を取得
    #[inline]
    pub fn get(&self, prev_pc: Piece, prev_to: Square, pc: Piece, to: Square) -> i16 {
        self.get_table(prev_pc, prev_to).get(pc, to)
    }

    /// 値を更新
    #[inline]
    pub fn update(&mut self, prev_pc: Piece, prev_to: Square, pc: Piece, to: Square, bonus: i32) {
        self.get_table_mut(prev_pc, prev_to).update(pc, to, bonus);
    }

    /// クリア
    pub fn clear(&mut self) {
        for entry in self.table.iter_mut() {
            entry.clear();
        }
    }
}

impl Default for ContinuationHistory {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// PawnHistory
// =============================================================================

/// PawnHistory: [pawn_key_index][piece][to] -> score
///
/// 歩の陣形に対する履歴。
pub struct PawnHistory {
    table: Vec<[[StatsEntry<8192>; Square::NUM]; PIECE_NUM]>,
}

impl PawnHistory {
    /// 新しいPawnHistoryを作成
    pub fn new() -> Self {
        let row = [[StatsEntry::default(); Square::NUM]; PIECE_NUM];
        Self {
            table: vec![row; PAWN_HISTORY_SIZE],
        }
    }

    /// 値を取得
    #[inline]
    pub fn get(&self, pawn_key_index: usize, pc: Piece, to: Square) -> i16 {
        self.table[pawn_key_index][pc.index()][to.index()].get()
    }

    /// 値を更新
    #[inline]
    pub fn update(&mut self, pawn_key_index: usize, pc: Piece, to: Square, bonus: i32) {
        self.table[pawn_key_index][pc.index()][to.index()].update(bonus);
    }

    /// クリア
    pub fn clear(&mut self) {
        for pawn_table in self.table.iter_mut() {
            for pc_table in pawn_table.iter_mut() {
                for entry in pc_table.iter_mut() {
                    entry.set(0);
                }
            }
        }
    }
}

impl Default for PawnHistory {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// CounterMoveHistory
// =============================================================================

/// CounterMoveHistory: [piece][square] -> Move
///
/// 直前の相手の手に対するカウンター手。
pub struct CounterMoveHistory {
    table: [[Move; Square::NUM]; PIECE_NUM],
}

impl CounterMoveHistory {
    /// 新しいCounterMoveHistoryを作成
    pub fn new() -> Self {
        Self {
            table: [[Move::NONE; Square::NUM]; PIECE_NUM],
        }
    }

    /// 値を取得
    #[inline]
    pub fn get(&self, pc: Piece, sq: Square) -> Move {
        self.table[pc.index()][sq.index()]
    }

    /// 値を設定
    #[inline]
    pub fn set(&mut self, pc: Piece, sq: Square, mv: Move) {
        self.table[pc.index()][sq.index()] = mv;
    }

    /// クリア
    pub fn clear(&mut self) {
        for pc_table in &mut self.table {
            for entry in pc_table.iter_mut() {
                *entry = Move::NONE;
            }
        }
    }
}

impl Default for CounterMoveHistory {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// CorrectionHistory
// =============================================================================

/// CorrectionHistory (YaneuraOu準拠)
///
/// - Pawn/Minor: [key_index][color] -> correction
/// - NonPawn: [key_index][side_to_move][piece_color] -> correction
/// - Continuation: [prev_pc][prev_to][pc][to] -> correction
pub struct CorrectionHistory {
    pawn: Box<[[StatsEntry<CORRECTION_HISTORY_LIMIT>; Color::NUM]; CORRECTION_HISTORY_SIZE]>,
    minor: Box<[[StatsEntry<CORRECTION_HISTORY_LIMIT>; Color::NUM]; CORRECTION_HISTORY_SIZE]>,
    non_pawn: Box<
        [[[StatsEntry<CORRECTION_HISTORY_LIMIT>; Color::NUM]; Color::NUM]; CORRECTION_HISTORY_SIZE],
    >,
    continuation: Box<
        [[[[StatsEntry<CORRECTION_HISTORY_LIMIT>; Square::NUM]; Piece::NUM]; Square::NUM];
            Piece::NUM],
    >,
}

impl CorrectionHistory {
    /// 新しいCorrectionHistoryを作成（初期値込み）
    pub fn new() -> Self {
        let mut history = Self {
            pawn: Box::new([[StatsEntry::default(); Color::NUM]; CORRECTION_HISTORY_SIZE]),
            minor: Box::new([[StatsEntry::default(); Color::NUM]; CORRECTION_HISTORY_SIZE]),
            non_pawn: Box::new(
                [[[StatsEntry::default(); Color::NUM]; Color::NUM]; CORRECTION_HISTORY_SIZE],
            ),
            continuation: Box::new(
                [[[[StatsEntry::default(); Square::NUM]; Piece::NUM]; Square::NUM]; Piece::NUM],
            ),
        };
        history.fill_initial_values();
        history
    }

    /// 初期値で埋め直す
    pub fn clear(&mut self) {
        self.fill_initial_values();
    }

    fn fill_initial_values(&mut self) {
        for entry in self.pawn.iter_mut().flatten() {
            entry.set(5);
        }
        for entry in self.minor.iter_mut().flatten() {
            entry.set(0);
        }
        for entry in self.non_pawn.iter_mut().flatten().flatten() {
            entry.set(0);
        }
        for prev_pc in self.continuation.iter_mut() {
            for prev_to in prev_pc.iter_mut() {
                for entry in prev_to.iter_mut().flatten() {
                    entry.set(8);
                }
            }
        }
    }

    #[inline]
    pub fn pawn_value(&self, idx: usize, color: Color) -> i16 {
        self.pawn[idx % CORRECTION_HISTORY_SIZE][color.index()].get()
    }

    #[inline]
    pub fn update_pawn(&mut self, idx: usize, color: Color, bonus: i32) {
        self.pawn[idx % CORRECTION_HISTORY_SIZE][color.index()].update(bonus);
    }

    #[inline]
    pub fn minor_value(&self, idx: usize, color: Color) -> i16 {
        self.minor[idx % CORRECTION_HISTORY_SIZE][color.index()].get()
    }

    #[inline]
    pub fn update_minor(&mut self, idx: usize, color: Color, bonus: i32) {
        self.minor[idx % CORRECTION_HISTORY_SIZE][color.index()].update(bonus);
    }

    #[inline]
    pub fn non_pawn_value(&self, idx: usize, board_color: Color, stm: Color) -> i16 {
        self.non_pawn[idx % CORRECTION_HISTORY_SIZE][board_color.index()][stm.index()].get()
    }

    #[inline]
    pub fn update_non_pawn(&mut self, idx: usize, board_color: Color, stm: Color, bonus: i32) {
        self.non_pawn[idx % CORRECTION_HISTORY_SIZE][board_color.index()][stm.index()]
            .update(bonus);
    }

    #[inline]
    pub fn continuation_value(
        &self,
        prev_pc: Piece,
        prev_to: Square,
        pc: Piece,
        to: Square,
    ) -> i16 {
        self.continuation[prev_pc.index()][prev_to.index()][pc.index()][to.index()].get()
    }

    #[inline]
    pub fn update_continuation(
        &mut self,
        prev_pc: Piece,
        prev_to: Square,
        pc: Piece,
        to: Square,
        bonus: i32,
    ) {
        self.continuation[prev_pc.index()][prev_to.index()][pc.index()][to.index()].update(bonus);
    }
}

impl Default for CorrectionHistory {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// ボーナス計算（YaneuraOu準拠）
// =============================================================================

/// History更新用のボーナスを計算
///
/// YaneuraOu準拠: `min(170*depth-87, 1598) + 332*(bestMove == ttMove)`
/// - `is_tt_move`: bestMoveがTT手と一致する場合はtrue
#[inline]
pub fn stat_bonus(depth: i32, is_tt_move: bool) -> i32 {
    let base = (170 * depth - 87).min(1598);
    if is_tt_move {
        base + 332
    } else {
        base
    }
}

/// Quiet手用のマイナスボーナス（ペナルティ）を計算
///
/// YaneuraOu準拠: `min(743*depth-180, 2287) - 33*quietsSearched.size()`
#[inline]
pub fn quiet_malus(depth: i32, quiets_count: usize) -> i32 {
    let base = (743 * depth - 180).min(2287);
    base - 33 * quiets_count as i32
}

/// 捕獲手用のマイナスボーナス（ペナルティ）を計算
///
/// YaneuraOu準拠: `min(708*depth-148, 2287) - 29*capturesSearched.size()`
#[inline]
pub fn capture_malus(depth: i32, captures_count: usize) -> i32 {
    let base = (708 * depth - 148).min(2287);
    base - 29 * captures_count as i32
}

// 後方互換性のため旧APIも残す（非推奨）
/// History更新用のボーナスを計算（旧API、非推奨）
#[deprecated(note = "Use stat_bonus(depth, is_tt_move) instead")]
#[inline]
pub fn stat_bonus_old(depth: i32) -> i32 {
    (130 * depth - 103).min(1652)
}

/// マイナスボーナス（ペナルティ）を計算（旧API、非推奨）
#[deprecated(note = "Use quiet_malus(depth, quiets_count) instead")]
#[inline]
pub fn stat_malus(depth: i32) -> i32 {
    (303 * depth - 273).min(1352)
}

// =============================================================================
// YaneuraOu準拠: 更新ヘルパー関数
// =============================================================================

/// LowPlyHistory用のボーナスを計算（YaneuraOu準拠）
#[inline]
pub fn low_ply_history_bonus(bonus: i32) -> i32 {
    bonus * LOW_PLY_HISTORY_MULTIPLIER / 1024 + LOW_PLY_HISTORY_OFFSET
}

/// ContinuationHistory用のボーナスを計算（YaneuraOu準拠）
///
/// 正のボーナスと負のボーナスで倍率が異なる。
#[inline]
pub fn continuation_history_bonus(bonus: i32) -> i32 {
    if bonus > 0 {
        bonus * CONTINUATION_HISTORY_POS_MULTIPLIER / 1024
    } else {
        bonus * CONTINUATION_HISTORY_NEG_MULTIPLIER / 1024
    }
}

/// ContinuationHistory近接ply（1,2手前）用のオフセット込みボーナスを計算
///
/// YaneuraOu準拠: オフセットは常に加算（負のボーナス時もペナルティを緩める）
/// `(bonus * weight / 1024) + 80 * (i < 2)`
#[inline]
pub fn continuation_history_bonus_with_offset(bonus: i32, ply_back: usize) -> i32 {
    let base = continuation_history_bonus(bonus);
    if ply_back <= 2 {
        // YaneuraOu: 負のボーナスでも+80を加算してペナルティを緩める
        base + CONTINUATION_HISTORY_NEAR_PLY_OFFSET
    } else {
        base
    }
}

/// PawnHistory用のボーナスを計算（YaneuraOu準拠）
///
/// YaneuraOu準拠: オフセットは常に加算（負のボーナス時もペナルティを緩める）
/// `(bonus * (bonus > 0 ? 704 : 439) / 1024) + 70`
#[inline]
pub fn pawn_history_bonus(bonus: i32) -> i32 {
    if bonus > 0 {
        bonus * PAWN_HISTORY_POS_MULTIPLIER / 1024 + PAWN_HISTORY_OFFSET
    } else {
        // YaneuraOu: 負のボーナスでも+70を加算してペナルティを緩める
        bonus * PAWN_HISTORY_NEG_MULTIPLIER / 1024 + PAWN_HISTORY_OFFSET
    }
}

// =============================================================================
// テスト
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stats_entry_default() {
        let entry = StatsEntry::<1000>::default();
        assert_eq!(entry.get(), 0);
    }

    #[test]
    fn test_stats_entry_update_positive() {
        let mut entry = StatsEntry::<1000>::default();

        // ボーナスを加算
        entry.update(100);
        assert!(entry.get() > 0);
        assert!(entry.get() <= 1000);
    }

    #[test]
    fn test_stats_entry_update_convergence() {
        let mut entry = StatsEntry::<1000>::default();

        // 繰り返し更新してもDを超えない
        for _ in 0..100 {
            entry.update(1000);
        }
        assert!(entry.get() <= 1000);
        assert!(entry.get() > 900); // 収束に近づく
    }

    #[test]
    fn test_stats_entry_update_negative() {
        let mut entry = StatsEntry::<1000>::default();

        // マイナス方向
        for _ in 0..100 {
            entry.update(-1000);
        }
        assert!(entry.get() >= -1000);
        assert!(entry.get() < -900); // 収束に近づく
    }

    #[test]
    fn test_stats_entry_decay() {
        let mut entry = StatsEntry::<1000>::default();

        // 大きなボーナスで値を上げる
        for _ in 0..50 {
            entry.update(1000);
        }
        let high_value = entry.get();
        assert!(high_value > 0, "値が上がっているべき");

        // マイナスボーナスで徐々に減衰
        for _ in 0..5 {
            entry.update(-100);
        }
        let decayed_value = entry.get();

        // 減衰していることを確認（完全に0になる可能性もある）
        assert!(decayed_value < high_value, "減衰しているべき");
    }

    #[test]
    fn test_butterfly_history() {
        let mut history = ButterflyHistory::new();
        let mv = Move::from_usi("7g7f").unwrap();

        assert_eq!(history.get(Color::Black, mv), 0);

        history.update(Color::Black, mv, 100);
        assert!(history.get(Color::Black, mv) > 0);
        assert_eq!(history.get(Color::White, mv), 0); // 別の色は影響なし
    }

    #[test]
    fn test_low_ply_history() {
        let mut history = LowPlyHistory::new();
        let mv = Move::from_usi("7g7f").unwrap();

        history.update(0, mv, 100);
        assert!(history.get(0, mv) > 0);
        assert_eq!(history.get(1, mv), 0);

        // 範囲外のplyは0を返す
        assert_eq!(history.get(LOW_PLY_HISTORY_SIZE, mv), 0);
    }

    #[test]
    fn test_counter_move_history() {
        let mut history = CounterMoveHistory::new();
        let mv = Move::from_usi("7g7f").unwrap();
        let pc = Piece::B_PAWN;
        let sq = Square::SQ_55;

        assert!(history.get(pc, sq).is_none());

        history.set(pc, sq, mv);
        assert_eq!(history.get(pc, sq), mv);
    }

    #[test]
    fn test_stat_bonus() {
        // YaneuraOu準拠: min(170*depth-87, 1598) + 332*(is_tt_move)
        // depth=1, is_tt_move=false: 170*1-87 = 83
        assert_eq!(stat_bonus(1, false), 83);
        // depth=1, is_tt_move=true: 83 + 332 = 415
        assert_eq!(stat_bonus(1, true), 415);
        // depth=20: min(170*20-87, 1598) = min(3313, 1598) = 1598
        assert_eq!(stat_bonus(20, false), 1598);
        assert_eq!(stat_bonus(20, true), 1598 + 332);
    }

    #[test]
    fn test_quiet_malus() {
        // YaneuraOu準拠: min(743*depth-180, 2287) - 33*quiets_count
        // depth=1, quiets_count=0: 743*1-180 = 563
        assert_eq!(quiet_malus(1, 0), 563);
        // depth=1, quiets_count=10: 563 - 33*10 = 563 - 330 = 233
        assert_eq!(quiet_malus(1, 10), 233);
        // depth=10: min(743*10-180, 2287) = min(7250, 2287) = 2287
        assert_eq!(quiet_malus(10, 0), 2287);
    }

    #[test]
    fn test_capture_malus() {
        // YaneuraOu準拠: min(708*depth-148, 2287) - 29*captures_count
        // depth=1, captures_count=0: 708*1-148 = 560
        assert_eq!(capture_malus(1, 0), 560);
        // depth=1, captures_count=5: 560 - 29*5 = 560 - 145 = 415
        assert_eq!(capture_malus(1, 5), 415);
        // depth=10: min(708*10-148, 2287) = min(6932, 2287) = 2287
        assert_eq!(capture_malus(10, 0), 2287);
    }
}
