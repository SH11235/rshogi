//! Shared state for parallel search
//!
//! Lock-free data structures shared between search threads

#[cfg(feature = "ybwc")]
use crate::shogi::Position;
use crate::{
    shogi::{Move, PieceType, Square},
    Color,
};
use crossbeam_utils::CachePadded;
use std::sync::atomic::{
    AtomicBool, AtomicI32, AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering,
};
use std::sync::Arc;
#[cfg(feature = "ybwc")]
use std::sync::RwLock;

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

/// Split point for YBWC (Young Brothers Wait Concept)
/// Represents a position where search can be split among threads
#[cfg(feature = "ybwc")]
#[derive(Debug)]
pub struct SplitPoint {
    /// Position being searched
    pub position: Position,
    /// Depth remaining in the search
    pub depth: u8,
    /// Alpha bound
    pub alpha: i32,
    /// Beta bound
    pub beta: i32,
    /// Moves to search (siblings after PV move)
    pub moves: Vec<Move>,
    /// Index of the first move that hasn't been searched yet
    pub next_move_index: AtomicUsize,
    /// Number of threads working on this split point
    pub active_threads: AtomicUsize,
    /// Flag indicating if a beta cutoff has been found
    pub cutoff_found: AtomicBool,
    /// Best score found so far at this split point
    pub best_score: AtomicI32,
    /// Best move found at this split point
    pub best_move: AtomicU32,
    /// PV move has been searched (for YBWC)
    pub pv_searched: AtomicBool,
}

#[cfg(feature = "ybwc")]
impl SplitPoint {
    /// Create a new split point
    pub fn new(position: Position, depth: u8, alpha: i32, beta: i32, moves: Vec<Move>) -> Self {
        Self {
            position,
            depth,
            alpha,
            beta,
            moves,
            next_move_index: AtomicUsize::new(0),
            active_threads: AtomicUsize::new(0),
            cutoff_found: AtomicBool::new(false),
            best_score: AtomicI32::new(alpha),
            best_move: AtomicU32::new(0),
            pv_searched: AtomicBool::new(false),
        }
    }

    /// Get the next move to search
    pub fn get_next_move(&self) -> Option<Move> {
        // If cutoff found, no more moves to search
        if self.cutoff_found.load(Ordering::Acquire) {
            return None;
        }

        // Atomically get and increment the move index
        let index = self.next_move_index.fetch_add(1, Ordering::AcqRel);

        if index < self.moves.len() {
            Some(self.moves[index])
        } else {
            None
        }
    }

    /// Mark that PV move has been searched
    pub fn mark_pv_searched(&self) {
        self.pv_searched.store(true, Ordering::Release);
    }

    /// Check if PV move has been searched (for YBWC)
    pub fn is_pv_searched(&self) -> bool {
        self.pv_searched.load(Ordering::Acquire)
    }

    /// Update best score and move if better
    pub fn update_best(&self, score: i32, mv: Move) -> bool {
        let mut current_best = self.best_score.load(Ordering::Acquire);

        while score > current_best {
            match self.best_score.compare_exchange_weak(
                current_best,
                score,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    self.best_move.store(mv.to_u16() as u32, Ordering::Release);

                    // Check for beta cutoff
                    if score >= self.beta {
                        self.cutoff_found.store(true, Ordering::Release);
                        return true;
                    }
                    return false;
                }
                Err(x) => current_best = x,
            }
        }
        false
    }

    /// Add a thread working on this split point
    pub fn add_thread(&self) {
        self.active_threads.fetch_add(1, Ordering::AcqRel);
    }

    /// Remove a thread from this split point
    pub fn remove_thread(&self) {
        self.active_threads.fetch_sub(1, Ordering::AcqRel);
    }

    /// Get number of active threads
    pub fn active_thread_count(&self) -> usize {
        self.active_threads.load(Ordering::Acquire)
    }
}

