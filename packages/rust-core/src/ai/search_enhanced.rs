//! Enhanced search engine with advanced pruning techniques
//!
//! Implements alpha-beta search with:
//! - Null Move Pruning
//! - Late Move Reductions (LMR)
//! - Futility Pruning
//! - History Heuristics
//! - Transposition Table

use super::board::{Color, Position};
use super::evaluate::Evaluator;
use super::history::History;
use super::move_picker::MovePicker;
use super::moves::Move;
use super::tt::{NodeType, TranspositionTable};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Maximum search depth
#[allow(dead_code)]
const MAX_DEPTH: i32 = 127;

/// Maximum ply from root
const MAX_PLY: usize = 127;

/// Infinity score for search bounds
const INFINITY: i32 = 30000;

/// Mate score threshold
const MATE_SCORE: i32 = 28000;

/// Draw score
const DRAW_SCORE: i32 = 0;

/// Search stack entry
#[derive(Clone, Default)]
pub struct SearchStack {
    /// Current move being searched
    pub current_move: Option<Move>,
    /// Static evaluation
    pub static_eval: i32,
    /// Killer moves
    pub killers: [Option<Move>; 2],
    /// Move count
    pub move_count: u32,
    /// PV node flag
    pub pv: bool,
    /// Null move tried flag
    pub null_move: bool,
    /// In check flag
    pub in_check: bool,
}

/// Search parameters
pub struct SearchParams {
    /// Null move reduction
    pub null_move_reduction: fn(i32) -> i32,
    /// LMR reduction table [depth][move_count]
    pub lmr_reductions: [[i32; 64]; 64],
    /// Futility margin
    pub futility_margin: fn(i32) -> i32,
    /// Late move count margin
    pub late_move_count: fn(i32) -> i32,
}

impl Default for SearchParams {
    fn default() -> Self {
        // Initialize LMR reduction table
        let mut lmr_reductions = [[0; 64]; 64];
        for (depth_idx, depth_row) in lmr_reductions.iter_mut().enumerate().skip(1) {
            for (move_idx, reduction) in depth_row.iter_mut().enumerate().skip(1) {
                *reduction =
                    (0.75 + (depth_idx as f64).ln() * (move_idx as f64).ln() / 2.25) as i32;
            }
        }

        SearchParams {
            null_move_reduction: |depth| 3 + depth / 6,
            lmr_reductions,
            futility_margin: |depth| 150 * depth,
            late_move_count: |depth| 3 + depth * depth / 2,
        }
    }
}

/// Enhanced searcher with advanced techniques
pub struct EnhancedSearcher {
    /// Transposition table
    tt: Arc<TranspositionTable>,
    /// History tables
    history: History,
    /// Search parameters
    params: SearchParams,
    /// Node counter
    nodes: AtomicU64,
    /// Stop flag
    stop: AtomicBool,
    /// Time limit
    time_limit: Option<Instant>,
    /// Node limit
    node_limit: Option<u64>,
    /// Search start time
    start_time: Instant,
    /// Evaluator
    evaluator: Arc<dyn Evaluator + Send + Sync>,
}

impl EnhancedSearcher {
    /// Create new enhanced searcher
    pub fn new(tt_size_mb: usize, evaluator: Arc<dyn Evaluator + Send + Sync>) -> Self {
        EnhancedSearcher {
            tt: Arc::new(TranspositionTable::new(tt_size_mb)),
            history: History::new(),
            params: SearchParams::default(),
            nodes: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            time_limit: None,
            node_limit: None,
            start_time: Instant::now(),
            evaluator,
        }
    }

