use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::Condvar;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use engine_core::engine::controller::{Engine, EngineType};
use engine_core::search::parallel::EngineStopBridge;
use engine_core::search::parallel::FinalizerMsg;
use engine_core::shogi::Position;
use engine_core::time_management::TimeControl;

#[derive(Debug)]
pub struct ReaperJob {
    pub handle: thread::JoinHandle<()>,
    pub label: String,
}

#[derive(Clone, Debug)]
pub struct UsiOptions {
    // Core engine settings
    pub hash_mb: usize,
    pub threads: usize,
    pub ponder: bool,
    pub engine_type: EngineType,
    pub eval_file: Option<String>,

    // Time parameters
    pub overhead_ms: u64,
    pub network_delay_ms: u64,
    pub network_delay2_ms: u64,
    pub min_think_ms: u64,

    // Byoyomi and policy extras
    pub byoyomi_periods: u32,
    pub byoyomi_early_finish_ratio: u8, // 50-95
    pub byoyomi_safety_ms: u64,         // hard-limit減算
    pub pv_stability_base: u64,         // 10-200
    pub pv_stability_slope: u64,        // 0-20
    pub slow_mover_pct: u8,             // 50-200
    pub max_time_ratio_pct: u32,        // 100-800 (% → x/100)
    pub move_horizon_trigger_ms: u64,
    pub move_horizon_min_moves: u32,

    // Others
    pub stochastic_ponder: bool,
    pub force_terminate_on_hard_deadline: bool, // 受理のみ（非推奨）
    pub mate_early_stop: bool,
    // Stop bounded wait time
    pub stop_wait_ms: u64,
    // 純秒読みでGUIの厳格締切より少し手前で確実に返すための追加リード（ms）
    // network_delay2_ms に加算して最終化を前倒しする。既定: 300ms
    pub byoyomi_deadline_lead_ms: u64,
    // MultiPV lines
    pub multipv: u8,
    // Policy: gameover時にもbestmoveを送るか
    pub gameover_sends_bestmove: bool,
    // Fail-safe guard (parallel) を有効化するか
    pub fail_safe_guard: bool,
    // SIMD clamp (runtime). None = Auto
    pub simd_max_level: Option<String>,
    // NNUE SIMD clamp (runtime). None = Auto
    pub nnue_simd: Option<String>,
}

