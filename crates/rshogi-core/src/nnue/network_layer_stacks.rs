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
//! LayerStacks (両玉の相対段ベースの9バケット選択後):
//!   L1: L1 → LS_L1_OUT
//!   SqrReLU + concat: LS_L2_IN (= 2 * (LS_L1_OUT - 1))
//!   L2: LS_L2_IN → 32
//!   Output: 32 → 1 + skip
//! ```
//!
//! ## バケット選択
//!
//! 両玉の相対段（0-8）に基づいて9個のバケットから1つを選択：
//! - 味方玉の段: 0-2 → 0, 3-5 → 3, 6-8 → 6
//! - 相手玉の段: 0-2 → 0, 3-5 → 1, 6-8 → 2
//! - bucket = f_index + e_index (0-8)

use super::accumulator::Aligned;
use super::accumulator_layer_stacks::{AccumulatorLayerStacks, AccumulatorStackLayerStacks};
use super::constants::{
    DEFAULT_NUM_BUCKETS, FV_SCALE_HALFKA, MAX_ARCH_LEN, MAX_LAYER_STACK_BUCKETS,
    NNUE_VERSION_HALFKA, NNUE_VERSION_LAYERSTACK_V2,
};
#[cfg(any(
    feature = "ls-size-1536x16x32",
    feature = "ls-size-768x16x32",
    feature = "ls-size-512x16x32"
))]
use super::constants::{LAYER_STACK_16X32_L1_OUT, LAYER_STACK_16X32_L2_IN};
#[cfg(feature = "ls-size-1536x32x32")]
use super::constants::{LAYER_STACK_32X32_L1_OUT, LAYER_STACK_32X32_L2_IN};
use super::feature_transformer_layer_stacks::FeatureTransformerLayerStacks;
use super::layer_stacks::{LayerStacks, sqr_clipped_relu_transform};
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
use super::network::{
    LayerStackBucketMode, compute_layer_stack_progress8kpabs_bucket_index, get_fv_scale_override,
    get_layer_stack_bucket_mode, get_layer_stack_progress_kpabs_weights, parse_fv_scale_from_arch,
};
use crate::position::Position;
use crate::types::{Color, Value};
#[cfg(feature = "diagnostics")]
use log::info;
use std::fs::File;
#[cfg(feature = "ls-arch")]
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
        LayerStackBucketMode::Progress8KPAbs => {
            let weights = get_layer_stack_progress_kpabs_weights();
            compute_layer_stack_progress8kpabs_bucket_index(pos, side_to_move, weights, num_buckets)
        }
    }
}

