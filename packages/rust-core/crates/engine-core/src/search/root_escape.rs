use crate::movegen::MoveGenerator;
use crate::search::{mate1ply, root_threat};
use crate::shogi::{Move, Position};

/// Summary of root escape scan results.
#[derive(Clone, Debug, Default)]
pub struct RootEscapeSummary {
    pub safe: Vec<Move>,
    pub risky: Vec<(Move, Move)>,    // (our_move, enemy_mate_mv)
    pub see_risky: Vec<(Move, i32)>, // (our_move, loss_cp)
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

    /// Returns SEE loss if `mv` is classified as static risky.
    #[inline]
    pub fn see_loss(&self, mv: Move) -> Option<i32> {
        self.see_risky
            .iter()
            .find_map(|&(candidate, loss)| (candidate == mv).then_some(loss))
    }

    #[inline]
    pub fn remove_safe(&mut self, mv: Move) {
        if let Some(idx) = self.safe.iter().position(|&m| m == mv) {
            self.safe.swap_remove(idx);
        }
    }

    #[inline]
    pub fn push_see_risky(&mut self, mv: Move, loss: i32) {
        self.see_risky.push((mv, loss));
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

pub fn apply_static_risks(pos: &Position, summary: &mut RootEscapeSummary, threshold_cp: i32) {
    if threshold_cp <= 0 {
        return;
    }
    let mut promote: Vec<(Move, i32)> = Vec::new();
    for &mv in summary.safe.clone().iter() {
        if let Some(loss) = see_loss_for_move(pos, mv) {
            if loss <= -threshold_cp {
                promote.push((mv, loss));
            }
        }
    }
    for (mv, loss) in promote {
        summary.remove_safe(mv);
        summary.push_see_risky(mv, loss);
    }
}

pub fn see_loss_for_move(pos: &Position, mv: Move) -> Option<i32> {
    if mv.is_drop() {
        return None;
    }
    let see = if mv.is_capture_hint() {
        pos.see(mv)
    } else {
        pos.see_landing_after_move(mv, 0)
    };
    (see < 0).then_some(see)
}

pub fn apply_threat_risks(
    pos: &Position,
    summary: &mut RootEscapeSummary,
    candidates: &[Move],
    max_candidates: usize,
    threshold_cp: i32,
) {
    if threshold_cp <= 0 || candidates.is_empty() {
        return;
    }
    let us = pos.side_to_move;
    let mut child = pos.clone();
    let mut evaluated = 0usize;
    for &mv in candidates {
        if max_candidates != usize::MAX && evaluated >= max_candidates {
            break;
        }
        if !summary.is_safe(mv) {
            continue;
        }
        evaluated += 1;
        let undo = child.do_move(mv);
        if let Some(threat) = root_threat::detect_major_threat(&child, us, threshold_cp) {
            let loss = threat_loss(threat, threshold_cp);
            summary.remove_safe(mv);
            summary.push_see_risky(mv, loss);
        }
        child.undo_move(mv, undo);
    }
}

fn threat_loss(threat: root_threat::RootThreat, threshold_cp: i32) -> i32 {
    match threat {
        root_threat::RootThreat::OppXsee { loss, .. } => loss,
        root_threat::RootThreat::PawnDropHead { .. } => -threshold_cp.max(1),
    }
}
