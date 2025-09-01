//! Tests for immediate bestmove emission on SearchFinished

use crate::bestmove_emitter::BestmoveEmitter;
use crate::command_handler::CommandContext;
use crate::state::SearchState;
use crate::types::PositionState;
use crate::worker::WorkerMessage;
use crossbeam_channel::{bounded, Receiver, Sender};
use engine_core::search::types::{StopInfo, TerminationReason};
use engine_core::search::CommittedIteration;
use engine_core::Position;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_context() -> (
        Arc<Mutex<crate::engine_adapter::EngineAdapter>>,
        Sender<WorkerMessage>,
        Receiver<WorkerMessage>,
        SearchState,
        u64,
        u64,
        bool,
        Option<BestmoveEmitter>,
        Option<Arc<AtomicBool>>,
        Option<Arc<AtomicBool>>,
        Option<CommittedIteration>,
        Option<StopInfo>,
    ) {
        let engine_adapter = Arc::new(Mutex::new(crate::engine_adapter::EngineAdapter::new()));
        let (tx, rx) = bounded(100);
        let search_state = SearchState::Searching;
        let search_id_counter = 1;
        let current_search_id = 1;
        let current_search_is_ponder = false;
        let current_bestmove_emitter = Some(BestmoveEmitter::new(1));
        let current_finalized_flag = Some(Arc::new(AtomicBool::new(false)));
        let current_stop_flag = Some(Arc::new(AtomicBool::new(false)));

        // Create a committed iteration for testing
        let committed = Some(CommittedIteration {
            depth: 5,
            seldepth: Some(7),
            score: 100,
            pv: vec![],
            node_type: engine_core::search::types::NodeType::Exact,
            nodes: 10000,
            elapsed: std::time::Duration::from_millis(100),
        });

        let pending_stop_info = None;

        (
            engine_adapter,
            tx,
            rx,
            search_state,
            search_id_counter,
            current_search_id,
            current_search_is_ponder,
            current_bestmove_emitter,
            current_finalized_flag,
            current_stop_flag,
            committed,
            pending_stop_info,
        )
    }

    #[test]
    fn test_searchfinished_emits_bestmove_immediately() {
        std::env::set_var("USI_DRY_RUN", "1");
        let start_info_count = crate::usi::output::test_info_len();

        let (
            engine,
            worker_tx,
            worker_rx,
            mut search_state,
            mut search_id_counter,
            mut current_search_id,
            mut current_search_is_ponder,
            mut current_bestmove_emitter,
            mut current_finalized_flag,
            mut current_stop_flag,
            mut current_committed,
            mut pending_stop_info,
        ) = create_test_context();

        // Setup minimal position state
        let mut position_state = Some(PositionState {
            cmd_canonical: "position startpos".to_string(),
            move_len: 0,
            root_hash: 0,
            sfen_snapshot: "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1"
                .to_string(),
            stored_at: Instant::now(),
        });

        // Setup engine with a position
        {
            let mut adapter = engine.lock().unwrap();
            let pos = Position::startpos();
            adapter.set_raw_position(pos);
        }

        let stop_flag = Arc::new(AtomicBool::new(false));
        let mut worker_handle = None;
        let allow_null_move = true;
        let program_start = Instant::now();
        let mut last_partial_result = None;
        let mut search_start_time = Some(Instant::now());
        let mut latest_nodes = 0;
        let mut soft_limit_ms_ctx = 500;
        let mut root_legal_moves = None;
        let mut hard_deadline_taken = false;
        let mut pre_session_fallback = None;
        let mut pre_session_fallback_hash = None;
        let mut last_bestmove_sent_at = None;
        let mut last_go_begin_at = None;
        let mut final_pv_injected = false;
        let mut pending_returned_engine = None;

        let mut legacy_session: Option<()> = None;
        let mut ctx = CommandContext {
            engine: &engine,
            stop_flag: &stop_flag,
            worker_tx: &worker_tx,
            worker_rx: &worker_rx,
            worker_handle: &mut worker_handle,
            search_state: &mut search_state,
            search_id_counter: &mut search_id_counter,
            current_search_id: &mut current_search_id,
            current_search_is_ponder: &mut current_search_is_ponder,
            current_session: &mut legacy_session,
            current_bestmove_emitter: &mut current_bestmove_emitter,
            current_finalized_flag: &mut current_finalized_flag,
            current_stop_flag: &mut current_stop_flag,
            allow_null_move,
            position_state: &mut position_state,
            program_start,
            last_partial_result: &mut last_partial_result,
            search_start_time: &mut search_start_time,
            latest_nodes: &mut latest_nodes,
            soft_limit_ms_ctx: &mut soft_limit_ms_ctx,
            root_legal_moves: &mut root_legal_moves,
            hard_deadline_taken: &mut hard_deadline_taken,
            pre_session_fallback: &mut pre_session_fallback,
            pre_session_fallback_hash: &mut pre_session_fallback_hash,
            current_committed: &mut current_committed,
            last_bestmove_sent_at: &mut last_bestmove_sent_at,
            last_go_begin_at: &mut last_go_begin_at,
            final_pv_injected: &mut final_pv_injected,
            pending_stop_info: &mut pending_stop_info,
            pending_returned_engine: &mut pending_returned_engine,
        };

        // Create SearchFinished message
        let stop_info = StopInfo {
            reason: TerminationReason::Completed,
            elapsed_ms: 100,
            nodes: 10000,
            depth_reached: 5,
            hard_timeout: false,
            soft_limit_ms: 500,
            hard_limit_ms: 1000,
        };

        let msg = WorkerMessage::SearchFinished {
            root_hash: 0,
            search_id: 1,
            stop_info: Some(stop_info),
        };

        // Handle the message
        let result = crate::handle_worker_message(msg, &mut ctx);
        assert!(result.is_ok());

        // Verify state transitioned to Finalized
        assert_eq!(
            *ctx.search_state,
            SearchState::Finalized,
            "Expected state to be Finalized after bestmove emission"
        );

        // Verify bestmove was sent by checking info messages
        let infos = crate::usi::output::test_info_from(start_info_count);
        let bestmove_sent_count = infos.iter().filter(|s| s.contains("kind=bestmove_sent")).count();
        assert!(
            bestmove_sent_count > 0,
            "Expected at least one bestmove_sent message, got: {:?}",
            infos
        );

        // Verify pending_stop_info is None (consumed)
        assert!(
            ctx.pending_stop_info.is_none(),
            "Expected pending_stop_info to be None after successful emission"
        );
    }

    #[test]
    fn test_finished_skips_if_already_finalized() {
        std::env::set_var("USI_DRY_RUN", "1");
        let start_info_count = crate::usi::output::test_info_len();

        let (
            engine,
            worker_tx,
            worker_rx,
            mut search_state,
            mut search_id_counter,
            mut current_search_id,
            mut current_search_is_ponder,
            mut current_bestmove_emitter,
            mut current_finalized_flag,
            mut current_stop_flag,
            mut current_committed,
            mut pending_stop_info,
        ) = create_test_context();

        // Set state to already Finalized
        search_state = SearchState::Finalized;

        // Clear the emitter to simulate finalization
        current_bestmove_emitter = None;

        let mut position_state = None;
        let stop_flag = Arc::new(AtomicBool::new(false));
        let mut worker_handle = None;
        let allow_null_move = true;
        let program_start = Instant::now();
        let mut last_partial_result = None;
        let mut search_start_time = None;
        let mut latest_nodes = 0;
        let mut soft_limit_ms_ctx = 0;
        let mut root_legal_moves = None;
        let mut hard_deadline_taken = false;
        let mut pre_session_fallback = None;
        let mut pre_session_fallback_hash = None;
        let mut last_bestmove_sent_at = None;
        let mut last_go_begin_at = None;
        let mut final_pv_injected = false;
        let mut pending_returned_engine = None;

        let mut legacy_session: Option<()> = None;
        let mut ctx = CommandContext {
            engine: &engine,
            stop_flag: &stop_flag,
            worker_tx: &worker_tx,
            worker_rx: &worker_rx,
            worker_handle: &mut worker_handle,
            search_state: &mut search_state,
            search_id_counter: &mut search_id_counter,
            current_search_id: &mut current_search_id,
            current_search_is_ponder: &mut current_search_is_ponder,
            current_session: &mut legacy_session,
            current_bestmove_emitter: &mut current_bestmove_emitter,
            current_finalized_flag: &mut current_finalized_flag,
            current_stop_flag: &mut current_stop_flag,
            allow_null_move,
            position_state: &mut position_state,
            program_start,
            last_partial_result: &mut last_partial_result,
            search_start_time: &mut search_start_time,
            latest_nodes: &mut latest_nodes,
            soft_limit_ms_ctx: &mut soft_limit_ms_ctx,
            root_legal_moves: &mut root_legal_moves,
            hard_deadline_taken: &mut hard_deadline_taken,
            pre_session_fallback: &mut pre_session_fallback,
            pre_session_fallback_hash: &mut pre_session_fallback_hash,
            current_committed: &mut current_committed,
            last_bestmove_sent_at: &mut last_bestmove_sent_at,
            last_go_begin_at: &mut last_go_begin_at,
            final_pv_injected: &mut final_pv_injected,
            pending_stop_info: &mut pending_stop_info,
            pending_returned_engine: &mut pending_returned_engine,
        };

        // Create Finished message
        let msg = WorkerMessage::Finished {
            from_guard: false,
            search_id: 1,
        };

        // Handle the message
        let result = crate::handle_worker_message(msg, &mut ctx);
        assert!(result.is_ok());

        // Verify no additional bestmove was emitted
        let infos = crate::usi::output::test_info_from(start_info_count);
        let bestmove_sent_count = infos.iter().filter(|s| s.contains("kind=bestmove_sent")).count();
        assert_eq!(
            bestmove_sent_count, 0,
            "Expected no bestmove to be emitted when already finalized"
        );
    }
}
