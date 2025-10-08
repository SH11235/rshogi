//! Centralized getters for environment-driven search policies.
//! Values are cached via OnceLock to avoid repeated env lookups on hot paths.

use std::sync::OnceLock;

#[inline]
pub fn abdada_enabled() -> bool {
    static CACHED: OnceLock<bool> = OnceLock::new();
    *CACHED.get_or_init(|| match std::env::var("SHOGI_ABDADA") {
        Ok(s) => matches!(s.as_str(), "1" | "true" | "on"),
        Err(_) => false,
    })
}

#[inline]
pub fn tt_suppress_below_depth() -> Option<i32> {
    static CACHED: OnceLock<Option<i32>> = OnceLock::new();
    *CACHED.get_or_init(|| {
        std::env::var("SHOGI_TT_SUPPRESS_BELOW_DEPTH")
            .ok()
            .and_then(|s| s.parse::<i32>().ok())
            .filter(|&d| d >= 0)
    })
}

#[inline]
pub fn asp_fail_low_pct() -> i32 {
    static CACHED: OnceLock<i32> = OnceLock::new();
    *CACHED.get_or_init(|| {
        std::env::var("SHOGI_ASP_FAILLOW_PCT")
            .ok()
            .and_then(|s| s.parse::<i32>().ok())
            .map(|v| v.clamp(10, 200))
            .unwrap_or(33)
    })
}

#[inline]
pub fn asp_fail_high_pct() -> i32 {
    static CACHED: OnceLock<i32> = OnceLock::new();
    *CACHED.get_or_init(|| {
        std::env::var("SHOGI_ASP_FAILHIGH_PCT")
            .ok()
            .and_then(|s| s.parse::<i32>().ok())
            .map(|v| v.clamp(10, 200))
            .unwrap_or(33)
    })
}
