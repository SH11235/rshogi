//! Search session management for thread-safe bestmove handling
//!
//! This module provides SearchSession to encapsulate all search-related data
//! within a single worker thread, preventing cross-thread contamination.

use crate::utils::to_usi_score;
use engine_core::shogi::Move;
use smallvec::SmallVec;

// Re-export Score from usi::output for backward compatibility
pub use crate::usi::output::Score;

/// Partial search result for fallback when search is interrupted
#[derive(Clone, Debug)]
pub struct PartialResult {
    /// Best move found so far (USI format)
    pub move_str: String,

    /// Search depth when this result was found
    pub depth: u8,

    /// Evaluation score
    pub score: i32,
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

    /// Partial result for fallback (updated during search)
    pub partial_result: Option<PartialResult>,
}

impl SearchSession {
    /// Create a new search session
    pub fn new(id: u64, root_hash: u64) -> Self {
        Self {
            id,
            root_hash,
            committed_best: None,
            current_iteration_best: None,
            partial_result: None,
        }
    }

    /// Commit current iteration results
    pub fn commit_iteration(&mut self) {
        if let Some(current) = self.current_iteration_best.clone() {
            self.committed_best = Some(current);
        }
    }

    /// Update current iteration best
    pub fn update_current_best(&mut self, depth: u8, score: i32, pv: Vec<Move>) {
        self.current_iteration_best = Some(CommittedBest {
            depth,
            seldepth: None, // Will be updated separately when available
            score: to_usi_score(score),
            pv: pv.into_iter().collect(),
        });
    }

    /// Update current iteration best with seldepth
    pub fn update_current_best_with_seldepth(
        &mut self,
        depth: u8,
        seldepth: Option<u8>,
        score: i32,
        pv: Vec<Move>,
    ) {
        self.current_iteration_best = Some(CommittedBest {
            depth,
            seldepth,
            score: to_usi_score(score),
            pv: pv.into_iter().collect(),
        });
    }

    /// Update partial result for fallback purposes
    ///
    /// This is called during search to store intermediate results
    /// that can be used if the search is interrupted.
    pub fn update_partial_result(&mut self, move_str: String, depth: u8, score: i32) {
        self.partial_result = Some(PartialResult {
            move_str,
            depth,
            score,
        });
    }
}

/// Committed best move information (internal representation)
#[derive(Clone, Debug)]
pub struct CommittedBest {
    /// Search depth
    pub depth: u8,

    /// Selective depth
    pub seldepth: Option<u8>,

    /// Evaluation score (preserves cp/mate)
    pub score: Score,

    /// Principal variation (fixed-size optimized)
    pub pv: SmallVec<[Move; 32]>,
}
