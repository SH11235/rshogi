//! TT読み取りの健全性チェック補助
//!
//! lock-free TT は読み取り競合で自己矛盾データを拾うことがあるため、
//! YaneuraOu の `is_valid(ttData.value)` 相当の防御を集約する。

use crate::types::{Bound, Move, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

const MAX_SANITY_LOGS: usize = 128;
const MAX_TRACE_LOGS: usize = 20000;
static TRACE_COUNT: AtomicUsize = AtomicUsize::new(0);
static TRACE_LOCK: Mutex<()> = Mutex::new(());
static TRACE_ROOT_MOVE_FILTER: OnceLock<Option<String>> = OnceLock::new();
static HELPER_TT_MIN_DEPTH: OnceLock<Option<i32>> = OnceLock::new();

#[inline]
pub(super) fn is_valid_tt_stored_value(value: Value) -> bool {
    value == Value::NONE
        || (-Value::INFINITE.raw() < value.raw() && value.raw() < Value::INFINITE.raw())
}

#[inline]
pub(super) fn is_valid_tt_eval(eval: Value) -> bool {
    eval == Value::NONE || eval.raw().abs() < Value::INFINITE.raw()
}

#[inline]
fn tt_sanity_debug_enabled() -> bool {
    env_flag("RSHOGI_DEBUG_TT_SANITY")
}

#[inline]
fn tt_trace_debug_enabled() -> bool {
    env_flag("RSHOGI_DEBUG_TT_TRACE")
}

#[inline]
fn trace_root_move_filter() -> Option<&'static str> {
    TRACE_ROOT_MOVE_FILTER
        .get_or_init(|| std::env::var("RSHOGI_TT_TRACE_ROOT_MOVE").ok().filter(|v| !v.is_empty()))
        .as_deref()
}

#[inline]
fn trace_root_move_matches(root_move: Move) -> bool {
    match trace_root_move_filter() {
        Some(filter) => root_move.to_usi() == filter,
        None => true,
    }
}

#[inline]
fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "on" | "ON"))
        .unwrap_or(false)
}

#[inline]
fn helper_tt_min_depth() -> Option<i32> {
    *HELPER_TT_MIN_DEPTH.get_or_init(|| {
        std::env::var("RSHOGI_HELPER_TT_MIN_DEPTH")
            .ok()
            .and_then(|v| v.parse::<i32>().ok())
    })
}

#[inline]
pub(super) fn helper_tt_write_enabled(thread_id: usize) -> bool {
    if thread_id == 0 {
        return true;
    }
    !env_flag("RSHOGI_DISABLE_HELPER_TT_WRITE")
}

#[inline]
pub(super) fn helper_tt_write_enabled_for(thread_id: usize, bound: Bound) -> bool {
    if !helper_tt_write_enabled(thread_id) {
        return false;
    }
    if thread_id != 0 {
        if env_flag("RSHOGI_DISABLE_HELPER_TT_LOWER") && bound == Bound::Lower {
            return false;
        }
        if env_flag("RSHOGI_DISABLE_HELPER_TT_UPPER") && bound == Bound::Upper {
            return false;
        }
        if env_flag("RSHOGI_DISABLE_HELPER_TT_EXACT") && bound == Bound::Exact {
            return false;
        }
    }
    if thread_id != 0 && env_flag("RSHOGI_HELPER_TT_EXACT_ONLY") && bound != Bound::Exact {
        return false;
    }
    true
}

#[inline]
pub(super) fn helper_tt_write_enabled_for_depth(
    thread_id: usize,
    bound: Bound,
    depth: i32,
) -> bool {
    if !helper_tt_write_enabled_for(thread_id, bound) {
        return false;
    }
    if thread_id != 0 && helper_tt_min_depth().is_some_and(|min_depth| depth < min_depth) {
        return false;
    }
    true
}

pub(super) struct InvalidTtLog<'a> {
    pub reason: &'a str,
    pub stage: &'a str,
    pub thread_id: usize,
    pub ply: i32,
    pub key: u64,
    pub depth: i32,
    pub bound: Bound,
    pub tt_move: Move,
    pub stored_value: Value,
    pub converted_value: Value,
    pub eval: Value,
}

pub(super) struct TtProbeTrace<'a> {
    pub stage: &'a str,
    pub thread_id: usize,
    pub ply: i32,
    pub key: u64,
    pub hit: bool,
    pub depth: i32,
    pub bound: Bound,
    pub tt_move: Move,
    pub stored_value: Value,
    pub converted_value: Value,
    pub eval: Value,
    pub root_move: Move,
}

