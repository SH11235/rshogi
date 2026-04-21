//! NetworkLayerStacks - LayerStacksアーキテクチャのNNUEネットワーク
//!
//! HalfKA_hm^ 特徴量 + LayerStacks 構造の NNUE を実装する。
//! nnue-pytorch で学習したファイルを読み込み、評価を行う。
//!
//! ## アーキテクチャ
//!
//! ```text
//! Feature Transformer (HalfKA_hm^): 73,305 → L1 (各視点)
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
use super::constants::{FV_SCALE_HALFKA, MAX_ARCH_LEN};
#[cfg(any(
    feature = "layerstacks-1536x16x32",
    feature = "layerstacks-768x16x32",
    feature = "layerstacks-512x16x32"
))]
use super::constants::{LAYER_STACK_16X32_L1_OUT, LAYER_STACK_16X32_L2_IN};
#[cfg(feature = "layerstacks-1536x32x32")]
use super::constants::{LAYER_STACK_32X32_L1_OUT, LAYER_STACK_32X32_L2_IN};
use super::feature_transformer_layer_stacks::FeatureTransformerLayerStacks;
use super::layer_stacks::{LayerStacks, sqr_clipped_relu_transform};
use super::network::{
    LayerStackBucketMode, compute_layer_stack_progress8kpabs_bucket_index, get_fv_scale_override,
    get_layer_stack_bucket_mode, get_layer_stack_progress_kpabs_weights, parse_fv_scale_from_arch,
    parse_hand_count_dense_dims,
};
use crate::position::Position;
use crate::types::{Color, Value};
#[cfg(feature = "diagnostics")]
use log::info;
use std::fs::File;
use std::io::{self, BufReader, Cursor, Read, Seek};
use std::path::Path;

#[inline]
fn compute_layer_stacks_bucket_index(pos: &Position, side_to_move: Color) -> usize {
    match get_layer_stack_bucket_mode() {
        LayerStackBucketMode::Progress8KPAbs => {
            let weights = get_layer_stack_progress_kpabs_weights();
            compute_layer_stack_progress8kpabs_bucket_index(pos, side_to_move, weights)
        }
    }
}

