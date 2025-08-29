use crate::engine_adapter::EngineAdapter;
// use crate::helpers::{generate_fallback_move, wait_for_search_completion};
use crate::search_session::SearchSession;
use crate::state::SearchState;
use crate::types::{BestmoveSource, PositionState};
use crate::usi::{send_info_string, send_response, UsiCommand, UsiResponse};
use crate::worker::{lock_or_recover_adapter, WorkerMessage};
use anyhow::Result;
use crossbeam_channel::{Receiver, Sender};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

use crate::bestmove_emitter::{BestmoveEmitter, BestmoveMeta};
use crate::emit_utils::{build_meta, log_tsv};
use engine_core::search::types::StopInfo;

/// Context for handling USI commands
pub struct CommandContext<'a> {
    pub engine: &'a Arc<Mutex<EngineAdapter>>,
    pub stop_flag: &'a Arc<AtomicBool>, // Global stop flag (for shutdown)
    pub worker_tx: &'a Sender<WorkerMessage>,
    pub worker_rx: &'a Receiver<WorkerMessage>,
    pub worker_handle: &'a mut Option<JoinHandle<()>>,
    pub search_state: &'a mut SearchState,
    pub search_id_counter: &'a mut u64,
    pub current_search_id: &'a mut u64,
    pub current_search_is_ponder: &'a mut bool,
    pub current_session: &'a mut Option<SearchSession>,
    pub current_bestmove_emitter: &'a mut Option<BestmoveEmitter>,
    pub current_stop_flag: &'a mut Option<Arc<AtomicBool>>, // Per-search stop flag
    pub allow_null_move: bool,
    pub position_state: &'a mut Option<PositionState>, // Store position state for recovery
    pub program_start: Instant, // Program start time for elapsed calculations
    /// Last received partial result (move, depth, score) for current search
    pub last_partial_result: &'a mut Option<(String, u8, i32)>,
    /// Precomputed root fallback move captured at go-time for stop-time emergencies
    pub pre_session_fallback: &'a mut Option<String>,
    /// Hash of the position when pre_session_fallback was computed
    pub pre_session_fallback_hash: &'a mut Option<u64>,
}

