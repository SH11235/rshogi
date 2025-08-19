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
/// Based on USI protocol default (depth 6) for compatibility
/// This value can be overridden by engine configuration if needed
pub const DEFAULT_SEARCH_DEPTH: u8 = 6;

/// Relative maximum depth for quiescence search
/// This is the primary limit for qsearch recursion depth
/// Note: This limit is not applied when in check to ensure proper check evasion
pub const MAX_QPLY: u8 = 32;

/// Absolute maximum depth for quiescence search
/// Safety limit to prevent stack overflow in extreme cases
/// This is now a secondary safeguard, increased from 32 to allow deeper main searches
pub const MAX_QUIESCE_DEPTH: u16 = 96;

/// Quiescence search evaluation penalty for check positions
/// Applied when in check at depth limit to make evaluation slightly pessimistic
pub const QUIESCE_CHECK_EVAL_PENALTY: i32 = 50;

/// Aspiration window constants
///
/// These values control the alpha-beta window narrowing optimization:
/// - Initial window: 25 centipawns provides a good balance between search reduction
///   and re-search frequency. Narrower windows (10-20) cause more re-searches,
///   wider windows (50+) reduce the optimization benefit.
/// - Delta: Minimum expansion of 25 ensures progress even with small score changes
/// - Expansion factor: 1.5x provides geometric growth that adapts to score volatility
/// - Retry limit: 3 attempts prevents excessive re-searching in volatile positions
///
/// These values are derived from:
/// 1. Empirical testing showing 20-30 cp windows work well for Shogi
/// 2. Common practice in strong engines (Stockfish, Komodo use similar ranges)
/// 3. The principle that window size should scale with position complexity
pub const ASPIRATION_WINDOW_INITIAL: i32 = 25; // 25 centipawns (0.25 pawn)
pub const ASPIRATION_WINDOW_DELTA: i32 = 25; // Minimum expansion step
pub const ASPIRATION_WINDOW_EXPANSION: f32 = 1.5; // Geometric growth rate
pub const ASPIRATION_RETRY_LIMIT: u32 = 3; // Max retries before full window

/// Maximum window adjustment based on volatility (prevents extreme windows)
pub const ASPIRATION_WINDOW_MAX_VOLATILITY_ADJUSTMENT: i32 = 100; // 1 pawn max adjustment

/// Maximum aspiration window size (4x initial window)
/// Prevents excessively wide windows that negate the optimization benefit
pub const ASPIRATION_WINDOW_MAX: i32 = 100;

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
pub const DEFAULT_QNODES_LIMIT: u64 = 1_000_000;

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

        // Ensure aspiration window constants are reasonable
        assert!(ASPIRATION_WINDOW_INITIAL > 0);
        assert!(ASPIRATION_WINDOW_DELTA > 0);
        assert!(ASPIRATION_WINDOW_MAX >= ASPIRATION_WINDOW_INITIAL);
        assert!(ASPIRATION_WINDOW_EXPANSION > 1.0);
        assert!(ASPIRATION_RETRY_LIMIT > 0);
    }

    #[test]
    fn test_mate_distance_range() {
        // Ensure we can represent all mate distances
        let max_mate_distance = MAX_PLY as i32;
        assert!(MATE_SCORE - max_mate_distance > 0);
        assert!(MATE_SCORE + max_mate_distance < SEARCH_INF);
    }
}
