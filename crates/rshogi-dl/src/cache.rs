//! 宣言された形式の完全入力で照合する、容量固定の raw policy/value キャッシュ。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::mem::size_of;

use anyhow::{Result, ensure};
use rshogi_core::position::Position;

use crate::features::{FEATURES1_SIZE, FEATURES2_SIZE, MAX_MOVE_LABEL_NUM, make_input_features};
use crate::input::zeroed;
use crate::{EncodedBatch, EncodedInput, Evaluator, InputBatchBuffer, InputFormat, tokens};

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
        BasicKey::from_input(EncodedInput::basic_encoded(&self.first, &self.second))
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

trait BatchKey: Copy + Eq + Hash {
    fn empty() -> Self;
    fn from_input(input: EncodedInput<'_>) -> Result<Self>;
}

impl BatchKey for BasicKey {
    fn empty() -> Self {
        [0; KEY_WORDS]
    }
    fn from_input(input: EncodedInput<'_>) -> Result<Self> {
        let (first, second) =
            input.basic().ok_or_else(|| anyhow::anyhow!("basic input required"))?;
        let mut key = [0; KEY_WORDS];
        for (index, value) in first.iter().chain(second).enumerate() {
            match value.to_bits() {
                0 => {}
                0x3f800000 => key[index / 64] |= 1 << (index % 64),
                _ => anyhow::bail!("cache requires binary basic inputs"),
            }
        }
        Ok(key)
    }
}
impl BatchKey for TokenKey {
    fn empty() -> Self {
        [0; tokens::TOKEN_COUNT]
    }
    fn from_input(input: EncodedInput<'_>) -> Result<Self> {
        input.tokens().copied().ok_or_else(|| anyhow::anyhow!("token input required"))
    }
}

struct BatchOutput {
    format: InputFormat,
    capacity: usize,
    values: Vec<f32>,
    policies: Vec<f32>,
}
impl BatchOutput {
    fn new(format: InputFormat, capacity: usize) -> Result<Self> {
        Ok(Self {
            format,
            capacity,
            values: zeroed(capacity)?,
            policies: zeroed(capacity * MAX_MOVE_LABEL_NUM)?,
        })
    }
    fn validate(&self, input: EncodedBatch<'_>, values: &[f32], policies: &[f32]) -> Result<()> {
        ensure!(input.format() == self.format, "encoded batch format mismatch");
        ensure!(input.len() <= self.capacity, "batch capacity exceeded");
        ensure!(
            values.len() == input.len() && policies.len() == input.len() * MAX_MOVE_LABEL_NUM,
            "incorrect output buffers"
        );
        Ok(())
    }
    fn validate_outputs(&self, n: usize) -> Result<()> {
        ensure!(
            self.values[..n].iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)),
            "invalid value probability"
        );
        ensure!(
            self.policies[..n * MAX_MOVE_LABEL_NUM].iter().all(|v| v.is_finite()),
            "non-finite policy logit"
        );
        Ok(())
    }
    fn heap_bytes(&self) -> usize {
        (self.values.capacity() + self.policies.capacity()) * size_of::<f32>()
    }
}
struct BatchState<K> {
    output: BatchOutput,
    misses: InputBatchBuffer,
    keys: Vec<K>,
    slots: Vec<usize>,
    miss_rows: Vec<usize>,
}
impl<K: BatchKey> BatchState<K> {
    fn new(format: InputFormat, capacity: usize) -> Result<Self> {
        Ok(Self {
            output: BatchOutput::new(format, capacity)?,
            misses: InputBatchBuffer::new(format, capacity)?,
            keys: {
                let mut keys = Vec::new();
                keys.try_reserve_exact(capacity)?;
                keys.resize(capacity, K::empty());
                keys
            },
            slots: zeroed(capacity)?,
            miss_rows: zeroed(capacity)?,
        })
    }
    fn heap_bytes(&self) -> usize {
        self.output.heap_bytes() + self.misses.allocated_bytes() - size_of::<InputBatchBuffer>()
            + self.keys.capacity() * size_of::<K>()
            + (self.slots.capacity() + self.miss_rows.capacity()) * size_of::<usize>()
    }
    fn evaluate<E: Evaluator>(
        &mut self,
        table: &mut Table<K>,
        inner: &mut E,
        input: EncodedBatch<'_>,
        outputs: (&mut [f32], &mut [f32]),
        counters: (&mut CacheStats, &mut u64),
    ) -> Result<()> {
        let (values, policies) = outputs;
        let (stats, nn_positions) = counters;
        let n = input.len();
        let mut count = 0;
        for row in 0..n {
            let key = K::from_input(input.row(row).expect("validated batch row"))?;
            let slot = table.index(&key);
            self.keys[row] = key;
            self.slots[row] = slot;
            let entry = &table.entries[slot];
            if !entry.occupied || entry.key != key {
                self.miss_rows[count] = row;
                count += 1;
            }
        }
        if count != 0 {
            self.misses.clear();
            let backend_input = if count == n {
                input
            } else {
                for &row in &self.miss_rows[..count] {
                    self.misses.push_encoded(input.row(row).expect("validated miss row"))?;
                }
                self.misses.view()
            };
            stats.nn_calls += 1;
            *nn_positions += count as u64;
            inner.evaluate_encoded_batch(
                backend_input,
                &mut self.output.values[..count],
                &mut self.output.policies[..count * MAX_MOVE_LABEL_NUM],
            )?;
            self.output.validate_outputs(count)?;
            // miss 行は昇順かつ row >= compact index。逆順コピーなら未読の結果を壊さない。
            for compact in (0..count).rev() {
                let row = self.miss_rows[compact];
                self.output.values[row] = self.output.values[compact];
                self.output.policies.copy_within(
                    compact * MAX_MOVE_LABEL_NUM..(compact + 1) * MAX_MOVE_LABEL_NUM,
                    row * MAX_MOVE_LABEL_NUM,
                );
            }
        }
        let mut miss = 0;
        for row in 0..n {
            if miss < count && self.miss_rows[miss] == row {
                miss += 1;
                continue;
            }
            let entry = &table.entries[self.slots[row]];
            self.output.values[row] = entry.value;
            self.output.policies[row * MAX_MOVE_LABEL_NUM..(row + 1) * MAX_MOVE_LABEL_NUM]
                .copy_from_slice(&entry.logits);
        }
        // 全 hit を読み終えてから登録する。同一 batch 内の衝突で hit が失われるのを防ぐ。
        for &row in &self.miss_rows[..count] {
            let entry = &mut table.entries[self.slots[row]];
            entry.key = self.keys[row];
            entry.value = self.output.values[row];
            entry.logits.copy_from_slice(
                &self.output.policies[row * MAX_MOVE_LABEL_NUM..(row + 1) * MAX_MOVE_LABEL_NUM],
            );
            entry.occupied = true;
        }
        stats.hits += (n - count) as u64;
        values.copy_from_slice(&self.output.values[..n]);
        policies.copy_from_slice(&self.output.policies[..n * MAX_MOVE_LABEL_NUM]);
        Ok(())
    }
}
enum BatchWorkspace {
    Direct(BatchOutput),
    Basic(BatchState<BasicKey>),
    Tokens(BatchState<TokenKey>),
}
impl BatchWorkspace {
    fn new(format: InputFormat, capacity: usize, cached: bool) -> Result<Self> {
        let input_bytes = InputBatchBuffer::required_heap_bytes(format, capacity)?;
        let key_bytes = match format {
            InputFormat::Basic => size_of::<BasicKey>(),
            InputFormat::StmTokensV1 => size_of::<TokenKey>(),
        };
        let row_bytes = (MAX_MOVE_LABEL_NUM + 1) * size_of::<f32>()
            + if cached {
                key_bytes + 2 * size_of::<usize>()
            } else {
                0
            };
        let heap = capacity
            .checked_mul(row_bytes)
            .and_then(|bytes| bytes.checked_add(if cached { input_bytes } else { 0 }))
            .ok_or_else(|| anyhow::anyhow!("batch workspace size overflow"))?;
        ensure!(
            heap.checked_add(size_of::<Self>())
                .is_some_and(|bytes| bytes <= isize::MAX as usize),
            "batch workspace size overflow"
        );
        if !cached {
            return Ok(Self::Direct(BatchOutput::new(format, capacity)?));
        }
        match format {
            InputFormat::Basic => Ok(Self::Basic(BatchState::new(format, capacity)?)),
            InputFormat::StmTokensV1 => Ok(Self::Tokens(BatchState::new(format, capacity)?)),
        }
    }
    fn output(&self) -> &BatchOutput {
        match self {
            Self::Direct(out) => out,
            Self::Basic(state) => &state.output,
            Self::Tokens(state) => &state.output,
        }
    }
    fn bytes(&self) -> usize {
        size_of::<Self>()
            + match self {
                Self::Direct(out) => out.heap_bytes(),
                Self::Basic(state) => state.heap_bytes(),
                Self::Tokens(state) => state.heap_bytes(),
            }
    }
}

