//! NetworkLayerStacks - LayerStacksアーキテクチャのNNUEネットワーク
//!
//! 5 種類の FT (HalfKp / HalfKaSplit / HalfKaMerged / HalfKaHmSplit / HalfKaHmMerged)
//! いずれかを `LsFeatureSpec` 経由で受け取り、LayerStacks 構造の NNUE を実装する。
//! nnue-pytorch で学習したファイルを読み込み、評価を行う。
//!
//! ## アーキテクチャ
//!
//! ```text
//! Feature Transformer (FT::DIMENSIONS 次元): → L1 (各視点)
//! 視点結合: 両視点を連結 → L1*2
//! SqrClippedReLU: L1*2 → L1
//! LayerStacks (選択した bucket ごとのスタック):
//!   L1: L1 → LS_L1_OUT
//!   SqrReLU + concat: LS_L2_IN (= 2 * (LS_L1_OUT - 1))
//!   L2: LS_L2_IN → 32
//!   Output: 32 → 1 + skip
//! ```
//!
//! ## バケット選択
//!
//! `LS_BUCKET_MODE` で `progresskpabs` または両玉の相対段に基づく `kingrank9` を選ぶ。

use super::accumulator::Aligned;
use super::accumulator_layer_stacks::{AccumulatorLayerStacks, AccumulatorStackLayerStacks};
#[cfg(feature = "nnue-effect-bucket")]
use super::constants::HALFKA_EFFECT_BUCKET_DIMENSIONS;
#[cfg(feature = "layerstack-arch")]
use super::constants::NNUE_PYTORCH_L3;
use super::constants::{
    DEFAULT_NUM_BUCKETS, FV_SCALE_HALFKA, MAX_ARCH_LEN, MAX_LAYER_STACK_BUCKETS,
    NNUE_VERSION_HALFKA, NNUE_VERSION_LAYERSTACK_NUM_BUCKETS,
};
#[cfg(feature = "layerstacks-768x8x32")]
use super::constants::{LAYER_STACK_8X32_L1_OUT, LAYER_STACK_8X32_L2_IN};
#[cfg(any(
    feature = "layerstacks-1536x16x32",
    feature = "layerstacks-768x16x32",
    feature = "layerstacks-512x16x32",
    feature = "layerstacks-1024x16x32",
    feature = "layerstacks-3072x16x32"
))]
use super::constants::{LAYER_STACK_16X32_L1_OUT, LAYER_STACK_16X32_L2_IN};
#[cfg(feature = "layerstacks-1536x32x32")]
use super::constants::{LAYER_STACK_32X32_L1_OUT, LAYER_STACK_32X32_L2_IN};
use super::feature_transformer_layer_stacks::FeatureTransformerLayerStacks;
use super::layer_stacks::{
    LayerStacks, LsSaturationCounts, sqr_clipped_relu_new, sqr_clipped_relu_transform,
};
#[cfg(feature = "layerstack-arch")]
use super::layers::AffineTransform;
#[cfg(feature = "ft-halfka_hm_merged")]
use super::ls_feature_spec::HalfKaHmMergedSpec;
#[cfg(feature = "ft-halfka_hm_split")]
use super::ls_feature_spec::HalfKaHmSplitSpec;
#[cfg(feature = "ft-halfka_merged")]
use super::ls_feature_spec::HalfKaMergedSpec;
#[cfg(feature = "ft-halfka_split")]
use super::ls_feature_spec::HalfKaSplitSpec;
#[cfg(feature = "ft-halfkp")]
use super::ls_feature_spec::HalfKpSpec;
use super::ls_feature_spec::LsFeatureSpec;
#[cfg(feature = "layerstack-arch")]
use super::net_delta::{
    NetCoefficientId, NetTensorKind, NetTensorShape, add_i16_delta, add_i32_delta,
};
use super::network::{
    LayerStackBucketMode, compute_layer_stack_progresskpabs_bucket_index, get_fv_scale_override,
    get_layer_stack_bucket_mode, get_layer_stack_progress_buckets,
    get_layer_stack_progress_kpabs_weights, parse_fv_scale_from_arch,
};
#[cfg(feature = "nnue-effect-bucket")]
use super::{EFFECT_BUCKET_KING_BUCKETED, EFFECT_BUCKET_NB};
use crate::position::Position;
use crate::types::{Color, Value};
#[cfg(feature = "diagnostics")]
use log::info;
use std::fs::File;
#[cfg(feature = "layerstack-arch")]
use std::io::SeekFrom;
use std::io::{self, BufReader, Cursor, Read, Seek};
use std::marker::PhantomData;
use std::path::Path;

#[inline]
fn compute_layer_stacks_bucket_index(
    pos: &Position,
    side_to_move: Color,
    num_buckets: usize,
) -> usize {
    match get_layer_stack_bucket_mode() {
        LayerStackBucketMode::KingRank9 => {
            compute_layer_stack_kingrank9_bucket_index(pos, side_to_move, num_buckets)
        }
        LayerStackBucketMode::ProgressKPAbs => {
            let weights = get_layer_stack_progress_kpabs_weights();
            let routing_buckets = get_layer_stack_progress_buckets()
                .expect("LayerStacks progress routing is not configured");
            assert!(
                routing_buckets <= num_buckets,
                "LayerStacks progress routing uses {routing_buckets} buckets, but the network stores only {num_buckets}"
            );
            compute_layer_stack_progresskpabs_bucket_index(
                pos,
                side_to_move,
                weights,
                routing_buckets,
            )
        }
    }
}

/// YaneuraOu KingRank9 と同じ両玉の相対段から bucket index を計算する。
///
/// `num_buckets` は 9 でなければならない。USI は `isready` で事前検証する。
#[inline]
pub fn compute_layer_stack_kingrank9_bucket_index(
    pos: &Position,
    side_to_move: Color,
    num_buckets: usize,
) -> usize {
    assert_eq!(num_buckets, 9, "kingrank9 requires exactly 9 stored buckets");
    const F_TO_INDEX: [usize; 9] = [0, 0, 0, 3, 3, 3, 6, 6, 6];
    const E_TO_INDEX: [usize; 9] = [0, 0, 0, 1, 1, 1, 2, 2, 2];

    let f_king = pos.king_square(side_to_move);
    let e_king = pos.king_square(!side_to_move);
    let f_rank = if side_to_move == Color::Black {
        f_king.rank().index()
    } else {
        f_king.inverse().rank().index()
    };
    let e_rank = if side_to_move == Color::Black {
        e_king.inverse().rank().index()
    } else {
        e_king.rank().index()
    };

    F_TO_INDEX[f_rank] + E_TO_INDEX[e_rank]
}

/// i16 配列の要素和: dst[i] = a[i] + b[i] (SIMD 最適化)
#[cfg(feature = "nnue-threat")]
#[inline]
fn add_i16_arrays<const L1: usize>(dst: &mut [i16; L1], a: &[i16; L1], b: &[i16; L1]) {
    // AVX2 ループは `L1 / 16` 回で全要素を処理する前提。L1 が 16 の倍数で
    // ない場合は末端要素が取り残されるため、monomorphization 時に失敗させる。
    const {
        assert!(L1.is_multiple_of(16), "L1 must be a multiple of 16 for AVX2 SIMD loops");
    }
    // AVX2: 256bit = 16 x i16, L1/16 iterations
    #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
    {
        // SAFETY:
        // - 入力は 64 バイトアラインの AccumulatorLayerStacks 内の piece/Threat 配列。
        //   L1 は 16 の倍数なので視点ごとの開始位置も 32 バイトアライン。
        // - 出力は呼び出し元の Aligned<[i16; L1]>。store の 32 バイト要件を満たす。
        // - L1 / 16 回の load/store は各配列の範囲内に収まる。
        unsafe {
            use std::arch::x86_64::*;
            let dst_ptr = dst.as_mut_ptr();
            let a_ptr = a.as_ptr();
            let b_ptr = b.as_ptr();
            for i in 0..(L1 / 16) {
                let va = _mm256_load_si256(a_ptr.add(i * 16) as *const __m256i);
                let vb = _mm256_load_si256(b_ptr.add(i * 16) as *const __m256i);
                let result = _mm256_add_epi16(va, vb);
                _mm256_store_si256(dst_ptr.add(i * 16) as *mut __m256i, result);
            }
        }
    }

    // スカラーフォールバック（AVX2 非対応環境のみコンパイル）
    #[cfg(not(all(target_arch = "x86_64", target_feature = "avx2")))]
    for i in 0..L1 {
        dst[i] = a[i].wrapping_add(b[i]);
    }
}

/// LayerStacksアーキテクチャのNNUEネットワーク
///
/// `FT` は LS の Feature Transformer 軸 (5 種類のうち 1 つ) を表す marker type。
/// FT::DIMENSIONS 次元 + L1 次元 Feature Transformer + `num_buckets` 個の bucket
/// による LayerStacks。`num_buckets` は net file の header から読まれる
/// (ADR `2026-05-26`)。
pub struct NetworkLayerStacks<
    const L1: usize,
    const LS_L1_OUT: usize,
    const LS_L2_IN: usize,
    const LS_L2_PADDED_INPUT: usize,
    FT: LsFeatureSpec,
> {
    /// Feature Transformer (FT::DIMENSIONS → L1)
    pub feature_transformer: FeatureTransformerLayerStacks<L1, FT>,
    /// LayerStacks (`num_buckets` 個の bucket)
    pub layer_stacks: LayerStacks<L1, LS_L1_OUT, LS_L2_IN, LS_L2_PADDED_INPUT>,
    /// 評価値スケーリング係数（アーキテクチャ文字列から取得、USIオプションでオーバーライド可）
    pub fv_scale: i32,
    /// bucket 数 (= net file の `num_buckets` field、legacy `.bin` は 9)
    pub num_buckets: usize,
    _ft: PhantomData<FT>,
}

impl<
    const L1: usize,
    const LS_L1_OUT: usize,
    const LS_L2_IN: usize,
    const LS_L2_PADDED_INPUT: usize,
    FT: LsFeatureSpec,
