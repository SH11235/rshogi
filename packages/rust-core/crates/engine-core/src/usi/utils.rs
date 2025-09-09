/// Canonicalize a position command for reliable storage and comparison.
///
/// Normalizes the position command string by:
/// - Removing extra whitespace in SFEN
/// - Ensuring consistent token separation
/// - Preserving move order and trimming each move token
///
/// # Arguments
/// * `startpos` - Whether this is the starting position
/// * `sfen` - Optional SFEN string (mutually exclusive with startpos)
/// * `moves` - List of moves from the position
///
/// # Returns
/// A normalized position command string like:
/// - "position startpos"
/// - "position sfen <sfen> moves <m1> <m2> ..."
pub fn canonicalize_position_cmd(startpos: bool, sfen: Option<&str>, moves: &[String]) -> String {
    let mut cmd = String::from("position");

    if startpos {
        cmd.push_str(" startpos");
    } else if let Some(sfen_str) = sfen {
        cmd.push_str(" sfen ");
        // Normalize SFEN by trimming and collapsing multiple spaces
        let normalized_sfen = sfen_str.split_whitespace().collect::<Vec<_>>().join(" ");
        cmd.push_str(&normalized_sfen);
    }

    if !moves.is_empty() {
        cmd.push_str(" moves");
        for mv in moves {
            cmd.push(' ');
            // Trim each move to ensure no extra whitespace
            cmd.push_str(mv.trim());
        }
    }

    cmd
}

use crate::movegen::MoveGenerator;
use crate::search::types::NodeType;
use crate::shogi::{Move, Position};
use crate::usi::{parse_sfen, parse_usi_move, position_to_sfen};
use anyhow::{anyhow, Result};
use log::debug;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreSource {
    Command,
    Snapshot,
}

/// Create a Position from USI position command arguments.
///
/// This mirrors the CLI-side implementation but lives in core for reuse.
pub fn create_position(startpos: bool, sfen: Option<&str>, moves: &[String]) -> Result<Position> {
    // Create initial position
    let mut pos = if startpos {
        Position::startpos()
    } else if let Some(sfen_str) = sfen {
        parse_sfen(sfen_str).map_err(|e| anyhow!(e))?
    } else {
        return Err(anyhow!(
            "Position command must specify either 'startpos' or 'sfen <fen_string>'"
        ));
    };

    // Apply moves with validation
    let move_gen = MoveGenerator::new();

    for (i, move_str) in moves.iter().enumerate() {
        let mv = parse_usi_move(move_str).map_err(|e| anyhow!(e))?;

        // Generate all legal moves for current position
        let legal_moves = move_gen
            .generate_all(&pos)
            .map_err(|e| anyhow!("Failed to generate legal moves: {}", e))?;

        // Find matching legal move with promotion priority
        let mut fallback: Option<Move> = None;
        let mut exact: Option<Move> = None;

        for &lm in legal_moves.as_slice() {
            let matched = if mv.is_drop() || lm.is_drop() {
                // Drop moves: match to square and drop piece type
                mv.is_drop() == lm.is_drop()
                    && mv.drop_piece_type() == lm.drop_piece_type()
                    && mv.to() == lm.to()
            } else {
                // Normal moves: match from and to squares
                mv.from() == lm.from() && mv.to() == lm.to()
            };

            if matched {
                if lm.is_promote() == mv.is_promote() {
                    // Found exact promotion match
                    exact = Some(lm);
                    break;
                }
                if fallback.is_none() {
                    // Store first match as fallback
                    fallback = Some(lm);
                }
            }
        }

        let legal_move = exact.or(fallback);

        if let Some(legal_mv) = legal_move {
            // Log if USI specified promotion but it's not possible
            if mv.is_promote() && !legal_mv.is_promote() {
                debug!(
                    "Move {} specified promotion (+) but piece cannot promote at this position",
                    move_str
                );
            }
            // Use the actual legal move which has correct piece type and capture info
            pos.do_move(legal_mv);
        } else {
            debug!(
                "Parsed move details: from={:?}, to={:?}, drop={}, promote={}",
                mv.from(),
                mv.to(),
                mv.is_drop(),
                mv.is_promote()
            );

            // Additional diagnostics for nearby legal moves
            let mut found_from_square = false;
            for &legal_mv in legal_moves.as_slice() {
                if legal_mv.from() == mv.from() {
                    found_from_square = true;
                    if legal_mv.to() == mv.to() {
                        debug!(
                            "Found similar legal move: from={:?}, to={:?}, drop={}, promote={}",
                            legal_mv.from(),
                            legal_mv.to(),
                            legal_mv.is_drop(),
                            legal_mv.is_promote()
                        );
                    }
                }
            }
            if !found_from_square {
                debug!("No legal moves found from square {:?}", mv.from());
                debug!("First 10 legal moves:");
                for (i, &legal_mv) in legal_moves.as_slice().iter().take(10).enumerate() {
                    debug!("  {}: from={:?}, to={:?}", i, legal_mv.from(), legal_mv.to());
                }
            }

            return Err(anyhow!(
                "Illegal move '{}' at move {} in position after: {} (parsed: {:?}, side_to_move: {:?}, legal_moves_count: {}, sfen: {})",
                move_str,
                i + 1,
                if i == 0 { "initial position".to_string() } else { format!("{i} moves") },
                mv,
                pos.side_to_move,
                legal_moves.len(),
                position_to_sfen(&pos)
            ));
        }
    }

    Ok(pos)
}

