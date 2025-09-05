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

// Cache format version (v1)
// - Two-sample (black + white perspectives) per position
// - Oriented labels (black baseline)
// - No meta padding
// - Extended header with header_size/payload fields for forward compatibility
const CACHE_VERSION: u32 = 1;
const FEATURE_SET_ID: u32 = 0x48414C46; // "HALF" for HalfKP

// Header constants (v1). Bytes after magic ("NNFC").
const HEADER_SIZE: u32 = 48;

// Flags (shared across versions)
const FLAG_BOTH_EXACT: u8 = 1 << 0;
const FLAG_MATE_BOUNDARY: u8 = 1 << 1;
// Additional flags (reader may ignore unknown bits):
const FLAG_PERSPECTIVE_BLACK: u8 = 1 << 2;
const FLAG_STM_BLACK: u8 = 1 << 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PayloadEncodingKind {
    None,
    Gzip,
    #[cfg(feature = "zstd")]
    Zstd,
}

impl PayloadEncodingKind {
    fn code(self) -> u8 {
        match self {
            PayloadEncodingKind::None => 0,
            PayloadEncodingKind::Gzip => 1,
            #[cfg(feature = "zstd")]
            PayloadEncodingKind::Zstd => 2,
        }
    }
}

#[derive(Debug)]
struct CacheConfig {
    label_type: String,
    scale: f32,
    cp_clip: i32,
    chunk_size: u32,
    exclude_no_legal_move: bool,
    exclude_fallback: bool,
    payload_encoding: PayloadEncodingKind,
    compress_level: Option<i32>,
    dedup_features: bool,
}

// No concrete header struct; header is written field-by-field for stability.

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
        .arg(arg!(--compress "Enable payload compression"))
        .arg(arg!(--"compressor" <KIND> "Compressor kind: gz|zst (default gz when --compress)").required(false))
        .arg(arg!(--"compress-level" <N> "Compression level (gz: 0-9, zst: e.g. 1-19)").required(false))
        .arg(arg!(--"dedup-features" "Sort & deduplicate active features per sample (slower)"))
        .get_matches();

    let input_path = app.get_one::<String>("input").unwrap();
    let output_path = app.get_one::<String>("output").unwrap();
    let label_type = app.get_one::<String>("label").unwrap();
    let scale: f32 = app.get_one::<String>("scale").unwrap().parse()?;
    let cp_clip: i32 = app.get_one::<String>("cp-clip").unwrap().parse()?;
    let chunk_size: u32 = app.get_one::<String>("chunk-size").unwrap().parse()?;
    let exclude_no_legal_move = app.get_flag("exclude-no-legal-move");
    let exclude_fallback = app.get_flag("exclude-fallback");
    let compress_flag = app.get_flag("compress");
    let compressor_kind = app
        .get_one::<String>("compressor")
        .map(|s| s.to_ascii_lowercase());

    println!("Building feature cache:");
    println!("  Input: {}", input_path);
    println!("  Output: {}", output_path);
    println!("  Label type: {}", label_type);
    println!("  Chunk size: {}", chunk_size);
    let payload_encoding = if compress_flag {
        match compressor_kind.as_deref() {
            Some("gz") | None => {
                println!("  Compression: gzip");
                PayloadEncodingKind::Gzip
            }
            Some("zst") => {
                #[cfg(feature = "zstd")]
                {
                    println!("  Compression: zstd");
                    PayloadEncodingKind::Zstd
                }
                #[cfg(not(feature = "zstd"))]
                {
                    eprintln!(
                        "Error: zstd requested but 'tools' crate built without 'zstd' feature"
                    );
                    std::process::exit(1);
                }
            }
            Some(other) => {
                eprintln!("Error: unknown compressor '{}'. Use gz|zst", other);
                std::process::exit(1);
            }
        }
    } else {
        println!("  Compression: none");
        PayloadEncodingKind::None
    };

    let compress_level: Option<i32> = app
        .get_one::<String>("compress-level")
        .and_then(|s| s.parse::<i32>().ok());
    if let Some(lvl) = compress_level {
        println!("  Compression level: {}", lvl);
    }
    let dedup_features = app.get_flag("dedup-features");

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
        payload_encoding,
        compress_level,
        dedup_features,
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
        "  Feature dedup: {}",
        if config.dedup_features { "enabled" } else { "disabled" }
    );
    println!(
        "  Cache file size: {} MB",
        std::fs::metadata(output_path)?.len() / (1024 * 1024)
    );

    Ok(())
}

