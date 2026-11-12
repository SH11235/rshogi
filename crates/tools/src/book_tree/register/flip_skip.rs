//! 登録した元データが反転キーで合流した局面で伝播を止めるとき、取り込まない局面を探して足し、通るまで繰り返す。
//! 最初に木ごとに独立に調べ、残った問題 (木の組み合わせで止まる分) は全部の木の局面から絞る。

use anyhow::{Result, ensure};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

/// 探した結果。
#[derive(Debug, PartialEq)]
pub(super) struct Found {
    /// 足した局面の key (見つけた順)。
    pub keys: Vec<String>,
    /// 最後の検査を通ったか (偽なら上限の回数を使い切った)。
    pub passed: bool,
    /// 検査を回した回数。
    pub trials: usize,
}

/// 木の番号の組と取り込まない局面で登録した元データを検査し、反転キーで止まるならその理由を返す。
pub(super) trait Trial: Sync {
    fn failure(&self, trees: &[usize], skip: &BTreeSet<String>) -> Result<Option<String>>;
    /// `failure` より軽い検査。まれに `failure` と結果が違ってよい。探す途中の組だけに使い、原因の木と見つけた
    /// 局面は `failure` で確かめる。
    fn rough_failure(&self, trees: &[usize], skip: &BTreeSet<String>) -> Result<Option<String>> {
        self.failure(trees, skip)
    }
    /// その木が取り込む局面のうち、`skip` で除かれていないもの。
    fn keys(&self, trees: &[usize], skip: &BTreeSet<String>) -> BTreeSet<String>;
}

/// 全部の木の局面から絞るとき、候補を一度に分ける数。
const ALL_TREES_WIDTH: usize = 6;

/// 1 回の試行の結果。
enum Outcome {
    Pass,
    Flip(String),
    /// 反転キー以外の理由で止まった。
    Other(anyhow::Error),
}

