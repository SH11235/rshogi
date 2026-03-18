/// PSV ファイルの局面重複チェックツール
///
/// PackedSfen（先頭32バイト）の重複を検出し、統計を出力する。
///
/// Usage:
///   # ファイル指定（従来互換）
///   psv_dedup_check --data /path/to/data.psv [--by-ply]
///
///   # ディレクトリ指定
///   psv_dedup_check --input-dir /path/to/dir --pattern "*.bin" [--by-ply]
///
///   # 大規模データ向け近似モード（固定メモリ）
///   psv_dedup_check --input-dir /path/to/dir --pattern "*.bin" --table-size 4G
///
use std::{
    collections::HashMap,
    fs::File,
    io::{self, BufReader, Read},
    path::PathBuf,
};

use clap::Parser;
use tools::common::dedup::{
    PSV_SIZE, SFEN_SIZE, collect_input_paths, game_ply_from_record, hash_packed_sfen,
};

#[derive(Parser, Debug)]
#[command(name = "psv_dedup_check")]
struct Args {
    /// PSV data files (comma-separated)
    #[arg(long)]
    data: Option<String>,

    /// 入力ディレクトリ。--pattern と組み合わせて使用。--data と排他
    #[arg(long)]
    input_dir: Option<PathBuf>,

    /// --input-dir 使用時の glob パターン
    #[arg(long, default_value = "*.bin")]
    pattern: String,

    /// Max positions to check (0 = all)
    #[arg(long, default_value = "0")]
    max_positions: usize,

    /// Show per-ply duplicate statistics
    #[arg(long)]
    by_ply: bool,

    /// 近似モード: direct-mapped テーブルサイズ（例: 1G, 4G, 512M）。
    /// 指定時は固定メモリで近似的な重複チェックを行う。
    /// 未指定時は HashMap による正確なチェック（大規模データではメモリ不足になる）。
    #[arg(long, value_parser = parse_size_suffix)]
    table_size: Option<u64>,
}

/// "4G", "512M", "1G" 等のサイズ指定をパースしてエントリ数に変換する
fn parse_size_suffix(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let (num_str, multiplier) = if let Some(n) = s.strip_suffix('G') {
        (n, 1024 * 1024 * 1024u64)
    } else if let Some(n) = s.strip_suffix('M') {
        (n, 1024 * 1024u64)
    } else if let Some(n) = s.strip_suffix('K') {
        (n, 1024u64)
    } else {
        (s, 1u64)
    };
    let num: u64 = num_str.parse().map_err(|_| format!("無効なサイズ指定: {s}"))?;
    Ok(num * multiplier)
}

/// Direct-mapped テーブルによる近似重複チェック（SharedDedupHash と同方式）
struct ApproxDedupTable {
    table: Vec<u64>,
    mask: u64,
}

impl ApproxDedupTable {
    fn new(size: u64) -> Self {
        let size = size.next_power_of_two();
        let table = vec![0u64; size as usize];
        eprintln!(
            "Approximate mode: {} entries ({:.1} GB)",
            size,
            size as f64 * 8.0 / (1024.0 * 1024.0 * 1024.0),
        );
        Self {
            table,
            mask: size - 1,
        }
    }

    /// 重複なら true、新規なら挿入して false を返す。
    /// eviction（上書き）により古いエントリが消えるため近似的。
    fn check_and_insert(&mut self, key: u64) -> bool {
        let effective_key = if key == 0 { 1 } else { key };
        let idx = (effective_key & self.mask) as usize;
        let old = self.table[idx];
        if old == effective_key {
            return true;
        }
        self.table[idx] = effective_key;
        false
    }

    /// テーブル内の非ゼロエントリ数（近似ユニーク数）
    fn occupied(&self) -> u64 {
        self.table.iter().filter(|&&v| v != 0).count() as u64
    }
}

