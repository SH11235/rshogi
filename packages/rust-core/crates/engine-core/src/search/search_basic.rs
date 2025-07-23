//! Search engine for shogi
//!
//! Implements alpha-beta search with basic enhancements

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::evaluation::evaluate::Evaluator;
use crate::shogi::{Move, MoveList};
use crate::{MoveGen, Position};

/// Infinity score for search bounds
const INFINITY_SCORE: i32 = 30000;

/// Search limits
#[derive(Clone)]
pub struct SearchLimits {
    /// Maximum search depth
    pub depth: u8,
    /// Maximum search time
    pub time: Option<Duration>,
    /// Maximum nodes to search
    pub nodes: Option<u64>,
    /// Stop flag for interrupting search
    pub stop_flag: Option<Arc<AtomicBool>>,
}

impl Default for SearchLimits {
    fn default() -> Self {
        SearchLimits {
            depth: 6,
            time: None,
            nodes: None,
            stop_flag: None,
        }
    }
}

impl std::fmt::Debug for SearchLimits {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchLimits")
            .field("depth", &self.depth)
            .field("time", &self.time)
            .field("nodes", &self.nodes)
            .field("stop_flag", &self.stop_flag.is_some())
            .finish()
    }
}

/// Search statistics
#[derive(Clone, Debug, Default)]
pub struct SearchStats {
    /// Nodes searched
    pub nodes: u64,
    /// Time elapsed
    pub elapsed: Duration,
    /// Principal variation
    pub pv: Vec<Move>,
}

/// Search result
#[derive(Clone, Debug)]
pub struct SearchResult {
    /// Best move found
    pub best_move: Option<Move>,
    /// Evaluation score (from side to move perspective)
    pub score: i32,
    /// Search statistics
    pub stats: SearchStats,
}

/// Search engine
pub struct Searcher<E: Evaluator> {
    /// Search limits
    limits: SearchLimits,
    /// Start time
    start_time: Instant,
    /// Node counter
    nodes: u64,
    /// Principal variation
    pv: Vec<Vec<Move>>,
    /// Evaluation function
    evaluator: Arc<E>,
}

impl<E: Evaluator> Searcher<E> {
    /// Create new searcher with limits and evaluator
    pub fn new(limits: SearchLimits, evaluator: Arc<E>) -> Self {
        Searcher {
            limits,
            start_time: Instant::now(),
            nodes: 0,
            pv: vec![vec![]; 128], // Max depth 128
            evaluator,
        }
    }

    /// Search position for best move
    pub fn search(&mut self, pos: &mut Position) -> SearchResult {
        self.start_time = Instant::now();
        self.nodes = 0;

        let mut best_move = None;
        let mut best_score = -INFINITY_SCORE;

        // If no move is found during iterative deepening, we need a fallback
        // Generate all legal moves first as a fallback
        let mut gen = MoveGen::new();
        let mut legal_moves = MoveList::new();
        gen.generate_all(pos, &mut legal_moves);

        // If there are legal moves, use the first one as a fallback
        if !legal_moves.is_empty() {
            best_move = Some(legal_moves[0]);
        }

        // Iterative deepening
        for depth in 1..=self.limits.depth {
            let score = self.alpha_beta(pos, depth, -INFINITY_SCORE, INFINITY_SCORE);

            // Check time limit
            if self.should_stop() {
                break;
            }

            best_score = score;
            if !self.pv[0].is_empty() {
                best_move = Some(self.pv[0][0]);
            }
        }

        SearchResult {
            best_move,
            score: best_score,
            stats: SearchStats {
                nodes: self.nodes,
                elapsed: self.start_time.elapsed(),
                pv: self.pv[0].clone(),
            },
        }
    }

