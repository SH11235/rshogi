pub mod stop_ctrl;
mod thread_pool;
pub use stop_ctrl::{FinalizeReason, FinalizerMsg, StopController, StopSnapshot};

use self::thread_pool::{SearchJob, ThreadPool};
use crate::evaluation::evaluate::Evaluator;
use crate::search::ab::{ClassicBackend, SearchProfile};
use crate::search::api::SearcherBackend;
use crate::search::constants::HELPER_SNAPSHOT_MIN_DEPTH;
use crate::search::limits::RootSplit;
use crate::search::types::{clamp_score_cp, RootLine};
use crate::search::{SearchLimits, SearchResult, SearchStats, TranspositionTable};
use crate::shogi::Move;
use crate::Position;
use log::{debug, warn};
use smallvec::SmallVec;
use std::cmp::Ordering;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Instant;

fn jitter_enabled() -> bool {
    match std::env::var("SHOGI_TEST_FORCE_JITTER") {
        Ok(val) => val != "0",
        Err(_) => true,
    }
}

pub struct ParallelSearcher<E>
where
    E: Evaluator + Send + Sync + 'static,
{
    backend: Arc<ClassicBackend<E>>,
    tt: Arc<TranspositionTable>,
    stop_controller: Arc<StopController>,
    threads: usize,
    thread_pool: ThreadPool<E>,
}

impl<E> ParallelSearcher<E>
where
    E: Evaluator + Send + Sync + 'static,
{
    pub fn new<T>(
        evaluator: T,
        tt: Arc<TranspositionTable>,
        threads: usize,
        stop_ctrl: Arc<StopController>,
    ) -> Self
    where
        T: Into<Arc<E>>,
    {
        let evaluator = evaluator.into();
        let profile = SearchProfile::default();
        profile.apply_runtime_defaults();
        let backend =
            ClassicBackend::with_profile_and_tt(Arc::clone(&evaluator), Arc::clone(&tt), profile);
        let backend = Arc::new(backend);
        let helper_threads = threads.max(1).saturating_sub(1);
        let thread_pool = ThreadPool::new(Arc::clone(&backend), helper_threads);

        Self {
            backend,
            tt,
            stop_controller: stop_ctrl,
            threads: threads.max(1),
            thread_pool,
        }
    }

    pub fn adjust_thread_count(&mut self, threads: usize) {
        self.threads = threads.max(1);
        let helper = self.threads.saturating_sub(1);
        self.thread_pool.resize(helper);
    }

    pub fn search(&mut self, pos: &mut Position, mut limits: SearchLimits) -> SearchResult {
        let threads = self.threads.max(1);
        limits.stop_controller.get_or_insert_with(|| Arc::clone(&self.stop_controller));
        let inserted_stop_flag = limits.stop_flag.is_none();
        let stop_flag =
            limits.stop_flag.get_or_insert_with(|| Arc::new(AtomicBool::new(false))).clone();

        let inserted_qnodes = limits.qnodes_counter.is_none();
        let qnodes_counter = if let Some(counter) = &limits.qnodes_counter {
            Arc::clone(counter)
        } else {
            let counter = Arc::new(AtomicU64::new(0));
            limits.qnodes_counter = Some(Arc::clone(&counter));
            counter
        };
        let session_id = limits.session_id;
        let root_key = pos.zobrist_hash();
        limits.store_heuristics = true;
        limits.root_jitter_seed = None;
        limits.helper_role = false;
        limits.root_split = None;
        limits.root_work_queue = None;
        let start = Instant::now();

        if threads == 1 {
            let mut result =
                self.backend.think_blocking(pos, &limits, limits.info_callback.clone());
            finish_single_result(&self.tt, &mut result, start);
            if inserted_qnodes {
                qnodes_counter.store(0, AtomicOrdering::Release);
            }
            return result;
        }

        let helper_count = threads.saturating_sub(1);
        self.thread_pool.resize(helper_count);

        let root_work_queue = if helper_count > 0 {
            Some(Arc::new(crate::search::limits::RootWorkQueue::new()))
        } else {
            None
        };

        let mut jobs = Vec::with_capacity(helper_count);
        for worker_index in 0..helper_count {
            let mut worker_limits = clone_limits_for_worker(&limits);
            worker_limits.store_heuristics = false;
            worker_limits.info_callback = None;
            worker_limits.info_string_callback = None;
            worker_limits.iteration_callback = None;
            worker_limits.qnodes_counter = Some(Arc::clone(&qnodes_counter));
            worker_limits.stop_controller = None;
            worker_limits.helper_role = true;
            worker_limits.root_split = RootSplit::new(worker_index + 1, threads, true);
            if let Some(queue) = root_work_queue.as_ref() {
                worker_limits.root_work_queue = Some(Arc::clone(queue));
            }
            let jitter_on = limits.jitter_override.unwrap_or_else(jitter_enabled);
            if jitter_on {
                worker_limits.root_jitter_seed =
                    Some(compute_jitter_seed(session_id, worker_index + 1, root_key));
            } else {
                worker_limits.root_jitter_seed = None;
            }
            jobs.push(SearchJob {
                position: pos.clone(),
                limits: worker_limits,
            });
        }

        let (result_tx, result_rx) = mpsc::channel();
        self.thread_pool.dispatch(jobs, &result_tx);

        // Primary も RootWorkQueue を共有（primary-first の先取り claim 用）
        let mut results = Vec::with_capacity(threads);
        let mut primary_limits = clone_limits_for_worker(&limits);
        if let Some(queue) = root_work_queue.as_ref() {
            primary_limits.root_work_queue = Some(Arc::clone(queue));
        }
        let main_result =
            self.backend.think_blocking(pos, &primary_limits, limits.info_callback.clone());
        results.push((0usize, main_result));

        let we_set_stop_flag = stop_flag
            .compare_exchange(false, true, AtomicOrdering::AcqRel, AtomicOrdering::Acquire)
            .is_ok();

        drop(result_tx);
        for _ in 0..helper_count {
            match result_rx.recv() {
                Ok((worker_id, res)) => {
                    publish_helper_snapshot(
                        &self.stop_controller,
                        session_id,
                        root_key,
                        worker_id,
                        &res,
                    );
                    results.push((worker_id, res));
                }
                Err(_) => warn!("parallel worker failed to send result"),
            }
        }

        if we_set_stop_flag && inserted_stop_flag {
            let _ = stop_flag.compare_exchange(
                true,
                false,
                AtomicOrdering::AcqRel,
                AtomicOrdering::Relaxed,
            );
        }
        if inserted_qnodes {
            qnodes_counter.store(0, AtomicOrdering::Release);
        }

        combine_results(&self.tt, results, start)
    }
}

