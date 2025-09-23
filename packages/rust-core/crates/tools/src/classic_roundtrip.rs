use std::io::{BufRead, BufReader, Read};

use anyhow::{anyhow, bail, Context, Result};
use engine_core::evaluation::nnue::features::{self, flip_us_them, FE_END};
use engine_core::shogi::SHOGI_BOARD_SIZE;
use engine_core::Position;
use serde::Deserialize;
use std::fs::File;
use std::path::{Path, PathBuf};

const DEFAULT_FT_SHIFT: i32 = 6;
const DEFAULT_FT_SCALE: f32 = 64.0;
const QUANT_MIN_I32: i32 = -127;
const QUANT_MAX_I32: i32 = 127;
const QUANT_MIN_F32: f32 = QUANT_MIN_I32 as f32;
const QUANT_MAX_F32: f32 = QUANT_MAX_I32 as f32;
const CLASSIC_V1_ARCH_ID: u32 = 0x7AF3_2F16;

#[derive(Debug, Clone)]
pub struct ClassicFp32Network {
    pub acc_dim: usize,
    pub input_dim: usize,
    pub h1_dim: usize,
    pub h2_dim: usize,
    pub relu_clip: f32,
    ft_weights: Vec<f32>,
    ft_biases: Vec<f32>,
    hidden1_weights: Vec<f32>,
    hidden1_biases: Vec<f32>,
    hidden2_weights: Vec<f32>,
    hidden2_biases: Vec<f32>,
    output_weights: Vec<f32>,
    output_bias: f32,
}

#[derive(Debug, Clone)]
pub struct LayerOutputs {
    /// Feature transformer outputs (float domain) in accumulator order.
    pub ft: Vec<f32>,
    /// Hidden layer 1 post-activation (ClippedReLU) values rescaled to float.
    pub h1: Vec<f32>,
    /// Hidden layer 2 post-activation (ClippedReLU) values rescaled to float.
    pub h2: Vec<f32>,
    /// Final network output (typically a logit before cp/WDL conversion).
    pub output: f32,
}

/// Scratch buffers for `ClassicFp32Network::forward_with_scratch` to avoid repeated allocations.
pub struct ClassicFp32Scratch {
    acc_us: Vec<f32>,
    acc_them: Vec<f32>,
    input: Vec<f32>,
    h1: Vec<f32>,
    h2: Vec<f32>,
}

impl ClassicFp32Scratch {
    pub fn new(acc_dim: usize, h1_dim: usize, h2_dim: usize) -> Self {
        Self {
            acc_us: vec![0.0; acc_dim],
            acc_them: vec![0.0; acc_dim],
            input: vec![0.0; acc_dim * 2],
            h1: vec![0.0; h1_dim],
            h2: vec![0.0; h2_dim],
        }
    }
}

impl ClassicFp32Network {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let file = File::open(path).with_context(|| {
            format!("failed to open Classic FP32 network at {}", path.display())
        })?;
        let mut reader = BufReader::new(file);

        let mut line = String::new();
        reader.read_line(&mut line)?;
        if line.trim() != "NNUE" {
            bail!("invalid FP32 header magic: {}", line.trim());
        }

        let mut acc_dim: Option<usize> = None;
        let mut h1_dim: Option<usize> = None;
        let mut h2_dim: Option<usize> = None;
        let mut feature_dim_text: Option<usize> = None;
        let mut relu_clip: Option<f32> = None;

        loop {
            line.clear();
            if reader.read_line(&mut line)? == 0 {
                bail!("unexpected EOF when reading FP32 header");
            }
            let trimmed = line.trim();
            if trimmed == "END_HEADER" {
                break;
            }
            if trimmed.is_empty() {
                continue;
            }
            let mut parts = trimmed.split_whitespace();
            let key = parts.next().unwrap();
            match key {
                "ACC_DIM" => acc_dim = parts.next().and_then(|v| v.parse().ok()),
                "H1_DIM" => h1_dim = parts.next().and_then(|v| v.parse().ok()),
                "H2_DIM" => h2_dim = parts.next().and_then(|v| v.parse().ok()),
                "FEATURE_DIM" => feature_dim_text = parts.next().and_then(|v| v.parse().ok()),
                "RELU_CLIP" => relu_clip = parts.next().and_then(|v| v.parse().ok()),
                _ => {}
            }
        }

        let input_dim = read_u32(&mut reader)? as usize;
        let acc_dim_payload = read_u32(&mut reader)? as usize;
        let h1_dim_payload = read_u32(&mut reader)? as usize;
        let h2_dim_payload = read_u32(&mut reader)? as usize;

        let acc_dim = acc_dim.or(Some(acc_dim_payload)).context("missing ACC_DIM")?;
        let h1_dim = h1_dim.or(Some(h1_dim_payload)).context("missing H1_DIM")?;
        let h2_dim = h2_dim.or(Some(h2_dim_payload)).context("missing H2_DIM")?;
        let feature_dim = feature_dim_text.unwrap_or(input_dim);
        if feature_dim != input_dim {
            bail!("feature_dim mismatch: header {}, payload {}", feature_dim, input_dim);
        }
        let relu_clip = relu_clip.unwrap_or(127.0);

        let ft_weights = read_f32_vec(&mut reader, input_dim * acc_dim)?;
        let ft_biases = read_f32_vec(&mut reader, acc_dim)?;
        let hidden1_weights = read_f32_vec(&mut reader, acc_dim * 2 * h1_dim)?;
        let hidden1_biases = read_f32_vec(&mut reader, h1_dim)?;
        let hidden2_weights = read_f32_vec(&mut reader, h1_dim * h2_dim)?;
        let hidden2_biases = read_f32_vec(&mut reader, h2_dim)?;
        let output_weights = read_f32_vec(&mut reader, h2_dim)?;
        let output_bias = read_f32(&mut reader)?;

        Ok(Self {
            acc_dim,
            input_dim,
            h1_dim,
            h2_dim,
            relu_clip,
            ft_weights,
            ft_biases,
            hidden1_weights,
            hidden1_biases,
            hidden2_weights,
            hidden2_biases,
            output_weights,
            output_bias,
        })
    }

    pub fn forward(&self, features_us: &[usize], features_them: &[usize]) -> LayerOutputs {
        let mut scratch = ClassicFp32Scratch::new(self.acc_dim, self.h1_dim, self.h2_dim);
        self.forward_with_scratch(features_us, features_them, &mut scratch)
    }

    pub fn forward_with_scratch(
        &self,
        features_us: &[usize],
        features_them: &[usize],
        scratch: &mut ClassicFp32Scratch,
    ) -> LayerOutputs {
        scratch.acc_us.copy_from_slice(&self.ft_biases);
        accumulate_fp32(
            &mut scratch.acc_us,
            features_us,
            self.acc_dim,
            self.input_dim,
            &self.ft_weights,
        );

        scratch.acc_them.copy_from_slice(&self.ft_biases);
        accumulate_fp32(
            &mut scratch.acc_them,
            features_them,
            self.acc_dim,
            self.input_dim,
            &self.ft_weights,
        );

        scratch.input.resize(self.acc_dim * 2, 0.0);
        scratch.input[..self.acc_dim].copy_from_slice(&scratch.acc_us);
        scratch.input[self.acc_dim..].copy_from_slice(&scratch.acc_them);

        let input_dim_h1 = self.acc_dim * 2;
        scratch.h1.resize(self.h1_dim, 0.0);
        for i in 0..self.h1_dim {
            let mut sum = self.hidden1_biases[i];
            let weights = &self.hidden1_weights[i * input_dim_h1..(i + 1) * input_dim_h1];
            for (x, w) in scratch.input.iter().zip(weights.iter()) {
                sum += x * w;
            }
            scratch.h1[i] = sum.max(0.0).min(self.relu_clip);
        }

        scratch.h2.resize(self.h2_dim, 0.0);
        for i in 0..self.h2_dim {
            let mut sum = self.hidden2_biases[i];
            let weights = &self.hidden2_weights[i * self.h1_dim..(i + 1) * self.h1_dim];
            for (x, w) in scratch.h1.iter().zip(weights.iter()) {
                sum += x * w;
            }
            scratch.h2[i] = sum.max(0.0).min(self.relu_clip);
        }

        let mut output = self.output_bias;
        for (w, x) in self.output_weights.iter().zip(scratch.h2.iter()) {
            output += w * x;
        }

        let mut ft_combined = Vec::with_capacity(self.acc_dim * 2);
        ft_combined.extend_from_slice(&scratch.acc_us);
        ft_combined.extend_from_slice(&scratch.acc_them);

        LayerOutputs {
            ft: ft_combined,
            h1: scratch.h1.clone(),
            h2: scratch.h2.clone(),
            output,
        }
    }

    pub fn relu_clip_value(&self) -> f32 {
        self.relu_clip
    }
}

