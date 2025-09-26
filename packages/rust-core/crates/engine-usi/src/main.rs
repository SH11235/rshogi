use anyhow::{anyhow, Result};
use engine_core::engine::controller::{Engine, EngineType, FinalBestSource};
use engine_core::search::limits::{SearchLimits, SearchLimitsBuilder};
use engine_core::shogi::{Color, Position};
use engine_core::time_management::{TimeControl, TimeParameters, TimeParametersBuilder};
use engine_core::usi::{
    append_usi_score_and_bound, create_position, move_to_usi, score_view_from_internal,
};
use log::info;
use std::error::Error as StdError;
use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
struct UsiOptions {
    // Core engine settings
    hash_mb: usize,
    threads: usize,
    ponder: bool,
    engine_type: EngineType,
    eval_file: Option<String>,

    // Time parameters
    overhead_ms: u64,
    network_delay_ms: u64,
    network_delay2_ms: u64,
    min_think_ms: u64,

    // Byoyomi and policy extras
    byoyomi_periods: u32,
    byoyomi_early_finish_ratio: u8, // 50-95
    byoyomi_safety_ms: u64,         // hard-limit減算
    pv_stability_base: u64,         // 10-200
    pv_stability_slope: u64,        // 0-20
    slow_mover_pct: u8,             // 50-200
    max_time_ratio_pct: u32,        // 100-800 (% → x/100)
    move_horizon_trigger_ms: u64,
    move_horizon_min_moves: u32,

    // Others
    stochastic_ponder: bool,
    force_terminate_on_hard_deadline: bool, // 受理のみ（非推奨）
    mate_early_stop: bool,
    // Stop bounded wait time
    stop_wait_ms: u64,
    // MultiPV lines
    multipv: u8,
    // Policy: gameover時にもbestmoveを送るか
    gameover_sends_bestmove: bool,
    // Fail-safe guard (parallel) を有効化するか
    fail_safe_guard: bool,
    // SIMD clamp (runtime). None = Auto
    simd_max_level: Option<String>,
    // NNUE SIMD clamp (runtime). None = Auto
    nnue_simd: Option<String>,
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
            multipv: 1,
            gameover_sends_bestmove: false,
            fail_safe_guard: false,
            simd_max_level: None,
            nnue_simd: None,
        }
    }
}

#[derive(Debug, Default, Clone)]
struct GoParams {
    depth: Option<u32>,
    nodes: Option<u64>,
    movetime: Option<u64>,
    infinite: bool,
    ponder: bool,
    btime: Option<u64>,
    wtime: Option<u64>,
    binc: Option<u64>,
    winc: Option<u64>,
    byoyomi: Option<u64>,
    periods: Option<u32>,
    moves_to_go: Option<u32>,
}

struct EngineState {
    engine: Arc<Mutex<Engine>>,
    position: Position,
    // Canonicalized last position command parts (for Stochastic_Ponder)
    pos_from_startpos: bool,
    pos_sfen: Option<String>,
    pos_moves: Vec<String>,
    opts: UsiOptions,
    // runtime flags
    searching: bool,
    stop_flag: Option<Arc<AtomicBool>>,
    ponder_hit_flag: Option<Arc<AtomicBool>>,
    worker: Option<thread::JoinHandle<()>>,
    result_rx: Option<mpsc::Receiver<(u64, engine_core::search::SearchResult)>>,
    // Stochastic Ponder control
    current_is_stochastic_ponder: bool,
    current_is_ponder: bool,
    stoch_suppress_result: bool,
    pending_research_after_ponderhit: bool,
    last_go_params: Option<GoParams>,
    // Session root hash for stale-result guard
    current_root_hash: Option<u64>,
    current_search_id: u64,
    // Ensure we emit at most one bestmove per go-session
    bestmove_emitted: bool,
    // Current (inner) time control for stop/gameover policy decisions
    current_time_control: Option<TimeControl>,
    // Reaper: background joiner for detached worker threads
    reaper_tx: Option<mpsc::Sender<std::thread::JoinHandle<()>>>,
    reaper_handle: Option<std::thread::JoinHandle<()>>,
    reaper_queue_len: Arc<AtomicUsize>,
}

impl EngineState {
    fn new() -> Self {
        // Initialize engine-core static tables once
        engine_core::init::init_all_tables_once();

        let mut engine = Engine::new(EngineType::Material);
        engine.set_threads(1);
        engine.set_hash_size(1024);

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
            reaper_tx: None,
            reaper_handle: None,
            reaper_queue_len: Arc::new(AtomicUsize::new(0)),
            current_time_control: None,
        }
    }

    fn apply_options_to_engine(&mut self) {
        if let Ok(ref mut eng) = self.engine.lock() {
            eng.set_engine_type(self.opts.engine_type);
            eng.set_threads(self.opts.threads);
            eng.set_hash_size(self.opts.hash_mb);
            // Persist MultiPV so it survives ClearHash/TT resize
            eng.set_multipv_persistent(self.opts.multipv);
            // NNUE weights
            if matches!(self.opts.engine_type, EngineType::Nnue | EngineType::EnhancedNnue) {
                if let Some(ref path) = self.opts.eval_file {
                    if !path.is_empty() {
                        if let Err(e) = eng.load_nnue_weights(path) {
                            log_nnue_load_error(path, &*e);
                        }
                        // Log NNUE backend kind (classic/single) for diagnostics
                        if let Some(kind) = eng.nnue_backend_kind() {
                            info_string(format!("nnue_backend={}", kind));
                        }
                    }
                }
            }
        }
        // MateEarlyStop: global toggle
        engine_core::search::config::set_mate_early_stop_enabled(self.opts.mate_early_stop);
    }
}

