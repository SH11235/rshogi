#![cfg_attr(
    all(target_arch = "wasm32", feature = "wasm-threads"),
    feature(thread_local)
)]

use std::cell::RefCell;
use std::io::ErrorKind;

use rshogi_core::eval::set_eval_hash_enabled;
use rshogi_core::movegen::{generate_legal_all_with_pass, MoveList};
use rshogi_core::nnue::{detect_format, init_nnue_from_bytes};
use rshogi_core::position::{Position, SFEN_HIRATE};
use rshogi_core::search::{LimitsType, Search, SearchInfo, SearchResult, SkillOptions};
use rshogi_core::types::json::BoardStateJson;
use rshogi_core::types::{Move, Value};
use serde::{Deserialize, Serialize};
use serde_wasm_bindgen as swb;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

const DEFAULT_TT_SIZE_MB: usize = 64;
const DEFAULT_EVAL_HASH_SIZE_MB: usize = 16;
const DEFAULT_USE_EVAL_HASH: bool = false;

thread_local! {
    static ENGINE: RefCell<Option<EngineState>> = const { RefCell::new(None) };
    static EVENT_CALLBACK: RefCell<Option<js_sys::Function>> = const { RefCell::new(None) };
}

#[cfg(all(target_arch = "wasm32", feature = "wasm-threads"))]
#[used]
#[thread_local]
static TLS_DUMMY: u8 = 0;

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InitOptions {
    tt_size_mb: Option<usize>,
    eval_hash_size_mb: Option<usize>,
    use_eval_hash: Option<bool>,
    multi_pv: Option<usize>,
    threads: Option<usize>,
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

fn get_optional_i64(object: &JsValue, key: &str) -> Result<Option<i64>, JsValue> {
    let value = js_sys::Reflect::get(object, &JsValue::from_str(key))
        .map_err(|_| JsValue::from_str(&format!("failed to read property {key}")))?;
    if value.is_null() || value.is_undefined() {
        return Ok(None);
    }
    if let Some(raw) = value.as_f64() {
        if !raw.is_finite() {
            return Err(JsValue::from_str(&format!("{key} must be finite")));
        }
        if raw.fract() != 0.0 {
            return Err(JsValue::from_str(&format!("{key} must be an integer")));
        }
        if raw < i64::MIN as f64 || raw > i64::MAX as f64 {
            return Err(JsValue::from_str(&format!("{key} is out of range")));
        }
        return Ok(Some(raw as i64));
    }
    if let Some(text) = value.as_string() {
        let parsed = text
            .parse::<i64>()
            .map_err(|_| JsValue::from_str(&format!("{key} must be a number (got {text})")))?;
        return Ok(Some(parsed));
    }
    Err(JsValue::from_str(&format!("{key} must be a number")))
}

fn get_optional_usize(object: &JsValue, key: &str) -> Result<Option<usize>, JsValue> {
    get_optional_i64(object, key).map(|value| value.and_then(|v| (v >= 0).then_some(v as usize)))
}

fn get_optional_u64(object: &JsValue, key: &str) -> Result<Option<u64>, JsValue> {
    get_optional_i64(object, key).map(|value| value.and_then(|v| (v >= 0).then_some(v as u64)))
}

fn get_optional_i32(object: &JsValue, key: &str) -> Result<Option<i32>, JsValue> {
    let value = get_optional_i64(object, key)?;
    value
        .map(|v| i32::try_from(v).map_err(|_| JsValue::from_str(&format!("{key} is out of range"))))
        .transpose()
}

fn get_optional_bool(object: &JsValue, key: &str) -> Result<Option<bool>, JsValue> {
    let value = js_sys::Reflect::get(object, &JsValue::from_str(key))
        .map_err(|_| JsValue::from_str(&format!("failed to read property {key}")))?;
    if value.is_null() || value.is_undefined() {
        return Ok(None);
    }
    if let Some(v) = value.as_bool() {
        return Ok(Some(v));
    }
    if let Some(raw) = value.as_f64() {
        if !raw.is_finite() {
            return Err(JsValue::from_str(&format!("{key} must be finite")));
        }
        return Ok(Some(raw != 0.0));
    }
    if let Some(text) = value.as_string() {
        let parsed = text
            .parse::<bool>()
            .map_err(|_| JsValue::from_str(&format!("{key} must be a boolean (got {text})")))?;
        return Ok(Some(parsed));
    }
    Err(JsValue::from_str(&format!("{key} must be a boolean")))
}

fn parse_init_options(opts: Option<JsValue>) -> Result<InitOptions, JsValue> {
    let Some(opts) = opts else {
        return Ok(InitOptions::default());
    };
    if opts.is_null() || opts.is_undefined() {
        return Ok(InitOptions::default());
    }
    let _obj: js_sys::Object = opts
        .clone()
        .dyn_into()
        .map_err(|_| JsValue::from_str("init options must be an object"))?;

    Ok(InitOptions {
        tt_size_mb: get_optional_usize(&opts, "ttSizeMb")?,
        multi_pv: get_optional_usize(&opts, "multiPv")?,
        threads: get_optional_usize(&opts, "threads")?,
        eval_hash_size_mb: get_optional_usize(&opts, "evalHashSizeMb")?,
        use_eval_hash: get_optional_bool(&opts, "useEvalHash")?,
    })
}

fn parse_limits(value: JsValue) -> Result<JsLimits, JsValue> {
    let _obj: js_sys::Object = value
        .clone()
        .dyn_into()
        .map_err(|_| JsValue::from_str("limits must be an object"))?;
    Ok(JsLimits {
        max_depth: get_optional_i32(&value, "maxDepth")?,
        nodes: get_optional_u64(&value, "nodes")?,
        byoyomi_ms: get_optional_i64(&value, "byoyomiMs")?,
        movetime_ms: get_optional_i64(&value, "movetimeMs")?,
    })
}

fn parse_search_params(params: Option<JsValue>) -> Result<JsSearchParams, JsValue> {
    let Some(params) = params else {
        return Ok(JsSearchParams::default());
    };
    if params.is_null() || params.is_undefined() {
        return Ok(JsSearchParams::default());
    }
    let _obj: js_sys::Object = params
        .clone()
        .dyn_into()
        .map_err(|_| JsValue::from_str("search params must be an object"))?;

    let limits = js_sys::Reflect::get(&params, &JsValue::from_str("limits"))
        .map_err(|_| JsValue::from_str("failed to read property limits"))?;
    let parsed_limits = if limits.is_null() || limits.is_undefined() {
        None
    } else {
        Some(parse_limits(limits)?)
    };

    Ok(JsSearchParams {
        limits: parsed_limits,
        ponder: get_optional_bool(&params, "ponder")?,
    })
}

fn parse_moves(value: Option<JsValue>) -> Result<Vec<String>, JsValue> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    if value.is_null() || value.is_undefined() {
        return Ok(Vec::new());
    }
    let array: js_sys::Array =
        value.dyn_into().map_err(|_| JsValue::from_str("moves must be an array"))?;
    let mut moves = Vec::with_capacity(array.length() as usize);
    for mv in array.iter() {
        let mv = mv
            .as_string()
            .ok_or_else(|| JsValue::from_str("moves must be an array of strings"))?;
        moves.push(mv);
    }
    Ok(moves)
}

