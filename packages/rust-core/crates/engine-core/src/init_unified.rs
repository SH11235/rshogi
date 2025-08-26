//! Unified initialization module - consolidates all static initialization
//!
//! This module provides a single entry point for all engine initialization,
//! preventing duplicate initialization and ensuring proper ordering without
//! circular dependencies.

use std::sync::Once;

static INIT_ONCE: Once = Once::new();

/// Initialize all engine tables in the correct order
///
/// This function is safe to call multiple times from different threads.
/// It combines warmup (no I/O, no env vars) and remaining initialization.
///
/// Initialization order:
/// 1. Zobrist tables (via Position::startpos)
/// 2. Attack tables (via attacks module)
/// 3. Other static tables (via init_all_tables_once)
///
/// Note: MoveGen is intentionally NOT initialized here to avoid circular dependencies.
pub fn init_engine_tables() {
    INIT_ONCE.call_once(|| {
        // Phase 1: Warmup - initialize tables that might be used during I/O setup
        // This prevents deadlocks in subprocess contexts
        warm_up_static_tables_internal();

        // Phase 2: Remaining initialization
        // This initializes other tables that don't risk circular dependencies
        init_remaining_tables();
    });
}

/// Internal warmup function - no I/O, no environment variables
fn warm_up_static_tables_internal() {
    use crate::shogi::attacks;
    use crate::Square;

    // Initialize Zobrist tables by creating a position
    let _ = crate::Position::startpos();

    // Initialize attack tables by calling each type
    let center = Square::new(4, 4); // 5e
    let _ = attacks::king_attacks(center);
    let _ = attacks::gold_attacks(center, crate::Color::Black);
    let _ = attacks::silver_attacks(center, crate::Color::Black);
    let _ = attacks::knight_attacks(center, crate::Color::Black);
    let _ = attacks::pawn_attacks(center, crate::Color::Black);
    let _ = attacks::lance_attacks(center, crate::Color::Black);
    let _ = attacks::sliding_attacks(center, crate::Bitboard::EMPTY, crate::PieceType::Rook);
    let _ = attacks::sliding_attacks(center, crate::Bitboard::EMPTY, crate::PieceType::Bishop);
}

/// Initialize remaining tables that aren't covered by warmup
fn init_remaining_tables() {
    // Currently, all critical tables are initialized in warmup
    // This function is kept for future extensions that don't risk
    // circular dependencies or I/O issues

    // Note: We do NOT call init::init_all_tables_once() here because:
    // 1. It tries to create MoveGen which can cause circular dependencies
    // 2. All necessary tables are already initialized in warmup
    // 3. Any remaining lazy_static tables will initialize on first use
}

/// Check if initialization is complete (for testing)
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
        init_engine_tables();
        init_engine_tables();
        init_engine_tables();

        #[cfg(debug_assertions)]
        assert!(is_initialized());
    }

    #[test]
    fn test_init_from_multiple_threads() {
        use std::thread;

        let handles: Vec<_> = (0..10)
            .map(|_| {
                thread::spawn(|| {
                    init_engine_tables();
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
