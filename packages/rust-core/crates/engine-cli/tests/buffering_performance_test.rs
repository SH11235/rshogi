//! Performance test to measure the impact of buffering
//! Run with: cargo test -p engine-cli --test buffering_performance_test --release -- --nocapture

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc::channel;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug)]
struct PerformanceResult {
    name: String,
    mean_time: f64,
    std_dev: f64,
    median_time: f64,
    p90_time: f64,
    p99_time: f64,
    syscall_reduction: Option<f64>,
    samples: usize,
    outliers_removed: usize,
}

impl PerformanceResult {
    fn to_json(&self) -> String {
        format!(
            r#"{{"name":"{}","mean_time":{:.4},"std_dev":{:.4},"median_time":{:.4},"p90_time":{:.4},"p99_time":{:.4},"syscall_reduction":{},"samples":{},"outliers_removed":{}}}"#,
            self.name,
            self.mean_time,
            self.std_dev,
            self.median_time,
            self.p90_time,
            self.p99_time,
            self.syscall_reduction.map_or("null".to_string(), |v| format!("{v:.2}")),
            self.samples,
            self.outliers_removed
        )
    }
}

/// Platform-specific syscall measurement
#[cfg(target_os = "linux")]
fn measure_syscalls(cmd: &mut Command) -> Option<()> {
    // Check if strace is available
    if Command::new("which").arg("strace").output().ok()?.status.success() {
        // Use strace to count write syscalls
        let strace_args = vec![
            "-e",
            "trace=write,writev", // Only trace write syscalls
            "-c",                 // Count syscalls
            "-q",                 // Quiet mode
            "-f",                 // Follow forks
        ];

        // Prepend strace to the command
        let engine_path = cmd.get_program().to_str().unwrap().to_string();
        let mut new_cmd = Command::new("strace");
        for arg in strace_args {
            new_cmd.arg(arg);
        }
        new_cmd.arg(engine_path);

        // Copy CLI arguments
        for arg in cmd.get_args() {
            new_cmd.arg(arg);
        }

        // Copy environment variables
        for (key, value) in cmd.get_envs() {
            if let Some(value) = value {
                new_cmd.env(key, value);
            }
        }

        // Replace the command
        *cmd = new_cmd;

        return Some(()); // Marker that strace is enabled
    }

    // Try perf as fallback
    if Command::new("which").arg("perf").output().ok()?.status.success() {
        let engine_path = cmd.get_program().to_str().unwrap().to_string();
        let mut new_cmd = Command::new("perf");
        new_cmd.arg("stat");
        new_cmd.arg("-e");
        new_cmd.arg("syscalls:sys_enter_write");
        new_cmd.arg(engine_path);

        // Copy CLI arguments
        for arg in cmd.get_args() {
            new_cmd.arg(arg);
        }

        // Copy environment variables
        for (key, value) in cmd.get_envs() {
            if let Some(value) = value {
                new_cmd.env(key, value);
            }
        }

        *cmd = new_cmd;
        return Some(());
    }

    None
}

#[cfg(not(target_os = "linux"))]
fn measure_syscalls(_cmd: &mut Command) -> Option<()> {
    eprintln!("Syscall measurement not available on this platform");
    None
}

/// Parse strace output to extract syscall counts
#[cfg(target_os = "linux")]
fn parse_strace_output(stderr: String) -> Option<usize> {
    // Look for the summary section
    let mut total_writes = 0;
    let mut in_summary = false;

    for line in stderr.lines() {
        let line = line.trim_start(); // Handle leading spaces
        if line.contains("% time") && line.contains("calls") {
            in_summary = true;
            continue;
        }

        if in_summary && (line.contains("write") || line.contains("writev")) {
            // Extract the calls count (second column)
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                if let Ok(count) = parts[1].parse::<usize>() {
                    total_writes += count;
                }
            }
        }
    }

    if total_writes > 0 {
        Some(total_writes)
    } else {
        None
    }
}

