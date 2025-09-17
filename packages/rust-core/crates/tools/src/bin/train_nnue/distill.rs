use std::time::Instant;

use crate::classic::{ClassicFloatNetwork, ClassicIntNetworkBundle, ClassicQuantizationScales};
use crate::logging::StructuredLogger;
use crate::model::Network;
use crate::params::{CLASSIC_ACC_DIM, CLASSIC_H1_DIM, CLASSIC_H2_DIM};
use crate::types::{Config, DistillLossKind, DistillOptions, QuantScheme, Sample};
use engine_core::evaluation::nnue::features::flip_us_them;
use engine_core::evaluation::nnue::features::FE_END;
use engine_core::shogi::SHOGI_BOARD_SIZE;

const CLASSIC_RELU_CLIP: f32 = 127.0;
const MAX_DISTILL_SAMPLES: usize = 50_000;
const DISTILL_EPOCHS: usize = 2;
const DISTILL_LR: f32 = 1e-4;
const PROB_EPS: f32 = 1e-6;

struct ClassicScratch {
    acc_us: Vec<f32>,
    acc_them: Vec<f32>,
    input: Vec<f32>,
    z1: Vec<f32>,
    a1: Vec<f32>,
    z2: Vec<f32>,
    a2: Vec<f32>,
    d_a2: Vec<f32>,
    d_z2: Vec<f32>,
    d_a1: Vec<f32>,
    d_z1: Vec<f32>,
    d_input: Vec<f32>,
}

impl ClassicScratch {
    fn new(acc_dim: usize, h1_dim: usize, h2_dim: usize) -> Self {
        Self {
            acc_us: vec![0.0; acc_dim],
            acc_them: vec![0.0; acc_dim],
            input: vec![0.0; acc_dim * 2],
            z1: vec![0.0; h1_dim],
            a1: vec![0.0; h1_dim],
            z2: vec![0.0; h2_dim],
            a2: vec![0.0; h2_dim],
            d_a2: vec![0.0; h2_dim],
            d_z2: vec![0.0; h2_dim],
            d_a1: vec![0.0; h1_dim],
            d_z1: vec![0.0; h1_dim],
            d_input: vec![0.0; acc_dim * 2],
        }
    }
}

struct DistillSample {
    features_us: Vec<u32>,
    features_them: Vec<u32>,
    teacher_output: f32,
    label: f32,
    weight: f32,
}

#[inline]
fn relu_clip(x: f32) -> f32 {
    x.clamp(0.0, CLASSIC_RELU_CLIP)
}
fn relu_clip_grad(z: f32) -> f32 {
    if z > 0.0 && z < CLASSIC_RELU_CLIP {
        1.0
    } else {
        0.0
    }
}

#[inline]
fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

#[inline]
fn stable_logit(p: f32) -> f32 {
    let clamped = p.clamp(PROB_EPS, 1.0 - PROB_EPS);
    (clamped / (1.0 - clamped)).ln()
}

#[inline]
fn bce_with_logits_soft(logit: f32, target: f32) -> (f32, f32) {
    // target は [0,1] の任意値（ソフトラベル）
    let max_val = 0.0f32.max(logit);
    let loss = max_val - logit * target + ((-logit.abs()).exp() + 1.0).ln();
    let grad = sigmoid(logit) - target;
    (loss, grad)
}

