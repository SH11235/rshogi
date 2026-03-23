/// 大規模 PSV ファイルのブルームフィルタ重複除去ツール
///
/// PackedSfen（先頭32バイト）が重複するレコードを除去する。
/// ブルームフィルタにより省メモリで数百億レコードを1パスで処理可能。
/// 偽陽性により微量（デフォルト 0.1%）の非重複レコードも除去され得るが、
/// 教師データの学習品質にはほぼ影響しない。
///
/// Usage:
///   cargo run --release --bin psv_dedup_bloom -- \
///     --input-dir ../bullet-shogi/data/DLSuisho15b \
///     --pattern "*.bin" \
///     --output /path/to/deduped.bin
///
///   # FPR を変更（デフォルト 0.001 = 0.1%）
///   cargo run --release --bin psv_dedup_bloom -- \
///     --input-dir /path/to/dir \
///     --output deduped.bin --fpr 0.0001
use std::{
    fs::File,
    io::{self, BufReader, BufWriter, Read, Write},
    path::PathBuf,
};

use clap::Parser;
use tools::common::dedup::{PSV_SIZE, SFEN_SIZE, check_output_not_in_inputs, collect_input_paths};

#[derive(Parser, Debug)]
#[command(
    name = "psv_dedup_bloom",
    about = "ブルームフィルタによる大規模 PSV 重複除去"
)]
struct Args {
    /// 入力 PSV ファイル（カンマ区切りで複数可）。--input-dir と排他
    #[arg(long)]
    input: Option<String>,

    /// 入力ディレクトリ。--pattern と組み合わせて使用。--input と排他
    #[arg(long)]
    input_dir: Option<PathBuf>,

    /// --input-dir 使用時の glob パターン
    #[arg(long, default_value = "*.bin")]
    pattern: String,

    /// 出力ファイルパス
    #[arg(long)]
    output: PathBuf,

    /// 偽陽性率 (0.0〜1.0)。デフォルト 0.001 = 0.1%
    #[arg(long, default_value = "0.001")]
    fpr: f64,

    /// 処理する最大レコード数（0 = 全件）
    #[arg(long, default_value = "0")]
    max_positions: u64,
}

/// 省メモリブルームフィルタ
///
/// Enhanced double hashing (Kirsch-Mitzenmacher) を使用。
/// 2つの独立した FNV-1a ハッシュから k 個のプローブ位置を生成する。
struct BloomFilter {
    bits: Vec<u64>,
    num_bits: u64,
    num_hashes: u32,
}

impl BloomFilter {
    /// `num_elements` 個の要素に対して偽陽性率 `fpr` を達成するフィルタを確保する。
    fn new(num_elements: u64, fpr: f64) -> Self {
        let n = num_elements as f64;
        // m = -n * ln(p) / (ln2)^2
        let m = (-n * fpr.ln() / (2.0_f64.ln().powi(2))).ceil() as u64;
        // k = (m/n) * ln2
        let k = ((m as f64 / n) * 2.0_f64.ln()).round().max(1.0) as u32;

        let num_u64s = m.div_ceil(64) as usize;
        let actual_bits = num_u64s as u64 * 64;
        let size_gb = num_u64s as f64 * 8.0 / (1024.0 * 1024.0 * 1024.0);
        eprintln!(
            "Bloom filter: {size_gb:.1} GB ({actual_bits} bits, k={k}), target FPR={:.4}%",
            fpr * 100.0,
        );

        let bits = vec![0u64; num_u64s];
        eprintln!("Bloom filter allocated.");

        Self {
            bits,
            num_bits: actual_bits,
            num_hashes: k,
        }
    }

    /// フィルタに挿入し、挿入前に既に存在していた可能性があるかを返す。
    ///
    /// - `true` = おそらく重複（偽陽性あり）
    /// - `false` = 確実に新規
    #[inline]
    fn insert_or_check(&mut self, sfen: &[u8; SFEN_SIZE]) -> bool {
        let (h1, h2) = hash_pair(sfen);
        let mut all_set = true;
        for i in 0..self.num_hashes {
            let idx = self.probe_index(h1, h2, i);
            let word = (idx >> 6) as usize; // idx / 64
            let mask = 1u64 << (idx & 63); // idx % 64
            // SAFETY: probe_index は idx % self.num_bits を返し、
            // num_bits == self.bits.len() * 64 なので word < self.bits.len() が保証される。
            let w = unsafe { self.bits.get_unchecked_mut(word) };
            if *w & mask == 0 {
                all_set = false;
                *w |= mask;
            }
        }
        all_set
    }

    /// Enhanced double hashing: h_i = h1 + i*h2 + i*(i-1)/2 (mod num_bits)
    #[inline(always)]
    fn probe_index(&self, h1: u64, h2: u64, i: u32) -> u64 {
        let i = i as u64;
        h1.wrapping_add(i.wrapping_mul(h2))
            .wrapping_add(i.wrapping_mul(i.wrapping_sub(1)) >> 1)
            % self.num_bits
    }
}

