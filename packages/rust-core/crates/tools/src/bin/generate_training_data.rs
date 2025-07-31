use engine_core::engine::controller::{Engine, EngineType};
use engine_core::search::limits::SearchLimits;
use engine_core::Position;
use rayon::prelude::*;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    env_logger::init();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!(
            "Usage: {} <input_sfen_file> <output_training_data> [batch_size] [resume_from_line]",
            args[0]
        );
        eprintln!("  batch_size: Number of positions to process in parallel (default: 100)");
        eprintln!("  resume_from_line: Line number to resume from (default: 0)");
        std::process::exit(1);
    }

    let input_path = PathBuf::from(&args[1]);
    let output_path = PathBuf::from(&args[2]);
    let batch_size = args.get(3).and_then(|s| s.parse::<usize>().ok()).unwrap_or(100);
    let resume_from = args.get(4).and_then(|s| s.parse::<usize>().ok()).unwrap_or(0);

    // Count existing lines if resuming
    let existing_lines = if resume_from > 0 && output_path.exists() {
        let file = File::open(&output_path)?;
        let reader = BufReader::new(file);
        reader.lines().count()
    } else {
        0
    };

    if existing_lines > 0 {
        println!("Found {existing_lines} existing lines in output file");
    }

    // Open output file in append mode if resuming
    let output_file = Arc::new(Mutex::new(
        OpenOptions::new()
            .create(true)
            .write(true)
            .append(resume_from > 0)
            .truncate(resume_from == 0)
            .open(&output_path)?,
    ));

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

    // Skip already processed positions
    let sfen_lines = if resume_from > 0 || existing_lines > 0 {
        let skip = resume_from.max(existing_lines);
        println!("Skipping first {skip} positions (already processed)");
        sfen_lines.into_iter().skip(skip).collect()
    } else {
        sfen_lines
    };

    println!(
        "Processing {} remaining positions in batches of {}",
        sfen_lines.len(),
        batch_size
    );

    // Process in batches
    let processed_count = Arc::new(AtomicUsize::new(0));
    let error_count = Arc::new(AtomicUsize::new(0));
    let total_processed = Arc::new(AtomicUsize::new(existing_lines));

    for (batch_idx, chunk) in sfen_lines.chunks(batch_size).enumerate() {
        println!("Processing batch {} ({} positions)...", batch_idx + 1, chunk.len());

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
                            .fixed_time_ms(500)
                            .stop_flag(stop_flag)
                            .build();

                        // Perform the search
                        let result = engine.search(&mut position, limits);

                        // Extract evaluation value (score)
                        let eval = result.score;

                        // Update progress
                        let count = processed_count.fetch_add(1, Ordering::Relaxed) + 1;
                        if count % 10 == 0 {
                            print!(".");
                            std::io::stdout().flush().ok();
                        }

                        Some((*idx, format!("{sfen} eval {eval}")))
                    }
                    Err(e) => {
                        eprintln!("\nError parsing SFEN at line {}: {} - {}", idx + 1, sfen, e);
                        error_count.fetch_add(1, Ordering::Relaxed);
                        None
                    }
                }
            })
            .filter_map(|x| x)
            .collect();

        // Write batch results immediately
        println!("\nWriting {} results from batch {}...", batch_results.len(), batch_idx + 1);
        {
            let mut file = output_file.lock().unwrap();
            for (_, result) in batch_results {
                writeln!(file, "{result}")?;
            }
            file.flush()?; // Ensure data is written to disk
        }

        let total = total_processed.fetch_add(chunk.len(), Ordering::Relaxed) + chunk.len();
        println!("Progress: {total} / {total_positions} positions completed");

        // Reset counters for next batch
        processed_count.store(0, Ordering::Relaxed);
    }

    let final_errors = error_count.load(Ordering::Relaxed);
    let final_total = total_processed.load(Ordering::Relaxed);

    println!("\nCompleted! Processed {final_total} positions total, {final_errors} errors");

    if final_errors > 0 {
        println!("Note: {final_errors} positions had errors and were skipped");
    }

    Ok(())
}
