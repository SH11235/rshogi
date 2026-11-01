use super::journal::{Reading, simulation_readings};
use super::model::{GameLine, Tree, TreeParams};
use super::service::{Demand, reading_queue};
use super::targets::{CHECKPOINT, Checkpoint, QUEUE, STATUS_FILE, StatusFile};
use anyhow::{Context, Result, ensure};
use rand::SeedableRng;
use rand::seq::SliceRandom;
use rand_chacha::ChaCha8Rng;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
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

#[test]
#[ignore]
fn journal_simulation() -> Result<()> {
    let Some(input) = std::env::var_os("BOOK_TREE_SIM_INPUT") else {
        return Ok(());
    };
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
    let mut rng = ChaCha8Rng::seed_from_u64(42);
    let mut groups = Vec::new();
    for size in [5, 10, 20, 40] {
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
    println!("group\tmode\tN\tT_sec\tL_sec\t1-L/T\tU\tmixed\tutilization_20_pct");
    for (group, indices) in groups {
        let selected: Vec<_> = indices.iter().map(|i| &samples[*i]).collect();
        for (mode, timing) in [("max", Timing::Maximum), ("mean", Timing::Mean)] {
            let result = simulate(&selected, 128, timing)?;
            let gap = if result.elapsed == 0.0 {
                0.0
            } else {
                1.0 - result.lower / result.elapsed
            };
            let utilization = result
                .utilization
                .iter()
                .map(|u| format!("{:.1}", 100.0 * u))
                .collect::<Vec<_>>()
                .join(",");
            println!(
                "{group}\t{mode}\t{}\t{:.3}\t{:.3}\t{gap:.6}\t{}\t{}\t{utilization}",
                result.trees.len(),
                result.elapsed,
                result.lower,
                result.count,
                result.mixed
            );
        }
    }
    Ok(())
}