fn main() -> io::Result<()> {
    let args = Args::parse();

    let paths = collect_input_paths(args.data.as_deref(), args.input_dir.as_ref(), &args.pattern)?;

    if paths.is_empty() {
        eprintln!("No valid data files found");
        return Ok(());
    }

    eprintln!("{} files found", paths.len());

    let use_approx = args.table_size.is_some();

    // 近似モード
    let mut approx_table = args.table_size.map(ApproxDedupTable::new);

    // 正確モード
    let mut exact_seen: HashMap<u64, u32> = HashMap::new();

    // ply 別統計（by_ply 有効時のみ）
    let mut ply_total: HashMap<u16, u64> = HashMap::new();
    let mut ply_dup: HashMap<u16, u64> = HashMap::new();

    let mut total_records = 0u64;
    let mut dup_hits = 0u64;
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

            let is_dup = if let Some(ref mut table) = approx_table {
                table.check_and_insert(h)
            } else {
                let count = exact_seen.entry(h).or_insert(0);
                *count += 1;
                *count > 1
            };

            if is_dup {
                dup_hits += 1;
            }

            if args.by_ply {
                let ply = game_ply_from_record(&buf);
                *ply_total.entry(ply).or_insert(0) += 1;
                if is_dup {
                    *ply_dup.entry(ply).or_insert(0) += 1;
                }
            }

            if total_records.is_multiple_of(100_000_000) {
                let elapsed = start.elapsed().as_secs_f64();
                if use_approx {
                    eprintln!(
                        "  {} M records, {} M dup hits, {:.1} sec",
                        total_records / 1_000_000,
                        dup_hits / 1_000_000,
                        elapsed,
                    );
                } else {
                    eprintln!(
                        "  {} M records, {} M unique, {:.1} sec",
                        total_records / 1_000_000,
                        exact_seen.len() / 1_000_000,
                        elapsed,
                    );
                }
            }
        }
    }

    let elapsed = start.elapsed().as_secs_f64();

    if use_approx {
        let table = approx_table.as_ref().unwrap();
        let occupied = table.occupied();
        let table_size = table.mask + 1;
        let load_factor = occupied as f64 / table_size as f64;

        println!("=== Approximate Duplicate Check Summary ===");
        println!("Total records:    {total_records}");
        println!(
            "Duplicate hits:   {dup_hits} ({:.2}%)",
            100.0 * dup_hits as f64 / total_records.max(1) as f64
        );
        println!(
            "Non-dup records:  {} ({:.2}%)",
            total_records - dup_hits,
            100.0 * (total_records - dup_hits) as f64 / total_records.max(1) as f64
        );
        println!("Table occupied:   {occupied} / {table_size} (load {:.2}%)", load_factor * 100.0);
        if load_factor > 0.5 {
            println!(
                "WARNING: load factor > 50% — eviction が多く、重複の見逃しが増えています。--table-size を大きくしてください。"
            );
        }
        println!("Elapsed:          {elapsed:.1} sec");
        println!();
        println!(
            "NOTE: direct-mapped テーブルの近似値です。eviction により重複を見逃す場合があります。"
        );
    } else {
        let unique = exact_seen.len() as u64;
        let duplicates = total_records - unique;
        let dup_pct = if total_records > 0 {
            100.0 * duplicates as f64 / total_records as f64
        } else {
            0.0
        };

        // 重複回数の分布
        let mut freq: HashMap<u32, u64> = HashMap::new();
        for &count in exact_seen.values() {
            *freq.entry(count).or_insert(0) += 1;
        }
        let mut freq_sorted: Vec<(u32, u64)> = freq.into_iter().collect();
        freq_sorted.sort();

        println!("=== Duplicate Check Summary ===");
        println!("Total records:  {total_records}");
        println!(
            "Unique sfens:   {} ({:.2}%)",
            unique,
            100.0 * unique as f64 / total_records as f64
        );
        println!("Duplicate recs: {} ({:.2}%)", duplicates, dup_pct);
        println!("Elapsed:        {elapsed:.1} sec");
        println!();

        println!("=== Occurrence Distribution ===");
        println!("{:>10} {:>12} {:>12}", "count", "sfens", "records");
        for (count, sfens) in &freq_sorted {
            let records = *count as u64 * *sfens;
            println!("{:>10} {:>12} {:>12}", count, sfens, records);
            if freq_sorted.len() > 15 && *count > 10 {
                let remaining: u64 =
                    freq_sorted.iter().filter(|(c, _)| *c > *count).map(|(_c, s)| *s).sum();
                if remaining > 0 {
                    println!("{:>10} {:>12}", "...", remaining);
                }
                break;
            }
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
