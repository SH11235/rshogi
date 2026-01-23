use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use engine_core::movegen::{generate_legal_with_pass, MoveList};
use engine_core::position::{Position, SFEN_HIRATE};
use engine_core::search::{
    LimitsType, Search, SearchInfo, SearchResult, SkillOptions, TimeOptions,
    DEFAULT_MAX_MOVES_TO_DRAW,
};
use engine_core::types::json::{BoardStateJson, ReplayResultJson};
use engine_core::types::{Color, Move, Value};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager, State, Window};
use tokio::io::AsyncReadExt;

const ENGINE_EVENT: &str = "engine://event";
const SEARCH_STACK_SIZE: usize = 64 * 1024 * 1024;

#[derive(Serialize, Clone)]
#[serde(tag = "type")]
enum EngineEvent {
    #[serde(rename = "info")]
    Info {
        depth: Option<u32>,
        seldepth: Option<u32>,
        nodes: Option<u64>,
        nps: Option<u64>,
        #[serde(rename = "timeMs")]
        time_ms: Option<u64>,
        #[serde(rename = "scoreCp")]
        score_cp: Option<i32>,
        #[serde(rename = "scoreMate")]
        score_mate: Option<i32>,
        multipv: Option<usize>,
        pv: Option<Vec<String>>,
        hashfull: Option<u32>,
    },
    #[serde(rename = "bestmove")]
    BestMove {
        #[serde(rename = "move")]
        mv: String,
        ponder: Option<String>,
    },
    #[serde(rename = "error")]
    Error { message: String },
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
enum EngineStopMode {
    Cooperative,
    #[default]
    Terminate,
}

#[derive(Clone, Debug)]
struct EngineOptions {
    tt_size_mb: usize,
    multi_pv: usize,
    num_threads: usize,
    time_options: TimeOptions,
    skill_options: SkillOptions,
    max_moves_to_draw: i32,
    stop_mode: EngineStopMode,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            tt_size_mb: 256,
            multi_pv: 1,
            num_threads: 1,
            time_options: TimeOptions::default(),
            skill_options: SkillOptions::default(),
            max_moves_to_draw: DEFAULT_MAX_MOVES_TO_DRAW,
            stop_mode: EngineStopMode::default(),
        }
    }
}

impl EngineOptions {
    fn apply_to_search(&self, search: &mut Search) {
        search.resize_tt(self.tt_size_mb);
        search.set_num_threads(self.num_threads);
        search.set_time_options(self.time_options);
        search.set_skill_options(self.skill_options);
        search.set_max_moves_to_draw(self.max_moves_to_draw);
    }

