use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use engine_core::search::api::{InfoEvent, InfoEventCallback};
use engine_core::search::limits::{FallbackDeadlines, SearchLimits, SearchLimitsBuilder};
use engine_core::search::parallel::FinalizeReason;
use engine_core::search::types::InfoStringCallback;
use engine_core::shogi::Color;
use engine_core::time_management::{TimeControl, TimeParameters, TimeParametersBuilder};
use engine_core::usi::{create_position, move_to_usi};
use log::info;

use crate::finalize::{emit_bestmove_once, finalize_and_send, fmt_hash};
use crate::io::info_string;
use crate::oob::poll_oob_finalize;
use crate::state::{EngineState, GoParams, UsiOptions};
use crate::usi_adapter;

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
                gp.rtime = it.next().and_then(|v| v.parse().ok());
            }
            "movestogo" => gp.moves_to_go = it.next().and_then(|v| v.parse().ok()),
            // USI: go mate [<limit_ms>|infinite]
            "mate" => {
                gp.mate_mode = true;
                if let Some(next) = it.next() {
                    if next.eq_ignore_ascii_case("infinite") {
                        gp.mate_limit_ms = None;
                    } else if let Ok(v) = next.parse::<u64>() {
                        gp.mate_limit_ms = Some(v);
                    } else {
                        // 仕様上は数値 or infinite想定。異常値は infinite とみなす。
                        gp.mate_limit_ms = None;
                    }
                } else {
                    // 引数省略は infinite とみなす実装が一般的
                    gp.mate_limit_ms = None;
                }
            }
            _ => {}
        }
    }
    gp
}

fn handle_go_mate(_cmd: &str, state: &mut EngineState, _gp: &GoParams) -> Result<()> {
    // 暫定版: 即時判定のみ（探索なし）。bestmove は絶対に出さない。
    // 1) 王手判定 + 合法手0 → checkmate
    // 2) それ以外 → checkmate nomate
    let in_check = state.position.is_in_check();
    let mg = engine_core::movegen::MoveGenerator::new();
    let legal = mg.generate_all(&state.position).unwrap_or_default();

    // ログでモードを明示
    crate::io::info_string(format!(
        "mate_mode=1 in_check={} legal_count={}",
        in_check as u8,
        legal.len()
    ));
    if in_check && legal.is_empty() {
        crate::io::usi_println("checkmate");
    } else {
        // 将来: gp.mate_limit_ms を用いた打ち切り応答や探索結果に応じて切替
        crate::io::usi_println("checkmate nomate");
    }
    // 検討経路でもアイドル通知は出す（stop連携用の内部状態を早めに戻す）
    state.notify_idle();
    Ok(())
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
    let byoyomi_total = gp.byoyomi.unwrap_or(0);
    let b_main = gp.btime.unwrap_or(0);
    let w_main = gp.wtime.unwrap_or(0);
    let side_main_zero = match side {
        Color::Black => b_main == 0,
        Color::White => w_main == 0,
    };
    let pure_byoyomi = byoyomi_total > 0 && ((b_main == 0 && w_main == 0) || side_main_zero);
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

    if let Some(rtime_ms) = gp.rtime {
        builder = builder.random_time_ms(rtime_ms);
    }

    builder = builder.time_parameters(tp);
    builder = builder.stop_flag(Arc::clone(&stop_flag));
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
    let stop_for_info = Arc::clone(&stop_flag);
    let info_callback: InfoEventCallback = Arc::new(move |event| {
        if stop_for_info.load(Ordering::Relaxed) {
            return;
        }
        if let InfoEvent::PV { line } = event {
            // Emit a unified PV info line via the adapter. We pass multipv>1 to
            // decide whether to include a multipv tag in the output for compatibility.
            usi_adapter::emit_pv_line(line.as_ref(), multipv > 1);
        }
    });

    // Set up info string callback for textual diagnostics
    let stop_for_info_str = Arc::clone(&stop_flag);
    let info_string_callback: InfoStringCallback = Arc::new(move |msg: &str| {
        if stop_for_info_str.load(Ordering::Relaxed) {
            return;
        }
        println!("info string {msg}");
    });

    builder
        .multipv(opts.multipv)
        .enable_fail_safe(opts.fail_safe_guard)
        .info_callback(info_callback)
        .info_string_callback(info_string_callback)
        .build()
}

