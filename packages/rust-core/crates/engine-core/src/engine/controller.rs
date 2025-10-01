//! Main engine implementation that integrates search and evaluation
//!
//! Provides a simple interface for using different evaluators with the search engine

use crate::search::parallel::ParallelSearcher;
use crate::{
    engine::session::SearchSession,
    evaluation::evaluate::{Evaluator, MaterialEvaluator},
    evaluation::nnue::NNUEEvaluatorWrapper,
    search::parallel::EngineStopBridge,
    search::stub::run_stub_search,
    search::unified::UnifiedSearcher,
    search::{SearchLimits, SearchResult, SearchStats, TranspositionTable},
    Position,
};
use log::{debug, error, info, warn};
use parking_lot::RwLock;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    mpsc, Arc, Mutex,
};
use std::thread;

use crate::game_phase::{detect_game_phase, GamePhase, Profile};

/// The source used for final bestmove decision
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalBestSource {
    Book,
    Committed,
    TT,
    LegalFallback,
    Resign,
}

/// Result of final bestmove decision
pub struct FinalBest {
    pub best_move: Option<crate::shogi::Move>,
    pub pv: Vec<crate::shogi::Move>,
    pub source: FinalBestSource,
}

/// Lightweight TT debug snapshot for diagnostics
#[derive(Debug, Clone, Copy)]
pub struct TtDebugInfo {
    pub addr: usize,
    pub size_mb: usize,
    pub hf_permille: u16,
    pub store_attempts: u64,
}

/// Arguments bundle for search_in_thread to avoid too many function parameters
struct SearchThreadArgs {
    engine_type: EngineType,
    use_parallel: bool,
    num_threads: usize,
    material_evaluator: Arc<MaterialEvaluator>,
    nnue_evaluator: Arc<RwLock<Option<NNUEEvaluatorWrapper>>>,
    shared_tt: Arc<TranspositionTable>,
    stop_bridge: Arc<EngineStopBridge>,
    material_parallel_searcher: Arc<Mutex<Option<MaterialParallelSearcher>>>,
    nnue_parallel_searcher: Arc<Mutex<Option<NnueParallelSearcher>>>,
    material_searcher: Arc<Mutex<Option<MaterialSearcher>>>,
    nnue_basic_searcher: Arc<Mutex<Option<NnueBasicSearcher>>>,
    material_enhanced_searcher: Arc<Mutex<Option<MaterialEnhancedSearcher>>>,
    nnue_enhanced_searcher: Arc<Mutex<Option<NnueEnhancedSearcher>>>,
}

/// Arguments bundle for parallel search static helpers
struct ParallelSearchArgs {
    active_threads: usize,
    shared_tt: Arc<TranspositionTable>,
    stop_bridge: Arc<EngineStopBridge>,
}

/// RAII guard to ensure active_searches counter is decremented even on panic
struct ActiveSearchGuard(Arc<AtomicUsize>);

impl Drop for ActiveSearchGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

// Game phase detection is now handled by the game_phase module
// See docs/game-phase-module-guide.md for details

/// Type alias for unified searchers
type MaterialSearcher = UnifiedSearcher<MaterialEvaluator, true, false>;
type NnueBasicSearcher = UnifiedSearcher<NNUEEvaluatorProxy, true, false>;
type MaterialEnhancedSearcher = UnifiedSearcher<MaterialEvaluator, true, true>;
type NnueEnhancedSearcher = UnifiedSearcher<NNUEEvaluatorProxy, true, true>;

/// Type alias for parallel searchers
type MaterialParallelSearcher = ParallelSearcher<MaterialEvaluator>;
// 差分Accは常時有効化。フック対を踏むため HookSuppressor は不使用。
type NnueParallelSearcher = ParallelSearcher<NNUEEvaluatorProxy>;

/// Engine type selection
///
/// # Engine Types Overview
///
/// | Type | Search Algorithm | Evaluation | Use Case |
/// |------|-----------------|------------|----------|
/// | `EnhancedNnue` | Advanced (pruning) | NNUE | **Strongest - recommended for matches** |
/// | `Nnue` | Basic | NNUE | Fast analysis |
/// | `Enhanced` | Advanced (pruning) | Material | Lightweight environments |
/// | `Material` | Basic | Material | Debug/Testing |
///
/// # Performance Comparison
///
/// Relative strength with same thinking time (Material = 1.0):
/// - `EnhancedNnue`: 4.0-5.0x (deepest search + best evaluation)
/// - `Nnue`: 2.5-3.0x (good evaluation compensates simple search)
/// - `Enhanced`: 2.0-2.5x (deep search compensates simple evaluation)
/// - `Material`: 1.0x (baseline)
///
/// # Recommendations
///
/// - **For strongest play**: Use `EnhancedNnue`
/// - **For fast analysis**: Use `Nnue`
/// - **For low memory**: Use `Enhanced`
/// - **For debugging**: Use `Material`
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EngineType {
    /// Minimal stub searcher that returns a deterministic legal move without tree search
    /// Useful during Phase 0 (migration) to keep USI responsive while replacing search core
    Stub,
    /// Simple material-based evaluation with basic alpha-beta search
    /// - Simplest implementation
    /// - Best for debugging and testing
    /// - Memory usage: ~5MB
    Material,

    /// NNUE evaluation with basic alpha-beta search
    /// - Neural network evaluation for better positional understanding
    /// - Stable and fast for shallow searches
    /// - Memory usage: ~170MB (NNUE weights)
    Nnue,

    /// Enhanced search with material evaluation
    /// - Advanced pruning: Null Move, LMR, Futility Pruning
    /// - Uses a transposition table for caching (size configurable)
    /// - Good for learning search techniques
    /// - Memory usage depends on TT size
    Enhanced,

    /// Enhanced search with NNUE evaluation (Strongest)
    /// - Combines best search techniques with best evaluation
    /// - Deepest search depth in same time
    /// - Recommended for competitive play
    /// - Memory usage: ~200MB+
    EnhancedNnue,
}

/// Main engine struct
pub struct Engine {
    engine_type: EngineType,
    material_evaluator: Arc<MaterialEvaluator>,
    nnue_evaluator: Arc<RwLock<Option<NNUEEvaluatorWrapper>>>,
    // Unified searchers for each engine type
    material_searcher: Arc<Mutex<Option<MaterialSearcher>>>,
    nnue_basic_searcher: Arc<Mutex<Option<NnueBasicSearcher>>>,
    material_enhanced_searcher: Arc<Mutex<Option<MaterialEnhancedSearcher>>>,
    nnue_enhanced_searcher: Arc<Mutex<Option<NnueEnhancedSearcher>>>,
    // Parallel searchers
    material_parallel_searcher: Arc<Mutex<Option<MaterialParallelSearcher>>>,
    nnue_parallel_searcher: Arc<Mutex<Option<NnueParallelSearcher>>>,
    // Shared transposition table for parallel search
    shared_tt: Arc<TranspositionTable>,
    // Number of threads for parallel search
    num_threads: usize,
    // Whether to use parallel search
    use_parallel: bool,
    // Pending thread count for safe update during search
    pending_thread_count: Option<usize>,
    // Transposition table size in MB
    tt_size_mb: usize,
    // Pending TT size for safe update during search
    pending_tt_size: Option<usize>,
    // Desired MultiPV (applied to new/recreated searchers)
    desired_multi_pv: u8,
    // Active searches counter for clear_hash safety
    active_searches: Arc<AtomicUsize>,
    // Bridge to issue immediate stop requests without locking Engine mutex
    stop_bridge: Arc<EngineStopBridge>,
    // Session ID counter for async search
    session_counter: u64,
}

impl Engine {
    /// Create new engine with specified type
    pub fn new(engine_type: EngineType) -> Self {
        let material_evaluator = Arc::new(MaterialEvaluator);
        let default_tt_size = 1024; // Default TT size in MB

        let stop_bridge = Arc::new(EngineStopBridge::new());

        let nnue_evaluator = if matches!(engine_type, EngineType::Nnue | EngineType::EnhancedNnue) {
            // Initialize with zero weights for NNUE engine
            Arc::new(RwLock::new(Some(NNUEEvaluatorWrapper::zero())))
        } else {
            Arc::new(RwLock::new(None))
        };

        // Create shared TT (単スレ・並列ともにこの 1 本を共有)
        let shared_tt = {
            let arc0 = Arc::new(TranspositionTable::new(default_tt_size));
            #[cfg(feature = "tt_metrics")]
            {
                let mut arc1 = arc0;
                if let Some(tt) = Arc::get_mut(&mut arc1) {
                    tt.enable_metrics();
                }
                arc1
            }
            #[cfg(not(feature = "tt_metrics"))]
            {
                arc0
            }
        };

        // Initialize single-thread searchers based on engine type using shared_tt
        let material_searcher = if engine_type == EngineType::Material {
            Arc::new(Mutex::new(Some(MaterialSearcher::with_shared_tt(
                material_evaluator.clone(),
                shared_tt.clone(),
            ))))
        } else {
            Arc::new(Mutex::new(None))
        };

        let nnue_basic_searcher = if engine_type == EngineType::Nnue {
            let nnue_proxy = NNUEEvaluatorProxy {
                evaluator: nnue_evaluator.clone(),
                locals: thread_local::ThreadLocal::new(),
            };
            Arc::new(Mutex::new(Some(NnueBasicSearcher::with_shared_tt(
                Arc::new(nnue_proxy),
                shared_tt.clone(),
            ))))
        } else {
            Arc::new(Mutex::new(None))
        };

        let material_enhanced_searcher = if engine_type == EngineType::Enhanced {
            Arc::new(Mutex::new(Some(MaterialEnhancedSearcher::with_shared_tt(
                material_evaluator.clone(),
                shared_tt.clone(),
            ))))
        } else {
            Arc::new(Mutex::new(None))
        };

        let nnue_enhanced_searcher = if engine_type == EngineType::EnhancedNnue {
            let nnue_proxy = NNUEEvaluatorProxy {
                evaluator: nnue_evaluator.clone(),
                locals: thread_local::ThreadLocal::new(),
            };
            Arc::new(Mutex::new(Some(NnueEnhancedSearcher::with_shared_tt(
                Arc::new(nnue_proxy),
                shared_tt.clone(),
            ))))
        } else {
            Arc::new(Mutex::new(None))
        };

        Engine {
            engine_type,
            material_evaluator,
            nnue_evaluator,
            material_searcher,
            nnue_basic_searcher,
            material_enhanced_searcher,
            nnue_enhanced_searcher,
            material_parallel_searcher: Arc::new(Mutex::new(None)),
            nnue_parallel_searcher: Arc::new(Mutex::new(None)),
            shared_tt,
            num_threads: 1, // Default to single thread
            use_parallel: false,
            pending_thread_count: None,
            tt_size_mb: default_tt_size,
            pending_tt_size: None,
            desired_multi_pv: 1,
            active_searches: Arc::new(AtomicUsize::new(0)),
            stop_bridge,
            session_counter: 0,
        }
    }

