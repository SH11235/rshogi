//! 宣言された形式の完全入力で照合する、容量固定の raw policy/value キャッシュ。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::mem::size_of;

use anyhow::{Result, ensure};
use rshogi_core::position::Position;

use crate::features::{FEATURES1_SIZE, FEATURES2_SIZE, MAX_MOVE_LABEL_NUM, make_input_features};
use crate::{Evaluator, InputFormat, tokens};

const KEY_WORDS: usize = (FEATURES1_SIZE + FEATURES2_SIZE).div_ceil(64);
type Key = [u64; KEY_WORDS];
const TOKEN_TAG_WORD: usize = 150;
const TOKEN_TAG: u64 = 1 << 63;
const _: () = assert!(KEY_WORDS == 151);
const _: () = assert!(FEATURES1_SIZE + FEATURES2_SIZE <= TOKEN_TAG_WORD * 64 + 63);
const _: () = assert!(tokens::TOKEN_COUNT == 12 * 8);

struct Entry {
    key: Key,
    logits: [f32; MAX_MOVE_LABEL_NUM],
    value: f32,
    occupied: bool,
}

struct Cache {
    format: InputFormat,
    entries: Vec<Entry>,
    first: Vec<f32>,
    second: Vec<f32>,
    bytes: usize,
}

impl Cache {
    fn new(bytes: usize, format: InputFormat) -> Result<Self> {
        let (first_len, second_len) = match format {
            InputFormat::Basic => (FEATURES1_SIZE, FEATURES2_SIZE),
            InputFormat::StmTokensV1 => (0, 0),
        };
        let overhead = size_of::<Self>() + (first_len + second_len) * size_of::<f32>();
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
        first.try_reserve_exact(first_len)?;
        first.resize(first_len, 0.0);
        let mut second = Vec::new();
        second.try_reserve_exact(second_len)?;
        second.resize(second_len, 0.0);
        let allocated = size_of::<Self>()
            + entries.capacity() * size_of::<Entry>()
            + (first.capacity() + second.capacity()) * size_of::<f32>();
        ensure!(allocated <= bytes, "cache allocation exceeds budget");
        Ok(Self {
            format,
            entries,
            first,
            second,
            bytes: allocated,
        })
    }