fn run_search_test(flush_delay: &str, depth: u32) -> (Duration, usize, usize) {
    let mut engine = Command::new(env!("CARGO_BIN_EXE_engine-cli"))
        .env("USI_FLUSH_DELAY_MS", flush_delay)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped()) // Capture stderr for debugging
        .spawn()
        .expect("Failed to spawn engine");

    let mut stdin = engine.stdin.take().expect("Failed to get stdin");
    let stdout = engine.stdout.take().expect("Failed to get stdout");
    let stderr = engine.stderr.take().expect("Failed to get stderr");

    // Read stderr in background for debugging
    let stderr_handle = thread::spawn(move || {
        let reader = BufReader::new(stderr);
        let mut error_output = Vec::new();
        for line in reader.lines() {
            if let Ok(line) = line {
                error_output.push(line);
            }
        }
        error_output
    });

    // Channel to signal bestmove reception
    let (bestmove_tx, bestmove_rx) = channel::<()>();

    // Count lines and syscalls (approximated by flush count)
    let reader_handle = thread::spawn(move || {
        let reader = BufReader::new(stdout);
        let mut line_count = 0;
        let mut info_count = 0;
        let bestmove_sender = bestmove_tx;

        for line in reader.lines().map_while(Result::ok) {
            line_count += 1;
            if line.starts_with("info ") && !line.contains("string") {
                info_count += 1;
            }
            // Notify when bestmove is received
            if line.starts_with("bestmove ") {
                let _ = bestmove_sender.send(());
            }
        }
        (line_count, info_count)
    });

    // Initialize engine with error handling
    if let Err(e) = writeln!(stdin, "usi") {
        eprintln!("Failed to send usi command: {e}");
        if let Ok(stderr_output) = stderr_handle.join() {
            eprintln!("Engine stderr: {:?}", stderr_output);
        }
        panic!("Engine communication failed: {e}");
    }
    let _ = stdin.flush();
    thread::sleep(Duration::from_millis(100)); // Increase delay for Windows

    if let Err(e) = writeln!(stdin, "isready") {
        eprintln!("Failed to send isready command: {e}");
        if let Ok(stderr_output) = stderr_handle.join() {
            eprintln!("Engine stderr: {:?}", stderr_output);
        }
        panic!("Engine communication failed: {e}");
    }
    let _ = stdin.flush();
    thread::sleep(Duration::from_millis(100)); // Increase delay for Windows

    if let Err(e) = writeln!(stdin, "position startpos") {
        eprintln!("Failed to send position command: {e}");
        panic!("Engine communication failed: {e}");
    }
    let _ = stdin.flush();

    // Measure search time
    let start = Instant::now();
    if let Err(e) = writeln!(stdin, "go depth {depth}") {
        eprintln!("Failed to send go command: {e}");
        panic!("Engine communication failed: {e}");
    }
    let _ = stdin.flush();

    // Wait for bestmove with dynamic timeout
    // For depth-based search, estimate timeout based on depth
    // Rough estimate: 1 second per depth level + safety margin
    let timeout = Duration::from_secs((depth * 2) as u64 + 5);

    let search_time = match bestmove_rx.recv_timeout(timeout) {
        Ok(()) => start.elapsed(),
        Err(_) => {
            let elapsed = start.elapsed();
            eprintln!(
                "Warning: Timeout waiting for bestmove at depth {}, elapsed: {:.2}s (timeout: {:.2}s)",
                depth,
                elapsed.as_secs_f64(),
                timeout.as_secs_f64()
            );
            elapsed
        }
    };

    // Quit
    let _ = writeln!(stdin, "quit");
    let _ = stdin.flush();
    drop(stdin);

    // Wait for engine to exit gracefully
    // Note: wait_timeout is not available in standard library, so we just wait normally
    match engine.wait() {
        Ok(status) => {
            if !status.success() {
                eprintln!("Engine exited with non-zero status: {status}");
            }
        }
        Err(e) => eprintln!("Error waiting for engine: {e}"),
    }

    let (line_count, info_count) = reader_handle.join().unwrap();

    (search_time, line_count, info_count)
}

