use super::{k_fastpath, k_int_fastpath};
#[cfg(target_arch = "x86")]
use core::arch::x86::*;
#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::*;

/// AVX + FMA 経路（f32×8）
#[cfg(feature = "nnue_fast_fma")]
#[target_feature(enable = "avx,fma")]
pub(super) unsafe fn add_row_scaled_f32_avx_fma(dst: &mut [f32], row: &[f32], k: f32) {
    debug_assert_eq!(dst.len(), row.len());
    let n = dst.len();
    let mut i = 0usize;

    if let Some(s) = k_fastpath(k) {
        if s > 0 {
            while i + 8 <= n {
                let d = _mm256_loadu_ps(dst.as_ptr().add(i));
                let r = _mm256_loadu_ps(row.as_ptr().add(i));
                let v = _mm256_add_ps(d, r);
                _mm256_storeu_ps(dst.as_mut_ptr().add(i), v);
                i += 8;
            }
        } else {
            while i + 8 <= n {
                let d = _mm256_loadu_ps(dst.as_ptr().add(i));
                let r = _mm256_loadu_ps(row.as_ptr().add(i));
                let v = _mm256_sub_ps(d, r);
                _mm256_storeu_ps(dst.as_mut_ptr().add(i), v);
                i += 8;
            }
        }
    } else if let Some(t) = k_int_fastpath(k) {
        if t > 0 {
            while i + 8 <= n {
                let d = _mm256_loadu_ps(dst.as_ptr().add(i));
                let r = _mm256_loadu_ps(row.as_ptr().add(i));
                let rr = _mm256_add_ps(r, r);
                let v = _mm256_add_ps(d, rr);
                _mm256_storeu_ps(dst.as_mut_ptr().add(i), v);
                i += 8;
            }
        } else {
            while i + 8 <= n {
                let d = _mm256_loadu_ps(dst.as_ptr().add(i));
                let r = _mm256_loadu_ps(row.as_ptr().add(i));
                let rr = _mm256_add_ps(r, r);
                let v = _mm256_sub_ps(d, rr);
                _mm256_storeu_ps(dst.as_mut_ptr().add(i), v);
                i += 8;
            }
        }
    } else {
        let kk = _mm256_set1_ps(k);
        while i + 8 <= n {
            let d = _mm256_loadu_ps(dst.as_ptr().add(i));
            let r = _mm256_loadu_ps(row.as_ptr().add(i));
            let v = _mm256_fmadd_ps(r, kk, d);
            _mm256_storeu_ps(dst.as_mut_ptr().add(i), v);
            i += 8;
        }
    }

    // tail: スカラ
    while i < n {
        dst[i] += k * row[i];
        i += 1;
    }
}

/// AVX 経路（f32×8）
#[target_feature(enable = "avx")]
pub(super) unsafe fn add_row_scaled_f32_avx(dst: &mut [f32], row: &[f32], k: f32) {
    debug_assert_eq!(dst.len(), row.len());
    let n = dst.len();
    let mut i = 0usize;

    if let Some(s) = k_fastpath(k) {
        if s > 0 {
            while i + 8 <= n {
                let d = _mm256_loadu_ps(dst.as_ptr().add(i));
                let r = _mm256_loadu_ps(row.as_ptr().add(i));
                let v = _mm256_add_ps(d, r);
                _mm256_storeu_ps(dst.as_mut_ptr().add(i), v);
                i += 8;
            }
        } else {
            while i + 8 <= n {
                let d = _mm256_loadu_ps(dst.as_ptr().add(i));
                let r = _mm256_loadu_ps(row.as_ptr().add(i));
                let v = _mm256_sub_ps(d, r);
                _mm256_storeu_ps(dst.as_mut_ptr().add(i), v);
                i += 8;
            }
        }
    } else if let Some(t) = k_int_fastpath(k) {
        if t > 0 {
            while i + 8 <= n {
                let d = _mm256_loadu_ps(dst.as_ptr().add(i));
                let r = _mm256_loadu_ps(row.as_ptr().add(i));
                let rr = _mm256_add_ps(r, r);
                let v = _mm256_add_ps(d, rr);
                _mm256_storeu_ps(dst.as_mut_ptr().add(i), v);
                i += 8;
            }
        } else {
            while i + 8 <= n {
                let d = _mm256_loadu_ps(dst.as_ptr().add(i));
                let r = _mm256_loadu_ps(row.as_ptr().add(i));
                let rr = _mm256_add_ps(r, r);
                let v = _mm256_sub_ps(d, rr);
                _mm256_storeu_ps(dst.as_mut_ptr().add(i), v);
                i += 8;
            }
        }
    } else {
        let kk = _mm256_set1_ps(k);
        while i + 8 <= n {
            let d = _mm256_loadu_ps(dst.as_ptr().add(i));
            let r = _mm256_loadu_ps(row.as_ptr().add(i));
            let v = _mm256_add_ps(d, _mm256_mul_ps(r, kk));
            _mm256_storeu_ps(dst.as_mut_ptr().add(i), v);
            i += 8;
        }
    }

    // tail: スカラ
    while i < n {
        dst[i] += k * row[i];
        i += 1;
    }
}

