/// 大規模 PSV ファイルのブルームフィルタ重複除去ツール
///
/// PackedSfen（先頭32バイト）が重複するレコードを除去する。
/// 巨大なビット表で「見たことがある局面か」を近似判定することで、
/// 局面そのものを全件保持せずに数百億レコードを1パス処理できる。
/// `fpr` は false positive rate（偽陽性率）で、本当は新規の局面を
/// 誤って重複扱いする確率を表す。フィルタサイズは入力件数と `fpr`
/// から自動計算され、今回のような大規模入力では数十 GiB になる。
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

    /// メモリ不足でもブルームフィルタ確保を強制する
    #[arg(long)]
    force: bool,
}

/// Cache-line blocked ブルームフィルタ
///
/// 標準ブルームフィルタは k 回のプローブが全域に散らばるため、
/// フィルタサイズが L3 キャッシュを超えるとメモリレイテンシが支配的になる。
///
/// Blocked Bloom Filter は全域を 512 bit (= 64 bytes = 1 cache line) のブロックに分割し、
/// h1 でブロックを選択、h2 で同一ブロック内に k 個のビットを配置する。
/// 1レコードあたりのキャッシュミスが k 回 → 1 回に削減される。
///
/// 同じ総ビット数・同じ k であれば、ブロック内の負荷が均一な限り
/// FPR は標準ブルームフィルタとほぼ同等。
struct BloomFilter {
    blocks: Vec<u64>,
    num_blocks: u64,
    num_hashes: u32,
}

/// 1ブロック = 8 × u64 = 512 bits = 64 bytes = 1 cache line
const BLOCK_U64S: usize = 8;
const BLOCK_BITS: u32 = (BLOCK_U64S * 64) as u32; // 512

/// ブルームフィルタのサイズパラメータ（確保前に計算）
struct BloomParams {
    num_blocks: u64,
    num_hashes: u32,
    total_u64s: usize,
    size_bytes: u64,
}

impl BloomFilter {
    /// 必要なフィルタサイズを算出する（メモリ確保はしない）。
    fn estimate(num_elements: u64, fpr: f64) -> BloomParams {
        let n = num_elements as f64;
        // m = -n * ln(p) / (ln2)^2
        let m = (-n * fpr.ln() / (2.0_f64.ln().powi(2))).ceil() as u64;
        // k = (m/n) * ln2
        let k = ((m as f64 / n) * 2.0_f64.ln()).round().max(1.0) as u32;
        let num_blocks = m.div_ceil(BLOCK_BITS as u64);
        let total_u64s = num_blocks as usize * BLOCK_U64S;
        let size_bytes = total_u64s as u64 * 8;
        BloomParams {
            num_blocks,
            num_hashes: k,
            total_u64s,
            size_bytes,
        }
    }

    /// 算出済みパラメータでフィルタを確保する。
    fn allocate(params: &BloomParams) -> Self {
        let blocks = vec![0u64; params.total_u64s];
        Self {
            blocks,
            num_blocks: params.num_blocks,
            num_hashes: params.num_hashes,
        }
    }

    /// フィルタに挿入し、挿入前に既に存在していた可能性があるかを返す。
    ///
    /// - `true` = おそらく重複（偽陽性あり）
    /// - `false` = 確実に新規
    #[inline]
    fn insert_or_check(&mut self, sfen: &[u8; SFEN_SIZE]) -> bool {
        let (h1, h2) = hash_pair(sfen);

        // h1 でブロックを選択（1回のキャッシュミスで 512 bit をロード）
        let block_idx = (h1 % self.num_blocks) as usize;
        let block_offset = block_idx * BLOCK_U64S;

        // h2 からブロック内の k 個のプローブ位置を生成
        // h2a + i * h2b (mod 512) — h2b を奇数にして 512 との互いに素を保証
        let h2a = h2 as u32;
        let h2b = (h2 >> 32) as u32 | 1;

        let mut all_set = true;
        for i in 0..self.num_hashes {
            let bit_pos = h2a.wrapping_add(i.wrapping_mul(h2b)) % BLOCK_BITS;
            let word_in_block = (bit_pos >> 6) as usize; // bit_pos / 64
            let mask = 1u64 << (bit_pos & 63);
            // SAFETY: block_idx < num_blocks かつ word_in_block < BLOCK_U64S (8) なので
            // block_offset + word_in_block < blocks.len() が保証される。
            let w = unsafe { self.blocks.get_unchecked_mut(block_offset + word_in_block) };
            if *w & mask == 0 {
                all_set = false;
                *w |= mask;
            }
        }
        all_set
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

/// /proc/meminfo から MemAvailable をバイト単位で取得する。
/// 取得できない環境（非 Linux）では None を返す。
fn get_mem_available() -> Option<u64> {
    let content = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("MemAvailable:") {
            let kb_str = rest.trim().strip_suffix("kB")?.trim();
            let kb: u64 = kb_str.parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

fn format_gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
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
        && !parent.as_os_str().is_empty()
        && !parent.is_dir()
    {
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

    if total_expected == 0 {
        eprintln!("処理対象レコードが 0 件のため終了します");
        return Ok(());
    }

    // ブルームフィルタサイズ算出（確保前）
    let params = BloomFilter::estimate(total_expected, args.fpr);
    let elements_per_block = total_expected as f64 / params.num_blocks as f64;
    eprintln!(
        "Blocked Bloom filter: {} ({} blocks × {} bits, k={}, ~{:.1} elem/block), target FPR={:.4}%",
        format_gib(params.size_bytes),
        params.num_blocks,
        BLOCK_BITS,
        params.num_hashes,
        elements_per_block,
        args.fpr * 100.0,
    );

    // メモリ充足チェック
    if let Some(mem_available) = get_mem_available() {
        let threshold = (mem_available as f64 * 0.8) as u64;
        eprintln!(
            "  required: {} / available: {} (80% threshold: {})",
            format_gib(params.size_bytes),
            format_gib(mem_available),
            format_gib(threshold),
        );
        if params.size_bytes > threshold {
            if args.force {
                eprintln!("Warning: メモリ不足ですが --force が指定されているため続行します");
            } else {
                return Err(io::Error::other(format!(
                    "メモリ不足: フィルタに {} 必要ですが、利用可能メモリは {} です。\n\
                         対処法:\n\
                         - --fpr を緩める: --fpr 0.01 なら約 {} で済みます\n\
                         - --force で強制続行（swap 使用の可能性あり）",
                    format_gib(params.size_bytes),
                    format_gib(mem_available),
                    format_gib(BloomFilter::estimate(total_expected, 0.01).size_bytes),
                )));
            }
        }
    } else {
        eprintln!(
            "  required: {} (MemAvailable の取得不可、メモリチェックをスキップ)",
            format_gib(params.size_bytes),
        );
    }

    // ブルームフィルタ確保
    let mut bloom = BloomFilter::allocate(&params);
    eprintln!("Bloom filter allocated.");

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
