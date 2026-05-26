//! FeatureTransformerLayerStacks - LayerStacksアーキテクチャ用のL1次元Feature Transformer
//!
//! 5 種類の FT (HalfKp / HalfKaSplit / HalfKaMerged / HalfKaHmSplit / HalfKaHmMerged)
//! から、片側 L1 次元×両視点の中間表現を生成する。
//! FT 軸は `LsFeatureSpec` trait + `PhantomData<FT>` で type level に表現し、
//! monomorphization で既存 HalfKaHmMerged 専用実装と bit-identical な機械語を得る。

use super::accumulator::{Aligned, AlignedBox};
use super::accumulator::{DirtyPiece, IndexList, MAX_ACTIVE_FEATURES, MAX_CHANGED_FEATURES};
use super::accumulator_layer_stacks::{
    AccumulatorCacheLayerStacks, AccumulatorLayerStacks, AccumulatorStackLayerStacks,
};
use super::bona_piece::BonaPiece;
#[cfg(feature = "ls-ext-psqt")]
use super::constants::MAX_LAYER_STACK_BUCKETS;
use super::features::{Feature, FeatureSet};
use super::leb128::read_compressed_tensor_i16_all;
use super::ls_feature_spec::LsFeatureSpec;
use super::piece_list::PieceNumber;
use super::stats::{count_refresh, count_update};
#[cfg(feature = "ls-ext-threat")]
use super::threat_features::{self, MAX_CHANGED_THREAT_FEATURES, THREAT_DIMENSIONS};
use crate::position::Position;
use crate::types::Color;
use std::io::{self, Read};
use std::marker::PhantomData;

/// 特徴インデックスの範囲外アクセス時のパニック
#[cold]
#[inline(never)]
fn feature_index_oob(index: usize, max: usize) -> ! {
    panic!("Feature index out of range: {index} (max: {max})")
}

#[inline]
fn append_changed_indices<FT: LsFeatureSpec>(
    dirty_piece: &DirtyPiece,
    perspective: Color,
    king_sq: crate::types::Square,
    removed: &mut IndexList<MAX_CHANGED_FEATURES>,
    added: &mut IndexList<MAX_CHANGED_FEATURES>,
) {
    <FT::Feature as Feature>::append_changed_indices(
        dirty_piece,
        perspective,
        king_sq,
        removed,
        added,
    );
}

#[inline]
fn append_active_indices<FT: LsFeatureSpec>(
    pos: &Position,
    perspective: Color,
    active: &mut IndexList<MAX_ACTIVE_FEATURES>,
) {
    <FT::Feature as Feature>::append_active_indices(pos, perspective, active);
}

#[inline]
fn feature_index_from_bona_piece<FT: LsFeatureSpec>(
    bp: BonaPiece,
    perspective: Color,
    king_sq: crate::types::Square,
) -> usize {
    FT::feature_index(bp, perspective, king_sq)
}

/// nnue-pytorch用のFeatureTransformer（L1次元出力）
///
/// `FT` は LS の Feature Transformer 軸 (5 種類のうち 1 つ) を表す marker type。
#[repr(C, align(64))]
pub struct FeatureTransformerLayerStacks<const L1: usize, FT: LsFeatureSpec> {
    /// バイアス [L1]
    pub biases: Aligned<[i16; L1]>,

    /// 重み [FT::DIMENSIONS][L1]
    /// 64バイトアラインメントで確保
    pub weights: AlignedBox<i16>,

    /// PSQT バイアス（先頭 `num_buckets` 個のみ有効、それ以降はゼロ）
    ///
    /// 配列のサイズは hot-path 固定 (`MAX_LAYER_STACK_BUCKETS`) だが、有効範囲は
    /// `num_buckets` で動的に決まる。未使用エリアは load 時に 0 で初期化され、
    /// `psqt_add_or_sub` が `n` 引数で範囲を絞るため副作用無し。
    #[cfg(feature = "ls-ext-psqt")]
    pub(crate) psqt_biases: [i32; MAX_LAYER_STACK_BUCKETS],

    /// PSQT 重み (長さ = `FT::DIMENSIONS × num_buckets`、layout
    /// `psqt_weights[feature_idx * num_buckets + bucket]`)
    #[cfg(feature = "ls-ext-psqt")]
    pub(crate) psqt_weights: AlignedBox<i32>,

    /// PSQT 重みの bucket 数 (= net file の `num_buckets`)。
    ///
    /// `has_psqt == false` のとき `0`。このとき `add/sub_psqt_weights` は
    /// `has_psqt` ガード (`refresh_psqt` / 各 caller) により呼ばれないため、
    /// `0` が伝播して `index * 0 = 0` で誤動作する経路は無い。
    #[cfg(feature = "ls-ext-psqt")]
    pub(crate) psqt_num_buckets: usize,

    /// PSQT が有効か（アーキテクチャ文字列で判定）
    #[cfg(feature = "ls-ext-psqt")]
    pub(crate) has_psqt: bool,

    /// Threat 重み [THREAT_DIMENSIONS × L1]
    #[cfg(feature = "ls-ext-threat")]
    pub(crate) threat_weights: AlignedBox<i8>,

    /// Threat が有効か（アーキテクチャ文字列で判定）
    #[cfg(feature = "ls-ext-threat")]
    pub(crate) has_threat: bool,

    _ft: PhantomData<FT>,
}

/// PSQT アキュムレータ (`[i32; MAX_LAYER_STACK_BUCKETS]`) の先頭 `n` 要素に
/// `weights[0..n]` を加算 (`ADD = true`) または減算する。
///
/// `n` は net file の `num_buckets` で、`MAX_LAYER_STACK_BUCKETS = 16` 以下。
/// 配列の `[n..MAX]` 範囲は load 時にゼロのまま残し、本関数はここを触らない
/// (未使用 bucket の値を保存)。
///
/// `wrapping_add` / `wrapping_sub` で明示 wrap する。PSQT 値は ±数千オーダー、
/// FT 累積でも i32 を溢れないため挙動差は無視できる。
///
/// # 実装方針
///
/// `MAX_LAYER_STACK_BUCKETS = 16` が AVX-512 1 命令のレーン数と一致するため、
/// AVX-512F では runtime `n` から `(1 << n) - 1` の 16-bit mask を作り
/// `_mm512_maskz_loadu_epi32` + `_mm512_mask_storeu_epi32` の 1 セットで完了する。
/// AVX2 (AVX-512 無し) では `_mm256_maskload_epi32` / `_mm256_maskstore_epi32`
/// で 8 lane ごとに mask する (n ≤ 16 のため最大 2 chunk)。SSE2 / NEON /
/// WASM SIMD128 が無い build は scalar fallback。
///
/// # Safety
/// `weights` は少なくとも `n` 個の i32 が連続して読める必要がある（呼び出し元が保証）。
#[cfg(feature = "ls-ext-psqt")]
#[inline(always)]
fn psqt_add_or_sub<const ADD: bool>(
    psqt_acc: &mut [i32; MAX_LAYER_STACK_BUCKETS],
    weights: *const i32,
    n: usize,
) {
    debug_assert!(n <= MAX_LAYER_STACK_BUCKETS);

    // AVX-512F: 16-lane mask で 1 命令にまとめる
    #[cfg(all(target_arch = "x86_64", target_feature = "avx512f"))]
    {
        // SAFETY:
        // - `psqt_acc` は `[i32; 16]` = 64 bytes 連続 (AVX-512 1 vector と一致)。
        //   全 16 レーンが有効領域内。
        // - `weights` は呼び出し元が **n 個 i32 連続を読める** ことを保証する。
        //   ポインタ自体は n 個分の領域を指し、`weights.add(k)` for `k < n` は
        //   in-bounds。
        // - mask `(1 << n) - 1` は下位 n bit のみ立つ。`_mm512_maskz_loadu_epi32`
        //   と `_mm512_mask_storeu_epi32` は **mask bit が 0 のレーンに対応する
        //   メモリアドレスへの load/store を発行しない** (Intel SDM Vol. 2C:
        //   VMOVDQU32 with `{k1}{z}` — "Masked-out elements are zeroed. No fault
        //   is signaled for masked-out elements regardless of whether the
        //   corresponding memory operand would have caused a fault")。よって
        //   `weights[n..16]` が割り当て外でも安全。
        unsafe {
            use std::arch::x86_64::*;
            const _ASSERT: () = assert!(MAX_LAYER_STACK_BUCKETS == 16);
            let mask: __mmask16 = if n >= 16 {
                !0u16
            } else {
                ((1u32 << n) - 1) as u16
            };
            let acc_ptr = psqt_acc.as_mut_ptr();
            let a = _mm512_maskz_loadu_epi32(mask, acc_ptr);
            let w = _mm512_maskz_loadu_epi32(mask, weights);
            let result = if ADD {
                _mm512_add_epi32(a, w)
            } else {
                _mm512_sub_epi32(a, w)
            };
            _mm512_mask_storeu_epi32(acc_ptr, mask, result);
        }
    }

    // AVX2 (AVX-512 無し): 8-lane × 最大 2 chunk を runtime mask で処理
    #[cfg(all(
        target_arch = "x86_64",
        target_feature = "avx2",
        not(target_feature = "avx512f")
    ))]
    {
        // SAFETY:
        // - `psqt_acc` は `[i32; 16]` = 64 bytes 連続、`acc_ptr.add(0)` と
        //   `acc_ptr.add(8)` の両方が in-bounds (offset 32 / 64 bytes、配列終端は
        //   64 bytes)。
        // - `weights` は呼び出し元が **n 個 i32 連続を読める** ことを保証する。
        //   chunk loop は `covered += 8` で進むため、`weights.add(covered)` は
        //   `covered < n` の chunk 内では下位レーン (`indices[0]`) が必ず in-bounds。
        // - `_mm256_maskload_epi32` / `_mm256_maskstore_epi32` (VPMASKMOVD) は
        //   **mask 最上位 bit が 0 のレーンに対応するメモリアドレスへの load/store
        //   を発行しない** (Intel SDM Vol. 2B: VPMASKMOVD — "If the mask is 0,
        //   the corresponding memory location is not accessed and no fault is
        //   signaled")。よって `weights[k]` (mask = 0 のレーン位置) が割り当て外で
        //   あっても安全。
        // - mask は `_mm256_cmpgt_epi32(remaining_broadcast, indices)` で下位
        //   `remaining` 個のレーンのみ all-ones、それ以上は all-zeros として生成。
        unsafe {
            use std::arch::x86_64::*;
            let acc_ptr = psqt_acc.as_mut_ptr();
            let indices = _mm256_setr_epi32(0, 1, 2, 3, 4, 5, 6, 7);
            let mut covered = 0usize;
            while covered < n {
                let remaining = (n - covered).min(8) as i32;
                let r = _mm256_set1_epi32(remaining);
                let mask = _mm256_cmpgt_epi32(r, indices);
                let a = _mm256_maskload_epi32(acc_ptr.add(covered), mask);
                let w = _mm256_maskload_epi32(weights.add(covered), mask);
                let result = if ADD {
                    _mm256_add_epi32(a, w)
                } else {
                    _mm256_sub_epi32(a, w)
                };
                _mm256_maskstore_epi32(acc_ptr.add(covered), mask, result);
                covered += 8;
            }
        }
    }

    // 上記のいずれの SIMD path にも該当しない build (SSE2 のみ / NEON / WASM /
    // その他) は scalar fallback。`MAX = 16` で n ≤ 16 のループは LLVM が
    // auto-vectorize する余地がある。
    #[cfg(not(any(
        all(target_arch = "x86_64", target_feature = "avx512f"),
        all(target_arch = "x86_64", target_feature = "avx2"),
    )))]
    {
        // SAFETY: 呼び出し元は `weights` が n 個 i32 連続して読めることを保証する。
        let w_slice: &[i32] = unsafe { std::slice::from_raw_parts(weights, n) };
        for (acc, &w) in psqt_acc[..n].iter_mut().zip(w_slice) {
            if ADD {
                *acc = acc.wrapping_add(w);
            } else {
                *acc = acc.wrapping_sub(w);
            }
        }
    }
}

impl<const L1: usize, FT: LsFeatureSpec> FeatureTransformerLayerStacks<L1, FT> {
    /// 重み配列をプロセス間共有メモリへ移行する（成功時のみ）。
    ///
    /// 多プロセス実行時のメモリ常駐・L3 競合を削減する。ネットワーク構築が完全に
    /// 終わった後（重みへの全書込が済んだ後）に 1 回だけ呼ぶこと。共有後の重み box は
    /// read-only になる。空 box（PSQT/Threat 無効モデル）は内部でスキップされる。
    pub(crate) fn share_weights(&mut self) {
        super::shared_weights::try_share(&mut self.weights, "FT weights");
        #[cfg(feature = "ls-ext-psqt")]
        super::shared_weights::try_share(&mut self.psqt_weights, "FT psqt");
        #[cfg(feature = "ls-ext-threat")]
        super::shared_weights::try_share(&mut self.threat_weights, "FT threat");
    }

