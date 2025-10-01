//! Main engine implementation that integrates search and evaluation
//!
//! Provides a simple interface for using different evaluators with the search engine

use crate::search::parallel::ParallelSearcher;
use crate::search::types::StopInfo;
use crate::{
    engine::session::SearchSession,
    evaluation::evaluate::{Evaluator, MaterialEvaluator},
    evaluation::nnue::NNUEEvaluatorWrapper,
    search::api::{InfoEventCallback, SearcherBackend, StubBackend},
    search::parallel::{EngineStopBridge, StopController},
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
    // New stop controller facade（移行期間は併用）
    _stop_ctrl: StopController,
    // Session ID counter for async search
    session_counter: u64,
    // Optional new backend (Phase 1). When Some, Engine will delegate search to backend.
    backend: Option<Arc<dyn SearcherBackend + Send + Sync>>,
}

impl Engine {
    /// Create new engine with specified type
    pub fn new(engine_type: EngineType) -> Self {
        let material_evaluator = Arc::new(MaterialEvaluator);
        let default_tt_size = 1024; // Default TT size in MB

        let stop_bridge = Arc::new(EngineStopBridge::new());
        let stop_ctrl = StopController::new();

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

        let backend: Option<Arc<dyn SearcherBackend + Send + Sync>> = match engine_type {
            EngineType::Stub => Some(Arc::new(StubBackend::new())),
            EngineType::Material | EngineType::Enhanced => {
                Some(Arc::new(crate::search::ab::ClassicBackend::with_tt(
                    material_evaluator.clone(),
                    shared_tt.clone(),
                )))
            }
            _ => None,
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
            _stop_ctrl: stop_ctrl,
            session_counter: 0,
            backend,
        }
    }

    /// Issue an immediate stop request to the currently running search without acquiring locks.
    pub fn request_stop_immediate(&self) {
        self.stop_bridge.request_stop();
        // Mirror via new controller (安全に重ね打ち)
        self._stop_ctrl.request_stop();
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

        // Prime StopInfo snapshot with static limits if available
        let mut base_stop_info = StopInfo::default();
        if let Some(deadlines) = limits.fallback_deadlines {
            base_stop_info.soft_limit_ms = deadlines.soft_limit_ms;
            base_stop_info.hard_limit_ms = deadlines.hard_limit_ms;
        } else if let Some(limit) = limits.time_limit() {
            let ms = limit.as_millis() as u64;
            base_stop_info.soft_limit_ms = ms;
            base_stop_info.hard_limit_ms = ms;
        }
        self._stop_ctrl.prime_stop_info(base_stop_info.clone());
        self.stop_bridge.prime_stop_info(base_stop_info.clone());

        // Set session ID in limits for OOB coordination
        limits.session_id = session_id;
        // Publish via new stop controller（外部停止フラグを合わせて登録）
        self._stop_ctrl.publish_session(limits.stop_flag.as_ref(), session_id);

        // Apply pending configuration
        self.apply_pending_thread_count();
        self.apply_pending_tt_size();

        // Bump TT age for new search epoch
        self.shared_tt.bump_age();

        // Create result channel
        let (tx, rx) = mpsc::channel();

        // Backend path (Phase 1). If present, delegate search to backend.
        if let Some(backend) = self.backend.as_ref() {
            // Publish external stop flag
            self.stop_bridge.update_external_stop_flag(limits.stop_flag.as_ref());
            self._stop_ctrl.update_external_stop_flag(limits.stop_flag.as_ref());
            // Count active
            self.active_searches.fetch_add(1, Ordering::SeqCst);
            let active_searches = self.active_searches.clone();
            let pos_clone = pos.clone();
            let limits_clone = limits.clone();
            let backend_arc = Arc::clone(backend);

            // Build InfoEvent adapter from legacy callbacks in limits
            let legacy_info = limits.info_callback.clone();
            let legacy_info_string = limits.info_string_callback.clone();
            let stop_ctrl = self._stop_ctrl.clone();
            let root_hash = pos_clone.zobrist_hash();
            let sid = session_id;
            let event_cb: Option<InfoEventCallback> = legacy_info.as_ref().map(|cb| {
                let cb = cb.clone();
                let cb_str = legacy_info_string.clone();
                Arc::new(move |evt: crate::search::api::InfoEvent| {
                    use crate::search::api::{AspirationOutcome, InfoEvent};
                    match evt {
                        InfoEvent::PV { line } => {
                            // Publish snapshot via StopController (best line only)
                            stop_ctrl.publish_root_line(sid, root_hash, &line);
                            let depth_u8 = (line.depth as u8).min(127);
                            let nodes = line.nodes.unwrap_or(0);
                            let elapsed =
                                std::time::Duration::from_millis(line.time_ms.unwrap_or(0));
                            let score = line.score_cp;
                            let pv_owned: Vec<crate::shogi::Move> =
                                line.pv.iter().copied().collect();
                            cb(depth_u8, score, nodes, elapsed, &pv_owned, line.bound);
                        }
                        InfoEvent::Depth { depth, seldepth } => {
                            if let Some(s) = &cb_str {
                                s(&format!("depth {} seldepth {}", depth, seldepth));
                            }
                        }
                        InfoEvent::CurrMove { mv, number } => {
                            if let Some(s) = &cb_str {
                                s(&format!(
                                    "currmove {} currmovenumber {}",
                                    crate::usi::move_to_usi(&mv),
                                    number
                                ));
                            }
                        }
                        InfoEvent::Hashfull(h) => {
                            if let Some(s) = &cb_str {
                                s(&format!("hashfull {}", h));
                            }
                        }
                        InfoEvent::Aspiration {
                            outcome,
                            old_alpha,
                            old_beta,
                            new_alpha,
                            new_beta,
                        } => {
                            let tag = match outcome {
                                AspirationOutcome::FailHigh => "fail-high",
                                AspirationOutcome::FailLow => "fail-low",
                            };
                            if let Some(s) = &cb_str {
                                s(&format!(
                                    "aspiration {} old=[{},{}] new=[{},{}]",
                                    tag, old_alpha, old_beta, new_alpha, new_beta
                                ));
                            }
                        }
                        InfoEvent::String(msg) => {
                            if let Some(s) = &cb_str {
                                s(&msg);
                            }
                        }
                    }
                }) as InfoEventCallback
            });
            let handle = thread::Builder::new()
                .name(format!("engine-backend-search-{}", session_id))
                .spawn(move || {
                    let _guard = ActiveSearchGuard(active_searches);
                    let result = backend_arc.think_blocking(&pos_clone, &limits_clone, event_cb);
                    let _ = tx.send(result);
                })
                .expect("spawn backend search thread");
            return SearchSession::new(session_id, rx, Some(handle));
        }

        // Fast path: Stub searcher (no tree search, deterministic legal move)
        if self.engine_type == EngineType::Stub {
            let stop_bridge = self.stop_bridge.clone();
            let pos_clone = pos.clone();
            let tx2 = tx.clone();
            // Publish external stop flag (if provided)
            self.stop_bridge.update_external_stop_flag(limits.stop_flag.as_ref());
            self._stop_ctrl.update_external_stop_flag(limits.stop_flag.as_ref());
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
        self._stop_ctrl.update_external_stop_flag(limits.stop_flag.as_ref());

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
        let mut result = if args.use_parallel {
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
                    args.stop_bridge.clone(),
                ),
                EngineType::Nnue => Self::search_single_nnue_static(
                    pos,
                    limits,
                    args.nnue_evaluator,
                    args.shared_tt,
                    args.nnue_basic_searcher,
                    args.stop_bridge.clone(),
                ),
                EngineType::Enhanced => Self::search_single_enhanced_material_static(
                    pos,
                    limits,
                    args.material_evaluator,
                    args.shared_tt,
                    args.material_enhanced_searcher,
                    args.stop_bridge.clone(),
                ),
                EngineType::EnhancedNnue => Self::search_single_enhanced_nnue_static(
                    pos,
                    limits,
                    args.nnue_evaluator,
                    args.shared_tt,
                    args.nnue_enhanced_searcher,
                    args.stop_bridge.clone(),
                ),
                EngineType::Stub => run_stub_search(pos, &limits),
            }
        };

