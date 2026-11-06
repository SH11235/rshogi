//! tournament の先後交換ペアの重複排除。

use std::collections::{BTreeMap, BTreeSet};

/// `(pair_index, attempt)` ごとに両スロットを揃える。重複スロットは最初の結果を保持する。
pub struct ResultPairs<T> {
    pending: BTreeMap<(u32, u32), [Option<T>; 2]>,
    completed: BTreeSet<(u32, u32)>,
}

impl<T> Default for ResultPairs<T> {
    fn default() -> Self {
        Self {
            pending: BTreeMap::new(),
            completed: BTreeSet::new(),
        }
    }
}

impl<T> ResultPairs<T> {
    /// 結果を受け取り、両スロットが揃ったときだけ返す。slot は 0 または 1。
    pub fn push(&mut self, key: (u32, u32), slot: usize, result: T) -> Option<[T; 2]> {
        if slot > 1 || self.completed.contains(&key) {
            eprintln!(
                "警告: pair_index={}, attempt={}, slot={slot} の重複または不正な結果を除外します",
                key.0, key.1
            );
            return None;
        }
        let entry = self.pending.entry(key).or_insert_with(|| [None, None]);
        if entry[slot].is_some() {
            eprintln!(
                "警告: pair_index={}, attempt={}, slot={slot} の重複結果を除外します",
                key.0, key.1
            );
            return None;
        }
        entry[slot] = Some(result);
        if entry.iter().any(Option::is_none) {
            return None;
        }
        let [Some(slot0), Some(slot1)] = self.pending.remove(&key)? else {
            return None;
        };
        self.completed.insert(key);
        Some([slot0, slot1])
    }

    /// 片スロットしかない未完了ペアの数。
    pub fn incomplete(&self) -> usize {
        self.pending.len()
    }
}
