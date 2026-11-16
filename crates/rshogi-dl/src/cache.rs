//! 基本形式の完全入力で照合する、容量固定の raw policy/value キャッシュ。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::mem::size_of;

use anyhow::{Result, ensure};
use rshogi_core::position::Position;

use crate::Evaluator;
use crate::features::{FEATURES1_SIZE, FEATURES2_SIZE, MAX_MOVE_LABEL_NUM, make_input_features};

const KEY_WORDS: usize = (FEATURES1_SIZE + FEATURES2_SIZE).div_ceil(64);
type Key = [u64; KEY_WORDS];

struct Entry {
    key: Key,
    logits: [f32; MAX_MOVE_LABEL_NUM],
    value: f32,
    occupied: bool,
}

struct Cache {
    entries: Vec<Entry>,
    first: Vec<f32>,
    second: Vec<f32>,
    bytes: usize,
}

impl Cache {
    fn new(bytes: usize) -> Result<Self> {
        let overhead = size_of::<Self>() + (FEATURES1_SIZE + FEATURES2_SIZE) * size_of::<f32>();
        ensure!(bytes > overhead, "cache budget is too small");
        let count = (bytes - overhead) / size_of::<Entry>();
        ensure!(count > 0, "cache has no entries");
        let mut entries = Vec::new();
        entries.try_reserve_exact(count)?;
        entries.resize_with(count, || Entry {
            key: [0; KEY_WORDS],
            logits: [0.0; MAX_MOVE_LABEL_NUM],
            value: 0.0,
            occupied: false,
        });
        let mut first = Vec::new();
        first.try_reserve_exact(FEATURES1_SIZE)?;
        first.resize(FEATURES1_SIZE, 0.0);
        let mut second = Vec::new();
        second.try_reserve_exact(FEATURES2_SIZE)?;
        second.resize(FEATURES2_SIZE, 0.0);
        let allocated = size_of::<Self>()
            + entries.capacity() * size_of::<Entry>()
            + (first.capacity() + second.capacity()) * size_of::<f32>();
        ensure!(allocated <= bytes, "cache allocation exceeds budget");
        Ok(Self {
            entries,
            first,
            second,
            bytes: allocated,
        })
    }

    fn key(&mut self, pos: &Position) -> Key {
        self.first.fill(0.0);
        self.second.fill(0.0);
        make_input_features(pos, &mut self.first, &mut self.second);
        let mut key = [0; KEY_WORDS];
        for (index, &value) in self.first.iter().chain(&self.second).enumerate() {
            // 基本形式の全 plane は 0/1。全要素を保持し、手数や履歴を key に混ぜない。
            debug_assert!(value == 0.0 || value == 1.0);
            if value != 0.0 {
                key[index / 64] |= 1 << (index % 64);
            }
        }
        key
    }

    fn index(&self, key: &Key) -> usize {
        let mut hash = DefaultHasher::new();
        key.hash(&mut hash);
        (hash.finish() % self.entries.len() as u64) as usize
    }
}

/// 最後の reset 以降の評価要求と、実際の backend 呼び出し。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    /// キャッシュを含む評価要求数。
    pub requests: u64,
    /// backend を呼んだ数。失敗した呼び出しも含む。
    pub nn_calls: u64,
    /// キャッシュから返した数。
    pub hits: u64,
}

/// 基本入力だけに依存する決定的な evaluator を所有し、生の出力を再利用する。
/// モデル・provider を変えるときは新しく構築し、対局境界では clear を呼ぶ。
pub struct CachedEvaluator<E> {
    inner: E,
    cache: Option<Box<Cache>>,
    stats: CacheStats,
}

impl<E: Evaluator> CachedEvaluator<E> {
    /// 上限 MiB を指定する。0 は特徴量作成もせず backend へ委譲する。
    pub fn new(inner: E, mib: usize) -> Result<Self> {
        ensure!(mib <= 1024, "NN cache must be 0..=1024 MiB");
        let cache = if mib == 0 {
            None
        } else {
            Some(Box::new(Cache::new(mib * 1024 * 1024)?))
        };
        Ok(Self {
            inner,
            cache,
            stats: CacheStats::default(),
        })
    }

    /// 保存済みの出力と統計を破棄する。モデル自体は保持する。
    pub fn clear(&mut self) {
        if let Some(cache) = &mut self.cache {
            for entry in &mut cache.entries {
                entry.occupied = false;
            }
        }
        self.reset_stats();
    }

    /// 保存済みの出力を保ち、計数だけをリセットする。
    pub fn reset_stats(&mut self) {
        self.stats = CacheStats::default();
    }

    /// 最後の reset 以降の統計。
    pub fn stats(&self) -> CacheStats {
        self.stats
    }

    /// key・出力・metadata・特徴量 buffer の合計 bytes。allocator の管理領域は含まない。
    pub fn allocated_bytes(&self) -> usize {
        self.cache.as_ref().map_or(0, |cache| cache.bytes)
    }
}

