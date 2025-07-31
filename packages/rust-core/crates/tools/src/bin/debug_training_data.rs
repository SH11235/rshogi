use engine_core::engine::controller::{Engine, EngineType};
use engine_core::search::limits::SearchLimits;
use engine_core::Position;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

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

    let input_file = File::open(&input_path)?;
    let reader = BufReader::new(input_file);

    let mut output_file =
        OpenOptions::new().create(true).write(true).truncate(true).open(&output_path)?;

    let engine = Engine::new(EngineType::Material);

    let mut processed = 0;
    let mut errors = 0;

    println!("Processing SFEN positions sequentially with debug info...");

    for (line_num, line) in reader.lines().enumerate() {
        let line = line?;
        let line = line.trim();

        if line.is_empty() || !line.contains("sfen") {
            continue;
        }

        // Extract SFEN string from the line
        let sfen_part = if let Some(start_idx) = line.find("sfen ") {
            &line[start_idx + 5..]
        } else {
            continue;
        };

        println!("Processing line {} (position {}): {}", line_num + 1, processed + 1, sfen_part);
        let start_time = Instant::now();

        // Try to parse the position
        match Position::from_sfen(sfen_part) {
            Ok(mut position) => {
                // Setup search parameters for shallow search (depth 4)
                let stop_flag = Arc::new(AtomicBool::new(false));
                let stop_flag_clone = stop_flag.clone();

                // Create a timeout thread
                let timeout_handle = std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(500)); // 500ms timeout
                    stop_flag_clone.store(true, Ordering::Relaxed);
                });

                let limits = SearchLimits::builder()
                    .depth(4)
                    .fixed_time_ms(100)
                    .stop_flag(stop_flag.clone())
                    .build();

                // Perform the search
                let result = engine.search(&mut position, limits);

                // Check if timeout occurred
                let timed_out = stop_flag.load(Ordering::Relaxed);
                if timed_out {
                    println!("  WARNING: Search timed out after {:?}", start_time.elapsed());
                }

                // Clean up timeout thread
                timeout_handle.join().ok();

                // Extract evaluation value (score)
                let eval = result.score;
                let elapsed = start_time.elapsed();

                println!("  Completed in {elapsed:?}, eval: {eval}");

                // Format: SFEN eval <value>
                writeln!(output_file, "{sfen_part} eval {eval}")?;

                processed += 1;

                // Warn if processing took too long
                if elapsed > Duration::from_millis(200) {
                    println!("  WARNING: Slow position detected ({}ms)", elapsed.as_millis());
                }
            }
            Err(e) => {
                eprintln!("Error parsing SFEN at line {}: {} - {}", line_num + 1, sfen_part, e);
                errors += 1;
            }
        }
    }

    println!("Completed! Processed {processed} positions, {errors} errors");
    Ok(())
}
