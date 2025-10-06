use std::collections::HashMap;
use std::env;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use log::warn;

use crate::evaluation::evaluate::Evaluator;
use crate::movegen::MoveGenerator;
use crate::search::api::{BackendSearchTask, InfoEvent, InfoEventCallback, SearcherBackend};
use crate::search::parallel::FinalizeReason;
use crate::search::params as dynp;
use crate::search::types::{NodeType, RootLine, SearchStack, StopInfo, TerminationReason};
use crate::search::{SearchLimits, SearchResult, SearchStats, TranspositionTable};
use crate::Position;
use smallvec::SmallVec;

use super::ordering::{self, Heuristics};
use super::profile::{PruneToggles, SearchProfile};
use super::pvs::{self, SearchContext};
#[cfg(feature = "diagnostics")]
use super::qsearch::{publish_qsearch_diagnostics, reset_qsearch_diagnostics};
use crate::search::tt::TTProbe;
use crate::time_management::TimeControl;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeadlineHit {
    Stop,
    Hard,
    Soft,
}

static SEARCH_THREAD_SEQ: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub struct ClassicBackend<E: Evaluator + Send + Sync + 'static> {
    pub(super) evaluator: Arc<E>,
    pub(super) tt: Option<Arc<TranspositionTable>>, // 共有TT（Hashfull出力用、将来はprobe/storeでも使用）
    pub(super) profile: SearchProfile,
}