/// i16 配列の要素和: dst[i] = a[i] + b[i] (SIMD 最適化)
#[cfg(feature = "nnue-threat")]
#[inline]
fn add_i16_arrays<const L1: usize>(dst: &mut [i16; L1], a: &[i16; L1], b: &[i16; L1]) {
    // AVX2 ループは `L1 / 16` 回で全要素を処理する前提。L1 が 16 の倍数で
    // ない場合は末端要素が取り残されるため、monomorphization 時に失敗させる。
    const {
        assert!(L1 % 16 == 0, "L1 must be a multiple of 16 for AVX2 SIMD loops");
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
/// HalfKA_hm^ 特徴量（73,305次元）+ L1次元 Feature Transformer + 9バケット LayerStacks
pub struct NetworkLayerStacks<
    const L1: usize,
    const LS_L1_OUT: usize,
    const LS_L2_IN: usize,
    const LS_L2_PADDED_INPUT: usize,
> {
    /// Feature Transformer (73,305 → L1)
    pub feature_transformer: FeatureTransformerLayerStacks<L1>,
    /// LayerStacks (9バケット)
    pub layer_stacks: LayerStacks<L1, LS_L1_OUT, LS_L2_IN, LS_L2_PADDED_INPUT>,
    /// 評価値スケーリング係数（アーキテクチャ文字列から取得、USIオプションでオーバーライド可）
    pub fv_scale: i32,
}

impl<
    const L1: usize,
    const LS_L1_OUT: usize,
    const LS_L2_IN: usize,
    const LS_L2_PADDED_INPUT: usize,
> NetworkLayerStacks<L1, LS_L1_OUT, LS_L2_IN, LS_L2_PADDED_INPUT>
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

        // version（呼び出し元で検証済み）
        reader.read_exact(&mut buf4)?;

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
        // nnue-psqt/nnue-threat feature 有効時は
        // read_psqt/read_threat_weights で変更するため mut
        #[cfg(any(feature = "nnue-psqt", feature = "nnue-threat"))]
        let mut feature_transformer = FeatureTransformerLayerStacks::read_leb128(reader)?;
        #[cfg(not(any(feature = "nnue-psqt", feature = "nnue-threat")))]
        let feature_transformer = FeatureTransformerLayerStacks::read_leb128(reader)?;

        // PSQT 読み込み:
        // - psqt_override == Some(true): USI オプションで PSQT 強制 ON（arch_str を無視）
        // - psqt_override == Some(false): USI オプションで PSQT 強制 OFF（arch_str を無視）
        // - psqt_override == None: arch_str から自動検出
        #[cfg(feature = "nnue-psqt")]
        {
            let has_psqt = psqt_override.unwrap_or_else(|| arch_str.contains("PSQT="));
            if has_psqt {
                feature_transformer.read_psqt(reader)?;
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
        #[cfg(not(feature = "nnue-threat"))]
        if arch_str.contains("Threat=") {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Threat model requires nnue-threat feature",
            ));
        }

        // HandCount Dense 検出
        //
        // arch_str に `HandCountDense={dims},` が含まれるモデルは LayerStack の L1
        // 重みを `pad32(ft_out + dims)` byte/row で書き出している。先頭 ft_out は FT
        // 由来の通常重み、続く dims が HandCount Dense 入力用、残りは padding。
        //
        // - `nnue-hand-count-dense` feature 有効時: `LayerStacks::read_with_hand_count`
        //   で HC 重みを分離して格納し、評価時に `extract_hand_count(pos)` と合わせて
        //   L1 出力に寄与させる。
        // - feature 無効時: 現状の reader ではバイト境界がずれるため明示的に拒否する
        //   (破損した重みを読み込まないための safeguard)。
        let hand_count_dims = match parse_hand_count_dense_dims(&arch_str) {
            Ok(v) => v,
            Err(msg) => return Err(io::Error::new(io::ErrorKind::InvalidData, msg)),
        };

        // LayerStacks を読み込み（FC層は常に非圧縮）
        #[cfg(feature = "nnue-hand-count-dense")]
        let layer_stacks = if let Some(dims) = hand_count_dims {
            LayerStacks::read_with_hand_count(reader, dims)?
        } else {
            LayerStacks::read(reader)?
        };
        #[cfg(not(feature = "nnue-hand-count-dense"))]
        let layer_stacks = {
            if hand_count_dims.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "HandCountDense model requires nnue-hand-count-dense feature.\n\
                     Rebuild with `--features nnue-hand-count-dense` to load this model.",
                ));
            }
            LayerStacks::read(reader)?
        };

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

        Ok(Self {
            feature_transformer,
            layer_stacks,
            fv_scale,
        })
    }

    /// 読み込み時の診断ログを出力
    #[cfg(feature = "diagnostics")]
    fn log_load_diagnostics(
        ft: &FeatureTransformerLayerStacks<L1>,
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
        let bucket_index = compute_layer_stacks_bucket_index(pos, side_to_move);
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
        #[cfg(feature = "nnue-threat")]
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
        #[cfg(not(feature = "nnue-threat"))]
        {
            sqr_clipped_relu_transform(us_acc, them_acc, &mut transformed.0);
        }

        // LayerStacks で評価（HandCount Dense 有効時は HC 寄与も加算）
        #[cfg(feature = "nnue-hand-count-dense")]
        let raw_score = if self.layer_stacks.has_hand_count {
            let hc = super::hand_count::extract_hand_count(pos);
            self.layer_stacks
                .evaluate_raw_with_hand_count(bucket_index, &transformed.0, Some(&hc))
        } else {
            self.layer_stacks.evaluate_raw(bucket_index, &transformed.0)
        };
        #[cfg(not(feature = "nnue-hand-count-dense"))]
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
        let bucket_index = compute_layer_stacks_bucket_index(pos, side_to_move);
        info!(
            "[NNUE Eval] bucket_mode={:?}, bucket_index={bucket_index}",
            get_layer_stack_bucket_mode()
        );

        // LayerStacks で評価（詳細ログ付き、HandCount Dense 有効時は HC 寄与も反映）
        #[cfg(feature = "nnue-hand-count-dense")]
        let (raw_score, l1_out, l1_skip) = if self.layer_stacks.has_hand_count {
            let hc = super::hand_count::extract_hand_count(pos);
            info!("[NNUE Eval] hand_count: {:?}", hc);
            self.layer_stacks.evaluate_raw_with_diagnostics_with_hand_count(
                bucket_index,
                &transformed.0,
                Some(&hc),
            )
        } else {
            self.layer_stacks.evaluate_raw_with_diagnostics(bucket_index, &transformed.0)
        };
        #[cfg(not(feature = "nnue-hand-count-dense"))]
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

#[cfg(feature = "layerstacks-1536x16x32")]
pub type NetworkLayerStacks1536x16x32 =
    NetworkLayerStacks<1536, LAYER_STACK_16X32_L1_OUT, LAYER_STACK_16X32_L2_IN, 32>;
#[cfg(feature = "layerstacks-1536x32x32")]
pub type NetworkLayerStacks1536x32x32 =
    NetworkLayerStacks<1536, LAYER_STACK_32X32_L1_OUT, LAYER_STACK_32X32_L2_IN, 64>;
#[cfg(feature = "layerstacks-768x16x32")]
pub type NetworkLayerStacks768x16x32 =
    NetworkLayerStacks<768, LAYER_STACK_16X32_L1_OUT, LAYER_STACK_16X32_L2_IN, 32>;
#[cfg(feature = "layerstacks-512x16x32")]
pub type NetworkLayerStacks512x16x32 =
    NetworkLayerStacks<512, LAYER_STACK_16X32_L1_OUT, LAYER_STACK_16X32_L2_IN, 32>;

// =============================================================================
// LayerStacksNetwork - L1 サイズ dispatch enum
// =============================================================================

/// LayerStacks ネットワークの L1 サイズ dispatch enum
///
/// Cargo feature `layerstacks-1536x16x32` / `layerstacks-1536x32x32`
/// / `layerstacks-768x16x32` / `layerstacks-512x16x32` で
/// 有効なバリアントが制御される。
///
/// **重要**: 大会ビルドでは必ず単一バリアントのみを有効化すること。複数バリアントを
/// 同時有効にすると dispatch match の overhead で約 5% NPS 退行する（実測値）。
pub enum LayerStacksNetwork {
    #[cfg(feature = "layerstacks-1536x16x32")]
    L1536x16x32(Box<NetworkLayerStacks1536x16x32>),
    #[cfg(feature = "layerstacks-1536x32x32")]
    L1536x32x32(Box<NetworkLayerStacks1536x32x32>),
    #[cfg(feature = "layerstacks-768x16x32")]
    L768x16x32(Box<NetworkLayerStacks768x16x32>),
    #[cfg(feature = "layerstacks-512x16x32")]
    L512x16x32(Box<NetworkLayerStacks512x16x32>),
}

impl LayerStacksNetwork {
    /// アーキテクチャ寸法 (L1, L2, L3) を返す
    pub fn architecture_dims(&self) -> (usize, usize, usize) {
        let spec = self.architecture_spec();
        (spec.l1, spec.l2, spec.l3)
    }

    /// L1 サイズを取得
    pub fn l1_size(&self) -> usize {
        match self {
            #[cfg(feature = "layerstacks-1536x16x32")]
            Self::L1536x16x32(_) => 1536,
            #[cfg(feature = "layerstacks-1536x32x32")]
            Self::L1536x32x32(_) => 1536,
            #[cfg(feature = "layerstacks-768x16x32")]
            Self::L768x16x32(_) => 768,
            #[cfg(feature = "layerstacks-512x16x32")]
            Self::L512x16x32(_) => 512,
            #[cfg(not(any(
                feature = "layerstacks-1536x16x32",
                feature = "layerstacks-1536x32x32",
                feature = "layerstacks-768x16x32",
                feature = "layerstacks-512x16x32"
            )))]
            _ => unreachable!("no LayerStacks variant enabled"),
        }
    }

    /// アーキテクチャ仕様を取得
    pub fn architecture_spec(&self) -> super::spec::ArchitectureSpec {
        match self {
            #[cfg(feature = "layerstacks-1536x16x32")]
            Self::L1536x16x32(_) => super::spec::ArchitectureSpec::new(
                super::spec::FeatureSet::LayerStacks,
                1536,
                16,
                32,
                super::spec::Activation::CReLU,
            ),
            #[cfg(feature = "layerstacks-1536x32x32")]
            Self::L1536x32x32(_) => super::spec::ArchitectureSpec::new(
                super::spec::FeatureSet::LayerStacks,
                1536,
                32,
                32,
                super::spec::Activation::CReLU,
            ),
            #[cfg(feature = "layerstacks-768x16x32")]
            Self::L768x16x32(_) => super::spec::ArchitectureSpec::new(
                super::spec::FeatureSet::LayerStacks,
                768,
                16,
                32,
                super::spec::Activation::CReLU,
            ),
            #[cfg(feature = "layerstacks-512x16x32")]
            Self::L512x16x32(_) => super::spec::ArchitectureSpec::new(
                super::spec::FeatureSet::LayerStacks,
                512,
                16,
                32,
                super::spec::Activation::CReLU,
            ),
            #[cfg(not(any(
                feature = "layerstacks-1536x16x32",
                feature = "layerstacks-1536x32x32",
                feature = "layerstacks-768x16x32",
                feature = "layerstacks-512x16x32"
            )))]
            _ => unreachable!("no LayerStacks variant enabled"),
        }
    }

    /// FV_SCALE を取得
    pub fn fv_scale(&self) -> i32 {
        match self {
            #[cfg(feature = "layerstacks-1536x16x32")]
            Self::L1536x16x32(net) => net.fv_scale,
            #[cfg(feature = "layerstacks-1536x32x32")]
            Self::L1536x32x32(net) => net.fv_scale,
            #[cfg(feature = "layerstacks-768x16x32")]
            Self::L768x16x32(net) => net.fv_scale,
            #[cfg(feature = "layerstacks-512x16x32")]
            Self::L512x16x32(net) => net.fv_scale,
            #[cfg(not(any(
                feature = "layerstacks-1536x16x32",
                feature = "layerstacks-1536x32x32",
                feature = "layerstacks-768x16x32",
                feature = "layerstacks-512x16x32"
            )))]
            _ => unreachable!("no LayerStacks variant enabled"),
        }
    }

    /// ファイルから読み込み（exact architecture で dispatch）
    pub fn read_with_options<R: Read + Seek>(
        reader: &mut R,
        l1: usize,
        l2: usize,
        l3: usize,
        psqt_override: Option<bool>,
    ) -> io::Result<Self> {
        match (l1, l2, l3) {
            #[cfg(feature = "layerstacks-1536x16x32")]
            (1536, 16, 32) => {
                let net = NetworkLayerStacks1536x16x32::read_with_options(reader, psqt_override)?;
                Ok(Self::L1536x16x32(Box::new(net)))
            }
            #[cfg(feature = "layerstacks-1536x32x32")]
            (1536, 32, 32) => {
                let net = NetworkLayerStacks1536x32x32::read_with_options(reader, psqt_override)?;
                Ok(Self::L1536x32x32(Box::new(net)))
            }
            #[cfg(feature = "layerstacks-768x16x32")]
            (768, 16, 32) => {
                let net = NetworkLayerStacks768x16x32::read_with_options(reader, psqt_override)?;
                Ok(Self::L768x16x32(Box::new(net)))
            }
            #[cfg(feature = "layerstacks-512x16x32")]
            (512, 16, 32) => {
                let net = NetworkLayerStacks512x16x32::read_with_options(reader, psqt_override)?;
                Ok(Self::L512x16x32(Box::new(net)))
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Unsupported LayerStacks architecture: {l1}x{l2}x{l3}"),
            )),
        }
    }

    /// 評価値を計算
    pub fn evaluate(
        &self,
        pos: &Position,
        stack: &super::accumulator_layer_stacks::LayerStacksAccStack,
    ) -> Value {
        let (net_l1, net_l2, net_l3) = self.architecture_dims();
        let (stack_l1, stack_l2, stack_l3) = stack.architecture_dims();
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
            #[cfg(feature = "layerstacks-512x16x32")]
            (
                Self::L512x16x32(net),
                super::accumulator_layer_stacks::LayerStacksAccStack::L512x16x32(st),
            ) => net.evaluate(pos, &st.current().accumulator),
            #[allow(unreachable_patterns)]
            _ => panic!(
                "LayerStacksNetwork/LayerStacksAccStack architecture mismatch: network={net_l1}x{net_l2}x{net_l3}, stack={stack_l1}x{stack_l2}x{stack_l3}"
            ),
        }
    }

    /// アキュムレータを更新（キャッシュ対応）
    pub fn update_accumulator(
        &self,
        pos: &Position,
        stack: &mut super::accumulator_layer_stacks::LayerStacksAccStack,
        cache: &mut Option<super::accumulator_layer_stacks::LayerStacksAccCache>,
    ) {
        let (net_l1, net_l2, net_l3) = self.architecture_dims();
        let (stack_l1, stack_l2, stack_l3) = stack.architecture_dims();
        macro_rules! do_update {
            ($net:expr, $stack:expr, $cache_variant:ident) => {{
                let current_entry = $stack.current();
                if current_entry.accumulator.computed_accumulation {
                    return;
                }

                let mut updated = false;

                // --- Tier 1: 直前局面 (depth=1) で差分更新 ---
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

                // --- Tier 2: 祖先探索 + forward_update_incremental ---
                // Tier 1 で失敗しても、MAX_DEPTH=4 以内の computed 祖先があれば
                // そこから forward 方向に dirty_piece を適用して更新できる。
                // HalfKA_hm / HalfKA / HalfKP では既に有効だが LayerStacks では
                // 未使用だった。
                if !updated {
                    if let Some((source_idx, _depth)) = $stack.find_usable_accumulator() {
                        updated = $net.forward_update_incremental(pos, $stack, source_idx);
                    }
                }

                // --- Tier 3: 全計算 (cache 経由) ---
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
            #[cfg(feature = "layerstacks-512x16x32")]
            (
                Self::L512x16x32(net),
                super::accumulator_layer_stacks::LayerStacksAccStack::L512x16x32(st),
            ) => {
                do_update!(net, st, L512x16x32);
            }
            #[allow(unreachable_patterns)]
            _ => panic!(
                "LayerStacksNetwork/LayerStacksAccStack architecture mismatch: network={net_l1}x{net_l2}x{net_l3}, stack={stack_l1}x{stack_l2}x{stack_l3}"
            ),
        }
    }

    /// 新しい L1 サイズに対応する AccStack を作成
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
            #[cfg(feature = "layerstacks-512x16x32")]
            Self::L512x16x32(_) => {
                super::accumulator_layer_stacks::LayerStacksAccStack::L512x16x32(
                    super::accumulator_layer_stacks::AccumulatorStackLayerStacks::<512>::new(),
                )
            }
            #[cfg(not(any(
                feature = "layerstacks-1536x16x32",
                feature = "layerstacks-1536x32x32",
                feature = "layerstacks-768x16x32",
                feature = "layerstacks-512x16x32"
            )))]
            _ => unreachable!("no LayerStacks variant enabled"),
        }
    }

    /// 新しい L1 サイズに対応する AccCache を作成
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
            #[cfg(feature = "layerstacks-512x16x32")]
            Self::L512x16x32(_) => {
                super::accumulator_layer_stacks::LayerStacksAccCache::L512x16x32(
                    super::accumulator_layer_stacks::AccumulatorCacheLayerStacks::<512>::new(),
                )
            }
            #[cfg(not(any(
                feature = "layerstacks-1536x16x32",
                feature = "layerstacks-1536x32x32",
                feature = "layerstacks-768x16x32",
                feature = "layerstacks-512x16x32"
            )))]
            _ => unreachable!("no LayerStacks variant enabled"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nnue::constants::{FV_SCALE_HALFKA, NNUE_PYTORCH_L1};
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
        use crate::nnue::features::{FeatureSet, HalfKA_hm_FeatureSet};
        use crate::types::Color;
        let active_black = HalfKA_hm_FeatureSet::collect_active_indices(&pos, Color::Black);
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
}
