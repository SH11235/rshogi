//! Move ordering utilities for search algorithms
//!
//! Provides common functions for ordering moves to improve alpha-beta pruning efficiency

use crate::shogi::PieceType;

/// Get victim score for MVV-LVA (Most Valuable Victim - Least Valuable Attacker) ordering
/// Higher value pieces get higher scores
///
/// This is used in multiple places:
/// - Quiescence search for ordering captures
/// - Main search for move ordering
/// - Other search algorithms that need capture ordering
#[inline]
pub const fn victim_score(pt: PieceType) -> i32 {
    match pt {
        PieceType::Pawn => 100,
        PieceType::Lance => 300,
        PieceType::Knight => 400,
        PieceType::Silver => 500,
        PieceType::Gold => 600,
        PieceType::Bishop => 800,
        PieceType::Rook => 1000,
        PieceType::King => 10000, // Should never happen in normal play
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_victim_score_ordering() {
        // Ensure pieces are ordered by value
        assert!(victim_score(PieceType::Pawn) < victim_score(PieceType::Lance));
        assert!(victim_score(PieceType::Lance) < victim_score(PieceType::Knight));
        assert!(victim_score(PieceType::Knight) < victim_score(PieceType::Silver));
        assert!(victim_score(PieceType::Silver) < victim_score(PieceType::Gold));
        assert!(victim_score(PieceType::Gold) < victim_score(PieceType::Bishop));
        assert!(victim_score(PieceType::Bishop) < victim_score(PieceType::Rook));
        assert!(victim_score(PieceType::Rook) < victim_score(PieceType::King));
    }

    #[test]
    fn test_victim_score_values() {
        // Test specific values to ensure consistency
        assert_eq!(victim_score(PieceType::Pawn), 100);
        assert_eq!(victim_score(PieceType::Lance), 300);
        assert_eq!(victim_score(PieceType::Knight), 400);
        assert_eq!(victim_score(PieceType::Silver), 500);
        assert_eq!(victim_score(PieceType::Gold), 600);
        assert_eq!(victim_score(PieceType::Bishop), 800);
        assert_eq!(victim_score(PieceType::Rook), 1000);
        assert_eq!(victim_score(PieceType::King), 10000);
    }
}