fn parse_set_option_value(value: Option<JsValue>) -> Result<serde_json::Value, JsValue> {
    let Some(value) = value else {
        return Ok(serde_json::Value::Null);
    };
    if value.is_null() || value.is_undefined() {
        return Ok(serde_json::Value::Null);
    }
    if let Some(v) = value.as_bool() {
        return Ok(serde_json::Value::Bool(v));
    }
    if let Some(v) = value.as_string() {
        return Ok(serde_json::Value::String(v));
    }
    if let Some(raw) = value.as_f64() {
        if !raw.is_finite() {
            return Err(JsValue::from_str("setOption value must be finite"));
        }
        if raw.fract() == 0.0 && raw >= i64::MIN as f64 && raw <= i64::MAX as f64 {
            return Ok(serde_json::Value::Number((raw as i64).into()));
        }
        let num = serde_json::Number::from_f64(raw)
            .ok_or_else(|| JsValue::from_str("setOption value is not a valid JSON number"))?;
        return Ok(serde_json::Value::Number(num));
    }
    Err(JsValue::from_str("setOption value must be string/number/boolean"))
}

/// パス権設定
#[derive(Debug, Clone, Copy, Deserialize)]
struct PassRightsInput {
    sente: u8,
    gote: u8,
}

fn build_position(
    sfen: &str,
    moves: &[String],
    pass_rights: Option<PassRightsInput>,
) -> Result<Position, JsValue> {
    let mut position = Position::new();
    if sfen.trim() == "startpos" {
        position.set_sfen(SFEN_HIRATE).map_err(|e| JsValue::from_str(&e.to_string()))?;
    } else {
        position.set_sfen(sfen).map_err(|e| JsValue::from_str(&e.to_string()))?;
    }

    // パス権を有効化（movesを適用する前に設定）
    if let Some(pr) = pass_rights {
        position.enable_pass_rights(pr.sente, pr.gote);
    }

    for mv in moves {
        let parsed =
            Move::from_usi(mv).ok_or_else(|| JsValue::from_str(&format!("invalid move: {mv}")))?;
        let gives_check = position.gives_check(parsed);
        position.do_move(parsed, gives_check);
    }

    Ok(position)
}