/// 最後の reset 以降の評価要求と、実際の backend 呼び出し。
/// batch は複数局面を一度に推論するため、requests は hits + nn_calls と一致しない。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    /// キャッシュを含む評価要求数。
    pub requests: u64,
    /// backend を呼んだ数。batch は局面数にかかわらず 1。失敗した呼び出しも含む。
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
    nn_positions: u64,
    batch: Option<Box<BatchWorkspace>>,
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
            nn_positions: 0,
            batch: None,
        })
    }

    /// 明示的 batch 用 workspace を追加確保する。cache の MiB 上限とは別に管理する。
    pub fn with_batch_capacity(inner: E, mib: usize, capacity: usize) -> Result<Self> {
        ensure!(
            capacity > 0 && inner.encoded_batch_capacity().is_some_and(|max| capacity <= max),
            "unsupported batch capacity"
        );
        let format = inner
            .input_format()
            .ok_or_else(|| anyhow::anyhow!("batch requires an input format"))?;
        let batch = BatchWorkspace::new(format, capacity, mib != 0)?;
        let mut evaluator = Self::new(inner, mib)?;
        evaluator.batch = Some(Box::new(batch));
        Ok(evaluator)
    }

    /// backend に渡した局面数。失敗した試行も含み、reset_stats と clear でゼロになる。
    pub fn nn_positions(&self) -> u64 {
        self.nn_positions
    }

    /// 明示的 batch の出力 staging・miss 入力・key・索引を含む別枠の bytes。
    pub fn batch_allocated_bytes(&self) -> usize {
        self.batch.as_ref().map_or(0, |batch| batch.bytes())
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
        self.nn_positions = 0;
    }

    /// 最後の reset 以降の統計。
    pub fn stats(&self) -> CacheStats {
        self.stats
    }

    /// scalar cache の key・出力・metadata・特徴量 buffer の bytes。batch workspace は含まない。
    pub fn allocated_bytes(&self) -> usize {
        self.cache.as_ref().map_or(0, |cache| cache.bytes())
    }
}