pub fn handle_go(cmd: &str, state: &mut EngineState) -> Result<()> {
    // 入口診断ログ（go発行直後にプロセスが落ちるケースの切り分け用）
    crate::io::info_string(format!("go_enter cmd={}", cmd));
    // 新しい go を受理する前に、前回探索から残っている OOB finalize 要求を掃除しておく。
    // SessionStart が届く前の Finalize を握りつぶすことで stale=1 ログを抑止する。
    poll_oob_finalize(state);

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

    // go mate は通常探索から完全分岐（bestmove を出さない）。
    if gp.mate_mode {
        return handle_go_mate(cmd, state, &gp);
    }

    state.last_go_params = Some(gp.clone());

    // Threads連動の自動既定をここでも適用（手動 setoption は尊重）。
    // 検索直前の適用により、`isready` 後の setoption 変更も反映される。
    crate::options::maybe_apply_thread_based_defaults(state);
    crate::options::apply_options_to_engine(state);
    crate::options::log_effective_profile(state);

    // 新しい go セッションに入る前に bestmove の送信状態をリセットしておく。
    // 早期リターン経路（合法手 0/1 件）では search_session を作成せずに
    // emit_bestmove_once() を用いるため、前回探索のフラグが残っていると
    // bestmove が送信されない退行が起きる。
    state.bestmove_emitted = false;
    // Clear pending_ponder_result to avoid stale buffer from previous session
    state.pending_ponder_result = None;

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
                let _ = emit_bestmove_once(state, String::from("resign"), None);
                state.notify_idle();
                return Ok(());
            } else if slice.len() == 1 {
                let mv_usi = move_to_usi(&slice[0]);
                info_string(format!(
                    "early_return reason=only_one_legal_move ply={} hash={} move={}",
                    state.position.ply,
                    fmt_hash(state.position.zobrist_hash()),
                    mv_usi
                ));
                let _ = emit_bestmove_once(state, mv_usi, None);
                state.notify_idle();
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
        info_string(format!("time_budget soft_ms={} hard_ms={} tc={:?}", soft, hard, tc_for_stop));

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
            let lead = if state.opts.byoyomi_deadline_lead_ms > 0 {
                state.opts.byoyomi_deadline_lead_ms
            } else if matches!(tc_for_stop, TimeControl::Byoyomi { .. }) {
                200 // fallback lead for pure byoyomi to match YaneuraOu behavior
            } else {
                0
            };
            state.deadline_near = if lead > 0 {
                hard_deadline.checked_sub(Duration::from_millis(lead))
            } else {
                None
            };
            state.deadline_near_notified = false;
        } else {
            // No meaningful time budget → clear deadlines
            state.deadline_hard = None;
            state.deadline_near = None;
            state.deadline_near_notified = false;
        }
    }

    // Use start_search() - non-blocking, Engine lock released immediately
    state.current_search_id = state.current_search_id.wrapping_add(1);
    let session = {
        let mut engine_guard = state.lock_engine();
        engine_guard.start_search(search_position.clone(), limits)
    }; // Engine lock released here immediately

    state.searching = true;
    state.stop_flag = Some(Arc::clone(&stop_flag));
    state.ponder_hit_flag = ponder_flag;
    let session_id = session.session_id();
    // Early bind session_id to avoid race with SessionStart message from StopController
    state.current_session_core_id = Some(session_id);
    state.active_time_manager = session.time_manager();
    if gp.ponder {
        state.active_time_manager = None;
        info_string("ponder_time_manager_detached=1");
    }
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
        let session_id = session.session_id();
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
                let time_manager = state.active_time_manager.take();

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
                                let mut engine_guard = state.lock_engine();
                                engine_guard.start_search(state.position.clone(), limits)
                            };

                            state.searching = true;
                            state.stop_flag = Some(stop_flag);
                            state.ponder_hit_flag = None;
                            state.active_time_manager = session.time_manager();
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
                    // Buffer the result for instant finalize on ponderhit
                    // Use actual session_id from SearchSession to avoid race with SessionStart message
                    state.pending_ponder_result = Some(crate::state::PonderResult {
                        best_move: result.best_move.map(|m| move_to_usi(&m)),
                        score: result.score,
                        depth: result.stats.depth,
                        nodes: result.stats.nodes,
                        elapsed_ms: result.stats.elapsed.as_millis() as u64,
                        pv_second: result.stats.pv.get(1).map(move_to_usi),
                        session_id: Some(session_id),
                        root_hash: state
                            .current_root_hash
                            .unwrap_or_else(|| state.position.zobrist_hash()),
                    });
                    let root =
                        state.current_root_hash.unwrap_or_else(|| state.position.zobrist_hash());
                    info_string(format!(
                        "search_completion_guard=ponder sid={} root={} elapsed_ms={} nodes={} depth={} ponder_result_buffered=1",
                        session_id,
                        fmt_hash(root),
                        result.stats.elapsed.as_millis(),
                        result.stats.nodes,
                        result.stats.depth
                    ));
                    // do nothing per USI specification
                } else {
                    // Instant finalize for short mate (if enabled and not ponder)
                    // Only consider positive scores (we are mating the opponent)
                    // Negative scores would indicate we are getting mated, which should not trigger early move
                    use engine_core::search::constants::mate_distance;
                    let should_instant_finalize = if state.opts.instant_mate_move_enabled {
                        mate_distance(result.score)
                            .map(|dist| {
                                dist > 0 && dist <= state.opts.instant_mate_move_max_distance as i32
                            })
                            .unwrap_or(false)
                    } else {
                        false
                    };

                    if should_instant_finalize {
                        let mate_dist = mate_distance(result.score).unwrap();
                        info_string(format!(
                            "instant_mate_move score={} distance={} max_distance={}",
                            result.score, mate_dist, state.opts.instant_mate_move_max_distance
                        ));
                    }

                    if let Some(tm) = time_manager {
                        let elapsed_ms = result.stats.elapsed.as_millis() as u64;
                        let time_state = state.time_state_for_update(elapsed_ms);
                        tm.update_after_move(elapsed_ms, time_state);
                    }
                    let stale = state
                        .current_root_hash
                        .map(|h| h != state.position.zobrist_hash())
                        .unwrap_or(false);
                    finalize_and_send(state, "finalize", Some(&result), stale, None);
                    if !state.bestmove_emitted {
                        let fallback = result
                            .best_move
                            .map(|mv| move_to_usi(&mv))
                            .unwrap_or_else(|| "resign".to_string());
                        let _ = emit_bestmove_once(state, fallback, None);
                    }
                    state.current_time_control = None;
                    state.current_root_hash = None;
                    state.pending_ponder_result = None;
                    state.notify_idle();
                }
            }
            TryResult::Pending => {
                // Still searching (no result yet)
            }
            TryResult::Disconnected => {
                // Search thread disconnected without sending result (panic or early exit)
                // Clean up state and emit fallback bestmove（PoisonErrorも救済）
                use engine_core::usi::move_to_usi;
                use log::error;

                error!(
                    "Search thread disconnected unexpectedly for session {}",
                    session.session_id()
                );
                let elapsed_ms =
                    state.active_time_manager.as_ref().map(|tm| tm.elapsed_ms()).unwrap_or(0);
                let stop_flag_state = state
                    .stop_flag
                    .as_ref()
                    .map(|flag| flag.load(Ordering::Acquire) as u8)
                    .unwrap_or(0);
                info_string(format!(
                    "search_thread_disconnected session_id={} root_hash={} elapsed_ms={} stop_flag={}",
                    session.session_id(),
                    state
                        .current_root_hash
                        .map(fmt_hash)
                        .unwrap_or_else(|| "none".to_string()),
                    elapsed_ms,
                    stop_flag_state
                ));

                state.searching = false;
                // Clear stop_flag - each session gets a fresh flag to avoid race conditions
                state.stop_flag = None;
                state.ponder_hit_flag = None;
                state.search_session = None;
                state.finalize_time_manager();
                state.current_time_control = None;
                state.current_root_hash = None;

                // Fallback: try to get a safe bestmove from Engine (TT or legal moves)
                let bestmove = match state.engine.lock() {
                    Ok(engine) => {
                        let final_best = engine.choose_final_bestmove(&state.position, None);
                        final_best.best_move.map(|m| move_to_usi(&m))
                    }
                    Err(poison) => {
                        let engine = poison.into_inner();
                        let final_best = engine.choose_final_bestmove(&state.position, None);
                        final_best.best_move.map(|m| move_to_usi(&m))
                    }
                }
                .or_else(|| {
                    let mg = engine_core::movegen::MoveGenerator::new();
                    if let Ok(list) = mg.generate_all(&state.position) {
                        list.as_slice().first().map(move_to_usi)
                    } else {
                        None
                    }
                });

                match bestmove {
                    Some(mv) => {
                        info_string(format!("fallback_bestmove move={mv} source=tt_or_legal"));
                        let _ = emit_bestmove_once(state, mv, None);
                    }
                    None => {
                        info_string("fallback_bestmove move=resign source=no_legal_moves");
                        let _ = emit_bestmove_once(state, String::from("resign"), None);
                    }
                }

                state.notify_idle();
            }
        }
    }
}

