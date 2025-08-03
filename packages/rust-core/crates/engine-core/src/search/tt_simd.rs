//! SIMD-optimized operations for Transposition Table
//!
//! This module provides SIMD implementations for performance-critical
//! TT operations, with automatic fallback to scalar implementations.

#[allow(unused_imports)]
use crate::search::tt::{AGE_MASK, GENERATION_CYCLE};

// Scalar implementations (fallback)
pub mod scalar {
    use crate::search::tt::{AGE_MASK, GENERATION_CYCLE};

    /// Scalar implementation of key search
    pub fn find_matching_key(keys: &[u64], target: u64) -> Option<usize> {
        keys.iter().position(|&key| key == target)
    }

    /// Scalar implementation of priority score calculation
    pub fn calculate_priority_scores(
        depths: &[u8; 4],
        ages: &[u8; 4],
        is_pv: &[bool; 4],
        is_exact: &[bool; 4],
        current_age: u8,
    ) -> [i32; 4] {
        let mut scores = [0i32; 4];

        for i in 0..4 {
            // Calculate cyclic distance (Apery-style)
            let age_distance = ((GENERATION_CYCLE + current_age as u16 - ages[i] as u16)
                & (AGE_MASK as u16)) as i32;

            // Base priority: depth minus age distance
            let mut priority = depths[i] as i32 - age_distance;

            // Bonus for PV nodes
            if is_pv[i] {
                priority += 32;
            }

            // Bonus for exact entries
            if is_exact[i] {
                priority += 16;
            }

            scores[i] = priority;
        }

        scores
    }
}

// x86_64 SIMD implementations
#[cfg(target_arch = "x86_64")]
pub mod x86_64 {
    use crate::search::tt::{AGE_MASK, GENERATION_CYCLE};
    use std::arch::x86_64::*;

    /// AVX2 implementation of key search
    ///
    /// # Safety
    /// Requires AVX2 CPU feature. Caller must ensure AVX2 is available.
    #[target_feature(enable = "avx2")]
    pub unsafe fn find_matching_key_avx2(keys: &[u64], target: u64) -> Option<usize> {
        debug_assert_eq!(keys.len(), 4);

        // Load 4 keys into 256-bit register
        let keys_vec = _mm256_loadu_si256(keys.as_ptr() as *const __m256i);

        // Broadcast target to all lanes
        let target_vec = _mm256_set1_epi64x(target as i64);

        // Compare all 4 keys simultaneously
        let cmp_result = _mm256_cmpeq_epi64(keys_vec, target_vec);

        // Extract comparison mask (each matching 64-bit gives 0xFF bytes)
        let mask = _mm256_movemask_epi8(cmp_result) as u32;

        if mask != 0 {
            // Each 64-bit match produces 8 consecutive 0xFF bytes
            // So we need to find the first set of 8 0xFF bytes
            match mask {
                m if m & 0x000000FF == 0x000000FF => Some(0),
                m if m & 0x0000FF00 == 0x0000FF00 => Some(1),
                m if m & 0x00FF0000 == 0x00FF0000 => Some(2),
                m if m & 0xFF000000 == 0xFF000000 => Some(3),
                _ => None,
            }
        } else {
            None
        }
    }

    /// SSE2 implementation of key search
    ///
    /// # Safety
    /// Requires SSE2 CPU feature. Caller must ensure SSE2 is available.
    #[target_feature(enable = "sse2")]
    pub unsafe fn find_matching_key_sse2(keys: &[u64], target: u64) -> Option<usize> {
        debug_assert_eq!(keys.len(), 4);

        // Process 2 keys at a time with SSE2 (128-bit registers)
        for chunk_idx in 0..2 {
            let offset = chunk_idx * 2;

            // Load 2 keys
            let keys_vec = _mm_loadu_si128(keys[offset..].as_ptr() as *const __m128i);

            // Broadcast target to both lanes
            let target_vec = _mm_set1_epi64x(target as i64);

            // Compare 2 keys
            let cmp_result = _mm_cmpeq_epi32(keys_vec, target_vec);

            // Get comparison mask
            let mask = _mm_movemask_epi8(cmp_result) as u16;

            if mask != 0 {
                // Check which 64-bit value matched
                if mask & 0x00FF == 0x00FF {
                    return Some(offset);
                }
                if mask & 0xFF00 == 0xFF00 {
                    return Some(offset + 1);
                }
            }
        }

        None
    }

