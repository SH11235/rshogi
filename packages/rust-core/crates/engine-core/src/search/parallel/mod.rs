pub mod stop_ctrl;
pub use stop_ctrl::{FinalizeReason, FinalizerMsg, StopController, StopSnapshot};

use crate::evaluation::evaluate::Evaluator;
use crate::search::ab::{ClassicBackend, SearchProfile};
use crate::search::api::SearcherBackend;
use crate::search::{SearchLimits, SearchResult, SearchStats, TranspositionTable};
use crate::Position;
use log::warn;
use std::cmp::Ordering;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering};
use std::sync::Arc;
use std::thread;
use std::time::Instant;

pub struct ParallelSearcher<E>
where
    E: Evaluator + Send + Sync + 'static,
{
    backend: Arc<ClassicBackend<E>>,
    tt: Arc<TranspositionTable>,
    stop_controller: Arc<StopController>,
    threads: usize,
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

        Self {
            backend: Arc::new(backend),
            tt,
            stop_controller: stop_ctrl,
            threads: threads.max(1),
        }
    }

    pub fn adjust_thread_count(&mut self, threads: usize) {
        self.threads = threads.max(1);
    }

    pub fn search(&mut self, pos: &mut Position, mut limits: SearchLimits) -> SearchResult {
        let threads = self.threads.max(1);
        limits.stop_controller.get_or_insert_with(|| Arc::clone(&self.stop_controller));
        let stop_flag =
            limits.stop_flag.get_or_insert_with(|| Arc::new(AtomicBool::new(false))).clone();

        let qnodes_counter =
            limits.qnodes_counter.get_or_insert_with(|| Arc::new(AtomicU64::new(0))).clone();
        let start = Instant::now();

        if threads == 1 {
            let mut result =
                self.backend.think_blocking(pos, &limits, limits.info_callback.clone());
            finish_single_result(&self.tt, &mut result, start);
            return result;
        }

        let mut handles = Vec::with_capacity(threads - 1);
        for worker_id in 1..threads {
            let backend = Arc::clone(&self.backend);
            let mut worker_limits = clone_limits_for_worker(&limits);
            worker_limits.info_callback = None;
            worker_limits.info_string_callback = None;
            worker_limits.iteration_callback = None;
            worker_limits.qnodes_counter = Some(Arc::clone(&qnodes_counter));
            let worker_pos = pos.clone();
            handles.push(thread::spawn(move || {
                let result = backend.think_blocking(&worker_pos, &worker_limits, None);
                (worker_id, result)
            }));
        }

        let mut results = Vec::with_capacity(threads);
        let main_result = self.backend.think_blocking(pos, &limits, limits.info_callback.clone());
        results.push((0usize, main_result));

        // Signal helpers to wind down once primary completes.
        stop_flag.store(true, AtomicOrdering::Release);

        for handle in handles {
            match handle.join() {
                Ok(res) => results.push(res),
                Err(_) => warn!("parallel worker panicked; ignoring result"),
            }
        }

        // Reset stop flag so future searches start clean.
        stop_flag.store(false, AtomicOrdering::Release);
        qnodes_counter.store(0, AtomicOrdering::Release);

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
    let total_qnodes: u64 = results.iter().map(|(_, r)| r.stats.qnodes).sum();
    let max_depth = results.iter().map(|(_, r)| r.depth).max().unwrap_or(0);
    let max_seldepth_opt =
        results.iter().filter_map(|(_, r)| r.stats.seldepth.map(|v| v as u32)).max();
    let primary_nodes = results[best_idx].1.nodes;

    let mut final_result = results.swap_remove(best_idx).1;

    final_result.stats.elapsed = elapsed;
    final_result.stats.nodes = total_nodes;
    final_result.stats.qnodes = total_qnodes;
    final_result.stats.depth = max_depth.min(u32::from(u8::MAX)) as u8;
    final_result.depth = max_depth;
    final_result.stats.seldepth = max_seldepth_opt.map(|v| v.min(u32::from(u8::MAX)) as u8);
    final_result.seldepth = max_seldepth_opt.unwrap_or(max_depth);
    if total_nodes > 0 {
        let duplication =
            (total_nodes.saturating_sub(primary_nodes)) as f64 / (total_nodes as f64) * 100.0;
        final_result.stats.duplication_percentage = Some(duplication);
    }
    final_result.hashfull = tt.hashfull() as u32;
    final_result.refresh_summary();

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
        immediate_eval_at_depth_zero: base.immediate_eval_at_depth_zero,
        multipv: base.multipv,
        enable_fail_safe: base.enable_fail_safe,
        fallback_deadlines: base.fallback_deadlines,
        time_manager: base.time_manager.clone(),
        stop_controller: base.stop_controller.clone(),
    }
}