/// i16 配列の要素和: dst[i] = a[i] + b[i] (SIMD 最適化)
#[cfg(feature = "ls-ext-threat")]
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
        // - `a_ptr` / `b_ptr`: 唯一の呼び出し元は `NetworkLayerStacks::evaluate`
        //   であり、そこから渡される `us_t` / `them_t` は
        //   `AccumulatorLayerStacks::get_threat()` が返す
        //   `&[i16; L1]` (親構造体 `AccumulatorLayerStacks` が
        //   `#[repr(C, align(64))]` で 64 バイトアライン）。
        //   → `_mm256_load_si256` の 32 バイトアライン要件を満たす
        // - `dst_ptr`: 呼び出し元の `sum_t: &mut Aligned<[i16; L1]>`
        //   （`#[repr(C, align(64))]`、64 バイトアライン）→ store 要件を満たす
        // - ループ回数 `L1 / 16` は const generics 由来。`add_i16_arrays` は
        //   `AccumulatorLayerStacks<L1>` で `L1 ∈ {512, 768, 1536}`（全て 16 の倍数）
        //   からのみ呼ばれるため末端要素が取り残されない
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
        // - `NNUE_VERSION_HALFKA` (= legacy `0x7AF32F20`): num_buckets field 無し、暗黙 9
        // - `NNUE_VERSION_LAYERSTACK_V2` (= `0x7AF32F21`): arch_str 直後に num_buckets u32
        //
        // HalfKP version `NNUE_VERSION (0x7AF32F16)` を `NNUE_ARCHITECTURE=LayerStacks`
        // override 経由で本関数に渡されるケースの防衛: silent な偽 9-bucket 読込を
        // 避けるため、ここで明示的に reject する。
        reader.read_exact(&mut buf4)?;
        let version = u32::from_le_bytes(buf4);
        if version != NNUE_VERSION_HALFKA && version != NNUE_VERSION_LAYERSTACK_V2 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "LayerStack reader expected version {NNUE_VERSION_HALFKA:#x} (legacy) or \
                     {NNUE_VERSION_LAYERSTACK_V2:#x} (V2), got {version:#x}. \
                     Non-LayerStack `.bin` cannot be dispatched to LayerStacks even via \
                     `NNUE_ARCHITECTURE=LayerStacks` override."
                ),
            ));
        }
        let has_num_buckets_field = version == NNUE_VERSION_LAYERSTACK_V2;

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

        // V2: num_buckets: u32 を arch_str 直後・ft_hash 直前に読む。
        // legacy: field 無し → DEFAULT_NUM_BUCKETS (9) として進める。
        // tatara `save_quantised` (`crates/nnue-format/src/layerstack_weights.rs:481`)
        // の write 順と対称 (ADR `2026-05-26` §2.2)。
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

        // FV_SCALE 検出
        let fv_scale = parse_fv_scale_from_arch(&arch_str).unwrap_or(FV_SCALE_HALFKA);

        // Factorizedモデルの検出
        if arch_str.contains("Factorizer") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Unsupported model format: factorized (non-coalesced) model detected.\n\
                     This engine only supports coalesced models.\n\n\
                     To fix: Re-export the model using nnue-pytorch serialize.py:\n\
                       python serialize.py model.ckpt output.nnue\n\n\
                     Architecture string: {arch_str}"
                ),
            ));
        }

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
        #[cfg(feature = "ls-ext-psqt")]
        {
            let has_psqt = psqt_override.unwrap_or_else(|| arch_str.contains("PSQT="));
            if has_psqt {
                feature_transformer.read_psqt(reader, num_buckets)?;
            }
        }
        #[cfg(not(feature = "ls-ext-psqt"))]
        if psqt_override.unwrap_or_else(|| arch_str.contains("PSQT=")) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "PSQT model requires ls-ext-psqt feature",
            ));
        }

        // Threat 読み込み（arch_str に "Threat=" があれば）
        #[cfg(feature = "ls-ext-threat")]
        {
            let has_threat = arch_str.contains("Threat=");
            if has_threat {
                // ThreatProfile= が arch_str にあれば profile id を読み込み検証
                // なければ旧モデル (v91 以前): profile 0 と見なす
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
        #[cfg(not(feature = "ls-ext-threat"))]
        if arch_str.contains("Threat=") {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Threat model requires ls-ext-threat feature",
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

    /// 評価値を計算
    ///
    /// 配列はMaybeUninitで確保し、直後のsqr_clipped_relu_transformで全要素が上書きされる。
    pub fn evaluate(&self, pos: &Position, acc: &AccumulatorLayerStacks<L1>) -> Value {
        let side_to_move = pos.side_to_move();
        let bucket_index = compute_layer_stacks_bucket_index(pos, side_to_move, self.num_buckets);
        self.evaluate_with_bucket(pos, acc, bucket_index)
    }

    /// 評価値を計算（事前計算済み bucket index を使用）
    pub fn evaluate_with_bucket(
        &self,
        pos: &Position,
        acc: &AccumulatorLayerStacks<L1>,
        bucket_index: usize,
    ) -> Value {
        let side_to_move = pos.side_to_move();

        // SqrClippedReLU変換
        let (us_acc, them_acc) = if side_to_move == Color::Black {
            (acc.get(Color::Black as usize), acc.get(Color::White as usize))
        } else {
            (acc.get(Color::White as usize), acc.get(Color::Black as usize))
        };

        // SAFETY: 直後のsqr_clipped_relu_transformで全要素が上書きされる
        let mut transformed: Aligned<[u8; L1]> = unsafe { Aligned::new_uninit() };

        // Threat の寄与を含めて combined accumulator を構築する。
        // 無効なら piece_acc を直接 SCReLU に渡す。
        #[cfg(feature = "ls-ext-threat")]
        {
            if self.feature_transformer.has_threat {
                let mut us_combined = Aligned([0i16; L1]);
                let mut them_combined = Aligned([0i16; L1]);
                us_combined.0.copy_from_slice(us_acc);
                them_combined.0.copy_from_slice(them_acc);

                let (us_t, them_t) = if side_to_move == Color::Black {
                    (acc.get_threat(Color::Black as usize), acc.get_threat(Color::White as usize))
                } else {
                    (acc.get_threat(Color::White as usize), acc.get_threat(Color::Black as usize))
                };
                let mut tmp_us = Aligned([0i16; L1]);
                let mut tmp_them = Aligned([0i16; L1]);
                add_i16_arrays::<L1>(&mut tmp_us.0, &us_combined.0, us_t);
                add_i16_arrays::<L1>(&mut tmp_them.0, &them_combined.0, them_t);
                us_combined = tmp_us;
                them_combined = tmp_them;

                sqr_clipped_relu_transform(&us_combined.0, &them_combined.0, &mut transformed.0);
            } else {
                sqr_clipped_relu_transform(us_acc, them_acc, &mut transformed.0);
            }
        }
        #[cfg(not(feature = "ls-ext-threat"))]
        {
            sqr_clipped_relu_transform(us_acc, them_acc, &mut transformed.0);
        }

        // LayerStacks で評価
        let raw_score = self.layer_stacks.evaluate_raw(bucket_index, &transformed.0);

        // PSQT ショートカット (Stockfish 準拠: (stm - nstm) / 2)
        // 各駒は両視点に逆符号で寄与するため、stm - nstm は正味の配置価値を
        // 約2倍にカウントする。/2 はこの二重カウントを補正する正規化。
        #[cfg(feature = "ls-ext-psqt")]
        let psqt_value = if self.feature_transformer.has_psqt {
            let stm = side_to_move as usize;
            let nstm = (!side_to_move) as usize;
            (acc.psqt_accumulation[stm][bucket_index] - acc.psqt_accumulation[nstm][bucket_index])
                / 2
        } else {
            0
        };
        #[cfg(not(feature = "ls-ext-psqt"))]
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
        #[cfg(feature = "ls-ext-threat")]
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
        #[cfg(not(feature = "ls-ext-threat"))]
        {
            sqr_clipped_relu_transform(us_acc, them_acc, &mut transformed.0);
        }

        let transformed_nonzero: usize = transformed.0.iter().filter(|&&x| x > 0).count();
        let transformed_sum: u64 = transformed.0.iter().map(|&x| x as u64).sum();
        info!("[NNUE Eval] transformed: nonzero={transformed_nonzero}/{L1}, sum={transformed_sum}");
        info!("[NNUE Eval] transformed first 32: {:?}", &transformed.0[0..32]);

        // バケットインデックスを計算（通常パスと同じ共通関数を使用）
        let bucket_index = compute_layer_stacks_bucket_index(pos, side_to_move);
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
        #[cfg(feature = "ls-ext-psqt")]
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
        #[cfg(not(feature = "ls-ext-psqt"))]
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
#[cfg(all(feature = "ls-size-1536x16x32", feature = "ft-halfka_hm_merged"))]
pub type NetworkLayerStacks1536x16x32 = NetworkLayerStacks<
    1536,
    LAYER_STACK_16X32_L1_OUT,
    LAYER_STACK_16X32_L2_IN,
    32,
    HalfKaHmMergedSpec,
>;
#[cfg(all(feature = "ls-size-1536x32x32", feature = "ft-halfka_hm_merged"))]
pub type NetworkLayerStacks1536x32x32 = NetworkLayerStacks<
    1536,
    LAYER_STACK_32X32_L1_OUT,
    LAYER_STACK_32X32_L2_IN,
    64,
    HalfKaHmMergedSpec,
>;
#[cfg(all(feature = "ls-size-768x16x32", feature = "ft-halfka_hm_merged"))]
pub type NetworkLayerStacks768x16x32 = NetworkLayerStacks<
    768,
    LAYER_STACK_16X32_L1_OUT,
    LAYER_STACK_16X32_L2_IN,
    32,
    HalfKaHmMergedSpec,
>;
#[cfg(all(feature = "ls-size-512x16x32", feature = "ft-halfka_hm_merged"))]
pub type NetworkLayerStacks512x16x32 = NetworkLayerStacks<
    512,
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
/// この内部 enum が L1 軸 (4 サイズのうち 1 つ) で dispatch する。
///
/// **重要**: 大会ビルドでは必ず単一 (FT, L1) のみを有効化すること。複数
/// バリアントを同時有効にすると dispatch match の overhead が出る (実測 ~5%)。
pub enum LsNetByFt<FT: LsFeatureSpec + 'static> {
    #[cfg(feature = "ls-size-1536x16x32")]
    L1536x16x32(
        Box<NetworkLayerStacks<1536, LAYER_STACK_16X32_L1_OUT, LAYER_STACK_16X32_L2_IN, 32, FT>>,
    ),
    #[cfg(feature = "ls-size-1536x32x32")]
    L1536x32x32(
        Box<NetworkLayerStacks<1536, LAYER_STACK_32X32_L1_OUT, LAYER_STACK_32X32_L2_IN, 64, FT>>,
    ),
    #[cfg(feature = "ls-size-768x16x32")]
    L768x16x32(
        Box<NetworkLayerStacks<768, LAYER_STACK_16X32_L1_OUT, LAYER_STACK_16X32_L2_IN, 32, FT>>,
    ),
    #[cfg(feature = "ls-size-512x16x32")]
    L512x16x32(
        Box<NetworkLayerStacks<512, LAYER_STACK_16X32_L1_OUT, LAYER_STACK_16X32_L2_IN, 32, FT>>,
    ),
    #[cfg(not(any(
        feature = "ls-size-1536x16x32",
        feature = "ls-size-1536x32x32",
        feature = "ls-size-768x16x32",
        feature = "ls-size-512x16x32",
    )))]
    _Unused(std::convert::Infallible, PhantomData<FT>),
}

