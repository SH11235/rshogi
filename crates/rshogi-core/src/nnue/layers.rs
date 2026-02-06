//! ネットワーク層の実装
//!
//! - `AffineTransform`: 全結合アフィン変換層（入力×重み + バイアス）
//! - `ClippedReLU`: 整数スケーリング付きのクリップ付き ReLU 層
//! - `SCReLU`: Squared Clipped ReLU 層（bullet-shogi SCReLUモデル用）

use super::accumulator::AlignedBox;
use super::constants::WEIGHT_SCALE_BITS;
use std::io::{self, Read};

/// パディング済み入力次元（SIMDアライメント用）
const fn padded_input(input_dim: usize) -> usize {
    input_dim.div_ceil(32) * 32
}

/// AVX2での水平加算（i32×8 → i32）
#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
#[inline]
unsafe fn hsum_i32_avx2(v: std::arch::x86_64::__m256i) -> i32 {
    use std::arch::x86_64::*;

    // 上位128bitと下位128bitを加算
    let hi = _mm256_extracti128_si256(v, 1);
    let lo = _mm256_castsi256_si128(v);
    let sum128 = _mm_add_epi32(lo, hi);

    // 64bit加算
    let hi64 = _mm_unpackhi_epi64(sum128, sum128);
    let sum64 = _mm_add_epi32(sum128, hi64);

    // 32bit加算
    let hi32 = _mm_shuffle_epi32(sum64, 1);
    let sum32 = _mm_add_epi32(sum64, hi32);

    _mm_cvtsi128_si32(sum32)
}

/// AVX512-VNNI用 DPBUSD（512bit版）
///
/// Intel Ice Lake以降/AMD Zen 4以降で利用可能。
/// `vpdpbusd` 命令で u8×i8→i32 積和演算を1命令で実行。
/// 512bit = 64バイト = 16 x i32 を一度に処理。
#[cfg(all(target_arch = "x86_64", target_feature = "avx512vnni"))]
#[inline]
unsafe fn m512_add_dpbusd_epi32(
    acc: &mut std::arch::x86_64::__m512i,
    a: std::arch::x86_64::__m512i,
    b: std::arch::x86_64::__m512i,
) {
    use std::arch::x86_64::*;
    *acc = _mm512_dpbusd_epi32(*acc, a, b);
}

/// AVX512用 DPBUSD エミュレーション（VNNI非対応時）
///
/// AVX512 があるが VNNI がない場合のフォールバック。
#[cfg(all(
    target_arch = "x86_64",
    target_feature = "avx512bw",
    not(target_feature = "avx512vnni")
))]
#[inline]
unsafe fn m512_add_dpbusd_epi32(
    acc: &mut std::arch::x86_64::__m512i,
    a: std::arch::x86_64::__m512i,
    b: std::arch::x86_64::__m512i,
) {
    use std::arch::x86_64::*;
    // maddubs: u8×i8 → i16 (飽和加算)
    let product = _mm512_maddubs_epi16(a, b);
    // madd: i16×i16 → i32 (隣接ペアの積和)
    let product32 = _mm512_madd_epi16(product, _mm512_set1_epi16(1));
    *acc = _mm512_add_epi32(*acc, product32);
}

/// AVX512-VNNI用 DPBUSD（256bit版、VL拡張使用）
///
/// Intel Ice Lake以降/AMD Zen 4以降で利用可能。
/// `vpdpbusd` 命令で u8×i8→i32 積和演算を1命令で実行。
#[cfg(all(
    target_arch = "x86_64",
    target_feature = "avx512vnni",
    target_feature = "avx512vl"
))]
#[inline]
unsafe fn m256_add_dpbusd_epi32(
    acc: &mut std::arch::x86_64::__m256i,
    a: std::arch::x86_64::__m256i,
    b: std::arch::x86_64::__m256i,
) {
    use std::arch::x86_64::*;
    *acc = _mm256_dpbusd_epi32(*acc, a, b);
}

/// AVX2用 DPBUSD エミュレーション（u8×i8→i32積和演算）
///
/// VNNI非対応CPU向け。`maddubs` + `madd` の2命令で積和演算を実行。
#[cfg(all(
    target_arch = "x86_64",
    target_feature = "avx2",
    not(all(target_feature = "avx512vnni", target_feature = "avx512vl"))
))]
#[inline]
unsafe fn m256_add_dpbusd_epi32(
    acc: &mut std::arch::x86_64::__m256i,
    a: std::arch::x86_64::__m256i,
    b: std::arch::x86_64::__m256i,
) {
    use std::arch::x86_64::*;
    let product = _mm256_maddubs_epi16(a, b);
    let product32 = _mm256_madd_epi16(product, _mm256_set1_epi16(1));
    *acc = _mm256_add_epi32(*acc, product32);
}

/// SSE2での水平加算（i32×4 → i32）
#[cfg(all(
    target_arch = "x86_64",
    target_feature = "sse2",
    not(target_feature = "avx2")
))]
#[inline]
unsafe fn hsum_i32_sse2(v: std::arch::x86_64::__m128i) -> i32 {
    use std::arch::x86_64::*;

    // 64bit加算
    let hi64 = _mm_unpackhi_epi64(v, v);
    let sum64 = _mm_add_epi32(v, hi64);

    // 32bit加算
    let hi32 = _mm_shuffle_epi32(sum64, 1);
    let sum32 = _mm_add_epi32(sum64, hi32);

    _mm_cvtsi128_si32(sum32)
}

/// SSSE3用 DPBUSD エミュレーション（u8×i8→i32積和演算）
/// _mm_maddubs_epi16 を使用（SSSE3命令）
#[cfg(all(
    target_arch = "x86_64",
    target_feature = "ssse3",
    not(target_feature = "avx2")
))]
#[inline]
unsafe fn m128_add_dpbusd_epi32(
    acc: &mut std::arch::x86_64::__m128i,
    a: std::arch::x86_64::__m128i,
    b: std::arch::x86_64::__m128i,
) {
    use std::arch::x86_64::*;
    let product = _mm_maddubs_epi16(a, b); // SSSE3命令
    let product32 = _mm_madd_epi16(product, _mm_set1_epi16(1));
    *acc = _mm_add_epi32(*acc, product32);
}

