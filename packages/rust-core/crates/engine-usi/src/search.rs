use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use engine_core::search::limits::{FallbackDeadlines, SearchLimits, SearchLimitsBuilder};
use engine_core::search::types::{InfoCallback, InfoStringCallback};
use engine_core::shogi::{Color, Position};
use engine_core::time_management::{TimeControl, TimeParameters, TimeParametersBuilder};
use engine_core::usi::{append_usi_score_and_bound, create_position, move_to_usi};
use log::info;

use crate::finalize::{finalize_and_send, fmt_hash};
use crate::io::info_string;
use crate::state::{EngineState, GoParams, IdleStateSnapshot, UsiOptions};
use crate::util::{emit_bestmove, score_view_with_clamp};

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

#[derive(Debug)]
struct IdleWaitOutcome {
    waited: Duration,
    timed_out: bool,
    final_status: IdleStateSnapshot,
}

fn compute_idle_wait_budget(gp: &GoParams, opts: &UsiOptions) -> Duration {
    let stop_wait = Duration::from_millis(opts.stop_wait_ms.min(2_000));
    let byoyomi = gp.byoyomi.unwrap_or(0);
    if byoyomi == 0 {
        return stop_wait.max(Duration::from_millis(50)).min(Duration::from_millis(1_000));
    }
    let byoyomi_quarter = Duration::from_millis((byoyomi / 4).min(1_000));
    let cap = Duration::from_millis(1_000);
    stop_wait
        .checked_add(Duration::from_millis(100))
        .unwrap_or(cap)
        .max(Duration::from_millis(50))
        .min(byoyomi_quarter.max(Duration::from_millis(100)))
        .min(cap)
}

fn wait_for_engine_idle(state: &EngineState, max_wait: Duration) -> IdleWaitOutcome {
    let check_interval = Duration::from_millis(50);
    let start = Instant::now();
    loop {
        let status = state.idle_status();
        if status.is_idle() {
            return IdleWaitOutcome {
                waited: start.elapsed(),
                timed_out: false,
                final_status: status,
            };
        }
        let elapsed = start.elapsed();
        if elapsed >= max_wait {
            return IdleWaitOutcome {
                waited: elapsed,
                timed_out: true,
                final_status: status,
            };
        }
        let remaining = max_wait.saturating_sub(elapsed);
        let wait_dur = if remaining < check_interval {
            remaining
        } else {
            check_interval
        };
        if wait_dur.is_zero() {
            continue;
        }
        state.idle_sync.wait_timeout(wait_dur);
    }
}

pub fn limits_from_go(
    gp: &GoParams,
    side: Color,
    opts: &UsiOptions,
    ponder_flag: Option<Arc<AtomicBool>>,
    stop_flag: Arc<AtomicBool>,
) -> SearchLimits {
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
    builder.multipv(opts.multipv).enable_fail_safe(opts.fail_safe_guard).build()
}

pub fn run_search_thread(
    engine: Arc<std::sync::Mutex<engine_core::engine::controller::Engine>>,
    mut position: Position,
    mut limits: SearchLimits,
    info_enabled: bool,
    tx: mpsc::Sender<(u64, engine_core::search::SearchResult)>,
    search_id: u64,
) {
    if info_enabled {
        use std::sync::Arc as StdArc;

        let multipv = limits.multipv;
        let callback: InfoCallback =
            StdArc::new(move |depth, score, nodes, elapsed, pv, node_type| {
                let mut line = String::from("info");
                line.push_str(&format!(" depth {}", depth));
                if multipv > 1 {
                    line.push_str(" multipv 1");
                }
                line.push_str(&format!(" time {}", elapsed.as_millis()));
                line.push_str(&format!(" nodes {}", nodes));
                if elapsed.as_millis() > 0 {
                    let nps = (nodes as u128).saturating_mul(1000) / elapsed.as_millis();
                    line.push_str(&format!(" nps {}", nps));
                }

                let view = score_view_with_clamp(score);
                append_usi_score_and_bound(&mut line, view, node_type);

                if !pv.is_empty() {
                    line.push_str(" pv");
                    for m in pv.iter() {
                        line.push(' ');
                        line.push_str(&move_to_usi(m));
                    }
                }

                crate::io::usi_println(&line);
            });

        limits.info_callback = Some(callback);
    }

    let info_string_cb: InfoStringCallback = Arc::new(|msg: &str| crate::io::info_string(msg));
    limits.info_string_callback = Some(info_string_cb);

    let start_ts = Instant::now();
    let mut result = {
        let mut eng = engine.lock().unwrap();
        eng.search(&mut position, limits)
    };
    if result.stats.elapsed.as_millis() == 0 {
        result.stats.elapsed = start_ts.elapsed();
    }
    let _ = tx.send((search_id, result));
}

