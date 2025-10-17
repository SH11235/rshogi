#![cfg(any(debug_assertions, feature = "diagnostics"))]

use crate::search::parallel::StopController;
use crate::search::types::SearchStack;
use crate::shogi::{Color, Position};
use crate::usi::{move_to_usi, position_to_sfen};
use log::warn;
use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};
use std::env;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

const RING_CAPACITY: usize = 256;
const STACK_TRACE_PLY_THRESHOLD: u16 = 60;

#[derive(Default)]
struct StopHandles {
    controller: Option<Arc<StopController>>,
    flag: Option<Arc<std::sync::atomic::AtomicBool>>,
}

impl StopHandles {
    fn set(
        &mut self,
        controller: Option<Arc<StopController>>,
        flag: Option<Arc<std::sync::atomic::AtomicBool>>,
    ) {
        self.controller = controller;
        self.flag = flag;
    }
}

static STOP_HANDLES: OnceLock<Mutex<StopHandles>> = OnceLock::new();
static ABORT_ON_WARN: OnceLock<bool> = OnceLock::new();
static ABORT_NOW: AtomicBool = AtomicBool::new(false);
static TRACE_WINDOW: OnceLock<Option<(u16, u16)>> = OnceLock::new();
static FAULT_TAGS: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
static LAST_FAULT: OnceLock<Mutex<Option<&'static str>>> = OnceLock::new();

#[derive(Clone, Debug)]
struct RingEvent {
    tag: &'static str,
    ply: u32,
    depth: i32,
    alpha: i32,
    beta: i32,
    is_pv: bool,
    side: Color,
    hash: u64,
    stage: Option<&'static str>,
    moveno: Option<usize>,
    mv: Option<String>,
    extra: Option<String>,
}

thread_local! {
    static RING: RefCell<VecDeque<RingEvent>> = RefCell::new(VecDeque::with_capacity(RING_CAPACITY));
    static LAST_STAGE: RefCell<Option<&'static str>> = const { RefCell::new(None) };
}

fn abort_on_warn_enabled() -> bool {
    *ABORT_ON_WARN.get_or_init(|| match env::var("DIAG_ABORT_ON_WARN") {
        Ok(val) => {
            let normalized = val.trim().to_ascii_lowercase();
            !(normalized == "0"
                || normalized == "false"
                || normalized == "off"
                || normalized == "no")
        }
        Err(_) => false,
    })
}

fn trace_window() -> Option<(u16, u16)> {
    *TRACE_WINDOW.get_or_init(|| {
        let min = env::var("TRACE_PLY_MIN").ok().and_then(|v| v.trim().parse::<u16>().ok());
        let max = env::var("TRACE_PLY_MAX").ok().and_then(|v| v.trim().parse::<u16>().ok());
        match (min, max) {
            (Some(lo), Some(hi)) => Some((lo, hi.max(lo))),
            (Some(lo), None) => Some((lo, u16::MAX)),
            _ => None,
        }
    })
}

fn within_trace_window(ply: u16) -> bool {
    if let Some((lo, hi)) = trace_window() {
        return ply >= lo && ply <= hi;
    }
    ply >= STACK_TRACE_PLY_THRESHOLD
}

fn echo_tags_enabled() -> bool {
    static ECHO: OnceLock<bool> = OnceLock::new();
    *ECHO.get_or_init(|| match env::var("DIAG_ECHO_TAGS") {
        Ok(val) => {
            let s = val.trim().to_ascii_lowercase();
            !(s == "0" || s == "off" || s == "false" || s == "no")
        }
        Err(_) => false,
    })
}

pub(crate) fn configure_abort_handles(
    stop_controller: Option<Arc<StopController>>,
    stop_flag: Option<Arc<std::sync::atomic::AtomicBool>>,
) {
    ABORT_NOW.store(false, Ordering::Release);
    if let Some(store) = FAULT_TAGS.get() {
        store.lock().unwrap().clear();
    }
    if let Some(last) = LAST_FAULT.get() {
        *last.lock().unwrap() = None;
    }
    let handles = STOP_HANDLES.get_or_init(|| Mutex::new(StopHandles::default()));
    handles.lock().unwrap().set(stop_controller, stop_flag);
}