impl<E: Evaluator + Send + Sync + 'static> ClassicBackend<E> {
    #[inline]
    fn is_byoyomi_active(limits: &SearchLimits) -> bool {
        matches!(limits.time_control, TimeControl::Byoyomi { .. })
            || limits.time_manager.as_ref().is_some_and(|tm| tm.is_in_byoyomi())
    }

    fn compute_qnodes_limit(limits: &SearchLimits, depth: i32, pv_idx: usize) -> u64 {
        let mut limit =
            limits.qnodes_limit.unwrap_or(crate::search::constants::DEFAULT_QNODES_LIMIT);
        let byoyomi_active = Self::is_byoyomi_active(limits);

        if let Some(tm) = limits.time_manager.as_ref() {
            let soft = tm.soft_limit_ms();
            if soft > 0 && soft != u64::MAX {
                let base_scaled = soft.saturating_mul(crate::search::constants::QNODES_PER_MS);
                limit = limit.min(base_scaled);

                let elapsed = tm.elapsed_ms();
                if elapsed < soft {
                    let remaining = soft - elapsed;
                    let dynamic = remaining
                        .saturating_mul(crate::search::constants::QNODES_PER_MS)
                        .saturating_add(crate::search::constants::MIN_QNODES_LIMIT / 2);
                    limit = limit.min(dynamic);
                } else {
                    limit = limit.min(crate::search::constants::MIN_QNODES_LIMIT);
                }
            }
        } else if let Some(duration) = limits.time_limit() {
            let soft_ms = duration.as_millis() as u64;
            if soft_ms > 0 {
                let scaled = soft_ms.saturating_mul(crate::search::constants::QNODES_PER_MS);
                limit = limit.min(scaled);
            }
        }

        if pv_idx > 1 {
            let divisor = (pv_idx as u64).saturating_add(1);
            limit /= divisor;
        }

        if byoyomi_active {
            let base = (limit / 2).max(crate::search::constants::MIN_QNODES_LIMIT);
            let depth_scale = 100
                + (depth.max(1) as u64)
                    .saturating_mul(crate::search::constants::QNODES_DEPTH_BONUS_PCT);
            limit = base.saturating_mul(depth_scale).saturating_add(99) / 100;
        }

        limit.clamp(
            crate::search::constants::MIN_QNODES_LIMIT,
            crate::search::constants::DEFAULT_QNODES_LIMIT,
        )
    }

    #[cfg(test)]
    pub(crate) fn compute_qnodes_limit_for_test(
        limits: &SearchLimits,
        depth: i32,
        pv_idx: usize,
    ) -> u64 {
        Self::compute_qnodes_limit(limits, depth, pv_idx)
    }

    #[inline]
    fn deadline_hit(
        start: Instant,
        soft: Option<Duration>,
        hard: Option<Duration>,
        limits: &SearchLimits,
        min_think_ms: u64,
        current_nodes: u64,
    ) -> Option<DeadlineHit> {
        if Self::should_stop(limits) {
            return Some(DeadlineHit::Stop);
        }
        if let Some(tm) = limits.time_manager.as_ref() {
            if tm.should_stop(current_nodes) {
                let hard_limit = tm.hard_limit_ms();
                let tm_elapsed = tm.elapsed_ms();
                if hard_limit != u64::MAX && tm_elapsed >= hard_limit {
                    return Some(DeadlineHit::Hard);
                }
                return Some(DeadlineHit::Soft);
            }
            return None;
        }

        let elapsed = start.elapsed();
        let elapsed_ms = elapsed.as_millis() as u64;
        let min_think_satisfied = min_think_ms == 0 || elapsed_ms >= min_think_ms;

        if let Some(limit) = hard {
            if elapsed >= limit {
                return Some(DeadlineHit::Hard);
            }
        }
        if let Some(limit) = soft {
            if elapsed >= limit && min_think_satisfied {
                return Some(DeadlineHit::Soft);
            }
        }
        if let Some(limit) = limits.time_limit() {
            if elapsed >= limit && min_think_satisfied {
                // 固定時間探索はリードウィンドウで緩やかに停止させる方針のため、min_think を満たした後は Hard ではなく Soft とみなす。
                return Some(DeadlineHit::Soft);
            }
        }
        None
    }
    #[inline]
    pub(crate) fn classify_root_bound(local_best: i32, alpha_win: i32, beta_win: i32) -> NodeType {
        if local_best <= alpha_win {
            NodeType::UpperBound
        } else if local_best >= beta_win {
            NodeType::LowerBound
        } else {
            NodeType::Exact
        }
    }

    fn currmove_throttle_ms() -> Option<u64> {
        static POLICY: OnceLock<Option<u64>> = OnceLock::new();
        *POLICY.get_or_init(|| match env::var("SHOGI_CURRMOVE_THROTTLE_MS") {
            Ok(val) => {
                let val = val.trim().to_ascii_lowercase();
                if val == "off" || val == "0" {
                    None
                } else {
                    val.parse::<u64>().ok().filter(|v| *v > 0)
                }
            }
            Err(_) => Some(150),
        })
    }

    pub fn new(evaluator: Arc<E>) -> Self {
        Self::with_profile(evaluator, SearchProfile::default())
    }

    pub fn with_tt(evaluator: Arc<E>, tt: Arc<TranspositionTable>) -> Self {
        Self::with_profile_and_tt(evaluator, tt, SearchProfile::default())
    }

    pub fn with_tt_and_toggles(
        evaluator: Arc<E>,
        tt: Arc<TranspositionTable>,
        toggles: PruneToggles,
    ) -> Self {
        let mut profile = SearchProfile::enhanced_material();
        profile.prune = toggles;
        Self::with_profile_and_tt(evaluator, tt, profile)
    }

    pub fn with_tt_and_toggles_apply_defaults(
        evaluator: Arc<E>,
        tt: Arc<TranspositionTable>,
        toggles: PruneToggles,
    ) -> Self {
        let mut profile = SearchProfile::enhanced_material();
        profile.prune = toggles;
        profile.apply_runtime_defaults();
        Self::with_profile_and_tt(evaluator, tt, profile)
    }

    pub fn with_profile(evaluator: Arc<E>, profile: SearchProfile) -> Self {
        Self {
            evaluator,
            tt: None,
            profile,
        }
    }

    pub fn with_profile_apply_defaults(evaluator: Arc<E>, profile: SearchProfile) -> Self {
        profile.apply_runtime_defaults();
        Self::with_profile(evaluator, profile)
    }

    pub fn with_profile_and_tt(
        evaluator: Arc<E>,
        tt: Arc<TranspositionTable>,
        profile: SearchProfile,
    ) -> Self {
        Self {
            evaluator,
            tt: Some(tt),
            profile,
        }
    }

    pub fn with_profile_and_tt_apply_defaults(
        evaluator: Arc<E>,
        tt: Arc<TranspositionTable>,
        profile: SearchProfile,
    ) -> Self {
        profile.apply_runtime_defaults();
        Self::with_profile_and_tt(evaluator, tt, profile)
    }

    pub(super) fn should_stop(limits: &SearchLimits) -> bool {
        if let Some(flag) = &limits.stop_flag {
            return flag.load(Ordering::Acquire);
        }
        false
    }

    pub(super) fn iterative(
        &self,
        root: &Position,
        limits: &SearchLimits,
        info: Option<&InfoEventCallback>,
    ) -> SearchResult {
        let max_depth = limits.depth_limit_u8() as i32;
        let mut best: Option<crate::shogi::Move> = None;
        let mut best_score = 0;
        let mut nodes: u64 = 0;
        let t0 = Instant::now();
        let deadlines = limits.fallback_deadlines;
        let (soft_deadline, hard_deadline) = if let Some(dl) = deadlines {
            (
                (dl.soft_limit_ms > 0).then(|| Duration::from_millis(dl.soft_limit_ms)),
                (dl.hard_limit_ms > 0).then(|| Duration::from_millis(dl.hard_limit_ms)),
            )
        } else if let Some(limit) = limits.time_limit() {
            // 固定時間探索は Hard 判定を用いず、リードウィンドウと Soft 停止で丸める。
            (Some(limit), None)
        } else {
            (None, None)
        };
        let min_think_ms = if limits.is_ponder {
            0
        } else {
            limits.time_parameters.as_ref().map(|tp| tp.min_think_ms).unwrap_or(0)
        };
        let _last_hashfull_emit_ms = 0u64;
        let mut prev_score = 0;
        // Aspiration initial params
        const ASP_DELTA0: i32 = 30;
        const ASP_DELTA_MAX: i32 = 350;
        const SELDEPTH_EXTRA_MARGIN: u32 = 32;

        // Cumulative counters for diagnostics
        let mut cum_tt_hits: u64 = 0;
        let mut cum_beta_cuts: u64 = 0;
        let mut cum_lmr_counter: u64 = 0;
        let mut cum_lmr_trials: u64 = 0;
        let mut stats_hint_exists: u64 = 0;
        let mut stats_hint_used: u64 = 0;

        self.evaluator.on_set_position(root);

        #[cfg(feature = "diagnostics")]
        reset_qsearch_diagnostics();

        #[cfg(any(debug_assertions, feature = "diagnostics"))]
        {
            super::diagnostics::clear();
            super::diagnostics::configure_abort_handles(
                limits.stop_controller.clone(),
                limits.stop_flag.clone(),
            );
        }

        let mut final_lines: Option<SmallVec<[RootLine; 4]>> = None;
        let mut final_depth_reached: u8 = 0;
        let mut final_seldepth_reached: Option<u8> = None;
        let mut final_seldepth_raw: Option<u32> = None;
        let mut iterative_heur = Heuristics::default();
        let stop_controller = limits.stop_controller.clone();
        let mut finalize_soft_sent = false;
        let mut last_deadline_hit: Option<DeadlineHit> = None;
        let mut lead_window_soft_break = false;
        let mut finalize_hard_sent = false;
        let mut notify_deadline = |hit: DeadlineHit, nodes_now: u64| {
            if let Some(cb) = limits.info_string_callback.as_ref() {
                let elapsed = t0.elapsed().as_millis();
                cb(&format!(
                    "deadline_hit kind={:?} elapsed_ms={} nodes={}",
                    hit, elapsed, nodes_now
                ));
            }
            if let Some(ctrl) = stop_controller.as_ref() {
                match hit {
                    DeadlineHit::Hard => {
                        if !finalize_hard_sent {
                            ctrl.request_finalize(FinalizeReason::Hard);
                            finalize_hard_sent = true;
                        }
                    }
                    DeadlineHit::Soft => {
                        if !finalize_soft_sent {
                            ctrl.request_finalize(FinalizeReason::Planned);
                            finalize_soft_sent = true;
                        }
                    }
                    DeadlineHit::Stop => {}
                }
            }
        };

        static LEAD_WINDOW_FINALIZE_ENABLED: OnceLock<bool> = OnceLock::new();
        let lead_window_finalize = *LEAD_WINDOW_FINALIZE_ENABLED.get_or_init(|| {
            match env::var("SHOGI_LEAD_WINDOW_FINALIZE") {
                Ok(val) => {
                    let normalized = val.trim().to_ascii_lowercase();
                    !(normalized == "off" || normalized == "0" || normalized == "false")
                }
                Err(_) => true,
            }
        });

        for d in 1..=max_depth {
            #[cfg(any(debug_assertions, feature = "diagnostics"))]
            if super::diagnostics::should_abort_now() {
                last_deadline_hit = Some(DeadlineHit::Stop);
                break;
            }
            #[cfg(feature = "diagnostics")]
            reset_qsearch_diagnostics();
            if let Some(cb) = limits.info_string_callback.as_ref() {
                cb(&format!("iter_start depth={} nodes={}", d, nodes));
                if let Some(tt) = &self.tt {
                    cb(&format!(
                        "tt_snapshot depth={} hf={} hf_phys={} attempts={}",
                        d,
                        tt.hashfull_permille(),
                        tt.hashfull_physical_permille(),
                        tt.store_attempts()
                    ));
                }
            }
            if let Some(hit) =
                Self::deadline_hit(t0, soft_deadline, hard_deadline, limits, min_think_ms, nodes)
            {
                notify_deadline(hit, nodes);
                last_deadline_hit = Some(hit);
                break;
            }
            let mut seldepth: u32 = 0;
            let throttle_ms = Self::currmove_throttle_ms();
            let mut last_currmove_emit = Instant::now();
            let prev_root_lines = final_lines.as_ref().map(|lines| lines.as_slice());
            // Build root move list for CurrMove events and basic ordering
            let mg = MoveGenerator::new();
            let Ok(list) = mg.generate_all(root) else {
                break;
            };
            // Root TT hint boost（存在すれば大ボーナス）
            let mut root_tt_hint_mv: Option<crate::shogi::Move> = None;
            let root_hash = root.zobrist_hash();
            if let Some(tt) = &self.tt {
                if dynp::tt_prefetch_enabled() {
                    tt.prefetch_l2(root_hash, root.side_to_move);
                }
                if let Some(entry) = tt.probe(root_hash, root.side_to_move) {
                    if let Some(ttm) = entry.get_move() {
                        root_tt_hint_mv = Some(ttm);
                    }
                }
            }
            let mut root_picker =
                ordering::RootPicker::new(root, list.as_slice(), root_tt_hint_mv, prev_root_lines);
            let mut root_moves: Vec<(crate::shogi::Move, i32)> =
                Vec::with_capacity(list.as_slice().len());
            while let Some((mv, key)) = root_picker.next() {
                root_moves.push((mv, key));
            }
            if root_moves.is_empty() {
                break;
            }
            let root_rank: Vec<crate::shogi::Move> = root_moves.iter().map(|(m, _)| *m).collect();
            let mut rank_map: HashMap<u32, u32> = HashMap::with_capacity(root_rank.len());
            for (idx, mv) in root_rank.iter().enumerate() {
                rank_map.entry(mv.to_u32()).or_insert(idx as u32 + 1);
            }

            let root_static_eval = self.evaluator.evaluate(root);
            let root_static_eval_i16 =
                root_static_eval.clamp(i16::MIN as i32, i16::MAX as i32) as i16;

            // MultiPV（逐次選抜）
            let k = limits.multipv.max(1) as usize;
            let mut excluded: SmallVec<[crate::shogi::Move; 32]> = SmallVec::new();
            let mut depth_lines: SmallVec<[RootLine; 4]> = SmallVec::new();
            let required_multipv_lines = if k > 1 {
                root_moves.len().min(k).max(1)
            } else {
                1
            };

            // Counters aggregate across PVs at this depth
            let mut depth_tt_hits: u64 = 0;
            let mut depth_beta_cuts: u64 = 0;
            let mut depth_lmr_counter: u64 = 0;
            let mut depth_lmr_trials: u64 = 0;
            let mut _local_best_for_next_iter: Option<(crate::shogi::Move, i32)> = None;
            let mut depth_hint_exists: u64 = 0;
            let mut depth_hint_used: u64 = 0;
            let mut line_nodes_checkpoint = nodes;
            let mut line_time_checkpoint = t0.elapsed().as_millis() as u64;
            if d % 2 == 0 {
                iterative_heur.age_all();
            }
            let mut shared_heur = std::mem::take(&mut iterative_heur);
            shared_heur.lmr_trials = 0;
            for pv_idx in 1..=k {
                if let Some(hit) = Self::deadline_hit(
                    t0,
                    soft_deadline,
                    hard_deadline,
                    limits,
                    min_think_ms,
                    nodes,
                ) {
                    notify_deadline(hit, nodes);
                    last_deadline_hit = Some(hit);
                    match hit {
                        DeadlineHit::Stop | DeadlineHit::Hard => break,
                        DeadlineHit::Soft => {
                            if depth_lines.len() >= required_multipv_lines {
                                break;
                            }
                        }
                    }
                }
                // Aspiration window per PV head
                let mut alpha = if d == 1 {
                    i32::MIN / 2
                } else {
                    prev_score - ASP_DELTA0
                };
                let mut beta = if d == 1 {
                    i32::MAX / 2
                } else {
                    prev_score + ASP_DELTA0
                };
                let mut delta = ASP_DELTA0;
                let mut window_alpha = alpha;
                let mut window_beta = beta;

                // 検索用stack/heuristicsを初期化
                let mut stack = vec![SearchStack::default(); crate::search::constants::MAX_PLY + 1];
                let mut heur = std::mem::take(&mut shared_heur);
                let lmr_trials_checkpoint = heur.lmr_trials;
                let mut tt_hits: u64 = 0;
                let mut beta_cuts: u64 = 0;
                let mut lmr_counter: u64 = 0;
                let mut root_tt_hint_exists: u64 = 0;
                let mut root_tt_hint_used: u64 = 0;
                let mut qnodes: u64 = 0;
                let qnodes_limit = Self::compute_qnodes_limit(limits, d, pv_idx);

                // 作業用root move配列（excludedを除外）
                let excluded_keys: SmallVec<[u32; 32]> =
                    excluded.iter().map(|m| m.to_u32()).collect();
                let active_moves: SmallVec<[(crate::shogi::Move, i32); 64]> = root_moves
                    .iter()
                    .copied()
                    .filter(|(m, _)| {
                        let key = m.to_u32();
                        // MultiPV では完全一致の手のみ除外し、昇成・不成などの派生は別ラインとして扱う。
                        !excluded_keys.contains(&key)
                    })
                    .collect();

                // 探索ループ（Aspiration）
                let mut local_best_mv = None;
                let mut local_best = i32::MIN / 2;
                loop {
                    if let Some(hit) = Self::deadline_hit(
                        t0,
                        soft_deadline,
                        hard_deadline,
                        limits,
                        min_think_ms,
                        nodes,
                    ) {
                        notify_deadline(hit, nodes);
                        last_deadline_hit = Some(hit);
                        match hit {
                            DeadlineHit::Stop | DeadlineHit::Hard => break,
                            DeadlineHit::Soft => {
                                if depth_lines.len() >= required_multipv_lines {
                                    break;
                                }
                            }
                        }
                    }
                    #[cfg(any(debug_assertions, feature = "diagnostics"))]
                    if super::diagnostics::should_abort_now() {
                        last_deadline_hit = Some(DeadlineHit::Stop);
                        break;
                    }
                    if active_moves.is_empty() {
                        break;
                    }
                    let (old_alpha, old_beta) = (alpha, beta);
                    window_alpha = old_alpha;
                    window_beta = old_beta;
                    // Root move loop with CurrMove events
                    for (idx, (mv, _)) in active_moves.iter().copied().enumerate() {
                        #[cfg(any(debug_assertions, feature = "diagnostics"))]
                        if super::diagnostics::should_abort_now() {
                            last_deadline_hit = Some(DeadlineHit::Stop);
                            break;
                        }
                        if let Some(hit) = Self::deadline_hit(
                            t0,
                            soft_deadline,
                            hard_deadline,
                            limits,
                            min_think_ms,
                            nodes,
                        ) {
                            notify_deadline(hit, nodes);
                            last_deadline_hit = Some(hit);
                            match hit {
                                DeadlineHit::Stop | DeadlineHit::Hard => break,
                                DeadlineHit::Soft => {
                                    if depth_lines.len() >= required_multipv_lines {
                                        break;
                                    }
                                }
                            }
                        }
                        if let Some(cb) = info {
                            let emit = match throttle_ms {
                                None => true,
                                Some(ms) => {
                                    idx == 0
                                        || last_currmove_emit.elapsed() >= Duration::from_millis(ms)
                                }
                            };
                            if emit {
                                last_currmove_emit = Instant::now();
                                let number =
                                    rank_map.get(&mv.to_u32()).copied().unwrap_or((idx as u32) + 1);
                                cb(InfoEvent::CurrMove { mv, number });
                            }
                        }
                        let mut child = root.clone();
                        let score = {
                            let _guard =
                                ordering::EvalMoveGuard::new(self.evaluator.as_ref(), root, mv);
                            child.do_move(mv);
                            if idx == 0 {
                                let mut search_ctx = SearchContext {
                                    limits,
                                    start_time: &t0,
                                    nodes: &mut nodes,
                                    seldepth: &mut seldepth,
                                    qnodes: &mut qnodes,
                                    qnodes_limit,
                                };
                                let (sc, _) = self.alphabeta(
                                    pvs::ABArgs {
                                        pos: &child,
                                        depth: d - 1,
                                        alpha: -beta,
                                        beta: -alpha,
                                        ply: 1,
                                        is_pv: true,
                                        stack: &mut stack,
                                        heur: &mut heur,
                                        tt_hits: &mut tt_hits,
                                        beta_cuts: &mut beta_cuts,
                                        lmr_counter: &mut lmr_counter,
                                    },
                                    &mut search_ctx,
                                );
                                -sc
                            } else {
                                let mut search_ctx_nw = SearchContext {
                                    limits,
                                    start_time: &t0,
                                    nodes: &mut nodes,
                                    seldepth: &mut seldepth,
                                    qnodes: &mut qnodes,
                                    qnodes_limit,
                                };
                                let (sc_nw, _) = self.alphabeta(
                                    pvs::ABArgs {
                                        pos: &child,
                                        depth: d - 1,
                                        alpha: -(alpha + 1),
                                        beta: -alpha,
                                        ply: 1,
                                        is_pv: false,
                                        stack: &mut stack,
                                        heur: &mut heur,
                                        tt_hits: &mut tt_hits,
                                        beta_cuts: &mut beta_cuts,
                                        lmr_counter: &mut lmr_counter,
                                    },
                                    &mut search_ctx_nw,
                                );
                                let mut s = -sc_nw;
                                if s > alpha && s < beta {
                                    let mut search_ctx_fw = SearchContext {
                                        limits,
                                        start_time: &t0,
                                        nodes: &mut nodes,
                                        seldepth: &mut seldepth,
                                        qnodes: &mut qnodes,
                                        qnodes_limit,
                                    };
                                    let (sc_fw, _) = self.alphabeta(
                                        pvs::ABArgs {
                                            pos: &child,
                                            depth: d - 1,
                                            alpha: -beta,
                                            beta: -alpha,
                                            ply: 1,
                                            is_pv: true,
                                            stack: &mut stack,
                                            heur: &mut heur,
                                            tt_hits: &mut tt_hits,
                                            beta_cuts: &mut beta_cuts,
                                            lmr_counter: &mut lmr_counter,
                                        },
                                        &mut search_ctx_fw,
                                    );
                                    s = -sc_fw;
                                }
                                s
                            }
                        };
                        if score > local_best {
                            local_best = score;
                            local_best_mv = Some(mv);
                        }
                        if score > alpha {
                            alpha = score;
                        }
                        if alpha >= beta {
                            break; // fail-high
                        }
                    }

                    #[cfg(any(debug_assertions, feature = "diagnostics"))]
                    if super::diagnostics::should_abort_now() {
                        break;
                    }

                    if let Some(hit) = Self::deadline_hit(
                        t0,
                        soft_deadline,
                        hard_deadline,
                        limits,
                        min_think_ms,
                        nodes,
                    ) {
                        notify_deadline(hit, nodes);
                        last_deadline_hit = Some(hit);
                        match hit {
                            DeadlineHit::Stop | DeadlineHit::Hard => break,
                            DeadlineHit::Soft => {
                                if depth_lines.len() >= required_multipv_lines {
                                    break;
                                }
                            }
                        }
                    }
                    if local_best <= old_alpha {
                        if let Some(cb) = info {
                            cb(InfoEvent::Aspiration {
                                outcome: crate::search::api::AspirationOutcome::FailLow,
                                old_alpha,
                                old_beta,
                                new_alpha: old_alpha.saturating_sub(2 * delta),
                                new_beta: old_beta,
                            });
                        }
                        alpha = old_alpha.saturating_sub(2 * delta).max(i32::MIN / 2);
                        beta = old_beta;
                        delta = (delta * 2).min(ASP_DELTA_MAX);
                        continue;
                    }
                    if local_best >= old_beta {
                        if let Some(cb) = info {
                            cb(InfoEvent::Aspiration {
                                outcome: crate::search::api::AspirationOutcome::FailHigh,
                                old_alpha,
                                old_beta,
                                new_alpha: old_alpha,
                                new_beta: old_beta.saturating_add(2 * delta),
                            });
                        }
                        alpha = old_alpha;
                        beta = old_beta.saturating_add(2 * delta).min(i32::MAX / 2);
                        delta = (delta * 2).min(ASP_DELTA_MAX);
                        continue;
                    }
                    break; // success within window
                }

                #[cfg(any(debug_assertions, feature = "diagnostics"))]
                if super::diagnostics::should_abort_now() {
                    last_deadline_hit = Some(DeadlineHit::Stop);
                    break;
                }

                // Counters aggregate
                depth_tt_hits = depth_tt_hits.saturating_add(tt_hits);
                depth_beta_cuts = depth_beta_cuts.saturating_add(beta_cuts);
                depth_lmr_counter = depth_lmr_counter.saturating_add(lmr_counter);
                depth_lmr_trials = depth_lmr_trials
                    .saturating_add(heur.lmr_trials.saturating_sub(lmr_trials_checkpoint));
                shared_heur = heur;

                // 発火: Depth / Hashfull（深さ1回の発火で十分）
                if pv_idx == 1 {
                    if let Some(cb) = info {
                        let reported_sd =
                            seldepth.min(d as u32 + SELDEPTH_EXTRA_MARGIN).min(u8::MAX as u32);
                        cb(InfoEvent::Depth {
                            depth: d as u32,
                            seldepth: reported_sd,
                        });
                        if let Some(tt) = &self.tt {
                            let hf = tt.hashfull_permille() as u32;
                            cb(InfoEvent::Hashfull(hf));
                        }
                    }
                }

                // PV 行の生成と発火
                if let Some(m) = local_best_mv {
                    // 次反復のAspiration用に pv_idx==1 を採用
                    if pv_idx == 1 {
                        best = Some(m);
                        best_score = local_best;
                        prev_score = local_best;
                        if let Some(hint) = root_tt_hint_mv {
                            root_tt_hint_exists = 1;
                            if m.to_u32() == hint.to_u32() {
                                root_tt_hint_used = 1;
                            }
                        }
                        depth_hint_exists = root_tt_hint_exists;
                        depth_hint_used = root_tt_hint_used;
                        _local_best_for_next_iter = Some((m, local_best));
                    }
                    // 可能ならTTからPVを復元し、だめなら軽量再探索へフォールバック
                    let mut pv = self.reconstruct_root_pv_from_tt(root, d, m).unwrap_or_default();
                    if pv.is_empty() {
                        let pv_ex = self.extract_pv(root, d, m, limits, &mut nodes);
                        if pv_ex.is_empty() {
                            pv.push(m);
                        } else {
                            pv = pv_ex;
                        }
                    }
                    let elapsed_ms_total = t0.elapsed().as_millis() as u64;
                    let current_nodes = nodes;
                    let line_nodes = current_nodes.saturating_sub(line_nodes_checkpoint);
                    let line_time_ms = elapsed_ms_total.saturating_sub(line_time_checkpoint);
                    let line_nps = if line_time_ms > 0 {
                        Some(line_nodes.saturating_mul(1000) / line_time_ms.max(1))
                    } else {
                        None
                    };
                    let alpha = window_alpha;
                    let beta = window_beta;
                    let bound = Self::classify_root_bound(local_best, alpha, beta);
                    let line = RootLine {
                        multipv_index: pv_idx as u8,
                        root_move: m,
                        score_internal: local_best,
                        score_cp: local_best,
                        bound,
                        depth: d as u32,
                        seldepth: Some(
                            seldepth.min(d as u32 + SELDEPTH_EXTRA_MARGIN).min(u8::MAX as u32)
                                as u8,
                        ),
                        pv,
                        nodes: Some(line_nodes),
                        time_ms: Some(line_time_ms),
                        nps: line_nps,
                        exact_exhausted: false,
                        exhaust_reason: None,
                        mate_distance: None,
                    };
                    let line_arc = Arc::new(line);
                    if let Some(cb) = info {
                        cb(InfoEvent::PV {
                            line: Arc::clone(&line_arc),
                        });
                    }
                    depth_lines.push(match Arc::try_unwrap(line_arc) {
                        Ok(line) => line,
                        Err(arc) => (*arc).clone(),
                    });
                    // TT保存は 1行目のみ（Exact, PV=true）
                    if pv_idx == 1 {
                        if let (Some(tt), Some(best_mv_root)) = (&self.tt, best) {
                            let node_type = NodeType::Exact;
                            let store_score =
                                crate::search::common::adjust_mate_score_for_tt(best_score, 0)
                                    .clamp(i16::MIN as i32, i16::MAX as i32)
                                    as i16;
                            let mut args = crate::search::tt::TTStoreArgs::new(
                                root_hash,
                                Some(best_mv_root),
                                store_score,
                                root_static_eval_i16,
                                d as u8,
                                node_type,
                                root.side_to_move,
                            );
                            args.is_pv = true;
                            tt.store(args);
                        }
                    }
                    // 除外へ追加
                    excluded.push(m);
                    line_nodes_checkpoint = current_nodes;
                    line_time_checkpoint = elapsed_ms_total;
                } else {
                    // 局面が詰み/手なし等でPVが取れない → 打ち切り
                    break;
                }
            }

            // 深さ集計を累積
            cum_tt_hits = cum_tt_hits.saturating_add(depth_tt_hits);
            cum_beta_cuts = cum_beta_cuts.saturating_add(depth_beta_cuts);
            cum_lmr_counter = cum_lmr_counter.saturating_add(depth_lmr_counter);
            cum_lmr_trials = cum_lmr_trials.saturating_add(depth_lmr_trials);

            // 反復ごとのrootヒント統計（最終反復で掲載）
            stats_hint_exists = depth_hint_exists;
            stats_hint_used = depth_hint_used;
            // この深さのMultiPV行を最終結果候補として保持
            final_lines = Some(depth_lines.clone());
            final_depth_reached = d as u8;
            let capped_seldepth =
                seldepth.min(d as u32 + SELDEPTH_EXTRA_MARGIN).min(u8::MAX as u32) as u8;

            iterative_heur = shared_heur;
            final_seldepth_reached = Some(capped_seldepth);
            final_seldepth_raw = Some(seldepth);

            let mut lead_ms = 10u64;

            if let Some(cb) = limits.info_string_callback.as_ref() {
                let elapsed_ms = t0.elapsed().as_millis();
                let msg =
                    format!("iter_complete depth={} elapsed_ms={} nodes={}", d, elapsed_ms, nodes);
                cb(msg.as_str());
            }

            #[cfg(feature = "diagnostics")]
            publish_qsearch_diagnostics(d, limits.info_string_callback.as_ref());

            if Self::should_stop(limits) {
                if let Some(cb) = limits.info_string_callback.as_ref() {
                    let elapsed = t0.elapsed().as_millis();
                    cb(&format!(
                        "stop_flag_break depth={} elapsed_ms={} nodes={}",
                        d, elapsed, nodes
                    ));
                }
                break;
            }

            let mut check_lead_window =
                |reason: &'static str, deadline: Duration, lead_ms_current: u64| {
                    if t0.elapsed() + Duration::from_millis(lead_ms_current) >= deadline {
                        if let Some(cb) = limits.info_string_callback.as_ref() {
                            let elapsed = t0.elapsed().as_millis();
                            cb(&format!(
                            "stop_lead_break reason={} depth={} elapsed_ms={} nodes={} lead_ms={}",
                            reason, d, elapsed, nodes, lead_ms_current
                        ));
                        }
                        if lead_window_finalize {
                            notify_deadline(DeadlineHit::Soft, nodes);
                        }
                        if !matches!(last_deadline_hit, Some(DeadlineHit::Hard)) {
                            last_deadline_hit = Some(DeadlineHit::Soft);
                        }
                        lead_window_soft_break = true;
                        return true;
                    }
                    false
                };

            if let Some(hard) = hard_deadline {
                if let Some(soft) = soft_deadline {
                    if hard > soft {
                        let diff = hard.as_millis().saturating_sub(soft.as_millis()) as u64;
                        if diff > 0 {
                            lead_ms = lead_ms.max(diff);
                        }
                    }
                }

                // 固定時間探索では min_think よりも締切回避を優先したいので、リードウィンドウは min_think 判定を迂回して早期停止させる。
                if check_lead_window("hard_window", hard, lead_ms) {
                    break;
                }

                continue;
            }

            if let Some(limit) = limits.time_limit() {
                if check_lead_window("time_limit", limit, lead_ms) {
                    break;
                }
            }
        }
        // stats は最終反復の集計値を使う
        let mut stats = SearchStats {
            nodes,
            ..Default::default()
        };
        stats.elapsed = t0.elapsed();
        stats.depth = final_depth_reached;
        stats.seldepth = final_seldepth_reached;
        stats.raw_seldepth = final_seldepth_raw.map(|v| v.min(u16::MAX as u32) as u16);
        stats.tt_hits = Some(cum_tt_hits);
        stats.lmr_count = Some(cum_lmr_counter);
        stats.lmr_trials = Some(cum_lmr_trials);
        stats.root_fail_high_count = Some(cum_beta_cuts);
        stats.root_tt_hint_exists = Some(stats_hint_exists);
        stats.root_tt_hint_used = Some(stats_hint_used);
        if let Some(first_line) = final_lines.as_ref().and_then(|lines| lines.first()) {
            stats.pv = first_line.pv.iter().copied().collect();
        }
        let mut result = SearchResult::new(best, best_score, stats);
        if let Some(lines) = final_lines {
            result.lines = Some(lines);
            result.refresh_summary();
        }
        if let Some(tt) = &self.tt {
            result.hashfull = tt.hashfull_permille() as u32;
        }

        if result.stop_info.is_none() {
            if let Some(tm) = limits.time_manager.as_ref() {
                let elapsed = tm.elapsed_ms();
                let hard_ms = tm.hard_limit_ms();
                let soft_ms = tm.soft_limit_ms();
                let hard_timeout = matches!(last_deadline_hit, Some(DeadlineHit::Hard))
                    || (hard_ms != u64::MAX && elapsed >= hard_ms);
                let reason = match last_deadline_hit {
                    Some(DeadlineHit::Stop) => TerminationReason::UserStop,
                    Some(DeadlineHit::Hard) | Some(DeadlineHit::Soft) => {
                        TerminationReason::TimeLimit
                    }
                    None => {
                        if Self::should_stop(limits) {
                            TerminationReason::UserStop
                        } else {
                            TerminationReason::Completed
                        }
                    }
                };
                result.stop_info = Some(StopInfo {
                    reason,
                    elapsed_ms: elapsed,
                    nodes,
                    depth_reached: final_depth_reached,
                    hard_timeout,
                    soft_limit_ms: if soft_ms != u64::MAX { soft_ms } else { 0 },
                    hard_limit_ms: if hard_ms != u64::MAX { hard_ms } else { 0 },
                });
                result.end_reason = reason;
            } else if let Some(dl) = limits.fallback_deadlines {
                let elapsed = t0.elapsed().as_millis() as u64;
                let hard_timeout = elapsed >= dl.hard_limit_ms;
                let soft_hit = dl.soft_limit_ms > 0 && elapsed >= dl.soft_limit_ms;
                let time_limited = hard_timeout
                    || soft_hit
                    || lead_window_soft_break
                    || matches!(last_deadline_hit, Some(DeadlineHit::Soft));
                let reason = if time_limited {
                    TerminationReason::TimeLimit
                } else if matches!(last_deadline_hit, Some(DeadlineHit::Stop))
                    || Self::should_stop(limits)
                {
                    TerminationReason::UserStop
                } else {
                    TerminationReason::Completed
                };
                result.stop_info = Some(StopInfo {
                    reason,
                    elapsed_ms: elapsed,
                    nodes,
                    depth_reached: final_depth_reached,
                    hard_timeout: hard_timeout && !lead_window_soft_break,
                    soft_limit_ms: dl.soft_limit_ms,
                    hard_limit_ms: dl.hard_limit_ms,
                });
                result.end_reason = reason;
            } else if let Some(limit) = limits.time_limit() {
                let cap_ms = limit.as_millis() as u64;
                let elapsed = t0.elapsed().as_millis() as u64;
                let mut hard_timeout = match last_deadline_hit {
                    Some(DeadlineHit::Hard) => true,
                    Some(DeadlineHit::Soft) => false,
                    _ => elapsed >= cap_ms,
                };
                if lead_window_soft_break {
                    hard_timeout = false;
                }
                let reason = if hard_timeout
                    || matches!(last_deadline_hit, Some(DeadlineHit::Soft))
                    || lead_window_soft_break
                {
                    TerminationReason::TimeLimit
                } else if matches!(last_deadline_hit, Some(DeadlineHit::Stop))
                    || Self::should_stop(limits)
                {
                    TerminationReason::UserStop
                } else {
                    TerminationReason::Completed
                };

                result.stop_info = Some(StopInfo {
                    reason,
                    elapsed_ms: elapsed,
                    nodes,
                    depth_reached: final_depth_reached,
                    hard_timeout,
                    soft_limit_ms: cap_ms,
                    hard_limit_ms: cap_ms,
                });
                result.end_reason = reason;
            }
        }
        if let Some(cb) = limits.info_string_callback.as_ref() {
            let reason = result
                .stop_info
                .as_ref()
                .map(|info| format!("{:?}", info.reason))
                .unwrap_or_else(|| "Unknown".to_string());
            cb(&format!(
                "iterative_complete depth={} elapsed_ms={} reason={}",
                final_depth_reached,
                t0.elapsed().as_millis(),
                reason
            ));
        }
        result
    }
}

