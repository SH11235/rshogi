//! LayerStacks L1 の screening 用切替。既定は既存カーネル。

use std::sync::atomic::{AtomicU8, Ordering};

/// 隠し USI option `LsL1Kernel` の選択値。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum LsL1Kernel {
    /// 既存の変換と密 L1。
    Legacy,
    /// 64 出力単位の変換と既存の密 L1。
    Xf64,
    /// 変換と L1 積和を融合。
    Fused,
}

static LS_L1_KERNEL: AtomicU8 = AtomicU8::new(LsL1Kernel::Legacy as u8);

impl LsL1Kernel {
    /// combo の文字列を解釈する。未知の値は受け付けない。
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "legacy" => Some(Self::Legacy),
            "xf64" => Some(Self::Xf64),
            "fused" => Some(Self::Fused),
            _ => None,
        }
    }
}

/// screening 用カーネルを選択する。未対応の形状・build は legacy を使う。
pub fn set_ls_l1_kernel(kernel: LsL1Kernel) {
    LS_L1_KERNEL.store(kernel as u8, Ordering::Relaxed);
}

/// 評価の入口で一度だけ読み取る。重みの公開とは独立した設定値。
#[cfg(all(
    target_arch = "x86_64",
    target_feature = "avx512f",
    target_feature = "avx512bw",
    target_feature = "avx512vnni"
))]
pub(super) fn selected() -> LsL1Kernel {
    match LS_L1_KERNEL.load(Ordering::Relaxed) {
        1 => LsL1Kernel::Xf64,
        2 => LsL1Kernel::Fused,
        _ => LsL1Kernel::Legacy,
    }
}

#[cfg(all(
    target_arch = "x86_64",
    target_feature = "avx512f",
    target_feature = "avx512bw",
    target_feature = "avx512vnni"
))]
pub(super) mod avx512 {
    use super::super::accumulator::{Aligned, AlignedBox};
    use super::super::layers::AffineTransform;
    use std::arch::x86_64::*;

    pub(crate) fn reorder<const INPUT: usize, const OUTPUT: usize>(
        l1: &AffineTransform<INPUT, OUTPUT>,
    ) -> Option<AlignedBox<i8>> {
        if INPUT != 1536 || OUTPUT != 16 {
            return None;
        }
        let mut weights = AlignedBox::new_zeroed(1536 * 16);
        // packus の lane L / dword j を、出力 4g+d の重みへ対応付ける。
        for k in 0..24 {
            for g in 0..4 {
                for j in 0..4 {
                    for lane in 0..4 {
                        let chunk = 16 * k + 2 * lane + (j & 1) + 8 * (j >> 1);
                        for d in 0..4 {
                            for byte in 0..4 {
                                weights[((k * 4 + g) * 4 + j) * 64 + lane * 16 + d * 4 + byte] =
                                    l1.weights[chunk * 64 + (4 * g + d) * 4 + byte];
                            }
                        }
                    }
                }
            }
        }
        Some(weights)
    }

    /// # Safety
    /// acc は少なくとも offset+832 個の i16 を指し、offset は 0..=704。
    #[inline]
    #[target_feature(enable = "avx512f,avx512bw,avx512vnni")]
    unsafe fn pack64(acc: *const i16, offset: usize) -> __m512i {
        // SAFETY: 呼び出し側が 1536 要素中の offset..offset+64 と
        // offset+768..offset+832 を保証する。unaligned load を使う。
        // clamp 後の積は最大16129で i16 に収まり、右シフト後は0..=126。
        unsafe {
            let zero = _mm512_setzero_si512();
            let max = _mm512_set1_epi16(127);
            let a0 = _mm512_min_epi16(
                _mm512_max_epi16(_mm512_loadu_si512(acc.add(offset).cast()), zero),
                max,
            );
            let a1 = _mm512_min_epi16(
                _mm512_max_epi16(_mm512_loadu_si512(acc.add(offset + 32).cast()), zero),
                max,
            );
            let b0 = _mm512_min_epi16(
                _mm512_max_epi16(_mm512_loadu_si512(acc.add(offset + 768).cast()), zero),
                max,
            );
            let b1 = _mm512_min_epi16(
                _mm512_max_epi16(_mm512_loadu_si512(acc.add(offset + 800).cast()), zero),
                max,
            );
            let p0 = _mm512_srli_epi16::<7>(_mm512_mullo_epi16(a0, b0));
            let p1 = _mm512_srli_epi16::<7>(_mm512_mullo_epi16(a1, b1));
            _mm512_packus_epi16(p0, p1)
        }
    }

