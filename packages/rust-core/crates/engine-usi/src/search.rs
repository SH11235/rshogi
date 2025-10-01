use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use engine_core::search::limits::{FallbackDeadlines, SearchLimits, SearchLimitsBuilder};
use engine_core::shogi::Color;
use engine_core::time_management::{TimeControl, TimeParameters, TimeParametersBuilder};
use engine_core::usi::{create_position, move_to_usi};
use log::info;

use crate::finalize::{finalize_and_send, fmt_hash};
use crate::io::info_string;
use crate::state::{EngineState, GoParams, UsiOptions};
use crate::usi_adapter;
use crate::util::emit_bestmove;

pub fn parse_position(cmd: &str, state: &mut EngineState) -> Result<()> {
    let mut tokens = cmd.split_whitespace().skip(1).peekable();
    let mut have_pos = false;
    state.pos_from_startpos = false;
    state.pos_sfen = None;
    state.pos_moves.clear();

    while let Some(tok) = tokens.peek().cloned() {
        match tok {
            "startpos" => {
                let _ = tokens.next();
                have_pos = true;
                state.pos_from_startpos = true;
                state.pos_sfen = None;
            }
            "sfen" => {
                let _ = tokens.next();
                let mut sfen_parts: Vec<String> = Vec::new();
                while let Some(t) = tokens.peek() {
                    if *t == "moves" {
                        break;
                    }
                    sfen_parts.push(tokens.next().unwrap().to_string());
                }
                let sfen = sfen_parts.join(" ");
                if sfen.trim().is_empty() {
                    info_string("invalid_sfen_empty");
                    return Err(anyhow!("Empty SFEN in position command"));
                }
                have_pos = true;
                state.pos_from_startpos = false;
                state.pos_sfen = Some(sfen);
            }
            "moves" => {
                let _ = tokens.next();
                for mstr in tokens.by_ref() {
                    state.pos_moves.push(mstr.to_string());
                }
            }
            _ => {
                let _ = tokens.next();
            }
        }
    }

    if !have_pos {
        state.pos_from_startpos = true;
        state.pos_sfen = None;
    }

    let pos =
        create_position(state.pos_from_startpos, state.pos_sfen.as_deref(), &state.pos_moves)?;
    state.position = pos;
    Ok(())
}

pub fn parse_go(cmd: &str) -> GoParams {
    let mut gp = GoParams::default();
    let mut it = cmd.split_whitespace().skip(1);
    while let Some(tok) = it.next() {
        match tok {
            "depth" => gp.depth = it.next().and_then(|v| v.parse().ok()),
            "nodes" => gp.nodes = it.next().and_then(|v| v.parse().ok()),
            "movetime" => gp.movetime = it.next().and_then(|v| v.parse().ok()),
            "infinite" => gp.infinite = true,
            "ponder" => gp.ponder = true,
            "btime" => gp.btime = it.next().and_then(|v| v.parse().ok()),
            "wtime" => gp.wtime = it.next().and_then(|v| v.parse().ok()),
            "binc" => gp.binc = it.next().and_then(|v| v.parse().ok()),
            "winc" => gp.winc = it.next().and_then(|v| v.parse().ok()),
            "byoyomi" => gp.byoyomi = it.next().and_then(|v| v.parse().ok()),
            // periods: 秒読みの残り回数（将来のGUI/スクリプト互換のため事前対応）
            "periods" => gp.periods = it.next().and_then(|v| v.parse().ok()),
            "rtime" => {
                let _ = it.next();
            }
            "movestogo" => gp.moves_to_go = it.next().and_then(|v| v.parse().ok()),
            "mate" => {
                let _ = it.next();
            }
            _ => {}
        }
    }
    gp
}