    /// Issue an immediate stop request to the currently running search without acquiring locks.
    pub fn request_stop_immediate(&self) {
        self.stop_bridge.request_stop_immediate();
    }

    /// Obtain a clone of the stop bridge for out-of-engine coordination (USI layer).
    pub fn stop_bridge_handle(&self) -> Arc<EngineStopBridge> {
        Arc::clone(&self.stop_bridge)
    }

    /// Return TT metrics summary string if available (tt_metrics feature only)
    #[cfg(feature = "tt_metrics")]
    pub fn tt_metrics_summary(&self) -> Option<String> {
        // TT は 1 本化しているため常に shared を参照
        self.shared_tt.metrics_summary_string()
    }

    /// Get current hashfull estimate of the transposition table (permille: 0-1000)
    /// 常に shared TT の値を返す
    pub fn tt_hashfull_permille(&self) -> u16 {
        self.shared_tt.hashfull()
    }

    /// Set MultiPV lines (1 = single PV). Applies to all active searchers.
    pub fn set_multipv(&self, k: u8) {
        let k = k.clamp(1, 20);
        // Record desired value for future recreated searchers
        // SAFETY: casting self to mut via interior mutability is avoided; we accept not updating here.
        // The value is primarily applied immediately to live searchers below and on future clear_hash/set_engine_type.
        // To guarantee persistence across resets, callers should set again after reset_for_position().
        // As a stronger guarantee, we maintain desired_multi_pv via a separate setter (see below).
        // For each searcher type, set if present
        if let Ok(mut guard) = self.material_searcher.lock() {
            if let Some(ref mut s) = *guard {
                s.set_multi_pv(k);
            }
        }
        if let Ok(mut guard) = self.nnue_basic_searcher.lock() {
            if let Some(ref mut s) = *guard {
                s.set_multi_pv(k);
            }
        }
        if let Ok(mut guard) = self.material_enhanced_searcher.lock() {
            if let Some(ref mut s) = *guard {
                s.set_multi_pv(k);
            }
        }
        if let Ok(mut guard) = self.nnue_enhanced_searcher.lock() {
            if let Some(ref mut s) = *guard {
                s.set_multi_pv(k);
            }
        }

        // Parallel searchers: MultiPV is not yet supported in parallel mode.
    }

    /// Persist desired MultiPV and apply to current searchers
    pub fn set_multipv_persistent(&mut self, k: u8) {
        let k = k.clamp(1, 20);
        self.desired_multi_pv = k;
        self.set_multipv(k);
    }

    /// Set pruning teacher profile across searchers
    pub fn set_teacher_profile(&self, profile: crate::search::types::TeacherProfile) {
        if let Ok(mut guard) = self.material_searcher.lock() {
            if let Some(ref mut s) = *guard {
                s.set_teacher_profile(profile);
            }
        }
        if let Ok(mut guard) = self.nnue_basic_searcher.lock() {
            if let Some(ref mut s) = *guard {
                s.set_teacher_profile(profile);
            }
        }
        if let Ok(mut guard) = self.material_enhanced_searcher.lock() {
            if let Some(ref mut s) = *guard {
                s.set_teacher_profile(profile);
            }
        }
        if let Ok(mut guard) = self.nnue_enhanced_searcher.lock() {
            if let Some(ref mut s) = *guard {
                s.set_teacher_profile(profile);
            }
        }
    }

    /// Reset state for a fresh position: TT, heuristics, and thread policy.
    /// Ensures reproducibility for teacher data generation.
    pub fn reset_for_position(&mut self) {
        // Clear TT and rebuild searchers to point at fresh TT
        self.clear_hash();

        // Reset per-searcher heuristics for reproducibility
        if let Ok(mut guard) = self.material_searcher.lock() {
            if let Some(ref mut s) = *guard {
                s.reset_history();
            }
        }
        if let Ok(mut guard) = self.nnue_basic_searcher.lock() {
            if let Some(ref mut s) = *guard {
                s.reset_history();
            }
        }
        if let Ok(mut guard) = self.material_enhanced_searcher.lock() {
            if let Some(ref mut s) = *guard {
                s.reset_history();
            }
        }
        if let Ok(mut guard) = self.nnue_enhanced_searcher.lock() {
            if let Some(ref mut s) = *guard {
                s.reset_history();
            }
        }

        // Force single-threaded deterministic search for teacher data
        self.use_parallel = false;
        self.num_threads = 1;
    }

    /// Calculate active threads based on game phase
    #[cfg(test)]
    fn calculate_active_threads(&self, position: &Position) -> usize {
        let phase = detect_game_phase(position, position.ply as u32, Profile::Search);
        let base_threads = self.num_threads;

        match phase {
            GamePhase::Opening => base_threads,             // All threads
            GamePhase::MiddleGame => base_threads,          // All threads
            GamePhase::EndGame => base_threads.div_ceil(2), // Half threads (rounded up)
        }
    }

    /// Helper to generate next session ID
    fn next_session_id(&mut self) -> u64 {
        self.session_counter = self.session_counter.wrapping_add(1);
        self.session_counter
    }

    /// Start an asynchronous search and return immediately.
    ///
    /// This method releases the Engine lock immediately after spawning the search thread,
    /// allowing concurrent operations. The caller can poll the returned `SearchSession`
    /// for results without blocking.
    ///
    /// This is the preferred method for normal game play, as it follows the yanerau-o
    /// design pattern of non-blocking search initiation.
    pub fn start_search(&mut self, pos: Position, mut limits: SearchLimits) -> SearchSession {
        // Generate unique session ID
        let session_id = self.next_session_id();
        debug!("Starting async search session {session_id}");

        // Set session ID in limits for OOB coordination
        limits.session_id = session_id;

        // Apply pending configuration
        self.apply_pending_thread_count();
        self.apply_pending_tt_size();

        // Bump TT age for new search epoch
        self.shared_tt.bump_age();

        // Create result channel
        let (tx, rx) = mpsc::channel();

        // Fast path: Stub searcher (no tree search, deterministic legal move)
        if self.engine_type == EngineType::Stub {
            let stop_bridge = self.stop_bridge.clone();
            let pos_clone = pos.clone();
            let tx2 = tx.clone();
            // Publish external stop flag (if provided)
            self.stop_bridge.update_external_stop_flag(limits.stop_flag.as_ref());
            self.active_searches.fetch_add(1, Ordering::SeqCst);
            let active_searches = self.active_searches.clone();
            let handle = thread::Builder::new()
                .name(format!("engine-stub-search-{}", session_id))
                .spawn(move || {
                    let _guard = ActiveSearchGuard(active_searches);
                    if let Some(ext) = limits.stop_flag.as_ref() {
                        stop_bridge.update_external_stop_flag(Some(ext));
                    }
                    let result = run_stub_search(&pos_clone, &limits);
                    let _ = tx2.send(result);
                })
                .expect("spawn stub search thread");
            return SearchSession::new(session_id, rx, Some(handle));
        }

        // Clone necessary data for the background thread
        let mut pos_clone = pos;
        let engine_type = self.engine_type;
        let use_parallel = self.use_parallel;
        let num_threads = self.num_threads;

        // Clone searchers and evaluators
        let material_evaluator = self.material_evaluator.clone();
        let nnue_evaluator = self.nnue_evaluator.clone();
        let shared_tt = self.shared_tt.clone();
        let stop_bridge = self.stop_bridge.clone();

        let material_parallel_searcher = self.material_parallel_searcher.clone();
        let nnue_parallel_searcher = self.nnue_parallel_searcher.clone();
        let material_searcher = self.material_searcher.clone();
        let nnue_basic_searcher = self.nnue_basic_searcher.clone();
        let material_enhanced_searcher = self.material_enhanced_searcher.clone();
        let nnue_enhanced_searcher = self.nnue_enhanced_searcher.clone();

        // Update external stop flag in bridge before spawning thread
        self.stop_bridge.update_external_stop_flag(limits.stop_flag.as_ref());

        // Increment active searches before spawning thread
        self.active_searches.fetch_add(1, Ordering::SeqCst);
        let active_searches = self.active_searches.clone();

        // Publish session to stop bridge immediately before spawning thread.
        // This ensures that request_stop_immediate() can reach the search session
        // even if called right after start_search() returns.
        // For single-threaded searches, the internal stop flag will be published later
        // by the search thread, but the external stop flag is already registered above.
        if limits.stop_flag.is_some() {
            // External stop flag is already registered via update_external_stop_flag() above.
            // The search thread will call publish_session/update again with full handles.
            debug!("Pre-publishing external stop flag for session {session_id}");
        }

        // Spawn search in background thread with named thread for debugging
        let handle = thread::Builder::new()
            .name(format!("engine-search-{}", session_id))
            .spawn(move || {
                // RAII guard ensures active_searches is decremented even on panic
                let _guard = ActiveSearchGuard(active_searches);

                // Build search arguments
                let args = SearchThreadArgs {
                    engine_type,
                    use_parallel,
                    num_threads,
                    material_evaluator,
                    nnue_evaluator,
                    shared_tt,
                    stop_bridge,
                    material_parallel_searcher,
                    nnue_parallel_searcher,
                    material_searcher,
                    nnue_basic_searcher,
                    material_enhanced_searcher,
                    nnue_enhanced_searcher,
                };

                // Catch panics to ensure we don't send on a disconnected channel
                let search_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    Self::search_in_thread(&mut pos_clone, limits, args)
                }));