    /// Search position with iterative deepening
    pub fn search(
        &mut self,
        pos: &mut Position,
        max_depth: i32,
        time_limit: Option<Duration>,
        node_limit: Option<u64>,
    ) -> (Option<Move>, i32) {
        // Reset search state
        self.nodes.store(0, Ordering::Relaxed);
        self.stop.store(false, Ordering::Relaxed);
        self.start_time = Instant::now();
        self.time_limit = time_limit.map(|d| self.start_time + d);
        self.node_limit = node_limit;
        self.history.clear_all();

        // New search generation
        Arc::get_mut(&mut self.tt).unwrap().new_search();

        let mut stack = vec![SearchStack::default(); MAX_PLY + 10];
        let mut best_move = None;
        let mut best_score = -INFINITY;

        // Iterative deepening
        for depth in 1..=max_depth {
            let score = self.alpha_beta(pos, -INFINITY, INFINITY, depth, 0, &mut stack);

            if self.stop.load(Ordering::Relaxed) {
                break;
            }

            // Extract best move from TT
            if let Some(tt_entry) = self.tt.probe(pos.hash) {
                best_move = tt_entry.get_move();
                best_score = score;
            }

            // Check time
            if self.should_stop() {
                break;
            }
        }

        (best_move, best_score)
    }

