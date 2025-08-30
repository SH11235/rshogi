#![cfg(test)]

use crate::bestmove_emitter::BestmoveEmitter; // ensure emitter is linked
use crate::command_handler::CommandContext;
use crate::handlers::go::handle_go_command;
use crate::handlers::ponder::handle_ponder_hit;
use crate::state::SearchState;
use crate::usi::output::{test_info_from, test_info_len};
use crate::usi::GoParams;
use crate::worker::{lock_or_recover_adapter, WorkerMessage};
use crossbeam_channel::unbounded;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn setup_ctx() -> (
    Arc<Mutex<crate::engine_adapter::EngineAdapter>>,
    CommandContext<'static>,
) {
    std::env::set_var("USI_DRY_RUN", "1");

    // Engine and initial position
    let engine = Arc::new(Mutex::new(crate::engine_adapter::EngineAdapter::new()));
    {
        let mut adapter = engine.lock().unwrap();
        adapter.set_position(true, None, &[]).unwrap();
    }

    // Channels and flags
    let (tx, rx) = unbounded::<WorkerMessage>();
    let stop_flag = Arc::new(AtomicBool::new(false));

    // Context fields
    static mut WORKER_HANDLE: Option<std::thread::JoinHandle<()>> = None;
    static mut SEARCH_STATE: SearchState = SearchState::Idle;
    static mut SEARCH_ID_COUNTER: u64 = 0;
    static mut CURRENT_SEARCH_ID: u64 = 0;
    static mut CURRENT_SEARCH_IS_PONDER: bool = false;
    static mut CURRENT_SESSION: Option<()> = None;
    static mut CURRENT_BESTMOVE_EMITTER: Option<BestmoveEmitter> = None;
    static mut CURRENT_FINALIZED_FLAG: Option<Arc<AtomicBool>> = None;
    static mut CURRENT_STOP_FLAG: Option<Arc<AtomicBool>> = None;
    static mut POSITION_STATE: Option<crate::types::PositionState> = None;
    static mut LAST_PARTIAL_RESULT: Option<(String, u8, i32)> = None;
    static mut PRE_SESSION_FALLBACK: Option<String> = None;
    static mut PRE_SESSION_FALLBACK_HASH: Option<u64> = None;
    static mut CURRENT_COMMITTED: Option<engine_core::search::CommittedIteration> = None;
    static mut LAST_BESTMOVE_SENT_AT: Option<Instant> = None;
    static mut LAST_GO_BEGIN_AT: Option<Instant> = None;
    static mut FINAL_PV_INJECTED: bool = false;
    static mut HARD_DEADLINE_TAKEN: bool = false;
    static mut ROOT_LEGAL_MOVES: Option<Vec<String>> = None;

    let ctx = unsafe {
        CommandContext {
            engine: &engine,
            stop_flag: &stop_flag,
            worker_tx: &tx,
            worker_rx: &rx,
            worker_handle: &mut WORKER_HANDLE,
            search_state: &mut SEARCH_STATE,
            search_id_counter: &mut SEARCH_ID_COUNTER,
            current_search_id: &mut CURRENT_SEARCH_ID,
            current_search_is_ponder: &mut CURRENT_SEARCH_IS_PONDER,
            current_session: &mut CURRENT_SESSION,
            current_committed: &mut CURRENT_COMMITTED,
            current_bestmove_emitter: &mut CURRENT_BESTMOVE_EMITTER,
            current_finalized_flag: &mut CURRENT_FINALIZED_FLAG,
            current_stop_flag: &mut CURRENT_STOP_FLAG,
            allow_null_move: false,
            position_state: &mut POSITION_STATE,
            program_start: Instant::now(),
            last_partial_result: &mut LAST_PARTIAL_RESULT,
            root_legal_moves: &mut ROOT_LEGAL_MOVES,
            hard_deadline_taken: &mut HARD_DEADLINE_TAKEN,
            pre_session_fallback: &mut PRE_SESSION_FALLBACK,
            pre_session_fallback_hash: &mut PRE_SESSION_FALLBACK_HASH,
            last_bestmove_sent_at: &mut LAST_BESTMOVE_SENT_AT,
            last_go_begin_at: &mut LAST_GO_BEGIN_AT,
            final_pv_injected: &mut FINAL_PV_INJECTED,
        }
    };

    (engine, ctx)
}