/// メインスレッドで TimeManager を監視し、探索停止を司るウォッチドッグ。
pub fn tick_time_watchdog(state: &mut EngineState) {
    if !state.searching {
        return;
    }

    let (tm, stop_flag) = match (state.active_time_manager.as_ref(), state.stop_flag.as_ref()) {
        (Some(tm), Some(flag)) => (tm, flag),
        _ => return,
    };

    if stop_flag.load(Ordering::Acquire) {
        return;
    }

    let elapsed = tm.elapsed_ms();
    let hard = tm.hard_limit_ms();
    let scheduled = tm.scheduled_end_ms();
    let opt = tm.opt_limit_ms();
    let mut finalize_reason: Option<FinalizeReason> = None;

    if hard != u64::MAX && elapsed >= hard {
        finalize_reason = Some(FinalizeReason::Hard);
    } else if scheduled != u64::MAX && elapsed >= scheduled {
        finalize_reason = Some(FinalizeReason::Planned);
    } else {
        if scheduled == u64::MAX && opt != u64::MAX && elapsed >= opt {
            tm.ensure_scheduled_stop(elapsed);
            let new_deadline = tm.scheduled_end_ms();
            if new_deadline != u64::MAX {
                info_string(format!(
                    "tm_watchdog_schedule elapsed_ms={} opt_ms={} scheduled_ms={}",
                    elapsed, opt, new_deadline
                ));
            }
        }

        if tm.is_time_critical() {
            finalize_reason = Some(FinalizeReason::TimeManagerStop);
        }
    }

    if let Some(reason) = finalize_reason {
        stop_flag.store(true, Ordering::Release);
        info_string(format!("tm_watchdog_stop reason={:?} elapsed_ms={}", reason, elapsed));
        // StopController 経由で finalize を要求し、優先度制御と stop_flag 連携を統一する。
        state.stop_controller.request_finalize(reason);
    }
}

