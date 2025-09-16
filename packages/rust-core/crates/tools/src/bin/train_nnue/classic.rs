use crate::params::{
    CLASSIC_V1_ARCH_ID, I16_QMAX, I8_QMAX, QUANTIZATION_MAX, QUANTIZATION_METADATA_SIZE,
    QUANTIZATION_MIN,
};
use std::fs::File;
use std::io::Write;
use std::path::Path;

#[allow(dead_code)]
#[inline]
pub fn round_away_from_zero(val: f32) -> i32 {
    if val.is_nan() || val.is_infinite() {
        return 0;
    }
    if val >= 0.0 {
        (val + 0.5).floor() as i32
    } else {
        (val - 0.5).ceil() as i32
    }
}

#[allow(dead_code)]
#[inline]
pub fn clip_sym(value: i32, qmax: i32) -> i32 {
    value.clamp(-qmax, qmax)
}

#[allow(dead_code)]
pub fn quantize_symmetric_i8(
    weights: &[f32],
    per_channel: bool,
    channels: usize,
) -> (Vec<i8>, Vec<f32>) {
    if weights.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let mut scales = if per_channel {
        vec![0.0f32; channels]
    } else {
        vec![0.0f32; 1]
    };
    let mut quantized = Vec::with_capacity(weights.len());
    if per_channel {
        let stride = weights.len() / channels;
        for (ch, slice) in weights.chunks(stride).enumerate() {
            let max_abs = slice.iter().fold(0.0f32, |m, &v| m.max(v.abs())).max(1e-12);
            let scale = max_abs / I8_QMAX as f32;
            scales[ch] = scale;
            for &w in slice {
                let q = round_away_from_zero(w / scale);
                quantized.push(clip_sym(q, I8_QMAX) as i8);
            }
        }
    } else {
        let max_abs = weights.iter().fold(0.0f32, |m, &v| m.max(v.abs())).max(1e-12);
        let scale = max_abs / I8_QMAX as f32;
        scales[0] = scale;
        for &w in weights {
            let q = round_away_from_zero(w / scale);
            quantized.push(clip_sym(q, I8_QMAX) as i8);
        }
    }
    (quantized, scales)
}

#[allow(dead_code)]
pub fn quantize_symmetric_i16(
    weights: &[f32],
    per_channel: bool,
    channels: usize,
) -> (Vec<i16>, Vec<f32>) {
    if weights.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let mut scales = if per_channel {
        vec![0.0f32; channels]
    } else {
        vec![0.0f32; 1]
    };
    let mut quantized = Vec::with_capacity(weights.len());
    if per_channel {
        let stride = weights.len() / channels;
        for (ch, slice) in weights.chunks(stride).enumerate() {
            let max_abs = slice.iter().fold(0.0f32, |m, &v| m.max(v.abs())).max(1e-12);
            let scale = max_abs / I16_QMAX as f32;
            scales[ch] = scale;
            for &w in slice {
                let q = round_away_from_zero(w / scale);
                quantized.push(clip_sym(q, I16_QMAX) as i16);
            }
        }
    } else {
        let max_abs = weights.iter().fold(0.0f32, |m, &v| m.max(v.abs())).max(1e-12);
        let scale = max_abs / I16_QMAX as f32;
        scales[0] = scale;
        for &w in weights {
            let q = round_away_from_zero(w / scale);
            quantized.push(clip_sym(q, I16_QMAX) as i16);
        }
    }
    (quantized, scales)
}

#[allow(dead_code)]
pub fn quantize_bias_i32(bias: &[f32], input_scale: f32, weight_scales: &[f32]) -> Vec<i32> {
    if weight_scales.is_empty() {
        return vec![0; bias.len()];
    }
    bias.iter()
        .zip(weight_scales.iter().cycle())
        .map(|(&b, &s_w)| {
            let denom = (input_scale * s_w).max(1e-20);
            round_away_from_zero(b / denom)
        })
        .collect()
}

