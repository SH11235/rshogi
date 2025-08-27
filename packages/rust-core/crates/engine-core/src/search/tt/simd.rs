//! SIMD-aware helpers for the Transposition Table
//!
//! Provides feature detection (`simd_kind`, `simd_enabled`) and
//! scalar baselines for probe/priority utilities. Platform-specific
//! intrinsics can be layered later behind cfg without changing
//! public function signatures.

use std::sync::OnceLock;

/// Available SIMD kinds detected at runtime (where applicable)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SimdKind {
    None,
    #[cfg(target_arch = "x86_64")]
    Sse2,
    #[cfg(target_arch = "x86_64")]
    Avx2,
    #[cfg(target_arch = "aarch64")]
    Neon,
}

/// Detect SIMD support once at runtime
pub fn simd_kind() -> SimdKind {
    static KIND: OnceLock<SimdKind> = OnceLock::new();
    *KIND.get_or_init(|| {
        #[cfg(target_arch = "x86_64")]
        {
            if std::is_x86_feature_detected!("avx2") {
                SimdKind::Avx2
            } else if std::is_x86_feature_detected!("sse2") {
                SimdKind::Sse2
            } else {
                SimdKind::None
            }
        }

        #[cfg(target_arch = "aarch64")]
        {
            // Most aarch64 targets have NEON; treat as available.
            SimdKind::Neon
        }

        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            SimdKind::None
        }
    })
}

/// Whether SIMD is enabled (not None)
#[inline]
pub fn simd_enabled() -> bool {
    simd_kind() != SimdKind::None
}

/// Calculate priority scores for 4 entries (scalar baseline)
pub fn calculate_priority_scores(
    depths: &[u8; 4],
    ages: &[u8; 4],
    is_pv: &[bool; 4],
    is_exact: &[bool; 4],
    current_age: u8,
) -> [i32; 4] {
    let mut out = [0i32; 4];
    for i in 0..4 {
        let age_distance = current_age.wrapping_sub(ages[i]) as i32;
        let depth = depths[i] as i32;
        let pv_bonus = if is_pv[i] { 4 } else { 0 };
        let exact_bonus = if is_exact[i] { 2 } else { 0 };
        // Lower score = more replaceable
        out[i] = depth + pv_bonus + exact_bonus - age_distance;
    }
    out
}

/// Calculate priority scores for 8 entries (scalar baseline)
pub fn calculate_priority_scores_8(
    depths: &[u8; 8],
    ages: &[u8; 8],
    is_pv: &[bool; 8],
    is_exact: &[bool; 8],
    current_age: u8,
) -> [i32; 8] {
    let mut out = [0i32; 8];
    for i in 0..8 {
        let age_distance = current_age.wrapping_sub(ages[i]) as i32;
        let depth = depths[i] as i32;
        let pv_bonus = if is_pv[i] { 4 } else { 0 };
        let exact_bonus = if is_exact[i] { 2 } else { 0 };
        out[i] = depth + pv_bonus + exact_bonus - age_distance;
    }
    out
}

/// Find matching key among 4 entries (scalar baseline)
pub fn find_matching_key(keys: &[u64; 4], target: u64) -> Option<usize> {
    (0..4).find(|&i| keys[i] == target)
}

/// Find matching key among 8 entries (scalar baseline)
pub fn find_matching_key_8(keys: &[u64; 8], target: u64) -> Option<usize> {
    (0..8).find(|&i| keys[i] == target)
}

/// Find matching key among 16 entries (scalar baseline)
pub fn find_matching_key_16(keys: &[u64; 16], target: u64) -> Option<usize> {
    (0..16).find(|&i| keys[i] == target)
}
