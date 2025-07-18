//! Command-line tool for converting YaneuraOu SFEN opening books to binary format

use anyhow::Result;
use clap::Parser;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::PathBuf;
use std::time::Instant;
use tools::opening_book::SfenParser;

#[derive(Parser, Debug)]
#[clap(
    author,
    version,
    about = "Convert YaneuraOu SFEN opening book to binary format"
)]
struct Args {
    /// Input SFEN file path
    #[clap(short, long)]
    input: PathBuf,

    /// Output binary file path
    #[clap(short, long)]
    output: PathBuf,

    /// Maximum moves from initial position (default: 50)
    #[clap(long, default_value = "50")]
    max_moves: usize,

    /// Minimum analysis depth (default: 0)
    #[clap(long, default_value = "0")]
    min_depth: u32,

    /// Minimum evaluation score (default: -1000)
    #[clap(long, default_value = "-1000")]
    min_eval: i32,

    /// Maximum evaluation score (default: 1000)
    #[clap(long, default_value = "1000")]
    max_eval: i32,

    /// Enable gzip compression
    #[clap(long)]
    compress: bool,

    /// Show progress every N positions (default: 10000)
    #[clap(long, default_value = "10000")]
    progress_interval: usize,

    /// Validate conversion by reading back the output
    #[clap(long)]
    validate: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();

    println!("Converting {} to {}", args.input.display(), args.output.display());
    println!("Filter settings:");
    println!("  Max moves: {}", args.max_moves);
    println!("  Min depth: {}", args.min_depth);
    println!("  Evaluation range: {} to {}", args.min_eval, args.max_eval);

    let start_time = Instant::now();

    // Open input file
    let input_file = File::open(&args.input)?;
    let file_size = input_file.metadata()?.len();
    println!("Input file size: {:.2} MB", file_size as f64 / 1_048_576.0);

    let reader = BufReader::new(input_file);

    // Parse SFEN entries
    let mut parser = SfenParser::new();
    let mut entries = Vec::new();
    let mut line_count = 0;
    let mut position_count = 0;

    println!("\nParsing SFEN file...");

    for line in reader.lines() {
        let line = line?;
        line_count += 1;

        match parser.parse_line(&line) {
            Ok(Some(entry)) => {
                entries.push(entry);
                position_count += 1;

                if position_count % args.progress_interval == 0 {
                    println!("  Parsed {position_count} positions from {line_count} lines");
                }
            }
            Ok(None) => {
                // Line processed but no complete entry yet
            }
            Err(e) => {
                eprintln!("Warning: Error parsing line {line_count}: {e}");
            }
        }
    }

    // Handle last entry if exists
    if let Ok(Some(entry)) = parser.parse_line("") {
        entries.push(entry);
        position_count += 1;
    }

    println!("Parsed {position_count} positions from {line_count} lines");

    // Apply filter and convert
    let filter = PositionFilter::new(args.max_moves, args.min_depth, args.min_eval, args.max_eval);

    println!("\nApplying filters and converting to binary...");

    let converter = BinaryConverter::new();
    let mut filtered_entries = Vec::new();
    let mut filtered_count = 0;

    for (i, mut entry) in entries.into_iter().enumerate() {
        if filter.filter_entry(&mut entry) {
            filtered_entries.push(entry);
            filtered_count += 1;
        }

        if (i + 1) % args.progress_interval == 0 {
            println!("  Processed {} positions, kept {}", i + 1, filtered_count);
        }
    }

    println!(
        "Filtered to {} positions ({:.1}%)",
        filtered_count,
        filtered_count as f64 / position_count as f64 * 100.0
    );

    // Write binary output
    let output_file = File::create(&args.output)?;
    let mut writer = BufWriter::new(output_file);

    println!("\nWriting binary output...");

    let stats = if args.compress {
        // Write to memory first, then compress
        let mut buffer = Vec::new();
        let stats = converter.write_binary(&filtered_entries, &mut buffer)?;

        println!("Uncompressed size: {} bytes", buffer.len());

        let compressed = converter.compress_data(&buffer)?;
        println!(
            "Compressed size: {} bytes ({:.1}% of original)",
            compressed.len(),
            compressed.len() as f64 / buffer.len() as f64 * 100.0
        );

        writer.write_all(&compressed)?;

        ConversionStats {
            positions_written: stats.positions_written,
            total_moves: stats.total_moves,
            bytes_written: compressed.len(),
            compression_ratio: compressed.len() as f64 / buffer.len() as f64,
        }
    } else {
        converter.write_binary(&filtered_entries, &mut writer)?
    };

    writer.flush()?;

    let output_size = std::fs::metadata(&args.output)?.len();
    let elapsed = start_time.elapsed();

    println!("\nConversion complete!");
    println!("Statistics:");
    println!("  Positions written: {}", stats.positions_written);
    println!("  Total moves: {}", stats.total_moves);
    println!("  Output file size: {:.2} MB", output_size as f64 / 1_048_576.0);
    println!(
        "  Size reduction: {:.1}%",
        (1.0 - output_size as f64 / file_size as f64) * 100.0
    );
    println!("  Time elapsed: {:.2}s", elapsed.as_secs_f64());
    println!(
        "  Processing speed: {:.0} positions/sec",
        position_count as f64 / elapsed.as_secs_f64()
    );

    // Validate if requested
    if args.validate {
        println!("\nValidating output...");
        validate_output(&args.output, args.compress, stats.positions_written)?;
        println!("Validation successful!");
    }

    Ok(())
}

fn validate_output(path: &PathBuf, compressed: bool, expected_count: usize) -> Result<()> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);

    let converter = BinaryConverter::new();

    let entries = if compressed {
        let mut compressed_data = Vec::new();
        reader.read_to_end(&mut compressed_data)?;

        let decompressed = converter.decompress_data(&compressed_data)?;
        let mut cursor = std::io::Cursor::new(decompressed);
        converter.read_binary(&mut cursor)?
    } else {
        converter.read_binary(&mut reader)?
    };

    if entries.len() != expected_count {
        anyhow::bail!(
            "Validation failed: expected {} entries, found {}",
            expected_count,
            entries.len()
        );
    }

    // Verify some entries can be decoded
    for (i, entry) in entries.iter().take(10).enumerate() {
        let best_move = MoveEncoder::decode_move(entry.header.best_move)?;
        println!(
            "  Entry {}: hash={:016x}, best_move={}, eval={}",
            i, entry.header.position_hash, best_move, entry.header.evaluation
        );
    }

    Ok(())
}