> NetworkLayerStacks<L1, LS_L1_OUT, LS_L2_IN, LS_L2_PADDED_INPUT, FT>
{
    /// ファイルから読み込み
    pub fn load<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        Self::read(&mut reader)
    }

    /// リーダーから読み込み（PSQT は arch_str から自動検出）
    pub fn read<R: Read + Seek>(reader: &mut R) -> io::Result<Self> {
        Self::read_with_options(reader, None)
    }

    /// リーダーから読み込み（PSQT オーバーライドオプション付き）
    ///
    /// `psqt_override`:
    /// - `None`: arch_str から自動検出（デフォルト）
    /// - `Some(true)`: arch_str を無視して PSQT ブロックを読む
    /// - `Some(false)`: arch_str を無視して PSQT ブロックを読まない
    pub fn read_with_options<R: Read + Seek>(
        reader: &mut R,
        psqt_override: Option<bool>,
    ) -> io::Result<Self> {
        let mut buf4 = [0u8; 4];

        // version（呼び出し元 NNUENetwork::read で大枠の受理範囲を確認済み）
        // ここでは LayerStack として受理する 2 つ:
        // - `NNUE_VERSION_HALFKA` (= `0x7AF32F20`): num_buckets field 無し、格納数は 9
        // - `NNUE_VERSION_LAYERSTACK_NUM_BUCKETS` (= `0x7AF32F21`): arch_str 直後に
        //   格納 bucket 数を示す num_buckets u32 field を持つ self-describing layout
        // version は binary layout の判別にだけ使い、推論 routing の意味論は決めない。
        //
        // HalfKP version `NNUE_VERSION (0x7AF32F16)` を `NNUE_ARCHITECTURE=LayerStacks`
        // override 経由で本関数に渡されるケースの防衛: silent な偽 9-bucket 読込を
        // 避けるため、ここで明示的に reject する。
        reader.read_exact(&mut buf4)?;
        let version = u32::from_le_bytes(buf4);
        if version != NNUE_VERSION_HALFKA && version != NNUE_VERSION_LAYERSTACK_NUM_BUCKETS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "LayerStack reader expected version {NNUE_VERSION_HALFKA:#x} (legacy, \
                     implicit num_buckets=9) or {NNUE_VERSION_LAYERSTACK_NUM_BUCKETS:#x} \
                     (self-describing with num_buckets header), got {version:#x}. \
                     Non-LayerStack `.bin` cannot be dispatched to LayerStacks even via \
                     `NNUE_ARCHITECTURE=LayerStacks` override."
                ),
            ));
        }
        let has_num_buckets_field = version == NNUE_VERSION_LAYERSTACK_NUM_BUCKETS;

        // 構造ハッシュ
        reader.read_exact(&mut buf4)?;

        // アーキテクチャ文字列を読み込み
        reader.read_exact(&mut buf4)?;
        let arch_len = u32::from_le_bytes(buf4) as usize;
        if arch_len == 0 || arch_len > MAX_ARCH_LEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Invalid arch string length: {arch_len} (max: {MAX_ARCH_LEN})"),
            ));
        }
        let mut arch = vec![0u8; arch_len];
        reader.read_exact(&mut arch)?;

        // アーキテクチャ文字列を解析
        let arch_str = String::from_utf8_lossy(&arch);

        // num_buckets-header layout: u32 を arch_str 直後・ft_hash 直前に読む。
        // legacy: field 無し → DEFAULT_NUM_BUCKETS (9) として進める。
        // tatara `save_quantised` の write 順と対称 (version → network_hash →
        // arch_len → arch_str → num_buckets → ft_hash → FT/PSQT/LayerStack blocks)。
        let num_buckets = if has_num_buckets_field {
            reader.read_exact(&mut buf4)?;
            let n = u32::from_le_bytes(buf4) as usize;
            if n == 0 || n > MAX_LAYER_STACK_BUCKETS {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "NNUE LayerStack num_buckets={n} out of range (1..={MAX_LAYER_STACK_BUCKETS}). \
                         Rebuild rshogi-core with a larger MAX_LAYER_STACK_BUCKETS if needed \
                         (see ADR 2026-05-26)."
                    ),
                ));
            }
            n
        } else {
            DEFAULT_NUM_BUCKETS
        };

        #[cfg(feature = "nnue-effect-bucket")]
        {
            let model_config = super::spec::parse_effect_bucket_config(&arch_str).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "effect bucket build requires EffectBucket= or E4= token in arch string: {arch_str}"
                    ),
                )
            })?;
            if (model_config.nb, model_config.king_bucketed)
                != (EFFECT_BUCKET_NB, EFFECT_BUCKET_KING_BUCKETED)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "effect bucket config mismatch: model=({}x{}), engine=({}x{}). \
                         Use a model trained with the matching effect bucket config.",
                        model_config.nb,
                        if model_config.king_bucketed {
                            "bucketed"
                        } else {
                            "fixed"
                        },
                        EFFECT_BUCKET_NB,
                        if EFFECT_BUCKET_KING_BUCKETED {
                            "bucketed"
                        } else {
                            "fixed"
                        },
                    ),
                ));
            }
            let model_dims =
                super::spec::parse_feature_input_dimensions(&arch_str).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "effect bucket model is missing feature dimensions in arch string: {arch_str}"
                        ),
                    )
                })?;
            if model_dims != HALFKA_EFFECT_BUCKET_DIMENSIONS {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "effect bucket input dimensions mismatch: model={model_dims}, engine={HALFKA_EFFECT_BUCKET_DIMENSIONS}"
                    ),
                ));
            }
        }
        #[cfg(not(feature = "nnue-effect-bucket"))]
        if matches!(
            super::spec::detect_layer_stacks_feature(&arch_str),
            Ok(super::spec::FeatureSet::HalfKaHmMergedEffectBucket)
        ) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "effect bucket model requires nnue-effect-bucket feature",
            ));
        }

        // FV_SCALE 検出
        let fv_scale = parse_fv_scale_from_arch(&arch_str).unwrap_or(FV_SCALE_HALFKA);

        let threat_dimensions =
            super::spec::validate_layer_stacks_architecture_header(&arch_str)
                .map_err(|message| io::Error::new(io::ErrorKind::InvalidData, message))?;

        // Feature transformer hash を読み飛ばす
        reader.read_exact(&mut buf4)?;
        let _ft_hash = u32::from_le_bytes(buf4);

        // Feature Transformer を読み込み（圧縮形式を自動検出）
        // read_psqt/read_threat_weights と末尾の share_weights() で変更するため mut
        let mut feature_transformer = FeatureTransformerLayerStacks::read_leb128(reader)?;

        // PSQT 読み込み:
        // - psqt_override == Some(true): USI オプションで PSQT 強制 ON（arch_str を無視）
        // - psqt_override == Some(false): USI オプションで PSQT 強制 OFF（arch_str を無視）
        // - psqt_override == None: arch_str から自動検出
        #[cfg(feature = "nnue-psqt")]
        {
            let has_psqt = psqt_override.unwrap_or_else(|| arch_str.contains("PSQT="));
            if has_psqt {
                feature_transformer.read_psqt(reader, num_buckets)?;
            }
        }
        #[cfg(not(feature = "nnue-psqt"))]
        if psqt_override.unwrap_or_else(|| arch_str.contains("PSQT=")) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "PSQT model requires nnue-psqt feature",
            ));
        }

        // Threat 読み込み（arch_str に "Threat=" があれば）
        #[cfg(feature = "nnue-threat")]
        {
            // arch_str の `Threat=<dims>` を構造化 parse し compiled THREAT_DIMENSIONS と
            // 照合。tatara export は profile の dims を必ず書くため、不一致は engine と
            // model の profile / feature set 不整合を意味する (旧 profile 0 net の
            // Threat=216720 は engine profile 0 のとき通る)。
            if let Some(model_dims) = threat_dimensions {
                let engine_dims = super::threat_features::THREAT_DIMENSIONS;
                if model_dims != engine_dims {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "Threat dims mismatch: model={model_dims}, engine={engine_dims}. \
                             Use a model trained with the matching threat profile / feature set."
                        ),
                    ));
                }
                // ThreatProfile= が arch_str にあれば profile id を読み込み検証
                // なければ旧モデル (profile 0): profile id フィールド無し
                let has_profile_field = arch_str.contains("ThreatProfile=");
                if has_profile_field {
                    reader.read_exact(&mut buf4)?;
                    let model_profile_id = u32::from_le_bytes(buf4);
                    let engine_profile_id = super::threat_exclusion::THREAT_PROFILE_ID;
                    if model_profile_id != engine_profile_id {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "Threat profile mismatch: model={model_profile_id}, engine={engine_profile_id}"
                            ),
                        ));
                    }
                } else {
                    // 旧モデル: profile id フィールドなし → profile 0 と見なす
                    let engine_profile_id = super::threat_exclusion::THREAT_PROFILE_ID;
                    if engine_profile_id != 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "Old model (no ThreatProfile) requires engine profile 0, \
                                 but engine has profile {engine_profile_id}. \
                                 Use a model trained with the matching exclusion profile."
                            ),
                        ));
                    }
                }
                feature_transformer.read_threat_weights(reader)?;
            }
        }
        #[cfg(not(feature = "nnue-threat"))]
        if threat_dimensions.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Threat model requires nnue-threat feature",
            ));
        }

        // LayerStacks を読み込み（FC 層は常に非圧縮、num_buckets 個分）
        let layer_stacks = LayerStacks::read(reader, num_buckets)?;

        // EOF検証: 余りデータがないことを確認
        // factorizedモデル（非coalesced）を誤って読んだ場合、
        // 余りデータが発生する可能性がある。
        let mut probe = [0u8; 1];
        match reader.read(&mut probe) {
            Ok(0) => {
                // EOF到達 - 正常（coalesce済みモデル）
            }
            Ok(_) => {
                // 余りデータあり - おそらくfactorizedモデル
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "NNUE file has unexpected trailing data.\n\
                     This likely indicates a factorized (non-coalesced) model.\n\
                     This engine only supports coalesced models.\n\n\
                     To fix: Re-export the model using nnue-pytorch serialize.py:\n\
                       python serialize.py model.ckpt output.nnue\n\n\
                     The serialize.py script automatically coalesces factor weights.",
                ));
            }
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                // EOF - 正常
            }
            Err(e) => {
                // その他のIOエラー
                return Err(e);
            }
        }

        // 診断ログを出力
        #[cfg(feature = "diagnostics")]
        {
            Self::log_load_diagnostics(&feature_transformer, &layer_stacks);
        }

        // 重みをプロセス間共有メモリへ移行（多プロセス時のメモリ常駐・L3 競合を削減）。
        // ネットワーク構築完了後・採用前に 1 回だけ実行する。
        feature_transformer.share_weights();

        Ok(Self {
            feature_transformer,
            layer_stacks,
            fv_scale,
            num_buckets,
            _ft: PhantomData,
        })
    }

    /// 読み込み時の診断ログを出力
    #[cfg(feature = "diagnostics")]
    fn log_load_diagnostics(
        ft: &FeatureTransformerLayerStacks<L1, FT>,
        ls: &LayerStacks<L1, LS_L1_OUT, LS_L2_IN, LS_L2_PADDED_INPUT>,
    ) {
        // FT統計
        let bias_sum: i64 = ft.biases.0.iter().map(|&x| x as i64).sum();
        let weight_min = ft.weights.iter().copied().min().unwrap_or(0);
        let weight_max = ft.weights.iter().copied().max().unwrap_or(0);
        let weight_nonzero: usize = ft.weights.iter().filter(|&&x| x != 0).count();
        let weight_total = ft.weights.len();

        info!("[NNUE Load] FT bias sum: {bias_sum}");
        info!("[NNUE Load] FT weight: min={weight_min}, max={weight_max}");
        info!(
            "[NNUE Load] FT weight nonzero: {weight_nonzero}/{weight_total} ({:.2}%)",
            weight_nonzero as f64 / weight_total as f64 * 100.0
        );

        // LayerStacks bucket0 の l1_biases
        let l1_biases = &ls.buckets[0].l1.biases;
        info!("[NNUE Load] LayerStacks bucket0 l1_biases: {l1_biases:?}");
    }

    /// バイト列から読み込み
    pub fn from_bytes(bytes: &[u8]) -> io::Result<Self> {
        let mut cursor = Cursor::new(bytes);
        Self::read(&mut cursor)
    }

    #[cfg(feature = "layerstack-arch")]
    pub(crate) fn net_tensor_shape(&self, kind: NetTensorKind) -> NetTensorShape {
        match kind {
            NetTensorKind::OutputWeight => NetTensorShape {
                bucket_count: Some(self.num_buckets),
                element_count: AffineTransform::<NNUE_PYTORCH_L3, 1>::weight_len(),
            },
            NetTensorKind::OutputBias => NetTensorShape {
                bucket_count: Some(self.num_buckets),
                element_count: 1,
            },
            NetTensorKind::FtBias => NetTensorShape {
                bucket_count: None,
                element_count: L1,
            },
            NetTensorKind::L2Weight => NetTensorShape {
                bucket_count: Some(self.num_buckets),
                element_count: AffineTransform::<LS_L2_IN, NNUE_PYTORCH_L3>::weight_len(),
            },
        }
    }

    #[cfg(feature = "layerstack-arch")]
    pub(crate) fn net_coefficient(&self, id: &NetCoefficientId) -> i32 {
        match id.kind {
            NetTensorKind::OutputWeight => i32::from(
                self.layer_stacks.buckets[id.bucket.expect("validated bucket")]
                    .output
                    .file_weight(id.index),
            ),
            NetTensorKind::OutputBias => {
                self.layer_stacks.buckets[id.bucket.expect("validated bucket")].output.biases[0]
            }
            NetTensorKind::FtBias => i32::from(self.feature_transformer.biases.0[id.index]),
            NetTensorKind::L2Weight => i32::from(
                self.layer_stacks.buckets[id.bucket.expect("validated bucket")]
                    .l2
                    .file_weight(id.index),
            ),
        }
    }

    #[cfg(feature = "layerstack-arch")]
    pub(crate) fn apply_net_delta(&mut self, id: &NetCoefficientId, delta: i32) -> bool {
        match id.kind {
            NetTensorKind::OutputWeight => self.layer_stacks.buckets
                [id.bucket.expect("validated bucket")]
            .output
            .apply_file_weight_delta(id.index, delta),
            NetTensorKind::OutputBias => {
                let bias = &mut self.layer_stacks.buckets[id.bucket.expect("validated bucket")]
                    .output
                    .biases[0];
                let (value, clamped) = add_i32_delta(*bias, delta);
                *bias = value;
                clamped
            }
            NetTensorKind::FtBias => {
                let bias = &mut self.feature_transformer.biases.0[id.index];
                let (value, clamped) = add_i16_delta(*bias, delta);
                *bias = value;
                clamped
            }
            NetTensorKind::L2Weight => self.layer_stacks.buckets
                [id.bucket.expect("validated bucket")]
            .l2
            .apply_file_weight_delta(id.index, delta),
        }
    }

    /// 評価値を計算
    ///
    pub fn evaluate(&self, pos: &Position, acc: &AccumulatorLayerStacks<L1>) -> Value {
        let side_to_move = pos.side_to_move();
        let bucket_index = compute_layer_stacks_bucket_index(pos, side_to_move, self.num_buckets);
        self.evaluate_with_bucket(pos, acc, bucket_index)
    }

    // forward と診断で同じ piece + Threat 因子を使用する。PSQT は別経路。
    #[inline]
    fn with_combined_accumulators<R>(
        &self,
        acc: &AccumulatorLayerStacks<L1>,
        side_to_move: Color,
        f: impl FnOnce(&[i16; L1], &[i16; L1]) -> R,
    ) -> R {
        let (us, them) = if side_to_move == Color::Black {
            (acc.get(0), acc.get(1))
        } else {
            (acc.get(1), acc.get(0))
        };
        #[cfg(feature = "nnue-threat")]
        if self.feature_transformer.has_threat {
            let (us_threat, them_threat) = if side_to_move == Color::Black {
                (acc.get_threat(0), acc.get_threat(1))
            } else {
                (acc.get_threat(1), acc.get_threat(0))
            };
            let mut us_combined = Aligned([0i16; L1]);
            let mut them_combined = Aligned([0i16; L1]);
            add_i16_arrays(&mut us_combined.0, us, us_threat);
            add_i16_arrays(&mut them_combined.0, them, them_threat);
            return f(&us_combined.0, &them_combined.0);
        }
        f(us, them)
    }

    /// forward と同じ因子・入力から活性飽和数を計測する。
    ///
    /// 戻り値は両視点の FT 因子が 127 以上となる数（総数は 2 * L1）。
    /// `counts` には指定 bucket の dense 活性飽和数を加算する。
    /// Threat 有効時は i16 の wrapping 和を使い、PSQT は含めない。
    pub fn accumulate_saturation_counts(
        &self,
        acc: &AccumulatorLayerStacks<L1>,
        side_to_move: Color,
        bucket_index: usize,
        counts: &mut LsSaturationCounts,
    ) -> u64 {
        self.with_combined_accumulators(acc, side_to_move, |us, them| {
            let ft_saturated = us.iter().chain(them.iter()).filter(|&&x| x >= 127).count() as u64;
            let mut transformed = Aligned([0u8; L1]);
            sqr_clipped_relu_transform(us, them, &mut transformed.0);
            self.layer_stacks.buckets[bucket_index]
                .propagate_counting_saturation(&transformed.0, counts);
            ft_saturated
        })
    }

    /// 評価値を計算（事前計算済み bucket index を使用）
    pub fn evaluate_with_bucket(
        &self,
        pos: &Position,
        acc: &AccumulatorLayerStacks<L1>,
        bucket_index: usize,
    ) -> Value {
        let side_to_move = pos.side_to_move();

        let transformed = self.with_combined_accumulators(acc, side_to_move, |us, them| {
            sqr_clipped_relu_new(us, them)
        });

        // LayerStacks で評価
        let raw_score = self.layer_stacks.evaluate_raw(bucket_index, &transformed.0);

        // PSQT ショートカット (Stockfish 準拠: (stm - nstm) / 2)
        // 各駒は両視点に逆符号で寄与するため、stm - nstm は正味の配置価値を
        // 約2倍にカウントする。/2 はこの二重カウントを補正する正規化。
        #[cfg(feature = "nnue-psqt")]
        let psqt_value = if self.feature_transformer.has_psqt {
            let stm = side_to_move as usize;
            let nstm = (!side_to_move) as usize;
            (acc.psqt_accumulation[stm][bucket_index] - acc.psqt_accumulation[nstm][bucket_index])
                / 2
        } else {
            0
        };
        #[cfg(not(feature = "nnue-psqt"))]
        let psqt_value = 0;

        let fv_scale = get_fv_scale_override().unwrap_or(self.fv_scale);
        Value::new(raw_score.saturating_add(psqt_value) / fv_scale)
    }

    /// 評価値を計算（詳細診断ログ付き）
    ///
    /// Python (nnue-pytorch) との比較検証用。
    /// 各中間値をログ出力する。
    #[cfg(feature = "diagnostics")]
    pub fn evaluate_with_diagnostics(
        &self,
        pos: &Position,
        acc: &AccumulatorLayerStacks<L1>,
    ) -> Value {
        use log::info;

        let side_to_move = pos.side_to_move();

        // アキュムレータの統計
        let (us_acc, them_acc) = if side_to_move == Color::Black {
            (acc.get(Color::Black as usize), acc.get(Color::White as usize))
        } else {
            (acc.get(Color::White as usize), acc.get(Color::Black as usize))
        };

        // us_acc の統計
        let us_min = us_acc.iter().copied().min().unwrap_or(0);
        let us_max = us_acc.iter().copied().max().unwrap_or(0);
        let us_first_half_positive: usize = us_acc[0..L1 / 2].iter().filter(|&&x| x > 0).count();
        let us_second_half_positive: usize = us_acc[L1 / 2..L1].iter().filter(|&&x| x > 0).count();

        info!("[NNUE Eval] us_acc: min={us_min}, max={us_max}");
        let half = L1 / 2;
        info!(
            "[NNUE Eval] us_acc positive: first_half={us_first_half_positive}/{half}, second_half={us_second_half_positive}/{half}"
        );
        info!("[NNUE Eval] us_acc (piece) first 16: {:?}", &us_acc[0..16]);

        // Threat 結合 (evaluate_with_bucket と同一ロジック)
        let mut transformed: Aligned<[u8; L1]> = Aligned([0u8; L1]);
        #[cfg(feature = "nnue-threat")]
        {
            if self.feature_transformer.has_threat {
                let mut us_combined = [0i16; L1];
                let mut them_combined = [0i16; L1];
                us_combined.copy_from_slice(us_acc);
                them_combined.copy_from_slice(them_acc);

                let (us_t, them_t) = if side_to_move == Color::Black {
                    (acc.get_threat(Color::Black as usize), acc.get_threat(Color::White as usize))
                } else {
                    (acc.get_threat(Color::White as usize), acc.get_threat(Color::Black as usize))
                };
                info!("[NNUE Eval] us_threat first 16: {:?}", &us_t[0..16]);
                for i in 0..L1 {
                    us_combined[i] = us_combined[i].wrapping_add(us_t[i]);
                    them_combined[i] = them_combined[i].wrapping_add(them_t[i]);
                }

                info!("[NNUE Eval] us_combined (piece+threat) first 16: {:?}", &us_combined[0..16]);
                sqr_clipped_relu_transform(&us_combined, &them_combined, &mut transformed.0);
            } else {
                sqr_clipped_relu_transform(us_acc, them_acc, &mut transformed.0);
            }
        }
        #[cfg(not(feature = "nnue-threat"))]
        {
            sqr_clipped_relu_transform(us_acc, them_acc, &mut transformed.0);
        }

        let transformed_nonzero: usize = transformed.0.iter().filter(|&&x| x > 0).count();
        let transformed_sum: u64 = transformed.0.iter().map(|&x| x as u64).sum();
        info!("[NNUE Eval] transformed: nonzero={transformed_nonzero}/{L1}, sum={transformed_sum}");
        info!("[NNUE Eval] transformed first 32: {:?}", &transformed.0[0..32]);

        // バケットインデックスを計算（通常パスと同じ共通関数を使用）
        let bucket_index = compute_layer_stacks_bucket_index(pos, side_to_move, self.num_buckets);
        info!(
            "[NNUE Eval] bucket_mode={:?}, bucket_index={bucket_index}",
            get_layer_stack_bucket_mode()
        );

        // LayerStacks で評価（詳細ログ付き）
        let (raw_score, l1_out, l1_skip) =
            self.layer_stacks.evaluate_raw_with_diagnostics(bucket_index, &transformed.0);

        info!("[NNUE Eval] l1_out (16 elements): {l1_out:?}");
        info!("[NNUE Eval] l1_skip: {l1_skip}");
        info!("[NNUE Eval] raw_score (with skip): {raw_score}");

        // PSQT ショートカット
        #[cfg(feature = "nnue-psqt")]
        let psqt_value = if self.feature_transformer.has_psqt {
            let stm = side_to_move as usize;
            let nstm = (!side_to_move) as usize;
            let v = (acc.psqt_accumulation[stm][bucket_index]
                - acc.psqt_accumulation[nstm][bucket_index])
                / 2;
            info!(
                "[NNUE Eval] psqt_acc[stm][{bucket_index}]: {}",
                acc.psqt_accumulation[stm][bucket_index]
            );
            info!(
                "[NNUE Eval] psqt_acc[nstm][{bucket_index}]: {}",
                acc.psqt_accumulation[nstm][bucket_index]
            );
            info!("[NNUE Eval] psqt_value: {v}");
            v
        } else {
            info!("[NNUE Eval] PSQT: disabled");
            0
        };
        #[cfg(not(feature = "nnue-psqt"))]
        let psqt_value = {
            info!("[NNUE Eval] PSQT: disabled (feature not enabled)");
            0
        };

        let fv_scale = get_fv_scale_override().unwrap_or(self.fv_scale);
        let combined = raw_score.saturating_add(psqt_value);
        let score = combined / fv_scale;
        let score_float = combined as f64 / fv_scale as f64;
        info!("[NNUE Eval] fv_scale: {fv_scale}");
        info!(
            "[NNUE Eval] score: {score} (raw_score={raw_score} + psqt={psqt_value} = {combined}, float: {score_float:.4})"
        );

        Value::new(score)
    }

    /// 差分計算を使わずにAccumulatorを計算
    pub fn refresh_accumulator(&self, pos: &Position, acc: &mut AccumulatorLayerStacks<L1>) {
        self.feature_transformer.refresh_accumulator(pos, acc);
    }

    /// 差分計算を使わずにAccumulatorを計算（キャッシュ使用版）
    pub fn refresh_accumulator_with_cache(
        &self,
        pos: &Position,
        acc: &mut AccumulatorLayerStacks<L1>,
        cache: &mut super::accumulator_layer_stacks::AccumulatorCacheLayerStacks<L1>,
    ) {
        self.feature_transformer.refresh_accumulator_with_cache(pos, acc, cache);
    }

    /// 差分計算でAccumulatorを更新
    pub fn update_accumulator(
        &self,
        pos: &Position,
        dirty_piece: &super::accumulator::DirtyPiece,
        acc: &mut AccumulatorLayerStacks<L1>,
        prev_acc: &AccumulatorLayerStacks<L1>,
    ) {
        self.feature_transformer.update_accumulator(pos, dirty_piece, acc, prev_acc);
    }

    /// 差分計算でAccumulatorを更新（キャッシュ使用版）
    pub fn update_accumulator_with_cache(
        &self,
        pos: &Position,
        dirty_piece: &super::accumulator::DirtyPiece,
        acc: &mut AccumulatorLayerStacks<L1>,
        prev_acc: &AccumulatorLayerStacks<L1>,
        cache: &mut super::accumulator_layer_stacks::AccumulatorCacheLayerStacks<L1>,
    ) {
        self.feature_transformer.update_accumulator_with_cache(
            pos,
            dirty_piece,
            acc,
            prev_acc,
            cache,
        );
    }

    /// 複数手分の差分を適用してアキュムレータを更新
    pub fn forward_update_incremental(
        &self,
        pos: &Position,
        stack: &mut AccumulatorStackLayerStacks<L1>,
        source_idx: usize,
    ) -> bool {
        self.feature_transformer.forward_update_incremental(pos, stack, source_idx)
    }
}

