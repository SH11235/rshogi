//! WebAssembly SIMD implementations
//!
//! SIMD128 optimized versions for WebAssembly targets

// Note: WASM SIMD support is still experimental and requires specific flags
// For now, we'll provide stubs that fall back to scalar implementations

// Placeholder implementations that use scalar fallbacks
// In the future, these can be replaced with actual WASM SIMD implementations

pub use super::scalar::{
    affine_transform_scalar as affine_transform_simd128,
    clipped_relu_scalar as clipped_relu_simd128,
    transform_features_scalar as transform_features_simd128,
    update_accumulator_scalar as update_accumulator_simd128,
};