fn log_nnue_load_error(path: &str, err: &(dyn StdError + 'static)) {
    use engine_core::evaluation::nnue::error::NNUEError;

    // Try to downcast to typed NNUEError for structured reporting
    if let Some(ne) = err.downcast_ref::<NNUEError>() {
        match ne {
            NNUEError::Weights(we) => {
                log::error!("[NNUE] Failed to load classic weights '{}': {}", path, we);
                if let Some(src) = we.source() {
                    log::debug!("  caused by: {}", src);
                }
            }
            NNUEError::SingleWeights(se) => {
                log::error!("[NNUE] Failed to load SINGLE weights '{}': {}", path, se);
                if let Some(src) = se.source() {
                    log::debug!("  caused by: {}", src);
                }
            }
            NNUEError::BothWeightsLoadFailed { classic, single } => {
                log::error!(
                    "[NNUE] Failed to load weights '{}': classic={}, single={}",
                    path,
                    classic,
                    single
                );
                if let Some(src) = classic.source() {
                    log::debug!("  classic caused by: {}", src);
                }
                if let Some(src) = single.source() {
                    log::debug!("  single caused by: {}", src);
                }
            }
            NNUEError::Io(ioe) => {
                log::error!("[NNUE] I/O error reading '{}': {}", path, ioe);
            }
            NNUEError::KingNotFound(color) => {
                log::error!("[NNUE] Internal error: king not found for {:?}", color);
            }
            NNUEError::EmptyAccumulatorStack => {
                log::error!("[NNUE] Internal error: empty accumulator stack");
            }
            NNUEError::InvalidPiece(sq) => {
                log::error!("[NNUE] Internal error: invalid piece at {:?}", sq);
            }
            NNUEError::InvalidMove(desc) => {
                log::error!("[NNUE] Internal error: invalid move: {}", desc);
            }
            NNUEError::DimensionMismatch { expected, actual } => {
                log::error!(
                    "[NNUE] Weight dimension mismatch (expected {}, got {}) for '{}': please use matching weights",
                    expected, actual, path
                );
            }
            _ => {
                // Future-proof for non_exhaustive
                log::error!("[NNUE] Error while loading weights '{}': {}", path, ne);
            }
        }
        return;
    }

    // Fallback: untyped error
    log::error!("[NNUE] Failed to load NNUE weights '{}': {}", path, err);
}

fn print_engine_type_options() {
    usi_println("option name EngineType type combo default Material var Material var Enhanced var Nnue var EnhancedNnue");
}

fn print_time_policy_options(opts: &UsiOptions) {
    usi_println(&format!(
        "option name OverheadMs type spin default {} min 0 max 5000",
        opts.overhead_ms
    ));
    usi_println("option name ByoyomiOverheadMs type spin default 200 min 0 max 5000");
    usi_println(&format!(
        "option name ByoyomiSafetyMs type spin default {} min 0 max 2000",
        opts.byoyomi_safety_ms
    ));
    usi_println(&format!(
        "option name ByoyomiPeriods type spin default {} min 1 max 10",
        opts.byoyomi_periods
    ));
    usi_println(&format!(
        "option name ByoyomiEarlyFinishRatio type spin default {} min 50 max 95",
        opts.byoyomi_early_finish_ratio
    ));
    usi_println(&format!(
        "option name PVStabilityBase type spin default {} min 10 max 200",
        opts.pv_stability_base
    ));
    usi_println(&format!(
        "option name PVStabilitySlope type spin default {} min 0 max 20",
        opts.pv_stability_slope
    ));
    usi_println(&format!(
        "option name SlowMover type spin default {} min 50 max 200",
        opts.slow_mover_pct
    ));
    usi_println(&format!(
        "option name MaxTimeRatioPct type spin default {} min 100 max 800",
        opts.max_time_ratio_pct
    ));
    usi_println(&format!(
        "option name MoveHorizonTriggerMs type spin default {} min 0 max 600000",
        opts.move_horizon_trigger_ms
    ));
    usi_println(&format!(
        "option name MoveHorizonMinMoves type spin default {} min 0 max 200",
        opts.move_horizon_min_moves
    ));
    // Stop bounded-wait configuration
    usi_println(&format!(
        "option name StopWaitMs type spin default {} min 0 max 2000",
        opts.stop_wait_ms
    ));
    // Gameover policy: whether to emit bestmove on gameover
    usi_println(&format!(
        "option name GameoverSendsBestmove type check default {}",
        if opts.gameover_sends_bestmove {
            "true"
        } else {
            "false"
        }
    ));
    // Fail-safe guard toggle
    usi_println(&format!(
        "option name FailSafeGuard type check default {}",
        if opts.fail_safe_guard {
            "true"
        } else {
            "false"
        }
    ));
}

// -------------- USI helpers --------------

fn usi_println(s: &str) {
    let mut out = io::stdout();
    writeln!(out, "{}", s).ok();
    out.flush().ok();
}

#[inline]
fn info_string<S: AsRef<str>>(s: S) {
    usi_println(&format!("info string {}", s.as_ref()));
}

#[inline]
fn fmt_hash(h: u64) -> String {
    format!("{:016x}", h)
}

#[inline]
fn source_to_str(src: FinalBestSource) -> &'static str {
    match src {
        FinalBestSource::Book => "book",
        FinalBestSource::Committed => "committed",
        FinalBestSource::TT => "tt",
        FinalBestSource::LegalFallback => "legal",
        FinalBestSource::Resign => "resign",
    }
}

// score formatting is provided by engine_core::usi::append_usi_score_and_bound / ScoreView