/// WASM SIMD128: u8×i8 の16要素内積を i32x4 に集約
#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
#[inline]
unsafe fn dot_i8x16_u8i8_preexpanded(
    in_lo: std::arch::wasm32::v128,
    in_hi: std::arch::wasm32::v128,
    w_vec: std::arch::wasm32::v128,
) -> std::arch::wasm32::v128 {
    use std::arch::wasm32::*;
    let w_lo = i16x8_extend_low_i8x16(w_vec);
    let w_hi = i16x8_extend_high_i8x16(w_vec);

    let prod_lo = i16x8_mul(in_lo, w_lo);
    let prod_hi = i16x8_mul(in_hi, w_hi);

    let sum32_lo_lo = i32x4_extend_low_i16x8(prod_lo);
    let sum32_lo_hi = i32x4_extend_high_i16x8(prod_lo);
    let sum32_hi_lo = i32x4_extend_low_i16x8(prod_hi);
    let sum32_hi_hi = i32x4_extend_high_i16x8(prod_hi);

    let mut acc = i32x4_add(sum32_lo_lo, sum32_lo_hi);
    acc = i32x4_add(acc, sum32_hi_lo);
    i32x4_add(acc, sum32_hi_hi)
}

/// WASM SIMD128: 入力ベクトルをu16拡張して内積を計算
#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
#[inline]
unsafe fn dot_i8x16_u8i8(
    in_vec: std::arch::wasm32::v128,
    w_vec: std::arch::wasm32::v128,
) -> std::arch::wasm32::v128 {
    use std::arch::wasm32::*;
    let in_lo = i16x8_extend_low_u8x16(in_vec);
    let in_hi = i16x8_extend_high_u8x16(in_vec);
    dot_i8x16_u8i8_preexpanded(in_lo, in_hi, w_vec)
}

/// WASM SIMD128: i32x4 の水平加算
#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
#[inline]
unsafe fn hsum_i32x4(v: std::arch::wasm32::v128) -> i32 {
    use std::arch::wasm32::*;
    i32x4_extract_lane::<0>(v)
        + i32x4_extract_lane::<1>(v)
        + i32x4_extract_lane::<2>(v)
        + i32x4_extract_lane::<3>(v)
}

/// WASM SIMD128: 2本のi32x4を水平加算（シャッフル + 加算）
#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
#[inline]
unsafe fn hadd_i32x4(
    x0: std::arch::wasm32::v128,
    x1: std::arch::wasm32::v128,
) -> std::arch::wasm32::v128 {
    use std::arch::wasm32::*;
    i32x4_add(i32x4_shuffle::<0, 2, 4, 6>(x0, x1), i32x4_shuffle::<1, 3, 5, 7>(x0, x1))
}

/// WASM SIMD128: 4本のi32x4を水平加算して1本のi32x4に詰める
#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
#[inline]
unsafe fn haddx4(
    z0: std::arch::wasm32::v128,
    z1: std::arch::wasm32::v128,
    z2: std::arch::wasm32::v128,
    z3: std::arch::wasm32::v128,
) -> std::arch::wasm32::v128 {
    hadd_i32x4(hadd_i32x4(z0, z1), hadd_i32x4(z2, z3))
}

/// アフィン変換層
pub struct AffineTransform<const INPUT_DIM: usize, const OUTPUT_DIM: usize> {
    /// バイアス
    pub biases: [i32; OUTPUT_DIM],
    /// 重み（転置形式で保持、64バイトアライン）
    pub weights: AlignedBox<i8>,
}

impl<const INPUT_DIM: usize, const OUTPUT_DIM: usize> AffineTransform<INPUT_DIM, OUTPUT_DIM> {
    const PADDED_INPUT: usize = padded_input(INPUT_DIM);