// 旧 alias (HalfKaHmMerged 固定の名前): 既存 tools / tests からの参照を切らないため
// 互換のため保持する。新規コードは `NetworkLayerStacks<L1, ..., FT>` を直接書く。
#[cfg(all(feature = "layerstacks-1536x16x32", feature = "ft-halfka_hm_merged"))]
pub type NetworkLayerStacks1536x16x32 = NetworkLayerStacks<
    1536,
    LAYER_STACK_16X32_L1_OUT,
    LAYER_STACK_16X32_L2_IN,
    32,
    HalfKaHmMergedSpec,
>;
#[cfg(all(feature = "layerstacks-1536x32x32", feature = "ft-halfka_hm_merged"))]
pub type NetworkLayerStacks1536x32x32 = NetworkLayerStacks<
    1536,
    LAYER_STACK_32X32_L1_OUT,
    LAYER_STACK_32X32_L2_IN,
    64,
    HalfKaHmMergedSpec,
>;
#[cfg(all(feature = "layerstacks-768x16x32", feature = "ft-halfka_hm_merged"))]
pub type NetworkLayerStacks768x16x32 = NetworkLayerStacks<
    768,
    LAYER_STACK_16X32_L1_OUT,
    LAYER_STACK_16X32_L2_IN,
    32,
    HalfKaHmMergedSpec,
>;
#[cfg(all(feature = "layerstacks-768x8x32", feature = "ft-halfka_hm_merged"))]
pub type NetworkLayerStacks768x8x32 = NetworkLayerStacks<
    768,
    LAYER_STACK_8X32_L1_OUT,
    LAYER_STACK_8X32_L2_IN,
    32,
    HalfKaHmMergedSpec,
>;
#[cfg(all(feature = "layerstacks-512x16x32", feature = "ft-halfka_hm_merged"))]
pub type NetworkLayerStacks512x16x32 = NetworkLayerStacks<
    512,
    LAYER_STACK_16X32_L1_OUT,
    LAYER_STACK_16X32_L2_IN,
    32,
    HalfKaHmMergedSpec,
>;
#[cfg(all(feature = "layerstacks-1024x16x32", feature = "ft-halfka_hm_merged"))]
pub type NetworkLayerStacks1024x16x32 = NetworkLayerStacks<
    1024,
    LAYER_STACK_16X32_L1_OUT,
    LAYER_STACK_16X32_L2_IN,
    32,
    HalfKaHmMergedSpec,
>;
#[cfg(all(feature = "layerstacks-3072x16x32", feature = "ft-halfka_hm_merged"))]
pub type NetworkLayerStacks3072x16x32 = NetworkLayerStacks<
    3072,
    LAYER_STACK_16X32_L1_OUT,
    LAYER_STACK_16X32_L2_IN,
    32,
    HalfKaHmMergedSpec,
>;

// =============================================================================
// LayerStacksNetwork - 2-tier (FT, L1) dispatch enum
// =============================================================================

/// LayerStacks ネットワークの FT 別内部 enum。L1 サイズ軸を持つ。
///
/// 外側の `LayerStacksNetwork` が FT 軸 (5 種類のうち 1 つ) で dispatch し、
/// この内部 enum が L1 軸 (5 サイズのうち 1 つ) で dispatch する。
///
/// **重要**: 大会ビルドでは必ず単一 (FT, L1) のみを有効化すること。複数
/// バリアントを同時有効にすると dispatch match の overhead が出る (実測 ~5%)。
pub enum LsNetByFt<FT: LsFeatureSpec + 'static> {
    #[cfg(feature = "layerstacks-1536x16x32")]
    L1536x16x32(
        Box<NetworkLayerStacks<1536, LAYER_STACK_16X32_L1_OUT, LAYER_STACK_16X32_L2_IN, 32, FT>>,
    ),
    #[cfg(feature = "layerstacks-1536x32x32")]
    L1536x32x32(
        Box<NetworkLayerStacks<1536, LAYER_STACK_32X32_L1_OUT, LAYER_STACK_32X32_L2_IN, 64, FT>>,
    ),
    #[cfg(feature = "layerstacks-1024x16x32")]
    L1024x16x32(
        Box<NetworkLayerStacks<1024, LAYER_STACK_16X32_L1_OUT, LAYER_STACK_16X32_L2_IN, 32, FT>>,
    ),
    #[cfg(feature = "layerstacks-3072x16x32")]
    L3072x16x32(
        Box<NetworkLayerStacks<3072, LAYER_STACK_16X32_L1_OUT, LAYER_STACK_16X32_L2_IN, 32, FT>>,
    ),
    #[cfg(feature = "layerstacks-768x16x32")]
    L768x16x32(
        Box<NetworkLayerStacks<768, LAYER_STACK_16X32_L1_OUT, LAYER_STACK_16X32_L2_IN, 32, FT>>,
    ),
    #[cfg(feature = "layerstacks-768x8x32")]
    L768x8x32(
        Box<NetworkLayerStacks<768, LAYER_STACK_8X32_L1_OUT, LAYER_STACK_8X32_L2_IN, 32, FT>>,
    ),
    #[cfg(feature = "layerstacks-512x16x32")]
    L512x16x32(
        Box<NetworkLayerStacks<512, LAYER_STACK_16X32_L1_OUT, LAYER_STACK_16X32_L2_IN, 32, FT>>,
    ),
    #[cfg(not(any(
        feature = "layerstacks-1536x16x32",
        feature = "layerstacks-1536x32x32",
        feature = "layerstacks-768x16x32",
        feature = "layerstacks-768x8x32",
        feature = "layerstacks-512x16x32",
        feature = "layerstacks-1024x16x32",
        feature = "layerstacks-3072x16x32",
    )))]
    _Unused(std::convert::Infallible, PhantomData<FT>),
}

/// `LsNetByFt<FT>` の variants 上で同じ式を展開する dispatch マクロ。
///
/// すべての layerstacks-* feature が無効の場合 (例: WASM ビルド) は本来到達不能で、
/// wildcard arm が必要になる。
macro_rules! ls_match_size {
    ($val:expr, $pat:ident => $body:expr) => {
        match $val {
            #[cfg(feature = "layerstacks-1536x16x32")]
            LsNetByFt::L1536x16x32($pat) => $body,
            #[cfg(feature = "layerstacks-1536x32x32")]
            LsNetByFt::L1536x32x32($pat) => $body,
            #[cfg(feature = "layerstacks-768x16x32")]
            LsNetByFt::L768x16x32($pat) => $body,
            #[cfg(feature = "layerstacks-768x8x32")]
            LsNetByFt::L768x8x32($pat) => $body,
            #[cfg(feature = "layerstacks-512x16x32")]
            LsNetByFt::L512x16x32($pat) => $body,
            #[cfg(feature = "layerstacks-1024x16x32")]
            LsNetByFt::L1024x16x32($pat) => $body,
            #[cfg(feature = "layerstacks-3072x16x32")]
            LsNetByFt::L3072x16x32($pat) => $body,
            #[cfg(not(any(
                feature = "layerstacks-1536x16x32",
                feature = "layerstacks-1536x32x32",
                feature = "layerstacks-768x16x32",
                feature = "layerstacks-768x8x32",
                feature = "layerstacks-512x16x32",
                feature = "layerstacks-1024x16x32",
                feature = "layerstacks-3072x16x32",
            )))]
            _ => unreachable!("no LayerStacks size variant enabled"),
        }
    };
}