/// 木ごとに独立に除く局面を一度探した後、全部の木で止まるなら、全部の木の局面から、取り込まなければ全部の木で通る
/// 局面の組を探して `skip` に足す。足した後の全部の木でもう一度検査し、`rounds` 回まで繰り返す。局面を減らした
/// 途中の組が反転キー以外の理由で止まったときは、その組は通らない組として探し続ける。探す途中の組は軽い検査で
/// 調べ、全部の木・止まる木・見つけた局面は元の検査で確かめる。
pub(super) fn search<T: Trial>(
    trial: &T,
    names: &[PathBuf],
    skip: &BTreeSet<String>,
    rounds: usize,
    jobs: usize,
) -> Result<Found> {
    ensure!(jobs > 0, "並列に調べる木の数は 1 以上にしてください");
    let started = Instant::now();
    let all: Vec<usize> = (0..names.len()).collect();
    let mut skip = skip.clone();
    let mut keys = Vec::new();
    let mut stats = Stats::reporting();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(rayon::current_num_threads().max(jobs))
        .build()?;
    let mut independent_keys = 0;
    let mut independent_done = false;
    let mut round = 0;
    // 見つけた局面を足した全部の木が通ると確かめてある。
    let mut verified = false;
    let passed = loop {
        if verified {
            break true;
        }
        let message = match stats.check(trial, &all, &skip) {
            Outcome::Pass => break true,
            Outcome::Flip(message) => message,
            Outcome::Other(error) => {
                return Err(error.context("全部の木の登録が反転キー以外の理由で止まりました"));
            }
        };
        eprintln!("反転キーの検査 {round}: 止まります: {message}");
        if round == rounds {
            break false;
        }
        if !independent_done {
            ensure!(
                !matches!(stats.check(trial, &[], &skip), Outcome::Flip(_)),
                "木を取り込まない元データだけで反転キーの検査で止まります"
            );
            independent_done = true;
            let tree_started = Instant::now();
            let checks = parallel(&pool, jobs, &all, |&t| {
                let mut stats = Stats::default();
                let bad = matches!(stats.probe(trial, &[t], &skip), Outcome::Flip(_))
                    && matches!(stats.check(trial, &[t], &skip), Outcome::Flip(_));
                (t, bad, stats)
            });
            eprintln!(
                "木ごとの検査: {} 本中 {} 本が反転キーで止まります / {:.1} 秒",
                checks.len(),
                checks.iter().filter(|(_, failed, _)| *failed).count(),
                tree_started.elapsed().as_secs_f64()
            );
            let results = parallel(&pool, jobs, &checks, |(t, failed, _)| {
                let mut stats = Stats::default();
                if !failed {
                    return (0, None, stats);
                }
                let started = Instant::now();
                let candidates: Vec<_> = trial.keys(&[*t], &skip).into_iter().collect();
                let with = |extra: &[String]| -> BTreeSet<String> {
                    skip.iter().cloned().chain(extra.iter().cloned()).collect()
                };
                let found =
                    if matches!(stats.probe(trial, &[*t], &with(&candidates)), Outcome::Flip(_)) {
                        None
                    } else {
                        let found = narrow(&candidates, 2, |sets| {
                            sets.iter().position(|extra| {
                                matches!(stats.probe(trial, &[*t], &with(extra)), Outcome::Pass)
                            })
                        });
                        matches!(stats.check(trial, &[*t], &with(&found)), Outcome::Pass)
                            .then_some(found)
                    };
                eprintln!(
                    "  木ごとの絞り込み: {} / 試行 {} / {:.1} 秒",
                    names[*t].display(),
                    stats.trials,
                    started.elapsed().as_secs_f64()
                );
                (candidates.len(), found, stats)
            });
            for ((t, _, initial), (candidates, found, local)) in checks.into_iter().zip(results) {
                stats.merge(initial);
                stats.merge(local);
                if let Some(found) = found {
                    eprintln!("  原因の木: {}", names[t].display());
                    log_keys(candidates, &found);
                    for key in found {
                        if skip.insert(key.clone()) {
                            keys.push(key);
                        }
                    }
                }
            }
            independent_keys = keys.len();
            if independent_keys > 0 {
                continue;
            }
        }
        round += 1;
        let round_started = Instant::now();
        let candidates: Vec<String> = trial.keys(&all, &skip).into_iter().collect();
        let with = |extra: &[String]| -> BTreeSet<String> {
            skip.iter().cloned().chain(extra.iter().cloned()).collect()
        };
        let before = stats.trials;
        let narrowed = if matches!(stats.probe(trial, &all, &with(&candidates)), Outcome::Flip(_)) {
            None
        } else {
            // 分けた部分を並列に確かめる。どれが通ったかは分けた順で決めるので、結果は実行順にも jobs にもよらない。
            let found = narrow(&candidates, ALL_TREES_WIDTH, |sets| {
                if jobs == 1 {
                    return sets.iter().position(|extra| {
                        matches!(stats.probe(trial, &all, &with(extra)), Outcome::Pass)
                    });
                }
                let results = parallel(&pool, jobs, sets, |extra| {
                    let mut stats = Stats::default();
                    let passed = matches!(stats.probe(trial, &all, &with(extra)), Outcome::Pass);
                    (passed, stats)
                });
                let mut first = None;
                for (i, (passed, local)) in results.into_iter().enumerate() {
                    stats.merge(local);
                    if passed && first.is_none() {
                        first = Some(i);
                    }
                }
                first
            });
            matches!(stats.check(trial, &all, &with(&found)), Outcome::Pass).then_some(found)
        };
        eprintln!(
            "  全部の木の絞り込み: 試行 {} / {:.1} 秒",
            stats.trials - before,
            round_started.elapsed().as_secs_f64()
        );
        let found = match narrowed {
            Some(found) => {
                verified = true;
                log_keys(candidates.len(), &found);
                found
            }
            // 半分ずつの絞り込みで通る組が見つからなければ、原因の木の組から元の検査で探す。
            None => combination(trial, names, &all, &skip, &mut stats)?,
        };
        skip.extend(found.iter().cloned());
        keys.extend(found);
    };
    stats.report();
    eprintln!(
        "反転キーの検査: {} / 足した局面 {} (木ごと {independent_keys} / その後の巡 {}) / 試行 {} (うち軽い検査 {}・反転キー以外の理由で止まった組 {}) / {:.1} 秒",
        if passed {
            "通りました"
        } else {
            "上限の回数で通りません"
        },
        keys.len(),
        keys.len() - independent_keys,
        stats.trials,
        stats.rough,
        stats.others,
        started.elapsed().as_secs_f64()
    );
    Ok(Found {
        keys,
        passed,
        trials: stats.trials,
    })
}

