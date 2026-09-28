//! HalfKX各系統のread・SIMD経路を行優先の参照積和と照合する。

use super::super::accumulator::AlignedBox;
use super::padded_input;

fn dense_weight(input: usize, output: usize) -> i8 {
    ((input * 7 + output * 11) % 9) as i8 - 4
}

/// padding列にも非ゼロ重みを置き、forwardのゼロ埋め漏れを検出する。
pub(crate) fn dense_fixture(input: usize, output: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    for o in 0..output {
        bytes.extend_from_slice(&(1024 + o as i32 * 17).to_le_bytes());
    }
    for o in 0..output {
        for i in 0..padded_input(input) {
            bytes.push(if i < input {
                dense_weight(i, o) as u8
            } else {
                127
            });
        }
    }
    bytes
}

pub(crate) fn dense_reference(input: &[u8], output: usize) -> Vec<i32> {
    (0..output)
        .map(|o| {
            input.iter().enumerate().fold(1024 + o as i32 * 17, |sum, (i, &x)| {
                sum + i32::from(x) * i32::from(dense_weight(i, o))
            })
        })
        .collect()
}

#[test]
fn padded_affine_input_preserves_logical_bytes_and_zero_tail() {
    fn check<const N: usize>() {
        let mut input = super::PaddedAffineInput::<N>::new();
        input.0.fill(255);
        let padded = input.as_padded();
        assert_eq!(padded.len(), padded_input(N));
        assert_eq!(padded.as_ptr() as usize % 64, 0);
        assert!(padded[..N].iter().all(|&x| x == 255));
        assert!(padded[N..].iter().all(|&x| x == 0));
    }
    check::<0>();
    check::<1>();
    check::<8>();
    check::<16>();
    check::<31>();
    check::<32>();
    check::<33>();
    check::<1024>();
}

pub(crate) fn check<const INPUT: usize, const OUTPUT: usize>(
    mut propagate: impl FnMut(&[u8], &[u8], bool) -> [i32; OUTPUT],
) {
    let padded = padded_input(INPUT);
    let mut state = 0x243f_6a88_85a3_08d3u64;
    let mut next = || {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (state >> 32) as u32
    };
    for case in 0..24 {
        let biases: [i32; OUTPUT] = std::array::from_fn(|_| {
            // SIMDの複数出力経路ではi32の周回も照合する。
            if case >= 20
                && (OUTPUT == 8 || OUTPUT.is_multiple_of(16))
                && cfg!(all(target_arch = "x86_64", target_feature = "ssse3"))
            {
                next() as i32
            } else {
                (next() % 200_001) as i32 - 100_000
            }
        });
        let mut bytes = Vec::new();
        for b in biases {
            bytes.extend_from_slice(&b.to_le_bytes());
        }
        let logical: Vec<i8> = (0..OUTPUT * padded)
            .map(|i| {
                if i % padded >= INPUT {
                    0
                } else {
                    match case {
                        0 => -128,
                        1 => 127,
                        _ => next() as i8,
                    }
                }
            })
            .collect();
        bytes.extend(logical.iter().map(|&w| w as u8));
        for seven_bit in [false, true] {
            let max = if seven_bit { 127 } else { 255 };
            let mut input = AlignedBox::<u8>::new_zeroed(padded);
            for x in input.iter_mut().take(INPUT) {
                *x = match case {
                    0 | 1 => max,
                    2 => 0,
                    _ => next() as u8 & max,
                };
            }
            let expected: [i32; OUTPUT] = std::array::from_fn(|o| {
                input.iter().take(INPUT).enumerate().fold(biases[o], |acc, (i, &x)| {
                    acc.wrapping_add(i32::from(x) * i32::from(logical[o * padded + i]))
                })
            });
            assert_eq!(
                propagate(&bytes, &input, seven_bit),
                expected,
                "INPUT={INPUT}, OUTPUT={OUTPUT}, case={case}, seven_bit={seven_bit}"
            );
        }
    }
}

/// 各系統のprivateな7bit・AVX2経路を同じデータで直接検証する。
macro_rules! halfkx_affine_tests {
    ($affine:ident) => {
        #[test]
        fn affine_randomized_all_halfkx_shapes() {
            fn check<const INPUT: usize, const OUTPUT: usize>() {
                crate::nnue::layers::halfkx_affine_tests::check::<INPUT, OUTPUT>(
                    |bytes, input, seven_bit| {
                        let layer = $affine::<INPUT, OUTPUT>::read(&mut &bytes[..]).unwrap();
                        let mut output = [0; OUTPUT];
                        layer.propagate(input, &mut output);
                        if seven_bit {
                            let mut seven = [0; OUTPUT];
                            layer.propagate_7bit(input, &mut seven);
                            assert_eq!(output, seven, "7bit / full range");
                        }
                        #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
                        {
                            let mut avx2 = [0; OUTPUT];
                            // SAFETY: cfgがAVX2を保証する。入力は64バイトアラインかつ
                            // PADDED_INPUT要素を持ち、readが対応する重み配置を構築する。
                            unsafe {
                                layer.propagate_avx2_loop_inverted::<true>(input, &mut avx2);
                                assert_eq!(output, avx2, "AVX2 / dispatch");
                                if seven_bit {
                                    layer.propagate_avx2_loop_inverted::<false>(input, &mut avx2);
                                    assert_eq!(output, avx2, "AVX2 7bit / dispatch");
                                }
                            }
                        }
                        output
                    },
                );
            }
            // CReLU/SCReLU/PairwiseのL1、隠れ層、最終層の実使用寸法。
            check::<256, 32>();
            check::<512, 32>();
            check::<1024, 32>();
            check::<512, 8>();
            check::<1024, 8>();
            check::<2048, 8>();
            check::<768, 16>();
            check::<1536, 16>();
            check::<8, 32>();
            check::<8, 64>();
            check::<8, 96>();
            check::<16, 64>();
            check::<32, 32>();
            check::<32, 1>();
            check::<64, 1>();
            check::<96, 1>();
            // 3組の端数と、AVX2/SSSE3/スカラーへのフォールバック境界。
            check::<33, 48>();
            check::<760, 8>();
            // 2チャンク対の端数、8組未満、8組を割り切れない入力長。
            check::<1, 8>();
            check::<33, 8>();
            check::<65, 8>();
            check::<1025, 8>();
            check::<2049, 8>();
            check::<32, 4>();
        }
    };
}

pub(crate) use halfkx_affine_tests;
