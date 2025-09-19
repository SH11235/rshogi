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
        let mut acc_us = self.ft_biases.clone();
        accumulate_fp32(&mut acc_us, features_us, self.acc_dim, self.input_dim, &self.ft_weights);

        let mut acc_them = self.ft_biases.clone();
        accumulate_fp32(
            &mut acc_them,
            features_them,
            self.acc_dim,
            self.input_dim,
            &self.ft_weights,
        );

        let mut input = Vec::with_capacity(self.acc_dim * 2);
        input.extend(acc_us.iter().copied());
        input.extend(acc_them.iter().copied());

        let mut h1 = Vec::with_capacity(self.h1_dim);
        let mut h1_act = Vec::with_capacity(self.h1_dim);
        let input_dim_h1 = self.acc_dim * 2;
        for i in 0..self.h1_dim {
            let mut sum = self.hidden1_biases[i];
            let weights = &self.hidden1_weights[i * input_dim_h1..(i + 1) * input_dim_h1];
            for (x, w) in input.iter().zip(weights.iter()) {
                sum += x * w;
            }
            let activated = sum.max(0.0).min(self.relu_clip);
            h1.push(sum);
            h1_act.push(activated);
        }

        let mut h2 = Vec::with_capacity(self.h2_dim);
        let mut h2_act = Vec::with_capacity(self.h2_dim);
        for i in 0..self.h2_dim {
            let mut sum = self.hidden2_biases[i];
            let weights = &self.hidden2_weights[i * self.h1_dim..(i + 1) * self.h1_dim];
            for (x, w) in h1_act.iter().zip(weights.iter()) {
                sum += x * w;
            }
            let activated = sum.max(0.0).min(self.relu_clip);
            h2.push(sum);
            h2_act.push(activated);
        }

        let mut output = self.output_bias;
        for (w, x) in self.output_weights.iter().zip(h2_act.iter()) {
            output += w * x;
        }

        let mut ft_combined = Vec::with_capacity(self.acc_dim * 2);
        ft_combined.extend(acc_us);
        ft_combined.extend(acc_them);

        LayerOutputs {
            ft: ft_combined,
            h1: h1_act,
            h2: h2_act,
            output,
        }
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
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct QuantSchemeReportData {
    pub ft: Option<String>,
    pub h1: Option<String>,
    pub h2: Option<String>,
    #[serde(rename = "out", default)]
    pub out: Option<String>,
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
        let scales: ClassicQuantizationScalesData = serde_json::from_reader(scales_file)
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
        if scales.input_dim != SHOGI_BOARD_SIZE * FE_END {
            log::warn!(
                "scales input_dim {} differs from canonical {}",
                scales.input_dim,
                SHOGI_BOARD_SIZE * FE_END
            );
        }
        if scales.acc_dim != acc_dim {
            log::warn!("scales acc_dim mismatch: {} vs {}", scales.acc_dim, acc_dim);
        }

        let mut ft_weights = read_i16_vec(&mut reader, input_dim * acc_dim)?;
        let mut ft_biases = read_i32_vec(&mut reader, acc_dim)?;
        let hidden1_weights = read_i8_vec(&mut reader, acc_dim * 2 * h1_dim)?;
        let hidden1_biases = read_i32_vec(&mut reader, h1_dim)?;
        let hidden2_weights = read_i8_vec(&mut reader, h1_dim * h2_dim)?;
        let hidden2_biases = read_i32_vec(&mut reader, h2_dim)?;
        let output_weights = read_i8_vec(&mut reader, h2_dim)?;
        let output_bias = read_i32(&mut reader)?;

        validate_scale_lengths(&scales, h1_dim, h2_dim)?;
        validate_scale_values(&scales)?;

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
            let act_i8 = sum.clamp(0, 127) as i8;
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
            let act_i8 = sum.clamp(0, 127) as i8;
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
            let shifted = (value as i32) >> shift;
            return shifted.clamp(-127, 127) as i8;
        }

        let scale = if self.ft_scale <= 0.0 {
            DEFAULT_FT_SCALE
        } else {
            self.ft_scale
        };

        let scaled = (value as f32 / scale).floor();
        scaled.clamp(-127.0, 127.0) as i8
    }
}

fn validate_scale_lengths(
    scales: &ClassicQuantizationScalesData,
    h1_dim: usize,
    h2_dim: usize,
) -> Result<()> {
    fn check(name: &str, values: &[f32], expected: usize) -> Result<()> {
        if values.is_empty() {
            bail!("{name} scale vector is empty (expected length 1 or {expected})");
        }
        if values.len() == 1 || values.len() == expected {
            Ok(())
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

fn compute_ft_scale(scales: &ClassicQuantizationScalesData) -> (f32, Option<i32>) {
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
            let shift = raw_shift.clamp(0, 30);
            if raw_shift != shift {
                log::warn!(
                    "ft shift {} out of range; clamped to {}",
                    raw_shift,
                    shift
                );
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
    if scales.is_empty() {
        1.0
    } else if scales.len() == 1 {
        scales[0]
    } else if idx < scales.len() {
        scales[idx]
    } else {
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
            }
        });
        serde_json::to_writer_pretty(File::create(scales_path).unwrap(), &scales).unwrap();
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
            let expected = ((v as i32) >> DEFAULT_FT_SHIFT).clamp(-127, 127) as i8;
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
            let expected = ((v as f32 / 60.0).floor()).clamp(-127.0, 127.0) as i8;
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
    }

    #[test]
    fn default_shift_matches_scale() {
        let diff = 2f32.powi(DEFAULT_FT_SHIFT) - DEFAULT_FT_SCALE;
        assert!(diff.abs() < 1e-6);
    }
}
