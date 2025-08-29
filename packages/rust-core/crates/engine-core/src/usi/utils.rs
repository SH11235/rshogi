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

#[cfg(test)]
mod tests {
    use super::canonicalize_position_cmd;

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
}