/// SSE2 経路（f32×4）
#[target_feature(enable = "sse2")]
pub(super) unsafe fn add_row_scaled_f32_sse2(dst: &mut [f32], row: &[f32], k: f32) {
    debug_assert_eq!(dst.len(), row.len());
    let n = dst.len();
    let mut i = 0usize;

    if let Some(s) = k_fastpath(k) {
        if s > 0 {
            while i + 4 <= n {
                let d = _mm_loadu_ps(dst.as_ptr().add(i));
                let r = _mm_loadu_ps(row.as_ptr().add(i));
                let v = _mm_add_ps(d, r);
                _mm_storeu_ps(dst.as_mut_ptr().add(i), v);
                i += 4;
            }
        } else {
            while i + 4 <= n {
                let d = _mm_loadu_ps(dst.as_ptr().add(i));
                let r = _mm_loadu_ps(row.as_ptr().add(i));
                let v = _mm_sub_ps(d, r);
                _mm_storeu_ps(dst.as_mut_ptr().add(i), v);
                i += 4;
            }
        }
    } else if let Some(t) = k_int_fastpath(k) {
        if t > 0 {
            while i + 4 <= n {
                let d = _mm_loadu_ps(dst.as_ptr().add(i));
                let r = _mm_loadu_ps(row.as_ptr().add(i));
                let rr = _mm_add_ps(r, r);
                let v = _mm_add_ps(d, rr);
                _mm_storeu_ps(dst.as_mut_ptr().add(i), v);
                i += 4;
            }
        } else {
            while i + 4 <= n {
                let d = _mm_loadu_ps(dst.as_ptr().add(i));
                let r = _mm_loadu_ps(row.as_ptr().add(i));
                let rr = _mm_add_ps(r, r);
                let v = _mm_sub_ps(d, rr);
                _mm_storeu_ps(dst.as_mut_ptr().add(i), v);
                i += 4;
            }
        }
    } else {
        let kk = _mm_set1_ps(k);
        while i + 4 <= n {
            let d = _mm_loadu_ps(dst.as_ptr().add(i));
            let r = _mm_loadu_ps(row.as_ptr().add(i));
            let v = _mm_add_ps(d, _mm_mul_ps(r, kk));
            _mm_storeu_ps(dst.as_mut_ptr().add(i), v);
            i += 4;
        }
    }

    // tail: スカラ
    while i < n {
        dst[i] += k * row[i];
        i += 1;
    }
}
/// AVX512F 経路（f32×16）
#[target_feature(enable = "avx512f")]
pub(super) unsafe fn add_row_scaled_f32_avx512f(dst: &mut [f32], row: &[f32], k: f32) {
    debug_assert_eq!(dst.len(), row.len());
    let n = dst.len();
    let mut i = 0usize;

    if let Some(s) = k_fastpath(k) {
        if s > 0 {
            while i + 16 <= n {
                let d = _mm512_loadu_ps(dst.as_ptr().add(i));
                let r = _mm512_loadu_ps(row.as_ptr().add(i));
                let v = _mm512_add_ps(d, r);
                _mm512_storeu_ps(dst.as_mut_ptr().add(i), v);
                i += 16;
            }
            // tail: AVX-512 mask
            let rem = n - i;
            if rem > 0 {
                let mask: u16 = ((1u32 << rem) - 1) as u16;
                let d = _mm512_maskz_loadu_ps(mask, dst.as_ptr().add(i));
                let r = _mm512_maskz_loadu_ps(mask, row.as_ptr().add(i));
                let v = _mm512_add_ps(d, r);
                _mm512_mask_storeu_ps(dst.as_mut_ptr().add(i), mask, v);
            }
        } else {
            while i + 16 <= n {
                let d = _mm512_loadu_ps(dst.as_ptr().add(i));
                let r = _mm512_loadu_ps(row.as_ptr().add(i));
                let v = _mm512_sub_ps(d, r);
                _mm512_storeu_ps(dst.as_mut_ptr().add(i), v);
                i += 16;
            }
            // tail: AVX-512 mask
            let rem = n - i;
            if rem > 0 {
                let mask: u16 = ((1u32 << rem) - 1) as u16;
                let d = _mm512_maskz_loadu_ps(mask, dst.as_ptr().add(i));
                let r = _mm512_maskz_loadu_ps(mask, row.as_ptr().add(i));
                let v = _mm512_sub_ps(d, r);
                _mm512_mask_storeu_ps(dst.as_mut_ptr().add(i), mask, v);
            }
        }
    } else if let Some(t) = k_int_fastpath(k) {
        if t > 0 {
            while i + 16 <= n {
                let d = _mm512_loadu_ps(dst.as_ptr().add(i));
                let r = _mm512_loadu_ps(row.as_ptr().add(i));
                let rr = _mm512_add_ps(r, r);
                let v = _mm512_add_ps(d, rr);
                _mm512_storeu_ps(dst.as_mut_ptr().add(i), v);
                i += 16;
            }
            let rem = n - i;
            if rem > 0 {
                let mask: u16 = ((1u32 << rem) - 1) as u16;
                let d = _mm512_maskz_loadu_ps(mask, dst.as_ptr().add(i));
                let r = _mm512_maskz_loadu_ps(mask, row.as_ptr().add(i));
                let rr = _mm512_add_ps(r, r);
                let v = _mm512_add_ps(d, rr);
                _mm512_mask_storeu_ps(dst.as_mut_ptr().add(i), mask, v);
            }
        } else {
            while i + 16 <= n {
                let d = _mm512_loadu_ps(dst.as_ptr().add(i));
                let r = _mm512_loadu_ps(row.as_ptr().add(i));
                let rr = _mm512_add_ps(r, r);
                let v = _mm512_sub_ps(d, rr);
                _mm512_storeu_ps(dst.as_mut_ptr().add(i), v);
                i += 16;
            }
            let rem = n - i;
            if rem > 0 {
                let mask: u16 = ((1u32 << rem) - 1) as u16;
                let d = _mm512_maskz_loadu_ps(mask, dst.as_ptr().add(i));
                let r = _mm512_maskz_loadu_ps(mask, row.as_ptr().add(i));
                let rr = _mm512_add_ps(r, r);
                let v = _mm512_sub_ps(d, rr);
                _mm512_mask_storeu_ps(dst.as_mut_ptr().add(i), mask, v);
            }
        }
    } else {
        let kk = _mm512_set1_ps(k);
        while i + 16 <= n {
            let d = _mm512_loadu_ps(dst.as_ptr().add(i));
            let r = _mm512_loadu_ps(row.as_ptr().add(i));
            #[cfg(feature = "nnue_fast_fma")]
            let v = _mm512_fmadd_ps(r, kk, d);
            #[cfg(not(feature = "nnue_fast_fma"))]
            let v = _mm512_add_ps(d, _mm512_mul_ps(r, kk));
            _mm512_storeu_ps(dst.as_mut_ptr().add(i), v);
            i += 16;
        }
        // tail: AVX-512 mask
        let rem = n - i;
        if rem > 0 {
            let mask: u16 = ((1u32 << rem) - 1) as u16;
            let d = _mm512_maskz_loadu_ps(mask, dst.as_ptr().add(i));
            let r = _mm512_maskz_loadu_ps(mask, row.as_ptr().add(i));
            #[cfg(feature = "nnue_fast_fma")]
            let v = _mm512_fmadd_ps(r, kk, d);
            #[cfg(not(feature = "nnue_fast_fma"))]
            let v = _mm512_add_ps(d, _mm512_mul_ps(r, kk));
            _mm512_mask_storeu_ps(dst.as_mut_ptr().add(i), mask, v);
        }
    }
}