impl<FT: LsFeatureSpec + 'static> LsNetByFt<FT> {
    /// L1 サイズを取得
    pub fn l1_size(&self) -> usize {
        match self {
            #[cfg(feature = "layerstacks-1536x16x32")]
            Self::L1536x16x32(_) => 1536,
            #[cfg(feature = "layerstacks-1536x32x32")]
            Self::L1536x32x32(_) => 1536,
            #[cfg(feature = "layerstacks-768x16x32")]
            Self::L768x16x32(_) => 768,
            #[cfg(feature = "layerstacks-768x8x32")]
            Self::L768x8x32(_) => 768,
            #[cfg(feature = "layerstacks-512x16x32")]
            Self::L512x16x32(_) => 512,
            #[cfg(feature = "layerstacks-1024x16x32")]
            Self::L1024x16x32(_) => 1024,
            #[cfg(feature = "layerstacks-3072x16x32")]
            Self::L3072x16x32(_) => 3072,
            #[cfg(not(any(
                feature = "layerstacks-1536x16x32",
                feature = "layerstacks-1536x32x32",
                feature = "layerstacks-768x16x32",
                feature = "layerstacks-768x8x32",
                feature = "layerstacks-512x16x32",
                feature = "layerstacks-1024x16x32",
                feature = "layerstacks-3072x16x32",
            )))]
            _ => unreachable!("no LayerStacks size variant enabled"),
        }
    }

    /// (L1, L2, L3) を取得
    pub fn architecture_dims(&self) -> (usize, usize, usize) {
        match self {
            #[cfg(feature = "layerstacks-1536x16x32")]
            Self::L1536x16x32(_) => (1536, 16, 32),
            #[cfg(feature = "layerstacks-1536x32x32")]
            Self::L1536x32x32(_) => (1536, 32, 32),
            #[cfg(feature = "layerstacks-768x16x32")]
            Self::L768x16x32(_) => (768, 16, 32),
            #[cfg(feature = "layerstacks-768x8x32")]
            Self::L768x8x32(_) => (768, 8, 32),
            #[cfg(feature = "layerstacks-512x16x32")]
            Self::L512x16x32(_) => (512, 16, 32),
            #[cfg(feature = "layerstacks-1024x16x32")]
            Self::L1024x16x32(_) => (1024, 16, 32),
            #[cfg(feature = "layerstacks-3072x16x32")]
            Self::L3072x16x32(_) => (3072, 16, 32),
            #[cfg(not(any(
                feature = "layerstacks-1536x16x32",
                feature = "layerstacks-1536x32x32",
                feature = "layerstacks-768x16x32",
                feature = "layerstacks-768x8x32",
                feature = "layerstacks-512x16x32",
                feature = "layerstacks-1024x16x32",
                feature = "layerstacks-3072x16x32",
            )))]
            _ => unreachable!("no LayerStacks size variant enabled"),
        }
    }

    /// アーキテクチャ仕様を取得
    pub fn architecture_spec(&self) -> super::spec::ArchitectureSpec {
        let (l1, l2, l3) = self.architecture_dims();
        super::spec::ArchitectureSpec::new(
            super::spec::FeatureSet::LayerStacks,
            l1,
            l2,
            l3,
            super::spec::Activation::CReLU,
        )
    }

    /// FV_SCALE を取得
    pub fn fv_scale(&self) -> i32 {
        ls_match_size!(self, net => net.fv_scale)
    }

    /// 現在 load されている net の bucket 数 (= `.bin` header の `num_buckets`)
    pub fn num_buckets(&self) -> usize {
        ls_match_size!(self, net => net.num_buckets)
    }

    #[cfg(feature = "layerstack-arch")]
    pub(crate) fn net_tensor_shape(&self, kind: NetTensorKind) -> NetTensorShape {
        ls_match_size!(self, net => net.net_tensor_shape(kind))
    }

    #[cfg(feature = "layerstack-arch")]
    pub(crate) fn net_coefficient(&self, id: &NetCoefficientId) -> i32 {
        ls_match_size!(self, net => net.net_coefficient(id))
    }

    #[cfg(feature = "layerstack-arch")]
    pub(crate) fn apply_net_delta(&mut self, id: &NetCoefficientId, delta: i32) -> bool {
        ls_match_size!(self, net => net.apply_net_delta(id, delta))
    }

    /// (L1, L2, L3) と PSQT override から読み込み (FT は型レベルで固定)。
    #[cfg(feature = "layerstack-arch")]
    fn read_with_options<R: Read + Seek>(
        reader: &mut R,
        l1: usize,
        l2: usize,
        l3: usize,
        psqt_override: Option<bool>,
    ) -> io::Result<Self> {
        match (l1, l2, l3) {
            #[cfg(feature = "layerstacks-1536x16x32")]
            (1536, 16, 32) => {
                let net = NetworkLayerStacks::<
                    1536,
                    LAYER_STACK_16X32_L1_OUT,
                    LAYER_STACK_16X32_L2_IN,
                    32,
                    FT,
                >::read_with_options(reader, psqt_override)?;
                Ok(Self::L1536x16x32(Box::new(net)))
            }
            #[cfg(feature = "layerstacks-1536x32x32")]
            (1536, 32, 32) => {
                let net = NetworkLayerStacks::<
                    1536,
                    LAYER_STACK_32X32_L1_OUT,
                    LAYER_STACK_32X32_L2_IN,
                    64,
                    FT,
                >::read_with_options(reader, psqt_override)?;
                Ok(Self::L1536x32x32(Box::new(net)))
            }
            #[cfg(feature = "layerstacks-768x16x32")]
            (768, 16, 32) => {
                let net = NetworkLayerStacks::<
                    768,
                    LAYER_STACK_16X32_L1_OUT,
                    LAYER_STACK_16X32_L2_IN,
                    32,
                    FT,
                >::read_with_options(reader, psqt_override)?;
                Ok(Self::L768x16x32(Box::new(net)))
            }
            #[cfg(feature = "layerstacks-768x8x32")]
            (768, 8, 32) => {
                let net = NetworkLayerStacks::<
                    768,
                    LAYER_STACK_8X32_L1_OUT,
                    LAYER_STACK_8X32_L2_IN,
                    32,
                    FT,
                >::read_with_options(reader, psqt_override)?;
                Ok(Self::L768x8x32(Box::new(net)))
            }
            #[cfg(feature = "layerstacks-512x16x32")]
            (512, 16, 32) => {
                let net = NetworkLayerStacks::<
                    512,
                    LAYER_STACK_16X32_L1_OUT,
                    LAYER_STACK_16X32_L2_IN,
                    32,
                    FT,
                >::read_with_options(reader, psqt_override)?;
                Ok(Self::L512x16x32(Box::new(net)))
            }
            #[cfg(feature = "layerstacks-1024x16x32")]
            (1024, 16, 32) => {
                let net = NetworkLayerStacks::<
                    1024,
                    LAYER_STACK_16X32_L1_OUT,
                    LAYER_STACK_16X32_L2_IN,
                    32,
                    FT,
                >::read_with_options(reader, psqt_override)?;
                Ok(Self::L1024x16x32(Box::new(net)))
            }
            #[cfg(feature = "layerstacks-3072x16x32")]
            (3072, 16, 32) => {
                let net = NetworkLayerStacks::<
                    3072,
                    LAYER_STACK_16X32_L1_OUT,
                    LAYER_STACK_16X32_L2_IN,
                    32,
                    FT,
                >::read_with_options(reader, psqt_override)?;
                Ok(Self::L3072x16x32(Box::new(net)))
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Unsupported LayerStacks architecture: {l1}x{l2}x{l3}"),
            )),
        }
    }

    /// 評価値を計算 (stack の L1 と一致する variant 上で実行)。
    #[cfg(feature = "layerstack-arch")]
    pub fn evaluate(
        &self,
        pos: &Position,
        stack: &super::accumulator_layer_stacks::LayerStacksAccStack,
    ) -> Value {
        // (self, stack) tuple match で同じ L1 variant の組のみ matched arm を持つ。
        // 2 サイズ以上 enable のときだけ cross-pair の不一致 arm が到達可能で、
        // 単一 size build では 1 arm の match 自体が exhaustive となる。
        //
        // 以下の 21-pair (C(7,2)) cfg は本 file 内で 4 箇所 (本 fallback / update_accumulator
        // の net_dims / stack_dims / fallback) に同じ式を持つ。LS サイズ追加時は
        // すべての any(all(...)) を C(N,2) に揃えて同期更新すること (match arm は
        // item ではないため共通 cfg を file-local macro に括り出せない)。
        match (self, stack) {
            #[cfg(feature = "layerstacks-1536x16x32")]
            (
                Self::L1536x16x32(net),
                super::accumulator_layer_stacks::LayerStacksAccStack::L1536x16x32(st),
            ) => net.evaluate(pos, &st.current().accumulator),
            #[cfg(feature = "layerstacks-1536x32x32")]
            (
                Self::L1536x32x32(net),
                super::accumulator_layer_stacks::LayerStacksAccStack::L1536x32x32(st),
            ) => net.evaluate(pos, &st.current().accumulator),
            #[cfg(feature = "layerstacks-768x16x32")]
            (
                Self::L768x16x32(net),
                super::accumulator_layer_stacks::LayerStacksAccStack::L768x16x32(st),
            ) => net.evaluate(pos, &st.current().accumulator),
            #[cfg(feature = "layerstacks-768x8x32")]
            (
                Self::L768x8x32(net),
                super::accumulator_layer_stacks::LayerStacksAccStack::L768x8x32(st),
            ) => net.evaluate(pos, &st.current().accumulator),
            #[cfg(feature = "layerstacks-512x16x32")]
            (
                Self::L512x16x32(net),
                super::accumulator_layer_stacks::LayerStacksAccStack::L512x16x32(st),
            ) => net.evaluate(pos, &st.current().accumulator),
            #[cfg(feature = "layerstacks-1024x16x32")]
            (
                Self::L1024x16x32(net),
                super::accumulator_layer_stacks::LayerStacksAccStack::L1024x16x32(st),
            ) => net.evaluate(pos, &st.current().accumulator),
            #[cfg(feature = "layerstacks-3072x16x32")]
            (
                Self::L3072x16x32(net),
                super::accumulator_layer_stacks::LayerStacksAccStack::L3072x16x32(st),
            ) => net.evaluate(pos, &st.current().accumulator),
            #[cfg(any(
                all(feature = "layerstacks-1536x16x32", feature = "layerstacks-1536x32x32"),
                all(feature = "layerstacks-1536x16x32", feature = "layerstacks-768x16x32"),
                all(feature = "layerstacks-1536x16x32", feature = "layerstacks-768x8x32"),
                all(feature = "layerstacks-1536x16x32", feature = "layerstacks-512x16x32"),
                all(feature = "layerstacks-1536x32x32", feature = "layerstacks-768x16x32"),
                all(feature = "layerstacks-1536x32x32", feature = "layerstacks-768x8x32"),
                all(feature = "layerstacks-1536x32x32", feature = "layerstacks-512x16x32"),
                all(feature = "layerstacks-768x16x32", feature = "layerstacks-768x8x32"),
                all(feature = "layerstacks-768x16x32", feature = "layerstacks-512x16x32"),
                all(feature = "layerstacks-768x8x32", feature = "layerstacks-512x16x32"),
                all(feature = "layerstacks-1536x16x32", feature = "layerstacks-1024x16x32"),
                all(feature = "layerstacks-1536x32x32", feature = "layerstacks-1024x16x32"),
                all(feature = "layerstacks-768x16x32", feature = "layerstacks-1024x16x32"),
                all(feature = "layerstacks-768x8x32", feature = "layerstacks-1024x16x32"),
                all(feature = "layerstacks-512x16x32", feature = "layerstacks-1024x16x32"),
                all(feature = "layerstacks-1536x16x32", feature = "layerstacks-3072x16x32"),
                all(feature = "layerstacks-1536x32x32", feature = "layerstacks-3072x16x32"),
                all(feature = "layerstacks-768x16x32", feature = "layerstacks-3072x16x32"),
                all(feature = "layerstacks-768x8x32", feature = "layerstacks-3072x16x32"),
                all(feature = "layerstacks-512x16x32", feature = "layerstacks-3072x16x32"),
                all(feature = "layerstacks-1024x16x32", feature = "layerstacks-3072x16x32"),
            ))]
            _ => panic!(
                "LayerStacksNetwork / LayerStacksAccStack の L1 サイズが不一致 (net={:?}, stack={:?})",
                self.architecture_dims(),
                stack.architecture_dims()
            ),
        }
    }

    /// アキュムレータを更新 (キャッシュ対応)。
    #[cfg(feature = "layerstack-arch")]
    pub fn update_accumulator(
        &self,
        pos: &Position,
        stack: &mut super::accumulator_layer_stacks::LayerStacksAccStack,
        cache: &mut Option<super::accumulator_layer_stacks::LayerStacksAccCache>,
    ) {
        // mismatch arm の panic で表示する用に事前計算しておく (stack は match 内で
        // mutable borrow されるため arm 内では参照できない)。2 サイズ以上 enable の
        // ときだけ mismatch arm が到達可能なので同じ cfg gate を共有する。
        #[cfg(any(
            all(feature = "layerstacks-1536x16x32", feature = "layerstacks-1536x32x32"),
            all(feature = "layerstacks-1536x16x32", feature = "layerstacks-768x16x32"),
            all(feature = "layerstacks-1536x16x32", feature = "layerstacks-768x8x32"),
            all(feature = "layerstacks-1536x16x32", feature = "layerstacks-512x16x32"),
            all(feature = "layerstacks-1536x32x32", feature = "layerstacks-768x16x32"),
            all(feature = "layerstacks-1536x32x32", feature = "layerstacks-768x8x32"),
            all(feature = "layerstacks-1536x32x32", feature = "layerstacks-512x16x32"),
            all(feature = "layerstacks-768x16x32", feature = "layerstacks-768x8x32"),
            all(feature = "layerstacks-768x16x32", feature = "layerstacks-512x16x32"),
            all(feature = "layerstacks-768x8x32", feature = "layerstacks-512x16x32"),
            all(feature = "layerstacks-1536x16x32", feature = "layerstacks-1024x16x32"),
            all(feature = "layerstacks-1536x32x32", feature = "layerstacks-1024x16x32"),
            all(feature = "layerstacks-768x16x32", feature = "layerstacks-1024x16x32"),
            all(feature = "layerstacks-768x8x32", feature = "layerstacks-1024x16x32"),
            all(feature = "layerstacks-512x16x32", feature = "layerstacks-1024x16x32"),
            all(feature = "layerstacks-1536x16x32", feature = "layerstacks-3072x16x32"),
            all(feature = "layerstacks-1536x32x32", feature = "layerstacks-3072x16x32"),
            all(feature = "layerstacks-768x16x32", feature = "layerstacks-3072x16x32"),
            all(feature = "layerstacks-768x8x32", feature = "layerstacks-3072x16x32"),
            all(feature = "layerstacks-512x16x32", feature = "layerstacks-3072x16x32"),
            all(feature = "layerstacks-1024x16x32", feature = "layerstacks-3072x16x32"),
        ))]
        let net_dims = self.architecture_dims();
        #[cfg(any(
            all(feature = "layerstacks-1536x16x32", feature = "layerstacks-1536x32x32"),
            all(feature = "layerstacks-1536x16x32", feature = "layerstacks-768x16x32"),
            all(feature = "layerstacks-1536x16x32", feature = "layerstacks-768x8x32"),
            all(feature = "layerstacks-1536x16x32", feature = "layerstacks-512x16x32"),
            all(feature = "layerstacks-1536x32x32", feature = "layerstacks-768x16x32"),
            all(feature = "layerstacks-1536x32x32", feature = "layerstacks-768x8x32"),
            all(feature = "layerstacks-1536x32x32", feature = "layerstacks-512x16x32"),
            all(feature = "layerstacks-768x16x32", feature = "layerstacks-768x8x32"),
            all(feature = "layerstacks-768x16x32", feature = "layerstacks-512x16x32"),
            all(feature = "layerstacks-768x8x32", feature = "layerstacks-512x16x32"),
            all(feature = "layerstacks-1536x16x32", feature = "layerstacks-1024x16x32"),
            all(feature = "layerstacks-1536x32x32", feature = "layerstacks-1024x16x32"),
            all(feature = "layerstacks-768x16x32", feature = "layerstacks-1024x16x32"),
            all(feature = "layerstacks-768x8x32", feature = "layerstacks-1024x16x32"),
            all(feature = "layerstacks-512x16x32", feature = "layerstacks-1024x16x32"),
            all(feature = "layerstacks-1536x16x32", feature = "layerstacks-3072x16x32"),
            all(feature = "layerstacks-1536x32x32", feature = "layerstacks-3072x16x32"),
            all(feature = "layerstacks-768x16x32", feature = "layerstacks-3072x16x32"),
            all(feature = "layerstacks-768x8x32", feature = "layerstacks-3072x16x32"),
            all(feature = "layerstacks-512x16x32", feature = "layerstacks-3072x16x32"),
            all(feature = "layerstacks-1024x16x32", feature = "layerstacks-3072x16x32"),
        ))]
        let stack_dims = stack.architecture_dims();
        macro_rules! do_update {
            ($net:expr, $stack:expr, $cache_variant:ident) => {{
                let current_entry = $stack.current();
                if current_entry.accumulator.computed_accumulation {
                    return;
                }

                let mut updated = false;

                if let Some(prev_idx) = current_entry.previous {
                    let prev_computed = $stack.entry_at(prev_idx).accumulator.computed_accumulation;
                    if prev_computed {
                        let dirty_piece = $stack.current().dirty_piece;
                        let (prev_acc, current_acc) =
                            $stack.get_prev_and_current_accumulators(prev_idx);
                        if let Some(
                            super::accumulator_layer_stacks::LayerStacksAccCache::$cache_variant(c),
                        ) = cache
                        {
                            $net.update_accumulator_with_cache(
                                pos,
                                &dirty_piece,
                                current_acc,
                                prev_acc,
                                c,
                            );
                        } else {
                            $net.update_accumulator(pos, &dirty_piece, current_acc, prev_acc);
                        }
                        updated = true;
                    }
                }

                if !updated {
                    // 遡及上限は runtime のモデル種別で決める。non-threat は forward-chain の
                    // piece/PSQT 差分が玉移動 refresh より安く深さ 4 が速いが、threat モデルは
                    // path>=2 の forward update が threat accumulator を full 再列挙する(Finny 非対象)
                    // ため refresh(piece は Finny)に落ちる深さ 1 が速い(1 は YaneuraOu 方式相当)。
                    // edition-universal は threat 非対応モデルも同一バイナリで動くので runtime 判定。
                    #[cfg(feature = "nnue-effect-bucket")]
                    let max_depth = 0;
                    #[cfg(all(feature = "nnue-threat", not(feature = "nnue-effect-bucket")))]
                    let max_depth = if $net.feature_transformer.has_threat { 1 } else { 4 };
                    #[cfg(not(any(feature = "nnue-threat", feature = "nnue-effect-bucket")))]
                    let max_depth = 4;
                    if let Some((source_idx, _depth)) =
                        $stack.find_usable_accumulator(max_depth)
                    {
                        updated = $net.forward_update_incremental(pos, $stack, source_idx);
                    }
                }

                if !updated {
                    let acc = &mut $stack.current_mut().accumulator;
                    if let Some(
                        super::accumulator_layer_stacks::LayerStacksAccCache::$cache_variant(c),
                    ) = cache
                    {
                        $net.refresh_accumulator_with_cache(pos, acc, c);
                    } else {
                        $net.refresh_accumulator(pos, acc);
                    }
                }
            }};
        }

        match (self, stack) {
            #[cfg(feature = "layerstacks-1536x16x32")]
            (
                Self::L1536x16x32(net),
                super::accumulator_layer_stacks::LayerStacksAccStack::L1536x16x32(st),
            ) => {
                do_update!(net, st, L1536x16x32);
            }
            #[cfg(feature = "layerstacks-1536x32x32")]
            (
                Self::L1536x32x32(net),
                super::accumulator_layer_stacks::LayerStacksAccStack::L1536x32x32(st),
            ) => {
                do_update!(net, st, L1536x32x32);
            }
            #[cfg(feature = "layerstacks-768x16x32")]
            (
                Self::L768x16x32(net),
                super::accumulator_layer_stacks::LayerStacksAccStack::L768x16x32(st),
            ) => {
                do_update!(net, st, L768x16x32);
            }
            #[cfg(feature = "layerstacks-768x8x32")]
            (
                Self::L768x8x32(net),
                super::accumulator_layer_stacks::LayerStacksAccStack::L768x8x32(st),
            ) => {
                do_update!(net, st, L768x8x32);
            }
            #[cfg(feature = "layerstacks-512x16x32")]
            (
                Self::L512x16x32(net),
                super::accumulator_layer_stacks::LayerStacksAccStack::L512x16x32(st),
            ) => {
                do_update!(net, st, L512x16x32);
            }
            #[cfg(feature = "layerstacks-1024x16x32")]
            (
                Self::L1024x16x32(net),
                super::accumulator_layer_stacks::LayerStacksAccStack::L1024x16x32(st),
            ) => {
                do_update!(net, st, L1024x16x32);
            }
            #[cfg(feature = "layerstacks-3072x16x32")]
            (
                Self::L3072x16x32(net),
                super::accumulator_layer_stacks::LayerStacksAccStack::L3072x16x32(st),
            ) => {
                do_update!(net, st, L3072x16x32);
            }
            #[cfg(any(
                all(feature = "layerstacks-1536x16x32", feature = "layerstacks-1536x32x32"),
                all(feature = "layerstacks-1536x16x32", feature = "layerstacks-768x16x32"),
                all(feature = "layerstacks-1536x16x32", feature = "layerstacks-768x8x32"),
                all(feature = "layerstacks-1536x16x32", feature = "layerstacks-512x16x32"),
                all(feature = "layerstacks-1536x32x32", feature = "layerstacks-768x16x32"),
                all(feature = "layerstacks-1536x32x32", feature = "layerstacks-768x8x32"),
                all(feature = "layerstacks-1536x32x32", feature = "layerstacks-512x16x32"),
                all(feature = "layerstacks-768x16x32", feature = "layerstacks-768x8x32"),
                all(feature = "layerstacks-768x16x32", feature = "layerstacks-512x16x32"),
                all(feature = "layerstacks-768x8x32", feature = "layerstacks-512x16x32"),
                all(feature = "layerstacks-1536x16x32", feature = "layerstacks-1024x16x32"),
                all(feature = "layerstacks-1536x32x32", feature = "layerstacks-1024x16x32"),
                all(feature = "layerstacks-768x16x32", feature = "layerstacks-1024x16x32"),
                all(feature = "layerstacks-768x8x32", feature = "layerstacks-1024x16x32"),
                all(feature = "layerstacks-512x16x32", feature = "layerstacks-1024x16x32"),
                all(feature = "layerstacks-1536x16x32", feature = "layerstacks-3072x16x32"),
                all(feature = "layerstacks-1536x32x32", feature = "layerstacks-3072x16x32"),
                all(feature = "layerstacks-768x16x32", feature = "layerstacks-3072x16x32"),
                all(feature = "layerstacks-768x8x32", feature = "layerstacks-3072x16x32"),
                all(feature = "layerstacks-512x16x32", feature = "layerstacks-3072x16x32"),
                all(feature = "layerstacks-1024x16x32", feature = "layerstacks-3072x16x32"),
            ))]
            _ => panic!(
                "LayerStacksNetwork / LayerStacksAccStack の L1 サイズが不一致 (net={:?}, stack={:?})",
                net_dims, stack_dims
            ),
        }
    }

    /// 新しい L1 サイズに対応する AccStack を作成
    #[cfg(feature = "layerstack-arch")]
    pub fn new_acc_stack(&self) -> super::accumulator_layer_stacks::LayerStacksAccStack {
        match self {
            #[cfg(feature = "layerstacks-1536x16x32")]
            Self::L1536x16x32(_) => {
                super::accumulator_layer_stacks::LayerStacksAccStack::L1536x16x32(
                    super::accumulator_layer_stacks::AccumulatorStackLayerStacks::<1536>::new(),
                )
            }
            #[cfg(feature = "layerstacks-1536x32x32")]
            Self::L1536x32x32(_) => {
                super::accumulator_layer_stacks::LayerStacksAccStack::L1536x32x32(
                    super::accumulator_layer_stacks::AccumulatorStackLayerStacks::<1536>::new(),
                )
            }
            #[cfg(feature = "layerstacks-768x16x32")]
            Self::L768x16x32(_) => {
                super::accumulator_layer_stacks::LayerStacksAccStack::L768x16x32(
                    super::accumulator_layer_stacks::AccumulatorStackLayerStacks::<768>::new(),
                )
            }
            #[cfg(feature = "layerstacks-768x8x32")]
            Self::L768x8x32(_) => super::accumulator_layer_stacks::LayerStacksAccStack::L768x8x32(
                super::accumulator_layer_stacks::AccumulatorStackLayerStacks::<768>::new(),
            ),
            #[cfg(feature = "layerstacks-512x16x32")]
            Self::L512x16x32(_) => {
                super::accumulator_layer_stacks::LayerStacksAccStack::L512x16x32(
                    super::accumulator_layer_stacks::AccumulatorStackLayerStacks::<512>::new(),
                )
            }
            #[cfg(feature = "layerstacks-1024x16x32")]
            Self::L1024x16x32(_) => {
                super::accumulator_layer_stacks::LayerStacksAccStack::L1024x16x32(
                    super::accumulator_layer_stacks::AccumulatorStackLayerStacks::<1024>::new(),
                )
            }
            #[cfg(feature = "layerstacks-3072x16x32")]
            Self::L3072x16x32(_) => {
                super::accumulator_layer_stacks::LayerStacksAccStack::L3072x16x32(
                    super::accumulator_layer_stacks::AccumulatorStackLayerStacks::<3072>::new(),
                )
            }
            #[cfg(not(any(
                feature = "layerstacks-1536x16x32",
                feature = "layerstacks-1536x32x32",
                feature = "layerstacks-768x16x32",
                feature = "layerstacks-768x8x32",
                feature = "layerstacks-512x16x32",
                feature = "layerstacks-1024x16x32",
                feature = "layerstacks-3072x16x32",
            )))]
            _ => unreachable!("no LayerStacks size variant enabled"),
        }
    }

    /// 新しい L1 サイズに対応する AccCache を作成
    #[cfg(feature = "layerstack-arch")]
    pub fn new_acc_cache(&self) -> super::accumulator_layer_stacks::LayerStacksAccCache {
        match self {
            #[cfg(feature = "layerstacks-1536x16x32")]
            Self::L1536x16x32(_) => {
                super::accumulator_layer_stacks::LayerStacksAccCache::L1536x16x32(
                    super::accumulator_layer_stacks::AccumulatorCacheLayerStacks::<1536>::new(),
                )
            }
            #[cfg(feature = "layerstacks-1536x32x32")]
            Self::L1536x32x32(_) => {
                super::accumulator_layer_stacks::LayerStacksAccCache::L1536x32x32(
                    super::accumulator_layer_stacks::AccumulatorCacheLayerStacks::<1536>::new(),
                )
            }
            #[cfg(feature = "layerstacks-768x16x32")]
            Self::L768x16x32(_) => {
                super::accumulator_layer_stacks::LayerStacksAccCache::L768x16x32(
                    super::accumulator_layer_stacks::AccumulatorCacheLayerStacks::<768>::new(),
                )
            }
            #[cfg(feature = "layerstacks-768x8x32")]
            Self::L768x8x32(_) => super::accumulator_layer_stacks::LayerStacksAccCache::L768x8x32(
                super::accumulator_layer_stacks::AccumulatorCacheLayerStacks::<768>::new(),
            ),
            #[cfg(feature = "layerstacks-512x16x32")]
            Self::L512x16x32(_) => {
                super::accumulator_layer_stacks::LayerStacksAccCache::L512x16x32(
                    super::accumulator_layer_stacks::AccumulatorCacheLayerStacks::<512>::new(),
                )
            }
            #[cfg(feature = "layerstacks-1024x16x32")]
            Self::L1024x16x32(_) => {
                super::accumulator_layer_stacks::LayerStacksAccCache::L1024x16x32(
                    super::accumulator_layer_stacks::AccumulatorCacheLayerStacks::<1024>::new(),
                )
            }
            #[cfg(feature = "layerstacks-3072x16x32")]
            Self::L3072x16x32(_) => {
                super::accumulator_layer_stacks::LayerStacksAccCache::L3072x16x32(
                    super::accumulator_layer_stacks::AccumulatorCacheLayerStacks::<3072>::new(),
                )
            }
            #[cfg(not(any(
                feature = "layerstacks-1536x16x32",
                feature = "layerstacks-1536x32x32",
                feature = "layerstacks-768x16x32",
                feature = "layerstacks-768x8x32",
                feature = "layerstacks-512x16x32",
                feature = "layerstacks-1024x16x32",
                feature = "layerstacks-3072x16x32",
            )))]
            _ => unreachable!("no LayerStacks size variant enabled"),
        }
    }

    /// `eval diag` 用: refresh + evaluate_with_diagnostics を全 L1 variant 上で実行する。
    ///
    /// `LayerStacksNetwork::refresh_and_evaluate_with_diagnostics` から委譲される。
    #[cfg(all(feature = "layerstack-arch", feature = "diagnostics"))]
    pub fn refresh_and_evaluate_with_diagnostics(&self, pos: &Position) -> Value {
        match self {
            #[cfg(feature = "layerstacks-1536x16x32")]
            Self::L1536x16x32(net) => {
                let mut acc = AccumulatorLayerStacks::<1536>::new();
                net.refresh_accumulator(pos, &mut acc);
                net.evaluate_with_diagnostics(pos, &acc)
            }
            #[cfg(feature = "layerstacks-1536x32x32")]
            Self::L1536x32x32(net) => {
                let mut acc = AccumulatorLayerStacks::<1536>::new();
                net.refresh_accumulator(pos, &mut acc);
                net.evaluate_with_diagnostics(pos, &acc)
            }
            #[cfg(feature = "layerstacks-768x16x32")]
            Self::L768x16x32(net) => {
                let mut acc = AccumulatorLayerStacks::<768>::new();
                net.refresh_accumulator(pos, &mut acc);
                net.evaluate_with_diagnostics(pos, &acc)
            }
            #[cfg(feature = "layerstacks-768x8x32")]
            Self::L768x8x32(net) => {
                let mut acc = AccumulatorLayerStacks::<768>::new();
                net.refresh_accumulator(pos, &mut acc);
                net.evaluate_with_diagnostics(pos, &acc)
            }
            #[cfg(feature = "layerstacks-512x16x32")]
            Self::L512x16x32(net) => {
                let mut acc = AccumulatorLayerStacks::<512>::new();
                net.refresh_accumulator(pos, &mut acc);
                net.evaluate_with_diagnostics(pos, &acc)
            }
            #[cfg(feature = "layerstacks-1024x16x32")]
            Self::L1024x16x32(net) => {
                let mut acc = AccumulatorLayerStacks::<1024>::new();
                net.refresh_accumulator(pos, &mut acc);
                net.evaluate_with_diagnostics(pos, &acc)
            }
            #[cfg(feature = "layerstacks-3072x16x32")]
            Self::L3072x16x32(net) => {
                let mut acc = AccumulatorLayerStacks::<3072>::new();
                net.refresh_accumulator(pos, &mut acc);
                net.evaluate_with_diagnostics(pos, &acc)
            }
            #[cfg(not(any(
                feature = "layerstacks-1536x16x32",
                feature = "layerstacks-1536x32x32",
                feature = "layerstacks-768x16x32",
                feature = "layerstacks-768x8x32",
                feature = "layerstacks-512x16x32",
                feature = "layerstacks-1024x16x32",
                feature = "layerstacks-3072x16x32",
            )))]
            _ => unreachable!("no LayerStacks size variant enabled"),
        }
    }
}

