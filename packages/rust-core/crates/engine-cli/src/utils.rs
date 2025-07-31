//! Common utility functions

use std::sync::{Mutex, MutexGuard};

/// Generic helper function to lock a mutex with recovery for Poisoned state
pub fn lock_or_recover_generic<T>(mutex: &Mutex<T>) -> MutexGuard<T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            log::error!("Mutex was poisoned, attempting recovery");
            poisoned.into_inner()
        }
    }
}