impl<E: Evaluator> Evaluator for CachedEvaluator<E> {
    fn input_format(&self) -> Option<InputFormat> {
        self.inner.input_format()
    }

    fn encoded_batch_capacity(&self) -> Option<usize> {
        self.batch.as_ref().map(|batch| batch.output().capacity)
    }

    fn evaluate_encoded_batch(
        &mut self,
        input: EncodedBatch<'_>,
        values: &mut [f32],
        policies: &mut [f32],
    ) -> Result<()> {
        let batch = self
            .batch
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("batch workspace is not prepared"))?;
        batch.output().validate(input, values, policies)?;
        self.stats.requests += input.len() as u64;
        if input.is_empty() {
            return Ok(());
        }
        match (self.cache.as_deref_mut(), batch.as_mut()) {
            (None, BatchWorkspace::Direct(out)) => {
                self.stats.nn_calls += 1;
                self.nn_positions += input.len() as u64;
                self.inner.evaluate_encoded_batch(
                    input,
                    &mut out.values[..input.len()],
                    &mut out.policies[..policies.len()],
                )?;
                out.validate_outputs(input.len())?;
                values.copy_from_slice(&out.values[..values.len()]);
                policies.copy_from_slice(&out.policies[..policies.len()]);
            }
            (Some(Cache::Basic(table)), BatchWorkspace::Basic(state)) => state.evaluate(
                table,
                &mut self.inner,
                input,
                (values, policies),
                (&mut self.stats, &mut self.nn_positions),
            )?,
            (Some(Cache::Tokens(table)), BatchWorkspace::Tokens(state)) => state.evaluate(
                table,
                &mut self.inner,
                input,
                (values, policies),
                (&mut self.stats, &mut self.nn_positions),
            )?,
            _ => anyhow::bail!("batch workspace format mismatch"),
        }
        Ok(())
    }

    fn evaluate(&mut self, pos: &Position, logits: &mut [f32]) -> Result<f32> {
        self.stats.requests += 1;
        let Some(cache) = &mut self.cache else {
            self.stats.nn_calls += 1;
            self.nn_positions += 1;
            return self.inner.evaluate(pos, logits);
        };
        ensure!(logits.len() == MAX_MOVE_LABEL_NUM, "incorrect policy buffer length");
        let previous = self.stats.nn_calls;
        let result = cache.evaluate(&mut self.inner, pos, logits, &mut self.stats);
        self.nn_positions += self.stats.nn_calls - previous;
        result
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

    struct BatchFake {
        format: InputFormat,
        calls: usize,
        rows: Vec<usize>,
        fail: bool,
        invalid: bool,
        invalid_policy: bool,
    }
    impl Evaluator for BatchFake {
        fn input_format(&self) -> Option<InputFormat> {
            Some(self.format)
        }
        fn encoded_batch_capacity(&self) -> Option<usize> {
            Some(8)
        }
        fn evaluate(&mut self, _: &Position, _: &mut [f32]) -> Result<f32> {
            anyhow::bail!("unexpected scalar fallback")
        }
        fn evaluate_encoded_batch(
            &mut self,
            input: EncodedBatch<'_>,
            values: &mut [f32],
            policies: &mut [f32],
        ) -> Result<()> {
            self.calls += 1;
            self.rows.push(input.len());
            for (row, value) in values.iter_mut().enumerate() {
                *value = 0.25;
                policies[row * MAX_MOVE_LABEL_NUM..(row + 1) * MAX_MOVE_LABEL_NUM]
                    .fill(input_marker(input.row(row).unwrap()));
            }
            ensure!(!self.fail, "backend failed");
            if self.invalid {
                values[0] = f32::NAN;
            }
            if self.invalid_policy {
                policies[0] = f32::NAN;
            }
            Ok(())
        }
    }
    fn batch_fake(format: InputFormat) -> BatchFake {
        BatchFake {
            format,
            calls: 0,
            rows: Vec::new(),
            fail: false,
            invalid: false,
            invalid_policy: false,
        }
    }
    fn batch_positions() -> Result<Vec<Position>> {
        [
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 2",
            "4k4/9/9/9/9/9/9/9/4K4 b R 1",
        ]
        .iter()
        .map(|sfen| {
            let mut pos = Position::new();
            pos.set_sfen(sfen)?;
            Ok(pos)
        })
        .collect()
    }
    fn batch_buffer(
        format: InputFormat,
        positions: &[Position],
        rows: &[usize],
    ) -> Result<InputBatchBuffer> {
        let mut input = InputBatchBuffer::new(format, rows.len().max(1))?;
        for &row in rows {
            input.push(&positions[row])?;
        }
        Ok(input)
    }

    #[test]
    fn batch_mixed_hits_misses_duplicates_and_counters() -> Result<()> {
        let positions = batch_positions()?;
        for format in [InputFormat::Basic, InputFormat::StmTokensV1] {
            let mut evaluator = CachedEvaluator::with_batch_capacity(batch_fake(format), 1, 8)?;
            let cache_bytes = evaluator.allocated_bytes();
            let batch_bytes = evaluator.batch_allocated_bytes();
            let seed = batch_buffer(format, &positions, &[0])?;
            evaluator.evaluate_encoded_batch(
                seed.view(),
                &mut [0.0],
                &mut [0.0; MAX_MOVE_LABEL_NUM],
            )?;
            let input = batch_buffer(format, &positions, &[0, 1, 0, 2, 1])?;
            let mut values = [0.0; 5];
            let mut policy = vec![0.0; 5 * MAX_MOVE_LABEL_NUM];
            evaluator.evaluate_encoded_batch(input.view(), &mut values, &mut policy)?;
            assert_eq!(evaluator.inner.rows, [1, 3]);
            assert_eq!(evaluator.nn_positions(), 4);
            assert_eq!(
                evaluator.stats(),
                CacheStats {
                    requests: 6,
                    nn_calls: 2,
                    hits: 2
                }
            );
            for row in 0..5 {
                assert_eq!(values[row], 0.25);
                assert!(
                    policy[row * MAX_MOVE_LABEL_NUM..(row + 1) * MAX_MOVE_LABEL_NUM]
                        .iter()
                        .all(|&v| v == input_marker(input.view().row(row).unwrap()))
                );
            }
            evaluator.evaluate_encoded_batch(input.view(), &mut values, &mut policy)?;
            assert_eq!(evaluator.inner.calls, 2);
            assert_eq!(evaluator.stats().hits, 7);
            assert_eq!(evaluator.allocated_bytes(), cache_bytes);
            assert_eq!(evaluator.batch_allocated_bytes(), batch_bytes);
            evaluator.clear();
            assert_eq!(evaluator.nn_positions(), 0);
            assert_eq!(evaluator.stats(), CacheStats::default());
            evaluator.evaluate_encoded_batch(input.view(), &mut values, &mut policy)?;
            assert_eq!(evaluator.inner.rows, [1, 3, 5]);
        }
        Ok(())
    }

    #[test]
    fn batch_one_matches_scalar_and_empty_batches_do_not_call_backend() -> Result<()> {
        let positions = batch_positions()?;
        for format in [InputFormat::Basic, InputFormat::StmTokensV1] {
            for mib in [0, 1] {
                let mut scalar =
                    CachedEvaluator::new(encoded_fake(format, EncodedMode::Supported), mib)?;
                assert_eq!(scalar.encoded_batch_capacity(), None);
                assert_eq!(scalar.batch_allocated_bytes(), 0);
                let mut batch = CachedEvaluator::with_batch_capacity(batch_fake(format), mib, 1)?;
                assert_eq!(batch.encoded_batch_capacity(), Some(1));
                let empty = InputBatchBuffer::new(format, 1)?;
                batch.evaluate_encoded_batch(empty.view(), &mut [], &mut [])?;
                assert_eq!(batch.inner.calls, 0);
                for position in &positions {
                    let mut input = InputBatchBuffer::new(format, 1)?;
                    input.push(position)?;
                    let mut scalar_policy = [0.0; MAX_MOVE_LABEL_NUM];
                    let mut batch_policy = [0.0; MAX_MOVE_LABEL_NUM];
                    let mut value = [0.0];
                    batch.evaluate_encoded_batch(input.view(), &mut value, &mut batch_policy)?;
                    assert_eq!(scalar.evaluate(position, &mut scalar_policy)?, value[0]);
                    assert_eq!(scalar_policy, batch_policy);
                }
                let input = batch_buffer(format, &positions, &[0, 1])?;
                assert!(
                    batch
                        .evaluate_encoded_batch(
                            input.view(),
                            &mut [0.0; 2],
                            &mut vec![0.0; 2 * MAX_MOVE_LABEL_NUM]
                        )
                        .is_err()
                );
                assert_eq!(batch.inner.calls, 3);
                assert_eq!(batch.nn_positions(), 3);
                assert!(scalar.evaluate_encoded_batch(empty.view(), &mut [], &mut []).is_err());
            }
        }
        Ok(())
    }

    #[test]
    fn batch_errors_leave_outputs_and_cache_unchanged() -> Result<()> {
        let positions = batch_positions()?;
        for format in [InputFormat::Basic, InputFormat::StmTokensV1] {
            for mib in [0, 1] {
                let mut evaluator =
                    CachedEvaluator::with_batch_capacity(batch_fake(format), mib, 8)?;
                let seed = batch_buffer(format, &positions, &[0])?;
                evaluator.evaluate_encoded_batch(
                    seed.view(),
                    &mut [0.0],
                    &mut [0.0; MAX_MOVE_LABEL_NUM],
                )?;
                let input = batch_buffer(format, &positions, &[0, 1])?;
                let mut values = [77.0; 2];
                let mut policy = vec![77.0; 2 * MAX_MOVE_LABEL_NUM];
                for failure in 0..3 {
                    evaluator.inner.fail = failure == 0;
                    evaluator.inner.invalid = failure == 1;
                    evaluator.inner.invalid_policy = failure == 2;
                    assert!(
                        evaluator
                            .evaluate_encoded_batch(input.view(), &mut values, &mut policy)
                            .is_err()
                    );
                    assert_eq!(values, [77.0; 2]);
                    assert!(policy.iter().all(|&v| v == 77.0));
                }
                evaluator.inner.fail = false;
                evaluator.inner.invalid = false;
                evaluator.inner.invalid_policy = false;
                evaluator.evaluate_encoded_batch(
                    seed.view(),
                    &mut [0.0],
                    &mut [0.0; MAX_MOVE_LABEL_NUM],
                )?;
                assert_eq!(evaluator.stats().hits, if mib == 0 { 0 } else { 1 });
                assert_eq!(evaluator.nn_positions(), if mib == 0 { 8 } else { 4 });
                assert_eq!(evaluator.stats().nn_calls, if mib == 0 { 5 } else { 4 });
                let calls = evaluator.inner.calls;
                assert!(
                    evaluator.evaluate_encoded_batch(input.view(), &mut [], &mut policy).is_err()
                );
                assert_eq!(evaluator.inner.calls, calls);
                assert!(
                    evaluator
                        .evaluate_encoded_batch(
                            batch_buffer(
                                if format == InputFormat::Basic {
                                    InputFormat::StmTokensV1
                                } else {
                                    InputFormat::Basic
                                },
                                &positions,
                                &[0]
                            )?
                            .view(),
                            &mut [77.0],
                            &mut [77.0; MAX_MOVE_LABEL_NUM]
                        )
                        .is_err()
                );
                evaluator.reset_stats();
                assert_eq!(evaluator.nn_positions(), 0);
            }
        }
        assert!(
            CachedEvaluator::with_batch_capacity(
                Fake {
                    calls: 0,
                    fail: false
                },
                1,
                1
            )
            .is_err()
        );
        assert!(
            CachedEvaluator::with_batch_capacity(batch_fake(InputFormat::Basic), 1, 9).is_err()
        );
        assert!(BatchWorkspace::new(InputFormat::Basic, usize::MAX, true).is_err());
        Ok(())
    }

    #[test]
    fn batch_collision_reads_hits_before_request_order_registration() -> Result<()> {
        let positions = batch_positions()?;
        for format in [InputFormat::Basic, InputFormat::StmTokensV1] {
            let bytes = size_of::<Cache>()
                + match format {
                    InputFormat::Basic => {
                        (FEATURES1_SIZE + FEATURES2_SIZE) * size_of::<f32>()
                            + size_of::<Entry<BasicKey>>()
                    }
                    InputFormat::StmTokensV1 => size_of::<Entry<TokenKey>>(),
                };
            let mut evaluator = CachedEvaluator::with_batch_capacity(batch_fake(format), 1, 8)?;
            evaluator.cache = Some(Box::new(Cache::new(bytes, format)?));
            let seed = batch_buffer(format, &positions, &[0])?;
            evaluator.evaluate_encoded_batch(
                seed.view(),
                &mut [0.0],
                &mut [0.0; MAX_MOVE_LABEL_NUM],
            )?;
            let input = batch_buffer(format, &positions, &[1, 0, 2, 0])?;
            let mut values = [0.0; 4];
            let mut policy = vec![0.0; 4 * MAX_MOVE_LABEL_NUM];
            evaluator.evaluate_encoded_batch(input.view(), &mut values, &mut policy)?;
            assert_eq!(evaluator.inner.rows, [1, 2]);
            for row in 0..4 {
                assert_eq!(
                    policy[row * MAX_MOVE_LABEL_NUM],
                    input_marker(input.view().row(row).unwrap())
                );
            }
            let final_miss = batch_buffer(format, &positions, &[2])?;
            evaluator.evaluate_encoded_batch(
                final_miss.view(),
                &mut [0.0],
                &mut [0.0; MAX_MOVE_LABEL_NUM],
            )?;
            assert_eq!(evaluator.inner.calls, 2);
            assert_eq!(evaluator.stats().hits, 3);
        }
        Ok(())
    }

    #[test]
    fn reverse_scatter_preserves_every_monotonic_miss_subset() {
        for bits in 0u32..(1 << 8) {
            let rows: Vec<_> = (0..8).filter(|row| bits & (1 << row) != 0).collect();
            let mut output = [usize::MAX; 8];
            for (compact, &row) in rows.iter().enumerate() {
                assert!(row >= compact);
                output[compact] = row;
            }
            for compact in (0..rows.len()).rev() {
                output[rows[compact]] = output[compact];
            }
            for row in rows {
                assert_eq!(output[row], row);
            }
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
                nn_positions: 0,
                batch: None,
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
