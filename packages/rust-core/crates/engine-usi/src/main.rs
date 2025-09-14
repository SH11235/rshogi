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
            stop_wait_ms: 200,
            multipv: 1,
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
            reaper_tx: None,
            reaper_handle: None,
            reaper_queue_len: Arc::new(AtomicUsize::new(0)),
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
        state.current_root_hash = None;
        return;
    }
    // Build committed when applicable
    let committed = if let Some(res) = result {
        if !stale {
            Some(engine_core::search::CommittedIteration {
                depth: res.stats.depth,
                seldepth: res.stats.seldepth,
                score: res.score,
                pv: res.stats.pv.clone(),
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

    // Emit MultiPV info lines (if available and not stale)
    if let Some(res) = result {
        if !stale {
            if let Some(ref lines) = res.lines {
                if !lines.is_empty() {
                    // Obtain hashfull once (permille) and compute aggregate nps
                    let hf_permille = {
                        let eng = state.engine.lock().unwrap();
                        eng.tt_hashfull_permille()
                    };
                    let nps_agg: u128 = if res.stats.elapsed.as_millis() > 0 {
                        (res.stats.nodes as u128).saturating_mul(1000)
                            / res.stats.elapsed.as_millis()
                    } else {
                        0
                    };
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
                        let view = score_view_from_internal(ln.score_internal);
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
                }
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
            // score: normalize to mate or cp with proper bound tag placement
            let view = score_view_from_internal(score);
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

    // Do the search
    let result = {
        let mut eng = engine.lock().unwrap();
        eng.search(&mut position, limits)
    };
    let _ = tx.send((search_id, result));
}

fn main() -> Result<()> {
    env_logger::init();
    let stdin = io::stdin();
    let mut state = EngineState::new();

    // 起動時に core のビルド時 feature を一度だけ出力（デバッグ/再現に有用）
    let feat = engine_core::evaluation::nnue::enabled_features_str();
    info_string(format!("core_features={}", feat));

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

            if cmd.starts_with("setoption ") {
                // setoption name <n> value <v>
                let mut name: Option<String> = None;
                let mut value: Option<String> = None;
                let mut it = cmd.split_whitespace().skip(1);
                while let Some(tok) = it.next() {
                    match tok {
                        "name" => {
                            let mut n = String::new();
                            for t in it.by_ref() {
                                if t == "value" {
                                    break;
                                } else {
                                    if !n.is_empty() {
                                        n.push(' ');
                                    }
                                    n.push_str(t);
                                }
                            }
                            name = Some(n);
                        }
                        "value" => {
                            let v = it.clone().collect::<Vec<_>>().join(" ");
                            value = Some(v);
                            break;
                        }
                        _ => {}
                    }
                }

                if let Some(n) = name {
                    match n.as_str() {
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
                            continue;
                        } else if slice.len() == 1 {
                            usi_println(&format!("bestmove {}", move_to_usi(&slice[0])));
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
                        let deadline =
                            Instant::now() + Duration::from_millis(state.opts.stop_wait_ms);
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
                                    finalized = true;
                                }
                                Err(mpsc::RecvTimeoutError::Timeout) => { /* continue */ }
                                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                            }
                        }
                        if !finalized {
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

                            // Emit bestmove even if this was a ponder search, to avoid GUI timeouts after stop.
                            let stale = state
                                .current_root_hash
                                .map(|h| h != state.position.zobrist_hash())
                                .unwrap_or(false);
                            finalize_and_send(&mut state, "stop_timeout_finalize", None, stale);
                            // Reset ponder state after explicit stop
                            state.current_is_ponder = false;
                            state.current_root_hash = None;
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
                // Treat as stop, but do not emit bestmove (silent cleanup)
                if let Some(flag) = &state.stop_flag {
                    flag.store(true, Ordering::SeqCst);
                }
                if let Some(h) = state.worker.take() {
                    let _ = h.join();
                }
                state.searching = false;
                state.stop_flag = None;
                state.ponder_hit_flag = None;
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
