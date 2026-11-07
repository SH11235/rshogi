//! 頻度の高い相手の応手を含めた、定跡の継続手数。
use crate::book_db::{BookDb, apply_usi_move, position_from_sfen, strip_ply};
use anyhow::Result;
use clap::Args;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

#[derive(Args, Debug)]
pub(super) struct DeepArgs {
    /// 継続手数の判定に使う相手の頻度定跡 (YaneuraOu 形式 .db)
    #[arg(long)]
    pub deep_opp_freq: Option<PathBuf>,
    /// 継続手数で辿る相手の手の局数の下限
    #[arg(long, default_value_t = 3)]
    pub deep_min_games: u64,
}

pub(super) struct Continuation<'a, F> {
    pub freq: &'a BookDb,
    pub min_games: u64,
    pub main: F,
}

#[derive(Clone)]
struct Cached {
    depth: usize,
    visited: BTreeSet<String>,
}

impl<F: Fn(&str) -> Option<String>> Continuation<'_, F> {
    pub fn after(&self, key: &str, mv: &str, limit: usize) -> Result<usize> {
        let mut pos = position_from_sfen(&format!("{key} 1"))?;
        if apply_usi_move(&mut pos, mv).is_err() {
            return Ok(0);
        }
        let own = key.split_whitespace().nth(1) == Some("w");
        let mut path = BTreeSet::from([key.to_owned()]);
        let mut memo = BTreeMap::new();
        Ok(self.depth(strip_ply(&pos.to_sfen()), own, limit, &mut path, &mut memo)?.depth)
    }

    fn depth(
        &self,
        key: &str,
        own: bool,
        remaining: usize,
        path: &mut BTreeSet<String>,
        memo: &mut BTreeMap<(String, usize), Cached>,
    ) -> Result<Cached> {
        let mut result = Cached {
            depth: 0,
            visited: BTreeSet::new(),
        };
        if remaining == 0 {
            return Ok(result);
        }
        result.visited.insert(key.to_owned());
        if path.contains(key) {
            return Ok(result);
        }
        let cache_key = (key.to_owned(), remaining);
        // 継続手数は祖先との循環に依存するため、探索した局面が現在の経路と重なるメモは使わない。
        if let Some(cached) = memo.get(&cache_key)
            && cached.visited.is_disjoint(path)
        {
            return Ok(cached.clone());
        }
        let mut moves = BTreeSet::new();
        if let Some(main) = (self.main)(key) {
            moves.insert(main);
        }
        if (key.split_whitespace().nth(1) == Some("w")) != own
            && let Some(hit) = self.freq.find(key)
        {
            for m in &self.freq.entries[&hit.key].moves {
                if m.count >= self.min_games
                    && let Some(mv) = &m.move_usi
                {
                    let mv = if hit.flipped {
                        rshogi_book::flip_usi_move(mv)
                    } else {
                        Some(mv.clone())
                    };
                    if let Some(mv) = mv {
                        moves.insert(mv);
                    }
                }
            }
        }
        path.insert(key.to_owned());
        let pos = position_from_sfen(&format!("{key} 1"))?;
        let mut shortest = remaining;
        for mv in &moves {
            let mut child = pos.clone();
            if apply_usi_move(&mut child, mv).is_err() {
                shortest = 0;
                break;
            }
            let next = self.depth(strip_ply(&child.to_sfen()), own, remaining - 1, path, memo)?;
            shortest = shortest.min(1 + next.depth);
            result.visited.extend(next.visited);
            if shortest == 1 {
                break;
            }
        }
        path.remove(key);
        result.depth = if moves.is_empty() { 0 } else { shortest };
        // 祖先への循環で打ち切った値は、別の経路では短すぎるので保存しない。
        if result.visited.is_disjoint(path) {
            memo.insert(cache_key, result.clone());
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn memo_does_not_reuse_a_continuation_stopped_by_an_external_ancestor() {
        let a = "4k4/9/9/9/9/9/9/5K3/9 w -";
        let mut pos = position_from_sfen(&format!("{a} 1")).unwrap();
        apply_usi_move(&mut pos, "5a5b").unwrap();
        let b = strip_ply(&pos.to_sfen()).to_owned();
        let mains = BTreeMap::from([(a.to_owned(), "5a5b"), (b.clone(), "4h4g")]);
        let calls = Cell::new(0);
        let freq = BookDb::default();
        let continuation = Continuation {
            freq: &freq,
            min_games: 3,
            main: |key: &str| {
                calls.set(calls.get() + 1);
                mains.get(key).map(|m| (*m).to_owned())
            },
        };
        let mut memo = BTreeMap::new();
        let mut path = BTreeSet::from([b]);
        assert_eq!(continuation.depth(a, false, 3, &mut path, &mut memo).unwrap().depth, 1);
        path.clear();
        assert_eq!(continuation.depth(a, false, 3, &mut path, &mut memo).unwrap().depth, 2);
        let before = calls.get();
        assert_eq!(continuation.depth(a, false, 3, &mut path, &mut memo).unwrap().depth, 2);
        assert_eq!(calls.get(), before);
    }
}