pub(super) struct TtWriteTrace<'a> {
    pub stage: &'a str,
    pub thread_id: usize,
    pub ply: i32,
    pub key: u64,
    pub depth: i32,
    pub bound: Bound,
    pub is_pv: bool,
    pub tt_move: Move,
    pub stored_value: Value,
    pub eval: Value,
    pub root_move: Move,
}

pub(super) struct TtCutoffTrace<'a> {
    pub stage: &'a str,
    pub thread_id: usize,
    pub ply: i32,
    pub key: u64,
    pub search_depth: i32,
    pub depth: i32,
    pub bound: Bound,
    pub value: Value,
    pub beta: Value,
    pub root_move: Move,
}

pub(super) fn maybe_log_invalid_tt_data(log: InvalidTtLog<'_>) {
    if !tt_sanity_debug_enabled() {
        return;
    }
    static LOG_COUNT: AtomicUsize = AtomicUsize::new(0);
    let idx = LOG_COUNT.fetch_add(1, Ordering::Relaxed);
    if idx >= MAX_SANITY_LOGS {
        return;
    }
    eprintln!(
        "[TT-SANITY] reason={} stage={} tid={} ply={} key=0x{:016x} depth={} bound={:?} move={} stored={} converted={} eval={}",
        log.reason,
        log.stage,
        log.thread_id,
        log.ply,
        log.key,
        log.depth,
        log.bound,
        log.tt_move.to_usi(),
        log.stored_value.raw(),
        log.converted_value.raw(),
        log.eval.raw(),
    );
}

pub(super) fn maybe_trace_tt_probe(log: TtProbeTrace<'_>) {
    if !tt_trace_debug_enabled() || log.ply > 2 || !trace_root_move_matches(log.root_move) {
        return;
    }
    let seq = TRACE_COUNT.fetch_add(1, Ordering::Relaxed);
    if seq >= MAX_TRACE_LOGS {
        return;
    }
    let _guard = TRACE_LOCK.lock().ok();
    eprintln!(
        "[TT-TRACE] seq={} kind=probe stage={} tid={} ply={} root={} hit={} key=0x{:016x} depth={} bound={:?} move={} stored={} converted={} eval={}",
        seq,
        log.stage,
        log.thread_id,
        log.ply,
        log.root_move.to_usi(),
        log.hit as u8,
        log.key,
        log.depth,
        log.bound,
        log.tt_move.to_usi(),
        log.stored_value.raw(),
        log.converted_value.raw(),
        log.eval.raw(),
    );
}

pub(super) fn maybe_trace_tt_write(log: TtWriteTrace<'_>) {
    if !tt_trace_debug_enabled() || log.ply > 2 || !trace_root_move_matches(log.root_move) {
        return;
    }
    let seq = TRACE_COUNT.fetch_add(1, Ordering::Relaxed);
    if seq >= MAX_TRACE_LOGS {
        return;
    }
    let _guard = TRACE_LOCK.lock().ok();
    eprintln!(
        "[TT-TRACE] seq={} kind=write stage={} tid={} ply={} root={} key=0x{:016x} depth={} bound={:?} pv={} move={} stored={} eval={}",
        seq,
        log.stage,
        log.thread_id,
        log.ply,
        log.root_move.to_usi(),
        log.key,
        log.depth,
        log.bound,
        log.is_pv as u8,
        log.tt_move.to_usi(),
        log.stored_value.raw(),
        log.eval.raw(),
    );
}

pub(super) fn maybe_trace_tt_cutoff(log: TtCutoffTrace<'_>) {
    if !tt_trace_debug_enabled() || log.ply > 2 || !trace_root_move_matches(log.root_move) {
        return;
    }
    let seq = TRACE_COUNT.fetch_add(1, Ordering::Relaxed);
    if seq >= MAX_TRACE_LOGS {
        return;
    }
    let _guard = TRACE_LOCK.lock().ok();
    eprintln!(
        "[TT-TRACE] seq={} kind=cutoff stage={} tid={} ply={} root={} key=0x{:016x} search_depth={} depth={} bound={:?} value={} beta={}",
        seq,
        log.stage,
        log.thread_id,
        log.ply,
        log.root_move.to_usi(),
        log.key,
        log.search_depth,
        log.depth,
        log.bound,
        log.value.raw(),
        log.beta.raw(),
    );
}
