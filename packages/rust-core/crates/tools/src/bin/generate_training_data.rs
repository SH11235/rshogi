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
        eprintln!("  batch_size: Number of positions to process in parallel (default: 25)");
        eprintln!("  resume_from_line: Line number to resume from (default: 0)");
        std::process::exit(1);
    }

    let input_path = PathBuf::from(&args[1]);
    let output_path = PathBuf::from(&args[2]);
    let batch_size = args.get(3).and_then(|s| s.parse::<usize>().ok()).unwrap_or(25);
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
    let timeout_count = Arc::new(AtomicUsize::new(0));
    let total_processed = Arc::new(AtomicUsize::new(existing_lines));

    // Display parallelism info
    println!("CPU cores available: {:?}", std::thread::available_parallelism());
    println!("Rayon thread pool size: {}", rayon::current_num_threads());
    if let Ok(threads) = std::env::var("RAYON_NUM_THREADS") {
        println!("RAYON_NUM_THREADS is set to: {threads}");
    }

    // Collect timeout positions for retry
    let timeout_positions = Arc::new(Mutex::new(Vec::new()));

    for (batch_idx, chunk) in sfen_lines.chunks(batch_size).enumerate() {
        println!("Processing batch {} ({} positions)...", batch_idx + 1, chunk.len());
        let batch_start = std::time::Instant::now();

        let batch_results: Vec<_> = chunk
            .par_iter()
            .map(|(idx, sfen)| {
                let mut engine = Engine::new(EngineType::Material);

                match Position::from_sfen(sfen) {
                    Ok(mut position) => {
                        // Setup search parameters for shallow search (depth 4)
                        let stop_flag = Arc::new(AtomicBool::new(false));
                        let stop_flag_clone = stop_flag.clone();

                        // Start timing before creating timeout thread
                        let total_start = std::time::Instant::now();

                        // Create a thread to enforce timeout
                        let timeout_occurred = Arc::new(AtomicBool::new(false));
                        let timeout_occurred_clone = timeout_occurred.clone();
                        let timeout_handle = std::thread::spawn(move || {
                            std::thread::sleep(std::time::Duration::from_millis(250)); // Earlier than time limit
                            stop_flag_clone.store(true, Ordering::Release);
                            timeout_occurred_clone.store(true, Ordering::Release);
                        });

                        let limits = SearchLimits::builder()
                            .depth(8) // Even deeper to ensure timeout
                            .fixed_time_ms(10000) // Very long time limit so stop_flag is the only way to stop
                            .stop_flag(stop_flag.clone())
                            .build();

                        // Perform the search
                        let search_start = std::time::Instant::now();
                        let result = engine.search(&mut position, limits);
                        let search_time = search_start.elapsed();

                        // Check if stop flag was actually set during search
                        let was_stopped = timeout_occurred.load(Ordering::Acquire)
                            && search_time.as_millis() >= 240;

                        // Wait for timeout thread to complete
                        timeout_handle.join().ok();

                        let total_time = total_start.elapsed();

                        // Log processing time for timeout positions
                        if was_stopped || search_time.as_millis() >= 240 {
                            eprintln!(
                                "Position {} took {:.2}s (search: {:.2}s)",
                                idx,
                                total_time.as_secs_f32(),
                                search_time.as_secs_f32()
                            );
                        }

                        // Check if we hit timeout
                        if was_stopped || search_time.as_millis() >= 240 {
                            // Position likely timed out, save for retry
                            timeout_count.fetch_add(1, Ordering::Relaxed);
                            timeout_positions.lock().unwrap().push((*idx, sfen.clone()));

                            // Still return the result we got
                            let eval = result.score;
                            if *idx < 10 || (*idx + 1) % 100 == 0 {
                                eprintln!(
                                    "Timeout on position {} (depth reached: {}, eval: {})",
                                    idx + 1,
                                    result.stats.depth,
                                    eval
                                );
                            }
                            Some((
                                *idx,
                                format!(
                                    "{sfen} eval {eval} # timeout_depth_{}",
                                    result.stats.depth
                                ),
                            ))
                        } else {
                            // Normal completion
                            let eval = result.score;

                            // Update progress
                            let count = processed_count.fetch_add(1, Ordering::Relaxed) + 1;
                            if count % 10 == 0 {
                                print!(".");
                                std::io::stdout().flush().ok();
                            }

                            Some((*idx, format!("{sfen} eval {eval}")))
                        }
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
        let batch_elapsed = batch_start.elapsed();
        let positions_per_second = chunk.len() as f64 / batch_elapsed.as_secs_f64();
        println!(
            "\nBatch {} completed in {:?} ({:.1} positions/sec)",
            batch_idx + 1,
            batch_elapsed,
            positions_per_second
        );
        println!("Writing {} results from batch {}...", batch_results.len(), batch_idx + 1);
        {
            let mut file = output_file.lock().unwrap();
            for (_, result) in batch_results {
                writeln!(file, "{result}")?;
            }
            file.flush()?; // Ensure data is written to disk
        }

        let total = total_processed.fetch_add(chunk.len(), Ordering::Relaxed) + chunk.len();
        let timeouts = timeout_count.load(Ordering::Relaxed);
        println!("Progress: {total} / {total_positions} positions completed ({timeouts} timeouts so far)");

        // Reset counters for next batch
        processed_count.store(0, Ordering::Relaxed);
    }

    let final_errors = error_count.load(Ordering::Relaxed);
    let final_timeouts = timeout_count.load(Ordering::Relaxed);
    let final_total = total_processed.load(Ordering::Relaxed);

    println!("\nPhase 1 completed! Processed {final_total} positions total, {final_errors} errors, {final_timeouts} timeouts");

    // Process timeout positions with reduced depth
    let timeout_positions = timeout_positions.lock().unwrap().clone();
    if !timeout_positions.is_empty() {
        println!(
            "\nProcessing {} timeout positions with reduced depth...",
            timeout_positions.len()
        );

        let retry_results: Vec<_> = timeout_positions
            .par_iter()
            .map(|(_idx, sfen)| {
                let mut engine = Engine::new(EngineType::Material);

                match Position::from_sfen(sfen) {
                    Ok(mut position) => {
                        // Reduced depth for timeout positions
                        let stop_flag = Arc::new(AtomicBool::new(false));
                        let stop_flag_clone = stop_flag.clone();

                        // Timeout thread for retry
                        std::thread::spawn(move || {
                            std::thread::sleep(std::time::Duration::from_millis(180));
                            stop_flag_clone.store(true, Ordering::Release);
                        });

                        let limits = SearchLimits::builder()
                            .depth(3)
                            .fixed_time_ms(200)
                            .stop_flag(stop_flag)
                            .build();

                        let result = engine.search(&mut position, limits);
                        let eval = result.score;

                        Some(format!("{sfen} eval {eval} # retry_depth_3"))
                    }
                    Err(_) => None,
                }
            })
            .filter_map(|x| x)
            .collect();

        // Append retry results
        if !retry_results.is_empty() {
            println!("Writing {} retry results...", retry_results.len());
            let mut file = output_file.lock().unwrap();
            for result in retry_results {
                writeln!(file, "{result}")?;
            }
            file.flush()?;
        }
    }

    if final_errors > 0 {
        println!("Note: {final_errors} positions had errors and were skipped");
    }

    Ok(())
}
