pub mod emergency;
pub mod panic;
pub mod search_helpers;
pub mod usi_helpers;

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
    std::env::var("CI").is_ok() || std::env::var("GITHUB_ACTIONS").is_ok()
}
