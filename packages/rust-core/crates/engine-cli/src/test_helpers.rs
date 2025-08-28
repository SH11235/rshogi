//! Test helper functions for engine-cli tests

#[cfg(test)]
pub mod test_utils {
    use crate::types::ResignReason;
    use engine_core::shogi::Position;

    /// Check if ResignReason matches expected checkmate/no-legal-moves conditions
    pub fn verify_resign_reason(
        _position: &Position,
        has_legal_moves: bool,
        is_in_check: bool,
    ) -> ResignReason {
        if !has_legal_moves {
            if is_in_check {
                ResignReason::Checkmate
            } else {
                ResignReason::NoLegalMovesButNotInCheck
            }
        } else {
            panic!("Position has legal moves, should not resign");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_utils::*;
    use crate::types::ResignReason;

    #[test]
    fn test_verify_resign_reason_checkmate() {
        // Dummy position (not actually used in test helper)
        let pos = engine_core::shogi::Position::startpos();

        // Checkmate: no legal moves and in check
        let reason = verify_resign_reason(&pos, false, true);
        assert_eq!(reason, ResignReason::Checkmate);
    }

    #[test]
    fn test_verify_resign_reason_no_legal_moves() {
        let pos = engine_core::shogi::Position::startpos();

        // No legal moves but not in check (error condition)
        let reason = verify_resign_reason(&pos, false, false);
        assert_eq!(reason, ResignReason::NoLegalMovesButNotInCheck);
    }

    #[test]
    #[should_panic(expected = "Position has legal moves")]
    fn test_verify_resign_reason_panic() {
        let pos = engine_core::shogi::Position::startpos();

        // Should panic when there are legal moves
        verify_resign_reason(&pos, true, false);
    }
}
