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
//! - `HistoryCell`: 内部可変性ラッパー（参照リークを型で封じる）

use std::cell::UnsafeCell;
use std::marker::PhantomData;
use std::rc::Rc;

use crate::types::{Color, Move, Piece, PieceType, Square};

use super::tt_history::TTMoveHistory;
use super::tune_params::SearchTuneParams;

// =============================================================================
// 定数
// =============================================================================

/// PawnHistoryのサイズ（2のべき乗）
pub const PAWN_HISTORY_SIZE: usize = 8192;

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
    [(1, 1157), (2, 648), (3, 288), (4, 576), (5, 140), (6, 441)];

/// update_quiet_histories用のlowPlyHistory倍率
pub const LOW_PLY_HISTORY_MULTIPLIER: i32 = 761;
pub const LOW_PLY_HISTORY_OFFSET: i32 = 0;

/// update_quiet_histories用のcontinuationHistory倍率（正負共通）
pub const CONTINUATION_HISTORY_MULTIPLIER: i32 = 955;

/// update_quiet_histories用のpawnHistory倍率（正のボーナス時）
pub const PAWN_HISTORY_POS_MULTIPLIER: i32 = 850;
/// update_quiet_histories用のpawnHistory倍率（負のボーナス時）
pub const PAWN_HISTORY_NEG_MULTIPLIER: i32 = 550;

/// ContinuationHistory近接ply（1,2手前）へのオフセット
/// YaneuraOu: update_continuation_histories で (bonus * weight / 1024) + 88 * (i < 2)
pub const CONTINUATION_HISTORY_NEAR_PLY_OFFSET: i32 = 88;

/// Prior Capture Countermove Bonus（fail low時の前の捕獲手へのボーナス）
/// YaneuraOu: yaneuraou-search.cpp:3976
pub const PRIOR_CAPTURE_COUNTERMOVE_BONUS: i32 = 964;

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
///
/// PERF: 約3.5MB。HistoryTables内の連続領域に配置するため配列で保持する。
pub struct CapturePieceToHistory {
    table: [[[StatsEntry<10692>; PIECE_TYPE_NUM]; Square::NUM]; PIECE_NUM],
}

impl CapturePieceToHistory {
    /// 新しいCapturePieceToHistoryを作成
    pub fn new() -> Self {
        Self {
            table: [[[StatsEntry::default(); PIECE_TYPE_NUM]; Square::NUM]; PIECE_NUM],
        }
    }

    /// 新しいCapturePieceToHistoryを作成（ヒープ確保）
    ///
    /// 大きな配列のスタック確保を避けるために `Box::new_zeroed` を使う。
    pub fn new_boxed() -> Box<Self> {
        // SAFETY: StatsEntryはi16のみで構成され、ゼロ初期化は常に有効。
        unsafe { Box::<Self>::new_zeroed().assume_init() }
    }

