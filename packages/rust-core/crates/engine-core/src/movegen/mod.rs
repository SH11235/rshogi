mod compat;
pub mod generator;

#[cfg(test)]
mod tests;

// Debug utilities for hang investigation (enabled for all builds during investigation)
pub mod debug;

pub use compat::MoveGen;