fn forward(net: &ClassicFloatNetwork, sample: &DistillSample, scratch: &mut ClassicScratch) -> f32 {
    scratch.acc_us.copy_from_slice(&net.ft_biases);
    for &feat in &sample.features_us {
        let idx = feat as usize * net.acc_dim;
        let row = &net.ft_weights[idx..idx + net.acc_dim];
        for (dst, &w) in scratch.acc_us.iter_mut().zip(row.iter()) {
            *dst += w;
        }
    }

    scratch.acc_them.copy_from_slice(&net.ft_biases);
    for &feat in &sample.features_them {
        let idx = feat as usize * net.acc_dim;
        let row = &net.ft_weights[idx..idx + net.acc_dim];
        for (dst, &w) in scratch.acc_them.iter_mut().zip(row.iter()) {
            *dst += w;
        }
    }

    // Convert to input vector (us || them)
    scratch.input[..net.acc_dim].copy_from_slice(&scratch.acc_us);
    scratch.input[net.acc_dim..].copy_from_slice(&scratch.acc_them);

    // Hidden1
    let in_dim = net.acc_dim * 2;
    for i in 0..net.h1_dim {
        let row = &net.hidden1_weights[i * in_dim..(i + 1) * in_dim];
        let mut sum = net.hidden1_biases[i];
        for (w, &x) in row.iter().zip(scratch.input.iter()) {
            sum += w * x;
        }
        scratch.z1[i] = sum;
        scratch.a1[i] = relu_clip(sum);
    }

    // Hidden2
    for i in 0..net.h2_dim {
        let row = &net.hidden2_weights[i * net.h1_dim..(i + 1) * net.h1_dim];
        let mut sum = net.hidden2_biases[i];
        for (w, &x) in row.iter().zip(scratch.a1.iter()) {
            sum += w * x;
        }
        scratch.z2[i] = sum;
        scratch.a2[i] = relu_clip(sum);
    }

    let mut out = net.output_bias;
    for (w, &x) in net.output_weights.iter().zip(scratch.a2.iter()) {
        out += w * x;
    }
    out
}

fn backward_update(
    net: &mut ClassicFloatNetwork,
    scratch: &mut ClassicScratch,
    sample: &DistillSample,
    grad_output: f32,
    lr: f32,
) {
    let h1_dim = net.h1_dim;
    let h2_dim = net.h2_dim;
    let acc_dim = net.acc_dim;
    let input_dim = acc_dim * 2;

    // Output layer gradients
    for (i, grad_a2) in scratch.d_a2.iter_mut().enumerate().take(h2_dim) {
        *grad_a2 = grad_output * net.output_weights[i];
        scratch.d_z2[i] = *grad_a2 * relu_clip_grad(scratch.z2[i]);
    }

    // Gradient wrt output weights/bias
    for i in 0..h2_dim {
        let grad_w = grad_output * scratch.a2[i];
        net.output_weights[i] -= lr * grad_w;
    }
    net.output_bias -= lr * grad_output;

    // Propagate to hidden1 (using current weights before update)
    scratch.d_a1.fill(0.0);
    for (i, row) in net.hidden2_weights.chunks(h1_dim).enumerate().take(h2_dim) {
        let delta = scratch.d_z2[i];
        for (da1, &w) in scratch.d_a1.iter_mut().zip(row.iter()) {
            *da1 += delta * w;
        }
    }

    scratch.d_z1.fill(0.0);
    for (dz1, (&da1, &z1)) in
        scratch.d_z1.iter_mut().zip(scratch.d_a1.iter().zip(scratch.z1.iter()))
    {
        *dz1 = da1 * relu_clip_grad(z1);
    }

    // Compute d_input before mutating hidden1 weights
    scratch.d_input.fill(0.0);
    for (j, row) in net.hidden1_weights.chunks(input_dim).enumerate().take(h1_dim) {
        let delta = scratch.d_z1[j];
        for (din, &w) in scratch.d_input.iter_mut().zip(row.iter()) {
            *din += delta * w;
        }
    }

    // Update hidden2 weights/biases
    for i in 0..h2_dim {
        let delta = scratch.d_z2[i];
        let row = &mut net.hidden2_weights[i * h1_dim..(i + 1) * h1_dim];
        for (w, &a1) in row.iter_mut().zip(scratch.a1.iter()) {
            *w -= lr * delta * a1;
        }
        net.hidden2_biases[i] -= lr * delta;
    }

    // Update hidden1 weights/biases
    for j in 0..h1_dim {
        let delta = scratch.d_z1[j];
        let row = &mut net.hidden1_weights[j * input_dim..(j + 1) * input_dim];
        for (w, &inp) in row.iter_mut().zip(scratch.input.iter()) {
            *w -= lr * delta * inp;
        }
        net.hidden1_biases[j] -= lr * delta;
    }

    // Split input gradients for accumulator channels
    let (d_acc_us, d_acc_them) = scratch.d_input.split_at(acc_dim);

    // Update FT biases (shared)
    for i in 0..acc_dim {
        let grad = d_acc_us[i] + d_acc_them[i];
        net.ft_biases[i] -= lr * grad;
    }

    // Update FT weights for active features
    for &feat in &sample.features_us {
        let base = feat as usize * acc_dim;
        let row = &mut net.ft_weights[base..base + acc_dim];
        for (w, &grad) in row.iter_mut().zip(d_acc_us.iter()) {
            *w -= lr * grad;
        }
    }
    for &feat in &sample.features_them {
        let base = feat as usize * acc_dim;
        let row = &mut net.ft_weights[base..base + acc_dim];
        for (w, &grad) in row.iter_mut().zip(d_acc_them.iter()) {
            *w -= lr * grad;
        }
    }
}

