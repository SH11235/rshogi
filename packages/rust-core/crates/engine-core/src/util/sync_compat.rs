//! Common utilities for conditional compilation with loom
//!
//! This module provides a unified interface for atomic types and synchronization
//! primitives that switches between standard library and loom implementations
//! based on the `loom` feature flag.

// Atomic types
#[cfg(feature = "loom")]
pub use loom::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, AtomicU8, Ordering};

#[cfg(not(feature = "loom"))]
pub use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, AtomicU8, Ordering};

// Arc
#[cfg(feature = "loom")]
pub use loom::sync::Arc;

#[cfg(not(feature = "loom"))]
pub use std::sync::Arc;

// Thread
#[cfg(feature = "loom")]
pub use loom::thread;

#[cfg(not(feature = "loom"))]
pub use std::thread;
