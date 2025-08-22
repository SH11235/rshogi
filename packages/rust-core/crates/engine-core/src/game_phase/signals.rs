//! Signal computation for game phase detection

use crate::shogi::{Color, PieceType};
use crate::Position;

use super::config::{PhaseParameters, PhaseWeights};

/// Phase signals (continuous values 0.0 - 1.0)
#[derive(Debug, Clone, Copy)]
pub struct PhaseSignals {
    /// Material signal (0.0 = full material, 1.0 = no material)
    pub material: f32,
    /// Ply signal (0.0 = early game, 1.0 = late game)
    pub ply: f32,
}

/// Compute phase signals from position
#[inline]
pub fn compute_signals(
    pos: &Position,
    ply: u32,
    weights: &PhaseWeights,
    params: &PhaseParameters,
) -> PhaseSignals {
    let material = compute_material_signal(pos, weights);
    let ply_signal = compute_ply_signal(ply, params);

    PhaseSignals {
        material,
        ply: ply_signal,
    }
}

/// Compute material-based signal (0.0 = full material, 1.0 = no material)
fn compute_material_signal(pos: &Position, weights: &PhaseWeights) -> f32 {
    let mut total = 0u16;

    // Count pieces on board and in hand
    for &piece_type in &[
        PieceType::Rook,
        PieceType::Bishop,
        PieceType::Gold,
        PieceType::Silver,
        PieceType::Knight,
        PieceType::Lance,
    ] {
        let weight = weights.get_weight(piece_type);

        // On board (includes promoted pieces)
        let on_board = pos.count_piece_on_board(piece_type);

        // In hand
        let in_hand_black = pos.count_piece_in_hand(Color::Black, piece_type);
        let in_hand_white = pos.count_piece_in_hand(Color::White, piece_type);

        total += weight * (on_board + in_hand_black + in_hand_white);
    }

    // Normalize to 0.0-1.0 (inverted: 0.0 = full material, 1.0 = no material)
    let initial_total = weights.initial_total();
    debug_assert!(total <= initial_total, "Material total exceeds initial");

    1.0 - (total as f32 / initial_total as f32)
}

/// Compute ply-based signal (0.0 = early game, 1.0 = late game)
fn compute_ply_signal(ply: u32, params: &PhaseParameters) -> f32 {
    if ply <= params.ply_opening {
        // Early game (including exactly at ply_opening)
        0.0
    } else if ply >= params.ply_endgame {
        // Late game
        1.0
    } else {
        // Linear interpolation
        let range = params.ply_endgame - params.ply_opening;
        let progress = ply - params.ply_opening;
        progress as f32 / range as f32
    }
}

impl PhaseSignals {
    /// Compute combined score with given weights
    pub fn combined_score(&self, w_material: f32, w_ply: f32) -> f32 {
        w_material * self.material + w_ply * self.ply
    }
}
