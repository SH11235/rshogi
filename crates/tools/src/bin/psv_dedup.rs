/// PSV ファイルの局面重複削除ツール
///
/// PackedSfen（先頭32バイト）が重複するレコードを除去し、
/// 最初に出現したレコードのみを保持する（first-wins 方式）。
///
/// Usage:
///   cargo run --release --bin psv_dedup -- \
///     --input /path/to/data.psv \
///     --output /path/to/deduped.psv
///
///   # ディレクトリ内の全 .psv を一括処理
///   cargo run --release --bin psv_dedup -- \
///     --input-dir /path/to/dir \
///     --output /path/to/deduped.psv
///
use std::{
    collections::HashSet,
    fs::File,
    io::{self, BufReader, BufWriter, Read, Write},
    path::PathBuf,
};

use clap::Parser;
use tools::common::dedup::{PSV_SIZE, SFEN_SIZE, collect_input_paths, hash_packed_sfen};

#[derive(Parser, Debug)]
#[command(name = "psv_dedup")]
struct Args {
    /// 入力 PSV ファイル（カンマ区切りで複数可）。--input-dir と排他
    #[arg(long)]
    input: Option<String>,

    /// 入力ディレクトリ。--pattern と組み合わせて使用。--input と排他
    #[arg(long)]
    input_dir: Option<PathBuf>,

    /// --input-dir 使用時の glob パターン
    #[arg(long, default_value = "*.psv")]
    pattern: String,

    /// 出力ファイルパス
    #[arg(long)]
    output: PathBuf,

    /// 処理する最大レコード数（0 = 全件）
    #[arg(long, default_value = "0")]
    max_positions: usize,
}

fn main() -> io::Result<()> {
    let args = Args::parse();

    let paths = collect_input_paths(args.input.as_deref(), args.input_dir.as_ref(), &args.pattern)?;
    if paths.is_empty() {
        eprintln!("入力ファイルが見つかりません");
        return Ok(());
    }

    // 入力と出力が同一パスでないことを確認
    let output_canonical = args.output.canonicalize().ok();
    for p in &paths {
        if let Ok(input_canonical) = p.canonicalize()
            && Some(&input_canonical) == output_canonical.as_ref()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("出力ファイルが入力ファイルと同一です: {}", p.display()),
            ));
        }
    }

    let out_file = File::create(&args.output)?;
    let mut writer = BufWriter::with_capacity(1 << 20, out_file);

    let mut seen: HashSet<u64> = HashSet::new();
    let mut total_records = 0u64;
    let mut written_records = 0u64;
    let mut buf = [0u8; PSV_SIZE];

    let start = std::time::Instant::now();

    for path in &paths {
        eprintln!("Reading: {}", path.display());
        let file = File::open(path)?;
        let mut reader = BufReader::with_capacity(1 << 20, file);

        loop {
            if args.max_positions > 0 && total_records >= args.max_positions as u64 {
                break;
            }

            match reader.read_exact(&mut buf) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e),
            }

            total_records += 1;

            let sfen: &[u8; SFEN_SIZE] = buf[..SFEN_SIZE].try_into().unwrap();
            let h = hash_packed_sfen(sfen);

            if seen.insert(h) {
                writer.write_all(&buf)?;
                written_records += 1;
            }

            if total_records.is_multiple_of(10_000_000) {
                let elapsed = start.elapsed().as_secs_f64();
                eprintln!(
                    "  {} M read, {} M written, {:.1} sec",
                    total_records / 1_000_000,
                    written_records / 1_000_000,
                    elapsed,
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

    println!("=== Dedup Summary ===");
    println!("Input records:   {total_records}");
    println!(
        "Output records:  {} ({:.2}%)",
        written_records,
        100.0 * written_records as f64 / total_records.max(1) as f64,
    );
    println!("Removed:         {removed} ({removed_pct:.2}%)");
    println!("Output file:     {}", args.output.display());
    println!("Elapsed:         {elapsed:.1} sec");

    Ok(())
}