#[allow(dead_code)]
#[inline]
pub fn clamp_i32_to_i16(v: i32) -> i16 {
    v.clamp(-I16_QMAX, I16_QMAX) as i16
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct ClassicFeatureTransformerInt {
    pub weights: Vec<i16>,
    pub biases: Vec<i32>,
    pub acc_dim: usize,
    pub input_dim: usize,
}

#[allow(dead_code)]
impl ClassicFeatureTransformerInt {
    pub fn new(weights: Vec<i16>, biases: Vec<i32>, acc_dim: usize) -> Self {
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

    pub fn accumulate_u32(&self, features: &[u32], out: &mut [i16]) {
        let features_usize: Vec<usize> = features.iter().map(|&f| f as usize).collect();
        self.accumulate(&features_usize, out);
    }

    pub fn accumulate(&self, features: &[usize], out: &mut [i16]) {
        debug_assert_eq!(out.len(), self.acc_dim);
        out.iter_mut()
            .zip(self.biases.iter())
            .for_each(|(dst, &b)| *dst = clamp_i32_to_i16(b));

        for &feat in features {
            if feat >= self.input_dim {
                continue;
            }
            let base = feat * self.acc_dim;
            let row = &self.weights[base..base + self.acc_dim];
            for (dst, &w) in out.iter_mut().zip(row.iter()) {
                let sum = *dst as i32 + w as i32;
                *dst = clamp_i32_to_i16(sum);
            }
        }
    }
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
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

#[allow(dead_code)]
impl ClassicQuantizedNetwork {
    pub fn new(p: ClassicQuantizedNetworkParams) -> Self {
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

    pub fn propagate_from_acc(&self, acc_us: &[i16], acc_them: &[i16]) -> i32 {
        debug_assert_eq!(acc_us.len(), self.acc_dim);
        debug_assert_eq!(acc_them.len(), self.acc_dim);

        let input_dim = self.acc_dim * 2;
        let mut input = vec![0i8; input_dim];
        for (i, (&us, &them)) in acc_us.iter().zip(acc_them.iter()).enumerate() {
            if i >= self.acc_dim { break; }
            input[i] = Self::quantize_ft_output(us);
            input[self.acc_dim + i] = Self::quantize_ft_output(them);
        }

        let mut h1 = vec![0i32; self.h1_dim];
        let mut h1_act = vec![0i8; self.h1_dim];
        self.affine_layer(
            &input,
            &self.hidden1_weights,
            &self.hidden1_biases,
            input_dim,
            self.h1_dim,
            &mut h1,
        );
        Self::apply_clipped_relu(&h1, &mut h1_act);

        let mut h2 = vec![0i32; self.h2_dim];
        let mut h2_act = vec![0i8; self.h2_dim];
        self.affine_layer(
            &h1_act,
            &self.hidden2_weights,
            &self.hidden2_biases,
            self.h1_dim,
            self.h2_dim,
            &mut h2,
        );
        Self::apply_clipped_relu(&h2, &mut h2_act);

        let mut output = self.output_bias;
        for (i, &w) in self.output_weights.iter().enumerate() {
            if i >= self.h2_dim {
                break;
            }
            output += w as i32 * h2_act[i] as i32;
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
        for (dst, &src) in output.iter_mut().zip(input.iter()) {
            let clipped = src.clamp(0, I8_QMAX);
            *dst = clipped as i8;
        }
    }

    #[inline]
    fn quantize_ft_output(v: i16) -> i8 {
        let shifted = (v as i32) >> 6;
        shifted.clamp(-I8_QMAX, I8_QMAX) as i8
    }
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct ClassicIntNetworkBundle {
    pub transformer: ClassicFeatureTransformerInt,
    pub network: ClassicQuantizedNetwork,
}

#[allow(dead_code)]
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

    pub fn propagate_with_features(&self, features_us: &[u32], features_them: &[u32]) -> i32 {
        let mut acc_us = vec![0i16; self.transformer.acc_dim];
        let mut acc_them = vec![0i16; self.transformer.acc_dim];
        self.transformer.accumulate_u32(features_us, &mut acc_us);
        self.transformer.accumulate_u32(features_them, &mut acc_them);
        self.network.propagate_from_acc(&acc_us, &acc_them)
    }
}

#[allow(dead_code)]
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
    pub fn validate(&self) -> Result<(), String> {
        if self.acc_dim == 0 || self.h1_dim == 0 || self.h2_dim == 0 {
            return Err("dimensions must be non-zero".into());
        }
        if self.ft_weights.len() != self.input_dim * self.acc_dim {
            return Err("ft_weights length mismatch".into());
        }
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

    pub fn total_bytes(&self) -> u64 {
        let mut total = 16u64;
        total += (self.ft_weights.len() * 2) as u64;
        total += (self.ft_biases.len() * 4) as u64;
        total += self.hidden1_weights.len() as u64;
        total += (self.hidden1_biases.len() * 4) as u64;
        total += self.hidden2_weights.len() as u64;
        total += (self.hidden2_biases.len() * 4) as u64;
        total += self.output_weights.len() as u64;
        total += 4;
        total
    }
}

#[allow(dead_code)]
pub fn write_classic_v1_file(
    path: &Path,
    data: &ClassicV1Serialized<'_>,
) -> Result<(), Box<dyn std::error::Error>> {
    data.validate().map_err(|msg| msg.to_string())?;

    let total_bytes = data.total_bytes();
    if total_bytes > u32::MAX as u64 {
        return Err("Classic v1 blob exceeds 4GB".into());
    }

    let mut writer = std::io::BufWriter::new(File::create(path)?);
    writer.write_all(b"NNUE")?;
    writer.write_all(&1u32.to_le_bytes())?;
    writer.write_all(&CLASSIC_V1_ARCH_ID.to_le_bytes())?;
    writer.write_all(&(total_bytes as u32).to_le_bytes())?;

    for &w in data.ft_weights {
        writer.write_all(&w.to_le_bytes())?;
    }
    for &b in data.ft_biases {
        writer.write_all(&b.to_le_bytes())?;
    }
    for &w in data.hidden1_weights {
        writer.write_all(&[w as u8])?;
    }
    for &b in data.hidden1_biases {
        writer.write_all(&b.to_le_bytes())?;
    }
    for &w in data.hidden2_weights {
        writer.write_all(&[w as u8])?;
    }
    for &b in data.hidden2_biases {
        writer.write_all(&b.to_le_bytes())?;
    }
    for &w in data.output_weights {
        writer.write_all(&[w as u8])?;
    }
    writer.write_all(&data.output_bias.to_le_bytes())?;
    writer.flush()?;
    Ok(())
}

#[allow(dead_code)]
pub fn write_classic_v1_bundle(
    path: &Path,
    bundle: &ClassicIntNetworkBundle,
) -> Result<(), Box<dyn std::error::Error>> {
    let serialized = bundle.as_serialized();
    write_classic_v1_file(path, &serialized)
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
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

#[allow(dead_code)]
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

    pub fn accumulate_ft(&mut self, base: usize, values: &[f32]) {
        for (dst, &src) in self.ft_weights[base..base + self.acc_dim].iter_mut().zip(values.iter())
        {
            *dst += src;
        }
    }

    pub fn accumulate_ft_bias(&mut self, delta: &[f32]) {
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
        self.validate()?;

        let ft_weights = self
            .ft_weights
            .iter()
            .map(|&w| clamp_i32_to_i16(round_away_from_zero(w)))
            .collect::<Vec<_>>();
        let ft_biases = self.ft_biases.iter().map(|&b| round_away_from_zero(b)).collect::<Vec<_>>();

        let hidden1_weights = self
            .hidden1_weights
            .iter()
            .map(|&w| clip_sym(round_away_from_zero(w), I8_QMAX) as i8)
            .collect::<Vec<_>>();
        let hidden1_biases =
            self.hidden1_biases.iter().map(|&b| round_away_from_zero(b)).collect::<Vec<_>>();

        let hidden2_weights = self
            .hidden2_weights
            .iter()
            .map(|&w| clip_sym(round_away_from_zero(w), I8_QMAX) as i8)
            .collect::<Vec<_>>();
        let hidden2_biases =
            self.hidden2_biases.iter().map(|&b| round_away_from_zero(b)).collect::<Vec<_>>();

        let output_weights = self
            .output_weights
            .iter()
            .map(|&w| clip_sym(round_away_from_zero(w), I8_QMAX) as i8)
            .collect::<Vec<_>>();
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
#[allow(dead_code)]
pub struct QuantizationParams {
    pub scale: f32,
    pub zero_point: i32,
}

#[allow(dead_code)]
impl QuantizationParams {
    pub fn from_weights(weights: &[f32]) -> Self {
        let min_val = weights.iter().fold(f32::INFINITY, |a, &b| a.min(b));
        let max_val = weights.iter().fold(f32::NEG_INFINITY, |a, &b| a.max(b));

        let range = (max_val - min_val).max(1e-8);
        let scale = range / 255.0;
        let zero_point =
            (-min_val / scale - 128.0).round().clamp(QUANTIZATION_MIN, QUANTIZATION_MAX) as i32;

        Self { scale, zero_point }
    }

    pub fn metadata_size() -> usize {
        QUANTIZATION_METADATA_SIZE
    }
}