/// PackedSfen から2つの独立した 64bit FNV-1a ハッシュを生成する。
/// h2 は奇数にして double hashing の分布を改善する。
#[inline]
fn hash_pair(sfen: &[u8; SFEN_SIZE]) -> (u64, u64) {
    let mut h1: u64 = 0xcbf29ce484222325; // FNV offset basis
    let mut h2: u64 = 0x6c62272e07bb0142; // 異なる初期値
    for &b in sfen.iter() {
        h1 ^= b as u64;
        h1 = h1.wrapping_mul(0x100000001b3);
        h2 ^= b as u64;
        h2 = h2.wrapping_mul(0x100000001b3);
    }
    (h1, h2 | 1)
}

fn main() -> io::Result<()> {
    let args = Args::parse();

    if args.fpr <= 0.0 || args.fpr >= 1.0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--fpr は 0.0〜1.0 の間で指定してください",
        ));
    }

    let paths = collect_input_paths(args.input.as_deref(), args.input_dir.as_ref(), &args.pattern)?;
    if paths.is_empty() {
        eprintln!("入力ファイルが見つかりません");
        return Ok(());
    }

    // 入力と出力の重複チェック（出力ファイルが未作成でも検出可能）
    check_output_not_in_inputs(&args.output, &paths)?;

    // 出力先の親ディレクトリ存在確認（ブルームフィルタ確保前に検出する）
    if let Some(parent) = args.output.parent()
        && !parent.as_os_str().is_empty() && !parent.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("出力先の親ディレクトリが存在しません: {}", parent.display()),
            ));
        }

    // ファイルサイズからレコード数を算出
    let mut total_expected: u64 = 0;
    for p in &paths {
        let sz = std::fs::metadata(p)?.len();
        let n = sz / PSV_SIZE as u64;
        if sz % PSV_SIZE as u64 != 0 {
            eprintln!(
                "Warning: {} のサイズが {} の倍数ではありません (残余 {} bytes)",
                p.display(),
                PSV_SIZE,
                sz % PSV_SIZE as u64,
            );
        }
        total_expected += n;
        eprintln!("  {}: {n} records ({:.2} GB)", p.display(), sz as f64 / 1e9);
    }
    if args.max_positions > 0 && args.max_positions < total_expected {
        total_expected = args.max_positions;
    }
    eprintln!("Total expected records: {total_expected}");

    // ブルームフィルタ確保
    let mut bloom = BloomFilter::new(total_expected, args.fpr);

    // 出力
    let out_file = File::create(&args.output)?;
    let mut writer = BufWriter::with_capacity(8 << 20, out_file);

    let mut total_records = 0u64;
    let mut written_records = 0u64;
    let mut buf = [0u8; PSV_SIZE];
    let start = std::time::Instant::now();

    for path in &paths {
        eprintln!("Reading: {}", path.display());
        let file = File::open(path)?;
        let mut reader = BufReader::with_capacity(8 << 20, file);

        loop {
            if args.max_positions > 0 && total_records >= args.max_positions {
                break;
            }

            match reader.read_exact(&mut buf) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e),
            }

            total_records += 1;

            let sfen: &[u8; SFEN_SIZE] = buf[..SFEN_SIZE].try_into().unwrap();
            if !bloom.insert_or_check(sfen) {
                writer.write_all(&buf)?;
                written_records += 1;
            }

            if total_records.is_multiple_of(100_000_000) {
                let elapsed = start.elapsed().as_secs_f64();
                let speed = total_records as f64 / elapsed / 1e6;
                let remaining = (total_expected - total_records) as f64 / (speed * 1e6);
                eprintln!(
                    "  {:.0}M read, {:.0}M written, {:.1}s ({:.1}M rec/s, ETA {:.0}s)",
                    total_records as f64 / 1e6,
                    written_records as f64 / 1e6,
                    elapsed,
                    speed,
                    remaining,
                );
            }
        }
    }

    writer.flush()?;

    let elapsed = start.elapsed().as_secs_f64();
    let removed = total_records - written_records;
    let removed_pct = if total_records > 0 {
        100.0 * removed as f64 / total_records as f64
    } else {
        0.0
    };

    println!("=== Bloom Dedup Summary ===");
    println!("Input records:   {total_records}");
    println!(
        "Output records:  {written_records} ({:.2}%)",
        100.0 * written_records as f64 / total_records.max(1) as f64,
    );
    println!("Removed:         {removed} ({removed_pct:.4}%)");
    println!("FPR setting:     {:.4}%", args.fpr * 100.0);
    println!("Output file:     {}", args.output.display());
    println!("Elapsed:         {elapsed:.1} sec");

    Ok(())
}