/// `LsNetByFt<FT>` の variants 上で同じ式を展開する dispatch マクロ。
///
/// すべての ls-size-* feature が無効の場合 (例: WASM ビルド) は本来到達不能で、
/// wildcard arm が必要になる。
macro_rules! ls_match_size {
    ($val:expr, $pat:ident => $body:expr) => {
        match $val {
            #[cfg(feature = "ls-size-1536x16x32")]
            LsNetByFt::L1536x16x32($pat) => $body,
            #[cfg(feature = "ls-size-1536x32x32")]
            LsNetByFt::L1536x32x32($pat) => $body,
            #[cfg(feature = "ls-size-768x16x32")]
            LsNetByFt::L768x16x32($pat) => $body,
            #[cfg(feature = "ls-size-512x16x32")]
            LsNetByFt::L512x16x32($pat) => $body,
            #[cfg(not(any(
                feature = "ls-size-1536x16x32",
                feature = "ls-size-1536x32x32",
                feature = "ls-size-768x16x32",
                feature = "ls-size-512x16x32",
            )))]
            _ => unreachable!("no LayerStacks size variant enabled"),
        }
    };
}

impl<FT: LsFeatureSpec + 'static> LsNetByFt<FT> {
    /// L1 サイズを取得
    pub fn l1_size(&self) -> usize {
        match self {
            #[cfg(feature = "ls-size-1536x16x32")]
            Self::L1536x16x32(_) => 1536,
            #[cfg(feature = "ls-size-1536x32x32")]
            Self::L1536x32x32(_) => 1536,
            #[cfg(feature = "ls-size-768x16x32")]
            Self::L768x16x32(_) => 768,
            #[cfg(feature = "ls-size-512x16x32")]
            Self::L512x16x32(_) => 512,
            #[cfg(not(any(
                feature = "ls-size-1536x16x32",
                feature = "ls-size-1536x32x32",
                feature = "ls-size-768x16x32",
                feature = "ls-size-512x16x32",
            )))]
            _ => unreachable!("no LayerStacks size variant enabled"),
        }
    }

    /// (L1, L2, L3) を取得
    pub fn architecture_dims(&self) -> (usize, usize, usize) {
        match self {
            #[cfg(feature = "ls-size-1536x16x32")]
            Self::L1536x16x32(_) => (1536, 16, 32),
            #[cfg(feature = "ls-size-1536x32x32")]
            Self::L1536x32x32(_) => (1536, 32, 32),
            #[cfg(feature = "ls-size-768x16x32")]
            Self::L768x16x32(_) => (768, 16, 32),
            #[cfg(feature = "ls-size-512x16x32")]
            Self::L512x16x32(_) => (512, 16, 32),
            #[cfg(not(any(
                feature = "ls-size-1536x16x32",
                feature = "ls-size-1536x32x32",
                feature = "ls-size-768x16x32",
                feature = "ls-size-512x16x32",
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

    /// (L1, L2, L3) と PSQT override から読み込み (FT は型レベルで固定)。
    #[cfg(feature = "ls-arch")]
    fn read_with_options<R: Read + Seek>(
        reader: &mut R,
        l1: usize,
        l2: usize,
        l3: usize,
        psqt_override: Option<bool>,
    ) -> io::Result<Self> {
        match (l1, l2, l3) {
            #[cfg(feature = "ls-size-1536x16x32")]
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
            #[cfg(feature = "ls-size-1536x32x32")]
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
            #[cfg(feature = "ls-size-768x16x32")]
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
            #[cfg(feature = "ls-size-512x16x32")]
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
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Unsupported LayerStacks architecture: {l1}x{l2}x{l3}"),
            )),
        }
    }

    /// 評価値を計算 (stack の L1 と一致する variant 上で実行)。
    #[cfg(feature = "ls-arch")]
    pub fn evaluate(
        &self,
        pos: &Position,
        stack: &super::accumulator_layer_stacks::LayerStacksAccStack,
    ) -> Value {
        // (self, stack) tuple match で同じ L1 variant の組のみ matched arm を持つ。
        // 2 サイズ以上 enable のときだけ cross-pair の不一致 arm が到達可能で、
        // 単一 size build では 1 arm の match 自体が exhaustive となる。
        //
        // 以下の 6-pair cfg は本 file 内で 4 箇所 (本 fallback / update_accumulator
        // の net_dims / stack_dims / fallback) に同じ式を持つ。LS サイズ追加時は
        // すべての any(all(...)) を C(N,2) に揃えて同期更新すること (match arm は
        // item ではないため共通 cfg を file-local macro に括り出せない)。
        match (self, stack) {
            #[cfg(feature = "ls-size-1536x16x32")]
            (
                Self::L1536x16x32(net),
                super::accumulator_layer_stacks::LayerStacksAccStack::L1536x16x32(st),
            ) => net.evaluate(pos, &st.current().accumulator),
            #[cfg(feature = "ls-size-1536x32x32")]
            (
                Self::L1536x32x32(net),
                super::accumulator_layer_stacks::LayerStacksAccStack::L1536x32x32(st),
            ) => net.evaluate(pos, &st.current().accumulator),
            #[cfg(feature = "ls-size-768x16x32")]
            (
                Self::L768x16x32(net),
                super::accumulator_layer_stacks::LayerStacksAccStack::L768x16x32(st),
            ) => net.evaluate(pos, &st.current().accumulator),
            #[cfg(feature = "ls-size-512x16x32")]
            (
                Self::L512x16x32(net),
                super::accumulator_layer_stacks::LayerStacksAccStack::L512x16x32(st),
            ) => net.evaluate(pos, &st.current().accumulator),
            #[cfg(any(
                all(feature = "ls-size-1536x16x32", feature = "ls-size-1536x32x32"),
                all(feature = "ls-size-1536x16x32", feature = "ls-size-768x16x32"),
                all(feature = "ls-size-1536x16x32", feature = "ls-size-512x16x32"),
                all(feature = "ls-size-1536x32x32", feature = "ls-size-768x16x32"),
                all(feature = "ls-size-1536x32x32", feature = "ls-size-512x16x32"),
                all(feature = "ls-size-768x16x32", feature = "ls-size-512x16x32"),
            ))]
            _ => panic!(
                "LayerStacksNetwork / LayerStacksAccStack の L1 サイズが不一致 (net={:?}, stack={:?})",
                self.architecture_dims(),
                stack.architecture_dims()
            ),
        }
    }

    /// アキュムレータを更新 (キャッシュ対応)。
    #[cfg(feature = "ls-arch")]
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
            all(feature = "ls-size-1536x16x32", feature = "ls-size-1536x32x32"),
            all(feature = "ls-size-1536x16x32", feature = "ls-size-768x16x32"),
            all(feature = "ls-size-1536x16x32", feature = "ls-size-512x16x32"),
            all(feature = "ls-size-1536x32x32", feature = "ls-size-768x16x32"),
            all(feature = "ls-size-1536x32x32", feature = "ls-size-512x16x32"),
            all(feature = "ls-size-768x16x32", feature = "ls-size-512x16x32"),
        ))]
        let net_dims = self.architecture_dims();
        #[cfg(any(
            all(feature = "ls-size-1536x16x32", feature = "ls-size-1536x32x32"),
            all(feature = "ls-size-1536x16x32", feature = "ls-size-768x16x32"),
            all(feature = "ls-size-1536x16x32", feature = "ls-size-512x16x32"),
            all(feature = "ls-size-1536x32x32", feature = "ls-size-768x16x32"),
            all(feature = "ls-size-1536x32x32", feature = "ls-size-512x16x32"),
            all(feature = "ls-size-768x16x32", feature = "ls-size-512x16x32"),
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
                    if let Some((source_idx, _depth)) = $stack.find_usable_accumulator() {
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
            #[cfg(feature = "ls-size-1536x16x32")]
            (
                Self::L1536x16x32(net),
                super::accumulator_layer_stacks::LayerStacksAccStack::L1536x16x32(st),
            ) => {
                do_update!(net, st, L1536x16x32);
            }
            #[cfg(feature = "ls-size-1536x32x32")]
            (
                Self::L1536x32x32(net),
                super::accumulator_layer_stacks::LayerStacksAccStack::L1536x32x32(st),
            ) => {
                do_update!(net, st, L1536x32x32);
            }
            #[cfg(feature = "ls-size-768x16x32")]
            (
                Self::L768x16x32(net),
                super::accumulator_layer_stacks::LayerStacksAccStack::L768x16x32(st),
            ) => {
                do_update!(net, st, L768x16x32);
            }
            #[cfg(feature = "ls-size-512x16x32")]
            (
                Self::L512x16x32(net),
                super::accumulator_layer_stacks::LayerStacksAccStack::L512x16x32(st),
            ) => {
                do_update!(net, st, L512x16x32);
            }
            #[cfg(any(
                all(feature = "ls-size-1536x16x32", feature = "ls-size-1536x32x32"),
                all(feature = "ls-size-1536x16x32", feature = "ls-size-768x16x32"),
                all(feature = "ls-size-1536x16x32", feature = "ls-size-512x16x32"),
                all(feature = "ls-size-1536x32x32", feature = "ls-size-768x16x32"),
                all(feature = "ls-size-1536x32x32", feature = "ls-size-512x16x32"),
                all(feature = "ls-size-768x16x32", feature = "ls-size-512x16x32"),
            ))]
            _ => panic!(
                "LayerStacksNetwork / LayerStacksAccStack の L1 サイズが不一致 (net={:?}, stack={:?})",
                net_dims, stack_dims
            ),
        }
    }

    /// 新しい L1 サイズに対応する AccStack を作成
    #[cfg(feature = "ls-arch")]
    pub fn new_acc_stack(&self) -> super::accumulator_layer_stacks::LayerStacksAccStack {
        match self {
            #[cfg(feature = "ls-size-1536x16x32")]
            Self::L1536x16x32(_) => {
                super::accumulator_layer_stacks::LayerStacksAccStack::L1536x16x32(
                    super::accumulator_layer_stacks::AccumulatorStackLayerStacks::<1536>::new(),
                )
            }
            #[cfg(feature = "ls-size-1536x32x32")]
            Self::L1536x32x32(_) => {
                super::accumulator_layer_stacks::LayerStacksAccStack::L1536x32x32(
                    super::accumulator_layer_stacks::AccumulatorStackLayerStacks::<1536>::new(),
                )
            }
            #[cfg(feature = "ls-size-768x16x32")]
            Self::L768x16x32(_) => {
                super::accumulator_layer_stacks::LayerStacksAccStack::L768x16x32(
                    super::accumulator_layer_stacks::AccumulatorStackLayerStacks::<768>::new(),
                )
            }
            #[cfg(feature = "ls-size-512x16x32")]
            Self::L512x16x32(_) => {
                super::accumulator_layer_stacks::LayerStacksAccStack::L512x16x32(
                    super::accumulator_layer_stacks::AccumulatorStackLayerStacks::<512>::new(),
                )
            }
            #[cfg(not(any(
                feature = "ls-size-1536x16x32",
                feature = "ls-size-1536x32x32",
                feature = "ls-size-768x16x32",
                feature = "ls-size-512x16x32",
            )))]
            _ => unreachable!("no LayerStacks size variant enabled"),
        }
    }

    /// 新しい L1 サイズに対応する AccCache を作成
    #[cfg(feature = "ls-arch")]
    pub fn new_acc_cache(&self) -> super::accumulator_layer_stacks::LayerStacksAccCache {
        match self {
            #[cfg(feature = "ls-size-1536x16x32")]
            Self::L1536x16x32(_) => {
                super::accumulator_layer_stacks::LayerStacksAccCache::L1536x16x32(
                    super::accumulator_layer_stacks::AccumulatorCacheLayerStacks::<1536>::new(),
                )
            }
            #[cfg(feature = "ls-size-1536x32x32")]
            Self::L1536x32x32(_) => {
                super::accumulator_layer_stacks::LayerStacksAccCache::L1536x32x32(
                    super::accumulator_layer_stacks::AccumulatorCacheLayerStacks::<1536>::new(),
                )
            }
            #[cfg(feature = "ls-size-768x16x32")]
            Self::L768x16x32(_) => {
                super::accumulator_layer_stacks::LayerStacksAccCache::L768x16x32(
                    super::accumulator_layer_stacks::AccumulatorCacheLayerStacks::<768>::new(),
                )
            }
            #[cfg(feature = "ls-size-512x16x32")]
            Self::L512x16x32(_) => {
                super::accumulator_layer_stacks::LayerStacksAccCache::L512x16x32(
                    super::accumulator_layer_stacks::AccumulatorCacheLayerStacks::<512>::new(),
                )
            }
            #[cfg(not(any(
                feature = "ls-size-1536x16x32",
                feature = "ls-size-1536x32x32",
                feature = "ls-size-768x16x32",
                feature = "ls-size-512x16x32",
            )))]
            _ => unreachable!("no LayerStacks size variant enabled"),
        }
    }

    /// `eval diag` 用: refresh + evaluate_with_diagnostics を全 L1 variant 上で実行する。
    ///
    /// `LayerStacksNetwork::refresh_and_evaluate_with_diagnostics` から委譲される。
    #[cfg(all(feature = "ls-arch", feature = "diagnostics"))]
    pub fn refresh_and_evaluate_with_diagnostics(&self, pos: &Position) -> Value {
        match self {
            #[cfg(feature = "ls-size-1536x16x32")]
            Self::L1536x16x32(net) => {
                let mut acc = AccumulatorLayerStacks::<1536>::new();
                net.refresh_accumulator(pos, &mut acc);
                net.evaluate_with_diagnostics(pos, &acc)
            }
            #[cfg(feature = "ls-size-1536x32x32")]
            Self::L1536x32x32(net) => {
                let mut acc = AccumulatorLayerStacks::<1536>::new();
                net.refresh_accumulator(pos, &mut acc);
                net.evaluate_with_diagnostics(pos, &acc)
            }
            #[cfg(feature = "ls-size-768x16x32")]
            Self::L768x16x32(net) => {
                let mut acc = AccumulatorLayerStacks::<768>::new();
                net.refresh_accumulator(pos, &mut acc);
                net.evaluate_with_diagnostics(pos, &acc)
            }
            #[cfg(feature = "ls-size-512x16x32")]
            Self::L512x16x32(net) => {
                let mut acc = AccumulatorLayerStacks::<512>::new();
                net.refresh_accumulator(pos, &mut acc);
                net.evaluate_with_diagnostics(pos, &acc)
            }
            #[cfg(not(any(
                feature = "ls-size-1536x16x32",
                feature = "ls-size-1536x32x32",
                feature = "ls-size-768x16x32",
                feature = "ls-size-512x16x32",
            )))]
            _ => unreachable!("no LayerStacks size variant enabled"),
        }
    }
}

