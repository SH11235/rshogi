//! 1536×16 の LayerStacks L1 と FT 出力変換を融合する VNNI カーネル。

#[cfg(all(
    target_arch = "x86_64",
    target_feature = "avx512f",
    target_feature = "avx512bw",
    target_feature = "avx512vnni"
))]
pub(super) mod avx512 {
    use super::super::accumulator::AlignedBox;
    use super::super::layers::AffineTransform;
    use std::arch::x86_64::*;

    /// reorder だけが構築する、64B 境界の [24][4][4][64] 融合用重み。
    /// 内部バッファの長さ・配置は構築後に変更しない。
    pub(crate) struct FusedWeights(AlignedBox<i8>);

    pub(crate) fn reorder<const INPUT: usize, const OUTPUT: usize>(
        l1: &AffineTransform<INPUT, OUTPUT>,
    ) -> Option<FusedWeights> {
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
        Some(FusedWeights(weights))
    }

    /// 正常終了と unwind のどちらでも、編集した L1 から融合用重みを再構築する。
    pub(crate) fn edit_l1<const INPUT: usize, const OUTPUT: usize>(
        l1: &mut AffineTransform<INPUT, OUTPUT>,
        weights: &mut Option<FusedWeights>,
        edit: impl FnOnce(&mut AffineTransform<INPUT, OUTPUT>),
    ) {
        struct Rebuild<'a, const INPUT: usize, const OUTPUT: usize> {
            l1: &'a mut AffineTransform<INPUT, OUTPUT>,
            weights: &'a mut Option<FusedWeights>,
        }
        impl<const INPUT: usize, const OUTPUT: usize> Drop for Rebuild<'_, INPUT, OUTPUT> {
            fn drop(&mut self) {
                *self.weights = reorder(self.l1);
            }
        }
        let guard = Rebuild { l1, weights };
        edit(guard.l1);
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

    /// 重みロードを積和のメモリオペランドへ畳み込ませず、register 形式を保つ。
    #[inline]
    fn dpbusd_register(mut sum: __m512i, input: __m512i, weight: __m512i) -> __m512i {
        // SAFETY: モジュールの cfg が AVX512VNNI を保証する。すべて512bit registerで、
        // unsigned input × signed weight の非飽和加算だけを行う。メモリ・flagsには触れない。
        unsafe {
            std::arch::asm!(
                "vpdpbusd {sum}, {input}, {weight}",
                sum = inout(zmm_reg) sum,
                input = in(zmm_reg) input,
                weight = in(zmm_reg) weight,
                options(pure, nomem, nostack, preserves_flags),
            );
        }
        sum
    }

    #[inline]
    pub(crate) fn fused<const N: usize, const OUT: usize>(
        us: &[i16; N],
        them: &[i16; N],
        weights: &FusedWeights,
        biases: &[i32; OUT],
    ) -> [i32; OUT] {
        assert_eq!(N, 1536);
        assert_eq!(OUT, 16);
        let mut output = [0; OUT];
        // SAFETY: cfg が命令セットを保証する。入力・重み・出力の寸法は確認済み。
        // FusedWeights は reorder の [24][4][4][64] 配置と64B境界を保証する。
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
                    let w = weights.0.as_ptr().add((base + block) * 1024).cast::<__m512i>();
                    for (g, group) in sums.iter_mut().enumerate() {
                        for (j, sum) in group.iter_mut().enumerate() {
                            *sum = dpbusd_register(
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