fn prepare_distill_samples(
    teacher: &Network,
    samples: &[Sample],
    max_samples: usize,
) -> Vec<DistillSample> {
    let mut result = Vec::new();
    let mut acc_buf = vec![0.0f32; teacher.acc_dim];
    let mut act_buf = vec![0.0f32; teacher.acc_dim];

    for sample in samples.iter().filter(|s| s.weight > 0.0) {
        if result.len() >= max_samples {
            break;
        }
        let teacher_out =
            teacher.forward_with_buffers(&sample.features, &mut acc_buf, &mut act_buf);
        let features_them: Vec<u32> =
            sample.features.iter().map(|&f| flip_us_them(f as usize) as u32).collect();
        result.push(DistillSample {
            features_us: sample.features.clone(),
            features_them,
            teacher_output: teacher_out,
            label: sample.label,
            weight: sample.weight,
        });
    }

    result
}

pub struct ClassicDistillConfig<'a> {
    pub quant_ft: QuantScheme,
    pub quant_h1: QuantScheme,
    pub quant_h2: QuantScheme,
    pub quant_out: QuantScheme,
    pub structured: Option<&'a StructuredLogger>,
}

impl<'a> ClassicDistillConfig<'a> {
    pub fn new(
        quant_ft: QuantScheme,
        quant_h1: QuantScheme,
        quant_h2: QuantScheme,
        quant_out: QuantScheme,
        structured: Option<&'a StructuredLogger>,
    ) -> Self {
        Self {
            quant_ft,
            quant_h1,
            quant_h2,
            quant_out,
            structured,
        }
    }
}

