//! Shared state for parallel search
//!
//! Lock-free data structures shared between search threads

use crate::{
    shogi::{Move, PieceType, Square},
    Color,
};
use crossbeam_utils::CachePadded;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;

/// Statistics for tracking work duplication in parallel search
#[derive(Debug)]
pub struct DuplicationStats {
    /// Total nodes searched by all threads
    pub total_nodes: AtomicU64,
    /// Unique nodes (positions not previously searched)
    pub unique_nodes: AtomicU64,
    /// Transposition table hits
    pub tt_hits: AtomicU64,
    /// Duplicated positions (searched by multiple threads)
    pub duplicated_positions: AtomicU64,
}

impl DuplicationStats {
    /// Create new duplication statistics tracker
    pub fn new() -> Self {
        Self {
            total_nodes: AtomicU64::new(0),
            unique_nodes: AtomicU64::new(0),
            tt_hits: AtomicU64::new(0),
            duplicated_positions: AtomicU64::new(0),
        }
    }

    /// Reset all statistics
    pub fn reset(&self) {
        self.total_nodes.store(0, Ordering::Relaxed);
        self.unique_nodes.store(0, Ordering::Relaxed);
        self.tt_hits.store(0, Ordering::Relaxed);
        self.duplicated_positions.store(0, Ordering::Relaxed);
    }

    /// Calculate duplication percentage
    pub fn duplication_percentage(&self) -> f64 {
        let total = self.total_nodes.load(Ordering::Relaxed);
        let unique = self.unique_nodes.load(Ordering::Relaxed);
        if total > 0 {
            ((total - unique) as f64 / total as f64) * 100.0
        } else {
            0.0
        }
    }

    /// Get effective nodes (unique nodes explored)
    pub fn effective_nodes(&self) -> u64 {
        self.unique_nodes.load(Ordering::Relaxed)
    }

    /// Get TT hit rate
    pub fn tt_hit_rate(&self) -> f64 {
        let total = self.total_nodes.load(Ordering::Relaxed);
        let hits = self.tt_hits.load(Ordering::Relaxed);
        if total > 0 {
            (hits as f64 / total as f64) * 100.0
        } else {
            0.0
        }
    }

    /// Add node to statistics
    pub fn add_node(&self, is_tt_hit: bool, is_duplicate: bool) {
        self.total_nodes.fetch_add(1, Ordering::Relaxed);
        if is_tt_hit {
            self.tt_hits.fetch_add(1, Ordering::Relaxed);
        }
        if is_duplicate {
            self.duplicated_positions.fetch_add(1, Ordering::Relaxed);
        } else {
            self.unique_nodes.fetch_add(1, Ordering::Relaxed);
        }
    }
}

impl Default for DuplicationStats {
    fn default() -> Self {
        Self::new()
    }
}

// Architecture-specific cache line alignment for optimal performance
#[repr(align(128))]
#[cfg(target_arch = "aarch64")]
struct Align128<T>(T);

// Cache line size aware padding for different architectures
//
// 【キャッシュライン最適化の学び】
// - x86_64 (Intel/AMD): 理論上は64Bキャッシュラインだが、Crossbeam CachePaddedは
//   安全マージンのため128Bでパディングしている（最新プロセッサトレンド対応）
// - ARM64 (Apple M1/M2): 実際に128Bキャッシュラインを使用
// - 結果: 現在の実装では両アーキテクチャとも実質128Bで同じサイズ
//
// 【条件付きコンパイルの価値】
// 1. 将来の拡張性: ARM64で128B以上が必要になった場合の準備
// 2. ライブラリ独立性: Crossbeamの内部実装変更に依存しない制御
// 3. 明示的な意図: コードでアーキテクチャ意識を表現
// 4. テスト可能性: 各環境で適切なアライメントを検証
//
// Apple M1/M2 and some ARM64 servers use 128-byte cache lines
// x86_64 (including Ryzen 7950X) uses 64-byte cache lines
#[cfg(target_arch = "aarch64")]
type PaddedAtomicU32 = Align128<AtomicU32>; // 128B alignment for ARM64

