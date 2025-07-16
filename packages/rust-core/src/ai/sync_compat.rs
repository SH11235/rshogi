//! Common utilities for conditional compilation with loom
//!
//! This module provides a unified interface for atomic types and synchronization
//! primitives that switches between standard library and loom implementations
//! based on the `loom` feature flag.

#[cfg(all(feature = "loom", not(target_arch = "wasm32")))]
pub use loom::sync::atomic::{AtomicU64, Ordering};

#[cfg(not(all(feature = "loom", not(target_arch = "wasm32"))))]
pub use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(all(feature = "loom", not(target_arch = "wasm32")))]
pub use loom::sync::Arc;

#[cfg(not(all(feature = "loom", not(target_arch = "wasm32"))))]
pub use std::sync::Arc;

#[cfg(all(feature = "loom", not(target_arch = "wasm32")))]
pub use loom::thread;

#[cfg(not(all(feature = "loom", not(target_arch = "wasm32"))))]
pub use std::thread;