    /// チャンクサイズ（u8×4 = i32として読む単位）
    /// スクランブル形式重みとループ逆転最適化用
    #[cfg(any(
        all(target_arch = "x86_64", target_feature = "avx2"),
        all(
            target_arch = "x86_64",
            target_feature = "ssse3",
            not(target_feature = "avx2")
        )
    ))]
    const CHUNK_SIZE: usize = 4;

    /// 入力チャンク数（ループ逆転最適化用）
    #[cfg(any(
        all(target_arch = "x86_64", target_feature = "avx2"),
        all(
            target_arch = "x86_64",
            target_feature = "ssse3",
            not(target_feature = "avx2")
        )
    ))]
    const NUM_INPUT_CHUNKS: usize = Self::PADDED_INPUT / Self::CHUNK_SIZE;

    /// スクランブル形式のウェイトを使用するかどうか
    /// AVX2: OUTPUT_DIM % 8 == 0、SSSE3: OUTPUT_DIM % 4 == 0 の場合に使用
    #[cfg(any(
        all(target_arch = "x86_64", target_feature = "avx2"),
        all(
            target_arch = "x86_64",
            target_feature = "ssse3",
            not(target_feature = "avx2")
        )
    ))]
    #[inline]
    const fn should_use_scrambled_weights() -> bool {
        if cfg!(all(target_arch = "x86_64", target_feature = "avx2")) {
            OUTPUT_DIM.is_multiple_of(8) && OUTPUT_DIM > 0
        } else {
            OUTPUT_DIM.is_multiple_of(4) && OUTPUT_DIM > 0
        }
    }

    /// 重みインデックスのスクランブル変換
    /// 行優先（output→input）から列優先（input_chunk→output）に変換
    ///
    /// 元のレイアウト: weights[output][input]
    /// 変換後: weights[input_chunk][output][4]
    ///
    /// i = output * PADDED_INPUT + input の元インデックスに対して
    /// スクランブル後のインデックスを返す
    #[cfg(any(
        all(target_arch = "x86_64", target_feature = "avx2"),
        all(
            target_arch = "x86_64",
            target_feature = "ssse3",
            not(target_feature = "avx2")
        )
    ))]
    #[inline]
    const fn get_weight_index_scrambled(i: usize) -> usize {
        // i = output * PADDED_INPUT + input
        // output = i / PADDED_INPUT
        // input = i % PADDED_INPUT
        // input_chunk = input / CHUNK_SIZE
        // byte_in_chunk = input % CHUNK_SIZE
        //
        // 変換後: input_chunk * OUTPUT_DIM * CHUNK_SIZE + output * CHUNK_SIZE + byte_in_chunk
        (i / Self::CHUNK_SIZE) % (Self::PADDED_INPUT / Self::CHUNK_SIZE)
            * OUTPUT_DIM
            * Self::CHUNK_SIZE
            + i / Self::PADDED_INPUT * Self::CHUNK_SIZE
            + i % Self::CHUNK_SIZE
    }

    /// ファイルから読み込み
    pub fn read<R: Read>(reader: &mut R) -> io::Result<Self> {
        // バイアスを読み込み
        let mut biases = [0i32; OUTPUT_DIM];
        let mut buf4 = [0u8; 4];
        for bias in biases.iter_mut() {
            reader.read_exact(&mut buf4)?;
            *bias = i32::from_le_bytes(buf4);
        }

        // 重みを読み込み（64バイトアラインで確保）
        let weight_size = OUTPUT_DIM * Self::PADDED_INPUT;
        let mut weights = AlignedBox::new_zeroed(weight_size);
        let mut buf1 = [0u8; 1];

        // AVX2: OUTPUT_DIM % 8 == 0、SSSE3: OUTPUT_DIM % 4 == 0 の場合はスクランブル形式で格納
        #[cfg(any(
            all(target_arch = "x86_64", target_feature = "avx2"),
            all(
                target_arch = "x86_64",
                target_feature = "ssse3",
                not(target_feature = "avx2")
            )
        ))]
        {
            for i in 0..weight_size {
                reader.read_exact(&mut buf1)?;
                let idx = if Self::should_use_scrambled_weights() {
                    Self::get_weight_index_scrambled(i)
                } else {
                    i
                };
                weights[idx] = buf1[0] as i8;
            }
        }

        // 非AVX2/非SSSE3環境: 元の順序で格納
        #[cfg(not(any(
            all(target_arch = "x86_64", target_feature = "avx2"),
            all(
                target_arch = "x86_64",
                target_feature = "ssse3",
                not(target_feature = "avx2")
            )
        )))]
        {
            for i in 0..weight_size {
                reader.read_exact(&mut buf1)?;
                weights[i] = buf1[0] as i8;
            }
        }

        Ok(Self { biases, weights })
    }

    /// LEB128圧縮形式から読み込み
    pub fn read_leb128<R: Read>(reader: &mut R) -> io::Result<Self> {
        use super::leb128::read_signed_leb128;

        // バイアスを読み込み
        let mut biases = [0i32; OUTPUT_DIM];
        for bias in biases.iter_mut() {
            let val = read_signed_leb128(reader)?;
            *bias = val as i32;
        }

        // 重みを読み込み（64バイトアラインで確保）
        let weight_size = OUTPUT_DIM * Self::PADDED_INPUT;
        let mut weights = AlignedBox::new_zeroed(weight_size);

        // AVX2/SSSE3: スクランブル形式で格納
        #[cfg(any(
            all(target_arch = "x86_64", target_feature = "avx2"),
            all(
                target_arch = "x86_64",
                target_feature = "ssse3",
                not(target_feature = "avx2")
            )
        ))]
        {
            for i in 0..weight_size {
                let val = read_signed_leb128(reader)?;
                let idx = if Self::should_use_scrambled_weights() {
                    Self::get_weight_index_scrambled(i)
                } else {
                    i
                };
                weights[idx] = val as i8;
            }
        }

        // 非AVX2/非SSSE3環境: 元の順序で格納
        #[cfg(not(any(
            all(target_arch = "x86_64", target_feature = "avx2"),
            all(
                target_arch = "x86_64",
                target_feature = "ssse3",
                not(target_feature = "avx2")
            )
        )))]
        {
            for i in 0..weight_size {
                let val = read_signed_leb128(reader)?;
                weights[i] = val as i8;
            }
        }

        Ok(Self { biases, weights })
    }

    /// 順伝播
    ///
    /// AVX2/SSE2/WASMのSIMD最適化版。
    /// 密な行列積方式（YaneuraOuスタイル）で実装。
    ///
    /// # アライメント要件
    ///
    /// **重要**: 入力スライスは64バイトアライメントが必要です。
    ///
    /// | ターゲット | 必要アライメント | 使用命令 |
    /// |-----------|-----------------|----------|
    /// | AVX2 (`x86_64`) | 32バイト以上 | `_mm256_load_si256` |
    /// | SSE2 (`x86_64`) | 16バイト以上 | `_mm_load_si128` |
    /// | WASM SIMD128 | 不要 | `v128_load`（任意アドレス対応） |
    /// | スカラー | 不要 | - |
    ///
    /// アライメントを保証するには、[`Aligned`](super::accumulator::Aligned) ラッパーを使用してください:
    ///
    /// ```ignore
    /// use crate::nnue::accumulator::Aligned;
    ///
    /// let mut input = Aligned([0u8; 512]);  // 64バイトアライン
    /// transform.propagate(&input.0, &mut output);
    /// ```
    ///
    /// **警告**: アライメントされていない入力を渡すと、AVX2/SSE2環境で
    /// 未定義動作（SIGSEGV等）が発生します。
    ///
    /// # 入力サイズの契約
    ///
    /// 入力スライスは `PADDED_INPUT` バイト以上である必要がある。
    /// SIMD実装は32バイト（AVX2）または16バイト（SSE2）単位で処理するため、
    /// `INPUT_DIM` より小さい入力を渡すと境界外アクセスが発生する。
    ///
    /// # 入力密度
    ///
    /// 実測結果（2025-12-18）: 約40%（39-42%）
    /// → スパース最適化には高すぎるため、密な行列積方式が正しい選択。
    /// 詳細は `network.rs` の diagnostics 計測コードを参照。
    pub fn propagate(&self, input: &[u8], output: &mut [i32; OUTPUT_DIM]) {
        debug_assert!(
            input.len() >= Self::PADDED_INPUT,
            "input length {} is less than PADDED_INPUT {}",
            input.len(),
            Self::PADDED_INPUT
        );

        // AVX-512: 512bit = 64 x u8/i8 または 16 x i32
        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "avx512f",
            any(target_feature = "avx512vnni", target_feature = "avx512bw")
        ))]
        {
            // SAFETY:
            // - input.len() >= PADDED_INPUT (debug_assert で検証済み)
            // - weights.len() >= OUTPUT_DIM * PADDED_INPUT (構造上保証)
            // - input は Aligned<[u8; N]> で64バイトアライン
            // - weights は AlignedBox<i8> で64バイトアライン（スクランブル形式）
            // - PADDED_INPUT は32の倍数なのでオフセットは常に64バイト境界
            // - biases/output はアライン未保証だが、unaligned load/store を使用
            unsafe {
                use std::arch::x86_64::*;

                // OUTPUT_DIM % 16 == 0 の場合: ループ逆転最適化版（AVX-512）
                // 入力をブロードキャストして全出力に同時適用
                #[allow(clippy::needless_range_loop)]
                if OUTPUT_DIM.is_multiple_of(16) && OUTPUT_DIM > 0 {
                    // 出力レジスタ数（16出力/レジスタ）
                    const MAX_REGS: usize = 64; // 最大1024出力まで対応
                    let num_regs = OUTPUT_DIM / 16;
                    debug_assert!(num_regs <= MAX_REGS);

                    // アキュムレータをバイアスで初期化
                    let mut acc = [_mm512_setzero_si512(); MAX_REGS];
                    let bias_ptr = self.biases.as_ptr() as *const __m512i;
                    for k in 0..num_regs {
                        acc[k] = _mm512_loadu_si512(bias_ptr.add(k) as *const i32);
                    }

                    let input32 = input.as_ptr() as *const i32;
                    let weights_ptr = self.weights.as_ptr();

                    // 外側: 入力チャンク（入力4バイト = 1 i32）
                    for i in 0..Self::NUM_INPUT_CHUNKS {
                        // 入力4バイトを全レーンにブロードキャスト
                        let in_val = _mm512_set1_epi32(*input32.add(i));

                        // この入力チャンクに対応する重みの開始位置
                        // スクランブル形式: weights[input_chunk][output][4]
                        let col =
                            weights_ptr.add(i * OUTPUT_DIM * Self::CHUNK_SIZE) as *const __m512i;

                        // 内側: 全出力レジスタに積和演算
                        for k in 0..num_regs {
                            m512_add_dpbusd_epi32(
                                &mut acc[k],
                                in_val,
                                _mm512_load_si512(col.add(k) as *const i32),
                            );
                        }
                    }

                    // 結果を出力
                    let out_ptr = output.as_mut_ptr() as *mut __m512i;
                    for k in 0..num_regs {
                        _mm512_storeu_si512(out_ptr.add(k) as *mut i32, acc[k]);
                    }
                    return;
                }

                // OUTPUT_DIM % 16 != 0 だが % 8 == 0 の場合: AVX2 にフォールスルー
            }
        }

        // AVX2: 256bit = 32 x u8/i8
        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "avx2",
            not(all(
                target_feature = "avx512f",
                any(target_feature = "avx512vnni", target_feature = "avx512bw")
            ))
        ))]
        {
            // SAFETY:
            // - input.len() >= PADDED_INPUT (debug_assert で検証済み)
            // - weights.len() >= OUTPUT_DIM * PADDED_INPUT (構造上保証)
            // - input は Aligned<[u8; N]> で64バイトアライン
            // - weights は AlignedBox<i8> で64バイトアライン（スクランブル形式）
            // - PADDED_INPUT は32の倍数なのでオフセットは常に32バイト境界
            // - biases/output はアライン未保証だが、unaligned load/store を使用
            unsafe {
                use std::arch::x86_64::*;

                // OUTPUT_DIM % 8 == 0 の場合: ループ逆転最適化版
                // 入力をブロードキャストして全出力に同時適用
                //
                // 疎入力処理は密度40%では効果なし（find_nnzオーバーヘッドが利点を上回る）
                // 計測結果: 疎入力版 634K NPS vs 密版 655K NPS
                #[allow(clippy::needless_range_loop)]
                if OUTPUT_DIM.is_multiple_of(8) && OUTPUT_DIM > 0 {
                    // 出力レジスタ数（8出力/レジスタ）
                    const MAX_REGS: usize = 128; // 最大1024出力まで対応
                    let num_regs = OUTPUT_DIM / 8;
                    debug_assert!(num_regs <= MAX_REGS);

                    // アキュムレータをバイアスで初期化
                    let mut acc = [_mm256_setzero_si256(); MAX_REGS];
                    let bias_ptr = self.biases.as_ptr() as *const __m256i;
                    for k in 0..num_regs {
                        acc[k] = _mm256_loadu_si256(bias_ptr.add(k));
                    }

                    let input32 = input.as_ptr() as *const i32;
                    let weights_ptr = self.weights.as_ptr();

                    // 外側: 入力チャンク（入力4バイト = 1 i32）
                    for i in 0..Self::NUM_INPUT_CHUNKS {
                        // 入力4バイトを全レーンにブロードキャスト
                        let in_val = _mm256_set1_epi32(*input32.add(i));

                        // この入力チャンクに対応する重みの開始位置
                        // スクランブル形式: weights[input_chunk][output][4]
                        let col =
                            weights_ptr.add(i * OUTPUT_DIM * Self::CHUNK_SIZE) as *const __m256i;

                        // 内側: 全出力レジスタに積和演算
                        for k in 0..num_regs {
                            m256_add_dpbusd_epi32(
                                &mut acc[k],
                                in_val,
                                _mm256_load_si256(col.add(k)),
                            );
                        }
                    }

                    // 結果を出力
                    let out_ptr = output.as_mut_ptr() as *mut __m256i;
                    for k in 0..num_regs {
                        _mm256_storeu_si256(out_ptr.add(k), acc[k]);
                    }
                    return;
                }

                // OUTPUT_DIM % 8 != 0 の場合: 従来の実装（出力ごとに処理）
                let num_chunks = Self::PADDED_INPUT / 32;
                let one = _mm256_set1_epi16(1);
                let input_ptr = input.as_ptr();
                let weights_ptr = self.weights.as_ptr();

                for (j, (out, &bias)) in output.iter_mut().zip(&self.biases).enumerate() {
                    let mut acc = _mm256_setzero_si256();
                    let weight_row_offset = j * Self::PADDED_INPUT;

                    for k in 0..num_chunks {
                        let offset = k * 32;
                        let in_vec = _mm256_load_si256(input_ptr.add(offset) as *const __m256i);
                        let w_vec = _mm256_load_si256(
                            weights_ptr.add(weight_row_offset + offset) as *const __m256i
                        );
                        let prod16 = _mm256_maddubs_epi16(in_vec, w_vec);
                        let prod32 = _mm256_madd_epi16(prod16, one);
                        acc = _mm256_add_epi32(acc, prod32);
                    }

                    *out = bias + hsum_i32_avx2(acc);
                }
            }
            return;
        }

        // SSSE3: 128bit = 16 x u8/i8 (ループ逆転最適化版)
        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "ssse3",
            not(target_feature = "avx2")
        ))]
        {
            // SAFETY:
            // - input.len() >= PADDED_INPUT (debug_assert で検証済み)
            // - weights.len() >= OUTPUT_DIM * PADDED_INPUT (構造上保証)
            // - input は Aligned<[u8; N]> で64バイトアライン（16バイト境界も満たす）
            // - weights は AlignedBox<i8> で64バイトアライン（スクランブル形式）
            // - PADDED_INPUT は32の倍数なのでオフセットは常に16バイト境界
            // - biases/output はアライン未保証だが、unaligned load/store を使用
            unsafe {
                use std::arch::x86_64::*;

                // OUTPUT_DIM % 4 == 0 の場合: ループ逆転最適化版
                // 入力をブロードキャストして全出力に同時適用
                #[allow(clippy::needless_range_loop)]
                if OUTPUT_DIM.is_multiple_of(4) && OUTPUT_DIM > 0 {
                    // 出力レジスタ数（4出力/レジスタ）
                    const MAX_REGS: usize = 256; // 最大1024出力まで対応
                    let num_regs = OUTPUT_DIM / 4;
                    debug_assert!(num_regs <= MAX_REGS);

                    // アキュムレータをバイアスで初期化
                    let mut acc = [_mm_setzero_si128(); MAX_REGS];
                    let bias_ptr = self.biases.as_ptr() as *const __m128i;
                    for k in 0..num_regs {
                        acc[k] = _mm_loadu_si128(bias_ptr.add(k));
                    }

                    let input32 = input.as_ptr() as *const i32;
                    let weights_ptr = self.weights.as_ptr();

                    // 外側: 入力チャンク（入力4バイト = 1 i32）
                    for i in 0..Self::NUM_INPUT_CHUNKS {
                        // 入力4バイトを全レーンにブロードキャスト
                        let in_val = _mm_set1_epi32(*input32.add(i));

                        // この入力チャンクに対応する重みの開始位置
                        // スクランブル形式: weights[input_chunk][output][4]
                        let col =
                            weights_ptr.add(i * OUTPUT_DIM * Self::CHUNK_SIZE) as *const __m128i;

                        // 内側: 全出力レジスタに積和演算
                        for k in 0..num_regs {
                            m128_add_dpbusd_epi32(&mut acc[k], in_val, _mm_load_si128(col.add(k)));
                        }
                    }

                    // 結果を出力
                    let out_ptr = output.as_mut_ptr() as *mut __m128i;
                    for k in 0..num_regs {
                        _mm_storeu_si128(out_ptr.add(k), acc[k]);
                    }
                    return;
                }

                // OUTPUT_DIM % 4 != 0 の場合: SSSE3の_mm_maddubs_epi16を使う通常版
                let num_chunks = Self::PADDED_INPUT / 16;
                let one = _mm_set1_epi16(1);
                let input_ptr = input.as_ptr();
                let weights_ptr = self.weights.as_ptr();

                for (j, (out, &bias)) in output.iter_mut().zip(&self.biases).enumerate() {
                    let mut acc = _mm_setzero_si128();
                    let weight_row_offset = j * Self::PADDED_INPUT;

                    for k in 0..num_chunks {
                        let offset = k * 16;
                        let in_vec = _mm_load_si128(input_ptr.add(offset) as *const __m128i);
                        let w_vec = _mm_load_si128(
                            weights_ptr.add(weight_row_offset + offset) as *const __m128i
                        );
                        // SSSE3: _mm_maddubs_epi16
                        let prod16 = _mm_maddubs_epi16(in_vec, w_vec);
                        let prod32 = _mm_madd_epi16(prod16, one);
                        acc = _mm_add_epi32(acc, prod32);
                    }

                    *out = bias + hsum_i32_sse2(acc);
                }
            }
            return;
        }

        // SSE2: 128bit = 16 x u8/i8 (SSSE3非対応環境のフォールバック)
        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "sse2",
            not(target_feature = "ssse3")
        ))]
        {
            // SAFETY:
            // - input.len() >= PADDED_INPUT (debug_assert で検証済み)
            // - weights.len() >= OUTPUT_DIM * PADDED_INPUT (構造上保証)
            // - input は Aligned<[u8; N]> で64バイトアライン（16バイト境界も満たす）
            // - weights は AlignedBox<i8> で64バイトアライン
            // - PADDED_INPUT は32の倍数なのでオフセットは常に16バイト境界
            unsafe {
                use std::arch::x86_64::*;

                let num_chunks = Self::PADDED_INPUT / 16;

                // 定数をループ外でホイスト
                let one = _mm_set1_epi16(1);
                let zero = _mm_setzero_si128();

                // ポインタを事前に取得（境界チェック排除）
                let input_ptr = input.as_ptr();
                let weights_ptr = self.weights.as_ptr();

                for (j, (out, &bias)) in output.iter_mut().zip(&self.biases).enumerate() {
                    let mut acc = _mm_setzero_si128();
                    let weight_row_offset = j * Self::PADDED_INPUT;

                    // 入力を16バイトずつ処理
                    for k in 0..num_chunks {
                        let offset = k * 16;
                        let in_vec = _mm_load_si128(input_ptr.add(offset) as *const __m128i);
                        let w_vec = _mm_load_si128(
                            weights_ptr.add(weight_row_offset + offset) as *const __m128i
                        );

                        // SSE2にはmaddubs_epi16がないので、手動で実装
                        // u8をi16にゼロ拡張
                        let in_lo = _mm_unpacklo_epi8(in_vec, zero);
                        let in_hi = _mm_unpackhi_epi8(in_vec, zero);
                        // i8をi16に符号拡張（cmpgtで符号ビットマスクを生成）
                        let sign = _mm_cmpgt_epi8(zero, w_vec);
                        let w_lo = _mm_unpacklo_epi8(w_vec, sign);
                        let w_hi = _mm_unpackhi_epi8(w_vec, sign);

                        // i16乗算
                        let prod_lo = _mm_mullo_epi16(in_lo, w_lo);
                        let prod_hi = _mm_mullo_epi16(in_hi, w_hi);

                        // i16 → i32 にワイドニング加算
                        let sum32_lo = _mm_madd_epi16(prod_lo, one);
                        let sum32_hi = _mm_madd_epi16(prod_hi, one);

                        acc = _mm_add_epi32(acc, sum32_lo);
                        acc = _mm_add_epi32(acc, sum32_hi);
                    }

                    // 水平加算してバイアスを加える
                    *out = bias + hsum_i32_sse2(acc);
                }
            }
            return;
        }

        // WASM SIMD128
        #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
        {
            // SAFETY:
            // - input.len() >= PADDED_INPUT (debug_assert で検証済み)
            // - weights.len() >= OUTPUT_DIM * PADDED_INPUT (構造上保証)
            // - WASM SIMD128 はアライメント不要（v128_load/v128_store は任意のアドレスで動作）
            // - biases/output は4出力ずつ（j += 4）でアクセスし、i32配列なので
            //   オフセットは 4 * sizeof(i32) = 16バイトの倍数となり16バイト境界
            unsafe {
                use std::arch::wasm32::*;

                let num_chunks = Self::PADDED_INPUT / 16;

                // ポインタを事前に取得（境界チェック排除）
                let input_ptr = input.as_ptr();
                let weights_ptr = self.weights.as_ptr();

                // 4出力同時処理: 入力ロードを再利用（YaneuraOu dot4方式）
                if OUTPUT_DIM.is_multiple_of(4) && OUTPUT_DIM > 0 {
                    let mut j = 0;
                    while j < OUTPUT_DIM {
                        let mut acc0 = i32x4_splat(0);
                        let mut acc1 = i32x4_splat(0);
                        let mut acc2 = i32x4_splat(0);
                        let mut acc3 = i32x4_splat(0);

                        let row0 = weights_ptr.add((j + 0) * Self::PADDED_INPUT);
                        let row1 = weights_ptr.add((j + 1) * Self::PADDED_INPUT);
                        let row2 = weights_ptr.add((j + 2) * Self::PADDED_INPUT);
                        let row3 = weights_ptr.add((j + 3) * Self::PADDED_INPUT);

                        // 入力を16バイトずつ処理
                        for k in 0..num_chunks {
                            let offset = k * 16;
                            let in_vec = v128_load(input_ptr.add(offset) as *const v128);
                            let in_lo = i16x8_extend_low_u8x16(in_vec);
                            let in_hi = i16x8_extend_high_u8x16(in_vec);

                            let w0 = v128_load(row0.add(offset) as *const v128);
                            let w1 = v128_load(row1.add(offset) as *const v128);
                            let w2 = v128_load(row2.add(offset) as *const v128);
                            let w3 = v128_load(row3.add(offset) as *const v128);

                            acc0 = i32x4_add(acc0, dot_i8x16_u8i8_preexpanded(in_lo, in_hi, w0));
                            acc1 = i32x4_add(acc1, dot_i8x16_u8i8_preexpanded(in_lo, in_hi, w1));
                            acc2 = i32x4_add(acc2, dot_i8x16_u8i8_preexpanded(in_lo, in_hi, w2));
                            acc3 = i32x4_add(acc3, dot_i8x16_u8i8_preexpanded(in_lo, in_hi, w3));
                        }

                        let sum_vec = haddx4(acc0, acc1, acc2, acc3);
                        let bias_vec = v128_load(self.biases.as_ptr().add(j) as *const v128);
                        let out_vec = i32x4_add(bias_vec, sum_vec);
                        v128_store(output.as_mut_ptr().add(j) as *mut v128, out_vec);
                        j += 4;
                    }
                    return;
                }

                for (j, (out, &bias)) in output.iter_mut().zip(&self.biases).enumerate() {
                    let mut acc = i32x4_splat(0);
                    let weight_row_offset = j * Self::PADDED_INPUT;

                    // 入力を16バイトずつ処理
                    for k in 0..num_chunks {
                        let offset = k * 16;
                        let in_vec = v128_load(input_ptr.add(offset) as *const v128);
                        let w_vec =
                            v128_load(weights_ptr.add(weight_row_offset + offset) as *const v128);

                        acc = i32x4_add(acc, dot_i8x16_u8i8(in_vec, w_vec));
                    }

                    // 水平加算
                    let sum = hsum_i32x4(acc);

                    *out = bias + sum;
                }
            }
            return;
        }

        // スカラーフォールバック
        #[allow(unreachable_code)]
        {
            // バイアスで初期化
            output.copy_from_slice(&self.biases);

            // 行列×ベクトル（密な計算）
            for (i, &in_byte) in input.iter().enumerate().take(INPUT_DIM) {
                let in_val = in_byte as i32;
                for (j, out) in output.iter_mut().enumerate() {
                    let weight_idx = j * Self::PADDED_INPUT + i;
                    *out += self.weights[weight_idx] as i32 * in_val;
                }
            }
        }
    }
}

