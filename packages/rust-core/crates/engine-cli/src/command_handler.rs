use crate::engine_adapter::EngineAdapter;
// use crate::helpers::{generate_fallback_move, wait_for_search_completion};
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
use engine_core::engine::controller::Engine;
use engine_core::search::{types::StopInfo, CommittedIteration};
use engine_core::usi::move_to_usi;

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
    // legacy session removed
    pub current_session: &'a mut Option<()>,
    /// Latest committed iteration from core (preferred over session)
    pub current_committed: &'a mut Option<CommittedIteration>,
    pub current_bestmove_emitter: &'a mut Option<BestmoveEmitter>,
    /// Per-search finalized flag shared with worker for sender-side suppression
    pub current_finalized_flag: &'a mut Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    pub current_stop_flag: &'a mut Option<Arc<AtomicBool>>, // Per-search stop flag
    pub allow_null_move: bool,
    pub position_state: &'a mut Option<PositionState>, // Store position state for recovery
    pub program_start: Instant, // Program start time for elapsed calculations
    /// Last received partial result (move, depth, score) for current search
    pub last_partial_result: &'a mut Option<(String, u8, i32)>,
    /// Search start time for current search (worker reported)
    pub search_start_time: &'a mut Option<std::time::Instant>,
    /// Latest global nodes snapshot (from committed iteration)
    pub latest_nodes: &'a mut u64,
    /// Soft limit ms (best-effort; may be refined when budgets are available)
    pub soft_limit_ms_ctx: &'a mut u64,
    /// Root legal move set snapshot captured from latest committed iteration
    pub root_legal_moves: &'a mut Option<Vec<String>>,
    /// Guard to ensure HardDeadlineFire backstop emits exactly-once per search
    pub hard_deadline_taken: &'a mut bool,
    /// Precomputed root fallback move captured at go-time for stop-time emergencies
    pub pre_session_fallback: &'a mut Option<String>,
    /// Hash of the position when pre_session_fallback was computed
    pub pre_session_fallback_hash: &'a mut Option<u64>,
    /// Timestamp of last bestmove successfully sent
    pub last_bestmove_sent_at: &'a mut Option<std::time::Instant>,
    /// Timestamp when the latest go handler started
    pub last_go_begin_at: &'a mut Option<std::time::Instant>,
    /// Guard to ensure final PV is injected exactly once per search
    pub final_pv_injected: &'a mut bool,
    /// Pending StopInfo from SearchFinished to be used after join
    pub pending_stop_info: &'a mut Option<StopInfo>,
    /// Pending Engine returned by worker guard, to be returned to adapter after join
    pub pending_returned_engine: &'a mut Option<Engine>,
}

impl<'a> CommandContext<'a> {
    /// Inject a final PV info line once per search, guarded by a flag
    pub fn inject_final_pv(&mut self, info: crate::usi::output::SearchInfo, source: &str) {
        // Safety: if emitter is finalized or terminated, suppress any late injections
        if let Some(em) = self.current_bestmove_emitter.as_ref() {
            if em.is_finalized() || em.is_terminated() {
                return;
            }
        }

        if !*self.final_pv_injected {
            let _ = crate::usi::send_response(crate::usi::UsiResponse::Info(info));
            let _ = crate::usi::send_info_string(crate::emit_utils::log_tsv(&[
                ("kind", "final_pv_injected"),
                ("source", source),
            ]));
            *self.final_pv_injected = true;
        }
    }