pub(crate) fn should_abort_now() -> bool {
    ABORT_NOW.load(Ordering::Acquire)
}

pub fn last_fault_tag() -> Option<String> {
    LAST_FAULT
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap()
        .map(|tag| tag.to_string())
}

pub(crate) fn note_fault(tag: &'static str) {
    if FAULT_TAGS
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap()
        .insert(tag)
    {
        warn!("[diag] fault_tag={tag}");
    }
    *LAST_FAULT.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some(tag);

    if abort_on_warn_enabled() && !ABORT_NOW.swap(true, Ordering::AcqRel) {
        if let Some(handles) = STOP_HANDLES.get() {
            let guard = handles.lock().unwrap();
            if let Some(flag) = guard.flag.as_ref() {
                flag.store(true, Ordering::Release);
            }
            if let Some(ctrl) = guard.controller.as_ref() {
                ctrl.request_stop();
            }
        }
    }
}

fn push_event(event: RingEvent) {
    RING.with(|buf| {
        let mut buf = buf.borrow_mut();
        if buf.len() == RING_CAPACITY {
            buf.pop_front();
        }
        buf.push_back(event);
    });
}

pub(crate) fn record_stage(stage: &'static str) {
    LAST_STAGE.with(|cell| {
        *cell.borrow_mut() = Some(stage);
    });
}

fn take_stage() -> Option<&'static str> {
    LAST_STAGE.with(|cell| *cell.borrow())
}

pub(crate) fn record_ab_enter(
    pos: &Position,
    depth: i32,
    alpha: i32,
    beta: i32,
    is_pv: bool,
    tag: &'static str,
) {
    if !within_trace_window(pos.ply) {
        return;
    }
    push_event(RingEvent {
        tag,
        ply: pos.ply as u32,
        depth,
        alpha,
        beta,
        is_pv,
        side: pos.side_to_move,
        hash: pos.hash,
        stage: None,
        moveno: None,
        mv: None,
        extra: None,
    });
}

pub(crate) fn record_null_event(
    pos: &Position,
    depth: i32,
    alpha: i32,
    beta: i32,
    is_pv: bool,
    phase: &'static str,
) {
    if !within_trace_window(pos.ply) {
        return;
    }
    push_event(RingEvent {
        tag: phase,
        ply: pos.ply as u32,
        depth,
        alpha,
        beta,
        is_pv,
        side: pos.side_to_move,
        hash: pos.hash,
        stage: Some("null"),
        moveno: None,
        mv: None,
        extra: None,
    });
}

pub(crate) fn record_iid_event(
    pos: &Position,
    depth: i32,
    alpha: i32,
    beta: i32,
    is_pv: bool,
    phase: &'static str,
) {
    if !within_trace_window(pos.ply) {
        return;
    }
    push_event(RingEvent {
        tag: phase,
        ply: pos.ply as u32,
        depth,
        alpha,
        beta,
        is_pv,
        side: pos.side_to_move,
        hash: pos.hash,
        stage: Some("iid"),
        moveno: None,
        mv: None,
        extra: None,
    });
}

pub(crate) struct MovePickContext<'a> {
    pub pos: &'a Position,
    pub depth: i32,
    pub alpha: i32,
    pub beta: i32,
    pub is_pv: bool,
    pub moveno: usize,
    pub mv: crate::shogi::Move,
    pub gives_check: bool,
    pub is_capture: bool,
    pub reduction: i32,
}

pub(crate) fn record_move_pick(ctx: MovePickContext) {
    if !within_trace_window(ctx.pos.ply) {
        return;
    }
    let stage = take_stage();
    let mv_str = move_to_usi(&ctx.mv);
    let extra = format!(
        "moveno={} gives_check={} capture={} reduction={}",
        ctx.moveno, ctx.gives_check, ctx.is_capture, ctx.reduction
    );
    push_event(RingEvent {
        tag: "move",
        ply: ctx.pos.ply as u32,
        depth: ctx.depth,
        alpha: ctx.alpha,
        beta: ctx.beta,
        is_pv: ctx.is_pv,
        side: ctx.pos.side_to_move,
        hash: ctx.pos.hash,
        stage,
        moveno: Some(ctx.moveno),
        mv: Some(mv_str),
        extra: Some(extra),
    });
}

