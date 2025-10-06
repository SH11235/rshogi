pub mod stop_ctrl;
mod thread_pool;
pub use stop_ctrl::{FinalizeReason, FinalizerMsg, StopController, StopSnapshot};

use self::thread_pool::{SearchJob, ThreadPool};
use crate::evaluation::evaluate::Evaluator;
use crate::search::ab::{ClassicBackend, SearchProfile};
use crate::search::api::SearcherBackend;
use crate::search::{SearchLimits, SearchResult, SearchStats, TranspositionTable};
use crate::Position;
use log::{debug, warn};
use std::cmp::Ordering;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Instant;

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
        let backend = ClassicBackend::with_profile_and_tt(
            Arc::clone(&evaluator),
            Arc::clone(&tt),
            profile.clone(),
        );
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
        let stop_flag = if let Some(flag) = &limits.stop_flag {
            Arc::clone(flag)
        } else {
            let flag = Arc::new(AtomicBool::new(false));
            limits.stop_flag = Some(Arc::clone(&flag));
            flag
        };

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
        limits.root_jitter_seed = None;
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

        let mut jobs = Vec::with_capacity(helper_count);
        for worker_id in 1..=helper_count {
            let mut worker_limits = clone_limits_for_worker(&limits);
            worker_limits.info_callback = None;
            worker_limits.info_string_callback = None;
            worker_limits.iteration_callback = None;
            worker_limits.qnodes_counter = Some(Arc::clone(&qnodes_counter));
            worker_limits.stop_controller = None;
            worker_limits.root_jitter_seed =
                Some(compute_jitter_seed(session_id, worker_id, root_key));
            jobs.push(SearchJob {
                position: pos.clone(),
                limits: worker_limits,
            });
        }

        let (result_tx, result_rx) = mpsc::channel();
        self.thread_pool.dispatch(jobs, &result_tx);

        let mut results = Vec::with_capacity(threads);
        let main_result = self.backend.think_blocking(pos, &limits, limits.info_callback.clone());
        results.push((0usize, main_result));

        let original_stop = stop_flag.load(AtomicOrdering::Acquire);
        stop_flag.store(true, AtomicOrdering::Release);

        drop(result_tx);
        for _ in 0..helper_count {
            match result_rx.recv() {
                Ok(res) => results.push(res),
                Err(_) => warn!("parallel worker failed to send result"),
            }
        }

        stop_flag.store(original_stop, AtomicOrdering::Release);
        if inserted_qnodes {
            qnodes_counter.store(0, AtomicOrdering::Release);
        }

        combine_results(&self.tt, results, start)
    }
}

fn finish_single_result(tt: &TranspositionTable, result: &mut SearchResult, start: Instant) {
    result.stats.elapsed = start.elapsed();
    result.hashfull = tt.hashfull() as u32;
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
        fallback.hashfull = tt.hashfull() as u32;
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
    let total_qnodes: u64 = results.iter().map(|(_, r)| r.stats.qnodes).max().unwrap_or(0);
    let max_depth = results.iter().map(|(_, r)| r.depth).max().unwrap_or(0);
    let max_seldepth = results.iter().map(|(_, r)| r.seldepth).max().unwrap_or(max_depth);
    let primary_nodes = results
        .iter()
        .find(|(id, _)| *id == 0)
        .map(|(_, r)| r.nodes)
        .unwrap_or(results[best_idx].1.nodes);

    let mut final_result = results.swap_remove(best_idx).1;

    final_result.stats.elapsed = elapsed;
    final_result.stats.nodes = total_nodes;
    final_result.stats.qnodes = total_qnodes;
    final_result.stats.depth = max_depth.min(u32::from(u8::MAX)) as u8;
    final_result.depth = max_depth;
    final_result.seldepth = max_seldepth;
    final_result.stats.seldepth = Some(final_result.seldepth.min(u32::from(u8::MAX)) as u8);
    if total_nodes > 0 {
        let duplication =
            (total_nodes.saturating_sub(primary_nodes)) as f64 / (total_nodes as f64) * 100.0;
        final_result.stats.duplication_percentage = Some(duplication);
    }
    if let Some(info) = final_result.stop_info.as_mut() {
        info.nodes = total_nodes;
        info.elapsed_ms = elapsed.as_millis() as u64;
        info.depth_reached = max_depth.min(u32::from(u8::MAX)) as u8;
    }
    final_result.hashfull = tt.hashfull() as u32;
    final_result.refresh_summary();

    let mut merged_heuristics = final_result.stats.heuristics.as_ref().map(|arc| (**arc).clone());
    for (_, res) in &results {
        if let Some(h) = res.stats.heuristics.as_ref() {
            let snapshot = (**h).clone();
            if let Some(acc) = merged_heuristics.as_mut() {
                acc.merge_from(&snapshot);
            } else {
                merged_heuristics = Some(snapshot);
            }
        }
    }
    if let Some(merged) = merged_heuristics {
        final_result.stats.heuristics = Some(Arc::new(merged));
    }

    if let Some(dup) = final_result.stats.duplication_percentage {
        if dup > 75.0 {
            debug!("lazy_smp duplication {:.2}%", dup);
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

    candidate.0 < current.0
}

fn clone_limits_for_worker(base: &SearchLimits) -> SearchLimits {
    SearchLimits {
        time_control: base.time_control.clone(),
        moves_to_go: base.moves_to_go,
        depth: base.depth,
        nodes: base.nodes,
        qnodes_limit: base.qnodes_limit,
        time_parameters: base.time_parameters.clone(),
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
        immediate_eval_at_depth_zero: base.immediate_eval_at_depth_zero,
        multipv: base.multipv,
        enable_fail_safe: base.enable_fail_safe,
        fallback_deadlines: base.fallback_deadlines,
        time_manager: base.time_manager.clone(),
        stop_controller: base.stop_controller.clone(),
    }
}

fn compute_jitter_seed(session_id: u64, worker_id: usize, root_key: u64) -> u64 {
    let wid = worker_id as u64;
    session_id.wrapping_mul(0x9E37_79B1_85EB_CA87)
        ^ root_key.rotate_left((wid as u32) & 31)
        ^ (wid << 32)
        ^ wid
}