/// ClippedReLU層（静的サイズ版）
/// 入力: i32、出力: u8（0-127にクランプ）
///
/// SIMD最適化版（AVX2/SSE2/WASM対応）
/// tanuki-/YaneuraOu の clipped_relu.h を参考にフォールスルー構造で実装。
///
/// # パフォーマンス特性
///
/// 小さい次元（DIM=8, 32, 96など）ではSIMDセットアップオーバーヘッドが
/// 相対的に大きく、スカラー版との差は約1-2%程度。
/// ClippedReLUは計算全体に占める割合が小さいため、全体への影響は限定的。
///
/// ## ベンチマーク結果 (AMD Ryzen 9 5950X)
///
/// ### ClippedReLU SIMD効果（HalfKP 256x2-32-32, DIM=32）
/// - スカラー版: ~667 kNPS
/// - SIMD版: ~673 kNPS (~1%改善)
///
/// ### NNUEアーキテクチャ別NPS比較
/// | アーキテクチャ | L1 | NPS | 備考 |
/// |---------------|-----|-----|------|
/// | HalfKP 256x2-32-32 | 256 | ~703 kNPS | 本構造体を使用 |
/// | HalfKA_hm 512x2-8-96 | 512 | ~512 kNPS | 動的版使用 |
/// | HalfKA_hm 1024x2-8-96 | 1024 | ~406 kNPS | 動的版使用 |
pub struct ClippedReLU<const DIM: usize>;