    /// AVX2 implementation of priority score calculation
    ///
    /// # Safety
    /// Requires AVX2 CPU feature. Caller must ensure AVX2 is available.
    #[target_feature(enable = "avx2")]
    pub unsafe fn calculate_priority_scores_avx2(
        depths: &[u8; 4],
        ages: &[u8; 4],
        is_pv: &[bool; 4],
        is_exact: &[bool; 4],
        current_age: u8,
    ) -> [i32; 4] {
        // Convert arrays to i32 for SIMD processing
        let depths_i32 = [
            depths[0] as i32,
            depths[1] as i32,
            depths[2] as i32,
            depths[3] as i32,
        ];

        let ages_i32 = [
            ages[0] as i32,
            ages[1] as i32,
            ages[2] as i32,
            ages[3] as i32,
        ];

        // Load depths and ages into SIMD registers
        let depths_vec = _mm_loadu_si128(depths_i32.as_ptr() as *const __m128i);
        let ages_vec = _mm_loadu_si128(ages_i32.as_ptr() as *const __m128i);

        // Calculate age distances
        let current_age_vec = _mm_set1_epi32(current_age as i32);
        let cycle_vec = _mm_set1_epi32(GENERATION_CYCLE as i32);
        let mask_vec = _mm_set1_epi32(AGE_MASK as i32);

        // age_distance = ((GENERATION_CYCLE + current_age - age) & AGE_MASK)
        let age_dist = _mm_add_epi32(cycle_vec, current_age_vec);
        let age_dist = _mm_sub_epi32(age_dist, ages_vec);
        let age_dist = _mm_and_si128(age_dist, mask_vec);

        // priority = depth - age_distance
        let mut priority = _mm_sub_epi32(depths_vec, age_dist);

        // Add PV bonus (32 if PV node)
        let pv_bonus = [
            if is_pv[0] { 32 } else { 0 },
            if is_pv[1] { 32 } else { 0 },
            if is_pv[2] { 32 } else { 0 },
            if is_pv[3] { 32 } else { 0 },
        ];
        let pv_vec = _mm_loadu_si128(pv_bonus.as_ptr() as *const __m128i);
        priority = _mm_add_epi32(priority, pv_vec);

        // Add exact bonus (16 if exact node)
        let exact_bonus = [
            if is_exact[0] { 16 } else { 0 },
            if is_exact[1] { 16 } else { 0 },
            if is_exact[2] { 16 } else { 0 },
            if is_exact[3] { 16 } else { 0 },
        ];
        let exact_vec = _mm_loadu_si128(exact_bonus.as_ptr() as *const __m128i);
        priority = _mm_add_epi32(priority, exact_vec);

        // Store results
        let mut results = [0i32; 4];
        _mm_storeu_si128(results.as_mut_ptr() as *mut __m128i, priority);

        results
    }
}

/// SIMD operations dispatcher
pub mod simd {
    /// Find matching key in array using SIMD when available
    ///
    /// # Arguments
    /// * `keys` - Array of keys to search (must be length 4)
    /// * `target` - Target key to find
    ///
    /// # Returns
    /// Index of matching key, or None if not found
    pub fn find_matching_key(keys: &[u64], target: u64) -> Option<usize> {
        debug_assert_eq!(keys.len(), 4, "SIMD operations require exactly 4 keys");

        #[cfg(target_arch = "x86_64")]
        {
            #[cfg(target_feature = "avx2")]
            {
                return unsafe { super::x86_64::find_matching_key_avx2(keys, target) };
            }

            if std::is_x86_feature_detected!("avx2") {
                return unsafe { super::x86_64::find_matching_key_avx2(keys, target) };
            }

            if std::is_x86_feature_detected!("sse2") {
                return unsafe { super::x86_64::find_matching_key_sse2(keys, target) };
            }
        }

        // Fallback to scalar implementation
        super::scalar::find_matching_key(keys, target)
    }

    /// Calculate priority scores for multiple entries using SIMD
    pub fn calculate_priority_scores(
        depths: &[u8; 4],
        ages: &[u8; 4],
        is_pv: &[bool; 4],
        is_exact: &[bool; 4],
        current_age: u8,
    ) -> [i32; 4] {
        #[cfg(target_arch = "x86_64")]
        {
            #[cfg(target_feature = "avx2")]
            {
                return unsafe {
                    super::x86_64::calculate_priority_scores_avx2(
                        depths,
                        ages,
                        is_pv,
                        is_exact,
                        current_age,
                    )
                };
            }

            if std::is_x86_feature_detected!("avx2") {
                return unsafe {
                    super::x86_64::calculate_priority_scores_avx2(
                        depths,
                        ages,
                        is_pv,
                        is_exact,
                        current_age,
                    )
                };
            }
        }

        super::scalar::calculate_priority_scores(depths, ages, is_pv, is_exact, current_age)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_matching_key() {
        let keys = [
            0x1234567890ABCDEF,
            0xFEDCBA0987654321,
            0x1111111111111111,
            0x2222222222222222,
        ];

        // Test scalar implementation
        assert_eq!(scalar::find_matching_key(&keys, 0x1111111111111111), Some(2));
        assert_eq!(scalar::find_matching_key(&keys, 0x3333333333333333), None);

        // Test SIMD implementation (will use scalar if SIMD not available)
        assert_eq!(simd::find_matching_key(&keys, 0x1111111111111111), Some(2));
        assert_eq!(simd::find_matching_key(&keys, 0x3333333333333333), None);
    }

    #[test]
    fn test_calculate_priority_scores() {
        let depths = [10, 20, 15, 5];
        let ages = [0, 1, 2, 3];
        let is_pv = [true, false, true, false];
        let is_exact = [true, true, false, false];
        let current_age = 4;

        // Test scalar implementation
        let scores =
            scalar::calculate_priority_scores(&depths, &ages, &is_pv, &is_exact, current_age);

        // Verify some reasonable values
        assert!(scores[0] > scores[3]); // First entry has higher depth and bonuses

        // Test SIMD implementation
        let simd_scores =
            simd::calculate_priority_scores(&depths, &ages, &is_pv, &is_exact, current_age);

        // SIMD and scalar should produce same results
        assert_eq!(scores, simd_scores);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_simd_correctness() {
        if !std::is_x86_feature_detected!("avx2") && !std::is_x86_feature_detected!("sse2") {
            println!("Skipping SIMD tests - no SIMD support detected");
            return;
        }

        // Test many random cases to ensure SIMD matches scalar
        use rand::Rng;
        let mut rng = rand::rng();

        for _ in 0..100 {
            let keys = [
                rng.random::<u64>(),
                rng.random::<u64>(),
                rng.random::<u64>(),
                rng.random::<u64>(),
            ];

            let target = keys[rng.random_range(0..4)];

            let scalar_result = scalar::find_matching_key(&keys, target);
            let simd_result = simd::find_matching_key(&keys, target);

            assert_eq!(
                scalar_result, simd_result,
                "SIMD and scalar results differ for keys: {keys:?}, target: {target:#x}"
            );
        }
    }
}
