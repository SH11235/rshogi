use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc::channel;
use std::thread;
use std::time::{Duration, Instant};

/// Common engine runner with improved initialization and cleanup
fn run_engine_with_command<F>(flush_delay: &str, command_fn: F) -> (Duration, usize)
where
    F: FnOnce(&mut dyn Write) -> std::io::Result<()>,
{
    let mut engine = Command::new(env!("CARGO_BIN_EXE_engine-cli"))
        .env("USI_FLUSH_DELAY_MS", flush_delay)
        .env("USI_BENCH_MODE", "1") // Enable 0ms flush for benchmarks
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("Failed to spawn engine");

    let mut stdin = engine.stdin.take().expect("Failed to get stdin");
    let stdout = engine.stdout.take().expect("Failed to get stdout");

    // Channels for various signals
    let (usiok_tx, usiok_rx) = channel::<()>();
    let (readyok_tx, readyok_rx) = channel::<()>();
    let (bestmove_tx, bestmove_rx) = channel::<()>();
    let (info_count_tx, info_count_rx) = channel::<usize>();

    // Reader thread
    let reader_handle = thread::spawn(move || {
        let reader = BufReader::new(stdout);
        let mut info_count = 0;
        let usiok_sender = usiok_tx;
        let readyok_sender = readyok_tx;
        let bestmove_sender = bestmove_tx;
        let info_count_sender = info_count_tx;

        for line in reader.lines().map_while(Result::ok) {
            if line == "usiok" {
                let _ = usiok_sender.send(());
            } else if line == "readyok" {
                let _ = readyok_sender.send(());
            } else if line.starts_with("info ") && !line.contains("string") {
                info_count += 1;
            } else if line.starts_with("bestmove ") {
                let _ = bestmove_sender.send(());
                let _ = info_count_sender.send(info_count);
                break; // Exit after bestmove
            }
        }
    });

    // Initialize engine with proper synchronization
    writeln!(stdin, "usi").unwrap();
    stdin.flush().unwrap();

    // Wait for usiok
    if usiok_rx.recv_timeout(Duration::from_secs(5)).is_err() {
        eprintln!("Warning: Timeout waiting for usiok");
        let _ = engine.kill();
        panic!("Engine initialization failed");
    }

    writeln!(stdin, "isready").unwrap();
    stdin.flush().unwrap();

    // Wait for readyok
    if readyok_rx.recv_timeout(Duration::from_secs(5)).is_err() {
        eprintln!("Warning: Timeout waiting for readyok");
        let _ = engine.kill();
        panic!("Engine initialization failed");
    }

    writeln!(stdin, "position startpos").unwrap();
    stdin.flush().unwrap();

    // Execute the search command and measure time
    let start = Instant::now();
    command_fn(&mut stdin).unwrap();
    stdin.flush().unwrap();

    // Wait for bestmove with appropriate timeout
    let timeout = Duration::from_secs(30); // Conservative timeout
    let search_time = match bestmove_rx.recv_timeout(timeout) {
        Ok(()) => start.elapsed(),
        Err(_) => {
            eprintln!("Warning: Timeout waiting for bestmove");
            // Kill the engine to prevent leak
            let _ = engine.kill();
            start.elapsed()
        }
    };

    // Get info count
    let info_count = info_count_rx.recv_timeout(Duration::from_millis(100)).unwrap_or(0);

    // Clean shutdown
    writeln!(stdin, "quit").unwrap();
    stdin.flush().unwrap();
    drop(stdin);

    // Give the process a chance to exit cleanly
    thread::sleep(Duration::from_millis(100));

    // Force kill if still running and wait for exit
    let _ = engine.kill();
    let _ = engine.wait();

    let _ = reader_handle.join();

    (search_time, info_count)
}

fn bench_buffered_search(c: &mut Criterion) {
    check_buffered_io_feature();

    let mut group = c.benchmark_group("buffered_io");

    // Set measurement time for expensive benchmarks
    group.measurement_time(Duration::from_secs(30));
    group.sample_size(20); // Reduce sample size for faster runs

    // Test different depths
    for depth in [3, 4, 5] {
        // Run once to get actual info count for throughput
        let (_, info_count) =
            run_engine_with_command("0", |stdin| writeln!(stdin, "go depth {depth}"));

        group.throughput(Throughput::Elements(info_count as u64));

        group.bench_with_input(BenchmarkId::new("immediate", depth), &depth, |b, &depth| {
            b.iter(|| {
                let (duration, _) = run_engine_with_command("0", |stdin| {
                    writeln!(stdin, "go depth {}", black_box(depth))
                });
                duration
            })
        });

        group.bench_with_input(BenchmarkId::new("buffered_100ms", depth), &depth, |b, &depth| {
            b.iter(|| {
                let (duration, _) = run_engine_with_command("100", |stdin| {
                    writeln!(stdin, "go depth {}", black_box(depth))
                });
                duration
            })
        });
    }

    group.finish();
}

fn bench_time_based_search(c: &mut Criterion) {
    let mut group = c.benchmark_group("time_based_search");

    // Set measurement time
    group.measurement_time(Duration::from_secs(20));
    group.sample_size(10); // Even smaller sample size for time-based tests

    // Test different time controls
    for movetime in [100u64, 200, 500] {
        group.bench_with_input(
            BenchmarkId::new("immediate", movetime),
            &movetime,
            |b, &movetime| {
                b.iter(|| {
                    let (duration, _) = run_engine_with_command("0", |stdin| {
                        writeln!(stdin, "go movetime {}", black_box(movetime))
                    });
                    duration
                })
            },
        );

        group.bench_with_input(
            BenchmarkId::new("buffered_100ms", movetime),
            &movetime,
            |b, &movetime| {
                b.iter(|| {
                    let (duration, _) = run_engine_with_command("100", |stdin| {
                        writeln!(stdin, "go movetime {}", black_box(movetime))
                    });
                    duration
                })
            },
        );
    }

    group.finish();
}

// Check if buffered-io feature is enabled
fn check_buffered_io_feature() {
    #[cfg(not(feature = "buffered-io"))]
    {
        eprintln!("Warning: Running benchmarks without --features buffered-io");
        eprintln!("Buffering benchmarks may not show expected differences.");
        eprintln!("Run with: cargo bench --features buffered-io");
    }
}

criterion_group! {
    name = benches;
    config = Criterion::default().with_plots();
    targets = bench_buffered_search, bench_time_based_search
}

criterion_main!(benches);
