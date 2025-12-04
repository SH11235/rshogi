use std::cell::RefCell;
use std::io::ErrorKind;

use engine_core::nnue::init_nnue_from_bytes;
use engine_core::position::{Position, SFEN_HIRATE};
use engine_core::search::{
    init_search_module, LimitsType, Search, SearchInfo, SearchResult, SkillOptions,
};
use engine_core::types::{Move, Value};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

const DEFAULT_TT_SIZE_MB: usize = 64;

thread_local! {
    static ENGINE: RefCell<Option<EngineState>> = const { RefCell::new(None) };
    static EVENT_CALLBACK: RefCell<Option<js_sys::Function>> = const { RefCell::new(None) };
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InitOptions {
    tt_size_mb: Option<usize>,
    multi_pv: Option<usize>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsLimits {
    max_depth: Option<i32>,
    nodes: Option<u64>,
    byoyomi_ms: Option<i64>,
    movetime_ms: Option<i64>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsSearchParams {
    limits: Option<JsLimits>,
    ponder: Option<bool>,
}

#[derive(Serialize)]
#[serde(tag = "type")]
enum EventPayload {
    #[serde(rename = "info")]
    Info {
        depth: Option<u32>,
        seldepth: Option<u32>,
        #[serde(rename = "scoreCp")]
        score_cp: Option<i32>,
        #[serde(rename = "scoreMate")]
        score_mate: Option<i32>,
        nodes: Option<u64>,
        nps: Option<u64>,
        #[serde(rename = "timeMs")]
        time_ms: Option<u64>,
        multipv: Option<u32>,
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

#[cfg(target_arch = "wasm32")]
fn install_panic_hook() {
    console_error_panic_hook::set_once();
}

#[cfg(not(target_arch = "wasm32"))]
fn install_panic_hook() {}

struct EngineState {
    search: Search,
    position: Position,
    default_multi_pv: usize,
}

impl EngineState {
    fn new(tt_size_mb: usize) -> Self {
        let mut position = Position::new();
        position.set_sfen(SFEN_HIRATE).unwrap();

        Self {
            search: Search::new(tt_size_mb),
            position,
            default_multi_pv: 1,
        }
    }
}

fn parse_json_or_default<T>(raw: Option<String>) -> Result<T, JsValue>
where
    T: DeserializeOwned + Default,
{
    if let Some(text) = raw {
        serde_json::from_str(&text).map_err(|err| JsValue::from_str(&err.to_string()))
    } else {
        Ok(T::default())
    }
}

#[allow(deprecated)]
fn emit_event(event: EventPayload) {
    EVENT_CALLBACK.with(|callback| {
        if let Some(cb) = callback.borrow().as_ref() {
            if let Ok(value) = JsValue::from_serde(&event) {
                let _ = cb.call1(&JsValue::NULL, &value);
            }
        }
    });
}

fn emit_info(info: &SearchInfo) {
    let (score_cp, score_mate) = score_fields(info.score);
    emit_event(EventPayload::Info {
        depth: Some(info.depth.max(0) as u32),
        seldepth: Some(info.sel_depth.max(0) as u32),
        score_cp,
        score_mate,
        nodes: Some(info.nodes),
        nps: Some(info.nps),
        time_ms: Some(info.time_ms),
        multipv: Some(info.multi_pv as u32),
        pv: Some(info.pv.iter().map(|m| m.to_usi()).collect()),
        hashfull: Some(info.hashfull),
    });
}

fn emit_bestmove(result: SearchResult) {
    emit_event(EventPayload::BestMove {
        mv: result.best_move.to_usi(),
        ponder: if result.ponder_move == Move::NONE {
            None
        } else {
            Some(result.ponder_move.to_usi())
        },
    });
}

fn score_fields(value: Value) -> (Option<i32>, Option<i32>) {
    if value.is_mate_score() {
        let mate_ply = value.mate_ply();
        let signed = if value.is_loss() { -mate_ply } else { mate_ply };
        (None, Some(signed))
    } else {
        (Some(value.raw()), None)
    }
}

fn with_engine_mut<R, F>(f: F) -> Result<R, JsValue>
where
    F: FnOnce(&mut EngineState) -> Result<R, JsValue>,
{
    install_panic_hook();
    init_search_module();
    ENGINE.with(|cell| {
        let mut guard = cell.borrow_mut();
        if guard.is_none() {
            *guard = Some(EngineState::new(DEFAULT_TT_SIZE_MB));
        }
        let engine = guard.as_mut().unwrap();
        f(engine)
    })
}

fn as_i64(value: &serde_json::Value) -> Option<i64> {
    match value {
        serde_json::Value::Number(num) => num.as_i64(),
        serde_json::Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn as_usize(value: &serde_json::Value) -> Option<usize> {
    as_i64(value).and_then(|v| if v >= 0 { Some(v as usize) } else { None })
}

fn as_bool(value: &serde_json::Value) -> Option<bool> {
    match value {
        serde_json::Value::Bool(v) => Some(*v),
        serde_json::Value::Number(n) => n.as_i64().map(|v| v != 0),
        serde_json::Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn as_string(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

#[wasm_bindgen]
pub fn set_event_handler(callback: Option<js_sys::Function>) {
    EVENT_CALLBACK.with(|cb| {
        *cb.borrow_mut() = callback;
    });
}

#[wasm_bindgen]
pub fn init(opts_json: Option<String>) -> Result<(), JsValue> {
    let opts: InitOptions = parse_json_or_default(opts_json)?;
    init_search_module();
    install_panic_hook();

    ENGINE.with(|state| {
        let mut engine = EngineState::new(opts.tt_size_mb.unwrap_or(DEFAULT_TT_SIZE_MB));
        if let Some(mpv) = opts.multi_pv {
            engine.default_multi_pv = mpv.max(1);
        }
        *state.borrow_mut() = Some(engine);
    });

    Ok(())
}

#[wasm_bindgen]
pub fn load_model(bytes: &[u8]) -> Result<(), JsValue> {
    init_nnue_from_bytes(bytes)
        .or_else(|err| {
            if err.kind() == ErrorKind::AlreadyExists {
                Ok(())
            } else {
                Err(err)
            }
        })
        .map_err(|err| JsValue::from_str(&err.to_string()))?;
    Ok(())
}

#[wasm_bindgen]
pub fn load_position(sfen: &str, moves_json: Option<String>) -> Result<(), JsValue> {
    let moves: Vec<String> = parse_json_or_default(moves_json)?;

    with_engine_mut(|engine| {
        if sfen.trim() == "startpos" {
            engine
                .position
                .set_sfen(SFEN_HIRATE)
                .map_err(|e| JsValue::from_str(&e.to_string()))?;
        } else {
            engine.position.set_sfen(sfen).map_err(|e| JsValue::from_str(&e.to_string()))?;
        }

        for mv in moves {
            let m = Move::from_usi(&mv)
                .ok_or_else(|| JsValue::from_str(&format!("invalid move: {mv}")))?;
            let gives_check = engine.position.gives_check(m);
            engine.position.do_move(m, gives_check);
        }

        Ok(())
    })
}

#[wasm_bindgen]
pub fn search(params_json: Option<String>) -> Result<(), JsValue> {
    let params: JsSearchParams = parse_json_or_default(params_json)?;

    with_engine_mut(|engine| {
        let mut limits = LimitsType::new();
        limits.set_start_time();
        limits.multi_pv = engine.default_multi_pv.max(1);
        limits.ponder = params.ponder.unwrap_or(false);

        if let Some(lim) = params.limits {
            if let Some(depth) = lim.max_depth {
                limits.depth = depth;
            }
            if let Some(nodes) = lim.nodes {
                limits.nodes = nodes;
            }
            if let Some(byo) = lim.byoyomi_ms {
                limits.byoyomi = [byo, byo];
                limits.time = [byo, byo];
            }
            if let Some(mt) = lim.movetime_ms {
                limits.movetime = mt;
            }
        }

        let result = engine.search.go(
            &mut engine.position,
            limits,
            Some(|info: &SearchInfo| emit_info(info)),
        );
        emit_bestmove(result);
        Ok(())
    })
}

#[wasm_bindgen]
pub fn stop() {
    let _ = with_engine_mut(|engine| {
        engine.search.stop();
        Ok(())
    });
}

#[wasm_bindgen]
pub fn set_option(name: &str, value_json: Option<String>) -> Result<(), JsValue> {
    let value: serde_json::Value = if let Some(raw) = value_json {
        serde_json::from_str(&raw).map_err(|err| JsValue::from_str(&err.to_string()))?
    } else {
        serde_json::Value::Null
    };

    with_engine_mut(|engine| {
        match name {
            "USI_Hash" => {
                if let Some(size) = as_usize(&value) {
                    engine.search.resize_tt(size.max(1));
                }
            }
            "NetworkDelay" => {
                if let Some(v) = as_i64(&value) {
                    let mut opts = engine.search.time_options();
                    opts.network_delay = v;
                    engine.search.set_time_options(opts);
                }
            }
            "NetworkDelay2" => {
                if let Some(v) = as_i64(&value) {
                    let mut opts = engine.search.time_options();
                    opts.network_delay2 = v;
                    engine.search.set_time_options(opts);
                }
            }
            "MinimumThinkingTime" => {
                if let Some(v) = as_i64(&value) {
                    let mut opts = engine.search.time_options();
                    opts.minimum_thinking_time = v;
                    engine.search.set_time_options(opts);
                }
            }
            "SlowMover" => {
                if let Some(v) = as_i64(&value) {
                    let mut opts = engine.search.time_options();
                    opts.slow_mover = v as i32;
                    engine.search.set_time_options(opts);
                }
            }
            "USI_Ponder" => {
                if let Some(v) = as_bool(&value) {
                    let mut opts = engine.search.time_options();
                    opts.usi_ponder = v;
                    engine.search.set_time_options(opts);
                }
            }
            "Stochastic_Ponder" => {
                if let Some(v) = as_bool(&value) {
                    let mut opts = engine.search.time_options();
                    opts.stochastic_ponder = v;
                    engine.search.set_time_options(opts);
                }
            }
            "MultiPV" => {
                if let Some(v) = as_usize(&value) {
                    engine.default_multi_pv = v.max(1);
                }
            }
            "Skill Level" => {
                if let Some(v) = as_i64(&value) {
                    let mut opts: SkillOptions = engine.search.skill_options();
                    opts.skill_level = v as i32;
                    engine.search.set_skill_options(opts);
                }
            }
            "UCI_LimitStrength" => {
                if let Some(v) = as_bool(&value) {
                    let mut opts: SkillOptions = engine.search.skill_options();
                    opts.uci_limit_strength = v;
                    engine.search.set_skill_options(opts);
                }
            }
            "UCI_Elo" => {
                if let Some(v) = as_i64(&value) {
                    let mut opts: SkillOptions = engine.search.skill_options();
                    opts.uci_elo = v as i32;
                    engine.search.set_skill_options(opts);
                }
            }
            "MaxMovesToDraw" => {
                if let Some(v) = as_i64(&value) {
                    engine.search.set_max_moves_to_draw(v as i32);
                }
            }
            other => {
                if let Some(val) = as_string(&value) {
                    emit_event(EventPayload::Error {
                        message: format!("Unknown option {other}={val}"),
                    });
                }
            }
        }

        Ok(())
    })
}

#[wasm_bindgen]
pub fn dispose() {
    ENGINE.with(|state| {
        state.borrow_mut().take();
    });
}