/// Rebuild a position from USI position args and verify its root hash.
pub fn rebuild_and_verify(
    startpos: bool,
    sfen: Option<&str>,
    moves: &[String],
    expected_hash: u64,
) -> Result<Position> {
    let pos = create_position(startpos, sfen, moves)?;
    if pos.zobrist_hash() == expected_hash {
        Ok(pos)
    } else {
        Err(anyhow!(
            "hash mismatch after rebuild: expected={:#016x} actual={:#016x}",
            expected_hash,
            pos.zobrist_hash()
        ))
    }
}

/// Restore a position from SFEN snapshot and verify its root hash.
pub fn restore_snapshot_and_verify(sfen_snapshot: &str, expected_hash: u64) -> Result<Position> {
    let pos = parse_sfen(sfen_snapshot).map_err(|e| anyhow!(e))?;
    if pos.zobrist_hash() == expected_hash {
        Ok(pos)
    } else {
        Err(anyhow!(
            "hash mismatch after snapshot restore: expected={:#016x} actual={:#016x}",
            expected_hash,
            pos.zobrist_hash()
        ))
    }
}

/// Try to rebuild from command; if that fails, try snapshot; verify hash in either case.
pub fn rebuild_then_snapshot_fallback(
    startpos: bool,
    sfen: Option<&str>,
    moves: &[String],
    sfen_snapshot: Option<&str>,
    expected_hash: u64,
) -> Result<(Position, RestoreSource)> {
    if let Ok(pos) = rebuild_and_verify(startpos, sfen, moves, expected_hash) {
        return Ok((pos, RestoreSource::Command));
    }
    if let Some(snapshot) = sfen_snapshot {
        if let Ok(pos) = restore_snapshot_and_verify(snapshot, expected_hash) {
            return Ok((pos, RestoreSource::Snapshot));
        }
    }
    Err(anyhow!("rebuild_then_snapshot_fallback: both rebuild and snapshot failed"))
}

/// Normalized USI score view (cp or signed mate distance)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoreView {
    /// Centipawn score from side-to-move perspective
    Cp(i32),
    /// Signed mate distance in plies (positive = giving mate, negative = getting mated)
    Mate(i32),
}

/// Append normalized `score ... [lowerbound|upperbound]` tokens to `out`.
/// - Places bound tag immediately after the score in accordance with common USI practice.
pub fn append_usi_score_and_bound(out: &mut String, view: ScoreView, bound: NodeType) {
    match view {
        ScoreView::Cp(cp) => {
            out.push_str(&format!(" score cp {}", cp));
        }
        ScoreView::Mate(d) => {
            // Normalize -0 to 0 to avoid "-0"
            let dz = if d == 0 { 0 } else { d };
            out.push_str(&format!(" score mate {}", dz));
        }
    }

    match bound {
        NodeType::UpperBound => out.push_str(" upperbound"),
        NodeType::LowerBound => out.push_str(" lowerbound"),
        _ => {}
    }
}