    /// YaneuraOu の `type_of(capturedPiece)` 相当。
    ///
    /// 捕獲された駒そのものを渡す版。下位4bit（0=NONE, 1..14=PieceType）を
    /// インデックスに使うため、呼び出し側で `piece_type()` を取らなくてよい。
    /// 捕獲がない場合（captured = NONE）は index=0 を使う。
    #[inline]
    pub fn get_with_captured_piece(&self, pc: Piece, to: Square, captured: Piece) -> i16 {
        let captured_idx = (captured.raw() & 0x0F) as usize;
        debug_assert!(
            captured_idx < PIECE_TYPE_NUM,
            "captured_idx {} out of bounds (PIECE_TYPE_NUM = {})",
            captured_idx,
            PIECE_TYPE_NUM
        );
        self.table[pc.index()][to.index()][captured_idx].get()
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
///
/// PERF: 約1.3MBのサイズがあり、SearchWorkerでは[2][2]で4つ保持（計約5.2MB）。
/// HistoryTables内の連続領域に配置するため配列で保持する。
pub struct ContinuationHistory {
    table: [[PieceToHistory; Square::NUM]; PIECE_NUM],
}

impl ContinuationHistory {
    /// 新しいContinuationHistoryを作成
    pub fn new() -> Self {
        let table = std::array::from_fn(|_| std::array::from_fn(|_| PieceToHistory::new()));
        Self { table }
    }

    #[inline]
    pub fn get_table(&self, prev_pc: Piece, prev_to: Square) -> &PieceToHistory {
        &self.table[prev_pc.index()][prev_to.index()]
    }

    /// 内部テーブルへの可変参照を取得
    #[inline]
    pub fn get_table_mut(&mut self, prev_pc: Piece, prev_to: Square) -> &mut PieceToHistory {
        &mut self.table[prev_pc.index()][prev_to.index()]
    }

    /// 新しいContinuationHistoryを作成（ヒープ確保）
    ///
    /// 大きな配列のスタック確保を避けるために `Box::new_zeroed` を使う。
    pub fn new_boxed() -> Box<Self> {
        // SAFETY: PieceToHistoryはStatsEntry(i16)のみで構成され、ゼロ初期化は有効。
        unsafe { Box::<Self>::new_zeroed().assume_init() }
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
        for row in self.table.iter_mut() {
            for entry in row.iter_mut() {
                entry.clear();
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
///
/// PERF: 約39MB。HistoryTables内の連続領域に配置するため配列で保持する。
pub struct PawnHistory {
    table: [[[StatsEntry<8192>; Square::NUM]; PIECE_NUM]; PAWN_HISTORY_SIZE],
}

impl PawnHistory {
    /// 新しいPawnHistoryを作成
    pub fn new() -> Self {
        let row = [[StatsEntry::default(); Square::NUM]; PIECE_NUM];
        Self {
            table: [row; PAWN_HISTORY_SIZE],
        }
    }

    /// 新しいPawnHistoryを作成（ヒープ確保）
    ///
    /// 大きな配列のスタック確保を避けるために `Box::new_zeroed` を使う。
    pub fn new_boxed() -> Box<Self> {
        // SAFETY: StatsEntryはi16のみで構成され、ゼロ初期化は常に有効。
        unsafe { Box::<Self>::new_zeroed().assume_init() }
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
///
/// PERF: 約4.5MB。HistoryTables内の連続領域に配置するため配列で保持する。
pub struct CorrectionHistory {
    pawn: [[StatsEntry<CORRECTION_HISTORY_LIMIT>; Color::NUM]; CORRECTION_HISTORY_SIZE],
    minor: [[StatsEntry<CORRECTION_HISTORY_LIMIT>; Color::NUM]; CORRECTION_HISTORY_SIZE],
    non_pawn:
        [[[StatsEntry<CORRECTION_HISTORY_LIMIT>; Color::NUM]; Color::NUM]; CORRECTION_HISTORY_SIZE],
    continuation: [[[[StatsEntry<CORRECTION_HISTORY_LIMIT>; Square::NUM]; Piece::NUM]; Square::NUM];
        Piece::NUM],
}

impl CorrectionHistory {
    /// 新しいCorrectionHistoryを作成（初期値込み）
    pub fn new() -> Self {
        let mut history = Self {
            pawn: [[StatsEntry::default(); Color::NUM]; CORRECTION_HISTORY_SIZE],
            minor: [[StatsEntry::default(); Color::NUM]; CORRECTION_HISTORY_SIZE],
            non_pawn: [[[StatsEntry::default(); Color::NUM]; Color::NUM]; CORRECTION_HISTORY_SIZE],
            continuation: [[[[StatsEntry::default(); Square::NUM]; Piece::NUM]; Square::NUM];
                Piece::NUM],
        };
        history.fill_initial_values();
        history
    }

    /// 新しいCorrectionHistoryを作成（ヒープ確保）
    ///
    /// 大きな配列のスタック確保を避けるために `Box::new_zeroed` を使う。
    pub fn new_boxed() -> Box<Self> {
        // SAFETY: StatsEntryはi16のみで構成され、ゼロ初期化は常に有効。
        let mut history = unsafe { Box::<Self>::new_zeroed().assume_init() };
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
// HistoryTables
// =============================================================================

/// 履歴/統計テーブルをまとめて保持するコンテナ
///
/// 大きな配列を単一のヒープ領域に配置し、MovePickerの参照経路を短縮する。
pub struct HistoryTables {
    pub main_history: ButterflyHistory,
    pub low_ply_history: LowPlyHistory,
    pub capture_history: CapturePieceToHistory,
    pub continuation_history: [[ContinuationHistory; 2]; 2],
    pub pawn_history: PawnHistory,
    pub correction_history: CorrectionHistory,
    pub tt_move_history: TTMoveHistory,
}

impl HistoryTables {
    /// 新しいHistoryTablesを作成（ヒープ確保）
    ///
    /// `Box::new_zeroed` で一括確保し、CorrectionHistoryのみ初期値を設定する。
    pub fn new_boxed() -> Box<Self> {
        // SAFETY: 各テーブルは数値型のみで構成され、ゼロ初期化は常に有効。
        let mut history = unsafe { Box::<Self>::new_zeroed().assume_init() };
        history.correction_history.fill_initial_values();
        history
    }

    /// すべての履歴テーブルをクリア
    pub fn clear(&mut self) {
        self.main_history.clear();
        self.low_ply_history.clear();
        self.capture_history.clear();
        for row in &mut self.continuation_history {
            for ch in row {
                ch.clear();
            }
        }
        self.pawn_history.clear();
        self.correction_history.clear();
        self.tt_move_history.clear();
    }
}

// =============================================================================
// HistoryCell（内部可変性ラッパー）
// =============================================================================

/// History用の内部可変ラッパー
///
/// 参照を外部に漏らさないために `with_read`/`with_write` を使用する。
/// これにより「束縛禁止」を型レベルで強制できる。
///
/// ## 安全性
///
/// - 参照はクロージャ内に閉じ込められ、外部に漏れない
/// - `PhantomData<Rc<()>>` により `!Send` を強制（単一スレッド保証）
/// - 再帰呼び出し時の参照エイリアス問題を設計で防止
///
/// ## 使用パターン
///
/// ```ignore
/// let mv = ctx.history.with_read(|h| mp.next_move(pos, h));
/// // この時点で &HistoryTables は存在しない（型で保証）
/// let value = search_node(st, ctx, pos, ...);
/// ```
pub struct HistoryCell {
    inner: UnsafeCell<HistoryTables>,
    /// `!Send` を強制するためのマーカー（単一スレッド保証）
    _marker: PhantomData<Rc<()>>,
}

impl HistoryCell {
    /// 新しいHistoryCellを作成
    pub fn new(history: HistoryTables) -> Self {
        Self {
            inner: UnsafeCell::new(history),
            _marker: PhantomData,
        }
    }

    /// Boxから作成（大きな配列のスタック確保を避ける）
    ///
    /// # Safety
    ///
    /// ゼロ初期化されたメモリでHistoryCellを構築する。
    /// 内部のHistoryTablesはゼロ初期化が有効な型のみで構成されている。
    pub fn new_boxed() -> Box<Self> {
        // SAFETY: HistoryCellはUnsafeCell<HistoryTables>とPhantomDataで構成
        // UnsafeCell<T>はTと同じレイアウトを持ち、HistoryTablesはゼロ初期化が有効
        // PhantomDataはサイズ0のマーカー型でゼロ初期化は無害
        // new_zeroedで直接ヒープに確保し、スタックオーバーフローを回避
        let cell = unsafe { Box::<Self>::new_zeroed().assume_init() };
        // correction_historyは初期値が0ではないので再初期化が必要
        cell.with_write(|h| h.correction_history.fill_initial_values());
        cell
    }

    /// 読み取りアクセス（クロージャ内に参照を閉じ込める）
    ///
    /// 参照はクロージャ内でのみ有効。外部に漏れないため、
    /// 再帰呼び出し時に親の参照が残存する問題を型で防止できる。
    ///
    /// # Safety
    ///
    /// このメソッドは内部でunsafeを使用するが、以下の不変条件により安全：
    /// - 参照はクロージャ内に閉じ込められ、外部に漏れない
    /// - 設計上、`with_read`中に`with_write`を呼ばない
    #[inline]
    pub fn with_read<R>(&self, f: impl FnOnce(&HistoryTables) -> R) -> R {
        // SAFETY: 参照はクロージャ内に閉じる（型で保証）
        // MovePickerループ内でのみ使用し、再帰呼び出し時には参照は存在しない
        unsafe { f(&*self.inner.get()) }
    }

    /// 書き込みアクセス（クロージャ内に参照を閉じ込める）
    ///
    /// # Safety
    ///
    /// このメソッドは内部でunsafeを使用するが、以下の不変条件により安全：
    /// - MovePickerループ外でのみ呼ぶ（設計で保証）
    /// - 参照はクロージャ内に閉じ込められる
    #[inline]
    pub fn with_write<R>(&self, f: impl FnOnce(&mut HistoryTables) -> R) -> R {
        // SAFETY: MovePickerループ外でのみ呼ぶ（設計で保証）
        // ノード末尾でHistory更新時に使用
        unsafe { f(&mut *self.inner.get()) }
    }

    /// 内部の HistoryTables への可変参照を取得（初期化用）
    ///
    /// # Safety
    ///
    /// 探索中は使用しないこと。初期化やクリア時のみ使用。
    #[inline]
    pub fn get_mut(&mut self) -> &mut HistoryTables {
        self.inner.get_mut()
    }

    /// すべての履歴テーブルをクリア
    pub fn clear(&mut self) {
        self.inner.get_mut().clear();
    }
}

// =============================================================================
// ボーナス計算（YaneuraOu準拠）
// =============================================================================

/// History更新用のボーナスを計算
///
/// YaneuraOu準拠: `min(121*depth-77, 1633) + 375*(bestMove == ttMove)`
/// - `is_tt_move`: bestMoveがTT手と一致する場合はtrue
#[inline]
pub fn stat_bonus(depth: i32, is_tt_move: bool, tune_params: &SearchTuneParams) -> i32 {
    let base = (tune_params.stat_bonus_depth_mult * depth + tune_params.stat_bonus_offset)
        .min(tune_params.stat_bonus_max);
    if is_tt_move {
        base + tune_params.stat_bonus_tt_bonus
    } else {
        base
    }
}

/// マイナスボーナス（ペナルティ）を計算
///
/// YaneuraOu準拠: `min(825*depth-196, 2159) - 16*moveCount`
/// quiet/capture 共通で使用。
#[inline]
pub fn stat_malus(depth: i32, move_count: i32, tune_params: &SearchTuneParams) -> i32 {
    (tune_params.stat_malus_depth_mult * depth + tune_params.stat_malus_offset)
        .min(tune_params.stat_malus_max)
        - tune_params.stat_malus_move_count_mult * move_count
}

// 後方互換性のため旧APIも残す（非推奨）
/// History更新用のボーナスを計算（旧API、非推奨）
#[deprecated(note = "Use stat_bonus(depth, is_tt_move, tune_params) instead")]
#[inline]
pub fn stat_bonus_old(depth: i32) -> i32 {
    (130 * depth - 103).min(1652)
}

// =============================================================================
// YaneuraOu準拠: 更新ヘルパー関数
// =============================================================================

/// LowPlyHistory用のボーナスを計算（YaneuraOu準拠）
#[inline]
pub fn low_ply_history_bonus(bonus: i32, tune_params: &SearchTuneParams) -> i32 {
    bonus * tune_params.low_ply_history_multiplier / 1024 + tune_params.low_ply_history_offset
}

/// ContinuationHistory用のボーナスを計算（YaneuraOu準拠）
///
/// 正負共通の倍率を使用。
#[inline]
pub fn continuation_history_bonus(bonus: i32, tune_params: &SearchTuneParams) -> i32 {
    bonus * tune_params.continuation_history_multiplier / 1024
}

/// ContinuationHistory更新重みを取得する。
///
/// `ply_back` は 1..=6 を想定する。範囲外は0を返す。
#[inline]
pub fn continuation_history_weight(tune_params: &SearchTuneParams, ply_back: usize) -> i32 {
    match ply_back {
        1 => tune_params.continuation_history_weight_1,
        2 => tune_params.continuation_history_weight_2,
        3 => tune_params.continuation_history_weight_3,
        4 => tune_params.continuation_history_weight_4,
        5 => tune_params.continuation_history_weight_5,
        6 => tune_params.continuation_history_weight_6,
        _ => 0,
    }
}

/// ContinuationHistory近接ply（1,2手前）用のオフセット込みボーナスを計算
///
/// YaneuraOu準拠: オフセットは常に加算（負のボーナス時もペナルティを緩める）
/// `(bonus * weight / 1024) + near_ply_offset * (i < 2)`
#[inline]
pub fn continuation_history_bonus_with_offset(
    bonus: i32,
    ply_back: usize,
    tune_params: &SearchTuneParams,
) -> i32 {
    let base = continuation_history_bonus(bonus, tune_params);
    if ply_back < 2 {
        // YaneuraOu準拠: 88 * (i < 2) → offset=1のみ近接plyオフセットを加算
        base + tune_params.continuation_history_near_ply_offset
    } else {
        base
    }
}

/// PawnHistory用のボーナスを計算（YaneuraOu準拠）
///
/// `bonus * (bonus > 0 ? 850 : 550) / 1024`
#[inline]
pub fn pawn_history_bonus(bonus: i32, tune_params: &SearchTuneParams) -> i32 {
    if bonus > 0 {
        bonus * tune_params.pawn_history_pos_multiplier / 1024
    } else {
        bonus * tune_params.pawn_history_neg_multiplier / 1024
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
        let tune = SearchTuneParams::default();
        // YaneuraOu準拠: min(121*depth-77, 1633) + 375*(is_tt_move)
        // depth=1, is_tt_move=false: 121*1-77 = 44
        assert_eq!(stat_bonus(1, false, &tune), 44);
        // depth=1, is_tt_move=true: 44 + 375 = 419
        assert_eq!(stat_bonus(1, true, &tune), 419);
        // depth=20: min(121*20-77, 1633) = min(2343, 1633) = 1633
        assert_eq!(stat_bonus(20, false, &tune), 1633);
        assert_eq!(stat_bonus(20, true, &tune), 1633 + 375);
    }

    #[test]
    fn test_stat_malus() {
        let tune = SearchTuneParams::default();
        // YaneuraOu準拠: min(825*depth-196, 2159) - 16*moveCount
        // depth=1, moveCount=0: 825*1-196 = 629
        assert_eq!(stat_malus(1, 0, &tune), 629);
        // depth=1, moveCount=10: 629 - 16*10 = 469
        assert_eq!(stat_malus(1, 10, &tune), 469);
        // depth=10, moveCount=0: min(825*10-196, 2159) = min(8054, 2159) = 2159
        assert_eq!(stat_malus(10, 0, &tune), 2159);
    }

    #[test]
    fn test_capture_piece_to_history_with_captured_piece() {
        let mut history = CapturePieceToHistory::new_boxed();
        let pc = Piece::B_GOLD;
        let to = Square::SQ_55;
        let captured = Piece::W_SILVER;

        // 初期値は0
        assert_eq!(history.get_with_captured_piece(pc, to, captured), 0);

        // 更新後、get/with_captured_pieceが同じ値を返す
        history.update(pc, to, captured.piece_type(), 100);
        assert_eq!(
            history.get_with_captured_piece(pc, to, captured),
            history.get(pc, to, captured.piece_type())
        );

        // NONEの場合はindex=0を使う
        let captured_none = Piece::NONE;
        assert_eq!(history.get_with_captured_piece(pc, to, captured_none), 0);
    }

    #[test]
    fn test_history_cell_with_read() {
        // HistoryTablesは大きいのでnew_boxedを使用
        let cell = HistoryCell::new_boxed();

        // with_readでクロージャ内で参照を使用
        let value =
            cell.with_read(|h| h.main_history.get(Color::Black, Move::from_usi("7g7f").unwrap()));
        assert_eq!(value, 0);
    }

    #[test]
    fn test_history_cell_with_write() {
        // HistoryTablesは大きいのでnew_boxedを使用
        let cell = HistoryCell::new_boxed();
        let mv = Move::from_usi("7g7f").unwrap();

        // with_writeで更新
        cell.with_write(|h| {
            h.main_history.update(Color::Black, mv, 100);
        });

        // 更新が反映されていることを確認
        let value = cell.with_read(|h| h.main_history.get(Color::Black, mv));
        assert!(value > 0);
    }

    #[test]
    fn test_history_cell_clear() {
        // HistoryTablesは大きいのでnew_boxedを使用
        let mut cell = HistoryCell::new_boxed();
        let mv = Move::from_usi("7g7f").unwrap();

        // 更新
        cell.with_write(|h| {
            h.main_history.update(Color::Black, mv, 100);
        });

        // クリア
        cell.clear();

        // クリア後は0に戻る
        let value = cell.with_read(|h| h.main_history.get(Color::Black, mv));
        assert_eq!(value, 0);
    }
}
