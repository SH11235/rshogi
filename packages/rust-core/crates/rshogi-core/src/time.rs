#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub use web_time::Instant;

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub use std::time::Instant;
