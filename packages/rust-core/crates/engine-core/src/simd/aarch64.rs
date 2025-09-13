use super::{k_fastpath, k_int_fastpath};
#[cfg(target_arch = "aarch64")]
use core::arch::aarch64::*;

/// AArch64 NEON 経路（f32×4）
#[cfg(target_arch = "aarch64")]
pub(super) unsafe fn add_row_scaled_f32_neon(dst: &mut [f32], row: &[f32], k: f32) {
    debug_assert_eq!(dst.len(), row.len());
    let n = dst.len();
    let mut i = 0usize;

    if let Some(s) = k_fastpath(k) {
        if s > 0 {
            while i + 4 <= n {
                let d = vld1q_f32(dst.as_ptr().add(i));
                let r = vld1q_f32(row.as_ptr().add(i));
                let v = vaddq_f32(d, r);
                vst1q_f32(dst.as_mut_ptr().add(i), v);
                i += 4;
            }
        } else {
            while i + 4 <= n {
                let d = vld1q_f32(dst.as_ptr().add(i));
                let r = vld1q_f32(row.as_ptr().add(i));
                let v = vsubq_f32(d, r);
                vst1q_f32(dst.as_mut_ptr().add(i), v);
                i += 4;
            }
        }
    } else if let Some(t) = k_int_fastpath(k) {
        if t > 0 {
            while i + 4 <= n {
                let d = vld1q_f32(dst.as_ptr().add(i));
                let r = vld1q_f32(row.as_ptr().add(i));
                let rr = vaddq_f32(r, r);
                let v = vaddq_f32(d, rr);
                vst1q_f32(dst.as_mut_ptr().add(i), v);
                i += 4;
            }
        } else {
            while i + 4 <= n {
                let d = vld1q_f32(dst.as_ptr().add(i));
                let r = vld1q_f32(row.as_ptr().add(i));
                let rr = vaddq_f32(r, r);
                let v = vsubq_f32(d, rr);
                vst1q_f32(dst.as_mut_ptr().add(i), v);
                i += 4;
            }
        }
    } else {
        let kk = vdupq_n_f32(k);
        while i + 4 <= n {
            let d = vld1q_f32(dst.as_ptr().add(i));
            let r = vld1q_f32(row.as_ptr().add(i));
            #[cfg(feature = "nnue_fast_fma")]
            let v = vfmaq_f32(d, r, kk);
            #[cfg(not(feature = "nnue_fast_fma"))]
            let v = vaddq_f32(d, vmulq_f32(r, kk));
            vst1q_f32(dst.as_mut_ptr().add(i), v);
            i += 4;
        }
    }

    while i < n {
        dst[i] += k * row[i];
        i += 1;
    }
}