/// LayerStacks ネットワークの FT 軸 dispatch enum。各 variant の内部に L1 軸 dispatch
/// `LsNetByFt<FT>` を持つ二段構造。
///
/// active な FT variant は `ft-*` feature で、active な L1 variant は `ls-size-*` feature で
/// 制御される。
pub enum LayerStacksNetwork {
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
/// 5 FT × 4 L1 = 20 (FT, L1) 組合せを `all(ft-*, ls-size-*)` cfg gate 付きで列挙し、
/// マッチした variant の内部 `&NetworkLayerStacks<L1, ..., FT>` (concrete 型) を
/// `$inner` として body に渡す。tools crate (bench / eval / verify) の dispatch
/// マクロをこれに統合することで、FT/L1 軸が増えたときの 3 ファイル同時更新漏れを防ぐ。
///
/// マッチアームは呼び出し crate 側の cfg を見て展開されるため、`rshogi-core` 側で
/// 有効な variant を caller 側がすべては有効化していない場合に備えて `_ =>`
/// fallback を引数として受け取る。caller のすべての (`ft-*`, `ls-size-*`) feature が
/// 揃っている = 20 arm で exhaustive なときは fallback arm を cfg gate でドロップし、
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
            #[cfg(all(feature = "ft-halfka_hm_merged", feature = "ls-size-1536x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaHmMerged(
                $crate::nnue::LsNetByFt::L1536x16x32($inner),
            ) => $body,
            #[cfg(all(feature = "ft-halfka_hm_merged", feature = "ls-size-1536x32x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaHmMerged(
                $crate::nnue::LsNetByFt::L1536x32x32($inner),
            ) => $body,
            #[cfg(all(feature = "ft-halfka_hm_merged", feature = "ls-size-768x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaHmMerged(
                $crate::nnue::LsNetByFt::L768x16x32($inner),
            ) => $body,
            #[cfg(all(feature = "ft-halfka_hm_merged", feature = "ls-size-512x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaHmMerged(
                $crate::nnue::LsNetByFt::L512x16x32($inner),
            ) => $body,
            #[cfg(all(feature = "ft-halfka_hm_split", feature = "ls-size-1536x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaHmSplit(
                $crate::nnue::LsNetByFt::L1536x16x32($inner),
            ) => $body,
            #[cfg(all(feature = "ft-halfka_hm_split", feature = "ls-size-1536x32x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaHmSplit(
                $crate::nnue::LsNetByFt::L1536x32x32($inner),
            ) => $body,
            #[cfg(all(feature = "ft-halfka_hm_split", feature = "ls-size-768x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaHmSplit(
                $crate::nnue::LsNetByFt::L768x16x32($inner),
            ) => $body,
            #[cfg(all(feature = "ft-halfka_hm_split", feature = "ls-size-512x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaHmSplit(
                $crate::nnue::LsNetByFt::L512x16x32($inner),
            ) => $body,
            #[cfg(all(feature = "ft-halfka_merged", feature = "ls-size-1536x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaMerged(
                $crate::nnue::LsNetByFt::L1536x16x32($inner),
            ) => $body,
            #[cfg(all(feature = "ft-halfka_merged", feature = "ls-size-1536x32x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaMerged(
                $crate::nnue::LsNetByFt::L1536x32x32($inner),
            ) => $body,
            #[cfg(all(feature = "ft-halfka_merged", feature = "ls-size-768x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaMerged(
                $crate::nnue::LsNetByFt::L768x16x32($inner),
            ) => $body,
            #[cfg(all(feature = "ft-halfka_merged", feature = "ls-size-512x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaMerged(
                $crate::nnue::LsNetByFt::L512x16x32($inner),
            ) => $body,
            #[cfg(all(feature = "ft-halfka_split", feature = "ls-size-1536x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaSplit(
                $crate::nnue::LsNetByFt::L1536x16x32($inner),
            ) => $body,
            #[cfg(all(feature = "ft-halfka_split", feature = "ls-size-1536x32x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaSplit(
                $crate::nnue::LsNetByFt::L1536x32x32($inner),
            ) => $body,
            #[cfg(all(feature = "ft-halfka_split", feature = "ls-size-768x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaSplit($crate::nnue::LsNetByFt::L768x16x32(
                $inner,
            )) => $body,
            #[cfg(all(feature = "ft-halfka_split", feature = "ls-size-512x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKaSplit($crate::nnue::LsNetByFt::L512x16x32(
                $inner,
            )) => $body,
            #[cfg(all(feature = "ft-halfkp", feature = "ls-size-1536x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKP($crate::nnue::LsNetByFt::L1536x16x32(
                $inner,
            )) => $body,
            #[cfg(all(feature = "ft-halfkp", feature = "ls-size-1536x32x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKP($crate::nnue::LsNetByFt::L1536x32x32(
                $inner,
            )) => $body,
            #[cfg(all(feature = "ft-halfkp", feature = "ls-size-768x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKP($crate::nnue::LsNetByFt::L768x16x32(
                $inner,
            )) => $body,
            #[cfg(all(feature = "ft-halfkp", feature = "ls-size-512x16x32"))]
            $crate::nnue::LayerStacksNetwork::HalfKP($crate::nnue::LsNetByFt::L512x16x32(
                $inner,
            )) => $body,
            // caller の (ft-*, ls-size-*) を 5 × 4 全部有効化したときは 20 arm が
            // exhaustive。そのときだけ fallback arm を cfg gate でドロップして
            // unreachable warning を避ける。
            #[cfg(not(all(
                feature = "ft-halfka_hm_merged",
                feature = "ft-halfka_hm_split",
                feature = "ft-halfka_merged",
                feature = "ft-halfka_split",
                feature = "ft-halfkp",
                feature = "ls-size-1536x16x32",
                feature = "ls-size-1536x32x32",
                feature = "ls-size-768x16x32",
                feature = "ls-size-512x16x32",
            )))]
            _ => $fallback,
        }
    };
}

