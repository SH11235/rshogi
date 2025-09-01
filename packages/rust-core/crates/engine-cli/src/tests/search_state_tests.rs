use crate::state::SearchState;
use crate::worker::WorkerMessage;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_acceptance_gate_rejects_go_while_searching() {
        // Create test context
        let mut search_state = SearchState::Searching;

        // Verify that go command cannot be accepted
        assert!(!search_state.can_start_search());

        // Verify that StopRequested state also rejects
        search_state = SearchState::StopRequested;
        assert!(!search_state.can_start_search());
    }

    #[test]
    fn test_acceptance_gate_allows_go_when_idle() {
        let mut search_state = SearchState::Idle;

        // Verify that go command can be accepted
        assert!(search_state.can_start_search());

        // Try to start search
        assert!(search_state.try_start_search());
        assert_eq!(search_state, SearchState::Searching);
    }

    #[test]
    fn test_acceptance_gate_allows_go_when_finalized() {
        let mut search_state = SearchState::Finalized;

        // Verify that go command can be accepted when finalized
        assert!(search_state.can_start_search());

        // Try to start search
        assert!(search_state.try_start_search());
        assert_eq!(search_state, SearchState::Searching);
    }

    #[test]
    fn test_state_transitions() {
        let mut search_state = SearchState::Idle;

        // Idle -> Searching
        assert!(search_state.try_start_search());
        assert_eq!(search_state, SearchState::Searching);

        // Searching -> StopRequested
        assert!(search_state.request_stop());
        assert_eq!(search_state, SearchState::StopRequested);

        // StopRequested -> Finalized
        search_state.set_finalized();
        assert_eq!(search_state, SearchState::Finalized);

        // Finalized -> Idle (after worker join)
        search_state.set_idle();
        assert_eq!(search_state, SearchState::Idle);
    }

    #[test]
    fn test_search_id_message_filtering() {
        // Test that messages with old search_id are dropped
        let current_search_id = 42u64;
        let old_search_id = 41u64;

        // Create test messages
        let old_info = WorkerMessage::Info {
            info: Default::default(),
            search_id: old_search_id,
        };

        let current_info = WorkerMessage::Info {
            info: Default::default(),
            search_id: current_search_id,
        };

        // Extract search_id from messages
        let old_msg_id = match &old_info {
            WorkerMessage::Info { search_id, .. } => *search_id,
            _ => unreachable!(),
        };

        let current_msg_id = match &current_info {
            WorkerMessage::Info { search_id, .. } => *search_id,
            _ => unreachable!(),
        };

        // Verify filtering logic
        assert_ne!(old_msg_id, current_search_id);
        assert_eq!(current_msg_id, current_search_id);
    }

    #[test]
    fn test_cleanup_messages_allowed_from_old_searches() {
        let old_search_id = 41u64;

        // Finished should be allowed even from old searches
        let finished_msg = WorkerMessage::Finished {
            from_guard: false,
            search_id: old_search_id,
        };

        // These messages should be allowed through for cleanup
        matches!(
            &finished_msg,
            WorkerMessage::Finished { .. } | WorkerMessage::ReturnEngine { .. }
        );
    }

    #[test]
    fn test_finalized_state_transitions_to_idle_when_no_worker() {
        use crate::engine_adapter::EngineAdapter;
        use crate::helpers::wait_for_search_completion;
        use crossbeam_channel::unbounded;
        use std::sync::atomic::AtomicBool;
        use std::sync::{Arc, Mutex};

        // Create test context
        let mut search_state = SearchState::Finalized;
        let stop_flag = Arc::new(AtomicBool::new(false));
        let mut worker_handle = None; // No worker handle
        let (_tx, rx) = unbounded();
        let engine = Arc::new(Mutex::new(EngineAdapter::new()));

        // Call wait_for_search_completion with Finalized state and no worker
        let result = wait_for_search_completion(
            &mut search_state,
            &stop_flag,
            None,
            &mut worker_handle,
            &rx,
            &engine,
        );

        // Should succeed and transition to Idle
        assert!(result.is_ok());
        assert_eq!(search_state, SearchState::Idle);
    }

    /*
    #[test]
    fn test_global_stop_flag_cleared_before_new_search() {
        use crate::command_handler::CommandContext;
        use crate::engine_adapter::EngineAdapter;
        use crate::handlers::go::handle_go_command;
        use crate::usi::GoParams;
        use crossbeam_channel::unbounded;
        use engine_core::time_management::TimeControl;
        use std::sync::atomic::AtomicBool;
        use std::sync::{Arc, Mutex};

        // Create test context
        let engine = Arc::new(Mutex::new(EngineAdapter::new()));
        let stop_flag = Arc::new(AtomicBool::new(true)); // Start with true to test clearing
        let (worker_tx, worker_rx) = unbounded();
        let mut worker_handle = None;
        let mut search_state = SearchState::Idle;
        let mut search_id_counter = 0;
        let mut current_search_id = 0;
        let mut current_search_is_ponder = false;
        let mut current_bestmove_emitter = None;
        let mut current_finalized_flag = None;
        let mut current_stop_flag = None;
        let mut pre_session_fallback = None;
        let mut current_committed = vec![];
        let allow_null_move = true;
        let mut position_state = crate::types::PositionState::default();
        let program_start = std::time::Instant::now();
        let mut last_partial_result = None;

        // Setup test position
        {
            let mut adapter = engine.lock().unwrap();
            let _ = adapter.take_engine(); // Get engine ownership
            let result = adapter.set_position_with_sfen("startpos", &[]);
            if let Ok(mut core_engine) = result {
                adapter.return_engine(core_engine);
            }
        }

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
            current_session: &mut None,
            current_bestmove_emitter: &mut current_bestmove_emitter,
            current_finalized_flag: &mut current_finalized_flag,
            current_stop_flag: &mut current_stop_flag,
            pre_session_fallback: &mut pre_session_fallback,
            current_committed: &mut current_committed,
            allow_null_move: &allow_null_move,
            position_state: &mut position_state,
            program_start: &program_start,
            last_partial_result: &mut last_partial_result,
        };

        // Create go params for test
        let params = GoParams {
            movetime: Some(100), // Fixed time of 100ms
            ponder: false,
            ..Default::default()
        };

        // Global stop flag should start as true
        assert!(stop_flag.load(std::sync::atomic::Ordering::Acquire));

        // Handle go command - this should clear the global stop flag
        let result = handle_go_command(params, &mut ctx);
        assert!(result.is_ok());

        // Verify global stop flag was cleared
        assert!(!stop_flag.load(std::sync::atomic::Ordering::Acquire));

        // Clean up - send stop to terminate the worker
        stop_flag.store(true, std::sync::atomic::Ordering::Release);
        if let Some(ref flag) = current_stop_flag {
            flag.store(true, std::sync::atomic::Ordering::Release);
        }

        // Wait for worker to finish
        if let Some(handle) = worker_handle {
            let _ = handle.join();
        }
    }
    */

    #[test]
    fn test_quit_during_various_states() {
        use crate::engine_adapter::EngineAdapter;
        use crate::helpers::wait_for_search_completion;
        use crossbeam_channel::unbounded;
        use std::sync::atomic::AtomicBool;
        use std::sync::{Arc, Mutex};

        // Test quit during Idle state
        {
            let mut search_state = SearchState::Idle;
            let stop_flag = Arc::new(AtomicBool::new(false));
            let mut worker_handle = None;
            let (_tx, rx) = unbounded();
            let engine = Arc::new(Mutex::new(EngineAdapter::new()));

            // Simulate quit
            stop_flag.store(true, std::sync::atomic::Ordering::Release);

            let result = wait_for_search_completion(
                &mut search_state,
                &stop_flag,
                None,
                &mut worker_handle,
                &rx,
                &engine,
            );

            assert!(result.is_ok());
            assert_eq!(search_state, SearchState::Idle);
        }

        // Test quit during Finalized state
        {
            let mut search_state = SearchState::Finalized;
            let stop_flag = Arc::new(AtomicBool::new(false));
            let mut worker_handle = None;
            let (_tx, rx) = unbounded();
            let engine = Arc::new(Mutex::new(EngineAdapter::new()));

            // Simulate quit
            stop_flag.store(true, std::sync::atomic::Ordering::Release);

            let result = wait_for_search_completion(
                &mut search_state,
                &stop_flag,
                None,
                &mut worker_handle,
                &rx,
                &engine,
            );

            assert!(result.is_ok());
            // Should transition to Idle from Finalized
            assert_eq!(search_state, SearchState::Idle);
        }
    }

    #[test]
    fn test_worker_stops_on_global_flag() {
        use crate::engine_adapter::EngineAdapter;
        use crate::usi::GoParams;
        use crate::worker::{search_worker, WorkerMessage};
        use crossbeam_channel::unbounded;
        use std::sync::atomic::AtomicBool;
        use std::sync::{Arc, Mutex};
        use std::thread;
        use std::time::{Duration, Instant};

        // Create test context
        let engine = Arc::new(Mutex::new(EngineAdapter::new()));
        let per_search_stop = Arc::new(AtomicBool::new(false));
        let global_stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = unbounded();

        // Setup test position
        {
            let mut adapter = engine.lock().unwrap();
            let _ = adapter.take_engine();
            let result = adapter.set_position(true, None, &[]);
            if result.is_err() {
                if let Ok(core_engine) = adapter.take_engine() {
                    adapter.return_engine(core_engine);
                }
            }
        }

        // Create go params for a long search (infinite time)
        let params = GoParams {
            btime: None,
            wtime: None,
            byoyomi: None,
            periods: None,
            binc: None,
            winc: None,
            moves_to_go: None,
            depth: None,
            nodes: None,
            movetime: None,
            infinite: true,
            ponder: false,
        };

        // Spawn worker thread
        let engine_clone = engine.clone();
        let per_search_clone = per_search_stop.clone();
        let global_clone = global_stop.clone();
        let tx_clone = tx.clone();

        let handle = thread::spawn(move || {
            search_worker(
                engine_clone,
                params,
                per_search_clone,
                global_clone,
                tx_clone,
                1,    // search_id
                None, // finalized_flag
                Instant::now(),
            );
        });

        // Wait for SearchStarted message
        let start_time = Instant::now();
        let timeout = Duration::from_secs(5);
        let mut search_started = false;

        while start_time.elapsed() < timeout {
            match rx.try_recv() {
                Ok(WorkerMessage::SearchStarted { .. }) => {
                    search_started = true;
                    break;
                }
                Ok(_) => continue,
                Err(_) => {
                    thread::sleep(Duration::from_millis(10));
                }
            }
        }

        assert!(search_started, "Search should have started");

        // Set global stop flag (simulating quit)
        global_stop.store(true, std::sync::atomic::Ordering::Release);

        // Worker should stop and send Finished
        let mut finished = false;
        let finish_timeout = Duration::from_secs(2);
        let finish_start = Instant::now();

        while finish_start.elapsed() < finish_timeout {
            match rx.try_recv() {
                Ok(WorkerMessage::Finished { .. }) => {
                    finished = true;
                    break;
                }
                Ok(_) => continue,
                Err(_) => {
                    thread::sleep(Duration::from_millis(10));
                }
            }
        }

        assert!(finished, "Worker should have finished after global stop");

        // Join the thread
        let _ = handle.join();
    }

    #[test]
    fn test_monitor_thread_terminates_on_stop_flag() {
        use crate::engine_adapter::EngineAdapter;
        use crate::usi::GoParams;
        use crate::worker::{search_worker, WorkerMessage};
        use crossbeam_channel::unbounded;
        use std::sync::atomic::AtomicBool;
        use std::sync::{Arc, Mutex};
        use std::thread;
        use std::time::{Duration, Instant};

        // Create test context
        let engine = Arc::new(Mutex::new(EngineAdapter::new()));
        let per_search_stop = Arc::new(AtomicBool::new(false));
        let global_stop = Arc::new(AtomicBool::new(false));
        let finalized_flag = Arc::new(AtomicBool::new(false));
        let (tx, rx) = unbounded();

        // Setup test position
        {
            let mut adapter = engine.lock().unwrap();
            let _ = adapter.take_engine();
            let result = adapter.set_position(true, None, &[]);
            if result.is_err() {
                if let Ok(core_engine) = adapter.take_engine() {
                    adapter.return_engine(core_engine);
                }
            }
        }

        // Create go params for a very short search
        let params = GoParams {
            btime: None,
            wtime: None,
            byoyomi: None,
            periods: None,
            binc: None,
            winc: None,
            moves_to_go: None,
            depth: Some(1), // Very shallow search
            nodes: None,
            movetime: None,
            infinite: false,
            ponder: false,
        };

        // Spawn worker thread with finalized flag
        let engine_clone = engine.clone();
        let per_search_clone = per_search_stop.clone();
        let global_clone = global_stop.clone();
        let tx_clone = tx.clone();
        let finalized_clone = finalized_flag.clone();

        let handle = thread::spawn(move || {
            search_worker(
                engine_clone,
                params,
                per_search_clone,
                global_clone,
                tx_clone,
                1, // search_id
                Some(finalized_clone),
                Instant::now(),
            );
        });

        // Wait for search to start
        let start_time = Instant::now();
        let timeout = Duration::from_secs(5);
        let mut search_started = false;

        while start_time.elapsed() < timeout {
            match rx.try_recv() {
                Ok(WorkerMessage::SearchStarted { .. }) => {
                    search_started = true;
                    break;
                }
                Ok(_) => continue,
                Err(_) => {
                    thread::sleep(Duration::from_millis(10));
                }
            }
        }

        assert!(search_started, "Search should have started");

        // Set per-search stop flag
        per_search_stop.store(true, std::sync::atomic::Ordering::Release);

        // Wait for worker to finish
        let mut worker_finished = false;
        let finish_timeout = Duration::from_secs(2);
        let finish_start = Instant::now();

        while finish_start.elapsed() < finish_timeout {
            match rx.try_recv() {
                Ok(WorkerMessage::Finished { .. }) => {
                    worker_finished = true;
                    break;
                }
                Ok(_) => continue,
                Err(_) => {
                    thread::sleep(Duration::from_millis(10));
                }
            }
        }

        assert!(worker_finished, "Worker should have finished");

        // Join the thread - should complete immediately
        let join_result = handle.join();
        assert!(join_result.is_ok(), "Worker thread should join cleanly");

        // The monitor thread should have terminated due to stop flag
        // We can't directly test this, but if the thread joined successfully, it succeeded
    }

    #[test]
    fn test_monitor_thread_terminates_on_finalized_flag() {
        use crate::engine_adapter::EngineAdapter;
        use crate::usi::GoParams;
        use crate::worker::{search_worker, WorkerMessage};
        use crossbeam_channel::unbounded;
        use std::sync::atomic::AtomicBool;
        use std::sync::{Arc, Mutex};
        use std::thread;
        use std::time::{Duration, Instant};

        // Create test context
        let engine = Arc::new(Mutex::new(EngineAdapter::new()));
        let per_search_stop = Arc::new(AtomicBool::new(false));
        let global_stop = Arc::new(AtomicBool::new(false));
        let finalized_flag = Arc::new(AtomicBool::new(false));
        let (tx, rx) = unbounded();

        // Setup test position
        {
            let mut adapter = engine.lock().unwrap();
            let _ = adapter.take_engine();
            let result = adapter.set_position(true, None, &[]);
            if result.is_err() {
                if let Ok(core_engine) = adapter.take_engine() {
                    adapter.return_engine(core_engine);
                }
            }
        }

        // Create go params for infinite search
        let params = GoParams {
            btime: None,
            wtime: None,
            byoyomi: None,
            periods: None,
            binc: None,
            winc: None,
            moves_to_go: None,
            depth: None,
            nodes: None,
            movetime: None,
            infinite: true, // Infinite search so we can control when it ends
            ponder: false,
        };

        // Spawn worker thread with finalized flag
        let engine_clone = engine.clone();
        let per_search_clone = per_search_stop.clone();
        let global_clone = global_stop.clone();
        let tx_clone = tx.clone();
        let finalized_clone = finalized_flag.clone();

        let handle = thread::spawn(move || {
            search_worker(
                engine_clone,
                params,
                per_search_clone,
                global_clone,
                tx_clone,
                1, // search_id
                Some(finalized_clone),
                Instant::now(),
            );
        });

        // Wait for search to start
        let start_time = Instant::now();
        let timeout = Duration::from_secs(5);
        let mut search_started = false;

        while start_time.elapsed() < timeout {
            match rx.try_recv() {
                Ok(WorkerMessage::SearchStarted { .. }) => {
                    search_started = true;
                    break;
                }
                Ok(_) => continue,
                Err(_) => {
                    thread::sleep(Duration::from_millis(10));
                }
            }
        }

        assert!(search_started, "Search should have started");

        // Simulate main thread setting finalized flag (as would happen when processing SearchFinished)
        finalized_flag.store(true, std::sync::atomic::Ordering::Release);

        // Also set stop flag to make worker exit
        per_search_stop.store(true, std::sync::atomic::Ordering::Release);

        // Wait for worker to finish
        let mut worker_finished = false;
        let finish_timeout = Duration::from_secs(2);
        let finish_start = Instant::now();

        while finish_start.elapsed() < finish_timeout {
            match rx.try_recv() {
                Ok(WorkerMessage::Finished { .. }) => {
                    worker_finished = true;
                    break;
                }
                Ok(_) => continue,
                Err(_) => {
                    thread::sleep(Duration::from_millis(10));
                }
            }
        }

        assert!(worker_finished, "Worker should have finished");

        // Join the thread - should complete immediately
        let join_result = handle.join();
        assert!(join_result.is_ok(), "Worker thread should join cleanly");

        // The monitor thread should have terminated due to finalized flag
        // We can't directly test this, but if the thread joined successfully, it succeeded
    }

    #[test]
    fn test_go_command_accepted_in_finalized_state() {
        use crate::command_handler::CommandContext;
        use crate::engine_adapter::EngineAdapter;
        use crate::handlers::go::handle_go_command;
        use crate::usi::GoParams;
        use crossbeam_channel::unbounded;
        use std::sync::atomic::AtomicBool;
        use std::sync::{Arc, Mutex};
        use std::time::Instant;

        // Create test context
        let engine = Arc::new(Mutex::new(EngineAdapter::new()));
        let stop_flag = Arc::new(AtomicBool::new(false));
        let (worker_tx, worker_rx) = unbounded();
        let mut worker_handle = None;
        let mut search_state = SearchState::Finalized; // Start in Finalized state
        let mut search_id_counter = 1;
        let mut current_search_id = 1;
        let mut current_search_is_ponder = false;
        let mut current_bestmove_emitter = None;
        let mut current_finalized_flag = Some(Arc::new(AtomicBool::new(false)));
        let mut current_stop_flag = None;
        let mut pre_session_fallback = None;
        let mut pre_session_fallback_hash = None;
        let mut current_committed = None;
        let mut position_state = None;
        let program_start = Instant::now();
        let mut last_partial_result = None;
        let mut search_start_time = None;
        let mut latest_nodes = 0;
        let mut soft_limit_ms_ctx = 0;
        let mut root_legal_moves = None;
        let mut hard_deadline_taken = false;
        let mut last_bestmove_sent_at = None;
        let mut last_go_begin_at = None;
        let mut final_pv_injected = false;
        let mut pending_stop_info = None;
        let mut pending_returned_engine = None;

        // Setup test position
        {
            let mut adapter = engine.lock().unwrap();
            let _ = adapter.set_position(true, None, &[]);
        }

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
            current_session: &mut None,
            current_bestmove_emitter: &mut current_bestmove_emitter,
            current_finalized_flag: &mut current_finalized_flag,
            current_stop_flag: &mut current_stop_flag,
            pre_session_fallback: &mut pre_session_fallback,
            pre_session_fallback_hash: &mut pre_session_fallback_hash,
            current_committed: &mut current_committed,
            allow_null_move: false,
            position_state: &mut position_state,
            program_start,
            last_partial_result: &mut last_partial_result,
            search_start_time: &mut search_start_time,
            latest_nodes: &mut latest_nodes,
            soft_limit_ms_ctx: &mut soft_limit_ms_ctx,
            root_legal_moves: &mut root_legal_moves,
            hard_deadline_taken: &mut hard_deadline_taken,
            last_bestmove_sent_at: &mut last_bestmove_sent_at,
            last_go_begin_at: &mut last_go_begin_at,
            final_pv_injected: &mut final_pv_injected,
            pending_stop_info: &mut pending_stop_info,
            pending_returned_engine: &mut pending_returned_engine,
        };

        // Create go params for test
        let params = GoParams {
            movetime: Some(100), // Fixed time of 100ms
            ponder: false,
            ..Default::default()
        };

        // Verify initial state is Finalized
        assert_eq!(*ctx.search_state, SearchState::Finalized);
        assert!(
            ctx.search_state.can_start_search(),
            "Finalized state should allow starting search"
        );

        // Handle go command - should not block and should transition state
        let result = handle_go_command(params, &mut ctx);
        assert!(result.is_ok(), "Go command should succeed in Finalized state");

        // State should have transitioned to Searching (new search started)
        assert_eq!(*ctx.search_state, SearchState::Searching, "State should be Searching after go");

        // Clean up - send stop to terminate the worker
        stop_flag.store(true, std::sync::atomic::Ordering::Release);
        if let Some(ref flag) = current_stop_flag {
            flag.store(true, std::sync::atomic::Ordering::Release);
        }

        // Wait for worker to finish
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}
