//! 登録した元データが反転キーで合流した局面で伝播を止めるとき、原因の木とその木の局面を絞り、取り込まない局面を
//! 足して通るまで繰り返す。

use anyhow::{Result, ensure};
use std::collections::BTreeSet;
use std::path::PathBuf;
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
pub(super) trait Trial {
    fn failure(&mut self, trees: &[usize], skip: &BTreeSet<String>) -> Result<Option<String>>;
    /// その木が取り込む局面のうち、`skip` で除かれていないもの。
    fn keys(&self, trees: &[usize], skip: &BTreeSet<String>) -> BTreeSet<String>;
}

/// 1 回の試行の結果。
enum Outcome {
    Pass,
    Flip(String),
    /// 反転キー以外の理由で止まった。
    Other(anyhow::Error),
}

/// 全部の木で検査し、止まるなら原因の木の組 (1 本除くと止まらなくなる組) を、次にその組だけで通るようになる
/// 局面の組 (1 つ除くと止まる組) を探して `skip` に足す。足した後の全部の木でもう一度検査し、`rounds` 回まで
/// 繰り返す。木や局面を減らした途中の組が反転キー以外の理由で止まったときは、その組は反転キーで止まらない、
/// 通らない組として探し続ける。
pub(super) fn search<T: Trial>(
    trial: &mut T,
    names: &[PathBuf],
    skip: &BTreeSet<String>,
    rounds: usize,
) -> Result<Found> {
    let started = Instant::now();
    let all: Vec<usize> = (0..names.len()).collect();
    let mut skip = skip.clone();
    let mut keys = Vec::new();
    let mut trials = 0;
    let mut others = 0;
    let mut check = |trial: &mut T, trees: &[usize], skip: &BTreeSet<String>| {
        trials += 1;
        match trial.failure(trees, skip) {
            Ok(None) => Outcome::Pass,
            Ok(Some(message)) => Outcome::Flip(message),
            Err(error) => {
                if others == 0 {
                    eprintln!(
                        "  途中の組が反転キー以外の理由で止まりました (その組は通らない組とみます): {error:#}"
                    );
                }
                others += 1;
                Outcome::Other(error)
            }
        }
    };
    let mut round = 0;
    let passed = loop {
        let message = match check(trial, &all, &skip) {
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
        if round == 0 {
            ensure!(
                !matches!(check(trial, &[], &skip), Outcome::Flip(_)),
                "木を取り込まない元データだけで反転キーの検査で止まります"
            );
        }
        round += 1;
        let bad = ddmin(&all, |trees| Ok(matches!(check(trial, trees, &skip), Outcome::Flip(_))))?;
        let bad_names: Vec<_> = bad.iter().map(|&t| names[t].display().to_string()).collect();
        eprintln!("  原因の木: {}", bad_names.join(" "));
        let candidates: Vec<String> = trial.keys(&bad, &skip).into_iter().collect();
        let with = |extra: &[String]| -> BTreeSet<String> {
            skip.iter().cloned().chain(extra.iter().cloned()).collect()
        };
        ensure!(
            matches!(check(trial, &bad, &with(&candidates)), Outcome::Pass),
            "原因の木 {} の局面を全部取り込まなくても通りません",
            bad_names.join(" ")
        );
        let found = ddmin(&candidates, |extra| {
            Ok(matches!(check(trial, &bad, &with(extra)), Outcome::Pass))
        })?;
        eprintln!(
            "  候補の局面 {} から取り込まない局面 {} を足します",
            candidates.len(),
            found.len()
        );
        for key in &found {
            eprintln!("    {key}");
        }
        skip.extend(found.iter().cloned());
        keys.extend(found);
    };
    eprintln!(
        "反転キーの検査: {} / 足した局面 {} / 試行 {trials} (うち反転キー以外の理由で止まった組 {others}) / {:.1} 秒",
        if passed {
            "通りました"
        } else {
            "上限の回数で通りません"
        },
        keys.len(),
        started.elapsed().as_secs_f64()
    );
    Ok(Found {
        keys,
        passed,
        trials,
    })
}

/// `ok(items)` が真のとき、`ok` が真のままの小さい部分 (1 つ除くと偽になる組) を delta debugging で探す。
fn ddmin<T: Clone>(items: &[T], mut ok: impl FnMut(&[T]) -> Result<bool>) -> Result<Vec<T>> {
    let mut current = items.to_vec();
    let mut n = 2;
    while current.len() >= 2 {
        let len = current.len();
        let parts: Vec<&[T]> = (0..n).map(|i| &current[i * len / n..(i + 1) * len / n]).collect();
        let mut next = None;
        for part in &parts {
            if ok(part)? {
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
                if ok(&rest)? {
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
    Ok(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ddmin_finds_a_minimal_failing_pair() {
        let items: Vec<usize> = (0..20).collect();
        let found = ddmin(&items, |s| Ok(s.contains(&3) && s.contains(&17))).unwrap();
        assert_eq!(found, [3, 17]);
        assert_eq!(ddmin(&items, |s| Ok(s.contains(&9))).unwrap(), [9]);
    }

    /// 木 1 の局面 "a" と木 2 の局面 "c" が、取り込むと止まる局面。
    struct Fake {
        calls: usize,
    }

    impl Trial for Fake {
        fn failure(&mut self, trees: &[usize], skip: &BTreeSet<String>) -> Result<Option<String>> {
            self.calls += 1;
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
    fn search_adds_keys_until_every_tree_passes() {
        let names: Vec<PathBuf> = ["t0", "t1", "t2", "t3"].map(PathBuf::from).to_vec();
        let mut fake = Fake { calls: 0 };
        let found = search(&mut fake, &names, &BTreeSet::new(), 10).unwrap();
        assert_eq!(found.keys, ["a", "c"]);
        assert!(found.passed);
        assert_eq!(found.trials, fake.calls);

        let found = search(&mut Fake { calls: 0 }, &names, &BTreeSet::new(), 1).unwrap();
        assert_eq!(found.keys, ["a"]);
        assert!(!found.passed);

        let skip = BTreeSet::from(["a".to_owned(), "c".to_owned()]);
        let found = search(&mut Fake { calls: 0 }, &names, &skip, 10).unwrap();
        assert_eq!((found.keys.len(), found.passed, found.trials), (0, true, 1));

        // 木 1 を除くと反転キー以外の理由で止まる元データでも、木 2 を原因の木とみて探し続ける。
        struct Fragile(Fake);
        impl Trial for Fragile {
            fn failure(
                &mut self,
                trees: &[usize],
                skip: &BTreeSet<String>,
            ) -> Result<Option<String>> {
                anyhow::ensure!(trees.contains(&1), "木 1 が無い");
                self.0.failure(trees, skip)
            }
            fn keys(&self, trees: &[usize], skip: &BTreeSet<String>) -> BTreeSet<String> {
                self.0.keys(trees, skip)
            }
        }
        let found = search(&mut Fragile(Fake { calls: 0 }), &names, &BTreeSet::new(), 10).unwrap();
        assert!(found.passed);
        assert_eq!(found.keys, ["a", "c"]);
    }
}
