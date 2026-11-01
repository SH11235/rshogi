use super::journal::{Reading, simulation_readings};
use super::model::{GameLine, Tree, TreeParams};
use super::service::{Demand, reading_queue};
use super::targets::{CHECKPOINT, Checkpoint, QUEUE, STATUS_FILE, StatusFile};
use anyhow::{Context, Result, ensure};
use rand::SeedableRng;
use rand::seq::SliceRandom;
use rand_chacha::ChaCha8Rng;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(super) struct Sample {
    pub params: TreeParams,
    pub lines: Vec<GameLine>,
    pub answers: Arc<BTreeMap<String, Reading>>,
    pub requested: BTreeSet<String>,
    pub fingerprint: String,
    pub priority: u32,
    pub order: u64,
}

impl Sample {
    fn tree(&self) -> Result<Tree> {
        Tree::build(self.params.clone(), &self.lines)
    }
}

#[derive(Clone, Copy)]
pub(super) enum Timing {
    Maximum,
    Mean,
}

pub(super) struct Simulation {
    pub elapsed: f64,
    pub lower: f64,
    pub count: usize,
    pub utilization: [f64; 20],
    pub mixed: usize,
    pub trees: Vec<Tree>,
}

pub(super) fn simulate(samples: &[&Sample], workers: usize, timing: Timing) -> Result<Simulation> {
    ensure!(workers > 0, "worker 数は正数が必要です");
    let mut durations = BTreeMap::<String, f64>::new();
    for sample in samples {
        for key in &sample.requested {
            let reading =
                sample.answers.get(key).with_context(|| format!("読みが足りない: {key}"))?;
            ensure!(reading.sec.is_finite() && reading.sec >= 0.0, "読み時間が不正: {key}");
            durations
                .entry(key.clone())
                .and_modify(|sec| *sec = sec.max(reading.sec))
                .or_insert(reading.sec);
        }
    }
    let mean = if durations.is_empty() {
        0.0
    } else {
        durations.values().sum::<f64>() / durations.len() as f64
    };
    if matches!(timing, Timing::Mean) {
        for sec in durations.values_mut() {
            *sec = mean;
        }
    }
    let mut trees = samples.iter().map(|s| s.tree()).collect::<Result<Vec<_>>>()?;
    let mut delivered = vec![BTreeMap::new(); samples.len()];
    let mut completed = BTreeSet::new();
    let mut running = BTreeMap::<String, f64>::new();
    let mut requested = BTreeMap::<String, BTreeSet<&str>>::new();
    let mut elapsed = 0.0;
    let mut total = 0.0;
    let mut intervals = Vec::new();
    loop {
        let mut demands = Vec::new();
        for ((tree, sample), cache) in trees.iter_mut().zip(samples).zip(&mut delivered) {
            if tree.finished {
                continue;
            }
            let missing = tree.advance(cache)?;
            for (key, _) in &missing {
                ensure!(
                    sample.answers.contains_key(key),
                    "木 {} の読みが足りない: {key}",
                    sample.order
                );
            }
            if !tree.finished {
                demands.push(Demand {
                    priority: sample.priority,
                    order: sample.order,
                    missing,
                    ply: &tree.ply,
                });
            }
        }
        let busy = running.keys().cloned().collect();
        let queue = reading_queue(demands, &busy);
        for (key, _) in queue.into_iter().take(workers - running.len()) {
            ensure!(!completed.contains(&key), "完了済みの局面を再配布: {key}");
            let duration = durations[&key];
            total += duration;
            running.insert(key, elapsed + duration);
        }
        if trees.iter().all(|t| t.finished) {
            ensure!(running.is_empty(), "木の完了後に読みが残っています");
            break;
        }
        let next = running
            .values()
            .copied()
            .min_by(f64::total_cmp)
            .context("未完了の木があるのに配布できません")?;
        intervals.push((elapsed, next, running.len()));
        elapsed = next;
        let done: Vec<_> = running
            .iter()
            .filter(|(_, end)| **end == next)
            .map(|(key, _)| key.clone())
            .collect();
        for key in done {
            running.remove(&key);
            completed.insert(key.clone());
            for (sample, cache) in samples.iter().zip(&mut delivered) {
                if let Some(answer) = sample.answers.get(&key) {
                    cache.insert(key.clone(), answer.clone());
                }
            }
        }
    }
    for (tree, sample) in trees.iter().zip(samples) {
        for key in tree.nodes.keys() {
            requested.entry(key.clone()).or_default().insert(&sample.fingerprint);
        }
    }
    let mut utilization = [0.0; 20];
    if elapsed > 0.0 {
        let width = elapsed / 20.0;
        for (i, value) in utilization.iter_mut().enumerate() {
            let left = i as f64 * width;
            let right = (i + 1) as f64 * width;
            for &(start, end, busy) in &intervals {
                *value += (end.min(right) - start.max(left)).max(0.0) * busy as f64;
            }
            *value /= width * workers as f64;
        }
    }
    Ok(Simulation {
        elapsed,
        lower: match timing {
            Timing::Maximum => total / workers as f64,
            Timing::Mean => completed.len().div_ceil(workers) as f64 * mean,
        },
        count: completed.len(),
        utilization,
        mixed: requested.values().filter(|conditions| conditions.len() > 1).count(),
        trees,
    })
}

