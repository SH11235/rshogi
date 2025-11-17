use crate::movegen::MoveGenerator;
use crate::search::mate1ply;
use crate::shogi::{Move, Position};

/// Summary of root escape scan results.
#[derive(Clone, Debug, Default)]
pub struct RootEscapeSummary {
    pub safe: Vec<Move>,
    pub risky: Vec<(Move, Move)>, // (our_move, enemy_mate_mv)
}

impl RootEscapeSummary {
    /// Returns true when `mv` is in the safe set.
    #[inline]
    pub fn is_safe(&self, mv: Move) -> bool {
        self.safe.contains(&mv)
    }

    /// Returns the opponent mate move if `mv` is classified as risky.
    #[inline]
    pub fn risky_mate_move(&self, mv: Move) -> Option<Move> {
        self.risky
            .iter()
            .find_map(|&(candidate, mate)| (candidate == mv).then_some(mate))
    }
}

/// Runs a Root Escape scan. `max_moves` limits the number of generated moves inspected.
pub fn root_escape_scan(pos: &Position, max_moves: Option<usize>) -> RootEscapeSummary {
    let mut summary = RootEscapeSummary::default();
    let limit = max_moves.unwrap_or(usize::MAX);
    if limit == 0 {
        return summary;
    }

    let generator = MoveGenerator::new();
    let Ok(moves) = generator.generate_all(pos) else {
        return summary;
    };
    let scan_limit = limit.min(moves.as_slice().len());
    let mut scratch = pos.clone();
    for &mv in moves.as_slice().iter().take(scan_limit) {
        if !pos.is_legal_move(mv) {
            continue;
        }
        if let Some(mate_mv) = mate1ply::enemy_mate_in_one_after(&mut scratch, mv) {
            summary.risky.push((mv, mate_mv));
        } else {
            summary.safe.push(mv);
        }
    }
    summary
}
