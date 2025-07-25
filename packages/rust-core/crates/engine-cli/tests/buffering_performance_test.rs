//! Performance test to measure the impact of buffering
//! Run with: cargo test -p engine-cli --test buffering_performance_test --release -- --nocapture

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn run_search_test(flush_delay: &str, depth: u32) -> (Duration, usize, usize) {
    let mut engine = Command::new(env!("CARGO_BIN_EXE_engine-cli"))
        .env("USI_FLUSH_DELAY_MS", flush_delay)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("Failed to spawn engine");

    let mut stdin = engine.stdin.take().expect("Failed to get stdin");
    let stdout = engine.stdout.take().expect("Failed to get stdout");

    // Count lines and syscalls (approximated by flush count)
    let reader_handle = thread::spawn(move || {
        let reader = BufReader::new(stdout);
        let mut line_count = 0;
        let mut info_count = 0;

        for line in reader.lines().flatten() {
            line_count += 1;
            if line.starts_with("info ") && !line.contains("string") {
                info_count += 1;
            }
        }
        (line_count, info_count)
    });

    // Initialize engine
    writeln!(stdin, "usi").unwrap();
    stdin.flush().unwrap();
    thread::sleep(Duration::from_millis(50));

    writeln!(stdin, "isready").unwrap();
    stdin.flush().unwrap();
    thread::sleep(Duration::from_millis(50));

    writeln!(stdin, "position startpos").unwrap();
    stdin.flush().unwrap();

    // Measure search time
    let start = Instant::now();
    writeln!(stdin, "go depth {depth}").unwrap();
    stdin.flush().unwrap();

    // Wait for bestmove
    thread::sleep(Duration::from_secs(10)); // Generous timeout

    let search_time = start.elapsed();

    // Quit
    writeln!(stdin, "quit").unwrap();
    stdin.flush().unwrap();
    drop(stdin);

    let _ = engine.wait();
    let (line_count, info_count) = reader_handle.join().unwrap();

    (search_time, line_count, info_count)
}

#[test]
fn test_buffering_performance_impact() {
    println!("\n=== Buffering Performance Test ===\n");

    // Test with different configurations
    let configs = [
        ("0", "Immediate flush"),
        ("100", "100ms buffering"),
        ("500", "500ms buffering"),
    ];

    let depth = 4; // Moderate depth for quick testing

    for (delay, desc) in &configs {
        println!("Testing {desc}: ");

        // Run multiple times and average
        let mut total_time = Duration::ZERO;
        let mut total_lines = 0;
        let mut total_infos = 0;
        let runs = 3;

        for run in 0..runs {
            let (time, lines, infos) = run_search_test(delay, depth);
            total_time += time;
            total_lines += lines;
            total_infos += infos;
            println!(
                "  Run {}: {:.2}s, {} lines, {} info messages",
                run + 1,
                time.as_secs_f64(),
                lines,
                infos
            );
        }

        let avg_time = total_time / runs;
        let avg_lines = total_lines / runs as usize;
        let avg_infos = total_infos / runs as usize;

        println!(
            "  Average: {:.2}s, {} lines, {} info messages",
            avg_time.as_secs_f64(),
            avg_lines,
            avg_infos
        );

        // Estimate syscall reduction
        let syscalls_immediate = avg_infos + 5; // Each info + critical messages
        let syscalls_buffered = if *delay == "0" {
            syscalls_immediate
        } else {
            // Assume batches of ~10 messages
            (avg_infos / 10) + 5
        };

        println!(
            "  Estimated syscalls: {} ({}% reduction)\n",
            syscalls_buffered,
            if *delay == "0" {
                0
            } else {
                ((syscalls_immediate - syscalls_buffered) * 100) / syscalls_immediate
            }
        );
    }
}

#[test]
#[ignore] // This test takes a long time
fn test_buffering_stress() {
    println!("\n=== Buffering Stress Test ===\n");

    // Stress test with very deep search
    let depth = 10;

    println!("Running deep search (depth {depth}) with buffering...");
    let start = Instant::now();
    let (time_buffered, lines_buffered, infos_buffered) = run_search_test("100", depth);
    println!(
        "Buffered: {:.2}s, {} lines, {} info messages",
        time_buffered.as_secs_f64(),
        lines_buffered,
        infos_buffered
    );

    println!("\nRunning deep search (depth {depth}) without buffering...");
    let (time_immediate, lines_immediate, infos_immediate) = run_search_test("0", depth);
    println!(
        "Immediate: {:.2}s, {} lines, {} info messages",
        time_immediate.as_secs_f64(),
        lines_immediate,
        infos_immediate
    );

    let speedup = time_immediate.as_secs_f64() / time_buffered.as_secs_f64();
    println!("\nSpeedup with buffering: {speedup:.2}x");

    println!("Total test time: {:.2}s", start.elapsed().as_secs_f64());
}