#[derive(Clone, Copy, Default)]
pub(super) enum Arrival {
    #[default]
    All,
    Steady,
    Waves,
}

#[derive(Clone, Copy)]
pub(super) struct PrefetchOptions {
    pub workers: usize,
    pub fallback: Timing,
    pub reuse: bool,
    pub rebuilds_per_turn: usize,
    pub work_limit: usize,
    pub record_candidates: bool,
    pub arrival: Arrival,
    pub preempt_below: Option<f64>,
    pub stop_cost: f64,
    pub observe_nodes: bool,
}

impl Default for PrefetchOptions {
    fn default() -> Self {
        Self {
            workers: 128,
            fallback: Timing::Mean,
            reuse: true,
            rebuilds_per_turn: 4,
            work_limit: super::model::PREFETCH_WORK,
            record_candidates: false,
            arrival: Arrival::All,
            preempt_below: None,
            stop_cost: 0.0,
            observe_nodes: true,
        }
    }
}

#[derive(Default)]
pub(super) struct PrefetchSimulation {
    pub trees_done: f64,
    pub first_waits: Vec<f64>,
    pub wait_count: usize,
    pub stopped: usize,
    pub discarded: f64,
    pub mistaken_progress: Vec<f64>,
    pub workers_done: f64,
    pub read: usize,
    pub used: usize,
    pub absent: usize,
    pub wasted_seconds: f64,
    pub same_wait: f64,
    pub other_wait: f64,
    pub same_p95: f64,
    pub other_p95: f64,
    pub same_positive: usize,
    pub other_positive: usize,
    pub same_max: f64,
    pub other_max: f64,
    pub max_concurrent: usize,
    pub added: usize,
    pub added_moves: usize,
    pub work_limits: usize,
    pub unread: usize,
    pub cyclic: usize,
    pub no_slot: usize,
    pub rebuilds: usize,
    pub rebuild_total: f64,
    pub rebuild_max: f64,
    pub candidates: Vec<(f64, Vec<(String, u32)>)>,
    pub trees: Vec<Tree>,
}

#[derive(Default)]
struct ShadowCache {
    dirty: bool,
    candidates: Vec<super::model::PrefetchCandidate>,
}
struct SimJob {
    start: f64,
    duration: f64,
    end: f64,
    prefetch: bool,
    priority: u32,
    yielding: bool,
}

#[derive(Default)]
struct RecentDurations(VecDeque<f64>);

impl RecentDurations {
    fn push(&mut self, seconds: f64) {
        self.0.push_back(seconds);
        if self.0.len() > 50 {
            self.0.pop_front();
        }
    }

    fn estimate(&self) -> Option<f64> {
        (self.0.len() >= 10).then(|| median(self.0.iter().copied().collect()))
    }
}

fn median(mut values: Vec<f64>) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f64::total_cmp);
    (values[(values.len() - 1) / 2] + values[values.len() / 2]) / 2.0
}

impl SimJob {
    fn true_progress(&self, now: f64) -> f64 {
        if self.duration == 0.0 {
            1.0
        } else {
            ((now - self.start) / self.duration).clamp(0.0, 1.0)
        }
    }

    fn progress(&self, now: f64, estimate: f64, observe: bool) -> f64 {
        let elapsed = now - self.start;
        let time = if estimate > 0.0 {
            elapsed / estimate
        } else {
            f64::INFINITY
        };
        let nodes = if observe && self.duration > 0.0 {
            (elapsed / 60.0).floor() * 60.0 / self.duration
        } else {
            0.0
        };
        time.max(nodes)
    }
}

fn preemption_candidates(
    running: &BTreeMap<String, SimJob>,
    demanded: &BTreeSet<String>,
    unassigned: usize,
    recent: &RecentDurations,
    now: f64,
    options: PrefetchOptions,
) -> Vec<String> {
    let (Some(estimate), Some(threshold)) = (recent.estimate(), options.preempt_below) else {
        return Vec::new();
    };
    let yielding = running.values().filter(|j| j.yielding).count();
    let mut candidates: Vec<_> = running
        .iter()
        .filter(|(key, job)| job.prefetch && !job.yielding && !demanded.contains(*key))
        .map(|(key, job)| (key, job, job.progress(now, estimate, options.observe_nodes)))
        .filter(|(_, _, progress)| *progress < threshold)
        .collect();
    candidates.sort_by(|(ka, a, pa), (kb, b, pb)| {
        pa.total_cmp(pb)
            .then_with(|| b.priority.cmp(&a.priority))
            .then_with(|| ka.cmp(kb))
    });
    candidates
        .into_iter()
        .take(unassigned.saturating_sub(yielding))
        .map(|(key, _, _)| key.clone())
        .collect()
}

fn percentile95(mut values: Vec<f64>) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f64::total_cmp);
    values[(values.len() * 95).div_ceil(100) - 1]
}