fn accumulate_fp32(
    acc: &mut [f32],
    features: &[usize],
    acc_dim: usize,
    input_dim: usize,
    weights: &[f32],
) {
    for &feat in features {
        if feat >= input_dim {
            continue;
        }
        let base = feat * acc_dim;
        let slice = &weights[base..base + acc_dim];
        for (dst, &w) in acc.iter_mut().zip(slice.iter()) {
            *dst += w;
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClassicQuantizationScalesData {
    pub schema_version: u32,
    pub format_version: String,
    pub arch: String,
    pub acc_dim: usize,
    pub h1_dim: usize,
    pub h2_dim: usize,
    pub input_dim: usize,
    pub s_w0: f32,
    pub s_w1: Vec<f32>,
    pub s_w2: Vec<f32>,
    pub s_w3: Vec<f32>,
    pub s_in_1: f32,
    pub s_in_2: f32,
    pub s_in_3: f32,
    pub bundle_sha256: String,
    #[serde(default)]
    pub quant_scheme: Option<QuantSchemeReportData>,
    #[serde(default)]
    pub activation: Option<ClassicActivationSummaryData>,
    #[serde(default)]
    pub calibration_metrics: Option<ClassicQuantMetricsData>,
    #[serde(default)]
    pub eval_metrics: Option<ClassicQuantMetricsData>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct QuantSchemeReportData {
    pub ft: Option<String>,
    pub h1: Option<String>,
    pub h2: Option<String>,
    #[serde(rename = "out", default)]
    pub out: Option<String>,
}

fn normalize_quant_label(label: &str) -> Option<&'static str> {
    let normalized = label.trim().replace('_', "-");
    match normalized.to_ascii_lowercase().as_str() {
        "per-tensor" => Some("per-tensor"),
        "per-channel" => Some("per-channel"),
        _ => None,
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClassicActivationSummaryData {
    pub ft_max_abs: f32,
    pub h1_max_abs: f32,
    pub h2_max_abs: f32,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ClassicQuantMetricsData {
    pub n: usize,
    #[serde(default)]
    pub mae_cp: Option<f32>,
    #[serde(default)]
    pub p95_cp: Option<f32>,
    #[serde(default)]
    pub max_cp: Option<f32>,
    #[serde(default)]
    pub mae_logit: Option<f32>,
    #[serde(default)]
    pub p95_logit: Option<f32>,
    #[serde(default)]
    pub max_logit: Option<f32>,
}

#[derive(Debug, Clone)]
pub struct ClassicIntNetwork {
    transformer_weights: Vec<i16>,
    transformer_biases: Vec<i32>,
    acc_dim: usize,
    input_dim: usize,
    hidden1_weights: Vec<i8>,
    hidden1_biases: Vec<i32>,
    hidden2_weights: Vec<i8>,
    hidden2_biases: Vec<i32>,
    output_weights: Vec<i8>,
    output_bias: i32,
    h1_dim: usize,
    h2_dim: usize,
    scales: ClassicQuantizationScalesData,
    ft_scale: f32,
    ft_shift: Option<i32>,
}

impl ClassicIntNetwork {
    pub fn load(int_path: impl AsRef<Path>, scales_path: Option<PathBuf>) -> Result<Self> {
        let int_path = int_path.as_ref();
        let scales_path = scales_path.unwrap_or_else(|| {
            let mut candidate = int_path.to_path_buf();
            candidate.set_file_name("nn.classic.scales.json");
            candidate
        });

        let scales_file = File::open(&scales_path)
            .with_context(|| format!("failed to open scales JSON: {}", scales_path.display()))?;
        let mut scales: ClassicQuantizationScalesData = serde_json::from_reader(scales_file)
            .with_context(|| format!("failed to parse scales JSON: {}", scales_path.display()))?;
        if scales.schema_version != 1 {
            bail!("unsupported scales schema_version {} (expected 1)", scales.schema_version);
        }

        let file = File::open(int_path).with_context(|| {
            format!("failed to open Classic INT network: {}", int_path.display())
        })?;
        let mut reader = BufReader::new(file);
        let mut header = [0u8; 16];
        reader.read_exact(&mut header)?;
        if &header[0..4] != b"NNUE" {
            bail!("invalid Classic INT magic");
        }
        let version = u32::from_le_bytes(header[4..8].try_into().unwrap());
        if version != 1 {
            bail!("unsupported Classic NNUE version {version}");
        }
        let arch = u32::from_le_bytes(header[8..12].try_into().unwrap());
        if arch != CLASSIC_V1_ARCH_ID {
            bail!("unexpected architecture ID 0x{arch:08X}");
        }
        let declared_size = u32::from_le_bytes(header[12..16].try_into().unwrap()) as u64;
        let metadata_size = reader.get_ref().metadata()?.len();
        if declared_size != metadata_size {
            bail!(
                "size mismatch in nn.classic.nnue (declared {}, actual {})",
                declared_size,
                metadata_size
            );
        }

        let acc_dim = scales.acc_dim;
        let input_dim = scales.input_dim;
        let h1_dim = scales.h1_dim;
        let h2_dim = scales.h2_dim;

        // canonical dims (FT canonical size is SHOGI_BOARD_SIZE * FE_END)
        if scales.input_dim > SHOGI_BOARD_SIZE * FE_END {
            bail!(
                "scales input_dim {} exceeds canonical {}",
                scales.input_dim,
                SHOGI_BOARD_SIZE * FE_END
            );
        }
        if scales.input_dim != SHOGI_BOARD_SIZE * FE_END {
            log::warn!(
                "scales input_dim {} differs from canonical {}",
                scales.input_dim,
                SHOGI_BOARD_SIZE * FE_END
            );
        }
        let canonical_input_dim = SHOGI_BOARD_SIZE * FE_END;
        let mut ft_weights = read_i16_vec(&mut reader, input_dim * acc_dim)?;
        let padding_ft_bytes = canonical_input_dim
            .saturating_sub(input_dim)
            .saturating_mul(acc_dim)
            .saturating_mul(std::mem::size_of::<i16>());
        if padding_ft_bytes > 0 {
            let rest_bytes = {
                let mut total = 0usize;
                total = total
                    .checked_add(acc_dim * std::mem::size_of::<i32>())
                    .context("ft_biases payload overflow")?;
                total = total
                    .checked_add(acc_dim * 2 * h1_dim)
                    .context("hidden1 weights payload overflow")?;
                total = total
                    .checked_add(h1_dim * std::mem::size_of::<i32>())
                    .context("hidden1 biases payload overflow")?;
                total = total
                    .checked_add(h1_dim * h2_dim)
                    .context("hidden2 weights payload overflow")?;
                total = total
                    .checked_add(h2_dim * std::mem::size_of::<i32>())
                    .context("hidden2 biases payload overflow")?;
                total = total.checked_add(h2_dim).context("output weights payload overflow")?;
                total = total
                    .checked_add(std::mem::size_of::<i32>())
                    .context("output bias payload overflow")?;
                total
            };

            let header_bytes = 16usize;
            let ft_section_bytes = input_dim
                .checked_mul(acc_dim)
                .and_then(|v| v.checked_mul(std::mem::size_of::<i16>()))
                .context("ft section size overflow")?;
            let metadata_size =
                usize::try_from(metadata_size).context("nn.classic.nnue size exceeds usize")?;
            let consumed_so_far =
                header_bytes.checked_add(ft_section_bytes).context("consumed size overflow")?;
            let available_padding = metadata_size.saturating_sub(consumed_so_far + rest_bytes);

            let pad_to_read = available_padding.min(padding_ft_bytes);
            if pad_to_read > 0 {
                let mut buf = vec![0u8; pad_to_read];
                reader.read_exact(&mut buf)?;
                if buf.iter().any(|&b| b != 0) {
                    log::warn!("non-zero bytes found in FT padding region; ignoring extra payload");
                }
            }
            if available_padding > pad_to_read {
                let extra = available_padding - pad_to_read;
                let mut buf = vec![0u8; extra];
                reader.read_exact(&mut buf)?;
                log::warn!("extra {} bytes beyond expected FT padding; discarding", extra);
            }
            if padding_ft_bytes > pad_to_read {
                let missing = padding_ft_bytes - pad_to_read;
                if missing > 0 {
                    log::debug!(
                        "ft padding missing {} bytes; treating absent padding as zero",
                        missing
                    );
                }
            }
        }
        let mut ft_biases = read_i32_vec(&mut reader, acc_dim)?;
        let hidden1_weights = read_i8_vec(&mut reader, acc_dim * 2 * h1_dim)?;
        let hidden1_biases = read_i32_vec(&mut reader, h1_dim)?;
        let hidden2_weights = read_i8_vec(&mut reader, h1_dim * h2_dim)?;
        let hidden2_biases = read_i32_vec(&mut reader, h2_dim)?;
        let output_weights = read_i8_vec(&mut reader, h2_dim)?;
        let output_bias = read_i32(&mut reader)?;

        validate_scale_lengths(&scales, h1_dim, h2_dim)?;
        sanitize_scale_values(&mut scales);
        validate_scale_values(&scales)?;
        verify_quant_scheme(&scales, h1_dim, h2_dim)?;

        let (ft_scale, ft_shift) = compute_ft_scale(&scales);

        // Avoid retaining potential padding in biases/weights (not expected, but be safe)
        ft_weights.truncate(input_dim * acc_dim);
        ft_biases.truncate(acc_dim);

        Ok(Self {
            transformer_weights: ft_weights,
            transformer_biases: ft_biases,
            acc_dim,
            input_dim,
            hidden1_weights,
            hidden1_biases,
            hidden2_weights,
            hidden2_biases,
            output_weights,
            output_bias,
            h1_dim,
            h2_dim,
            scales,
            ft_scale,
            ft_shift,
        })
    }

    pub fn forward(&self, features_us: &[usize], features_them: &[usize]) -> LayerOutputs {
        let (acc_us_i16, ft_us_float) = self.accumulate_ft(features_us);
        let (acc_them_i16, ft_them_float) = self.accumulate_ft(features_them);

        let mut input = Vec::with_capacity(self.acc_dim * 2);
        for &v in &acc_us_i16 {
            input.push(self.quantize_ft_value(v));
        }
        for &v in &acc_them_i16 {
            input.push(self.quantize_ft_value(v));
        }

        let mut h1_act_i8 = Vec::with_capacity(self.h1_dim);
        let mut h1_act_f32 = Vec::with_capacity(self.h1_dim);
        let input_dim_h1 = self.acc_dim * 2;
        for i in 0..self.h1_dim {
            let mut sum = self.hidden1_biases[i];
            let row = &self.hidden1_weights[i * input_dim_h1..(i + 1) * input_dim_h1];
            for (j, &w) in row.iter().enumerate() {
                sum += input[j] as i32 * w as i32;
            }
            let act_i8 = sum.clamp(0, QUANT_MAX_I32) as i8;
            h1_act_i8.push(act_i8);
            h1_act_f32.push(act_i8 as f32 * self.scales.s_in_2);
        }

        let mut h2_act_i8 = Vec::with_capacity(self.h2_dim);
        let mut h2_act_f32 = Vec::with_capacity(self.h2_dim);
        for i in 0..self.h2_dim {
            let mut sum = self.hidden2_biases[i];
            let row = &self.hidden2_weights[i * self.h1_dim..(i + 1) * self.h1_dim];
            for (j, &w) in row.iter().enumerate() {
                sum += h1_act_i8[j] as i32 * w as i32;
            }
            let act_i8 = sum.clamp(0, QUANT_MAX_I32) as i8;
            h2_act_i8.push(act_i8);
            h2_act_f32.push(act_i8 as f32 * self.scales.s_in_3);
        }

        let mut sum = self.output_bias;
        for (w, act_i8) in self.output_weights.iter().zip(h2_act_i8.iter()) {
            sum += *w as i32 * *act_i8 as i32;
        }
        let ws = scale_for_channel(&self.scales.s_w3, 0);
        let output_float = sum as f32 * self.scales.s_in_3 * ws;

        let mut ft_combined = Vec::with_capacity(self.acc_dim * 2);
        ft_combined.extend(ft_us_float);
        ft_combined.extend(ft_them_float);

        LayerOutputs {
            ft: ft_combined,
            h1: h1_act_f32,
            h2: h2_act_f32,
            output: output_float,
        }
    }

    fn accumulate_ft(&self, features: &[usize]) -> (Vec<i16>, Vec<f32>) {
        let mut acc: Vec<i32> = self.transformer_biases.clone();
        for &feat in features {
            if feat >= self.input_dim {
                continue;
            }
            let base = feat * self.acc_dim;
            let row = &self.transformer_weights[base..base + self.acc_dim];
            for (dst, &w) in acc.iter_mut().zip(row.iter()) {
                *dst += w as i32;
            }
        }

        let mut acc_i16 = Vec::with_capacity(self.acc_dim);
        let mut acc_float = Vec::with_capacity(self.acc_dim);
        for sum in acc.into_iter() {
            let clamped = sum.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            acc_i16.push(clamped);
            acc_float.push(clamped as f32 * self.scales.s_w0);
        }
        (acc_i16, acc_float)
    }

    #[inline]
    fn quantize_ft_value(&self, value: i16) -> i8 {
        if let Some(shift) = self.ft_shift {
            if shift <= 0 {
                return (value as i32).clamp(QUANT_MIN_I32, QUANT_MAX_I32) as i8;
            }
            let shifted = (value as i32) >> shift;
            return shifted.clamp(QUANT_MIN_I32, QUANT_MAX_I32) as i8;
        }

        let scale = if self.ft_scale <= 0.0 {
            DEFAULT_FT_SCALE
        } else {
            self.ft_scale
        };

        // NOTE: 非 2 冪スケール時は算術右シフトと同じ丸め（負側はより負方向への切り捨て）を再現し、
        // SIMD 実装と整合させる。
        let scaled = (value as f32 / scale).floor();
        scaled.clamp(QUANT_MIN_F32, QUANT_MAX_F32) as i8
    }

    pub fn scales(&self) -> &ClassicQuantizationScalesData {
        &self.scales
    }
}

fn validate_scale_lengths(
    scales: &ClassicQuantizationScalesData,
    h1_dim: usize,
    h2_dim: usize,
) -> Result<()> {
    fn check(name: &str, values: &[f32], expected: usize) -> Result<()> {
        if values.is_empty() {
            bail!("{name} scale vector is empty (expected length {expected})");
        }
        if values.len() == 1 || values.len() == expected {
            Ok(())
        } else if expected == 1 {
            bail!("{name} scale vector length {} must be 1", values.len());
        } else {
            bail!("{name} scale vector length {} must be 1 or {}", values.len(), expected);
        }
    }

    check("s_w1", &scales.s_w1, h1_dim)?;
    check("s_w2", &scales.s_w2, h2_dim)?;
    check("s_w3", &scales.s_w3, 1)?;

    Ok(())
}

fn validate_scale_values(sc: &ClassicQuantizationScalesData) -> Result<()> {
    fn ensure_pos(name: &str, value: f32) -> Result<()> {
        if !value.is_finite() || value <= 0.0 {
            bail!("{name} must be finite and > 0 (got {value})");
        }
        Ok(())
    }

    ensure_pos("s_in_1", sc.s_in_1)?;
    ensure_pos("s_in_2", sc.s_in_2)?;
    ensure_pos("s_in_3", sc.s_in_3)?;
    ensure_pos("s_w0", sc.s_w0)?;
    for (idx, &value) in sc.s_w1.iter().enumerate() {
        ensure_pos(&format!("s_w1[{idx}]"), value)?;
    }
    for (idx, &value) in sc.s_w2.iter().enumerate() {
        ensure_pos(&format!("s_w2[{idx}]"), value)?;
    }
    for (idx, &value) in sc.s_w3.iter().enumerate() {
        ensure_pos(&format!("s_w3[{idx}]"), value)?;
    }
    Ok(())
}

fn sanitize_scale_values(scales: &mut ClassicQuantizationScalesData) {
    const MIN_VALUE: f32 = 1e-6;

    fn sanitize_scalar(name: &str, value: &mut f32) {
        if !value.is_finite() {
            log::warn!("{name} is {} (invalid); falling back to {:.6}", *value, MIN_VALUE);
            *value = MIN_VALUE;
        } else if *value > 0.0 && *value < MIN_VALUE {
            log::warn!("{name} is {:.6} (too small); clamping to {:.6}", *value, MIN_VALUE);
            *value = MIN_VALUE;
        }
    }

    fn sanitize_vec(name: &str, values: &mut [f32]) {
        for (idx, val) in values.iter_mut().enumerate() {
            if !val.is_finite() {
                log::warn!("{name}[{idx}] is {} (invalid); falling back to {:.6}", *val, MIN_VALUE);
                *val = MIN_VALUE;
            } else if *val > 0.0 && *val < MIN_VALUE {
                log::warn!(
                    "{name}[{idx}] is {:.6} (too small); clamping to {:.6}",
                    *val,
                    MIN_VALUE
                );
                *val = MIN_VALUE;
            }
        }
    }

    sanitize_scalar("s_w0", &mut scales.s_w0);
    sanitize_scalar("s_in_1", &mut scales.s_in_1);
    sanitize_scalar("s_in_2", &mut scales.s_in_2);
    sanitize_scalar("s_in_3", &mut scales.s_in_3);
    sanitize_vec("s_w1", &mut scales.s_w1);
    sanitize_vec("s_w2", &mut scales.s_w2);
    sanitize_vec("s_w3", &mut scales.s_w3);
}

fn verify_quant_scheme(
    scales: &ClassicQuantizationScalesData,
    h1_dim: usize,
    h2_dim: usize,
) -> Result<()> {
    let derived_ft = "per-tensor";
    let derived_h1 = if scales.s_w1.len() == h1_dim && h1_dim > 0 {
        "per-channel"
    } else {
        "per-tensor"
    };
    let derived_h2 = if scales.s_w2.len() == h2_dim && h2_dim > 0 {
        "per-channel"
    } else {
        "per-tensor"
    };
    let derived_out = if scales.s_w3.len() > 1 {
        "per-channel"
    } else {
        "per-tensor"
    };

    if let Some(report) = scales.quant_scheme.as_ref() {
        if let Some(raw) = report.ft.as_deref() {
            let ft = normalize_quant_label(raw)
                .ok_or_else(|| anyhow!("quant_scheme.ft='{raw}' is not recognized"))?;
            if ft != derived_ft {
                bail!("quant_scheme.ft='{ft}' but payload implies '{derived_ft}'");
            }
        }
        if let Some(raw) = report.h1.as_deref() {
            let h1 = normalize_quant_label(raw)
                .ok_or_else(|| anyhow!("quant_scheme.h1='{raw}' is not recognized"))?;
            if h1 != derived_h1 {
                bail!(
                    "quant_scheme.h1='{h1}' but payload implies '{derived_h1}' (s_w1 len {})",
                    scales.s_w1.len()
                );
            }
        } else if scales.s_w1.len() == h1_dim {
            // No report but payload indicates per-channel; accept.
        }
        if let Some(raw) = report.h2.as_deref() {
            let h2 = normalize_quant_label(raw)
                .ok_or_else(|| anyhow!("quant_scheme.h2='{raw}' is not recognized"))?;
            if h2 != derived_h2 {
                bail!(
                    "quant_scheme.h2='{h2}' but payload implies '{derived_h2}' (s_w2 len {})",
                    scales.s_w2.len()
                );
            }
        }
        if let Some(raw) = report.out.as_deref() {
            let out = normalize_quant_label(raw)
                .ok_or_else(|| anyhow!("quant_scheme.out='{raw}' is not recognized"))?;
            if out != derived_out {
                bail!(
                    "quant_scheme.out='{out}' but payload implies '{derived_out}' (s_w3 len {})",
                    scales.s_w3.len()
                );
            }
        }
    }

    // Additional sanity: derived per-channel vectors must match dims exactly
    if scales.s_w1.len() != 1 && scales.s_w1.len() != h1_dim {
        bail!("s_w1 length {} inconsistent with h1_dim {}", scales.s_w1.len(), h1_dim);
    }
    if scales.s_w2.len() != 1 && scales.s_w2.len() != h2_dim {
        bail!("s_w2 length {} inconsistent with h2_dim {}", scales.s_w2.len(), h2_dim);
    }

    Ok(())
}

fn compute_ft_scale(scales: &ClassicQuantizationScalesData) -> (f32, Option<i32>) {
    // validate_scale_values() 側で s_w0 の異常は弾く想定であり通常到達しないが、他経路からの利用や将来の回帰に備えて冗長チェックする。
    if !scales.s_w0.is_finite() || scales.s_w0.abs() <= f32::EPSILON {
        log::warn!("s_w0 is zero or non-finite; using default ft scale");
        return (DEFAULT_FT_SCALE, Some(DEFAULT_FT_SHIFT));
    }

    let ratio = scales.s_in_1 / scales.s_w0;
    if !ratio.is_finite() || ratio <= 0.0 {
        log::warn!("invalid s_in_1/s_w0 ratio {}; falling back to default", ratio);
        return (DEFAULT_FT_SCALE, Some(DEFAULT_FT_SHIFT));
    }

    let log2_ratio = ratio.log2();
    let rounded = log2_ratio.round();
    let nearest_pow2 = 2f32.powi(rounded as i32);
    let rel_error = ((nearest_pow2 - ratio) / nearest_pow2).abs();

    if rel_error <= 1e-4 {
        if rounded >= 0.0 {
            let raw_shift = rounded as i32;
            let shift = raw_shift.clamp(0, 30); // 31bit 以上の右シフトは未定義なので安全側に 30 で頭打ち。Clamped to 30 to avoid implementation-defined wide shifts.
            if raw_shift != shift {
                log::warn!("ft shift {} out of range; clamped to {}", raw_shift, shift);
            }
            log::info!("ft scale uses pow2 shift = {}", shift);
            return (2f32.powi(shift), Some(shift));
        }
        // Classic engine は固定ビットシフト (右シフト) のみを想定しており、
        // スケール < 1.0 となる負のシフトは対応外なので安全側でフォールバックする。
        log::warn!(
            "ft scale ratio {} implies negative shift {:.3}; falling back to float path",
            ratio,
            rounded
        );
    }

    log::warn!(
        "ft scale ratio {} not power-of-two; engine expects ~{:.3} (rel err {:.3e})",
        ratio,
        nearest_pow2,
        rel_error
    );

    log::info!("ft scale uses floating path scale = {:.6}", ratio);

    (ratio, None)
}

pub fn extract_feature_indices(pos: &Position) -> Result<(Vec<usize>, Vec<usize>)> {
    let stm = pos.side_to_move;
    let king_sq = pos
        .king_square(stm)
        .ok_or_else(|| anyhow!("missing king square for {:?}", stm))?;
    let features_us = features::extract_features(pos, king_sq, stm);
    let us: Vec<usize> = features_us.as_slice().to_vec();
    let them: Vec<usize> = us.iter().map(|&f| flip_us_them(f)).collect();
    Ok((us, them))
}

fn read_u32<R: Read>(reader: &mut R) -> Result<u32> {
    let mut buf = [0u8; 4];
    reader.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

fn read_f32<R: Read>(reader: &mut R) -> Result<f32> {
    let mut buf = [0u8; 4];
    reader.read_exact(&mut buf)?;
    Ok(f32::from_le_bytes(buf))
}

fn read_f32_vec<R: Read>(reader: &mut R, len: usize) -> Result<Vec<f32>> {
    let mut buf = vec![0u8; len * 4];
    reader.read_exact(&mut buf)?;
    let mut out = Vec::with_capacity(len);
    for chunk in buf.chunks_exact(4) {
        out.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    Ok(out)
}

fn read_i16_vec<R: Read>(reader: &mut R, len: usize) -> Result<Vec<i16>> {
    let mut buf = vec![0u8; len * 2];
    reader.read_exact(&mut buf)?;
    let mut out = Vec::with_capacity(len);
    for chunk in buf.chunks_exact(2) {
        out.push(i16::from_le_bytes([chunk[0], chunk[1]]));
    }
    Ok(out)
}

fn read_i32_vec<R: Read>(reader: &mut R, len: usize) -> Result<Vec<i32>> {
    let mut buf = vec![0u8; len * 4];
    reader.read_exact(&mut buf)?;
    let mut out = Vec::with_capacity(len);
    for chunk in buf.chunks_exact(4) {
        out.push(i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    Ok(out)
}

fn read_i32<R: Read>(reader: &mut R) -> Result<i32> {
    let mut buf = [0u8; 4];
    reader.read_exact(&mut buf)?;
    Ok(i32::from_le_bytes(buf))
}

fn read_i8_vec<R: Read>(reader: &mut R, len: usize) -> Result<Vec<i8>> {
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    Ok(buf.into_iter().map(|b| b as i8).collect())
}

fn scale_for_channel(scales: &[f32], idx: usize) -> f32 {
    debug_assert!(
        !scales.is_empty(),
        "scale vectors should be validated before calling scale_for_channel"
    );
    if scales.len() == 1 {
        scales[0]
    } else if idx < scales.len() {
        scales[idx]
    } else {
        debug_assert!(idx < scales.len(), "validated scale lookup should not overflow");
        // In release builds we still fall back to the last element to stay robust, but hitting this path means validation failed upstream.
        *scales.last().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_fp32_fixture(path: &Path) {
        let mut file = File::create(path).unwrap();
        use std::io::Write;
        writeln!(file, "NNUE").unwrap();
        writeln!(file, "VERSION 1").unwrap();
        writeln!(file, "FEATURES HALFKP").unwrap();
        writeln!(file, "ARCHITECTURE CLASSIC").unwrap();
        writeln!(file, "ACC_DIM 2").unwrap();
        writeln!(file, "H1_DIM 2").unwrap();
        writeln!(file, "H2_DIM 1").unwrap();
        writeln!(file, "RELU_CLIP 127").unwrap();
        writeln!(file, "FEATURE_DIM 4").unwrap();
        writeln!(file, "END_HEADER").unwrap();

        file.write_all(&(4u32.to_le_bytes())).unwrap();
        file.write_all(&(2u32.to_le_bytes())).unwrap();
        file.write_all(&(2u32.to_le_bytes())).unwrap();
        file.write_all(&(1u32.to_le_bytes())).unwrap();

        let ft_weights = [0.2f32, -0.1, 0.05, 0.15, -0.2, 0.3, 0.4, -0.25];
        for v in ft_weights.iter() {
            file.write_all(&v.to_le_bytes()).unwrap();
        }
        let ft_biases = [0.01f32, -0.02];
        for v in ft_biases.iter() {
            file.write_all(&v.to_le_bytes()).unwrap();
        }
        let hidden1_weights = [0.3f32, -0.1, 0.05, 0.2, -0.2, 0.25, -0.15, 0.1];
        for v in hidden1_weights.iter() {
            file.write_all(&v.to_le_bytes()).unwrap();
        }
        let hidden1_biases = [0.05f32, -0.03];
        for v in hidden1_biases.iter() {
            file.write_all(&v.to_le_bytes()).unwrap();
        }
        let hidden2_weights = [0.4f32, -0.35];
        for v in hidden2_weights.iter() {
            file.write_all(&v.to_le_bytes()).unwrap();
        }
        let hidden2_biases = [0.02f32];
        file.write_all(&hidden2_biases[0].to_le_bytes()).unwrap();
        let output_weights = [0.5f32];
        file.write_all(&output_weights[0].to_le_bytes()).unwrap();
        let output_bias = 0.01f32;
        file.write_all(&output_bias.to_le_bytes()).unwrap();
    }

    #[test]
    fn fp32_forward_matches_manual() {
        let td = tempdir().unwrap();
        let path = td.path().join("nn.fp32.bin");
        write_fp32_fixture(&path);

        let net = ClassicFp32Network::load(&path).unwrap();
        let features_us = vec![0usize, 1];
        let features_them = vec![2usize, 3];
        let outputs = net.forward(&features_us, &features_them);

        assert_eq!(net.acc_dim, 2);
        assert_eq!(net.h1_dim, 2);
        assert_eq!(net.h2_dim, 1);

        let expected_output = 0.0483f32;
        assert!((outputs.output - expected_output).abs() < 1e-4);
        assert_eq!(outputs.h1.len(), 2);
        assert_eq!(outputs.h2.len(), 1);
    }

    fn write_int_fixture(nn_path: &Path, scales_path: &Path) {
        let mut file = File::create(nn_path).unwrap();
        use std::io::Write;
        let total_size = 67u32;
        file.write_all(b"NNUE").unwrap();
        file.write_all(&1u32.to_le_bytes()).unwrap();
        file.write_all(&CLASSIC_V1_ARCH_ID.to_le_bytes()).unwrap();
        file.write_all(&(total_size as u32).to_le_bytes()).unwrap();

        let ft_weights: [i16; 8] = [32, 16, 20, 8, 12, 24, 10, 6];
        for v in ft_weights.iter() {
            file.write_all(&v.to_le_bytes()).unwrap();
        }
        let ft_biases: [i32; 2] = [64, 32];
        for v in ft_biases.iter() {
            file.write_all(&v.to_le_bytes()).unwrap();
        }
        let hidden1_weights: [i8; 8] = [3, -2, 1, 4, 2, 1, -3, 1];
        let hidden1_bytes: Vec<u8> = hidden1_weights.iter().map(|v| *v as u8).collect();
        file.write_all(&hidden1_bytes).unwrap();
        let hidden1_biases: [i32; 2] = [5, -4];
        for v in hidden1_biases.iter() {
            file.write_all(&v.to_le_bytes()).unwrap();
        }
        let hidden2_weights: [i8; 2] = [4, -2];
        let hidden2_bytes: Vec<u8> = hidden2_weights.iter().map(|v| *v as u8).collect();
        file.write_all(&hidden2_bytes).unwrap();
        let hidden2_biases: [i32; 1] = [3];
        file.write_all(&hidden2_biases[0].to_le_bytes()).unwrap();
        let output_weights: [i8; 1] = [5];
        file.write_all(&[output_weights[0] as u8]).unwrap();
        let output_bias: i32 = 2;
        file.write_all(&output_bias.to_le_bytes()).unwrap();

        let scales = serde_json::json!({
            "schema_version": 1,
            "format_version": "classic-v1",
            "arch": "HALFKP_2X2_2_1",
            "generated_at_utc": "2025-01-01T00:00:00Z",
            "acc_dim": 2,
            "h1_dim": 2,
            "h2_dim": 1,
            "input_dim": 4,
            "s_w0": 0.015625,
            "s_w1": [0.05, 0.05],
            "s_w2": [0.04],
            "s_w3": [0.02],
            "s_in_1": 1.0,
            "s_in_2": 0.5,
            "s_in_3": 0.25,
            "bundle_sha256": "test",
            "quant_scheme": {
                "ft": "per-tensor",
                "h1": "per-channel",
                "h2": "per-channel",
                "out": "per-tensor"
            },
            "activation": {
                "ft_max_abs": 2.5,
                "h1_max_abs": 1.5,
                "h2_max_abs": 0.75
            }
        });
        serde_json::to_writer_pretty(File::create(scales_path).unwrap(), &scales).unwrap();
    }

    fn sample_scales() -> ClassicQuantizationScalesData {
        ClassicQuantizationScalesData {
            schema_version: 1,
            format_version: "classic-v1".to_string(),
            arch: "HALFKP_2X2_2_1".to_string(),
            acc_dim: 2,
            h1_dim: 2,
            h2_dim: 1,
            input_dim: 4,
            s_w0: 0.015625,
            s_w1: vec![0.05, 0.05],
            s_w2: vec![0.04],
            s_w3: vec![0.02],
            s_in_1: 1.0,
            s_in_2: 0.5,
            s_in_3: 0.25,
            bundle_sha256: "test".to_string(),
            quant_scheme: None,
            activation: Some(ClassicActivationSummaryData {
                ft_max_abs: 2.5,
                h1_max_abs: 1.5,
                h2_max_abs: 0.75,
            }),
            calibration_metrics: None,
            eval_metrics: None,
        }
    }

    #[test]
    fn ft_quantization_matches_shift_for_pow2_scale() {
        let td = tempdir().unwrap();
        let nn_path = td.path().join("nn.classic.nnue");
        let scales_path = td.path().join("nn.classic.scales.json");
        write_int_fixture(&nn_path, &scales_path);

        let net = ClassicIntNetwork::load(&nn_path, Some(scales_path)).unwrap();
        assert_eq!(net.ft_shift, Some(DEFAULT_FT_SHIFT));

        let cases = [-32768i16, -1025, -65, -64, -1, 0, 1, 63, 64, 127, 8191];

        for &v in &cases {
            let expected =
                ((v as i32) >> DEFAULT_FT_SHIFT).clamp(QUANT_MIN_I32, QUANT_MAX_I32) as i8;
            assert_eq!(net.quantize_ft_value(v), expected, "value {v}");
        }
    }

    #[test]
    fn ft_quantization_floor_for_non_pow2_scale() {
        let td = tempdir().unwrap();
        let nn_path = td.path().join("nn.classic.nnue");
        let scales_path = td.path().join("nn.classic.scales.json");
        write_int_fixture(&nn_path, &scales_path);

        let mut net = ClassicIntNetwork::load(&nn_path, Some(scales_path)).unwrap();
        net.ft_shift = None;
        net.ft_scale = 60.0;

        let cases = [-130i16, -65, -1, 0, 1, 63, 64, 130];
        for &v in &cases {
            let expected = ((v as f32 / 60.0).floor()).clamp(QUANT_MIN_F32, QUANT_MAX_F32) as i8;
            assert_eq!(net.quantize_ft_value(v), expected, "value {v}");
        }
    }

    #[test]
    fn int_forward_matches_manual() {
        let td = tempdir().unwrap();
        let nn_path = td.path().join("nn.classic.nnue");
        let scales_path = td.path().join("nn.classic.scales.json");
        write_int_fixture(&nn_path, &scales_path);

        let net = ClassicIntNetwork::load(&nn_path, Some(scales_path)).unwrap();
        let features_us = vec![0usize, 1];
        let features_them = vec![2usize, 3];
        let outputs = net.forward(&features_us, &features_them);

        assert_eq!(outputs.h1.len(), 2);
        assert_eq!(outputs.h2.len(), 1);
        let actual = outputs.output;
        assert!((actual - 0.985).abs() < 1e-3, "expected ~0.985, got {actual}");
    }

    #[test]
    fn classic_v1_write_read_roundtrip() {
        let td = tempdir().unwrap();
        let nn_path = td.path().join("nn.classic.nnue");
        let scales_path = td.path().join("nn.classic.scales.json");

        let ft_weights: Vec<i16> = vec![32, 16, 20, 8, 12, 24, 10, 6];
        let ft_biases: Vec<i32> = vec![64, 32];
        let hidden1_weights: Vec<i8> = vec![3, -2, 1, 4, 2, 1, -3, 1];
        let hidden1_biases: Vec<i32> = vec![5, -4];
        let hidden2_weights: Vec<i8> = vec![4, -2];
        let hidden2_biases: Vec<i32> = vec![3];
        let output_weights: Vec<i8> = vec![5];
        let output_bias: i32 = 2;

        {
            use std::io::Write;
            let mut file = File::create(&nn_path).unwrap();
            let payload = ft_weights.len() * 2
                + ft_biases.len() * 4
                + hidden1_weights.len()
                + hidden1_biases.len() * 4
                + hidden2_weights.len()
                + hidden2_biases.len() * 4
                + output_weights.len()
                + 4; // output bias
            let total_size = 16 + payload;
            file.write_all(b"NNUE").unwrap();
            file.write_all(&1u32.to_le_bytes()).unwrap();
            file.write_all(&CLASSIC_V1_ARCH_ID.to_le_bytes()).unwrap();
            file.write_all(&(total_size as u32).to_le_bytes()).unwrap();

            for &w in &ft_weights {
                file.write_all(&w.to_le_bytes()).unwrap();
            }
            for &b in &ft_biases {
                file.write_all(&b.to_le_bytes()).unwrap();
            }
            file.write_all(&hidden1_weights.iter().map(|&v| v as u8).collect::<Vec<u8>>())
                .unwrap();
            for &b in &hidden1_biases {
                file.write_all(&b.to_le_bytes()).unwrap();
            }
            file.write_all(&hidden2_weights.iter().map(|&v| v as u8).collect::<Vec<u8>>())
                .unwrap();
            for &b in &hidden2_biases {
                file.write_all(&b.to_le_bytes()).unwrap();
            }
            file.write_all(&output_weights.iter().map(|&v| v as u8).collect::<Vec<u8>>())
                .unwrap();
            file.write_all(&output_bias.to_le_bytes()).unwrap();
        }

        let mut scales_value = serde_json::json!({
            "schema_version": 1,
            "format_version": "classic-v1",
            "arch": "HALFKP_2X2_2_1",
            "generated_at_utc": "2025-01-01T00:00:00Z",
            "acc_dim": 2,
            "h1_dim": 2,
            "h2_dim": 1,
            "input_dim": 4,
            "s_w0": 0.015625,
            "s_w1": [0.05, 0.05],
            "s_w2": [0.04],
            "s_w3": [0.02],
            "s_in_1": 1.0,
            "s_in_2": 0.5,
            "s_in_3": 0.25,
            "bundle_sha256": "test",
            "quant_scheme": {
                "ft": "per-tensor",
                "h1": "per-channel",
                "h2": "per-channel",
                "out": "per-tensor"
            }
        });
        scales_value["activation"] = serde_json::json!({
            "ft_max_abs": 2.5,
            "h1_max_abs": 1.5,
            "h2_max_abs": 0.75
        });
        serde_json::to_writer_pretty(File::create(&scales_path).unwrap(), &scales_value).unwrap();

        let loaded = ClassicIntNetwork::load(&nn_path, Some(scales_path)).expect("load roundtrip");

        assert_eq!(loaded.transformer_weights, ft_weights);
        assert_eq!(loaded.transformer_biases, ft_biases);
        assert_eq!(loaded.hidden1_weights, hidden1_weights);
        assert_eq!(loaded.hidden1_biases, hidden1_biases);
        assert_eq!(loaded.hidden2_weights, hidden2_weights);
        assert_eq!(loaded.hidden2_biases, hidden2_biases);
        assert_eq!(loaded.output_weights, output_weights);
        assert_eq!(loaded.output_bias, output_bias);

        let features_us: Vec<usize> = vec![0, 1];
        let features_them: Vec<usize> =
            features_us.iter().map(|&f| flip_us_them(f) as usize).collect();
        let outputs_loaded = loaded.forward(&features_us, &features_them);
        assert!(outputs_loaded.output.is_finite());
    }

    #[test]
    fn classic_v1_payload_size_matches_expectation() {
        let td = tempdir().unwrap();
        let nn_path = td.path().join("nn.classic.nnue");

        let ft_weights = vec![0i16; 8];
        let ft_biases = vec![0i32; 2];
        let hidden1_weights = vec![0i8; 8];
        let hidden1_biases = vec![0i32; 2];
        let hidden2_weights = vec![0i8; 2];
        let hidden2_biases = vec![0i32; 1];
        let output_weights = vec![0i8; 1];
        let output_bias = 0i32;

        {
            use std::io::Write;
            let mut file = File::create(&nn_path).unwrap();
            let payload = ft_weights.len() * 2
                + ft_biases.len() * 4
                + hidden1_weights.len()
                + hidden1_biases.len() * 4
                + hidden2_weights.len()
                + hidden2_biases.len() * 4
                + output_weights.len()
                + 4;
            let total_size = 16 + payload;
            file.write_all(b"NNUE").unwrap();
            file.write_all(&1u32.to_le_bytes()).unwrap();
            file.write_all(&CLASSIC_V1_ARCH_ID.to_le_bytes()).unwrap();
            file.write_all(&(total_size as u32).to_le_bytes()).unwrap();

            for &w in &ft_weights {
                file.write_all(&w.to_le_bytes()).unwrap();
            }
            for &b in &ft_biases {
                file.write_all(&b.to_le_bytes()).unwrap();
            }
            file.write_all(&hidden1_weights.iter().map(|&v| v as u8).collect::<Vec<u8>>())
                .unwrap();
            for &b in &hidden1_biases {
                file.write_all(&b.to_le_bytes()).unwrap();
            }
            file.write_all(&hidden2_weights.iter().map(|&v| v as u8).collect::<Vec<u8>>())
                .unwrap();
            for &b in &hidden2_biases {
                file.write_all(&b.to_le_bytes()).unwrap();
            }
            file.write_all(&output_weights.iter().map(|&v| v as u8).collect::<Vec<u8>>())
                .unwrap();
            file.write_all(&output_bias.to_le_bytes()).unwrap();
        }

        let file_size = std::fs::metadata(&nn_path).unwrap().len();
        let expected_payload = ft_weights.len() * 2
            + ft_biases.len() * 4
            + hidden1_weights.len()
            + hidden1_biases.len() * 4
            + hidden2_weights.len()
            + hidden2_biases.len() * 4
            + output_weights.len()
            + 4;
        assert_eq!(file_size, (expected_payload + 16) as u64);
    }

    #[test]
    fn int_load_skips_nonzero_padding_without_offset() {
        use std::fs::File;
        use std::io::Write;

        let td = tempdir().unwrap();
        let nn_path = td.path().join("nn.classic.nnue");
        let scales_path = td.path().join("nn.classic.scales.json");
        write_int_fixture(&nn_path, &scales_path);

        let ft_weights: [i16; 8] = [32, 16, 20, 8, 12, 24, 10, 6];
        let ft_biases: [i32; 2] = [64, 32];
        let hidden1_weights: [i8; 8] = [3, -2, 1, 4, 2, 1, -3, 1];
        let hidden1_biases: [i32; 2] = [5, -4];
        let hidden2_weights: [i8; 2] = [4, -2];
        let hidden2_biases: [i32; 1] = [3];
        let output_weights: [i8; 1] = [5];
        let output_bias: i32 = 2;

        let mut payload = Vec::new();
        for v in ft_weights.iter() {
            payload.extend_from_slice(&v.to_le_bytes());
        }
        let canonical_input_dim = SHOGI_BOARD_SIZE * FE_END;
        let pad_elements = canonical_input_dim
            .saturating_sub(4)
            .saturating_mul(2) // acc_dim
            .saturating_mul(std::mem::size_of::<i16>());
        let mut pad = vec![0u8; pad_elements];
        if !pad.is_empty() {
            let mid = pad.len() / 2;
            pad[mid] = 0xAA;
        }
        payload.extend_from_slice(&pad);
        for v in ft_biases.iter() {
            payload.extend_from_slice(&v.to_le_bytes());
        }
        payload.extend(hidden1_weights.iter().map(|v| *v as u8));
        for v in hidden1_biases.iter() {
            payload.extend_from_slice(&v.to_le_bytes());
        }
        payload.extend(hidden2_weights.iter().map(|v| *v as u8));
        payload.extend_from_slice(&hidden2_biases[0].to_le_bytes());
        payload.extend_from_slice(&(output_weights[0] as u8).to_le_bytes());
        payload.extend_from_slice(&output_bias.to_le_bytes());

        let total_size = 16 + payload.len();
        let mut file = File::create(&nn_path).unwrap();
        file.write_all(b"NNUE").unwrap();
        file.write_all(&1u32.to_le_bytes()).unwrap();
        file.write_all(&CLASSIC_V1_ARCH_ID.to_le_bytes()).unwrap();
        file.write_all(&(total_size as u32).to_le_bytes()).unwrap();
        file.write_all(&payload).unwrap();

        drop(file);

        let net = ClassicIntNetwork::load(&nn_path, Some(scales_path)).unwrap();

        assert_eq!(net.transformer_biases, vec![64, 32]);
        assert_eq!(net.hidden1_biases, vec![5, -4]);
        assert_eq!(net.hidden2_biases, vec![3]);
        assert_eq!(net.output_bias, 2);
    }

    #[test]
    fn load_fails_for_invalid_scale_values() {
        let td = tempdir().unwrap();
        let nn_path = td.path().join("nn.classic.nnue");
        let scales_path = td.path().join("nn.classic.scales.json");
        write_int_fixture(&nn_path, &scales_path);

        let original: serde_json::Value =
            serde_json::from_reader(File::open(&scales_path).unwrap()).unwrap();

        let mut broken = original.clone();
        broken["s_in_2"] = serde_json::json!(-0.5);
        serde_json::to_writer_pretty(File::create(&scales_path).unwrap(), &broken).unwrap();

        let err = ClassicIntNetwork::load(&nn_path, Some(scales_path.clone())).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("s_in_2 must be finite and > 0"), "unexpected err: {msg}");

        let mut broken = original.clone();
        broken["s_w0"] = serde_json::json!(-0.125);
        serde_json::to_writer_pretty(File::create(&scales_path).unwrap(), &broken).unwrap();

        let err = ClassicIntNetwork::load(&nn_path, Some(scales_path.clone())).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("s_w0 must be finite and > 0"), "unexpected err: {msg}");

        let mut broken = original.clone();
        broken["s_w1"][0] = serde_json::json!(0.0);
        serde_json::to_writer_pretty(File::create(&scales_path).unwrap(), &broken).unwrap();

        let err = ClassicIntNetwork::load(&nn_path, Some(scales_path.clone())).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("s_w1[0] must be finite and > 0"), "unexpected err: {msg}");

        let mut broken = original.clone();
        broken["s_w2"][0] = serde_json::json!(-0.25);
        serde_json::to_writer_pretty(File::create(&scales_path).unwrap(), &broken).unwrap();

        let err = ClassicIntNetwork::load(&nn_path, Some(scales_path.clone())).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("s_w2[0] must be finite and > 0"), "unexpected err: {msg}");

        let mut broken = original.clone();
        broken["s_w3"][0] = serde_json::json!(0.0);
        serde_json::to_writer_pretty(File::create(&scales_path).unwrap(), &broken).unwrap();

        let err = ClassicIntNetwork::load(&nn_path, Some(scales_path.clone())).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("s_w3[0] must be finite and > 0"), "unexpected err: {msg}");
    }

    #[test]
    fn load_accepts_unknown_scale_fields() {
        let td = tempdir().unwrap();
        let nn_path = td.path().join("nn.classic.nnue");
        let scales_path = td.path().join("nn.classic.scales.json");
        write_int_fixture(&nn_path, &scales_path);

        let mut value: serde_json::Value =
            serde_json::from_reader(File::open(&scales_path).unwrap()).unwrap();
        value["future_field"] = serde_json::json!({"foo": "bar", "version": 42});
        value["quant_scheme"]["experimental"] = serde_json::json!("per-block");
        serde_json::to_writer_pretty(File::create(&scales_path).unwrap(), &value).unwrap();

        let net = ClassicIntNetwork::load(&nn_path, Some(scales_path.clone())).unwrap();
        assert_eq!(net.scales.bundle_sha256, "test");
    }

    #[test]
    fn load_fails_when_required_scale_field_missing() {
        let td = tempdir().unwrap();
        let nn_path = td.path().join("nn.classic.nnue");
        let scales_path = td.path().join("nn.classic.scales.json");
        write_int_fixture(&nn_path, &scales_path);

        let mut value: serde_json::Value =
            serde_json::from_reader(File::open(&scales_path).unwrap()).unwrap();
        value.as_object_mut().unwrap().remove("s_w0");
        serde_json::to_writer_pretty(File::create(&scales_path).unwrap(), &value).unwrap();

        let err = ClassicIntNetwork::load(&nn_path, Some(scales_path)).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("s_w0") || msg.contains("failed to parse scales JSON"),
            "unexpected message: {msg}"
        );
    }

    #[test]
    fn load_fails_when_input_dim_exceeds_canonical() {
        let td = tempdir().unwrap();
        let nn_path = td.path().join("nn.classic.nnue");
        let scales_path = td.path().join("nn.classic.scales.json");
        write_int_fixture(&nn_path, &scales_path);

        let mut value: serde_json::Value =
            serde_json::from_reader(File::open(&scales_path).unwrap()).unwrap();
        let canonical = (SHOGI_BOARD_SIZE * FE_END + 1) as usize;
        value["input_dim"] = serde_json::json!(canonical);
        serde_json::to_writer_pretty(File::create(&scales_path).unwrap(), &value).unwrap();

        let err = ClassicIntNetwork::load(&nn_path, Some(scales_path)).unwrap_err();
        assert!(err.to_string().contains("exceeds canonical"));
    }

    #[test]
    fn load_fails_for_mismatched_quant_scheme() {
        let td = tempdir().unwrap();
        let nn_path = td.path().join("nn.classic.nnue");
        let scales_path = td.path().join("nn.classic.scales.json");
        write_int_fixture(&nn_path, &scales_path);

        let original: serde_json::Value =
            serde_json::from_reader(File::open(&scales_path).unwrap()).unwrap();

        let mut broken = original.clone();
        broken["quant_scheme"]["h1"] = serde_json::json!("per-tensor");
        serde_json::to_writer_pretty(File::create(&scales_path).unwrap(), &broken).unwrap();

        let err = ClassicIntNetwork::load(&nn_path, Some(scales_path.clone())).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("quant_scheme.h1"), "unexpected err: {msg}");

        let mut broken = original.clone();
        broken["quant_scheme"]["out"] = serde_json::json!("per-channel");
        serde_json::to_writer_pretty(File::create(&scales_path).unwrap(), &broken).unwrap();

        let err = ClassicIntNetwork::load(&nn_path, Some(scales_path.clone())).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("quant_scheme.out"), "unexpected err: {msg}");

        let mut broken = original.clone();
        broken["quant_scheme"]["h2"] = serde_json::json!("unknown");
        serde_json::to_writer_pretty(File::create(&scales_path).unwrap(), &broken).unwrap();

        let err = ClassicIntNetwork::load(&nn_path, Some(scales_path)).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("quant_scheme.h2='unknown'"), "unexpected err: {msg}");
    }

    #[test]
    fn compute_ft_scale_falls_back_for_ratio_below_unity() {
        let mut scales = sample_scales();
        scales.s_w0 = 1.0;
        scales.s_in_1 = 0.5;

        let (scale, shift) = compute_ft_scale(&scales);
        assert!(shift.is_none());
        assert!((scale - 0.5).abs() < 1e-6);
    }

    #[test]
    fn compute_ft_scale_respects_relative_error_threshold() {
        let mut scales = sample_scales();
        scales.s_w0 = 1.0;

        scales.s_in_1 = 2.0 * (1.0 + 9e-5);
        let (pow2_scale, pow2_shift) = compute_ft_scale(&scales);
        assert_eq!(pow2_shift, Some(1));
        assert!((pow2_scale - 2.0).abs() < 1e-6);

        scales.s_in_1 = 2.0 * (1.0 + 1.2e-4);
        let (float_scale, float_shift) = compute_ft_scale(&scales);
        assert!(float_shift.is_none());
        let expected = scales.s_in_1 / scales.s_w0;
        assert!((float_scale - expected).abs() < 1e-6);
    }

    #[test]
    fn compute_ft_scale_clamps_large_shift_to_30() {
        let mut scales = sample_scales();
        scales.s_w0 = 1.0;
        scales.s_in_1 = 2f32.powi(40);

        let (scale, shift) = compute_ft_scale(&scales);
        assert_eq!(shift, Some(30));
        assert!((scale - 2f32.powi(30)).abs() < 1e-3);
    }

    #[test]
    fn default_shift_matches_scale() {
        let diff = 2f32.powi(DEFAULT_FT_SHIFT) - DEFAULT_FT_SCALE;
        assert!(diff.abs() < 1e-6);
    }
}
