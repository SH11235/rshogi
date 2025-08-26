//! Debug utilities for MoveGen hang investigation

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Trace phases for movegen
pub static PHASE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Check if a phase should be traced
pub fn should_trace_phase(phase: &str) -> bool {
    if let Ok(phases) = std::env::var("MOVEGEN_TRACE") {
        phases.contains(phase)
    } else {
        false
    }
}

/// Trace entry into a phase
pub fn trace_phase(phase: &str) {
    if should_trace_phase(phase) {
        let count = PHASE_COUNTER.fetch_add(1, Ordering::SeqCst);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        eprintln!("timestamp={}\tphase={}\tcount={}", timestamp, phase, count);
    }
}

/// Check if a phase is disabled
pub fn is_phase_disabled(phase: &str) -> bool {
    std::env::var(format!("MOVEGEN_DISABLE_{}", phase.to_uppercase()))
        .map(|v| v == "1")
        .unwrap_or(false)
}

/// Phase names
pub const PHASE_PRE: &str = "pre";
pub const PHASE_CHECKERS_PINS: &str = "checkers_pins";
pub const PHASE_KING: &str = "king";
pub const PHASE_PIECES: &str = "pieces";
pub const PHASE_ROOK: &str = "rook";
pub const PHASE_BISHOP: &str = "bishop";
pub const PHASE_GOLD: &str = "gold";
pub const PHASE_SILVER: &str = "silver";
pub const PHASE_KNIGHT: &str = "knight";
pub const PHASE_LANCE: &str = "lance";
pub const PHASE_PAWN: &str = "pawn";
pub const PHASE_DROPS: &str = "drops";
pub const PHASE_POST: &str = "post";
pub const PHASE_EARLY_EXIT: &str = "early_exit";