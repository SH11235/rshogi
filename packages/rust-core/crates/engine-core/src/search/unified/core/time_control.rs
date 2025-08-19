//! Time control utilities for search
//!
//! Manages event polling intervals based on time control settings

use crate::{
    evaluation::evaluate::Evaluator, search::unified::UnifiedSearcher, time_management::TimeControl,
};

/// Get event polling mask based on time limit
///
/// Returns a bitmask that determines how frequently to check for events (time limit, stop flag, etc).
/// Lower values mean more frequent checks:
/// - 0x0 (0): Check every node (immediate response when already stopped)
/// - 0x1F (31): Check every 32 nodes (responsive stop handling)
/// - 0x3F (63): Check every 64 nodes (fixed nodes or ponder mode)
/// - 0x7F-0x3FF: Check every 128-1024 nodes (time-based controls)
pub fn get_event_poll_mask<
    E,
    const USE_TT: bool,
    const USE_PRUNING: bool,
    const TT_SIZE_MB: usize,
>(
    searcher: &UnifiedSearcher<E, USE_TT, USE_PRUNING, TT_SIZE_MB>,
) -> u64
where
    E: Evaluator + Send + Sync + 'static,
{
    // If already stopped, check every node for immediate exit
    if searcher.context.should_stop() {
        return 0x0; // Check every node for immediate response
    }

    // If stop_flag is present, use more frequent polling for responsiveness
    if searcher.context.limits().stop_flag.is_some() {
        return 0x1F; // Check every 32 nodes for responsive stop handling
    }

    // Check if we have FixedNodes in either limits or time manager
    if let TimeControl::FixedNodes { .. } = &searcher.context.limits().time_control {
        return 0x3F; // Check every 64 nodes
    }

    // Check if we're in ponder mode - need frequent polling for ponderhit
    if matches!(&searcher.context.limits().time_control, TimeControl::Ponder(_)) {
        return 0x3F; // Check every 64 nodes for responsive ponderhit detection
    }

    // Special handling for Byoyomi time control - need more frequent checks
    if let Some(tm) = &searcher.time_manager {
        if let TimeControl::Byoyomi { .. } = tm.time_control() {
            // For Byoyomi, check more frequently due to strict time limits
            return 0x1F; // Check every 32 nodes for responsive byoyomi handling
        }
    }

    // For time-based controls, use adaptive intervals based on soft limit
    if let Some(tm) = &searcher.time_manager {
        // Check if TimeManager is in ponder mode (soft_limit would be u64::MAX)
        let soft_limit = tm.soft_limit_ms();
        if soft_limit == u64::MAX {
            // Ponder mode or infinite search - need frequent polling
            return 0x3F; // Check every 64 nodes
        }

        match soft_limit {
            0..=50 => 0x1F,    // ≤50ms → 32 nodes
            51..=100 => 0x3F,  // ≤100ms → 64 nodes
            101..=200 => 0x7F, // ≤200ms → 128 nodes
            201..=500 => 0xFF, // ≤0.5s → 256 nodes
            _ => 0x3FF,        // default 1024 nodes
        }
    } else {
        // For searches without TimeManager (infinite search, depth-only, etc)
        // Use more frequent polling to ensure responsive stop command handling
        0x7F // Check every 128 nodes for better responsiveness
    }
}