    #[inline]
    pub(crate) fn transform<const N: usize>(us: &[i16; N], them: &[i16; N]) -> Aligned<[u8; N]> {
        assert_eq!(N, 1536);
        let mut output = std::mem::MaybeUninit::<Aligned<[u8; N]>>::uninit();
        // SAFETY: cfg が命令セットを保証する。N=1536 を確認済み。
        // 両視点で768Bずつ全出力を初期化し、入力とは重ならない。
        unsafe {
            let perm = _mm512_setr_epi64(0, 2, 4, 6, 1, 3, 5, 7);
            for (acc, base) in [(us, 0), (them, 768)] {
                for offset in (0..768).step_by(64) {
                    let packed = pack64(acc.as_ptr(), offset);
                    _mm512_store_si512(
                        output.as_mut_ptr().cast::<u8>().add(base + offset).cast(),
                        _mm512_permutexvar_epi64(perm, packed),
                    );
                }
            }
            output.assume_init()
        }
    }

    #[inline]
    pub(crate) fn fused<const N: usize, const OUT: usize>(
        us: &[i16; N],
        them: &[i16; N],
        weights: &AlignedBox<i8>,
        biases: &[i32; OUT],
    ) -> [i32; OUT] {
        assert_eq!(N, 1536);
        assert_eq!(OUT, 16);
        assert_eq!(weights.len(), 1536 * 16);
        let mut output = [0; OUT];
        // SAFETY: cfg が命令セットを保証する。入力・重み・出力の寸法は確認済み。
        // weights は reorder の [24][4][4][64] 配置、各loadは64B境界。
        // pack64 は各視点の範囲内だけを読み、store は4出力ずつ行う。
        unsafe {
            let mut sums = [[_mm512_setzero_si512(); 4]; 4];
            for (acc, base) in [(us, 0), (them, 12)] {
                for block in 0..12 {
                    let x = pack64(acc.as_ptr(), block * 64);
                    let broadcasts = [
                        _mm512_shuffle_epi32::<0x00>(x),
                        _mm512_shuffle_epi32::<0x55>(x),
                        _mm512_shuffle_epi32::<0xaa>(x),
                        _mm512_shuffle_epi32::<0xff>(x),
                    ];
                    let w = weights.as_ptr().add((base + block) * 1024).cast::<__m512i>();
                    for (g, group) in sums.iter_mut().enumerate() {
                        for (j, sum) in group.iter_mut().enumerate() {
                            *sum = _mm512_dpbusd_epi32(
                                *sum,
                                broadcasts[j],
                                _mm512_load_si512(w.add(g * 4 + j)),
                            );
                        }
                    }
                }
            }
            for (g, group) in sums.iter().enumerate() {
                let sum = _mm512_add_epi32(
                    _mm512_add_epi32(group[0], group[1]),
                    _mm512_add_epi32(group[2], group[3]),
                );
                let halves = _mm256_add_epi32(
                    _mm512_castsi512_si256(sum),
                    _mm512_extracti64x4_epi64::<1>(sum),
                );
                let lanes = _mm_add_epi32(
                    _mm256_castsi256_si128(halves),
                    _mm256_extracti128_si256::<1>(halves),
                );
                let bias = _mm_loadu_si128(biases.as_ptr().add(g * 4).cast());
                _mm_storeu_si128(output.as_mut_ptr().add(g * 4).cast(), _mm_add_epi32(lanes, bias));
            }
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combo_values() {
        for (text, value) in [
            ("legacy", LsL1Kernel::Legacy),
            ("xf64", LsL1Kernel::Xf64),
            ("fused", LsL1Kernel::Fused),
        ] {
            assert_eq!(LsL1Kernel::parse(text), Some(value));
        }
        assert_eq!(LsL1Kernel::parse("unknown"), None);
    }
}
