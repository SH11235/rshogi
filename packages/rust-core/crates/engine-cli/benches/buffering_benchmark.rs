use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc::channel;
use std::thread;
use std::time::{Duration, Instant};

fn run_search_benchmark(flush_delay: &str, depth: u32) -> Duration {
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

    // Count lines in background
    let reader_handle = thread::spawn(move || {
        let reader = BufReader::new(stdout);
        let mut info_count = 0;
        let bestmove_sender = bestmove_tx;

        for line in reader.lines().flatten() {
            if line.starts_with("info ") && !line.contains("string") {
                info_count += 1;
            }
            if line.starts_with("bestmove ") {
                let _ = bestmove_sender.send(());
            }
        }
        info_count
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
    let timeout = Duration::from_secs((depth * 2) as u64 + 5);
    let _ = bestmove_rx.recv_timeout(timeout);
    let search_time = start.elapsed();

    // Quit
    writeln!(stdin, "quit").unwrap();
    stdin.flush().unwrap();
    drop(stdin);

    let _ = engine.wait();
    let _ = reader_handle.join();

    search_time
}

fn bench_buffered_search(c: &mut Criterion) {
    let mut group = c.benchmark_group("buffered_io");

    // Test different depths
    for depth in [3, 4, 5] {
        // Estimate expected info messages based on depth
        let expected_info_msgs = depth as u64 * 2; // Rough estimate
        group.throughput(Throughput::Elements(expected_info_msgs));

        group.bench_with_input(BenchmarkId::new("immediate", depth), &depth, |b, &depth| {
            b.iter(|| run_search_benchmark("0", black_box(depth)))
        });

        group.bench_with_input(BenchmarkId::new("buffered_100ms", depth), &depth, |b, &depth| {
            b.iter(|| run_search_benchmark("100", black_box(depth)))
        });
    }

    group.finish();
}

fn bench_time_based_search(c: &mut Criterion) {
    let mut group = c.benchmark_group("time_based_search");

    // Helper function for time-based searches
    fn run_timed_search(flush_delay: &str, movetime_ms: u64) -> Duration {
        let mut engine = Command::new(env!("CARGO_BIN_EXE_engine-cli"))
            .env("USI_FLUSH_DELAY_MS", flush_delay)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("Failed to spawn engine");

        let mut stdin = engine.stdin.take().expect("Failed to get stdin");
        let stdout = engine.stdout.take().expect("Failed to get stdout");

        let (bestmove_tx, bestmove_rx) = channel::<()>();

        let reader_handle = thread::spawn(move || {
            let reader = BufReader::new(stdout);
            let bestmove_sender = bestmove_tx;

            for line in reader.lines().flatten() {
                if line.starts_with("bestmove ") {
                    let _ = bestmove_sender.send(());
                }
            }
        });

        // Initialize
        writeln!(stdin, "usi").unwrap();
        stdin.flush().unwrap();
        thread::sleep(Duration::from_millis(50));

        writeln!(stdin, "isready").unwrap();
        stdin.flush().unwrap();
        thread::sleep(Duration::from_millis(50));

        writeln!(stdin, "position startpos").unwrap();
        stdin.flush().unwrap();

        // Time-based search
        let start = Instant::now();
        writeln!(stdin, "go movetime {movetime_ms}").unwrap();
        stdin.flush().unwrap();

        let timeout = Duration::from_millis(movetime_ms * 3 + 1000);
        let _ = bestmove_rx.recv_timeout(timeout);
        let search_time = start.elapsed();

        writeln!(stdin, "quit").unwrap();
        stdin.flush().unwrap();
        drop(stdin);

        let _ = engine.wait();
        let _ = reader_handle.join();

        search_time
    }

    // Test different time controls
    for movetime in [100u64, 200, 500] {
        group.bench_with_input(
            BenchmarkId::new("immediate", movetime),
            &movetime,
            |b, &movetime| b.iter(|| run_timed_search("0", black_box(movetime))),
        );

        group.bench_with_input(
            BenchmarkId::new("buffered_100ms", movetime),
            &movetime,
            |b, &movetime| b.iter(|| run_timed_search("100", black_box(movetime))),
        );
    }

    group.finish();
}

criterion_group!(benches, bench_buffered_search, bench_time_based_search);
criterion_main!(benches);
