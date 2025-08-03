//! TT prefetching strategies for improved cache performance

use crate::{
    engine::zobrist::ZOBRIST,
    evaluation::evaluate::Evaluator,
    search::unified::UnifiedSearcher,
    shogi::{Move, Position},
};

impl<E, const USE_TT: bool, const USE_PRUNING: bool, const TT_SIZE_MB: usize>
    UnifiedSearcher<E, USE_TT, USE_PRUNING, TT_SIZE_MB>
where
    E: Evaluator + Send + Sync + 'static,
{
    /// Prefetch TT entries for the next moves to be searched
    /// This is called after move ordering but before starting the search loop
    #[inline]
    pub(crate) fn prefetch_next_moves(&self, pos: &Position, moves: &[Move], max_prefetch: usize) {
        if !USE_TT {
            return;
        }

        // Prefetch up to max_prefetch moves
        let prefetch_count = moves.len().min(max_prefetch);

        // For efficient prefetching, we use the incremental zobrist hash update
        // This avoids the cost of actually making moves
        for &mv in moves.iter().take(prefetch_count) {
            // Calculate more accurate hash after move using actual Zobrist tables
            let approx_hash = if mv.is_drop() {
                // For drops, XOR in the piece at the destination
                let piece_type = mv.drop_piece_type();
                let to = mv.to();
                let color = pos.side_to_move;

                // Use actual Zobrist table for accurate hash
                let piece = crate::shogi::Piece::new(piece_type, color);
                let piece_hash = ZOBRIST.piece_square_hash(piece, to);

                // Also account for hand piece removal (approximate)
                let hand_hash = ZOBRIST.hand_hash(color, piece_type, 1);

                // Update hash with side to move change
                pos.zobrist_hash ^ piece_hash ^ hand_hash ^ ZOBRIST.side_to_move
            } else {
                // For normal moves, use more accurate hash calculation
                let from = mv.from().unwrap();
                let to = mv.to();

                // Get the moving piece
                if let Some(piece) = pos.board.piece_on(from) {
                    let from_hash = ZOBRIST.piece_square_hash(piece, from);
                    let to_hash = ZOBRIST.piece_square_hash(piece, to);

                    // Handle promotion if applicable
                    let to_hash = if mv.is_promote() && piece.piece_type.can_promote() {
                        let promoted = piece.promote();
                        ZOBRIST.piece_square_hash(promoted, to)
                    } else {
                        to_hash
                    };

                    // Check for capture
                    let capture_hash = if let Some(captured) = pos.board.piece_on(to) {
                        ZOBRIST.piece_square_hash(captured, to)
                    } else {
                        0
                    };

                    // Update hash with side to move change
                    pos.zobrist_hash ^ from_hash ^ to_hash ^ capture_hash ^ ZOBRIST.side_to_move
                } else {
                    // Fallback to simple hash if piece not found
                    pos.zobrist_hash ^ (from.index() as u64) ^ (to.index() as u64)
                }
            };

            // Prefetch the TT entry for this approximate hash
            self.prefetch_tt(approx_hash);
        }
    }

    /// Prefetch TT entries during move generation
    /// This allows prefetching to happen while CPU is busy with move generation
    #[inline]
    #[allow(dead_code)]
    pub(crate) fn prefetch_during_movegen(&self, pos: &Position) {
        if !USE_TT {
            return;
        }

        // Prefetch current position's TT entry
        self.prefetch_tt(pos.zobrist_hash);

        // Also prefetch some common child positions
        // These are positions we're likely to explore soon
        // Use simple hash perturbations that approximate common moves
        self.prefetch_tt(pos.zobrist_hash ^ 0x1234); // Approximate pawn move
        self.prefetch_tt(pos.zobrist_hash ^ 0x5678); // Approximate piece move
    }

    /// Prefetch TT entries for positions in the principal variation
    /// Called when we have a PV from previous iteration
    #[inline]
    #[allow(dead_code)]
    pub(crate) fn prefetch_pv_line(&self, pos: &Position, pv: &[Move]) {
        if !USE_TT || pv.is_empty() {
            return;
        }

        let mut current_hash = pos.zobrist_hash;

        // Prefetch first few moves in PV
        for (i, &_mv) in pv.iter().take(4).enumerate() {
            // Use simplified hash update for PV moves
            current_hash ^= (i as u64 + 1) * 0x9E3779B97F4A7C15; // Golden ratio constant
            self.prefetch_tt(current_hash);
        }
    }
}
