use engine_core::engine::controller::{Engine, EngineType};
use engine_core::search::limits::SearchLimits;
use engine_core::Position;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn process_position_with_timeout(
    idx: usize,
    sfen: &str,
    timeout_ms: u64,
) -> Option<(usize, String)> {
    // Use thread with timeout to avoid hanging
    let sfen_str = sfen.to_string();
    let sfen_clone = sfen_str.clone();
    let (tx, rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let mut engine = Engine::new(EngineType::Material);

        match Position::from_sfen(&sfen_str) {
            Ok(mut position) => {
                // Try depth 3 first with shorter timeout
                let limits = SearchLimits::builder().depth(3).fixed_time_ms(200).build();

                let result = engine.search(&mut position, limits);
                let _ = tx.send(Some((idx, format!("{} eval {}", sfen_str, result.score))));
            }
            Err(e) => {
                eprintln!("Error parsing SFEN at line {}: {} - {}", idx + 1, sfen_str, e);
                let _ = tx.send(None);
            }
        }
    });

    // Wait for result with timeout
    match rx.recv_timeout(Duration::from_millis(timeout_ms)) {
        Ok(result) => result,
        Err(_) => {
            eprintln!("Timeout on position {} after {}ms", idx + 1, timeout_ms);
            // Return a default evaluation
            Some((idx, format!("{sfen_clone} eval 0")))
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!(
            "Usage: {} <input_sfen_file> <output_training_data> [batch_size] [resume_from_line]",
            args[0]
        );
        eprintln!("  batch_size: Number of positions to process in parallel (default: 20)");
        eprintln!("  resume_from_line: Line number to resume from (default: 0)");
        std::process::exit(1);
    }

    let input_path = PathBuf::from(&args[1]);
    let output_path = PathBuf::from(&args[2]);
    let batch_size = args.get(3).and_then(|s| s.parse::<usize>().ok()).unwrap_or(20);
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

    println!("CPU cores available: {:?}", std::thread::available_parallelism());

    for (batch_idx, chunk) in sfen_lines.chunks(batch_size).enumerate() {
        println!("Processing batch {} ({} positions)...", batch_idx + 1, chunk.len());
        let batch_start = Instant::now();

        // Process each position with individual timeout
        let batch_results: Vec<_> = chunk
            .iter()
            .filter_map(|(idx, sfen)| {
                let result = process_position_with_timeout(*idx, sfen, 300);

                // Update progress
                let count = processed_count.fetch_add(1, Ordering::Relaxed) + 1;
                if count % 10 == 0 {
                    print!(".");
                    std::io::stdout().flush().ok();
                }

                result
            })
            .collect();

        // Write batch results immediately
        let batch_elapsed = batch_start.elapsed();
        println!("\nBatch {} completed in {:?}", batch_idx + 1, batch_elapsed);
        println!("Writing {} results from batch {}...", batch_results.len(), batch_idx + 1);
        {
            let mut file = output_file.lock().unwrap();
            for (_, result) in batch_results {
                writeln!(file, "{result}")?;
            }
            file.flush()?;
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