fn cp_to_wdl(cp: i32, scale: f32) -> f32 {
    1.0 / (1.0 + (-cp as f32 / scale).exp())
}

fn write_samples_to_sink<W: Write>(
    mut sink: W,
    reader: BufReader<File>,
    config: &CacheConfig,
) -> Result<(u64, u64, u64, u64), Box<dyn std::error::Error>> {
    let mut num_samples: u64 = 0;
    let mut total_features: u64 = 0;
    let mut skipped = 0;
    let mut processed = 0;
    // Reusable feature buffer (typical active features << 256)
    let mut features_buf: Vec<u32> = Vec::with_capacity(256);

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

        // Build common metadata flags
        let mut base_flags = 0u8;
        // both_exact
        let both_exact = pos_data.bound1.as_deref() == Some("Exact")
            && pos_data.bound2.as_deref() == Some("Exact");
        if both_exact {
            base_flags |= FLAG_BOTH_EXACT;
        }
        // mate boundary
        if pos_data.mate_boundary.unwrap_or(false) {
            base_flags |= FLAG_MATE_BOUNDARY;
        }

        // Side to move (for label orientation and flags)
        let stm = position.side_to_move;

        // Compute oriented CP for each perspective
        let cp_black = if stm == Color::Black { cp } else { -cp };
        let cp_white = -cp_black;

        // Helper to write one perspective
        let mut write_perspective = |perspective: Color, king_sq| -> std::io::Result<usize> {
            let feats = extract_features(&position, king_sq, perspective);
            features_buf.clear();
            features_buf.extend(feats.as_slice().iter().map(|&f| f as u32));
            if config.dedup_features {
                features_buf.sort_unstable();
                features_buf.dedup();
            }

            let cp_oriented = if perspective == Color::Black { cp_black } else { cp_white };
            let label = match config.label_type.as_str() {
                "wdl" => cp_to_wdl(cp_oriented, config.scale),
                "cp" => (cp_oriented.clamp(-config.cp_clip, config.cp_clip) as f32) / 100.0,
                _ => return Ok(0),
            };

            let mut flags = base_flags;
            if perspective == Color::Black {
                flags |= FLAG_PERSPECTIVE_BLACK;
            }
            if stm == Color::Black {
                flags |= FLAG_STM_BLACK;
            }

            // Write sample (no padding; meta layout fixed)
            let n_features = features_buf.len() as u32;
            sink.write_all(&n_features.to_le_bytes())?;
            // Bulk write features
            for &feat in &features_buf {
                sink.write_all(&feat.to_le_bytes())?;
            }
            sink.write_all(&label.to_le_bytes())?;
            let gap = pos_data.best2_gap_cp.unwrap_or(0).clamp(0, u16::MAX as i32) as u16;
            sink.write_all(&gap.to_le_bytes())?;
            sink.write_all(&[pos_data.depth.unwrap_or(0)])?;
            sink.write_all(&[pos_data.seldepth.unwrap_or(0)])?;
            sink.write_all(&[flags])?;

            total_features += features_buf.len() as u64;
            num_samples += 1;
            Ok(features_buf.len())
        };

        // Black perspective sample
        let _ = write_perspective(Color::Black, black_king)?;
        // White perspective sample
        let _ = write_perspective(Color::White, white_king)?;
    }

    Ok((num_samples, total_features, skipped, processed))
}

