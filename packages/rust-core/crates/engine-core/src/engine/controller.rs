//! Main engine implementation that integrates search and evaluation
//!
//! Provides a simple interface for using different evaluators with the search engine

use crate::search::types::StopInfo;
use crate::time_management::{
    detect_game_phase_for_time, TimeControl, TimeLimits, TimeManager, TimeState,
};
use crate::{
    engine::session::SearchSession,
    evaluation::evaluate::{Evaluator, MaterialEvaluator},
    evaluation::nnue::NNUEEvaluatorWrapper,
    search::ab::{ClassicBackend, SearchProfile},
    search::api::{InfoEventCallback, SearcherBackend},
    search::parallel::{EngineStopBridge, StopController},
    search::{SearchLimits, SearchResult, SearchStats, TranspositionTable},
    Position,
};
use log::{debug, info, warn};
use parking_lot::RwLock;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use std::time::Instant;

fn search_profile_for_engine_type(engine_type: EngineType) -> SearchProfile {
    match engine_type {
        EngineType::Enhanced => SearchProfile::enhanced_material(),
        EngineType::EnhancedNnue => SearchProfile::enhanced_nnue(),
        EngineType::Nnue => SearchProfile::basic_nnue(),
        EngineType::Material => SearchProfile::basic_material(),
    }
}

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
    shared_tt: Arc<TranspositionTable>,
    num_threads: usize,
    pending_thread_count: Option<usize>,
    tt_size_mb: usize,
    pending_tt_size: Option<usize>,
    desired_multi_pv: u8,
    active_searches: Arc<AtomicUsize>,
    stop_bridge: Arc<EngineStopBridge>,
    _stop_ctrl: StopController,
    session_counter: u64,
    backend: Option<Arc<dyn SearcherBackend + Send + Sync>>,
}

impl Engine {
    fn build_time_manager_for_search(
        pos: &Position,
        limits: &SearchLimits,
    ) -> Option<Arc<TimeManager>> {
        if limits.is_ponder {
            return None;
        }

        match limits.time_control {
            TimeControl::Infinite | TimeControl::FixedNodes { .. } => return None,
            TimeControl::Ponder(_) => return None,
            _ => {}
        }

        let tm_limits = TimeLimits {
            time_control: limits.time_control.clone(),
            moves_to_go: limits.moves_to_go,
            depth: limits.depth.map(|d| d as u32),
            nodes: limits.nodes,
            time_parameters: limits.time_parameters,
            random_time_ms: limits.random_time_ms,
        };
        let game_phase = detect_game_phase_for_time(pos, pos.ply as u32);
        let manager = TimeManager::new(&tm_limits, pos.side_to_move, pos.ply as u32, game_phase);
        Some(Arc::new(manager))
    }

    /// Create new engine with specified type
    pub fn new(engine_type: EngineType) -> Self {
        let material_evaluator = Arc::new(MaterialEvaluator);
        let default_tt_size = 1024; // Default TT size in MB

        let stop_bridge = Arc::new(EngineStopBridge::new());
        let stop_ctrl = StopController::new();

        let nnue_evaluator = Arc::new(RwLock::new(None));
        if matches!(engine_type, EngineType::Nnue | EngineType::EnhancedNnue) {
            let mut guard = nnue_evaluator.write();
            *guard = Some(NNUEEvaluatorWrapper::zero());
        }

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

        let mut engine = Engine {
            engine_type,
            material_evaluator,
            nnue_evaluator,
            shared_tt,
            num_threads: 1,
            pending_thread_count: None,
            tt_size_mb: default_tt_size,
            pending_tt_size: None,
            desired_multi_pv: 1,
            active_searches: Arc::new(AtomicUsize::new(0)),
            stop_bridge,
            _stop_ctrl: stop_ctrl,
            session_counter: 0,
            backend: None,
        };

        let profile = search_profile_for_engine_type(engine_type);
        profile.apply_runtime_defaults();

        engine.rebuild_backend();
        engine
    }

    fn ensure_nnue_evaluator_initialized(&mut self) {
        if matches!(self.engine_type, EngineType::Nnue | EngineType::EnhancedNnue) {
            let mut guard = self.nnue_evaluator.write();
            if guard.is_none() {
                *guard = Some(NNUEEvaluatorWrapper::zero());
            }
        }
    }

    fn maybe_drop_nnue_when_inactive(&mut self) {
        if !matches!(self.engine_type, EngineType::Nnue | EngineType::EnhancedNnue) {
            let mut guard = self.nnue_evaluator.write();
            if guard.is_some() {
                *guard = None;
                info!("NNUE weights dropped after switching to {:?}", self.engine_type);
            }
        }
    }

