use std::sync::atomic::AtomicBool;
use std::sync::Condvar;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Instant;

use engine_core::engine::controller::{Engine, EngineType};
use engine_core::engine::session::SearchSession;
use engine_core::search::parallel::{FinalizerMsg, StopController};
use engine_core::shogi::Position;
use engine_core::time_management::{TimeControl, TimeManager, TimeState};
use engine_core::Color;

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
    // Main-loop watchdog polling interval (ms)
    pub watchdog_poll_ms: u64,
    // 純秒読みでGUIの厳格締切より少し手前で確実に返すための追加リード（ms）
    // network_delay2_ms に加算して最終化を前倒しする。手番側 main=0 でも適用。
    // 既定: 300ms
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
            watchdog_poll_ms: 2,
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
    pub rtime: Option<u64>,
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
    // Async search session (non-blocking)
    pub search_session: Option<SearchSession>,
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
    pub stop_controller: Arc<StopController>,
    // OOB finalize channel (from engine-core time manager via StopController)
    pub finalizer_rx: Option<mpsc::Receiver<FinalizerMsg>>,
    // Current engine-core session id (epoch) for matching finalize requests
    pub current_session_core_id: Option<u64>,
    pub idle_sync: Arc<IdleSync>,
    // Deadlines for OOB finalize enforcement (computed at search start)
    pub deadline_hard: Option<Instant>,
    pub deadline_near: Option<Instant>,
    pub deadline_near_notified: bool,
    pub active_time_manager: Option<Arc<TimeManager>>,
}

impl EngineState {
    pub fn new() -> Self {
        // Initialize engine-core static tables once
        engine_core::init::init_all_tables_once();

        let mut engine = Engine::new(EngineType::Material);
        engine.set_threads(1);
        engine.set_hash_size(1024);
        let stop_controller = engine.stop_controller_handle();
        // Register OOB finalizer channel（StopController 経由に統一）
        let (fin_tx, fin_rx) = mpsc::channel();
        stop_controller.register_finalizer(fin_tx.clone());

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
            search_session: None,
            current_is_stochastic_ponder: false,
            current_is_ponder: false,
            stoch_suppress_result: false,
            pending_research_after_ponderhit: false,
            last_go_params: None,
            current_root_hash: None,
            current_search_id: 0,
            bestmove_emitted: false,
            current_time_control: None,
            stop_controller,
            finalizer_rx: Some(fin_rx),
            current_session_core_id: None,
            idle_sync,
            deadline_hard: None,
            deadline_near: None,
            deadline_near_notified: false,
            active_time_manager: None,
        }
    }

    /// 探索終了後に TimeManager::update_after_move へ渡す TimeState を計算する
    ///
    /// Byoyomi では go コマンドの残り時間と経過時間から推定し、それ以外は NonByoyomi を返す。
    pub fn time_state_for_update(&self, elapsed_ms: u64) -> TimeState {
        if let Some(TimeControl::Byoyomi { main_time_ms, .. }) = &self.current_time_control {
            let side_to_move = self.position.side_to_move;
            let from_go = self.last_go_params.as_ref().and_then(|gp| match side_to_move {
                Color::Black => gp.btime,
                Color::White => gp.wtime,
            });

            let main_before = from_go.unwrap_or(*main_time_ms);

            if main_before == 0 {
                return TimeState::Byoyomi { main_left_ms: 0 };
            }

            let remaining = main_before.saturating_sub(elapsed_ms);
            if remaining > 0 {
                return TimeState::Main {
                    main_left_ms: remaining,
                };
            }

            return TimeState::Byoyomi { main_left_ms: 0 };
        }

        TimeState::NonByoyomi
    }

    /// 現在保持している TimeManager に消費時間を通知した上で破棄する
    pub fn finalize_time_manager(&mut self) {
        if let Some(tm) = self.active_time_manager.take() {
            let elapsed_ms = tm.elapsed_ms();
            let time_state = self.time_state_for_update(elapsed_ms);
            tm.update_after_move(elapsed_ms, time_state);
        }
    }

    #[inline]
    pub fn notify_idle(&self) {
        self.idle_sync.notify_all();
    }
}

#[derive(Default)]
pub struct IdleSync {
    condvar: Condvar,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_state_defaults_to_main_without_go_params() {
        let mut state = EngineState::new();
        state.current_time_control = Some(TimeControl::Byoyomi {
            main_time_ms: 60_000,
            byoyomi_ms: 1_000,
            periods: 3,
        });
        state.last_go_params = None;

        match state.time_state_for_update(1_000) {
            TimeState::Main { main_left_ms } => assert_eq!(main_left_ms, 59_000),
            other => panic!("unexpected time state: {:?}", other),
        }
    }

    #[test]
    fn time_state_respects_zero_main_time_from_go() {
        let mut state = EngineState::new();
        state.current_time_control = Some(TimeControl::Byoyomi {
            main_time_ms: 60_000,
            byoyomi_ms: 1_000,
            periods: 3,
        });
        let mut go = GoParams::default();
        go.btime = Some(0);
        go.wtime = Some(0);
        state.last_go_params = Some(go);

        match state.time_state_for_update(500) {
            TimeState::Byoyomi { main_left_ms } => assert_eq!(main_left_ms, 0),
            other => panic!("unexpected time state: {:?}", other),
        }
    }
}

impl IdleSync {
    pub fn notify_all(&self) {
        self.condvar.notify_all();
    }
}