    /// Alpha-beta search with enhancements
    fn alpha_beta(
        &mut self,
        pos: &mut Position,
        mut alpha: i32,
        mut beta: i32,
        depth: i32,
        ply: usize,
        stack: &mut [SearchStack],
    ) -> i32 {
        // Check limits
        if self.should_stop() {
            self.stop.store(true, Ordering::Relaxed);
            return 0;
        }

        // Update node count
        self.nodes.fetch_add(1, Ordering::Relaxed);

        // Check for draws
        if pos.is_draw() {
            return DRAW_SCORE;
        }

        // Mate distance pruning
        alpha = alpha.max(-MATE_SCORE + ply as i32);
        beta = beta.min(MATE_SCORE - ply as i32 - 1);
        if alpha >= beta {
            return alpha;
        }

        // Initialize stack entry
        stack[ply].pv = (beta - alpha) > 1;
        stack[ply].in_check = pos.in_check();

        // Quiescence search at leaf nodes
        if depth <= 0 {
            return self.quiescence(pos, alpha, beta, ply, stack);
        }

        // Probe transposition table
        let tt_hit = self.tt.probe(pos.hash);
        let mut tt_move = None;
        let tt_value;
        let mut tt_eval = 0;

        if let Some(entry) = tt_hit {
            tt_move = entry.get_move();
            tt_value = entry.score() as i32;
            tt_eval = entry.eval() as i32;

            // TT cutoff
            if entry.depth() >= depth as u8 && ply > 0 {
                match entry.node_type() {
                    NodeType::Exact => return tt_value,
                    NodeType::LowerBound => {
                        if tt_value >= beta {
                            return tt_value;
                        }
                        alpha = alpha.max(tt_value);
                    }
                    NodeType::UpperBound => {
                        if tt_value <= alpha {
                            return tt_value;
                        }
                        beta = beta.min(tt_value);
                    }
                }
            }
        }

        // Static evaluation
        let static_eval = if stack[ply].in_check {
            -INFINITY
        } else if tt_hit.is_some() {
            tt_eval
        } else {
            self.evaluator.evaluate(pos)
        };
        stack[ply].static_eval = static_eval;

        // Null move pruning
        if !stack[ply].pv
            && !stack[ply].in_check
            && depth >= 3
            && static_eval >= beta
            && !stack[ply].null_move
            && self.has_non_pawn_material(pos, pos.side_to_move)
        {
            let r = (self.params.null_move_reduction)(depth);

            // Make null move
            stack[ply + 1].null_move = true;
            let undo = self.do_null_move(pos);

            let score = -self.alpha_beta(pos, -beta, -beta + 1, depth - r - 1, ply + 1, stack);

            // Undo null move
            self.undo_null_move(pos, undo);
            stack[ply + 1].null_move = false;

            if score >= beta {
                return score;
            }
        }

        // Use MovePicker for efficient move ordering
        let history_arc = Arc::new(self.history.clone());
        let mut move_picker = MovePicker::new(pos, tt_move, &history_arc, &stack[ply]);

        let mut best_score = -INFINITY;
        let mut best_move = None;
        let mut moves_searched = 0;
        let mut quiets_tried = Vec::new();

        // Search moves
        while let Some(mv) = move_picker.next_move() {
            // Late move pruning
            if !stack[ply].in_check
                && moves_searched >= (self.params.late_move_count)(depth)
                && !self.is_capture(pos, mv)
                && !mv.is_promote()
            {
                continue;
            }

            // Make move
            let undo = pos.do_move(mv);
            moves_searched += 1;

            // Prefetch TT
            self.tt.prefetch(pos.hash);

            let mut new_depth = depth - 1;

            // Extensions
            if pos.in_check() {
                new_depth += 1; // Check extension
            }

            // Late move reductions
            let mut score;
            if depth >= 3
                && moves_searched > 1
                && !stack[ply].in_check
                && !self.is_capture(pos, mv)
                && !mv.is_promote()
            {
                let r = self.params.lmr_reductions[depth.min(63) as usize]
                    [moves_searched.min(63) as usize];
                let reduced_depth = (new_depth - r).max(1);

                // Reduced search
                score = -self.alpha_beta(pos, -alpha - 1, -alpha, reduced_depth, ply + 1, stack);

                // Re-search if failed high
                if score > alpha {
                    score = -self.alpha_beta(pos, -beta, -alpha, new_depth, ply + 1, stack);
                }
            } else if moves_searched > 1 {
                // Null window search
                score = -self.alpha_beta(pos, -alpha - 1, -alpha, new_depth, ply + 1, stack);

                // Re-search with full window if needed
                if score > alpha && score < beta {
                    score = -self.alpha_beta(pos, -beta, -alpha, new_depth, ply + 1, stack);
                }
            } else {
                // Full window search
                score = -self.alpha_beta(pos, -beta, -alpha, new_depth, ply + 1, stack);
            }

            // Undo move
            pos.undo_move(mv, undo);

            if self.stop.load(Ordering::Relaxed) {
                return best_score;
            }

            // Track quiet moves for history update
            if !self.is_capture(pos, mv) {
                quiets_tried.push(mv);
            }

            if score > best_score {
                best_score = score;
                best_move = Some(mv);

                if score > alpha {
                    alpha = score;

                    if score >= beta {
                        // Update history for good quiet move
                        if !self.is_capture(pos, mv) {
                            self.history.update_cutoff(pos.side_to_move, mv, depth, None);

                            // Update killers
                            if stack[ply].killers[0] != Some(mv) {
                                stack[ply].killers[1] = stack[ply].killers[0];
                                stack[ply].killers[0] = Some(mv);
                            }

                            // Penalize other quiet moves that were tried
                            for &quiet_mv in &quiets_tried {
                                if quiet_mv != mv {
                                    self.history.update_quiet(
                                        pos.side_to_move,
                                        quiet_mv,
                                        depth,
                                        None,
                                    );
                                }
                            }
                        }
                        break; // Beta cutoff
                    }
                }
            }
        }

        // Check for no legal moves
        if moves_searched == 0 {
            return if stack[ply].in_check {
                -MATE_SCORE + ply as i32
            } else {
                DRAW_SCORE
            };
        }

        // Store in transposition table
        let node_type = if best_score >= beta {
            NodeType::LowerBound
        } else if best_score <= alpha {
            NodeType::UpperBound
        } else {
            NodeType::Exact
        };

        self.tt.store(
            pos.hash,
            best_move,
            best_score as i16,
            static_eval as i16,
            depth as u8,
            node_type,
        );

        best_score
    }