fn write_cache_file_streaming(
    input_path: &str,
    output_path: &str,
    config: &CacheConfig,
) -> Result<(u64, u64), Box<dyn std::error::Error>> {
    // Create file and write magic + placeholder header
    let mut file = File::create(output_path)?;
    file.write_all(b"NNFC")?;
    let header_pos = file.stream_position()?; // right after magic
    let header_placeholder = vec![0u8; HEADER_SIZE as usize];
    file.write_all(&header_placeholder)?;
    // Payload starts here
    let payload_offset = file.stream_position()?;

    // Prepare input reader
    let input_file = File::open(input_path)?;
    let reader = BufReader::new(input_file);

    // Write samples either raw or compressed
    let (num_samples, total_features, skipped, processed) = match config.payload_encoding {
        PayloadEncodingKind::None => {
            // Uncompressed payload
            let sink = BufWriter::new(file);
            write_samples_to_sink(sink, reader, config)?
        }
        PayloadEncodingKind::Gzip => {
            // Gzip compressed payload
            use flate2::write::GzEncoder;
            use flate2::Compression;
            let level = config
                .compress_level
                .map(|l| l.clamp(0, 9) as u32)
                .unwrap_or(6);
            let mut encoder = GzEncoder::new(file, Compression::new(level));
            let (num_samples, total_features, skipped, processed) =
                write_samples_to_sink(&mut encoder, reader, config)?;
            // Ensure encoder is finished (flush data to file)
            let _file = encoder.finish()?;
            (num_samples, total_features, skipped, processed)
        }
        #[cfg(feature = "zstd")]
        PayloadEncodingKind::Zstd => {
            // Zstd compressed payload
            let level = config.compress_level.unwrap_or(0);
            let mut encoder = zstd::Encoder::new(file, level)?; // 0 = default level
            let (num_samples, total_features, skipped, processed) = {
                // zstd::Encoder implements Write
                write_samples_to_sink(&mut encoder, reader, config)?
            };
            let _file = encoder.finish()?; // returns File
            (num_samples, total_features, skipped, processed)
        }
    };

    println!("\rProcessed {} positions (skipped {})", processed, skipped);

    // Reopen file for header update if not available (always reopen for simplicity)
    let mut f_header = File::options().write(true).open(output_path)?;
    f_header.seek(SeekFrom::Start(header_pos))?;

    // Write v1 extended header fields
    f_header.write_all(&CACHE_VERSION.to_le_bytes())?; // version
    f_header.write_all(&FEATURE_SET_ID.to_le_bytes())?; // feature_set_id
    f_header.write_all(&num_samples.to_le_bytes())?; // num_samples
    f_header.write_all(&config.chunk_size.to_le_bytes())?; // chunk_size
    f_header.write_all(&HEADER_SIZE.to_le_bytes())?; // header_size
    f_header.write_all(&[0u8])?; // endianness (0 = LE)
    f_header.write_all(&[config.payload_encoding.code()])?; // payload_encoding
    f_header.write_all(&[0u8; 2])?; // reserved16
    f_header.write_all(&payload_offset.to_le_bytes())?; // payload_offset
    let sample_flags_mask: u32 =
        (FLAG_BOTH_EXACT as u32) | (FLAG_MATE_BOUNDARY as u32) | (FLAG_PERSPECTIVE_BLACK as u32) | (FLAG_STM_BLACK as u32);
    f_header.write_all(&sample_flags_mask.to_le_bytes())?; // flags mask
    // pad reserved to HEADER_SIZE
    let written = 4 + 4 + 8 + 4 + 4 + 1 + 1 + 2 + 8 + 4; // 40
    let reserved_tail = (HEADER_SIZE as usize).saturating_sub(written);
    if reserved_tail > 0 {
        f_header.write_all(&vec![0u8; reserved_tail])?;
    }

    Ok((num_samples, total_features))
}
