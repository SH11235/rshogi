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
        eprintln!("Usage: {} <input_sfen_file> <output_training_data>", args[0]);
        std::process::exit(1);
    }

    let input_path = PathBuf::from(&args[1]);
    let output_path = PathBuf::from(&args[2]);

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

    // Process in parallel
    let processed_count = Arc::new(AtomicUsize::new(0));
    let error_count = Arc::new(AtomicUsize::new(0));

    // Process positions in parallel and collect results
    // Note: You can limit threads with RAYON_NUM_THREADS environment variable
    let results: Vec<_> = sfen_lines
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

    println!("All positions processed. Sorting results...");

    // Sort results by original index to maintain order
    let mut sorted_results = results;
    sorted_results.sort_by_key(|(idx, _)| *idx);

    println!("Sorting complete. Writing to file...");

    // Write results to file
    let mut output_file =
        OpenOptions::new().create(true).write(true).truncate(true).open(&output_path)?;

    // Write with progress
    let total_results = sorted_results.len();
    for (i, (_, result)) in sorted_results.into_iter().enumerate() {
        writeln!(output_file, "{result}")?;
        if (i + 1) % 5000 == 0 {
            println!("Written {} / {} lines to file...", i + 1, total_results);
        }
    }

    let final_processed = processed_count.load(Ordering::Relaxed);
    let final_errors = error_count.load(Ordering::Relaxed);

    println!("File writing complete!");
    println!("Completed! Processed {final_processed} positions, {final_errors} errors");
    Ok(())
}