    /// Alpha-beta search
    fn alpha_beta(&mut self, pos: &mut Position, depth: u8, mut alpha: i32, beta: i32) -> i32 {
        self.nodes += 1;

        // Check limits (every 1024 nodes for efficiency)
        if self.nodes & 1023 == 0 && self.should_stop() {
            return 0;
        }

        // Leaf node - return static evaluation
        if depth == 0 {
            return self.evaluator.evaluate(pos);
        }

        // Clear PV for this ply
        let ply = self.limits.depth - depth;
        self.pv[ply as usize].clear();

        // Generate moves
        let mut gen = MoveGen::new();
        let mut moves = MoveList::new();
        gen.generate_all(pos, &mut moves);

        // No legal moves - checkmate or stalemate
        if moves.is_empty() {
            if pos.in_check() {
                // Checkmate - return negative score
                return -INFINITY_SCORE + ply as i32;
            } else {
                // Stalemate (shouldn't happen in shogi)
                return 0;
            }
        }

        let mut best_score = -INFINITY_SCORE;

        // Search all moves
        for &mv in moves.as_slice() {
            // Make move
            let undo_info = pos.do_move(mv);

            // Recursive search
            let score = -self.alpha_beta(pos, depth - 1, -beta, -alpha);

            // Unmake move
            pos.undo_move(mv, undo_info);

            // Update best score
            if score > best_score {
                best_score = score;

                // Update alpha
                if score > alpha {
                    alpha = score;

                    // Update principal variation
                    self.pv[ply as usize].clear();
                    self.pv[ply as usize].push(mv);
                    // Copy from next ply's PV
                    let next_ply = (ply + 1) as usize;
                    if next_ply < self.pv.len() {
                        let next_pv = self.pv[next_ply].clone();
                        self.pv[ply as usize].extend_from_slice(&next_pv);
                    }

                    // Beta cutoff
                    if score >= beta {
                        break;
                    }
                }
            }
        }

        best_score
    }

    /// Check if search should stop
    fn should_stop(&self) -> bool {
        // Check stop flag
        if let Some(ref stop_flag) = self.limits.stop_flag {
            if stop_flag.load(Ordering::Relaxed) {
                return true;
            }
        }

        // Check node limit
        if let Some(max_nodes) = self.limits.nodes {
            if self.nodes >= max_nodes {
                return true;
            }
        }

        // Check time limit
        if let Some(max_time) = self.limits.time {
            if self.start_time.elapsed() >= max_time {
                return true;
            }
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use crate::evaluate::MaterialEvaluator;

    use super::*;

    #[test]
    fn test_search_startpos() {
        let mut pos = Position::startpos();
        let limits = SearchLimits {
            depth: 3,
            time: Some(Duration::from_secs(1)),
            nodes: None,
            stop_flag: None,
        };

        let evaluator = Arc::new(MaterialEvaluator);
        let mut searcher = Searcher::new(limits, evaluator);
        let result = searcher.search(&mut pos);

        // Should find a move
        assert!(result.best_move.is_some());

        // Should have searched some nodes
        assert!(result.stats.nodes > 0);

        // Score should be reasonable
        assert!(result.score.abs() < 1000);
    }

    #[test]
    fn test_search_with_stop_flag() {
        use std::thread;
        use std::time::Instant;
        
        let mut pos = Position::startpos();
        let stop_flag = Arc::new(AtomicBool::new(false));
        let limits = SearchLimits {
            depth: 10, // Deep search that would normally take a while
            time: None,
            nodes: None,
            stop_flag: Some(stop_flag.clone()),
        };

        let evaluator = Arc::new(MaterialEvaluator);
        let mut searcher = Searcher::new(limits, evaluator);
        
        // Set stop flag after a short delay
        let stop_flag_clone = stop_flag.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            stop_flag_clone.store(true, Ordering::Relaxed);
        });
        
        let start = Instant::now();
        let result = searcher.search(&mut pos);
        let elapsed = start.elapsed();
        
        // Should find a move (even if search was stopped)
        assert!(result.best_move.is_some());
        
        // Should have stopped quickly (well before searching to depth 10)
        assert!(elapsed < Duration::from_secs(1));
        
        // Should have searched relatively few nodes
        assert!(result.stats.nodes < 1_000_000);
    }
}
