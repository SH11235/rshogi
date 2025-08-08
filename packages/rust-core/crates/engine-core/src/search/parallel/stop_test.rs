//! Minimal stop propagation test module
//!
//! This module tests the most basic stop mechanism to ensure threads
//! can be reliably stopped without complex coordination primitives.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// Minimal worker that just checks stop flag
struct SimpleWorker {
    id: usize,
    stop_flag: Arc<AtomicBool>,
    work_counter: Arc<AtomicU64>,
}

impl SimpleWorker {
    fn new(id: usize, stop_flag: Arc<AtomicBool>, work_counter: Arc<AtomicU64>) -> Self {
        Self {
            id,
            stop_flag,
            work_counter,
        }
    }

    /// Simple work loop that checks stop flag frequently
    fn run(&self) {
        let mut local_counter = 0u64;

        while !self.stop_flag.load(Ordering::Acquire) {
            // Simulate some work
            local_counter += 1;

            // Check stop flag every 1000 iterations
            if local_counter % 1000 == 0 {
                if self.stop_flag.load(Ordering::Acquire) {
                    break;
                }
                // Update global counter periodically
                self.work_counter.fetch_add(1000, Ordering::Relaxed);
            }
        }

        // Final update
        let remainder = local_counter % 1000;
        if remainder > 0 {
            self.work_counter.fetch_add(remainder, Ordering::Relaxed);
        }

        println!("Worker {} stopped after {} iterations", self.id, local_counter);
    }
}

/// Test that all threads stop within reasonable time
pub fn test_basic_stop_propagation() {
    println!("\n=== Testing Basic Stop Propagation ===");

    let num_threads = 4;
    let stop_flag = Arc::new(AtomicBool::new(false));
    let work_counter = Arc::new(AtomicU64::new(0));

    let start_time = Instant::now();

    // Spawn worker threads
    let mut handles = Vec::new();
    for id in 0..num_threads {
        let stop_clone = stop_flag.clone();
        let counter_clone = work_counter.clone();

        let handle = thread::spawn(move || {
            let worker = SimpleWorker::new(id, stop_clone, counter_clone);
            worker.run();
        });

        handles.push(handle);
    }

    // Let threads work for 100ms
    thread::sleep(Duration::from_millis(100));

    // Set stop flag
    println!("Setting stop flag...");
    stop_flag.store(true, Ordering::Release);

    let stop_time = Instant::now();

    // Wait for all threads - they should stop quickly
    let join_start = Instant::now();

    for (id, handle) in handles.into_iter().enumerate() {
        handle.join().expect("Thread panicked");
        println!("Thread {id} joined successfully");
    }

    let join_duration = join_start.elapsed();

    let total_work = work_counter.load(Ordering::Relaxed);
    let stop_latency = stop_time.elapsed();

    println!("\n=== Results ===");
    println!("Total iterations: {total_work}");
    println!("Work duration: {:?}", stop_time.duration_since(start_time));
    println!("Stop latency: {stop_latency:?}");
    println!("Join duration: {join_duration:?}");

    // Allow reasonable time for stop propagation (environment-dependent)
    assert!(join_duration < Duration::from_millis(100), "Threads took too long to join");
    assert!(stop_latency < Duration::from_millis(50), "Stop took too long");
}

/// Test stop propagation under heavy load
pub fn test_stop_under_load() {
    println!("\n=== Testing Stop Under Heavy Load ===");

    let num_threads = 8;
    let stop_flag = Arc::new(AtomicBool::new(false));
    let work_counter = Arc::new(AtomicU64::new(0));

    // Spawn worker threads
    let mut handles = Vec::new();
    for id in 0..num_threads {
        let stop_clone = stop_flag.clone();
        let counter_clone = work_counter.clone();

        let handle = thread::spawn(move || {
            let mut local_counter = 0u64;

            while !stop_clone.load(Ordering::Acquire) {
                // Heavy computation to simulate real work
                let mut sum = 0u64;
                for i in 0..100 {
                    sum = sum.wrapping_add(i);
                }
                local_counter += 1;

                // Less frequent checks under heavy load
                if local_counter % 100 == 0 {
                    if stop_clone.load(Ordering::Acquire) {
                        break;
                    }
                    counter_clone.fetch_add(100, Ordering::Relaxed);
                }

                // Prevent optimization
                std::hint::black_box(sum);
            }

            println!("Heavy worker {id} stopped after {local_counter} iterations");
        });

        handles.push(handle);
    }

    // Let threads work briefly
    thread::sleep(Duration::from_millis(50));

    // Measure stop latency
    let stop_start = Instant::now();
    stop_flag.store(true, Ordering::Release);

    // Join all threads
    for handle in handles {
        handle.join().expect("Thread panicked");
    }

    let stop_duration = stop_start.elapsed();
    println!("Stop duration under load: {stop_duration:?}");

    // More relaxed constraint for heavy load scenario
    assert!(stop_duration < Duration::from_millis(200), "Stop took too long under load");
}

/// Test immediate stop (no work done yet)
pub fn test_immediate_stop() {
    println!("\n=== Testing Immediate Stop ===");

    let stop_flag = Arc::new(AtomicBool::new(true)); // Pre-set to stop
    let work_counter = Arc::new(AtomicU64::new(0));

    let stop_clone = stop_flag.clone();
    let counter_clone = work_counter.clone();

    let handle = thread::spawn(move || {
        let worker = SimpleWorker::new(0, stop_clone, counter_clone);
        worker.run();
    });

    handle.join().expect("Thread panicked");

    let total_work = work_counter.load(Ordering::Relaxed);
    println!("Work done with immediate stop: {total_work}");

    assert_eq!(total_work, 0, "No work should be done with immediate stop");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_stop_tests() {
        test_basic_stop_propagation();
        test_stop_under_load();
        test_immediate_stop();
    }
}
