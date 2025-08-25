//! Deadlock detection using parking_lot
//!
//! This module provides deadlock detection functionality for debug builds.
//! It periodically checks for deadlocks and logs them with structured TSV format.

use log::{error, info, warn};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static DEADLOCK_DETECTOR_INSTALLED: AtomicBool = AtomicBool::new(false);

/// Install deadlock detector for debug builds
///
/// This function starts a background thread that periodically checks for deadlocks
/// in parking_lot mutexes and logs any detected deadlocks.
#[cfg(debug_assertions)]
pub fn install_deadlock_detector() {
    if DEADLOCK_DETECTOR_INSTALLED
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
    {
        thread::spawn(|| {
            info!("Deadlock detector thread started (debug build only)");
            deadlock_detection_thread();
        });
    }
}

#[cfg(not(debug_assertions))]
pub fn install_deadlock_detector() {
    // No-op in release builds
}

#[cfg(debug_assertions)]
fn deadlock_detection_thread() {
    loop {
        thread::sleep(Duration::from_secs(1));

        let deadlocks = parking_lot::deadlock::check_deadlock();
        if !deadlocks.is_empty() {
            let timestamp =
                SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();

            error!("timestamp={timestamp}\tkind=deadlock_detected\tcount={}", deadlocks.len());

            for (i, threads) in deadlocks.iter().enumerate() {
                error!(
                    "timestamp={timestamp}\tkind=deadlock_cycle\tcycle_id={i}\tthread_count={}",
                    threads.len()
                );

                for (j, thread) in threads.iter().enumerate() {
                    // In parking_lot 0.12, DeadlockedThread doesn't have direct thread access
                    // We can still get the backtrace
                    error!(
                        "timestamp={timestamp}\tkind=deadlock_thread\tcycle_id={i}\tthread_index={j}"
                    );

                    // Dump backtrace for each thread in the cycle
                    let bt = thread.backtrace();
                    let bt_str = format!("{:?}", bt);

                    for (frame_idx, frame) in bt_str.lines().enumerate() {
                        warn!(
                            "timestamp={timestamp}\tkind=deadlock_backtrace\tcycle_id={i}\tthread_index={j}\tframe_id={frame_idx}\tframe_data={}",
                            frame.trim()
                        );
                    }
                }
            }

            // Optional: panic on deadlock detection for immediate attention
            if std::env::var("PANIC_ON_DEADLOCK").as_deref() == Ok("1") {
                panic!("Deadlock detected! See logs for details.");
            }
        }
    }
}

/// Check if we're running with deadlock detection enabled
#[allow(dead_code)]
pub fn is_deadlock_detection_enabled() -> bool {
    cfg!(debug_assertions) && DEADLOCK_DETECTOR_INSTALLED.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use std::sync::Arc;

    #[test]
    #[ignore = "Manual test - creates actual deadlock"]
    fn test_deadlock_detection() {
        install_deadlock_detector();

        // Create two mutexes
        let mutex1 = Arc::new(Mutex::new(1));
        let mutex2 = Arc::new(Mutex::new(2));

        let m1_clone = mutex1.clone();
        let m2_clone = mutex2.clone();

        // Thread 1: locks mutex1 then mutex2
        let thread1 = thread::spawn(move || {
            let _guard1 = m1_clone.lock();
            thread::sleep(Duration::from_millis(100));
            let _guard2 = m2_clone.lock(); // This will deadlock
        });

        // Thread 2: locks mutex2 then mutex1
        let thread2 = thread::spawn(move || {
            let _guard2 = mutex2.lock();
            thread::sleep(Duration::from_millis(100));
            let _guard1 = mutex1.lock(); // This will deadlock
        });

        // Give detector time to find the deadlock
        thread::sleep(Duration::from_secs(2));

        // This test will hang due to deadlock - that's expected
        let _ = thread1.join();
        let _ = thread2.join();
    }
}
