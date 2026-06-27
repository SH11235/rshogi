//! hcpe 教師プールの汚染除去・重複除去・シャッフル・分割ツール。

use std::{
    collections::HashSet,
    fs::{self, File},
    io::{BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use clap::Parser;
use rand::{Rng, SeedableRng, seq::SliceRandom};
use rand_chacha::ChaCha8Rng;

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

fn load_exclude_keys(paths: &[PathBuf]) -> Result<HashSet<PositionKey>> {
    let mut keys = HashSet::new();
    for path in paths {
        for_each_record(path, |record| {
            keys.insert(position_key(&record));
            Ok(())
        })?;
    }
    Ok(keys)
}

/// 入力を streaming で走査し、exclude（汚染/クロス重複）と bloom 自己重複を落としつつ、
/// 生き残りレコードを集める。`target > 0` のときは **reservoir sampling**（Algorithm R）で
/// target 件の無偏標本だけを保持するため、ピークメモリは入力件数でなく `target` で有界になる。
/// `target == 0` のときは全生き残りを保持する（その場合のみメモリは生き残り件数に比例）。
/// reservoir に同じ `rng` を使うため、seed + 入力順固定で結果は決定的。
fn collect_records(
    paths: &[PathBuf],
    exclude_keys: &HashSet<PositionKey>,
    bloom: &mut BloomFilter,
    target: usize,
    input_records: u64,
    rng: &mut ChaCha8Rng,
) -> Result<(Vec<Record>, Summary)> {
    // target>0 のとき reservoir は最大 min(target, 生き残り件数) 件しか保持しない。逐次 push の
    // 幾何成長だと最終容量が必要量を大きく超え、再確保中は旧/新領域が同時に必要になって OOM
    // しやすい（doc 例の target=100M で約 3.54 GiB）ため事前確保する。容量は生き残り件数の上限
    // である総入力件数 input_records で頭打ちにする。これにより (a) target≫入力 のときの過剰確保と
    // (b) 巨大 target（usize::MAX 近傍）での capacity overflow panic の両方を避ける。
    let mut kept: Vec<Record> = if target > 0 {
        Vec::with_capacity(target.min(input_records as usize))
    } else {
        Vec::new()
    };
    let mut summary = Summary::default();
    // これまでに「生き残った」レコード数（reservoir のインデックス）。
    let mut kept_seen: u64 = 0;

    for path in paths {
        for_each_record(path, |record| {
            summary.read += 1;
            let key = position_key(&record);
            if exclude_keys.contains(&key) {
                summary.excluded += 1;
            } else if bloom.insert_or_check(&key) {
                summary.deduped += 1;
            } else {
                if target == 0 || kept.len() < target {
                    kept.push(record);
                } else {
                    // Algorithm R: kept_seen 番目（0-index）の生き残りを確率 target/(kept_seen+1) で採用。
                    // u64 のまま比較し（先に usize へ cast すると 32-bit で切り詰めうる）、採用時のみ
                    // index 化する（このとき j < target ≤ usize::MAX なので安全）。
                    let j = rng.random_range(0..=kept_seen);
                    if j < target as u64 {
                        kept[j as usize] = record;
                    }
                }
                kept_seen += 1;
            }
            Ok(())
        })?;
    }
    summary.kept = kept_seen; // 生き残り総数（標本化前）
    Ok((kept, summary))
}

fn write_chunks(
    records: &[Record],
    out_dir: &Path,
    prefix: &str,
    chunk_records: usize,
) -> Result<usize> {
    fs::create_dir_all(out_dir)
        .with_context(|| format!("出力ディレクトリを作成できません: {}", out_dir.display()))?;

    for (chunk_index, chunk) in records.chunks(chunk_records).enumerate() {
        let path = out_dir.join(format!("{prefix}_{chunk_index:05}.hcpe"));
        let file = File::create(&path)
            .with_context(|| format!("出力ファイルを作成できません: {}", path.display()))?;
        let mut writer = BufWriter::new(file);
        for record in chunk {
            writer
                .write_all(record)
                .with_context(|| format!("レコードを書き込めません: {}", path.display()))?;
        }
        writer
            .flush()
            .with_context(|| format!("出力を flush できません: {}", path.display()))?;
    }
    Ok(records.len().div_ceil(chunk_records))
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

    let exclude_keys = load_exclude_keys(&excludes)?;
    let mut bloom = BloomFilter::new(config.expected_records, config.false_positive_rate)?;
    let mut rng = ChaCha8Rng::seed_from_u64(config.seed);
    // streaming で集める（target>0 は reservoir でメモリ有界）。reservoir と shuffle は同一 rng。
    let (mut records, mut summary) = collect_records(
        &inputs,
        &exclude_keys,
        &mut bloom,
        config.target,
        total_input_records,
        &mut rng,
    )?;

    // reservoir の保持順は一様でないので、最終出力順を seed 固定で無作為化する。
    records.shuffle(&mut rng);

    summary.written = records.len() as u64;
    summary.chunks = write_chunks(&records, &config.out_dir, &config.prefix, config.chunk_records)?;
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
        let exclude_keys = HashSet::from([position_key(&excluded)]);
        let mut bloom = BloomFilter::new(100, 1e-9)?;
        let mut rng = ChaCha8Rng::seed_from_u64(0);

        let (records, summary) =
            collect_records(&[input], &exclude_keys, &mut bloom, 0, 3, &mut rng)?;

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

        let (records, summary) =
            collect_records(&[input], &HashSet::new(), &mut bloom, 0, 3, &mut rng)?;

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

        let (records, summary) =
            collect_records(&inputs, &HashSet::new(), &mut bloom, 4, recs.len() as u64, &mut rng)?;

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
            &HashSet::new(),
            &mut bloom2,
            4,
            recs.len() as u64,
            &mut rng2,
        )?;
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
}