    fn key(&mut self, pos: &Position) -> Result<Key> {
        if self.format == InputFormat::StmTokensV1 {
            let encoded = tokens::encode(pos)?;
            let mut key = [0; KEY_WORDS];
            for (index, token) in encoded.into_iter().enumerate() {
                key[index / 8] |= u64::from(token) << ((index % 8) * 8);
            }
            key[TOKEN_TAG_WORD] = TOKEN_TAG;
            return Ok(key);
        }
        self.first.fill(0.0);
        self.second.fill(0.0);
        make_input_features(pos, &mut self.first, &mut self.second);
        let mut key = [0; KEY_WORDS];
        for (index, &value) in self.first.iter().chain(&self.second).enumerate() {
            // 基本形式の全 plane は 0/1。全要素を保持し、手数や履歴を key に混ぜない。
            match value.to_bits() {
                0 => {}
                0x3f800000 => key[index / 64] |= 1 << (index % 64),
                _ => anyhow::bail!("cache requires binary basic inputs"),
            }
        }
        Ok(key)
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

/// 宣言した入力だけに依存する決定的な evaluator を所有し、生の出力を再利用する。
/// モデル・provider を変えるときは新しく構築し、対局境界では clear を呼ぶ。
pub struct CachedEvaluator<E> {
    inner: E,
    cache: Option<Box<Cache>>,
    stats: CacheStats,
}

impl<E: Evaluator> CachedEvaluator<E> {
    /// 上限 MiB を指定する。0 は特徴量作成もせず backend へ委譲する。
    /// 0 以外では、backend が対応する入力形式を宣言している必要がある。
    pub fn new(inner: E, mib: usize) -> Result<Self> {
        ensure!(mib <= 1024, "NN cache must be 0..=1024 MiB");
        let cache = if mib == 0 {
            None
        } else {
            let format = inner
                .input_format()
                .ok_or_else(|| anyhow::anyhow!("NN cache requires a supported input format"))?;
            Some(Box::new(Cache::new(mib * 1024 * 1024, format)?))
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
    fn input_format(&self) -> Option<InputFormat> {
        self.inner.input_format()
    }

    fn evaluate(&mut self, pos: &Position, logits: &mut [f32]) -> Result<f32> {
        self.stats.requests += 1;
        let Some(cache) = &mut self.cache else {
            self.stats.nn_calls += 1;
            return self.inner.evaluate(pos, logits);
        };
        ensure!(logits.len() == MAX_MOVE_LABEL_NUM, "incorrect policy buffer length");
        let key = cache.key(pos)?;
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
        fn input_format(&self) -> Option<InputFormat> {
            Some(InputFormat::Basic)
        }

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
        let key = cache.key(&pos)?;
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
        let mut cache = Cache::new(1024 * 1024, InputFormat::Basic)?;
        let pos = position();
        let key = cache.key(&pos)?;
        let mut later = Position::new();
        let sfen = pos.to_sfen();
        let mut parts: Vec<_> = sfen.split_whitespace().collect();
        parts[3] = "200";
        later.set_sfen(&parts.join(" "))?;
        assert_eq!(cache.key(&later)?, key);
        let first = cache.first.clone();
        let second = cache.second.clone();
        for (i, &x) in first.iter().chain(&second).enumerate() {
            assert_eq!((key[i / 64] >> (i % 64)) & 1, u64::from(x == 1.0));
        }
        later.set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 2")?;
        assert_ne!(cache.key(&later)?, key);
        Ok(())
    }
    #[test]
    fn distinct_inputs_return_their_own_policy() -> Result<()> {
        let mut evaluator = CachedEvaluator::new(
            Fake {
                calls: 0,
                fail: false,
            },
            1,
        )?;
        let mut checked = Position::new();
        checked.set_sfen("4k4/4R4/9/9/9/9/9/9/K8 w - 1")?;
        assert!(checked.in_check());
        let mut logits = [0.0; MAX_MOVE_LABEL_NUM];
        for (pos, expected) in [(position(), 0.0), (checked, 1.0), (position(), 0.0)] {
            evaluator.evaluate(&pos, &mut logits)?;
            assert!(logits.iter().all(|&x| x == expected));
        }
        assert_eq!(evaluator.stats().nn_calls, 2);
        assert_eq!(evaluator.stats().hits, 1);
        Ok(())
    }

    #[test]
    fn hand_and_attack_planes_are_binary_and_preserved() -> Result<()> {
        let mut pos = Position::new();
        let mut cache = Cache::new(1024 * 1024, InputFormat::Basic)?;
        for sfen in [
            "4k4/9/9/9/9/9/9/9/4K4 b 18P4L4N4S4G2B2R 1",
            "+B+N4R+Pl/KBG2g2l/P+P2P+N3/6p2/2p3s2/4S2L1/G8/7p+l/7+pk b G2S4Pr2n6p 1",
        ] {
            pos.set_sfen(sfen)?;
            let key = cache.key(&pos)?;
            for (index, &x) in cache.first.iter().chain(&cache.second).enumerate() {
                assert!(x == 0.0 || x == 1.0);
                assert_eq!((key[index / 64] >> (index % 64)) & 1, u64::from(x == 1.0));
            }
        }
        Ok(())
    }

    struct TokenFake(Fake);
    impl Evaluator for TokenFake {
        fn input_format(&self) -> Option<InputFormat> {
            Some(InputFormat::StmTokensV1)
        }

        fn evaluate(&mut self, pos: &Position, logits: &mut [f32]) -> Result<f32> {
            self.0.evaluate(pos, logits)
        }
    }

    #[test]
    fn unknown_format_requires_cache_off_and_is_forwarded() -> Result<()> {
        struct Unknown;
        impl Evaluator for Unknown {
            fn evaluate(&mut self, _: &Position, logits: &mut [f32]) -> Result<f32> {
                logits.fill(0.0);
                Ok(0.5)
            }
        }
        assert!(CachedEvaluator::new(Unknown, 1).is_err());
        let mut disabled = CachedEvaluator::new(Unknown, 0)?;
        assert_eq!(disabled.input_format(), None);
        let mut logits = [];
        assert_eq!(disabled.evaluate(&Position::new(), &mut logits)?, 0.5);
        assert_eq!(disabled.allocated_bytes(), 0);
        assert_eq!(disabled.stats().nn_calls, 1);
        Ok(())
    }

    #[test]
    fn token_key_contains_every_byte_and_only_the_reserved_tag() -> Result<()> {
        let mut cache = Cache::new(1024 * 1024, InputFormat::StmTokensV1)?;
        let pos = position();
        let encoded = tokens::encode(&pos)?;
        let key = cache.key(&pos)?;
        for (index, token) in encoded.into_iter().enumerate() {
            assert_eq!(((key[index / 8] >> ((index % 8) * 8)) & 255) as u8, token);
        }
        assert!(key[12..TOKEN_TAG_WORD].iter().all(|&word| word == 0));
        assert_eq!(key[TOKEN_TAG_WORD], TOKEN_TAG);
        assert!(cache.first.is_empty() && cache.second.is_empty());
        let mut basic = Cache::new(1024 * 1024, InputFormat::Basic)?;
        assert_eq!(basic.key(&pos)?[TOKEN_TAG_WORD] & TOKEN_TAG, 0);
        Ok(())
    }

    #[test]
    fn basic_key_and_hash_match_kings_fixture() -> Result<()> {
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/9/9/9/9/9/9/4K4 b - 1")?;
        let mut expected = [0; KEY_WORDS];
        for (word, bits) in [
            (9, 0x800000000),
            (27, 0x6010180),
            (35, 0xc000000000000000),
            (36, 0x30080),
            (48, 0x40000000000),
            (66, 0x602018000),
            (75, 0x30100c0),
        ] {
            expected[word] = bits;
        }
        let mut cache = Cache::new(1024 * 1024, InputFormat::Basic)?;
        let key = cache.key(&pos)?;
        assert_eq!(key, expected);
        let mut hash = DefaultHasher::new();
        key.hash(&mut hash);
        assert_eq!(hash.finish(), 2_955_507_323_088_843_908);
        assert_eq!(
            cache.index(&key),
            (2_955_507_323_088_843_908u64 % cache.entries.len() as u64) as usize
        );
        Ok(())
    }

    #[test]
    fn exact_pawn_counts_do_not_alias_saturated_basic_planes() -> Result<()> {
        let mut basic = Cache::new(1024 * 1024, InputFormat::Basic)?;
        let mut token_cache = Cache::new(1024 * 1024, InputFormat::StmTokensV1)?;
        let mut basic_keys = Vec::new();
        let mut token_keys = Vec::new();
        for count in [8, 9, 18] {
            let mut pos = Position::new();
            pos.set_sfen(&format!("4k4/9/9/9/9/9/9/9/4K4 b {count}P 1"))?;
            basic_keys.push(basic.key(&pos)?);
            token_keys.push(token_cache.key(&pos)?);
        }
        assert_eq!(basic_keys[0], basic_keys[1]);
        assert_eq!(basic_keys[1], basic_keys[2]);
        assert_ne!(token_keys[0], token_keys[1]);
        assert_ne!(token_keys[0], token_keys[2]);
        assert_ne!(token_keys[1], token_keys[2]);
        Ok(())
    }

    #[test]
    fn original_side_changes_token_key_after_canonicalization() -> Result<()> {
        let mut black = Position::new();
        black.set_hirate();
        let mut white = Position::new();
        white.set_sfen(&black.to_sfen().replace(" b ", " w "))?;
        let mut cache = Cache::new(1024 * 1024, InputFormat::StmTokensV1)?;
        let black_key = cache.key(&black)?;
        let white_key = cache.key(&white)?;
        assert_eq!(&black_key[..11], &white_key[..11]);
        assert_eq!(black_key[11] & 0x00ffffffffffffff, white_key[11] & 0x00ffffffffffffff);
        assert_ne!(black_key, white_key);
        Ok(())
    }

    #[test]
    fn token_hits_clear_failures_and_format_forwarding() -> Result<()> {
        let mut evaluator = CachedEvaluator::new(
            TokenFake(Fake {
                calls: 0,
                fail: false,
            }),
            1,
        )?;
        assert_eq!(evaluator.input_format(), Some(InputFormat::StmTokensV1));
        let mut logits = [0.0; MAX_MOVE_LABEL_NUM];
        evaluator.evaluate(&position(), &mut logits)?;
        evaluator.evaluate(&position(), &mut logits)?;
        assert_eq!(evaluator.stats().nn_calls, 1);
        assert_eq!(evaluator.stats().hits, 1);
        evaluator.clear();
        evaluator.inner.0.fail = true;
        for _ in 0..2 {
            assert!(evaluator.evaluate(&position(), &mut logits).is_err());
        }
        assert_eq!(evaluator.stats().nn_calls, 2);
        assert_eq!(evaluator.stats().hits, 0);
        evaluator.inner.0.fail = false;
        evaluator.evaluate(&position(), &mut logits)?;
        assert_eq!(evaluator.stats().nn_calls, 3);
        Ok(())
    }

    #[test]
    fn both_formats_account_for_allocated_vectors_and_enforce_budget() -> Result<()> {
        assert!(
            CachedEvaluator::new(
                TokenFake(Fake {
                    calls: 0,
                    fail: false
                }),
                1025
            )
            .is_err()
        );
        for format in [InputFormat::Basic, InputFormat::StmTokensV1] {
            assert!(Cache::new(0, format).is_err());
            assert!(Cache::new(size_of::<Cache>() + size_of::<Entry>() - 1, format).is_err());
            let cache = Cache::new(1024 * 1024, format)?;
            let actual = size_of::<Cache>()
                + cache.entries.capacity() * size_of::<Entry>()
                + (cache.first.capacity() + cache.second.capacity()) * size_of::<f32>();
            assert_eq!(cache.bytes, actual);
            assert!(actual <= 1024 * 1024);
            let expected_inputs = if format == InputFormat::Basic {
                FEATURES1_SIZE + FEATURES2_SIZE
            } else {
                0
            };
            assert_eq!(cache.first.len() + cache.second.len(), expected_inputs);
            assert!(actual + size_of::<Entry>() > 1024 * 1024);
        }
        Ok(())
    }
}