pub fn handle_go(cmd: &str, state: &mut EngineState) -> Result<()> {
    if state.searching {
        info!("Ignoring go while searching");
        return Ok(());
    }

    let stop_flag = Arc::new(AtomicBool::new(false));
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

    let wait_budget = compute_idle_wait_budget(&gp, &state.opts);
    let idle_outcome = wait_for_engine_idle(state, wait_budget);
    let waited_before_go_ms = idle_outcome.waited.as_millis() as u64;
    if idle_outcome.timed_out {
        let status = idle_outcome.final_status;
        info_string(format!(
            "go_wait_for_idle_timeout=1 waited_ms={} worker_active={} reaper_pending={} pending_work={} active_workers={}",
            waited_before_go_ms,
            status.worker_active as u8,
            status.reaper_pending,
            status.pending_work_items,
            status.active_workers
        ));
        state.stop_bridge.force_clear();
        state.notify_idle();
    } else if waited_before_go_ms > 0 {
        let status = idle_outcome.final_status;
        info_string(format!(
            "go_wait_for_idle waited_ms={} worker_active={} reaper_pending={} pending_work={} active_workers={}",
            waited_before_go_ms,
            status.worker_active as u8,
            status.reaper_pending,
            status.pending_work_items,
            status.active_workers
        ));
    }

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
                emit_bestmove("resign", None);
                state.bestmove_emitted = true;
                return Ok(());
            } else if slice.len() == 1 {
                let mv_usi = move_to_usi(&slice[0]);
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
            params.network_delay2_ms = params.network_delay2_ms.saturating_add(waited_before_go_ms);
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

        if hard != u64::MAX && hard > 0 {
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
        }
    }

    let (tx, rx) = mpsc::channel();
    let engine = Arc::clone(&state.engine);
    let pos = search_position.clone();
    let info_enabled = true;
    state.current_search_id = state.current_search_id.wrapping_add(1);
    let sid = state.current_search_id;
    let handle =
        thread::spawn(move || run_search_thread(engine, pos, limits, info_enabled, tx, sid));

    state.searching = true;
    state.stop_flag = Some(Arc::clone(&stop_flag));
    state.ponder_hit_flag = ponder_flag;
    state.worker = Some(handle);
    state.result_rx = Some(rx);
    state.current_is_stochastic_ponder = current_is_stochastic_ponder;
    state.current_is_ponder = gp.ponder;
    state.current_root_hash = Some(search_position.zobrist_hash());
    state.bestmove_emitted = false;
    info_string(format!(
        "search_started root={} gui={} ponder={} stoch={}",
        fmt_hash(search_position.zobrist_hash()),
        fmt_hash(state.position.zobrist_hash()),
        gp.ponder,
        state.current_is_stochastic_ponder
    ));
    Ok(())
}

pub fn poll_search_completion(state: &mut EngineState) {
    if !state.searching {
        return;
    }
    if let Some(rx) = &state.result_rx {
        match rx.try_recv() {
            Ok((sid, result)) => {
                if sid != state.current_search_id {
                    info_string(format!(
                        "ignore_result stale_sid={} current_sid={}",
                        sid, state.current_search_id
                    ));
                    return;
                }
                if let Some(h) = state.worker.take() {
                    let _ = h.join();
                    state.notify_idle();
                }
                state.searching = false;
                state.stop_flag = None;
                state.ponder_hit_flag = None;
                state.result_rx = None;

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
                            let (tx2, rx2) = mpsc::channel();
                            let engine = Arc::clone(&state.engine);
                            let pos2 = state.position.clone();
                            let info_enabled = true;
                            state.current_search_id = state.current_search_id.wrapping_add(1);
                            let sid2 = state.current_search_id;
                            let handle = thread::spawn(move || {
                                run_search_thread(engine, pos2, limits, info_enabled, tx2, sid2)
                            });
                            state.searching = true;
                            state.stop_flag = Some(stop_flag);
                            state.ponder_hit_flag = None;
                            state.worker = Some(handle);
                            state.result_rx = Some(rx2);
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
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                if let Some(h) = state.worker.take() {
                    let _ = h.join();
                    state.notify_idle();
                }
                state.searching = false;
                state.stop_flag = None;
                state.ponder_hit_flag = None;
                state.result_rx = None;
                state.current_time_control = None;
                emit_bestmove("resign", None);
                state.notify_idle();
            }
        }
    }
}
