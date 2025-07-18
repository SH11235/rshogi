//! Tool for verifying the contents of converted opening book binary files

use anyhow::Result;
use clap::Parser;
use engine_core::opening_book::*;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[clap(
    author,
    version,
    about = "Verify converted opening book binary against original SFEN"
)]
struct Args {
    /// Binary file to verify
    #[clap(short, long)]
    binary: PathBuf,

    /// Original SFEN file for comparison (optional)
    #[clap(short, long)]
    original: Option<PathBuf>,

    /// Number of entries to display (default: 10)
    #[clap(long, default_value = "10")]
    show_entries: usize,

    /// Show detailed move information
    #[clap(long)]
    detailed: bool,

    /// Check specific position (SFEN format)
    #[clap(long)]
    check_position: Option<String>,

    /// Export to readable text format
    #[clap(long)]
    export_txt: Option<PathBuf>,

    /// Show statistics only
    #[clap(long)]
    stats_only: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();

    println!("Verifying binary file: {}", args.binary.display());
    println!("{}", "=".repeat(60));

    // Load binary file
    let converter = BinaryConverter::new();
    let file = File::open(&args.binary)?;
    let mut reader = BufReader::new(file);

    // Check if compressed
    let mut magic = [0u8; 2];
    reader.read_exact(&mut magic)?;
    reader = BufReader::new(File::open(&args.binary)?); // Reset reader

    let entries = if magic == [0x1f, 0x8b] {
        // Gzip compressed
        println!("Detected gzip compressed file");
        let mut compressed_data = Vec::new();
        reader.read_to_end(&mut compressed_data)?;
        let decompressed = converter.decompress_data(&compressed_data)?;
        let mut cursor = std::io::Cursor::new(decompressed);
        converter.read_binary(&mut cursor)?
    } else {
        converter.read_binary(&mut reader)?
    };

    println!("Successfully loaded {} positions", entries.len());
    println!();

    // Show statistics
    show_statistics(&entries);

    if args.stats_only {
        return Ok(());
    }

    println!();

    // Check specific position if requested
    if let Some(check_pos) = args.check_position {
        check_specific_position(&check_pos, &entries)?;
        return Ok(());
    }

    // Show sample entries
    if args.show_entries > 0 {
        show_sample_entries(&entries, args.show_entries, args.detailed)?;
    }

    // Compare with original if provided
    if let Some(original_path) = args.original {
        println!("\nComparing with original file...");
        compare_with_original(&original_path, &entries)?;
    }

    // Export to text if requested
    if let Some(export_path) = args.export_txt {
        export_to_text(&entries, &export_path)?;
        println!("\nExported to: {}", export_path.display());
    }

    Ok(())
}

fn show_statistics(entries: &[BinaryEntry]) {
    let total_moves: usize = entries.iter().map(|e| e.moves.len()).sum();
    let avg_moves = if entries.is_empty() {
        0.0
    } else {
        total_moves as f64 / entries.len() as f64
    };

    let mut depth_distribution = HashMap::new();
    let mut eval_distribution = HashMap::new();

    for entry in entries {
        depth_distribution
            .entry(entry.header.depth)
            .and_modify(|c| *c += 1)
            .or_insert(1);

        let eval_bucket = (entry.header.evaluation / 100) * 100;
        eval_distribution.entry(eval_bucket).and_modify(|c| *c += 1).or_insert(1);
    }

    println!("Statistics:");
    println!("  Total positions: {}", entries.len());
    println!("  Total moves: {total_moves}");
    println!("  Average moves per position: {avg_moves:.2}");

    println!("\nDepth distribution:");
    let mut depths: Vec<_> = depth_distribution.into_iter().collect();
    depths.sort_by_key(|&(d, _)| d);
    for (depth, count) in depths {
        println!("  Depth {depth}: {count} positions");
    }

    println!("\nEvaluation distribution:");
    let mut evals: Vec<_> = eval_distribution.into_iter().collect();
    evals.sort_by_key(|&(e, _)| e);
    for (eval, count) in evals {
        println!("  [{} to {}]: {} positions", eval, eval + 99, count);
    }
}

fn show_sample_entries(entries: &[BinaryEntry], count: usize, detailed: bool) -> Result<()> {
    println!("Sample entries (showing first {}):", count.min(entries.len()));
    println!("{}", "-".repeat(60));

    for (i, entry) in entries.iter().take(count).enumerate() {
        println!("\nEntry #{}:", i + 1);
        println!("  Position hash: 0x{:016x}", entry.header.position_hash);

        // Decode best move
        let best_move = MoveEncoder::decode_move(entry.header.best_move)?;
        println!(
            "  Best move: {} (eval: {}, depth: {})",
            best_move, entry.header.evaluation, entry.header.depth
        );

        println!("  Number of moves: {}", entry.header.move_count);

        if detailed {
            println!("  All moves:");
            for (j, move_data) in entry.moves.iter().enumerate() {
                let move_str = MoveEncoder::decode_move(move_data.move_encoded)?;
                println!(
                    "    {}. {} (eval: {}, depth: {})",
                    j + 1,
                    move_str,
                    move_data.evaluation,
                    move_data.depth
                );
            }
        }
    }

    Ok(())
}