    fn update_from_init(&mut self, opts: &InitOptions) {
        if let Some(tt) = opts.tt_size_mb {
            self.tt_size_mb = tt.max(1);
        }
        if let Some(multi) = opts.multi_pv {
            self.multi_pv = multi.max(1);
        }
        if let Some(stop_mode) = opts.stop_mode {
            self.stop_mode = stop_mode;
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InitOptions {
    stop_mode: Option<EngineStopMode>,
    tt_size_mb: Option<usize>,
    multi_pv: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchLimitsInput {
    max_depth: Option<i32>,
    nodes: Option<u64>,
    byoyomi_ms: Option<i64>,
    movetime_ms: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchParamsInput {
    limits: Option<SearchLimitsInput>,
    ponder: Option<bool>,
}

struct SearchTaskResult {
    search: Search,
}

struct ActiveSearch {
    handle: thread::JoinHandle<SearchTaskResult>,
    stop_flag: Arc<AtomicBool>,
    _ponderhit_flag: Arc<AtomicBool>,
}

struct EngineState {
    inner: Mutex<EngineStateInner>,
}

struct EngineStateInner {
    options: EngineOptions,
    position: Position,
    search: Option<Search>,
    active_search: Option<ActiveSearch>,
}

impl EngineStateInner {
    fn new() -> Self {
        let options = EngineOptions::default();
        let mut search = Search::new(options.tt_size_mb);
        options.apply_to_search(&mut search);

        Self {
            options,
            position: Position::new(),
            search: Some(search),
            active_search: None,
        }
    }

    fn create_search(&self) -> Search {
        let mut search = Search::new(self.options.tt_size_mb);
        self.options.apply_to_search(&mut search);
        search
    }

    fn reclaim_finished(&mut self) {
        if let Some(active) = self.active_search.as_ref() {
            if active.handle.is_finished() {
                let active = self.active_search.take().unwrap();
                let result = active.handle.join();
                self.restore_search(result);
            }
        }
    }

    fn restore_search(&mut self, result: thread::Result<SearchTaskResult>) {
        let mut search = match result {
            Ok(task) => task.search,
            Err(err) => {
                eprintln!("Engine search thread panicked: {err:?}");
                self.create_search()
            }
        };
        self.options.apply_to_search(&mut search);
        self.search = Some(search);
    }

    fn stop_active_search(&mut self) -> Option<ActiveSearch> {
        if let Some(active) = self.active_search.take() {
            active.stop_flag.store(true, Ordering::SeqCst);
            Some(active)
        } else {
            None
        }
    }
}

impl EngineState {
    fn new() -> Self {
        Self {
            inner: Mutex::new(EngineStateInner::new()),
        }
    }
}

impl Default for EngineState {
    fn default() -> Self {
        Self::new()
    }
}

struct EngineEventEmitter {
    window: Window,
}

impl EngineEventEmitter {
    fn new(window: Window) -> Self {
        Self { window }
    }

    fn emit_info(&mut self, info: &SearchInfo) {
        emit_event(&self.window, info_event(info));
    }

    fn emit_bestmove(&self, result: &SearchResult) {
        emit_event(&self.window, bestmove_event(result));
    }
}

fn emit_event(window: &Window, event: EngineEvent) {
    if let Err(err) = window.emit(ENGINE_EVENT, event) {
        eprintln!("Failed to emit engine event: {err:?}");
    }
}

fn info_event(info: &SearchInfo) -> EngineEvent {
    let (score_cp, score_mate) =
        if info.score.is_mate_score() && info.score.raw().abs() < Value::INFINITE.raw() {
            let mate = info.score.mate_ply();
            let signed = if info.score.is_loss() { -mate } else { mate };
            (None, Some(signed))
        } else {
            (Some(info.score.raw()), None)
        };

    let pv = if info.pv.is_empty() {
        None
    } else {
        Some(info.pv.iter().map(|m| m.to_usi()).collect())
    };

    EngineEvent::Info {
        depth: Some(info.depth.max(0) as u32),
        seldepth: Some(info.sel_depth.max(0) as u32),
        nodes: Some(info.nodes),
        nps: Some(info.nps),
        time_ms: Some(info.time_ms),
        multipv: Some(info.multi_pv),
        pv,
        hashfull: Some(info.hashfull),
        score_cp,
        score_mate,
    }
}

fn bestmove_event(result: &SearchResult) -> EngineEvent {
    let mv = if result.best_move == Move::NONE {
        "resign".to_string()
    } else {
        result.best_move.to_usi()
    };
    let ponder = if result.ponder_move == Move::NONE {
        None
    } else {
        Some(result.ponder_move.to_usi())
    };

    EngineEvent::BestMove { mv, ponder }
}

fn spawn_search(
    window: Window,
    mut search: Search,
    mut position: Position,
    limits: LimitsType,
) -> Result<ActiveSearch, String> {
    eprintln!(
        "spawn_search: limits (depth={}, nodes={}, byoyomi={:?}, movetime={}, ponder={})",
        limits.depth, limits.nodes, limits.byoyomi, limits.movetime, limits.ponder
    );
    eprintln!("spawn_search: position SFEN = {}", position.to_sfen());

    // Generate legal moves to debug
    use engine_core::movegen::{generate_legal_with_pass, MoveList};
    let mut legal_moves = MoveList::new();
    generate_legal_with_pass(&position, &mut legal_moves);
    eprintln!("spawn_search: legal moves count = {}", legal_moves.len());
    if !legal_moves.is_empty() {
        eprintln!("spawn_search: first few legal moves:");
        for (i, m) in legal_moves.iter().take(5).enumerate() {
            eprintln!("  {}: {}", i, m.to_usi());
        }
    }

    let stop_flag = search.stop_flag();
    let ponderhit_flag = search.ponderhit_flag();

    let handle = thread::Builder::new()
        .name("engine-search".into())
        .stack_size(SEARCH_STACK_SIZE)
        .spawn(move || {
            eprintln!(
                "search thread: calling search.go() with limits (depth={}, nodes={}, byoyomi={:?}, movetime={})",
                limits.depth, limits.nodes, limits.byoyomi, limits.movetime
            );
            let mut emitter = EngineEventEmitter::new(window);
            let result = search.go(
                &mut position,
                limits,
                Some(|info: &SearchInfo| emitter.emit_info(info)),
            );
            eprintln!(
                "engine_search: finished bestmove={} ponder={} depth={} nodes={} score={}",
                if result.best_move == Move::NONE {
                    "resign".to_string()
                } else {
                    result.best_move.to_usi()
                },
                if result.ponder_move == Move::NONE {
                    "-".to_string()
                } else {
                    result.ponder_move.to_usi()
                },
                result.depth,
                result.nodes,
                result.score.raw()
            );
            emitter.emit_bestmove(&result);
            SearchTaskResult { search }
        })
        .map_err(|e| format!("Failed to spawn search thread: {e}"))?;

    Ok(ActiveSearch {
        handle,
        stop_flag,
        _ponderhit_flag: ponderhit_flag,
    })
}

/// パス権設定
#[derive(Clone, Debug, Deserialize)]
struct PassRightsInput {
    sente: u8,
    gote: u8,
}

fn parse_position(
    sfen: &str,
    moves: Option<Vec<String>>,
    pass_rights: Option<PassRightsInput>,
) -> Result<Position, String> {
    let mut position = Position::new();

    if sfen.trim() == "startpos" {
        position
            .set_sfen(SFEN_HIRATE)
            .map_err(|e| format!("Failed to set startpos: {e}"))?;
    } else {
        position
            .set_sfen(sfen)
            .map_err(|e| format!("Failed to parse SFEN: {e}"))?;
    }

    // パス権を有効化（movesを適用する前に設定）
    if let Some(pr) = pass_rights {
        position.enable_pass_rights(pr.sente, pr.gote);
    }

    if let Some(moves) = moves {
        for mv in moves {
            let parsed =
                Move::from_usi(&mv).ok_or_else(|| format!("Invalid move in position: {mv}"))?;
            let gives_check = position.gives_check(parsed);
            position.do_move(parsed, gives_check);
        }
    }

    Ok(position)
}

fn build_limits(params: &SearchParamsInput, options: &EngineOptions) -> LimitsType {
    let mut limits = LimitsType::default();
    limits.set_start_time();

    eprintln!("build_limits: params.limits = {:?}", params.limits);

    if let Some(limits_input) = &params.limits {
        if let Some(depth) = limits_input.max_depth {
            eprintln!("build_limits: setting depth = {}", depth);
            limits.depth = depth;
        }
        if let Some(nodes) = limits_input.nodes {
            eprintln!("build_limits: setting nodes = {}", nodes);
            limits.nodes = nodes;
        }
        if let Some(byoyomi) = limits_input.byoyomi_ms {
            eprintln!("build_limits: setting byoyomi = {}", byoyomi);
            limits.byoyomi = [byoyomi; Color::NUM];
            eprintln!(
                "build_limits: after setting, limits.byoyomi = {:?}",
                limits.byoyomi
            );
        }
        if let Some(movetime) = limits_input.movetime_ms {
            eprintln!("build_limits: setting movetime = {}", movetime);
            limits.movetime = movetime;
        }
    }

    limits.ponder = params.ponder.unwrap_or(false);
    limits.multi_pv = options.multi_pv;

    eprintln!(
        "build_limits: final limits -> depth={}, nodes={}, time={:?}, byoyomi={:?}, movetime={}, ponder={}",
        limits.depth, limits.nodes, limits.time, limits.byoyomi, limits.movetime, limits.ponder
    );

    limits
}

fn value_as_i64(v: &serde_json::Value) -> Option<i64> {
    match v {
        serde_json::Value::Number(n) => n.as_i64(),
        serde_json::Value::String(s) => s.parse::<i64>().ok(),
        _ => None,
    }
}

fn value_as_i32(v: &serde_json::Value) -> Option<i32> {
    value_as_i64(v).and_then(|v| i32::try_from(v).ok())
}

fn value_as_usize(v: &serde_json::Value) -> Option<usize> {
    match v {
        serde_json::Value::Number(n) => n.as_u64().and_then(|v| usize::try_from(v).ok()),
        serde_json::Value::String(s) => s.parse::<usize>().ok(),
        _ => None,
    }
}

fn value_as_bool(v: &serde_json::Value) -> Option<bool> {
    match v {
        serde_json::Value::Bool(b) => Some(*b),
        serde_json::Value::String(s) => s.parse::<bool>().ok(),
        serde_json::Value::Number(n) => Some(n.as_u64().unwrap_or_default() != 0),
        _ => None,
    }
}

fn apply_engine_option(
    inner: &mut EngineStateInner,
    name: &str,
    value: &serde_json::Value,
) -> Result<(), String> {
    match name {
        "USI_Hash" | "Hash" => {
            let size_mb =
                value_as_usize(value).ok_or_else(|| "USI_Hash expects a number".to_string())?;
            inner.options.tt_size_mb = size_mb;
        }
        "NetworkDelay" => {
            if let Some(v) = value_as_i64(value) {
                inner.options.time_options.network_delay = v;
            }
        }
        "NetworkDelay2" => {
            if let Some(v) = value_as_i64(value) {
                inner.options.time_options.network_delay2 = v;
            }
        }
        "MinimumThinkingTime" => {
            if let Some(v) = value_as_i64(value) {
                inner.options.time_options.minimum_thinking_time = v;
            }
        }
        "SlowMover" => {
            if let Some(v) = value_as_i32(value) {
                inner.options.time_options.slow_mover = v;
            }
        }
        "USI_Ponder" => {
            if let Some(v) = value_as_bool(value) {
                inner.options.time_options.usi_ponder = v;
            }
        }
        "Stochastic_Ponder" => {
            if let Some(v) = value_as_bool(value) {
                inner.options.time_options.stochastic_ponder = v;
            }
        }
        "Skill Level" => {
            if let Some(v) = value_as_i32(value) {
                let clamped = v.clamp(0, 20);
                inner.options.skill_options.skill_level = clamped;
            }
        }
        "UCI_LimitStrength" => {
            if let Some(v) = value_as_bool(value) {
                inner.options.skill_options.uci_limit_strength = v;
            }
        }
        "UCI_Elo" => {
            if let Some(v) = value_as_i32(value) {
                inner.options.skill_options.uci_elo = v;
            }
        }
        "MaxMovesToDraw" => {
            if let Some(v) = value_as_i32(value) {
                inner.options.max_moves_to_draw = if v > 0 { v } else { DEFAULT_MAX_MOVES_TO_DRAW };
            }
        }
        "MultiPV" => {
            if let Some(v) = value_as_usize(value) {
                inner.options.multi_pv = v.max(1);
            }
        }
        "Threads" => {
            if let Some(v) = value_as_usize(value) {
                inner.options.num_threads = v.max(1);
            }
        }
        _ => {
            // Unknown options are ignored for now.
        }
    }

    let options = inner.options.clone();
    if let Some(search) = inner.search.as_mut() {
        options.apply_to_search(search);
    }

    Ok(())
}

fn stop_active_search(state: &State<EngineState>) -> Result<(), String> {
    let active = {
        let mut inner = state
            .inner
            .lock()
            .map_err(|e| format!("Failed to acquire engine state lock: {e}"))?;
        inner.reclaim_finished();
        inner.stop_active_search()
    };

    if let Some(active) = active {
        let join_result = active.handle.join();
        let mut inner = state
            .inner
            .lock()
            .map_err(|e| format!("Failed to acquire engine state lock: {e}"))?;
        inner.restore_search(join_result);
    }

    Ok(())
}

#[tauri::command]
fn engine_init(state: State<EngineState>, opts: Option<serde_json::Value>) -> Result<(), String> {
    stop_active_search(&state)?;

    let parsed_opts: Option<InitOptions> = if let Some(opts) = opts {
        Some(
            serde_json::from_value(opts)
                .map_err(|e| format!("Invalid engine init options: {e}"))?,
        )
    } else {
        None
    };

    let mut inner = state
        .inner
        .lock()
        .map_err(|e| format!("Failed to acquire engine state lock: {e}"))?;

    inner.reclaim_finished();
    if let Some(opts) = parsed_opts.as_ref() {
        inner.options.update_from_init(opts);
    }

    if inner.search.is_none() {
        inner.search = Some(inner.create_search());
    }
    let options = inner.options.clone();
    if let Some(search) = inner.search.as_mut() {
        options.apply_to_search(search);
        search.clear_tt();
    }

    let mut position = Position::new();
    position
        .set_sfen(SFEN_HIRATE)
        .map_err(|e| format!("Failed to set startpos: {e}"))?;
    inner.position = position;

    Ok(())
}

#[tauri::command]
fn engine_position(
    state: State<EngineState>,
    sfen: String,
    moves: Option<Vec<String>>,
    pass_rights: Option<PassRightsInput>,
) -> Result<(), String> {
    eprintln!(
        "engine_position: sfen={}, moves={:?}, passRights={:?}",
        sfen, moves, pass_rights
    );
    let position = parse_position(&sfen, moves, pass_rights)?;

    eprintln!("engine_position: resulting SFEN = {}", position.to_sfen());

    let mut inner = state
        .inner
        .lock()
        .map_err(|e| format!("Failed to acquire engine state lock: {e}"))?;
    inner.reclaim_finished();
    inner.position = position;

    Ok(())
}

#[tauri::command]
fn engine_option(
    state: State<EngineState>,
    name: String,
    value: serde_json::Value,
) -> Result<(), String> {
    stop_active_search(&state)?;

    let mut inner = state
        .inner
        .lock()
        .map_err(|e| format!("Failed to acquire engine state lock: {e}"))?;
    inner.reclaim_finished();
    apply_engine_option(&mut inner, &name, &value)
}

#[tauri::command]
fn engine_search(
    window: Window,
    state: State<'_, EngineState>,
    params: serde_json::Value,
) -> Result<(), String> {
    stop_active_search(&state)?;

    eprintln!("engine_search: received params = {}", params);

    let search_params: SearchParamsInput = match serde_json::from_value(params.clone()) {
        Ok(value) => value,
        Err(err) => {
            eprintln!("engine_search: deserialization error: {}", err);
            let message = format!("Invalid search params: {err}");
            emit_event(
                &window,
                EngineEvent::Error {
                    message: message.clone(),
                },
            );
            return Err(message);
        }
    };

    eprintln!("engine_search: parsed params = {:?}", search_params);

    let (position, options, search) = {
        let mut inner = state
            .inner
            .lock()
            .map_err(|e| format!("Failed to acquire engine state lock: {e}"))?;
        inner.reclaim_finished();
        let position = inner.position.clone();
        let options = inner.options.clone();
        let search = inner.search.take().unwrap_or_else(|| inner.create_search());
        (position, options, search)
    };

    eprintln!("engine_search: position SFEN = {}", position.to_sfen());

    let limits = build_limits(&search_params, &options);
    // Emit a starter info so the UI can confirm subscription before search results arrive.
    emit_event(
        &window,
        EngineEvent::Info {
            depth: Some(0),
            seldepth: None,
            nodes: Some(0),
            nps: None,
            time_ms: None,
            multipv: Some(options.multi_pv),
            pv: None,
            hashfull: None,
            score_cp: None,
            score_mate: None,
        },
    );
    let active_search = match spawn_search(window.clone(), search, position, limits) {
        Ok(active) => active,
        Err(err) => {
            emit_event(
                &window,
                EngineEvent::Error {
                    message: err.clone(),
                },
            );
            let mut inner = state
                .inner
                .lock()
                .map_err(|e| format!("Failed to acquire engine state lock: {e}"))?;
            inner.search = Some(inner.create_search());
            return Err(err);
        }
    };

    let mut inner = state
        .inner
        .lock()
        .map_err(|e| format!("Failed to acquire engine state lock: {e}"))?;
    inner.active_search = Some(active_search);

    Ok(())
}

#[tauri::command]
fn engine_stop(state: State<EngineState>) -> Result<(), String> {
    eprintln!("engine_stop: requested");
    stop_active_search(&state)
}

#[tauri::command]
fn engine_legal_moves(
    sfen: String,
    moves: Option<Vec<String>>,
    pass_rights: Option<PassRightsInput>,
) -> Result<Vec<String>, String> {
    let position = parse_position(&sfen, moves, pass_rights)?;
    let mut list = MoveList::new();
    generate_legal_with_pass(&position, &mut list);
    let usi_moves = list.as_slice().iter().map(|mv| mv.to_usi()).collect();
    Ok(usi_moves)
}

/// Thread information for debugging parallel search
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ThreadInfoResponse {
    /// Number of threads currently configured
    active_threads: usize,
    /// Maximum threads allowed (hardware concurrency)
    max_threads: usize,
    /// Whether threaded execution is available (always true for native)
    threaded_available: bool,
    /// Hardware concurrency reported by the system
    hardware_concurrency: usize,
}

#[tauri::command]
fn engine_thread_info(state: State<EngineState>) -> Result<ThreadInfoResponse, String> {
    let inner = state
        .inner
        .lock()
        .map_err(|e| format!("Failed to acquire engine state lock: {e}"))?;

    let hardware_concurrency = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);

    Ok(ThreadInfoResponse {
        active_threads: inner.options.num_threads,
        max_threads: hardware_concurrency,
        threaded_available: true,
        hardware_concurrency,
    })
}

fn get_initial_board_impl() -> Result<BoardStateJson, String> {
    Ok(Position::initial_board_json())
}

fn parse_sfen_to_board_impl(sfen: String) -> Result<BoardStateJson, String> {
    Position::parse_sfen_to_json(&sfen)
}

fn board_to_sfen_impl(board: BoardStateJson) -> Result<String, String> {
    let pos = Position::from_board_state_json(&board)?;
    Ok(pos.to_sfen())
}

fn engine_replay_moves_strict_impl(
    sfen: String,
    moves: Vec<String>,
    pass_rights: Option<PassRightsInput>,
) -> Result<ReplayResultJson, String> {
    let pass_rights_tuple = pass_rights.map(|pr| (pr.sente, pr.gote));
    Position::replay_moves_strict(&sfen, &moves, pass_rights_tuple)
}

#[tauri::command]
fn get_initial_board() -> Result<BoardStateJson, String> {
    get_initial_board_impl()
}

#[tauri::command]
fn parse_sfen_to_board(sfen: String) -> Result<BoardStateJson, String> {
    parse_sfen_to_board_impl(sfen)
}

#[tauri::command]
fn board_to_sfen(board: BoardStateJson) -> Result<String, String> {
    board_to_sfen_impl(board)
}

#[tauri::command]
fn engine_replay_moves_strict(
    sfen: String,
    moves: Vec<String>,
    pass_rights: Option<PassRightsInput>,
) -> Result<ReplayResultJson, String> {
    engine_replay_moves_strict_impl(sfen, moves, pass_rights)
}

// テスト用にコマンド実装を公開
pub fn get_initial_board_for_test() -> Result<BoardStateJson, String> {
    get_initial_board_impl()
}

pub fn parse_sfen_to_board_for_test(sfen: String) -> Result<BoardStateJson, String> {
    parse_sfen_to_board_impl(sfen)
}

pub fn board_to_sfen_for_test(board: BoardStateJson) -> Result<String, String> {
    board_to_sfen_impl(board)
}

// ============================================================
// NNUE ファイル管理
// ============================================================

/// NNUE ファイルの保存ディレクトリを取得
fn get_nnue_dir(app: &AppHandle) -> PathBuf {
    app.path()
        .app_data_dir()
        .expect("failed to get app data dir")
        .join("nnue")
}

/// NNUE インポート結果
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NnueImportResult {
    id: String,
    size: u64,
    path: String,
}

/// ユーザーが選択したファイルを app_data にコピー
/// ⚠️ Vec<u8> でバイト列を渡さない（IPC のオーバーヘッドを避ける）
/// ⚠️ 一時ファイル経由で原子的にコピー（途中失敗でゴミファイルが残らない）
#[tauri::command]
async fn import_nnue_from_path(
    app: AppHandle,
    src_path: String,
    id: String,
) -> Result<NnueImportResult, String> {
    let dir = get_nnue_dir(&app);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("Failed to create NNUE directory: {e}"))?;

    let temp_path = dir.join(format!("{id}.tmp"));
    let dest_path = dir.join(format!("{id}.nnue"));

    // 1. 一時ファイルにコピー（途中で失敗しても .nnue は残らない）
    if let Err(e) = tokio::fs::copy(&src_path, &temp_path).await {
        // 失敗時は一時ファイルを削除（存在する場合）
        let _ = tokio::fs::remove_file(&temp_path).await;
        return Err(format!("Failed to copy NNUE file: {e}"));
    }

    // 2. 一時ファイルを本番パスにリネーム（ほぼ原子的）
    // ⚠️ Windows では rename が既存ファイルを上書きできないため、先に削除
    #[cfg(target_os = "windows")]
    if dest_path.exists() {
        let _ = tokio::fs::remove_file(&dest_path).await;
    }

    if let Err(e) = tokio::fs::rename(&temp_path, &dest_path).await {
        // リネーム失敗時は一時ファイルを削除
        let _ = tokio::fs::remove_file(&temp_path).await;
        return Err(format!("Failed to finalize NNUE file: {e}"));
    }

    // メタデータ取得
    let metadata = tokio::fs::metadata(&dest_path)
        .await
        .map_err(|e| format!("Failed to get file metadata: {e}"))?;

    Ok(NnueImportResult {
        id,
        size: metadata.len(),
        path: dest_path.to_string_lossy().into_owned(),
    })
}

/// NNUE ファイルのパスを取得
#[tauri::command]
fn get_nnue_path(app: AppHandle, id: String) -> Result<String, String> {
    let path = get_nnue_dir(&app).join(format!("{id}.nnue"));
    if path.exists() {
        Ok(path.to_string_lossy().into_owned())
    } else {
        Err(format!("NNUE not found: {id}"))
    }
}

/// NNUE ファイルを削除
#[tauri::command]
async fn delete_nnue(app: AppHandle, id: String) -> Result<(), String> {
    let path = get_nnue_dir(&app).join(format!("{id}.nnue"));
    if path.exists() {
        tokio::fs::remove_file(&path)
            .await
            .map_err(|e| format!("Failed to delete NNUE file: {e}"))?;
    }
    Ok(())
}

/// SHA-256 ハッシュを計算（検証用）
/// ⚠️ ストリーム方式で計算（72MB 丸読みを避ける）
#[tauri::command]
async fn calculate_nnue_hash(app: AppHandle, id: String) -> Result<String, String> {
    let path = get_nnue_dir(&app).join(format!("{id}.nnue"));
    let mut file = tokio::fs::File::open(&path)
        .await
        .map_err(|e| format!("Failed to open NNUE file: {e}"))?;

    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024]; // 64KB バッファ

    loop {
        let n = file
            .read(&mut buffer)
            .await
            .map_err(|e| format!("Failed to read NNUE file: {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }

    let hash = hasher.finalize();
    Ok(hex::encode(hash))
}

/// NNUE ファイル一覧を取得
#[tauri::command]
async fn list_nnue_files(app: AppHandle) -> Result<Vec<String>, String> {
    let dir = get_nnue_dir(&app);
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut entries = tokio::fs::read_dir(&dir)
        .await
        .map_err(|e| format!("Failed to read NNUE directory: {e}"))?;

    let mut ids = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| format!("Failed to read directory entry: {e}"))?
    {
        let path = entry.path();
        if let Some(ext) = path.extension() {
            if ext == "nnue" {
                if let Some(stem) = path.file_stem() {
                    ids.push(stem.to_string_lossy().into_owned());
                }
            }
        }
    }

    Ok(ids)
}

/// NNUE チャンクを保存（大きいファイル対応）
/// チャンクごとに base64 エンコードされたデータを受け取り、一時ファイルに追記
#[tauri::command]
async fn save_nnue_chunk(
    app: AppHandle,
    id: String,
    chunk_index: u32,
    data_base64: String,
) -> Result<(), String> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use tokio::io::AsyncWriteExt;

    let dir = get_nnue_dir(&app);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("Failed to create NNUE directory: {e}"))?;

    // base64 デコード
    let data = STANDARD
        .decode(&data_base64)
        .map_err(|e| format!("Failed to decode base64 data: {e}"))?;

    let temp_path = dir.join(format!("{id}.tmp"));
    let index_path = dir.join(format!("{id}.idx"));

    // チャンク順序の検証
    if chunk_index == 0 {
        // 最初のチャンク: 一時ファイルを新規作成、インデックスファイルも作成
        if temp_path.exists() {
            let _ = tokio::fs::remove_file(&temp_path).await;
        }
        if index_path.exists() {
            let _ = tokio::fs::remove_file(&index_path).await;
        }
        tokio::fs::write(&temp_path, &data)
            .await
            .map_err(|e| format!("Failed to write first chunk: {e}"))?;
        tokio::fs::write(&index_path, b"1")
            .await
            .map_err(|e| format!("Failed to write index file: {e}"))?;
    } else {
        // 後続のチャンク: インデックスを検証して追記
        let expected_index = tokio::fs::read_to_string(&index_path)
            .await
            .map_err(|e| format!("Failed to read index file (chunk order error?): {e}"))?
            .parse::<u32>()
            .map_err(|e| format!("Invalid index file: {e}"))?;

        if chunk_index != expected_index {
            return Err(format!(
                "Chunk order mismatch: expected {}, got {}",
                expected_index, chunk_index
            ));
        }

        // 追記
        let mut file = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&temp_path)
            .await
            .map_err(|e| format!("Failed to open temp file for append: {e}"))?;

        file.write_all(&data)
            .await
            .map_err(|e| format!("Failed to append chunk: {e}"))?;

        // インデックスを更新
        tokio::fs::write(&index_path, (chunk_index + 1).to_string())
            .await
            .map_err(|e| format!("Failed to update index file: {e}"))?;
    }

    Ok(())
}

/// NNUE 保存を完了（一時ファイルを正式ファイルにリネーム）
#[tauri::command]
async fn finalize_nnue_save(app: AppHandle, id: String) -> Result<NnueImportResult, String> {
    let dir = get_nnue_dir(&app);
    let temp_path = dir.join(format!("{id}.tmp"));
    let index_path = dir.join(format!("{id}.idx"));
    let dest_path = dir.join(format!("{id}.nnue"));

    // 一時ファイルの存在確認
    if !temp_path.exists() {
        return Err(format!("Temp file not found: {id}"));
    }

    // ファイルサイズ確認（0バイト対策）
    let metadata = tokio::fs::metadata(&temp_path)
        .await
        .map_err(|e| format!("Failed to get temp file metadata: {e}"))?;

    if metadata.len() == 0 {
        let _ = tokio::fs::remove_file(&temp_path).await;
        let _ = tokio::fs::remove_file(&index_path).await;
        return Err("Temp file is empty".to_string());
    }

    // Windows では既存ファイルを先に削除
    #[cfg(target_os = "windows")]
    if dest_path.exists() {
        let _ = tokio::fs::remove_file(&dest_path).await;
    }

    // リネーム（原子的）
    if let Err(e) = tokio::fs::rename(&temp_path, &dest_path).await {
        let _ = tokio::fs::remove_file(&temp_path).await;
        let _ = tokio::fs::remove_file(&index_path).await;
        return Err(format!("Failed to finalize NNUE file: {e}"));
    }

    // インデックスファイルを削除
    let _ = tokio::fs::remove_file(&index_path).await;

    Ok(NnueImportResult {
        id,
        size: metadata.len(),
        path: dest_path.to_string_lossy().into_owned(),
    })
}

/// NNUE 保存を中止（一時ファイルを削除）
#[tauri::command]
async fn abort_nnue_save(app: AppHandle, id: String) -> Result<(), String> {
    let dir = get_nnue_dir(&app);
    let temp_path = dir.join(format!("{id}.tmp"));
    let index_path = dir.join(format!("{id}.idx"));

    let _ = tokio::fs::remove_file(&temp_path).await;
    let _ = tokio::fs::remove_file(&index_path).await;

    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(EngineState::default())
        .invoke_handler(tauri::generate_handler![
            engine_init,
            engine_position,
            engine_search,
            engine_stop,
            engine_option,
            engine_legal_moves,
            engine_thread_info,
            get_initial_board,
            parse_sfen_to_board,
            board_to_sfen,
            engine_replay_moves_strict,
            // NNUE 管理
            import_nnue_from_path,
            get_nnue_path,
            delete_nnue,
            calculate_nnue_hash,
            list_nnue_files,
            save_nnue_chunk,
            finalize_nnue_save,
            abort_nnue_save
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
