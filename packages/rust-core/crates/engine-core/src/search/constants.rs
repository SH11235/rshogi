//! Common constants for search algorithms

/// Infinity score for search bounds (must fit comfortably in i32)
pub const SEARCH_INF: i32 = 32_000;

/// Mate score threshold (below infinity to allow mate distance calculation)
pub const MATE_SCORE: i32 = SEARCH_INF - 2_000;

/// Draw score (neutral evaluation)
pub const DRAW_SCORE: i32 = 0;

/// Special value to indicate search was interrupted (sentinel value)
/// Must be outside the valid score range [-SEARCH_INF, SEARCH_INF]
pub const SEARCH_INTERRUPTED: i32 = SEARCH_INF + 1;

/// Maximum ply depth from root position
/// Used for PV table size and mate distance pruning
pub const MAX_PLY: usize = 127;

/// Default search depth when not specified
/// Raised to 32 so MinThink を満たすまで反復が継続できるようにする
pub const DEFAULT_SEARCH_DEPTH: u8 = 32;

/// Relative maximum depth for quiescence search
/// This is the primary limit for qsearch recursion depth
/// Note: This limit is not applied when in check to ensure proper check evasion
pub const MAX_QPLY: u8 = 32;

/// Absolute maximum depth for quiescence search
/// Safety limit to prevent stack overflow in extreme cases
/// This is now a secondary safeguard, increased from 32 to allow deeper main searches
pub const MAX_QUIESCE_DEPTH: u16 = 96;

/// Aspiration window constants used in iterative deepening
///
/// These values control the alpha-beta window narrowing optimization:
/// - Initial delta: 30 centipawns provides a good balance between search reduction
///   and re-search frequency for Shogi positions
/// - Maximum delta: 350 centipawns caps the window expansion to prevent
///   excessively wide windows in volatile positions
///
/// The window starts narrow and expands geometrically on fail-high/fail-low,
/// providing good performance across diverse position types.
///
/// These values are empirically tuned for the ClassicBackend implementation.
pub const ASPIRATION_DELTA_INITIAL: i32 = 30; // Initial window half-width (centipawns)
pub const ASPIRATION_DELTA_MAX: i32 = 350; // Maximum window expansion limit

/// Time pressure threshold for search decisions
/// When remaining time < elapsed time * threshold, enter time pressure mode
pub const TIME_PRESSURE_THRESHOLD: f64 = 0.1;

/// Time check masks - check every N nodes using bitwise AND
/// These masks control how frequently we check time limits during search
/// Using bitwise AND with node count is extremely fast (single CPU instruction)
pub const TIME_CHECK_MASK_NORMAL: u64 = 0x1FFF; // 8192 nodes - for normal time controls
pub const TIME_CHECK_MASK_BYOYOMI: u64 = 0x7FF; // 2048 nodes - more frequent for byoyomi
pub const EVENT_CHECK_MASK: u64 = 0x1FFF; // 8192 nodes - for ponder hit events

/// Default quiescence search node limit (1 million nodes)
/// This prevents explosion in complex positions with many captures.
/// Can be overridden with SearchLimits::builder().qnodes_limit()
pub const DEFAULT_QNODES_LIMIT: u64 = 300_000;
pub const MIN_QNODES_LIMIT: u64 = 10_000;
pub const QNODES_PER_MS: u64 = 10;
pub const QNODES_DEPTH_BONUS_PCT: u64 = 5;

/// Near-deadline window (ms) used by lightweight time polling to increase
/// responsiveness as we approach either the hard limit or a scheduled
/// rounded stop time.
pub const NEAR_DEADLINE_WINDOW_MS: u64 = 50;

/// Lightweight polling interval (ms) for AB/QS time checks when not inside
/// the near-deadline window.
pub const LIGHT_POLL_INTERVAL_MS: u64 = 8;

/// Main-thread guard window before starting a new iteration or distributing work.
/// If we are within this window of the planned or hard deadline, we avoid
/// starting a new heavy iteration to guarantee timely self-stop.
pub const MAIN_NEAR_DEADLINE_WINDOW_MS: u64 = 500;

/// Window (ms) before the hard deadline at which we proactively finalize
/// the current best move and exit without waiting for GUI stop.
pub const NEAR_HARD_FINALIZE_MS: u64 = 500;

/// Minimum depth for helper thread snapshot publication.
/// Helper snapshots shallower than this are suppressed to reduce USI noise
/// and avoid reporting low-quality partial results.
/// This constant is shared between engine-core (parallel search) and engine-usi (finalize).
pub const HELPER_SNAPSHOT_MIN_DEPTH: u32 = 3;

/// Validate that constants maintain proper relationships
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn test_constant_relationships() {
        // Ensure mate score is below infinity
        assert!(MATE_SCORE < SEARCH_INF);
        assert!(MATE_SCORE >= SEARCH_INF - 2000); // Leave room for mate distances

        // Ensure sentinel value is outside valid range
        assert!(SEARCH_INTERRUPTED > SEARCH_INF);

        // Ensure MAX_PLY fits in u8 for efficient storage
        assert!(MAX_PLY <= u8::MAX as usize);

        // Ensure draw score is neutral
        assert_eq!(DRAW_SCORE, 0);

        // Ensure aspiration delta constants are reasonable
        assert!(ASPIRATION_DELTA_INITIAL > 0);
        assert!(ASPIRATION_DELTA_MAX >= ASPIRATION_DELTA_INITIAL);
    }

    #[test]
    fn test_mate_distance_range() {
        // Ensure we can represent all mate distances
        let max_mate_distance = MAX_PLY as i32;
        assert!(MATE_SCORE - max_mate_distance > 0);
        assert!(MATE_SCORE + max_mate_distance < SEARCH_INF);
    }
}