fn apply_move_to_position(position: &mut Position, mv: &str) -> Result<(), JsValue> {
    let parsed =
        Move::from_usi(mv).ok_or_else(|| JsValue::from_str(&format!("invalid move: {mv}")))?;
    let gives_check = position.gives_check(parsed);
    position.do_move(parsed, gives_check);
    Ok(())
}

fn emit_event(event: EventPayload) {
    EVENT_CALLBACK.with(|callback| {
        if let Some(cb) = callback.borrow().as_ref() {
            if let Ok(value) = swb::to_value(&event) {
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
    // Match the logic in SearchInfo::to_usi_string:
    // Only treat as mate score if it's a true mate score AND within valid range
    // Boundary values like ±INFINITE should be treated as CP scores
    if value.is_mate_score() && value.raw().abs() < Value::INFINITE.raw() {
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
pub fn wasm_get_initial_board() -> Result<JsValue, JsValue> {
    let board = Position::initial_board_json();
    swb::to_value(&board).map_err(|e| JsValue::from_str(&format!("Serialization error: {e}")))
}

#[wasm_bindgen]
pub fn wasm_parse_sfen_to_board(sfen: String) -> Result<JsValue, JsValue> {
    let board = Position::parse_sfen_to_json(&sfen).map_err(|e| JsValue::from_str(&e))?;
    swb::to_value(&board).map_err(|e| JsValue::from_str(&format!("Serialization error: {e}")))
}

#[wasm_bindgen]
pub fn wasm_board_to_sfen(board_json: JsValue) -> Result<String, JsValue> {
    let board: BoardStateJson = swb::from_value(board_json)
        .map_err(|e| JsValue::from_str(&format!("Deserialization error: {e}")))?;
    let pos = Position::from_board_state_json(&board).map_err(|e| JsValue::from_str(&e))?;
    Ok(pos.to_sfen())
}

#[wasm_bindgen]
pub fn wasm_get_legal_moves(
    sfen: String,
    moves: Option<JsValue>,
    pass_rights: Option<JsValue>,
) -> Result<JsValue, JsValue> {
    let parsed_moves = parse_moves(moves)?;
    let pass_rights: Option<PassRightsInput> = pass_rights
        .map(swb::from_value)
        .transpose()
        .map_err(|e| JsValue::from_str(&format!("invalid passRights: {e}")))?;

    let position = build_position(&sfen, &parsed_moves, pass_rights)?;
    let mut list = MoveList::new();
    generate_legal_all_with_pass(&position, &mut list);
    let legal_moves: Vec<String> = list.as_slice().iter().map(|mv| mv.to_usi()).collect();

    swb::to_value(&legal_moves).map_err(|e| JsValue::from_str(&format!("Serialization error: {e}")))
}

#[wasm_bindgen]
pub fn wasm_replay_moves_strict(
    sfen: String,
    moves: JsValue,
    pass_rights: Option<JsValue>,
) -> Result<JsValue, JsValue> {
    let parsed_moves = parse_moves(Some(moves))?;
    let pass_rights: Option<PassRightsInput> = pass_rights
        .map(swb::from_value)
        .transpose()
        .map_err(|e| JsValue::from_str(&format!("invalid passRights: {e}")))?;
    let pass_rights_tuple = pass_rights.map(|pr| (pr.sente, pr.gote));
    let result = Position::replay_moves_strict(&sfen, &parsed_moves, pass_rights_tuple)
        .map_err(|e| JsValue::from_str(&e))?;
    swb::to_value(&result).map_err(|e| JsValue::from_str(&format!("Serialization error: {e}")))
}

#[wasm_bindgen]
pub fn init(opts: Option<JsValue>) -> Result<(), JsValue> {
    let opts = parse_init_options(opts)?;
    install_panic_hook();

    ENGINE.with(|state| {
        let mut engine = EngineState::new(opts.tt_size_mb.unwrap_or(DEFAULT_TT_SIZE_MB));
        if let Some(mpv) = opts.multi_pv {
            engine.default_multi_pv = mpv.max(1);
        }
        if let Some(threads) = opts.threads {
            engine.search.set_num_threads(threads);
        }
        if let Some(size) = opts.eval_hash_size_mb {
            engine.search.resize_eval_hash(size);
        } else {
            engine.search.resize_eval_hash(DEFAULT_EVAL_HASH_SIZE_MB);
        }
        set_eval_hash_enabled(opts.use_eval_hash.unwrap_or(DEFAULT_USE_EVAL_HASH));
        *state.borrow_mut() = Some(engine);
    });

    Ok(())
}

// Re-export wasm-bindgen-rayon's init_thread_pool for async Worker initialization.
// This returns a Promise that resolves when all workers are ready.
#[cfg(feature = "wasm-threads")]
pub use wasm_bindgen_rayon::init_thread_pool;

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

/// NNUE バッファの最大サイズ（500MB）
const MAX_NNUE_SIZE: usize = 500 * 1024 * 1024;

/// NNUE モデル用のバッファを確保する
///
/// JavaScript から Wasm メモリに直接書き込むためのバッファを確保します。
/// 確保したメモリは [`load_model_from_ptr`] または [`free_nnue_buffer`] で解放する必要があります。
///
/// # Arguments
///
/// * `len` - 確保するバイト数
///
/// # Returns
///
/// 確保したメモリ領域へのポインタ。失敗時は null ポインタを返します。
///
/// # Safety
///
/// 返されたポインタは以下のいずれかの方法で解放する必要があります：
/// - [`load_model_from_ptr`] に渡す（所有権が移譲され、自動的に解放される）
/// - [`free_nnue_buffer`] を呼び出す
///
/// # Example (JavaScript)
///
/// ```javascript
/// const len = nnueData.byteLength;
/// const ptr = alloc_nnue_buffer(len);
/// if (ptr === 0) {
///     throw new Error("Failed to allocate NNUE buffer");
/// }
/// // ptr にデータをコピーして load_model_from_ptr を呼び出す
/// ```
#[wasm_bindgen]
pub fn alloc_nnue_buffer(len: usize) -> *mut u8 {
    if len == 0 || len > MAX_NNUE_SIZE {
        return std::ptr::null_mut();
    }
    let mut buf = vec![0u8; len];
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

/// NNUE モデルをポインタから直接ロードする
///
/// [`alloc_nnue_buffer`] で確保したメモリ領域から NNUE モデルをロードします。
/// この関数は渡されたポインタの所有権を取得し、成功・失敗に関わらず
/// メモリを自動的に解放します。
///
/// # Arguments
///
/// * `ptr` - [`alloc_nnue_buffer`] で確保されたメモリ領域へのポインタ
/// * `len` - データのバイト長（[`alloc_nnue_buffer`] に渡した値と同じ）
///
/// # Safety
///
/// - `ptr` は [`alloc_nnue_buffer(len)`] の戻り値でなければなりません
/// - この関数は `ptr` の所有権を取得し、`Vec::from_raw_parts` を使用して
///   メモリを管理します
/// - 成功・失敗に関わらず、呼び出し側は `ptr` を再度使用してはいけません
/// - 失敗した場合でも、メモリは自動的に解放されます
///
/// # Errors
///
/// - `ptr` が null の場合
/// - `len` が 0 の場合
/// - `len` が最大サイズ（500MB）を超える場合
/// - NNUE データのロードに失敗した場合
///
/// # Example (JavaScript)
///
/// ```javascript
/// const len = nnueData.byteLength;
/// const ptr = alloc_nnue_buffer(len);
/// try {
///     const target = new Uint8Array(memory.buffer, ptr, len);
///     target.set(nnueData);
///     load_model_from_ptr(ptr, len); // ptr の所有権を移譲
/// } catch (e) {
///     // エラー時も自動的にメモリは解放される
///     // free_nnue_buffer を呼ぶ必要はない
/// }
/// ```
#[wasm_bindgen]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn load_model_from_ptr(ptr: *mut u8, len: usize) -> Result<(), JsValue> {
    if ptr.is_null() || len == 0 {
        return Err(JsValue::from_str("invalid NNUE buffer"));
    }

    if len > MAX_NNUE_SIZE {
        // ポインタが有効な場合は解放してからエラーを返す
        unsafe {
            drop(Vec::from_raw_parts(ptr, len, len));
        }
        return Err(JsValue::from_str("NNUE buffer too large (max 500MB)"));
    }

    let buf = unsafe { Vec::from_raw_parts(ptr, len, len) };
    init_nnue_from_bytes(&buf)
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

/// NNUE バッファを解放する
///
/// [`alloc_nnue_buffer`] で確保したメモリを解放します。
/// [`load_model_from_ptr`] を呼び出さずにバッファを破棄する場合に使用します。
///
/// # Arguments
///
/// * `ptr` - [`alloc_nnue_buffer`] で確保されたメモリ領域へのポインタ
/// * `len` - バッファのバイト長（[`alloc_nnue_buffer`] に渡した値と同じ）
///
/// # Safety
///
/// - `ptr` は [`alloc_nnue_buffer(len)`] の戻り値でなければなりません
/// - 同じポインタに対して複数回呼び出してはいけません
/// - [`load_model_from_ptr`] に渡したポインタに対して呼び出してはいけません
///   （二重解放になります）
#[wasm_bindgen]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn free_nnue_buffer(ptr: *mut u8, len: usize) {
    if ptr.is_null() || len == 0 {
        return;
    }
    unsafe {
        drop(Vec::from_raw_parts(ptr, len, len));
    }
}

/// NNUE フォーマット情報（JS 向け）
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NnueFormatInfoJs {
    /// アーキテクチャ名（例: "HalfKA1024", "HalfKA_hm1024", "LayerStacks"）
    architecture: String,

    /// L1 次元（例: 256, 512, 1024, 1536）
    l1_dimension: u32,

    /// L2 次元（例: 8, 32）
    l2_dimension: u32,

    /// L3 次元（例: 32, 96）
    l3_dimension: u32,

    /// 活性化関数（"CReLU" or "SCReLU"）
    activation: String,

    /// バージョンヘッダ（16進数文字列）
    version_header: String,
}

/// NNUE ファイルのフォーマット情報を検出（ロードせずにヘッダのみ解析）
///
/// # Arguments
/// * `bytes` - NNUE ファイルの先頭 1KB 以上のバイト列
///
/// # Returns
/// * フォーマット情報を含む JS オブジェクト
#[wasm_bindgen]
pub fn detect_nnue_format(bytes: &[u8]) -> Result<JsValue, JsValue> {
    let info = detect_format(bytes).map_err(|e| JsValue::from_str(&e.to_string()))?;

    let result = NnueFormatInfoJs {
        architecture: info.architecture,
        l1_dimension: info.l1_dimension,
        l2_dimension: info.l2_dimension,
        l3_dimension: info.l3_dimension,
        activation: info.activation,
        version_header: format!("0x{:08X}", info.version),
    };

    swb::to_value(&result).map_err(|e| JsValue::from_str(&format!("Serialization error: {e}")))
}

/// NNUE ファイルが現在のエンジンと互換性があるか確認
#[wasm_bindgen]
pub fn is_nnue_compatible(bytes: &[u8]) -> bool {
    detect_format(bytes).is_ok()
}

#[wasm_bindgen]
pub fn load_position(
    sfen: &str,
    moves: Option<JsValue>,
    pass_rights: Option<JsValue>,
) -> Result<(), JsValue> {
    let moves = parse_moves(moves)?;
    let pass_rights: Option<PassRightsInput> = pass_rights
        .map(swb::from_value)
        .transpose()
        .map_err(|e| JsValue::from_str(&format!("invalid passRights: {e}")))?;
    let position = build_position(sfen, &moves, pass_rights)?;

    with_engine_mut(|engine| {
        engine.position = position;
        Ok(())
    })
}

#[wasm_bindgen]
pub fn apply_moves(moves: JsValue) -> Result<(), JsValue> {
    let parsed_moves = parse_moves(Some(moves))?;

    with_engine_mut(|engine| {
        for mv in &parsed_moves {
            apply_move_to_position(&mut engine.position, mv)?;
        }
        Ok(())
    })
}

#[wasm_bindgen]
pub fn search(params: Option<JsValue>) -> Result<(), JsValue> {
    let params = parse_search_params(params)?;

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
pub fn set_option(name: &str, value: Option<JsValue>) -> Result<(), JsValue> {
    let value = parse_set_option_value(value)?;

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