/// LayerStacks ネットワークの FT 軸 dispatch enum。各 variant の内部に L1 軸 dispatch
/// `LsNetByFt<FT>` を持つ二段構造。
///
/// active な FT variant は `ft-*` feature で、active な L1 variant は `layerstacks-*` feature で
/// 制御される。
pub enum LayerStacksNetwork {
    #[cfg(feature = "nnue-effect-bucket")]
    HalfKaHmMergedEffectBucket(LsNetByFt<HalfKaHmMergedSpec>),
    #[cfg(feature = "ft-halfka_hm_merged")]
    HalfKaHmMerged(LsNetByFt<HalfKaHmMergedSpec>),
    #[cfg(feature = "ft-halfka_hm_split")]
    HalfKaHmSplit(LsNetByFt<HalfKaHmSplitSpec>),
    #[cfg(feature = "ft-halfka_merged")]
    HalfKaMerged(LsNetByFt<HalfKaMergedSpec>),
    #[cfg(feature = "ft-halfka_split")]
    HalfKaSplit(LsNetByFt<HalfKaSplitSpec>),
    #[cfg(feature = "ft-halfkp")]
    HalfKP(LsNetByFt<HalfKpSpec>),
}

/// `LayerStacksNetwork` の FT × L1 軸を 1 段 match で展開する公開マクロ。
///
/// 5 FT × 7 L1 = 35 (FT, L1) 組合せを `all(ft-*, layerstacks-*)` cfg gate 付きで列挙し、
/// マッチした variant の内部 `&NetworkLayerStacks<L1, ..., FT>` (concrete 型) を
/// `$inner` として body に渡す。tools crate (bench / eval / verify) の dispatch
/// マクロをこれに統合することで、FT/L1 軸が増えたときの 3 ファイル同時更新漏れを防ぐ。
///
/// マッチアームは呼び出し crate 側の cfg を見て展開されるため、`rshogi-core` 側で
/// 有効な variant を caller 側がすべては有効化していない場合に備えて `_ =>`
/// fallback を引数として受け取る。caller のすべての (`ft-*`, `layerstacks-*`) feature が
/// 揃っている = 35 arm で exhaustive なときは fallback arm を cfg gate でドロップし、
/// `#[allow(unreachable_patterns)]` を不要にする。
///
/// 構文 (既存の `with_ls_net!` 等と互換):
/// ```ignore
/// use rshogi_core::nnue::ls_dispatch_ft_size;
/// ls_dispatch_ft_size!(ls_net, |net| {
///     run_layer_stack_bench(net, /* ... */)?;
/// }, _ => bail!("有効な LayerStacks (FT × L1) バリアントがありません"))
/// ```
#[macro_export]
macro_rules! ls_dispatch_ft_size {
    ($net:expr, |$inner:ident| $body:expr, _ => $fallback:expr $(,)?) => {
        match $net {
            #[cfg(all(feature = "nnue-effect-bucket", feature = "layerstacks-1536x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaHmMergedEffectBucket(
                $crate::nnue::LsNetByFt::L1536x16x32($inner),
            ) => $body,
            #[cfg(all(feature = "nnue-effect-bucket", feature = "layerstacks-1536x32x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaHmMergedEffectBucket(
                $crate::nnue::LsNetByFt::L1536x32x32($inner),
            ) => $body,
            #[cfg(all(feature = "nnue-effect-bucket", feature = "layerstacks-768x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaHmMergedEffectBucket(
                $crate::nnue::LsNetByFt::L768x16x32($inner),
            ) => $body,
            #[cfg(all(feature = "nnue-effect-bucket", feature = "layerstacks-768x8x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaHmMergedEffectBucket(
                $crate::nnue::LsNetByFt::L768x8x32($inner),
            ) => $body,
            #[cfg(all(feature = "nnue-effect-bucket", feature = "layerstacks-512x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaHmMergedEffectBucket(
                $crate::nnue::LsNetByFt::L512x16x32($inner),
            ) => $body,
            #[cfg(all(feature = "nnue-effect-bucket", feature = "layerstacks-1024x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaHmMergedEffectBucket(
                $crate::nnue::LsNetByFt::L1024x16x32($inner),
            ) => $body,
            #[cfg(all(feature = "nnue-effect-bucket", feature = "layerstacks-3072x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaHmMergedEffectBucket(
                $crate::nnue::LsNetByFt::L3072x16x32($inner),
            ) => $body,
            #[cfg(all(feature = "ft-halfka_hm_merged", feature = "layerstacks-1536x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaHmMerged(
                $crate::nnue::LsNetByFt::L1536x16x32($inner),
            ) => $body,
            #[cfg(all(feature = "ft-halfka_hm_merged", feature = "layerstacks-1536x32x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaHmMerged(
                $crate::nnue::LsNetByFt::L1536x32x32($inner),
            ) => $body,
            #[cfg(all(feature = "ft-halfka_hm_merged", feature = "layerstacks-768x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaHmMerged(
                $crate::nnue::LsNetByFt::L768x16x32($inner),
            ) => $body,
            #[cfg(all(feature = "ft-halfka_hm_merged", feature = "layerstacks-768x8x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaHmMerged(
                $crate::nnue::LsNetByFt::L768x8x32($inner),
            ) => $body,
            #[cfg(all(feature = "ft-halfka_hm_merged", feature = "layerstacks-512x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaHmMerged(
                $crate::nnue::LsNetByFt::L512x16x32($inner),
            ) => $body,
            #[cfg(all(feature = "ft-halfka_hm_merged", feature = "layerstacks-1024x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaHmMerged(
                $crate::nnue::LsNetByFt::L1024x16x32($inner),
            ) => $body,
            #[cfg(all(feature = "ft-halfka_hm_merged", feature = "layerstacks-3072x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaHmMerged(
                $crate::nnue::LsNetByFt::L3072x16x32($inner),
            ) => $body,
            #[cfg(all(feature = "ft-halfka_hm_split", feature = "layerstacks-1536x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaHmSplit(
                $crate::nnue::LsNetByFt::L1536x16x32($inner),
            ) => $body,
            #[cfg(all(feature = "ft-halfka_hm_split", feature = "layerstacks-1536x32x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaHmSplit(
                $crate::nnue::LsNetByFt::L1536x32x32($inner),
            ) => $body,
            #[cfg(all(feature = "ft-halfka_hm_split", feature = "layerstacks-768x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaHmSplit(
                $crate::nnue::LsNetByFt::L768x16x32($inner),
            ) => $body,
            #[cfg(all(feature = "ft-halfka_hm_split", feature = "layerstacks-768x8x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaHmSplit(
                $crate::nnue::LsNetByFt::L768x8x32($inner),
            ) => $body,
            #[cfg(all(feature = "ft-halfka_hm_split", feature = "layerstacks-512x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaHmSplit(
                $crate::nnue::LsNetByFt::L512x16x32($inner),
            ) => $body,
            #[cfg(all(feature = "ft-halfka_hm_split", feature = "layerstacks-1024x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaHmSplit(
                $crate::nnue::LsNetByFt::L1024x16x32($inner),
            ) => $body,
            #[cfg(all(feature = "ft-halfka_hm_split", feature = "layerstacks-3072x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaHmSplit(
                $crate::nnue::LsNetByFt::L3072x16x32($inner),
            ) => $body,
            #[cfg(all(feature = "ft-halfka_merged", feature = "layerstacks-1536x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaMerged(
                $crate::nnue::LsNetByFt::L1536x16x32($inner),
            ) => $body,
            #[cfg(all(feature = "ft-halfka_merged", feature = "layerstacks-1536x32x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaMerged(
                $crate::nnue::LsNetByFt::L1536x32x32($inner),
            ) => $body,
            #[cfg(all(feature = "ft-halfka_merged", feature = "layerstacks-768x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaMerged(
                $crate::nnue::LsNetByFt::L768x16x32($inner),
            ) => $body,
            #[cfg(all(feature = "ft-halfka_merged", feature = "layerstacks-768x8x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaMerged($crate::nnue::LsNetByFt::L768x8x32(
                $inner,
            )) => $body,
            #[cfg(all(feature = "ft-halfka_merged", feature = "layerstacks-512x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaMerged(
                $crate::nnue::LsNetByFt::L512x16x32($inner),
            ) => $body,
            #[cfg(all(feature = "ft-halfka_merged", feature = "layerstacks-1024x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaMerged(
                $crate::nnue::LsNetByFt::L1024x16x32($inner),
            ) => $body,
            #[cfg(all(feature = "ft-halfka_merged", feature = "layerstacks-3072x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaMerged(
                $crate::nnue::LsNetByFt::L3072x16x32($inner),
            ) => $body,
            #[cfg(all(feature = "ft-halfka_split", feature = "layerstacks-1536x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaSplit(
                $crate::nnue::LsNetByFt::L1536x16x32($inner),
            ) => $body,
            #[cfg(all(feature = "ft-halfka_split", feature = "layerstacks-1536x32x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaSplit(
                $crate::nnue::LsNetByFt::L1536x32x32($inner),
            ) => $body,
            #[cfg(all(feature = "ft-halfka_split", feature = "layerstacks-768x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaSplit($crate::nnue::LsNetByFt::L768x16x32(
                $inner,
            )) => $body,
            #[cfg(all(feature = "ft-halfka_split", feature = "layerstacks-768x8x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaSplit($crate::nnue::LsNetByFt::L768x8x32(
                $inner,
            )) => $body,
            #[cfg(all(feature = "ft-halfka_split", feature = "layerstacks-512x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaSplit($crate::nnue::LsNetByFt::L512x16x32(
                $inner,
            )) => $body,
            #[cfg(all(feature = "ft-halfka_split", feature = "layerstacks-1024x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaSplit(
                $crate::nnue::LsNetByFt::L1024x16x32($inner),
            ) => $body,
            #[cfg(all(feature = "ft-halfka_split", feature = "layerstacks-3072x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaSplit(
                $crate::nnue::LsNetByFt::L3072x16x32($inner),
            ) => $body,
            #[cfg(all(feature = "ft-halfkp", feature = "layerstacks-1536x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKP($crate::nnue::LsNetByFt::L1536x16x32(
                $inner,
            )) => $body,
            #[cfg(all(feature = "ft-halfkp", feature = "layerstacks-1536x32x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKP($crate::nnue::LsNetByFt::L1536x32x32(
                $inner,
            )) => $body,
            #[cfg(all(feature = "ft-halfkp", feature = "layerstacks-768x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKP($crate::nnue::LsNetByFt::L768x16x32(
                $inner,
            )) => $body,
            #[cfg(all(feature = "ft-halfkp", feature = "layerstacks-768x8x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKP($crate::nnue::LsNetByFt::L768x8x32(
                $inner,
            )) => $body,
            #[cfg(all(feature = "ft-halfkp", feature = "layerstacks-512x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKP($crate::nnue::LsNetByFt::L512x16x32(
                $inner,
            )) => $body,
            #[cfg(all(feature = "ft-halfkp", feature = "layerstacks-1024x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKP($crate::nnue::LsNetByFt::L1024x16x32(
                $inner,
            )) => $body,
            #[cfg(all(feature = "ft-halfkp", feature = "layerstacks-3072x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKP($crate::nnue::LsNetByFt::L3072x16x32(
                $inner,
            )) => $body,
            // caller の (ft-*, layerstacks-*) を 5 × 7 全部有効化したときは 35 arm が
            // exhaustive。そのときだけ fallback arm を cfg gate でドロップして
            // unreachable warning を避ける。
            #[cfg(not(all(
                feature = "ft-halfka_hm_merged",
                feature = "ft-halfka_hm_split",
                feature = "ft-halfka_merged",
                feature = "ft-halfka_split",
                feature = "ft-halfkp",
                feature = "layerstacks-1536x16x32",
                feature = "layerstacks-1536x32x32",
                feature = "layerstacks-768x16x32",
                feature = "layerstacks-768x8x32",
                feature = "layerstacks-512x16x32",
                feature = "layerstacks-1024x16x32",
                feature = "layerstacks-3072x16x32",
            )))]
            _ => $fallback,
        }
    };
}

