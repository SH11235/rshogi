use engine_core::engine::controller::{Engine, EngineType};
use engine_core::search::limits::SearchLimits;
use engine_core::Position;
use rayon::prelude::*;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    env_logger::init();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: {} <input_sfen_file> <output_training_data> [batch_size]", args[0]);
        std::process::exit(1);
    }

    let input_path = PathBuf::from(&args[1]);
    let output_path = PathBuf::from(&args[2]);
    let batch_size = args.get(3).and_then(|s| s.parse::<usize>().ok()).unwrap_or(1000);

    // Read all lines into memory
    let input_file = File::open(&input_path)?;
    let reader = BufReader::new(input_file);
    let lines: Vec<String> = reader.lines().collect::<Result<_, _>>()?;

    // Filter valid SFEN lines
    let sfen_lines: Vec<(usize, String)> = lines
        .into_iter()
        .enumerate()
        .filter_map(|(idx, line)| {
            let line = line.trim();
            if line.is_empty() || !line.contains("sfen") {
                return None;
            }

            // Extract SFEN string from the line
            if let Some(start_idx) = line.find("sfen ") {
                let sfen_part = line[start_idx + 5..].to_string();
                Some((idx, sfen_part))
            } else {
                None
            }
        })
        .collect();

    let total_positions = sfen_lines.len();
    println!("Found {total_positions} SFEN positions to process");
    println!("Processing in batches of {batch_size}");

    // Process in parallel
    let processed_count = Arc::new(AtomicUsize::new(0));
    let error_count = Arc::new(AtomicUsize::new(0));

    // Process in batches to reduce memory pressure
    let mut all_results = Vec::with_capacity(total_positions);

    for (batch_idx, chunk) in sfen_lines.chunks(batch_size).enumerate() {
        println!("Processing batch {} of {}", batch_idx + 1, total_positions.div_ceil(batch_size));

        let batch_results: Vec<_> = chunk
            .par_iter()
            .map(|(idx, sfen)| {
                let engine = Engine::new(EngineType::Material);

                match Position::from_sfen(sfen) {
                    Ok(mut position) => {
                        // Setup search parameters for shallow search (depth 4)
                        let stop_flag = Arc::new(AtomicBool::new(false));
                        let limits = SearchLimits::builder()
                            .depth(4)
                            .fixed_time_ms(500) // Increased from 100ms to 500ms
                            .stop_flag(stop_flag)
                            .build();

                        // Perform the search
                        let result = engine.search(&mut position, limits);

                        // Extract evaluation value (score)
                        let eval = result.score;

                        // Update progress
                        let count = processed_count.fetch_add(1, Ordering::Relaxed) + 1;
                        if count % 100 == 0 {
                            println!("Processed {count} positions...");
                        }

                        Some((*idx, format!("{sfen} eval {eval}")))
                    }
                    Err(e) => {
                        eprintln!("Error parsing SFEN: {sfen} - {e}");
                        error_count.fetch_add(1, Ordering::Relaxed);
                        None
                    }
                }
            })
            .filter_map(|x| x)
            .collect();

        all_results.extend(batch_results);

        // Optional: Force garbage collection between batches
        // This can help with memory pressure
        std::thread::yield_now();
    }

    // Sort results by original index to maintain order
    all_results.sort_by_key(|(idx, _)| *idx);

    // Write results to file
    let mut output_file =
        OpenOptions::new().create(true).write(true).truncate(true).open(&output_path)?;

    for (_, result) in all_results {
        writeln!(output_file, "{result}")?;
    }

    let final_processed = processed_count.load(Ordering::Relaxed);
    let final_errors = error_count.load(Ordering::Relaxed);

    println!("Completed! Processed {final_processed} positions, {final_errors} errors");
    Ok(())
}