    /// Central finalize: choose final bestmove via core and emit once, with minimal guards
    /// Returns true if emission occurred.
    pub fn finalize_emit_if_possible(
        &mut self,
        path: &str,
        stop_info: Option<StopInfo>,
    ) -> Result<bool> {
        let finalize_begin = Instant::now();
        // Flags snapshot
        let (emitter_present, finalized, terminated) =
            if let Some(ref em) = self.current_bestmove_emitter {
                (true, em.is_finalized(), em.is_terminated())
            } else {
                (false, false, false)
            };
        let ponder = *self.current_search_is_ponder;
        // Log attempt with flags
        let _ = send_info_string(log_tsv(&[
            ("kind", "finalize_attempt"),
            ("search_id", &self.current_search_id.to_string()),
            ("path", path),
            ("finalized", if finalized { "1" } else { "0" }),
            ("emitter_present", if emitter_present { "1" } else { "0" }),
            ("ponder", if ponder { "1" } else { "0" }),
            ("state", &format!("{:?}", *self.search_state)),
        ]));

        // Minimal guard: only block if already finalized or terminated
        if finalized || terminated {
            let _ = send_info_string(log_tsv(&[
                ("kind", "finalize_guard_blocked"),
                ("search_id", &self.current_search_id.to_string()),
                ("finalized", if finalized { "1" } else { "0" }),
                ("terminated", if terminated { "1" } else { "0" }),
            ]));
            return Ok(false);
        }

        // Decide bestmove via core
        let adapter = crate::worker::lock_or_recover_adapter(self.engine);
        if let Some((bm0, pv0, src0)) =
            adapter.choose_final_bestmove_core(self.current_committed.as_ref())
        {
            // Defensive legality guard before emit: verify chosen move is legal in current position.
            // If not, pick the first move that passes Position::is_legal_move(), else resign.
            let mut bm = bm0;
            let mut pv = pv0;
            let mut src = src0;

            if let Some(pos) = adapter.get_position() {
                if let Ok(parsed) = engine_core::usi::parse_usi_move(&bm) {
                    if !pos.is_legal_move(parsed) {
                        // Attempt to find a guaranteed legal move by filtering movegen output
                        let mg = engine_core::movegen::MoveGenerator::new();
                        let mut replaced = false;
                        if let Ok(list) = mg.generate_all(pos) {
                            if let Some(mv) =
                                list.as_slice().iter().copied().find(|&m| pos.is_legal_move(m))
                            {
                                let usi = engine_core::usi::move_to_usi(&mv);
                                let _ = send_info_string(log_tsv(&[
                                    ("kind", "finalize_illegal_move_guard"),
                                    ("old", &bm),
                                    ("new", &usi),
                                ]));
                                bm = usi.clone();
                                pv = vec![usi];
                                src = "illegal_guard".to_string();
                                replaced = true;
                            }
                        }
                        if !replaced {
                            let _ = send_info_string(log_tsv(&[
                                ("kind", "finalize_illegal_move_guard"),
                                ("old", &bm),
                                ("new", "resign"),
                            ]));
                            bm = "resign".to_string();
                            pv = vec!["resign".to_string()];
                            src = "illegal_guard_resign".to_string();
                        }
                    }
                }
            }

            // Inject final PV then emit
            let info = crate::usi::output::SearchInfo {
                multipv: Some(1),
                pv,
                ..Default::default()
            };
            self.inject_final_pv(info, "central_finalize");
            let meta = crate::emit_utils::build_meta(
                crate::types::BestmoveSource::CoreFinalize,
                0,
                None,
                Some(format!("string core_src={src}")),
                stop_info,
            );
            self.emit_and_finalize(bm.clone(), None, meta, &format!("CentralFinalize:{path}"))?;
            let _ = send_info_string(log_tsv(&[
                ("kind", "finalize_source"),
                ("search_id", &self.current_search_id.to_string()),
                ("source", &src),
            ]));
            let _ = send_info_string(log_tsv(&[
                ("kind", "finalize_elapsed_ms"),
                ("search_id", &self.current_search_id.to_string()),
                ("ms", &finalize_begin.elapsed().as_millis().to_string()),
            ]));
            return Ok(true);
        }

        let _ = send_info_string(log_tsv(&[
            ("kind", "finalize_guard_blocked"),
            ("reason", "engine_or_position_missing"),
            ("search_id", &self.current_search_id.to_string()),
        ]));
        Ok(false)
    }
    /// Try to emit bestmove from a committed iteration
    pub(crate) fn emit_best_from_committed(
        &mut self,
        committed: &CommittedIteration,
        from: BestmoveSource,
        stop_info: Option<StopInfo>,
        finalize_label: &str,
    ) -> Result<bool> {
        let adapter = lock_or_recover_adapter(self.engine);
        if let Some(position) = adapter.get_position() {
            if let Ok((best_move, ponder, ponder_source)) =
                adapter.validate_and_get_bestmove_from_committed(committed, position)
            {
                // Build score string from engine-internal score
                let score_enum = crate::utils::to_usi_score(committed.score);
                let score_str = Some(match score_enum {
                    crate::usi::output::Score::Cp(cp) => format!("cp {cp}"),
                    crate::usi::output::Score::Mate(m) => format!("mate {m}"),
                });

                // Metrics
                let pv_len_str = committed.pv.len().to_string();
                let ponder_src_str = ponder_source.to_string();
                let metrics = log_tsv(&[
                    ("kind", "bestmove_metrics"),
                    ("search_id", &self.current_search_id.to_string()),
                    ("pv_len", &pv_len_str),
                    ("ponder_source", &ponder_src_str),
                    ("ponder_present", if ponder.is_some() { "true" } else { "false" }),
                ]);
                let _ = send_info_string(metrics);

                // Inject a final PV built from the committed iteration just before bestmove
                // Only emit when the committed PV's first move matches the bestmove to avoid
                // showing a PV that doesn't correspond to the emitted move (e.g., emergency fallback).
                if let Some(first_mv) = committed.pv.first() {
                    let pv_best = move_to_usi(first_mv);
                    if pv_best == best_move {
                        let elapsed_ms = committed.elapsed.as_millis() as u64;
                        let time_opt = if elapsed_ms > 0 {
                            Some(elapsed_ms)
                        } else {
                            None
                        };
                        let nps_opt = if elapsed_ms > 0 && committed.nodes > 0 {
                            Some(committed.nodes.saturating_mul(1000) / elapsed_ms)
                        } else {
                            None
                        };
                        let info = crate::usi::output::SearchInfo {
                            multipv: Some(1),
                            depth: Some(committed.depth as u32),
                            seldepth: committed.seldepth.map(|v| v as u32),
                            time: time_opt,
                            nodes: Some(committed.nodes),
                            pv: committed.pv.iter().map(move_to_usi).collect::<Vec<_>>(),
                            score: Some(score_enum),
                            score_bound: Some(committed.node_type.into()),
                            nps: nps_opt,
                            ..Default::default()
                        };
                        self.inject_final_pv(info, "committed");
                    }
                }

                let meta =
                    build_meta(from, committed.depth, committed.seldepth, score_str, stop_info);
                self.emit_and_finalize(best_move, ponder, meta, finalize_label)?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    #[inline]
    pub fn finalize_search(&mut self, where_: &str) {
        log::debug!("Finalize search {} ({})", *self.current_search_id, where_);

        // Guard against multiple finalization
        if *self.search_state == SearchState::Finalized {
            log::debug!("Search already finalized, skipping finalize_search from {}", where_);
            return;
        }

        self.search_state.set_finalized();
        *self.current_search_is_ponder = false;
        *self.current_bestmove_emitter = None;
        // Mark finalized flag if present for sender-side suppression
        if let Some(flag) = self.current_finalized_flag.as_ref() {
            flag.store(true, std::sync::atomic::Ordering::Release);
        }
        *self.current_session = None;

        // Drop the current stop flag without resetting it
        // This prevents race conditions where worker might miss the stop signal
        let _ = self.current_stop_flag.take();

        // USI-visible: state transitioned to Finalized immediately after finalize
        let _ = send_info_string(crate::emit_utils::log_tsv(&[
            ("kind", "state_finalized_after_bestmove"),
            ("search_id", &self.current_search_id.to_string()),
        ]));
    }

    /// Transition from Finalized to Idle state if currently Finalized
    /// This helper ensures consistent state transition with logging
    pub fn transition_to_idle_if_finalized(&mut self, reason: &str) {
        if *self.search_state == SearchState::Finalized {
            *self.search_state = SearchState::Idle;
            let _ = send_info_string(crate::emit_utils::log_tsv(&[
                ("kind", "state_idle_after_finished"),
                ("search_id", &self.current_search_id.to_string()),
                ("reason", reason),
            ]));
            log::debug!("Transitioned to Idle from Finalized ({})", reason);
        }
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
        let finalize_start = std::time::Instant::now();
        // Note: finalize chain must log finalize_end and transition to Idle.
        let bm_for_log = best_move.clone();
        // USI-visible diagnostic: finalize entry
        let _ =
            send_info_string(crate::emit_utils::log_tsv(&[("kind", "bestmove_finalize_begin")]));
        // Emit paths below call finalize_search() themselves; guard tail removed.
        // Metrics logging is handled before this call in emit_best_from_session
        // Try to emit via BestmoveEmitter if available
        if let Some(ref emitter) = self.current_bestmove_emitter {
            match emitter.emit(best_move.clone(), ponder.clone(), meta.clone()) {
                Ok(()) => {
                    // Mark the time when bestmove was sent successfully
                    *self.last_bestmove_sent_at = Some(std::time::Instant::now());
                    // Emit unified bestmove_sent (centralized here)
                    let seldepth_str = meta
                        .stats
                        .seldepth
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "-".to_string());
                    let ponder_str = ponder.as_deref().unwrap_or("none");
                    let info_string = format!(
                        "kind=bestmove_sent\t\
                         search_id={}\t\
                         bestmove_from={}\t\
                         stop_reason={}\t\
                         depth={}\t\
                         seldepth={}\t\
                         depth_reached={}\t\
                         score={}\t\
                         nodes={}\t\
                         nps={}\t\
                         elapsed_ms={}\t\
                         time_soft_ms={}\t\
                         time_hard_ms={}\t\
                         hard_timeout={}\t\
                         bestmove={}\t\
                         ponder={}",
                        self.current_search_id,
                        meta.from,
                        meta.stop_info.reason,
                        meta.stats.depth,
                        seldepth_str,
                        meta.stop_info.depth_reached,
                        meta.stats.score,
                        meta.stats.nodes,
                        meta.stats.nps,
                        meta.stop_info.elapsed_ms,
                        meta.stop_info.soft_limit_ms,
                        meta.stop_info.hard_limit_ms,
                        meta.stop_info.hard_timeout,
                        bm_for_log.clone(),
                        ponder_str
                    );
                    let _ = send_info_string(info_string);
                    // Additional confirm log for observability
                    let _ = send_info_string(crate::emit_utils::log_tsv(&[
                        ("kind", "bestmove_sent_logged"),
                        ("search_id", &self.current_search_id.to_string()),
                    ]));
                    // Emit finalize_end before transitioning Idle
                    let _ = send_info_string(crate::emit_utils::log_tsv(&[
                        ("kind", "bestmove_finalize_end"),
                        ("path", "emitter"),
                    ]));
                    self.finalize_search(finalize_label);
                    // Latency from finalize_begin to finalize_end
                    let _ = send_info_string(crate::emit_utils::log_tsv(&[
                        ("kind", "bestmove_finalize_latency"),
                        ("ms", &finalize_start.elapsed().as_millis().to_string()),
                    ]));
                    // Do not return here; fall through to tail guard
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
                    // Inject final PV before direct send (emitter failed)
                    let pv_info = crate::usi::output::SearchInfo {
                        multipv: Some(1),
                        pv: vec![best_move.clone()],
                        ..Default::default()
                    };
                    self.inject_final_pv(pv_info, "emitter_failed");
                    // Try direct send as fallback
                    if let Err(e) = send_response(UsiResponse::BestMove { best_move, ponder }) {
                        log::error!("Failed to send bestmove even with direct fallback: {e}");
                        // Continue without propagating error - USI requires best effort
                    }
                    // Always finalize search after attempting to emit
                    *self.last_bestmove_sent_at = Some(std::time::Instant::now());
                    // Emit unified bestmove_sent for fallback as well
                    let seldepth_str = meta
                        .stats
                        .seldepth
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "-".to_string());
                    let ponder_str = "none"; // unknown on fallback failure path
                    let info_string = format!(
                        "kind=bestmove_sent\t\
                         search_id={}\t\
                         bestmove_from={}\t\
                         stop_reason={}\t\
                         depth={}\t\
                         seldepth={}\t\
                         depth_reached={}\t\
                         score={}\t\
                         nodes={}\t\
                         nps={}\t\
                         elapsed_ms={}\t\
                         time_soft_ms={}\t\
                         time_hard_ms={}\t\
                         hard_timeout={}\t\
                         bestmove={}\t\
                         ponder={}",
                        self.current_search_id,
                        meta.from,
                        meta.stop_info.reason,
                        meta.stats.depth,
                        seldepth_str,
                        meta.stop_info.depth_reached,
                        meta.stats.score,
                        meta.stats.nodes,
                        meta.stats.nps,
                        meta.stop_info.elapsed_ms,
                        meta.stop_info.soft_limit_ms,
                        meta.stop_info.hard_limit_ms,
                        meta.stop_info.hard_timeout,
                        bm_for_log.clone(),
                        ponder_str
                    );
                    let _ = send_info_string(info_string);
                    // Additional confirm log for observability
                    let _ = send_info_string(crate::emit_utils::log_tsv(&[
                        ("kind", "bestmove_sent_logged"),
                        ("search_id", &self.current_search_id.to_string()),
                    ]));
                    let _ = send_info_string(crate::emit_utils::log_tsv(&[
                        ("kind", "bestmove_finalize_end"),
                        ("path", "emitter_fallback"),
                    ]));
                    self.finalize_search(finalize_label);
                    let _ = send_info_string(crate::emit_utils::log_tsv(&[
                        ("kind", "bestmove_finalize_latency"),
                        ("ms", &finalize_start.elapsed().as_millis().to_string()),
                    ]));
                    // Do not return here; fall through to tail guard
                }
            }
        } else {
            log::warn!("BestmoveEmitter not available; sending bestmove directly");
            // Send TSV log for direct send
            self.send_fallback_tsv_log(&best_move, ponder.as_deref(), Some(&meta), "no_emitter");
            // Inject final PV before direct send (no emitter)
            let pv_info = crate::usi::output::SearchInfo {
                multipv: Some(1),
                pv: vec![best_move.clone()],
                ..Default::default()
            };
            self.inject_final_pv(pv_info, "direct_emit");
            if let Err(e) = send_response(UsiResponse::BestMove { best_move, ponder }) {
                log::error!("Failed to send bestmove directly: {e}");
                // Continue without propagating error - USI requires best effort
            }
            // Always finalize search after attempting to emit
            *self.last_bestmove_sent_at = Some(std::time::Instant::now());
            // Emit unified bestmove_sent for direct path as well
            let seldepth_str =
                meta.stats.seldepth.map(|v| v.to_string()).unwrap_or_else(|| "-".to_string());
            let ponder_str = "none";
            let info_string = format!(
                "kind=bestmove_sent\t\
                 search_id={}\t\
                 bestmove_from={}\t\
                 stop_reason={}\t\
                 depth={}\t\
                 seldepth={}\t\
                 depth_reached={}\t\
                 score={}\t\
                 nodes={}\t\
                 nps={}\t\
                 elapsed_ms={}\t\
                 time_soft_ms={}\t\
                 time_hard_ms={}\t\
                 hard_timeout={}\t\
                 bestmove={}\t\
                 ponder={}",
                self.current_search_id,
                meta.from,
                meta.stop_info.reason,
                meta.stats.depth,
                seldepth_str,
                meta.stop_info.depth_reached,
                meta.stats.score,
                meta.stats.nodes,
                meta.stats.nps,
                meta.stop_info.elapsed_ms,
                meta.stop_info.soft_limit_ms,
                meta.stop_info.hard_limit_ms,
                meta.stop_info.hard_timeout,
                bm_for_log,
                ponder_str
            );
            let _ = send_info_string(info_string);
            // Additional confirm log for observability
            let _ = send_info_string(crate::emit_utils::log_tsv(&[
                ("kind", "bestmove_sent_logged"),
                ("search_id", &self.current_search_id.to_string()),
            ]));
            // finalize_end before transitioning Idle
            let _ = send_info_string(crate::emit_utils::log_tsv(&[
                ("kind", "bestmove_finalize_end"),
                ("path", "direct"),
            ]));
            self.finalize_search(finalize_label);
            let _ = send_info_string(crate::emit_utils::log_tsv(&[
                ("kind", "bestmove_finalize_latency"),
                ("ms", &finalize_start.elapsed().as_millis().to_string()),
            ]));
            // Do not return here; fall through to tail guard
        }
        // Tail guard removed: finalize is executed in each branch above.
        Ok(())
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
    use crate::handlers::go::handle_go_command;
    use crate::handlers::ponder::handle_ponder_hit;
    use crate::usi::output::{test_info_from, test_info_len};
    use crossbeam_channel::unbounded;
    use engine_core::search::{types::TerminationReason, CommittedIteration, NodeType};
    use engine_core::usi::parse_usi_move;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};

    #[test]
    fn test_build_meta_reason_mapping() {
        use crate::types::BestmoveSource as S;

        // Timeout sources map to TimeLimit and hard_timeout true only for explicit timeout variants
        let timeout_sources = [S::EmergencyFallbackTimeout, S::PartialResultTimeout];
        for &src in &timeout_sources {
            let m = build_meta(src, 7, Some(9), Some("cp 10".into()), None);
            assert_eq!(m.stop_info.reason, TerminationReason::TimeLimit);
            assert!(m.stop_info.hard_timeout);
            assert_eq!(m.stop_info.depth_reached, 7);
        }

        // Normal completion
        for &src in &[S::EmergencyFallbackOnFinish, S::CoreFinalize] {
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
        let m = build_meta(S::CoreFinalize, 1, None, Some("cp 0".into()), Some(si.clone()));
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
        let mut current_session: Option<()> = None;
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

        let mut final_pv_injected_flag = false;
        let mut hard_deadline_taken = false;
        let mut root_legal_moves: Option<Vec<String>> = None;
        let mut search_start_time: Option<Instant> = None;
        let mut latest_nodes: u64 = 0;
        let mut soft_limit_ms_ctx: u64 = 0;
        // Pending fields for unified finalize
        let mut pending_stop_info: Option<StopInfo> = None;
        let mut pending_returned_engine: Option<Engine> = None;
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
            current_finalized_flag: &mut None,
            current_stop_flag: &mut current_stop_flag,
            allow_null_move: false,
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
            current_committed: &mut None,
            last_bestmove_sent_at: &mut None,
            last_go_begin_at: &mut None,
            final_pv_injected: &mut final_pv_injected_flag,
            pending_stop_info: &mut pending_stop_info,
            pending_returned_engine: &mut pending_returned_engine,
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
        let mut current_session: Option<()> = None;
        let mut current_bestmove_emitter: Option<BestmoveEmitter> =
            Some(BestmoveEmitter::new(current_search_id));
        let mut current_stop_flag: Option<Arc<AtomicBool>> = Some(search_stop_flag);
        let mut position_state: Option<PositionState> = None;
        let program_start = Instant::now();
        let mut last_partial_result: Option<(String, u8, i32)> = None;
        let mut pre_session_fallback: Option<String> = Some("7g7f".to_string());
        let mut pre_session_fallback_hash: Option<u64> = Some(0); // Intentional mismatch
        let mut current_committed: Option<CommittedIteration> = None;

        // Clear test hooks
        let start_idx = test_info_len();

        let mut final_pv_injected_flag = false;
        let mut hard_deadline_taken = false;
        let mut root_legal_moves: Option<Vec<String>> = None;
        let mut search_start_time: Option<Instant> = None;
        let mut latest_nodes: u64 = 0;
        let mut soft_limit_ms_ctx: u64 = 0;
        // Pending fields for unified finalize
        let mut pending_stop_info: Option<StopInfo> = None;
        let mut pending_returned_engine: Option<Engine> = None;
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
            current_finalized_flag: &mut None,
            current_stop_flag: &mut current_stop_flag,
            allow_null_move: true, // permit null move emergency if needed
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
            last_bestmove_sent_at: &mut None,
            last_go_begin_at: &mut None,
            final_pv_injected: &mut final_pv_injected_flag,
            pending_stop_info: &mut pending_stop_info,
            pending_returned_engine: &mut pending_returned_engine,
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

    /// Verify stop prefers committed when committed best exists
    #[test]
    fn test_on_stop_source_session_committed() {
        std::env::set_var("USI_DRY_RUN", "1");

        // Engine and position
        let engine = Arc::new(Mutex::new(EngineAdapter::new()));
        {
            let mut adapter = engine.lock().unwrap();
            adapter.set_position(true, None, &[]).unwrap();
        }

        // Build a committed iteration best
        let mv = parse_usi_move("7g7f").unwrap();
        let committed_iter = CommittedIteration {
            depth: 12,
            seldepth: Some(14),
            score: 32,
            pv: vec![mv],
            node_type: NodeType::Exact,
            nodes: 0,
            elapsed: std::time::Duration::from_millis(0),
        };

        // Channels and stop flag
        let (tx, rx) = unbounded();
        let search_stop_flag = Arc::new(AtomicBool::new(false));

        // Context
        let mut worker_handle = None;
        let mut search_state = SearchState::Searching;
        let mut search_id_counter = 0u64;
        let mut current_search_id = 10u64;
        let mut current_search_is_ponder = false;
        let mut current_session: Option<()> = None;
        let mut current_bestmove_emitter: Option<BestmoveEmitter> =
            Some(BestmoveEmitter::new(current_search_id));
        let mut current_stop_flag: Option<Arc<AtomicBool>> = Some(search_stop_flag);
        let mut position_state: Option<PositionState> = None;
        let program_start = Instant::now();
        let mut last_partial_result: Option<(String, u8, i32)> = None;
        let mut pre_session_fallback: Option<String> = None;
        let mut pre_session_fallback_hash: Option<u64> = None;
        let mut current_committed: Option<CommittedIteration> = Some(committed_iter);

        let start_idx = test_info_len();

        let mut final_pv_injected_flag = false;
        let mut hard_deadline_taken = false;
        let mut root_legal_moves: Option<Vec<String>> = None;
        let mut search_start_time: Option<Instant> = None;
        let mut latest_nodes: u64 = 0;
        let mut soft_limit_ms_ctx: u64 = 0;
        // Pending fields for unified finalize
        let mut pending_stop_info: Option<StopInfo> = None;
        let mut pending_returned_engine: Option<Engine> = None;
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
            current_finalized_flag: &mut None,
            current_stop_flag: &mut current_stop_flag,
            allow_null_move: false,
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
            last_bestmove_sent_at: &mut None,
            last_go_begin_at: &mut None,
            final_pv_injected: &mut final_pv_injected_flag,
            pending_stop_info: &mut pending_stop_info,
            pending_returned_engine: &mut pending_returned_engine,
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
            .any(|s| s.contains("kind=on_stop_source") && s.contains("src=committed"));
        assert!(found, "on_stop_source=committed not found in infos: {:?}", infos);
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
        let mut current_session: Option<()> = None;
        let mut current_bestmove_emitter: Option<BestmoveEmitter> =
            Some(BestmoveEmitter::new(current_search_id));
        let mut current_stop_flag: Option<Arc<AtomicBool>> = Some(flag);
        let mut position_state: Option<PositionState> = None;
        let program_start = Instant::now();
        let mut last_partial_result: Option<(String, u8, i32)> = None;
        let mut pre_session_fallback: Option<String> = Some("7g7f".to_string());
        let mut pre_session_fallback_hash: Option<u64> = Some(root_hash);

        let start_idx = test_info_len();

        let mut final_pv_injected_flag = false;
        let mut hard_deadline_taken = false;
        let mut root_legal_moves: Option<Vec<String>> = None;
        let mut search_start_time: Option<Instant> = None;
        let mut latest_nodes: u64 = 0;
        let mut soft_limit_ms_ctx: u64 = 0;
        // Pending fields for unified finalize
        let mut pending_stop_info: Option<StopInfo> = None;
        let mut pending_returned_engine: Option<Engine> = None;
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
            current_finalized_flag: &mut None,
            current_stop_flag: &mut current_stop_flag,
            allow_null_move: false,
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
            current_committed: &mut None,
            last_bestmove_sent_at: &mut None,
            last_go_begin_at: &mut None,
            final_pv_injected: &mut final_pv_injected_flag,
            pending_stop_info: &mut pending_stop_info,
            pending_returned_engine: &mut pending_returned_engine,
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
        let mut current_session: Option<()> = None;
        let mut current_bestmove_emitter: Option<BestmoveEmitter> =
            Some(BestmoveEmitter::new(current_search_id));
        let mut current_stop_flag: Option<Arc<AtomicBool>> = Some(flag);
        let mut position_state: Option<PositionState> = None;
        let program_start = Instant::now();
        let mut last_partial_result: Option<(String, u8, i32)> = None;
        let mut pre_session_fallback: Option<String> = None;
        let mut pre_session_fallback_hash: Option<u64> = None;

        let start_idx = test_info_len();

        let mut final_pv_injected_flag = false;
        let mut hard_deadline_taken = false;
        let mut root_legal_moves: Option<Vec<String>> = None;
        let mut search_start_time: Option<Instant> = None;
        let mut latest_nodes: u64 = 0;
        let mut soft_limit_ms_ctx: u64 = 0;
        // Pending fields for unified finalize
        let mut pending_stop_info: Option<StopInfo> = None;
        let mut pending_returned_engine: Option<Engine> = None;
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
            current_finalized_flag: &mut None,
            current_stop_flag: &mut current_stop_flag,
            allow_null_move: false,
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
            current_committed: &mut None,
            last_bestmove_sent_at: &mut None,
            last_go_begin_at: &mut None,
            final_pv_injected: &mut final_pv_injected_flag,
            pending_stop_info: &mut pending_stop_info,
            pending_returned_engine: &mut pending_returned_engine,
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

        // Send a dummy Finished message to simulate worker completion
        // This is needed because wait_for_worker_sync blocks on recv()
        // waiting for WorkerMessage::Finished when SearchState::Searching
        tx.send(WorkerMessage::Finished {
            from_guard: true,
            search_id: 30,
        })
        .unwrap();

        let mut worker_handle = None;
        let mut search_state = SearchState::Searching;
        let mut search_id_counter = 0u64;
        let mut current_search_id = 30u64;
        let mut current_search_is_ponder = false;
        let mut current_session: Option<()> = None;
        let mut current_bestmove_emitter: Option<BestmoveEmitter> =
            Some(BestmoveEmitter::new(current_search_id));
        let mut current_stop_flag: Option<Arc<AtomicBool>> = Some(flag);
        let mut position_state: Option<PositionState> = None;
        let program_start = Instant::now();
        let mut last_partial_result: Option<(String, u8, i32)> = None;
        let mut pre_session_fallback: Option<String> = None;
        let mut pre_session_fallback_hash: Option<u64> = None;

        let start_idx = test_info_len();

        // Local flag for PV injection guard
        let mut final_pv_injected_flag = false;
        let mut hard_deadline_taken = false;
        let mut root_legal_moves: Option<Vec<String>> = None;
        let mut search_start_time: Option<Instant> = None;
        let mut latest_nodes: u64 = 0;
        let mut soft_limit_ms_ctx: u64 = 0;

        // Invoke GameOver
        // Pending fields for unified finalize
        let mut pending_stop_info: Option<StopInfo> = None;
        let mut pending_returned_engine: Option<Engine> = None;
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
            current_finalized_flag: &mut None,
            current_stop_flag: &mut current_stop_flag,
            allow_null_move: false,
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
            current_committed: &mut None,
            last_bestmove_sent_at: &mut None,
            last_go_begin_at: &mut None,
            final_pv_injected: &mut final_pv_injected_flag,
            pending_stop_info: &mut pending_stop_info,
            pending_returned_engine: &mut pending_returned_engine,
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
        // Ensure search finalized (but not yet idle - worker not joined)
        assert_eq!(*ctx.search_state, SearchState::Finalized);
        assert!(ctx.current_bestmove_emitter.is_none());
    }

    // (old wait_for_bestmove_sent_since removed; pump_until_bestmove drives the loop)

    // Helper: pump worker messages and drive finalization similar to main loop
    fn pump_until_bestmove(
        ctx: &mut CommandContext,
        timeout_ms: u64,
        start_idx: usize,
    ) -> Vec<String> {
        let start = std::time::Instant::now();
        while start.elapsed().as_millis() as u64 <= timeout_ms {
            // Check if bestmove was already sent
            let infos = test_info_from(start_idx);
            if infos
                .iter()
                .any(|s| s.contains("kind=bestmove_sent") && s.contains("bestmove="))
            {
                return infos;
            }

            // Try to receive a message from worker
            match ctx.worker_rx.recv_timeout(std::time::Duration::from_millis(20)) {
                Ok(crate::worker::WorkerMessage::SearchFinished {
                    root_hash: _,
                    search_id,
                    stop_info,
                }) => {
                    if search_id == *ctx.current_search_id {
                        // Non-ponder path: finalize
                        let _ = ctx.finalize_emit_if_possible("search_finished", stop_info);
                    }
                }
                Ok(crate::worker::WorkerMessage::HardDeadlineFire { search_id, hard_ms }) => {
                    if search_id == *ctx.current_search_id {
                        // Build minimal StopInfo (hard timeout)
                        let base_stop = engine_core::search::types::StopInfo {
                            reason: engine_core::search::types::TerminationReason::TimeLimit,
                            elapsed_ms: 0,
                            nodes: 0,
                            depth_reached: 0,
                            hard_timeout: true,
                            soft_limit_ms: *ctx.soft_limit_ms_ctx,
                            hard_limit_ms: hard_ms,
                        };
                        // Try committed path first
                        if let Some(committed) = ctx.current_committed.clone() {
                            let _ = ctx.emit_best_from_committed(
                                &committed,
                                crate::types::BestmoveSource::EmergencyFallbackTimeout,
                                Some(base_stop.clone()),
                                "HardCommittedTest",
                            );
                        } else if let Some(legal) = ctx.root_legal_moves.as_ref() {
                            if !legal.is_empty() {
                                // Use first legal move
                                let chosen = legal[0].clone();
                                // Inject PV and emit
                                let info = crate::usi::output::SearchInfo {
                                    multipv: Some(1),
                                    pv: vec![chosen.clone()],
                                    ..Default::default()
                                };
                                ctx.inject_final_pv(info, "HardRootLegalTest");
                                let meta = crate::emit_utils::build_meta(
                                    crate::types::BestmoveSource::EmergencyFallbackTimeout,
                                    0,
                                    None,
                                    None,
                                    Some(base_stop.clone()),
                                );
                                let _ =
                                    ctx.emit_and_finalize(chosen, None, meta, "HardRootLegalTest");
                            }
                        } else {
                            // Fallback: emergency
                            if let Ok((move_str, _)) = crate::helpers::generate_fallback_move(
                                ctx.engine, None, false, true,
                            ) {
                                let meta = crate::emit_utils::build_meta(
                                    crate::types::BestmoveSource::EmergencyFallbackTimeout,
                                    0,
                                    None,
                                    None,
                                    Some(base_stop),
                                );
                                let _ = ctx.emit_and_finalize(
                                    move_str,
                                    None,
                                    meta,
                                    "HardEmergencyTest",
                                );
                            }
                        }
                    }
                }
                Ok(crate::worker::WorkerMessage::IterationCommitted {
                    committed,
                    search_id,
                }) => {
                    if search_id == *ctx.current_search_id {
                        // Cache latest committed (used by finalize)
                        *ctx.current_committed = Some(committed);
                    }
                }
                Ok(crate::worker::WorkerMessage::PartialResult { .. }) => {
                    // Not needed for this test
                }
                Ok(crate::worker::WorkerMessage::SearchStarted { start_time, .. }) => {
                    *ctx.search_start_time = Some(start_time);
                }
                Ok(crate::worker::WorkerMessage::Finished {
                    from_guard,
                    search_id,
                }) => {
                    if search_id == *ctx.current_search_id && !from_guard {
                        // Emulate main loop: ensure engine is returned, then finalize
                        if let Some(engine) = ctx.pending_returned_engine.take() {
                            let mut adapter = crate::worker::lock_or_recover_adapter(ctx.engine);
                            adapter.return_engine(engine);
                        }
                        let si = ctx.pending_stop_info.take();
                        let _ = ctx.finalize_emit_if_possible("finished_pump", si);
                    }
                }
                Ok(crate::worker::WorkerMessage::Info { info, .. }) => {
                    // Mirror main loop behavior by forwarding info lines
                    if let Some(s) = info.string {
                        let _ = crate::usi::send_info_string(s);
                    }
                }
                Ok(crate::worker::WorkerMessage::Error { message, .. }) => {
                    let _ =
                        crate::usi::send_info_string(format!("kind=worker_error\tmsg={}", message));
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                    // continue loop
                }
                Ok(crate::worker::WorkerMessage::ReturnEngine { engine, search_id }) => {
                    if search_id == *ctx.current_search_id {
                        *ctx.pending_returned_engine = Some(engine);
                    } else if let Ok(mut adapter) = ctx.engine.try_lock() {
                        adapter.return_engine(engine);
                    } else {
                        let mut adapter = crate::worker::lock_or_recover_adapter(ctx.engine);
                        adapter.return_engine(engine);
                    }
                }
                Err(_) => break,
            }
        }
        test_info_from(start_idx)
    }

    #[test]
    fn test_go_fixedtime_emits_time_limit() {
        // Avoid actual stdout writes; capture info strings in-memory
        std::env::set_var("USI_DRY_RUN", "1");

        // Engine and startpos
        let engine = Arc::new(Mutex::new(EngineAdapter::new()));
        {
            let mut adapter = engine.lock().unwrap();
            adapter.set_position(true, None, &[]).unwrap();
        }

        // Channels and flags
        let (tx, rx) = unbounded::<WorkerMessage>();
        let global_stop = Arc::new(AtomicBool::new(false));

        // Context fields
        let mut worker_handle = None;
        let mut search_state = SearchState::Idle;
        let mut search_id_counter = 0u64;
        let mut current_search_id = 0u64;
        let mut current_search_is_ponder = false;
        let mut current_session: Option<()> = None;
        let mut current_bestmove_emitter: Option<BestmoveEmitter> = None;
        let mut current_finalized_flag: Option<Arc<AtomicBool>> = None;
        let mut current_stop_flag: Option<Arc<AtomicBool>> = None;
        let mut position_state: Option<crate::types::PositionState> = None;
        let program_start = std::time::Instant::now();
        let mut last_partial_result: Option<(String, u8, i32)> = None;
        let mut search_start_time: Option<std::time::Instant> = None;
        let mut latest_nodes: u64 = 0;
        let mut soft_limit_ms_ctx: u64 = 0;
        let mut root_legal_moves: Option<Vec<String>> = None;
        let mut hard_deadline_taken = false;
        let mut pre_session_fallback: Option<String> = None;
        let mut pre_session_fallback_hash: Option<u64> = None;
        let mut last_bestmove_sent_at: Option<std::time::Instant> = None;
        let mut last_go_begin_at: Option<std::time::Instant> = None;
        let mut final_pv_injected = false;
        let mut pending_stop_info: Option<StopInfo> = None;
        let mut pending_returned_engine: Option<Engine> = None;

        let mut ctx = CommandContext {
            engine: &engine,
            stop_flag: &global_stop,
            worker_tx: &tx,
            worker_rx: &rx,
            worker_handle: &mut worker_handle,
            search_state: &mut search_state,
            search_id_counter: &mut search_id_counter,
            current_search_id: &mut current_search_id,
            current_search_is_ponder: &mut current_search_is_ponder,
            current_session: &mut current_session,
            current_committed: &mut None,
            current_bestmove_emitter: &mut current_bestmove_emitter,
            current_finalized_flag: &mut current_finalized_flag,
            current_stop_flag: &mut current_stop_flag,
            allow_null_move: false,
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
            last_bestmove_sent_at: &mut last_bestmove_sent_at,
            last_go_begin_at: &mut last_go_begin_at,
            final_pv_injected: &mut final_pv_injected,
            pending_stop_info: &mut pending_stop_info,
            pending_returned_engine: &mut pending_returned_engine,
        };

        // Start fixed-time search
        let params = crate::usi::GoParams {
            movetime: Some(100), // 100ms
            ..Default::default()
        };
        let start_idx = test_info_len();
        handle_go_command(params, &mut ctx).unwrap();

        // Pump events until bestmove is emitted
        let infos = pump_until_bestmove(&mut ctx, 5000, start_idx);
        assert!(
            infos
                .iter()
                .any(|s| s.contains("kind=bestmove_sent") && s.contains("stop_reason=time_limit")),
            "bestmove_sent with time_limit not found. Infos: {:?}",
            infos
        );
    }

    #[test]
    fn test_go_byoyomi_emits_time_limit() {
        std::env::set_var("USI_DRY_RUN", "1");

        let engine = Arc::new(Mutex::new(EngineAdapter::new()));
        {
            let mut adapter = engine.lock().unwrap();
            adapter.set_position(true, None, &[]).unwrap();
        }

        let (tx, rx) = unbounded::<WorkerMessage>();
        let global_stop = Arc::new(AtomicBool::new(false));

        let mut worker_handle = None;
        let mut search_state = SearchState::Idle;
        let mut search_id_counter = 0u64;
        let mut current_search_id = 0u64;
        let mut current_search_is_ponder = false;
        let mut current_session: Option<()> = None;
        let mut current_bestmove_emitter: Option<BestmoveEmitter> = None;
        let mut current_finalized_flag: Option<Arc<AtomicBool>> = None;
        let mut current_stop_flag: Option<Arc<AtomicBool>> = None;
        let mut position_state: Option<crate::types::PositionState> = None;
        let program_start = std::time::Instant::now();
        let mut last_partial_result: Option<(String, u8, i32)> = None;
        let mut search_start_time: Option<std::time::Instant> = None;
        let mut latest_nodes: u64 = 0;
        let mut soft_limit_ms_ctx: u64 = 0;
        let mut root_legal_moves: Option<Vec<String>> = None;
        let mut hard_deadline_taken = false;
        let mut pre_session_fallback: Option<String> = None;
        let mut pre_session_fallback_hash: Option<u64> = None;
        let mut last_bestmove_sent_at: Option<std::time::Instant> = None;
        let mut last_go_begin_at: Option<std::time::Instant> = None;
        let mut final_pv_injected = false;
        let mut pending_stop_info: Option<StopInfo> = None;
        let mut pending_returned_engine: Option<Engine> = None;

        let mut ctx = CommandContext {
            engine: &engine,
            stop_flag: &global_stop,
            worker_tx: &tx,
            worker_rx: &rx,
            worker_handle: &mut worker_handle,
            search_state: &mut search_state,
            search_id_counter: &mut search_id_counter,
            current_search_id: &mut current_search_id,
            current_search_is_ponder: &mut current_search_is_ponder,
            current_session: &mut current_session,
            current_committed: &mut None,
            current_bestmove_emitter: &mut current_bestmove_emitter,
            current_finalized_flag: &mut current_finalized_flag,
            current_stop_flag: &mut current_stop_flag,
            allow_null_move: false,
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
            last_bestmove_sent_at: &mut last_bestmove_sent_at,
            last_go_begin_at: &mut last_go_begin_at,
            final_pv_injected: &mut final_pv_injected,
            pending_stop_info: &mut pending_stop_info,
            pending_returned_engine: &mut pending_returned_engine,
        };

        // In-byoyomi for side to move
        let params = crate::usi::GoParams {
            btime: Some(0),
            wtime: Some(0),
            byoyomi: Some(300),
            periods: Some(1),
            ..Default::default()
        };
        let start_idx = test_info_len();
        handle_go_command(params, &mut ctx).unwrap();

        let infos = pump_until_bestmove(&mut ctx, 7000, start_idx);
        assert!(
            infos
                .iter()
                .any(|s| s.contains("kind=bestmove_sent") && s.contains("stop_reason=time_limit")),
            "bestmove_sent with time_limit not found (byoyomi). Infos: {:?}",
            infos
        );
    }

    #[test]
    fn test_ponderhit_converts_and_emits() {
        std::env::set_var("USI_DRY_RUN", "1");

        // Engine and startpos
        let engine = Arc::new(Mutex::new(EngineAdapter::new()));
        {
            let mut adapter = engine.lock().unwrap();
            adapter.set_position(true, None, &[]).unwrap();
        }

        // Channels and flags
        let (tx, rx) = unbounded::<WorkerMessage>();
        let global_stop = Arc::new(AtomicBool::new(false));

        // Context fields
        let mut worker_handle = None;
        let mut search_state = SearchState::Idle;
        let mut search_id_counter = 0u64;
        let mut current_search_id = 0u64;
        let mut current_search_is_ponder = false;
        let mut current_session: Option<()> = None;
        let mut current_bestmove_emitter: Option<BestmoveEmitter> = None;
        let mut current_finalized_flag: Option<Arc<AtomicBool>> = None;
        let mut current_stop_flag: Option<Arc<AtomicBool>> = None;
        let mut position_state: Option<crate::types::PositionState> = None;
        let program_start = std::time::Instant::now();
        let mut last_partial_result: Option<(String, u8, i32)> = None;
        let mut search_start_time: Option<std::time::Instant> = None;
        let mut latest_nodes: u64 = 0;
        let mut soft_limit_ms_ctx: u64 = 0;
        let mut root_legal_moves: Option<Vec<String>> = None;
        let mut hard_deadline_taken = false;
        let mut pre_session_fallback: Option<String> = None;
        let mut pre_session_fallback_hash: Option<u64> = None;
        let mut last_bestmove_sent_at: Option<std::time::Instant> = None;
        let mut last_go_begin_at: Option<std::time::Instant> = None;
        let mut final_pv_injected = false;
        let mut pending_stop_info: Option<StopInfo> = None;
        let mut pending_returned_engine: Option<Engine> = None;

        let mut ctx = CommandContext {
            engine: &engine,
            stop_flag: &global_stop,
            worker_tx: &tx,
            worker_rx: &rx,
            worker_handle: &mut worker_handle,
            search_state: &mut search_state,
            search_id_counter: &mut search_id_counter,
            current_search_id: &mut current_search_id,
            current_search_is_ponder: &mut current_search_is_ponder,
            current_session: &mut current_session,
            current_committed: &mut None,
            current_bestmove_emitter: &mut current_bestmove_emitter,
            current_finalized_flag: &mut current_finalized_flag,
            current_stop_flag: &mut current_stop_flag,
            allow_null_move: false,
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
            last_bestmove_sent_at: &mut last_bestmove_sent_at,
            last_go_begin_at: &mut last_go_begin_at,
            final_pv_injected: &mut final_pv_injected,
            pending_stop_info: &mut pending_stop_info,
            pending_returned_engine: &mut pending_returned_engine,
        };

        // Start ponder search with inner fixed-time
        let params = crate::usi::GoParams {
            ponder: true,
            movetime: Some(400),
            ..Default::default()
        };
        let start_idx = test_info_len();
        handle_go_command(params, &mut ctx).unwrap();

        // Ensure no bestmove is sent during ponder (short wait)
        std::thread::sleep(std::time::Duration::from_millis(150));
        let infos_mid = test_info_from(start_idx);
        assert!(
            !infos_mid.iter().any(|s| s.contains("kind=bestmove_sent")),
            "bestmove should not be sent during ponder: {:?}",
            infos_mid
        );

        // Trigger ponderhit (convert in-place)
        handle_ponder_hit(&mut ctx).unwrap();

        // Simulate worker completion messages under new unified finalize model
        let eng = engine_core::engine::controller::Engine::new(
            engine_core::engine::controller::EngineType::Material,
        );
        tx.send(WorkerMessage::ReturnEngine {
            engine: eng,
            search_id: 1,
        })
        .unwrap();
        tx.send(WorkerMessage::Finished {
            from_guard: false,
            search_id: 1,
        })
        .unwrap();

        // Now bestmove should be sent within time
        let infos = pump_until_bestmove(&mut ctx, 7000, start_idx);
        assert!(
            infos.iter().any(|s| s.contains("kind=bestmove_sent")),
            "bestmove_sent not found after ponderhit. Infos: {:?}",
            infos
        );
    }

    #[test]
    fn test_hard_deadline_emits_from_committed() {
        std::env::set_var("USI_DRY_RUN", "1");

        // Engine and startpos
        let engine = Arc::new(Mutex::new(EngineAdapter::new()));
        {
            let mut adapter = engine.lock().unwrap();
            adapter.set_position(true, None, &[]).unwrap();
        }

        // Channels and flags
        let (tx, rx) = unbounded::<WorkerMessage>();
        let global_stop = Arc::new(AtomicBool::new(false));

        // Context fields
        let mut worker_handle = None;
        let mut search_state = SearchState::Searching; // simulate active search
        let mut search_id_counter = 0u64;
        let mut current_search_id = 1u64;
        let mut current_search_is_ponder = false;
        let mut current_session: Option<()> = None;
        let mut current_bestmove_emitter: Option<BestmoveEmitter> = Some(BestmoveEmitter::new(1));
        let mut current_finalized_flag: Option<Arc<AtomicBool>> = None;
        let mut current_stop_flag: Option<Arc<AtomicBool>> = None;
        let mut position_state: Option<crate::types::PositionState> = None;
        let program_start = std::time::Instant::now();
        let mut last_partial_result: Option<(String, u8, i32)> = None;
        let mut search_start_time: Option<std::time::Instant> = Some(std::time::Instant::now());
        let mut latest_nodes: u64 = 0;
        let mut soft_limit_ms_ctx: u64 = 0;
        let mut root_legal_moves: Option<Vec<String>> = None;
        let mut hard_deadline_taken = false;
        let mut pre_session_fallback: Option<String> = None;
        let mut pre_session_fallback_hash: Option<u64> = None;
        let mut last_bestmove_sent_at: Option<std::time::Instant> = None;
        let mut last_go_begin_at: Option<std::time::Instant> = None;
        let mut final_pv_injected = false;

        // Build a committed iteration with a legal move from startpos (e.g., 7g7f)
        let pv_head = engine_core::usi::parse_usi_move("7g7f").unwrap();
        let committed = CommittedIteration {
            depth: 5,
            seldepth: Some(7),
            score: 20,
            pv: vec![pv_head],
            node_type: NodeType::Exact,
            nodes: 10_000,
            elapsed: std::time::Duration::from_millis(50),
        };

        // Pending fields for unified finalize
        let mut pending_stop_info: Option<StopInfo> = None;
        let mut pending_returned_engine: Option<Engine> = None;
        let mut ctx = CommandContext {
            engine: &engine,
            stop_flag: &global_stop,
            worker_tx: &tx,
            worker_rx: &rx,
            worker_handle: &mut worker_handle,
            search_state: &mut search_state,
            search_id_counter: &mut search_id_counter,
            current_search_id: &mut current_search_id,
            current_search_is_ponder: &mut current_search_is_ponder,
            current_session: &mut current_session,
            current_committed: &mut Some(committed.clone()),
            current_bestmove_emitter: &mut current_bestmove_emitter,
            current_finalized_flag: &mut current_finalized_flag,
            current_stop_flag: &mut current_stop_flag,
            allow_null_move: false,
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
            last_bestmove_sent_at: &mut last_bestmove_sent_at,
            last_go_begin_at: &mut last_go_begin_at,
            final_pv_injected: &mut final_pv_injected,
            pending_stop_info: &mut pending_stop_info,
            pending_returned_engine: &mut pending_returned_engine,
        };

        let start_idx = test_info_len();
        // Inject a HardDeadlineFire message for the current search
        tx.send(WorkerMessage::HardDeadlineFire {
            search_id: 1,
            hard_ms: 1000,
        })
        .unwrap();

        let infos = pump_until_bestmove(&mut ctx, 3000, start_idx);
        assert!(
            infos.iter().any(|s| s.contains("kind=bestmove_sent")
                && s.contains("stop_reason=time_limit")
                && s.contains("hard_timeout=true")),
            "bestmove_sent with time_limit hard_timeout=true not found. Infos: {:?}",
            infos
        );
    }

    #[test]
    fn test_hard_deadline_emits_from_root_legal() {
        std::env::set_var("USI_DRY_RUN", "1");

        let engine = Arc::new(Mutex::new(EngineAdapter::new()));
        {
            let mut adapter = engine.lock().unwrap();
            adapter.set_position(true, None, &[]).unwrap();
        }

        let (tx, rx) = unbounded::<WorkerMessage>();
        let global_stop = Arc::new(AtomicBool::new(false));

        let mut worker_handle = None;
        let mut search_state = SearchState::Searching;
        let mut search_id_counter = 0u64;
        let mut current_search_id = 2u64;
        let mut current_search_is_ponder = false;
        let mut current_session: Option<()> = None;
        let mut current_bestmove_emitter: Option<BestmoveEmitter> = Some(BestmoveEmitter::new(2));
        let mut current_finalized_flag: Option<Arc<AtomicBool>> = None;
        let mut current_stop_flag: Option<Arc<AtomicBool>> = None;
        let mut position_state: Option<crate::types::PositionState> = None;
        let program_start = std::time::Instant::now();
        let mut last_partial_result: Option<(String, u8, i32)> = None;
        let mut search_start_time: Option<std::time::Instant> = Some(std::time::Instant::now());
        let mut latest_nodes: u64 = 0;
        let mut soft_limit_ms_ctx: u64 = 0;
        let mut root_legal_moves: Option<Vec<String>> = Some(vec!["7g7f".to_string()]);
        let mut hard_deadline_taken = false;
        let mut pre_session_fallback: Option<String> = None;
        let mut pre_session_fallback_hash: Option<u64> = None;
        let mut last_bestmove_sent_at: Option<std::time::Instant> = None;
        let mut last_go_begin_at: Option<std::time::Instant> = None;
        let mut final_pv_injected = false;

        // Pending fields for unified finalize
        let mut pending_stop_info: Option<StopInfo> = None;
        let mut pending_returned_engine: Option<Engine> = None;
        let mut ctx = CommandContext {
            engine: &engine,
            stop_flag: &global_stop,
            worker_tx: &tx,
            worker_rx: &rx,
            worker_handle: &mut worker_handle,
            search_state: &mut search_state,
            search_id_counter: &mut search_id_counter,
            current_search_id: &mut current_search_id,
            current_search_is_ponder: &mut current_search_is_ponder,
            current_session: &mut current_session,
            current_committed: &mut None,
            current_bestmove_emitter: &mut current_bestmove_emitter,
            current_finalized_flag: &mut current_finalized_flag,
            current_stop_flag: &mut current_stop_flag,
            allow_null_move: false,
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
            last_bestmove_sent_at: &mut last_bestmove_sent_at,
            last_go_begin_at: &mut last_go_begin_at,
            final_pv_injected: &mut final_pv_injected,
            pending_stop_info: &mut pending_stop_info,
            pending_returned_engine: &mut pending_returned_engine,
        };

        let start_idx = test_info_len();
        tx.send(WorkerMessage::HardDeadlineFire {
            search_id: 2,
            hard_ms: 1200,
        })
        .unwrap();
        let infos = pump_until_bestmove(&mut ctx, 3000, start_idx);
        assert!(
            infos.iter().any(|s| s.contains("kind=bestmove_sent")
                && s.contains("stop_reason=time_limit")
                && s.contains("hard_timeout=true")),
            "bestmove_sent (root_legal) not found. Infos: {:?}",
            infos
        );
    }

    #[test]
    fn test_hard_deadline_emits_emergency() {
        std::env::set_var("USI_DRY_RUN", "1");

        let engine = Arc::new(Mutex::new(EngineAdapter::new()));
        {
            let mut adapter = engine.lock().unwrap();
            adapter.set_position(true, None, &[]).unwrap();
        }

        let (tx, rx) = unbounded::<WorkerMessage>();
        let global_stop = Arc::new(AtomicBool::new(false));

        let mut worker_handle = None;
        let mut search_state = SearchState::Searching;
        let mut search_id_counter = 0u64;
        let mut current_search_id = 3u64;
        let mut current_search_is_ponder = false;
        let mut current_session: Option<()> = None;
        let mut current_bestmove_emitter: Option<BestmoveEmitter> = Some(BestmoveEmitter::new(3));
        let mut current_finalized_flag: Option<Arc<AtomicBool>> = None;
        let mut current_stop_flag: Option<Arc<AtomicBool>> = None;
        let mut position_state: Option<crate::types::PositionState> = None;
        let program_start = std::time::Instant::now();
        let mut last_partial_result: Option<(String, u8, i32)> = None;
        let mut search_start_time: Option<std::time::Instant> = Some(std::time::Instant::now());
        let mut latest_nodes: u64 = 0;
        let mut soft_limit_ms_ctx: u64 = 0;
        // Set a valid legal move to test hard deadline handling
        let mut root_legal_moves: Option<Vec<String>> = Some(vec!["7g7f".to_string()]); // common opening move
        let mut hard_deadline_taken = false;
        let mut pre_session_fallback: Option<String> = None;
        let mut pre_session_fallback_hash: Option<u64> = None;
        let mut last_bestmove_sent_at: Option<std::time::Instant> = None;
        let mut last_go_begin_at: Option<std::time::Instant> = None;
        let mut final_pv_injected = false;

        // Pending fields for unified finalize
        let mut pending_stop_info: Option<StopInfo> = None;
        let mut pending_returned_engine: Option<Engine> = None;
        let mut ctx = CommandContext {
            engine: &engine,
            stop_flag: &global_stop,
            worker_tx: &tx,
            worker_rx: &rx,
            worker_handle: &mut worker_handle,
            search_state: &mut search_state,
            search_id_counter: &mut search_id_counter,
            current_search_id: &mut current_search_id,
            current_search_is_ponder: &mut current_search_is_ponder,
            current_session: &mut current_session,
            current_committed: &mut None,
            current_bestmove_emitter: &mut current_bestmove_emitter,
            current_finalized_flag: &mut current_finalized_flag,
            current_stop_flag: &mut current_stop_flag,
            allow_null_move: false,
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
            last_bestmove_sent_at: &mut last_bestmove_sent_at,
            last_go_begin_at: &mut last_go_begin_at,
            final_pv_injected: &mut final_pv_injected,
            pending_stop_info: &mut pending_stop_info,
            pending_returned_engine: &mut pending_returned_engine,
        };

        let start_idx = test_info_len();
        tx.send(WorkerMessage::HardDeadlineFire {
            search_id: 3,
            hard_ms: 800,
        })
        .unwrap();
        let infos = pump_until_bestmove(&mut ctx, 3000, start_idx);
        assert!(
            infos.iter().any(|s| s.contains("kind=bestmove_sent")
                && s.contains("stop_reason=time_limit")
                && s.contains("hard_timeout=true")),
            "bestmove_sent (emergency) not found. Infos: {:?}",
            infos
        );
    }
}
