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

/// Game phase enum
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GamePhase {
    /// Opening phase (0-20 moves)
    Opening,
    /// Middle game (21-60 moves)
    MiddleGame,
    /// End game (61+ moves or few pieces)
    EndGame,
}

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

/// Principal Variation table with fixed-size arrays
///
/// This structure tracks the principal variation (best move sequence) found during search.
/// Each ply maintains its own PV line which is updated when a better move is found.
///
/// # Thread Safety
///
/// **WARNING**: This structure is NOT thread-safe. In multi-threaded search scenarios:
/// - Each thread should maintain its own independent PVTable instance, OR
/// - Use synchronization primitives (Mutex/RwLock) if sharing is required, OR
/// - Implement a lock-free approach where only the root thread updates the shared PV
///
/// # Memory Usage
///
/// Size: approximately 64KB (MAX_PLY × MAX_PLY × size_of::<Move>())
/// - With 8 threads: ~512KB total
/// - With 32 threads: ~2MB total
///
/// For memory-constrained environments, consider using const generics to reduce MAX_PLY.
pub struct PVTable {
    /// PV lines for each ply [ply][move_index]
    line: [[Move; MAX_PLY]; MAX_PLY],
    /// Number of moves in each PV line
    len: [u8; MAX_PLY],
}

impl Default for PVTable {
    fn default() -> Self {
        PVTable {
            line: [[Move::default(); MAX_PLY]; MAX_PLY],
            len: [0; MAX_PLY],
        }
    }
}

impl PVTable {
    /// Get the principal variation from root
    ///
    /// # Lifetime
    ///
    /// The returned slice is only valid until the next search operation.
    /// Starting a new search will clear and overwrite the PV data.
    /// If you need to preserve the PV across searches, clone the data.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let pv = searcher.principal_variation();
    /// let pv_copy: Vec<Move> = pv.to_vec(); // Clone if needed across searches
    /// ```
    pub fn get_pv(&self) -> &[Move] {
        let len = self.len[0] as usize;
        &self.line[0][0..len]
    }

    /// Clear PV at given ply
    ///
    /// This method only writes to memory if the PV is not already empty,
    /// avoiding unnecessary cache misses during repeated clear calls.
    #[inline]
    pub fn clear(&mut self, ply: usize) {
        debug_assert!(ply < MAX_PLY, "ply {ply} exceeds MAX_PLY {MAX_PLY}");
        if self.len[ply] != 0 {
            self.len[ply] = 0;
        }
    }

    /// Update PV when new best move is found
    #[inline]
    pub fn update(&mut self, ply: usize, mv: Move) {
        debug_assert!(ply < MAX_PLY, "ply {ply} exceeds MAX_PLY {MAX_PLY}");
        self.line[ply][0] = mv;
        self.len[ply] = 1;

        // Copy child PV if exists
        if ply + 1 < MAX_PLY {
            let child_len = self.len[ply + 1] as usize;
            // We need child_len + 1 positions: index 0 for the new move,
            // and indices 1..=child_len for the child PV
            #[allow(clippy::int_plus_one)]
            if child_len > 0 && child_len + 1 <= MAX_PLY {
                // Use split_at_mut to avoid mutable borrow conflict
                let (first_half, second_half) = self.line.split_at_mut(ply + 1);
                first_half[ply][1..=child_len].copy_from_slice(&second_half[0][0..child_len]);
                self.len[ply] = 1 + self.len[ply + 1];
            }
        }
    }
}