pub fn limits_from_go(
    gp: &GoParams,
    side: Color,
    opts: &UsiOptions,
    ponder_flag: Option<Arc<AtomicBool>>,
    stop_flag: Arc<AtomicBool>,
) -> SearchLimits {
    use engine_core::search::types::{InfoCallback, InfoStringCallback};
    let builder = TimeParametersBuilder::new()
        .overhead_ms(opts.overhead_ms)
        .unwrap()
        .network_delay_ms(opts.network_delay_ms)
        .unwrap()
        .network_delay2_ms(opts.network_delay2_ms)
        .unwrap()
        .byoyomi_safety_ms(opts.byoyomi_safety_ms)
        .unwrap()
        .pv_stability_base(opts.pv_stability_base)
        .unwrap()
        .pv_stability_slope(opts.pv_stability_slope)
        .unwrap();
    let mut tp: TimeParameters = builder.build();
    tp.min_think_ms = opts.min_think_ms;
    tp.byoyomi_soft_ratio = (opts.byoyomi_early_finish_ratio as f64 / 100.0).clamp(0.5, 0.95);
    tp.slow_mover_pct = opts.slow_mover_pct;
    tp.max_time_ratio = (opts.max_time_ratio_pct as f64 / 100.0).max(1.0);
    tp.move_horizon_trigger_ms = opts.move_horizon_trigger_ms;
    tp.move_horizon_min_moves = opts.move_horizon_min_moves;

    // 純秒読み（main=0）時の締切リードを worst-case ネット遅延に加算して、
    // エンジンの最終化をGUI締切（byoyomi）より前倒しにする。
    // goコマンドのパラメータから純秒読みを推定: btime=wtime=0 かつ byoyomi>0
    let pure_byoyomi =
        gp.byoyomi.unwrap_or(0) > 0 && gp.btime.unwrap_or(0) == 0 && gp.wtime.unwrap_or(0) == 0;
    if pure_byoyomi && opts.byoyomi_deadline_lead_ms > 0 {
        // 上限 2000ms（オプション側でも clamp 済み）。
        let before = tp.network_delay2_ms;
        tp.network_delay2_ms = tp.network_delay2_ms.saturating_add(opts.byoyomi_deadline_lead_ms);
        // デバッグ容易性のためのログ
        info_string(format!(
            "deadline_lead_applied=1 before={} add={} after={}",
            before, opts.byoyomi_deadline_lead_ms, tp.network_delay2_ms
        ));
    }

    let mut builder = SearchLimitsBuilder::default();

    if let Some(d) = gp.depth {
        builder = builder.depth(d.min(127) as u8);
    }
    if let Some(n) = gp.nodes {
        builder = builder.nodes(n);
    }

    let mut builder = if gp.infinite {
        builder.infinite()
    } else if let Some(ms) = gp.movetime {
        builder.fixed_time_ms(ms)
    } else if let Some(byo) = gp.byoyomi {
        let main_time = match side {
            Color::Black => gp.btime.unwrap_or_default(),
            Color::White => gp.wtime.unwrap_or_default(),
        };
        let periods = gp.periods.unwrap_or(opts.byoyomi_periods).clamp(1, 10);
        builder.byoyomi(main_time, byo, periods)
    } else if let (Some(b), Some(w)) = (gp.btime, gp.wtime) {
        let white_inc = gp.winc.unwrap_or_default();
        let black_inc = gp.binc.unwrap_or_default();
        let inc = if side == Color::White {
            white_inc
        } else {
            black_inc
        };
        let mut bldr = builder.fischer(w, b, inc);
        if let Some(mtg) = gp.moves_to_go {
            bldr = bldr.moves_to_go(mtg);
        }
        bldr
    } else {
        builder.infinite()
    };

    builder = builder.time_parameters(tp);
    builder = builder.stop_flag(stop_flag);
    // 重要: Ponder フラグは "go ponder" のときだけ有効化する。
    // USI の Ponder オプション（ON/OFF）に関わらず、通常探索では Ponder に包まない。
    if gp.ponder {
        if let Some(flag) = ponder_flag {
            builder = builder.ponder_hit_flag(flag).ponder_with_inner();
            crate::io::info_string("ponder_wrapper=1");
        } else {
            // go ponder だがフラグがないケース（オプションOFFなど）でも挙動明示
            crate::io::info_string("ponder_wrapper=0 (flag_missing)");
        }
    } else {
        crate::io::info_string("ponder_wrapper=0");
    }

    // Set up info callback for search progress reporting
    let multipv = opts.multipv.max(1);
    let info_callback: InfoCallback =
        Arc::new(move |depth, score, nodes, elapsed, pv, node_type| {
            // Emit a unified PV info line via the adapter. We pass multipv>1 to
            // decide whether to include "multipv 1" in the output for compatibility.
            usi_adapter::emit_pv_line(depth, score, nodes, elapsed, pv, node_type, multipv > 1);
        });

    // Set up info string callback for textual diagnostics
    let info_string_callback: InfoStringCallback = Arc::new(|msg: &str| {
        println!("info string {msg}");
    });

    builder
        .multipv(opts.multipv)
        .enable_fail_safe(opts.fail_safe_guard)
        .info_callback(info_callback)
        .info_string_callback(info_string_callback)
        .start_time(Instant::now())
        .build()
}