/// 待ちは共有キューのキーごとに数え、即時に満たされた要求も母集団へ入れる。
pub(super) fn simulate_prefetch(
    samples: &[&Sample],
    workers: usize,
    options: PrefetchOptions,
) -> Result<PrefetchSimulation> {
    ensure!(
        workers > 0 && options.rebuilds_per_turn > 0,
        "worker 数と周回の上限は正数が必要です"
    );
    ensure!(
        options.stop_cost.is_finite() && options.stop_cost >= 0.0,
        "停止費用は有限の非負数が必要です"
    );
    ensure!(
        options.preempt_below.is_none_or(|v| v.is_finite() && v > 0.0 && v <= 1.0),
        "横取りの線は 0 より大きく 1 以下が必要です"
    );
    let mut durations = BTreeMap::<String, f64>::new();
    for sample in samples {
        for key in &sample.requested {
            let answer = sample.answers.get(key).context("要求の読みがありません")?;
            ensure!(answer.sec.is_finite() && answer.sec >= 0.0, "読み時間が不正");
            durations
                .entry(key.clone())
                .and_modify(|t| *t = t.max(answer.sec))
                .or_insert(answer.sec);
        }
    }
    let fallback = match options.fallback {
        Timing::Mean => durations.values().sum::<f64>() / durations.len().max(1) as f64,
        Timing::Maximum => durations.values().copied().fold(0.0, f64::max),
    };
    let mut trees = samples.iter().map(|s| s.tree()).collect::<Result<Vec<_>>>()?;
    let mut delivered = vec![BTreeMap::new(); samples.len()];
    let mut shadows: Vec<_> = samples
        .iter()
        .map(|_| ShadowCache {
            dirty: true,
            ..Default::default()
        })
        .collect();
    let mut running = BTreeMap::<String, SimJob>::new();
    let mut completed = BTreeSet::new();
    let mut speculative = BTreeMap::<String, f64>::new();
    let mut waits = BTreeMap::<String, (f64, f64)>::new();
    let mut result = PrefetchSimulation::default();
    let mut predicted_pending = BTreeSet::new();
    let mut unread = BTreeSet::new();
    let mut cyclic = BTreeSet::new();
    let mut elapsed = 0.0;
    let mut finished_at = None;
    let mut refreshed = vec![false; samples.len()];
    let mut recent = RecentDurations::default();
    let mut order: Vec<_> = (0..samples.len()).collect();
    order.sort_by_key(|i| samples[*i].order);
    let mut arrivals = vec![f64::INFINITY; samples.len()];
    for (rank, &i) in order.iter().enumerate() {
        arrivals[i] = match options.arrival {
            Arrival::All => 0.0,
            Arrival::Steady => rank as f64 * 720.0,
            Arrival::Waves if rank < 20 => 0.0,
            Arrival::Waves => f64::INFINITY,
        };
    }
    let mut wave_start = 0;
    let mut wave_end = samples.len().min(20);
    let mut first_started = vec![None; samples.len()];
    loop {
        let mut missing = Vec::new();
        for (i, tree) in trees.iter_mut().enumerate() {
            if arrivals[i] > elapsed {
                missing.push(Vec::new());
                continue;
            }
            let before = tree.nodes.len();
            let pending = tree.advance(&delivered[i])?;
            shadows[i].dirty |= tree.nodes.len() != before;
            for (key, _) in &pending {
                ensure!(
                    samples[i].answers.contains_key(key),
                    "木 {} の読みが足りない: {key}",
                    samples[i].order
                );
                waits.entry(key.clone()).or_default();
            }
            missing.push(pending);
        }
        if matches!(options.arrival, Arrival::Waves)
            && wave_end < samples.len()
            && order[wave_start..wave_end].iter().filter(|i| trees[**i].finished).count()
                >= ((wave_end - wave_start) * 4).div_ceil(5)
        {
            wave_start = wave_end;
            wave_end = (wave_end + 20).min(samples.len());
            for &i in &order[wave_start..wave_end] {
                arrivals[i] = elapsed;
            }
            continue;
        }
        if trees.iter().all(|t| t.finished) && finished_at.is_none() {
            finished_at = Some(elapsed);
        }
        let demanded: BTreeSet<_> = missing.iter().flatten().map(|(k, _)| k.clone()).collect();
        for key in &demanded {
            if predicted_pending.remove(key) && !speculative.contains_key(key) {
                result.no_slot += 1;
            }
        }
        let busy = running.keys().cloned().collect();
        let demands = samples.iter().enumerate().map(|(i, sample)| Demand {
            priority: sample.priority,
            order: sample.order,
            missing: missing[i].clone(),
            ply: &trees[i].ply,
        });
        let queue = reading_queue(demands, &busy);
        let free = workers - running.len();
        for (key, _) in queue.iter().take(free) {
            ensure!(!completed.contains(key), "完了済みの局面を再配布");
            running.insert(
                key.clone(),
                SimJob {
                    start: elapsed,
                    duration: durations[key],
                    end: elapsed + durations[key],
                    prefetch: false,
                    priority: 0,
                    yielding: false,
                },
            );
        }
        let queued: Vec<_> = queue.into_iter().skip(free).map(|(k, _)| k).collect();
        let unassigned = demanded
            .iter()
            .filter(|key| running.get(*key).is_none_or(|j| j.yielding))
            .count();
        for key in preemption_candidates(&running, &demanded, unassigned, &recent, elapsed, options)
        {
            let job = running.get_mut(&key).unwrap();
            let progress = job.true_progress(elapsed);
            result.stopped += 1;
            result.discarded += progress;
            if progress >= options.preempt_below.unwrap() {
                result.mistaken_progress.push(progress);
            }
            job.yielding = true;
            job.end = elapsed + options.stop_cost;
            speculative.remove(&key);
            for shadow in &mut shadows {
                shadow.dirty |= shadow.candidates.iter().any(|c| c.key == key);
            }
        }
        for (i, pending) in missing.iter().enumerate() {
            if arrivals[i] <= elapsed
                && first_started[i].is_none()
                && (trees[i].finished
                    || trees[i].nodes.keys().any(|k| completed.contains(k))
                    || pending.iter().any(|(k, _)| running.get(k).is_some_and(|j| !j.yielding)))
            {
                first_started[i] = Some(elapsed - arrivals[i]);
            }
        }
        let speculative_running =
            running.iter().filter(|(k, j)| j.prefetch && !demanded.contains(*k)).count();
        let slots =
            (workers - running.len()).min(options.workers.saturating_sub(speculative_running));
        let mut deferred = false;
        if queued.is_empty() && slots > 0 && finished_at.is_none() {
            let mut rebuilt = 0;
            for (i, shadow) in shadows.iter_mut().enumerate() {
                if arrivals[i] > elapsed {
                    continue;
                }
                if trees[i].finished
                    || trees[i].failed
                    || trees[i].def.depth == 0
                    || trees[i].round >= trees[i].def.rounds
                {
                    shadow.candidates.clear();
                    shadow.dirty = false;
                    continue;
                }
                if shadow.dirty || (!options.reuse && !refreshed[i]) {
                    if rebuilt == options.rebuilds_per_turn {
                        deferred = true;
                        continue;
                    }
                    let start = std::time::Instant::now();
                    let report = trees[i].prefetch_report(
                        &samples[i].lines,
                        &delivered[i],
                        false,
                        options.work_limit,
                    )?;
                    let seconds = start.elapsed().as_secs_f64();
                    result.work_limits += usize::from(report.limited);
                    unread.extend(report.unread.into_iter().map(|k| (i, trees[i].round, k)));
                    cyclic.extend(report.cyclic.into_iter().map(|k| (i, trees[i].round, k)));
                    shadow.candidates = report.candidates;
                    predicted_pending.extend(
                        shadow
                            .candidates
                            .iter()
                            .filter(|c| !demanded.contains(&c.key) && !completed.contains(&c.key))
                            .map(|c| c.key.clone()),
                    );
                    result.rebuilds += 1;
                    result.rebuild_total += seconds;
                    result.rebuild_max = result.rebuild_max.max(seconds);
                    shadow.dirty = false;
                    refreshed[i] = true;
                    rebuilt += 1;
                }
            }
            if deferred {
                continue;
            }
            let mut candidates = BTreeMap::<String, (u32, u64, usize, u32)>::new();
            for (i, shadow) in shadows.iter().enumerate() {
                // 更新を後回しにした影は、古い候補を配る前に作り直す。
                if shadow.dirty || arrivals[i] > elapsed {
                    continue;
                }
                for candidate in &shadow.candidates {
                    let key = &candidate.key;
                    if demanded.contains(key)
                        || running.contains_key(key)
                        || completed.contains(key)
                    {
                        continue;
                    }
                    let rank =
                        (samples[i].priority, samples[i].order, candidate.level, candidate.ply);
                    candidates
                        .entry(key.clone())
                        .and_modify(|old| {
                            if (rank.0, rank.1, rank.2) < (old.0, old.1, old.2) {
                                *old = rank;
                            }
                        })
                        .or_insert(rank);
                }
            }
            let mut candidates: Vec<_> = candidates.into_iter().collect();
            candidates.sort_by(|(ka, a), (kb, b)| (a.0, a.1, a.2, ka).cmp(&(b.0, b.1, b.2, kb)));
            if options.record_candidates {
                result.candidates.push((
                    elapsed,
                    candidates.iter().map(|(k, rank)| (k.clone(), rank.3)).collect(),
                ));
            }
            for (key, rank) in candidates.into_iter().take(slots) {
                let duration = durations.get(&key).copied().or_else(|| {
                    samples
                        .iter()
                        .filter_map(|s| s.answers.get(&key))
                        .map(|r| r.sec)
                        .reduce(f64::max)
                });
                let duration = if let Some(sec) = duration {
                    ensure!(sec.is_finite() && sec >= 0.0, "先読み時間が不正");
                    sec
                } else {
                    result.absent += 1;
                    fallback
                };
                speculative.insert(key.clone(), duration);
                running.insert(
                    key,
                    SimJob {
                        start: elapsed,
                        duration,
                        end: elapsed + duration,
                        prefetch: true,
                        priority: rank.0,
                        yielding: false,
                    },
                );
            }
        }
        result.max_concurrent = result
            .max_concurrent
            .max(running.iter().filter(|(k, j)| j.prefetch && !demanded.contains(*k)).count());
        if running.is_empty() && finished_at.is_some() {
            break;
        }
        let next = running
            .values()
            .map(|j| j.end)
            .chain(arrivals.iter().copied().filter(|t| t.is_finite() && *t > elapsed))
            .min_by(f64::total_cmp)
            .context("未完了の木があるのに配布できません")?;
        let dt = next - elapsed;
        for key in &demanded {
            if running.get(key).is_some_and(|j| j.prefetch) {
                waits.get_mut(key).unwrap().0 += dt;
            }
        }
        // 別の先読みを外した場合に空く枠に入る要求だけに、占有時間を帰属させる。
        for key in queued.iter().take(speculative_running) {
            waits.get_mut(key).unwrap().1 += dt;
        }
        elapsed = next;
        refreshed.fill(false);
        let done: Vec<_> =
            running.iter().filter(|(_, j)| j.end == next).map(|(k, _)| k.clone()).collect();
        for key in done {
            let job = running.remove(&key).unwrap();
            if job.yielding {
                continue;
            }
            recent.push(job.duration);
            completed.insert(key.clone());
            for (i, sample) in samples.iter().enumerate() {
                if let Some(answer) = sample.answers.get(&key) {
                    delivered[i].insert(key.clone(), answer.clone());
                    shadows[i].dirty |= shadows[i].candidates.iter().any(|c| c.key == key);
                }
            }
        }
    }
    let used: BTreeSet<_> = trees.iter().flat_map(|t| t.nodes.keys()).collect();
    for key in &used {
        waits.entry((*key).clone()).or_default();
    }
    result.added = trees.iter().flat_map(|t| &t.round_reports).map(|(_, n)| n).sum();
    result.added_moves = trees.iter().flat_map(|t| t.force.values()).map(BTreeSet::len).sum();
    result.unread = unread.len();
    result.cyclic = cyclic.len();
    result.first_waits = first_started
        .into_iter()
        .collect::<Option<Vec<_>>>()
        .context("初回開始の記録がありません")?;
    result.wait_count = waits.len();
    result.same_positive = waits.values().filter(|w| w.0 > 0.0).count();
    result.other_positive = waits.values().filter(|w| w.1 > 0.0).count();
    result.same_max = waits.values().map(|w| w.0).fold(0.0, f64::max);
    result.other_max = waits.values().map(|w| w.1).fold(0.0, f64::max);
    result.read = speculative.len();
    result.used = speculative.keys().filter(|k| used.contains(k)).count();
    result.wasted_seconds =
        speculative.iter().filter(|(k, _)| !used.contains(k)).map(|(_, sec)| sec).sum();
    result.same_wait = waits.values().map(|w| w.0).sum();
    result.other_wait = waits.values().map(|w| w.1).sum();
    result.same_p95 = percentile95(waits.values().map(|w| w.0).collect());
    result.other_p95 = percentile95(waits.values().map(|w| w.1).collect());
    result.trees_done = finished_at.unwrap_or(0.0);
    result.workers_done = elapsed;
    result.trees = trees;
    Ok(result)
}