impl<const DIM: usize> ClippedReLU<DIM> {
    /// 順伝播
    ///
    /// AVX2/SSE2/WASMのSIMD最適化版。
    /// i32入力を右シフトし、0-127にクランプしてu8に変換。
    ///
    /// フォールスルー構造:
    /// 1. AVX2で32要素ずつ処理
    /// 2. 残りをSSE2で16要素ずつ処理
    /// 3. 残りをSSE2で8要素ずつ処理（DIM=8対応）
    /// 4. 残りをスカラーで処理
    pub fn propagate(input: &[i32; DIM], output: &mut [u8; DIM]) {
        let mut processed: usize = 0;

        // === AVX2: 32要素ずつ処理 ===
        #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
        {
            let num_chunks = DIM / 32;
            if num_chunks > 0 {
                // SAFETY:
                // - num_chunks > 0 を確認済み
                // - loadu/storeu を使用するためアライメント不要
                unsafe {
                    use std::arch::x86_64::*;

                    let zero = _mm256_setzero_si256();
                    let offsets = _mm256_set_epi32(7, 3, 6, 2, 5, 1, 4, 0);

                    let in_ptr = input.as_ptr() as *const __m256i;
                    let out_ptr = output.as_mut_ptr() as *mut __m256i;

                    for i in 0..num_chunks {
                        let in0 = _mm256_loadu_si256(in_ptr.add(i * 4));
                        let in1 = _mm256_loadu_si256(in_ptr.add(i * 4 + 1));
                        let in2 = _mm256_loadu_si256(in_ptr.add(i * 4 + 2));
                        let in3 = _mm256_loadu_si256(in_ptr.add(i * 4 + 3));

                        let words0 = _mm256_srai_epi16(
                            _mm256_packs_epi32(in0, in1),
                            WEIGHT_SCALE_BITS as i32,
                        );
                        let words1 = _mm256_srai_epi16(
                            _mm256_packs_epi32(in2, in3),
                            WEIGHT_SCALE_BITS as i32,
                        );

                        let bytes = _mm256_max_epi8(_mm256_packs_epi16(words0, words1), zero);
                        let result = _mm256_permutevar8x32_epi32(bytes, offsets);

                        _mm256_storeu_si256(out_ptr.add(i), result);
                    }
                }
                processed = num_chunks * 32;
            }
        }

        // === SSE2: 16要素ずつ処理（残り部分） ===
        #[cfg(all(target_arch = "x86_64", target_feature = "sse2"))]
        {
            let remaining = DIM - processed;
            let num_chunks = remaining / 16;
            if num_chunks > 0 {
                // SAFETY: 同上
                unsafe {
                    use std::arch::x86_64::*;

                    #[cfg(target_feature = "sse4.1")]
                    let zero = _mm_setzero_si128();
                    #[cfg(not(target_feature = "sse4.1"))]
                    let k0x80s = _mm_set1_epi8(-128i8);

                    let in_ptr = input.as_ptr().add(processed) as *const __m128i;
                    let out_ptr = output.as_mut_ptr().add(processed) as *mut __m128i;

                    for i in 0..num_chunks {
                        let in0 = _mm_loadu_si128(in_ptr.add(i * 4));
                        let in1 = _mm_loadu_si128(in_ptr.add(i * 4 + 1));
                        let in2 = _mm_loadu_si128(in_ptr.add(i * 4 + 2));
                        let in3 = _mm_loadu_si128(in_ptr.add(i * 4 + 3));

                        let words0 =
                            _mm_srai_epi16(_mm_packs_epi32(in0, in1), WEIGHT_SCALE_BITS as i32);
                        let words1 =
                            _mm_srai_epi16(_mm_packs_epi32(in2, in3), WEIGHT_SCALE_BITS as i32);

                        let packedbytes = _mm_packs_epi16(words0, words1);

                        #[cfg(target_feature = "sse4.1")]
                        let result = _mm_max_epi8(packedbytes, zero);
                        #[cfg(not(target_feature = "sse4.1"))]
                        let result = _mm_subs_epi8(_mm_adds_epi8(packedbytes, k0x80s), k0x80s);

                        _mm_storeu_si128(out_ptr.add(i), result);
                    }
                }
                processed += num_chunks * 16;
            }
        }

        // === SSE2: 8要素処理（DIM=8対応） ===
        #[cfg(all(target_arch = "x86_64", target_feature = "sse2"))]
        {
            let remaining = DIM - processed;
            if remaining >= 8 {
                // SAFETY: 同上
                // 8個のi32を2つの__m128iで読み込み、1つの__m128iの下位8バイトに出力
                unsafe {
                    use std::arch::x86_64::*;

                    #[cfg(target_feature = "sse4.1")]
                    let zero = _mm_setzero_si128();
                    #[cfg(not(target_feature = "sse4.1"))]
                    let k0x80s = _mm_set1_epi8(-128i8);

                    let in_ptr = input.as_ptr().add(processed) as *const __m128i;
                    let out_ptr = output.as_mut_ptr().add(processed);

                    // 8個のi32を読み込み（2つの__m128i）
                    let in0 = _mm_loadu_si128(in_ptr);
                    let in1 = _mm_loadu_si128(in_ptr.add(1));

                    // i32 → i16 にパック（8要素）
                    let words = _mm_packs_epi32(in0, in1);
                    // 右シフト
                    let shifted = _mm_srai_epi16(words, WEIGHT_SCALE_BITS as i32);
                    // i16 → i8 にパック（下位8バイトが有効）
                    let packedbytes = _mm_packs_epi16(shifted, shifted);

                    // max(0, x)
                    #[cfg(target_feature = "sse4.1")]
                    let result = _mm_max_epi8(packedbytes, zero);
                    #[cfg(not(target_feature = "sse4.1"))]
                    let result = _mm_subs_epi8(_mm_adds_epi8(packedbytes, k0x80s), k0x80s);

                    // 下位8バイトのみ書き出し
                    _mm_storel_epi64(out_ptr as *mut __m128i, result);
                }
                processed += 8;
            }
        }

        // === WASM SIMD128: 8要素ずつ処理 ===
        #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
        {
            let num_chunks = (DIM - processed) / 8;
            if num_chunks > 0 {
                // SAFETY: 同上
                unsafe {
                    use std::arch::wasm32::*;

                    let zero = i8x16_splat(0);
                    let in_ptr = input.as_ptr().add(processed) as *const v128;
                    let out_ptr = output.as_mut_ptr().add(processed) as *mut i64;

                    for i in 0..num_chunks {
                        let in0 = v128_load(in_ptr.add(i * 2));
                        let in1 = v128_load(in_ptr.add(i * 2 + 1));

                        let shifted0 = i32x4_shr(in0, WEIGHT_SCALE_BITS as u32);
                        let shifted1 = i32x4_shr(in1, WEIGHT_SCALE_BITS as u32);
                        let words = i16x8_narrow_i32x4(shifted0, shifted1);

                        let bytes = i8x16_narrow_i16x8(words, words);
                        let result = i8x16_max(bytes, zero);

                        *out_ptr.add(i) = i64x2_extract_lane::<0>(result);
                    }
                }
                processed += num_chunks * 8;
            }
        }

        // === スカラーフォールバック（残り要素） ===
        for i in processed..DIM {
            let shifted = input[i] >> WEIGHT_SCALE_BITS;
            output[i] = shifted.clamp(0, 127) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nnue::accumulator::Aligned;

    #[test]
    fn test_affine_transform_propagate() {
        // 小さいテスト用の変換
        // PADDED_INPUT = padded_input(4) = 32 なので、入力も32バイト必要
        let mut weights = AlignedBox::new_zeroed(64); // 2行 × 32バイト
        weights[0] = 1;
        weights[1] = 2; // 行0: [1, 2, 0, ...]
        weights[32] = 3;
        weights[33] = 4; // 行1: [3, 4, 0, ...]

        let transform: AffineTransform<4, 2> = AffineTransform {
            biases: [10, 20],
            weights,
        };

        // 入力はPADDED_INPUT（32バイト）にパディングする必要がある
        // SIMD実装は32バイト単位で処理するため、64バイトアライン必須
        let mut input = Aligned([0u8; 32]);
        input.0[0] = 1;
        input.0[1] = 2;
        let mut output = [0i32; 2];

        transform.propagate(&input.0, &mut output);

        // output[0] = 10 + 1*1 + 2*2 = 15
        // output[1] = 20 + 1*3 + 2*4 = 31
        assert_eq!(output[0], 15);
        assert_eq!(output[1], 31);
    }

    #[test]
    fn test_clipped_relu() {
        let input = [0i32, 64, 128, -64, 256];
        let mut output = [0u8; 5];

        // WEIGHT_SCALE_BITS = 6 なので、64 >> 6 = 1, 128 >> 6 = 2, etc.
        ClippedReLU::propagate(&input, &mut output);

        assert_eq!(output[0], 0); // 0 >> 6 = 0
        assert_eq!(output[1], 1); // 64 >> 6 = 1
        assert_eq!(output[2], 2); // 128 >> 6 = 2
        assert_eq!(output[3], 0); // -64 >> 6 = -1, clamped to 0
        assert_eq!(output[4], 4); // 256 >> 6 = 4
    }

    #[test]
    fn test_affine_transform_real_size() {
        // 実際の使用サイズ（512入力→32出力）に近いテスト
        // PADDED_INPUT = padded_input(512) = 512
        let mut weights = AlignedBox::new_zeroed(32 * 512);

        // 対角成分を1に設定（出力iに入力iが1:1で対応）
        // スクランブル形式が有効な場合は変換して設定
        for i in 0..32 {
            let raw_idx = i * 512 + i; // 元のインデックス: weights[output][input]
            #[cfg(any(
                all(target_arch = "x86_64", target_feature = "avx2"),
                all(
                    target_arch = "x86_64",
                    target_feature = "ssse3",
                    not(target_feature = "avx2")
                )
            ))]
            let idx = if AffineTransform::<512, 32>::should_use_scrambled_weights() {
                AffineTransform::<512, 32>::get_weight_index_scrambled(raw_idx)
            } else {
                raw_idx
            };
            #[cfg(not(any(
                all(target_arch = "x86_64", target_feature = "avx2"),
                all(
                    target_arch = "x86_64",
                    target_feature = "ssse3",
                    not(target_feature = "avx2")
                )
            )))]
            let idx = raw_idx;
            weights[idx] = 1;
        }

        let transform: AffineTransform<512, 32> = AffineTransform {
            biases: [10; 32],
            weights,
        };

        // 入力は64バイトアライン必須
        let mut input = Aligned([0u8; 512]);
        for (i, val) in input.0.iter_mut().take(32).enumerate() {
            *val = (i + 1) as u8; // 1, 2, 3, ..., 32
        }
        let mut output = [0i32; 32];

        transform.propagate(&input.0, &mut output);

        // output[i] = 10 + input[i] * 1 = 10 + (i+1)
        for (i, &val) in output.iter().enumerate() {
            assert_eq!(val, 10 + (i + 1) as i32, "mismatch at index {i}");
        }
    }
}