pub(crate) fn record_stack_state(pos: &Position, stack: &SearchStack, tag: &'static str) {
    if !within_trace_window(pos.ply) {
        return;
    }
    let current = stack.current_move.map(|mv| move_to_usi(&mv)).unwrap_or_else(|| "-".to_string());
    let killer0 = stack
        .killers
        .first()
        .and_then(|opt| opt.map(|mv| move_to_usi(&mv)))
        .unwrap_or_else(|| "-".to_string());
    let killer1 = stack
        .killers
        .get(1)
        .and_then(|opt| opt.map(|mv| move_to_usi(&mv)))
        .unwrap_or_else(|| "-".to_string());
    let extra = format!(
        "killers=[{},{}] null_move={} move_count={} pv={} quiet_moves={} consecutive_checks={}",
        killer0,
        killer1,
        stack.null_move,
        stack.move_count,
        stack.pv,
        stack.quiet_moves.len(),
        stack.consecutive_checks
    );
    push_event(RingEvent {
        tag,
        ply: pos.ply as u32,
        depth: stack.ply as i32,
        alpha: 0,
        beta: 0,
        is_pv: stack.pv,
        side: pos.side_to_move,
        hash: pos.hash,
        stage: None,
        moveno: Some(stack.move_count as usize),
        mv: Some(current),
        extra: Some(extra),
    });
}

pub(crate) fn record_tag(pos: &Position, tag: &'static str, extra: Option<String>) {
    if !within_trace_window(pos.ply) {
        return;
    }
    if echo_tags_enabled() {
        warn!("[diag] {tag} {}", extra.as_deref().unwrap_or("-"));
    }
    push_event(RingEvent {
        tag,
        ply: pos.ply as u32,
        depth: pos.ply as i32,
        alpha: 0,
        beta: 0,
        is_pv: false,
        side: pos.side_to_move,
        hash: pos.hash,
        stage: None,
        moveno: None,
        mv: None,
        extra,
    });
}

pub(crate) fn dump(label: &str, pos: &Position, suspect_move: Option<crate::shogi::Move>) {
    let sfen = position_to_sfen(pos);
    let suspect = suspect_move.map(|mv| move_to_usi(&mv)).unwrap_or_else(|| "-".to_string());
    let mut events: Vec<RingEvent> = Vec::new();
    RING.with(|ring| {
        events.extend(ring.borrow().iter().cloned());
    });
    if events.is_empty() {
        warn!(
            "[trace] {label}: no events recorded (ply={} side={:?} hash={:016x} sfen={} suspect={})",
            pos.ply,
            pos.side_to_move,
            pos.hash,
            sfen,
            suspect
        );
        return;
    }
    warn!(
        "[trace] {label}: dumping {} events (ply={} side={:?} hash={:016x} suspect={} sfen={})",
        events.len(),
        pos.ply,
        pos.side_to_move,
        pos.hash,
        suspect,
        sfen
    );
    for (idx, event) in events.iter().enumerate() {
        let stage = event.stage.unwrap_or("-");
        let mv = event.mv.as_deref().unwrap_or("-");
        let extra = event.extra.as_deref().unwrap_or("-");
        warn!(
            "[trace] #{:03} tag={} ply={} depth={} alpha={} beta={} pv={} side={:?} stage={} moveno={} move={} hash={:016x} extra={}",
            idx,
            event.tag,
            event.ply,
            event.depth,
            event.alpha,
            event.beta,
            event.is_pv,
            event.side,
            stage,
            event.moveno.unwrap_or(0),
            mv,
            event.hash,
            extra
        );
    }
}

pub(crate) fn clear() {
    RING.with(|ring| ring.borrow_mut().clear());
    ABORT_NOW.store(false, Ordering::Release);
    if let Some(store) = FAULT_TAGS.get() {
        store.lock().unwrap().clear();
    }
    if let Some(last) = LAST_FAULT.get() {
        *last.lock().unwrap() = None;
    }
}
