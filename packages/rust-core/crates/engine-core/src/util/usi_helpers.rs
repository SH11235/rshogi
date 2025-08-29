use crate::movegen::MoveGenerator;
use crate::shogi::{Move, Position};
use crate::usi::{move_to_usi, position_to_sfen};

/// Check if a USI move string is legal in the given position.
///
/// This function is tolerant to a specific USI edge: it treats a move with a promotion flag
/// as matching a non-promoting legal move with same from/to (for cases where `+` is present
/// even when promotion is impossible). This mirrors CLI-side behavior to avoid regressions.
pub fn is_legal_usi_move(position: &Position, usi_move: &str) -> bool {
    // Parse the USI move
    let Ok(parsed_move) = crate::usi::parse_usi_move(usi_move) else {
        return false;
    };

    // Generate legal moves and compare semantically
    let movegen = MoveGenerator::new();
    let Ok(legal_moves) = movegen.generate_all(position) else {
        return false;
    };

    legal_moves.as_slice().iter().any(|&legal_move| {
        // Basic move matching
        parsed_move.from() == legal_move.from()
            && parsed_move.to() == legal_move.to()
            && parsed_move.is_drop() == legal_move.is_drop()
            && (!parsed_move.is_drop()
                || parsed_move.drop_piece_type() == legal_move.drop_piece_type())
            && (
                // Either promotion flags match exactly
                parsed_move.is_promote() == legal_move.is_promote()
                // OR the parsed move tries to promote but the legal move doesn't allow it
                // (This mirrors existing CLI tolerance for e.g. "2b8h+")
                || (parsed_move.is_promote() && !legal_move.is_promote())
            )
    })
}

/// Resolve a USI move string to a concrete legal Move for the given position.
///
/// - Prefers exact promotion flag match when both promoted and non-promoted are legal
/// - Falls back to the first legal match with same from/to (or drop target)
pub fn resolve_usi_move(position: &Position, usi_move: &str) -> Option<Move> {
    let Ok(parsed) = crate::usi::parse_usi_move(usi_move) else {
        return None;
    };
    let movegen = MoveGenerator::new();
    let Ok(legal_moves) = movegen.generate_all(position) else {
        return None;
    };

    let mut fallback: Option<Move> = None;
    for &lm in legal_moves.as_slice() {
        let matched = if parsed.is_drop() || lm.is_drop() {
            parsed.is_drop() == lm.is_drop()
                && parsed.drop_piece_type() == lm.drop_piece_type()
                && parsed.to() == lm.to()
        } else {
            parsed.from() == lm.from() && parsed.to() == lm.to()
        };

        if matched {
            if lm.is_promote() == parsed.is_promote() {
                return Some(lm);
            }
            if fallback.is_none() {
                fallback = Some(lm);
            }
        }
    }
    fallback
}

/// Normalize a USI move string to the engine's canonical USI for a legal Move.
/// Returns None if the move is not legal in the given position.
pub fn normalize_usi_move(position: &Position, usi_move: &str) -> Option<String> {
    resolve_usi_move(position, usi_move).map(|mv| move_to_usi(&mv))
}

/// A fully normalized move representation (Move and its canonical USI string)
pub struct NormalizedMove {
    pub mv: Move,
    pub usi: String,
}

/// Error type for USI normalization
#[derive(Debug, Clone)]
pub enum NormalizeError {
    Parse { input: String, message: String },
    NoLegalMatch { input: String },
    MovegenFailed { message: String },
}

/// Normalize a USI move string end-to-end with structured error logging.
///
/// - Parses the input string
/// - Resolves to a concrete legal Move (promotion flag preference handled)
/// - Returns both Move and canonical USI string
/// - On failure, logs a descriptive error and returns NormalizeError
pub fn normalize_usi_full(
    position: &Position,
    usi_move: &str,
) -> Result<NormalizedMove, NormalizeError> {
    // 1) Parse
    let parsed = match crate::usi::parse_usi_move(usi_move) {
        Ok(mv) => mv,
        Err(e) => {
            let pos_sfen = position_to_sfen(position);
            log::error!(
                "normalize_usi_full: parse_failed input='{}' error='{}' pos='{}'",
                usi_move,
                e,
                pos_sfen
            );
            return Err(NormalizeError::Parse {
                input: usi_move.to_string(),
                message: e.to_string(),
            });
        }
    };

    // 2) Generate legals
    let movegen = MoveGenerator::new();
    let legal_moves = match movegen.generate_all(position) {
        Ok(m) => m,
        Err(e) => {
            let pos_sfen = position_to_sfen(position);
            log::error!(
                "normalize_usi_full: movegen_failed input='{}' error='{}' pos='{}'",
                usi_move,
                e,
                pos_sfen
            );
            return Err(NormalizeError::MovegenFailed {
                message: e.to_string(),
            });
        }
    };

    // 3) Resolve to legal
    let mut fallback: Option<Move> = None;
    for &lm in legal_moves.as_slice() {
        let matched = if parsed.is_drop() || lm.is_drop() {
            parsed.is_drop() == lm.is_drop()
                && parsed.drop_piece_type() == lm.drop_piece_type()
                && parsed.to() == lm.to()
        } else {
            parsed.from() == lm.from() && parsed.to() == lm.to()
        };

        if matched {
            if lm.is_promote() == parsed.is_promote() {
                return Ok(NormalizedMove {
                    mv: lm,
                    usi: move_to_usi(&lm),
                });
            }
            if fallback.is_none() {
                fallback = Some(lm);
            }
        }
    }

    if let Some(mv) = fallback {
        return Ok(NormalizedMove {
            mv,
            usi: move_to_usi(&mv),
        });
    }

    // 4) No legal match
    let pos_sfen = position_to_sfen(position);
    log::error!("normalize_usi_full: no_legal_match input='{}' pos='{}'", usi_move, pos_sfen);
    Err(NormalizeError::NoLegalMatch {
        input: usi_move.to_string(),
    })
}

/// Convenience wrapper that returns only the normalized USI string and logs failures.
pub fn normalize_usi_move_str_logged(position: &Position, usi_move: &str) -> Option<String> {
    match normalize_usi_full(position, usi_move) {
        Ok(n) => Some(n.usi),
        Err(_e) => None, // errors were already logged in normalize_usi_full
    }
}