/// Manager for split points in YBWC
#[cfg(feature = "ybwc")]
pub struct SplitPointManager {
    /// Active split points
    split_points: RwLock<Vec<Arc<SplitPoint>>>,
    /// Maximum depth difference for creating split points
    _max_split_depth_diff: u8,
}

#[cfg(feature = "ybwc")]
impl SplitPointManager {
    /// Create a new split point manager
    pub fn new() -> Self {
        Self {
            split_points: RwLock::new(Vec::new()),
            _max_split_depth_diff: 3,
        }
    }

    /// Add a new split point
    pub fn add_split_point(&self, split_point: SplitPoint) -> Arc<SplitPoint> {
        let arc_split = Arc::new(split_point);
        self.split_points.write().unwrap().push(arc_split.clone());
        arc_split
    }

    /// Get an available split point for a thread to work on
    pub fn get_available_split_point(&self) -> Option<Arc<SplitPoint>> {
        let split_points = self.split_points.read().unwrap();

        // Find a split point with work available
        for sp in split_points.iter() {
            // YBWC: Only join if PV has been searched
            if !sp.is_pv_searched() {
                continue;
            }

            // Check if there's work available
            if !sp.cutoff_found.load(Ordering::Acquire)
                && sp.next_move_index.load(Ordering::Acquire) < sp.moves.len()
            {
                return Some(sp.clone());
            }
        }

        None
    }

    /// Remove completed split points
    pub fn cleanup_completed(&self) {
        let mut split_points = self.split_points.write().unwrap();
        split_points
            .retain(|sp| !sp.cutoff_found.load(Ordering::Acquire) || sp.active_thread_count() > 0);
    }

    /// Clear all split points
    pub fn clear(&self) {
        self.split_points.write().unwrap().clear();
    }

    /// Check if we should create a split point
    pub fn should_split(&self, depth: u8, move_count: usize, thread_utilization: f64) -> bool {
        // More relaxed conditions for split point creation

        // Allow splitting at depth 3 or deeper (was 4)
        if depth < 3 {
            return false;
        }

        // Allow splitting with fewer moves (was 8)
        if move_count < 4 {
            return false;
        }

        // Only prevent splitting if threads are very well utilized (was 0.7)
        if thread_utilization > 0.9 {
            return false;
        }

        true
    }
}

#[cfg(feature = "ybwc")]
impl Default for SplitPointManager {
    fn default() -> Self {
        Self::new()
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
        // 2 colors * 15 piece types * board squares entries
        let size = 2 * 15 * crate::shogi::SHOGI_BOARD_SIZE;
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

        color_idx * 15 * crate::shogi::SHOGI_BOARD_SIZE
            + piece_idx * crate::shogi::SHOGI_BOARD_SIZE
            + square_idx
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

use crate::search::common::SharedStopInfo;
use crate::search::snapshot::{RootSnapshot, RootSnapshotPublisher};

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

    /// Total quiescence nodes searched by all threads
    qnodes_searched: Arc<AtomicU64>,

    /// Stop flag for all threads
    pub stop_flag: Arc<AtomicBool>,

    /// Shared stop info for recording termination reason
    pub stop_info: Arc<SharedStopInfo>,

    /// Shared history table
    pub history: Arc<SharedHistory>,

    /// Duplication statistics
    pub duplication_stats: Arc<DuplicationStats>,

    /// Split point manager for YBWC
    #[cfg(feature = "ybwc")]
    pub split_point_manager: Arc<SplitPointManager>,

    /// Number of active threads (for utilization calculation)
    pub active_threads: AtomicUsize,

    /// Total number of threads
    pub total_threads: usize,

    /// Whether the search finalized early (for skipping worker joins)
    finalized_early: AtomicBool,

    /// Flag to prevent new work from being enqueued once stop is requested
    work_closed: AtomicBool,

    /// Root snapshot publisher for out-of-band finalize (read-only on USI side)
    pub snapshot: Arc<RootSnapshotPublisher>,
}

impl SharedSearchState {
    /// Create new shared search state
    pub fn new(stop_flag: Arc<AtomicBool>) -> Self {
        Self::with_threads(stop_flag, 1)
    }

