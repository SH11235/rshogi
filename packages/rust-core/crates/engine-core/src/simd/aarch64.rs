#[cfg(target_arch = "aarch64")]
use core::arch::aarch64::*;

#[inline(always)]
fn k_fastpath(k: f32) -> Option<i8> {
    match k.to_bits() {
        0x3f80_0000 => Some(1),  //  1.0
        0xbf80_0000 => Some(-1), // -1.0
        _ => None,
    }
}

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
    } else {
        let kk = vdupq_n_f32(k);
        while i + 4 <= n {
            let d = vld1q_f32(dst.as_ptr().add(i));
            let r = vld1q_f32(row.as_ptr().add(i));
            let v = vmlaq_f32(d, r, kk);
            vst1q_f32(dst.as_mut_ptr().add(i), v);
            i += 4;
        }
    }

    while i < n {
        *dst.get_unchecked_mut(i) += k * *row.get_unchecked(i);
        i += 1;
    }
}