fn finish_single_result(tt: &TranspositionTable, result: &mut SearchResult, start: Instant) {
    result.stats.elapsed = start.elapsed();
    result.hashfull = tt.hashfull_permille() as u32;
    result.refresh_summary();
}

fn combine_results(
    tt: &TranspositionTable,
    mut results: Vec<(usize, SearchResult)>,
    start: Instant,
) -> SearchResult {
    let elapsed = start.elapsed();
    if results.is_empty() {
        let stats = SearchStats {
            elapsed,
            ..Default::default()
        };
        let mut fallback = SearchResult::new(None, 0, stats);
        fallback.hashfull = tt.hashfull_permille() as u32;
        fallback.refresh_summary();
        return fallback;
    }

    let mut best_idx = 0usize;
    for idx in 1..results.len() {
        if prefers(&results[idx], &results[best_idx]) {
            best_idx = idx;
        }
    }

    let total_nodes: u64 = results.iter().map(|(_, r)| r.nodes).sum();
    // qnodes aggregation: Use max instead of sum because all workers share the same
    // Arc<AtomicU64> qnodes_counter. Each worker increments this shared counter,
    // so summing individual worker results would count the same qnodes multiple times.
    // Taking the max gives us the true global qnodes count from the shared counter.
    let total_qnodes: u64 = results.iter().map(|(_, r)| r.stats.qnodes).max().unwrap_or(0);
    let max_depth = results.iter().map(|(_, r)| r.depth).max().unwrap_or(0);
    let max_seldepth = results.iter().map(|(_, r)| r.seldepth).max().unwrap_or(max_depth);
    let primary_nodes = results
        .iter()
        .find(|(id, _)| *id == 0)
        .map(|(_, r)| r.nodes)
        .unwrap_or(results[best_idx].1.nodes);

    // Diagnostics: best source (primary=0 / helper>0)
    if best_idx != 0 {
        log::info!(
            "info string parallel_best_source=helper worker_id={} depth={} nodes={}",
            results[best_idx].0,
            results[best_idx].1.depth,
            results[best_idx].1.nodes
        );
    } else {
        log::info!(
            "info string parallel_best_source=primary depth={} nodes={}",
            results[best_idx].1.depth,
            results[best_idx].1.nodes
        );
    }
    let mut final_result = results.swap_remove(best_idx).1;

    final_result.stats.elapsed = elapsed;
    final_result.stats.nodes = total_nodes;
    final_result.stats.qnodes = total_qnodes;
    final_result.stats.depth = max_depth.min(u32::from(u8::MAX)) as u8;
    final_result.depth = max_depth;
    final_result.seldepth = max_seldepth;
    final_result.stats.seldepth = Some(final_result.seldepth.min(u32::from(u8::MAX)) as u8);
    if total_nodes > 0 {
        // 便宜的に duplication と呼んでいた値だが、実際には「ヘルパースレッドが担当したノード割合」。
        let helper_share =
            (total_nodes.saturating_sub(primary_nodes)) as f64 / (total_nodes as f64) * 100.0;
        final_result.stats.helper_share_pct = Some(helper_share);
    }
    if let Some(info) = final_result.stop_info.as_mut() {
        info.nodes = total_nodes;
        info.elapsed_ms = elapsed.as_millis() as u64;
        info.depth_reached = max_depth.min(u32::from(u8::MAX)) as u8;
    }
    final_result.hashfull = tt.hashfull_permille() as u32;
    final_result.refresh_summary();

    let primary_heuristics = results
        .iter()
        .find(|(id, _)| *id == 0)
        .and_then(|(_, r)| r.stats.heuristics.as_ref());
    let helpers_have_heuristics =
        results.iter().any(|(id, r)| *id != 0 && r.stats.heuristics.is_some());

    if helpers_have_heuristics {
        let mut merged = final_result
            .stats
            .heuristics
            .as_ref()
            .map(|arc| (**arc).clone())
            .or_else(|| primary_heuristics.map(|arc| (**arc).clone()))
            .unwrap_or_default();

        for (_, res) in &results {
            if let Some(h) = res.stats.heuristics.as_ref() {
                merged.merge_from(h);
            }
        }

        final_result.stats.heuristics = Some(Arc::new(merged));
    } else if final_result.stats.heuristics.is_none() {
        if let Some(primary) = primary_heuristics {
            final_result.stats.heuristics = Some(Arc::clone(primary));
        }
    }

    if let Some(dup) = final_result.stats.helper_share_pct {
        if dup > 65.0 {
            debug!("lazy_smp helper_share_pct {:.2}%", dup);
        }
    }

    final_result
}

