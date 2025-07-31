//! Common utility functions

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
}