pub(super) fn preflight(sample: &mut Sample) -> Result<()> {
    let mut tree = sample.tree()?;
    let missing = tree.advance(sample.answers.as_ref())?;
    ensure!(
        tree.finished,
        "読みが足りない: {} (不足 {} 局面)",
        missing.first().map_or("不明", |(key, _)| key.as_str()),
        missing.len()
    );
    sample.requested = tree.nodes.into_keys().collect();
    Ok(())
}

type AnswerCache = BTreeMap<(PathBuf, String), Arc<BTreeMap<String, Reading>>>;

fn load_sample(
    state: &Path,
    journal: &Path,
    id: &str,
    state_order: usize,
    sources: &mut AnswerCache,
) -> Result<Sample> {
    let checkpoint = Checkpoint::load(&state.join(CHECKPOINT))?.context("受付記録がありません")?;
    let status: StatusFile = serde_json::from_slice(&std::fs::read(state.join(STATUS_FILE))?)?;
    let queue = std::fs::read(state.join(QUEUE))?;
    let prefix = queue
        .get(..usize::try_from(checkpoint.processed_bytes)?)
        .context("キューが受付記録より短い")?;
    ensure!(
        format!("{:x}", Sha256::digest(prefix)) == checkpoint.processed_sha256,
        "キューと受付記録が一致しません"
    );
    ensure!(
        status.processed_bytes == checkpoint.processed_bytes
            && status.processed_sha256 == checkpoint.processed_sha256,
        "状態と受付記録が一致しません"
    );
    let targets: Vec<_> = checkpoint.targets.iter().filter(|t| t.id == id).collect();
    ensure!(targets.len() == 1, "id が存在しないか手番が一意でありません: {id}");
    let target = targets[0];
    let current = status
        .targets
        .iter()
        .find(|s| s.id == id && s.own == target.own)
        .context("今の世代の状態がありません")?;
    ensure!(
        current.generation == target.generation && current.out == target.out,
        "状態と受付記録の世代が違います"
    );
    ensure!(
        matches!(current.state.as_str(), "DONE" | "NOT_READY"),
        "木が未完了: {}",
        current.state
    );
    let source_key = (journal.to_owned(), checkpoint.fingerprint.clone());
    if !sources.contains_key(&source_key) {
        let readings = simulation_readings(journal, &checkpoint.fingerprint)?;
        sources.insert(source_key.clone(), Arc::new(readings));
    }
    let order = u64::try_from(state_order)?
        .checked_mul(1u64 << 32)
        .context("state 数が範囲外")?;
    ensure!(target.order < 1u64 << 32, "受付順が範囲外");
    Ok(Sample {
        params: target.definition.params(&target.own, &target.out),
        lines: target.definition.lines(),
        answers: sources[&source_key].clone(),
        requested: BTreeSet::new(),
        fingerprint: checkpoint.fingerprint,
        priority: target.priority,
        order: order | target.order,
    })
}

