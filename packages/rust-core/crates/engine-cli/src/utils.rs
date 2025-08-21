//! Common utility functions
//!
//! This module contains shared utility functions used across the engine adapter,
//! including move comparison, score conversion, and synchronization utilities.

use crate::usi::output::Score;
use engine_core::search::constants::{MATE_SCORE, MAX_PLY};
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
/// - For GUI compatibility, immediate mate (0 moves) is reported as "mate 1"
/// - Positive scores favor the side to move, negative scores favor the opponent
pub(crate) fn to_usi_score(raw_score: i32) -> Score {
    if raw_score.abs() >= MATE_SCORE - MAX_PLY as i32 {
        // It's a mate score - calculate mate distance
        let mate_in_half = MATE_SCORE - raw_score.abs();
        // Calculate mate in moves (1 move = 2 plies)
        let mate_in = (mate_in_half + 1) / 2;
        // Note: USI spec allows "mate 0" for immediate mate.
        // Some older GUIs may have issues with "mate 0", but we follow the spec.
        // TODO: Consider adding a USI option for "mate0_to_1" compatibility mode if needed
        
        // USI spec: positive mate N means we have mate in N moves,
        // negative mate N means we are being mated in N moves
        // Special case: to distinguish between winning and losing immediate mate,
        // we use mate 1 / mate -1 instead of mate 0 / mate -0 (which are the same)
        if raw_score > 0 {
            Score::Mate(mate_in.max(1))
        } else {
            Score::Mate(-(mate_in.max(1)))
        }
    } else {
        Score::Cp(raw_score)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
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
}
