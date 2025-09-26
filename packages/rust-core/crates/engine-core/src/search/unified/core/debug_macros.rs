//! Debug macros for unified search
//!
//! Provides macros for conditional debug logging based on compile-time and runtime flags.
//!
//! Policy:
//! - PV-related debug logs are controlled at compile time via the `pv_debug_logs` feature.
//!   Enable with: `cargo build --features engine-core/pv_debug_logs` (transitively via
//!   `engine-usi --features diagnostics`). When disabled, PV logs are not compiled in.
//! - Other ad-hoc debug logs may still use environment-variable guards (e.g., `SHOGI_DEBUG_SEARCH`)
//!   through the generic `debug_log!`/`debug_exec!` helpers below.

/// Macro for debug logging that checks both compile-time and runtime conditions
///
/// This macro reduces code duplication by centralizing the checks for:
/// - `#[cfg(debug_assertions)]`
/// - Environment variable checks (e.g., SHOGI_DEBUG_SEARCH)
///
/// # Examples
///
/// ```rust,no_run
/// # #[macro_use] extern crate engine_core;
/// # fn main() {
/// # let depth = 5;
/// # let ply = 3;
/// # let score = 100;
/// debug_log!(SHOGI_DEBUG_SEARCH, "Search node at ply {}: score={}", ply, score);
/// # }
/// ```
#[macro_export]
macro_rules! debug_log {
    ($env_var:ident, $($arg:tt)*) => {
        #[cfg(debug_assertions)]
        {
            if std::env::var(stringify!($env_var)).is_ok() {
                eprintln!($($arg)*);
            }
        }
    };
}

/// Macro for conditional debug execution
///
/// Executes a block of code only when debug assertions are enabled
/// and the specified environment variable is set.
///
/// # Examples
///
/// ```rust,no_run
/// # #[macro_use] extern crate engine_core;
/// # fn main() {
/// # let items = vec!["item1", "item2", "item3"];
/// debug_exec!(SHOGI_DEBUG_SEARCH, {
///     eprintln!("Complex debug output:");
///     for item in &items {
///         eprintln!("  - {}", item);
///     }
/// });
/// # }
/// ```
#[macro_export]
macro_rules! debug_exec {
    ($env_var:ident, $block:block) => {
        #[cfg(debug_assertions)]
        {
            if std::env::var(stringify!($env_var)).is_ok() {
                $block
            }
        }
    };
}

/// Macro for PV (Principal Variation) specific debug logging
///
/// Convenience macro for PV-related debug logging, compile‑time gated by the pv_debug_logs feature.
///
/// # Examples
///
/// ```rust,no_run
/// # #[macro_use] extern crate engine_core;
/// # fn main() {
/// # let move_str = "7g7f";
/// # let depth = 10;
/// // PV-related logs are compile-time gated by `pv_debug_logs`:
/// //   cargo build -p engine-usi --features diagnostics
/// pv_debug!("Invalid move {} in PV at depth {}", move_str, depth);
/// # }
/// ```
#[macro_export]
macro_rules! pv_debug {
    ($($arg:tt)*) => {
        #[cfg(feature = "pv_debug_logs")]
        {
            eprintln!($($arg)*);
        }
    };
}

/// Execute a block only when PV debug logs are enabled (compile-time)
#[macro_export]
macro_rules! pv_debug_exec {
    ($block:block) => {
        #[cfg(feature = "pv_debug_logs")]
        {
            $block
        }
    };
}

/// Macro for search-specific debug logging
///
/// Convenience wrapper around `debug_log!` for search-related debugging.
///
/// # Examples
///
/// ```rust,no_run
/// # #[macro_use] extern crate engine_core;
/// # fn main() {
/// # let alpha = -1000;
/// # let beta = 1000;
/// # let depth = 5;
/// search_debug!("Alpha-beta window: [{}, {}] at depth {}", alpha, beta, depth);
/// # }
/// ```
#[macro_export]
macro_rules! search_debug {
    ($($arg:tt)*) => {
        $crate::debug_log!(SHOGI_DEBUG_SEARCH, $($arg)*);
    };
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_debug_macros_compile() {
        // Test that macros compile correctly
        debug_log!(SHOGI_DEBUG_TEST, "Test message: {}", 42);

        debug_exec!(SHOGI_DEBUG_TEST, {
            let _x = 1 + 1;
        });

        pv_debug!("PV test: {}", "value");
        search_debug!("Search test: {}", 123);
    }
}
