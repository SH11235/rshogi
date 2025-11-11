pub mod emergency;
pub mod panic;
pub mod search_helpers;
pub mod usi_helpers;

/// WASM/ブラウザ環境での環境変数アクセスを安全に無効化する薄いラッパ。
///
/// - 非WASM環境: `std::env::var(key).ok()` を返す
/// - WASM (`wasm32-unknown-unknown` 等): 常に `None` を返す（`std` の有無に依らず安全）
#[inline]
pub fn env_var(key: &str) -> Option<String> {
    #[cfg(target_family = "wasm")]
    {
        let _ = key; // silence unused warnings on wasm targets
        None
    }
    #[cfg(not(target_family = "wasm"))]
    {
        std::env::var(key).ok()
    }
}

/// Check if running in CI environment
///
/// Returns true if either CI or GITHUB_ACTIONS environment variable is set.
/// This is useful for skipping performance tests or other CI-inappropriate tests.
///
/// # Examples
///
/// ```rust,no_run
/// #[test]
/// fn test_performance() {
///     if crate::util::is_ci_environment() {
///         println!("Skipping performance test in CI");
///         return;
///     }
///     // ... performance test code ...
/// }
/// ```
#[inline]
pub fn is_ci_environment() -> bool {
    env_var("CI").is_some() || env_var("GITHUB_ACTIONS").is_some()
}
