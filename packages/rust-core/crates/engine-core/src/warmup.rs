//! Warmup initialization for static tables
//!
//! This module provides early initialization of static tables before
//! I/O setup to prevent deadlocks in subprocess contexts.

use crate::shogi::attacks;
use crate::Square;

/// Warm up all static tables without any I/O or logging
///
/// This function must be called before any I/O setup in subprocess contexts
/// to prevent initialization deadlocks. It initializes:
/// - Zobrist hash tables
/// - Attack tables
/// - Board representation tables
///
/// # Safety
/// - No logging or I/O operations
/// - No environment variable access during initialization
/// - Thread-safe (can be called multiple times)
pub fn warm_up_static_tables() {
    // Initialize Zobrist tables by creating a position
    // Note: This will trigger ZOBRIST lazy_static initialization
    let _ = crate::Position::startpos();

    // Initialize attack tables by calling each type
    // This triggers ATTACK_TABLES lazy_static initialization
    let center = Square::new(4, 4); // 5e
    let _ = attacks::king_attacks(center);
    let _ = attacks::gold_attacks(center, crate::Color::Black);
    let _ = attacks::silver_attacks(center, crate::Color::Black);
    let _ = attacks::knight_attacks(center, crate::Color::Black);
    let _ = attacks::pawn_attacks(center, crate::Color::Black);
    let _ = attacks::lance_attacks(center, crate::Color::Black);
    // Sliding attacks for rook and bishop
    let _ = attacks::sliding_attacks(center, crate::Bitboard::EMPTY, crate::PieceType::Rook);
    let _ = attacks::sliding_attacks(center, crate::Bitboard::EMPTY, crate::PieceType::Bishop);

    // Note: We intentionally do NOT create MoveGen here
    // to avoid circular dependencies
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_warmup_safe_multiple_calls() {
        // Should be safe to call multiple times
        warm_up_static_tables();
        warm_up_static_tables();
        warm_up_static_tables();
    }
}