fn check_specific_position(sfen: &str, entries: &[BinaryEntry]) -> Result<()> {
    println!("Checking position: {sfen}");

    // Calculate hash
    let hash = PositionHasher::hash_position(sfen)?;
    println!("Position hash: 0x{hash:016x}");

    // Find in entries
    let found = entries.iter().find(|e| e.header.position_hash == hash);

    match found {
        Some(entry) => {
            println!("\nPosition FOUND in binary!");
            println!("Details:");

            let best_move = MoveEncoder::decode_move(entry.header.best_move)?;
            println!(
                "  Best move: {} (eval: {}, depth: {})",
                best_move, entry.header.evaluation, entry.header.depth
            );

            println!("  All {} moves:", entry.moves.len());
            for (i, move_data) in entry.moves.iter().enumerate() {
                let move_str = MoveEncoder::decode_move(move_data.move_encoded)?;
                println!(
                    "    {}. {} (eval: {}, depth: {})",
                    i + 1,
                    move_str,
                    move_data.evaluation,
                    move_data.depth
                );
            }
        }
        None => {
            println!("\nPosition NOT FOUND in binary!");
            println!("This position may have been filtered out during conversion.");
        }
    }

    Ok(())
}

fn compare_with_original(original_path: &PathBuf, binary_entries: &[BinaryEntry]) -> Result<()> {
    let file = File::open(original_path)?;
    let reader = BufReader::new(file);

    let mut parser = SfenParser::new();
    let mut original_entries = Vec::new();
    let mut line_count = 0;

    println!("Parsing original file...");

    for line in reader.lines() {
        let line = line?;
        line_count += 1;

        if let Ok(Some(entry)) = parser.parse_line(&line) {
            original_entries.push(entry);
        }

        if line_count % 100000 == 0 {
            print!(
                "\r  Processed {} lines, found {} positions",
                line_count,
                original_entries.len()
            );
            use std::io::Write;
            std::io::stdout().flush()?;
        }
    }

    // Handle last entry
    if let Ok(Some(entry)) = parser.parse_line("") {
        original_entries.push(entry);
    }

    println!("\r  Parsed {} positions from {} lines", original_entries.len(), line_count);

    // Create hash map for binary entries
    let binary_map: HashMap<u64, &BinaryEntry> =
        binary_entries.iter().map(|e| (e.header.position_hash, e)).collect();

    // Compare
    let mut matched = 0;
    let mut mismatched_moves = 0;
    let mut not_found = 0;

    for orig_entry in &original_entries {
        let hash = match PositionHasher::hash_position(&orig_entry.position) {
            Ok(h) => h,
            Err(_) => continue,
        };

        match binary_map.get(&hash) {
            Some(bin_entry) => {
                matched += 1;

                // Check move count
                if bin_entry.moves.len() != orig_entry.moves.len() {
                    mismatched_moves += 1;
                    if mismatched_moves <= 5 {
                        println!("\nMove count mismatch for position:");
                        println!(
                            "  Original: {} moves, Binary: {} moves",
                            orig_entry.moves.len(),
                            bin_entry.moves.len()
                        );
                    }
                }
            }
            None => {
                not_found += 1;
            }
        }
    }

    println!("\nComparison results:");
    println!("  Matched positions: {} / {}", matched, original_entries.len());
    println!("  Not found in binary: {not_found} (likely filtered out)");
    println!("  Positions with different move counts: {mismatched_moves}");

    let retention_rate = matched as f64 / original_entries.len() as f64 * 100.0;
    println!("  Retention rate: {retention_rate:.1}%");

    Ok(())
}

fn export_to_text(entries: &[BinaryEntry], output_path: &PathBuf) -> Result<()> {
    use std::io::Write;

    let mut file = std::fs::File::create(output_path)?;

    writeln!(file, "# Converted Opening Book")?;
    writeln!(file, "# Total positions: {}", entries.len())?;
    writeln!(file, "#")?;

    for (i, entry) in entries.iter().enumerate() {
        writeln!(file, "\n# Position {}", i + 1)?;
        writeln!(file, "# Hash: 0x{:016x}", entry.header.position_hash)?;

        let best_move = MoveEncoder::decode_move(entry.header.best_move)?;
        writeln!(
            file,
            "# Best: {} (eval: {}, depth: {})",
            best_move, entry.header.evaluation, entry.header.depth
        )?;

        writeln!(file, "# Moves:")?;
        for move_data in &entry.moves {
            let move_str = MoveEncoder::decode_move(move_data.move_encoded)?;
            writeln!(file, "{} eval={} depth={}", move_str, move_data.evaluation, move_data.depth)?;
        }
    }

    Ok(())
}