/// LayerStacksNetwork の FT variants を網羅する dispatch マクロ。
///
/// 全 FT feature が無効の場合 (現状の build.rs check では ls-arch + ft-* >= 1 を必須化
/// しているため発生しないが、念のため) は wildcard arm でコンパイルを通す。
macro_rules! ls_match_ft {
    ($val:expr, $pat:ident => $body:expr) => {
        match $val {
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

    /// アーキテクチャ仕様を取得
    pub fn architecture_spec(&self) -> super::spec::ArchitectureSpec {
        ls_match_ft!(self, by_ft => by_ft.architecture_spec())
    }

    /// FV_SCALE を取得
    pub fn fv_scale(&self) -> i32 {
        ls_match_ft!(self, by_ft => by_ft.fv_scale())
    }

    /// ファイルから読み込み (FT は arch_str から検出、L1/L2/L3 は呼び出し元が渡す)。
    #[cfg(feature = "ls-arch")]
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
    #[cfg(feature = "ls-arch")]
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
    #[cfg(feature = "ls-arch")]
    pub fn evaluate(
        &self,
        pos: &Position,
        stack: &super::accumulator_layer_stacks::LayerStacksAccStack,
    ) -> Value {
        ls_match_ft!(self, by_ft => by_ft.evaluate(pos, stack))
    }

    /// アキュムレータを更新 (キャッシュ対応)
    #[cfg(feature = "ls-arch")]
    pub fn update_accumulator(
        &self,
        pos: &Position,
        stack: &mut super::accumulator_layer_stacks::LayerStacksAccStack,
        cache: &mut Option<super::accumulator_layer_stacks::LayerStacksAccCache>,
    ) {
        ls_match_ft!(self, by_ft => by_ft.update_accumulator(pos, stack, cache))
    }

    /// 新しい L1 サイズに対応する AccStack を作成
    #[cfg(feature = "ls-arch")]
    pub fn new_acc_stack(&self) -> super::accumulator_layer_stacks::LayerStacksAccStack {
        ls_match_ft!(self, by_ft => by_ft.new_acc_stack())
    }

    /// 新しい L1 サイズに対応する AccCache を作成
    #[cfg(feature = "ls-arch")]
    pub fn new_acc_cache(&self) -> super::accumulator_layer_stacks::LayerStacksAccCache {
        ls_match_ft!(self, by_ft => by_ft.new_acc_cache())
    }

    /// 診断ログ向け: refresh + evaluate_with_diagnostics を全 FT × L1 variant 上で実行する。
    ///
    /// `eval diag` USI コマンドから呼ばれる。FT/L1 軸をすべて束ねた high-level helper。
    #[cfg(all(feature = "ls-arch", feature = "diagnostics"))]
    pub fn refresh_and_evaluate_with_diagnostics(&self, pos: &Position) -> Value {
        ls_match_ft!(self, by_ft => by_ft.refresh_and_evaluate_with_diagnostics(pos))
    }
}

/// reader の現在位置から LayerStacks ヘッダの arch_str を peek し、FT を判別する。
///
/// tatara emit 形式の arch_str は `Features=<FT>(Friend)[<dim>->1536x2],...` で、
/// `Features=` 直後のキーワード (HalfKaHmMerged / HalfKaHmSplit / HalfKaMerged /
/// HalfKaSplit / HalfKP の PascalCase 5 種) を最優先で読み取る。無ければ汎用
/// `parse_feature_set_from_arch` (LayerStacks fallback 含む) に委譲する。完全に
/// 判別不能なモデルは `FeatureSet::LayerStacks` を返し、上位の `read_with_feature_set`
/// で HalfKaHmMerged 互換扱いになる。
///
/// 読み取り後は `Seek::seek(SeekFrom::Start(original))` で reader 位置を巻き戻す。
/// `BufReader<File>` 等の seekable reader では seek 時に内部 buffer が破棄・再同期される
/// ため、後続の本読み込みに影響しない。peek 自体が失敗しても巻き戻しは試みる。
#[cfg(feature = "ls-arch")]
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
        Ok(detect_layer_stacks_feature_set(&arch_str))
    })();
    reader.seek(SeekFrom::Start(original))?;
    result
}