                match search_result {
                    Ok(result) => {
                        // Send result back (ignore send error if receiver dropped)
                        let _ = tx.send(result);
                    }
                    Err(panic_err) => {
                        // Extract panic message safely via downcast
                        let msg = if let Some(s) = panic_err.downcast_ref::<String>() {
                            s.clone()
                        } else if let Some(s) = panic_err.downcast_ref::<&'static str>() {
                            s.to_string()
                        } else {
                            "non-string panic payload".to_string()
                        };
                        error!("search_in_thread session={} panicked: {}", session_id, msg);
                        // Don't send result - USI layer will detect Disconnected via TryResult
                    }
                }
                // _guard is dropped here, ensuring active_searches is decremented
            })
            .expect("failed to spawn search thread");

        // Return session handle immediately (with JoinHandle for isready/quit)
        SearchSession::new(session_id, rx, Some(handle))
    }

    /// Internal helper to run search in a background thread.
    ///
    /// This is called by `start_search()` in a spawned thread and performs
    /// the actual search logic without holding any Engine locks.
    fn search_in_thread(
        pos: &mut Position,
        limits: SearchLimits,
        args: SearchThreadArgs,
    ) -> SearchResult {
        // Detect game phase and calculate active threads
        let phase = detect_game_phase(pos, pos.ply as u32, Profile::Search);
        let base_threads = args.num_threads;
        let active_threads = match phase {
            GamePhase::Opening => base_threads,
            GamePhase::MiddleGame => base_threads,
            GamePhase::EndGame => base_threads.div_ceil(2),
        };

        debug!(
            "search_in_thread: engine_type={:?}, parallel={}, active_threads={} (phase={:?})",
            args.engine_type, args.use_parallel, active_threads, phase
        );

        let parallel_args = ParallelSearchArgs {
            active_threads,
            shared_tt: args.shared_tt.clone(),
            stop_bridge: args.stop_bridge.clone(),
        };

        // Execute search based on engine type and parallelism
        if args.use_parallel {
            match args.engine_type {
                EngineType::Material | EngineType::Enhanced => {
                    Self::search_parallel_material_static(
                        pos,
                        limits,
                        parallel_args,
                        args.material_evaluator,
                        args.material_parallel_searcher,
                    )
                }
                EngineType::Nnue | EngineType::EnhancedNnue => Self::search_parallel_nnue_static(
                    pos,
                    limits,
                    parallel_args,
                    args.nnue_evaluator,
                    args.nnue_parallel_searcher,
                ),
                EngineType::Stub => SearchResult::new(None, 0, SearchStats::default()),
            }
        } else {
            // Single-threaded search
            match args.engine_type {
                EngineType::Material => Self::search_single_material_static(
                    pos,
                    limits,
                    args.material_evaluator,
                    args.shared_tt,
                    args.material_searcher,
                    args.stop_bridge,
                ),
                EngineType::Nnue => Self::search_single_nnue_static(
                    pos,
                    limits,
                    args.nnue_evaluator,
                    args.shared_tt,
                    args.nnue_basic_searcher,
                    args.stop_bridge,
                ),
                EngineType::Enhanced => Self::search_single_enhanced_material_static(
                    pos,
                    limits,
                    args.material_evaluator,
                    args.shared_tt,
                    args.material_enhanced_searcher,
                    args.stop_bridge,
                ),
                EngineType::EnhancedNnue => Self::search_single_enhanced_nnue_static(
                    pos,
                    limits,
                    args.nnue_evaluator,
                    args.shared_tt,
                    args.nnue_enhanced_searcher,
                    args.stop_bridge,
                ),
                EngineType::Stub => run_stub_search(pos, &limits),
            }
        }
    }

    // ===== Static helper methods for background search thread =====

    /// Static helper for parallel material search
    fn search_parallel_material_static(
        pos: &mut Position,
        limits: SearchLimits,
        args: ParallelSearchArgs,
        material_evaluator: Arc<MaterialEvaluator>,
        material_parallel_searcher: Arc<Mutex<Option<MaterialParallelSearcher>>>,
    ) -> SearchResult {
        debug!("Starting parallel material search with {} active threads", args.active_threads);

        // Take searcher out of Mutex to avoid holding lock during search
        let mut searcher = {
            let mut guard = match material_parallel_searcher.lock() {
                Ok(g) => g,
                Err(poison) => {
                    error!("Material parallel searcher mutex poisoned; recovering");
                    poison.into_inner()
                }
            };
            if guard.is_none() {
                *guard = Some(MaterialParallelSearcher::new(
                    material_evaluator,
                    args.shared_tt.clone(),
                    args.active_threads,
                    args.stop_bridge.clone(),
                ));
            }
            guard.take().expect("searcher must be Some after initialization")
        }; // Lock released here

        // Search without holding Mutex
        searcher.adjust_thread_count(args.active_threads);
        let result = searcher.search(pos, limits);

        // Put searcher back (best effort - if fails, next search will recreate)
        if let Ok(mut guard) = material_parallel_searcher.lock() {
            if guard.is_none() {
                *guard = Some(searcher);
            } // If already filled by concurrent operation, drop this instance
        }

        result
    }

    /// Static helper for parallel NNUE search
    fn search_parallel_nnue_static(
        pos: &mut Position,
        limits: SearchLimits,
        args: ParallelSearchArgs,
        nnue_evaluator: Arc<RwLock<Option<NNUEEvaluatorWrapper>>>,
        nnue_parallel_searcher: Arc<Mutex<Option<NnueParallelSearcher>>>,
    ) -> SearchResult {
        debug!("Starting parallel NNUE search with {} active threads", args.active_threads);

        // Take searcher out of Mutex to avoid holding lock during search
        let mut searcher = {
            let mut guard = match nnue_parallel_searcher.lock() {
                Ok(g) => g,
                Err(poison) => {
                    error!("NNUE parallel searcher mutex poisoned; recovering");
                    poison.into_inner()
                }
            };
            if guard.is_none() {
                let nnue_proxy = NNUEEvaluatorProxy {
                    evaluator: nnue_evaluator,
                    locals: thread_local::ThreadLocal::new(),
                };
                *guard = Some(NnueParallelSearcher::new(
                    Arc::new(nnue_proxy),
                    args.shared_tt.clone(),
                    args.active_threads,
                    args.stop_bridge.clone(),
                ));
            }

            guard.take().expect("searcher must be Some after initialization")
        }; // Lock released here

        // Search without holding Mutex
        searcher.adjust_thread_count(args.active_threads);
        let result = searcher.search(pos, limits);

        // Put searcher back (best effort - if fails, next search will recreate)
        if let Ok(mut guard) = nnue_parallel_searcher.lock() {
            if guard.is_none() {
                *guard = Some(searcher);
            }
        }

        result
    }

    /// Static helper for single-threaded material search
    fn search_single_material_static(
        pos: &mut Position,
        limits: SearchLimits,
        _material_evaluator: Arc<MaterialEvaluator>,
        _shared_tt: Arc<TranspositionTable>,
        material_searcher: Arc<Mutex<Option<MaterialSearcher>>>,
        stop_bridge: Arc<EngineStopBridge>,
    ) -> SearchResult {
        // Take searcher out of Mutex to avoid holding lock during search
        let mut searcher = match material_searcher.lock() {
            Ok(mut guard) => {
                if let Some(s) = guard.take() {
                    s
                } else {
                    error!("Material searcher not initialized");
                    return SearchResult::new(None, 0, SearchStats::default());
                }
            }
            Err(poison) => {
                error!("Material searcher mutex poisoned; recovering");
                let mut guard = poison.into_inner();
                if let Some(s) = guard.take() {
                    s
                } else {
                    error!("Material searcher not initialized after poison recovery");
                    return SearchResult::new(None, 0, SearchStats::default());
                }
            }
        }; // Lock released here

        // Publish session to stop bridge for single-threaded search
        // This ensures request_stop_immediate() can reach the search even in single-threaded mode
        if let Some(ext_stop) = limits.stop_flag.as_ref() {
            // Single-threaded searches don't have SharedSearchState or pending_work,
            // but we can still register the external stop flag
            debug!("Publishing external stop flag for single-threaded material search");
            stop_bridge.update_external_stop_flag(Some(ext_stop));
        }

        // Search without holding Mutex
        let result = searcher.search(pos, limits);

        // Put searcher back
        if let Ok(mut guard) = material_searcher.lock() {
            if guard.is_none() {
                *guard = Some(searcher);
            }
        }

        result
    }

    /// Static helper for single-threaded NNUE search
    fn search_single_nnue_static(
        pos: &mut Position,
        limits: SearchLimits,
        _nnue_evaluator: Arc<RwLock<Option<NNUEEvaluatorWrapper>>>,
        _shared_tt: Arc<TranspositionTable>,
        nnue_basic_searcher: Arc<Mutex<Option<NnueBasicSearcher>>>,
        stop_bridge: Arc<EngineStopBridge>,
    ) -> SearchResult {
        debug!("Starting NNUE search");

        // Take searcher out of Mutex to avoid holding lock during search
        let mut searcher = {
            let mut guard = match nnue_basic_searcher.lock() {
                Ok(g) => g,
                Err(poison) => {
                    error!("NNUE basic searcher mutex poisoned; recovering");
                    poison.into_inner()
                }
            };
            match guard.take() {
                Some(s) => s,
                None => {
                    error!("NNUE searcher not initialized");
                    return SearchResult::new(None, 0, SearchStats::default());
                }
            }
        }; // Lock released here

        // Publish session to stop bridge for single-threaded search
        if let Some(ext_stop) = limits.stop_flag.as_ref() {
            debug!("Publishing external stop flag for single-threaded NNUE search");
            stop_bridge.update_external_stop_flag(Some(ext_stop));
        }

        // Perform search without holding Mutex
        let result = searcher.search(pos, limits);
        debug!("NNUE search completed");

        // Put searcher back (best effort - OK if fails, will be recreated)
        if let Ok(mut guard) = nnue_basic_searcher.lock() {
            if guard.is_none() {
                *guard = Some(searcher);
            }
        }

        result
    }

    /// Static helper for single-threaded enhanced material search
    fn search_single_enhanced_material_static(
        pos: &mut Position,
        limits: SearchLimits,
        _material_evaluator: Arc<MaterialEvaluator>,
        _shared_tt: Arc<TranspositionTable>,
        material_enhanced_searcher: Arc<Mutex<Option<MaterialEnhancedSearcher>>>,
        stop_bridge: Arc<EngineStopBridge>,
    ) -> SearchResult {
        debug!("Starting Enhanced search");

        // Take searcher out of Mutex to avoid holding lock during search
        let mut searcher = {
            let mut guard = match material_enhanced_searcher.lock() {
                Ok(g) => g,
                Err(poison) => {
                    error!("Enhanced material searcher mutex poisoned; recovering");
                    poison.into_inner()
                }
            };
            match guard.take() {
                Some(s) => s,
                None => {
                    error!("Enhanced searcher not initialized");
                    return SearchResult::new(None, 0, SearchStats::default());
                }
            }
        }; // Lock released here

        // Publish session to stop bridge for single-threaded search
        if let Some(ext_stop) = limits.stop_flag.as_ref() {
            debug!("Publishing external stop flag for single-threaded enhanced material search");
            stop_bridge.update_external_stop_flag(Some(ext_stop));
        }

        // Perform search without holding Mutex
        let result = searcher.search(pos, limits);

        // Put searcher back (best effort - OK if fails, will be recreated)
        if let Ok(mut guard) = material_enhanced_searcher.lock() {
            if guard.is_none() {
                *guard = Some(searcher);
            }
        }

        result
    }

    /// Static helper for single-threaded enhanced NNUE search
    fn search_single_enhanced_nnue_static(
        pos: &mut Position,
        limits: SearchLimits,
        _nnue_evaluator: Arc<RwLock<Option<NNUEEvaluatorWrapper>>>,
        _shared_tt: Arc<TranspositionTable>,
        nnue_enhanced_searcher: Arc<Mutex<Option<NnueEnhancedSearcher>>>,
        stop_bridge: Arc<EngineStopBridge>,
    ) -> SearchResult {
        debug!("Starting Enhanced NNUE search");

        // Take searcher out of Mutex to avoid holding lock during search
        let mut searcher = {
            let mut guard = match nnue_enhanced_searcher.lock() {
                Ok(g) => g,
                Err(poison) => {
                    error!("Enhanced NNUE searcher mutex poisoned; recovering");
                    poison.into_inner()
                }
            };
            match guard.take() {
                Some(s) => s,
                None => {
                    error!("Enhanced NNUE searcher not initialized");
                    return SearchResult::new(None, 0, SearchStats::default());
                }
            }
        }; // Lock released here

        // Publish session to stop bridge for single-threaded search
        if let Some(ext_stop) = limits.stop_flag.as_ref() {
            debug!("Publishing external stop flag for single-threaded enhanced NNUE search");
            stop_bridge.update_external_stop_flag(Some(ext_stop));
        }

        // Perform search without holding Mutex
        let result = searcher.search(pos, limits);

        // Put searcher back (best effort - OK if fails, will be recreated)
        if let Ok(mut guard) = nnue_enhanced_searcher.lock() {
            if guard.is_none() {
                *guard = Some(searcher);
            }
        }

        result
    }

    /// Search for best move in position (synchronous wrapper around start_search)
    pub fn search(&mut self, pos: &mut Position, limits: SearchLimits) -> SearchResult {
        // Use the new async API internally, but wait synchronously for the result
        // This maintains backward compatibility while using the new non-blocking design
        debug!("Engine::search called (synchronous wrapper)");

        // Clone position for the background thread
        let pos_clone = pos.clone();

        // Start async search (releases Engine lock immediately after spawning thread)
        let session = self.start_search(pos_clone, limits);

        // Wait for result synchronously (for backward compatibility)
        // In the future, callers should use start_search() directly for true async behavior
        session
            .recv_result()
            .unwrap_or_else(|| SearchResult::new(None, 0, SearchStats::default()))
    }

    /// Try to get a ponder move directly from TT for the child position after `best_move`.
    /// Uses shared TT in parallel mode or the underlying searcher's TT otherwise.
    pub fn get_ponder_from_tt(
        &self,
        pos: &Position,
        best_move: crate::shogi::Move,
    ) -> Option<crate::shogi::Move> {
        // Apply best move to reach child position
        let mut child = pos.clone();
        // We don't need the undo handle here
        let _ = child.do_move(best_move);
        let child_hash = child.zobrist_hash;

        // 常に shared TT を参照
        let tt = self.shared_tt.clone();
        let entry = tt.probe_entry(child_hash, child.side_to_move)?;
        if !entry.matches(child_hash) {
            return None;
        }
        if entry.node_type() != crate::search::NodeType::Exact {
            return None;
        }
        if let Some(mv) = entry.get_move() {
            // Validate legality in child position
            if child.is_legal_move(mv) {
                return Some(mv);
            }
        }
        None
    }

    /// Set number of threads for parallel search
    pub fn set_threads(&mut self, threads: usize) {
        // Store in pending to be applied on next search
        self.pending_thread_count = Some(threads.max(1));
        info!("Thread count will be updated to {threads} on next search");
    }

    /// Apply pending thread count (called at search start)
    fn apply_pending_thread_count(&mut self) {
        if let Some(new_threads) = self.pending_thread_count.take() {
            self.num_threads = new_threads;
            self.use_parallel = new_threads > 1;

            // Clear existing parallel searchers so they'll be recreated with new thread count
            // Use try_lock to avoid blocking if searchers are in use
            if let Ok(mut guard) = self.material_parallel_searcher.try_lock() {
                *guard = None;
            } else {
                debug!("Could not acquire material parallel searcher lock, will be cleared on next recreation");
            }
            if let Ok(mut guard) = self.nnue_parallel_searcher.try_lock() {
                *guard = None;
            } else {
                debug!("Could not acquire NNUE parallel searcher lock, will be cleared on next recreation");
            }

            info!(
                "Applied thread count: {}, parallel search: {}",
                self.num_threads, self.use_parallel
            );
        }
    }

    /// Get current number of threads
    pub fn get_threads(&self) -> usize {
        self.num_threads
    }

    /// Get a diagnostic snapshot of the currently referenced TT
    pub fn tt_debug_info(&self) -> TtDebugInfo {
        use std::sync::Arc as StdArc;
        let addr = StdArc::as_ptr(&self.shared_tt) as usize;
        let size_mb = self.shared_tt.size();
        let hf = self.shared_tt.hashfull();
        let attempts = self.shared_tt.store_attempts();
        TtDebugInfo {
            addr,
            size_mb,
            hf_permille: hf,
            store_attempts: attempts,
        }
    }

    /// Run a TT roundtrip probe/store test for the given hash (diagnostics-only)
    #[cfg(any(debug_assertions, feature = "tt_metrics"))]
    pub fn tt_roundtrip_test(&self, key: u64) -> bool {
        self.shared_tt.debug_roundtrip(key)
    }

    /// Set transposition table size in MB
    pub fn set_hash_size(&mut self, size_mb: usize) {
        // Clamp to valid range (1-1024 MB)
        let clamped_size = size_mb.clamp(1, 1024);
        if clamped_size != size_mb {
            warn!("Hash size {size_mb}MB clamped to {clamped_size}MB (valid range: 1-1024)");
        }
        // Store in pending to be applied on next search
        self.pending_tt_size = Some(clamped_size);
        info!("Hash size will be updated to {clamped_size}MB on next search");
    }

    /// Apply pending TT size (called at search start)
    fn apply_pending_tt_size(&mut self) {
        if let Some(new_size) = self.pending_tt_size.take() {
            self.tt_size_mb = new_size;

            // Clear existing searchers so they'll be recreated with new TT size
            // Use try_lock to avoid blocking if searchers are in use
            if let Ok(mut guard) = self.material_searcher.try_lock() {
                *guard = None;
            } else {
                debug!(
                    "Could not acquire material searcher lock, will be cleared on next recreation"
                );
            }
            if let Ok(mut guard) = self.nnue_basic_searcher.try_lock() {
                *guard = None;
            } else {
                debug!("Could not acquire NNUE basic searcher lock, will be cleared on next recreation");
            }
            if let Ok(mut guard) = self.material_enhanced_searcher.try_lock() {
                *guard = None;
            } else {
                debug!("Could not acquire material enhanced searcher lock, will be cleared on next recreation");
            }
            if let Ok(mut guard) = self.nnue_enhanced_searcher.try_lock() {
                *guard = None;
            } else {
                debug!("Could not acquire NNUE enhanced searcher lock, will be cleared on next recreation");
            }
            if let Ok(mut guard) = self.material_parallel_searcher.try_lock() {
                *guard = None;
            } else {
                debug!("Could not acquire material parallel searcher lock, will be cleared on next recreation");
            }
            if let Ok(mut guard) = self.nnue_parallel_searcher.try_lock() {
                *guard = None;
            } else {
                debug!("Could not acquire NNUE parallel searcher lock, will be cleared on next recreation");
            }

            // Recreate shared TT with new size
            let new_tt_arc = {
                let arc0 = Arc::new(TranspositionTable::new(new_size));
                #[cfg(feature = "tt_metrics")]
                {
                    let mut arc1 = arc0;
                    if let Some(tt) = Arc::get_mut(&mut arc1) {
                        tt.enable_metrics();
                    }
                    arc1
                }
                #[cfg(not(feature = "tt_metrics"))]
                {
                    arc0
                }
            };
            self.shared_tt = new_tt_arc;

            // Recreate the single-threaded searcher for the current engine type using shared_tt
            match self.engine_type {
                EngineType::Material => {
                    if let Ok(mut guard) = self.material_searcher.lock() {
                        let mut s = MaterialSearcher::with_shared_tt(
                            self.material_evaluator.clone(),
                            self.shared_tt.clone(),
                        );
                        s.set_multi_pv(self.desired_multi_pv);
                        *guard = Some(s);
                    }
                }
                EngineType::Nnue => {
                    let nnue_proxy = NNUEEvaluatorProxy {
                        evaluator: self.nnue_evaluator.clone(),
                        locals: thread_local::ThreadLocal::new(),
                    };
                    if let Ok(mut guard) = self.nnue_basic_searcher.lock() {
                        let mut s = NnueBasicSearcher::with_shared_tt(
                            Arc::new(nnue_proxy),
                            self.shared_tt.clone(),
                        );
                        s.set_multi_pv(self.desired_multi_pv);
                        *guard = Some(s);
                    }
                }
                EngineType::Enhanced => {
                    if let Ok(mut guard) = self.material_enhanced_searcher.lock() {
                        let mut s = MaterialEnhancedSearcher::with_shared_tt(
                            self.material_evaluator.clone(),
                            self.shared_tt.clone(),
                        );
                        s.set_multi_pv(self.desired_multi_pv);
                        *guard = Some(s);
                    }
                }
                EngineType::EnhancedNnue => {
                    let nnue_proxy = NNUEEvaluatorProxy {
                        evaluator: self.nnue_evaluator.clone(),
                        locals: thread_local::ThreadLocal::new(),
                    };
                    if let Ok(mut guard) = self.nnue_enhanced_searcher.lock() {
                        let mut s = NnueEnhancedSearcher::with_shared_tt(
                            Arc::new(nnue_proxy),
                            self.shared_tt.clone(),
                        );
                        s.set_multi_pv(self.desired_multi_pv);
                        *guard = Some(s);
                    }
                }
                EngineType::Stub => {}
            }

            info!(
                "Applied hash size: {}MB; swapped shared_tt and rebound single-thread searcher",
                self.tt_size_mb
            );
        }
    }

    /// Get current hash size in MB
    pub fn get_hash_size(&self) -> usize {
        self.tt_size_mb
    }

    /// Load NNUE weights from file
    pub fn load_nnue_weights(&mut self, path: &str) -> Result<(), Box<dyn std::error::Error>> {
        // Validate that we're using NNUE engine
        if !matches!(self.engine_type, EngineType::Nnue | EngineType::EnhancedNnue) {
            return Err("Cannot load NNUE weights for non-NNUE engine".into());
        }

        // Load new NNUE evaluator from file
        let new_evaluator = NNUEEvaluatorWrapper::new(path)?;

        // Replace the evaluator
        let mut nnue_guard = self.nnue_evaluator.write();
        *nnue_guard = Some(new_evaluator);

        // 重み切替時はプロキシ/TLS を抱える検索器を再生成（安全側）
        if let Ok(mut guard) = self.nnue_parallel_searcher.lock() {
            *guard = None;
        }
        if let Ok(mut guard) = self.nnue_basic_searcher.lock() {
            *guard = None;
        }
        if let Ok(mut guard) = self.nnue_enhanced_searcher.lock() {
            *guard = None;
        }

        Ok(())
    }

    /// Return NNUE backend kind if available ("classic" or "single")
    pub fn nnue_backend_kind(&self) -> Option<&'static str> {
        let guard = self.nnue_evaluator.read();
        match &*guard {
            Some(wrapper) => Some(wrapper.backend_kind()),
            None => None,
        }
    }

    /// Get current engine type
    pub fn get_engine_type(&self) -> EngineType {
        self.engine_type
    }

    /// Set engine type
    pub fn set_engine_type(&mut self, engine_type: EngineType) {
        self.engine_type = engine_type;

        match engine_type {
            EngineType::Material => {
                // Initialize material searcher if not already
                match self.material_searcher.lock() {
                    Ok(mut searcher_guard) => {
                        if searcher_guard.is_none() {
                            *searcher_guard = Some(MaterialSearcher::with_shared_tt(
                                self.material_evaluator.clone(),
                                self.shared_tt.clone(),
                            ));
                        }
                    }
                    Err(e) => {
                        error!("Failed to lock material searcher during engine type change: {}", e);
                    }
                }
            }
            EngineType::Stub => {}
            EngineType::Nnue => {
                // Initialize NNUE evaluator if needed
                {
                    let mut nnue_guard = self.nnue_evaluator.write();
                    if nnue_guard.is_none() {
                        *nnue_guard = Some(NNUEEvaluatorWrapper::zero());
                    }
                }

                // Initialize NNUE basic searcher
                match self.nnue_basic_searcher.lock() {
                    Ok(mut searcher_guard) => {
                        if searcher_guard.is_none() {
                            let nnue_proxy = NNUEEvaluatorProxy {
                                evaluator: self.nnue_evaluator.clone(),
                                locals: thread_local::ThreadLocal::new(),
                            };
                            *searcher_guard = Some(NnueBasicSearcher::with_shared_tt(
                                Arc::new(nnue_proxy),
                                self.shared_tt.clone(),
                            ));
                        }
                    }
                    Err(e) => {
                        error!(
                            "Failed to lock NNUE basic searcher during engine type change: {}",
                            e
                        );
                    }
                }
            }
            EngineType::Enhanced => {
                // Initialize enhanced material searcher
                match self.material_enhanced_searcher.lock() {
                    Ok(mut searcher_guard) => {
                        if searcher_guard.is_none() {
                            *searcher_guard = Some(MaterialEnhancedSearcher::with_shared_tt(
                                self.material_evaluator.clone(),
                                self.shared_tt.clone(),
                            ));
                        }
                    }
                    Err(e) => {
                        error!("Failed to lock enhanced material searcher during engine type change: {}", e);
                    }
                }
            }
            EngineType::EnhancedNnue => {
                // Initialize NNUE evaluator if needed
                {
                    let mut nnue_guard = self.nnue_evaluator.write();
                    if nnue_guard.is_none() {
                        *nnue_guard = Some(NNUEEvaluatorWrapper::zero());
                    }
                }

                // Initialize enhanced NNUE searcher
                match self.nnue_enhanced_searcher.lock() {
                    Ok(mut searcher_guard) => {
                        if searcher_guard.is_none() {
                            let nnue_proxy = NNUEEvaluatorProxy {
                                evaluator: self.nnue_evaluator.clone(),
                                locals: thread_local::ThreadLocal::new(),
                            };
                            *searcher_guard = Some(NnueEnhancedSearcher::with_shared_tt(
                                Arc::new(nnue_proxy),
                                self.shared_tt.clone(),
                            ));
                        }
                    }
                    Err(e) => {
                        error!(
                            "Failed to lock enhanced NNUE searcher during engine type change: {}",
                            e
                        );
                    }
                }
            }
        }
        // Ensure new or existing searchers reflect the desired MultiPV setting
        self.set_multipv(self.desired_multi_pv);
    }

    /// Clear the transposition table (in-place)
    pub fn clear_hash(&mut self) {
        let active = self.active_searches.load(Ordering::SeqCst);
        if active != 0 {
            warn!(
                "clear_hash requested during active search; skipping (active_searches={})",
                active
            );
            return;
        }
        let tt_addr = Arc::as_ptr(&self.shared_tt) as usize;
        let tt_mb = self.shared_tt.size();
        // in-place クリア（Arc を差し替えない）
        self.shared_tt.clear_in_place();
        info!("Hash table cleared in-place (tt_addr=0x{:x} size={}MB)", tt_addr, tt_mb);
    }

    /// Reconstruct a PV from the current root position using the available TT
    fn reconstruct_root_pv_from_tt(
        &self,
        pos: &Position,
        max_depth: u8,
    ) -> Vec<crate::shogi::Move> {
        let mut tmp = pos.clone();
        crate::search::tt::reconstruct_pv_generic(self.shared_tt.as_ref(), &mut tmp, max_depth)
    }

    /// Choose final bestmove from book/committed/TT/legal
    /// - Must be lock-free and return in a few milliseconds
    /// - Ensures returned move is legal in the given position
    pub fn choose_final_bestmove(
        &self,
        pos: &Position,
        committed: Option<&crate::search::CommittedIteration>,
    ) -> FinalBest {
        use crate::movegen::MoveGenerator;

        // 1) Opening book (not integrated yet) — skipped for now

        // 2) Committed iteration PV head
        if let Some(ci) = committed {
            if let Some(&mv) = ci.pv.first() {
                // Double-check: pseudo-legal then fully legal (robust against stale/TT issues)
                if pos.is_pseudo_legal(mv) && pos.is_legal_move(mv) {
                    return FinalBest {
                        best_move: Some(mv),
                        pv: ci.pv.clone(),
                        source: FinalBestSource::Committed,
                    };
                }
            }
        }

        // 3) TT root PV reconstruction
        // Limit reconstruction depth conservatively to keep latency within a few ms
        let tt_pv = self.reconstruct_root_pv_from_tt(pos, 24);
        if let Some(&head) = tt_pv.first() {
            if pos.is_pseudo_legal(head) && pos.is_legal_move(head) {
                return FinalBest {
                    best_move: Some(head),
                    pv: tt_pv,
                    source: FinalBestSource::TT,
                };
            }
        }

        // 4) Legal fallback or resign
        // Be extra defensive: even though generate_all() returns legal moves,
        // verify with pos.is_legal_move() before emitting to avoid rare race/edge bugs.
        let gen = MoveGenerator::new();
        match gen.generate_all(pos) {
            Ok(moves) => {
                // Build a small preference: avoid king moves when not in check; prefer captures/drops
                let in_check = pos.is_in_check();
                let slice = moves.as_slice();
                // Helper closures
                let is_king_move = |m: &crate::shogi::Move| {
                    m.piece_type()
                        .or_else(|| {
                            m.from().and_then(|sq| pos.board.piece_on(sq).map(|p| p.piece_type))
                        })
                        .map(|pt| matches!(pt, crate::shogi::PieceType::King))
                        .unwrap_or(false)
                };
                let is_capture_or_drop =
                    |m: &crate::shogi::Move| m.is_drop() || m.is_capture_hint();

                // Legal filter
                let legal_moves: Vec<crate::shogi::Move> =
                    slice.iter().copied().filter(|&m| pos.is_legal_move(m)).collect();

                if !legal_moves.is_empty() {
                    // If not in check, try to avoid king moves, preferring captures/drops first
                    let chosen = if !in_check {
                        legal_moves
                            .iter()
                            .find(|m| is_capture_or_drop(m) && !is_king_move(m))
                            .copied()
                            .or_else(|| legal_moves.iter().find(|m| !is_king_move(m)).copied())
                            .unwrap_or_else(|| legal_moves[0])
                    } else {
                        // In check: any legal evasion is fine; keep first
                        legal_moves[0]
                    };

                    return FinalBest {
                        best_move: Some(chosen),
                        pv: vec![chosen],
                        source: FinalBestSource::LegalFallback,
                    };
                }
                warn!("generate_all returned no independently legal moves; resigning");
                FinalBest {
                    best_move: None,
                    pv: Vec::new(),
                    source: FinalBestSource::Resign,
                }
            }
            Err(e) => {
                warn!("move generation failed in final fallback: {}", e);
                FinalBest {
                    best_move: None,
                    pv: Vec::new(),
                    source: FinalBestSource::Resign,
                }
            }
        }
    }
}