#[cfg(test)]
mod watchdog_tests {
    use super::*;
    use crate::oob::poll_oob_finalize;
    use crate::stop::handle_stop;
    use engine_core::search::parallel::FinalizerMsg;
    use engine_core::time_management::{GamePhase, TimeLimits, TimeManager};
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering as AtomicOrdering;
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn go_ponder_emits_bestmove_only_after_stop() {
        let mut state = EngineState::new();
        state.opts.ponder = true;

        handle_go("go ponder btime 10000 wtime 10000 binc 0 winc 0", &mut state)
            .expect("go ponder should start search");

        // Wait briefly for the search session to spin up.
        for _ in 0..40 {
            if state.searching {
                break;
            }
            poll_oob_finalize(&mut state);
            poll_search_completion(&mut state);
            thread::sleep(Duration::from_millis(5));
        }

        assert!(state.searching, "ponder search should be active");
        assert!(state.current_is_ponder, "state must record ponder mode");
        assert!(!state.bestmove_emitted);

        // Poll for a short duration to ensure no bestmove is emitted while pondering.
        for _ in 0..10 {
            poll_oob_finalize(&mut state);
            poll_search_completion(&mut state);
            assert!(!state.bestmove_emitted);
            thread::sleep(Duration::from_millis(10));
        }

        handle_stop(&mut state);

        // Give the finalizer loop a moment to flush the result.
        for _ in 0..30 {
            poll_oob_finalize(&mut state);
            poll_search_completion(&mut state);
            if !state.searching && state.bestmove_emitted {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        assert!(state.bestmove_emitted, "bestmove must emit after stop");
        assert!(!state.current_is_ponder);
    }

    #[test]
    fn watchdog_triggers_after_hard_deadline() {
        let mut state = EngineState::new();
        state.searching = true;

        let stop_flag = Arc::new(AtomicBool::new(false));
        state.stop_flag = Some(Arc::clone(&stop_flag));

        let limits = TimeLimits {
            time_control: TimeControl::FixedTime { ms_per_move: 50 },
            ..Default::default()
        };

        let tm = Arc::new(TimeManager::new(&limits, Color::Black, 0, GamePhase::MiddleGame));
        state.active_time_manager = Some(Arc::clone(&tm));

        let timeout = Duration::from_secs(2);
        let start = Instant::now();

        while !stop_flag.load(AtomicOrdering::Acquire) {
            tick_time_watchdog(&mut state);
            if stop_flag.load(AtomicOrdering::Acquire) {
                break;
            }
            if start.elapsed() >= timeout {
                panic!(
                    "watchdog did not trigger within timeout (hard_limit_ms={})",
                    tm.hard_limit_ms()
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        assert!(stop_flag.load(AtomicOrdering::Acquire));
    }

    #[test]
    fn watchdog_establishes_scheduled_stop_before_planned_finalize() {
        let mut state = EngineState::new();
        state.searching = true;

        let stop_flag = Arc::new(AtomicBool::new(false));
        state.stop_flag = Some(Arc::clone(&stop_flag));

        let limits = TimeLimits {
            time_control: TimeControl::FixedTime { ms_per_move: 1_000 },
            ..Default::default()
        };

        let tm = Arc::new(TimeManager::new(&limits, Color::Black, 0, GamePhase::MiddleGame));
        state.active_time_manager = Some(Arc::clone(&tm));

        assert_eq!(tm.scheduled_end_ms(), u64::MAX);

        let deadline = Instant::now() + Duration::from_secs(4);
        while Instant::now() < deadline && tm.scheduled_end_ms() == u64::MAX {
            tick_time_watchdog(&mut state);
            std::thread::sleep(Duration::from_millis(10));
        }

        assert_ne!(
            tm.scheduled_end_ms(),
            u64::MAX,
            "watchdog should schedule a planned stop before hitting hard deadline"
        );
        assert!(!stop_flag.load(AtomicOrdering::Acquire));

        if let Some(rx) = state.finalizer_rx.as_ref() {
            assert!(rx.try_recv().is_err(), "no finalize should be emitted when only scheduling");
        }
    }

    #[test]
    fn watchdog_triggers_time_manager_stop_when_critical() {
        let mut state = EngineState::new();
        state.searching = true;

        let stop_flag = Arc::new(AtomicBool::new(false));
        state.stop_flag = Some(Arc::clone(&stop_flag));

        let limits = TimeLimits {
            time_control: TimeControl::Fischer {
                white_ms: 10,
                black_ms: 10,
                increment_ms: 0,
            },
            ..Default::default()
        };

        let tm = Arc::new(TimeManager::new(&limits, Color::Black, 0, GamePhase::MiddleGame));
        state.active_time_manager = Some(tm);

        tick_time_watchdog(&mut state);

        assert!(stop_flag.load(AtomicOrdering::Acquire));
        let rx = state.finalizer_rx.as_ref().expect("finalizer receiver available");
        match rx.recv_timeout(Duration::from_millis(20)) {
            Ok(FinalizerMsg::Finalize { reason, .. }) => {
                assert_eq!(reason, FinalizeReason::TimeManagerStop);
            }
            Ok(other) => panic!("unexpected finalizer message: {:?}", other),
            Err(err) => panic!("expected finalizer message, got error: {err}"),
        }
    }

    #[test]
    fn watchdog_triggers_planned_before_hard() {
        let mut state = EngineState::new();
        state.searching = true;

        let stop_flag = Arc::new(AtomicBool::new(false));
        state.stop_flag = Some(Arc::clone(&stop_flag));

        let limits = TimeLimits {
            time_control: TimeControl::FixedTime { ms_per_move: 250 },
            ..Default::default()
        };

        let tm = Arc::new(TimeManager::new(&limits, Color::Black, 0, GamePhase::MiddleGame));
        state.active_time_manager = Some(Arc::clone(&tm));

        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && !stop_flag.load(AtomicOrdering::Acquire) {
            tick_time_watchdog(&mut state);
            std::thread::sleep(Duration::from_millis(10));
        }

        assert!(stop_flag.load(AtomicOrdering::Acquire));
        let rx = state.finalizer_rx.as_ref().expect("finalizer receiver available");
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(FinalizerMsg::Finalize { reason, .. }) => {
                assert_eq!(reason, FinalizeReason::Planned);
            }
            Ok(other) => panic!("unexpected finalizer message: {:?}", other),
            Err(err) => panic!("expected planned finalize, got error: {err}"),
        }

        // ensure scheduled deadline was設定されていた
        assert_ne!(tm.scheduled_end_ms(), u64::MAX);
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