    /// ファイルから読み込み（非圧縮形式）
    pub fn read<R: Read>(reader: &mut R) -> io::Result<Self> {
        // バイアスを読み込み
        let mut biases = [0i16; L1];
        let mut buf = [0u8; 2];
        for bias in biases.iter_mut() {
            reader.read_exact(&mut buf)?;
            *bias = i16::from_le_bytes(buf);
        }

        // 重みを読み込み
        let weight_size = FT::DIMENSIONS * L1;
        let mut weights = AlignedBox::new_zeroed(weight_size);
        for weight in weights.iter_mut() {
            reader.read_exact(&mut buf)?;
            *weight = i16::from_le_bytes(buf);
        }

        Ok(Self {
            biases: Aligned(biases),
            weights,
            #[cfg(feature = "ls-ext-psqt")]
            psqt_biases: [0; MAX_LAYER_STACK_BUCKETS],
            #[cfg(feature = "ls-ext-psqt")]
            psqt_num_buckets: 0,
            #[cfg(feature = "ls-ext-psqt")]
            psqt_weights: AlignedBox::new_zeroed(0),
            #[cfg(feature = "ls-ext-psqt")]
            has_psqt: false,
            #[cfg(feature = "ls-ext-threat")]
            threat_weights: AlignedBox::new_zeroed(0),
            #[cfg(feature = "ls-ext-threat")]
            has_threat: false,
            _ft: PhantomData,
        })
    }

