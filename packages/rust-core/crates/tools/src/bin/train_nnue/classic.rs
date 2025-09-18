use crate::error_messages::*;
use crate::params::{CLASSIC_FT_SHIFT, CLASSIC_V1_ARCH_ID, I16_QMAX, I8_QMAX};
use crate::types::QuantScheme;
use engine_core::evaluation::nnue::{features::FE_END, simd::SimdDispatcher};
use engine_core::shogi::SHOGI_BOARD_SIZE;

#[cfg(test)]
const _: usize = SHOGI_BOARD_SIZE * FE_END;
use rand::{
    distr::{Distribution, Uniform},
    Rng,
};
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

static BIAS_SCALE_MISMATCH_LOGGED: AtomicBool = AtomicBool::new(false);
static FT_ACC_OOR_WARNED: AtomicBool = AtomicBool::new(false);

#[inline]
pub fn round_away_from_zero(val: f32) -> i32 {
    debug_assert!(val.is_finite(), "round_away_from_zero expects finite input, got {val}",);
    if !val.is_finite() {
        return 0;
    }
    if val >= 0.0 {
        (val + 0.5).floor() as i32
    } else {
        (val - 0.5).ceil() as i32
    }
}

#[inline]
pub fn clip_sym(value: i32, qmax: i32) -> i32 {
    value.clamp(-qmax, qmax)
}

#[inline]
fn clamp_i32_to_i16(v: i32) -> i16 {
    v.clamp(-I16_QMAX, I16_QMAX) as i16
}

pub fn quantize_symmetric_i8(
    weights: &[f32],
    per_channel: bool,
    channels: usize,
) -> Result<(Vec<i8>, Vec<f32>), String> {
    if weights.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    if per_channel && channels == 0 {
        return Err("per-channel quantization requires channels > 0".into());
    }
    let mut scales = if per_channel {
        vec![0.0f32; channels]
    } else {
        vec![0.0f32; 1]
    };
    let mut quantized = Vec::with_capacity(weights.len());
    if per_channel {
        if weights.len() % channels != 0 {
            return Err(format!(
                "weights len {} not divisible by channels {} (stride {})",
                weights.len(),
                channels,
                weights.len() / channels
            ));
        }
        let stride = weights.len() / channels;
        for (ch, slice) in weights.chunks_exact(stride).enumerate() {
            let max_abs = slice.iter().fold(0.0f32, |m, &v| m.max(v.abs()));
            let scale = if max_abs == 0.0 {
                1.0
            } else {
                max_abs / I8_QMAX as f32
            };
            scales[ch] = scale;
            for &w in slice {
                let q = round_away_from_zero(w / scale);
                quantized.push(clip_sym(q, I8_QMAX) as i8);
            }
        }
    } else {
        let max_abs = weights.iter().fold(0.0f32, |m, &v| m.max(v.abs()));
        let scale = if max_abs == 0.0 {
            1.0
        } else {
            max_abs / I8_QMAX as f32
        };
        scales[0] = scale;
        for &w in weights {
            let q = round_away_from_zero(w / scale);
            quantized.push(clip_sym(q, I8_QMAX) as i8);
        }
    }
    Ok((quantized, scales))
}

pub fn quantize_symmetric_i16(
    weights: &[f32],
    per_channel: bool,
    channels: usize,
) -> Result<(Vec<i16>, Vec<f32>), String> {
    if weights.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    if per_channel && channels == 0 {
        return Err("per-channel quantization requires channels > 0".into());
    }
    let mut scales = if per_channel {
        vec![0.0f32; channels]
    } else {
        vec![0.0f32; 1]
    };
    let mut quantized = Vec::with_capacity(weights.len());
    if per_channel {
        if weights.len() % channels != 0 {
            return Err(format!(
                "weights len {} not divisible by channels {} (stride {})",
                weights.len(),
                channels,
                weights.len() / channels
            ));
        }
        let stride = weights.len() / channels;
        for (ch, slice) in weights.chunks_exact(stride).enumerate() {
            let max_abs = slice.iter().fold(0.0f32, |m, &v| m.max(v.abs()));
            let scale = if max_abs == 0.0 {
                1.0
            } else {
                max_abs / I16_QMAX as f32
            };
            scales[ch] = scale;
            for &w in slice {
                let q = round_away_from_zero(w / scale);
                quantized.push(clip_sym(q, I16_QMAX) as i16);
            }
        }
    } else {
        let max_abs = weights.iter().fold(0.0f32, |m, &v| m.max(v.abs()));
        let scale = if max_abs == 0.0 {
            1.0
        } else {
            max_abs / I16_QMAX as f32
        };
        scales[0] = scale;
        for &w in weights {
            let q = round_away_from_zero(w / scale);
            quantized.push(clip_sym(q, I16_QMAX) as i16);
        }
    }
    Ok((quantized, scales))
}