    fn time_state_for_manager(tm: &TimeManager, elapsed_ms: u64) -> TimeState {
        match tm.time_control() {
            TimeControl::Byoyomi { main_time_ms, .. } => {
                if let Some((_, _, in_byoyomi)) = tm.get_byoyomi_state() {
                    if in_byoyomi {
                        return TimeState::Byoyomi { main_left_ms: 0 };
                    }
                }

                if main_time_ms == 0 {
                    TimeState::Byoyomi { main_left_ms: 0 }
                } else {
                    let remaining = main_time_ms.saturating_sub(elapsed_ms);
                    if remaining > 0 {
                        TimeState::Main {
                            main_left_ms: remaining,
                        }
                    } else {
                        TimeState::Byoyomi { main_left_ms: 0 }
                    }
                }
            }
            _ => TimeState::NonByoyomi,
        }
    }

    fn build_backend(&mut self) -> Option<Arc<dyn SearcherBackend + Send + Sync>> {
        match self.engine_type {
            EngineType::Material | EngineType::Enhanced => {
                let profile = search_profile_for_engine_type(self.engine_type);
                Some(Arc::new(ClassicBackend::with_profile_and_tt(
                    Arc::clone(&self.material_evaluator),
                    Arc::clone(&self.shared_tt),
                    profile,
                )))
            }
            EngineType::Nnue | EngineType::EnhancedNnue => {
                self.ensure_nnue_evaluator_initialized();
                let proxy = Arc::new(NNUEEvaluatorProxy {
                    evaluator: self.nnue_evaluator.clone(),
                    locals: thread_local::ThreadLocal::new(),
                });
                let profile = search_profile_for_engine_type(self.engine_type);
                Some(Arc::new(ClassicBackend::with_profile_and_tt(
                    proxy,
                    Arc::clone(&self.shared_tt),
                    profile,
                )))
            }
        }
    }