/// Centralized finalize + bestmove emission
/// - label: "finalize" | "stop_finalize" | "stop_timeout_finalize"
/// - result: Some(&SearchResult) when available to build committed PV and log soft/hard
/// - stale: whether root hash mismatched (when true: emits "bestmove resign" and returns early)
fn finalize_and_send(
    state: &mut EngineState,
    label: &str,
    result: Option<&engine_core::search::SearchResult>,
    stale: bool,
) {
    // If stale, do not try to select a move from the new position; emit resign and return.
    if stale {
        info_string(format!("{label}_stale resign=1"));
        usi_println("bestmove resign");
        state.bestmove_emitted = true;
        state.current_root_hash = None;
        return;
    }
    // Build committed when applicable（PVヘッドとbestの整合を強制）
    let committed = if let Some(res) = result {
        if !stale {
            let mut committed_pv = res.stats.pv.clone();
            if let Some(bm) = res.best_move {
                if committed_pv.first().copied() != Some(bm) {
                    info_string(format!(
                        "pv_head_mismatch=1 pv0={} best={}",
                        committed_pv.first().map(move_to_usi).unwrap_or_else(|| "-".to_string()),
                        move_to_usi(&bm)
                    ));
                    committed_pv.clear();
                    committed_pv.push(bm);
                }
            }
            Some(engine_core::search::CommittedIteration {
                depth: res.stats.depth,
                seldepth: res.stats.seldepth,
                score: res.score,
                pv: committed_pv,
                node_type: res.node_type,
                nodes: res.stats.nodes,
                elapsed: res.stats.elapsed,
            })
        } else {
            None
        }
    } else {
        None
    };

    let final_best = {
        let eng = state.engine.lock().unwrap();
        eng.choose_final_bestmove(&state.position, committed.as_ref())
    };

    // Optional soft/hard for logging
    let (soft_ms, hard_ms) = result
        .and_then(|r| r.stop_info.as_ref())
        .map(|si| (si.soft_limit_ms, si.hard_limit_ms))
        .unwrap_or((0, 0));

    // Snapshot (diagnostic): best, pv0, depth, nodes, elapsed, stop_reason
    if let Some(res) = result {
        let best_usi = {
            // Will be replaced below by finalize selection, but snapshot current best too
            res.best_move.map(|m| move_to_usi(&m)).unwrap_or_else(|| "resign".to_string())
        };
        let pv0_usi = res.stats.pv.first().map(move_to_usi).unwrap_or_else(|| "-".to_string());
        let stop_reason = res
            .stop_info
            .as_ref()
            .map(|si| format!("{:?}", si.reason))
            .unwrap_or_else(|| "Unknown".to_string());
        info_string(format!(
            "finalize_snapshot best={} pv0={} depth={} nodes={} elapsed_ms={} stop_reason={}",
            best_usi,
            pv0_usi,
            res.stats.depth,
            res.stats.nodes,
            res.stats.elapsed.as_millis(),
            stop_reason
        ));
    }

    // Log selection
    info_string(format!(
        "{}_select source={} move={} stale={} soft_ms={} hard_ms={}",
        label,
        source_to_str(final_best.source),
        final_best
            .best_move
            .map(|m| move_to_usi(&m))
            .unwrap_or_else(|| "resign".to_string()),
        if stale { 1 } else { 0 },
        soft_ms,
        hard_ms
    ));

    // Emit MultiPV info lines (if available and not stale). If absent, synthesize SinglePV line.
    if let Some(res) = result {
        if !stale {
            // Obtain hashfull once (permille) and compute aggregate nps
            let hf_permille = {
                let eng = state.engine.lock().unwrap();
                eng.tt_hashfull_permille()
            };
            let nps_agg: u128 = if res.stats.elapsed.as_millis() > 0 {
                (res.stats.nodes as u128).saturating_mul(1000) / res.stats.elapsed.as_millis()
            } else {
                0
            };

            if let Some(ref lines) = res.lines {
                if !lines.is_empty() {
                    for (i, ln) in lines.iter().enumerate() {
                        // info multipv N depth D time T nodes N score (cp|mate) X [lowerbound|upperbound] pv ...
                        let mut s = String::from("info");
                        s.push_str(&format!(" multipv {}", i + 1));
                        s.push_str(&format!(" depth {}", res.stats.depth));
                        if let Some(sd) = res.stats.seldepth {
                            s.push_str(&format!(" seldepth {}", sd));
                        }
                        s.push_str(&format!(" time {}", res.stats.elapsed.as_millis()));
                        s.push_str(&format!(" nodes {}", res.stats.nodes));
                        s.push_str(&format!(" nps {}", nps_agg));
                        s.push_str(&format!(" hashfull {}", hf_permille));

                        // Prefer mate output when available; otherwise cp
                        let mut view = score_view_from_internal(ln.score_internal);
                        // Guard against sentinel exposure (-SEARCH_INF)
                        if let engine_core::usi::ScoreView::Cp(cp) = view {
                            if cp <= -(engine_core::search::constants::SEARCH_INF - 1) {
                                view = engine_core::usi::ScoreView::Cp(-29_999);
                            }
                        }
                        append_usi_score_and_bound(&mut s, view, ln.bound);

                        if !ln.pv.is_empty() {
                            s.push_str(" pv");
                            for m in ln.pv.iter() {
                                s.push(' ');
                                s.push_str(&move_to_usi(m));
                            }
                        }
                        usi_println(&s);
                    }
                } else {
                    // linesが空の場合のフォールバック: SinglePV相当の行を1本出力
                    let mut s = String::from("info");
                    s.push_str(" multipv 1");
                    s.push_str(&format!(" depth {}", res.stats.depth));
                    if let Some(sd) = res.stats.seldepth {
                        s.push_str(&format!(" seldepth {}", sd));
                    }
                    s.push_str(&format!(" time {}", res.stats.elapsed.as_millis()));
                    s.push_str(&format!(" nodes {}", res.stats.nodes));
                    s.push_str(&format!(" nps {}", nps_agg));
                    s.push_str(&format!(" hashfull {}", hf_permille));
                    let mut view = score_view_from_internal(res.score);
                    if let engine_core::usi::ScoreView::Cp(cp) = view {
                        if cp <= -(engine_core::search::constants::SEARCH_INF - 1) {
                            view = engine_core::usi::ScoreView::Cp(-29_999);
                        }
                    }
                    append_usi_score_and_bound(&mut s, view, res.node_type);
                    let pv_ref: &[_] = if !final_best.pv.is_empty() {
                        &final_best.pv
                    } else {
                        &res.stats.pv
                    };
                    if !pv_ref.is_empty() {
                        s.push_str(" pv");
                        for m in pv_ref.iter() {
                            s.push(' ');
                            s.push_str(&move_to_usi(m));
                        }
                    }
                    usi_println(&s);
                }
            } else {
                // SinglePV: 合成して必ず1本出す
                let mut s = String::from("info");
                s.push_str(" multipv 1");
                s.push_str(&format!(" depth {}", res.stats.depth));
                if let Some(sd) = res.stats.seldepth {
                    s.push_str(&format!(" seldepth {}", sd));
                }
                s.push_str(&format!(" time {}", res.stats.elapsed.as_millis()));
                s.push_str(&format!(" nodes {}", res.stats.nodes));
                s.push_str(&format!(" nps {}", nps_agg));
                s.push_str(&format!(" hashfull {}", hf_permille));
                let mut view = score_view_from_internal(res.score);
                if let engine_core::usi::ScoreView::Cp(cp) = view {
                    if cp <= -(engine_core::search::constants::SEARCH_INF - 1) {
                        view = engine_core::usi::ScoreView::Cp(-29_999);
                    }
                }
                append_usi_score_and_bound(&mut s, view, res.node_type);
                let pv_ref: &[_] = if !final_best.pv.is_empty() {
                    &final_best.pv
                } else {
                    &res.stats.pv
                };
                if !pv_ref.is_empty() {
                    s.push_str(" pv");
                    for m in pv_ref.iter() {
                        s.push(' ');
                        s.push_str(&move_to_usi(m));
                    }
                }
                usi_println(&s);
            }
        }
    }

    // Emit TT metrics summary (feature: tt-metrics)
    #[cfg(feature = "tt-metrics")]
    {
        let summary_opt = {
            let eng = state.engine.lock().unwrap();
            eng.tt_metrics_summary()
        };
        if let Some(sum) = summary_opt {
            for line in sum.lines() {
                usi_println(&format!("info string tt_metrics {}", line));
            }
        }
    }

    // Emit bestmove (+ ponder)
    let final_usi = final_best
        .best_move
        .map(|m| move_to_usi(&m))
        .unwrap_or_else(|| "resign".to_string());
    let ponder_mv = if state.opts.ponder {
        // Prefer PV second move; fallback to TT-derived ponder for better UX
        final_best.pv.get(1).map(move_to_usi).or_else(|| {
            final_best.best_move.and_then(|bm| {
                let eng = state.engine.lock().unwrap();
                eng.get_ponder_from_tt(&state.position, bm).map(|m| move_to_usi(&m))
            })
        })
    } else {
        None
    };
    if let Some(p) = ponder_mv {
        usi_println(&format!("bestmove {} ponder {}", final_usi, p));
    } else {
        usi_println(&format!("bestmove {}", final_usi));
    }
    state.bestmove_emitted = true;
    state.current_root_hash = None;
}

/// Fast finalize that never takes the Engine lock.
///
/// Use this in timeout/immediate-finalize paths where the worker thread may still
/// be holding the Engine mutex. It selects a legal move directly from the current
/// position without consulting TT/committed PV, ensuring an immediate bestmove.
fn finalize_and_send_fast(state: &mut EngineState, label: &str) {
    // Generate legal moves without touching the Engine lock
    let mg = engine_core::movegen::MoveGenerator::new();
    match mg.generate_all(&state.position) {
        Ok(list) => {
            let slice = list.as_slice();
            if slice.is_empty() {
                info_string(format!(
                    "{}_fast_select source=legal move=resign stale=0 soft_ms=0 hard_ms=0",
                    label
                ));
                usi_println("bestmove resign");
                state.bestmove_emitted = true;
            } else {
                // Prefer non-king moves on non-check positions; prioritize capture/drop/promotion
                let in_check = state.position.is_in_check();
                let is_king_move = |m: &engine_core::shogi::Move| {
                    m.piece_type()
                        .or_else(|| {
                            m.from().and_then(|sq| {
                                state.position.board.piece_on(sq).map(|p| p.piece_type)
                            })
                        })
                        .map(|pt| matches!(pt, engine_core::shogi::PieceType::King))
                        .unwrap_or(false)
                };
                let is_tactical = |m: &engine_core::shogi::Move| -> bool {
                    m.is_drop() || m.is_capture_hint() || m.is_promote()
                };

                let chosen = if in_check {
                    slice[0]
                } else if let Some(&m) = slice.iter().find(|m| !is_king_move(m) && is_tactical(m)) {
                    m
                } else if let Some(&m) = slice.iter().find(|m| !is_king_move(m)) {
                    m
                } else {
                    slice[0]
                };

                let mv_usi = move_to_usi(&chosen);
                info_string(format!(
                    "{}_fast_select source=legal move={} stale=0 soft_ms=0 hard_ms=0",
                    label, mv_usi
                ));
                usi_println(&format!("bestmove {}", mv_usi));
                state.bestmove_emitted = true;
            }
        }
        Err(_) => {
            // In unexpected failure, be conservative
            info_string(format!("{}_fast_select_error resign_fallback=1", label));
            usi_println("bestmove resign");
            state.bestmove_emitted = true;
        }
    }
    state.current_root_hash = None;
}

