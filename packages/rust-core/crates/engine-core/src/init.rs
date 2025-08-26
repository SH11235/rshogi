//! Initialization module for static tables
//!
//! This module provides a safe way to initialize all static tables at once,
//! preventing circular dependencies in lazy initialization.

use std::sync::Once;

static INIT_ONCE: Once = Once::new();

/// Initialize all static tables once and only once.
///
/// This function is safe to call multiple times from different threads.
/// It ensures all static tables are initialized in a safe order,
/// preventing circular dependencies that can cause deadlocks.
///
/// # Safety
///
/// - No logging is performed during initialization to avoid I/O lock cycles
/// - Tables are initialized in dependency order: Zobrist → rays → attacks → board → eval
/// - Each table's initialization must not depend on other lazy_static tables
pub fn init_all_tables_once() {
    INIT_ONCE.call_once(|| {
        // Wrap initialization in panic handler to catch any issues
        let result = std::panic::catch_unwind(|| {
            // Force initialization of all static tables in safe dependency order
            // Note: No logging during initialization to avoid I/O lock cycles

            // Since the static tables are private, we initialize them by calling
            // functions that use them. This ensures they are initialized in a
            // controlled order without circular dependencies.

            // 1. Initialize zobrist by creating a position (uses ZOBRIST internally)
            use crate::Position;
            let _ = Position::startpos();

            // 2. Initialize attack tables by using attacks module functions
            use crate::shogi::attacks;
            use crate::Square;
            let _ = attacks::king_attacks(Square::new(4, 4)); // Center square (5e)

            // 3. Initialize time management tables
            // The MONO_BASE table is initialized on first access, so we don't need
            // to explicitly initialize it here. It will be initialized when needed.

            // 4. Initialize MoveGen related tables by creating an instance
            use crate::MoveGen;
            let _ = MoveGen::new();

            // Note: OnceLock-based tables (SharedStopInfo, SIMD KIND, START_TIME)
            // don't need explicit initialization as they're initialized on first use
            // and don't have circular dependencies
        });

        if let Err(e) = result {
            // Avoid any I/O during initialization to prevent deadlock
            // Re-panic to maintain original behavior
            std::panic::resume_unwind(e);
        }
    });
}

/// Get initialization status for debugging
#[cfg(debug_assertions)]
pub fn is_initialized() -> bool {
    INIT_ONCE.is_completed()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_init_multiple_times_safe() {
        // Should be safe to call multiple times
        init_all_tables_once();
        init_all_tables_once();
        init_all_tables_once();

        #[cfg(debug_assertions)]
        assert!(is_initialized());
    }

    #[test]
    fn test_init_from_multiple_threads() {
        use std::thread;

        let handles: Vec<_> = (0..10)
            .map(|_| {
                thread::spawn(|| {
                    init_all_tables_once();
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }

        #[cfg(debug_assertions)]
        assert!(is_initialized());
    }
}