fn load_samples(input: &Path) -> Result<Vec<Sample>> {
    let mut sources = BTreeMap::new();
    let mut samples = Vec::new();
    let mut states = BTreeMap::new();
    let mut excluded = 0;
    for (index, line) in std::fs::read_to_string(input)?.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let cols: Vec<_> = line.split('\t').collect();
        ensure!(cols.len() == 3, "入力は state / journal / id の TSV: 行 {}", index + 1);
        let loaded = std::fs::canonicalize(cols[0])
            .map_err(anyhow::Error::from)
            .and_then(|state| {
                let next_order = states.len();
                let state_order = *states.entry(state.clone()).or_insert(next_order);
                load_sample(&state, Path::new(cols[1]), cols[2], state_order, &mut sources)
            })
            .and_then(|mut sample| {
                preflight(&mut sample)?;
                Ok(sample)
            });
        match loaded {
            Ok(sample) => samples.push(sample),
            Err(error) => {
                excluded += 1;
                println!("EXCLUDED\t{}\t{error:#}", cols[2]);
            }
        }
    }
    println!("INPUT\taccepted={}\texcluded={excluded}\tworkers=128\tseed=42", samples.len());
    ensure!(!samples.is_empty(), "再生できる木がありません");
    Ok(samples)
}

#[test]
#[ignore]
fn journal_simulation() -> Result<()> {
    let Some(input) = std::env::var_os("BOOK_TREE_SIM_INPUT") else {
        return Ok(());
    };
    let samples = load_samples(Path::new(&input))?;
    let mut rng = ChaCha8Rng::seed_from_u64(42);
    let mut groups = Vec::new();
    for size in [20, 40, 5, 10] {
        if size > samples.len() {
            continue;
        }
        for trial in 1..=5 {
            let mut indices: Vec<_> = (0..samples.len()).collect();
            indices.shuffle(&mut rng);
            indices.truncate(size);
            indices.sort_unstable();
            groups.push((format!("{size}/{trial}"), indices));
        }
    }
    groups.push(("all".into(), (0..samples.len()).collect()));
    println!("NOTE\t20/40/all: six settings; 5/10: baseline only");
    println!(
        "group\tcap\tfallback\tT_sec\tTprime_sec\tdelta_sec\tdelta_pct\tworkers_done\tread\tused\twasted\tabsent\tabsent_pct\twaste_worker_sec\tsame_wait\tother_wait\tsame_p95\tother_p95\trebuilds\trebuild_max_sec\trebuild_total_sec\tadded_round_X\tunread_X_round\tcyclic_X_round\twork_limits\tno_slot_keys\tmax_concurrent\tsame_positive\tsame_max\tother_positive\tother_max\tL_sec\t1-L/T\tutilization_20\tbaseline_timing\tN\tU\tmixed\tadded_moves"
    );
    for (group, indices) in groups {
        let selected: Vec<_> = indices.iter().map(|i| &samples[*i]).collect();
        let baseline = simulate(&selected, 128, Timing::Maximum)?;
        for (baseline_mode, reference) in [
            ("max", simulate(&selected, 128, Timing::Maximum)?),
            ("mean", simulate(&selected, 128, Timing::Mean)?),
        ] {
            let added: usize =
                reference.trees.iter().flat_map(|t| &t.round_reports).map(|(_, n)| n).sum();
            let utilization = reference
                .utilization
                .iter()
                .map(|x| format!("{x:.6}"))
                .collect::<Vec<_>>()
                .join(",");
            println!(
                "{group}\t0\t-\t{:.3}\t{:.3}\t0\t0\t{:.3}\t0\t0\t0\t0\t0\t0\t0\t0\t0\t0\t0\t0\t0\t{added}\t0\t0\t0\t0\t0\t0\t0\t0\t0\t{:.3}\t{:.6}\t{utilization}\t{baseline_mode}\t{}\t{}\t{}\t{}",
                reference.elapsed,
                reference.elapsed,
                reference.elapsed,
                reference.lower,
                1.0 - reference.lower / reference.elapsed,
                reference.trees.len(),
                reference.count,
                reference.mixed,
                reference
                    .trees
                    .iter()
                    .flat_map(|t| t.force.values())
                    .map(BTreeSet::len)
                    .sum::<usize>()
            );
        }
        if indices.len() < 20 && group != "all" {
            continue;
        }
        for cap in [128, 64, 32] {
            for (mode, fallback) in [("mean", Timing::Mean), ("max", Timing::Maximum)] {
                let result = simulate_prefetch(
                    &selected,
                    128,
                    PrefetchOptions {
                        workers: cap,
                        fallback,
                        ..Default::default()
                    },
                )?;
                let delta = result.trees_done - baseline.elapsed;
                println!(
                    "{group}\t{cap}\t{mode}\t{:.3}\t{:.3}\t{delta:.3}\t{:.3}\t{:.3}\t{}\t{}\t{}\t{}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{}\t{:.6}\t{:.6}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:.3}\t{}\t{:.3}\t-\t-\t-\tmax\t{}\t{}\t{}\t{}",
                    baseline.elapsed,
                    result.trees_done,
                    100.0 * delta / baseline.elapsed,
                    result.workers_done,
                    result.read,
                    result.used,
                    result.read - result.used,
                    result.absent,
                    100.0 * result.absent as f64 / result.read.max(1) as f64,
                    result.wasted_seconds,
                    result.same_wait,
                    result.other_wait,
                    result.same_p95,
                    result.other_p95,
                    result.rebuilds,
                    result.rebuild_max,
                    result.rebuild_total,
                    result.added,
                    result.unread,
                    result.cyclic,
                    result.work_limits,
                    result.no_slot,
                    result.max_concurrent,
                    result.same_positive,
                    result.same_max,
                    result.other_positive,
                    result.other_max,
                    baseline.trees.len(),
                    baseline.count,
                    baseline.mixed,
                    result.added_moves
                );
            }
        }
    }
    Ok(())
}

