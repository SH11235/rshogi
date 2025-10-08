//! Centralized policy store for environment-driven search options.
//! Hot-path reads use atomic loads to allow runtime updates via USI setoption.

use std::sync::atomic::{AtomicI32, AtomicU8, Ordering};
use std::sync::OnceLock;

// --- ABDADA ---
const ABDADA_OFF: u8 = 0;
const ABDADA_ON: u8 = 1;

fn abdada_atomic() -> &'static AtomicU8 {
    static CELL: OnceLock<AtomicU8> = OnceLock::new();
    CELL.get_or_init(|| {
        let init = match std::env::var("SHOGI_ABDADA") {
            Ok(s) if matches!(s.as_str(), "1" | "true" | "on") => ABDADA_ON,
            _ => ABDADA_OFF,
        };
        AtomicU8::new(init)
    })
}

#[inline]
pub fn abdada_enabled() -> bool {
    abdada_atomic().load(Ordering::Relaxed) == ABDADA_ON
}

#[inline]
pub fn set_abdada(enabled: bool) {
    abdada_atomic().store(if enabled { ABDADA_ON } else { ABDADA_OFF }, Ordering::Relaxed);
}

// --- Helper Aspiration (mode/delta) ---
// mode: 0=Off, 1=Wide
const ASP_MODE_OFF: u8 = 0;
const ASP_MODE_WIDE: u8 = 1;

fn helper_asp_mode_atomic() -> &'static AtomicU8 {
    static CELL: OnceLock<AtomicU8> = OnceLock::new();
    CELL.get_or_init(|| {
        let init = match std::env::var("SHOGI_HELPER_ASP_MODE") {
            Ok(s) if s.eq_ignore_ascii_case("off") || s == "0" => ASP_MODE_OFF,
            _ => ASP_MODE_WIDE,
        };
        AtomicU8::new(init)
    })
}

fn helper_asp_delta_atomic() -> &'static AtomicI32 {
    static CELL: OnceLock<AtomicI32> = OnceLock::new();
    CELL.get_or_init(|| {
        let raw = std::env::var("SHOGI_HELPER_ASP_DELTA")
            .ok()
            .and_then(|s| s.parse::<i32>().ok())
            .unwrap_or(350);
        AtomicI32::new(raw.clamp(50, 600))
    })
}

#[inline]
pub fn helper_asp_mode_value() -> u8 {
    helper_asp_mode_atomic().load(Ordering::Relaxed)
}

#[inline]
pub fn helper_asp_delta_value() -> i32 {
    helper_asp_delta_atomic().load(Ordering::Relaxed)
}

#[inline]
pub fn set_helper_asp_mode(mode_off_wide: u8) {
    let m = if mode_off_wide == ASP_MODE_OFF {
        ASP_MODE_OFF
    } else {
        ASP_MODE_WIDE
    };
    helper_asp_mode_atomic().store(m, Ordering::Relaxed);
}

#[inline]
pub fn set_helper_asp_delta(delta: i32) {
    helper_asp_delta_atomic().store(delta.clamp(50, 600), Ordering::Relaxed);
}

/// Combined setter for Helper Aspiration (write order: delta -> mode)
#[inline]
pub fn set_helper_asp(mode_off_wide: u8, delta: i32) {
    set_helper_asp_delta(delta);
    set_helper_asp_mode(mode_off_wide);
}

// --- TT suppression below depth ---
fn tt_suppress_atomic() -> &'static AtomicI32 {
    static CELL: OnceLock<AtomicI32> = OnceLock::new();
    CELL.get_or_init(|| {
        let init = std::env::var("SHOGI_TT_SUPPRESS_BELOW_DEPTH")
            .ok()
            .and_then(|s| s.parse::<i32>().ok())
            .unwrap_or(-1);
        AtomicI32::new(init)
    })
}