    fn rebuild_backend(&mut self) {
        self.backend = self.build_backend();
        if let Some(backend) = self.backend.as_ref() {
            backend.update_threads(self.num_threads.max(1));
            backend.update_hash(self.tt_size_mb.max(1));
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

    /// Obtain a clone of the StopController for snapshot/stop-info consumers.
    pub fn stop_controller_handle(&self) -> StopController {
        self._stop_ctrl.clone()
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
    pub fn set_multipv(&mut self, k: u8) {
        self.desired_multi_pv = k.clamp(1, 20);
    }

    /// Persist desired MultiPV and apply to current searchers
    pub fn set_multipv_persistent(&mut self, k: u8) {
        self.set_multipv(k);
    }

    /// Set pruning teacher profile across searchers
    pub fn set_teacher_profile(&mut self, profile: crate::search::types::TeacherProfile) {
        let _ = profile;
        // ClassicBackend では未使用。将来の教師データ生成向け導入時に橋渡しを行う。
    }

    /// Reset state for a fresh position: TT, heuristics, and thread policy.
    /// Ensures reproducibility for teacher data generation.
    pub fn reset_for_position(&mut self) {
        self.clear_hash();
        self.pending_thread_count = None;
        self.num_threads = 1;
        if let Some(backend) = self.backend.as_ref() {
            backend.update_threads(1);
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
    /// This is the preferred method for normal game play, as it follows the yaneura-ou
    /// design pattern of non-blocking search initiation.
    pub fn start_search(&mut self, pos: Position, mut limits: SearchLimits) -> SearchSession {
        let session_id = self.next_session_id();
        debug!("Starting async search session {session_id}");

        limits.start_time = Instant::now();

        // Attach time manager if this search requires time control
        let time_manager = Self::build_time_manager_for_search(&pos, &limits);
        if let Some(ref tm) = time_manager {
            limits.time_manager = Some(Arc::clone(tm));
        }

        // Attach stop controller for downstream finalize coordination
        limits.stop_controller = Some(Arc::clone(&self.stop_bridge));

        let mut base_stop_info = StopInfo::default();
        if let Some(ref tm) = time_manager {
            base_stop_info.soft_limit_ms = tm.soft_limit_ms();
            base_stop_info.hard_limit_ms = tm.hard_limit_ms();
        } else if let Some(deadlines) = limits.fallback_deadlines {
            base_stop_info.soft_limit_ms = deadlines.soft_limit_ms;
            base_stop_info.hard_limit_ms = deadlines.hard_limit_ms;
        } else if let Some(limit) = limits.time_limit() {
            let ms = limit.as_millis() as u64;
            base_stop_info.soft_limit_ms = ms;
            base_stop_info.hard_limit_ms = ms;
        }
        self._stop_ctrl.prime_stop_info(base_stop_info.clone());
        self.stop_bridge.prime_stop_info(base_stop_info.clone());

        limits.session_id = session_id;
        self.apply_pending_thread_count();
        self.apply_pending_tt_size();

        if self.backend.is_none() {
            self.rebuild_backend();
        }

        self.shared_tt.bump_age();

        limits.multipv = self.desired_multi_pv.max(1);

        if limits.stop_flag.is_none() {
            limits.stop_flag = Some(Arc::new(AtomicBool::new(false)));
        }

        let stop_flag_ref = limits.stop_flag.as_ref();
        self._stop_ctrl.publish_session(stop_flag_ref, session_id);
        self.stop_bridge.publish_session(stop_flag_ref, session_id);

        self.stop_bridge.update_external_stop_flag(limits.stop_flag.as_ref());
        self._stop_ctrl.update_external_stop_flag(limits.stop_flag.as_ref());

        let backend =
            Arc::clone(self.backend.as_ref().expect("backend must be initialized after rebuild"));

        let legacy_info = limits.info_callback.clone();
        let legacy_info_string = limits.info_string_callback.clone();
        let stop_ctrl = self._stop_ctrl.clone();
        let root_hash = pos.zobrist_hash();
        let sid = session_id;
        let wants_events = legacy_info.is_some() || legacy_info_string.is_some();
        let event_cb: Option<InfoEventCallback> = if wants_events {
            let legacy_cb = legacy_info.clone();
            let cb_str = legacy_info_string.clone();
            Some(Arc::new(move |evt: crate::search::api::InfoEvent| {
                use crate::search::api::{AspirationOutcome, InfoEvent};
                match evt {
                    InfoEvent::PV { line } => {
                        stop_ctrl.publish_root_line(sid, root_hash, line.as_ref());
                        if let Some(cb) = &legacy_cb {
                            cb(InfoEvent::PV {
                                line: Arc::clone(&line),
                            });
                        }
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
            }) as InfoEventCallback)
        } else {
            None
        };
        let backend_task =
            backend.start_async(pos, limits, event_cb, Arc::clone(&self.active_searches));
        let (stop_handle, result_rx, join_handle) = backend_task.into_parts();
        SearchSession::new(session_id, result_rx, join_handle, stop_handle, time_manager)
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
        let result = session
            .recv_result()
            .unwrap_or_else(|| SearchResult::new(None, 0, SearchStats::default()));

        if let Some(tm) = session.time_manager() {
            let elapsed_ms = result.stats.elapsed.as_millis() as u64;
            let time_state = Self::time_state_for_manager(&tm, elapsed_ms);
            tm.update_after_move(elapsed_ms, time_state);
        }

        result
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
            let new_threads = new_threads.max(1);
            self.num_threads = new_threads;
            if let Some(backend) = self.backend.as_ref() {
                backend.update_threads(new_threads);
            }
            info!("Applied thread count: {}", self.num_threads);
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
        if self.active_searches.load(Ordering::SeqCst) != 0 {
            // Defer until current searches complete
            self.pending_tt_size = Some(clamped_size);
            info!("Hash size change to {clamped_size}MB deferred until active searches finish");
            return;
        }

        self.pending_tt_size = None;
        self.tt_size_mb = clamped_size;
        let new_tt_arc = {
            let arc0 = Arc::new(TranspositionTable::new(clamped_size));
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
        self.rebuild_backend();

        info!("Applied hash size immediately: {}MB", self.tt_size_mb);
    }

    /// Apply pending TT size (called at search start)
    fn apply_pending_tt_size(&mut self) {
        if let Some(new_size) = self.pending_tt_size.take() {
            self.tt_size_mb = new_size;
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

            self.rebuild_backend();

            info!("Applied hash size: {}MB; rebuilt ClassicBackend bindings", self.tt_size_mb);
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
        {
            let mut nnue_guard = self.nnue_evaluator.write();
            *nnue_guard = Some(new_evaluator);
        }

        // 重み切替後は ClassicBackend を再構築して TLS をリセット
        self.rebuild_backend();

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
        let profile = search_profile_for_engine_type(engine_type);
        profile.apply_runtime_defaults();
        if matches!(self.engine_type, EngineType::Nnue | EngineType::EnhancedNnue) {
            self.ensure_nnue_evaluator_initialized();
        } else {
            self.maybe_drop_nnue_when_inactive();
        }
        self.rebuild_backend();
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
                let is_tactical =
                    |m: &crate::shogi::Move| m.is_drop() || m.is_capture_hint() || m.is_promote();

                // Legal filter
                let legal_moves: Vec<crate::shogi::Move> =
                    slice.iter().copied().filter(|&m| pos.is_legal_move(m)).collect();

                if !legal_moves.is_empty() {
                    // If not in check, try to avoid king moves, preferring captures/drops first
                    let chosen = if !in_check {
                        legal_moves
                            .iter()
                            .find(|m| is_tactical(m) && !is_king_move(m))
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
    use crate::shogi::Position;
    use crate::usi::move_to_usi;

    #[test]
    fn legal_fallback_prefers_promotion_as_tactical() {
        let engine = Engine::new(EngineType::Material);
        let pos = Position::from_sfen("4k4/9/9/2P6/9/9/9/9/4K4 b - 1").unwrap();

        let best = engine.choose_final_bestmove(&pos, None);
        let mv = best.best_move.expect("expected fallback move");
        assert!(
            mv.is_promote(),
            "fallback should prefer promotion as tactical: {}",
            move_to_usi(&mv)
        );
        assert_eq!(best.source, FinalBestSource::LegalFallback);
    }
}