impl Default for UsiOptions {
    fn default() -> Self {
        Self {
            hash_mb: 1024,
            threads: 1,
            ponder: true,
            engine_type: EngineType::Material,
            eval_file: None,
            overhead_ms: 50,
            network_delay_ms: 120,
            network_delay2_ms: 800,
            min_think_ms: 200,
            byoyomi_periods: 1,
            byoyomi_early_finish_ratio: 80,
            byoyomi_safety_ms: 500,
            pv_stability_base: 80,
            pv_stability_slope: 5,
            slow_mover_pct: 100,
            max_time_ratio_pct: 500,
            move_horizon_trigger_ms: 0,
            move_horizon_min_moves: 0,
            stochastic_ponder: false,
            force_terminate_on_hard_deadline: true,
            mate_early_stop: true,
            stop_wait_ms: 0,
            byoyomi_deadline_lead_ms: 300,
            multipv: 1,
            gameover_sends_bestmove: false,
            fail_safe_guard: false,
            simd_max_level: None,
            nnue_simd: None,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct GoParams {
    pub depth: Option<u32>,
    pub nodes: Option<u64>,
    pub movetime: Option<u64>,
    pub infinite: bool,
    pub ponder: bool,
    pub btime: Option<u64>,
    pub wtime: Option<u64>,
    pub binc: Option<u64>,
    pub winc: Option<u64>,
    pub byoyomi: Option<u64>,
    pub periods: Option<u32>,
    pub moves_to_go: Option<u32>,
}

pub struct EngineState {
    pub engine: Arc<Mutex<Engine>>,
    pub position: Position,
    // Canonicalized last position command parts (for Stochastic_Ponder)
    pub pos_from_startpos: bool,
    pub pos_sfen: Option<String>,
    pub pos_moves: Vec<String>,
    pub opts: UsiOptions,
    // runtime flags
    pub searching: bool,
    pub stop_flag: Option<Arc<AtomicBool>>,
    pub ponder_hit_flag: Option<Arc<AtomicBool>>,
    pub worker: Option<thread::JoinHandle<()>>,
    pub result_rx: Option<mpsc::Receiver<(u64, engine_core::search::SearchResult)>>,
    // Stochastic Ponder control
    pub current_is_stochastic_ponder: bool,
    pub current_is_ponder: bool,
    pub stoch_suppress_result: bool,
    pub pending_research_after_ponderhit: bool,
    pub last_go_params: Option<GoParams>,
    // Session root hash for stale-result guard
    pub current_root_hash: Option<u64>,
    pub current_search_id: u64,
    // Ensure we emit at most one bestmove per go-session
    pub bestmove_emitted: bool,
    // Current (inner) time control for stop/gameover policy decisions
    pub current_time_control: Option<TimeControl>,
    // Reaper: background joiner for detached worker threads
    pub reaper_tx: Option<mpsc::Sender<ReaperJob>>,
    pub reaper_handle: Option<std::thread::JoinHandle<()>>,
    pub reaper_queue_len: Arc<AtomicUsize>,
    pub stop_bridge: Arc<EngineStopBridge>,
    // OOB finalize channel (from engine-core time manager via StopBridge)
    pub finalizer_rx: Option<mpsc::Receiver<FinalizerMsg>>,
    // Current engine-core session id (epoch) for matching finalize requests
    pub current_session_core_id: Option<u64>,
    pub idle_sync: Arc<IdleSync>,
}

impl EngineState {
    pub fn new() -> Self {
        // Initialize engine-core static tables once
        engine_core::init::init_all_tables_once();

        let mut engine = Engine::new(EngineType::Material);
        engine.set_threads(1);
        engine.set_hash_size(1024);
        let stop_bridge = engine.stop_bridge_handle();
        // Register OOB finalizer channel
        let (fin_tx, fin_rx) = mpsc::channel();
        stop_bridge.register_finalizer(fin_tx);

        let idle_sync = Arc::new(IdleSync::default());

        Self {
            engine: Arc::new(Mutex::new(engine)),
            position: Position::startpos(),
            pos_from_startpos: true,
            pos_sfen: None,
            pos_moves: Vec::new(),
            opts: UsiOptions::default(),
            searching: false,
            stop_flag: None,
            ponder_hit_flag: None,
            worker: None,
            result_rx: None,
            current_is_stochastic_ponder: false,
            current_is_ponder: false,
            stoch_suppress_result: false,
            pending_research_after_ponderhit: false,
            last_go_params: None,
            current_root_hash: None,
            current_search_id: 0,
            bestmove_emitted: false,
            current_time_control: None,
            reaper_tx: None,
            reaper_handle: None,
            reaper_queue_len: Arc::new(AtomicUsize::new(0)),
            stop_bridge,
            finalizer_rx: Some(fin_rx),
            current_session_core_id: None,
            idle_sync,
        }
    }

    #[inline]
    pub fn notify_idle(&self) {
        self.idle_sync.notify_all();
    }

    pub fn idle_status(&self) -> IdleStateSnapshot {
        let worker_active = self.worker.is_some();
        let reaper_pending = self.reaper_queue_len.load(std::sync::atomic::Ordering::Acquire);
        let stop_snapshot = self.stop_bridge.snapshot();
        IdleStateSnapshot {
            worker_active,
            reaper_pending,
            pending_work_items: stop_snapshot.pending_work_items,
            active_workers: stop_snapshot.active_workers,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct IdleStateSnapshot {
    pub worker_active: bool,
    pub reaper_pending: usize,
    pub pending_work_items: u64,
    pub active_workers: usize,
}

impl IdleStateSnapshot {
    #[inline]
    pub fn is_idle(&self) -> bool {
        !self.worker_active
            && self.reaper_pending == 0
            && self.pending_work_items == 0
            && self.active_workers == 0
    }
}

#[derive(Default)]
pub struct IdleSync {
    lock: Mutex<()>,
    condvar: Condvar,
}

impl IdleSync {
    pub fn notify_all(&self) {
        self.condvar.notify_all();
    }

    pub fn wait_timeout(&self, timeout: std::time::Duration) {
        let guard = self.lock.lock().unwrap();
        let _ = self.condvar.wait_timeout(guard, timeout).unwrap();
    }
}