#[inline]
pub fn tt_suppress_below_depth() -> Option<i32> {
    let v = tt_suppress_atomic().load(Ordering::Relaxed);
    if v >= 0 {
        Some(v)
    } else {
        None
    }
}

// --- Aspiration fail amplification (%) ---
fn asp_fail_low_atomic() -> &'static AtomicI32 {
    static CELL: OnceLock<AtomicI32> = OnceLock::new();
    CELL.get_or_init(|| {
        let v = std::env::var("SHOGI_ASP_FAILLOW_PCT")
            .ok()
            .and_then(|s| s.parse::<i32>().ok())
            .unwrap_or(33);
        AtomicI32::new(v.clamp(10, 200))
    })
}

fn asp_fail_high_atomic() -> &'static AtomicI32 {
    static CELL: OnceLock<AtomicI32> = OnceLock::new();
    CELL.get_or_init(|| {
        let v = std::env::var("SHOGI_ASP_FAILHIGH_PCT")
            .ok()
            .and_then(|s| s.parse::<i32>().ok())
            .unwrap_or(33);
        AtomicI32::new(v.clamp(10, 200))
    })
}

#[inline]
pub fn asp_fail_low_pct() -> i32 {
    asp_fail_low_atomic().load(Ordering::Relaxed)
}

#[inline]
pub fn asp_fail_high_pct() -> i32 {
    asp_fail_high_atomic().load(Ordering::Relaxed)
}

#[inline]
pub fn set_asp_fail_low_pct(pct: i32) {
    asp_fail_low_atomic().store(pct.clamp(10, 200), Ordering::Relaxed);
}

#[inline]
pub fn set_asp_fail_high_pct(pct: i32) {
    asp_fail_high_atomic().store(pct.clamp(10, 200), Ordering::Relaxed);
}

// --- Bench/Stop policy (env-initialized, read often) ---
fn bench_allrun_atomic() -> &'static AtomicU8 {
    static CELL: OnceLock<AtomicU8> = OnceLock::new();
    CELL.get_or_init(|| {
        let on = match std::env::var("SHOGI_PAR_BENCH_ALLRUN") {
            Ok(v) if matches!(v.as_str(), "1" | "true" | "on") => 1u8,
            _ => 0u8,
        };
        AtomicU8::new(on)
    })
}

#[inline]
pub fn bench_allrun_enabled() -> bool {
    bench_allrun_atomic().load(Ordering::Relaxed) == 1
}

fn bench_stop_on_mate_atomic() -> &'static AtomicU8 {
    static CELL: OnceLock<AtomicU8> = OnceLock::new();
    CELL.get_or_init(|| {
        let on = match std::env::var("SHOGI_BENCH_STOP_ON_MATE") {
            Ok(v) if matches!(v.as_str(), "0" | "false" | "off") => 0u8,
            _ => 1u8,
        };
        AtomicU8::new(on)
    })
}

#[inline]
pub fn bench_stop_on_mate_enabled() -> bool {
    bench_stop_on_mate_atomic().load(Ordering::Relaxed) == 1
}

#[inline]
pub fn set_bench_allrun(enabled: bool) {
    bench_allrun_atomic().store(if enabled { 1 } else { 0 }, Ordering::Relaxed);
}

#[inline]
pub fn set_bench_stop_on_mate(enabled: bool) {
    bench_stop_on_mate_atomic().store(if enabled { 1 } else { 0 }, Ordering::Relaxed);
}

// --- Lead window policy ---
/// Base lead-window margin in milliseconds used when approaching deadlines.
/// Env: SHOGI_LEAD_WINDOW_MS (default: 10)
#[inline]
pub fn lead_window_base_ms() -> u64 {
    static CELL: OnceLock<u64> = OnceLock::new();
    *CELL.get_or_init(|| {
        std::env::var("SHOGI_LEAD_WINDOW_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|&v| v <= 5_000)
            .unwrap_or(10)
    })
}
