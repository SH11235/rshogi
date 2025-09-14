#![allow(dead_code)]
use super::{k_fastpath, k_int_fastpath};
#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
use core::arch::wasm32::*;

/// wasm32 simd128 経路（f32×4）
#[inline(always)]
#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
pub(super) unsafe fn add_row_scaled_f32_wasm128(dst: &mut [f32], row: &[f32], k: f32) {
    debug_assert_eq!(dst.len(), row.len());
    let n = dst.len();
    let mut i = 0usize;

    if let Some(s) = k_fastpath(k) {
        if s > 0 {
            while i + 4 <= n {
                let d = v128_load(dst.as_ptr().add(i) as *const v128);
                let r = v128_load(row.as_ptr().add(i) as *const v128);
                let v = f32x4_add(d, r);
                v128_store(dst.as_mut_ptr().add(i) as *mut v128, v);
                i += 4;
            }
        } else {
            while i + 4 <= n {
                let d = v128_load(dst.as_ptr().add(i) as *const v128);
                let r = v128_load(row.as_ptr().add(i) as *const v128);
                let v = f32x4_sub(d, r);
                v128_store(dst.as_mut_ptr().add(i) as *mut v128, v);
                i += 4;
            }
        }
    } else if let Some(t) = k_int_fastpath(k) {
        if t > 0 {
            while i + 4 <= n {
                let d = v128_load(dst.as_ptr().add(i) as *const v128);
                let r = v128_load(row.as_ptr().add(i) as *const v128);
                let rr = f32x4_add(r, r);
                let v = f32x4_add(d, rr);
                v128_store(dst.as_mut_ptr().add(i) as *mut v128, v);
                i += 4;
            }
        } else {
            while i + 4 <= n {
                let d = v128_load(dst.as_ptr().add(i) as *const v128);
                let r = v128_load(row.as_ptr().add(i) as *const v128);
                let rr = f32x4_add(r, r);
                let v = f32x4_sub(d, rr);
                v128_store(dst.as_mut_ptr().add(i) as *mut v128, v);
                i += 4;
            }
        }
    } else {
        let kk = f32x4_splat(k);
        while i + 4 <= n {
            let d = v128_load(dst.as_ptr().add(i) as *const v128);
            let r = v128_load(row.as_ptr().add(i) as *const v128);
            let v = f32x4_add(d, f32x4_mul(r, kk));
            v128_store(dst.as_mut_ptr().add(i) as *mut v128, v);
            i += 4;
        }
    }

    if i < n {
        super::add_row_scaled_f32_scalar(&mut dst[i..], &row[i..], k);
    }
}