pub fn handle_go(cmd: &str, state: &mut EngineState) -> Result<()> {
    if state.searching {
        info!("Ignoring go while searching");
        return Ok(());
    }

    // Create a new stop_flag for each search session to avoid race conditions
    // with concurrent searches (previous session may still be running)
    let stop_flag = Arc::new(AtomicBool::new(false));
    info_string(format!("stop_flag_create addr={:p}", Arc::as_ptr(&stop_flag)));
    let ponder_flag = if state.opts.ponder {
        Some(Arc::new(AtomicBool::new(false)))
    } else {
        None
    };

    let mut gp = if cmd == "go" {
        GoParams::default()
    } else {
        parse_go(cmd)
    };

    if gp.ponder && !state.opts.ponder {
        info_string("ponder_disabled_guard=1");
        gp.ponder = false;
    }

    state.last_go_params = Some(gp.clone());

    // With start_search(), waiting is minimal since Engine lock releases immediately
    let waited_before_go_ms = 0_u64;
    let mut search_position = state.position.clone();
    let current_is_stochastic_ponder = gp.ponder && state.opts.stochastic_ponder;
    if current_is_stochastic_ponder {
        if !state.pos_moves.is_empty() {
            let prev_moves = &state.pos_moves[..state.pos_moves.len().saturating_sub(1)];
            if let Ok(prev) = engine_core::usi::create_position(
                state.pos_from_startpos,
                state.pos_sfen.as_deref(),
                prev_moves,
            ) {
                search_position = prev;
                info!("Stochastic Ponder: using previous position for pondering");
            } else {
                info!(
                    "Stochastic Ponder: failed to build previous position; using current position"
                );
            }
        } else {
            info!("Stochastic Ponder: no previous move; using current position");
        }
    }

    if !gp.ponder {
        let mg = engine_core::movegen::MoveGenerator::new();
        if let Ok(list) = mg.generate_all(&state.position) {
            let slice = list.as_slice();
            if slice.is_empty() {
                info_string(format!(
                    "early_return reason=no_legal_moves ply={} hash={}",
                    state.position.ply,
                    fmt_hash(state.position.zobrist_hash())
                ));
                emit_bestmove("resign", None);
                state.bestmove_emitted = true;
                return Ok(());
            } else if slice.len() == 1 {
                let mv_usi = move_to_usi(&slice[0]);
                info_string(format!(
                    "early_return reason=only_one_legal_move ply={} hash={} move={}",
                    state.position.ply,
                    fmt_hash(state.position.zobrist_hash()),
                    mv_usi
                ));
                emit_bestmove(&mv_usi, None);
                state.bestmove_emitted = true;
                return Ok(());
            }
        }
    }

    let mut limits = limits_from_go(
        &gp,
        search_position.side_to_move,
        &state.opts,
        ponder_flag.clone(),
        Arc::clone(&stop_flag),
    );

    if waited_before_go_ms > 0 {
        if let Some(ref mut params) = limits.time_parameters {
            // Phase 1: Accurate wait attribution for pure byoyomi (up to 2000ms)
            // Reflects actual startup delay in time budget to prevent TimeManager over-optimization
            let is_pure_byoyomi = gp.byoyomi.unwrap_or(0) > 0
                && gp.btime.unwrap_or(0) == 0
                && gp.wtime.unwrap_or(0) == 0;

            let clamped_wait = if is_pure_byoyomi {
                waited_before_go_ms.min(2000)
            } else {
                waited_before_go_ms
            };

            params.network_delay2_ms = params.network_delay2_ms.saturating_add(clamped_wait);

            if clamped_wait < waited_before_go_ms {
                info_string(format!(
                    "wait_time_clamped waited={} clamped={} pure_byo={}",
                    waited_before_go_ms, clamped_wait, is_pure_byoyomi as u8
                ));
            }
        }
    }

    let mut tc_for_stop = limits.time_control.clone();
    if let TimeControl::Ponder(inner) = tc_for_stop {
        tc_for_stop = *inner;
    }
    state.current_time_control = Some(tc_for_stop.clone());

    // Emit estimated time budget at search start (diagnostics aid)
    // Uses the same allocation routine as engine-core TimeManager.
    {
        use engine_core::time_management::{
            calculate_time_allocation, detect_game_phase_for_time, TimeParameters,
        };
        let params: TimeParameters = limits.time_parameters.unwrap_or_default();
        let phase = detect_game_phase_for_time(&search_position, search_position.ply as u32);
        let (soft, hard) = calculate_time_allocation(
            &tc_for_stop,
            search_position.side_to_move,
            search_position.ply as u32,
            gp.moves_to_go,
            phase,
            &params,
        );
        info_string(format!(
            "time_budget waited_ms={} soft_ms={} hard_ms={} tc={:?}",
            waited_before_go_ms, soft, hard, tc_for_stop
        ));

        if hard != u64::MAX && hard > 0 && !gp.ponder {
            let base = Instant::now();
            let hard_deadline = base + Duration::from_millis(hard);
            let soft_deadline = if soft != u64::MAX && soft > 0 {
                Some(base + Duration::from_millis(soft))
            } else {
                None
            };
            limits.fallback_deadlines = Some(FallbackDeadlines {
                soft_deadline,
                hard_deadline,
                soft_limit_ms: if soft != u64::MAX { soft } else { 0 },
                hard_limit_ms: hard,
            });

            // Record deadlines into EngineState for USI-side OOB finalize enforcement
            state.deadline_hard = Some(hard_deadline);
            // Conservative: near-hard is optional; if desired, compute as (hard - lead)
            let lead = state.opts.byoyomi_deadline_lead_ms;
            state.deadline_near = if lead > 0 {
                hard_deadline.checked_sub(Duration::from_millis(lead))
            } else {
                None
            };
        } else {
            // No meaningful time budget → clear deadlines
            state.deadline_hard = None;
            state.deadline_near = None;
        }
    }

    // Use start_search() - non-blocking, Engine lock released immediately
    state.current_search_id = state.current_search_id.wrapping_add(1);
    let session = {
        let mut engine_guard = state.engine.lock().unwrap();
        engine_guard.start_search(search_position.clone(), limits)
    }; // Engine lock released here immediately

    state.searching = true;
    state.stop_flag = Some(Arc::clone(&stop_flag));
    state.ponder_hit_flag = ponder_flag;
    let session_id = session.session_id();
    state.search_session = Some(session);
    state.current_is_stochastic_ponder = current_is_stochastic_ponder;
    state.current_is_ponder = gp.ponder;
    state.current_root_hash = Some(search_position.zobrist_hash());
    state.bestmove_emitted = false;
    info_string(format!(
        "search_started sid={} root={} gui={} ponder={} stoch={}",
        session_id,
        fmt_hash(search_position.zobrist_hash()),
        fmt_hash(state.position.zobrist_hash()),
        gp.ponder,
        state.current_is_stochastic_ponder
    ));

    // Enhanced diagnostics for time loss investigation
    let threads = state.opts.threads;
    info_string(format!("search_diagnostics sid={} threads={}", session_id, threads));

    Ok(())
}

