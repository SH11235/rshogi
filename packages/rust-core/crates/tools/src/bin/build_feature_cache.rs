//! Feature cache builder for NNUE training
//!
//! This tool converts JSONL training data into a binary cache format
//! with pre-extracted HalfKP features for faster training.

use std::fs::{create_dir_all, File};
use std::io::{BufRead, BufReader, BufWriter, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::time::Instant;

use clap::{arg, Command};
use engine_core::{evaluation::nnue::features::extract_features, Color, Position};
use serde::Deserialize;

const CACHE_VERSION: u32 = 1;
const FEATURE_SET_ID: u32 = 0x48414C46; // "HALF" for HalfKP

#[derive(Debug)]
struct CacheConfig {
    label_type: String,
    scale: f32,
    cp_clip: i32,
    chunk_size: u32,
    exclude_no_legal_move: bool,
    exclude_fallback: bool,
}

#[allow(dead_code)]
#[derive(Debug)]
struct CacheHeader {
    magic: [u8; 4],      // "NNFC" (NNUE Feature Cache)
    version: u32,        // Cache format version
    feature_set_id: u32, // Feature set identifier
    num_samples: u64,    // Total number of samples
    chunk_size: u32,     // Samples per chunk for shuffling
    reserved: [u8; 16],  // Reserved for future use
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct TrainingPosition {
    sfen: String,
    #[serde(default)]
    lines: Vec<LineInfo>,
    #[serde(default)]
    best2_gap_cp: Option<i32>,
    #[serde(default)]
    bound1: Option<String>,
    #[serde(default)]
    bound2: Option<String>,
    #[serde(default)]
    mate_boundary: Option<bool>,
    #[serde(default)]
    no_legal_move: Option<bool>,
    #[serde(default)]
    fallback_used: Option<bool>,
    #[serde(default)]
    eval: Option<i32>,
    #[serde(default)]
    depth: Option<u8>,
    #[serde(default)]
    seldepth: Option<u8>,
    #[serde(default)]
    nodes: Option<u64>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct LineInfo {
    #[serde(default)]
    score_cp: Option<i32>,
}

// Removed CachedSample and SampleMetadata structs as we're now streaming directly

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let app = Command::new("build_feature_cache")
        .about("Build feature cache from JSONL training data")
        .arg(arg!(-i --input <FILE> "Input JSONL file").required(true))
        .arg(arg!(-o --output <FILE> "Output cache file").required(true))
        .arg(arg!(-l --label <TYPE> "Label type: wdl, cp").default_value("wdl"))
        .arg(arg!(--scale <N> "Scale for cp->wdl conversion").default_value("600"))
        .arg(arg!(--"cp-clip" <N> "Clip CP values to this range").default_value("1200"))
        .arg(arg!(--"chunk-size" <N> "Samples per chunk").default_value("16384"))
        .arg(arg!(--"exclude-no-legal-move" "Exclude positions with no legal moves"))
        .arg(arg!(--"exclude-fallback" "Exclude positions where fallback was used"))
        .arg(arg!(--compress "Use zstd compression (future)"))
        .get_matches();

    let input_path = app.get_one::<String>("input").unwrap();
    let output_path = app.get_one::<String>("output").unwrap();
    let label_type = app.get_one::<String>("label").unwrap();
    let scale: f32 = app.get_one::<String>("scale").unwrap().parse()?;
    let cp_clip: i32 = app.get_one::<String>("cp-clip").unwrap().parse()?;
    let chunk_size: u32 = app.get_one::<String>("chunk-size").unwrap().parse()?;
    let exclude_no_legal_move = app.get_flag("exclude-no-legal-move");
    let exclude_fallback = app.get_flag("exclude-fallback");
    let compress = app.get_flag("compress");

    println!("Building feature cache:");
    println!("  Input: {}", input_path);
    println!("  Output: {}", output_path);
    println!("  Label type: {}", label_type);
    println!("  Chunk size: {}", chunk_size);
    if compress {
        println!("  Compression: enabled (not implemented yet)");
    }

    let start_time = Instant::now();

    // Create output directory if needed
    if let Some(parent) = PathBuf::from(output_path).parent() {
        create_dir_all(parent)?;
    }

    // Write cache file with streaming
    println!("\nProcessing and writing cache file...");
    let write_start = Instant::now();
    let config = CacheConfig {
        label_type: label_type.to_string(),
        scale,
        cp_clip,
        chunk_size,
        exclude_no_legal_move,
        exclude_fallback,
    };

    let (num_samples, total_features) =
        write_cache_file_streaming(input_path, output_path, &config)?;

    println!(
        "\nProcessed {} samples in {:.2}s",
        num_samples,
        write_start.elapsed().as_secs_f32()
    );

    println!("\nTotal time: {:.2}s", start_time.elapsed().as_secs_f32());

    // Print statistics
    let avg_features = if num_samples > 0 {
        total_features as f32 / num_samples as f32
    } else {
        0.0
    };
    println!("\nStatistics:");
    println!("  Total samples: {}", num_samples);
    println!("  Average features per sample: {:.1}", avg_features);
    println!(
        "  Cache file size: {} MB",
        std::fs::metadata(output_path)?.len() / (1024 * 1024)
    );

    Ok(())
}

fn cp_to_wdl(cp: i32, scale: f32) -> f32 {
    1.0 / (1.0 + (-cp as f32 / scale).exp())
}

fn write_cache_file_streaming(
    input_path: &str,
    output_path: &str,
    config: &CacheConfig,
) -> Result<(u64, u64), Box<dyn std::error::Error>> {
    let mut writer = BufWriter::new(File::create(output_path)?);

    // Write placeholder header
    writer.write_all(b"NNFC")?;
    let header_pos = writer.stream_position()?;
    writer.write_all(&[0u8; 4 + 4 + 8 + 4 + 16])?; // version, feature_set_id, num_samples, chunk_size, reserved
    writer.flush()?;

    // Process samples directly from input to output
    let file = File::open(input_path)?;
    let reader = BufReader::new(file);
    let mut num_samples: u64 = 0;
    let mut total_features: u64 = 0;
    let mut skipped = 0;
    let mut processed = 0;

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        processed += 1;
        if processed % 10000 == 0 {
            print!("\rProcessed {} positions...", processed);
            std::io::stdout().flush()?;
        }

        let pos_data: TrainingPosition = match serde_json::from_str(&line) {
            Ok(data) => data,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };

        // Skip based on filters
        if config.exclude_no_legal_move && pos_data.no_legal_move.unwrap_or(false) {
            skipped += 1;
            continue;
        }
        if config.exclude_fallback && pos_data.fallback_used.unwrap_or(false) {
            skipped += 1;
            continue;
        }

        // Get evaluation score
        let cp = if let Some(eval) = pos_data.eval {
            eval
        } else if let Some(line) = pos_data.lines.first() {
            line.score_cp.unwrap_or(0)
        } else {
            skipped += 1;
            continue;
        };

        // Create position
        let position = match Position::from_sfen(&pos_data.sfen) {
            Ok(pos) => pos,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };

        // Extract HalfKP features for both perspectives
        let black_king = match position.board.king_square(Color::Black) {
            Some(sq) => sq,
            None => {
                skipped += 1;
                continue;
            }
        };
        let white_king = match position.board.king_square(Color::White) {
            Some(sq) => sq,
            None => {
                skipped += 1;
                continue;
            }
        };

        let mut features = Vec::new();

        // Black perspective
        let black_features = extract_features(&position, black_king, Color::Black);
        features.extend(black_features.as_slice().iter().map(|&f| f as u32));

        // White perspective
        let white_features = extract_features(&position, white_king, Color::White);
        features.extend(white_features.as_slice().iter().map(|&f| f as u32));

        // Calculate label
        let label = match config.label_type.as_str() {
            "wdl" => cp_to_wdl(cp, config.scale),
            "cp" => (cp.clamp(-config.cp_clip, config.cp_clip) as f32) / 100.0,
            _ => continue,
        };

        // Build metadata
        let mut flags = 0u8;

        // Check exact bounds
        let both_exact = pos_data.bound1.as_deref() == Some("Exact")
            && pos_data.bound2.as_deref() == Some("Exact");
        if both_exact {
            flags |= 1 << 0;
        }

        // Check mate boundary
        if pos_data.mate_boundary.unwrap_or(false) {
            flags |= 1 << 1;
        }

        // Write sample directly to file
        let n_features = features.len() as u32;
        writer.write_all(&n_features.to_le_bytes())?;

        for &feat in &features {
            writer.write_all(&feat.to_le_bytes())?;
        }

        writer.write_all(&label.to_le_bytes())?;
        let gap = pos_data.best2_gap_cp.unwrap_or(0).clamp(0, u16::MAX as i32) as u16;
        writer.write_all(&gap.to_le_bytes())?;
        writer.write_all(&[pos_data.depth.unwrap_or(0)])?;
        writer.write_all(&[pos_data.seldepth.unwrap_or(0)])?;
        writer.write_all(&[flags])?;
        writer.write_all(&[0u8])?; // padding

        num_samples += 1;
        total_features += features.len() as u64;
    }

    println!("\rProcessed {} positions (skipped {})", processed, skipped);
    writer.flush()?;
    drop(writer);

    // Reopen and update header
    let mut file = File::options().write(true).open(output_path)?;
    file.seek(SeekFrom::Start(header_pos))?;
    file.write_all(&CACHE_VERSION.to_le_bytes())?;
    file.write_all(&FEATURE_SET_ID.to_le_bytes())?;
    file.write_all(&num_samples.to_le_bytes())?;
    file.write_all(&config.chunk_size.to_le_bytes())?;
    file.write_all(&[0u8; 16])?; // reserved

    Ok((num_samples, total_features))
}