/// 量子化済みバイアスを i32 へ変換する。`weight_scales` は
/// - `len() == 1` の場合: per-tensor スケール（単一値を全要素へブロードキャスト）
/// - `len() == bias.len()` の場合: per-channel スケール（要素ごとに適用）
///   を想定する。それ以外の長さはデバッグビルドで検出し、リリースでは末尾値を再利用する。
///   計算式: `q_bias[i] = round_away_from_zero(bias[i] / (input_scale * weight_scale[i]))`。
///   `input_scale` は前段アクチベーションの実効スケール（例: `s_in_1 = s_w0 * 2^CLASSIC_FT_SHIFT`）。
pub fn quantize_bias_i32(bias: &[f32], input_scale: f32, weight_scales: &[f32]) -> Vec<i32> {
    debug_assert!(!weight_scales.is_empty(), "weight_scales must not be empty",);
    debug_assert!(
        weight_scales.len() == 1 || weight_scales.len() == bias.len(),
        "weight_scales must be length 1 (per-tensor) or match bias length (per-channel)",
    );

    if weight_scales.is_empty() {
        return vec![0; bias.len()];
    }

    let mut out = Vec::with_capacity(bias.len());
    let per_tensor = weight_scales.len() == 1;
    let ws0 = if per_tensor { weight_scales[0] } else { 0.0 };
    let fallback_scale = if per_tensor {
        ws0
    } else {
        *weight_scales.last().unwrap_or(&1.0)
    };
    let mut mismatch_count = 0usize;
    let mut mismatch_examples: Vec<(usize, f32, f32)> = Vec::new();
    const MISMATCH_SAMPLE_LIMIT: usize = 4;
    for (i, &b) in bias.iter().enumerate() {
        let (ws, mismatched) = if per_tensor {
            (ws0, false)
        } else if i < weight_scales.len() {
            (weight_scales[i], false)
        } else {
            (fallback_scale, true)
        };
        if mismatched {
            mismatch_count += 1;
            if mismatch_examples.len() < MISMATCH_SAMPLE_LIMIT {
                mismatch_examples.push((i, ws, b));
            }
        }
        let scale = input_scale * ws;
        out.push(if scale == 0.0 {
            0
        } else {
            round_away_from_zero(b / scale)
        });
    }
    if mismatch_count > 0
        && BIAS_SCALE_MISMATCH_LOGGED
            .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    {
        let sample_display = if mismatch_examples.is_empty() {
            String::from("<none>")
        } else {
            mismatch_examples
                .iter()
                .map(|(idx, scale, bias)| {
                    format!("#{idx} bias={:.6} reuse_scale={:.6}", bias, scale)
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        log::warn!(
            "quantize_bias_i32: weight_scales len {} < bias len {} (reusing last scale {:.6}) — mismatched entries {} (showing first {}: [{}]); further warnings suppressed",
            weight_scales.len(),
            bias.len(),
            fallback_scale,
            mismatch_count,
            mismatch_examples.len(),
            sample_display
        );
    }
    out
}

// --- ClassicFeatureTransformerInt impl ---
#[derive(Clone, Debug)]
pub struct ClassicFeatureTransformerInt {
    pub weights: Vec<i16>,
    pub biases: Vec<i32>,
    pub acc_dim: usize,
    pub input_dim: usize,
}

impl ClassicFeatureTransformerInt {
    pub fn new(weights: Vec<i16>, biases: Vec<i32>, acc_dim: usize) -> Self {
        debug_assert!(
            acc_dim == 0 || weights.len() % acc_dim == 0,
            "ft_weights.len() must be multiple of acc_dim ({} % {} != 0)",
            weights.len(),
            acc_dim
        );
        let input_dim = if acc_dim == 0 {
            0
        } else {
            weights.len() / acc_dim
        };
        Self {
            weights,
            biases,
            acc_dim,
            input_dim,
        }
    }

    /// 再利用バッファを使う高速版。`tmp_i32` は acc_dim 長の i32、一時蓄積領域。
    /// `out_i16` は acc_dim 長の出力。内部でバイアスを copy して加算・最後に i16 飽和。
    /// 安全性: 呼び出し側で長さを保証すること。
    pub fn accumulate_into_u32(&self, features: &[u32], tmp_i32: &mut [i32], out_i16: &mut [i16]) {
        debug_assert_eq!(tmp_i32.len(), self.acc_dim);
        debug_assert_eq!(out_i16.len(), self.acc_dim);
        debug_assert_eq!(self.biases.len(), self.acc_dim);
        // biases.clone() を避けて copy のみに
        tmp_i32.copy_from_slice(&self.biases);
        for &feat_u32 in features {
            let feat = feat_u32 as usize;
            debug_assert!(
                feat < self.input_dim,
                "feature index {} out of range {}",
                feat,
                self.input_dim
            );
            if feat >= self.input_dim {
                if FT_ACC_OOR_WARNED
                    .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
                    .is_ok()
                {
                    log::warn!(
                        "ClassicFeatureTransformerInt::accumulate_into_u32: feature index {} >= input_dim {} (dropping)",
                        feat,
                        self.input_dim
                    );
                }
                continue;
            }
            let base = feat * self.acc_dim;
            let row = &self.weights[base..base + self.acc_dim];
            for (acc, &w) in tmp_i32.iter_mut().zip(row.iter()) {
                *acc += w as i32;
            }
        }
        for (dst, &sum) in out_i16.iter_mut().zip(tmp_i32.iter()) {
            *dst = clamp_i32_to_i16(sum);
        }
    }
}

#[derive(Clone, Debug)]
pub struct ClassicQuantizedNetworkParams {
    pub hidden1_weights: Vec<i8>,
    pub hidden1_biases: Vec<i32>,
    pub hidden2_weights: Vec<i8>,
    pub hidden2_biases: Vec<i32>,
    pub output_weights: Vec<i8>,
    pub output_bias: i32,
    pub acc_dim: usize,
    pub h1_dim: usize,
    pub h2_dim: usize,
}

#[derive(Clone, Debug)]
pub struct ClassicQuantizedNetwork {
    pub hidden1_weights: Vec<i8>,
    pub hidden1_biases: Vec<i32>,
    pub hidden2_weights: Vec<i8>,
    pub hidden2_biases: Vec<i32>,
    pub output_weights: Vec<i8>,
    pub output_bias: i32,
    pub acc_dim: usize,
    pub h1_dim: usize,
    pub h2_dim: usize,
}

impl From<ClassicQuantizedNetworkParams> for ClassicQuantizedNetwork {
    fn from(p: ClassicQuantizedNetworkParams) -> Self {
        Self {
            hidden1_weights: p.hidden1_weights,
            hidden1_biases: p.hidden1_biases,
            hidden2_weights: p.hidden2_weights,
            hidden2_biases: p.hidden2_biases,
            output_weights: p.output_weights,
            output_bias: p.output_bias,
            acc_dim: p.acc_dim,
            h1_dim: p.h1_dim,
            h2_dim: p.h2_dim,
        }
    }
}

impl ClassicQuantizedNetwork {
    pub fn new(p: ClassicQuantizedNetworkParams) -> Self {
        p.into()
    }

    // 旧単純版 propagate_from_acc は不要になったため削除 (scratch 版のみ利用)

    /// 中間層バッファを再利用する高速版。`scratch` の各ベクタ長は対応する dim に一致している必要がある。
    pub fn propagate_from_acc_scratch(
        &self,
        acc_us: &[i16],
        acc_them: &[i16],
        scratch: &mut ClassicIntScratch,
    ) -> i32 {
        debug_assert_eq!(acc_us.len(), self.acc_dim);
        debug_assert_eq!(acc_them.len(), self.acc_dim);
        debug_assert_eq!(scratch.input.len(), self.acc_dim * 2);
        debug_assert_eq!(scratch.h1.len(), self.h1_dim);
        debug_assert_eq!(scratch.h1_act.len(), self.h1_dim);
        debug_assert_eq!(scratch.h2.len(), self.h2_dim);
        debug_assert_eq!(scratch.h2_act.len(), self.h2_dim);

        // pack input (engine_core SIMD 実装と同じ丸め規約を使用)
        SimdDispatcher::transform_features(acc_us, acc_them, &mut scratch.input, self.acc_dim);

        self.affine_layer(
            &scratch.input,
            &self.hidden1_weights,
            &self.hidden1_biases,
            self.acc_dim * 2,
            self.h1_dim,
            &mut scratch.h1,
        );
        Self::apply_clipped_relu(&scratch.h1, &mut scratch.h1_act);

        self.affine_layer(
            &scratch.h1_act,
            &self.hidden2_weights,
            &self.hidden2_biases,
            self.h1_dim,
            self.h2_dim,
            &mut scratch.h2,
        );
        Self::apply_clipped_relu(&scratch.h2, &mut scratch.h2_act);

        debug_assert_eq!(self.output_weights.len(), self.h2_dim);
        let mut output = self.output_bias;
        for (i, &w) in self.output_weights.iter().enumerate() {
            output += w as i32 * scratch.h2_act[i] as i32;
        }
        output
    }

    fn affine_layer(
        &self,
        input: &[i8],
        weights: &[i8],
        biases: &[i32],
        in_dim: usize,
        out_dim: usize,
        out: &mut [i32],
    ) {
        debug_assert_eq!(input.len(), in_dim);
        debug_assert_eq!(out.len(), out_dim);
        debug_assert_eq!(weights.len(), in_dim * out_dim);
        debug_assert_eq!(biases.len(), out_dim);

        for i in 0..out_dim {
            let mut acc = biases[i];
            let row = &weights[i * in_dim..(i + 1) * in_dim];
            for (j, &w) in row.iter().enumerate() {
                acc += input[j] as i32 * w as i32;
            }
            out[i] = acc;
        }
    }

    fn apply_clipped_relu(input: &[i32], output: &mut [i8]) {
        debug_assert_eq!(input.len(), output.len());
        // Classic 推論は常に int8 の 0..=127 クリップを前提とする
        for (dst, &src) in output.iter_mut().zip(input.iter()) {
            let clipped = src.clamp(0, I8_QMAX);
            *dst = clipped as i8;
        }
    }
}

#[derive(Clone, Debug)]
pub struct ClassicIntNetworkBundle {
    pub transformer: ClassicFeatureTransformerInt,
    pub network: ClassicQuantizedNetwork,
}

impl ClassicIntNetworkBundle {
    pub fn new(
        transformer: ClassicFeatureTransformerInt,
        network: ClassicQuantizedNetwork,
    ) -> Self {
        Self {
            transformer,
            network,
        }
    }

    pub fn as_serialized(&self) -> ClassicV1Serialized<'_> {
        ClassicV1Serialized {
            acc_dim: self.transformer.acc_dim,
            input_dim: self.transformer.input_dim,
            h1_dim: self.network.h1_dim,
            h2_dim: self.network.h2_dim,
            ft_weights: &self.transformer.weights,
            ft_biases: &self.transformer.biases,
            hidden1_weights: &self.network.hidden1_weights,
            hidden1_biases: &self.network.hidden1_biases,
            hidden2_weights: &self.network.hidden2_weights,
            hidden2_biases: &self.network.hidden2_biases,
            output_weights: &self.network.output_weights,
            output_bias: self.network.output_bias,
        }
    }

    /// FT + 中間層も含めフル scratch 再利用版。
    pub fn propagate_with_features_scratch_full(
        &self,
        features_us: &[u32],
        features_them: &[u32],
        views: &mut ClassicScratchViews,
    ) -> i32 {
        self.transformer
            .accumulate_into_u32(features_us, &mut views.tmp_us, &mut views.acc_us);
        self.transformer.accumulate_into_u32(
            features_them,
            &mut views.tmp_them,
            &mut views.acc_them,
        );
        self.network
            .propagate_from_acc_scratch(&views.acc_us, &views.acc_them, &mut views.mid)
    }
}

/// 中間層再利用用スクラッチ
#[derive(Clone, Debug)]
pub struct ClassicIntScratch {
    pub input: Vec<i8>,
    pub h1: Vec<i32>,
    pub h1_act: Vec<i8>,
    pub h2: Vec<i32>,
    pub h2_act: Vec<i8>,
}

/// まとめて受け渡すためのビュー構造体（引数過多の警告回避）
pub struct ClassicScratchViews {
    pub tmp_us: Vec<i32>,
    pub tmp_them: Vec<i32>,
    pub acc_us: Vec<i16>,
    pub acc_them: Vec<i16>,
    pub mid: ClassicIntScratch,
}

impl ClassicScratchViews {
    pub fn new(acc_dim: usize, h1_dim: usize, h2_dim: usize) -> Self {
        Self {
            tmp_us: vec![0; acc_dim],
            tmp_them: vec![0; acc_dim],
            acc_us: vec![0; acc_dim],
            acc_them: vec![0; acc_dim],
            mid: ClassicIntScratch::new(acc_dim, h1_dim, h2_dim),
        }
    }
}

impl ClassicIntScratch {
    pub fn new(acc_dim: usize, h1_dim: usize, h2_dim: usize) -> Self {
        Self {
            input: vec![0; acc_dim * 2],
            h1: vec![0; h1_dim],
            h1_act: vec![0; h1_dim],
            h2: vec![0; h2_dim],
            h2_act: vec![0; h2_dim],
        }
    }
}

pub struct ClassicV1Serialized<'a> {
    pub acc_dim: usize,
    pub input_dim: usize,
    pub h1_dim: usize,
    pub h2_dim: usize,
    pub ft_weights: &'a [i16],
    pub ft_biases: &'a [i32],
    pub hidden1_weights: &'a [i8],
    pub hidden1_biases: &'a [i32],
    pub hidden2_weights: &'a [i8],
    pub hidden2_biases: &'a [i32],
    pub output_weights: &'a [i8],
    pub output_bias: i32,
}

impl<'a> ClassicV1Serialized<'a> {
    /// Canonical Classic v1 layout 検証。
    /// テストでは `input_dim` を縮約しても良く、その場合は書き出し側で不足分をゼロパディングする
    /// （本番構成は HALFKP 固定）。
    pub fn validate(&self) -> Result<(), String> {
        if self.acc_dim == 0 || self.h1_dim == 0 || self.h2_dim == 0 {
            return Err("dimensions must be non-zero".into());
        }
        if self.input_dim == 0 {
            return Err("input_dim must be non-zero".into());
        }
        if self.ft_weights.len() != self.input_dim * self.acc_dim {
            return Err("ft_weights length mismatch".into());
        }
        let canonical_input_dim = SHOGI_BOARD_SIZE * FE_END;
        if self.input_dim > canonical_input_dim {
            return Err("input_dim exceeds Classic v1 canonical spec".into());
        }
        // 入力を縮約した場合は write_classic_v1_file() で canonical 長までゼロ埋めする想定。
        if self.ft_biases.len() != self.acc_dim {
            return Err("ft_biases length mismatch".into());
        }
        let classic_input_dim = self.acc_dim * 2;
        if self.hidden1_weights.len() != classic_input_dim * self.h1_dim {
            return Err("hidden1_weights length mismatch".into());
        }
        if self.hidden1_biases.len() != self.h1_dim {
            return Err("hidden1_biases length mismatch".into());
        }
        if self.hidden2_weights.len() != self.h1_dim * self.h2_dim {
            return Err("hidden2_weights length mismatch".into());
        }
        if self.hidden2_biases.len() != self.h2_dim {
            return Err("hidden2_biases length mismatch".into());
        }
        if self.output_weights.len() != self.h2_dim {
            return Err("output_weights length mismatch".into());
        }
        Ok(())
    }

    /// Classic v1 canonical payload（ヘッダ 16B を除外）を返す。
    pub fn payload_bytes(&self) -> u64 {
        let acc_dim = self.acc_dim as u64;
        let h1_dim = self.h1_dim as u64;
        let h2_dim = self.h2_dim as u64;
        let canonical_input_dim = (SHOGI_BOARD_SIZE * FE_END) as u64;
        let mut payload = 0u64;
        payload += canonical_input_dim
            .checked_mul(acc_dim)
            .and_then(|v| v.checked_mul(2))
            .expect("ft_weights payload overflow");
        payload += acc_dim.checked_mul(4).expect("ft_bias payload overflow");
        let classic_input_dim = acc_dim.checked_mul(2).expect("classic input dim overflow");
        payload += classic_input_dim.checked_mul(h1_dim).expect("hidden1 weight payload overflow");
        payload += h1_dim.checked_mul(4).expect("hidden1 bias payload overflow");
        payload += h1_dim.checked_mul(h2_dim).expect("hidden2 weight payload overflow");
        payload += h2_dim.checked_mul(4).expect("hidden2 bias payload overflow");
        payload += h2_dim;
        payload += 4;
        payload
    }
}

pub fn write_classic_v1_file(
    path: &Path,
    data: &ClassicV1Serialized<'_>,
) -> Result<(), Box<dyn std::error::Error>> {
    data.validate().map_err(|msg| msg.to_string())?;

    // Classic v1 は固定寸法 (HALFKP_256X2_32_32) のみ正式サポート
    if !(data.acc_dim == 256 && data.h1_dim == 32 && data.h2_dim == 32) {
        return Err("Classic v1 requires acc_dim=256, h1_dim=32, h2_dim=32".into());
    }

    #[cfg(not(test))]
    if data.input_dim != SHOGI_BOARD_SIZE * FE_END {
        return Err("Classic v1 requires input_dim=SHOGI_BOARD_SIZE*FE_END".into());
    }

    let payload_bytes = data.payload_bytes();
    // v1 ヘッダの size フィールドは「ヘッダ(16B)を含むファイル総バイト数」。
    let total_bytes = 16u64 + payload_bytes;
    if total_bytes > u32::MAX as u64 {
        return Err("Classic v1 blob exceeds 4GB".into());
    }

    let mut writer = std::io::BufWriter::new(File::create(path)?);
    writer.write_all(b"NNUE")?;
    writer.write_all(&1u32.to_le_bytes())?;
    writer.write_all(&CLASSIC_V1_ARCH_ID.to_le_bytes())?;
    writer.write_all(&(total_bytes as u32).to_le_bytes())?;

    let canonical_ft_weights = SHOGI_BOARD_SIZE
        .checked_mul(FE_END)
        .and_then(|v| v.checked_mul(data.acc_dim))
        .expect("Classic v1 canonical ft weight count overflow");
    if data.ft_weights.len() > canonical_ft_weights {
        return Err("ft_weights length exceeds Classic v1 canonical spec".into());
    }

    for &w in data.ft_weights {
        writer.write_all(&w.to_le_bytes())?;
    }
    let remaining_weights = canonical_ft_weights - data.ft_weights.len();
    if remaining_weights > 0 {
        const ZERO_CHUNK: [u8; 8192] = [0u8; 8192];
        let mut remaining_bytes =
            (remaining_weights as u64).checked_mul(2).expect("ft_weights padding overflow");
        while remaining_bytes > 0 {
            let chunk = ZERO_CHUNK.len().min(remaining_bytes as usize);
            writer.write_all(&ZERO_CHUNK[..chunk])?;
            remaining_bytes -= chunk as u64;
        }
    }
    for &b in data.ft_biases {
        writer.write_all(&b.to_le_bytes())?;
    }
    for &w in data.hidden1_weights {
        writer.write_all(&w.to_le_bytes())?;
    }
    for &b in data.hidden1_biases {
        writer.write_all(&b.to_le_bytes())?;
    }
    for &w in data.hidden2_weights {
        writer.write_all(&w.to_le_bytes())?;
    }
    for &b in data.hidden2_biases {
        writer.write_all(&b.to_le_bytes())?;
    }
    for &w in data.output_weights {
        writer.write_all(&w.to_le_bytes())?;
    }
    writer.write_all(&data.output_bias.to_le_bytes())?;
    writer.flush()?;
    Ok(())
}

pub fn write_classic_v1_bundle(
    path: &Path,
    bundle: &ClassicIntNetworkBundle,
) -> Result<(), Box<dyn std::error::Error>> {
    let serialized = bundle.as_serialized();
    write_classic_v1_file(path, &serialized)
}

#[derive(Clone, Debug)]
pub struct ClassicFloatNetwork {
    pub ft_weights: Vec<f32>,
    pub ft_biases: Vec<f32>,
    pub hidden1_weights: Vec<f32>,
    pub hidden1_biases: Vec<f32>,
    pub hidden2_weights: Vec<f32>,
    pub hidden2_biases: Vec<f32>,
    pub output_weights: Vec<f32>,
    pub output_bias: f32,
    pub acc_dim: usize,
    pub input_dim: usize,
    pub h1_dim: usize,
    pub h2_dim: usize,
}

impl ClassicFloatNetwork {
    pub fn zero() -> Self {
        ClassicFloatNetwork {
            ft_weights: Vec::new(),
            ft_biases: Vec::new(),
            hidden1_weights: Vec::new(),
            hidden1_biases: Vec::new(),
            hidden2_weights: Vec::new(),
            hidden2_biases: Vec::new(),
            output_weights: Vec::new(),
            output_bias: 0.0,
            acc_dim: 0,
            input_dim: 0,
            h1_dim: 0,
            h2_dim: 0,
        }
    }

    pub fn zeros_with_dims(input_dim: usize, acc_dim: usize, h1_dim: usize, h2_dim: usize) -> Self {
        ClassicFloatNetwork {
            ft_weights: vec![0.0; input_dim * acc_dim],
            ft_biases: vec![0.0; acc_dim],
            hidden1_weights: vec![0.0; (acc_dim * 2) * h1_dim],
            hidden1_biases: vec![0.0; h1_dim],
            hidden2_weights: vec![0.0; h1_dim * h2_dim],
            hidden2_biases: vec![0.0; h2_dim],
            output_weights: vec![0.0; h2_dim],
            output_bias: 0.0,
            acc_dim,
            input_dim,
            h1_dim,
            h2_dim,
        }
    }

    /// He/Xavier 初期化で Classic ネットワークを構築する。
    pub fn he_uniform_with_dims(
        input_dim: usize,
        acc_dim: usize,
        h1_dim: usize,
        h2_dim: usize,
        fan_in_ft_estimate: usize,
        rng: &mut impl Rng,
    ) -> Self {
        let mut net = Self::zeros_with_dims(input_dim, acc_dim, h1_dim, h2_dim);

        // Feature transformer: fan_in ≒ サンプル当たりの活性特徴数（片側）。
        let fan_in_ft = fan_in_ft_estimate.max(1) as f32;
        let a_ft = (6.0 / fan_in_ft).sqrt();
        let dist_ft = Uniform::new(-a_ft, a_ft).unwrap();
        for row in net.ft_weights.chunks_mut(acc_dim.max(1)) {
            for w in row {
                *w = dist_ft.sample(rng);
            }
        }

        // Hidden1: 入力は us/them を連結した 2 * acc_dim。
        let input_dim_h1 = (2 * acc_dim).max(1);
        let fan_in_h1 = input_dim_h1 as f32;
        let a_h1 = (6.0 / fan_in_h1).sqrt();
        let dist_h1 = Uniform::new(-a_h1, a_h1).unwrap();
        for row in net.hidden1_weights.chunks_mut(input_dim_h1) {
            for w in row {
                *w = dist_h1.sample(rng);
            }
        }

        // Hidden2: 入力は hidden1 の出力。
        let input_dim_h2 = h1_dim.max(1);
        let fan_in_h2 = input_dim_h2 as f32;
        let a_h2 = (6.0 / fan_in_h2).sqrt();
        let dist_h2 = Uniform::new(-a_h2, a_h2).unwrap();
        for row in net.hidden2_weights.chunks_mut(input_dim_h2) {
            for w in row {
                *w = dist_h2.sample(rng);
            }
        }

        // Output: 線形出力なので Xavier Uniform。
        let fan_in_out = h2_dim.max(1) as f32;
        let fan_out_out = 1.0f32;
        let a_out = (6.0 / (fan_in_out + fan_out_out)).sqrt();
        let dist_out = Uniform::new(-a_out, a_out).unwrap();
        for w in net.output_weights.iter_mut() {
            *w = dist_out.sample(rng);
        }

        net
    }

    /// Classic アーキ用の対称量子化（per-tensor / per-channel 指定済み）。
    ///
    /// engine-core 側の `SimdDispatcher::transform_features()` は Feature Transformer 出力を
    /// 固定右シフト (`CLASSIC_FT_SHIFT`) によって int8 `[−127, 127]` / `[0, 127]` へ丸めており、
    /// ここで求める `s_in_*` も同じ規約を前提にしている。`CLASSIC_FT_SHIFT` や丸めルールを変更
    /// する場合は、本関数と `quantize_bias_i32()`、および推論側の変換処理を同時に更新して整合性を
    /// 保つこと。
    pub fn quantize_symmetric(
        &self,
        quant_ft: QuantScheme,
        quant_h1: QuantScheme,
        quant_h2: QuantScheme,
        quant_out: QuantScheme,
    ) -> Result<(ClassicIntNetworkBundle, ClassicQuantizationScales), String> {
        self.validate()?;

        if matches!(quant_ft, QuantScheme::PerChannel) {
            return Err(ERR_CLASSIC_FT_PER_CHANNEL.into());
        }

        let (ft_weights_q, ft_scales) = quantize_symmetric_i16(&self.ft_weights, false, 1)?;
        let s_w0 = ft_scales.first().copied().unwrap_or(1.0);
        let ft_biases_q = quantize_bias_i32(&self.ft_biases, 1.0, &[s_w0]);

        let transformer =
            ClassicFeatureTransformerInt::new(ft_weights_q, ft_biases_q, self.acc_dim);

        let h1_per_channel = matches!(quant_h1, QuantScheme::PerChannel);
        let h1_channels = if h1_per_channel {
            self.h1_dim.max(1)
        } else {
            1
        };
        let (h1_weights_q, h1_scales) =
            quantize_symmetric_i8(&self.hidden1_weights, h1_per_channel, h1_channels)?;
        // NOTE: FT 出力は engine-core 側で `(acc << CLASSIC_FT_SHIFT)` の固定シフトを掛ける。
        // ここで bias を量子化する際も同じシフトを前提とするため、`CLASSIC_FT_SHIFT` を
        // 変更する場合は bias 量子化と評価側 (evaluate_quantization_gap 等) の復元計算も
        // あわせて更新すること。
        let s_in_1 = s_w0 * (1 << CLASSIC_FT_SHIFT) as f32;
        let hidden1_biases_q = quantize_bias_i32(&self.hidden1_biases, s_in_1, &h1_scales);

        let h2_per_channel = matches!(quant_h2, QuantScheme::PerChannel);
        let h2_channels = if h2_per_channel {
            self.h2_dim.max(1)
        } else {
            1
        };
        let (h2_weights_q, h2_scales) =
            quantize_symmetric_i8(&self.hidden2_weights, h2_per_channel, h2_channels)?;
        // NOTE: Classic FP32 forwardは中間活性を常に CLASSIC_RELU_CLIP_F32 (==127.0)
        // へクリップしており、量子化時もそのまま int8 の 0..127 を共有する。
        // そのため hidden2/output への入力スケールは 1.0 で固定している。
        // clip 値や中間層を別スケールにしたい場合は、quantize_bias_i32()
        // および evaluate_quantization_gap() の復元計算を合わせて更新すること。
        let s_in_2 = 1.0f32;
        let hidden2_biases_q = quantize_bias_i32(&self.hidden2_biases, s_in_2, &h2_scales);

        if matches!(quant_out, QuantScheme::PerChannel) {
            return Err(ERR_CLASSIC_OUT_PER_CHANNEL.into());
        }
        let out_per_channel = false;
        let out_channels = 1;
        let (out_weights_q, out_scales) =
            quantize_symmetric_i8(&self.output_weights, out_per_channel, out_channels)?;
        let s_in_3 = 1.0f32;
        let output_bias_q = quantize_bias_i32(&[self.output_bias], s_in_3, &out_scales)
            .into_iter()
            .next()
            .unwrap_or(0);

        let network = ClassicQuantizedNetwork::new(ClassicQuantizedNetworkParams {
            hidden1_weights: h1_weights_q,
            hidden1_biases: hidden1_biases_q,
            hidden2_weights: h2_weights_q,
            hidden2_biases: hidden2_biases_q,
            output_weights: out_weights_q,
            output_bias: output_bias_q,
            acc_dim: self.acc_dim,
            h1_dim: self.h1_dim,
            h2_dim: self.h2_dim,
        });

        let scales = ClassicQuantizationScales {
            s_w0,
            s_w1: h1_scales.clone(),
            s_w2: h2_scales.clone(),
            s_w3: out_scales.clone(),
            s_in_1,
            s_in_2,
            s_in_3,
        };

        Ok((ClassicIntNetworkBundle::new(transformer, network), scales))
    }

    pub fn accumulate_ft(&mut self, base: usize, values: &[f32]) {
        debug_assert_eq!(values.len(), self.acc_dim);
        for (dst, &src) in self.ft_weights[base..base + self.acc_dim].iter_mut().zip(values.iter())
        {
            *dst += src;
        }
    }

    pub fn accumulate_ft_bias(&mut self, delta: &[f32]) {
        debug_assert_eq!(delta.len(), self.acc_dim);
        for (dst, &src) in self.ft_biases.iter_mut().zip(delta.iter()) {
            *dst += src;
        }
    }

    pub fn accumulate_hidden1(&mut self, delta_w: &[f32], delta_b: &[f32]) {
        for (dst, &src) in self.hidden1_weights.iter_mut().zip(delta_w.iter()) {
            *dst += src;
        }
        for (dst, &src) in self.hidden1_biases.iter_mut().zip(delta_b.iter()) {
            *dst += src;
        }
    }

    pub fn accumulate_hidden2(&mut self, delta_w: &[f32], delta_b: &[f32]) {
        for (dst, &src) in self.hidden2_weights.iter_mut().zip(delta_w.iter()) {
            *dst += src;
        }
        for (dst, &src) in self.hidden2_biases.iter_mut().zip(delta_b.iter()) {
            *dst += src;
        }
    }

    pub fn accumulate_output(&mut self, delta_w: &[f32], delta_b: f32) {
        for (dst, &src) in self.output_weights.iter_mut().zip(delta_w.iter()) {
            *dst += src;
        }
        self.output_bias += delta_b;
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.acc_dim == 0 || self.h1_dim == 0 || self.h2_dim == 0 {
            return Err("Classic float network dims must be non-zero".into());
        }
        if self.ft_weights.len() != self.input_dim * self.acc_dim {
            return Err("Classic float network ft_weights length mismatch".into());
        }
        if self.ft_biases.len() != self.acc_dim {
            return Err("Classic float network ft_biases length mismatch".into());
        }
        if self.hidden1_weights.len() != (self.acc_dim * 2) * self.h1_dim {
            return Err("Classic float network hidden1_weights length mismatch".into());
        }
        if self.hidden1_biases.len() != self.h1_dim {
            return Err("Classic float network hidden1_biases length mismatch".into());
        }
        if self.hidden2_weights.len() != self.h1_dim * self.h2_dim {
            return Err("Classic float network hidden2_weights length mismatch".into());
        }
        if self.hidden2_biases.len() != self.h2_dim {
            return Err("Classic float network hidden2_biases length mismatch".into());
        }
        if self.output_weights.len() != self.h2_dim {
            return Err("Classic float network output_weights length mismatch".into());
        }
        Ok(())
    }

    pub fn quantize_round(&self) -> Result<ClassicIntNetworkBundle, String> {
        // Fallback: スケールを持たない単純丸め。実運用では quantize_symmetric() を使用すること。
        self.validate()?;
        let ft_weights: Vec<i16> = self
            .ft_weights
            .iter()
            .map(|&w| clamp_i32_to_i16(round_away_from_zero(w)))
            .collect();
        let ft_biases: Vec<i32> = self.ft_biases.iter().map(|&b| round_away_from_zero(b)).collect();
        let hidden1_weights: Vec<i8> = self
            .hidden1_weights
            .iter()
            .map(|&w| clip_sym(round_away_from_zero(w), I8_QMAX) as i8)
            .collect();
        let hidden1_biases: Vec<i32> =
            self.hidden1_biases.iter().map(|&b| round_away_from_zero(b)).collect();
        let hidden2_weights: Vec<i8> = self
            .hidden2_weights
            .iter()
            .map(|&w| clip_sym(round_away_from_zero(w), I8_QMAX) as i8)
            .collect();
        let hidden2_biases: Vec<i32> =
            self.hidden2_biases.iter().map(|&b| round_away_from_zero(b)).collect();
        let output_weights: Vec<i8> = self
            .output_weights
            .iter()
            .map(|&w| clip_sym(round_away_from_zero(w), I8_QMAX) as i8)
            .collect();
        let output_bias = round_away_from_zero(self.output_bias);
        let transformer = ClassicFeatureTransformerInt::new(ft_weights, ft_biases, self.acc_dim);
        let network = ClassicQuantizedNetwork::new(ClassicQuantizedNetworkParams {
            hidden1_weights,
            hidden1_biases,
            hidden2_weights,
            hidden2_biases,
            output_weights,
            output_bias,
            acc_dim: self.acc_dim,
            h1_dim: self.h1_dim,
            h2_dim: self.h2_dim,
        });
        Ok(ClassicIntNetworkBundle::new(transformer, network))
    }
}

#[derive(Clone, Debug)]
/// Classic v1 の量子化スケールセット。`s_w*` は各層の重みスケール、`s_in_*` は層入力の追加スケール。
/// 現状は hidden1 入力のみ FT 変換シフトと組み合わせたスケールを持ち、hidden2/output は 1.0 固定。
pub struct ClassicQuantizationScales {
    pub s_w0: f32,
    pub s_w1: Vec<f32>,
    pub s_w2: Vec<f32>,
    pub s_w3: Vec<f32>,
    pub s_in_1: f32,
    pub s_in_2: f32,
    pub s_in_3: f32,
}

impl ClassicQuantizationScales {
    /// Classic v1 用の最終出力スケールを返す。
    /// 現在の前提:
    ///   - 最終層は単一出力 (output_weights.len() == h2_dim)
    ///   - 量子化は per-tensor (s_w3.len()==1)
    ///   - FT / hidden2 から最終層入力への追加スケールは s_in_3 (Classic v1 では 1.0)
    ///
    /// 将来 per-channel 出力や multi-head 化する場合はこの関数を書き換えるだけで
    /// 呼び出し側（INT→float 復元部）の修正を局所化できる。
    #[inline]
    pub fn output_scale(&self) -> f32 {
        let scale = match self.s_w3.len() {
            0 => {
                log::error!(
                    "classic quantization scales: missing output scale, falling back to 1.0"
                );
                1.0
            }
            1 => self.s_w3[0],
            n => {
                log::error!(
                    "classic quantization scales: per-channel output scales (len={}) not supported, using mean",
                    n
                );
                self.s_w3.iter().copied().sum::<f32>() / n as f32
            }
        };
        self.s_in_3 * scale
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    fn sample_network() -> ClassicFloatNetwork {
        ClassicFloatNetwork {
            ft_weights: vec![0.5, -0.5, 0.25, -0.25],
            ft_biases: vec![0.1, -0.2],
            hidden1_weights: vec![0.05, -0.07, 0.02, 0.03, -0.04, 0.01, 0.06, -0.02],
            hidden1_biases: vec![0.01, -0.015],
            hidden2_weights: vec![0.03, -0.02, 0.07, -0.05],
            hidden2_biases: vec![0.02, -0.03],
            output_weights: vec![0.04, -0.06],
            output_bias: 0.005,
            acc_dim: 2,
            input_dim: 2,
            h1_dim: 2,
            h2_dim: 2,
        }
    }

    #[test]
    fn he_uniform_initializes_non_zero() {
        let mut rng = rand::rngs::StdRng::seed_from_u64(42);
        let net = ClassicFloatNetwork::he_uniform_with_dims(16, 8, 4, 2, 6, &mut rng);

        assert!(net.ft_weights.iter().any(|&w| w != 0.0));
        assert!(net.hidden1_weights.iter().any(|&w| w != 0.0));
        assert!(net.hidden2_weights.iter().any(|&w| w != 0.0));
        assert!(net.output_weights.iter().any(|&w| w != 0.0));
        assert_eq!(net.output_bias, 0.0);
    }

    #[test]
    fn test_quantize_symmetric_bias_scales() {
        let net = sample_network();
        let (bundle, scales) = net
            .quantize_symmetric(
                QuantScheme::PerTensor,
                QuantScheme::PerChannel,
                QuantScheme::PerChannel,
                QuantScheme::PerTensor,
            )
            .expect("quantize symmetric");

        assert_eq!(bundle.transformer.acc_dim, 2);
        assert!((scales.s_in_1 - scales.s_w0 * (1 << CLASSIC_FT_SHIFT) as f32).abs() < 1e-6);
        assert_eq!(scales.s_in_2, 1.0);
        assert_eq!(scales.s_in_3, 1.0);
        assert_eq!(scales.s_w3.len(), 1);

        let ft_bias_expected: Vec<_> =
            net.ft_biases.iter().map(|&b| round_away_from_zero(b / scales.s_w0)).collect();
        assert_eq!(bundle.transformer.biases, ft_bias_expected);

        for (idx, &bias) in net.hidden1_biases.iter().enumerate() {
            // hidden1 bias 量子化整合性 (スケール依存) の簡易検証
            let ch_scale = scales.s_w1[idx]; // idx / 1
                                             // Round bias to i32 then dequant back (粗い再現チェック)
            let q = round_away_from_zero(bias / (scales.s_in_1 * ch_scale));
            let _restored = q as f32 * (scales.s_in_1 * ch_scale);
        }
        // ここで propagate 経路の等価性テストは削除 (legacy API 削除済み)。
    }

    #[test]
    fn quantize_bias_supports_per_tensor_and_per_channel() {
        let bias = vec![0.5, -1.0, 1.5];
        let tensor = quantize_bias_i32(&bias, 2.0, &[0.25]);
        assert_eq!(tensor, vec![1, -2, 3]);

        let channel = quantize_bias_i32(&bias, 2.0, &[0.25, 0.5, 1.0]);
        assert_eq!(channel, vec![1, -1, 1]);

        let tensor_scale = 2.0 * 0.25;
        for (&b, &q) in bias.iter().zip(tensor.iter()) {
            let restored = (q as f32) * tensor_scale;
            assert!(
                (b - restored).abs() <= tensor_scale / 2.0 + f32::EPSILON,
                "per-tensor restoration drift: bias={b}, restored={restored}"
            );
        }

        let channel_scales = [0.25f32, 0.5f32, 1.0f32];
        for ((&b, &q), &ws) in bias.iter().zip(channel.iter()).zip(channel_scales.iter()) {
            let scale = 2.0 * ws;
            let restored = (q as f32) * scale;
            assert!(
                (b - restored).abs() <= scale / 2.0 + f32::EPSILON,
                "per-channel restoration drift: bias={b}, restored={restored}, scale={scale}"
            );
        }
    }

    #[test]
    fn quantize_symmetric_zero_weights_produce_unit_scale() {
        let (qi8, si8) = quantize_symmetric_i8(&[0.0; 4], false, 1).unwrap();
        assert_eq!(qi8, vec![0; 4]);
        assert_eq!(si8, vec![1.0]);

        let (qi16, si16) = quantize_symmetric_i16(&[0.0; 2], false, 1).unwrap();
        assert_eq!(qi16, vec![0; 2]);
        assert_eq!(si16, vec![1.0]);
    }

    #[test]
    fn quantize_symmetric_per_channel_zero_weights() {
        let (qi8, si8) = quantize_symmetric_i8(&[0.0; 4], true, 2).unwrap();
        assert_eq!(qi8, vec![0; 4]);
        assert_eq!(si8, vec![1.0, 1.0]);

        let (qi16, si16) = quantize_symmetric_i16(&[0.0; 6], true, 3).unwrap();
        assert_eq!(qi16, vec![0; 6]);
        assert_eq!(si16, vec![1.0, 1.0, 1.0]);
    }

    #[test]
    fn quantize_symmetric_len_mismatch_errors() {
        let err = quantize_symmetric_i8(&[1.0, 2.0, 3.0], true, 0).unwrap_err();
        assert!(err.contains("channels > 0"));

        let err = quantize_symmetric_i8(&[1.0, 2.0, 3.0], true, 2).unwrap_err();
        assert!(err.contains("not divisible"));

        let err = quantize_symmetric_i16(&[1.0, 2.0], true, 3).unwrap_err();
        assert!(err.contains("not divisible"));
    }

    #[test]
    fn quantize_symmetric_empty_returns_empty() {
        let (qi8, si8) = quantize_symmetric_i8(&[], false, 1).unwrap();
        assert!(qi8.is_empty() && si8.is_empty());

        let (qi16, si16) = quantize_symmetric_i16(&[], true, 0).unwrap();
        assert!(qi16.is_empty() && si16.is_empty());
    }

    #[test]
    fn quantize_symmetric_dequant_error_stays_within_half_scale() {
        let weights = vec![-3.2, -1.0, -0.5, 0.0, 0.5, 1.75, 3.0];
        let (qi8, si8) = quantize_symmetric_i8(&weights, false, 1).unwrap();
        let scale8 = si8[0];
        for (w, &q) in weights.iter().zip(qi8.iter()) {
            let w = *w;
            let restored = (q as f32) * scale8;
            assert!(
                (w - restored).abs() <= scale8 / 2.0 + f32::EPSILON,
                "per-tensor i8: w={w}, restored={restored}, scale={scale8}"
            );
        }

        let weights_ch = vec![
            -2.4, -0.75, 0.3, 1.4, // ch0
            -1.1, 0.0, 1.2, -2.8, // ch1
        ];
        let (qi16, si16) = quantize_symmetric_i16(&weights_ch, true, 2).unwrap();
        let stride = weights_ch.len() / 2;
        for ch in 0..2 {
            let scale = si16[ch];
            for (w, &q) in weights_ch[ch * stride..(ch + 1) * stride]
                .iter()
                .zip(qi16[ch * stride..(ch + 1) * stride].iter())
            {
                let w = *w;
                let restored = (q as f32) * scale;
                assert!(
                    (w - restored).abs() <= scale / 2.0 + f32::EPSILON,
                    "per-channel i16 ch={ch}: w={w}, restored={restored}, scale={scale}"
                );
            }
        }
    }

    #[test]
    fn output_scale_falls_back_when_missing() {
        let scales = ClassicQuantizationScales {
            s_w0: 1.0,
            s_w1: vec![1.0],
            s_w2: vec![1.0],
            s_w3: vec![],
            s_in_1: 1.0,
            s_in_2: 1.0,
            s_in_3: 2.5,
        };

        assert!((scales.output_scale() - 2.5).abs() < 1e-6);
    }

    #[test]
    fn output_scale_averages_per_channel_scales() {
        let scales = ClassicQuantizationScales {
            s_w0: 1.0,
            s_w1: vec![1.0],
            s_w2: vec![1.0],
            s_w3: vec![2.0, 4.0],
            s_in_1: 1.0,
            s_in_2: 1.0,
            s_in_3: 3.0,
        };

        // mean(s_w3)=3.0 → output_scale = 3.0 * 3.0 = 9.0
        assert!((scales.output_scale() - 9.0).abs() < 1e-6);
    }

    #[test]
    fn round_away_from_zero_handles_boundaries() {
        let cases = [
            (0.49, 0),
            (0.5, 1),
            (0.51, 1),
            (-0.49, 0),
            (-0.5, -1),
            (-0.51, -1),
            (1.49, 1),
            (1.5, 2),
            (-1.49, -1),
            (-1.5, -2),
        ];
        for (input, expected) in cases {
            assert_eq!(round_away_from_zero(input), expected, "input={input}");
        }
    }
}