impl<E: Evaluator> Evaluator for CachedEvaluator<E> {
    fn evaluate(&mut self, pos: &Position, logits: &mut [f32]) -> Result<f32> {
        self.stats.requests += 1;
        let Some(cache) = &mut self.cache else {
            self.stats.nn_calls += 1;
            return self.inner.evaluate(pos, logits);
        };
        ensure!(logits.len() == MAX_MOVE_LABEL_NUM, "incorrect policy buffer length");
        let key = cache.key(pos);
        let index = cache.index(&key);
        let entry = &cache.entries[index];
        if entry.occupied && entry.key == key {
            logits.copy_from_slice(&entry.logits);
            self.stats.hits += 1;
            return Ok(entry.value);
        }
        self.stats.nn_calls += 1;
        let value = self.inner.evaluate(pos, logits)?;
        ensure!(value.is_finite() && (0.0..=1.0).contains(&value), "invalid value probability");
        ensure!(logits.iter().all(|x| x.is_finite()), "non-finite policy logit");
        let entry = &mut cache.entries[index];
        entry.key = key;
        entry.logits.copy_from_slice(logits);
        entry.value = value;
        entry.occupied = true;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        calls: usize,
        fail: bool,
    }
    impl Evaluator for Fake {
        fn evaluate(&mut self, pos: &Position, logits: &mut [f32]) -> Result<f32> {
            self.calls += 1;
            ensure!(!self.fail, "backend failed");
            logits.fill(if pos.in_check() { 1.0 } else { 0.0 });
            Ok(0.25)
        }
    }
    fn position() -> Position {
        let mut p = Position::new();
        p.set_hirate();
        p
    }

    #[test]
    fn off_delegates_every_request() -> Result<()> {
        let mut evaluator = CachedEvaluator::new(
            Fake {
                calls: 0,
                fail: false,
            },
            0,
        )?;
        let mut logits = [0.0; MAX_MOVE_LABEL_NUM];
        for _ in 0..2 {
            assert_eq!(evaluator.evaluate(&position(), &mut logits)?, 0.25);
        }
        assert_eq!(
            evaluator.stats(),
            CacheStats {
                requests: 2,
                nn_calls: 2,
                hits: 0
            }
        );
        assert_eq!(evaluator.allocated_bytes(), 0);
        Ok(())
    }

    #[test]
    fn exact_input_hit_clear_and_owned_model() -> Result<()> {
        let mut evaluator = CachedEvaluator::new(
            Fake {
                calls: 0,
                fail: false,
            },
            1,
        )?;
        let mut logits = [0.0; MAX_MOVE_LABEL_NUM];
        evaluator.evaluate(&position(), &mut logits)?;
        let first = logits;
        evaluator.evaluate(&position(), &mut logits)?;
        assert_eq!(logits, first);
        assert_eq!(
            evaluator.stats(),
            CacheStats {
                requests: 2,
                nn_calls: 1,
                hits: 1
            }
        );
        evaluator.clear();
        evaluator.evaluate(&position(), &mut logits)?;
        assert_eq!(evaluator.stats().nn_calls, 1);
        assert_eq!(evaluator.stats().hits, 0);
        assert_eq!(evaluator.inner.calls, 2);
        assert!(evaluator.allocated_bytes() <= 1024 * 1024);
        Ok(())
    }

    #[test]
    fn slot_collision_is_checked_against_full_key() -> Result<()> {
        let mut evaluator = CachedEvaluator::new(
            Fake {
                calls: 0,
                fail: false,
            },
            1,
        )?;
        let pos = position();
        let cache = evaluator.cache.as_mut().unwrap();
        let key = cache.key(&pos);
        let index = cache.index(&key);
        cache.entries[index].occupied = true;
        cache.entries[index].key = key;
        cache.entries[index].key[0] ^= 1;
        cache.entries[index].value = 0.75;
        let mut logits = [0.0; MAX_MOVE_LABEL_NUM];
        assert_eq!(evaluator.evaluate(&pos, &mut logits)?, 0.25);
        assert_eq!(evaluator.stats().nn_calls, 1);
        Ok(())
    }

    #[test]
    fn failed_evaluation_is_not_cached() -> Result<()> {
        let mut evaluator = CachedEvaluator::new(
            Fake {
                calls: 0,
                fail: true,
            },
            1,
        )?;
        let mut logits = [0.0; MAX_MOVE_LABEL_NUM];
        for _ in 0..2 {
            assert!(evaluator.evaluate(&position(), &mut logits).is_err());
        }
        assert_eq!(evaluator.stats().nn_calls, 2);
        assert_eq!(evaluator.stats().hits, 0);
        Ok(())
    }
    #[test]
    fn input_key_ignores_ply_and_checks_every_plane() -> Result<()> {
        let mut cache = Cache::new(1024 * 1024)?;
        let pos = position();
        let key = cache.key(&pos);
        let mut later = Position::new();
        let sfen = pos.to_sfen();
        let mut parts: Vec<_> = sfen.split_whitespace().collect();
        parts[3] = "200";
        later.set_sfen(&parts.join(" "))?;
        assert_eq!(cache.key(&later), key);
        let first = cache.first.clone();
        let second = cache.second.clone();
        for (i, &x) in first.iter().chain(&second).enumerate() {
            assert_eq!((key[i / 64] >> (i % 64)) & 1, u64::from(x == 1.0));
        }
        later.set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 2")?;
        assert_ne!(cache.key(&later), key);
        Ok(())
    }
}
