//! Common utility functions
//!
//! This module contains shared utility functions used across the engine adapter,
//! including move comparison, score conversion, synchronization utilities,
//! and I/O detection for subprocess handling.

use crate::usi::output::Score;
use engine_core::search::constants::{MATE_SCORE, MAX_PLY};
use once_cell::sync::OnceCell;
use std::sync::atomic::AtomicU64;
use std::sync::{Mutex, MutexGuard};

/// Generic helper function to lock a mutex with recovery for Poisoned state
pub fn lock_or_recover_generic<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            log::error!("Mutex was poisoned, attempting recovery");
            poisoned.into_inner()
        }
    }
}

/// Convert raw engine score to USI score format (Cp or Mate)
///
/// This function transforms internal engine scores to the USI protocol format:
/// - Regular evaluations are reported as centipawns (cp)
/// - Mate scores are converted to "mate in N moves" format
///
/// # Arguments
///
/// * `raw_score` - The raw score from the engine (in centipawns or mate score)
///
/// # Returns
///
/// A `Score` enum variant representing either centipawns or mate distance
///
/// # Notes
///
/// - Mate scores are identified when the absolute value exceeds `MATE_SCORE - MAX_PLY`
/// - Immediate mate (0 moves) is reported as "mate 0" (USI spec compliant)
///   Note: Some legacy GUIs might prefer "mate 1"; if needed, consider an option later
/// - Positive scores favor the side to move, negative scores favor the opponent
pub fn to_usi_score(raw_score: i32) -> Score {
    if raw_score.abs() >= MATE_SCORE - MAX_PLY as i32 {
        // It's a mate score - calculate mate distance
        let plies_to_mate = MATE_SCORE - raw_score.abs();
        // Calculate mate in moves (1 move = 2 plies)
        let mate_in = (plies_to_mate + 1) / 2;
        // Note: USI spec allows "mate 0" for immediate mate.
        // Some older GUIs may have issues with "mate 0", but we follow the spec.
        // TODO: Consider adding a USI option for "mate0_to_1" compatibility mode if needed

        // USI spec: positive mate N means we have mate in N moves,
        // negative mate N means we are being mated in N moves
        // Note: "mate -0" doesn't exist in USI. Both winning and losing immediate mate
        // are represented as "mate 0". The sign must be determined from context.
        if raw_score > 0 {
            Score::Mate(mate_in)
        } else {
            Score::Mate(-mate_in)
        }
    } else {
        Score::Cp(raw_score)
    }
}

/// Counter for tracking timeout occurrences in has_legal_moves_with_timeout
pub static HUNG_MOVEGEN_CHECKS: AtomicU64 = AtomicU64::new(0);

/// Check if any of the standard I/O streams are piped (not TTY)
///
/// This function detects if the engine is running with piped I/O, which typically
/// indicates it's being used as a subprocess by a GUI or other controller.
/// The result is cached for efficiency.
///
/// # Returns
///
/// `true` if any of stdin, stdout, or stderr are piped; `false` if all are TTY
pub fn is_piped_stdio() -> bool {
    static CACHED: OnceCell<bool> = OnceCell::new();
    *CACHED.get_or_init(|| {
        !atty::is(atty::Stream::Stdin)
            || !atty::is(atty::Stream::Stdout)
            || !atty::is(atty::Stream::Stderr)
    })
}

