//! Test helper functions for engine-cli tests

#[cfg(test)]
pub mod test_utils {
    use crate::types::ResignReason;
    use crate::usi::output::SearchInfo;
    use engine_core::shogi::Position;
    use once_cell::sync::Lazy;

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

    /// Create a test SearchInfo with minimal fields
    pub fn make_test_search_info(depth: u8) -> SearchInfo {
        SearchInfo {
            depth: Some(depth as u32),
            time: Some(0),
            nodes: Some(0),
            string: Some("test".to_string()),
            ..Default::default()
        }
    }

    /// Ensure engine static tables are initialized for tests
    ///
    /// This function ensures that the engine's static tables are initialized
    /// exactly once across all tests. Since we removed initialization from
    /// EngineAdapter::new(), tests need to call this to ensure proper setup.
    ///
    /// # Usage
    ///
    /// Call this at the beginning of any test that uses EngineAdapter:
    /// ```
    /// #[test]
    /// fn my_test() {
    ///     ensure_engine_initialized();
    ///     let adapter = EngineAdapter::new();
    ///     // ... test code ...
    /// }
    /// ```
    pub fn ensure_engine_initialized() {
        static INIT: Lazy<()> = Lazy::new(|| {
            engine_core::init_engine_tables();
        });
        // Force evaluation of the lazy static
        Lazy::force(&INIT);
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