fn send_id_and_options(opts: &UsiOptions) {
    usi_println("id name RustShogi USI (core)");
    usi_println("id author RustShogi Team");

    // Options we support in this thin USI
    // Align max with engine clamp to avoid confusion
    usi_println(&format!(
        "option name USI_Hash type spin default {} min 1 max 1024",
        opts.hash_mb
    ));
    usi_println(&format!("option name Threads type spin default {} min 1 max 256", opts.threads));
    usi_println("option name USI_Ponder type check default true");
    usi_println(&format!("option name MultiPV type spin default {} min 1 max 20", opts.multipv));
    usi_println(&format!(
        "option name MinThinkMs type spin default {} min 0 max 10000",
        opts.min_think_ms
    ));
    // Engine type / model
    print_engine_type_options();
    usi_println("option name EvalFile type filename default ");
    usi_println("option name ClearHash type button");
    // SIMD clamp control (optional)
    usi_println("option name SIMDMaxLevel type combo default Auto var Auto var Scalar var SSE2 var AVX var AVX512F");
    usi_println(
        "option name NNUE_Simd type combo default Auto var Auto var Scalar var SSE41 var AVX2",
    );
    // Legacy/GUI向け 時間ポリシー（内部にマッピング）
    print_time_policy_options(opts);
    // 旧CLI系スイッチ
    usi_println("option name Stochastic_Ponder type check default false");
    usi_println("option name ForceTerminateOnHardDeadline type check default true");
    usi_println("option name MateEarlyStop type check default true");
}

fn parse_position(cmd: &str, state: &mut EngineState) -> Result<()> {
    // Format: position [startpos | sfen <sfen...>] [moves m1 m2 ...]
    let mut tokens = cmd.split_whitespace().skip(1).peekable();
    let mut have_pos = false;
    // Reset record of current position components
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
                // Remaining until optional "moves" is SFEN components
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
                // Defer parsing to core; just store SFEN parts here
                have_pos = true;
                state.pos_from_startpos = false;
                state.pos_sfen = Some(sfen);
            }
            "moves" => {
                let _ = tokens.next();
                // Collect moves only; legality will be validated by core
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

    // Build via core helper (validates legality and promotions)
    let pos =
        create_position(state.pos_from_startpos, state.pos_sfen.as_deref(), &state.pos_moves)?;
    state.position = pos;
    Ok(())
}