#[test]
fn percentile_includes_zero_wait_requests() {
    let mut waits = vec![0.0; 99];
    waits.push(100.0);
    assert_eq!(percentile95(waits), 0.0);
    assert_eq!(percentile95(vec![0.0, 10.0]), 10.0);
}

#[test]
#[ignore]
fn preemption_simulation() -> Result<()> {
    use std::io::Write;
    let input =
        std::env::var_os("BOOK_TREE_SIM_INPUT").context("BOOK_TREE_SIM_INPUT が必要です")?;
    let output =
        std::env::var_os("BOOK_TREE_SIM_OUTPUT").context("BOOK_TREE_SIM_OUTPUT が必要です")?;
    let samples = load_samples(Path::new(&input))?;
    ensure!(samples.len() == 93, "93 本すべての入力が必要です");
    let selected: Vec<_> = samples.iter().collect();
    let mut file = std::io::BufWriter::new(std::fs::File::create(output)?);
    writeln!(
        file,
        "arrival\tpolicy\tcost_sec\tnodes\ttrees\ttrees_done_sec\tfirst_count\tfirst_positive\tfirst_max_sec\tfirst_p95_sec\trequest_count\tsame_positive\tsame_total_sec\tsame_max_sec\tsame_p95_sec\tother_positive\tother_total_sec\tother_max_sec\tother_p95_sec\tstopped\tdiscarded_positions\tmistaken\tmistaken_median\tmistaken_max\tmisses\tabsent\tprefetch_read"
    )?;
    for (arrival_name, arrival) in [
        ("all", Arrival::All),
        ("steady", Arrival::Steady),
        ("waves", Arrival::Waves),
    ] {
        for observe_nodes in [true, false] {
            if !observe_nodes && !matches!(arrival, Arrival::Waves) {
                continue;
            }
            for (policy, cap, threshold) in [
                ("none", 0, None),
                ("no-preempt", 128, None),
                ("reserve-4", 124, None),
                ("reserve-16", 112, None),
                ("10%", 128, Some(0.1)),
                ("25%", 128, Some(0.25)),
                ("50%", 128, Some(0.5)),
                ("100%", 128, Some(1.0)),
            ] {
                for stop_cost in [0.0, 30.0] {
                    if stop_cost > 0.0 && !matches!(policy, "25%" | "100%") {
                        continue;
                    }
                    let result = simulate_prefetch(
                        &selected,
                        128,
                        PrefetchOptions {
                            workers: cap,
                            arrival,
                            preempt_below: threshold,
                            stop_cost,
                            observe_nodes,
                            ..Default::default()
                        },
                    )?;
                    for (tree, sample) in result.trees.iter().zip(&samples) {
                        let mut expected = sample.tree()?;
                        expected.advance(sample.answers.as_ref())?;
                        ensure!(
                            tree.nodes == expected.nodes
                                && tree.force == expected.force
                                && tree.round_reports == expected.round_reports,
                            "先読みにより木の結果が変わりました"
                        );
                    }
                    writeln!(
                        file,
                        "{arrival_name}\t{policy}\t{stop_cost}\t{}\t{}\t{:.3}\t{}\t{}\t{:.3}\t{:.3}\t{}\t{}\t{:.3}\t{:.3}\t{:.3}\t{}\t{:.3}\t{:.3}\t{:.3}\t{}\t{:.6}\t{}\t{:.6}\t{:.6}\t{}\t{}\t{}",
                        if observe_nodes { "60s" } else { "absent" },
                        samples.len(),
                        result.trees_done,
                        result.first_waits.len(),
                        result.first_waits.iter().filter(|t| **t > 0.0).count(),
                        result.first_waits.iter().copied().fold(0.0, f64::max),
                        percentile95(result.first_waits),
                        result.wait_count,
                        result.same_positive,
                        result.same_wait,
                        result.same_max,
                        result.same_p95,
                        result.other_positive,
                        result.other_wait,
                        result.other_max,
                        result.other_p95,
                        result.stopped,
                        result.discarded,
                        result.mistaken_progress.len(),
                        median(result.mistaken_progress.clone()),
                        result.mistaken_progress.iter().copied().fold(0.0, f64::max),
                        result.read - result.used,
                        result.absent,
                        result.read
                    )?;
                    file.flush()?;
                    eprintln!(
                        "SIM {arrival_name} {policy} cost={stop_cost} nodes={observe_nodes} done={:.3}",
                        result.trees_done
                    );
                }
            }
        }
    }
    Ok(())
}