#[cfg(target_arch = "x86_64")]
type PaddedAtomicU32 = CachePadded<AtomicU32>; // 64B is optimal for current x86_64

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
type PaddedAtomicU32 = CachePadded<AtomicU32>; // Fallback for other architectures

// Implementation for ARM64's 128-byte aligned atomic
#[cfg(target_arch = "aarch64")]
impl Align128<AtomicU32> {
    fn new(value: AtomicU32) -> Self {
        Align128(value)
    }

    fn load(&self, order: Ordering) -> u32 {
        self.0.load(order)
    }

    fn store(&self, value: u32, order: Ordering) {
        self.0.store(value, order)
    }

    fn fetch_update<F>(&self, set_order: Ordering, fetch_order: Ordering, f: F) -> Result<u32, u32>
    where
        F: FnMut(u32) -> Option<u32>,
    {
        self.0.fetch_update(set_order, fetch_order, f)
    }
}

/// Lock-free shared history table
///
/// 【実装の要点】
/// - False sharing回避: 各エントリを128Bでパディング（アーキテクチャ別最適化）
/// - メモリ効率: 4B -> 128Bで32倍のオーバーヘッドだが、パフォーマンスを優先
/// - 実際のメモリ使用量: 2430エントリ × 128B = 約304KB（メモリは安価、速度重視）
pub struct SharedHistory {
    /// History scores using atomic operations
    /// [color][piece_type][to_square]
    /// Each entry is cache-padded to prevent false sharing
    /// Uses architecture-aware padding for optimal cache line alignment
    table: Vec<PaddedAtomicU32>,
}

impl Default for SharedHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedHistory {
    /// Create a new shared history table
    pub fn new() -> Self {
        // 2 colors * 15 piece types * 81 squares = 2430 entries
        let size = 2 * 15 * 81;
        let mut table = Vec::with_capacity(size);
        for _ in 0..size {
            table.push(PaddedAtomicU32::new(AtomicU32::new(0)));
        }

        Self { table }
    }

    /// Get index for a move
    fn get_index(color: Color, piece_type: PieceType, to: Square) -> usize {
        let color_idx = color as usize;
        let piece_idx = piece_type as usize;
        let square_idx = to.index();

        color_idx * 15 * 81 + piece_idx * 81 + square_idx
    }

    /// Get history score
    pub fn get(&self, color: Color, piece_type: PieceType, to: Square) -> u32 {
        let idx = Self::get_index(color, piece_type, to);
        self.table[idx].load(Ordering::Relaxed)
    }

    /// Update history score (lock-free using fetch_update)
    ///
    /// 【パフォーマンス最適化】
    /// - 従来: load() -> compare_exchange()の2段階（読み取りオーバーヘッドあり）
    /// - 現在: fetch_update()で効率的な更新（読み取り+CASを1回の呼び出しで実行）
    /// - 効果: 最初の読み取りオーバーヘッドを削減、アトミック操作の最適化
    pub fn update(&self, color: Color, piece_type: PieceType, to: Square, bonus: u32) {
        let idx = Self::get_index(color, piece_type, to);

        // Use fetch_update for more efficient atomic update
        // 【CASループ最適化】
        // - 従来の手動CASループ: loop { load(); compare_exchange(); } → 読み取り無駄が発生
        // - fetch_update: 内部で効率的にload+CAS実行 → CPUの最適化機能をフル活用
        // - 結果: より少ない命令とメモリアクセスで同じ結果を実現
        let _ = self.table[idx].fetch_update(Ordering::Relaxed, Ordering::Relaxed, |old_value| {
            Some(old_value.saturating_add(bonus).min(10000))
        });
    }

    /// Age all history scores (divide by 2)
    pub fn age(&self) {
        for entry in &self.table {
            let old_value = entry.load(Ordering::Relaxed);
            entry.store(old_value / 2, Ordering::Relaxed);
        }
    }

    /// Clear all history
    pub fn clear(&self) {
        for entry in &self.table {
            entry.store(0, Ordering::Relaxed);
        }
    }
}