    /// LEB128圧縮形式から読み込み（自動検出）
    ///
    /// 最初のブロックを全デコードし、要素数で形式を判別する:
    /// - 要素数 == biases のみ → YO形式（2ブロック）: 続けて weights ブロックを読む
    /// - 要素数 == biases + weights → 旧bullet-shogi形式（1ブロック）
    pub fn read_leb128<R: Read>(reader: &mut R) -> io::Result<Self> {
        let weight_size = FT::DIMENSIONS * L1;
        let total_size = L1 + weight_size;

        // 最初のブロックを全値デコードして要素数で判別
        let first_block = read_compressed_tensor_i16_all(reader)?;

        if first_block.len() == total_size {
            // 旧bullet-shogi形式（1ブロック）: biases + weights が結合
            let mut biases = [0i16; L1];
            biases.copy_from_slice(&first_block[..L1]);

            let mut weights = AlignedBox::new_zeroed(weight_size);
            weights.copy_from_slice(&first_block[L1..]);

            return Ok(Self {
                biases: Aligned(biases),
                weights,
                #[cfg(feature = "ls-ext-psqt")]
                psqt_biases: [0; MAX_LAYER_STACK_BUCKETS],
                #[cfg(feature = "ls-ext-psqt")]
                psqt_num_buckets: 0,
                #[cfg(feature = "ls-ext-psqt")]
                psqt_weights: AlignedBox::new_zeroed(0),
                #[cfg(feature = "ls-ext-psqt")]
                has_psqt: false,
                #[cfg(feature = "ls-ext-threat")]
                threat_weights: AlignedBox::new_zeroed(0),
                #[cfg(feature = "ls-ext-threat")]
                has_threat: false,
                _ft: PhantomData,
            });
        }

        if first_block.len() == L1 {
            // YO形式（2ブロック）: 次に weights ブロックを読み込み
            let weights_block = read_compressed_tensor_i16_all(reader)?;
            if weights_block.len() != weight_size {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "FT weights block size mismatch: got {}, expected {}",
                        weights_block.len(),
                        weight_size
                    ),
                ));
            }

            let mut biases = [0i16; L1];
            biases.copy_from_slice(&first_block);

            let mut weights = AlignedBox::new_zeroed(weight_size);
            weights.copy_from_slice(&weights_block);

            return Ok(Self {
                biases: Aligned(biases),
                weights,
                #[cfg(feature = "ls-ext-psqt")]
                psqt_biases: [0; MAX_LAYER_STACK_BUCKETS],
                #[cfg(feature = "ls-ext-psqt")]
                psqt_num_buckets: 0,
                #[cfg(feature = "ls-ext-psqt")]
                psqt_weights: AlignedBox::new_zeroed(0),
                #[cfg(feature = "ls-ext-psqt")]
                has_psqt: false,
                #[cfg(feature = "ls-ext-threat")]
                threat_weights: AlignedBox::new_zeroed(0),
                #[cfg(feature = "ls-ext-threat")]
                has_threat: false,
                _ft: PhantomData,
            });
        }

        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "Unexpected LEB128 tensor size: got {}, expected {} or {}",
                first_block.len(),
                L1,
                total_size
            ),
        ))
    }

    /// PSQT 重み/バイアスをファイルから読み込み
    ///
    /// `num_buckets` は net file の `num_buckets` field (legacy `.bin` は
    /// `DEFAULT_NUM_BUCKETS = 9`)。`MAX_LAYER_STACK_BUCKETS` を超える値は
    /// 呼び出し元で reject されている前提だが、debug_assert で再確認する。
    ///
    /// ## 不変条件: `psqt_biases[num_buckets..MAX]` は常にゼロ
    ///
    /// `psqt_biases` は固定長 `[i32; MAX_LAYER_STACK_BUCKETS]` だが、有効範囲は
    /// 先頭 `num_buckets` 要素のみ。本関数は冒頭で `[0; MAX]` で zero 化してから
    /// 先頭 N 要素を file から読む。`[N..MAX]` 領域は不変条件としてゼロを保つ:
    ///
    /// - `read_psqt` は 1 回のみ呼ばれる (`network_layer_stacks::read_with_options`
    ///   が PSQT block ごとに 1 回 dispatch する構造)
    /// - 評価パスの `psqt_add_or_sub` は引数 `n` で `[..n]` のみ操作し
    ///   `[n..MAX]` を触らない
    /// - `refresh_or_cache_with_psqt` の `*psqt_acc = *psqt_biases` は
    ///   `[i32; MAX]` の Copy で `[N..MAX]` のゼロ部分も伝播する
    ///   (副作用なしで PSQT acc の `[N..MAX]` も 0 のままになる)
    ///
    /// 上記により評価時に `[N..MAX]` 要素が undefined / non-zero 値で読まれることが無い。
    #[cfg(feature = "ls-ext-psqt")]
    pub fn read_psqt<R: Read>(&mut self, reader: &mut R, num_buckets: usize) -> io::Result<()> {
        debug_assert!((1..=MAX_LAYER_STACK_BUCKETS).contains(&num_buckets));
        let mut buf4 = [0u8; 4];

        // Biases: i32 × num_buckets (固定長配列の先頭 num_buckets 要素にのみ書き、
        // それ以降はゼロのまま残す)。
        self.psqt_biases = [0i32; MAX_LAYER_STACK_BUCKETS];
        for bias in self.psqt_biases[..num_buckets].iter_mut() {
            reader.read_exact(&mut buf4)?;
            *bias = i32::from_le_bytes(buf4);
        }

        // Weights: i32 × FT::DIMENSIONS × num_buckets
        // layout: `psqt_weights[feature_idx * num_buckets + bucket]` (feature-major、
        // 各 feature 内で bucket 連番)。tatara `save_quantised` (`crates/nnue-format/
        // src/layerstack_weights.rs:518-541`) の write 順と対称。
        let weight_count = FT::DIMENSIONS * num_buckets;
        self.psqt_weights = AlignedBox::new_zeroed(weight_count);
        for w in self.psqt_weights.iter_mut() {
            reader.read_exact(&mut buf4)?;
            *w = i32::from_le_bytes(buf4);
        }

        self.psqt_num_buckets = num_buckets;
        // 注意: 読み込みが途中で失敗した場合、psqt_biases だけが更新された
        // 中途半端な状態になるが、呼び出し元でエラーが伝播し Self は破棄されるため問題ない。
        self.has_psqt = true;
        Ok(())
    }

    /// PSQT が有効かを返す。
    #[cfg(feature = "ls-ext-psqt")]
    pub fn has_psqt(&self) -> bool {
        self.has_psqt
    }

    /// PSQT バイアスを参照（外部の解析ツール向け、固定長配列の先頭 `psqt_num_buckets` 要素のみ有効）。
    #[cfg(feature = "ls-ext-psqt")]
    pub fn psqt_biases(&self) -> &[i32; MAX_LAYER_STACK_BUCKETS] {
        &self.psqt_biases
    }

    /// PSQT 重みを参照（外部の解析ツール向け）。
    /// レイアウト: `psqt_weights[feature_idx * psqt_num_buckets + bucket]`
    #[cfg(feature = "ls-ext-psqt")]
    pub fn psqt_weights(&self) -> &[i32] {
        &self.psqt_weights
    }

    /// PSQT bucket 数 (= net file の `num_buckets`)。PSQT が無効なら 0。
    #[cfg(feature = "ls-ext-psqt")]
    pub fn psqt_num_buckets(&self) -> usize {
        self.psqt_num_buckets
    }

    /// Threat 重みをファイルから読み込み (i8, raw)
    #[cfg(feature = "ls-ext-threat")]
    pub fn read_threat_weights<R: Read>(&mut self, reader: &mut R) -> io::Result<()> {
        let weight_count = THREAT_DIMENSIONS * L1;
        self.threat_weights = AlignedBox::new_zeroed(weight_count);
        // SAFETY:
        // - `AlignedBox::new_zeroed(weight_count)` は `weight_count` 個の `i8`
        //   を保持する領域をゼロ初期化で確保している。
        // - `i8` と `u8` はサイズ (1 バイト) もアラインメントも同一で、任意の
        //   バイトパターンが両者とも valid（Rust reference: "Numeric types").
        //   よって `*mut i8 → *mut u8` のキャストは valid で、同じメモリ領域を
        //   バイト列として参照するスライスを作るのは安全。
        // - 作ったスライスは `read_exact` 呼び出しの内側でしか使わず、
        //   関数リターン前にドロップされる。`self.threat_weights` への排他可変
        //   参照は関数シグネチャで保証されており、重複参照は発生しない。
        // - `weight_count == THREAT_DIMENSIONS * L1` は `AlignedBox` の長さと
        //   一致するため、`from_raw_parts_mut` の length 要件を満たす。
        let slice = unsafe {
            std::slice::from_raw_parts_mut(
                self.threat_weights.as_mut_ptr() as *mut u8,
                weight_count,
            )
        };
        reader.read_exact(slice)?;
        self.has_threat = true;
        Ok(())
    }

    /// Threat 重みの行を取得（i8[L1]）
    #[cfg(feature = "ls-ext-threat")]
    #[inline]
    fn threat_weight_row(&self, index: usize) -> &[i8] {
        let offset = index * L1;
        let end = offset + L1;
        debug_assert!(end <= self.threat_weights.len(), "threat index out of range: {index}");
        &self.threat_weights[offset..end]
    }

    /// Threat 重み (i8) を i16 アキュムレータに加算（SIMD 最適化）
    ///
    /// i8 重みを i16 に sign-extend してから加算。
    /// AVX2: `_mm256_cvtepi8_epi16` で 16 要素ずつ変換 + `_mm256_add_epi16`。
    #[cfg(feature = "ls-ext-threat")]
    #[inline]
    fn add_threat_weights(&self, accumulation: &mut [i16; L1], index: usize) {
        // AVX2 ループは `L1 / 16` 回で全要素を処理する前提のため、
        // L1 が 16 の倍数でない場合は monomorphization 時に弾く。
        const {
            assert!(L1.is_multiple_of(16), "L1 must be a multiple of 16 for AVX2 SIMD loops");
        }
        let weights = self.threat_weight_row(index);

        // AVX2: 128bit i8 → 256bit i16 sign-extend + add, L1/16 iterations
        #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
        {
            // SAFETY:
            // - `acc_ptr`: 呼び出し元が `AccumulatorLayerStacks::get_threat_mut()`
            //   等から取得する `&mut [i16; L1]` (親構造体 `AccumulatorLayerStacks`
            //   が `#[repr(C, align(64))]` で 64 バイトアライン）→
            //   `_mm256_load_si256` / `_mm256_store_si256` の 32 バイトアライン
            //   要件を満たす
            // - `w_ptr`: `AlignedBox<i8>` のポインタ。`_mm_loadu_si128`
            //   （非アラインロード）を使っているためアライメント要件は無い
            // - プリフェッチは hint のみで safety に影響しない
            // - ループ `L1 / 16` は const generics 由来。`L1 ∈ {512, 768, 1536}`
            //   は全て 16 の倍数なので末端要素が取り残されない
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let w_ptr = weights.as_ptr();

                // 行内プリフェッチ: 後半の cache lines を先読み
                // L1 bytes = L1/64 cache lines (64B), 先頭は触れた時点で fetch される
                if L1 > 512 {
                    _mm_prefetch(w_ptr.add(512), _MM_HINT_T0);
                }
                if L1 > 768 {
                    _mm_prefetch(w_ptr.add(768), _MM_HINT_T0);
                }
                if L1 > 1024 {
                    _mm_prefetch(w_ptr.add(1024), _MM_HINT_T0);
                }
                if L1 > 1280 {
                    _mm_prefetch(w_ptr.add(1280), _MM_HINT_T0);
                }

                for i in 0..(L1 / 16) {
                    let acc_vec = _mm256_load_si256(acc_ptr.add(i * 16) as *const __m256i);
                    let w8 = _mm_loadu_si128(w_ptr.add(i * 16) as *const __m128i);
                    let w16 = _mm256_cvtepi8_epi16(w8);
                    let result = _mm256_add_epi16(acc_vec, w16);
                    _mm256_store_si256(acc_ptr.add(i * 16) as *mut __m256i, result);
                }
            }
        }

        // スカラーフォールバック（AVX2 非対応環境のみコンパイル）
        #[cfg(not(all(target_arch = "x86_64", target_feature = "avx2")))]
        for (a, &w) in accumulation.iter_mut().zip(weights) {
            *a = a.wrapping_add(w as i16);
        }
    }

    /// Threat 重み (i8) を i16 アキュムレータから減算（SIMD 最適化）
    #[cfg(feature = "ls-ext-threat")]
    #[inline]
    fn sub_threat_weights(&self, accumulation: &mut [i16; L1], index: usize) {
        const {
            assert!(L1.is_multiple_of(16), "L1 must be a multiple of 16 for AVX2 SIMD loops");
        }
        let weights = self.threat_weight_row(index);

        // AVX2: 128bit i8 → 256bit i16 sign-extend + sub
        #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
        {
            // SAFETY: `add_threat_weights` と同じ契約。
            // - `acc_ptr`: `accumulation: &mut [i16; L1]` の親は
            //   `AccumulatorLayerStacks<L1>` (`#[repr(C, align(64))]`) → 32 バイト
            //   アラインが保証され `_mm256_load/store_si256` の要件を満たす
            // - `w_ptr`: `AlignedBox<i8>` だが `_mm_loadu_si128` 非アラインロード
            //   を使うためアライメント要件は無い
            // - `L1 ∈ {512, 768, 1536}` はすべて 16 の倍数
            unsafe {
                use std::arch::x86_64::*;

                // 行内プリフェッチ
                let w_ptr = weights.as_ptr();
                if L1 > 512 {
                    _mm_prefetch(w_ptr.add(512), _MM_HINT_T0);
                }
                if L1 > 768 {
                    _mm_prefetch(w_ptr.add(768), _MM_HINT_T0);
                }
                if L1 > 1024 {
                    _mm_prefetch(w_ptr.add(1024), _MM_HINT_T0);
                }
                if L1 > 1280 {
                    _mm_prefetch(w_ptr.add(1280), _MM_HINT_T0);
                }
                let acc_ptr = accumulation.as_mut_ptr();
                let w_ptr = weights.as_ptr();
                for i in 0..(L1 / 16) {
                    let acc_vec = _mm256_load_si256(acc_ptr.add(i * 16) as *const __m256i);
                    let w8 = _mm_loadu_si128(w_ptr.add(i * 16) as *const __m128i);
                    let w16 = _mm256_cvtepi8_epi16(w8);
                    let result = _mm256_sub_epi16(acc_vec, w16);
                    _mm256_store_si256(acc_ptr.add(i * 16) as *mut __m256i, result);
                }
            }
        }

        // スカラーフォールバック（AVX2 非対応環境のみコンパイル）
        #[cfg(not(all(target_arch = "x86_64", target_feature = "avx2")))]
        for (a, &w) in accumulation.iter_mut().zip(weights) {
            *a = a.wrapping_sub(w as i16);
        }
    }

    /// PSQT アキュムレータのフル計算
    #[cfg(feature = "ls-ext-psqt")]
    fn refresh_psqt(
        &self,
        active_indices: &IndexList<MAX_ACTIVE_FEATURES>,
        psqt_acc: &mut [i32; MAX_LAYER_STACK_BUCKETS],
    ) {
        *psqt_acc = self.psqt_biases;
        for index in active_indices.iter() {
            self.add_psqt_weights(psqt_acc, index);
        }
    }

    /// PSQT 重みを加算
    ///
    /// `psqt_acc[bucket] += psqt_weights[index * n + bucket]` を `n` (=
    /// `self.psqt_num_buckets`) bucket 分まとめて実行する。配列の `[n..MAX]` は
    /// 触らない (load 時に 0 のまま)。
    #[cfg(feature = "ls-ext-psqt")]
    #[inline]
    fn add_psqt_weights(&self, psqt_acc: &mut [i32; MAX_LAYER_STACK_BUCKETS], index: usize) {
        let n = self.psqt_num_buckets;
        let offset = index * n;
        debug_assert!(
            offset + n <= self.psqt_weights.len(),
            "psqt_weights index out of bounds: offset={offset}, n={n}, len={}",
            self.psqt_weights.len()
        );
        // SAFETY: debug_assert で境界確認済み。release では呼び出し元 (refresh / diff) が
        // active_indices を経由しており、index は features の有効範囲内。weights ポインタは
        // n i32 連続を指す。
        let weights_ptr = unsafe { self.psqt_weights.as_ptr().add(offset) };
        psqt_add_or_sub::<true>(psqt_acc, weights_ptr, n);
    }

    /// PSQT 重みを減算
    #[cfg(feature = "ls-ext-psqt")]
    #[inline]
    fn sub_psqt_weights(&self, psqt_acc: &mut [i32; MAX_LAYER_STACK_BUCKETS], index: usize) {
        let n = self.psqt_num_buckets;
        let offset = index * n;
        debug_assert!(
            offset + n <= self.psqt_weights.len(),
            "psqt_weights index out of bounds: offset={offset}, n={n}, len={}",
            self.psqt_weights.len()
        );
        // SAFETY: debug_assert で境界確認済み。release では呼び出し元 (refresh / diff) が
        // active_indices を経由しており、index は features の有効範囲内。weights ポインタは
        // n i32 連続を指す。
        let weights_ptr = unsafe { self.psqt_weights.as_ptr().add(offset) };
        psqt_add_or_sub::<false>(psqt_acc, weights_ptr, n);
    }

    /// 差分計算を使わずにAccumulatorを計算
    pub fn refresh_accumulator(&self, pos: &Position, acc: &mut AccumulatorLayerStacks<L1>) {
        for perspective in [Color::Black, Color::White] {
            let p = perspective as usize;
            let accumulation = acc.get_mut(p);

            // バイアスで初期化
            accumulation.copy_from_slice(&self.biases.0);

            // アクティブな特徴量の重みを加算
            let mut active_indices = IndexList::new();
            append_active_indices::<FT>(pos, perspective, &mut active_indices);
            for index in active_indices.iter() {
                self.add_weights(accumulation, index);
            }

            // PSQT アキュムレータ
            #[cfg(feature = "ls-ext-psqt")]
            if self.has_psqt {
                self.refresh_psqt(&active_indices, &mut acc.psqt_accumulation[p]);
            }

            // Threat アキュムレータ（bias なし: piece FT と bias を共有）
            #[cfg(feature = "ls-ext-threat")]
            if self.has_threat {
                let king_sq = pos.king_square(perspective);
                let threat_acc = acc.get_threat_mut(p);
                threat_acc.fill(0);
                threat_features::for_each_active_threat_index(pos, perspective, king_sq, |idx| {
                    self.add_threat_weights(threat_acc, idx);
                });
            }
        }

        acc.computed_accumulation = true;
        acc.computed_score = false;
    }

    /// 差分計算でAccumulatorを更新
    pub fn update_accumulator(
        &self,
        pos: &Position,
        dirty_piece: &DirtyPiece,
        acc: &mut AccumulatorLayerStacks<L1>,
        prev_acc: &AccumulatorLayerStacks<L1>,
    ) {
        for perspective in [Color::Black, Color::White] {
            let p = perspective as usize;
            let reset = <FT::Set as FeatureSet>::needs_refresh(dirty_piece, perspective);

            if reset {
                // 玉が移動した場合は全計算
                let accumulation = acc.get_mut(p);
                accumulation.copy_from_slice(&self.biases.0);

                let mut active_indices = IndexList::new();
                append_active_indices::<FT>(pos, perspective, &mut active_indices);
                for index in active_indices.iter() {
                    self.add_weights(accumulation, index);
                }

                #[cfg(feature = "ls-ext-psqt")]
                if self.has_psqt {
                    self.refresh_psqt(&active_indices, &mut acc.psqt_accumulation[p]);
                }
            } else {
                // 差分更新
                let mut removed = IndexList::new();
                let mut added = IndexList::new();
                append_changed_indices::<FT>(
                    dirty_piece,
                    perspective,
                    pos.king_square(perspective),
                    &mut removed,
                    &mut added,
                );

                let prev = prev_acc.get(p);
                let curr = acc.get_mut(p);
                curr.copy_from_slice(prev);
                if !self.try_apply_dirty_piece_fast(
                    curr,
                    dirty_piece,
                    perspective,
                    pos.king_square(perspective),
                ) {
                    for index in removed.iter() {
                        self.sub_weights(curr, index);
                    }

                    for index in added.iter() {
                        self.add_weights(curr, index);
                    }
                }

                // PSQT 差分更新
                #[cfg(feature = "ls-ext-psqt")]
                if self.has_psqt {
                    acc.psqt_accumulation[p] = prev_acc.psqt_accumulation[p];
                    for index in removed.iter() {
                        self.sub_psqt_weights(&mut acc.psqt_accumulation[p], index);
                    }
                    for index in added.iter() {
                        self.add_psqt_weights(&mut acc.psqt_accumulation[p], index);
                    }
                }
            }

            // Threat 更新
            //
            // Threat は HalfKaSplit とは独立な index 空間を持ち、king_sq に依存するのは
            // `is_hm_mirror` のみ。玉移動があっても HM mirror 境界を跨がなければ
            // 差分更新で正しく計算できる。HalfKaSplit 用の `reset = king_moved` を流用
            // せず、Threat 専用の `needs_threat_refresh` を使う。
            #[cfg(feature = "ls-ext-threat")]
            if self.has_threat {
                let king_sq = pos.king_square(perspective);
                let reset_threat =
                    threat_features::needs_threat_refresh(dirty_piece, king_sq, perspective);
                if reset_threat {
                    // HM mirror 境界を跨いだ場合のみ全計算
                    let threat_acc = acc.get_threat_mut(p);
                    threat_acc.fill(0);
                    threat_features::for_each_active_threat_index(
                        pos,
                        perspective,
                        king_sq,
                        |idx| {
                            self.add_threat_weights(threat_acc, idx);
                        },
                    );
                } else {
                    // Threat 差分更新
                    let prev_threat = prev_acc.get_threat(p);
                    let curr_threat = acc.get_threat_mut(p);
                    curr_threat.copy_from_slice(prev_threat);

                    let mut t_removed = IndexList::<MAX_CHANGED_THREAT_FEATURES>::new();
                    let mut t_added = IndexList::<MAX_CHANGED_THREAT_FEATURES>::new();
                    let ok = threat_features::append_changed_threat_indices(
                        pos,
                        dirty_piece,
                        perspective,
                        king_sq,
                        &mut t_removed,
                        &mut t_added,
                    );
                    if ok {
                        for idx in t_removed.iter() {
                            self.sub_threat_weights(curr_threat, idx);
                        }
                        for idx in t_added.iter() {
                            self.add_threat_weights(curr_threat, idx);
                        }
                    } else {
                        // overflow → full refresh
                        curr_threat.fill(0);
                        threat_features::for_each_active_threat_index(
                            pos,
                            perspective,
                            king_sq,
                            |idx| {
                                self.add_threat_weights(curr_threat, idx);
                            },
                        );
                    }
                }
            }
        }

        acc.computed_accumulation = true;
        acc.computed_score = false;
    }

    /// 差分計算でAccumulatorを更新（キャッシュ使用版）
    ///
    /// 玉移動時に full refresh が必要な視点では、AccumulatorCaches（Finny Tables）
    /// を参照して差分更新を行う。キャッシュにヒットした場合、全駒加算の代わりに
    /// 前回のキャッシュ状態との差分のみを適用するため高速。
    pub fn update_accumulator_with_cache(
        &self,
        pos: &Position,
        dirty_piece: &DirtyPiece,
        acc: &mut AccumulatorLayerStacks<L1>,
        prev_acc: &AccumulatorLayerStacks<L1>,
        cache: &mut AccumulatorCacheLayerStacks<L1>,
    ) {
        for perspective in [Color::Black, Color::White] {
            let p = perspective as usize;
            let reset = <FT::Set as FeatureSet>::needs_refresh(dirty_piece, perspective);

            if reset {
                count_refresh!();
                // 玉が移動した場合はキャッシュ経由で refresh。
                // PSQT も Finny Tables (AccCacheEntry) 経由で差分更新する。
                #[cfg(feature = "ls-ext-psqt")]
                {
                    // 同一 struct の異なるフィールドへの可変参照を同時に渡す。
                    // accumulation: [..; 2] と psqt_accumulation: [..; 2] は別フィールドなので
                    // 可変借用は競合しない。
                    let main_acc = &mut acc.accumulation[p];
                    let psqt_acc_slot = &mut acc.psqt_accumulation[p];
                    self.refresh_perspective_with_cache(
                        pos,
                        perspective,
                        main_acc,
                        psqt_acc_slot,
                        cache,
                    );
                }
                #[cfg(not(feature = "ls-ext-psqt"))]
                {
                    self.refresh_perspective_with_cache(pos, perspective, acc.get_mut(p), cache);
                }
            } else {
                count_update!();
                // 差分更新（キャッシュ不使用）
                let mut removed = IndexList::new();
                let mut added = IndexList::new();
                append_changed_indices::<FT>(
                    dirty_piece,
                    perspective,
                    pos.king_square(perspective),
                    &mut removed,
                    &mut added,
                );

                let prev = prev_acc.get(p);
                let curr = acc.get_mut(p);
                curr.copy_from_slice(prev);
                if !self.try_apply_dirty_piece_fast(
                    curr,
                    dirty_piece,
                    perspective,
                    pos.king_square(perspective),
                ) {
                    for index in removed.iter() {
                        self.sub_weights(curr, index);
                    }

                    for index in added.iter() {
                        self.add_weights(curr, index);
                    }
                }

                // PSQT 差分更新
                #[cfg(feature = "ls-ext-psqt")]
                if self.has_psqt {
                    acc.psqt_accumulation[p] = prev_acc.psqt_accumulation[p];
                    for index in removed.iter() {
                        self.sub_psqt_weights(&mut acc.psqt_accumulation[p], index);
                    }
                    for index in added.iter() {
                        self.add_psqt_weights(&mut acc.psqt_accumulation[p], index);
                    }
                }
            }

            // Threat 更新（キャッシュ版も非キャッシュ版と同じロジック）
            //
            // HalfKaSplit 用の `reset = king_moved` ではなく、Threat 専用の
            // `needs_threat_refresh` (is_hm_mirror 境界跨ぎのみ true) を使う。
            // 詳細: `threat_features::needs_threat_refresh` doc 参照。
            #[cfg(feature = "ls-ext-threat")]
            if self.has_threat {
                let king_sq = pos.king_square(perspective);
                let reset_threat =
                    threat_features::needs_threat_refresh(dirty_piece, king_sq, perspective);
                if reset_threat {
                    let threat_acc = acc.get_threat_mut(p);
                    threat_acc.fill(0);
                    threat_features::for_each_active_threat_index(
                        pos,
                        perspective,
                        king_sq,
                        |idx| {
                            self.add_threat_weights(threat_acc, idx);
                        },
                    );
                } else {
                    let prev_threat = prev_acc.get_threat(p);
                    let curr_threat = acc.get_threat_mut(p);
                    curr_threat.copy_from_slice(prev_threat);

                    let mut t_removed = IndexList::<MAX_CHANGED_THREAT_FEATURES>::new();
                    let mut t_added = IndexList::<MAX_CHANGED_THREAT_FEATURES>::new();
                    let ok = threat_features::append_changed_threat_indices(
                        pos,
                        dirty_piece,
                        perspective,
                        king_sq,
                        &mut t_removed,
                        &mut t_added,
                    );
                    if ok {
                        for idx in t_removed.iter() {
                            self.sub_threat_weights(curr_threat, idx);
                        }
                        for idx in t_added.iter() {
                            self.add_threat_weights(curr_threat, idx);
                        }
                    } else {
                        curr_threat.fill(0);
                        threat_features::for_each_active_threat_index(
                            pos,
                            perspective,
                            king_sq,
                            |idx| {
                                self.add_threat_weights(curr_threat, idx);
                            },
                        );
                    }
                }
            }
        }

        acc.computed_accumulation = true;
        acc.computed_score = false;
    }

    /// キャッシュ使用版の refresh（両視点）
    pub fn refresh_accumulator_with_cache(
        &self,
        pos: &Position,
        acc: &mut AccumulatorLayerStacks<L1>,
        cache: &mut AccumulatorCacheLayerStacks<L1>,
    ) {
        for perspective in [Color::Black, Color::White] {
            count_refresh!();
            let p = perspective as usize;
            // PSQT も Finny Tables (AccCacheEntry) 経由で差分更新する。
            #[cfg(feature = "ls-ext-psqt")]
            {
                let main_acc = &mut acc.accumulation[p];
                let psqt_acc_slot = &mut acc.psqt_accumulation[p];
                self.refresh_perspective_with_cache(
                    pos,
                    perspective,
                    main_acc,
                    psqt_acc_slot,
                    cache,
                );
            }
            #[cfg(not(feature = "ls-ext-psqt"))]
            {
                self.refresh_perspective_with_cache(pos, perspective, acc.get_mut(p), cache);
            }

            // Threat はキャッシュ非対象なのでフル再計算
            #[cfg(feature = "ls-ext-threat")]
            if self.has_threat {
                let king_sq = pos.king_square(perspective);
                let threat_acc = acc.get_threat_mut(p);
                threat_acc.fill(0);
                threat_features::for_each_active_threat_index(pos, perspective, king_sq, |idx| {
                    self.add_threat_weights(threat_acc, idx);
                });
            }
        }

        acc.computed_accumulation = true;
        acc.computed_score = false;
    }

    /// 単一視点のキャッシュ経由 refresh (Stockfish 風 piece_list 差分方式)
    ///
    /// 現在の `PieceList` を直接 cache に渡し、cache 内で slot-wise 差分
    /// を取って add/sub を適用する。`append_active_indices` + `sort_unstable`
    /// のオーバーヘッドを回避する。
    ///
    /// `ls-ext-psqt` 有効かつ `has_psqt == true` の場合は PSQT acc も
    /// 同時に cache 経由で差分更新する（Finny Tables に PSQT を載せる経路）。
    /// `has_psqt == false` のときは `psqt_acc` 引数は渡されるが参照されず、
    /// 既存の `cache.refresh_or_cache()` パスにフォールスルーする。
    ///
    /// **シグネチャ注記**: `ls-ext-psqt` feature 有効時のみ `psqt_acc` 引数が
    /// 追加される（`#[cfg(feature = "ls-ext-psqt")]` 付き）。Rust の有効な
    /// cfg-gated parameter パターンだが、呼び出し側も同様の cfg ブロックで
    /// 括る必要がある。
    #[allow(clippy::too_many_arguments)]
    fn refresh_perspective_with_cache(
        &self,
        pos: &Position,
        perspective: Color,
        accumulation: &mut [i16; L1],
        #[cfg(feature = "ls-ext-psqt")] psqt_acc: &mut [i32; MAX_LAYER_STACK_BUCKETS],
        cache: &mut AccumulatorCacheLayerStacks<L1>,
    ) {
        let king_sq = pos.king_square(perspective);

        let raw_piece_list = if perspective == Color::Black {
            pos.piece_list().piece_list_fb()
        } else {
            pos.piece_list().piece_list_fw()
        };

        // HalfKp は玉 BonaPiece を特徴量に含めないため、`refresh_or_cache` の
        // `if bp != ZERO` で skip されるよう玉スロット (KING / KING+1) を ZERO に
        // マスクして cache に渡す。`INCLUDE_KING_IN_PIECE_LIST = const` なので
        // HalfKa* 系では本分岐ごと DCE される。
        let piece_list_owned;
        let piece_list: &[BonaPiece; PieceNumber::NB] = if FT::INCLUDE_KING_IN_PIECE_LIST {
            raw_piece_list
        } else {
            piece_list_owned = {
                let mut pl = *raw_piece_list;
                pl[PieceNumber::KING as usize] = BonaPiece::ZERO;
                pl[(PieceNumber::KING + 1) as usize] = BonaPiece::ZERO;
                pl
            };
            &piece_list_owned
        };

        let idx_fn = move |bp: BonaPiece| FT::feature_index(bp, perspective, king_sq);

        #[cfg(feature = "ls-ext-psqt")]
        if self.has_psqt {
            cache.refresh_or_cache_with_psqt(
                king_sq,
                perspective,
                piece_list,
                &self.biases.0,
                &self.psqt_biases,
                accumulation,
                psqt_acc,
                idx_fn,
                |acc, idx| self.add_weights(acc, idx),
                |acc, idx| self.sub_weights(acc, idx),
                |pacc, idx| self.add_psqt_weights(pacc, idx),
                |pacc, idx| self.sub_psqt_weights(pacc, idx),
            );
            return;
        }

        cache.refresh_or_cache(
            king_sq,
            perspective,
            piece_list,
            &self.biases.0,
            accumulation,
            idx_fn,
            |acc, idx| self.add_weights(acc, idx),
            |acc, idx| self.sub_weights(acc, idx),
        );
    }

    /// 複数手分の差分を適用してアキュムレータを更新
    pub fn forward_update_incremental(
        &self,
        pos: &Position,
        stack: &mut AccumulatorStackLayerStacks<L1>,
        source_idx: usize,
    ) -> bool {
        let Some(path) = stack.collect_path(source_idx) else {
            // パスが途切れた場合、または MAX_PATH_LENGTH を超えた場合
            return false;
        };

        // source_acc から main + psqt + threat をコピー。
        let source_acc = stack.entry_at(source_idx).accumulator.clone();
        {
            let current_acc = &mut stack.current_mut().accumulator;
            for perspective in [Color::Black, Color::White] {
                let p = perspective as usize;
                current_acc.get_mut(p).copy_from_slice(source_acc.get(p));
                #[cfg(feature = "ls-ext-psqt")]
                {
                    current_acc.psqt_accumulation[p] = source_acc.psqt_accumulation[p];
                }
                #[cfg(feature = "ls-ext-threat")]
                if self.has_threat {
                    current_acc.get_threat_mut(p).copy_from_slice(source_acc.get_threat(p));
                }
            }
        }

        for entry_idx in path.iter() {
            let dirty_piece = stack.entry_at(entry_idx).dirty_piece;

            for perspective in [Color::Black, Color::White] {
                debug_assert!(
                    !dirty_piece.king_moved[perspective.index()],
                    "King moved between source and current"
                );

                let king_sq = pos.king_square(perspective);
                let mut removed = IndexList::new();
                let mut added = IndexList::new();
                append_changed_indices::<FT>(
                    &dirty_piece,
                    perspective,
                    king_sq,
                    &mut removed,
                    &mut added,
                );

                let p = perspective as usize;
                let accumulation = stack.current_mut().accumulator.get_mut(p);
                if !self.try_apply_dirty_piece_fast(
                    accumulation,
                    &dirty_piece,
                    perspective,
                    king_sq,
                ) {
                    for index in removed.iter() {
                        self.sub_weights(accumulation, index);
                    }
                    for index in added.iter() {
                        self.add_weights(accumulation, index);
                    }
                }

                // PSQT 差分更新
                // try_apply_dirty_piece_fast は main path 専用なので、
                // PSQT は removed/added を必ず明示的に適用する。
                #[cfg(feature = "ls-ext-psqt")]
                if self.has_psqt {
                    let psqt_acc = &mut stack.current_mut().accumulator.psqt_accumulation[p];
                    for index in removed.iter() {
                        self.sub_psqt_weights(psqt_acc, index);
                    }
                    for index in added.iter() {
                        self.add_psqt_weights(psqt_acc, index);
                    }
                }
            }
        }

        // Threat: パス長 1 なら pos が正しい after-state なので差分更新可能。
        // パス長 2+ では中間局面を再構成できないため full refresh。
        #[cfg(feature = "ls-ext-threat")]
        if self.has_threat {
            if path.len() == 1 {
                // 1 ply: pos が正しい after-state なので差分更新可能
                let entry_idx = path.iter().next().unwrap();
                let dirty_piece = stack.entry_at(entry_idx).dirty_piece;
                for perspective in [Color::Black, Color::White] {
                    let p = perspective as usize;
                    let king_sq = pos.king_square(perspective);
                    let mut t_removed = IndexList::<MAX_CHANGED_THREAT_FEATURES>::new();
                    let mut t_added = IndexList::<MAX_CHANGED_THREAT_FEATURES>::new();
                    let ok = threat_features::append_changed_threat_indices(
                        pos,
                        &dirty_piece,
                        perspective,
                        king_sq,
                        &mut t_removed,
                        &mut t_added,
                    );
                    let threat_acc = stack.current_mut().accumulator.get_threat_mut(p);
                    if ok {
                        for idx in t_removed.iter() {
                            self.sub_threat_weights(threat_acc, idx);
                        }
                        for idx in t_added.iter() {
                            self.add_threat_weights(threat_acc, idx);
                        }
                    } else {
                        // overflow → full refresh
                        threat_acc.fill(0);
                        threat_features::for_each_active_threat_index(
                            pos,
                            perspective,
                            king_sq,
                            |idx| {
                                self.add_threat_weights(threat_acc, idx);
                            },
                        );
                    }
                }
            } else {
                // 2+ plies: full refresh（中間局面が不明）
                for perspective in [Color::Black, Color::White] {
                    let p = perspective as usize;
                    let king_sq = pos.king_square(perspective);
                    let threat_acc = stack.current_mut().accumulator.get_threat_mut(p);
                    threat_acc.fill(0);
                    threat_features::for_each_active_threat_index(
                        pos,
                        perspective,
                        king_sq,
                        |idx| {
                            self.add_threat_weights(threat_acc, idx);
                        },
                    );
                }
            }
        }

        stack.current_mut().accumulator.computed_accumulation = true;
        stack.current_mut().accumulator.computed_score = false;
        true
    }

    /// 重みを累積値に加算（SIMD最適化版）
    ///
    /// L1 個の i16 要素を SIMD で加算。AVX512BW/AVX2/SSE2/WASM SIMD128 に対応。
    /// weights と accumulation は 64 バイトアラインされている前提で aligned load/store を使用。
    #[inline]
    fn add_weights(&self, accumulation: &mut [i16; L1], index: usize) {
        let weights = self.weight_row(index);

        // AVX-512 BW: 512bit = 32 x i16, L1/32 iterations
        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "avx512f",
            target_feature = "avx512bw"
        ))]
        {
            // SAFETY:
            // - weights: AlignedBox で 64 バイトアライン、各行は 3072 バイト (64の倍数)
            // - accumulation: Aligned<[i16; L1]> で 64 バイトアライン
            // - L1 要素 = 32 要素 × L1/32 回のループで完全にカバー
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let weight_ptr = weights.as_ptr();

                for i in 0..(L1 / 32) {
                    let acc_vec = _mm512_load_si512(acc_ptr.add(i * 32) as *const __m512i);
                    let weight_vec = _mm512_load_si512(weight_ptr.add(i * 32) as *const __m512i);
                    let result = _mm512_add_epi16(acc_vec, weight_vec);
                    _mm512_store_si512(acc_ptr.add(i * 32) as *mut __m512i, result);
                }
            }
            return;
        }

        // AVX2: 256bit = 16 x i16, L1/16 iterations
        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "avx2",
            not(target_feature = "avx512bw")
        ))]
        {
            // SAFETY:
            // - weights: AlignedBox で 64 バイトアライン、各行は 3072 バイト (64の倍数)
            // - accumulation: Aligned<[i16; L1]> で 64 バイトアライン
            // - L1 要素 = 16 要素 × L1/16 回のループで完全にカバー
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let weight_ptr = weights.as_ptr();

                for i in 0..(L1 / 16) {
                    let acc_vec = _mm256_load_si256(acc_ptr.add(i * 16) as *const __m256i);
                    let weight_vec = _mm256_load_si256(weight_ptr.add(i * 16) as *const __m256i);
                    let result = _mm256_add_epi16(acc_vec, weight_vec);
                    _mm256_store_si256(acc_ptr.add(i * 16) as *mut __m256i, result);
                }
            }
            return;
        }

        // SSE2: 128bit = 8 x i16, L1/8 iterations
        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "sse2",
            not(target_feature = "avx2")
        ))]
        {
            // SAFETY: 同上（16バイトアライン）
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let weight_ptr = weights.as_ptr();

                for i in 0..(L1 / 8) {
                    let acc_vec = _mm_load_si128(acc_ptr.add(i * 8) as *const __m128i);
                    let weight_vec = _mm_load_si128(weight_ptr.add(i * 8) as *const __m128i);
                    let result = _mm_add_epi16(acc_vec, weight_vec);
                    _mm_store_si128(acc_ptr.add(i * 8) as *mut __m128i, result);
                }
            }
            return;
        }

        // WASM SIMD128: 128bit = 8 x i16, L1/8 iterations
        #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
        {
            // SAFETY: WASM SIMD128 はアライメント不要
            unsafe {
                use std::arch::wasm32::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let weight_ptr = weights.as_ptr();

                for i in 0..(L1 / 8) {
                    let acc_vec = v128_load(acc_ptr.add(i * 8) as *const v128);
                    let weight_vec = v128_load(weight_ptr.add(i * 8) as *const v128);
                    let result = i16x8_add(acc_vec, weight_vec);
                    v128_store(acc_ptr.add(i * 8) as *mut v128, result);
                }
            }
            return;
        }

        // スカラーフォールバック（非飽和演算）
        #[allow(unreachable_code)]
        for (acc, &weight) in accumulation.iter_mut().zip(weights) {
            *acc = acc.wrapping_add(weight);
        }
    }

    #[inline]
    fn weight_row(&self, index: usize) -> &[i16] {
        let Some(offset) = index.checked_mul(L1) else {
            feature_index_oob(index, self.weights.len() / L1);
        };
        let Some(end) = offset.checked_add(L1) else {
            feature_index_oob(index, self.weights.len() / L1);
        };
        if end > self.weights.len() {
            feature_index_oob(index, self.weights.len() / L1);
        }
        &self.weights[offset..end]
    }

    #[inline]
    fn try_apply_dirty_piece_fast(
        &self,
        accumulation: &mut [i16; L1],
        dirty_piece: &DirtyPiece,
        perspective: Color,
        king_sq: crate::types::Square,
    ) -> bool {
        let changed = &dirty_piece.changed_piece;
        let old_new = |idx: usize| {
            let entry = &changed[idx];
            let old_bp = if perspective == Color::Black {
                entry.old_piece.fb
            } else {
                entry.old_piece.fw
            };
            let new_bp = if perspective == Color::Black {
                entry.new_piece.fb
            } else {
                entry.new_piece.fw
            };
            (old_bp, new_bp)
        };

        // HalfKp で相手玉移動は `needs_refresh` で拾えず fast path に流れる。
        // 玉 BonaPiece (`>= FE_END`) を `FT::feature_index` に渡すと OOR になるため
        // slow path (`append_changed_indices` が玉 BP を除外) にフォールバックする。
        if !FT::INCLUDE_KING_IN_PIECE_LIST {
            use super::bona_piece::FE_END;
            let dn = dirty_piece.dirty_num as usize;
            for entry in changed.iter().take(dn) {
                let (old_bp, new_bp) = if perspective == Color::Black {
                    (entry.old_piece.fb, entry.new_piece.fb)
                } else {
                    (entry.old_piece.fw, entry.new_piece.fw)
                };
                if (old_bp.value() as usize) >= FE_END || (new_bp.value() as usize) >= FE_END {
                    return false;
                }
            }
        }

        // dirty_num==1: 駒の移動（非捕獲）。打ち駒は old_bp==ZERO のためフォールバック。
        // dirty_num==2: 駒を取る指し手のみ。全 BonaPiece は非 ZERO のはずだが、
        //               ZERO チェックでフォールバックを保証する。
        // dirty_num==0: パス手（盤面変化なし）。_ => false でフォールバック。
        match dirty_piece.dirty_num as usize {
            1 => {
                let (old_bp, new_bp) = old_new(0);
                if old_bp != BonaPiece::ZERO && new_bp != BonaPiece::ZERO {
                    self.apply_sub_add_fused(
                        accumulation,
                        feature_index_from_bona_piece::<FT>(old_bp, perspective, king_sq),
                        feature_index_from_bona_piece::<FT>(new_bp, perspective, king_sq),
                    );
                    true
                } else {
                    false
                }
            }
            2 => {
                let (old_bp0, new_bp0) = old_new(0);
                let (old_bp1, new_bp1) = old_new(1);
                if old_bp0 != BonaPiece::ZERO
                    && new_bp0 != BonaPiece::ZERO
                    && old_bp1 != BonaPiece::ZERO
                    && new_bp1 != BonaPiece::ZERO
                {
                    self.apply_double_sub_add_fused(
                        accumulation,
                        feature_index_from_bona_piece::<FT>(old_bp0, perspective, king_sq),
                        feature_index_from_bona_piece::<FT>(new_bp0, perspective, king_sq),
                        feature_index_from_bona_piece::<FT>(old_bp1, perspective, king_sq),
                        feature_index_from_bona_piece::<FT>(new_bp1, perspective, king_sq),
                    );
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    #[inline]
    fn apply_sub_add_fused(
        &self,
        accumulation: &mut [i16; L1],
        sub_index: usize,
        add_index: usize,
    ) {
        let sub_weights = self.weight_row(sub_index);
        let add_weights = self.weight_row(add_index);

        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "avx512f",
            target_feature = "avx512bw"
        ))]
        {
            // SAFETY:
            // - accumulation は Aligned<[i16; L1]> 由来で 64 バイトアライン。
            // - weight row: AlignedBox の先頭が 64 バイトアライン、各行は
            //   L1 × sizeof(i16)(2) バイトなので
            //   全行の先頭も 64 バイト境界に揃う。
            // - L1 要素を 32 要素ずつ L1/32 回で完全に走査する。
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let sub_ptr = sub_weights.as_ptr();
                let add_ptr = add_weights.as_ptr();

                for i in 0..(L1 / 32) {
                    let acc_vec = _mm512_load_si512(acc_ptr.add(i * 32) as *const __m512i);
                    let sub_vec = _mm512_load_si512(sub_ptr.add(i * 32) as *const __m512i);
                    let add_vec = _mm512_load_si512(add_ptr.add(i * 32) as *const __m512i);
                    let result = _mm512_add_epi16(_mm512_sub_epi16(acc_vec, sub_vec), add_vec);
                    _mm512_store_si512(acc_ptr.add(i * 32) as *mut __m512i, result);
                }
            }
            return;
        }

        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "avx2",
            not(target_feature = "avx512bw")
        ))]
        {
            // SAFETY:
            // - accumulation / weight row はともに 32 バイトアライン（3072 = 32 × 96）。
            // - L1 要素を 16 要素ずつ L1/16 回で完全に走査する。
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let sub_ptr = sub_weights.as_ptr();
                let add_ptr = add_weights.as_ptr();

                for i in 0..(L1 / 16) {
                    let acc_vec = _mm256_load_si256(acc_ptr.add(i * 16) as *const __m256i);
                    let sub_vec = _mm256_load_si256(sub_ptr.add(i * 16) as *const __m256i);
                    let add_vec = _mm256_load_si256(add_ptr.add(i * 16) as *const __m256i);
                    let result = _mm256_add_epi16(_mm256_sub_epi16(acc_vec, sub_vec), add_vec);
                    _mm256_store_si256(acc_ptr.add(i * 16) as *mut __m256i, result);
                }
            }
            return;
        }

        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "sse2",
            not(target_feature = "avx2")
        ))]
        {
            // SAFETY:
            // - accumulation / weight row は 16 バイト境界にある。
            // - L1 要素を 8 要素ずつ L1/8 回で完全に走査する。
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let sub_ptr = sub_weights.as_ptr();
                let add_ptr = add_weights.as_ptr();

                for i in 0..(L1 / 8) {
                    let acc_vec = _mm_load_si128(acc_ptr.add(i * 8) as *const __m128i);
                    let sub_vec = _mm_load_si128(sub_ptr.add(i * 8) as *const __m128i);
                    let add_vec = _mm_load_si128(add_ptr.add(i * 8) as *const __m128i);
                    let result = _mm_add_epi16(_mm_sub_epi16(acc_vec, sub_vec), add_vec);
                    _mm_store_si128(acc_ptr.add(i * 8) as *mut __m128i, result);
                }
            }
            return;
        }

        #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
        {
            // SAFETY:
            // - WASM SIMD は unaligned load/store を許容する。
            // - L1 要素を 8 要素ずつ L1/8 回で完全に走査する。
            unsafe {
                use std::arch::wasm32::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let sub_ptr = sub_weights.as_ptr();
                let add_ptr = add_weights.as_ptr();

                for i in 0..(L1 / 8) {
                    let acc_vec = v128_load(acc_ptr.add(i * 8) as *const v128);
                    let sub_vec = v128_load(sub_ptr.add(i * 8) as *const v128);
                    let add_vec = v128_load(add_ptr.add(i * 8) as *const v128);
                    let result = i16x8_add(i16x8_sub(acc_vec, sub_vec), add_vec);
                    v128_store(acc_ptr.add(i * 8) as *mut v128, result);
                }
            }
            return;
        }

        #[allow(unreachable_code)]
        for ((acc, &sub_weight), &add_weight) in
            accumulation.iter_mut().zip(sub_weights.iter()).zip(add_weights.iter())
        {
            *acc = acc.wrapping_sub(sub_weight).wrapping_add(add_weight);
        }
    }

    #[inline]
    fn apply_double_sub_add_fused(
        &self,
        accumulation: &mut [i16; L1],
        sub_index0: usize,
        add_index0: usize,
        sub_index1: usize,
        add_index1: usize,
    ) {
        let sub_weights0 = self.weight_row(sub_index0);
        let add_weights0 = self.weight_row(add_index0);
        let sub_weights1 = self.weight_row(sub_index1);
        let add_weights1 = self.weight_row(add_index1);

        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "avx512f",
            target_feature = "avx512bw"
        ))]
        {
            // SAFETY:
            // - accumulation は Aligned<[i16; L1]> 由来で 64 バイトアライン。
            // - weight row: AlignedBox の先頭が 64 バイトアライン、各行は
            //   L1 × sizeof(i16)(2) バイトなので
            //   全行の先頭も 64 バイト境界に揃う。
            // - L1 要素を 32 要素ずつ L1/32 回で完全に走査する。
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let sub_ptr0 = sub_weights0.as_ptr();
                let add_ptr0 = add_weights0.as_ptr();
                let sub_ptr1 = sub_weights1.as_ptr();
                let add_ptr1 = add_weights1.as_ptr();

                for i in 0..(L1 / 32) {
                    let acc_vec = _mm512_load_si512(acc_ptr.add(i * 32) as *const __m512i);
                    let sub_vec0 = _mm512_load_si512(sub_ptr0.add(i * 32) as *const __m512i);
                    let add_vec0 = _mm512_load_si512(add_ptr0.add(i * 32) as *const __m512i);
                    let sub_vec1 = _mm512_load_si512(sub_ptr1.add(i * 32) as *const __m512i);
                    let add_vec1 = _mm512_load_si512(add_ptr1.add(i * 32) as *const __m512i);
                    let result = _mm512_add_epi16(
                        _mm512_add_epi16(_mm512_sub_epi16(acc_vec, sub_vec0), add_vec0),
                        _mm512_sub_epi16(add_vec1, sub_vec1),
                    );
                    _mm512_store_si512(acc_ptr.add(i * 32) as *mut __m512i, result);
                }
            }
            return;
        }

        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "avx2",
            not(target_feature = "avx512bw")
        ))]
        {
            // SAFETY:
            // - accumulation / 4 本の weight row はともに 32 バイトアライン（3072 = 32 × 96）。
            // - L1 要素を 16 要素ずつ L1/16 回で完全に走査する。
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let sub_ptr0 = sub_weights0.as_ptr();
                let add_ptr0 = add_weights0.as_ptr();
                let sub_ptr1 = sub_weights1.as_ptr();
                let add_ptr1 = add_weights1.as_ptr();

                for i in 0..(L1 / 16) {
                    let acc_vec = _mm256_load_si256(acc_ptr.add(i * 16) as *const __m256i);
                    let sub_vec0 = _mm256_load_si256(sub_ptr0.add(i * 16) as *const __m256i);
                    let add_vec0 = _mm256_load_si256(add_ptr0.add(i * 16) as *const __m256i);
                    let sub_vec1 = _mm256_load_si256(sub_ptr1.add(i * 16) as *const __m256i);
                    let add_vec1 = _mm256_load_si256(add_ptr1.add(i * 16) as *const __m256i);
                    let result = _mm256_add_epi16(
                        _mm256_add_epi16(_mm256_sub_epi16(acc_vec, sub_vec0), add_vec0),
                        _mm256_sub_epi16(add_vec1, sub_vec1),
                    );
                    _mm256_store_si256(acc_ptr.add(i * 16) as *mut __m256i, result);
                }
            }
            return;
        }

        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "sse2",
            not(target_feature = "avx2")
        ))]
        {
            // SAFETY:
            // - accumulation / 4 本の weight row は 16 バイト境界にある。
            // - L1 要素を 8 要素ずつ L1/8 回で完全に走査する。
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let sub_ptr0 = sub_weights0.as_ptr();
                let add_ptr0 = add_weights0.as_ptr();
                let sub_ptr1 = sub_weights1.as_ptr();
                let add_ptr1 = add_weights1.as_ptr();

                for i in 0..(L1 / 8) {
                    let acc_vec = _mm_load_si128(acc_ptr.add(i * 8) as *const __m128i);
                    let sub_vec0 = _mm_load_si128(sub_ptr0.add(i * 8) as *const __m128i);
                    let add_vec0 = _mm_load_si128(add_ptr0.add(i * 8) as *const __m128i);
                    let sub_vec1 = _mm_load_si128(sub_ptr1.add(i * 8) as *const __m128i);
                    let add_vec1 = _mm_load_si128(add_ptr1.add(i * 8) as *const __m128i);
                    let result = _mm_add_epi16(
                        _mm_add_epi16(_mm_sub_epi16(acc_vec, sub_vec0), add_vec0),
                        _mm_sub_epi16(add_vec1, sub_vec1),
                    );
                    _mm_store_si128(acc_ptr.add(i * 8) as *mut __m128i, result);
                }
            }
            return;
        }

        #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
        {
            // SAFETY:
            // - WASM SIMD は unaligned load/store を許容する。
            // - L1 要素を 8 要素ずつ L1/8 回で完全に走査する。
            unsafe {
                use std::arch::wasm32::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let sub_ptr0 = sub_weights0.as_ptr();
                let add_ptr0 = add_weights0.as_ptr();
                let sub_ptr1 = sub_weights1.as_ptr();
                let add_ptr1 = add_weights1.as_ptr();

                for i in 0..(L1 / 8) {
                    let acc_vec = v128_load(acc_ptr.add(i * 8) as *const v128);
                    let sub_vec0 = v128_load(sub_ptr0.add(i * 8) as *const v128);
                    let add_vec0 = v128_load(add_ptr0.add(i * 8) as *const v128);
                    let sub_vec1 = v128_load(sub_ptr1.add(i * 8) as *const v128);
                    let add_vec1 = v128_load(add_ptr1.add(i * 8) as *const v128);
                    let result = i16x8_add(
                        i16x8_add(i16x8_sub(acc_vec, sub_vec0), add_vec0),
                        i16x8_sub(add_vec1, sub_vec1),
                    );
                    v128_store(acc_ptr.add(i * 8) as *mut v128, result);
                }
            }
            return;
        }

        #[allow(unreachable_code)]
        for ((((acc, &sub_weight0), &add_weight0), &sub_weight1), &add_weight1) in accumulation
            .iter_mut()
            .zip(sub_weights0.iter())
            .zip(add_weights0.iter())
            .zip(sub_weights1.iter())
            .zip(add_weights1.iter())
        {
            *acc = acc
                .wrapping_sub(sub_weight0)
                .wrapping_add(add_weight0)
                .wrapping_sub(sub_weight1)
                .wrapping_add(add_weight1);
        }
    }

    /// 重みを累積値から減算（SIMD最適化版）
    ///
    /// L1 個の i16 要素を SIMD で減算。AVX512BW/AVX2/SSE2/WASM SIMD128 に対応。
    /// weights と accumulation は 64 バイトアラインされている前提で aligned load/store を使用。
    #[inline]
    fn sub_weights(&self, accumulation: &mut [i16; L1], index: usize) {
        let weights = self.weight_row(index);

        // AVX-512 BW: 512bit = 32 x i16, L1/32 iterations
        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "avx512f",
            target_feature = "avx512bw"
        ))]
        {
            // SAFETY: add_weights と同様
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let weight_ptr = weights.as_ptr();

                for i in 0..(L1 / 32) {
                    let acc_vec = _mm512_load_si512(acc_ptr.add(i * 32) as *const __m512i);
                    let weight_vec = _mm512_load_si512(weight_ptr.add(i * 32) as *const __m512i);
                    let result = _mm512_sub_epi16(acc_vec, weight_vec);
                    _mm512_store_si512(acc_ptr.add(i * 32) as *mut __m512i, result);
                }
            }
            return;
        }

        // AVX2: 256bit = 16 x i16, L1/16 iterations
        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "avx2",
            not(target_feature = "avx512bw")
        ))]
        {
            // SAFETY: add_weights と同様
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let weight_ptr = weights.as_ptr();

                for i in 0..(L1 / 16) {
                    let acc_vec = _mm256_load_si256(acc_ptr.add(i * 16) as *const __m256i);
                    let weight_vec = _mm256_load_si256(weight_ptr.add(i * 16) as *const __m256i);
                    let result = _mm256_sub_epi16(acc_vec, weight_vec);
                    _mm256_store_si256(acc_ptr.add(i * 16) as *mut __m256i, result);
                }
            }
            return;
        }

        // SSE2: 128bit = 8 x i16, L1/8 iterations
        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "sse2",
            not(target_feature = "avx2")
        ))]
        {
            // SAFETY: 同上（16バイトアライン）
            unsafe {
                use std::arch::x86_64::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let weight_ptr = weights.as_ptr();

                for i in 0..(L1 / 8) {
                    let acc_vec = _mm_load_si128(acc_ptr.add(i * 8) as *const __m128i);
                    let weight_vec = _mm_load_si128(weight_ptr.add(i * 8) as *const __m128i);
                    let result = _mm_sub_epi16(acc_vec, weight_vec);
                    _mm_store_si128(acc_ptr.add(i * 8) as *mut __m128i, result);
                }
            }
            return;
        }

        // WASM SIMD128: 128bit = 8 x i16, L1/8 iterations
        #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
        {
            // SAFETY: WASM SIMD128 はアライメント不要
            unsafe {
                use std::arch::wasm32::*;
                let acc_ptr = accumulation.as_mut_ptr();
                let weight_ptr = weights.as_ptr();

                for i in 0..(L1 / 8) {
                    let acc_vec = v128_load(acc_ptr.add(i * 8) as *const v128);
                    let weight_vec = v128_load(weight_ptr.add(i * 8) as *const v128);
                    let result = i16x8_sub(acc_vec, weight_vec);
                    v128_store(acc_ptr.add(i * 8) as *mut v128, result);
                }
            }
            return;
        }

        // スカラーフォールバック（非飽和演算）
        #[allow(unreachable_code)]
        for (acc, &weight) in accumulation.iter_mut().zip(weights) {
            *acc = acc.wrapping_sub(weight);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nnue::accumulator::ChangedBonaPiece;
    use crate::nnue::bona_piece::ExtBonaPiece;
    use crate::nnue::constants::{DEFAULT_NUM_BUCKETS, HALFKA_HM_DIMENSIONS, NNUE_PYTORCH_L1};
    use crate::nnue::ls_feature_spec::HalfKaHmMergedSpec;
    use crate::nnue::piece_list::PieceNumber;
    use crate::types::{File, Piece, PieceType, Rank, Square};

    const TEST_L1: usize = NNUE_PYTORCH_L1;

    type TestSpec = HalfKaHmMergedSpec;
    type TestFt = FeatureTransformerLayerStacks<TEST_L1, TestSpec>;

    fn make_test_transformer() -> TestFt {
        FeatureTransformerLayerStacks::<TEST_L1, TestSpec> {
            biases: Aligned([0; TEST_L1]),
            weights: AlignedBox::new_zeroed(TestSpec::DIMENSIONS * TEST_L1),
            #[cfg(feature = "ls-ext-psqt")]
            psqt_biases: [0; MAX_LAYER_STACK_BUCKETS],
            #[cfg(feature = "ls-ext-psqt")]
            psqt_num_buckets: 0,
            #[cfg(feature = "ls-ext-psqt")]
            psqt_weights: AlignedBox::new_zeroed(0),
            #[cfg(feature = "ls-ext-psqt")]
            has_psqt: false,
            #[cfg(feature = "ls-ext-threat")]
            threat_weights: AlignedBox::new_zeroed(0),
            #[cfg(feature = "ls-ext-threat")]
            has_threat: false,
            _ft: PhantomData,
        }
    }

    fn fill_weight_row(ft: &mut TestFt, index: usize, seed: i16) {
        let start = index * TEST_L1;
        for (i, slot) in ft.weights[start..start + TEST_L1].iter_mut().enumerate() {
            *slot = seed.wrapping_add((i % 29) as i16);
        }
    }

    fn apply_generic(
        ft: &TestFt,
        accumulation: &mut [i16; TEST_L1],
        dirty_piece: &DirtyPiece,
        perspective: Color,
        king_sq: Square,
    ) {
        let mut removed = IndexList::new();
        let mut added = IndexList::new();
        append_changed_indices::<TestSpec>(
            dirty_piece,
            perspective,
            king_sq,
            &mut removed,
            &mut added,
        );
        for index in removed.iter() {
            ft.sub_weights(accumulation, index);
        }
        for index in added.iter() {
            ft.add_weights(accumulation, index);
        }
    }

    #[test]
    fn test_feature_transformer_dimensions() {
        assert_eq!(TEST_L1, 1536);
        assert_eq!(TestSpec::DIMENSIONS, HALFKA_HM_DIMENSIONS);
        assert_eq!(TestSpec::DIMENSIONS, 73305);
    }

    #[test]
    fn test_try_apply_dirty_piece_fast_matches_generic_single_move() {
        let king_sq = Square::new(File::File5, Rank::Rank9);
        let mut ft = make_test_transformer();
        let mut dirty_piece = DirtyPiece::new();
        dirty_piece.dirty_num = 1;
        dirty_piece.piece_no[0] = PieceNumber(0);
        dirty_piece.changed_piece[0] = ChangedBonaPiece {
            old_piece: ExtBonaPiece::from_board(
                Piece::B_PAWN,
                Square::new(File::File7, Rank::Rank7),
            ),
            new_piece: ExtBonaPiece::from_board(
                Piece::B_PAWN,
                Square::new(File::File7, Rank::Rank6),
            ),
        };

        let old_index = feature_index_from_bona_piece::<TestSpec>(
            dirty_piece.changed_piece[0].old_piece.fb,
            Color::Black,
            king_sq,
        );
        let new_index = feature_index_from_bona_piece::<TestSpec>(
            dirty_piece.changed_piece[0].new_piece.fb,
            Color::Black,
            king_sq,
        );
        fill_weight_row(&mut ft, old_index, 11);
        fill_weight_row(&mut ft, new_index, 37);

        let mut generic = Aligned([5i16; TEST_L1]);
        let mut fast = Aligned([5i16; TEST_L1]);
        apply_generic(&ft, &mut generic.0, &dirty_piece, Color::Black, king_sq);
        assert!(ft.try_apply_dirty_piece_fast(&mut fast.0, &dirty_piece, Color::Black, king_sq));
        assert_eq!(generic.0, fast.0);
    }

    #[test]
    fn test_try_apply_dirty_piece_fast_matches_generic_capture() {
        let king_sq = Square::new(File::File5, Rank::Rank9);
        let mut ft = make_test_transformer();
        let mut dirty_piece = DirtyPiece::new();
        dirty_piece.dirty_num = 2;
        dirty_piece.piece_no[0] = PieceNumber(0);
        dirty_piece.changed_piece[0] = ChangedBonaPiece {
            old_piece: ExtBonaPiece::from_board(
                Piece::B_PAWN,
                Square::new(File::File2, Rank::Rank4),
            ),
            new_piece: ExtBonaPiece::from_board(
                Piece::B_PAWN,
                Square::new(File::File2, Rank::Rank3),
            ),
        };
        dirty_piece.piece_no[1] = PieceNumber(1);
        dirty_piece.changed_piece[1] = ChangedBonaPiece {
            old_piece: ExtBonaPiece::from_board(
                Piece::W_PAWN,
                Square::new(File::File2, Rank::Rank3),
            ),
            new_piece: ExtBonaPiece::from_hand(Color::Black, PieceType::Pawn, 1),
        };

        let indices = [
            feature_index_from_bona_piece::<TestSpec>(
                dirty_piece.changed_piece[0].old_piece.fb,
                Color::Black,
                king_sq,
            ),
            feature_index_from_bona_piece::<TestSpec>(
                dirty_piece.changed_piece[0].new_piece.fb,
                Color::Black,
                king_sq,
            ),
            feature_index_from_bona_piece::<TestSpec>(
                dirty_piece.changed_piece[1].old_piece.fb,
                Color::Black,
                king_sq,
            ),
            feature_index_from_bona_piece::<TestSpec>(
                dirty_piece.changed_piece[1].new_piece.fb,
                Color::Black,
                king_sq,
            ),
        ];
        for (seed, &index) in [13i16, 29, 43, 71].iter().zip(indices.iter()) {
            fill_weight_row(&mut ft, index, *seed);
        }

        let mut generic = Aligned([7i16; TEST_L1]);
        let mut fast = Aligned([7i16; TEST_L1]);
        apply_generic(&ft, &mut generic.0, &dirty_piece, Color::Black, king_sq);
        assert!(ft.try_apply_dirty_piece_fast(&mut fast.0, &dirty_piece, Color::Black, king_sq));
        assert_eq!(generic.0, fast.0);
    }

    #[test]
    fn test_try_apply_dirty_piece_fast_matches_generic_single_move_white() {
        // 後手視点: fw / king_sq.inverse() の分岐をカバー
        let king_sq = Square::new(File::File5, Rank::Rank1);
        let mut ft = make_test_transformer();
        let mut dirty_piece = DirtyPiece::new();
        dirty_piece.dirty_num = 1;
        dirty_piece.piece_no[0] = PieceNumber(0);
        dirty_piece.changed_piece[0] = ChangedBonaPiece {
            old_piece: ExtBonaPiece::from_board(
                Piece::W_PAWN,
                Square::new(File::File3, Rank::Rank3),
            ),
            new_piece: ExtBonaPiece::from_board(
                Piece::W_PAWN,
                Square::new(File::File3, Rank::Rank4),
            ),
        };

        let old_index = feature_index_from_bona_piece::<TestSpec>(
            dirty_piece.changed_piece[0].old_piece.fw,
            Color::White,
            king_sq,
        );
        let new_index = feature_index_from_bona_piece::<TestSpec>(
            dirty_piece.changed_piece[0].new_piece.fw,
            Color::White,
            king_sq,
        );
        fill_weight_row(&mut ft, old_index, 19);
        fill_weight_row(&mut ft, new_index, 53);

        let mut generic = Aligned([5i16; TEST_L1]);
        let mut fast = Aligned([5i16; TEST_L1]);
        apply_generic(&ft, &mut generic.0, &dirty_piece, Color::White, king_sq);
        assert!(ft.try_apply_dirty_piece_fast(&mut fast.0, &dirty_piece, Color::White, king_sq));
        assert_eq!(generic.0, fast.0);
    }

    #[test]
    fn test_try_apply_dirty_piece_fast_matches_generic_capture_white() {
        // 後手視点: dirty_num==2 の fw 分岐をカバー
        // 後手の角が先手の歩を取る想定
        let king_sq = Square::new(File::File5, Rank::Rank1);
        let mut ft = make_test_transformer();
        let mut dirty_piece = DirtyPiece::new();
        dirty_piece.dirty_num = 2;
        dirty_piece.piece_no[0] = PieceNumber(0);
        dirty_piece.changed_piece[0] = ChangedBonaPiece {
            old_piece: ExtBonaPiece::from_board(
                Piece::W_BISHOP,
                Square::new(File::File8, Rank::Rank2),
            ),
            new_piece: ExtBonaPiece::from_board(
                Piece::W_BISHOP,
                Square::new(File::File3, Rank::Rank7),
            ),
        };
        dirty_piece.piece_no[1] = PieceNumber(1);
        dirty_piece.changed_piece[1] = ChangedBonaPiece {
            old_piece: ExtBonaPiece::from_board(
                Piece::B_PAWN,
                Square::new(File::File3, Rank::Rank7),
            ),
            new_piece: ExtBonaPiece::from_hand(Color::White, PieceType::Pawn, 1),
        };

        let indices = [
            feature_index_from_bona_piece::<TestSpec>(
                dirty_piece.changed_piece[0].old_piece.fw,
                Color::White,
                king_sq,
            ),
            feature_index_from_bona_piece::<TestSpec>(
                dirty_piece.changed_piece[0].new_piece.fw,
                Color::White,
                king_sq,
            ),
            feature_index_from_bona_piece::<TestSpec>(
                dirty_piece.changed_piece[1].old_piece.fw,
                Color::White,
                king_sq,
            ),
            feature_index_from_bona_piece::<TestSpec>(
                dirty_piece.changed_piece[1].new_piece.fw,
                Color::White,
                king_sq,
            ),
        ];
        for (seed, &index) in [17i16, 31, 47, 67].iter().zip(indices.iter()) {
            fill_weight_row(&mut ft, index, *seed);
        }

        let mut generic = Aligned([7i16; TEST_L1]);
        let mut fast = Aligned([7i16; TEST_L1]);
        apply_generic(&ft, &mut generic.0, &dirty_piece, Color::White, king_sq);
        assert!(ft.try_apply_dirty_piece_fast(&mut fast.0, &dirty_piece, Color::White, king_sq));
        assert_eq!(generic.0, fast.0);
    }

    #[test]
    fn test_try_apply_dirty_piece_fast_returns_false_for_hand_only_change() {
        let king_sq = Square::new(File::File5, Rank::Rank9);
        let ft = make_test_transformer();
        let mut dirty_piece = DirtyPiece::new();
        dirty_piece.dirty_num = 1;
        dirty_piece.piece_no[0] = PieceNumber(0);
        dirty_piece.changed_piece[0] = ChangedBonaPiece {
            old_piece: ExtBonaPiece::ZERO,
            new_piece: ExtBonaPiece::from_hand(Color::Black, PieceType::Pawn, 1),
        };

        let mut accumulation = Aligned([0i16; TEST_L1]);
        assert!(!ft.try_apply_dirty_piece_fast(
            &mut accumulation.0,
            &dirty_piece,
            Color::Black,
            king_sq,
        ));
    }

    // =========================================================================
    // PSQT テスト
    // =========================================================================

    #[cfg(feature = "ls-ext-psqt")]
    fn make_test_transformer_with_psqt() -> TestFt {
        let n = DEFAULT_NUM_BUCKETS; // legacy デフォルトの 9 で test
        let psqt_weight_count = TestSpec::DIMENSIONS * n;
        let mut psqt_weights = AlignedBox::new_zeroed(psqt_weight_count);
        for feat in 0..TestSpec::DIMENSIONS {
            for bucket in 0..n {
                psqt_weights[feat * n + bucket] =
                    (feat as i32 * 7 + bucket as i32 * 3) % 1000 - 500;
            }
        }

        let mut psqt_biases = [0i32; MAX_LAYER_STACK_BUCKETS];
        for (i, b) in psqt_biases[..n].iter_mut().enumerate() {
            *b = (i as i32 + 1) * 10; // [10, 20, ..., 90]
        }

        FeatureTransformerLayerStacks::<TEST_L1, TestSpec> {
            biases: Aligned([0; TEST_L1]),
            weights: AlignedBox::new_zeroed(TestSpec::DIMENSIONS * TEST_L1),
            psqt_biases,
            psqt_num_buckets: n,
            psqt_weights,
            has_psqt: true,
            #[cfg(feature = "ls-ext-threat")]
            threat_weights: AlignedBox::new_zeroed(0),
            #[cfg(feature = "ls-ext-threat")]
            has_threat: false,
            _ft: PhantomData,
        }
    }

    /// refresh_psqt と add/sub_psqt_weights による差分更新が一致することを確認
    #[cfg(feature = "ls-ext-psqt")]
    #[test]
    fn test_psqt_refresh_matches_incremental() {
        let ft = make_test_transformer_with_psqt();

        // 初期特徴量: [100, 200, 300]
        let mut active_initial = IndexList::new();
        let _ = active_initial.push(100);
        let _ = active_initial.push(200);
        let _ = active_initial.push(300);

        // フル計算
        let mut full_acc = [0i32; MAX_LAYER_STACK_BUCKETS];
        ft.refresh_psqt(&active_initial, &mut full_acc);

        // 差分: 200 を削除、400 を追加 → [100, 300, 400]
        let mut incr_acc = full_acc;
        ft.sub_psqt_weights(&mut incr_acc, 200);
        ft.add_psqt_weights(&mut incr_acc, 400);

        // フル計算（[100, 300, 400]）
        let mut active_updated = IndexList::new();
        let _ = active_updated.push(100);
        let _ = active_updated.push(300);
        let _ = active_updated.push(400);
        let mut full_updated = [0i32; MAX_LAYER_STACK_BUCKETS];
        ft.refresh_psqt(&active_updated, &mut full_updated);

        assert_eq!(incr_acc, full_updated, "差分更新とフル計算の結果が不一致");
    }

    /// PSQT 有効モデルで既知の入力に対して期待値を確認
    #[cfg(feature = "ls-ext-psqt")]
    #[test]
    fn test_psqt_known_values() {
        let ft = make_test_transformer_with_psqt();

        let mut active = IndexList::new();
        let _ = active.push(0);
        let _ = active.push(1);

        let mut acc = [0i32; MAX_LAYER_STACK_BUCKETS];
        ft.refresh_psqt(&active, &mut acc);

        // feat=0: (0*7 + b*3) % 1000 - 500 = b*3 - 500
        // feat=1: (1*7 + b*3) % 1000 - 500 = 7 + b*3 - 500
        // bias + feat0 + feat1
        let n = ft.psqt_num_buckets;
        for (bucket, val) in acc[..n].iter().enumerate() {
            let b = bucket as i32;
            let bias = (b + 1) * 10; // [10, 20, ..., 90]
            let w0 = b * 3 - 500;
            let w1 = 7 + b * 3 - 500;
            let expected = bias + w0 + w1;
            assert_eq!(*val, expected, "bucket {bucket}: expected {expected}, got {val}");
        }
        // 未使用 bucket は 0 のまま
        for val in acc[n..].iter() {
            assert_eq!(*val, 0, "unused buckets should remain zero");
        }
    }

    // =========================================================================
    // 5 FT smoke tests
    // =========================================================================

    use crate::nnue::ls_feature_spec::{
        HalfKaHmSplitSpec, HalfKaMergedSpec, HalfKaSplitSpec, HalfKpSpec, LsFeatureSpec,
    };
    use crate::position::{Position, SFEN_HIRATE};

    fn smoke_refresh_for_spec<FT: LsFeatureSpec>() {
        let weights = AlignedBox::<i16>::new_zeroed(FT::DIMENSIONS * TEST_L1);
        let ft = FeatureTransformerLayerStacks::<TEST_L1, FT> {
            biases: Aligned([0; TEST_L1]),
            weights,
            #[cfg(feature = "ls-ext-psqt")]
            psqt_biases: [0; MAX_LAYER_STACK_BUCKETS],
            #[cfg(feature = "ls-ext-psqt")]
            psqt_num_buckets: 0,
            #[cfg(feature = "ls-ext-psqt")]
            psqt_weights: AlignedBox::new_zeroed(0),
            #[cfg(feature = "ls-ext-psqt")]
            has_psqt: false,
            #[cfg(feature = "ls-ext-threat")]
            threat_weights: AlignedBox::new_zeroed(0),
            #[cfg(feature = "ls-ext-threat")]
            has_threat: false,
            _ft: PhantomData,
        };
        let mut pos = Position::new();
        pos.set_sfen(SFEN_HIRATE).unwrap();
        let mut acc = AccumulatorLayerStacks::<TEST_L1>::new();
        ft.refresh_accumulator(&pos, &mut acc);
        assert!(acc.computed_accumulation);
        // weights/biases が全て 0 のため accumulator も全 0。FT が変わっても
        // 構造が壊れていないことを smoke レベルで保証する。
        for v in acc.get(0).iter().chain(acc.get(1).iter()) {
            assert_eq!(*v, 0, "zero-weights refresh should keep accumulation at 0");
        }
    }

    #[test]
    fn smoke_refresh_halfka_hm_merged() {
        smoke_refresh_for_spec::<HalfKaHmMergedSpec>();
    }

    #[test]
    fn smoke_refresh_halfka_hm_split() {
        smoke_refresh_for_spec::<HalfKaHmSplitSpec>();
    }

    #[test]
    fn smoke_refresh_halfka_merged() {
        smoke_refresh_for_spec::<HalfKaMergedSpec>();
    }

    #[test]
    fn smoke_refresh_halfka_split() {
        smoke_refresh_for_spec::<HalfKaSplitSpec>();
    }

    #[test]
    fn smoke_refresh_halfkp() {
        smoke_refresh_for_spec::<HalfKpSpec>();
    }

    /// HalfKp + cache 経由 refresh で玉 BonaPiece (`>= FE_END`) を `idx_fn` に渡さない
    /// ことを ply32 局面 (駒成り + 駒台手駒あり) と相手玉位置違い派生局面で保証する。
    #[test]
    fn refresh_with_cache_halfkp_complex_position() {
        let weights = AlignedBox::<i16>::new_zeroed(HalfKpSpec::DIMENSIONS * TEST_L1);
        let ft = FeatureTransformerLayerStacks::<TEST_L1, HalfKpSpec> {
            biases: Aligned([0; TEST_L1]),
            weights,
            #[cfg(feature = "ls-ext-psqt")]
            psqt_biases: [0; MAX_LAYER_STACK_BUCKETS],
            #[cfg(feature = "ls-ext-psqt")]
            psqt_num_buckets: 0,
            #[cfg(feature = "ls-ext-psqt")]
            psqt_weights: AlignedBox::new_zeroed(0),
            #[cfg(feature = "ls-ext-psqt")]
            has_psqt: false,
            #[cfg(feature = "ls-ext-threat")]
            threat_weights: AlignedBox::new_zeroed(0),
            #[cfg(feature = "ls-ext-threat")]
            has_threat: false,
            _ft: PhantomData,
        };

        let mut pos = Position::new();
        pos.set_sfen(
            "+B1sg1gsnl/2+N2k1b1/pP2pp2p/2p3p2/9/2PpP4/P1+p2PP1P/7R1/LN1GKGSNL w RLs3p 32",
        )
        .unwrap();

        let mut acc = AccumulatorLayerStacks::<TEST_L1>::new();
        let mut cache = AccumulatorCacheLayerStacks::<TEST_L1>::new();

        ft.refresh_accumulator_with_cache(&pos, &mut acc, &mut cache);
        assert!(acc.computed_accumulation);
        for v in acc.get(0).iter().chain(acc.get(1).iter()) {
            assert_eq!(*v, 0, "zero-weights refresh should keep accumulation at 0");
        }

        ft.refresh_accumulator_with_cache(&pos, &mut acc, &mut cache);
        for v in acc.get(0).iter().chain(acc.get(1).iter()) {
            assert_eq!(*v, 0);
        }

        let mut pos2 = Position::new();
        pos2.set_sfen(
            "+B1sg1gsnl/2+N4b1/pP2ppk1p/2p3p2/9/2PpP4/P1+p2PP1P/7R1/LN1GKGSNL w RLs3p 32",
        )
        .unwrap();
        ft.refresh_accumulator_with_cache(&pos2, &mut acc, &mut cache);
        for v in acc.get(0).iter().chain(acc.get(1).iter()) {
            assert_eq!(*v, 0);
        }
    }

    /// HalfKp で `refresh_accumulator` (slow path) と `refresh_accumulator_with_cache`
    /// (fast cache path、玉スロット ZERO マスク経由) の accumulation が
    /// 非ゼロ weights 下で bit 一致することを保証する。
    #[test]
    fn refresh_with_cache_halfkp_matches_slow_path() {
        let mut weights = AlignedBox::<i16>::new_zeroed(HalfKpSpec::DIMENSIONS * TEST_L1);
        for (i, slot) in weights.iter_mut().enumerate() {
            *slot = (((i as u32).wrapping_mul(2_654_435_761) >> 16) as i16) % 127 - 63;
        }
        let mut biases = Aligned([0i16; TEST_L1]);
        for (i, b) in biases.0.iter_mut().enumerate() {
            *b = ((i as i16) % 17) - 8;
        }

        let make_ft = || FeatureTransformerLayerStacks::<TEST_L1, HalfKpSpec> {
            biases,
            weights: weights.clone(),
            #[cfg(feature = "ls-ext-psqt")]
            psqt_biases: [0; MAX_LAYER_STACK_BUCKETS],
            #[cfg(feature = "ls-ext-psqt")]
            psqt_num_buckets: 0,
            #[cfg(feature = "ls-ext-psqt")]
            psqt_weights: AlignedBox::new_zeroed(0),
            #[cfg(feature = "ls-ext-psqt")]
            has_psqt: false,
            #[cfg(feature = "ls-ext-threat")]
            threat_weights: AlignedBox::new_zeroed(0),
            #[cfg(feature = "ls-ext-threat")]
            has_threat: false,
            _ft: PhantomData,
        };
        let ft_slow = make_ft();
        let ft_cache = make_ft();

        let sfens = [
            "+B1sg1gsnl/2+N2k1b1/pP2pp2p/2p3p2/9/2PpP4/P1+p2PP1P/7R1/LN1GKGSNL w RLs3p 32",
            "+B1sg1gsnl/2+N4b1/pP2ppk1p/2p3p2/9/2PpP4/P1+p2PP1P/7R1/LN1GKGSNL w RLs3p 32",
        ];

        let mut cache = AccumulatorCacheLayerStacks::<TEST_L1>::new();

        for (idx, sfen) in sfens.iter().enumerate() {
            let mut pos = Position::new();
            pos.set_sfen(sfen).unwrap();

            let mut acc_slow = AccumulatorLayerStacks::<TEST_L1>::new();
            ft_slow.refresh_accumulator(&pos, &mut acc_slow);

            let mut acc_cache = AccumulatorLayerStacks::<TEST_L1>::new();
            ft_cache.refresh_accumulator_with_cache(&pos, &mut acc_cache, &mut cache);

            for p in 0..2 {
                let slow = acc_slow.get(p);
                let fast = acc_cache.get(p);
                for (j, (s, f)) in slow.iter().zip(fast.iter()).enumerate() {
                    assert_eq!(s, f, "sfen #{idx} perspective {p} slot {j}: slow={s} cache={f}");
                }
            }
        }
    }

    /// HalfKp の `try_apply_dirty_piece_fast` が玉 BonaPiece (`>= FE_END`) を含む
    /// `DirtyPiece` を受け取ったとき `false` を返して slow path にフォールバック
    /// することを直接検証する。
    #[test]
    fn try_apply_dirty_piece_fast_halfkp_rejects_king_bp() {
        let weights = AlignedBox::<i16>::new_zeroed(HalfKpSpec::DIMENSIONS * TEST_L1);
        let ft = FeatureTransformerLayerStacks::<TEST_L1, HalfKpSpec> {
            biases: Aligned([0; TEST_L1]),
            weights,
            #[cfg(feature = "ls-ext-psqt")]
            psqt_biases: [0; MAX_LAYER_STACK_BUCKETS],
            #[cfg(feature = "ls-ext-psqt")]
            psqt_num_buckets: 0,
            #[cfg(feature = "ls-ext-psqt")]
            psqt_weights: AlignedBox::new_zeroed(0),
            #[cfg(feature = "ls-ext-psqt")]
            has_psqt: false,
            #[cfg(feature = "ls-ext-threat")]
            threat_weights: AlignedBox::new_zeroed(0),
            #[cfg(feature = "ls-ext-threat")]
            has_threat: false,
            _ft: PhantomData,
        };

        let king_sq = Square::new(File::File5, Rank::Rank9);
        let mut acc = Aligned([0i16; TEST_L1]);

        let mut dp_king_move = DirtyPiece::new();
        dp_king_move.dirty_num = 1;
        dp_king_move.piece_no[0] = PieceNumber(PieceNumber::KING + 1);
        dp_king_move.changed_piece[0] = ChangedBonaPiece {
            old_piece: ExtBonaPiece::from_board(
                Piece::W_KING,
                Square::new(File::File5, Rank::Rank1),
            ),
            new_piece: ExtBonaPiece::from_board(
                Piece::W_KING,
                Square::new(File::File4, Rank::Rank1),
            ),
        };
        assert!(
            !ft.try_apply_dirty_piece_fast(&mut acc.0, &dp_king_move, Color::Black, king_sq),
            "HalfKp fast path must reject king BonaPiece move"
        );

        let mut dp_pawn = DirtyPiece::new();
        dp_pawn.dirty_num = 1;
        dp_pawn.piece_no[0] = PieceNumber(0);
        dp_pawn.changed_piece[0] = ChangedBonaPiece {
            old_piece: ExtBonaPiece::from_board(
                Piece::B_PAWN,
                Square::new(File::File7, Rank::Rank7),
            ),
            new_piece: ExtBonaPiece::from_board(
                Piece::B_PAWN,
                Square::new(File::File7, Rank::Rank6),
            ),
        };
        assert!(
            ft.try_apply_dirty_piece_fast(&mut acc.0, &dp_pawn, Color::Black, king_sq),
            "HalfKp fast path must accept non-king BonaPiece move"
        );
    }
}