/// arch_str から LS の FT を判別する pure helper (peek の純粋ロジック部分)。
#[cfg(feature = "ls-arch")]
fn detect_layer_stacks_feature_set(arch_str: &str) -> super::spec::FeatureSet {
    use super::spec::FeatureSet as Fs;
    if let Some(name) = super::spec::parse_feature_set_keyword(arch_str) {
        match name {
            "HalfKP" => return Fs::HalfKP,
            "HalfKaSplit" => return Fs::HalfKaSplit,
            "HalfKaMerged" => return Fs::HalfKaMerged,
            "HalfKaHmSplit" => return Fs::HalfKaHmSplit,
            "HalfKaHmMerged" => return Fs::HalfKaHmMerged,
            _ => {}
        }
    }
    super::spec::parse_feature_set_from_arch(arch_str).unwrap_or(Fs::LayerStacks)
}

#[cfg(test)]
mod tests {
    #[cfg(all(feature = "ls-size-1536x16x32", feature = "ft-halfka_hm_merged"))]
    use super::*;
    use crate::nnue::constants::{FV_SCALE_HALFKA, NNUE_PYTORCH_L1};
    #[cfg(all(feature = "ls-size-1536x16x32", feature = "ft-halfka_hm_merged"))]
    use crate::position::{Position, SFEN_HIRATE};

