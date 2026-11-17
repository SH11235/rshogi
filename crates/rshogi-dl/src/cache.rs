//! 宣言された形式の完全入力で照合する、容量固定の raw policy/value キャッシュ。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::mem::size_of;

use anyhow::{Result, ensure};
use rshogi_core::position::Position;

use crate::features::{FEATURES1_SIZE, FEATURES2_SIZE, MAX_MOVE_LABEL_NUM, make_input_features};
use crate::{EncodedInput, Evaluator, InputFormat, tokens};

const KEY_WORDS: usize = (FEATURES1_SIZE + FEATURES2_SIZE).div_ceil(64);
type BasicKey = [u64; KEY_WORDS];
type TokenKey = [u8; tokens::TOKEN_COUNT];
const _: () = assert!(KEY_WORDS == 151);

struct Entry<K> {
    key: K,
    logits: [f32; MAX_MOVE_LABEL_NUM],
    value: f32,
    occupied: bool,
}

struct Table<K> {
    entries: Vec<Entry<K>>,
    first: Vec<f32>,
    second: Vec<f32>,
    bytes: usize,
}

enum Cache {
    Basic(Table<BasicKey>),
    Tokens(Table<TokenKey>),
}

impl<K: Copy + Eq + Hash> Table<K> {
    fn new(bytes: usize, empty_key: K, first_len: usize, second_len: usize) -> Result<Self> {
        let overhead = size_of::<Cache>() + (first_len + second_len) * size_of::<f32>();
        ensure!(bytes > overhead, "cache budget is too small");
        let count = (bytes - overhead) / size_of::<Entry<K>>();
        ensure!(count > 0, "cache has no entries");
        let mut entries = Vec::new();
        entries.try_reserve_exact(count)?;
        entries.resize_with(count, || Entry {
            key: empty_key,
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
        let allocated = size_of::<Cache>()
            + entries.capacity() * size_of::<Entry<K>>()
            + (first.capacity() + second.capacity()) * size_of::<f32>();
        ensure!(allocated <= bytes, "cache allocation exceeds budget");
        Ok(Self {
            entries,
            first,
            second,
            bytes: allocated,
        })
    }

    fn index(&self, key: &K) -> usize {
        let mut hash = DefaultHasher::new();
        key.hash(&mut hash);
        (hash.finish() % self.entries.len() as u64) as usize
    }

    fn clear(&mut self) {
        for entry in &mut self.entries {
            entry.occupied = false;
        }
    }
}

fn evaluate_entry<K: Copy + Eq, E: Evaluator>(
    entry: &mut Entry<K>,
    key: K,
    inner: &mut E,
    pos: &Position,
    input: EncodedInput<'_>,
    logits: &mut [f32],
    stats: &mut CacheStats,
) -> Result<f32> {
    if entry.occupied && entry.key == key {
        logits.copy_from_slice(&entry.logits);
        stats.hits += 1;
        return Ok(entry.value);
    }
    stats.nn_calls += 1;
    let value = match inner.evaluate_encoded(input, logits)? {
        Some(value) => value,
        None => inner.evaluate(pos, logits)?,
    };
    ensure!(value.is_finite() && (0.0..=1.0).contains(&value), "invalid value probability");
    ensure!(logits.iter().all(|x| x.is_finite()), "non-finite policy logit");
    entry.key = key;
    entry.logits.copy_from_slice(logits);
    entry.value = value;
    entry.occupied = true;
    Ok(value)
}

impl Table<BasicKey> {
    fn key(&mut self, pos: &Position) -> Result<BasicKey> {
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
}

impl Cache {
    fn new(bytes: usize, format: InputFormat) -> Result<Self> {
        match format {
            InputFormat::Basic => {
                Ok(Self::Basic(Table::new(bytes, [0; KEY_WORDS], FEATURES1_SIZE, FEATURES2_SIZE)?))
            }
            InputFormat::StmTokensV1 => {
                Ok(Self::Tokens(Table::new(bytes, [0; tokens::TOKEN_COUNT], 0, 0)?))
            }
        }
    }

    fn clear(&mut self) {
        match self {
            Self::Basic(table) => table.clear(),
            Self::Tokens(table) => table.clear(),
        }
    }

    fn bytes(&self) -> usize {
        match self {
            Self::Basic(table) => table.bytes,
            Self::Tokens(table) => table.bytes,
        }
    }

    fn evaluate<E: Evaluator>(
        &mut self,
        inner: &mut E,
        pos: &Position,
        logits: &mut [f32],
        stats: &mut CacheStats,
    ) -> Result<f32> {
        match self {
            Self::Basic(table) => {
                let key = table.key(pos)?;
                let index = table.index(&key);
                let input = EncodedInput::basic_encoded(&table.first, &table.second);
                evaluate_entry(&mut table.entries[index], key, inner, pos, input, logits, stats)
            }
            Self::Tokens(table) => {
                let key = tokens::encode(pos)?;
                let index = table.index(&key);
                let input = EncodedInput::tokens_encoded(&key);
                evaluate_entry(&mut table.entries[index], key, inner, pos, input, logits, stats)
            }
        }
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
            cache.clear();
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
        self.cache.as_ref().map_or(0, |cache| cache.bytes())
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
        cache.evaluate(&mut self.inner, pos, logits, &mut self.stats)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl<K: Copy + Eq + Hash> Table<K> {
        fn evaluate<E: Evaluator>(
            &mut self,
            key: K,
            inner: &mut E,
            pos: &Position,
            logits: &mut [f32],
            stats: &mut CacheStats,
        ) -> Result<f32> {
            let format = inner.input_format().ok_or_else(|| anyhow::anyhow!("missing format"))?;
            let mut buffer = crate::InputBuffer::new(format)?;
            let input = buffer.encode(pos)?;
            let index = self.index(&key);
            evaluate_entry(&mut self.entries[index], key, inner, pos, input, logits, stats)
        }
    }

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

    #[derive(Clone, Copy)]
    enum EncodedMode {
        Supported,
        Unsupported,
        Fail,
        BadValue,
        BadPolicy,
    }

    struct EncodedFake {
        format: InputFormat,
        scalar_calls: usize,
        encoded_calls: usize,
        mode: EncodedMode,
    }

    fn input_marker(input: EncodedInput<'_>) -> f32 {
        if let Some((first, second)) = input.basic() {
            first.iter().chain(second).sum()
        } else {
            input.tokens().unwrap().iter().map(|&value| f32::from(value)).sum()
        }
    }

    impl Evaluator for EncodedFake {
        fn input_format(&self) -> Option<InputFormat> {
            Some(self.format)
        }

        fn evaluate(&mut self, position: &Position, logits: &mut [f32]) -> Result<f32> {
            self.scalar_calls += 1;
            let mut input = crate::InputBuffer::new(self.format)?;
            logits.fill(input_marker(input.encode(position)?));
            Ok(0.25)
        }

        fn evaluate_encoded(
            &mut self,
            input: EncodedInput<'_>,
            logits: &mut [f32],
        ) -> Result<Option<f32>> {
            self.encoded_calls += 1;
            ensure!(input.format() == self.format, "incorrect encoded input format");
            match self.mode {
                EncodedMode::Unsupported => Ok(None),
                EncodedMode::Fail => anyhow::bail!("encoded backend failed"),
                EncodedMode::Supported | EncodedMode::BadValue | EncodedMode::BadPolicy => {
                    logits.fill(if matches!(self.mode, EncodedMode::BadPolicy) {
                        f32::NAN
                    } else {
                        input_marker(input)
                    });
                    Ok(Some(if matches!(self.mode, EncodedMode::BadValue) {
                        1.25
                    } else {
                        0.25
                    }))
                }
            }
        }
    }

    fn encoded_fake(format: InputFormat, mode: EncodedMode) -> EncodedFake {
        EncodedFake {
            format,
            scalar_calls: 0,
            encoded_calls: 0,
            mode,
        }
    }

    #[test]
    fn encoded_miss_hit_ply_clear_and_off_preserve_scalar_contract() -> Result<()> {
        for format in [InputFormat::Basic, InputFormat::StmTokensV1] {
            let position = position();
            let mut input = crate::InputBuffer::new(format)?;
            let expected = input_marker(input.encode(&position)?);
            let mut evaluator =
                CachedEvaluator::new(encoded_fake(format, EncodedMode::Supported), 1)?;
            let bytes = evaluator.allocated_bytes();
            let mut logits = [0.0; MAX_MOVE_LABEL_NUM];
            for _ in 0..2 {
                assert_eq!(evaluator.evaluate(&position, &mut logits)?, 0.25);
                assert!(logits.iter().all(|&value| value == expected));
            }
            let mut later = Position::new();
            later.set_sfen(&position.to_sfen().replace(" 1", " 37"))?;
            evaluator.evaluate(&later, &mut logits)?;
            assert_eq!(evaluator.inner.encoded_calls, 1);
            assert_eq!(evaluator.inner.scalar_calls, 0);
            assert_eq!(evaluator.stats().hits, 2);
            evaluator.clear();
            evaluator.evaluate(&position, &mut logits)?;
            assert_eq!(evaluator.inner.encoded_calls, 2);
            assert_eq!(evaluator.allocated_bytes(), bytes);

            let mut off = CachedEvaluator::new(encoded_fake(format, EncodedMode::Supported), 0)?;
            for _ in 0..2 {
                off.evaluate(&position, &mut logits)?;
            }
            assert_eq!(off.inner.scalar_calls, 2);
            assert_eq!(off.inner.encoded_calls, 0);
        }
        Ok(())
    }

    #[test]
    fn unsupported_encoded_input_falls_back_only_on_miss() -> Result<()> {
        for format in [InputFormat::Basic, InputFormat::StmTokensV1] {
            let mut evaluator =
                CachedEvaluator::new(encoded_fake(format, EncodedMode::Unsupported), 1)?;
            let mut logits = [0.0; MAX_MOVE_LABEL_NUM];
            for _ in 0..2 {
                evaluator.evaluate(&position(), &mut logits)?;
            }
            assert_eq!(evaluator.inner.encoded_calls, 1);
            assert_eq!(evaluator.inner.scalar_calls, 1);
            assert_eq!(evaluator.stats().nn_calls, 1);
            assert_eq!(evaluator.stats().hits, 1);
        }
        let mut legacy = Fake {
            calls: 0,
            fail: false,
        };
        let mut buffer = crate::InputBuffer::new(InputFormat::Basic)?;
        let mut logits = [7.0; MAX_MOVE_LABEL_NUM];
        assert!(legacy.evaluate_encoded(buffer.encode(&position())?, &mut logits)?.is_none());
        assert!(logits.iter().all(|&value| value == 7.0));
        assert_eq!(legacy.calls, 0);
        Ok(())
    }

    #[test]
    fn encoded_errors_and_invalid_outputs_never_fall_back_or_cache() -> Result<()> {
        for format in [InputFormat::Basic, InputFormat::StmTokensV1] {
            for mode in [
                EncodedMode::Fail,
                EncodedMode::BadValue,
                EncodedMode::BadPolicy,
            ] {
                let mut evaluator = CachedEvaluator::new(encoded_fake(format, mode), 1)?;
                let mut logits = [0.0; MAX_MOVE_LABEL_NUM];
                for _ in 0..2 {
                    assert!(evaluator.evaluate(&position(), &mut logits).is_err());
                }
                assert_eq!(evaluator.inner.encoded_calls, 2);
                assert_eq!(evaluator.inner.scalar_calls, 0);
                assert_eq!(evaluator.stats().hits, 0);
                evaluator.inner.mode = EncodedMode::Supported;
                evaluator.evaluate(&position(), &mut logits)?;
                evaluator.evaluate(&position(), &mut logits)?;
                assert_eq!(evaluator.inner.encoded_calls, 3);
                assert_eq!(evaluator.stats().hits, 1);
            }
        }
        let mut evaluator = encoded_fake(InputFormat::Basic, EncodedMode::Supported);
        let mut buffer = crate::InputBuffer::new(InputFormat::StmTokensV1)?;
        let mut logits = [0.0; MAX_MOVE_LABEL_NUM];
        assert!(evaluator.evaluate_encoded(buffer.encode(&position())?, &mut logits).is_err());
        assert_eq!(evaluator.scalar_calls, 0);
        Ok(())
    }

    #[test]
    fn encoded_collision_failure_keeps_old_entry() -> Result<()> {
        for format in [InputFormat::Basic, InputFormat::StmTokensV1] {
            let bytes = size_of::<Cache>()
                + match format {
                    InputFormat::Basic => {
                        (FEATURES1_SIZE + FEATURES2_SIZE) * size_of::<f32>()
                            + size_of::<Entry<BasicKey>>()
                    }
                    InputFormat::StmTokensV1 => size_of::<Entry<TokenKey>>(),
                };
            let mut evaluator = CachedEvaluator {
                inner: encoded_fake(format, EncodedMode::Supported),
                cache: Some(Box::new(Cache::new(bytes, format)?)),
                stats: CacheStats::default(),
            };
            let first = position();
            let mut other = Position::new();
            other.set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 2")?;
            let mut logits = [0.0; MAX_MOVE_LABEL_NUM];
            evaluator.evaluate(&first, &mut logits)?;
            let expected = logits;
            evaluator.inner.mode = EncodedMode::Fail;
            assert!(evaluator.evaluate(&other, &mut logits).is_err());
            evaluator.evaluate(&first, &mut logits)?;
            assert_eq!(logits, expected);
            assert_eq!(evaluator.inner.encoded_calls, 2);
            assert_eq!(evaluator.inner.scalar_calls, 0);
            assert_eq!(evaluator.stats().hits, 1);
        }
        Ok(())
    }

    fn basic_table(bytes: usize) -> Result<Table<BasicKey>> {
        let Cache::Basic(table) = Cache::new(bytes, InputFormat::Basic)? else {
            unreachable!()
        };
        Ok(table)
    }

    fn token_table(bytes: usize) -> Result<Table<TokenKey>> {
        let Cache::Tokens(table) = Cache::new(bytes, InputFormat::StmTokensV1)? else {
            unreachable!()
        };
        Ok(table)
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
        let Cache::Basic(cache) = evaluator.cache.as_mut().unwrap().as_mut() else {
            unreachable!()
        };
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
        let mut cache = basic_table(1024 * 1024)?;
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
        let mut cache = basic_table(1024 * 1024)?;
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
    fn token_key_uses_every_byte_without_padding() -> Result<()> {
        let mut table = token_table(1024 * 1024)?;
        let pos = position();
        let key = tokens::encode(&pos)?;
        let index = table.index(&key);
        let mut inner = TokenFake(Fake {
            calls: 0,
            fail: false,
        });
        let mut logits = [0.0; MAX_MOVE_LABEL_NUM];
        let mut stats = CacheStats::default();
        assert_eq!(size_of::<TokenKey>(), tokens::TOKEN_COUNT);
        for byte in 0..tokens::TOKEN_COUNT {
            let mut different = key;
            different[byte] ^= 1;
            table.entries[index].key = different;
            table.entries[index].occupied = true;
            table.entries[index].value = 0.75;
            assert_eq!(table.evaluate(key, &mut inner, &pos, &mut logits, &mut stats)?, 0.25);
            assert_eq!(stats.hits, 0);
            assert_eq!(stats.nn_calls, byte as u64 + 1);
        }
        assert!(table.first.is_empty() && table.second.is_empty());
        Ok(())
    }

    #[test]
    fn failing_token_collision_keeps_the_previous_entry() -> Result<()> {
        let minimum = size_of::<Cache>() + size_of::<Entry<TokenKey>>();
        let mut table = token_table(minimum)?;
        let pos = position();
        let key = tokens::encode(&pos)?;
        let mut other = key;
        other[95] ^= 1;
        assert_eq!(table.index(&key), table.index(&other));
        let mut inner = TokenFake(Fake {
            calls: 0,
            fail: false,
        });
        let mut logits = [0.0; MAX_MOVE_LABEL_NUM];
        let mut stats = CacheStats::default();
        table.evaluate(key, &mut inner, &pos, &mut logits, &mut stats)?;
        inner.0.fail = true;
        assert!(table.evaluate(other, &mut inner, &pos, &mut logits, &mut stats).is_err());
        assert_eq!(table.evaluate(key, &mut inner, &pos, &mut logits, &mut stats)?, 0.25);
        assert_eq!(stats.nn_calls, 2);
        assert_eq!(stats.hits, 1);
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
        let mut cache = basic_table(1024 * 1024)?;
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
        let mut basic = basic_table(1024 * 1024)?;
        let mut basic_keys = Vec::new();
        let mut token_keys = Vec::new();
        for count in [8, 9, 18] {
            let mut pos = Position::new();
            pos.set_sfen(&format!("4k4/9/9/9/9/9/9/9/4K4 b {count}P 1"))?;
            basic_keys.push(basic.key(&pos)?);
            token_keys.push(tokens::encode(&pos)?);
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
        let black_key = tokens::encode(&black)?;
        let white_key = tokens::encode(&white)?;
        assert_eq!(&black_key[..95], &white_key[..95]);
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
        evaluator.reset_stats();
        evaluator.evaluate(&position(), &mut logits)?;
        assert_eq!(evaluator.stats().nn_calls, 0);
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

    fn check_budget<K: Copy + Eq + Hash>(table: &Table<K>, budget: usize) {
        let actual = size_of::<Cache>()
            + table.entries.capacity() * size_of::<Entry<K>>()
            + (table.first.capacity() + table.second.capacity()) * size_of::<f32>();
        assert_eq!(table.bytes, actual);
        assert!(actual <= budget);
        assert!(actual + size_of::<Entry<K>>() > budget);
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
        let basic_overhead =
            size_of::<Cache>() + (FEATURES1_SIZE + FEATURES2_SIZE) * size_of::<f32>();
        for (format, minimum) in [
            (InputFormat::Basic, basic_overhead + size_of::<Entry<BasicKey>>()),
            (InputFormat::StmTokensV1, size_of::<Cache>() + size_of::<Entry<TokenKey>>()),
        ] {
            assert!(Cache::new(0, format).is_err());
            assert!(Cache::new(minimum - 1, format).is_err());
            let cache = Cache::new(minimum, format)?;
            assert_eq!(cache.bytes(), minimum);
            match cache {
                Cache::Basic(table) => {
                    assert_eq!(table.entries.len(), 1);
                    check_budget(&table, minimum);
                }
                Cache::Tokens(table) => {
                    assert_eq!(table.entries.len(), 1);
                    check_budget(&table, minimum);
                }
            }
        }
        let budget = 1024 * 1024;
        let basic = basic_table(budget)?;
        let token = token_table(budget)?;
        check_budget(&basic, budget);
        check_budget(&token, budget);
        assert_eq!(basic.first.len() + basic.second.len(), FEATURES1_SIZE + FEATURES2_SIZE);
        assert!(token.first.is_empty() && token.second.is_empty());
        assert!(size_of::<Entry<TokenKey>>() < size_of::<Entry<BasicKey>>());
        assert!(token.entries.len() > basic.entries.len());
        Ok(())
    }
}
