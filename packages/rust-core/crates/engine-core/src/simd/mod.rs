//! Common SIMD infrastructure and utilities
//!
//! This module provides shared SIMD functionality used by various components
//! like NNUE and Transposition Table.

/// CPU feature detection and dispatch
pub mod dispatch {
    /// Check if AVX2 is available at runtime
    #[inline]
    pub fn has_avx2() -> bool {
        #[cfg(target_arch = "x86_64")]
        {
            std::is_x86_feature_detected!("avx2")
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            false
        }
    }

    /// Check if SSE2 is available at runtime
    #[inline]
    pub fn has_sse2() -> bool {
        #[cfg(target_arch = "x86_64")]
        {
            std::is_x86_feature_detected!("sse2")
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            false
        }
    }

    /// Check if SSE4.1 is available at runtime
    #[inline]
    pub fn has_sse41() -> bool {
        #[cfg(target_arch = "x86_64")]
        {
            std::is_x86_feature_detected!("sse4.1")
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            false
        }
    }

    /// Select the best available SIMD level
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum SimdLevel {
        Scalar,
        Sse2,
        Sse41,
        Avx2,
    }

    impl SimdLevel {
        /// Detect the best available SIMD level for current CPU
        pub fn detect() -> Self {
            if has_avx2() {
                SimdLevel::Avx2
            } else if has_sse41() {
                SimdLevel::Sse41
            } else if has_sse2() {
                SimdLevel::Sse2
            } else {
                SimdLevel::Scalar
            }
        }
    }
}

/// Common SIMD utilities
pub mod utils {
    /// Alignment helpers
    pub const SIMD_ALIGN_16: usize = 16;
    pub const SIMD_ALIGN_32: usize = 32;
    pub const SIMD_ALIGN_64: usize = 64;

    /// Check if a pointer is aligned to the given boundary
    #[inline]
    pub fn is_aligned<T>(ptr: *const T, align: usize) -> bool {
        (ptr as usize) % align == 0
    }

    /// Round up to the nearest multiple of alignment
    #[inline]
    pub const fn align_up(value: usize, align: usize) -> usize {
        (value + align - 1) & !(align - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simd_detection() {
        let level = dispatch::SimdLevel::detect();
        println!("Detected SIMD level: {:?}", level);

        // Check that we detected a valid SIMD level
        // The level should be one of: Scalar, Sse2, Sse41, or Avx2
        match level {
            dispatch::SimdLevel::Scalar
            | dispatch::SimdLevel::Sse2
            | dispatch::SimdLevel::Sse41
            | dispatch::SimdLevel::Avx2 => {
                // Valid SIMD level detected - test passes
            }
        }
    }

    #[test]
    fn test_alignment() {
        assert_eq!(utils::align_up(0, 16), 0);
        assert_eq!(utils::align_up(1, 16), 16);
        assert_eq!(utils::align_up(15, 16), 16);
        assert_eq!(utils::align_up(16, 16), 16);
        assert_eq!(utils::align_up(17, 16), 32);
    }
}