    const TEST_L1: usize = NNUE_PYTORCH_L1;

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
    #[cfg(all(feature = "ls-size-1536x16x32", feature = "ft-halfka_hm_merged"))]
    #[test]
    #[ignore]
    fn test_load_layer_stacks_file() {
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
    }

    /// `detect_layer_stacks_feature_set` が tatara emit 形式の arch_str (PascalCase) を
    /// 5 FT 全てで正しく分岐することを確認する。
    ///
    /// 実 NNUE の arch_str は `LayerStacks` キーワードを含まないため、`SqrClippedReLU`
    /// と `ClippedReLU` の混在指紋で `parse_feature_set_from_arch` は `LayerStacks` を
    /// 返してしまう (旧バグの根因)。`detect_layer_stacks_feature_set` は `Features=`
    /// keyword 優先で FT を識別する。
    #[cfg(feature = "ls-arch")]
    #[test]
    fn test_detect_feature_set_from_real_arch_strings() {
        use crate::nnue::spec::FeatureSet as Fs;

        let cases: &[(&str, Fs)] = &[
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
        ];

        for (arch_str, expected) in cases {
            let got = detect_layer_stacks_feature_set(arch_str);
            assert_eq!(
                got, *expected,
                "arch_str={arch_str:?} → expected {expected:?}, got {got:?}"
            );
        }
    }

    /// `Features=` keyword が見つからない / 不明な FT のみ、`LayerStacks` fallback に
    /// 落ちることを確認する。
    #[cfg(feature = "ls-arch")]
    #[test]
    fn test_detect_feature_set_fallback() {
        use crate::nnue::spec::FeatureSet as Fs;
        // Features= が無く LayerStacks キーワードがあるケース → fallback で LayerStacks
        let got = detect_layer_stacks_feature_set("LayerStacks(...)");
        assert_eq!(got, Fs::LayerStacks);
        // 完全に未知 → fallback の unwrap_or で LayerStacks
        let got = detect_layer_stacks_feature_set("unknown-arch-string");
        assert_eq!(got, Fs::LayerStacks);
    }
}