/// Check if running as subprocess or with piped I/O
///
/// This combines the SUBPROCESS_MODE environment variable check with piped I/O detection.
/// Used to determine when to apply subprocess-specific behavior like skipping
/// potentially hanging operations.
///
/// # Returns
///
/// `true` if either SUBPROCESS_MODE is set or any I/O is piped
pub fn is_subprocess_or_piped() -> bool {
    std::env::var("SUBPROCESS_MODE").is_ok() || is_piped_stdio()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::thread;

    #[test]
    fn test_lock_or_recover_generic_with_poisoned_mutex() {
        // Test with different types to ensure generic works correctly

        // Test 1: String type
        let mutex_string = Arc::new(Mutex::new(String::from("initial")));
        let mutex_clone = mutex_string.clone();

        // Spawn thread that will panic while holding the lock
        let handle = thread::spawn(move || {
            let _guard = mutex_clone.lock().unwrap();
            panic!("Intentional panic to poison mutex");
        });

        // Wait for thread to panic
        let _ = handle.join();

        // Verify mutex is poisoned
        assert!(mutex_string.lock().is_err(), "Mutex should be poisoned");

        // Use our recovery function
        {
            let mut recovered = lock_or_recover_generic(&mutex_string);
            *recovered = String::from("recovered");
        }

        // Verify we can use the mutex again
        {
            let guard = lock_or_recover_generic(&mutex_string);
            assert_eq!(*guard, "recovered", "Should have recovered value");
        }

        // Test 2: i32 type
        let mutex_i32 = Arc::new(Mutex::new(42i32));
        let mutex_clone = mutex_i32.clone();

        let handle = thread::spawn(move || {
            let _guard = mutex_clone.lock().unwrap();
            panic!("Intentional panic");
        });

        let _ = handle.join();

        {
            let mut recovered = lock_or_recover_generic(&mutex_i32);
            *recovered = 100;
        }

        {
            let guard = lock_or_recover_generic(&mutex_i32);
            assert_eq!(*guard, 100);
        }

        // Test 3: Complex struct
        #[derive(Debug, PartialEq)]
        struct TestStruct {
            value: i32,
            name: String,
        }

        let mutex_struct = Arc::new(Mutex::new(TestStruct {
            value: 1,
            name: String::from("test"),
        }));
        let mutex_clone = mutex_struct.clone();

        let handle = thread::spawn(move || {
            let _guard = mutex_clone.lock().unwrap();
            panic!("Intentional panic");
        });

        let _ = handle.join();

        {
            let mut recovered = lock_or_recover_generic(&mutex_struct);
            recovered.value = 2;
            recovered.name = String::from("recovered");
        }

        {
            let guard = lock_or_recover_generic(&mutex_struct);
            assert_eq!(guard.value, 2);
            assert_eq!(guard.name, "recovered");
        }
    }

    #[test]
    fn test_lock_or_recover_generic_normal_operation() {
        // Test that function works normally when mutex is not poisoned
        let mutex = Mutex::new(vec![1, 2, 3]);

        // Normal lock should work
        {
            let mut guard = lock_or_recover_generic(&mutex);
            guard.push(4);
        }

        // Verify data is correct
        {
            let guard = lock_or_recover_generic(&mutex);
            assert_eq!(*guard, vec![1, 2, 3, 4]);
        }
    }

    #[test]
    fn test_lock_or_recover_generic_multiple_recoveries() {
        // Test multiple poison-recovery cycles
        let mutex = Arc::new(Mutex::new(0u64));

        for i in 0..5 {
            let mutex_clone = mutex.clone();

            // Poison the mutex
            let handle = thread::spawn(move || {
                let _guard = mutex_clone.lock().unwrap();
                panic!("Panic #{i}");
            });

            let _ = handle.join();

            // Recover and update
            {
                let mut guard = lock_or_recover_generic(&mutex);
                *guard = i + 1;
            }

            // Verify recovery worked
            {
                let guard = lock_or_recover_generic(&mutex);
                assert_eq!(*guard, i + 1);
            }
        }
    }

    #[test]
    fn test_to_usi_score() {
        // Test regular centipawn scores
        assert_eq!(to_usi_score(100), Score::Cp(100));
        assert_eq!(to_usi_score(-200), Score::Cp(-200));
        assert_eq!(to_usi_score(0), Score::Cp(0));

        // Test mate scores
        // Assuming MATE_SCORE = 30000 and MAX_PLY = 128
        let mate_threshold = MATE_SCORE - MAX_PLY as i32;

        // Mate in 1 (2 plies from mate)
        assert_eq!(to_usi_score(MATE_SCORE - 2), Score::Mate(1));
        assert_eq!(to_usi_score(-(MATE_SCORE - 2)), Score::Mate(-1));

        // Mate in 3 (6 plies from mate)
        assert_eq!(to_usi_score(MATE_SCORE - 6), Score::Mate(3));
        assert_eq!(to_usi_score(-(MATE_SCORE - 6)), Score::Mate(-3));

        // Immediate mate (0 plies) should be reported as mate 0
        assert_eq!(to_usi_score(MATE_SCORE), Score::Mate(0));
        assert_eq!(to_usi_score(-MATE_SCORE), Score::Mate(0));

        // Score just below mate threshold
        assert_eq!(to_usi_score(mate_threshold - 1), Score::Cp(mate_threshold - 1));
        assert_eq!(to_usi_score(-(mate_threshold - 1)), Score::Cp(-(mate_threshold - 1)));
    }

    #[test]
    fn test_is_piped_stdio() {
        // This test is primarily for ensuring the function doesn't panic
        // The actual result depends on the test environment
        let _ = is_piped_stdio();
        // Calling again should use cached value
        let _ = is_piped_stdio();
    }

    #[test]
    fn test_is_subprocess_or_piped() {
        // Save original env var state
        let original = std::env::var("SUBPROCESS_MODE").ok();
        
        // Test without SUBPROCESS_MODE
        std::env::remove_var("SUBPROCESS_MODE");
        let without_env = is_subprocess_or_piped();
        
        // Test with SUBPROCESS_MODE
        std::env::set_var("SUBPROCESS_MODE", "1");
        let with_env = is_subprocess_or_piped();
        
        // With env var set, should always be true
        assert!(with_env);
        
        // Restore original state
        if let Some(val) = original {
            std::env::set_var("SUBPROCESS_MODE", val);
        } else {
            std::env::remove_var("SUBPROCESS_MODE");
        }
        
        // Without env var, result depends on actual I/O state
        let _ = without_env;
    }

    #[test]
    fn test_hung_movegen_counter() {
        // Store initial value
        let initial = HUNG_MOVEGEN_CHECKS.load(Ordering::Relaxed);
        
        // Increment counter
        HUNG_MOVEGEN_CHECKS.fetch_add(1, Ordering::Relaxed);
        assert_eq!(HUNG_MOVEGEN_CHECKS.load(Ordering::Relaxed), initial + 1);
        
        // Increment again
        HUNG_MOVEGEN_CHECKS.fetch_add(1, Ordering::Relaxed);
        assert_eq!(HUNG_MOVEGEN_CHECKS.load(Ordering::Relaxed), initial + 2);
    }
}