fn parse_go(cmd: &str) -> GoParams {
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

fn limits_from_go(
    gp: &GoParams,
    side: Color,
    opts: &UsiOptions,
    ponder_flag: Option<Arc<AtomicBool>>,
    stop_flag: Arc<AtomicBool>,
) -> SearchLimits {
    // Build time parameters
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
    // Map percentages and extras
    tp.byoyomi_soft_ratio = (opts.byoyomi_early_finish_ratio as f64 / 100.0).clamp(0.5, 0.95);
    tp.slow_mover_pct = opts.slow_mover_pct;
    tp.max_time_ratio = (opts.max_time_ratio_pct as f64 / 100.0).max(1.0);
    tp.move_horizon_trigger_ms = opts.move_horizon_trigger_ms;
    tp.move_horizon_min_moves = opts.move_horizon_min_moves;

    let mut builder = SearchLimitsBuilder::default();

    // Depth/Nodes
    if let Some(d) = gp.depth {
        builder = builder.depth(d.min(127) as u8);
    }
    if let Some(n) = gp.nodes {
        builder = builder.nodes(n);
    }

    // Time control
    let tc = if gp.infinite {
        TimeControl::Infinite
    } else if let Some(ms) = gp.movetime {
        TimeControl::FixedTime { ms_per_move: ms }
    } else if let Some(byo) = gp.byoyomi {
        // treat as byoyomi if provided and > 0
        let mt = match side {
            Color::Black => gp.btime.unwrap_or(0),
            Color::White => gp.wtime.unwrap_or(0),
        };
        let periods = gp.periods.unwrap_or(opts.byoyomi_periods).max(1);
        TimeControl::Byoyomi {
            main_time_ms: mt,
            byoyomi_ms: byo,
            periods,
        }
    } else if gp.btime.is_some() || gp.wtime.is_some() || gp.binc.is_some() || gp.winc.is_some() {
        // Fischer / sudden-death
        let (white, black) = (gp.wtime.unwrap_or(0), gp.btime.unwrap_or(0));
        let inc = match side {
            Color::Black => gp.binc.unwrap_or(0),
            Color::White => gp.winc.unwrap_or(0),
        };
        TimeControl::Fischer {
            white_ms: white,
            black_ms: black,
            increment_ms: inc,
        }
    } else {
        TimeControl::Infinite
    };

    // Apply ponder wrapping if requested
    let tc = if gp.ponder {
        TimeControl::Ponder(Box::new(tc))
    } else {
        tc
    };

    builder
        .time_control(tc)
        .moves_to_go(gp.moves_to_go.unwrap_or(0))
        .time_parameters(tp)
        .stop_flag(stop_flag)
        .apply_if(gp.ponder && ponder_flag.is_some(), |b| {
            b.ponder_hit_flag(ponder_flag.clone().unwrap())
        })
        .apply_if(true, |b| b.enable_fail_safe(opts.fail_safe_guard))
        .build()
}

// Extension trait to make conditional builder calls ergonomic
trait BuilderApplyIf {
    fn apply_if<F: FnOnce(Self) -> Self>(self, cond: bool, f: F) -> Self
    where
        Self: Sized,
    {
        if cond {
            f(self)
        } else {
            self
        }
    }
}
impl BuilderApplyIf for SearchLimitsBuilder {}

fn run_search_thread(
    engine: Arc<Mutex<Engine>>,
    mut position: Position,
    limits: SearchLimits,
    info_enabled: bool,
    tx: mpsc::Sender<(u64, engine_core::search::SearchResult)>,
    search_id: u64,
) {
    // Optional info callback
    // IMPORTANT: The info callback must not lock the Engine mutex, because the
    // search runs while holding that lock. Locking here would deadlock.
    let info_callback: engine_core::search::types::InfoCallback =
        Arc::new(move |depth, score, nodes, elapsed, pv, node_type| {
            if !info_enabled {
                return;
            }
            let mut line = String::from("info");
            line.push_str(&format!(" depth {}", depth));
            line.push_str(&format!(" time {}", elapsed.as_millis()));
            line.push_str(&format!(" nodes {}", nodes));
            // Add nps (nodes per second) with minimal overhead
            let ems = elapsed.as_millis();
            if ems > 0 {
                let nps = (nodes as u128).saturating_mul(1000) / ems;
                line.push_str(&format!(" nps {}", nps));
            }
            // NOTE: Do not query hashfull here to avoid locking Engine during search.
            // If needed, it will be emitted at finalize timing.
            // score: normalize to mate or cp with proper bound tag placement
            let mut view = score_view_from_internal(score);
            if let engine_core::usi::ScoreView::Cp(cp) = view {
                if cp <= -(engine_core::search::constants::SEARCH_INF - 1) {
                    view = engine_core::usi::ScoreView::Cp(-29_999);
                }
            }
            append_usi_score_and_bound(&mut line, view, node_type);
            if !pv.is_empty() {
                line.push_str(" pv");
                for m in pv.iter() {
                    line.push(' ');
                    line.push_str(&move_to_usi(m));
                }
            }
            usi_println(&line);
        });

    // Build final limits with callback
    let limits = SearchLimits {
        info_callback: Some(info_callback),
        ..limits
    };

    // Do the search (ensure elapsed is set even on corner paths)
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

fn main() -> Result<()> {
    env_logger::init();
    let stdin = io::stdin();
    let mut state = EngineState::new();

    // 起動時に core のビルド時 feature を一度だけ出力（デバッグ/再現に有用）
    let feat = engine_core::evaluation::nnue::enabled_features_str();
    info_string(format!("core_features={}", feat));
    // 追加: 実効SIMDクランプの表示（環境変数ベース）
    match std::env::var("SHOGI_SIMD_MAX") {
        Ok(v) => info_string(format!("simd_clamp={}", v)),
        Err(_) => info_string("simd_clamp=auto"),
    }
    match std::env::var("SHOGI_NNUE_SIMD") {
        Ok(v) => info_string(format!("nnue_simd_clamp={}", v)),
        Err(_) => info_string("nnue_simd_clamp=auto"),
    }

    // Start background reaper thread to join detached workers
    let (reaper_tx, reaper_rx) = mpsc::channel::<thread::JoinHandle<()>>();
    let reaper_queue_len = Arc::clone(&state.reaper_queue_len);
    let reaper_handle = thread::Builder::new()
        .name("usi-reaper".to_string())
        .spawn(move || {
            let mut cum_ms: u128 = 0;
            for h in reaper_rx {
                let start = Instant::now();
                let _ = h.join();
                let dur = start.elapsed().as_millis();
                // Decrement queue length
                reaper_queue_len.fetch_sub(1, Ordering::SeqCst);
                if dur > 50 {
                    usi_println(&format!("info string reaper_join_ms={}", dur));
                }
                cum_ms += dur;
                if cum_ms >= 1000 {
                    usi_println(&format!("info string reaper_cum_join_ms={}", cum_ms));
                    cum_ms = 0;
                }
            }
        })
        .expect("failed to spawn reaper thread");
    state.reaper_tx = Some(reaper_tx);
    state.reaper_handle = Some(reaper_handle);

    // Spawn stdin reader thread
    let (line_tx, line_rx) = mpsc::channel::<String>();
    thread::spawn(move || {
        for line in stdin.lock().lines() {
            if let Ok(s) = line {
                let _ = line_tx.send(s);
            } else {
                break;
            }
        }
    });

    loop {
        // Finalize if a result has arrived
        if state.searching {
            if let Some(rx) = &state.result_rx {
                match rx.try_recv() {
                    Ok((sid, result)) => {
                        if sid != state.current_search_id {
                            info_string(format!(
                                "ignore_result stale_sid={} current_sid={}",
                                sid, state.current_search_id
                            ));
                            continue;
                        }
                        if let Some(h) = state.worker.take() {
                            let _ = h.join();
                        }
                        state.searching = false;
                        state.stop_flag = None;
                        state.ponder_hit_flag = None;
                        state.result_rx = None;

                        // Remember ponder status and clear it for next cycle
                        let was_ponder = state.current_is_ponder;
                        state.current_is_ponder = false;

                        if state.stoch_suppress_result {
                            // Suppress emission for stochastic ponder cancel
                            state.stoch_suppress_result = false;
                            // Start normal search if pending
                            if state.pending_research_after_ponderhit {
                                if let Some(mut gp2) = state.last_go_params.clone() {
                                    gp2.ponder = false;
                                    // Prepare new flags
                                    let stop_flag = Arc::new(AtomicBool::new(false));
                                    let ponder_flag: Option<Arc<AtomicBool>> = None;
                                    let limits = limits_from_go(
                                        &gp2,
                                        state.position.side_to_move,
                                        &state.opts,
                                        ponder_flag.clone(),
                                        stop_flag.clone(),
                                    );
                                    let (tx2, rx2) = mpsc::channel();
                                    let engine = Arc::clone(&state.engine);
                                    let pos2 = state.position.clone();
                                    let info_enabled = true;
                                    state.current_search_id =
                                        state.current_search_id.wrapping_add(1);
                                    let sid2 = state.current_search_id;
                                    let handle = thread::spawn(move || {
                                        run_search_thread(
                                            engine,
                                            pos2,
                                            limits,
                                            info_enabled,
                                            tx2,
                                            sid2,
                                        )
                                    });
                                    state.searching = true;
                                    state.stop_flag = Some(stop_flag);
                                    state.ponder_hit_flag = None;
                                    state.worker = Some(handle);
                                    state.result_rx = Some(rx2);
                                    state.current_is_stochastic_ponder = false;
                                    state.pending_research_after_ponderhit = false;
                                } else {
                                    state.pending_research_after_ponderhit = false;
                                }
                            }
                        } else if was_ponder {
                            // Ponder完了結果は送出しない（USI仕様）。GUIのponderhit/stopに従う。
                        } else {
                            // Normal finalize (centralized)
                            // まずステール結果のガード（rootハッシュ比較）
                            let stale = state
                                .current_root_hash
                                .map(|h| h != state.position.zobrist_hash())
                                .unwrap_or(false);

                            // Diagnostics: finalize snapshot
                            let stop_reason = result
                                .stop_info
                                .as_ref()
                                .map(|si| format!("{:?}", si.reason))
                                .unwrap_or_else(|| "Unknown".to_string());
                            let (soft_ms, hard_ms) = result
                                .stop_info
                                .as_ref()
                                .map(|si| (si.soft_limit_ms, si.hard_limit_ms))
                                .unwrap_or((0, 0));
                            info_string(format!(
                                "finalize root={} gui={} stale={} core_best={} stop_reason={} soft_ms={} hard_ms={}",
                                fmt_hash(state.current_root_hash.unwrap_or(0)),
                                fmt_hash(state.position.zobrist_hash()),
                                if stale { 1 } else { 0 },
                                result
                                    .best_move
                                    .map(|m| move_to_usi(&m))
                                    .unwrap_or_else(|| "-".to_string()),
                                stop_reason,
                                soft_ms,
                                hard_ms
                            ));

                            finalize_and_send(&mut state, "finalize", Some(&result), stale);
                            state.current_time_control = None;
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => {}
                    Err(mpsc::TryRecvError::Disconnected) => {
                        if let Some(h) = state.worker.take() {
                            let _ = h.join();
                        }
                        state.searching = false;
                        state.stop_flag = None;
                        state.ponder_hit_flag = None;
                        state.result_rx = None;
                        state.current_time_control = None;
                        usi_println("bestmove resign");
                    }
                }
            }
        }

        // Process one command if available
        if let Ok(line) = line_rx.try_recv() {
            let cmd = line.trim();
            if cmd.is_empty() {
                continue;
            }

            if cmd == "usi" {
                send_id_and_options(&state.opts);
                usi_println("usiok");
                continue;
            }

            if cmd == "isready" {
                state.apply_options_to_engine();
                usi_println("readyok");
                continue;
            }

            if let Some(body) = cmd.strip_prefix("setoption ") {
                // Robust parse: setoption name <n> [value <v>]
                let mut name: Option<String> = None;
                let mut value: Option<String> = None;
                if let Some(name_pos) = body.find("name") {
                    let after_name = body[name_pos + 4..].trim_start();
                    if let Some(val_pos) = after_name.find(" value ") {
                        name = Some(after_name[..val_pos].trim().to_string());
                        value = Some(after_name[val_pos + 7..].trim().to_string());
                    } else {
                        name = Some(after_name.trim().to_string());
                    }
                }

                if let Some(n) = name {
                    match n.as_str() {
                        "SIMDMaxLevel" => {
                            if let Some(v) = value {
                                let vnorm = v.trim().to_ascii_lowercase();
                                let env_val = match vnorm.as_str() {
                                    "auto" => None,
                                    "scalar" => Some("scalar"),
                                    "sse2" => Some("sse2"),
                                    "avx" => Some("avx"),
                                    "avx512" | "avx512f" => Some("avx512f"),
                                    _ => None,
                                };
                                state.opts.simd_max_level = env_val.map(|s| s.to_string());
                                if let Some(ref e) = state.opts.simd_max_level {
                                    std::env::set_var("SHOGI_SIMD_MAX", e);
                                    info_string(format!("simd_clamp={}", e));
                                } else {
                                    std::env::remove_var("SHOGI_SIMD_MAX");
                                    info_string("simd_clamp=auto");
                                }
                                // 注意: 既にSIMDカーネルが初期化済みの場合は反映されない
                                info_string("simd_clamp_note=may_not_apply_after_init");
                            }
                        }
                        "NNUE_Simd" => {
                            if let Some(v) = value {
                                let vnorm = v.trim().to_ascii_lowercase();
                                let env_val = match vnorm.as_str() {
                                    "auto" => None,
                                    "scalar" => Some("scalar"),
                                    "sse41" | "sse4.1" => Some("sse41"),
                                    "avx2" => Some("avx2"),
                                    _ => None,
                                };
                                state.opts.nnue_simd = env_val.map(|s| s.to_string());
                                if let Some(ref e) = state.opts.nnue_simd {
                                    std::env::set_var("SHOGI_NNUE_SIMD", e);
                                    info_string(format!("nnue_simd_clamp={}", e));
                                } else {
                                    std::env::remove_var("SHOGI_NNUE_SIMD");
                                    info_string("nnue_simd_clamp=auto");
                                }
                                info_string("nnue_simd_note=may_not_apply_after_init");
                            }
                        }
                        "USI_Hash" => {
                            if let Some(v) = value {
                                if let Ok(mb) = v.parse::<usize>() {
                                    state.opts.hash_mb = mb;
                                }
                            }
                        }
                        "Threads" => {
                            if let Some(v) = value {
                                if let Ok(t) = v.parse::<usize>() {
                                    state.opts.threads = t;
                                }
                            }
                        }
                        "USI_Ponder" => {
                            if let Some(v) = value {
                                let v = v.to_lowercase();
                                state.opts.ponder = matches!(v.as_str(), "true" | "1" | "on");
                            }
                        }
                        "MultiPV" => {
                            if let Some(v) = value {
                                if let Ok(k) = v.parse::<u8>() {
                                    state.opts.multipv = k.clamp(1, 20);
                                    // Persist immediately so it survives ClearHash/TT resize
                                    if let Ok(mut eng) = state.engine.lock() {
                                        eng.set_multipv_persistent(state.opts.multipv);
                                    }
                                }
                            }
                        }
                        "MinThinkMs" => {
                            if let Some(v) = value {
                                if let Ok(ms) = v.parse::<u64>() {
                                    state.opts.min_think_ms = ms;
                                }
                            }
                        }
                        // Engine/model
                        "EngineType" => {
                            if let Some(val) = value {
                                let et = match val.trim() {
                                    "Material" => EngineType::Material,
                                    "Enhanced" => EngineType::Enhanced,
                                    "Nnue" => EngineType::Nnue,
                                    "EnhancedNnue" => EngineType::EnhancedNnue,
                                    _ => EngineType::Material,
                                };
                                state.opts.engine_type = et;
                            }
                        }
                        "EvalFile" => {
                            if let Some(v) = value {
                                state.opts.eval_file = Some(v.to_string());
                            }
                        }
                        "ClearHash" => {
                            if let Ok(mut eng) = state.engine.lock() {
                                // Re-apply persistent MultiPV before clearing/recreating
                                eng.set_multipv_persistent(state.opts.multipv);
                                eng.clear_hash();
                            }
                        }
                        // Legacy policy options (map to TimeParameters)
                        "OverheadMs" => {
                            if let Some(v) = value {
                                if let Ok(ms) = v.parse::<u64>() {
                                    state.opts.overhead_ms = ms;
                                }
                            }
                        }
                        // Map ByoyomiOverheadMs to internal NetworkDelay2
                        "ByoyomiOverheadMs" => {
                            if let Some(v) = value {
                                if let Ok(ms) = v.parse::<u64>() {
                                    state.opts.network_delay2_ms = ms;
                                }
                            }
                        }
                        "ByoyomiSafetyMs" => {
                            if let Some(v) = value {
                                if let Ok(ms) = v.parse::<u64>() {
                                    state.opts.byoyomi_safety_ms = ms;
                                }
                            }
                        }
                        "ByoyomiPeriods" => {
                            if let Some(v) = value {
                                if let Ok(p) = v.parse::<u32>() {
                                    state.opts.byoyomi_periods = p.clamp(1, 10);
                                }
                            }
                        }
                        "ByoyomiEarlyFinishRatio" => {
                            if let Some(v) = value {
                                if let Ok(r) = v.parse::<u8>() {
                                    state.opts.byoyomi_early_finish_ratio = r.clamp(50, 95);
                                }
                            }
                        }
                        "PVStabilityBase" => {
                            if let Some(v) = value {
                                if let Ok(ms) = v.parse::<u64>() {
                                    state.opts.pv_stability_base = ms.clamp(10, 200);
                                }
                            }
                        }
                        "PVStabilitySlope" => {
                            if let Some(v) = value {
                                if let Ok(ms) = v.parse::<u64>() {
                                    state.opts.pv_stability_slope = ms.clamp(0, 20);
                                }
                            }
                        }
                        "SlowMover" => {
                            if let Some(v) = value {
                                if let Ok(p) = v.parse::<u8>() {
                                    state.opts.slow_mover_pct = p.clamp(50, 200);
                                }
                            }
                        }
                        "MaxTimeRatioPct" => {
                            if let Some(v) = value {
                                if let Ok(p) = v.parse::<u32>() {
                                    state.opts.max_time_ratio_pct = p.clamp(100, 800);
                                }
                            }
                        }
                        "MoveHorizonTriggerMs" => {
                            if let Some(v) = value {
                                if let Ok(ms) = v.parse::<u64>() {
                                    state.opts.move_horizon_trigger_ms = ms;
                                }
                            }
                        }
                        "MoveHorizonMinMoves" => {
                            if let Some(v) = value {
                                if let Ok(m) = v.parse::<u32>() {
                                    state.opts.move_horizon_min_moves = m;
                                }
                            }
                        }
                        // Feature toggles
                        "Stochastic_Ponder" => {
                            if let Some(v) = value {
                                let v = v.to_lowercase();
                                state.opts.stochastic_ponder =
                                    matches!(v.as_str(), "true" | "1" | "on");
                            }
                        }
                        "ForceTerminateOnHardDeadline" => {
                            if let Some(v) = value {
                                let v = v.to_lowercase();
                                state.opts.force_terminate_on_hard_deadline =
                                    matches!(v.as_str(), "true" | "1" | "on");
                            }
                        }
                        "MateEarlyStop" => {
                            if let Some(v) = value {
                                let v = v.to_lowercase();
                                state.opts.mate_early_stop =
                                    matches!(v.as_str(), "true" | "1" | "on");
                                engine_core::search::config::set_mate_early_stop_enabled(
                                    state.opts.mate_early_stop,
                                );
                            }
                        }
                        "StopWaitMs" => {
                            if let Some(v) = value {
                                if let Ok(ms) = v.parse::<u64>() {
                                    state.opts.stop_wait_ms = ms.min(2000);
                                }
                            }
                        }
                        "GameoverSendsBestmove" => {
                            if let Some(v) = value {
                                let v = v.to_lowercase();
                                state.opts.gameover_sends_bestmove =
                                    matches!(v.as_str(), "true" | "1" | "on");
                            }
                        }
                        "FailSafeGuard" => {
                            if let Some(v) = value {
                                let v = v.to_lowercase();
                                state.opts.fail_safe_guard =
                                    matches!(v.as_str(), "true" | "1" | "on");
                            }
                        }
                        _ => {}
                    }
                }
                continue;
            }

            if cmd.starts_with("position ") {
                parse_position(cmd, &mut state)?;
                continue;
            }

            if cmd == "usinewgame" {
                // No-op for now; position resets on "position" command
                continue;
            }

            if cmd == "quit" {
                // Cleanup: stop current worker and join
                if let Some(flag) = &state.stop_flag {
                    flag.store(true, Ordering::SeqCst);
                }
                if let Some(h) = state.worker.take() {
                    let _ = h.join();
                }
                // Shutdown reaper: drop sender and join reaper thread
                state.reaper_tx.take();
                if let Some(h) = state.reaper_handle.take() {
                    let _ = h.join();
                }
                break;
            }

            if cmd.starts_with("go ") || cmd == "go" {
                if state.searching {
                    // Reject new go while searching
                    info!("Ignoring go while searching");
                    continue;
                }

                // Prepare flags
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
                // Guard: if USI_Ponder is disabled, force gp.ponder=false to avoid silent ponder state
                if gp.ponder && !state.opts.ponder {
                    info_string("ponder_disabled_guard=1");
                    gp.ponder = false;
                }
                // Save last go params
                state.last_go_params = Some(gp.clone());
                // Stochastic Ponder: if go ponder && enabled → search from 1手前
                let mut search_position = state.position.clone();
                let current_is_stochastic_ponder = gp.ponder && state.opts.stochastic_ponder;
                if current_is_stochastic_ponder {
                    if !state.pos_moves.is_empty() {
                        // previous position by trimming last move
                        let prev_moves =
                            &state.pos_moves[..state.pos_moves.len().saturating_sub(1)];
                        if let Ok(prev) = engine_core::usi::create_position(
                            state.pos_from_startpos,
                            state.pos_sfen.as_deref(),
                            prev_moves,
                        ) {
                            search_position = prev;
                            info!("Stochastic Ponder: using previous position for pondering");
                        } else {
                            info!("Stochastic Ponder: failed to build previous position; using current position");
                        }
                    } else {
                        info!("Stochastic Ponder: no previous move; using current position");
                    }
                }
                // 入り口判定（通常goのみ）: 合法手0→投了 / 合法手1→即指し
                if !gp.ponder {
                    let mg = engine_core::movegen::MoveGenerator::new();
                    if let Ok(list) = mg.generate_all(&state.position) {
                        let slice = list.as_slice();
                        if slice.is_empty() {
                            usi_println("bestmove resign");
                            state.bestmove_emitted = true;
                            continue;
                        } else if slice.len() == 1 {
                            usi_println(&format!("bestmove {}", move_to_usi(&slice[0])));
                            state.bestmove_emitted = true;
                            continue;
                        }
                    }
                }
                let limits = limits_from_go(
                    &gp,
                    search_position.side_to_move,
                    &state.opts,
                    ponder_flag.clone(),
                    stop_flag.clone(),
                );
                // Keep inner time control for stop/gameover policy
                let mut tc_for_stop = limits.time_control.clone();
                if let TimeControl::Ponder(inner) = tc_for_stop {
                    tc_for_stop = *inner;
                }
                state.current_time_control = Some(tc_for_stop);

                // Spawn worker
                let (tx, rx) = mpsc::channel();
                let engine = Arc::clone(&state.engine);
                let pos = search_position.clone();
                let info_enabled = true;
                // Bump search id and pass to worker
                state.current_search_id = state.current_search_id.wrapping_add(1);
                let sid = state.current_search_id;
                let handle = thread::spawn(move || {
                    run_search_thread(engine, pos, limits, info_enabled, tx, sid)
                });

                state.searching = true;
                state.stop_flag = Some(stop_flag);
                state.ponder_hit_flag = ponder_flag;
                state.worker = Some(handle);
                state.result_rx = Some(rx);
                state.current_is_stochastic_ponder = current_is_stochastic_ponder;
                state.current_is_ponder = gp.ponder;
                state.current_root_hash = Some(search_position.zobrist_hash());
                state.bestmove_emitted = false;
                // Diagnostics: mark search start and show hashes
                info_string(format!(
                    "search_started root={} gui={} ponder={} stoch={}",
                    fmt_hash(search_position.zobrist_hash()),
                    fmt_hash(state.position.zobrist_hash()),
                    gp.ponder,
                    state.current_is_stochastic_ponder
                ));
                continue;
            }

            if cmd == "stop" {
                if let (true, Some(flag)) = (state.searching, &state.stop_flag) {
                    flag.store(true, Ordering::SeqCst);
                    info_string("stop_requested");
                    // Try to get result promptly using a small-slice loop; otherwise finalize immediately
                    if let Some(rx) = state.result_rx.take() {
                        // Policy: stopは原則即時化。特に以下は0ms扱い:
                        //  - FixedTime / Infinite
                        //  - Byoyomiで main_time_ms == 0（純秒読み）
                        //  - Byoyomiで main_time_ms <= NetworkDelay2（実質純秒読み扱い）
                        let mut wait_ms = state.opts.stop_wait_ms;
                        if let Some(tc) = &state.current_time_control {
                            match tc {
                                TimeControl::FixedTime { .. } | TimeControl::Infinite => {
                                    wait_ms = 0;
                                    info_string("stop_fast_finalize=fixed_or_infinite");
                                }
                                TimeControl::Byoyomi { main_time_ms, .. } => {
                                    if *main_time_ms == 0
                                        || *main_time_ms <= state.opts.network_delay2_ms
                                    {
                                        wait_ms = 0;
                                        info_string("stop_fast_finalize=byoyomi");
                                    }
                                }
                                _ => {}
                            }
                        }

                        let deadline = Instant::now() + Duration::from_millis(wait_ms);
                        let mut finalized = false;
                        while Instant::now() < deadline && !finalized {
                            let now = Instant::now();
                            let remain = if deadline > now {
                                deadline - now
                            } else {
                                Duration::from_millis(0)
                            };
                            let slice = if remain > Duration::from_millis(20) {
                                Duration::from_millis(20)
                            } else {
                                remain
                            };
                            match rx.recv_timeout(slice) {
                                Ok((sid, result)) => {
                                    if sid != state.current_search_id {
                                        info_string(format!(
                                            "ignore_result stale_sid={} current_sid={}",
                                            sid, state.current_search_id
                                        ));
                                        continue; // keep waiting within remaining time
                                    }
                                    if let Some(h) = state.worker.take() {
                                        let _ = h.join();
                                    }
                                    state.searching = false;
                                    state.stop_flag = None;
                                    state.ponder_hit_flag = None;

                                    // Note: Even if the current search is pondering, emit bestmove on explicit stop
                                    // for compatibility with GUIs that expect a bestmove response after stop.
                                    // Finalize centrally using choose_final_bestmove
                                    let stale = state
                                        .current_root_hash
                                        .map(|h| h != state.position.zobrist_hash())
                                        .unwrap_or(false);
                                    finalize_and_send(
                                        &mut state,
                                        "stop_finalize",
                                        Some(&result),
                                        stale,
                                    );
                                    // Reset ponder state after explicit stop
                                    state.current_is_ponder = false;
                                    state.current_root_hash = None;
                                    state.current_time_control = None;
                                    finalized = true;
                                }
                                Err(mpsc::RecvTimeoutError::Timeout) => { /* continue */ }
                                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                            }
                        }
                        if !finalized {
                            // One-shot try_recv to avoid dropping an already-ready result
                            match rx.try_recv() {
                                Ok((sid, result)) => {
                                    if sid == state.current_search_id {
                                        if let Some(h) = state.worker.take() {
                                            let _ = h.join();
                                        }
                                        state.searching = false;
                                        state.stop_flag = None;
                                        state.ponder_hit_flag = None;

                                        let stale = state
                                            .current_root_hash
                                            .map(|h| h != state.position.zobrist_hash())
                                            .unwrap_or(false);
                                        finalize_and_send(
                                            &mut state,
                                            "stop_finalize",
                                            Some(&result),
                                            stale,
                                        );
                                        state.current_is_ponder = false;
                                        state.current_root_hash = None;
                                        state.current_time_control = None;
                                        continue;
                                    } else {
                                        info_string(format!(
                                            "ignore_result stale_sid={} current_sid={}",
                                            sid, state.current_search_id
                                        ));
                                        // fall through to detach
                                    }
                                }
                                Err(_) => { /* fall through to detach */ }
                            }
                            // Timeout waiting for result; detach worker and finalize immediately
                            if let Some(h) = state.worker.take() {
                                if let Some(tx) = &state.reaper_tx {
                                    // Increment queue length and send with soft bound logging
                                    let q =
                                        state.reaper_queue_len.fetch_add(1, Ordering::SeqCst) + 1;
                                    let _ = tx.send(h);
                                    const REAPER_QUEUE_SOFT_MAX: usize = 128;
                                    if q > REAPER_QUEUE_SOFT_MAX {
                                        info_string(format!("reaper_queue_len_high len={}", q));
                                    } else {
                                        info_string(format!("reaper_detach queued len={}", q));
                                    }
                                }
                            }
                            state.searching = false;
                            state.stop_flag = None;
                            state.ponder_hit_flag = None;

                            finalize_and_send_fast(&mut state, "stop_timeout_finalize");
                            // Reset ponder state after explicit stop
                            state.current_is_ponder = false;
                            state.current_root_hash = None;
                            state.current_time_control = None;
                        }
                    }
                }
                continue;
            }

            if cmd == "ponderhit" {
                if state.opts.stochastic_ponder
                    && state.searching
                    && state.current_is_stochastic_ponder
                {
                    // Stop current pondering and prepare to start normal search without emitting bestmove
                    state.stoch_suppress_result = true;
                    state.pending_research_after_ponderhit = true;
                    if let Some(flag) = &state.stop_flag {
                        flag.store(true, Ordering::SeqCst);
                    }
                    // Don't start immediately; wait for worker to finish to avoid overlap
                } else {
                    // Normal ponderhit: notify core via flag to convert to normal search
                    if let Some(flag) = &state.ponder_hit_flag {
                        flag.store(true, Ordering::SeqCst);
                    }
                    // 通常探索へ移行したので、結果送出を許可
                    state.current_is_ponder = false;
                }
                continue;
            }

            if cmd.starts_with("gameover ") {
                if state.opts.gameover_sends_bestmove {
                    // Duplicate guard: if we already emitted bestmove for this go and are not searching, skip
                    if !state.searching && state.bestmove_emitted {
                        if let Some(flag) = &state.stop_flag {
                            flag.store(true, Ordering::SeqCst);
                        }
                        if let Some(h) = state.worker.take() {
                            let _ = h.join();
                        }
                        state.searching = false;
                        state.stop_flag = None;
                        state.ponder_hit_flag = None;
                        state.current_time_control = None;
                        continue;
                    }
                    // stopと同等に扱い、bestmoveを送る
                    if let Some(flag) = &state.stop_flag {
                        flag.store(true, Ordering::SeqCst);
                    }
                    if state.searching {
                        if let Some(rx) = state.result_rx.take() {
                            // stopと同じ待機ポリシー
                            let mut wait_ms = state.opts.stop_wait_ms;
                            if let Some(tc) = &state.current_time_control {
                                match tc {
                                    TimeControl::FixedTime { .. } | TimeControl::Infinite => {
                                        wait_ms = 0;
                                        info_string("gameover_fast_finalize=fixed_or_infinite");
                                    }
                                    TimeControl::Byoyomi { main_time_ms, .. } => {
                                        if *main_time_ms == 0
                                            || *main_time_ms <= state.opts.network_delay2_ms
                                        {
                                            wait_ms = 0;
                                            info_string("gameover_fast_finalize=byoyomi");
                                        }
                                    }
                                    _ => {}
                                }
                            }

                            let deadline = Instant::now() + Duration::from_millis(wait_ms);
                            let mut finalized = false;
                            while Instant::now() < deadline && !finalized {
                                let remain = {
                                    let now = Instant::now();
                                    if deadline > now {
                                        deadline - now
                                    } else {
                                        Duration::from_millis(0)
                                    }
                                };
                                let slice = if remain > Duration::from_millis(20) {
                                    Duration::from_millis(20)
                                } else {
                                    remain
                                };
                                match rx.recv_timeout(slice) {
                                    Ok((sid, result)) => {
                                        if sid != state.current_search_id {
                                            info_string(format!(
                                                "ignore_result stale_sid={} current_sid={}",
                                                sid, state.current_search_id
                                            ));
                                            continue;
                                        }
                                        if let Some(h) = state.worker.take() {
                                            let _ = h.join();
                                        }
                                        state.searching = false;
                                        state.stop_flag = None;
                                        state.ponder_hit_flag = None;
                                        // Finalize centrally
                                        let stale = state
                                            .current_root_hash
                                            .map(|h| h != state.position.zobrist_hash())
                                            .unwrap_or(false);
                                        finalize_and_send(
                                            &mut state,
                                            "gameover_finalize",
                                            Some(&result),
                                            stale,
                                        );
                                        state.current_is_ponder = false;
                                        state.current_root_hash = None;
                                        state.current_time_control = None;
                                        finalized = true;
                                    }
                                    Err(mpsc::RecvTimeoutError::Timeout) => { /* continue */ }
                                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                                }
                            }
                            if !finalized {
                                // One-shot try_recv to avoid dropping an already-ready result
                                match rx.try_recv() {
                                    Ok((sid, result)) => {
                                        if sid == state.current_search_id {
                                            if let Some(h) = state.worker.take() {
                                                let _ = h.join();
                                            }
                                            state.searching = false;
                                            state.stop_flag = None;
                                            state.ponder_hit_flag = None;
                                            let stale = state
                                                .current_root_hash
                                                .map(|h| h != state.position.zobrist_hash())
                                                .unwrap_or(false);
                                            finalize_and_send(
                                                &mut state,
                                                "gameover_finalize",
                                                Some(&result),
                                                stale,
                                            );
                                            state.current_is_ponder = false;
                                            state.current_root_hash = None;
                                            state.current_time_control = None;
                                            continue;
                                        } else {
                                            info_string(format!(
                                                "ignore_result stale_sid={} current_sid={}",
                                                sid, state.current_search_id
                                            ));
                                            // fall through to detach
                                        }
                                    }
                                    Err(_) => { /* fall through to detach */ }
                                }
                                if let Some(h) = state.worker.take() {
                                    if let Some(tx) = &state.reaper_tx {
                                        let q =
                                            state.reaper_queue_len.fetch_add(1, Ordering::SeqCst)
                                                + 1;
                                        let _ = tx.send(h);
                                        const REAPER_QUEUE_SOFT_MAX: usize = 128;
                                        if q > REAPER_QUEUE_SOFT_MAX {
                                            info_string(format!("reaper_queue_len_high len={}", q));
                                        } else {
                                            info_string(format!("reaper_detach queued len={}", q));
                                        }
                                    }
                                }
                                state.searching = false;
                                state.stop_flag = None;
                                state.ponder_hit_flag = None;
                                finalize_and_send_fast(&mut state, "gameover_timeout_finalize");
                                state.current_is_ponder = false;
                                state.current_root_hash = None;
                                state.current_time_control = None;
                            }
                        } else {
                            // No rx: そのまま即時finalize（最悪resignになる）
                            state.searching = false;
                            state.stop_flag = None;
                            state.ponder_hit_flag = None;
                            finalize_and_send_fast(&mut state, "gameover_immediate_finalize");
                            state.current_is_ponder = false;
                            state.current_root_hash = None;
                            state.current_time_control = None;
                        }
                    } else {
                        // 検索が開始前/既に停止済みでも、要求に従いbestmoveを返す（合法手フォールバックなど）。
                        state.searching = false;
                        state.stop_flag = None;
                        state.ponder_hit_flag = None;
                        finalize_and_send_fast(&mut state, "gameover_immediate_finalize");
                        state.current_is_ponder = false;
                        state.current_root_hash = None;
                        state.current_time_control = None;
                    }
                } else {
                    // 従来どおり: bestmoveを出さずサイレント停止
                    if let Some(flag) = &state.stop_flag {
                        flag.store(true, Ordering::SeqCst);
                    }
                    if let Some(h) = state.worker.take() {
                        let _ = h.join();
                    }
                    state.searching = false;
                    state.stop_flag = None;
                    state.ponder_hit_flag = None;
                    state.current_time_control = None;
                }
                continue;
            }

            // Unknown command: ignore
            info!("Ignoring command: {}", cmd);
        } else {
            // No command; small sleep to avoid busy loop
            thread::sleep(Duration::from_millis(2));
        }
    }

    Ok(())
}
