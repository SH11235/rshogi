//! Type definitions for move picker

use crate::shogi::Move;

/// Scored move for ordering
#[derive(Clone, Copy, Debug)]
pub(crate) struct ScoredMove {
    pub mv: Move,
    pub score: i32,
}

impl ScoredMove {
    pub fn new(mv: Move, score: i32) -> Self {
        ScoredMove { mv, score }
    }
}

/// Stage of move generation
///
/// The order is carefully chosen based on the probability of causing a beta cutoff:
/// 1. TT move - Most likely to be the best move from previous search
/// 2. Good captures - Positive SEE captures are often good moves
/// 3. Killers - Moves that caused cutoffs at the same depth
/// 4. Quiet moves - Non-captures ordered by history heuristic
/// 5. Bad captures - Negative SEE captures (least likely to be good)
///
/// Bad captures are intentionally placed last because:
/// - They have negative SEE value (lose material)
/// - They rarely cause beta cutoffs
/// - In quiescence search, they are often skipped entirely
/// - Late Move Reductions (LMR) work better with bad moves at the end
///
/// This ordering matches strong engines like Stockfish and maximizes
/// the efficiency of alpha-beta pruning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MovePickerStage {
    /// Root PV move (highest priority at root node)
    RootPV,
    /// TT move
    TTMove,
    /// Generate captures
    GenerateCaptures,
    /// Good captures (SEE >= 0)
    GoodCaptures,
    /// Killer moves
    Killers,
    /// Generate quiet moves
    GenerateQuiets,
    /// All quiet moves
    QuietMoves,
    /// Bad captures (SEE < 0)
    BadCaptures,
    /// End of moves
    End,
}