    /// Quiescence search
    fn quiescence(
        &mut self,
        pos: &mut Position,
        mut alpha: i32,
        beta: i32,
        ply: usize,
        stack: &mut [SearchStack],
    ) -> i32 {
        if self.should_stop() {
            self.stop.store(true, Ordering::Relaxed);
            return 0;
        }

        self.nodes.fetch_add(1, Ordering::Relaxed);

        // Stand pat
        let stand_pat = if stack[ply].in_check {
            -INFINITY
        } else {
            self.evaluator.evaluate(pos)
        };

        if stand_pat >= beta {
            return stand_pat;
        }

        alpha = alpha.max(stand_pat);

        // Use MovePicker for captures only
        let tt_hit = self.tt.probe(pos.hash);
        let tt_move = tt_hit.as_ref().and_then(|e| e.get_move());
        let history_arc = Arc::new(self.history.clone());
        let mut move_picker = MovePicker::new_quiescence(pos, tt_move, &history_arc, &stack[ply]);

        while let Some(mv) = move_picker.next_move() {
            let undo = pos.do_move(mv);
            let score = -self.quiescence(pos, -beta, -alpha, ply + 1, stack);
            pos.undo_move(mv, undo);

            if score > alpha {
                alpha = score;
                if score >= beta {
                    return score;
                }
            }
        }

        alpha
    }

    /// Check if move is capture
    fn is_capture(&self, pos: &Position, mv: Move) -> bool {
        if mv.is_drop() {
            return false;
        }
        let to = mv.to();
        pos.board.piece_on(to).is_some()
    }

    /// Check if position has non-pawn material
    fn has_non_pawn_material(&self, pos: &Position, color: Color) -> bool {
        let color_idx = color as usize;
        // Check for pieces other than pawns in hand
        pos.hands[color_idx][0] > 0 || // Rook
        pos.hands[color_idx][1] > 0 || // Bishop
        pos.hands[color_idx][2] > 0 || // Gold
        pos.hands[color_idx][3] > 0 || // Silver
        pos.hands[color_idx][4] > 0 || // Knight
        pos.hands[color_idx][5] > 0 // Lance
    }

    /// Do null move (returns previous side to move)
    fn do_null_move(&self, pos: &mut Position) -> Color {
        let prev_side = pos.side_to_move;
        pos.side_to_move = pos.side_to_move.opposite();
        pos.hash ^= super::zobrist::ZOBRIST.side_to_move;
        prev_side
    }

    /// Undo null move
    fn undo_null_move(&self, pos: &mut Position, prev_side: Color) {
        pos.side_to_move = prev_side;
        pos.hash ^= super::zobrist::ZOBRIST.side_to_move;
    }

    /// Check if search should stop
    fn should_stop(&self) -> bool {
        if self.stop.load(Ordering::Relaxed) {
            return true;
        }

        // Check time limit
        if let Some(limit) = self.time_limit {
            if Instant::now() >= limit {
                return true;
            }
        }

        // Check node limit
        if let Some(limit) = self.node_limit {
            if self.nodes.load(Ordering::Relaxed) >= limit {
                return true;
            }
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::evaluate::MaterialEvaluator;

    #[test]
    fn test_search_params() {
        let params = SearchParams::default();

        // Test null move reduction
        assert_eq!((params.null_move_reduction)(6), 4);
        assert_eq!((params.null_move_reduction)(12), 5);

        // Test LMR table
        assert!(params.lmr_reductions[10][10] > 0);
        assert!(params.lmr_reductions[20][20] > params.lmr_reductions[10][10]);

        // Test margins
        assert!((params.futility_margin)(1) > 0);
    }

    #[test]
    fn test_enhanced_search_basic() {
        let evaluator = Arc::new(MaterialEvaluator);
        let mut searcher = EnhancedSearcher::new(16, evaluator);
        let mut pos = Position::startpos();

        let (best_move, score) = searcher.search(&mut pos, 4, None, None);

        assert!(best_move.is_some());
        assert!(score.abs() < 1000); // Should be relatively balanced
    }
}