/// Proxy evaluator for thread-safe NNUE access（TLS でスレッド毎に独立状態）
struct NNUEEvaluatorProxy {
    // 重みの“源泉”：ロード/切替のためだけに使う
    evaluator: Arc<RwLock<Option<NNUEEvaluatorWrapper>>>,
    // スレッドローカルの Wrapper（差分 Acc スタックはここに保持）
    locals: thread_local::ThreadLocal<std::cell::RefCell<NNUEEvaluatorWrapper>>,
}

impl NNUEEvaluatorProxy {
    // no direct local() helper (unused)
    #[inline]
    fn ensure_local(&self) -> Option<std::cell::RefMut<'_, NNUEEvaluatorWrapper>> {
        if let Some(cell) = self.locals.get() {
            return Some(cell.borrow_mut());
        }
        let g = self.evaluator.read();
        let base = g.as_ref()?;
        let cell = self.locals.get_or(|| std::cell::RefCell::new(base.fork_stateless()));
        Some(cell.borrow_mut())
    }
    #[inline]
    fn ensure_local_ro(&self) -> Option<std::cell::Ref<'_, NNUEEvaluatorWrapper>> {
        if let Some(cell) = self.locals.get() {
            return Some(cell.borrow());
        }
        let g = self.evaluator.read();
        let base = g.as_ref()?;
        let cell = self.locals.get_or(|| std::cell::RefCell::new(base.fork_stateless()));
        Some(cell.borrow())
    }
}