fn preemption_fixture() -> (BTreeMap<String, SimJob>, RecentDurations, PrefetchOptions) {
    let jobs = [("slow", 90.0, 1), ("fast", 80.0, 2), ("tie", 90.0, 3)]
        .into_iter()
        .map(|(key, start, priority)| {
            (
                key.to_owned(),
                SimJob {
                    start,
                    duration: 100.0,
                    end: start + 100.0,
                    prefetch: true,
                    priority,
                    yielding: false,
                },
            )
        })
        .collect();
    let mut recent = RecentDurations::default();
    for _ in 0..10 {
        recent.push(100.0);
    }
    (
        jobs,
        recent,
        PrefetchOptions {
            preempt_below: Some(0.25),
            ..Default::default()
        },
    )
}

#[test]
fn preemption_chooses_least_progress_then_low_priority() {
    let (jobs, recent, options) = preemption_fixture();
    assert_eq!(
        preemption_candidates(&jobs, &BTreeSet::new(), 3, &recent, 100.0, options),
        ["tie", "slow", "fast"]
    );
    let demanded = BTreeSet::from(["tie".into()]);
    assert_eq!(preemption_candidates(&jobs, &demanded, 1, &recent, 100.0, options), ["slow"]);
}

#[test]
fn preemption_protects_threshold_and_observed_nodes() {
    let (mut jobs, recent, mut options) = preemption_fixture();
    options.preempt_below = Some(0.1);
    assert!(preemption_candidates(&jobs, &BTreeSet::new(), 3, &recent, 100.0, options).is_empty());
    jobs.retain(|k, _| k == "slow");
    let job = jobs.get_mut("slow").unwrap();
    job.start = 0.0;
    job.duration = 120.0;
    let mut long = RecentDurations::default();
    for _ in 0..10 {
        long.push(1000.0);
    }
    options.preempt_below = Some(0.5);
    assert!(preemption_candidates(&jobs, &BTreeSet::new(), 1, &long, 61.0, options).is_empty());
    options.observe_nodes = false;
    assert_eq!(
        preemption_candidates(&jobs, &BTreeSet::new(), 1, &long, 61.0, options),
        ["slow"]
    );
    assert_eq!(jobs["slow"].progress(59.0, 1000.0, true), 0.059);
}

