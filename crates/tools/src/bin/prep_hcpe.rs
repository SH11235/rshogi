//! hcpe 教師プールの汚染除去・重複除去・シャッフル・分割ツール。

use std::{
    collections::HashSet,
    fs::{self, File},
    io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use clap::Parser;
use rand::{Rng, SeedableRng, seq::SliceRandom};
use rand_chacha::ChaCha8Rng;

#[path = "prep_hcpe/external_sort.rs"]
mod external_sort;
use external_sort::{ExternalSort, SortedRecords, read_record};

const MEMORY_RECORDS: usize = 250_000;
const SHUFFLE_RECORD_SIZE: usize = 24 + RECORD_SIZE;

const RECORD_SIZE: usize = 38;
const KEY_SIZE: usize = 32;
const BLOCK_U64S: usize = 8;
const BLOCK_BITS: u32 = (BLOCK_U64S * 64) as u32;

type Record = [u8; RECORD_SIZE];
type PositionKey = [u8; KEY_SIZE];

#[derive(Parser, Debug)]
#[command(
    name = "prep_hcpe",
    about = "hcpe 教師プールを汚染除去・重複除去・シャッフル・分割する"
)]
struct Args {
    /// 入力 hcpe ファイル。引数順に依存しないよう内部でソート・重複除去する。
    #[arg(long = "in", required = true, num_args = 1..)]
    inputs: Vec<PathBuf>,

    /// 除外対象の局面を含む hcpe ファイル（複数指定可）。
    #[arg(long, num_args = 1..)]
    exclude: Vec<PathBuf>,

    /// 分割ファイルの出力先ディレクトリ。
    #[arg(long)]
    out_dir: PathBuf,

    /// 出力ファイル名の接頭辞。
    #[arg(long, default_value = "chunk")]
    prefix: String,

    /// 1ファイルあたりの最大レコード数。
    #[arg(long, default_value_t = 250_000)]
    chunk_records: usize,

    /// shuffle 後に残すレコード数。0 は全件。
    #[arg(long, default_value_t = 0)]
    target: usize,

    /// 決定的 shuffle の seed。
    #[arg(long, default_value_t = 42)]
    seed: u64,

    /// Bloom filter の想定投入レコード数。
    #[arg(long, default_value_t = 100_000_000)]
    expected_records: u64,

    /// Bloom filter の偽陽性率。
    #[arg(long, default_value_t = 1e-6)]
    false_positive_rate: f64,
}

/// 512 bit（1 cache line）単位の blocked Bloom filter。
struct BloomFilter {
    blocks: Vec<u64>,
    num_blocks: u64,
    num_hashes: u32,
}

impl BloomFilter {
    /// 想定要素数と偽陽性率からフィルタを確保する。
    fn new(num_elements: u64, false_positive_rate: f64) -> Result<Self> {
        if num_elements == 0 {
            bail!("--expected-records は 1 以上で指定してください");
        }
        // 開区間 (0, 1) の外を弾く。NaN は両比較が false になるため、否定形で NaN も拒否する
        // （`<= 0.0 || >= 1.0` 形だと NaN がすり抜け、後段 num_blocks=0 で 0 除算 panic になる）。
        if !(false_positive_rate > 0.0 && false_positive_rate < 1.0) {
            bail!("--false-positive-rate は 0 より大きく 1 より小さくしてください");
        }

        let n = num_elements as f64;
        let bit_count = (-n * false_positive_rate.ln() / 2.0_f64.ln().powi(2)).ceil() as u64;
        let num_hashes = ((bit_count as f64 / n) * 2.0_f64.ln()).round().max(1.0) as u32;
        let num_blocks = bit_count.div_ceil(u64::from(BLOCK_BITS));
        let total_u64s = usize::try_from(num_blocks)
            .ok()
            .and_then(|blocks| blocks.checked_mul(BLOCK_U64S))
            .context("Bloom filter のサイズがアドレス可能範囲を超えています")?;

        Ok(Self {
            blocks: vec![0; total_u64s],
            num_blocks,
            num_hashes,
        })
    }