/// 全部の木で止まるときの原因の木の組 (1 本除くと止まらなくなる組) を絞り、その組だけで通るようになる局面の組
/// (1 つ除くと止まる組) を返す。
fn combination<T: Trial>(
    trial: &T,
    names: &[PathBuf],
    all: &[usize],
    skip: &BTreeSet<String>,
    stats: &mut Stats,
) -> Result<Vec<String>> {
    let bad = ddmin(all, |trees| matches!(stats.check(trial, trees, skip), Outcome::Flip(_)));
    let bad_names: Vec<_> = bad.iter().map(|&t| names[t].display().to_string()).collect();
    eprintln!("  原因の木: {}", bad_names.join(" "));
    let candidates: Vec<String> = trial.keys(&bad, skip).into_iter().collect();
    let with = |extra: &[String]| -> BTreeSet<String> {
        skip.iter().cloned().chain(extra.iter().cloned()).collect()
    };
    // 全部除いた組が反転キー以外の理由で止まっても、一部だけ除いた組では通ることがあるので探す。
    // 見つけた組は最後に通ることを確かめる。
    ensure!(
        !matches!(stats.check(trial, &bad, &with(&candidates)), Outcome::Flip(_)),
        "原因の木 {} の局面を全部取り込まなくても反転キーの検査で止まります",
        bad_names.join(" ")
    );
    let found = ddmin(&candidates, |extra| {
        matches!(stats.check(trial, &bad, &with(extra)), Outcome::Pass)
    });
    ensure!(
        matches!(stats.check(trial, &bad, &with(&found)), Outcome::Pass),
        "原因の木 {} で、取り込まなければ通る局面の組が見つかりません",
        bad_names.join(" ")
    );
    log_keys(candidates.len(), &found);
    Ok(found)
}

// 入れ子の rayon 処理が待つ間も、木の試行の状態を持つワーカーは jobs 本までに抑える。
fn parallel<T: Sync, U: Send>(
    pool: &rayon::ThreadPool,
    jobs: usize,
    items: &[T],
    f: impl Fn(&T) -> U + Sync,
) -> Vec<U> {
    let next = AtomicUsize::new(0);
    let parts = pool.broadcast(|worker| {
        let mut results = Vec::new();
        if worker.index() >= jobs {
            return results;
        }
        loop {
            let i = next.fetch_add(1, Ordering::Relaxed);
            let Some(item) = items.get(i) else { break };
            results.push((i, f(item)));
        }
        results
    });
    let mut results: Vec<_> = parts.into_iter().flatten().collect();
    results.sort_by_key(|(i, _)| *i);
    results.into_iter().map(|(_, result)| result).collect()
}

fn log_keys(candidates: usize, found: &[String]) {
    eprintln!("  候補の局面 {candidates} から取り込まない局面 {} を足します", found.len());
    for key in found {
        eprintln!("    {key}");
    }
}

#[derive(Default)]
struct Stats {
    trials: usize,
    /// `trials` のうち軽い検査。
    rough: usize,
    others: usize,
    first_other: Option<String>,
    report_on_drop: bool,
}

impl Drop for Stats {
    fn drop(&mut self) {
        if self.report_on_drop {
            self.report();
        }
    }
}

impl Stats {
    /// 捨てるときに、途中の組が反転キー以外の理由で止まったことを報告する集計。
    fn reporting() -> Self {
        Self {
            report_on_drop: true,
            trials: 0,
            rough: 0,
            others: 0,
            first_other: None,
        }
    }