#[test]
fn preemption_needs_ten_completed_readings_and_keeps_fifty() {
    let (jobs, _, options) = preemption_fixture();
    let mut recent = RecentDurations::default();
    for _ in 0..9 {
        recent.push(100.0);
    }
    assert!(preemption_candidates(&jobs, &BTreeSet::new(), 3, &recent, 100.0, options).is_empty());
    recent.push(100.0);
    assert_eq!(recent.estimate(), Some(100.0));
    for _ in 0..50 {
        recent.push(200.0);
    }
    assert_eq!(recent.0.len(), 50);
    assert_eq!(recent.estimate(), Some(200.0));
}

#[test]
fn preemption_does_not_repeat_yielding_and_bounds_each_turn() {
    let (mut jobs, recent, options) = preemption_fixture();
    let selected = preemption_candidates(&jobs, &BTreeSet::new(), 2, &recent, 100.0, options);
    assert_eq!(selected.len(), 2);
    for key in selected {
        jobs.get_mut(&key).unwrap().yielding = true;
    }
    assert!(preemption_candidates(&jobs, &BTreeSet::new(), 2, &recent, 100.0, options).is_empty());
    assert_eq!(
        preemption_candidates(&jobs, &BTreeSet::new(), 3, &recent, 100.0, options),
        ["fast"]
    );
}
