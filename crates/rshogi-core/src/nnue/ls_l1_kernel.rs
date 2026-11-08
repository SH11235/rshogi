//! L1=1536 / 2304 / 3072、出力16 の LayerStacks L1 と FT 出力変換を融合する AVX-512 カーネル。

#[cfg(all(
    target_arch = "x86_64",
    target_feature = "avx512f",
    target_feature = "avx512bw",
    any(target_feature = "avx512vnni", ls_avx512bw_fused_1536)
))]
pub(super) mod avx512 {
    use super::super::accumulator::AlignedBox;
    use super::super::layers::AffineTransform;
    use std::arch::x86_64::*;

    /// reorder だけが構築する、64B 境界の [INPUT / 64][4][4][64] 融合用重み。
    /// 内部バッファの長さ・配置は構築後に変更しない。
    pub(crate) struct FusedWeights<const INPUT: usize>(AlignedBox<i8>);

    // 非 VNNI の融合は、性能を確認した通常 LayerStacks に限る。
    pub(crate) const fn supported<const INPUT: usize, const OUTPUT: usize>() -> bool {
        OUTPUT == 16
            && if cfg!(target_feature = "avx512vnni") {
                matches!(INPUT, 1536 | 2304 | 3072)
            } else {
                !cfg!(any(feature = "nnue-threat", feature = "nnue-psqt")) && matches!(INPUT, 1536)
            }
    }
    pub(crate) fn reorder<const INPUT: usize, const OUTPUT: usize>(
        l1: &AffineTransform<INPUT, OUTPUT>,
    ) -> Option<FusedWeights<INPUT>> {
        if !supported::<INPUT, OUTPUT>() {
            return None;
        }
        let mut weights = AlignedBox::new_zeroed(INPUT * OUTPUT);
        // packus の lane L / dword j を、出力 4g+d の重みへ対応付ける。
        for k in 0..INPUT / 64 {
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
        weights: &mut Option<FusedWeights<INPUT>>,
        edit: impl FnOnce(&mut AffineTransform<INPUT, OUTPUT>),
    ) {
        struct Rebuild<'a, const INPUT: usize, const OUTPUT: usize> {
            l1: &'a mut AffineTransform<INPUT, OUTPUT>,
            weights: &'a mut Option<FusedWeights<INPUT>>,
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
    /// acc は N 個の i16 を指し、THREAT のとき threat も N 個を指す。
    /// offset は64の倍数で offset+64 <= N/2。
    #[inline]
    #[target_feature(enable = "avx512f,avx512bw")]
    unsafe fn pack64<const N: usize, const THREAT: bool>(
        acc: *const i16,
        threat: *const i16,
        offset: usize,
    ) -> __m512i {
        // SAFETY: 呼び出し側が両入力の offset..offset+64 とその N/2 先を保証する。
        // THREAT=false では threat を読まない。unaligned load を使う。
        // clamp 後の積は最大16129で i16 に収まり、右シフト後は0..=126。
        unsafe {
            let zero = _mm512_setzero_si512();
            let max = _mm512_set1_epi16(127);
            let load = |index: usize| {
                let x = _mm512_loadu_si512(acc.add(index).cast());
                if THREAT {
                    _mm512_add_epi16(x, _mm512_loadu_si512(threat.add(index).cast()))
                } else {
                    x
                }
            };
            let a0 = _mm512_min_epi16(_mm512_max_epi16(load(offset), zero), max);
            let a1 = _mm512_min_epi16(_mm512_max_epi16(load(offset + 32), zero), max);
            let b0 = _mm512_min_epi16(_mm512_max_epi16(load(offset + N / 2), zero), max);
            let b1 = _mm512_min_epi16(_mm512_max_epi16(load(offset + N / 2 + 32), zero), max);
            let p0 = _mm512_srli_epi16::<7>(_mm512_mullo_epi16(a0, b0));
            let p1 = _mm512_srli_epi16::<7>(_mm512_mullo_epi16(a1, b1));
            _mm512_packus_epi16(p0, p1)
        }
    }

    // register を固定し、重みロードの load-op 化と spill を避ける。
    #[cfg(target_feature = "avx512vnni")]
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

    #[cfg(not(target_feature = "avx512vnni"))]
    #[inline]
    fn dpbusd_register(sum: __m512i, input: __m512i, weight: __m512i) -> __m512i {
        // SAFETY: cfg が AVX-512F/BW を保証する。input は 0..=126 なので
        // 隣接2項の積和は i16 の範囲に収まり、飽和しない。
        unsafe {
            let pairs = _mm512_maddubs_epi16(input, weight);
            let products = _mm512_madd_epi16(pairs, _mm512_set1_epi16(1));
            _mm512_add_epi32(sum, products)
        }
    }

    #[cfg(target_feature = "avx512vnni")]
    pub(crate) fn sparse_index_only<const N: usize, const OUT: usize>(
        us: &[i16; N],
        them: &[i16; N],
        weights: &[i8],
        biases: &[i32; OUT],
    ) -> [i32; OUT] {
        use std::mem::MaybeUninit;
        assert!(matches!(N, 2304 | 3072));
        assert_eq!(OUT, 16);
        assert!(weights.len() >= N * OUT);
        let mut values = [MaybeUninit::<u32>::uninit(); 768];
        let mut indices = [MaybeUninit::<u16>::uninit(); 768 + 16];
        let mut count = 0usize;
        let mut output = [0; OUT];
        // SAFETY: N は 2304 または 3072。pack64 の参照範囲は両視点の配列内。
        // 各 block は values の 16 要素を初期化し、indices は圧縮結果を最大 16 個ずつ書く。
        // count <= N/4、配列末尾には 16 要素の余白がある。積和では初期化済み index と
        // 対応する values、長さ N*16 の重み行だけを読み、16 要素の output に書く。
        unsafe {
            let permutation =
                _mm512_setr_epi32(0, 1, 4, 5, 8, 9, 12, 13, 2, 3, 6, 7, 10, 11, 14, 15);
            let lane_indices =
                _mm512_setr_epi32(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15);
            for (acc, base) in [(us, 0), (them, N / 128)] {
                for block in 0..N / 128 {
                    let block_index = base + block;
                    let x = pack64::<N, false>(acc.as_ptr(), acc.as_ptr(), block * 64);
                    let x = _mm512_permutexvar_epi32(permutation, x);
                    _mm512_storeu_si512(values.as_mut_ptr().add(block_index * 16).cast(), x);
                    let mask = _mm512_test_epi32_mask(x, x);
                    let offsets = _mm512_add_epi32(
                        lane_indices,
                        _mm512_set1_epi32((block_index * 16) as i32),
                    );
                    let packed = _mm512_maskz_compress_epi32(mask, offsets);
                    _mm256_storeu_si256(
                        indices.as_mut_ptr().add(count).cast(),
                        _mm512_cvtepi32_epi16(packed),
                    );
                    count += mask.count_ones() as usize;
                }
            }
            let mut sums = [_mm512_setzero_si512(); 4];
            let row = |i: usize| {
                let index = *indices.get_unchecked(i).assume_init_ref() as usize;
                let input =
                    _mm512_set1_epi32(*values.get_unchecked(index).assume_init_ref() as i32);
                let weight = _mm512_loadu_si512(weights.as_ptr().add(index * 64).cast());
                (input, weight)
            };
            let mut i = 0;
            while i + 4 <= count {
                for (k, sum) in sums.iter_mut().enumerate() {
                    let (input, weight) = row(i + k);
                    *sum = _mm512_dpbusd_epi32(*sum, input, weight);
                }
                i += 4;
            }
            while i < count {
                let (input, weight) = row(i);
                sums[0] = _mm512_dpbusd_epi32(sums[0], input, weight);
                i += 1;
            }
            let sum = _mm512_add_epi32(
                _mm512_add_epi32(sums[0], sums[1]),
                _mm512_add_epi32(sums[2], sums[3]),
            );
            let bias = _mm512_loadu_si512(biases.as_ptr().cast());
            _mm512_storeu_si512(output.as_mut_ptr().cast(), _mm512_add_epi32(sum, bias));
        }
        output
    }

    #[inline]
    pub(crate) fn fused<const N: usize, const OUT: usize>(
        us: &[i16; N],
        them: &[i16; N],
        weights: &FusedWeights<N>,
        biases: &[i32; OUT],
    ) -> [i32; OUT] {
        fused_impl::<N, OUT, false>(us, them, us, them, weights, biases)
    }

    #[cfg(feature = "nnue-threat")]
    #[inline]
    pub(crate) fn fused_with_threat<const N: usize, const OUT: usize>(
        us: &[i16; N],
        them: &[i16; N],
        us_threat: &[i16; N],
        them_threat: &[i16; N],
        weights: &FusedWeights<N>,
        biases: &[i32; OUT],
    ) -> [i32; OUT] {
        fused_impl::<N, OUT, true>(us, them, us_threat, them_threat, weights, biases)
    }

    #[inline]
    fn fused_impl<const N: usize, const OUT: usize, const THREAT: bool>(
        us: &[i16; N],
        them: &[i16; N],
        us_threat: &[i16; N],
        them_threat: &[i16; N],
        weights: &FusedWeights<N>,
        biases: &[i32; OUT],
    ) -> [i32; OUT] {
        assert!(matches!(N, 1536 | 2304 | 3072));
        assert_eq!(OUT, 16);
        let mut output = [0; OUT];
        // SAFETY: cfg が命令セットを保証する。入力・重み・出力の寸法は確認済み。
        // FusedWeights は reorder の [N / 64][4][4][64] 配置と64B境界を保証する。
        // pack64 は各視点の範囲内だけを読み、store は4出力ずつ行う。
        unsafe {
            const CHAINS: usize = if cfg!(target_feature = "avx512vnni") {
                4
            } else {
                2
            };
            let mut sums = [[_mm512_setzero_si512(); CHAINS]; 4];
            for (acc, threat, base) in [(us, us_threat, 0), (them, them_threat, N / 128)] {
                for block in 0..N / 128 {
                    let x = pack64::<N, THREAT>(acc.as_ptr(), threat.as_ptr(), block * 64);
                    let broadcasts = [
                        _mm512_shuffle_epi32::<0x00>(x),
                        _mm512_shuffle_epi32::<0x55>(x),
                        _mm512_shuffle_epi32::<0xaa>(x),
                        _mm512_shuffle_epi32::<0xff>(x),
                    ];
                    let w = weights.0.as_ptr().add((base + block) * 1024).cast::<__m512i>();
                    for (g, group) in sums.iter_mut().enumerate() {
                        for j in 0..4 {
                            let sum = &mut group[j % CHAINS];
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
                #[cfg(target_feature = "avx512vnni")]
                let sum = _mm512_add_epi32(
                    _mm512_add_epi32(group[0], group[1]),
                    _mm512_add_epi32(group[2], group[3]),
                );
                #[cfg(not(target_feature = "avx512vnni"))]
                let sum = _mm512_add_epi32(group[0], group[1]);
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
