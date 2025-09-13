//! Common SIMD infrastructure and utilities
//!
//! This module provides shared SIMD functionality used by various components
//! like NNUE and Transposition Table.

/// CPU feature detection and dispatch
pub mod dispatch {
    /// Check if AVX2 is available at runtime
    #[inline]
    pub fn has_avx2() -> bool {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            std::is_x86_feature_detected!("avx2")
        }
        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
        {
            false
        }
    }

    /// Check if AVX is available at runtime
    #[inline]
    pub fn has_avx() -> bool {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            std::is_x86_feature_detected!("avx")
        }
        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
        {
            false
        }
    }

    /// Check if AVX-512F is available at runtime
    #[inline]
    pub fn has_avx512f() -> bool {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            std::is_x86_feature_detected!("avx512f")
        }
        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
        {
            false
        }
    }

    /// Check if SSE2 is available at runtime
    #[inline]
    pub fn has_sse2() -> bool {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            std::is_x86_feature_detected!("sse2")
        }
        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
        {
            false
        }
    }

    /// Check if SSE4.1 is available at runtime
    #[inline]
    pub fn has_sse41() -> bool {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            std::is_x86_feature_detected!("sse4.1")
        }
        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
        {
            false
        }
    }

    /// Select the best available SIMD level
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum SimdLevel {
        Scalar,
        Sse2,
        Avx,
        Avx512f,
    }

    impl SimdLevel {
        /// Detect the best available SIMD level for current CPU
        pub fn detect() -> Self {
            if has_avx512f() {
                SimdLevel::Avx512f
            } else if has_avx() {
                // ランタイムでは AVX2/AVX をまとめて AVX として扱う
                SimdLevel::Avx
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

// -------------------------------------------------------------------------------------
// fp32 row add: dst[i] += k * row[i]
// 実行時 CPU 検出 + OnceLock キャッシュで最適カーネルにディスパッチ
// -------------------------------------------------------------------------------------

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
mod wasm32;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
mod x86;

#[inline(always)]
pub(super) fn k_fastpath(k: f32) -> Option<i8> {
    match k.to_bits() {
        0x3f80_0000 => Some(1),  //  1.0
        0xbf80_0000 => Some(-1), // -1.0
        _ => None,
    }
}

/// 小整数 k（±2）の高速経路
#[inline(always)]
pub(super) fn k_int_fastpath(k: f32) -> Option<i8> {
    match k.to_bits() {
        0x4000_0000 => Some(2),  //  2.0
        0xc000_0000 => Some(-2), // -2.0
        _ => None,
    }
}

#[inline(always)]
fn add_row_scaled_f32_scalar(dst: &mut [f32], row: &[f32], k: f32) {
    debug_assert_eq!(dst.len(), row.len());
    if let Some(s) = k_fastpath(k) {
        if s > 0 {
            for (d, r) in dst.iter_mut().zip(row.iter()) {
                *d += *r;
            }
        } else {
            for (d, r) in dst.iter_mut().zip(row.iter()) {
                *d -= *r;
            }
        }
    } else if let Some(t) = k_int_fastpath(k) {
        if t > 0 {
            for (d, r) in dst.iter_mut().zip(row.iter()) {
                // 2.0: r+r を加算（mul/fma を避ける）
                *d += *r + *r;
            }
        } else {
            for (d, r) in dst.iter_mut().zip(row.iter()) {
                *d -= *r + *r;
            }
        }
    } else {
        for (d, r) in dst.iter_mut().zip(row.iter()) {
            *d += k * *r;
        }
    }
}

/// fp32 行加算の公開 API: `dst[i] += k * row[i]`
///
/// 契約:
/// - `dst.len() == row.len()` であること。
/// - `dst` と `row` は同一領域を指してはならない（エイリアス禁止）。
#[inline]
pub fn add_row_scaled_f32(dst: &mut [f32], row: &[f32], k: f32) {
    debug_assert_eq!(dst.len(), row.len());

    // wasm32: simd128 が有効なら常時ON、無効ならスカラ
    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    {
        unsafe {
            return wasm32::add_row_scaled_f32_wasm128(dst, row, k);
        }
    }
    #[cfg(all(target_arch = "wasm32", not(target_feature = "simd128")))]
    {
        return add_row_scaled_f32_scalar(dst, row, k);
    }

    #[cfg(target_arch = "aarch64")]
    {
        // AArch64 は NEON 常時ON。実行時検出不要。
        unsafe {
            return aarch64::add_row_scaled_f32_neon(dst, row, k);
        }
    }

    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        use std::sync::OnceLock;
        type Kernel = fn(&mut [f32], &[f32], f32);
        static ADD_ROW_SCALED_F32: OnceLock<Kernel> = OnceLock::new();

        let f = ADD_ROW_SCALED_F32.get_or_init(|| {
            if std::arch::is_x86_feature_detected!("avx512f") {
                return |dst, row, k| unsafe { x86::add_row_scaled_f32_avx512f(dst, row, k) };
            }
            // 初回のみ CPU 機能を検出し最適カーネルを束縛
            #[cfg(feature = "nnue_fast_fma")]
            {
                if std::arch::is_x86_feature_detected!("avx")
                    && std::arch::is_x86_feature_detected!("fma")
                {
                    return |dst, row, k| unsafe { x86::add_row_scaled_f32_avx_fma(dst, row, k) };
                }
            }
            if std::arch::is_x86_feature_detected!("avx") {
                return |dst, row, k| unsafe { x86::add_row_scaled_f32_avx(dst, row, k) };
            }
            if std::arch::is_x86_feature_detected!("sse2") {
                return |dst, row, k| unsafe { x86::add_row_scaled_f32_sse2(dst, row, k) };
            }
            add_row_scaled_f32_scalar as Kernel
        });
        f(dst, row, k)
    }

    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
    {
        // 非 x86 系はスカラ
        add_row_scaled_f32_scalar(dst, row, k);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simd_detection() {
        let level = dispatch::SimdLevel::detect();
        log::debug!("Detected SIMD level: {level:?}");

        // Check that we detected a valid SIMD level
        // The level should be one of: Scalar, Sse2, Avx, or Avx512f
        match level {
            dispatch::SimdLevel::Scalar
            | dispatch::SimdLevel::Sse2
            | dispatch::SimdLevel::Avx
            | dispatch::SimdLevel::Avx512f => {
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

    #[inline]
    fn scalar_ref(dst: &mut [f32], row: &[f32], k: f32) {
        for (d, r) in dst.iter_mut().zip(row.iter()) {
            *d += k * *r;
        }
    }

    fn same_bits(a: f32, b: f32) -> bool {
        a.to_bits() == b.to_bits()
    }

    #[test]
    fn test_add_row_scaled_k_pos1_bits() {
        for &len in &[
            0usize, 1, 2, 3, 4, 5, 7, 8, 9, 15, 16, 17, 31, 32, 33, 255, 256, 257, 511, 512, 513,
        ] {
            let mut dst_a = vec![0.0f32; len];
            let mut dst_b = vec![0.0f32; len];
            let mut row = vec![0.0f32; len];
            for i in 0..len {
                row[i] = ((i as f32 + 0.5) * 0.01).sin();
            }
            scalar_ref(&mut dst_a, &row, 1.0);
            add_row_scaled_f32(&mut dst_b, &row, 1.0);
            for i in 0..len {
                assert!(same_bits(dst_a[i], dst_b[i]), "mismatch at {i}");
            }
        }
    }

    #[test]
    fn test_add_row_scaled_k_neg1_bits() {
        for &len in &[
            0usize, 1, 2, 3, 4, 5, 7, 8, 9, 15, 16, 17, 31, 32, 33, 255, 256, 257, 511, 512, 513,
        ] {
            let mut dst_a = vec![0.0f32; len];
            let mut dst_b = vec![0.0f32; len];
            let mut row = vec![0.0f32; len];
            for i in 0..len {
                row[i] = ((i as f32 + 0.5) * 0.01).cos();
            }
            scalar_ref(&mut dst_a, &row, -1.0);
            add_row_scaled_f32(&mut dst_b, &row, -1.0);
            for i in 0..len {
                assert!(same_bits(dst_a[i], dst_b[i]), "mismatch at {i}");
            }
        }
    }

    #[test]
    fn test_add_row_scaled_k_pos2_bits() {
        for &len in &[
            0usize, 1, 2, 3, 4, 5, 7, 8, 9, 15, 16, 17, 31, 32, 33, 255, 256, 257, 511, 512, 513,
        ] {
            let mut dst_ref = vec![0.0f32; len];
            let mut dst_k2 = vec![0.0f32; len];
            let mut row = vec![0.0f32; len];
            for i in 0..len {
                row[i] = ((i as f32 + 0.5) * 0.01).sin();
            }
            // 参照: k=1.0 を2回
            add_row_scaled_f32(&mut dst_ref, &row, 1.0);
            add_row_scaled_f32(&mut dst_ref, &row, 1.0);
            // 最適化経路: k=2.0（±2 専用分岐）
            add_row_scaled_f32(&mut dst_k2, &row, 2.0);
            for i in 0..len {
                assert!(same_bits(dst_ref[i], dst_k2[i]), "mismatch at {i}");
            }
        }
    }

    #[test]
    fn test_add_row_scaled_k_neg2_bits() {
        for &len in &[
            0usize, 1, 2, 3, 4, 5, 7, 8, 9, 15, 16, 17, 31, 32, 33, 255, 256, 257, 511, 512, 513,
        ] {
            let mut dst_ref = vec![0.0f32; len];
            let mut dst_k2 = vec![0.0f32; len];
            let mut row = vec![0.0f32; len];
            for i in 0..len {
                row[i] = ((i as f32 + 0.25) * 0.02).cos();
            }
            // 参照: k=-1.0 を2回
            add_row_scaled_f32(&mut dst_ref, &row, -1.0);
            add_row_scaled_f32(&mut dst_ref, &row, -1.0);
            // 最適化経路: k=-2.0（±2 専用分岐）
            add_row_scaled_f32(&mut dst_k2, &row, -2.0);
            for i in 0..len {
                assert!(same_bits(dst_ref[i], dst_k2[i]), "mismatch at {i}");
            }
        }
    }

    #[test]
    fn test_add_row_scaled_k_other_close() {
        let ks = [0.75f32, 1.25, -0.5];
        for &len in &[1usize, 3, 8, 9, 31, 32, 33, 255, 256, 257] {
            for &k in &ks {
                let mut dst_a = vec![0.0f32; len];
                let mut dst_b = vec![0.0f32; len];
                let mut row = vec![0.0f32; len];
                for i in 0..len {
                    row[i] = ((i as f32 + 0.5) * 0.01).tan().clamp(-1e3, 1e3);
                }
                scalar_ref(&mut dst_a, &row, k);
                add_row_scaled_f32(&mut dst_b, &row, k);

                // FMA 経路では丸めが異なる可能性があるため、許容誤差を用意
                let use_approx = {
                    #[cfg(all(
                        any(target_arch = "x86", target_arch = "x86_64"),
                        feature = "nnue_fast_fma"
                    ))]
                    {
                        std::arch::is_x86_feature_detected!("avx512f")
                            || (std::arch::is_x86_feature_detected!("avx")
                                && std::arch::is_x86_feature_detected!("fma"))
                    }
                    #[cfg(all(target_arch = "aarch64", feature = "nnue_fast_fma"))]
                    {
                        true
                    }
                    #[cfg(not(any(
                        all(
                            any(target_arch = "x86", target_arch = "x86_64"),
                            feature = "nnue_fast_fma"
                        ),
                        all(target_arch = "aarch64", feature = "nnue_fast_fma"),
                    )))]
                    {
                        false
                    }
                };
                for i in 0..len {
                    if use_approx {
                        let a = dst_a[i];
                        let b = dst_b[i];
                        let diff = (a - b).abs();
                        let tol = 1e-6f32 * (1.0 + a.abs().max(b.abs()));
                        assert!(
                            diff <= tol,
                            "approx mismatch at {i}: a={a} b={b} diff={diff} tol={tol}"
                        );
                    } else {
                        assert!(same_bits(dst_a[i], dst_b[i]), "mismatch at {i}");
                    }
                }
            }
        }
    }

    // Property-based tests: k=±1.0 でビット一致
    use proptest::prelude::*;
    proptest! {
        #[test]
        fn prop_add_row_scaled_k_pos1_bits(len in 0usize..514) {
            let mut dst_a = vec![0.0f32; len];
            let mut dst_b = vec![0.0f32; len];
            // 乱数でなく決定論的だが十分
            let mut row = vec![0.0f32; len];
            for i in 0..len { row[i] = ((i as f32 + 0.5) * 0.01).sin(); }
            scalar_ref(&mut dst_a, &row, 1.0);
            add_row_scaled_f32(&mut dst_b, &row, 1.0);
            for i in 0..len { prop_assert!(dst_a[i].to_bits() == dst_b[i].to_bits()); }
        }

        #[test]
        fn prop_add_row_scaled_k_neg1_bits(len in 0usize..514) {
            let mut dst_a = vec![0.0f32; len];
            let mut dst_b = vec![0.0f32; len];
            let mut row = vec![0.0f32; len];
            for i in 0..len { row[i] = ((i as f32 + 0.25) * 0.02).cos(); }
            scalar_ref(&mut dst_a, &row, -1.0);
            add_row_scaled_f32(&mut dst_b, &row, -1.0);
            for i in 0..len { prop_assert!(dst_a[i].to_bits() == dst_b[i].to_bits()); }
        }
    }
}