pub fn distill_classic_after_training(
    teacher: &Network,
    samples: &[Sample],
    config: &Config,
    distill: &DistillOptions,
    classic_cfg: ClassicDistillConfig<'_>,
) -> Result<(ClassicIntNetworkBundle, ClassicQuantizationScales), String> {
    if samples.is_empty() {
        return Err("Classic distillation requires in-memory training samples".into());
    }
    if config.label_type != "wdl" && config.label_type != "cp" {
        return Err(format!(
            "Classic distillation unsupported for label_type={}",
            config.label_type
        ));
    }
    if distill.temperature <= 0.0 {
        return Err("--kd-temperature must be > 0".into());
    }
    if config.label_type == "cp" && distill.loss != DistillLossKind::Mse {
        return Err("Classic distillation with cp labels supports --kd-loss=mse only".into());
    }

    let start = Instant::now();
    let prepared = prepare_distill_samples(teacher, samples, MAX_DISTILL_SAMPLES);
    if prepared.is_empty() {
        return Err("No samples available for classic distillation".into());
    }

    let mut classic = ClassicFloatNetwork::zeros_with_dims(
        SHOGI_BOARD_SIZE * FE_END,
        CLASSIC_ACC_DIM,
        CLASSIC_H1_DIM,
        CLASSIC_H2_DIM,
    );

    let mut scratch = ClassicScratch::new(CLASSIC_ACC_DIM, CLASSIC_H1_DIM, CLASSIC_H2_DIM);
    let temperature = distill.temperature;
    let loss_kind = distill.loss;

    for epoch in 0..DISTILL_EPOCHS {
        let mut epoch_loss = 0.0f64;
        let mut epoch_weight = 0.0f64;
        for sample in &prepared {
            let prediction = forward(&classic, sample, &mut scratch);
            let (loss_contrib, grad_output) = match config.label_type.as_str() {
                "wdl" => {
                    let teacher_prob = sigmoid(sample.teacher_output / temperature);
                    let label_prob = sample.label.clamp(PROB_EPS, 1.0 - PROB_EPS);
                    let target_prob = (distill.alpha * teacher_prob
                        + (1.0 - distill.alpha) * label_prob)
                        .clamp(PROB_EPS, 1.0 - PROB_EPS);

                    match loss_kind {
                        DistillLossKind::Mse => {
                            let target_logit = stable_logit(target_prob);
                            let diff = prediction - target_logit;
                            let loss = 0.5 * diff * diff * sample.weight;
                            let grad = diff * sample.weight;
                            (loss as f64, grad)
                        }
                        DistillLossKind::Bce | DistillLossKind::Kl => {
                            let (loss, grad) = bce_with_logits_soft(prediction, target_prob);
                            ((loss * sample.weight) as f64, grad * sample.weight)
                        }
                    }
                }
                "cp" => {
                    let target = distill.alpha * sample.teacher_output
                        + (1.0 - distill.alpha) * sample.label;
                    let diff = prediction - target;
                    let loss = 0.5 * diff * diff * sample.weight;
                    let grad = diff * sample.weight;
                    (loss as f64, grad)
                }
                _ => unreachable!(),
            };

            epoch_loss += loss_contrib;
            epoch_weight += sample.weight as f64;
            backward_update(&mut classic, &mut scratch, sample, grad_output, DISTILL_LR);
        }
        if let Some(lg) = classic_cfg.structured {
            let loss_avg = if epoch_weight > 0.0 {
                epoch_loss / epoch_weight
            } else {
                0.0
            };
            let rec = serde_json::json!({
                "ts": chrono::Utc::now().to_rfc3339(),
                "phase": "distill_classic",
                "epoch": (epoch + 1) as i64,
                "loss": loss_avg,
                "samples": prepared.len(),
                "loss_kind": match loss_kind {
                    DistillLossKind::Mse => "mse",
                    DistillLossKind::Bce => "bce",
                    DistillLossKind::Kl => "kl",
                },
                "alpha": distill.alpha,
                "temperature": temperature,
            });
            lg.write_json(&rec);
        }
    }

    let (bundle, scales) = classic
        .quantize_symmetric(
            classic_cfg.quant_ft,
            classic_cfg.quant_h1,
            classic_cfg.quant_h2,
            classic_cfg.quant_out,
        )
        .map_err(|e| format!("quantize_symmetric failed: {e}"))?;

    if let Some(lg) = classic_cfg.structured {
        let rec = serde_json::json!({
            "ts": chrono::Utc::now().to_rfc3339(),
            "phase": "classic_quantize",
            "s_w0": scales.s_w0,
            "s_w1": scales.s_w1,
            "s_w2": scales.s_w2,
            "s_w3": scales.s_w3,
            "s_in_1": scales.s_in_1,
            "s_in_2": scales.s_in_2,
            "s_in_3": scales.s_in_3,
            "elapsed_sec": start.elapsed().as_secs_f64(),
        });
        lg.write_json(&rec);
    }

    Ok((bundle, scales))
}