/// Shared search state for parallel threads
pub struct SharedSearchState {
    /// Best move found so far (encoded as u32)
    best_move: AtomicU32,

    /// Best score found so far
    best_score: AtomicI32,

    /// Depth of best score
    best_depth: AtomicU8,

    /// Generation number for PV synchronization
    current_generation: AtomicU64,

    /// Total nodes searched by all threads
    nodes_searched: AtomicU64,

    /// Stop flag for all threads
    pub stop_flag: Arc<AtomicBool>,

    /// Shared history table
    pub history: Arc<SharedHistory>,

    /// Duplication statistics
    pub duplication_stats: Arc<DuplicationStats>,
}

impl SharedSearchState {
    /// Create new shared search state
    pub fn new(stop_flag: Arc<AtomicBool>) -> Self {
        Self {
            best_move: AtomicU32::new(0),
            best_score: AtomicI32::new(i32::MIN),
            best_depth: AtomicU8::new(0),
            current_generation: AtomicU64::new(0),
            nodes_searched: AtomicU64::new(0),
            stop_flag,
            history: Arc::new(SharedHistory::new()),
            duplication_stats: Arc::new(DuplicationStats::new()),
        }
    }

    /// Reset state for new search
    pub fn reset(&self) {
        self.best_move.store(0, Ordering::Relaxed);
        self.best_score.store(i32::MIN, Ordering::Relaxed);
        self.best_depth.store(0, Ordering::Relaxed);
        self.current_generation.fetch_add(1, Ordering::Relaxed);
        self.nodes_searched.store(0, Ordering::Relaxed);
        self.stop_flag.store(false, Ordering::Release); // IMPORTANT: Reset stop flag for new search
        self.history.clear();
        self.duplication_stats.reset();
    }