fn run_search_test_timed(flush_delay: &str, movetime_ms: u64) -> (Duration, usize, usize) {
    let mut engine = Command::new(env!("CARGO_BIN_EXE_engine-cli"))
        .env("USI_FLUSH_DELAY_MS", flush_delay)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("Failed to spawn engine");

    let mut stdin = engine.stdin.take().expect("Failed to get stdin");
    let stdout = engine.stdout.take().expect("Failed to get stdout");

    // Channel to signal bestmove reception
    let (bestmove_tx, bestmove_rx) = channel::<()>();

    // Count lines and syscalls (approximated by flush count)
    let reader_handle = thread::spawn(move || {
        let reader = BufReader::new(stdout);
        let mut line_count = 0;
        let mut info_count = 0;
        let bestmove_sender = bestmove_tx;

        for line in reader.lines().map_while(Result::ok) {
            line_count += 1;
            if line.starts_with("info ") && !line.contains("string") {
                info_count += 1;
            }
            // Notify when bestmove is received
            if line.starts_with("bestmove ") {
                let _ = bestmove_sender.send(());
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
    writeln!(stdin, "go movetime {movetime_ms}").unwrap();
    stdin.flush().unwrap();

    // Wait for bestmove with dynamic timeout
    // For time-based search: 3 × movetime + safety margin
    let timeout = Duration::from_millis(movetime_ms * 3 + 1000);

    let search_time = match bestmove_rx.recv_timeout(timeout) {
        Ok(()) => start.elapsed(),
        Err(_) => {
            let elapsed = start.elapsed();
            eprintln!(
                "Warning: Timeout waiting for bestmove at movetime {}ms, elapsed: {:.2}s (timeout: {:.2}s)",
                movetime_ms,
                elapsed.as_secs_f64(),
                timeout.as_secs_f64()
            );
            elapsed
        }
    };

    // Quit
    let _ = writeln!(stdin, "quit");
    let _ = stdin.flush();
    drop(stdin);

    let _ = engine.wait();
    let (line_count, info_count) = reader_handle.join().unwrap();

    (search_time, line_count, info_count)
}

/// Statistical helpers
fn calculate_statistics(values: &[f64]) -> (f64, f64, f64, f64, f64) {
    let n = values.len() as f64;
    let mean = values.iter().sum::<f64>() / n;

    let variance = values.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n;
    let std_dev = variance.sqrt();

    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let median = if sorted.len() % 2 == 0 {
        (sorted[sorted.len() / 2 - 1] + sorted[sorted.len() / 2]) / 2.0
    } else {
        sorted[sorted.len() / 2]
    };

    // Improved percentile calculation
    let p90_idx = ((sorted.len() as f64 * 0.9) as usize).min(sorted.len() - 1);
    let p99_idx = ((sorted.len() as f64 * 0.99) as usize).min(sorted.len() - 1);
    let p90 = sorted[p90_idx];
    let p99 = sorted[p99_idx];

    (mean, std_dev, median, p90, p99)
}

/// Remove outliers using IQR method with Tukey's factor
fn remove_outliers(values: &[f64]) -> Vec<f64> {
    if values.len() < 4 {
        return values.to_vec();
    }

    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let q1_idx = sorted.len() / 4;
    let q3_idx = 3 * sorted.len() / 4;
    let q1 = sorted[q1_idx];
    let q3 = sorted[q3_idx];
    let iqr = q3 - q1;

    // Handle IQR == 0 case (all values are similar)
    if iqr == 0.0 {
        // Return all values as there's no variation
        return values.to_vec();
    }

    // Use Tukey's factor 3.0 for extreme outliers (as suggested)
    let tukey_factor = 3.0;
    let lower_bound = q1 - tukey_factor * iqr;
    let upper_bound = q3 + tukey_factor * iqr;

    values
        .iter()
        .filter(|&&x| x >= lower_bound && x <= upper_bound)
        .copied()
        .collect()
}

#[test]
fn test_buffering_performance_impact() {
    println!("\n=== Buffering Performance Test (with Statistical Analysis) ===\n");

    // Test with different configurations
    let configs = [
        ("0", "Immediate flush"),
        ("100", "100ms buffering"),
        ("500", "500ms buffering"),
    ];

    let depth = 4; // Moderate depth for quick testing
    let mut results = Vec::new();

    // Baseline info count for syscall reduction calculation
    let mut baseline_infos = 0.0;

    for (delay, desc) in &configs {
        println!("Testing {desc}: ");

        // Run with warm-up
        let warm_up_runs = 1;
        let measurement_runs = 5;

        // Warm-up runs (not measured)
        println!("  Warm-up run...");
        for _ in 0..warm_up_runs {
            let _ = run_search_test(delay, depth);
        }

        // Measurement runs
        let mut times = Vec::new();
        let mut lines_counts = Vec::new();
        let mut info_counts = Vec::new();

        for run in 0..measurement_runs {
            let (time, lines, infos) = run_search_test(delay, depth);
            times.push(time.as_secs_f64());
            lines_counts.push(lines as f64);
            info_counts.push(infos as f64);

            println!(
                "  Run {}: {:.3}s, {} lines, {} info messages",
                run + 1,
                time.as_secs_f64(),
                lines,
                infos
            );
        }

        // Remove outliers
        let original_count = times.len();
        let clean_times = remove_outliers(&times);
        let outliers_removed = original_count - clean_times.len();
        println!("  Outliers removed: {outliers_removed} measurements");

        // Calculate statistics
        let (mean_time, std_dev, median_time, p90_time, p99_time) =
            calculate_statistics(&clean_times);
        let (mean_infos, _, _, _, _) = calculate_statistics(&info_counts);

        // Store baseline for immediate flush
        if *delay == "0" {
            baseline_infos = mean_infos;
        }

        println!(
            "  Time statistics: mean={mean_time:.3}s, std_dev={std_dev:.3}s, median={median_time:.3}s, p90={p90_time:.3}s, p99={p99_time:.3}s"
        );

        // Calculate syscall reduction
        let syscalls_immediate = baseline_infos as usize + 5;
        let syscalls_buffered = if *delay == "0" {
            syscalls_immediate
        } else {
            (mean_infos as usize / 10) + 5
        };

        let syscall_reduction = if *delay == "0" {
            None
        } else {
            Some(
                ((syscalls_immediate - syscalls_buffered) as f64 * 100.0)
                    / syscalls_immediate as f64,
            )
        };

        println!(
            "  Estimated syscalls: {} ({}% reduction)\n",
            syscalls_buffered,
            syscall_reduction.map_or(0, |r| r as usize)
        );

        // Store result
        results.push(PerformanceResult {
            name: desc.to_string(),
            mean_time,
            std_dev,
            median_time,
            p90_time,
            p99_time,
            syscall_reduction,
            samples: clean_times.len(),
            outliers_removed,
        });
    }

    // Output JSON results for CI
    println!("\n--- CI Results (JSON) ---");
    println!("PERF_RESULTS=[");
    for (i, result) in results.iter().enumerate() {
        print!("  {}", result.to_json());
        if i < results.len() - 1 {
            println!(",");
        } else {
            println!();
        }
    }
    println!("]");
}

#[test]
fn test_buffering_with_time_control() {
    println!("\n=== Buffering Performance Test (Time-based) ===\n");

    // Test with different time controls
    let configs = [
        ("0", "Immediate flush"),
        ("100", "100ms buffering"),
        ("500", "500ms buffering"),
    ];

    let movetime_ms = 500; // 500ms per move

    for (delay, desc) in &configs {
        println!("Testing {desc} with movetime {movetime_ms}ms: ");

        // Run multiple times and average
        let mut total_time = Duration::ZERO;
        let mut total_lines = 0;
        let mut total_infos = 0;
        let runs = 3;

        for run in 0..runs {
            let (time, lines, infos) = run_search_test_timed(delay, movetime_ms);
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

    // Stress test with moderately deep search (reduced for CI stability)
    let depth = 6;

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

/// Run search test with syscall measurement (Linux only)
fn run_search_test_with_syscalls(
    flush_delay: &str,
    depth: u32,
) -> (Duration, usize, usize, Option<usize>) {
    let mut engine_cmd = Command::new(env!("CARGO_BIN_EXE_engine-cli"));
    engine_cmd.env("USI_FLUSH_DELAY_MS", flush_delay);

    // Try to enable syscall measurement
    let strace_enabled = measure_syscalls(&mut engine_cmd);

    engine_cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(if strace_enabled.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });

    let mut engine = match engine_cmd.spawn() {
        Ok(proc) => proc,
        Err(e) => {
            eprintln!("Failed to spawn engine with syscall measurement: {e}");
            // Fall back to regular test without syscalls
            let (time, lines, infos) = run_search_test(flush_delay, depth);
            return (time, lines, infos, None);
        }
    };

    let mut stdin = engine.stdin.take().expect("Failed to get stdin");
    let stdout = engine.stdout.take().expect("Failed to get stdout");
    let stderr = engine.stderr.take();

    // Channel to signal bestmove reception
    let (bestmove_tx, bestmove_rx) = channel::<()>();

    // Count lines and syscalls (approximated by flush count)
    let reader_handle = thread::spawn(move || {
        let reader = BufReader::new(stdout);
        let mut line_count = 0;
        let mut info_count = 0;
        let bestmove_sender = bestmove_tx;

        for line in reader.lines().map_while(Result::ok) {
            line_count += 1;
            if line.starts_with("info ") && !line.contains("string") {
                info_count += 1;
            }
            // Notify when bestmove is received
            if line.starts_with("bestmove ") {
                let _ = bestmove_sender.send(());
            }
        }
        (line_count, info_count)
    });

    // Read stderr in background if strace is enabled
    let stderr_handle = stderr.map(|stderr| {
        thread::spawn(move || {
            let mut stderr_output = String::new();
            use std::io::Read;
            let mut reader = stderr;
            reader.read_to_string(&mut stderr_output).ok();
            stderr_output
        })
    });

    // Initialize engine with error handling
    if let Err(e) = writeln!(stdin, "usi") {
        eprintln!("Failed to send 'usi' command: {e}");

        // Try to get exit status
        drop(stdin);
        if let Ok(status) = engine.wait() {
            eprintln!("Engine exited with status: {status:?}");
        }

        // Fall back to regular test without syscalls
        let (time, lines, infos) = run_search_test(flush_delay, depth);
        return (time, lines, infos, None);
    }
    stdin.flush().unwrap_or_else(|e| {
        eprintln!("Failed to flush after 'usi': {e}");
    });
    thread::sleep(Duration::from_millis(100));

    if let Err(e) = writeln!(stdin, "isready") {
        eprintln!("Failed to send 'isready' command: {e}");
        drop(stdin);
        let _ = engine.wait();
        let (time, lines, infos) = run_search_test(flush_delay, depth);
        return (time, lines, infos, None);
    }
    stdin.flush().unwrap_or_else(|e| {
        eprintln!("Failed to flush after 'isready': {e}");
    });
    thread::sleep(Duration::from_millis(100));

    if let Err(e) = writeln!(stdin, "position startpos") {
        eprintln!("Failed to send 'position' command: {e}");
        drop(stdin);
        let _ = engine.wait();
        let (time, lines, infos) = run_search_test(flush_delay, depth);
        return (time, lines, infos, None);
    }
    stdin.flush().unwrap_or_else(|e| {
        eprintln!("Failed to flush after 'position': {e}");
    });

    // Measure search time
    let start = Instant::now();
    if let Err(e) = writeln!(stdin, "go depth {depth}") {
        eprintln!("Failed to send 'go' command: {e}");
        drop(stdin);
        let _ = engine.wait();
        let (time, lines, infos) = run_search_test(flush_delay, depth);
        return (time, lines, infos, None);
    }
    stdin.flush().unwrap_or_else(|e| {
        eprintln!("Failed to flush after 'go': {e}");
    });

    // Wait for bestmove with dynamic timeout
    let timeout = Duration::from_secs((depth * 2) as u64 + 5);

    let search_time = match bestmove_rx.recv_timeout(timeout) {
        Ok(()) => start.elapsed(),
        Err(_) => {
            let elapsed = start.elapsed();
            eprintln!(
                "Warning: Timeout waiting for bestmove at depth {}, elapsed: {:.2}s (timeout: {:.2}s)",
                depth,
                elapsed.as_secs_f64(),
                timeout.as_secs_f64()
            );
            elapsed
        }
    };

    // Quit
    let _ = writeln!(stdin, "quit");
    let _ = stdin.flush();
    drop(stdin);

    let _ = engine.wait();
    let (line_count, info_count) = reader_handle.join().unwrap();

    // Parse syscall count if strace was enabled
    let syscall_count = if let Some(handle) = stderr_handle {
        let stderr_output = handle.join().unwrap();
        #[cfg(target_os = "linux")]
        {
            parse_strace_output(stderr_output)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = stderr_output; // Suppress unused variable warning
            None
        }
    } else {
        None
    };

    (search_time, line_count, info_count, syscall_count)
}

#[test]
fn test_syscall_measurement() {
    println!("\n=== Syscall Measurement Test ===\n");

    let configs = [("0", "Immediate flush"), ("100", "100ms buffering")];

    let depth = 3; // Quick test

    for (delay, desc) in &configs {
        println!("Testing {desc}:");
        let (time, lines, infos, syscalls) = run_search_test_with_syscalls(delay, depth);

        println!("  Time: {:.2}s", time.as_secs_f64());
        println!("  Lines: {lines}, Info messages: {infos}");

        if let Some(count) = syscalls {
            println!("  Measured write syscalls: {count}");
        } else {
            println!("  Syscall measurement not available");
        }
        println!();
    }

    // Compare syscall counts
    let (_, _, _, syscalls_immediate) = run_search_test_with_syscalls("0", depth);
    let (_, _, _, syscalls_buffered) = run_search_test_with_syscalls("100", depth);

    if let (Some(immediate), Some(buffered)) = (syscalls_immediate, syscalls_buffered) {
        let reduction = if immediate > 0 {
            ((immediate - buffered) * 100) / immediate
        } else {
            0
        };
        println!("Syscall reduction with buffering: {reduction}%");
        println!("  Immediate: {immediate} syscalls");
        println!("  Buffered: {buffered} syscalls");
    }
}