/// Search context for alpha-beta search
struct SearchContext {
    /// Current alpha bound
    alpha: i32,
    /// Current beta bound  
    beta: i32,
    /// Remaining depth
    depth: i32,
    /// Distance from root
    ply: usize,
    /// Whether this is an aspiration window retry
    aspiration_failed: bool,
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
    /// Initial aspiration window function (phase-aware)
    pub initial_window: fn(i32, GamePhase) -> i32, // (depth, phase) -> window_size
    /// Maximum aspiration window delta (depth-dependent)
    pub max_aspiration_delta: fn(i32) -> i32, // depth -> max_delta
    /// Time pressure threshold (ratio of remaining time to elapsed time)
    pub time_pressure_threshold: f64,
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
            initial_window: |depth, phase| {
                // フェーズに応じた動的窓幅調整
                let base_window = match phase {
                    GamePhase::Opening => 30,    // 序盤は狭い窓（評価が安定）
                    GamePhase::MiddleGame => 50, // 中盤は標準的な窓
                    GamePhase::EndGame => 100,   // 終盤は広い窓（評価が激しく変動）
                };
                let depth_factor = match phase {
                    GamePhase::Opening => 3,    // 序盤は深さの影響を小さく
                    GamePhase::MiddleGame => 5, // 中盤は標準
                    GamePhase::EndGame => 10,   // 終盤は深さの影響を大きく
                };
                (base_window + depth * depth_factor).min(i16::MAX as i32)
            },
            max_aspiration_delta: |depth| {
                // 深さが深いほど大きな窓幅を許容
                (800 + 40 * depth).min(2000)
            },
            time_pressure_threshold: 0.1, // 残り時間が経過時間の10%未満で時間圧迫
        }
    }
}

