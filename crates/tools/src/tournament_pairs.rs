//! tournament の先後交換ペアの重複排除。

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

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
    /// path は重複・不正スロットの警告に表示する入力ファイル。
    pub fn push(&mut self, path: &Path, key: (u32, u32), slot: usize, result: T) -> Option<[T; 2]> {
        if slot > 1 {
            eprintln!("警告: {} — 不正な slot={slot} の結果を除外します。", path.display());
            return None;
        }
        if self.completed.contains(&key) {
            eprintln!(
                "警告: {} — 直接対決の pair_index={}, attempt={}, slot={slot} は既に集計済みです。重複結果を除外します。",
                path.display(),
                key.0,
                key.1
            );
            return None;
        }
        let entry = self.pending.entry(key).or_insert_with(|| [None, None]);
        if entry[slot].is_some() {
            eprintln!(
                "警告: {} — 直接対決の pair_index={}, attempt={}, slot={slot} が重複しています。重複結果を除外します。",
                path.display(),
                key.0,
                key.1
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