    /// Create new shared search state with specified number of threads
    pub fn with_threads(stop_flag: Arc<AtomicBool>, num_threads: usize) -> Self {
        Self {
            best_move: AtomicU32::new(0),
            best_score: AtomicI32::new(i32::MIN),
            best_depth: AtomicU8::new(0),
            current_generation: AtomicU64::new(0),
            nodes_searched: AtomicU64::new(0),
            qnodes_searched: Arc::new(AtomicU64::new(0)),
            stop_flag,
            stop_info: SharedStopInfo::new_arc(),
            history: Arc::new(SharedHistory::new()),
            duplication_stats: Arc::new(DuplicationStats::new()),
            #[cfg(feature = "ybwc")]
            split_point_manager: Arc::new(SplitPointManager::new()),
            active_threads: AtomicUsize::new(0),
            total_threads: num_threads,
            finalized_early: AtomicBool::new(false),
            work_closed: AtomicBool::new(false),
            snapshot: Arc::new(RootSnapshotPublisher::new()),
        }
    }

    /// Reset state for new search
    pub fn reset(&self) {
        self.best_move.store(0, Ordering::Relaxed);
        self.best_score.store(i32::MIN, Ordering::Relaxed);
        self.best_depth.store(0, Ordering::Relaxed);
        self.current_generation.fetch_add(1, Ordering::Relaxed);
        self.nodes_searched.store(0, Ordering::Relaxed);
        self.qnodes_searched.store(0, Ordering::Relaxed);
        self.stop_flag.store(false, Ordering::Release); // IMPORTANT: Reset stop flag for new search
        self.history.clear();
        self.duplication_stats.reset();
        #[cfg(feature = "ybwc")]
        self.split_point_manager.clear();
        self.active_threads.store(0, Ordering::Relaxed);
        self.finalized_early.store(false, Ordering::Release);
        self.work_closed.store(false, Ordering::Release);
        // Reset snapshot to a clean default for the new generation
        let snap = RootSnapshot {
            search_id: self.generation(),
            // 他フィールドは初期化リセット目的でデフォルト (root_key=0, best=None, pv empty 等)
            ..Default::default()
        };
        self.snapshot.publish(&snap);
    }

    /// Get current generation (epoch) for this search state.
    pub fn generation(&self) -> u64 {
        self.current_generation.load(Ordering::Acquire)
    }

    /// Publish a minimal snapshot from current shared state (best/depth/score/nodes only)
    pub fn publish_minimal_snapshot(&self, root_key: u64, elapsed_ms: u32) {
        let snap = RootSnapshot {
            search_id: self.generation(),
            root_key,
            best: self.get_best_move(),
            depth: self.get_best_depth(),
            score_cp: self.get_best_score(),
            nodes: self.get_nodes(),
            elapsed_ms,
            ..Default::default()
        };
        self.snapshot.publish(&snap);
    }

    /// Publish minimal fields while preserving previously published PV if any.
    /// This avoids wiping PV on iterations with no improvement.
    pub fn publish_minimal_snapshot_preserve_pv(&self, root_key: u64, elapsed_ms: u32) {
        let prev = self.snapshot.try_read();
        let snap = RootSnapshot {
            search_id: self.generation(),
            root_key,
            best: self.get_best_move(),
            depth: self.get_best_depth(),
            score_cp: self.get_best_score(),
            nodes: self.get_nodes(),
            elapsed_ms,
            pv: prev.map(|p| p.pv).unwrap_or_default(),
        };
        self.snapshot.publish(&snap);
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
        let encoded = self.best_move.load(Ordering::Acquire);
        if encoded == 0 {
            None
        } else {
            Some(Move::from_u16(encoded as u16))
        }
    }