        if let Some(si) = args.stop_bridge.try_read_stop_info() {
            result.stop_info = Some(si);
        }

        result
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
            // New controller mirrors the registration
            // (stop_ctrl is not passed through to keep signature stable during migration)
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
            // mirrored via StopController at engine level
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
            // mirrored via StopController at engine level
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
            self.shared_tt = new_tt_arc.clone();

            // Rebind backend (ClassicAB/Stub) to the new shared TT so that probe/store/hashfull are consistent
            self.backend = match self.engine_type {
                EngineType::Stub => Some(Arc::new(StubBackend::new())),
                EngineType::Material | EngineType::Enhanced => {
                    Some(Arc::new(crate::search::ab::ClassicBackend::with_tt(
                        self.material_evaluator.clone(),
                        new_tt_arc.clone(),
                    )))
                }
                _ => self.backend.take(),
            };

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
        // Backend assignment for Phase 1
        self.backend = match engine_type {
            EngineType::Stub => Some(Arc::new(StubBackend::new())),
            EngineType::Material | EngineType::Enhanced => {
                Some(Arc::new(crate::search::ab::ClassicBackend::with_tt(
                    self.material_evaluator.clone(),
                    self.shared_tt.clone(),
                )))
            }
            _ => None,
        };

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
            EngineType::Stub => {
                if let Ok(mut guard) = self.material_searcher.lock() {
                    *guard = None;
                }
                if let Ok(mut guard) = self.nnue_basic_searcher.lock() {
                    *guard = None;
                }
                if let Ok(mut guard) = self.material_enhanced_searcher.lock() {
                    *guard = None;
                }
                if let Ok(mut guard) = self.nnue_enhanced_searcher.lock() {
                    *guard = None;
                }
                if let Ok(mut guard) = self.material_parallel_searcher.lock() {
                    *guard = None;
                }
                if let Ok(mut guard) = self.nnue_parallel_searcher.lock() {
                    *guard = None;
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