fn wait_for_bestmove_sent_since(start_idx: usize, timeout_ms: u64) -> Vec<String> {
    let start = Instant::now();
    loop {
        let infos = test_info_from(start_idx);
        if infos
            .iter()
            .any(|s| s.contains("kind=bestmove_sent") && s.contains("bestmove="))
        {
            return infos;
        }
        if start.elapsed().as_millis() as u64 > timeout_ms {
            return infos;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn test_time_boundary_byoyomi_timelimit() {
    let (_engine, mut ctx) = setup_ctx();

    // Byoyomi: already in byoyomi (main time 0 for side to move)
    let params = GoParams {
        btime: Some(0),
        wtime: Some(0),
        byoyomi: Some(300), // 300ms period
        periods: Some(1),
        ..Default::default()
    };

    let start_idx = test_info_len();
    handle_go_command(params, &mut ctx).unwrap();
    let infos = wait_for_bestmove_sent_since(start_idx, 3000);
    assert!(
        infos.iter().any(|s| s.contains("kind=bestmove_sent") && s.contains("stop_reason=TimeLimit")),
        "bestmove_sent with TimeLimit not found. Infos: {:?}",
        infos
    );
}

#[test]
fn test_time_boundary_fixedtime_timelimit() {
    let (_engine, mut ctx) = setup_ctx();

    // Fixed time per move (short)
    let params = GoParams {
        movetime: Some(100), // 100ms
        ..Default::default()
    };

    let start_idx = test_info_len();
    handle_go_command(params, &mut ctx).unwrap();
    let infos = wait_for_bestmove_sent_since(start_idx, 3000);
    assert!(
        infos.iter().any(|s| s.contains("kind=bestmove_sent") && s.contains("stop_reason=TimeLimit")),
        "bestmove_sent with TimeLimit not found for fixedtime. Infos: {:?}",
        infos
    );
}

#[test]
fn test_time_boundary_ponderhit_converts_and_emits() {
    let (engine, mut ctx) = setup_ctx();

    // Start pondering with an inner fixed-time
    let params = GoParams {
        ponder: true,
        movetime: Some(400),
        ..Default::default()
    };

    let start_idx = test_info_len();
    handle_go_command(params, &mut ctx).unwrap();

    // Ensure no bestmove is sent during ponder (short wait)
    std::thread::sleep(Duration::from_millis(100));
    let infos_mid = test_info_from(start_idx);
    assert!(
        !infos_mid
            .iter()
            .any(|s| s.contains("kind=bestmove_sent")),
        "bestmove should not be sent during ponder: {:?}",
        infos_mid
    );

    // Trigger ponderhit
    handle_ponder_hit(&mut ctx).unwrap();
    // Verify adapter no longer in ponder
    {
        let adapter = lock_or_recover_adapter(&engine);
        assert!(!adapter.is_stochastic_ponder());
    }

    // Now bestmove should be sent within time
    let infos = wait_for_bestmove_sent_since(start_idx, 3000);
    assert!(
        infos.iter().any(|s| s.contains("kind=bestmove_sent")),
        "bestmove_sent not found after ponderhit. Infos: {:?}",
        infos
    );
}

#[test]
fn test_time_boundary_near_hard_emits() {
    let (_engine, mut ctx) = setup_ctx();

    // Very tight fixed time – ensure we still emit a move
    let params = GoParams {
        movetime: Some(60),
        ..Default::default()
    };

    let start_idx = test_info_len();
    handle_go_command(params, &mut ctx).unwrap();
    let infos = wait_for_bestmove_sent_since(start_idx, 3000);
    assert!(
        infos.iter().any(|s| s.contains("kind=bestmove_sent")),
        "bestmove_sent not found under near-hard timing. Infos: {:?}",
        infos
    );
}
