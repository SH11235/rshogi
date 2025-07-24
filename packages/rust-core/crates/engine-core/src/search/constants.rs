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

/// Maximum additional plies for quiescence search
/// Limits the depth of capture-only search to avoid explosion
pub const QUIESCE_MAX_PLY: u8 = 4;

/// Aspiration window constants
pub const ASPIRATION_WINDOW_INITIAL: i32 = 50;
pub const ASPIRATION_WINDOW_DELTA: i32 = 50;
pub const ASPIRATION_RETRY_LIMIT: u32 = 4;

/// Time pressure threshold for search decisions
/// When remaining time < elapsed time * threshold, enter time pressure mode
pub const TIME_PRESSURE_THRESHOLD: f64 = 0.1;

/// Validate that constants maintain proper relationships
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
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
    }
    
    #[test]
    fn test_mate_distance_range() {
        // Ensure we can represent all mate distances
        let max_mate_distance = MAX_PLY as i32;
        assert!(MATE_SCORE - max_mate_distance > 0);
        assert!(MATE_SCORE + max_mate_distance < SEARCH_INF);
    }
}