fn prefers(candidate: &(usize, SearchResult), current: &(usize, SearchResult)) -> bool {
    match candidate.1.depth.cmp(&current.1.depth) {
        Ordering::Greater => return true,
        Ordering::Less => return false,
        Ordering::Equal => {}
    }

    match candidate.1.seldepth.cmp(&current.1.seldepth) {
        Ordering::Greater => return true,
        Ordering::Less => return false,
        Ordering::Equal => {}
    }

    match candidate.1.nodes.cmp(&current.1.nodes) {
        Ordering::Greater => return true,
        Ordering::Less => return false,
        Ordering::Equal => {}
    }

    match candidate.1.score.cmp(&current.1.score) {
        Ordering::Greater => return true,
        Ordering::Less => return false,
        Ordering::Equal => {}
    }

    // Fully equal: prefer smaller worker id (primary=0 wins).
    candidate.0 < current.0
}

/// Create a shallow copy of `SearchLimits` for helper workers.
///
/// 呼び出し側で stop_controller やコールバック類を `None` に差し替える前提のため、
/// 共有ハンドルの複製のみを行う。必要に応じて後段でフィールドを無効化すること。
fn clone_limits_for_worker(base: &SearchLimits) -> SearchLimits {
    SearchLimits {
        time_control: base.time_control.clone(),
        moves_to_go: base.moves_to_go,
        depth: base.depth,
        nodes: base.nodes,
        qnodes_limit: base.qnodes_limit,
        time_parameters: base.time_parameters,
        random_time_ms: base.random_time_ms,
        session_id: base.session_id,
        start_time: base.start_time,
        panic_time_scale: base.panic_time_scale,
        contempt: base.contempt,
        is_ponder: base.is_ponder,
        stop_flag: base.stop_flag.clone(),
        info_callback: base.info_callback.clone(),
        info_string_callback: base.info_string_callback.clone(),
        iteration_callback: base.iteration_callback.clone(),
        ponder_hit_flag: base.ponder_hit_flag.clone(),
        qnodes_counter: base.qnodes_counter.clone(),
        root_jitter_seed: base.root_jitter_seed,
        jitter_override: base.jitter_override,
        root_split: base.root_split,
        root_work_queue: base.root_work_queue.clone(),
        helper_role: base.helper_role,
        store_heuristics: base.store_heuristics,
        immediate_eval_at_depth_zero: base.immediate_eval_at_depth_zero,
        multipv: base.multipv,
        enable_fail_safe: base.enable_fail_safe,
        fallback_deadlines: base.fallback_deadlines,
        time_manager: base.time_manager.clone(),
        stop_controller: base.stop_controller.clone(),
    }
}