    fn check<T: Trial>(&mut self, trial: &T, trees: &[usize], skip: &BTreeSet<String>) -> Outcome {
        self.record(trial.failure(trees, skip))
    }

    /// 軽い検査。
    fn probe<T: Trial>(&mut self, trial: &T, trees: &[usize], skip: &BTreeSet<String>) -> Outcome {
        self.rough += 1;
        self.record(trial.rough_failure(trees, skip))
    }

    fn record(&mut self, result: Result<Option<String>>) -> Outcome {
        self.trials += 1;
        match result {
            Ok(None) => Outcome::Pass,
            Ok(Some(message)) => Outcome::Flip(message),
            Err(error) => {
                if self.first_other.is_none() {
                    self.first_other = Some(format!("{error:#}"));
                }
                self.others += 1;
                Outcome::Other(error)
            }
        }
    }

    fn report(&mut self) {
        if let Some(error) = self.first_other.take() {
            eprintln!(
                "  途中の組が反転キー以外の理由で止まりました (その組は通らない組とみます): {error}"
            );
        }
    }

    fn merge(&mut self, mut other: Self) {
        self.trials += other.trials;
        self.rough += other.rough;
        self.others += other.others;
        if self.first_other.is_none() {
            self.first_other = other.first_other.take();
        }
    }
}

/// 全部足せば通り、何も足さなければ通らない `items` から、足せば通る小さい部分を `width` 個ずつに分けて探す。
/// `first` は渡した組のうち通る最初の組の番号を返す (1 つも通らなければ `None`)。要素を足しても通らなくならない
/// (単調な) 条件なら 1 つ除くと通らなくなる組が返り、`first` の呼び出しは「返る要素の数 × 要素数の対数」程度で済む。
/// 単調でない条件では通らない組を返し得るので、呼ぶ側が結果を確かめる。
fn narrow<T: Clone>(
    items: &[T],
    width: usize,
    mut first: impl FnMut(&[Vec<T>]) -> Option<usize>,
) -> Vec<T> {
    fn go<T: Clone>(
        base: &[T],
        items: &[T],
        width: usize,
        first: &mut impl FnMut(&[Vec<T>]) -> Option<usize>,
    ) -> Vec<T> {
        if items.len() <= 1 {
            return items.to_vec();
        }
        let (len, count) = (items.len(), width.min(items.len()));
        let parts: Vec<&[T]> =
            (0..count).map(|i| &items[i * len / count..(i + 1) * len / count]).collect();
        let alone: Vec<Vec<T>> = parts.iter().map(|part| [base, part].concat()).collect();
        if let Some(i) = first(&alone) {
            return go(base, parts[i], width, first);
        }
        // どの部分だけでも足りないので、複数の部分から要る要素を拾う。後ろの部分から順に、前の部分の全部と、
        // 後ろの部分から拾った要素を足した上で絞る。
        let mut found = vec![Vec::new(); count];
        for i in (0..count).rev() {
            let mut others = [base, &parts[..i].concat()].concat();
            others.extend(found[i + 1..].iter().flatten().cloned());
            // 2 つに分けたときは、もう片方だけでは足りないと分かっている。
            if count > 2 && first(std::slice::from_ref(&others)).is_some() {
                continue;
            }
            found[i] = go(&others, parts[i], width, first);
        }
        found.concat()
    }
    go(&[], items, width, &mut first)
}