/// LayerStacksNetwork の FT variants を網羅する dispatch マクロ。
///
/// 全 FT feature が無効の場合 (現状の build.rs check では layerstack-arch + ft-* >= 1 を必須化
/// しているため発生しないが、念のため) は wildcard arm でコンパイルを通す。
macro_rules! ls_match_ft {
    ($val:expr, $pat:ident => $body:expr) => {
        match $val {
            #[cfg(feature = "nnue-effect-bucket")]
            LayerStacksNetwork::HalfKaHmMergedEffectBucket($pat) => $body,
            #[cfg(feature = "ft-halfka_hm_merged")]
            LayerStacksNetwork::HalfKaHmMerged($pat) => $body,
            #[cfg(feature = "ft-halfka_hm_split")]
            LayerStacksNetwork::HalfKaHmSplit($pat) => $body,
            #[cfg(feature = "ft-halfka_merged")]
            LayerStacksNetwork::HalfKaMerged($pat) => $body,
            #[cfg(feature = "ft-halfka_split")]
            LayerStacksNetwork::HalfKaSplit($pat) => $body,
            #[cfg(feature = "ft-halfkp")]
            LayerStacksNetwork::HalfKP($pat) => $body,
            #[cfg(not(any(
                feature = "ft-halfka_hm_merged",
                feature = "ft-halfka_hm_split",
                feature = "ft-halfka_merged",
                feature = "ft-halfka_split",
                feature = "ft-halfkp",
                feature = "nnue-effect-bucket",
            )))]
            _ => unreachable!("no LayerStacks FT variant enabled"),
        }
    };
}

impl LayerStacksNetwork {
    /// アーキテクチャ寸法 (L1, L2, L3) を返す
    pub fn architecture_dims(&self) -> (usize, usize, usize) {
        ls_match_ft!(self, by_ft => by_ft.architecture_dims())
    }

    /// L1 サイズを取得
    pub fn l1_size(&self) -> usize {
        ls_match_ft!(self, by_ft => by_ft.l1_size())
    }

    /// 現在 load されている net の bucket 数 (= `.bin` header の `num_buckets`)
    pub fn num_buckets(&self) -> usize {
        ls_match_ft!(self, by_ft => by_ft.num_buckets())
    }

    #[cfg(feature = "layerstack-arch")]
    pub(crate) fn net_tensor_shape(&self, kind: NetTensorKind) -> NetTensorShape {
        ls_match_ft!(self, by_ft => by_ft.net_tensor_shape(kind))
    }

    #[cfg(feature = "layerstack-arch")]
    pub(crate) fn net_coefficient(&self, id: &NetCoefficientId) -> i32 {
        ls_match_ft!(self, by_ft => by_ft.net_coefficient(id))
    }

    #[cfg(feature = "layerstack-arch")]
    pub(crate) fn apply_net_delta(&mut self, id: &NetCoefficientId, delta: i32) -> bool {
        ls_match_ft!(self, by_ft => by_ft.apply_net_delta(id, delta))
    }

    /// アーキテクチャ仕様を取得
    pub fn architecture_spec(&self) -> super::spec::ArchitectureSpec {
        ls_match_ft!(self, by_ft => by_ft.architecture_spec())
    }

    /// FV_SCALE を取得
    pub fn fv_scale(&self) -> i32 {
        ls_match_ft!(self, by_ft => by_ft.fv_scale())
    }

    /// ファイルから読み込み (FT は arch_str から検出、L1/L2/L3 は呼び出し元が渡す)。
    #[cfg(feature = "layerstack-arch")]
    pub fn read_with_options<R: Read + Seek>(
        reader: &mut R,
        l1: usize,
        l2: usize,
        l3: usize,
        psqt_override: Option<bool>,
    ) -> io::Result<Self> {
        let ft_set = peek_layer_stacks_feature_set(reader)?;
        Self::read_with_feature_set(reader, ft_set, l1, l2, l3, psqt_override)
    }

    /// ファイルから読み込み (FT 明示)。テスト・診断ツールから FT を強制したい場合に使う。
    #[cfg(feature = "layerstack-arch")]
    pub fn read_with_feature_set<R: Read + Seek>(
        reader: &mut R,
        feature_set: super::spec::FeatureSet,
        l1: usize,
        l2: usize,
        l3: usize,
        psqt_override: Option<bool>,
    ) -> io::Result<Self> {
        // FT 軸を `match feature_set` で dispatch する。各 FT について該当 `ft-*` feature が
        // 有効なら `LsNetByFt::<spec>` に読み込み、無効なら Unsupported エラーを返す。
        // arch_str が `LayerStacks` キーワードのみの旧モデル (FT 未指定) は HalfKaHmMerged
        // (= 旧 HalfKA_hm デフォルト) と見なす。
        macro_rules! read_into_variant {
            ($ft_feat:literal, $ft_spec:ty, $self_variant:ident, $name:literal) => {{
                #[cfg(feature = $ft_feat)]
                {
                    let inner = LsNetByFt::<$ft_spec>::read_with_options(
                        reader,
                        l1,
                        l2,
                        l3,
                        psqt_override,
                    )?;
                    Ok(Self::$self_variant(inner))
                }
                #[cfg(not(feature = $ft_feat))]
                Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    concat!(
                        "LayerStacks FT `",
                        $name,
                        "` model requires the corresponding `",
                        $ft_feat,
                        "` feature; rebuild rshogi-core with an Edition that enables it.",
                    ),
                ))
            }};
        }
        use super::spec::FeatureSet as Fs;
        match feature_set {
            Fs::HalfKaHmMergedEffectBucket => {
                read_into_variant!(
                    "nnue-effect-bucket",
                    HalfKaHmMergedSpec,
                    HalfKaHmMergedEffectBucket,
                    "HalfKaHmMergedEffectBucket"
                )
            }
            Fs::HalfKaHmMerged | Fs::LayerStacks => {
                read_into_variant!(
                    "ft-halfka_hm_merged",
                    HalfKaHmMergedSpec,
                    HalfKaHmMerged,
                    "HalfKaHmMerged"
                )
            }
            Fs::HalfKaHmSplit => {
                read_into_variant!(
                    "ft-halfka_hm_split",
                    HalfKaHmSplitSpec,
                    HalfKaHmSplit,
                    "HalfKaHmSplit"
                )
            }
            Fs::HalfKaMerged => {
                read_into_variant!(
                    "ft-halfka_merged",
                    HalfKaMergedSpec,
                    HalfKaMerged,
                    "HalfKaMerged"
                )
            }
            Fs::HalfKaSplit => {
                read_into_variant!("ft-halfka_split", HalfKaSplitSpec, HalfKaSplit, "HalfKaSplit")
            }
            Fs::HalfKP => {
                read_into_variant!("ft-halfkp", HalfKpSpec, HalfKP, "HalfKP")
            }
        }
    }

    /// 評価値を計算
    #[cfg(feature = "layerstack-arch")]
    pub fn evaluate(
        &self,
        pos: &Position,
        stack: &super::accumulator_layer_stacks::LayerStacksAccStack,
    ) -> Value {
        ls_match_ft!(self, by_ft => by_ft.evaluate(pos, stack))
    }

    /// アキュムレータを更新 (キャッシュ対応)
    #[cfg(feature = "layerstack-arch")]
    pub fn update_accumulator(
        &self,
        pos: &Position,
        stack: &mut super::accumulator_layer_stacks::LayerStacksAccStack,
        cache: &mut Option<super::accumulator_layer_stacks::LayerStacksAccCache>,
    ) {
        ls_match_ft!(self, by_ft => by_ft.update_accumulator(pos, stack, cache))
    }

    /// 新しい L1 サイズに対応する AccStack を作成
    #[cfg(feature = "layerstack-arch")]
    pub fn new_acc_stack(&self) -> super::accumulator_layer_stacks::LayerStacksAccStack {
        ls_match_ft!(self, by_ft => by_ft.new_acc_stack())
    }

    /// 新しい L1 サイズに対応する AccCache を作成
    #[cfg(feature = "layerstack-arch")]
    pub fn new_acc_cache(&self) -> super::accumulator_layer_stacks::LayerStacksAccCache {
        ls_match_ft!(self, by_ft => by_ft.new_acc_cache())
    }

    /// 診断ログ向け: refresh + evaluate_with_diagnostics を全 FT × L1 variant 上で実行する。
    ///
    /// `eval diag` USI コマンドから呼ばれる。FT/L1 軸をすべて束ねた high-level helper。
    #[cfg(all(feature = "layerstack-arch", feature = "diagnostics"))]
    pub fn refresh_and_evaluate_with_diagnostics(&self, pos: &Position) -> Value {
        ls_match_ft!(self, by_ft => by_ft.refresh_and_evaluate_with_diagnostics(pos))
    }
}