    /// Try to update best move/score if better (lock-free)
    pub fn maybe_update_best(&self, score: i32, mv: Option<Move>, depth: u8, generation: u64) {
        // Check generation to avoid stale updates
        let current_gen = self.current_generation.load(Ordering::Relaxed);
        if generation != current_gen {
            return;
        }

        // Depth-based filtering
        let old_depth = self.best_depth.load(Ordering::Relaxed);
        if depth < old_depth {
            return;
        }

        // Try to update score
        let old_score = self.best_score.load(Ordering::Relaxed);
        if score > old_score || (score == old_score && depth > old_depth) {
            // Update score first
            match self.best_score.compare_exchange(
                old_score,
                score,
                Ordering::SeqCst,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    // Score updated successfully, now update move and depth
                    if let Some(m) = mv {
                        self.best_move.store(m.to_u16() as u32, Ordering::Relaxed);
                    }
                    self.best_depth.store(depth, Ordering::Release);
                }
                Err(_) => {
                    // Another thread updated the score, retry might be needed
                }
            }
        }
    }

    /// Get current best move
    pub fn get_best_move(&self) -> Option<Move> {
        let encoded = self.best_move.load(Ordering::Relaxed);
        if encoded == 0 {
            None
        } else {
            Some(Move::from_u16(encoded as u16))
        }
    }

    /// Get current best score
    pub fn get_best_score(&self) -> i32 {
        self.best_score.load(Ordering::Relaxed)
    }

    /// Get current best depth
    pub fn get_best_depth(&self) -> u8 {
        self.best_depth.load(Ordering::Relaxed)
    }

    /// Add to node count
    pub fn add_nodes(&self, nodes: u64) {
        self.nodes_searched.fetch_add(nodes, Ordering::Relaxed);
    }

    /// Get total nodes searched
    pub fn get_nodes(&self) -> u64 {
        self.nodes_searched.load(Ordering::Relaxed)
    }

    /// Check if search should stop
    pub fn should_stop(&self) -> bool {
        self.stop_flag.load(Ordering::Acquire)
    }

    /// Set stop flag
    pub fn set_stop(&self) {
        self.stop_flag.store(true, Ordering::Release);
    }

    /// Reset stop flag (for ensuring clean state)
    pub fn reset_stop_flag(&self) {
        self.stop_flag.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shogi::{Color, Move, PieceType, Square};
    use std::sync::{atomic::AtomicBool, Arc};

    #[test]
    fn test_shared_history() {
        let history = SharedHistory::new();

        // Test initial state
        assert_eq!(history.get(Color::Black, PieceType::Pawn, Square::new(5, 5)), 0);

        // Test update
        history.update(Color::Black, PieceType::Pawn, Square::new(5, 5), 100);
        assert_eq!(history.get(Color::Black, PieceType::Pawn, Square::new(5, 5)), 100);

        // Test saturation
        history.update(Color::Black, PieceType::Pawn, Square::new(5, 5), 10000);
        assert_eq!(history.get(Color::Black, PieceType::Pawn, Square::new(5, 5)), 10000);

        // Test aging
        history.age();
        assert_eq!(history.get(Color::Black, PieceType::Pawn, Square::new(5, 5)), 5000);

        // Test clear
        history.clear();
        assert_eq!(history.get(Color::Black, PieceType::Pawn, Square::new(5, 5)), 0);
    }

    #[test]
    fn test_shared_search_state() {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let state = SharedSearchState::new(stop_flag);

        // Test initial state
        assert_eq!(state.get_best_move(), None);
        assert_eq!(state.get_best_score(), i32::MIN);
        assert_eq!(state.get_best_depth(), 0);
        assert_eq!(state.get_nodes(), 0);

        // Test node counting
        state.add_nodes(1000);
        state.add_nodes(500);
        assert_eq!(state.get_nodes(), 1500);

        // Test best move update
        let test_move = Move::normal(Square::new(7, 7), Square::new(7, 6), false);
        state.maybe_update_best(100, Some(test_move), 5, 0);

        assert_eq!(state.get_best_move(), Some(test_move));
        assert_eq!(state.get_best_score(), 100);
        assert_eq!(state.get_best_depth(), 5);

        // Test depth filtering - lower depth should not update
        let worse_move = Move::normal(Square::new(2, 8), Square::new(2, 7), false);
        state.maybe_update_best(200, Some(worse_move), 3, 0);

        assert_eq!(state.get_best_move(), Some(test_move)); // Should not change
        assert_eq!(state.get_best_depth(), 5); // Should not change

        // Test better score at same or higher depth
        state.maybe_update_best(300, Some(worse_move), 5, 0);
        assert_eq!(state.get_best_move(), Some(worse_move)); // Should update
        assert_eq!(state.get_best_score(), 300);
    }

    #[test]
    fn test_cache_padded_size() {
        use std::mem::{align_of, size_of};

        // 【キャッシュパディング効果の検証】
        // - False sharing回避のため、適切なアライメントサイズを確保
        // - 各アーキテクチャに応じた最適なキャッシュライン境界での配置

        #[cfg(target_arch = "aarch64")]
        {
            // ARM64 should use 128-byte alignment for optimal performance
            assert_eq!(align_of::<PaddedAtomicU32>(), 128);
            assert_eq!(size_of::<PaddedAtomicU32>(), 128);
        }

        #[cfg(target_arch = "x86_64")]
        {
            // x86_64 uses 64-byte cache lines (Ryzen 7950X, Intel, etc.)
            // ただし、Crossbeam CachePaddedは実際には128Bでパディング（安全マージン）
            assert!(size_of::<PaddedAtomicU32>() >= 64);
            // CachePadded typically uses 64-byte or larger alignment
            assert!(align_of::<PaddedAtomicU32>() >= 64);
        }

        // General checks for all architectures
        assert!(size_of::<PaddedAtomicU32>() >= size_of::<AtomicU32>());
        assert!(align_of::<PaddedAtomicU32>() >= align_of::<AtomicU32>());

        println!(
            "PaddedAtomicU32: size={}, align={}",
            size_of::<PaddedAtomicU32>(),
            align_of::<PaddedAtomicU32>()
        );
    }
}