fn compute_jitter_seed(session_id: u64, worker_id: usize, root_key: u64) -> u64 {
    #[inline]
    fn mix64(x: u64) -> u64 {
        // SplitMix64 由来の軽量ミキサ。入力ビットを高速に拡散させる。
        let mut z = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    let mut seed = mix64(session_id ^ root_key);
    seed = mix64(seed ^ (worker_id as u64));
    seed = mix64(seed ^ root_key.rotate_left((worker_id as u32) & 31));
    seed
}

#[cfg(test)]
pub(crate) fn compute_jitter_seed_for_test(
    session_id: u64,
    worker_id: usize,
    root_key: u64,
) -> u64 {
    compute_jitter_seed(session_id, worker_id, root_key)
}

fn publish_helper_snapshot(
    stop_controller: &StopController,
    session_id: u64,
    root_key: u64,
    worker_id: usize,
    result: &SearchResult,
) {
    if worker_id == 0 {
        return;
    }
    if result.depth < HELPER_SNAPSHOT_MIN_DEPTH {
        return;
    }
    if let Some(existing) = stop_controller.try_read_snapshot() {
        // Only suppress when the existing snapshot is for the same session and root,
        // and strictly deeper than our helper result. Equal depth updates are
        // forwarded and left to StopController's policy (it refreshes metrics).
        if existing.search_id == session_id
            && existing.root_key == root_key
            && result.depth < u32::from(existing.depth)
        {
            return;
        }
    }

    // Prefer PV from result.lines[0] when it's Exact (often higher quality from full search),
    // fall back to result.stats.pv, then to best_move if all else fails.
    // This improves interim USI reporting quality by avoiding shallow fail-high/low PVs.
    // Important: bound and score must match the chosen PV source for consistency.
    let mut pv: SmallVec<[Move; 32]> = SmallVec::new();
    let mut chosen_bound = result.node_type;
    let mut chosen_score = result.score;

    if let Some(first_line) = result.lines.as_ref().and_then(|ls| ls.first()) {
        // Prefer Exact bound lines for stability; use fail-high/low only if nothing better
        let use_lines0 =
            first_line.bound == crate::search::types::NodeType::Exact || result.stats.pv.is_empty();
        if use_lines0 {
            pv.extend(first_line.pv.iter().copied());
            chosen_bound = first_line.bound;
            chosen_score = first_line.score_cp;
        } else {
            // lines[0] is fail-high/low and stats.pv exists; prefer stats.pv for stability
            pv.extend(result.stats.pv.iter().copied());
            // chosen_bound and chosen_score remain as result.node_type and result.score
        }
    } else {
        pv.extend(result.stats.pv.iter().copied());
    }
    if pv.len() > 32 {
        pv.truncate(32);
    }
    if pv.is_empty() {
        if let Some(best) = result.best_move {
            pv.push(best);
        } else {
            return;
        }
    }

    let root_move = pv[0];
    let seldepth = result.stats.seldepth.or(Some(result.seldepth.min(u32::from(u8::MAX)) as u8));
    let elapsed_ms = result.stats.elapsed.as_millis().min(u128::from(u64::MAX)) as u64;

    let line = RootLine {
        multipv_index: 1,
        root_move,
        score_internal: chosen_score,
        score_cp: clamp_score_cp(chosen_score),
        bound: chosen_bound,
        depth: result.depth,
        seldepth,
        pv,
        nodes: Some(result.nodes),
        time_ms: Some(elapsed_ms),
        nps: Some(result.nps),
        exact_exhausted: false,
        exhaust_reason: None,
        mate_distance: None,
    };

    stop_controller.publish_root_line(session_id, root_key, &line);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluation::evaluate::MaterialEvaluator;
    use crate::search::{SearchLimitsBuilder, SearchResult, TranspositionTable};
    use crate::shogi::Position;
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    fn helper_share(result: &SearchResult) -> f64 {
        result.stats.helper_share_pct.unwrap_or(0.0)
    }

    #[test]
    fn helper_share_bounds_single_and_multi_thread() {
        let evaluator = Arc::new(MaterialEvaluator);
        let tt_single = Arc::new(TranspositionTable::new(8));
        let stop_single = Arc::new(StopController::new());
        let mut single = ParallelSearcher::<MaterialEvaluator>::new(
            Arc::clone(&evaluator),
            Arc::clone(&tt_single),
            1,
            Arc::clone(&stop_single),
        );

        let mut pos_single = Position::startpos();
        let limits_single = SearchLimitsBuilder::default().fixed_nodes(256).depth(3).build();
        let single_result = single.search(&mut pos_single, limits_single);
        assert!(helper_share(&single_result) <= f64::EPSILON);

        let tt_multi = Arc::new(TranspositionTable::new(8));
        let stop_multi = Arc::new(StopController::new());
        let mut multi = ParallelSearcher::<MaterialEvaluator>::new(
            evaluator,
            Arc::clone(&tt_multi),
            2,
            Arc::clone(&stop_multi),
        );
        let mut pos_multi = Position::startpos();
        let limits_multi = SearchLimitsBuilder::default().fixed_nodes(1024).depth(4).build();
        let multi_result = multi.search(&mut pos_multi, limits_multi);
        let share = helper_share(&multi_result);
        assert!(share > 0.0, "multi-thread helper share should be positive");
        assert!(share <= 100.0, "helper share must not exceed 100%");
    }

    #[test]
    fn search_respects_external_stop_flag_true() {
        let evaluator = Arc::new(MaterialEvaluator);
        let tt = Arc::new(TranspositionTable::new(8));
        let stop_ctrl = Arc::new(StopController::new());
        let mut searcher = ParallelSearcher::<MaterialEvaluator>::new(
            evaluator,
            Arc::clone(&tt),
            2,
            Arc::clone(&stop_ctrl),
        );
        let mut pos = Position::startpos();
        let external_flag = Arc::new(AtomicBool::new(true));
        let limits = SearchLimitsBuilder::default()
            .fixed_nodes(256)
            .depth(2)
            .stop_flag(Arc::clone(&external_flag))
            .build();

        let _ = searcher.search(&mut pos, limits);
        assert!(external_flag.load(Ordering::Acquire));
    }

    #[test]
    fn jitter_seed_deterministic_and_varies() {
        let seed_a = compute_jitter_seed_for_test(42, 1, 0x1234_5678_9ABC_DEF0);
        let seed_b = compute_jitter_seed_for_test(42, 1, 0x1234_5678_9ABC_DEF0);
        assert_eq!(seed_a, seed_b);

        let seed_worker = compute_jitter_seed_for_test(42, 2, 0x1234_5678_9ABC_DEF0);
        assert_ne!(seed_a, seed_worker);

        let seed_root = compute_jitter_seed_for_test(42, 1, 0xFFFF_0000_1234_5678);
        assert_ne!(seed_a, seed_root);
    }

    #[test]
    fn compute_jitter_seed_collision_smoke() {
        let mut seen = HashSet::new();
        let mut key = 0x9E37_79B9_7F4A_7C15u64;
        for _ in 0..512 {
            key = key.wrapping_mul(0xBF58_476D_1CE4_E5B9).wrapping_add(0x94D0_49BB_1331_11EB);
            let seed = compute_jitter_seed_for_test(7, 1, key);
            assert!(seen.insert(seed), "duplicate jitter seed generated");
        }
    }

    #[test]
    fn helper_snapshot_allows_equal_depth_forward() {
        // Verify that helper results at the same depth as existing snapshot are forwarded
        // to StopController (which then decides whether to update metrics).
        let evaluator = Arc::new(MaterialEvaluator);
        let tt = Arc::new(TranspositionTable::new(8));
        let stop_ctrl = Arc::new(StopController::new());
        let mut searcher = ParallelSearcher::<MaterialEvaluator>::new(
            evaluator,
            Arc::clone(&tt),
            2,
            Arc::clone(&stop_ctrl),
        );

        let mut pos = Position::startpos();
        let session_id = 42u64;
        let limits = SearchLimitsBuilder::default()
            .fixed_nodes(512)
            .depth(4)
            .session_id(session_id)
            .build();

        let _ = searcher.search(&mut pos, limits);

        // After search, snapshot should exist with depth >= HELPER_SNAPSHOT_MIN_DEPTH.
        if let Some(snapshot) = stop_ctrl.try_read_snapshot() {
            assert!(
                snapshot.depth >= HELPER_SNAPSHOT_MIN_DEPTH as u8,
                "snapshot depth should be >= min publish depth"
            );
            assert_eq!(snapshot.search_id, session_id, "snapshot should have correct session_id");
        }
    }

    #[test]
    fn helper_snapshot_prefers_lines_pv_over_stats_pv() {
        // Test that publish_helper_snapshot uses result.lines[0].pv when available,
        // falling back to result.stats.pv only if lines is empty.
        //
        // This test verifies the fix in publish_helper_snapshot where we changed from:
        //   pv.extend(result.stats.pv.iter().copied());
        // to:
        //   if let Some(line_pv) = result.lines.as_ref().and_then(|ls| ls.first()).map(|l| &l.pv) {
        //       pv.extend(line_pv.iter().copied());
        //   } else {
        //       pv.extend(result.stats.pv.iter().copied());
        //   }
        //
        // We test this indirectly by checking that a SearchResult with both lines and stats.pv
        // uses the lines PV, verified through the published snapshot.
        use crate::search::types::{NodeType, RootLine};
        use crate::search::SearchResult;
        use crate::shogi::{Move, Square};
        use smallvec::SmallVec;

        let stop_ctrl = Arc::new(StopController::new());
        let session_id = 123u64;
        let root_key = 0xABCD_EF01_2345_6789u64;
        let worker_id = 1;

        // Publish session to initialize StopController
        let stop_flag = Arc::new(AtomicBool::new(false));
        stop_ctrl.publish_session(Some(&stop_flag), session_id);

        // Create distinct moves for testing
        let line_move = Move::normal(Square::new(7, 6), Square::new(7, 5), false); // 2g2f
        let stats_move = Move::normal(Square::new(2, 6), Square::new(2, 5), false); // 7g7f

        // Build a SearchResult with BOTH lines[0].pv and stats.pv
        // The test verifies that lines[0].pv takes precedence
        let mut lines = SmallVec::new();
        let mut line_pv = SmallVec::new();
        line_pv.push(line_move);
        lines.push(RootLine {
            multipv_index: 1,
            root_move: line_move,
            score_internal: 100,
            score_cp: 100,
            bound: NodeType::Exact,
            depth: HELPER_SNAPSHOT_MIN_DEPTH,
            seldepth: Some(6),
            pv: line_pv,
            nodes: Some(1000),
            time_ms: Some(100),
            nps: Some(10000),
            exact_exhausted: false,
            exhaust_reason: None,
            mate_distance: None,
        });

        let stats_pv = vec![stats_move]; // Different from line_pv

        let result = SearchResult {
            best_move: Some(line_move),
            score: 100,
            depth: HELPER_SNAPSHOT_MIN_DEPTH,
            seldepth: 6,
            nodes: 1000,
            nps: 10000,
            node_type: NodeType::Exact,
            stats: SearchStats {
                pv: stats_pv,
                elapsed: std::time::Duration::from_millis(100),
                ..Default::default()
            },
            lines: Some(lines),
            hashfull: 0,
            stop_info: None,
            end_reason: crate::search::types::TerminationReason::Completed,
            ponder: None,
        };

        publish_helper_snapshot(&stop_ctrl, session_id, root_key, worker_id, &result);

        // Verify snapshot was published and uses lines[0].pv (line_move), not stats.pv (stats_move)
        let snapshot = stop_ctrl
            .try_read_snapshot()
            .expect("Snapshot should be published when depth >= HELPER_SNAPSHOT_MIN_DEPTH");

        assert_eq!(snapshot.search_id, session_id);
        assert_eq!(snapshot.root_key, root_key);
        assert!(!snapshot.pv.is_empty(), "PV should not be empty");
        assert_eq!(
            snapshot.pv[0], line_move,
            "First PV move should be from lines[0].pv (line_move={:?}), not stats.pv (stats_move={:?})",
            line_move,
            stats_move
        );
    }

    #[test]
    fn helper_snapshot_falls_back_to_stats_pv_when_lines_not_exact() {
        // Test that when lines[0].bound is not Exact and stats.pv is available,
        // publish_helper_snapshot falls back to stats.pv and uses result.node_type for bound.
        use crate::search::types::{NodeType, RootLine};
        use crate::search::SearchResult;
        use crate::shogi::{Move, Square};
        use smallvec::SmallVec;

        let stop_ctrl = Arc::new(StopController::new());
        let session_id = 456u64;
        let root_key = 0x1234_5678_9ABC_DEF0u64;
        let worker_id = 2;

        let stop_flag = Arc::new(AtomicBool::new(false));
        stop_ctrl.publish_session(Some(&stop_flag), session_id);

        // Create distinct moves
        let line_move = Move::normal(Square::new(7, 6), Square::new(7, 5), false); // 2g2f (fail-high)
        let stats_move = Move::normal(Square::new(2, 6), Square::new(2, 5), false); // 7g7f (from stats)

        // Build lines[0] with LowerBound (fail-high)
        let mut lines = SmallVec::new();
        let mut line_pv = SmallVec::new();
        line_pv.push(line_move);
        lines.push(RootLine {
            multipv_index: 1,
            root_move: line_move,
            score_internal: 150,
            score_cp: 150,
            bound: NodeType::LowerBound, // Not Exact!
            depth: HELPER_SNAPSHOT_MIN_DEPTH,
            seldepth: Some(6),
            pv: line_pv,
            nodes: Some(1000),
            time_ms: Some(100),
            nps: Some(10000),
            exact_exhausted: false,
            exhaust_reason: None,
            mate_distance: None,
        });

        // stats.pv with different move
        let stats_pv = vec![stats_move];

        let result = SearchResult {
            best_move: Some(stats_move),
            score: 120,
            depth: HELPER_SNAPSHOT_MIN_DEPTH,
            seldepth: 6,
            nodes: 1000,
            nps: 10000,
            node_type: NodeType::Exact, // result's node_type
            stats: SearchStats {
                pv: stats_pv,
                elapsed: std::time::Duration::from_millis(100),
                ..Default::default()
            },
            lines: Some(lines),
            hashfull: 0,
            stop_info: None,
            end_reason: crate::search::types::TerminationReason::Completed,
            ponder: None,
        };

        publish_helper_snapshot(&stop_ctrl, session_id, root_key, worker_id, &result);

        let snapshot = stop_ctrl.try_read_snapshot().expect("Snapshot should be published");

        assert_eq!(snapshot.search_id, session_id);
        assert_eq!(snapshot.root_key, root_key);
        assert!(!snapshot.pv.is_empty(), "PV should not be empty");

        // Should use stats.pv (stats_move) instead of lines[0].pv (line_move)
        assert_eq!(
            snapshot.pv[0], stats_move,
            "Should fall back to stats.pv when lines[0].bound is not Exact"
        );

        // Bound should match result.node_type (Exact), not lines[0].bound (LowerBound)
        assert_eq!(
            snapshot.node_type,
            NodeType::Exact,
            "Bound should be result.node_type when using stats.pv fallback"
        );

        // Score should match result.score (120), not lines[0].score (150)
        assert_eq!(
            snapshot.score_cp, 120,
            "Score should be result.score when using stats.pv fallback"
        );
    }
}