/// `ok(items)` が真のとき、`ok` が真のままの小さい部分 (1 つ除くと偽になる組) を delta debugging で探す。
fn ddmin<T: Clone>(items: &[T], mut ok: impl FnMut(&[T]) -> bool) -> Vec<T> {
    let mut current = items.to_vec();
    let mut n = 2;
    while current.len() >= 2 {
        let len = current.len();
        let parts: Vec<&[T]> = (0..n).map(|i| &current[i * len / n..(i + 1) * len / n]).collect();
        let mut next = None;
        for part in &parts {
            if ok(part) {
                next = Some((part.to_vec(), 2));
                break;
            }
        }
        // 2 つに分けたときの補集合はもう片方なので、確かめ直さない。
        if next.is_none() && n > 2 {
            for i in 0..n {
                let rest: Vec<T> = parts
                    .iter()
                    .enumerate()
                    .filter(|&(j, _)| j != i)
                    .flat_map(|(_, part)| part.iter().cloned())
                    .collect();
                if ok(&rest) {
                    next = Some((rest, (n - 1).max(2)));
                    break;
                }
            }
        }
        match next {
            Some((items, parts)) => {
                current = items;
                n = parts;
            }
            None if n >= len => break,
            None => n = (2 * n).min(len),
        }
    }
    current
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ddmin_finds_a_minimal_failing_pair() {
        let items: Vec<usize> = (0..20).collect();
        assert_eq!(ddmin(&items, |s| s.contains(&3) && s.contains(&17)), [3, 17]);
        assert_eq!(ddmin(&items, |s| s.contains(&9)), [9]);
    }

    #[test]
    fn narrow_finds_scattered_elements_with_few_trials() {
        let items: Vec<usize> = (0..300).collect();
        for needed in [vec![9], vec![3, 170], vec![0, 1, 150, 299]] {
            for width in [2, 3, 6] {
                let (mut calls, mut batches) = (0, 0);
                let found = narrow(&items, width, |sets| {
                    batches += 1;
                    sets.iter().position(|s| {
                        calls += 1;
                        needed.iter().all(|n| s.contains(n))
                    })
                });
                assert_eq!(found, needed, "幅 {width}");
                assert!(calls <= needed.len() * 20 * width / 2, "幅 {width} 試行 {calls}");
                if width == 6 {
                    assert!(batches <= needed.len() * 10, "幅 {width} 組 {batches}");
                }
            }
        }
    }

    /// 木 1 の局面 "a" と木 2 の局面 "c" が、取り込むと止まる局面。
    #[derive(Default)]
    struct Fake {
        calls: AtomicUsize,
        all_calls: AtomicUsize,
    }

    impl Trial for Fake {
        fn failure(&self, trees: &[usize], skip: &BTreeSet<String>) -> Result<Option<String>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if trees.len() == 4 {
                self.all_calls.fetch_add(1, Ordering::Relaxed);
            }
            let bad =
                trees.iter().flat_map(|&t| self.keys(&[t], skip)).any(|k| k == "a" || k == "c");
            Ok(bad.then(|| "止まる".into()))
        }

        fn keys(&self, trees: &[usize], skip: &BTreeSet<String>) -> BTreeSet<String> {
            let of = |t: usize| match t {
                0 => vec!["x"],
                1 => vec!["a", "b"],
                2 => vec!["b", "c", "d"],
                _ => vec![],
            };
            trees
                .iter()
                .flat_map(|&t| of(t))
                .filter(|k| !skip.contains(*k))
                .map(str::to_owned)
                .collect()
        }
    }

    #[test]
    fn search_falls_back_for_a_failing_pair() {
        struct Pair(Fake);
        impl Trial for Pair {
            fn failure(&self, trees: &[usize], skip: &BTreeSet<String>) -> Result<Option<String>> {
                Ok((trees.contains(&1) && trees.contains(&2) && !skip.contains("a"))
                    .then(|| "組み合わせで止まる".into()))
            }
            fn keys(&self, trees: &[usize], skip: &BTreeSet<String>) -> BTreeSet<String> {
                self.0.keys(trees, skip)
            }
        }
        let names = ["t0", "t1", "t2", "t3"].map(PathBuf::from);
        for jobs in [1, 4] {
            let found = search(&Pair(Fake::default()), &names, &BTreeSet::new(), 1, jobs).unwrap();
            assert_eq!(found.keys, ["a"]);
            assert!(found.passed);
            let limited =
                search(&Pair(Fake::default()), &names, &BTreeSet::new(), 0, jobs).unwrap();
            assert!(limited.keys.is_empty());
            assert!(!limited.passed);
            assert_eq!(limited.trials, 1);
        }
    }

    #[test]
    fn search_discards_independent_results_that_cannot_pass() {
        struct Stuck {
            removed: AtomicUsize,
            flip_when_removed: bool,
        }
        impl Trial for Stuck {
            fn failure(&self, trees: &[usize], skip: &BTreeSet<String>) -> Result<Option<String>> {
                if trees.is_empty() {
                    return Ok(None);
                }
                if skip.contains("x") {
                    self.removed.fetch_add(1, Ordering::Relaxed);
                    ensure!(self.flip_when_removed, "局面を除くと別の理由で止まる");
                }
                Ok(Some("止まる".into()))
            }
            fn keys(&self, trees: &[usize], skip: &BTreeSet<String>) -> BTreeSet<String> {
                Fake::default().keys(trees, skip)
            }
        }
        for jobs in [1, 4] {
            for flip_when_removed in [true, false] {
                let trial = Stuck {
                    removed: AtomicUsize::new(0),
                    flip_when_removed,
                };
                let error = search(&trial, &[PathBuf::from("t0")], &BTreeSet::new(), 1, jobs)
                    .unwrap_err()
                    .to_string();
                // 木ごとの段・全部の木の絞り込み・原因の木の組からの探索が、それぞれ局面を除いて確かめる。
                if flip_when_removed {
                    assert!(error.contains("局面を全部取り込まなくても"), "{error}");
                    assert_eq!(trial.removed.load(Ordering::Relaxed), 3);
                } else {
                    assert!(error.contains("通る局面の組が見つかりません"), "{error}");
                    assert_eq!(trial.removed.load(Ordering::Relaxed), 6);
                }
            }
        }
    }

    #[test]
    fn search_confirms_rough_results_with_the_exact_check() {
        /// 軽い検査だけが `rough` の答えを返す。
        struct Rough {
            fake: Fake,
            rough: Option<&'static str>,
        }
        impl Trial for Rough {
            fn failure(&self, trees: &[usize], skip: &BTreeSet<String>) -> Result<Option<String>> {
                self.fake.failure(trees, skip)
            }
            fn rough_failure(&self, _: &[usize], _: &BTreeSet<String>) -> Result<Option<String>> {
                Ok(self.rough.map(str::to_owned))
            }
            fn keys(&self, trees: &[usize], skip: &BTreeSet<String>) -> BTreeSet<String> {
                self.fake.keys(trees, skip)
            }
        }
        let names = ["t0", "t1", "t2", "t3"].map(PathBuf::from);
        for jobs in [1, 4] {
            // 軽い検査が止まらないと答えても、元の検査で止まる限り原因の木の組から探す。
            // 軽い検査が止まると答えても、元の検査で止まらない木の局面は除かない。
            for rough in [None, Some("止まる")] {
                let trial = Rough {
                    fake: Fake::default(),
                    rough,
                };
                let found = search(&trial, &names, &BTreeSet::new(), 10, jobs).unwrap();
                assert!(found.passed);
                assert_eq!(found.keys, ["a", "c"], "{rough:?}");
            }
        }
    }

    #[test]
    fn parallel_bounds_live_trials_with_nested_rayon() {
        use rayon::prelude::*;
        for jobs in [1, 4] {
            let pool = rayon::ThreadPoolBuilder::new().num_threads(8).build().unwrap();
            let live = AtomicUsize::new(0);
            let peak = AtomicUsize::new(0);
            let items: Vec<_> = (0..32).collect();
            let results = parallel(&pool, jobs, &items, |&t| {
                let active = live.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(active, Ordering::SeqCst);
                assert_eq!(rayon::current_num_threads(), 8);
                let sum: usize = (0..256).into_par_iter().map(|i| i + t).sum();
                live.fetch_sub(1, Ordering::SeqCst);
                sum
            });
            assert!(peak.load(Ordering::SeqCst) <= jobs);
            assert_eq!(live.load(Ordering::SeqCst), 0);
            assert_eq!(
                results,
                items.iter().map(|t| (0..256).map(|i| i + t).sum::<usize>()).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn parallel_stats_count_errors_and_keep_tree_order() {
        struct Errors;
        impl Trial for Errors {
            fn failure(&self, trees: &[usize], skip: &BTreeSet<String>) -> Result<Option<String>> {
                if trees[0] == 0 && skip.is_empty() {
                    Ok(Some("止まる".into()))
                } else {
                    anyhow::bail!("木 {}", trees[0]);
                }
            }
            fn keys(&self, _trees: &[usize], _skip: &BTreeSet<String>) -> BTreeSet<String> {
                BTreeSet::new()
            }
        }
        for jobs in [1, 4] {
            let pool = rayon::ThreadPoolBuilder::new().num_threads(jobs).build().unwrap();
            let locals = parallel(&pool, jobs, &[0, 1, 2, 3], |&t| {
                let mut stats = Stats::default();
                stats.check(&Errors, &[t], &BTreeSet::new());
                stats.check(&Errors, &[t], &BTreeSet::from(["x".into()]));
                stats
            });
            let mut stats = Stats::default();
            for local in locals {
                stats.merge(local);
            }
            assert_eq!((stats.trials, stats.others), (8, 7));
            assert_eq!(stats.first_other.as_deref(), Some("木 0"));
        }
    }

    #[test]
    fn search_adds_keys_until_every_tree_passes() {
        let names: Vec<PathBuf> = ["t0", "t1", "t2", "t3"].map(PathBuf::from).to_vec();
        let fake = Fake::default();
        let found = search(&fake, &names, &BTreeSet::new(), 10, 4).unwrap();
        assert_eq!(found.keys, ["a", "c"]);
        assert!(found.passed);
        assert_eq!(found.trials, fake.calls.load(Ordering::Relaxed));
        assert_eq!(fake.all_calls.load(Ordering::Relaxed), 2);
        let sequential = search(&Fake::default(), &names, &BTreeSet::new(), 10, 1).unwrap();
        assert_eq!(found, sequential);

        let found = search(&Fake::default(), &names, &BTreeSet::new(), 1, 1).unwrap();
        assert_eq!(found.keys, ["a", "c"]);
        assert!(found.passed);

        let skip = BTreeSet::from(["a".to_owned(), "c".to_owned()]);
        let found = search(&Fake::default(), &names, &skip, 10, 4).unwrap();
        assert_eq!((found.keys.len(), found.passed, found.trials), (0, true, 1));

        // 木 1 を除くと反転キー以外の理由で止まる元データでも、木 2 を原因の木とみて探し続ける。
        struct Fragile(Fake);
        impl Trial for Fragile {
            fn failure(&self, trees: &[usize], skip: &BTreeSet<String>) -> Result<Option<String>> {
                anyhow::ensure!(trees.contains(&1), "木 1 が無い");
                self.0.failure(trees, skip)
            }
            fn keys(&self, trees: &[usize], skip: &BTreeSet<String>) -> BTreeSet<String> {
                self.0.keys(trees, skip)
            }
        }
        let found = search(&Fragile(Fake::default()), &names, &BTreeSet::new(), 10, 4).unwrap();
        assert!(found.passed);
        assert_eq!(found.keys, ["a", "c"]);

        // 元データは "b" が無いと反転キー以外の理由で止まる。"a" と "b" を両方除くと止まるが、"a" だけ除けば通る。
        struct NeedsB(Fake);
        impl Trial for NeedsB {
            fn failure(&self, trees: &[usize], skip: &BTreeSet<String>) -> Result<Option<String>> {
                let keys = self.0.keys(trees, skip);
                anyhow::ensure!(keys.contains("b"), "b が無い");
                Ok(keys.contains("a").then(|| "止まる".into()))
            }
            fn keys(&self, trees: &[usize], skip: &BTreeSet<String>) -> BTreeSet<String> {
                self.0.keys(trees, skip)
            }
        }
        let found = search(&NeedsB(Fake::default()), &names, &BTreeSet::new(), 10, 4).unwrap();
        assert!(found.passed);
        assert_eq!(found.keys, ["a"]);
    }
}
