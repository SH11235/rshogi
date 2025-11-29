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
const PIECE_TYPE_NUM: usize = 15; // None含む

/// 駒の数（Piece::NUM相当、先後含む）
const PIECE_NUM: usize = 31; // None含む

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
    table: Box<[[PieceToHistory; Square::NUM]; PIECE_NUM]>,
}

impl ContinuationHistory {
    /// 新しいContinuationHistoryを作成
    pub fn new() -> Self {
        Self {
            table: Box::new(std::array::from_fn(|_| {
                std::array::from_fn(|_| PieceToHistory::new())
            })),
        }
    }

    /// 内部テーブルへの参照を取得
    #[inline]
    pub fn get_table(&self, prev_pc: Piece, prev_to: Square) -> &PieceToHistory {
        &self.table[prev_pc.index()][prev_to.index()]
    }

    /// 内部テーブルへの可変参照を取得
    #[inline]
    pub fn get_table_mut(&mut self, prev_pc: Piece, prev_to: Square) -> &mut PieceToHistory {
        &mut self.table[prev_pc.index()][prev_to.index()]
    }

    /// 値を取得
    #[inline]
    pub fn get(&self, prev_pc: Piece, prev_to: Square, pc: Piece, to: Square) -> i16 {
        self.table[prev_pc.index()][prev_to.index()].get(pc, to)
    }

    /// 値を更新
    #[inline]
    pub fn update(&mut self, prev_pc: Piece, prev_to: Square, pc: Piece, to: Square, bonus: i32) {
        self.table[prev_pc.index()][prev_to.index()].update(pc, to, bonus);
    }

    /// クリア
    pub fn clear(&mut self) {
        for pc_table in self.table.iter_mut() {
            for sq_table in pc_table.iter_mut() {
                sq_table.clear();
            }
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
    table: Box<[[[StatsEntry<8192>; Square::NUM]; PIECE_NUM]; PAWN_HISTORY_SIZE]>,
}

impl PawnHistory {
    /// 新しいPawnHistoryを作成
    pub fn new() -> Self {
        Self {
            table: Box::new([[[StatsEntry::default(); Square::NUM]; PIECE_NUM]; PAWN_HISTORY_SIZE]),
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
// ボーナス計算
// =============================================================================

/// History更新用のボーナスを計算
///
/// YaneuraOu準拠の計算式。
#[inline]
pub fn stat_bonus(depth: i32) -> i32 {
    (130 * depth - 103).min(1652)
}

/// マイナスボーナス（ペナルティ）を計算
#[inline]
pub fn stat_malus(depth: i32) -> i32 {
    (303 * depth - 273).min(1352)
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
        assert_eq!(stat_bonus(1), 130 - 103);
        assert!(stat_bonus(20) <= 1652);
    }

    #[test]
    fn test_stat_malus() {
        assert_eq!(stat_malus(1), 303 - 273);
        assert!(stat_malus(20) <= 1352);
    }
}