    /// key を登録し、登録前から存在した可能性がある場合は `true` を返す。
    fn insert_or_check(&mut self, key: &PositionKey) -> bool {
        let (h1, h2) = hash_pair(key);
        let block_offset = (h1 % self.num_blocks) as usize * BLOCK_U64S;
        let h2a = h2 as u32;
        let h2b = (h2 >> 32) as u32 | 1;
        let mut all_set = true;

        for i in 0..self.num_hashes {
            let bit_pos = h2a.wrapping_add(i.wrapping_mul(h2b)) % BLOCK_BITS;
            let word = &mut self.blocks[block_offset + (bit_pos >> 6) as usize];
            let mask = 1_u64 << (bit_pos & 63);
            if *word & mask == 0 {
                all_set = false;
                *word |= mask;
            }
        }
        all_set
    }
}

/// 32-byte 局面 key から二つの FNV-1a ハッシュを生成する。
fn hash_pair(key: &PositionKey) -> (u64, u64) {
    let mut h1 = 0xcbf29ce484222325_u64;
    let mut h2 = 0x6c62272e07bb0142_u64;
    for &byte in key {
        h1 ^= u64::from(byte);
        h1 = h1.wrapping_mul(0x100000001b3);
        h2 ^= u64::from(byte);
        h2 = h2.wrapping_mul(0x100000001b3);
    }
    (h1, h2 | 1)
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Summary {
    read: u64,
    excluded: u64,
    deduped: u64,
    kept: u64,
    written: u64,
    chunks: usize,
}

struct Config {
    inputs: Vec<PathBuf>,
    excludes: Vec<PathBuf>,
    out_dir: PathBuf,
    prefix: String,
    chunk_records: usize,
    target: usize,
    seed: u64,
    expected_records: u64,
    false_positive_rate: f64,
}

fn sorted_unique_paths(mut paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths.sort();
    paths.dedup();
    paths
}

fn validate_file(path: &Path) -> Result<u64> {
    let size = fs::metadata(path)
        .with_context(|| format!("ファイル情報を取得できません: {}", path.display()))?
        .len();
    if size % RECORD_SIZE as u64 != 0 {
        bail!(
            "hcpe ファイルサイズが {RECORD_SIZE} byte の倍数ではありません: {} ({size} bytes)",
            path.display()
        );
    }
    Ok(size / RECORD_SIZE as u64)
}

fn for_each_record(path: &Path, mut f: impl FnMut(Record) -> Result<()>) -> Result<()> {
    let records = validate_file(path)?;
    let file = File::open(path)
        .with_context(|| format!("hcpe ファイルを開けません: {}", path.display()))?;
    let mut reader = BufReader::new(file);
    for _ in 0..records {
        let mut record = [0_u8; RECORD_SIZE];
        reader
            .read_exact(&mut record)
            .with_context(|| format!("hcpe レコードを読み込めません: {}", path.display()))?;
        f(record)?;
    }
    Ok(())
}

fn position_key(record: &Record) -> PositionKey {
    // KEY_SIZE < RECORD_SIZE は型で保証されているため try_into は infallible。
    record[..KEY_SIZE].try_into().unwrap()
}

enum Exclusions {
    Memory(HashSet<PositionKey>),
    Disk {
        file: File,
        records: SortedRecords<KEY_SIZE>,
    },
}

impl Exclusions {
    fn contains(&mut self, key: &PositionKey) -> Result<bool> {
        match self {
            Self::Memory(keys) => Ok(keys.contains(key)),
            Self::Disk { records, file } => {
                let (mut left, mut right) = (0, records.count);
                while left < right {
                    let middle = left + (right - left) / 2;
                    file.seek(SeekFrom::Start(middle * KEY_SIZE as u64))?;
                    let mut candidate = [0; KEY_SIZE];
                    file.read_exact(&mut candidate)?;
                    match candidate.cmp(key) {
                        std::cmp::Ordering::Less => left = middle + 1,
                        std::cmp::Ordering::Equal => return Ok(true),
                        std::cmp::Ordering::Greater => right = middle,
                    }
                }
                Ok(false)
            }
        }
    }
}

fn load_exclude_keys(paths: &[PathBuf]) -> Result<Exclusions> {
    let mut keys = HashSet::new();
    let mut sorter = None;
    for path in paths {
        for_each_record(path, |record| {
            let key = position_key(&record);
            if let Some(sorter) = &mut sorter {
                ExternalSort::push(sorter, key)?;
            } else {
                keys.insert(key);
                if keys.len() == MEMORY_RECORDS {
                    let mut disk = ExternalSort::new(MEMORY_RECORDS)?;
                    // HashSet の反復順を外部ソートで正規化する。
                    for key in keys.drain() {
                        disk.push(key)?;
                    }
                    keys.shrink_to_fit();
                    sorter = Some(disk);
                }
            }
            Ok(())
        })?;
    }
    if let Some(sorter) = sorter {
        let records = sorter.finish()?;
        let file = records.open()?;
        Ok(Exclusions::Disk { records, file })
    } else {
        Ok(Exclusions::Memory(keys))
    }
}

fn keyed_record(record: Record, ordinal: u64, rng: &mut ChaCha8Rng) -> [u8; SHUFFLE_RECORD_SIZE] {
    let mut keyed = [0; SHUFFLE_RECORD_SIZE];
    keyed[..16].copy_from_slice(&rng.random::<u128>().to_be_bytes());
    // 同じ乱数 key のレコードも入力順で一意に並ぶ。
    keyed[16..24].copy_from_slice(&ordinal.to_be_bytes());
    keyed[24..].copy_from_slice(&record);
    keyed
}

enum Records {
    Memory(Vec<Record>),
    Disk(SortedRecords<SHUFFLE_RECORD_SIZE>),
}

fn collect_records(
    paths: &[PathBuf],
    exclude_keys: &mut Exclusions,
    bloom: &mut BloomFilter,
    target: usize,
    input_records: u64,
    rng: &mut ChaCha8Rng,
) -> Result<(Records, Summary)> {
    let capacity = if target == 0 { MEMORY_RECORDS } else { target };
    let mut kept =
        Vec::with_capacity(capacity.min(usize::try_from(input_records).unwrap_or(usize::MAX)));
    let mut sorter = None;
    let mut summary = Summary::default();
    for path in paths {
        for_each_record(path, |record| {
            summary.read += 1;
            let key = position_key(&record);
            if exclude_keys.contains(&key)? {
                summary.excluded += 1;
            } else if bloom.insert_or_check(&key) {
                summary.deduped += 1;
            } else {
                if target == 0 {
                    if kept.len() == MEMORY_RECORDS && sorter.is_none() {
                        let mut disk = ExternalSort::new(MEMORY_RECORDS)?;
                        for (ordinal, record) in kept.drain(..).enumerate() {
                            disk.push(keyed_record(record, ordinal as u64, rng))?;
                        }
                        kept.shrink_to_fit();
                        sorter = Some(disk);
                    }
                    if let Some(sorter) = &mut sorter {
                        sorter.push(keyed_record(record, summary.kept, rng))?;
                    } else {
                        kept.push(record);
                    }
                } else if kept.len() < target {
                    kept.push(record);
                } else {
                    let index = rng.random_range(0..=summary.kept);
                    if index < target as u64 {
                        kept[index as usize] = record;
                    }
                }
                summary.kept += 1;
            }
            Ok(())
        })?;
    }
    let records = if let Some(sorter) = sorter {
        Records::Disk(sorter.finish()?)
    } else {
        Records::Memory(kept)
    };
    Ok((records, summary))
}

fn write_chunks(
    records: Records,
    out_dir: &Path,
    prefix: &str,
    chunk_records: usize,
    protected: &[&Path],
    rng: &mut ChaCha8Rng,
) -> Result<(u64, usize)> {
    let mut written = 0_u64;
    let mut chunks = 0;
    let mut writer = None;
    let mut write_record = |record: Record| -> Result<()> {
        if written.is_multiple_of(chunk_records as u64) {
            if let Some(mut previous) = writer.take() {
                BufWriter::flush(&mut previous)?;
            }
            let path = out_dir.join(format!("{prefix}_{chunks:05}.hcpe"));
            tools::output_path::ensure_safe_output_paths(&[&path], protected)?;
            let file = File::create_new(&path)
                .with_context(|| format!("出力ファイルを新規作成できません: {}", path.display()))?;
            writer = Some(BufWriter::new(file));
            chunks += 1;
        }
        writer.as_mut().context("出力が開いていません")?.write_all(&record)?;
        written += 1;
        Ok(())
    };
    match records {
        Records::Memory(mut records) => {
            records.shuffle(rng);
            for record in records {
                write_record(record)?;
            }
        }
        Records::Disk(records) => {
            let mut reader = BufReader::new(records.open()?);
            while let Some(keyed) = read_record::<SHUFFLE_RECORD_SIZE>(&mut reader)? {
                write_record(keyed[24..].try_into()?)?;
            }
        }
    }
    if let Some(mut writer) = writer {
        writer.flush()?;
    }
    Ok((written, chunks))
}

fn validate_output_directory(out_dir: &Path, prefix: &str) -> Result<()> {
    anyhow::ensure!(
        !prefix.is_empty() && prefix != "." && prefix != ".." && !prefix.contains(['/', '\\', ':']),
        "--prefix はパス区切りを含まないファイル名にしてください"
    );
    fs::create_dir_all(out_dir)?;
    for entry in fs::read_dir(out_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name
            .to_ascii_lowercase()
            .starts_with(&format!("{prefix}_").to_ascii_lowercase())
            && name.to_ascii_lowercase().ends_with(".hcpe")
        {
            bail!(
                "同じ prefix の既存出力があります。空の出力先か別の prefix を指定してください: {}",
                entry.path().display()
            );
        }
    }
    Ok(())
}

fn run(config: Config) -> Result<Summary> {
    if config.chunk_records == 0 {
        bail!("--chunk-records は 1 以上で指定してください");
    }
    if config.prefix.is_empty() {
        bail!("--prefix は空にできません");
    }

    let inputs = sorted_unique_paths(config.inputs);
    let excludes = sorted_unique_paths(config.excludes);

    // 総入力レコード数（ファイルサイズから安価に算出）が --expected-records を超えると、bloom の
    // 実効偽陽性率が悪化し dedup が新規局面を余計に落とす（安全側だが学習データ損失）。事前に警告。
    let mut total_input_records: u64 = 0;
    for path in &inputs {
        total_input_records += validate_file(path)?;
    }
    if total_input_records > config.expected_records {
        eprintln!(
            "warning: 総入力 {total_input_records} レコードが --expected-records {} を超えています。\
             bloom の偽陽性率が悪化し dedup が余計に落とす可能性があります（--expected-records を上げてください）",
            config.expected_records
        );
    }

    validate_output_directory(&config.out_dir, &config.prefix)?;
    let mut exclude_keys = load_exclude_keys(&excludes)?;
    let mut bloom = BloomFilter::new(config.expected_records, config.false_positive_rate)?;
    let mut rng = ChaCha8Rng::seed_from_u64(config.seed);
    // streaming で集める（target>0 は reservoir でメモリ有界）。reservoir と shuffle は同一 rng。
    let (records, mut summary) = collect_records(
        &inputs,
        &mut exclude_keys,
        &mut bloom,
        config.target,
        total_input_records,
        &mut rng,
    )?;

    let protected: Vec<&Path> = inputs.iter().chain(&excludes).map(PathBuf::as_path).collect();
    (summary.written, summary.chunks) = write_chunks(
        records,
        &config.out_dir,
        &config.prefix,
        config.chunk_records,
        &protected,
        &mut rng,
    )?;
    Ok(summary)
}

fn main() -> Result<()> {
    let args = Args::parse();
    let summary = run(Config {
        inputs: args.inputs,
        excludes: args.exclude,
        out_dir: args.out_dir,
        prefix: args.prefix,
        chunk_records: args.chunk_records,
        target: args.target,
        seed: args.seed,
        expected_records: args.expected_records,
        false_positive_rate: args.false_positive_rate,
    })?;

    println!(
        "read={} excluded(contam)={} deduped={} kept={} written={} chunks={}",
        summary.read,
        summary.excluded,
        summary.deduped,
        summary.kept,
        summary.written,
        summary.chunks
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Write};

    use anyhow::Result;
    use rand::{SeedableRng, seq::SliceRandom};
    use rand_chacha::ChaCha8Rng;
    use tempfile::tempdir;

    use super::*;

    fn record(key_byte: u8, payload_byte: u8) -> Record {
        let mut record = [payload_byte; RECORD_SIZE];
        record[..KEY_SIZE].fill(key_byte);
        record
    }

    fn write_records(path: &Path, records: &[Record]) -> Result<()> {
        let mut file = File::create(path)?;
        for record in records {
            file.write_all(record)?;
        }
        Ok(())
    }

    #[test]
    fn seeded_shuffle_is_deterministic() {
        let original: Vec<u32> = (0..32).collect();
        let mut first = original.clone();
        let mut second = original.clone();
        let mut different = original;
        first.shuffle(&mut ChaCha8Rng::seed_from_u64(42));
        second.shuffle(&mut ChaCha8Rng::seed_from_u64(42));
        different.shuffle(&mut ChaCha8Rng::seed_from_u64(43));
        assert_eq!(first, second);
        assert_ne!(first, different);
    }

    #[test]
    fn bloom_new_rejects_invalid_false_positive_rate() {
        // 開区間 (0, 1) の外と NaN はすべて弾く（NaN を通すと num_blocks=0 で 0 除算 panic）。
        for fpr in [0.0, 1.0, -0.1, 1.5, f64::NAN, f64::INFINITY] {
            assert!(BloomFilter::new(100, fpr).is_err(), "fpr={fpr} は拒否されるべき");
        }
        assert!(BloomFilter::new(100, 1e-6).is_ok());
    }

    #[test]
    fn exclude_filter_drops_matching_keys() -> Result<()> {
        let dir = tempdir()?;
        let input = dir.path().join("input.hcpe");
        let excluded = record(2, 20);
        write_records(&input, &[record(1, 10), excluded, record(3, 30)])?;
        let mut exclude_keys = Exclusions::Memory(HashSet::from([position_key(&excluded)]));
        let mut bloom = BloomFilter::new(100, 1e-9)?;
        let mut rng = ChaCha8Rng::seed_from_u64(0);

        let (records, summary) =
            collect_records(&[input], &mut exclude_keys, &mut bloom, 0, 3, &mut rng)?;

        let Records::Memory(records) = records else {
            panic!("small collection")
        };
        assert_eq!(records, vec![record(1, 10), record(3, 30)]);
        assert_eq!(summary.excluded, 1);
        Ok(())
    }

    #[test]
    fn self_dedup_drops_repeated_key() -> Result<()> {
        let dir = tempdir()?;
        let input = dir.path().join("input.hcpe");
        write_records(&input, &[record(1, 10), record(2, 20), record(1, 99)])?;
        let mut bloom = BloomFilter::new(100, 1e-9)?;
        let mut rng = ChaCha8Rng::seed_from_u64(0);

        let (records, summary) = collect_records(
            &[input],
            &mut Exclusions::Memory(HashSet::new()),
            &mut bloom,
            0,
            3,
            &mut rng,
        )?;

        let Records::Memory(records) = records else {
            panic!("small collection")
        };
        assert_eq!(records, vec![record(1, 10), record(2, 20)]);
        assert_eq!(summary.deduped, 1);
        Ok(())
    }

    #[test]
    fn reservoir_caps_memory_to_target_and_is_unbiased_size() -> Result<()> {
        // 生き残り 10 件・target 4 → reservoir は 4 件だけ保持（メモリ有界）、kept(総数)=10。
        let dir = tempdir()?;
        let input = dir.path().join("input.hcpe");
        let recs: Vec<Record> = (0..10).map(|i| record(i, i)).collect();
        write_records(&input, &recs)?;
        let inputs = [input];
        let mut bloom = BloomFilter::new(100, 1e-9)?;
        let mut rng = ChaCha8Rng::seed_from_u64(123);

        let (records, summary) = collect_records(
            &inputs,
            &mut Exclusions::Memory(HashSet::new()),
            &mut bloom,
            4,
            recs.len() as u64,
            &mut rng,
        )?;

        let Records::Memory(records) = records else {
            panic!("reservoir")
        };
        assert_eq!(records.len(), 4); // target で有界
        assert_eq!(summary.kept, 10); // 生き残り総数
        // 採用された 4 件はすべて入力に存在し重複しない。
        let unique: HashSet<_> = records.iter().collect();
        assert_eq!(unique.len(), 4);
        for r in &records {
            assert!(recs.contains(r));
        }
        // 同一 seed で再実行すると同一標本（決定的）。
        let mut bloom2 = BloomFilter::new(100, 1e-9)?;
        let mut rng2 = ChaCha8Rng::seed_from_u64(123);
        let (records2, _) = collect_records(
            &inputs,
            &mut Exclusions::Memory(HashSet::new()),
            &mut bloom2,
            4,
            recs.len() as u64,
            &mut rng2,
        )?;
        let Records::Memory(records2) = records2 else {
            panic!("reservoir")
        };
        assert_eq!(records, records2);
        Ok(())
    }

    #[test]
    fn end_to_end_writes_expected_chunks() -> Result<()> {
        let dir = tempdir()?;
        let input = dir.path().join("input.hcpe");
        let out_dir = dir.path().join("out");
        let mut expected = vec![record(1, 10), record(2, 20), record(3, 30)];
        write_records(&input, &expected)?;
        expected.shuffle(&mut ChaCha8Rng::seed_from_u64(7));

        let summary = run(Config {
            inputs: vec![input],
            excludes: vec![],
            out_dir: out_dir.clone(),
            prefix: "part".to_owned(),
            chunk_records: 2,
            target: 0,
            seed: 7,
            expected_records: 100,
            false_positive_rate: 1e-9,
        })?;

        let first = fs::read(out_dir.join("part_00000.hcpe"))?;
        let second = fs::read(out_dir.join("part_00001.hcpe"))?;
        assert_eq!(first, expected[..2].concat());
        assert_eq!(second, expected[2..].concat());
        assert_eq!(
            summary,
            Summary {
                read: 3,
                excluded: 0,
                deduped: 0,
                kept: 3,
                written: 3,
                chunks: 2,
            }
        );
        Ok(())
    }
    fn indexed_record(index: u64) -> Record {
        use sha2::{Digest, Sha256};
        let mut bytes = [0; RECORD_SIZE];
        bytes[..KEY_SIZE].copy_from_slice(&Sha256::digest(index.to_le_bytes()));
        bytes[KEY_SIZE..].copy_from_slice(&index.to_le_bytes()[..RECORD_SIZE - KEY_SIZE]);
        bytes
    }

    #[test]
    fn disk_shuffle_preserves_all_records_and_repeats_seeded_bytes() -> Result<()> {
        let dir = tempdir()?;
        let input = dir.path().join("input.hcpe");
        let count = MEMORY_RECORDS + 1;
        let mut writer = BufWriter::new(File::create(&input)?);
        for index in 0..count {
            writer.write_all(&indexed_record(index as u64))?;
        }
        writer.flush()?;
        let outputs = [dir.path().join("first"), dir.path().join("second")];
        for output in &outputs {
            let summary = run(Config {
                inputs: vec![input.clone()],
                excludes: vec![],
                out_dir: output.clone(),
                prefix: "part".into(),
                chunk_records: count,
                target: 0,
                seed: 42,
                expected_records: count as u64 * 16,
                false_positive_rate: 1e-9,
            })?;
            assert_eq!(summary.written, count as u64);
            assert_eq!(summary.deduped, 0);
        }
        let first = fs::read(outputs[0].join("part_00000.hcpe"))?;
        assert_eq!(first, fs::read(outputs[1].join("part_00000.hcpe"))?);
        let mut actual: Vec<Record> = first.as_chunks::<RECORD_SIZE>().0.to_vec();
        actual.sort_unstable();
        let mut expected: Vec<Record> =
            (0..count).map(|index| indexed_record(index as u64)).collect();
        expected.sort_unstable();
        assert_eq!(actual, expected);
        Ok(())
    }

    #[test]
    fn disk_exclusion_matches_first_middle_last_and_rejects_absent_keys() -> Result<()> {
        let dir = tempdir()?;
        let input = dir.path().join("exclude.hcpe");
        let mut writer = BufWriter::new(File::create(&input)?);
        for index in 0..MEMORY_RECORDS + 1 {
            writer.write_all(&indexed_record(index as u64))?;
        }
        writer.flush()?;
        let mut keys = load_exclude_keys(&[input])?;
        assert!(matches!(keys, Exclusions::Disk { .. }));
        for index in [0, MEMORY_RECORDS / 2, MEMORY_RECORDS] {
            assert!(keys.contains(&position_key(&indexed_record(index as u64)))?);
        }
        assert!(!keys.contains(&position_key(&indexed_record(u64::MAX)))?);
        Ok(())
    }

    #[test]
    fn output_validation_rejects_paths_and_existing_hardlinked_chunks() -> Result<()> {
        let dir = tempdir()?;
        for prefix in [
            "../escape",
            "nested/file",
            "nested\\file",
            "..",
            ".",
            "C:escape",
        ] {
            assert!(validate_output_directory(&dir.path().join("out"), prefix).is_err());
        }
        let source = dir.path().join("source.hcpe");
        fs::write(&source, indexed_record(0))?;
        let out = dir.path().join("out");
        fs::create_dir(&out)?;
        fs::hard_link(&source, out.join("chunk_00000.hcpe"))?;
        assert!(validate_output_directory(&out, "chunk").is_err());
        assert_eq!(fs::read(source)?, indexed_record(0));
        Ok(())
    }
}
