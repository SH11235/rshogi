/// PSV ファイルの局面重複チェックツール
///
/// PackedSfen（先頭32バイト）の重複を検出し、統計を出力する。
///
/// Usage:
///   cargo run --release --example psv_dedup_check -- \
///     --data /path/to/data.psv \
///     [--max-positions 0]       # 0 = 全件
///     [--by-ply]                # ply別の重複統計を表示
///
use std::{
    collections::HashMap,
    fs::File,
    io::{self, BufReader, Read},
    path::PathBuf,
};

use clap::Parser;

const PSV_SIZE: usize = 40;
const SFEN_SIZE: usize = 32;

#[derive(Parser, Debug)]
#[command(name = "psv_dedup_check")]
struct Args {
    /// PSV data files (comma-separated)
    #[arg(long)]
    data: String,

    /// Max positions to check (0 = all)
    #[arg(long, default_value = "0")]
    max_positions: usize,

    /// Show per-ply duplicate statistics
    #[arg(long)]
    by_ply: bool,
}

/// PackedSfen の 64bit ハッシュ（FxHash ライク）
fn hash_sfen(sfen: &[u8; SFEN_SIZE]) -> u64 {
    // FNV-1a 64bit
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in sfen.iter() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// PSV レコードから game_ply を取得（offset 36, u16 LE）
fn game_ply(record: &[u8; PSV_SIZE]) -> u16 {
    u16::from_le_bytes([record[36], record[37]])
}

fn main() -> io::Result<()> {
    let args = Args::parse();

    let paths: Vec<PathBuf> = args
        .data
        .split(',')
        .map(|s| PathBuf::from(s.trim()))
        .filter(|p| p.exists())
        .collect();

    if paths.is_empty() {
        eprintln!("No valid data files found");
        return Ok(());
    }

    // 64bit ハッシュ → 出現回数
    // 5700万エントリ × 16バイト(hash+count) ≈ 912MB（実際はハッシュマップオーバーヘッドで1.5GB程度）
    let mut seen: HashMap<u64, u32> = HashMap::new();

    // ply 別統計（by_ply 有効時のみ）
    let mut ply_total: HashMap<u16, u64> = HashMap::new();
    let mut ply_dup: HashMap<u16, u64> = HashMap::new();

    let mut total_records = 0u64;
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
            let h = hash_sfen(sfen);
            let count = seen.entry(h).or_insert(0);
            *count += 1;

            if args.by_ply {
                let ply = game_ply(&buf);
                *ply_total.entry(ply).or_insert(0) += 1;
                if *count > 1 {
                    *ply_dup.entry(ply).or_insert(0) += 1;
                }
            }

            if total_records.is_multiple_of(10_000_000) {
                let elapsed = start.elapsed().as_secs_f64();
                eprintln!(
                    "  {} M records, {} M unique, {:.1} sec",
                    total_records / 1_000_000,
                    seen.len() / 1_000_000,
                    elapsed,
                );
            }
        }
    }

    let elapsed = start.elapsed().as_secs_f64();
    let unique = seen.len() as u64;
    let duplicates = total_records - unique;
    let dup_pct = if total_records > 0 {
        100.0 * duplicates as f64 / total_records as f64
    } else {
        0.0
    };

    // 重複回数の分布
    let mut freq: HashMap<u32, u64> = HashMap::new();
    for &count in seen.values() {
        *freq.entry(count).or_insert(0) += 1;
    }
    let mut freq_sorted: Vec<(u32, u64)> = freq.into_iter().collect();
    freq_sorted.sort();

    println!("=== Duplicate Check Summary ===");
    println!("Total records:  {}", total_records);
    println!(
        "Unique sfens:   {} ({:.2}%)",
        unique,
        100.0 * unique as f64 / total_records as f64
    );
    println!("Duplicate recs: {} ({:.2}%)", duplicates, dup_pct);
    println!("Elapsed:        {:.1} sec", elapsed);
    println!();

    println!("=== Occurrence Distribution ===");
    println!("{:>10} {:>12} {:>12}", "count", "sfens", "records");
    for (count, sfens) in &freq_sorted {
        let records = *count as u64 * *sfens;
        println!("{:>10} {:>12} {:>12}", count, sfens, records);
        // 10行超えたら省略
        if freq_sorted.len() > 15 && *count > 10 {
            let remaining: u64 =
                freq_sorted.iter().filter(|(c, _)| *c > *count).map(|(_c, s)| *s).sum();
            if remaining > 0 {
                println!("{:>10} {:>12}", "...", remaining);
            }
            break;
        }
    }

    if args.by_ply {
        println!();
        println!("=== Per-Ply Duplicate Rate ===");
        println!("{:>6} {:>10} {:>10} {:>8}", "ply", "total", "dup", "dup%");
        let mut plies: Vec<u16> = ply_total.keys().copied().collect();
        plies.sort();
        for ply in plies.iter().take(50) {
            let t = ply_total.get(ply).copied().unwrap_or(0);
            let d = ply_dup.get(ply).copied().unwrap_or(0);
            let pct = if t > 0 {
                100.0 * d as f64 / t as f64
            } else {
                0.0
            };
            println!("{:>6} {:>10} {:>10} {:>7.2}%", ply, t, d, pct);
        }
        if plies.len() > 50 {
            println!("  ... ({} plies total)", plies.len());
        }
    }

    Ok(())
}
