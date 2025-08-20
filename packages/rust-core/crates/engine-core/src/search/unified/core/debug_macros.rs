//! Debug macros for unified search
//!
//! Provides macros for conditional debug logging based on compile-time and runtime flags

/// Macro for debug logging that checks both compile-time and runtime conditions
///
/// This macro reduces code duplication by centralizing the checks for:
/// - `#[cfg(debug_assertions)]`
/// - Environment variable checks (e.g., SHOGI_DEBUG_PV, SHOGI_DEBUG_SEARCH)
///
/// # Examples
///
/// ```rust,ignore
/// use crate::debug_log;
/// debug_log!(SHOGI_DEBUG_PV, "PV validation failed at depth {depth}");
/// debug_log!(SHOGI_DEBUG_SEARCH, "Search node at ply {}: score={}", ply, score);
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
/// ```rust,ignore
/// use crate::debug_exec;
/// debug_exec!(SHOGI_DEBUG_PV, {
///     eprintln!("Complex debug output:");
///     for item in &items {
///         eprintln!("  - {}", item);
///     }
/// });
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
/// Convenience wrapper around `debug_log!` for PV-related debugging.
///
/// # Examples
///
/// ```rust,ignore
/// use crate::pv_debug;
/// pv_debug!("Invalid move {} in PV at depth {}", move_str, depth);
/// ```
#[macro_export]
macro_rules! pv_debug {
    ($($arg:tt)*) => {
        $crate::debug_log!(SHOGI_DEBUG_PV, $($arg)*);
    };
}

/// Macro for search-specific debug logging
///
/// Convenience wrapper around `debug_log!` for search-related debugging.
///
/// # Examples
///
/// ```rust,ignore
/// use crate::search_debug;
/// search_debug!("Alpha-beta window: [{}, {}] at depth {}", alpha, beta, depth);
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