    /// Get current best score
    pub fn get_best_score(&self) -> i32 {
        self.best_score.load(Ordering::Acquire)
    }

    /// Get current best depth
    pub fn get_best_depth(&self) -> u8 {
        self.best_depth.load(Ordering::Acquire)
    }

    /// Add to node count
    pub fn add_nodes(&self, nodes: u64) {
        self.nodes_searched.fetch_add(nodes, Ordering::Relaxed);
    }

    /// Get total nodes searched
    pub fn get_nodes(&self) -> u64 {
        self.nodes_searched.load(Ordering::Relaxed)
    }

    /// Add to quiescence node count
    pub fn add_qnodes(&self, qnodes: u64) {
        self.qnodes_searched.fetch_add(qnodes, Ordering::Relaxed);
    }

    /// Get total quiescence nodes searched
    pub fn get_qnodes(&self) -> u64 {
        self.qnodes_searched.load(Ordering::Relaxed)
    }

    /// Get Arc reference to qnodes counter for sharing with SearchLimits
    pub fn get_qnodes_counter(&self) -> Arc<AtomicU64> {
        self.qnodes_searched.clone()
    }

    /// Check if search should stop
    pub fn should_stop(&self) -> bool {
        self.stop_flag.load(Ordering::Acquire)
    }

    /// Set stop flag
    pub fn set_stop(&self) {
        self.stop_flag.store(true, Ordering::Release);
    }

    /// Mark search as finalized early (main thread returning without worker join)
    pub fn mark_finalized_early(&self) {
        self.finalized_early.store(true, Ordering::Release);
    }

    /// Check whether search has finalized early.
    pub fn is_finalized_early(&self) -> bool {
        self.finalized_early.load(Ordering::Acquire)
    }

    /// Set stop flag with reason
    pub fn set_stop_with_reason(&self, stop_info: crate::search::types::StopInfo) {
        // Try to set stop info first (only first call succeeds)
        self.stop_info.try_set(stop_info);

        // Then set the stop flag
        self.stop_flag.store(true, Ordering::Release);
    }

    /// Reset stop flag (for ensuring clean state)
    pub fn reset_stop_flag(&self) {
        self.stop_flag.store(false, Ordering::Release);
    }

    /// Close work queues to prevent further enqueues (idempotent)
    pub fn close_work_queues(&self) {
        self.work_closed.store(true, Ordering::Release);
    }

    /// Re-open work queues for a fresh search session
    pub fn reopen_work_queues(&self) {
        self.work_closed.store(false, Ordering::Release);
    }

    /// Check if work queues are closed
    pub fn work_queues_closed(&self) -> bool {
        self.work_closed.load(Ordering::Acquire)
    }

    /// Increment active thread count
    pub fn increment_active_threads(&self) {
        self.active_threads.fetch_add(1, Ordering::AcqRel);
    }

    /// Decrement active thread count
    pub fn decrement_active_threads(&self) {
        self.active_threads.fetch_sub(1, Ordering::AcqRel);
    }

    /// Get thread utilization (0.0 to 1.0)
    pub fn get_thread_utilization(&self) -> f64 {
        let active = self.active_threads.load(Ordering::Acquire) as f64;
        let total = self.total_threads as f64;
        if total > 0.0 {
            active / total
        } else {
            0.0
        }
    }

    /// Snapshot current active worker count
    pub fn active_thread_count(&self) -> usize {
        self.active_threads.load(Ordering::Acquire)
    }

    /// Check if split point should be created based on current conditions
    #[cfg(feature = "ybwc")]
    pub fn should_create_split_point(&self, depth: u8, move_count: usize) -> bool {
        let utilization = self.get_thread_utilization();
        self.split_point_manager.should_split(depth, move_count, utilization)
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

        log::debug!(
            "PaddedAtomicU32: size={}, align={}",
            size_of::<PaddedAtomicU32>(),
            align_of::<PaddedAtomicU32>()
        );
    }
}