impl<'a> CommandContext<'a> {
    /// Try to emit bestmove from session
    /// Returns Ok(true) if bestmove was successfully emitted
    pub(crate) fn emit_best_from_session(
        &mut self,
        session: &SearchSession,
        from: BestmoveSource,
        stop_info: Option<StopInfo>,
        finalize_label: &str,
    ) -> Result<bool> {
        let adapter = lock_or_recover_adapter(self.engine);
        if let Some(position) = adapter.get_position() {
            if let Ok((best_move, ponder, ponder_source)) =
                adapter.validate_and_get_bestmove(session, position)
            {
                // Extract common score formatting and metadata
                let depth = session.committed_best.as_ref().map(|b| b.depth).unwrap_or(0);
                let seldepth = session.committed_best.as_ref().and_then(|b| b.seldepth);
                let score_str = session.committed_best.as_ref().map(|b| match &b.score {
                    crate::search_session::Score::Cp(cp) => format!("cp {cp}"),
                    crate::search_session::Score::Mate(mate) => format!("mate {mate}"),
                });

                log::debug!("Validated bestmove from session: depth={depth}");

                // Metrics: PV長・Ponderソース
                let pv_len_str = session
                    .committed_best
                    .as_ref()
                    .map(|b| b.pv.len().to_string())
                    .unwrap_or_else(|| "0".to_string());
                let ponder_src_str = ponder_source.to_string();
                let metrics = log_tsv(&[
                    ("kind", "bestmove_metrics"),
                    ("search_id", &self.current_search_id.to_string()),
                    ("pv_len", &pv_len_str),
                    ("ponder_source", &ponder_src_str),
                    ("ponder_present", if ponder.is_some() { "true" } else { "false" }),
                ]);
                let _ = send_info_string(metrics);

                let meta = build_meta(from, depth, seldepth, score_str, stop_info);
                self.emit_and_finalize(best_move, ponder, meta, finalize_label)?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    #[inline]
    pub fn finalize_search(&mut self, where_: &str) {
        log::debug!("Finalize search {} ({})", *self.current_search_id, where_);
        self.search_state.set_idle();
        *self.current_search_is_ponder = false;
        *self.current_bestmove_emitter = None;
        *self.current_session = None;

        // Drop the current stop flag without resetting it
        // This prevents race conditions where worker might miss the stop signal
        let _ = self.current_stop_flag.take();
    }

    /// Emit bestmove and always finalize search, even on error
    ///
    /// This ensures finalize_search is called even if emit fails.
    /// Following USI best practices, this method always succeeds (returns Ok)
    /// and makes best effort to send bestmove even if primary emission fails.
    pub fn emit_and_finalize(
        &mut self,
        best_move: String,
        ponder: Option<String>,
        meta: BestmoveMeta,
        finalize_label: &str,
    ) -> Result<()> {
        // Metrics logging is handled before this call in emit_best_from_session
        // Try to emit via BestmoveEmitter if available
        if let Some(ref emitter) = self.current_bestmove_emitter {
            match emitter.emit(best_move.clone(), ponder.clone(), meta.clone()) {
                Ok(()) => {
                    self.finalize_search(finalize_label);
                    Ok(())
                }
                Err(e) => {
                    log::error!("BestmoveEmitter::emit failed: {e}");
                    // Send TSV log for fallback
                    self.send_fallback_tsv_log(
                        &best_move,
                        ponder.as_deref(),
                        Some(&meta),
                        "emitter_failed",
                    );
                    // Try direct send as fallback
                    if let Err(e) = send_response(UsiResponse::BestMove { best_move, ponder }) {
                        log::error!("Failed to send bestmove even with direct fallback: {e}");
                        // Continue without propagating error - USI requires best effort
                    }
                    // Always finalize search after attempting to emit
                    self.finalize_search(finalize_label);
                    Ok(())
                }
            }
        } else {
            log::warn!("BestmoveEmitter not available; sending bestmove directly");
            // Send TSV log for direct send
            self.send_fallback_tsv_log(&best_move, ponder.as_deref(), Some(&meta), "no_emitter");
            if let Err(e) = send_response(UsiResponse::BestMove { best_move, ponder }) {
                log::error!("Failed to send bestmove directly: {e}");
                // Continue without propagating error - USI requires best effort
            }
            // Always finalize search after attempting to emit
            self.finalize_search(finalize_label);
            Ok(())
        }
    }

    /// Send TSV log for direct fallback bestmove (when BestmoveEmitter is not available or fails)
    fn send_fallback_tsv_log(
        &self,
        best_move: &str,
        ponder: Option<&str>,
        meta: Option<&BestmoveMeta>,
        fallback_reason: &str,
    ) {
        // Prepare TSV log similar to BestmoveEmitter's format
        let search_id_str = self.current_search_id.to_string();
        let ponder_str = ponder.unwrap_or("none");

        let info_string = if let Some(m) = meta {
            // Format metadata values as strings for log_tsv
            let from_str = m.from.to_string();
            let stop_reason_str = m.stop_info.reason.to_string();
            let depth_str = m.stats.depth.to_string();
            let seldepth_str =
                m.stats.seldepth.map(|v| v.to_string()).unwrap_or_else(|| "-".to_string());
            let depth_reached_str = m.stop_info.depth_reached.to_string();
            let nodes_str = m.stats.nodes.to_string();
            let nps_str = m.stats.nps.to_string();
            let elapsed_ms_str = m.stop_info.elapsed_ms.to_string();
            let hard_timeout_str = m.stop_info.hard_timeout.to_string();

            log_tsv(&[
                ("kind", "bestmove_direct_fallback"),
                ("search_id", &search_id_str),
                ("bestmove_from", &from_str),
                ("stop_reason", &stop_reason_str),
                ("depth", &depth_str),
                ("seldepth", &seldepth_str),
                ("depth_reached", &depth_reached_str),
                ("score", &m.stats.score),
                ("nodes", &nodes_str),
                ("nps", &nps_str),
                ("elapsed_ms", &elapsed_ms_str),
                ("hard_timeout", &hard_timeout_str),
                ("bestmove", best_move),
                ("ponder", ponder_str),
                ("fallback_reason", fallback_reason),
            ])
        } else {
            // Default values when no metadata is available
            log_tsv(&[
                ("kind", "bestmove_direct_fallback"),
                ("search_id", &search_id_str),
                ("bestmove_from", fallback_reason),
                ("stop_reason", "error"),
                ("depth", "0"),
                ("seldepth", "-"),
                ("depth_reached", "0"),
                ("score", "none"),
                ("nodes", "0"),
                ("nps", "0"),
                ("elapsed_ms", "0"),
                ("hard_timeout", "false"),
                ("bestmove", best_move),
                ("ponder", ponder_str),
                ("fallback_reason", fallback_reason),
            ])
        };

        if let Err(e) = send_info_string(info_string) {
            log::warn!("Failed to send fallback TSV log: {e}");
        }
    }
}

use crate::handlers::{
    game::handle_gameover, go::handle_go_command, options::handle_set_option,
    ponder::handle_ponder_hit, position::handle_position_command, stop::handle_stop_command,
};

pub fn handle_command(command: UsiCommand, ctx: &mut CommandContext) -> Result<()> {
    match command {
        UsiCommand::Usi => {
            send_response(UsiResponse::IdName("RustShogi 1.0".to_string()))?;
            send_response(UsiResponse::IdAuthor("RustShogi Team".to_string()))?;

            // Send available options
            {
                let engine = lock_or_recover_adapter(ctx.engine);
                for option in engine.get_options() {
                    send_response(UsiResponse::Option(option.to_string()))?;
                }
            }

            send_response(UsiResponse::UsiOk)?;
        }

        UsiCommand::IsReady => {
            // Initialize engine if needed
            // Note: Static tables are already initialized in main() using init_all_tables_once()
            // which is idempotent (using std::sync::Once internally)
            {
                let mut engine = lock_or_recover_adapter(ctx.engine);
                engine.initialize()?;
            }
            send_response(UsiResponse::ReadyOk)?;
        }

        UsiCommand::Position {
            startpos,
            sfen,
            moves,
        } => {
            handle_position_command(startpos, sfen, moves, ctx)?;
        }

        UsiCommand::Go(params) => {
            handle_go_command(params, ctx)?;
        }

        UsiCommand::Stop => {
            handle_stop_command(ctx)?;
        }

        UsiCommand::PonderHit => {
            handle_ponder_hit(ctx)?;
        }

        UsiCommand::SetOption { name, value } => {
            handle_set_option(name, value, ctx)?;
        }

        UsiCommand::GameOver { result } => {
            handle_gameover(result, ctx)?;
        }

        UsiCommand::UsiNewGame => {
            crate::handlers::game::handle_usi_new_game(ctx)?;
        }

        UsiCommand::Quit => {
            // Quit is handled in main loop
            unreachable!("Quit should be handled in main loop");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine_adapter::EngineAdapter;
    use crate::usi::output::{test_info_from, test_info_len};
    use crossbeam_channel::unbounded;
    use engine_core::search::types::TerminationReason;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};

    #[test]
    fn test_build_meta_reason_mapping() {
        use crate::types::BestmoveSource as S;

        // Timeout sources map to TimeLimit and hard_timeout true only for explicit timeout variants
        let timeout_sources = [
            S::ResignTimeout,
            S::EmergencyFallbackTimeout,
            S::PartialResultTimeout,
        ];
        for &src in &timeout_sources {
            let m = build_meta(src, 7, Some(9), Some("cp 10".into()), None);
            assert_eq!(m.stop_info.reason, TerminationReason::TimeLimit);
            assert!(m.stop_info.hard_timeout);
            assert_eq!(m.stop_info.depth_reached, 7);
        }

        // Normal completion
        for &src in &[
            S::EmergencyFallback,
            S::EmergencyFallbackOnFinish,
            S::SessionInSearchFinished,
        ] {
            let m = build_meta(src, 12, None, None, None);
            assert_eq!(m.stop_info.reason, TerminationReason::Completed);
            assert!(!m.stop_info.hard_timeout);
            assert_eq!(m.stop_info.depth_reached, 12);
        }

        // User stop
        let m = build_meta(S::SessionOnStop, 5, None, None, None);
        assert_eq!(m.stop_info.reason, TerminationReason::UserStop);
        assert!(!m.stop_info.hard_timeout);

        // Error
        for &src in &[S::Resign, S::ResignOnFinish] {
            let m = build_meta(src, 3, Some(4), None, None);
            assert_eq!(m.stop_info.reason, TerminationReason::Error);
            assert!(!m.stop_info.hard_timeout);
            assert_eq!(m.stats.depth, 3);
        }
    }

    #[test]
    fn test_build_meta_keeps_stopinfo_when_provided() {
        use crate::types::BestmoveSource as S;
        let si = StopInfo {
            reason: TerminationReason::Completed,
            elapsed_ms: 123,
            nodes: 456,
            depth_reached: 8,
            hard_timeout: false,
            soft_limit_ms: 111,
            hard_limit_ms: 222,
        };
        let m =
            build_meta(S::SessionInSearchFinished, 1, None, Some("cp 0".into()), Some(si.clone()));
        assert_eq!(m.stop_info.elapsed_ms, 123);
        assert_eq!(m.stop_info.nodes, 456);
        assert_eq!(m.stop_info.soft_limit_ms, 111);
        assert_eq!(m.stop_info.hard_limit_ms, 222);
        assert_eq!(m.stats.depth, 1);
        assert_eq!(m.stats.score, "cp 0");
    }

    /// Verify that normal stop uses pre_session fallback when available and hashes match
    #[test]
    fn test_on_stop_source_pre_session_normal() {
        // Avoid actual stdout writes
        std::env::set_var("USI_DRY_RUN", "1");

        // Engine and position
        let engine = Arc::new(Mutex::new(EngineAdapter::new()));
        {
            let mut adapter = engine.lock().unwrap();
            adapter.set_position(true, None, &[]).unwrap();
        }
        let root_hash = { engine.lock().unwrap().get_position().unwrap().zobrist_hash() };

        // Channels (not used by stop path, but required by types)
        let (tx, rx) = unbounded();

        // Per-search stop flag
        let search_stop_flag = Arc::new(AtomicBool::new(false));

        // Context fields
        let mut worker_handle = None;
        let mut search_state = SearchState::Searching;
        let mut search_id_counter = 0u64;
        let mut current_search_id = 1u64;
        let mut current_search_is_ponder = false;
        let mut current_session: Option<SearchSession> = None;
        let mut current_bestmove_emitter: Option<BestmoveEmitter> =
            Some(BestmoveEmitter::new(current_search_id));
        let mut current_stop_flag: Option<Arc<AtomicBool>> = Some(search_stop_flag);
        let mut position_state: Option<PositionState> = None;
        let program_start = Instant::now();
        let mut last_partial_result: Option<(String, u8, i32)> = None;
        let mut pre_session_fallback: Option<String> = Some("7g7f".to_string());
        let mut pre_session_fallback_hash: Option<u64> = Some(root_hash);

        // Clear test hooks
        let start_idx = test_info_len();

        let mut ctx = CommandContext {
            engine: &engine,
            stop_flag: &Arc::new(AtomicBool::new(false)),
            worker_tx: &tx,
            worker_rx: &rx,
            worker_handle: &mut worker_handle,
            search_state: &mut search_state,
            search_id_counter: &mut search_id_counter,
            current_search_id: &mut current_search_id,
            current_search_is_ponder: &mut current_search_is_ponder,
            current_session: &mut current_session,
            current_bestmove_emitter: &mut current_bestmove_emitter,
            current_stop_flag: &mut current_stop_flag,
            allow_null_move: false,
            position_state: &mut position_state,
            program_start,
            last_partial_result: &mut last_partial_result,
            pre_session_fallback: &mut pre_session_fallback,
            pre_session_fallback_hash: &mut pre_session_fallback_hash,
        };

        // Execute stop
        handle_stop_command(&mut ctx).unwrap();

        // Verify bestmove_sent for this search_id exactly once and on_stop_source=pre_session
        let infos = test_info_from(start_idx);
        let sent_count = infos
            .iter()
            .filter(|s| s.contains("kind=bestmove_sent") && s.contains("search_id=1\t"))
            .count();
        assert_eq!(sent_count, 1, "expected 1 bestmove_sent: {:?}", infos);
        let found = infos
            .iter()
            .any(|s| s.contains("kind=on_stop_source") && s.contains("src=pre_session"));
        assert!(found, "on_stop_source=pre_session not found in infos: {:?}", infos);
    }

    /// Verify that when pre_session hash mismatches, normal stop skips it and logs emergency
    #[test]
    fn test_on_stop_source_emergency_when_hash_mismatch() {
        // Avoid actual stdout writes
        std::env::set_var("USI_DRY_RUN", "1");

        // Engine and position
        let engine = Arc::new(Mutex::new(EngineAdapter::new()));
        {
            let mut adapter = engine.lock().unwrap();
            adapter.set_position(true, None, &[]).unwrap();
        }

        // Channels (not used by stop path, but required by types)
        let (tx, rx) = unbounded();

        // Per-search stop flag
        let search_stop_flag = Arc::new(AtomicBool::new(false));

        // Context fields
        let mut worker_handle = None;
        let mut search_state = SearchState::Searching;
        let mut search_id_counter = 0u64;
        let mut current_search_id = 2u64;
        let mut current_search_is_ponder = false;
        let mut current_session: Option<SearchSession> = None;
        let mut current_bestmove_emitter: Option<BestmoveEmitter> =
            Some(BestmoveEmitter::new(current_search_id));
        let mut current_stop_flag: Option<Arc<AtomicBool>> = Some(search_stop_flag);
        let mut position_state: Option<PositionState> = None;
        let program_start = Instant::now();
        let mut last_partial_result: Option<(String, u8, i32)> = None;
        let mut pre_session_fallback: Option<String> = Some("7g7f".to_string());
        let mut pre_session_fallback_hash: Option<u64> = Some(0); // Intentional mismatch

        // Clear test hooks
        let start_idx = test_info_len();

        let mut ctx = CommandContext {
            engine: &engine,
            stop_flag: &Arc::new(AtomicBool::new(false)),
            worker_tx: &tx,
            worker_rx: &rx,
            worker_handle: &mut worker_handle,
            search_state: &mut search_state,
            search_id_counter: &mut search_id_counter,
            current_search_id: &mut current_search_id,
            current_search_is_ponder: &mut current_search_is_ponder,
            current_session: &mut current_session,
            current_bestmove_emitter: &mut current_bestmove_emitter,
            current_stop_flag: &mut current_stop_flag,
            allow_null_move: true, // permit null move emergency if needed
            position_state: &mut position_state,
            program_start,
            last_partial_result: &mut last_partial_result,
            pre_session_fallback: &mut pre_session_fallback,
            pre_session_fallback_hash: &mut pre_session_fallback_hash,
        };

        // Execute stop
        handle_stop_command(&mut ctx).unwrap();

        // Verify bestmove_sent for this search_id exactly once and on_stop_source=emergency
        let infos = test_info_from(start_idx);
        let sent_count = infos
            .iter()
            .filter(|s| s.contains("kind=bestmove_sent") && s.contains("search_id=2\t"))
            .count();
        assert_eq!(sent_count, 1, "expected 1 bestmove_sent: {:?}", infos);
        let found = infos
            .iter()
            .any(|s| s.contains("kind=on_stop_source") && s.contains("src=emergency"));
        assert!(found, "on_stop_source=emergency not found in infos: {:?}", infos);
    }

    /// Verify stop prefers session when committed best exists
    #[test]
    fn test_on_stop_source_session_committed() {
        std::env::set_var("USI_DRY_RUN", "1");

        // Engine and position
        let engine = Arc::new(Mutex::new(EngineAdapter::new()));
        {
            let mut adapter = engine.lock().unwrap();
            adapter.set_position(true, None, &[]).unwrap();
        }
        let root_hash = { engine.lock().unwrap().get_position().unwrap().zobrist_hash() };

        // Build a session with committed best
        let mut session = SearchSession::new(10, root_hash);
        let mv = engine_core::usi::parse_usi_move("7g7f").unwrap();
        session.update_current_best_with_seldepth(12, Some(14), 32, vec![mv]);
        session.commit_iteration();

        // Channels and stop flag
        let (tx, rx) = unbounded();
        let search_stop_flag = Arc::new(AtomicBool::new(false));

        // Context
        let mut worker_handle = None;
        let mut search_state = SearchState::Searching;
        let mut search_id_counter = 0u64;
        let mut current_search_id = 10u64;
        let mut current_search_is_ponder = false;
        let mut current_session: Option<SearchSession> = Some(session);
        let mut current_bestmove_emitter: Option<BestmoveEmitter> =
            Some(BestmoveEmitter::new(current_search_id));
        let mut current_stop_flag: Option<Arc<AtomicBool>> = Some(search_stop_flag);
        let mut position_state: Option<PositionState> = None;
        let program_start = Instant::now();
        let mut last_partial_result: Option<(String, u8, i32)> = None;
        let mut pre_session_fallback: Option<String> = None;
        let mut pre_session_fallback_hash: Option<u64> = None;

        let start_idx = test_info_len();

        let mut ctx = CommandContext {
            engine: &engine,
            stop_flag: &Arc::new(AtomicBool::new(false)),
            worker_tx: &tx,
            worker_rx: &rx,
            worker_handle: &mut worker_handle,
            search_state: &mut search_state,
            search_id_counter: &mut search_id_counter,
            current_search_id: &mut current_search_id,
            current_search_is_ponder: &mut current_search_is_ponder,
            current_session: &mut current_session,
            current_bestmove_emitter: &mut current_bestmove_emitter,
            current_stop_flag: &mut current_stop_flag,
            allow_null_move: false,
            position_state: &mut position_state,
            program_start,
            last_partial_result: &mut last_partial_result,
            pre_session_fallback: &mut pre_session_fallback,
            pre_session_fallback_hash: &mut pre_session_fallback_hash,
        };

        handle_stop_command(&mut ctx).unwrap();

        let infos = test_info_from(start_idx);
        let sent_count = infos
            .iter()
            .filter(|s| s.contains("kind=bestmove_sent") && s.contains("search_id=10\t"))
            .count();
        assert_eq!(sent_count, 1, "expected 1 bestmove_sent: {:?}", infos);
        let found = infos
            .iter()
            .any(|s| s.contains("kind=on_stop_source") && s.contains("src=session"));
        assert!(found, "on_stop_source=session not found in infos: {:?}", infos);
    }

    /// Verify stop uses partial result when available and no committed session exists
    #[test]
    fn test_on_stop_source_partial_with_last_result() {
        std::env::set_var("USI_DRY_RUN", "1");

        // Engine and position
        let engine = Arc::new(Mutex::new(EngineAdapter::new()));
        {
            let mut adapter = engine.lock().unwrap();
            adapter.set_position(true, None, &[]).unwrap();
        }

        // Channels and stop flag
        let (tx, rx) = unbounded();
        let search_stop_flag = Arc::new(AtomicBool::new(false));

        // Context
        let mut worker_handle = None;
        let mut search_state = SearchState::Searching;
        let mut search_id_counter = 0u64;
        let mut current_search_id = 11u64;
        let mut current_search_is_ponder = false;
        let mut current_session: Option<SearchSession> = None;
        let mut current_bestmove_emitter: Option<BestmoveEmitter> =
            Some(BestmoveEmitter::new(current_search_id));
        let mut current_stop_flag: Option<Arc<AtomicBool>> = Some(search_stop_flag);
        let mut position_state: Option<PositionState> = None;
        let program_start = Instant::now();
        let mut last_partial_result: Option<(String, u8, i32)> =
            Some(("7g7f".to_string(), 12, 100));
        let mut pre_session_fallback: Option<String> = None;
        let mut pre_session_fallback_hash: Option<u64> = None;

        let start_idx = test_info_len();

        let mut ctx = CommandContext {
            engine: &engine,
            stop_flag: &Arc::new(AtomicBool::new(false)),
            worker_tx: &tx,
            worker_rx: &rx,
            worker_handle: &mut worker_handle,
            search_state: &mut search_state,
            search_id_counter: &mut search_id_counter,
            current_search_id: &mut current_search_id,
            current_search_is_ponder: &mut current_search_is_ponder,
            current_session: &mut current_session,
            current_bestmove_emitter: &mut current_bestmove_emitter,
            current_stop_flag: &mut current_stop_flag,
            allow_null_move: false,
            position_state: &mut position_state,
            program_start,
            last_partial_result: &mut last_partial_result,
            pre_session_fallback: &mut pre_session_fallback,
            pre_session_fallback_hash: &mut pre_session_fallback_hash,
        };

        handle_stop_command(&mut ctx).unwrap();

        let infos = test_info_from(start_idx);
        let sent_count = infos
            .iter()
            .filter(|s| s.contains("kind=bestmove_sent") && s.contains("search_id=11\t"))
            .count();
        assert_eq!(sent_count, 1, "expected 1 bestmove_sent: {:?}", infos);
        let found = infos
            .iter()
            .any(|s| s.contains("kind=on_stop_source") && s.contains("src=partial"));
        assert!(found, "on_stop_source=partial not found in infos: {:?}", infos);
    }

    /// Ponder stop should use pre_session if available (hash match)
    #[test]
    fn test_ponder_stop_uses_pre_session() {
        std::env::set_var("USI_DRY_RUN", "1");

        let engine = Arc::new(Mutex::new(EngineAdapter::new()));
        {
            let mut adapter = engine.lock().unwrap();
            adapter.set_position(true, None, &[]).unwrap();
        }
        let root_hash = { engine.lock().unwrap().get_position().unwrap().zobrist_hash() };

        let (tx, rx) = unbounded();
        let flag = Arc::new(AtomicBool::new(false));

        let mut worker_handle = None;
        let mut search_state = SearchState::Searching;
        let mut search_id_counter = 0u64;
        let mut current_search_id = 20u64;
        let mut current_search_is_ponder = true;
        let mut current_session: Option<SearchSession> = None;
        let mut current_bestmove_emitter: Option<BestmoveEmitter> =
            Some(BestmoveEmitter::new(current_search_id));
        let mut current_stop_flag: Option<Arc<AtomicBool>> = Some(flag);
        let mut position_state: Option<PositionState> = None;
        let program_start = Instant::now();
        let mut last_partial_result: Option<(String, u8, i32)> = None;
        let mut pre_session_fallback: Option<String> = Some("7g7f".to_string());
        let mut pre_session_fallback_hash: Option<u64> = Some(root_hash);

        let start_idx = test_info_len();

        let mut ctx = CommandContext {
            engine: &engine,
            stop_flag: &Arc::new(AtomicBool::new(false)),
            worker_tx: &tx,
            worker_rx: &rx,
            worker_handle: &mut worker_handle,
            search_state: &mut search_state,
            search_id_counter: &mut search_id_counter,
            current_search_id: &mut current_search_id,
            current_search_is_ponder: &mut current_search_is_ponder,
            current_session: &mut current_session,
            current_bestmove_emitter: &mut current_bestmove_emitter,
            current_stop_flag: &mut current_stop_flag,
            allow_null_move: false,
            position_state: &mut position_state,
            program_start,
            last_partial_result: &mut last_partial_result,
            pre_session_fallback: &mut pre_session_fallback,
            pre_session_fallback_hash: &mut pre_session_fallback_hash,
        };

        handle_stop_command(&mut ctx).unwrap();

        let infos = test_info_from(start_idx);
        let sent_count = infos
            .iter()
            .filter(|s| s.contains("kind=bestmove_sent") && s.contains("search_id=20\t"))
            .count();
        assert_eq!(sent_count, 1, "expected 1 bestmove_sent: {:?}", infos);
        let found = infos
            .iter()
            .any(|s| s.contains("kind=on_stop_source") && s.contains("src=pre_session"));
        assert!(found, "ponder on_stop_source=pre_session not found in infos: {:?}", infos);
    }

    /// Ponder stop with no session/partial/pre_session should use emergency
    #[test]
    fn test_ponder_stop_emergency() {
        std::env::set_var("USI_DRY_RUN", "1");

        let engine = Arc::new(Mutex::new(EngineAdapter::new()));
        {
            let mut adapter = engine.lock().unwrap();
            adapter.set_position(true, None, &[]).unwrap();
        }

        let (tx, rx) = unbounded();
        let flag = Arc::new(AtomicBool::new(false));

        let mut worker_handle = None;
        let mut search_state = SearchState::Searching;
        let mut search_id_counter = 0u64;
        let mut current_search_id = 21u64;
        let mut current_search_is_ponder = true;
        let mut current_session: Option<SearchSession> = None;
        let mut current_bestmove_emitter: Option<BestmoveEmitter> =
            Some(BestmoveEmitter::new(current_search_id));
        let mut current_stop_flag: Option<Arc<AtomicBool>> = Some(flag);
        let mut position_state: Option<PositionState> = None;
        let program_start = Instant::now();
        let mut last_partial_result: Option<(String, u8, i32)> = None;
        let mut pre_session_fallback: Option<String> = None;
        let mut pre_session_fallback_hash: Option<u64> = None;

        let start_idx = test_info_len();

        let mut ctx = CommandContext {
            engine: &engine,
            stop_flag: &Arc::new(AtomicBool::new(false)),
            worker_tx: &tx,
            worker_rx: &rx,
            worker_handle: &mut worker_handle,
            search_state: &mut search_state,
            search_id_counter: &mut search_id_counter,
            current_search_id: &mut current_search_id,
            current_search_is_ponder: &mut current_search_is_ponder,
            current_session: &mut current_session,
            current_bestmove_emitter: &mut current_bestmove_emitter,
            current_stop_flag: &mut current_stop_flag,
            allow_null_move: false,
            position_state: &mut position_state,
            program_start,
            last_partial_result: &mut last_partial_result,
            pre_session_fallback: &mut pre_session_fallback,
            pre_session_fallback_hash: &mut pre_session_fallback_hash,
        };

        handle_stop_command(&mut ctx).unwrap();

        let infos = test_info_from(start_idx);
        let sent_count = infos
            .iter()
            .filter(|s| s.contains("kind=bestmove_sent") && s.contains("search_id=21\t"))
            .count();
        assert_eq!(sent_count, 1, "expected 1 bestmove_sent: {:?}", infos);
        let found = infos
            .iter()
            .any(|s| s.contains("kind=on_stop_source") && s.contains("src=emergency"));
        assert!(found, "ponder on_stop_source=emergency not found in infos: {:?}", infos);
    }

    /// GameOver should finalize without emitting bestmove
    #[test]
    fn test_gameover_finalizes_without_bestmove() {
        std::env::set_var("USI_DRY_RUN", "1");

        let engine = Arc::new(Mutex::new(EngineAdapter::new()));
        {
            let mut adapter = engine.lock().unwrap();
            adapter.set_position(true, None, &[]).unwrap();
        }

        let (tx, rx) = unbounded();
        let flag = Arc::new(AtomicBool::new(false));

        let mut worker_handle = None;
        let mut search_state = SearchState::Searching;
        let mut search_id_counter = 0u64;
        let mut current_search_id = 30u64;
        let mut current_search_is_ponder = false;
        let mut current_session: Option<SearchSession> = None;
        let mut current_bestmove_emitter: Option<BestmoveEmitter> =
            Some(BestmoveEmitter::new(current_search_id));
        let mut current_stop_flag: Option<Arc<AtomicBool>> = Some(flag);
        let mut position_state: Option<PositionState> = None;
        let program_start = Instant::now();
        let mut last_partial_result: Option<(String, u8, i32)> = None;
        let mut pre_session_fallback: Option<String> = None;
        let mut pre_session_fallback_hash: Option<u64> = None;

        let start_idx = test_info_len();

        // Invoke GameOver
        let mut ctx = CommandContext {
            engine: &engine,
            stop_flag: &Arc::new(AtomicBool::new(false)),
            worker_tx: &tx,
            worker_rx: &rx,
            worker_handle: &mut worker_handle,
            search_state: &mut search_state,
            search_id_counter: &mut search_id_counter,
            current_search_id: &mut current_search_id,
            current_search_is_ponder: &mut current_search_is_ponder,
            current_session: &mut current_session,
            current_bestmove_emitter: &mut current_bestmove_emitter,
            current_stop_flag: &mut current_stop_flag,
            allow_null_move: false,
            position_state: &mut position_state,
            program_start,
            last_partial_result: &mut last_partial_result,
            pre_session_fallback: &mut pre_session_fallback,
            pre_session_fallback_hash: &mut pre_session_fallback_hash,
        };

        handle_command(
            UsiCommand::GameOver {
                result: crate::usi::commands::GameResult::Win,
            },
            &mut ctx,
        )
        .unwrap();

        let infos = test_info_from(start_idx);
        // No bestmove_sent for search_id=30
        let sent_count = infos
            .iter()
            .filter(|s| s.contains("kind=bestmove_sent") && s.contains("search_id=30\t"))
            .count();
        assert_eq!(sent_count, 0, "bestmove_sent should NOT be emitted on gameover: {:?}", infos);
        // Ensure search finalized to idle
        assert_eq!(*ctx.search_state, SearchState::Idle);
        assert!(ctx.current_bestmove_emitter.is_none());
    }
}