/// Convert an engine-internal score into a ScoreView
/// - Mate scores become signed mate distances (plies)
/// - Others are passed through as cp (assumed side-to-move perspective)
pub fn score_view_from_internal(raw: i32) -> ScoreView {
    if crate::search::common::is_mate_score(raw) {
        let d = crate::search::common::extract_mate_distance(raw).unwrap_or(0) as i32;
        let sign = if raw >= 0 { 1 } else { -1 };
        ScoreView::Mate(sign * d)
    } else {
        ScoreView::Cp(raw)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::common::{extract_mate_distance, is_mate_score, mate_score};

    #[test]
    fn test_canonicalize_position_cmd_startpos() {
        let cmd = canonicalize_position_cmd(true, None, &[]);
        assert_eq!(cmd, "position startpos");

        let cmd = canonicalize_position_cmd(true, None, &["7g7f".to_string(), "3c3d".to_string()]);
        assert_eq!(cmd, "position startpos moves 7g7f 3c3d");
    }

    #[test]
    fn test_canonicalize_position_cmd_sfen() {
        let sfen = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
        let cmd = canonicalize_position_cmd(false, Some(sfen), &[]);
        assert_eq!(
            cmd,
            "position sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1"
        );
    }

    #[test]
    fn test_canonicalize_position_cmd_normalization() {
        // Test with extra spaces in SFEN
        let sfen = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL  b   -  1";
        let cmd = canonicalize_position_cmd(false, Some(sfen), &[]);
        assert_eq!(
            cmd,
            "position sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1"
        );

        // Test with moves containing extra spaces
        let moves = vec![" 7g7f ".to_string(), "3c3d  ".to_string()];
        let cmd = canonicalize_position_cmd(true, None, &moves);
        assert_eq!(cmd, "position startpos moves 7g7f 3c3d");
    }

    #[test]
    fn test_append_usi_score_cp_exact() {
        let mut s = String::from("info");
        append_usi_score_and_bound(&mut s, ScoreView::Cp(123), NodeType::Exact);
        assert!(s.contains(" score cp 123"));
        assert!(!s.contains("upperbound"));
        assert!(!s.contains("lowerbound"));
    }

    #[test]
    fn test_append_usi_score_cp_with_bounds() {
        let mut s1 = String::from("info");
        append_usi_score_and_bound(&mut s1, ScoreView::Cp(-50), NodeType::UpperBound);
        assert!(s1.contains(" score cp -50 upperbound"));

        let mut s2 = String::from("info");
        append_usi_score_and_bound(&mut s2, ScoreView::Cp(80), NodeType::LowerBound);
        assert!(s2.contains(" score cp 80 lowerbound"));
    }

    #[test]
    fn test_append_usi_score_mate_signed() {
        let mut s = String::from("info");
        append_usi_score_and_bound(&mut s, ScoreView::Mate(3), NodeType::Exact);
        assert!(s.contains(" score mate 3"));
    }

    #[test]
    fn test_append_usi_score_mate_negative() {
        let mut s = String::from("info");
        append_usi_score_and_bound(&mut s, ScoreView::Mate(-3), NodeType::Exact);
        assert!(s.contains(" score mate -3"));
    }

    #[test]
    fn test_append_usi_score_mate_zero_normalization() {
        let mut s = String::from("info");
        append_usi_score_and_bound(&mut s, ScoreView::Mate(0), NodeType::Exact);
        assert!(s.contains(" score mate 0"));
        assert!(!s.contains(" score mate -0"));
    }

    #[test]
    fn test_mate_detection_helpers() {
        let win2 = mate_score(2, true);
        assert!(is_mate_score(win2));
        assert_eq!(extract_mate_distance(win2), Some(2));
        let lose1 = mate_score(1, false);
        assert!(is_mate_score(lose1));
        assert_eq!(extract_mate_distance(lose1), Some(1));
    }
}