pub fn poll_search_completion(state: &mut EngineState) {
    if !state.searching {
        return;
    }

    // Use SearchSession::try_poll() to detect thread disconnection
    if let Some(session) = &state.search_session {
        use engine_core::engine::TryResult;
        match session.try_poll() {
            TryResult::Ok(result) => {
                // Search completed, clean up state
                state.searching = false;
                // Clear stop_flag - each session gets a fresh flag to avoid race conditions
                state.stop_flag = None;
                state.ponder_hit_flag = None;
                state.search_session = None;
                state.notify_idle();

                let was_ponder = state.current_is_ponder;
                state.current_is_ponder = false;

                if state.stoch_suppress_result {
                    state.stoch_suppress_result = false;
                    if state.pending_research_after_ponderhit {
                        if let Some(mut gp2) = state.last_go_params.clone() {
                            gp2.ponder = false;
                            let stop_flag = Arc::new(AtomicBool::new(false));
                            let ponder_flag: Option<Arc<AtomicBool>> = None;
                            let limits = limits_from_go(
                                &gp2,
                                state.position.side_to_move,
                                &state.opts,
                                ponder_flag.clone(),
                                Arc::clone(&stop_flag),
                            );
                            let mut tc_for_stop = limits.time_control.clone();
                            if let TimeControl::Ponder(inner) = tc_for_stop {
                                tc_for_stop = *inner;
                            }

                            // Phase 2: Use start_search() for re-search after ponder hit
                            state.current_search_id = state.current_search_id.wrapping_add(1);
                            let session = {
                                let mut engine_guard = state.engine.lock().unwrap();
                                engine_guard.start_search(state.position.clone(), limits)
                            };

                            state.searching = true;
                            state.stop_flag = Some(stop_flag);
                            state.ponder_hit_flag = None;
                            state.search_session = Some(session);
                            state.current_is_stochastic_ponder = false;
                            state.current_time_control = Some(tc_for_stop);
                            state.current_root_hash = Some(state.position.zobrist_hash());
                            state.bestmove_emitted = false;
                            state.pending_research_after_ponderhit = false;
                        } else {
                            state.pending_research_after_ponderhit = false;
                        }
                    }
                } else if was_ponder {
                    // do nothing per USI specification
                } else {
                    let stale = state
                        .current_root_hash
                        .map(|h| h != state.position.zobrist_hash())
                        .unwrap_or(false);
                    finalize_and_send(state, "finalize", Some(&result), stale);
                    state.current_time_control = None;
                    state.current_root_hash = None;
                    state.notify_idle();
                }
            }
            TryResult::Pending => {
                // Still searching (no result yet)
            }
            TryResult::Disconnected => {
                // Search thread disconnected without sending result (panic or early exit)
                // Clean up state and emit fallback bestmove
                use engine_core::usi::move_to_usi;
                use log::error;

                error!(
                    "Search thread disconnected unexpectedly for session {}",
                    session.session_id()
                );
                info_string(format!(
                    "search_thread_disconnected session_id={} root_hash={}",
                    session.session_id(),
                    state.current_root_hash.map(fmt_hash).unwrap_or_else(|| "none".to_string())
                ));

                state.searching = false;
                // Clear stop_flag - each session gets a fresh flag to avoid race conditions
                state.stop_flag = None;
                state.ponder_hit_flag = None;
                state.search_session = None;
                state.current_time_control = None;
                state.current_root_hash = None;

                // Fallback: try to get a safe bestmove from Engine (TT or legal moves)
                let bestmove = {
                    let engine = state.engine.lock().unwrap();
                    let final_best = engine.choose_final_bestmove(&state.position, None);
                    final_best.best_move.map(|m| move_to_usi(&m))
                };

                match bestmove {
                    Some(mv) => {
                        info_string(format!("fallback_bestmove move={mv} source=tt_or_legal"));
                        emit_bestmove(&mv, None);
                    }
                    None => {
                        info_string("fallback_bestmove move=resign source=no_legal_moves");
                        emit_bestmove("resign", None);
                    }
                }

                state.bestmove_emitted = true;
                state.notify_idle();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use engine_core::search::limits::SearchLimits;
    use engine_core::time_management::TimeParameters;

    #[test]
    fn time_parameters_option_remains_after_unwrap() {
        let params = TimeParameters {
            network_delay2_ms: 1234,
            ..TimeParameters::default()
        };

        let limits = SearchLimits {
            time_parameters: Some(params),
            ..SearchLimits::default()
        };

        let extracted = limits.time_parameters.unwrap_or_default();
        assert_eq!(extracted.network_delay2_ms, 1234);
        assert!(limits.time_parameters.is_some(), "time_parameters was unexpectedly moved out");
        assert_eq!(limits.time_parameters.unwrap_or_default().network_delay2_ms, 1234);
    }
}