impl Evaluator for NNUEEvaluatorProxy {
    fn evaluate(&self, pos: &Position) -> i32 {
        if let Some(l) = self.ensure_local_ro() {
            return l.evaluate(pos);
        }
        warn!("NNUE evaluator not initialized");
        0
    }

    fn on_set_position(&self, pos: &Position) {
        if let Some(mut l) = self.ensure_local() {
            l.set_position(pos);
        }
    }

    fn on_do_move(&self, pre_pos: &Position, mv: crate::shogi::Move) {
        if let Some(mut l) = self.ensure_local() {
            let _ = l.do_move(pre_pos, mv);
        }
    }

    fn on_undo_move(&self) {
        if let Some(mut l) = self.ensure_local() {
            l.undo_move();
        }
    }

    fn on_do_null_move(&self, pre_pos: &Position) {
        // 差分Acc運用のため、null move でもスタック整合を保つ
        if let Some(mut l) = self.ensure_local() {
            let _ = l.do_move(pre_pos, crate::shogi::Move::null());
        }
    }

    fn on_undo_null_move(&self) {
        if let Some(mut l) = self.ensure_local() {
            l.undo_move();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // use std::sync::atomic::AtomicBool; // Commented out - used in test that's temporarily disabled
    // use std::sync::Arc; // Commented out - used in test that's temporarily disabled
    // use std::thread; // Commented out - used in test that's temporarily disabled
    use crate::shogi::{Color, Piece, PieceType, Square};
    use std::time::Duration;

    // Test constants (for compatibility with existing tests)
    const INITIAL_PHASE_TOTAL: u16 = 52;

    #[test]
    #[ignore] // Requires large stack size due to engine initialization
    fn test_material_engine() {
        let mut pos = Position::startpos();
        let mut engine = Engine::new(EngineType::Material);
        let limits = SearchLimits::builder().depth(3).fixed_time_ms(1000).build();

        let result = engine.search(&mut pos, limits);

        assert!(result.best_move.is_some());
        assert!(result.stats.nodes > 0);
    }

    #[test]
    #[cfg_attr(not(feature = "large-stack-tests"), ignore)]
    fn test_nnue_engine() {
        // This test requires a large stack size due to NNUE initialization
        let mut pos = Position::startpos();
        let mut engine = Engine::new(EngineType::Nnue);
        let limits = SearchLimits::builder().depth(3).fixed_time_ms(1000).build();

        let result = engine.search(&mut pos, limits);

        assert!(result.best_move.is_some());
        assert!(result.stats.nodes > 0);
    }

    #[test]
    #[ignore] // Requires large stack size due to Enhanced engine initialization
    fn test_enhanced_engine() {
        let mut pos = Position::startpos();
        let mut engine = Engine::new(EngineType::Enhanced);
        let limits = SearchLimits::builder().depth(3).fixed_time_ms(1000).build();

        let result = engine.search(&mut pos, limits);

        assert!(result.best_move.is_some());
        assert!(result.stats.nodes > 0);
        assert!(result.stats.elapsed < Duration::from_secs(2));
    }

    #[test]
    #[cfg_attr(not(feature = "large-stack-tests"), ignore)]
    fn test_enhanced_nnue_engine() {
        // This test requires a large stack size due to NNUE initialization
        // Run with: RUST_MIN_STACK=8388608 cargo test -- --ignored
        let mut pos = Position::startpos();
        let mut engine = Engine::new(EngineType::EnhancedNnue);
        let limits = SearchLimits::builder().depth(3).fixed_time_ms(1000).build();

        let result = engine.search(&mut pos, limits);

        assert!(result.best_move.is_some());
        assert!(result.stats.nodes > 0);
        assert!(result.stats.elapsed < Duration::from_secs(2));
    }

    // TODO: Fix this test after making Engine mutable
    /*
    #[test]
    #[ignore] // Requires large stack size due to Enhanced engine initialization
    fn test_enhanced_engine_with_stop_flag() {
        let pos = Position::startpos();
        let engine = Arc::new(Engine::new(EngineType::Enhanced));
        let stop_flag = Arc::new(AtomicBool::new(false));

        // Start search in a thread
        let engine_clone = engine.clone();
        let pos_clone = pos.clone();
        let stop_flag_clone = stop_flag.clone();

        let handle = thread::spawn(move || {
            let mut pos_mut = pos_clone;
            let limits = SearchLimits::builder()
                .depth(10)
                .fixed_time_ms(10000)
                .stop_flag(stop_flag_clone)
                .build();
            engine_clone.search(&mut pos_mut, limits)
        });

        // Set stop flag after short delay
        thread::sleep(Duration::from_millis(100));
        stop_flag.store(true, std::sync::atomic::Ordering::Release);

        // Wait for search to complete
        let result = handle.join().unwrap();

        // Should have stopped early (within 2000ms accounting for CI variability and Enhanced engine complexity)
        // Enhanced engine performs more complex operations than basic engines
        assert!(
            result.stats.elapsed < Duration::from_millis(2000),
            "Search took too long to stop: {:?}",
            result.stats.elapsed
        );
    }
    */

    #[test]
    #[ignore] // Requires large stack size due to NNUE initialization
    fn test_engine_type_switching_basic() {
        let mut engine = Engine::new(EngineType::Material);

        // Initially material engine
        assert!(matches!(engine.engine_type, EngineType::Material));

        // Switch to Enhanced
        engine.set_engine_type(EngineType::Enhanced);
        assert!(matches!(engine.engine_type, EngineType::Enhanced));

        // Can search with Enhanced engine
        let mut pos = Position::startpos();
        let limits = SearchLimits::builder().depth(2).fixed_time_ms(100).build();
        let result = engine.search(&mut pos, limits);
        assert!(result.best_move.is_some());

        // Switch back to Material
        engine.set_engine_type(EngineType::Material);
        assert!(matches!(engine.engine_type, EngineType::Material));

        let limits2 = SearchLimits::builder().depth(2).fixed_time_ms(100).build();
        let result2 = engine.search(&mut pos, limits2);
        assert!(result2.best_move.is_some());
    }

    #[test]
    #[cfg_attr(not(feature = "large-stack-tests"), ignore)]
    fn test_engine_type_switching_with_nnue() {
        // Separate test for NNUE due to stack size requirements
        let mut engine = Engine::new(EngineType::Material);

        // Switch to NNUE
        engine.set_engine_type(EngineType::Nnue);
        assert!(matches!(engine.engine_type, EngineType::Nnue));

        // Can still search
        let mut pos = Position::startpos();
        let limits = SearchLimits::builder().depth(2).fixed_time_ms(100).build();
        let result = engine.search(&mut pos, limits);
        assert!(result.best_move.is_some());
    }

    #[test]
    #[cfg_attr(not(feature = "large-stack-tests"), ignore)]
    fn test_engine_type_switching_with_enhanced_nnue() {
        // Separate test for EnhancedNnue due to stack size requirements
        // Run with: RUST_MIN_STACK=8388608 cargo test -- --ignored
        let mut engine = Engine::new(EngineType::Material);

        // Switch to EnhancedNnue
        engine.set_engine_type(EngineType::EnhancedNnue);
        assert!(matches!(engine.engine_type, EngineType::EnhancedNnue));

        // Can search with EnhancedNnue engine
        let mut pos = Position::startpos();
        let limits = SearchLimits::builder().depth(2).fixed_time_ms(100).build();
        let result = engine.search(&mut pos, limits);
        assert!(result.best_move.is_some());
    }

    #[test]
    #[ignore] // Requires large stack size due to NNUE initialization
    fn test_load_nnue_weights_wrong_engine_type() {
        let mut engine = Engine::new(EngineType::Material);
        let result = engine.load_nnue_weights("dummy.nnue");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().to_string(), "Cannot load NNUE weights for non-NNUE engine");
    }

    #[test]
    fn test_choose_final_bestmove_prefers_non_king_when_not_in_check() {
        // Black to move, both a king move and a capture by a silver are legal.
        // Expect: choose_final_bestmove picks non-king capture (LegalFallback heuristic).
        let mut pos = Position::empty();
        // Kings
        pos.board.put_piece(
            Square::from_usi_chars('5', 'i').unwrap(),
            Piece::new(PieceType::King, Color::Black),
        );
        pos.board.put_piece(
            Square::from_usi_chars('5', 'a').unwrap(),
            Piece::new(PieceType::King, Color::White),
        );
        // Black silver can capture a pawn
        pos.board.put_piece(
            Square::from_usi_chars('4', 'h').unwrap(),
            Piece::new(PieceType::Silver, Color::Black),
        );
        pos.board.put_piece(
            Square::from_usi_chars('5', 'g').unwrap(),
            Piece::new(PieceType::Pawn, Color::White),
        ); // target for capture 4h-5g
        pos.side_to_move = Color::Black;

        // Ensure non-zero hash to avoid TT probe panic in tests
        pos.hash = 1;
        pos.zobrist_hash = 1;
        let eng = Engine::new(EngineType::Material);
        let res = eng.choose_final_bestmove(&pos, None);
        assert!(res.best_move.is_some());
        let mv = res.best_move.unwrap();
        // The move should not be a king move when a capture exists
        let is_king = mv
            .piece_type()
            .or_else(|| mv.from().and_then(|sq| pos.board.piece_on(sq).map(|p| p.piece_type)))
            .map(|pt| matches!(pt, PieceType::King))
            .unwrap_or(false);
        assert!(
            !is_king,
            "Should avoid king move when not in check; got {}",
            crate::usi::move_to_usi(&mv)
        );
    }

    #[test]
    fn test_choose_final_bestmove_skips_illegal_committed() {
        // Committed PV contains an illegal move; engine should ignore and fallback to legal.
        let mut pos = Position::empty();
        // Kings
        pos.board.put_piece(
            Square::from_usi_chars('5', 'i').unwrap(),
            Piece::new(PieceType::King, Color::Black),
        );
        pos.board.put_piece(
            Square::from_usi_chars('5', 'a').unwrap(),
            Piece::new(PieceType::King, Color::White),
        );
        // One simple legal move: Black pawn can advance 7g7f
        pos.board.put_piece(
            Square::from_usi_chars('7', 'g').unwrap(),
            Piece::new(PieceType::Pawn, Color::Black),
        );
        pos.side_to_move = Color::Black;

        // Illegal committed move (move from empty square)
        let illegal = crate::usi::parse_usi_move("3a3b").unwrap();
        let committed = crate::search::CommittedIteration {
            depth: 1,
            seldepth: None,
            score: 0,
            pv: vec![illegal],
            node_type: crate::search::NodeType::Exact,
            nodes: 0,
            elapsed: std::time::Duration::from_millis(0),
        };

        pos.hash = 1;
        pos.zobrist_hash = 1;
        let eng = Engine::new(EngineType::Material);
        let res = eng.choose_final_bestmove(&pos, Some(&committed));
        assert!(res.best_move.is_some());
        assert_ne!(res.best_move.unwrap().to_u16(), illegal.to_u16());
    }

    // TODO: Fix this test after making Engine mutable
    /*
    #[test]
    #[cfg_attr(not(feature = "large-stack-tests"), ignore)]
    fn test_parallel_engine_execution() {
        // Create a shared engine with NNUE
        let engine = Arc::new(Engine::new(EngineType::Nnue));

        let mut handles = vec![];

        // Spawn multiple threads that use the engine concurrently
        for thread_id in 0..4 {
            let engine_clone = engine.clone();
            let handle = thread::spawn(move || {
                let mut pos = Position::startpos();
                let limits = SearchLimits::builder().depth(2).fixed_time_ms(50).build();

                // Each thread performs a search
                let result = engine_clone.search(&mut pos, limits);

                log::debug!("Thread {} completed search with {} nodes", thread_id, result.stats.nodes);

                // Verify we got a valid result
                assert!(result.best_move.is_some());
                assert!(result.stats.nodes > 0);

                result.stats.nodes
            });
            handles.push(handle);
        }

        // Wait for all threads to complete
        let mut total_nodes = 0u64;
        for handle in handles {
            let nodes = handle.join().unwrap();
            total_nodes += nodes;
        }

        log::debug!("Total nodes searched across all threads: {total_nodes}");
        assert!(total_nodes > 0);
    }
    */

    #[test]
    fn test_game_phase_detection() {
        // Opening phase (by move count)
        let mut pos = Position::startpos();
        assert_eq!(detect_game_phase(&pos, pos.ply as u32, Profile::Search), GamePhase::Opening);

        // Still opening because of high material score
        pos.ply = 50;
        assert_eq!(detect_game_phase(&pos, pos.ply as u32, Profile::Search), GamePhase::Opening);

        // With new system, high ply affects phase detection
        pos.ply = 150;
        // New system considers both material and ply
        let phase = detect_game_phase(&pos, pos.ply as u32, Profile::Search);
        eprintln!("Position at ply 150 phase: {:?}", phase);
        // Might be MiddleGame due to ply influence
        assert!(
            phase == GamePhase::MiddleGame || phase == GamePhase::Opening,
            "Expected Opening or MiddleGame, got {:?}",
            phase
        );

        // Test with actual endgame position
        let mut endgame_pos = Position::empty();
        endgame_pos.ply = 150;
        endgame_pos.board.put_piece(
            Square::from_usi_chars('5', 'i').unwrap(),
            Piece::new(PieceType::King, Color::Black),
        );
        endgame_pos.board.put_piece(
            Square::from_usi_chars('5', 'a').unwrap(),
            Piece::new(PieceType::King, Color::White),
        );
        assert_eq!(
            detect_game_phase(&endgame_pos, endgame_pos.ply as u32, Profile::Search),
            GamePhase::EndGame
        );

        // Test repetition scenario: high ply but full material
        let mut repetition_pos = Position::startpos();
        repetition_pos.ply = 200; // Very high move count
                                  // With new system, very high ply pushes toward endgame despite material
        let phase = detect_game_phase(&repetition_pos, repetition_pos.ply as u32, Profile::Search);
        eprintln!("Repetition position (ply 200) phase: {:?}", phase);
        // Could be MiddleGame or EndGame due to high ply
        assert!(
            phase == GamePhase::MiddleGame || phase == GamePhase::EndGame,
            "Expected MiddleGame or EndGame for high ply, got {:?}",
            phase
        );
    }

    #[test]
    fn test_calculate_active_threads() {
        let mut engine = Engine::new(EngineType::Material);
        engine.num_threads = 8;

        // Opening - all threads (by move count)
        let mut pos = Position::startpos();
        assert_eq!(engine.calculate_active_threads(&pos), 8);

        // Middle game - all threads (material score overrides)
        pos.ply = 50;
        assert_eq!(engine.calculate_active_threads(&pos), 8);

        // With new system, phase depends on material and ply
        pos.ply = 150;
        let threads = engine.calculate_active_threads(&pos);
        eprintln!("Threads at ply 150: {}", threads);
        // Should still use all threads unless EndGame
        assert_eq!(threads, 8);

        // Create actual endgame position with few pieces
        let mut endgame_pos = Position::empty();
        endgame_pos.ply = 150;
        endgame_pos.board.put_piece(
            Square::from_usi_chars('5', 'i').unwrap(),
            Piece::new(PieceType::King, Color::Black),
        );
        endgame_pos.board.put_piece(
            Square::from_usi_chars('5', 'a').unwrap(),
            Piece::new(PieceType::King, Color::White),
        );
        endgame_pos.board.put_piece(
            Square::from_usi_chars('1', 'i').unwrap(),
            Piece::new(PieceType::Rook, Color::Black),
        );
        // Endgame should use half threads
        assert_eq!(engine.calculate_active_threads(&endgame_pos), 4);
    }

    #[test]
    fn test_pending_thread_count() {
        let mut engine = Engine::new(EngineType::Material);

        // Initial state
        assert_eq!(engine.num_threads, 1);
        assert_eq!(engine.pending_thread_count, None);

        // Set threads - should be pending
        engine.set_threads(4);
        assert_eq!(engine.num_threads, 1); // Not applied yet
        assert_eq!(engine.pending_thread_count, Some(4));

        // Apply pending count
        engine.apply_pending_thread_count();
        assert_eq!(engine.num_threads, 4); // Now applied
        assert_eq!(engine.pending_thread_count, None);
        assert!(engine.use_parallel);
    }

    #[test]
    #[cfg_attr(not(feature = "large-stack-tests"), ignore)]
    fn test_concurrent_weight_loading() {
        // Create a mutable engine
        let mut engine = Engine::new(EngineType::Nnue);

        // Try to load weights (will fail since file doesn't exist)
        let result1 = engine.load_nnue_weights("nonexistent1.nnue");
        assert!(result1.is_err());

        // Engine type switching is thread-safe through mutex
        engine.set_engine_type(EngineType::Material);
        engine.set_engine_type(EngineType::Nnue);

        // Try another load
        let result2 = engine.load_nnue_weights("nonexistent2.nnue");
        assert!(result2.is_err());
    }

    #[test]
    fn test_hash_size_configuration() {
        let mut engine = Engine::new(EngineType::Material);

        // Initial hash size should be default
        assert_eq!(engine.get_hash_size(), 1024);

        // Set new hash size
        engine.set_hash_size(32);

        // Should still be 1024 until next search
        assert_eq!(engine.get_hash_size(), 1024);

        // After applying pending changes
        engine.apply_pending_tt_size();
        assert_eq!(engine.get_hash_size(), 32);
    }

    #[test]
    fn test_multipv_persistence_clear_hash() {
        let mut engine = Engine::new(EngineType::Material);
        engine.set_multipv_persistent(3);
        engine.clear_hash();

        // Inspect the underlying searcher directly to verify MultiPV persistence
        {
            let guard = engine.material_searcher.lock().expect("lock material searcher");
            let s = guard.as_ref().expect("material searcher should exist");
            assert_eq!(s.multi_pv(), 3);
        }
    }

    #[test]
    fn test_multipv_persistence_tt_resize() {
        let mut engine = Engine::new(EngineType::Material);
        engine.set_multipv_persistent(4);
        engine.set_hash_size(32); // pending until next apply
        engine.apply_pending_tt_size();

        // Inspect the recreated searcher
        {
            let guard = engine.material_searcher.lock().expect("lock material searcher");
            let s = guard.as_ref().expect("material searcher should exist");
            assert_eq!(s.multi_pv(), 4);
        }
    }

    #[test]
    fn test_multipv_persistence_engine_type_switch() {
        let mut engine = Engine::new(EngineType::Material);
        engine.set_multipv_persistent(2);
        engine.set_engine_type(EngineType::Enhanced);

        // Inspect the enhanced searcher
        {
            let guard = engine.material_enhanced_searcher.lock().expect("lock enhanced searcher");
            let s = guard.as_ref().expect("enhanced searcher should exist");
            assert_eq!(s.multi_pv(), 2);
        }
    }

    #[test]
    fn test_tt_hashfull_permille_fallback_shared_tt() {
        let engine_type = EngineType::Material;
        let engine = Engine::new(engine_type);
        // Simulate uninitialized searcher of current engine type
        if let Ok(mut guard) = engine.material_searcher.lock() {
            *guard = None;
        }
        // Should not panic and should return a value from shared TT
        let hf = engine.tt_hashfull_permille();
        assert!(hf <= 1000);
    }

    // Note: A higher-level USI-side test would verify bestmove ponder emission.

    #[test]
    fn test_game_phase_edge_cases() {
        // Test 1: Empty board except kings
        let mut pos = Position::empty();
        pos.board.put_piece(
            Square::from_usi_chars('5', 'i').unwrap(),
            Piece::new(PieceType::King, Color::Black),
        );
        pos.board.put_piece(
            Square::from_usi_chars('5', 'a').unwrap(),
            Piece::new(PieceType::King, Color::White),
        );
        assert_eq!(detect_game_phase(&pos, pos.ply as u32, Profile::Search), GamePhase::EndGame);

        // Test 2: Only one side has pieces
        pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()] = 18; // 18 pawns in hand
                                                                                      // Pawns have weight 0, so still endgame
        assert_eq!(detect_game_phase(&pos, pos.ply as u32, Profile::Search), GamePhase::EndGame);

        // Add valuable pieces to hand
        pos.hands[Color::Black as usize][PieceType::Rook.hand_index().unwrap()] = 2; // 2 rooks in hand
                                                                                     // 2 rooks * 4 = 8, normalized: (8 * 128) / 52 = 19
                                                                                     // Still below PHASE_ENDGAME_THRESHOLD (32)
        assert_eq!(detect_game_phase(&pos, pos.ply as u32, Profile::Search), GamePhase::EndGame);

        // Add more pieces to cross into middle game
        pos.hands[Color::Black as usize][PieceType::Bishop.hand_index().unwrap()] = 2; // 2 bishops
        pos.hands[Color::Black as usize][PieceType::Gold.hand_index().unwrap()] = 4; // 4 golds
                                                                                     // Total: 2*4 + 2*4 + 4*3 = 28, normalized: (28 * 128) / 52 = 68
        assert_eq!(detect_game_phase(&pos, pos.ply as u32, Profile::Search), GamePhase::MiddleGame);
    }

    #[test]
    fn test_initial_phase_total_compile_time() {
        // Verify the compile-time calculation is correct
        assert_eq!(INITIAL_PHASE_TOTAL, 52);

        // Manually verify the calculation
        let manual_total = 2 * 4 +  // Rook: 2 pieces * weight 4
            2 * 4 +  // Bishop: 2 pieces * weight 4
            4 * 3 +  // Gold: 4 pieces * weight 3
            4 * 2 +  // Silver: 4 pieces * weight 2
            4 * 2 +  // Knight: 4 pieces * weight 2
            4 * 2; // Lance: 4 pieces * weight 2
        assert_eq!(manual_total, 52);
        assert_eq!(manual_total, INITIAL_PHASE_TOTAL);
    }
}
