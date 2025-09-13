#![allow(dead_code)]
#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
use core::arch::wasm32::*;

#[inline(always)]
fn k_fastpath(k: f32) -> Option<i8> {
    match k.to_bits() {
        0x3f80_0000 => Some(1),  //  1.0
        0xbf80_0000 => Some(-1), // -1.0
        _ => None,
    }
}

/// wasm32 simd128 経路（f32×4）
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

    while i < n {
        *dst.get_unchecked_mut(i) += k * *row.get_unchecked(i);
        i += 1;
    }
}
