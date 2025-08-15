//! Search session management for thread-safe bestmove handling
//!
//! This module provides SearchSession to encapsulate all search-related data
//! within a single worker thread, preventing cross-thread contamination.

use engine_core::shogi::Move;
use smallvec::SmallVec;

/// Score representation that preserves cp/mate distinction
#[derive(Clone, Debug)]
pub enum Score {
    /// Centipawn score
    Cp(i32),
    /// Mate in N moves (positive = winning, negative = losing)
    Mate(i32),
}

impl Score {
    /// Convert from raw engine score
    pub fn from_raw(score: i32) -> Self {
        use engine_core::search::constants::{MATE_SCORE, MAX_PLY};

        if score.abs() >= MATE_SCORE - MAX_PLY as i32 {
            // It's a mate score - calculate mate distance
            let mate_in_half = MATE_SCORE - score.abs();
            let mate_in = ((mate_in_half + 1) / 2).max(1);
            Score::Mate(if score > 0 { mate_in } else { -mate_in })
        } else {
            Score::Cp(score)
        }
    }
}

/// Search session data encapsulated per worker thread
#[derive(Clone)]
pub struct SearchSession {
    /// Unique session ID for this search
    pub id: u64,

    /// Root position state for validation
    pub root_hash: u64,

    /// Committed best move (updated only on iteration completion)
    pub committed_best: Option<CommittedBest>,

    /// Current iteration's best (not sent to GUI)
    pub current_iteration_best: Option<CommittedBest>,
}

impl SearchSession {
    /// Create a new search session
    pub fn new(id: u64, root_hash: u64) -> Self {
        Self {
            id,
            root_hash,
            committed_best: None,
            current_iteration_best: None,
        }
    }

    /// Commit current iteration results
    pub fn commit_iteration(&mut self) {
        if let Some(current) = self.current_iteration_best.clone() {
            self.committed_best = Some(current);
        }
    }

    /// Update current iteration best
    pub fn update_current_best(&mut self, depth: u32, score: i32, pv: Vec<Move>) {
        self.current_iteration_best = Some(CommittedBest {
            depth,
            score: Score::from_raw(score),
            pv: pv.into_iter().collect(),
        });
    }
}

/// Committed best move information (internal representation)
#[derive(Clone, Debug)]
pub struct CommittedBest {
    /// Search depth
    pub depth: u32,

    /// Evaluation score (preserves cp/mate)
    pub score: Score,

    /// Principal variation (fixed-size optimized)
    pub pv: SmallVec<[Move; 32]>,
}