impl<E: Evaluator + Send + Sync + 'static> SearcherBackend for ClassicBackend<E> {
    fn start_async(
        self: Arc<Self>,
        root: Position,
        mut limits: SearchLimits,
        info: Option<InfoEventCallback>,
        active_counter: Arc<AtomicUsize>,
    ) -> BackendSearchTask {
        let stop_flag =
            limits.stop_flag.get_or_insert_with(|| Arc::new(AtomicBool::new(false))).clone();
        active_counter.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        let backend = self;
        let info_cb = info;
        let thread_suffix = if limits.session_id != 0 {
            limits.session_id
        } else {
            SEARCH_THREAD_SEQ.fetch_add(1, Ordering::Relaxed)
        };
        let thread_name = format!("classic-backend-search-{thread_suffix}");
        let handle = thread::Builder::new()
            .name(thread_name)
            .spawn({
                let counter = Arc::clone(&active_counter);
                move || {
                    struct Guard(Arc<AtomicUsize>);
                    impl Drop for Guard {
                        fn drop(&mut self) {
                            self.0.fetch_sub(1, Ordering::SeqCst);
                        }
                    }
                    let _guard = Guard(counter);
                    let result = panic::catch_unwind(AssertUnwindSafe(|| {
                        backend.iterative(&root, &limits, info_cb.as_ref())
                    }));
                    match result {
                        Ok(res) => {
                            let _ = tx.send(res);
                        }
                        Err(payload) => {
                            let panic_msg = if let Some(s) = payload.downcast_ref::<&str>() {
                                (*s).to_string()
                            } else if let Some(s) = payload.downcast_ref::<String>() {
                                s.clone()
                            } else {
                                let dyn_type = (*payload).type_id();
                                format!("unknown panic payload (type_id={dyn_type:?})")
                            };
                            warn!("classic backend search thread panicked: {panic_msg}");

                            let elapsed_base = limits.start_time.elapsed().as_millis() as u64;
                            let mut elapsed_ms = elapsed_base;
                            let mut soft_limit_ms = 0;
                            let mut hard_limit_ms = 0;
                            let mut hard_timeout = false;

                            if let Some(tm) = limits.time_manager.as_ref() {
                                let tm_elapsed = tm.elapsed_ms();
                                let tm_soft = tm.soft_limit_ms();
                                let tm_hard = tm.hard_limit_ms();
                                if tm_elapsed > 0 {
                                    elapsed_ms = tm_elapsed;
                                }
                                if tm_soft != u64::MAX {
                                    soft_limit_ms = tm_soft;
                                }
                                if tm_hard != u64::MAX {
                                    hard_limit_ms = tm_hard;
                                }
                            }

                            if hard_limit_ms > 0 {
                                hard_timeout = elapsed_ms >= hard_limit_ms;
                            }

                            if let Some(deadlines) = limits.fallback_deadlines {
                                if soft_limit_ms == 0 {
                                    soft_limit_ms = deadlines.soft_limit_ms;
                                }
                                if hard_limit_ms == 0 {
                                    hard_limit_ms = deadlines.hard_limit_ms;
                                }
                                if hard_limit_ms > 0 {
                                    hard_timeout =
                                        hard_timeout || elapsed_ms >= deadlines.hard_limit_ms;
                                }
                            }

                            if soft_limit_ms == 0 && hard_limit_ms == 0 {
                                if let Some(limit) = limits.time_limit() {
                                    let ms = limit.as_millis() as u64;
                                    soft_limit_ms = ms;
                                    hard_limit_ms = ms;
                                }
                            }

                            let stats = SearchStats {
                                elapsed: Duration::from_millis(elapsed_ms),
                                ..Default::default()
                            };

                            let mut fallback = SearchResult::new(None, 0, stats);
                            fallback.end_reason = TerminationReason::Error;
                            fallback.stop_info = Some(StopInfo {
                                reason: TerminationReason::Error,
                                elapsed_ms,
                                nodes: 0,
                                depth_reached: 0,
                                hard_timeout,
                                soft_limit_ms,
                                hard_limit_ms,
                            });
                            let _ = tx.send(fallback);
                        }
                    }
                }
            })
            .expect("spawn classic backend search thread");
        BackendSearchTask::new(stop_flag, rx, handle)
    }

    fn think_blocking(
        &self,
        root: &Position,
        limits: &SearchLimits,
        info: Option<InfoEventCallback>,
    ) -> SearchResult {
        self.iterative(root, limits, info.as_ref())
    }

    fn update_threads(&self, _n: usize) {}
    fn update_hash(&self, _mb: usize) {
        // Engine側でshared_tt再生成＋Backend再バインド方針のため未使用
    }
}