/// reader の現在位置から LayerStacks ヘッダの arch_str を peek し、FT を判別する。
///
/// tatara emit 形式の arch_str は `Features=<FT>(Friend)[<dim>->1536x2],...` で、
/// 共有 helper が EffectBucket alias、`Features=` keyword、旧 header の substring を
/// dynamic reader と同じ規則で判定する。明示された未知 keyword はエラーとし、
/// keyword が無く FT を特定できない旧 header だけ `FeatureSet::LayerStacks` へ fallback
/// して、上位の `read_with_feature_set` で HalfKaHmMerged 互換扱いにする。
///
/// 読み取り後は `Seek::seek(SeekFrom::Start(original))` で reader 位置を巻き戻す。
/// `BufReader<File>` 等の seekable reader では seek 時に内部 buffer が破棄・再同期される
/// ため、後続の本読み込みに影響しない。peek 自体が失敗しても巻き戻しは試みる。
#[cfg(feature = "layerstack-arch")]
fn peek_layer_stacks_feature_set<R: Read + Seek>(
    reader: &mut R,
) -> io::Result<super::spec::FeatureSet> {
    let original = reader.stream_position()?;
    let result = (|| -> io::Result<super::spec::FeatureSet> {
        let mut buf4 = [0u8; 4];
        reader.read_exact(&mut buf4)?;
        reader.read_exact(&mut buf4)?;
        reader.read_exact(&mut buf4)?;
        let arch_len = u32::from_le_bytes(buf4) as usize;
        if arch_len == 0 || arch_len > MAX_ARCH_LEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Invalid arch string length: {arch_len} (max: {MAX_ARCH_LEN})"),
            ));
        }
        let mut arch = vec![0u8; arch_len];
        reader.read_exact(&mut arch)?;
        let arch_str = String::from_utf8_lossy(&arch);
        detect_layer_stacks_feature_set(&arch_str)
            .map_err(|message| io::Error::new(io::ErrorKind::InvalidData, message))
    })();
    reader.seek(SeekFrom::Start(original))?;
    result
}