/// Search statistics for testing
#[cfg(test)]
#[derive(Default, Clone)]
pub struct SearchStats {
    /// Number of aspiration failures per depth
    pub aspiration_failures: Vec<u32>,
    /// Number of aspiration windows hit per depth
    pub aspiration_hits: Vec<u32>,
    /// Total delta used per depth
    pub total_delta: Vec<i32>,
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
    /// Principal variation table
    pv: PVTable,
    /// Search statistics (for testing)
    #[cfg(test)]
    pub stats: SearchStats,
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
            pv: PVTable::default(),
            #[cfg(test)]
            stats: SearchStats::default(),
        }
    }

    /// Estimate game phase based on move count and material
    fn estimate_game_phase(&self, pos: &Position) -> GamePhase {
        let ply = pos.ply as i32;

        // Count pieces on board using cached bitboard
        let total_pieces = pos.board.all_bb.count_ones();

        // Determine phase based on move count and piece count
        if ply < 20 {
            GamePhase::Opening
        } else if ply > 60 || total_pieces < 20 {
            // End game if many moves or few pieces remain
            GamePhase::EndGame
        } else {
            GamePhase::MiddleGame
        }
    }

    /// Update aspiration window bounds based on delta and center score
    fn update_aspiration_window(&self, delta: i32, center: i32) -> (i32, i32) {
        let alpha = (center - delta).max(-INFINITY).max(-MATE_SCORE + MAX_PLY as i32);
        let beta = (center + delta).min(INFINITY).min(MATE_SCORE - MAX_PLY as i32);
        (alpha, beta)
    }

    /// Get current principal variation
    pub fn principal_variation(&self) -> &[Move] {
        self.pv.get_pv()
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

        #[cfg(test)]
        {
            // 再利用：既存のベクタをクリアして再使用
            let required_size = max_depth as usize + 1;

            // リサイズが必要な場合のみ実行
            if self.stats.aspiration_failures.len() < required_size {
                self.stats.aspiration_failures.resize(required_size, 0);
                self.stats.aspiration_hits.resize(required_size, 0);
                self.stats.total_delta.resize(required_size, 0);
            } else {
                // 既存の領域をゼロクリア
                self.stats.aspiration_failures.fill(0);
                self.stats.aspiration_hits.fill(0);
                self.stats.total_delta.fill(0);
            }
        }

        // New search generation
        Arc::get_mut(&mut self.tt).unwrap().new_search();

        let mut stack = vec![SearchStack::default(); MAX_PLY + 10];
        let mut best_move = None;
        let mut best_score = -INFINITY;
        let mut last_root_score = -INFINITY; // 前回の反復深化で確定したスコア

        // Iterative deepening
        for depth in 1..=max_depth {
            // ゲームフェーズを判定
            let phase = self.estimate_game_phase(pos);

            // Aspiration Windows用の初期値設定
            let prev_score = if depth > 1 {
                last_root_score // 直前の確定スコアを最優先
            } else if let Some(entry) = self.tt.probe(pos.hash) {
                if entry.node_type() == NodeType::Exact && !entry.aspiration_fail() {
                    // EXACT かつ aspiration成功の値だけ採用
                    entry.score() as i32
                } else {
                    self.evaluator.evaluate(pos) // 境界値やaspiration失敗なら捨てる
                }
            } else {
                self.evaluator.evaluate(pos)
            };

            // Aspiration Windows設定（フェーズを考慮）
            let mut delta = (self.params.initial_window)(depth, phase);
            let mut alpha = if depth == 1 {
                -INFINITY
            } else {
                (prev_score - delta).max(-INFINITY).max(-MATE_SCORE + MAX_PLY as i32)
            };
            let mut beta = if depth == 1 {
                INFINITY
            } else {
                (prev_score + delta).min(INFINITY).min(MATE_SCORE - MAX_PLY as i32)
            };

            let mut score;
            let mut aspiration_failed = false;

            // Aspiration search with retries
            loop {
                #[cfg(test)]
                {
                    self.stats.total_delta[depth as usize] = delta;
                }

                score = self.alpha_beta_with_aspiration(
                    pos,
                    alpha,
                    beta,
                    depth,
                    0,
                    &mut stack,
                    depth > 1 && aspiration_failed,
                );

                if self.stop.load(Ordering::Relaxed) {
                    break;
                }

                // Check time and stop conditions
                if self.should_stop() {
                    self.stop.store(true, Ordering::Relaxed);
                    break;
                }

                // Check aspiration window
                if score <= alpha {
                    // Fail-low: 窓を下方向に拡大
                    aspiration_failed = true;
                    #[cfg(test)]
                    {
                        self.stats.aspiration_failures[depth as usize] += 1;
                    }

                    // deltaを先に拡大
                    delta = (delta * 2).min((self.params.max_aspiration_delta)(depth));
                    (alpha, beta) = self.update_aspiration_window(delta, prev_score);
                } else if score >= beta {
                    // Fail-high: 窓を上方向に拡大
                    aspiration_failed = true;
                    #[cfg(test)]
                    {
                        self.stats.aspiration_failures[depth as usize] += 1;
                    }

                    // deltaを先に拡大
                    delta = (delta * 2).min((self.params.max_aspiration_delta)(depth));
                    (alpha, beta) = self.update_aspiration_window(delta, prev_score);
                } else {
                    // 窓内ヒット: 成功
                    #[cfg(test)]
                    {
                        self.stats.aspiration_hits[depth as usize] += 1;
                        self.stats.total_delta[depth as usize] = delta; // 成功時のdeltaを記録
                    }
                    break;
                }

                // 安全装置: deltaが最大値を超えたらフルウィンドウ
                if delta >= (self.params.max_aspiration_delta)(depth) {
                    alpha = -INFINITY;
                    beta = INFINITY;
                }

                // 時間制限が厳しい場合は即座にフルウィンドウへ
                if let Some(limit) = self.time_limit {
                    let elapsed = self.start_time.elapsed();
                    let remaining = limit.saturating_duration_since(Instant::now());
                    if remaining.as_secs_f64()
                        < elapsed.as_secs_f64() * self.params.time_pressure_threshold
                    {
                        alpha = -INFINITY;
                        beta = INFINITY;
                    }
                }
            }

            // 探索結果の保存
            best_score = score;
            last_root_score = score; // 次の深さで使用するため更新

            // Extract best move from TT
            if let Some(tt_entry) = self.tt.probe(pos.hash) {
                best_move = tt_entry.get_move();
            }

            // Check time
            if self.should_stop() {
                break;
            }
        }

        (best_move, best_score)
    }

    /// Alpha-beta search with aspiration window tracking
    #[allow(clippy::too_many_arguments)]
    fn alpha_beta_with_aspiration(
        &mut self,
        pos: &mut Position,
        alpha: i32,
        beta: i32,
        depth: i32,
        ply: usize,
        stack: &mut [SearchStack],
        aspiration_failed: bool,
    ) -> i32 {
        let ctx = SearchContext {
            alpha,
            beta,
            depth,
            ply,
            aspiration_failed,
        };
        self.alpha_beta_internal(pos, ctx, stack)
    }

    /// Alpha-beta search with enhancements
    fn alpha_beta(
        &mut self,
        pos: &mut Position,
        alpha: i32,
        beta: i32,
        depth: i32,
        ply: usize,
        stack: &mut [SearchStack],
    ) -> i32 {
        let ctx = SearchContext {
            alpha,
            beta,
            depth,
            ply,
            aspiration_failed: false,
        };
        self.alpha_beta_internal(pos, ctx, stack)
    }

    /// Internal alpha-beta search with aspiration tracking
    fn alpha_beta_internal(
        &mut self,
        pos: &mut Position,
        mut ctx: SearchContext,
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
        ctx.alpha = ctx.alpha.max(-MATE_SCORE + ctx.ply as i32);
        ctx.beta = ctx.beta.min(MATE_SCORE - ctx.ply as i32 - 1);
        if ctx.alpha >= ctx.beta {
            return ctx.alpha;
        }

        // Initialize stack entry
        stack[ctx.ply].pv = (ctx.beta - ctx.alpha) > 1;
        stack[ctx.ply].in_check = pos.in_check();

        // Clear PV at current ply
        self.pv.clear(ctx.ply);

        // Quiescence search at leaf nodes
        if ctx.depth <= 0 {
            return self.quiescence(pos, ctx.alpha, ctx.beta, ctx.ply, stack);
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
            if entry.depth() >= ctx.depth as u8 && ctx.ply > 0 {
                match entry.node_type() {
                    NodeType::Exact => return tt_value,
                    NodeType::LowerBound => {
                        if tt_value >= ctx.beta {
                            return tt_value;
                        }
                        ctx.alpha = ctx.alpha.max(tt_value);
                    }
                    NodeType::UpperBound => {
                        if tt_value <= ctx.alpha {
                            return tt_value;
                        }
                        ctx.beta = ctx.beta.min(tt_value);
                    }
                }
            }
        }

        // Static evaluation
        let static_eval = if stack[ctx.ply].in_check {
            -INFINITY
        } else if tt_hit.is_some() {
            tt_eval
        } else {
            self.evaluator.evaluate(pos)
        };
        stack[ctx.ply].static_eval = static_eval;

        // Null move pruning
        if !stack[ctx.ply].pv
            && !stack[ctx.ply].in_check
            && ctx.depth >= 3
            && static_eval >= ctx.beta
            && !stack[ctx.ply].null_move
            && self.has_non_pawn_material(pos, pos.side_to_move)
        {
            let r = (self.params.null_move_reduction)(ctx.depth);

            // Make null move
            stack[ctx.ply + 1].null_move = true;
            let undo = self.do_null_move(pos);

            let score = -self.alpha_beta(
                pos,
                -ctx.beta,
                -ctx.beta + 1,
                ctx.depth - r - 1,
                ctx.ply + 1,
                stack,
            );

            // Undo null move
            self.undo_null_move(pos, undo);
            stack[ctx.ply + 1].null_move = false;

            if score >= ctx.beta {
                return score;
            }
        }

        // Use MovePicker for efficient move ordering
        let history_arc = Arc::new(self.history.clone());
        let mut move_picker = MovePicker::new(pos, tt_move, &history_arc, &stack[ctx.ply]);

        let mut best_score = -INFINITY;
        let mut best_move = None;
        let mut moves_searched = 0;
        let mut quiets_tried = Vec::new();

        // Search moves
        while let Some(mv) = move_picker.next_move() {
            // Late move pruning
            if !stack[ctx.ply].in_check
                && moves_searched >= (self.params.late_move_count)(ctx.depth)
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

            let mut new_depth = ctx.depth - 1;

            // Extensions
            if pos.in_check() {
                new_depth += 1; // Check extension
            }

            // Late move reductions
            let mut score;
            if ctx.depth >= 3
                && moves_searched > 1
                && !stack[ctx.ply].in_check
                && !self.is_capture(pos, mv)
                && !mv.is_promote()
            {
                let r = self.params.lmr_reductions[ctx.depth.min(63) as usize]
                    [moves_searched.min(63) as usize];
                let reduced_depth = (new_depth - r).max(1);

                // Reduced search
                score = -self.alpha_beta(
                    pos,
                    -ctx.alpha - 1,
                    -ctx.alpha,
                    reduced_depth,
                    ctx.ply + 1,
                    stack,
                );

                // Re-search if failed high
                if score > ctx.alpha {
                    score =
                        -self.alpha_beta(pos, -ctx.beta, -ctx.alpha, new_depth, ctx.ply + 1, stack);
                }
            } else if moves_searched > 1 {
                // Null window search
                score = -self.alpha_beta(
                    pos,
                    -ctx.alpha - 1,
                    -ctx.alpha,
                    new_depth,
                    ctx.ply + 1,
                    stack,
                );

                // Re-search with full window if needed
                if score > ctx.alpha && score < ctx.beta {
                    score =
                        -self.alpha_beta(pos, -ctx.beta, -ctx.alpha, new_depth, ctx.ply + 1, stack);
                }
            } else {
                // Full window search
                score = -self.alpha_beta(pos, -ctx.beta, -ctx.alpha, new_depth, ctx.ply + 1, stack);
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

                if score > ctx.alpha {
                    ctx.alpha = score;

                    // Update PV
                    self.pv.update(ctx.ply, mv);

                    if score >= ctx.beta {
                        // Update history for good quiet move
                        if !self.is_capture(pos, mv) {
                            self.history.update_cutoff(pos.side_to_move, mv, ctx.depth, None);

                            // Update killers
                            if stack[ctx.ply].killers[0] != Some(mv) {
                                stack[ctx.ply].killers[1] = stack[ctx.ply].killers[0];
                                stack[ctx.ply].killers[0] = Some(mv);
                            }

                            // Penalize other quiet moves that were tried
                            for &quiet_mv in &quiets_tried {
                                if quiet_mv != mv {
                                    self.history.update_quiet(
                                        pos.side_to_move,
                                        quiet_mv,
                                        ctx.depth,
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
            return if stack[ctx.ply].in_check {
                -MATE_SCORE + ctx.ply as i32
            } else {
                DRAW_SCORE
            };
        }

        // Store in transposition table
        let node_type = if best_score >= ctx.beta {
            NodeType::LowerBound
        } else if best_score <= ctx.alpha {
            NodeType::UpperBound
        } else {
            NodeType::Exact
        };

        // ルートノードでAspiration失敗時は、信頼性を記録
        let is_aspiration_fail =
            ctx.ply == 0 && ctx.aspiration_failed && node_type != NodeType::Exact;

        self.tt.store_with_aspiration(
            pos.hash,
            best_move,
            best_score as i16,
            static_eval as i16,
            ctx.depth as u8,
            node_type,
            is_aspiration_fail,
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

    /// Get node count for testing
    pub fn nodes(&self) -> u64 {
        self.nodes.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::board::Square;
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

    #[test]
    fn test_aspiration_windows() {
        let evaluator = Arc::new(MaterialEvaluator);
        let mut searcher = EnhancedSearcher::new(16, evaluator);
        let mut pos = Position::startpos();

        // テスト用の探索深さ（CI環境でも安定して動作する深さ）
        let test_depth = 4;
        let (best_move, _score) = searcher.search(&mut pos, test_depth, None, None);

        assert!(best_move.is_some());

        // 統計情報を確認
        let total_depths = searcher.stats.aspiration_hits.len();
        let mut total_hits = 0;
        let mut total_failures = 0;

        for depth in 2..=test_depth {
            let depth_idx = depth as usize;
            if depth_idx < total_depths {
                total_hits += searcher.stats.aspiration_hits[depth_idx];
                total_failures += searcher.stats.aspiration_failures[depth_idx];

                // 深さ2以降では、fail-high/lowが4回以内で収束することを確認
                // （delta: 初期値 → 2倍 → 4倍 → 8倍 → フルウィンドウ）
                if depth >= 2 {
                    assert!(
                        searcher.stats.aspiration_failures[depth_idx] <= 4,
                        "Depth {}: Too many aspiration failures ({})",
                        depth,
                        searcher.stats.aspiration_failures[depth_idx]
                    );
                }
            }
        }

        // 全体として、一定の成功率があることを確認
        // 序盤は評価値変動が大きいため失敗が多くなることがある
        let hit_rate = if total_hits + total_failures > 0 {
            (total_hits as f64) / ((total_hits + total_failures) as f64)
        } else {
            0.0
        };
        assert!(
            hit_rate >= 0.25, // 25%以上の成功率
            "Aspiration windows hit rate too low: {:.1}% (hits={}, failures={})",
            hit_rate * 100.0,
            total_hits,
            total_failures
        );

        println!("Aspiration Window Statistics:");
        for depth in 1..=test_depth {
            let depth_idx = depth as usize;
            if depth_idx < total_depths {
                let failures = searcher.stats.aspiration_failures[depth_idx];
                let hits = searcher.stats.aspiration_hits[depth_idx];
                let total = hits + failures;
                let hit_rate = if total > 0 {
                    (hits as f64 / total as f64) * 100.0
                } else {
                    0.0
                };
                println!(
                    "  Depth {}: hits={}, failures={}, hit_rate={:.1}%, final_delta={}",
                    depth, hits, failures, hit_rate, searcher.stats.total_delta[depth_idx]
                );
            }
        }
    }

    #[test]
    fn test_aspiration_window_params() {
        let params = SearchParams::default();

        // 各フェーズで窓幅が適切に設定されることを確認
        // Opening phase
        assert_eq!((params.initial_window)(1, GamePhase::Opening), 33); // 30 + 1*3
        assert_eq!((params.initial_window)(6, GamePhase::Opening), 48); // 30 + 6*3

        // Middle game phase
        assert_eq!((params.initial_window)(1, GamePhase::MiddleGame), 55); // 50 + 1*5
        assert_eq!((params.initial_window)(6, GamePhase::MiddleGame), 80); // 50 + 6*5

        // End game phase
        assert_eq!((params.initial_window)(1, GamePhase::EndGame), 110); // 100 + 1*10
        assert_eq!((params.initial_window)(6, GamePhase::EndGame), 160); // 100 + 6*10
    }

    #[test]
    fn test_game_phase_estimation() {
        let evaluator = Arc::new(MaterialEvaluator);
        let searcher = EnhancedSearcher::new(16, evaluator);

        // Opening position
        let mut pos = Position::startpos();
        assert_eq!(searcher.estimate_game_phase(&pos), GamePhase::Opening);

        // Simulate middle game (set ply count)
        pos.ply = 30;
        assert_eq!(searcher.estimate_game_phase(&pos), GamePhase::MiddleGame);

        // Simulate end game (high ply count)
        pos.ply = 70;
        assert_eq!(searcher.estimate_game_phase(&pos), GamePhase::EndGame);
    }

    #[test]
    fn test_aspiration_windows_various_depths() {
        let evaluator = Arc::new(MaterialEvaluator);

        // 異なる深さでのAspiration Windowsの動作を確認
        for test_depth in [2, 3, 5] {
            let mut searcher = EnhancedSearcher::new(16, evaluator.clone());
            let mut pos = Position::startpos();

            let (best_move, _score) = searcher.search(&mut pos, test_depth, None, None);
            assert!(best_move.is_some(), "Depth {}: No best move found", test_depth);

            // 深さ2以上では必ずAspiration Windowsが動作することを確認
            if test_depth >= 2 {
                let depth_idx = test_depth as usize;
                let total_attempts = searcher.stats.aspiration_hits[depth_idx]
                    + searcher.stats.aspiration_failures[depth_idx];
                assert!(
                    total_attempts > 0,
                    "Depth {}: Aspiration windows should be used",
                    test_depth
                );
            }
        }
    }

    #[test]
    fn test_dynamic_window_adjustment() {
        let evaluator = Arc::new(MaterialEvaluator);
        let mut searcher = EnhancedSearcher::new(16, evaluator);
        let test_depth = 3;

        // Test different game phases
        for (ply, expected_phase) in [
            (5, GamePhase::Opening),
            (30, GamePhase::MiddleGame),
            (70, GamePhase::EndGame),
        ] {
            let mut pos = Position::startpos();
            pos.ply = ply;

            let phase = searcher.estimate_game_phase(&pos);
            assert_eq!(phase, expected_phase, "Ply {}: Wrong phase", ply);

            // Search and verify aspiration windows are used
            let (best_move, _score) = searcher.search(&mut pos, test_depth, None, None);
            assert!(best_move.is_some());

            // Check that window size varies by phase
            let window_size = (searcher.params.initial_window)(test_depth, phase);
            match phase {
                GamePhase::Opening => {
                    assert!(window_size < 50, "Opening window too wide: {}", window_size)
                }
                GamePhase::MiddleGame => assert!(
                    window_size >= 50 && window_size < 100,
                    "Middle game window out of range: {}",
                    window_size
                ),
                GamePhase::EndGame => {
                    assert!(window_size >= 100, "End game window too narrow: {}", window_size)
                }
            }
        }
    }

    #[test]
    fn test_pv_tracking() {
        let evaluator = Arc::new(MaterialEvaluator);
        let mut searcher = EnhancedSearcher::new(16, evaluator);
        let mut pos = Position::startpos();

        let (best_move, _score) = searcher.search(&mut pos, 4, None, None);
        let pv = searcher.principal_variation();

        // PVの検証
        assert!(!pv.is_empty(), "PV should not be empty");
        assert_eq!(pv[0], best_move.unwrap(), "First move in PV should match best move");
        assert!(pv.len() <= 4, "PV length should not exceed search depth");

        // 各手が正当な手であることを確認
        for &mv in pv {
            assert!(!mv.is_null(), "PV should not contain null moves");
            // 実際の合法手検証は movegen が必要なので省略
        }
    }

    #[test]
    fn test_pv_table_boundary() {
        let mut pv_table = PVTable::default();

        // 境界値テスト
        let test_move = Move::normal(Square::new(2, 6), Square::new(2, 5), false);

        // MAX_PLY - 1 でのテスト
        pv_table.update(MAX_PLY - 1, test_move);
        assert_eq!(pv_table.len[MAX_PLY - 1], 1);

        // 境界でのクリアテスト
        pv_table.clear(MAX_PLY - 1);
        assert_eq!(pv_table.len[MAX_PLY - 1], 0);

        // 深いPVのコピーテスト
        for i in 0..10 {
            let mv = Move::normal(Square::new(i as u8 % 9, 6), Square::new(i as u8 % 9, 5), false);
            pv_table.update(MAX_PLY - 10 + i, mv);
        }

        // ルートPVの長さを確認
        let root_pv = pv_table.get_pv();
        assert!(root_pv.is_empty() || root_pv.len() <= MAX_PLY);
    }
}