/// arch_str から LS の FT を判別する pure helper (peek の純粋ロジック部分)。
#[cfg(feature = "layerstack-arch")]
fn detect_layer_stacks_feature_set(arch_str: &str) -> Result<super::spec::FeatureSet, String> {
    use super::spec::FeatureSet as Fs;
    match super::spec::detect_layer_stacks_feature(arch_str) {
        Ok(feature) => Ok(feature),
        Err(_) => {
            // 明示 keyword の解析エラーを伝播し、keyword が無い旧 header だけ互換扱いする。
            super::spec::parse_layer_stacks_feature_set_keyword(arch_str)?;
            Ok(Fs::LayerStacks)
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "layerstack-arch")]
    use super::*;
    use crate::nnue::constants::{FV_SCALE_HALFKA, NNUE_PYTORCH_L1};
    #[cfg(all(
        feature = "layerstack-arch",
        feature = "layerstacks-1536x16x32",
        feature = "ft-halfka_hm_merged"
    ))]
    use crate::position::{Position, SFEN_HIRATE};

    const TEST_L1: usize = NNUE_PYTORCH_L1;

    #[cfg(feature = "layerstack-arch")]
    #[test]
    fn test_kingrank9_oracle_fixtures() {
        let fixtures = [
            (
                "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
                8,
                "先手番: 自玉5iは f_rank=8 で kF=6、敵玉5aは反転後 e_rank=8 で kE=2",
            ),
            (
                "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL w - 1",
                8,
                "後手番: 自玉5aは反転後 f_rank=8 で kF=6、敵玉5iは e_rank=8 で kE=2",
            ),
            (
                "4K4/9/9/9/9/9/4k4/9/9 b - 1",
                0,
                "先手番: 自玉5aは f_rank=0 で kF=0、敵玉5gは反転後 e_rank=2 で kE=0",
            ),
            (
                "9/9/9/K8/8k/9/9/9/9 b - 1",
                4,
                "先手番: 自玉9dは f_rank=3 で kF=3、敵玉1eは反転後 e_rank=4 で kE=1",
            ),
        ];

        for (sfen, expected, reason) in fixtures {
            let mut pos = Position::new();
            pos.set_sfen(sfen).expect("oracle SFEN should be valid");

            // rshogi は段一=0、段九=8 で YO と同じ。inverse() が YO の Inv(sq) に対応する。
            assert_eq!(
                compute_layer_stack_kingrank9_bucket_index(
                    &pos,
                    pos.side_to_move(),
                    DEFAULT_NUM_BUCKETS,
                ),
                expected,
                "{reason}"
            );
        }
    }

    #[cfg(feature = "layerstack-arch")]
    #[test]
    #[should_panic(expected = "kingrank9 requires exactly 9 stored buckets")]
    fn test_kingrank9_rejects_non_nine_bucket_network() {
        let mut pos = Position::new();
        pos.set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
            .unwrap();

        compute_layer_stack_kingrank9_bucket_index(&pos, Color::Black, 4);
    }

    #[test]
    fn test_validate_layer_stacks_architecture_header() {
        use crate::nnue::spec::validate_layer_stacks_architecture_header as validate;
        assert_eq!(validate("FV_SCALE=16,Threat=216720,").unwrap(), Some(216720));
        assert_eq!(validate("Threat=96320,ThreatProfile=10,").unwrap(), Some(96320));
        assert_eq!(validate("ThreatProfile=10,Threat=96320,").unwrap(), Some(96320));
        assert_eq!(validate("ThreatProfile=10,").unwrap(), None);
        assert_eq!(validate("Threat=216720").unwrap(), Some(216720));
        assert_eq!(validate("PSQT=1,").unwrap(), None);
        assert!(validate("Threat=abc,").is_err());
        assert!(validate("Threat=0,").is_err());
        assert!(validate("Factorizer").is_err());
    }

    #[test]
    fn saturation_uses_forward_accumulators() {
        use super::super::ls_feature_spec::HalfKpSpec;
        use super::*;
        let mut network = NetworkLayerStacks::<64, 16, 30, 32, HalfKpSpec> {
            feature_transformer: FeatureTransformerLayerStacks::for_accumulator_tests(),
            layer_stacks: LayerStacks::with_num_buckets(1),
            fv_scale: 1,
            num_buckets: 1,
            _ft: PhantomData,
        };
        let bucket = &mut network.layer_stacks.buckets[0];
        bucket.l1.weights.fill(4);
        bucket.l2.weights.fill(4);
        bucket.output.weights.fill(1);
        let mut acc = AccumulatorLayerStacks::<64>::new();
        acc.accumulation[0].fill(50);
        acc.accumulation[1].fill(30);
        #[cfg(feature = "nnue-threat")]
        {
            acc.threat_accumulation[0].fill(100);
            acc.threat_accumulation[1].fill(110);
            // 飽和加算ではなく i16 wrapping で合成される境界も固定する。
            acc.accumulation[0][0] = i16::MAX;
            acc.threat_accumulation[0][0] = 1;
        }
        for use_threat in [false, true] {
            #[cfg(not(feature = "nnue-threat"))]
            if use_threat {
                continue;
            }
            #[cfg(feature = "nnue-threat")]
            {
                network.feature_transformer.has_threat = use_threat;
            }
            for side in [Color::Black, Color::White] {
                let us = side as usize;
                let them = (!side) as usize;
                let expected = |perspective: usize| {
                    std::array::from_fn::<i16, 64, _>(|i| {
                        let piece = acc.accumulation[perspective][i];
                        #[cfg(feature = "nnue-threat")]
                        if use_threat {
                            return piece.wrapping_add(acc.threat_accumulation[perspective][i]);
                        }
                        piece
                    })
                };
                let us_expected = Aligned(expected(us));
                let them_expected = Aligned(expected(them));
                network.with_combined_accumulators(&acc, side, |us_actual, them_actual| {
                    assert_eq!(*us_actual, us_expected.0);
                    assert_eq!(*them_actual, them_expected.0);
                });
                let mut transformed = Aligned([0u8; 64]);
                sqr_clipped_relu_transform(&us_expected.0, &them_expected.0, &mut transformed.0);
                let mut expected_counts = LsSaturationCounts::default();
                let raw = network.layer_stacks.buckets[0]
                    .propagate_counting_saturation(&transformed.0, &mut expected_counts);
                let expected_ft = us_expected
                    .0
                    .iter()
                    .chain(them_expected.0.iter())
                    .filter(|&&x| x >= 127)
                    .count() as u64;
                let mut counts = LsSaturationCounts::default();
                assert_eq!(
                    network.accumulate_saturation_counts(&acc, side, 0, &mut counts),
                    expected_ft
                );
                assert_eq!(
                    (
                        counts.l1_act_sat,
                        counts.l1_act_total,
                        counts.l2_act_sat,
                        counts.l2_act_total
                    ),
                    (
                        expected_counts.l1_act_sat,
                        expected_counts.l1_act_total,
                        expected_counts.l2_act_sat,
                        expected_counts.l2_act_total
                    )
                );
                if use_threat {
                    assert_eq!(expected_ft, 127);
                    assert_eq!((counts.l1_act_sat, counts.l2_act_sat), (30, 32));
                } else {
                    assert_eq!((counts.l1_act_sat, counts.l2_act_sat), (0, 0));
                }
                let mut pos = Position::new();
                pos.set_sfen(if side == Color::Black {
                    "4k4/9/9/9/9/9/9/9/4K4 b - 1"
                } else {
                    "4k4/9/9/9/9/9/9/9/4K4 w - 1"
                })
                .unwrap();
                assert_eq!(
                    network.evaluate_with_bucket(&pos, &acc, 0),
                    Value::new(raw / get_fv_scale_override().unwrap_or(1))
                );
                #[cfg(feature = "nnue-psqt")]
                {
                    network.feature_transformer.has_psqt = true;
                    acc.psqt_accumulation[us][0] = 300;
                    acc.psqt_accumulation[them][0] = 100;
                    let mut psqt_counts = LsSaturationCounts::default();
                    assert_eq!(
                        network.accumulate_saturation_counts(&acc, side, 0, &mut psqt_counts),
                        expected_ft
                    );
                    assert_eq!(
                        (psqt_counts.l1_act_sat, psqt_counts.l2_act_sat),
                        (counts.l1_act_sat, counts.l2_act_sat)
                    );
                    assert_eq!(
                        network.evaluate_with_bucket(&pos, &acc, 0),
                        Value::new(raw.saturating_add(100) / get_fv_scale_override().unwrap_or(1))
                    );
                    network.feature_transformer.has_psqt = false;
                }
            }
        }
    }

    #[test]
    fn initialized_layerstack_forward_matches_explicit_buffer() {
        use super::super::ls_feature_spec::HalfKpSpec;
        use super::*;
        let mut network = NetworkLayerStacks::<64, 16, 30, 32, HalfKpSpec> {
            feature_transformer: FeatureTransformerLayerStacks::read(&mut std::io::repeat(0))
                .unwrap(),
            layer_stacks: LayerStacks::with_num_buckets(1),
            fv_scale: 16,
            num_buckets: 1,
            _ft: PhantomData,
        };
        let bucket = &mut network.layer_stacks.buckets[0];
        bucket.l1.weights.fill(1);
        bucket.l1.biases.fill(1024);
        bucket.l2.weights.fill(1);
        bucket.l2.biases.fill(1024);
        bucket.output.weights.fill(1);
        let mut acc = AccumulatorLayerStacks::<64>::new();
        for i in 0..64 {
            acc.accumulation[0][i] = (i * 7) as i16;
            acc.accumulation[1][i] = (255 - i * 3) as i16;
        }
        for side in [Color::Black, Color::White] {
            let mut pos = Position::new();
            pos.set_sfen(if side == Color::Black {
                "4k4/9/9/9/9/9/9/9/4K4 b - 1"
            } else {
                "4k4/9/9/9/9/9/9/9/4K4 w - 1"
            })
            .unwrap();
            let mut transformed = Aligned([0u8; 64]);
            sqr_clipped_relu_transform(
                acc.get(side as usize),
                acc.get(1 - side as usize),
                &mut transformed.0,
            );
            assert!(transformed.0.iter().any(|&v| v != 0));
            let expected = network.layer_stacks.evaluate_raw(0, &transformed.0)
                / get_fv_scale_override().unwrap_or(16);
            assert_eq!(network.evaluate_with_bucket(&pos, &acc, 0), Value::new(expected));
        }
    }

    #[test]
    fn test_network_dimensions() {
        assert_eq!(TEST_L1, 1536);
        assert_eq!(FV_SCALE_HALFKA, 16);
    }

    /// LayerStacks NNUEファイルの読み込みと評価テスト
    ///
    /// このテストは外部NNUEファイルが必要なため通常はスキップ。
    /// 実行方法: `cargo test test_load_layer_stacks_file -- --ignored`
    ///
    /// テスト結果 (epoch82.nnue):
    /// - FT bias sum: -1
    /// - FT weight nonzero: 2,143,627
    /// - L1 bias (bucket 0): [-15, 57, -182, -97, -202, -55, 120, 1, 87, -133, -16, 44, -27, -37, -201, -186]
    /// - Initial position score: 0 (epoch82は学習初期のため)
    #[cfg(all(
        feature = "layerstack-arch",
        feature = "layerstacks-1536x16x32",
        feature = "ft-halfka_hm_merged"
    ))]
    #[test]
    #[ignore]
    fn test_load_layer_stacks_file() {
        let routing_guard = crate::nnue::network::layer_stack_routing_test_guard();
        use crate::nnue::layer_stacks::{compute_bucket_index, sqr_clipped_relu_transform};

        // テスト用NNUEファイルのパスを設定してください
        let path = std::env::var("NNUE_TEST_FILE")
            .unwrap_or_else(|_| "/path/to/your/layer_stacks.nnue".to_string());

        let network = match NetworkLayerStacks1536x16x32::load(path) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("Skipping test: {e}");
                return;
            }
        };

        // 評価前に routing の明示設定が必要。smoke 用に stored=routing の progresskpabs
        // (係数ゼロ = 常に中央 bucket) を使う。
        crate::nnue::configure_layer_stack_routing(
            crate::nnue::LayerStackBucketMode::ProgressKPAbs,
            network.num_buckets,
            Some(network.num_buckets),
        )
        .unwrap();

        // Feature Transformer のバイアスが読み込まれていることを確認
        let bias_sum: i64 = network.feature_transformer.biases.0.iter().map(|&x| x as i64).sum();
        eprintln!("FT bias sum: {bias_sum}");

        // Feature Transformer の重みの一部を確認
        let weight_sample: Vec<i16> = network.feature_transformer.weights[0..10].to_vec();
        eprintln!("FT weight sample (first 10): {weight_sample:?}");

        // 異なるオフセットで重みを確認
        let weight_total = network.feature_transformer.weights.len();
        let weight_nonzero: usize =
            network.feature_transformer.weights.iter().filter(|&&x| x != 0).count();
        eprintln!("FT weight total: {weight_total}, nonzero: {weight_nonzero}");

        // 中間位置の重みをサンプル
        let mid_offset = weight_total / 2;
        let weight_mid_sample: Vec<i16> =
            network.feature_transformer.weights[mid_offset..mid_offset + 10].to_vec();
        eprintln!("FT weight sample (mid): {weight_mid_sample:?}");

        // 最初のnonzero重みの位置を探す
        let first_nonzero_pos = network.feature_transformer.weights.iter().position(|&x| x != 0);
        if let Some(weight_pos) = first_nonzero_pos {
            let sample_end = (weight_pos + 10usize).min(weight_total);
            let first_nonzero_sample: Vec<i16> =
                network.feature_transformer.weights[weight_pos..sample_end].to_vec();
            eprintln!("First nonzero at position {weight_pos}, sample: {first_nonzero_sample:?}");
            // 特徴インデックスを計算 (weight layout: [feature_index][output_dim])
            let feature_idx = weight_pos / TEST_L1;
            eprintln!("  -> Feature index: {feature_idx}");
        }

        // LayerStacks の重みの一部を確認
        let l1_bias_sample: Vec<i32> = network.layer_stacks.buckets[0].l1.biases.to_vec();
        eprintln!("L1 bias (bucket 0): {l1_bias_sample:?}");

        // 初期局面を評価
        let mut pos = Position::new();
        pos.set_sfen(SFEN_HIRATE).unwrap();

        // アクティブ特徴量を確認
        use crate::nnue::features::{FeatureSet, HalfKaHmMergedFeatureSet};
        use crate::types::Color;
        let active_black = HalfKaHmMergedFeatureSet::collect_active_indices(&pos, Color::Black);
        eprintln!("Active features for Black: {} features", active_black.len());
        let first_5: Vec<usize> = active_black.iter().take(5).collect();
        eprintln!("  First 5 indices: {first_5:?}");

        // 最初のアクティブ特徴量の重みを確認
        if let Some(first_idx) = active_black.iter().next() {
            let offset = first_idx * TEST_L1;
            eprintln!("  Weight offset for feature {first_idx}: {offset}");
            if offset + 10 <= weight_total {
                let active_weight_sample: Vec<i16> =
                    network.feature_transformer.weights[offset..offset + 10].to_vec();
                eprintln!("  Weight sample for first active feature: {active_weight_sample:?}");
            }
        }

        let mut acc = AccumulatorLayerStacks::<TEST_L1>::new();
        network.refresh_accumulator(&pos, &mut acc);

        // Accumulatorの値を確認
        let black_acc = acc.get(0);
        let white_acc = acc.get(1);
        let black_acc_sum: i64 = black_acc.iter().map(|&x| x as i64).sum();
        let white_acc_sum: i64 = white_acc.iter().map(|&x| x as i64).sum();
        eprintln!("Black acc sum: {black_acc_sum}, White acc sum: {white_acc_sum}");
        eprintln!("Black acc sample (first 10): {:?}", &black_acc[0..10]);

        // アキュムレータの統計
        let black_min = black_acc.iter().copied().min().unwrap_or(0);
        let black_max = black_acc.iter().copied().max().unwrap_or(0);
        let black_positive: usize = black_acc.iter().filter(|&&x| x > 0).count();
        eprintln!(
            "Black acc: min={black_min}, max={black_max}, positive={black_positive}/{TEST_L1}"
        );

        // 前半と後半の統計（SqrClippedReLUでペア乗算される）
        let half = TEST_L1 / 2;
        let first_half = &black_acc[0..half];
        let second_half = &black_acc[half..TEST_L1];
        let first_positive: usize = first_half.iter().filter(|&&x| x > 0).count();
        let second_positive: usize = second_half.iter().filter(|&&x| x > 0).count();
        eprintln!(
            "First half positive: {first_positive}/{half}, Second half positive: {second_positive}/{half}"
        );

        // ペア乗算で非ゼロになるペアの数
        let mut pairs_both_positive = 0usize;
        for i in 0..half {
            if first_half[i] > 0 && second_half[i] > 0 {
                pairs_both_positive += 1;
            }
        }
        eprintln!("Pairs where both halves > 0: {pairs_both_positive}/{half}");

        // SqrClippedReLU変換後の値を確認
        let mut transformed: Aligned<[u8; TEST_L1]> = Aligned([0u8; TEST_L1]);
        sqr_clipped_relu_transform(black_acc, white_acc, &mut transformed.0);
        let transformed_sum: u64 = transformed.0.iter().map(|&x| x as u64).sum();
        let transformed_nonzero: usize = transformed.0.iter().filter(|&&x| x > 0).count();
        eprintln!("Transformed sum: {transformed_sum}, nonzero count: {transformed_nonzero}");
        eprintln!("Transformed sample (first 20): {:?}", &transformed.0[0..20]);

        // バケットインデックスを計算（玉の段に基づく）
        let side_to_move = pos.side_to_move();
        let f_king = pos.king_square(side_to_move);
        let e_king = pos.king_square(!side_to_move);
        let (f_rank, e_rank) =
            crate::nnue::layer_stacks::compute_king_ranks(side_to_move, f_king, e_king);
        let bucket_index = compute_bucket_index(f_rank, e_rank);
        eprintln!("King ranks: f={f_rank}, e={e_rank}, bucket index: {bucket_index}");

        // LayerStacks の生スコアを計算
        let raw_score = network.layer_stacks.evaluate_raw(bucket_index, &transformed.0);
        eprintln!("Raw score (before /fv_scale): {raw_score}, fv_scale: {}", network.fv_scale);

        // 評価値を計算
        let value = network.evaluate(&pos, &acc);
        eprintln!("Initial position score: {}", value.raw());

        // 評価値が妥当な範囲内であることを確認（-1000〜1000）
        assert!(value.raw().abs() < 1000, "Score {} is out of expected range", value.raw());

        // 様々な局面での評価値を確認
        eprintln!("\n=== Various positions ===");
        let test_positions = [
            ("初期局面", "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1"),
            ("後手1歩得", "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPP1/1B5R1/LNSGKGSNL b p 1"),
            ("先手1歩得", "lnsgkgsnl/1r5b1/pppppppp1/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b P 1"),
            ("後手飛車落ち", "lnsgkgsnl/7b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1"),
            ("先手角得", "lnsgkgsnl/1r7/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b B 1"),
        ];

        for (name, sfen) in test_positions {
            pos.set_sfen(sfen).unwrap();
            network.refresh_accumulator(&pos, &mut acc);

            // raw score（/600前）を計算
            let (us_acc, them_acc) = (acc.get(0), acc.get(1));
            let mut transformed: Aligned<[u8; TEST_L1]> = Aligned([0u8; TEST_L1]);
            sqr_clipped_relu_transform(us_acc, them_acc, &mut transformed.0);
            let stm = pos.side_to_move();
            let f_k = pos.king_square(stm);
            let e_k = pos.king_square(!stm);
            let (f_r, e_r) = crate::nnue::layer_stacks::compute_king_ranks(stm, f_k, e_k);
            let bucket_idx = compute_bucket_index(f_r, e_r);
            let raw = network.layer_stacks.evaluate_raw(bucket_idx, &transformed.0);

            let val = network.evaluate(&pos, &acc);
            eprintln!("{:15}: {:6} (raw: {:6})", name, val.raw(), raw);
        }

        drop(routing_guard);
    }

    /// `detect_layer_stacks_feature_set` が underscore / PascalCase の arch_str を
    /// 5 FT 全てで正しく分岐することを確認する。
    ///
    /// 実 NNUE の arch_str は `LayerStacks` キーワードを含まないため、`SqrClippedReLU`
    /// と `ClippedReLU` の混在指紋で `parse_feature_set_from_arch` は `LayerStacks` を
    /// 返してしまう (旧バグの根因)。`detect_layer_stacks_feature_set` は `Features=`
    /// keyword 優先で FT を識別する。
    #[cfg(feature = "layerstack-arch")]
    #[test]
    fn test_detect_feature_set_from_real_arch_strings() {
        use crate::nnue::spec::FeatureSet as Fs;

        let cases: &[(&str, Fs)] = &[
            (
                "Features=HalfKA(Friend)[138510->1536x2],Network=AffineTransform[1<-32](ClippedReLU[32](AffineTransform[32<-30](SqrClippedReLU[30](AffineTransform[16<-3072](InputSlice[3072(0:3072)]))))),fv_scale=28",
                Fs::HalfKaSplit,
            ),
            (
                "Features=HalfKA_merged(Friend)[131949->1536x2],Network=AffineTransform[1<-32](ClippedReLU[32](AffineTransform[32<-30](SqrClippedReLU[30](AffineTransform[16<-3072](InputSlice[3072(0:3072)]))))),fv_scale=28",
                Fs::HalfKaMerged,
            ),
            (
                "Features=HalfKA_hm_split(Friend)[76950->1536x2],Network=AffineTransform[1<-32](ClippedReLU[32](AffineTransform[32<-30](SqrClippedReLU[30](AffineTransform[16<-3072](InputSlice[3072(0:3072)]))))),fv_scale=28",
                Fs::HalfKaHmSplit,
            ),
            (
                "Features=HalfKA_hm(Friend)[73305->1536x2],Network=AffineTransform[1<-32](ClippedReLU[32](AffineTransform[32<-30](SqrClippedReLU[30](AffineTransform[16<-3072](InputSlice[3072(0:3072)]))))),fv_scale=28",
                Fs::HalfKaHmMerged,
            ),
            (
                "Features=HalfKP(Friend)[125388->1536x2],Network=AffineTransform[1<-32](ClippedReLU[32](AffineTransform[32<-30](SqrClippedReLU[30](AffineTransform[16<-3072](InputSlice[3072(0:3072)]))))),fv_scale=28",
                Fs::HalfKP,
            ),
            (
                "Features=HalfKaSplit(Friend)[138510->1536x2],Network=AffineTransform[1<-32](ClippedReLU[32](AffineTransform[32<-30](SqrClippedReLU[30](AffineTransform[16<-3072](InputSlice[3072(0:3072)]))))),fv_scale=28",
                Fs::HalfKaSplit,
            ),
            (
                "Features=HalfKaMerged(Friend)[131949->1536x2],Network=AffineTransform[1<-32](ClippedReLU[32](AffineTransform[32<-30](SqrClippedReLU[30](AffineTransform[16<-3072](InputSlice[3072(0:3072)]))))),fv_scale=28",
                Fs::HalfKaMerged,
            ),
            (
                "Features=HalfKaHmSplit(Friend)[76950->1536x2],Network=AffineTransform[1<-32](ClippedReLU[32](AffineTransform[32<-30](SqrClippedReLU[30](AffineTransform[16<-3072](InputSlice[3072(0:3072)]))))),fv_scale=28",
                Fs::HalfKaHmSplit,
            ),
            (
                "Features=HalfKaHmMerged(Friend)[73305->1536x2],Network=AffineTransform[1<-32](ClippedReLU[32](AffineTransform[32<-30](SqrClippedReLU[30](AffineTransform[16<-3072](InputSlice[3072(0:3072)]))))),fv_scale=28",
                Fs::HalfKaHmMerged,
            ),
            (
                "Features=HalfKP(Friend)[125388->1536x2],PSQT=9,Network=AffineTransform[1<-32](ClippedReLU[32](AffineTransform[32<-30](SqrClippedReLU[30](AffineTransform[16<-3072](InputSlice[3072(0:3072)]))))),fv_scale=28",
                Fs::HalfKP,
            ),
            (
                "Features=HalfKaHmMerged(Friend)[73305->1536x2],PSQT=9,Network=AffineTransform[1<-32](ClippedReLU[32](AffineTransform[32<-30](SqrClippedReLU[30](AffineTransform[16<-3072](InputSlice[3072(0:3072)]))))),fv_scale=28",
                Fs::HalfKaHmMerged,
            ),
            (
                "Features=HalfKaHmMerged(Friend)[73305->1536x2],EffectBucket=2x2fixed,Network=AffineTransform[1<-32](ClippedReLU[32](AffineTransform[32<-30](SqrClippedReLU[30](AffineTransform[16<-3072](InputSlice[3072(0:3072)]))))),fv_scale=28",
                Fs::HalfKaHmMergedEffectBucket,
            ),
            (
                "Features=HalfKaHmMerged(Friend)[293220->1536x2],E4=2x2fixed,Network=AffineTransform[1<-32](ClippedReLU[32](AffineTransform[32<-30](SqrClippedReLU[30](AffineTransform[16<-3072](InputSlice[3072(0:3072)]))))),fv_scale=28",
                Fs::HalfKaHmMergedEffectBucket,
            ),
            (
                "Features=HalfKaHmMerged(Friend)[293220->1536x2],E4=4xfixed,Network=AffineTransform[1<-32](ClippedReLU[32](AffineTransform[32<-30](SqrClippedReLU[30](AffineTransform[16<-3072](InputSlice[3072(0:3072)]))))),fv_scale=28",
                Fs::HalfKaHmMergedEffectBucket,
            ),
        ];

        for (arch_str, expected) in cases {
            let got = detect_layer_stacks_feature_set(arch_str).unwrap();
            assert_eq!(
                got, *expected,
                "arch_str={arch_str:?} → expected {expected:?}, got {got:?}"
            );
        }
    }

    /// FT を特定できない旧 header との互換性のため `LayerStacks` fallback を維持する。
    #[cfg(feature = "layerstack-arch")]
    #[test]
    fn test_detect_feature_set_fallback() {
        use crate::nnue::spec::FeatureSet as Fs;
        // FT 未指定の旧 header を HalfKaHmMerged 互換で読むため fallback を維持する。
        let got = detect_layer_stacks_feature_set("LayerStacks(...)").unwrap();
        assert_eq!(got, Fs::LayerStacks);
        // FT を特定できない旧 header も同じ互換経路で読む。
        let got = detect_layer_stacks_feature_set("unknown-arch-string").unwrap();
        assert_eq!(got, Fs::LayerStacks);
        // dynamic reader と同じ substring 判定にし、旧 header でも dispatch を一致させる。
        let got = detect_layer_stacks_feature_set("legacy-HalfKP").unwrap();
        assert_eq!(got, Fs::HalfKP);
        let err = detect_layer_stacks_feature_set(
            "Features=UnknownHalfKaHmMerged(Friend)[73305->1536x2]",
        )
        .unwrap_err();
        assert!(err.contains("Unknown feature set keyword"));
    }

    #[cfg(all(
        feature = "layerstack-arch",
        feature = "layerstacks-512x16x32",
        feature = "ft-halfka_hm_merged",
        not(feature = "nnue-effect-bucket")
    ))]
    #[test]
    fn read_with_feature_set_rejects_effect_bucket_alias_without_feature() {
        use crate::nnue::spec::FeatureSet;

        fn read_error(effect_bucket_token: &str) -> io::Error {
            let arch = format!(
                "Features=HalfKaHmMerged(Friend)[293220->512x2],{effect_bucket_token},l2=16,l3=32"
            );
            let mut bytes = Vec::new();
            bytes.extend_from_slice(&NNUE_VERSION_HALFKA.to_le_bytes());
            bytes.extend_from_slice(&0u32.to_le_bytes());
            bytes.extend_from_slice(&(arch.len() as u32).to_le_bytes());
            bytes.extend_from_slice(arch.as_bytes());
            let mut reader = Cursor::new(bytes);

            // 強制された FT dispatch でも effect-bucket の互換性検査を素通りさせない。
            match LayerStacksNetwork::read_with_feature_set(
                &mut reader,
                FeatureSet::HalfKaHmMerged,
                512,
                16,
                32,
                None,
            ) {
                Ok(_) => panic!("effect bucket model must be rejected"),
                Err(err) => err,
            }
        }

        let canonical = read_error("EffectBucket=2x2fixed");
        let alias = read_error("E4=4xfixed");
        assert_eq!(canonical.kind(), io::ErrorKind::Unsupported);
        assert_eq!(alias.kind(), canonical.kind());
        assert_eq!(alias.to_string(), canonical.to_string());
    }
}
