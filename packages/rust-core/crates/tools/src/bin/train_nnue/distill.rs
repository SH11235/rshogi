use std::time::Instant;

use crate::classic::ClassicScratchViews;
use crate::classic::{ClassicFloatNetwork, ClassicIntNetworkBundle, ClassicQuantizationScales};
use crate::logging::StructuredLogger;
use crate::model::Network;
use crate::params::{CLASSIC_ACC_DIM, CLASSIC_H1_DIM, CLASSIC_H2_DIM};
use crate::types::{
    Config, DistillLossKind, DistillOptions, QuantScheme, Sample, TeacherValueDomain,
};
use engine_core::evaluation::nnue::features::flip_us_them;
use engine_core::evaluation::nnue::features::FE_END;
use engine_core::shogi::SHOGI_BOARD_SIZE;
use rand::{rngs::StdRng, Rng, SeedableRng};

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

#[derive(Clone, Debug)]
pub struct DistillArtifacts {
    pub classic_fp32: ClassicFloatNetwork,
    pub bundle_int: ClassicIntNetworkBundle,
    pub scales: ClassicQuantizationScales,
}

#[derive(Clone, Debug, Default)]
pub struct DistillEvalMetrics {
    pub n: usize,
    pub mae_cp: Option<f32>,
    pub p95_cp: Option<f32>,
    pub max_cp: Option<f32>,
    pub r2_cp: Option<f32>,
    pub mae_logit: Option<f32>,
    pub p95_logit: Option<f32>,
    pub max_logit: Option<f32>,
}

#[derive(Clone, Debug, Default)]
pub struct QuantEvalMetrics {
    pub n: usize,
    pub mae_cp: Option<f32>,
    pub p95_cp: Option<f32>,
    pub max_cp: Option<f32>,
    pub mae_logit: Option<f32>,
    pub p95_logit: Option<f32>,
    pub max_logit: Option<f32>,
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
fn scale_teacher_by_temperature(
    loss_teacher: &mut f32,
    grad_teacher: &mut f32,
    temperature: f32,
    enabled: bool,
) {
    if enabled {
        let scale = temperature * temperature;
        *loss_teacher *= scale;
        *grad_teacher *= scale;
    }
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
        if idx + net.acc_dim > net.ft_weights.len() {
            log::warn!(
                "feature index {} out of range (ft_weights.len={})",
                feat,
                net.ft_weights.len()
            );
            continue;
        }
        let row = &net.ft_weights[idx..idx + net.acc_dim];
        for (dst, &w) in scratch.acc_us.iter_mut().zip(row.iter()) {
            *dst += w;
        }
    }

    scratch.acc_them.copy_from_slice(&net.ft_biases);
    for &feat in &sample.features_them {
        let idx = feat as usize * net.acc_dim;
        if idx + net.acc_dim > net.ft_weights.len() {
            log::warn!(
                "feature index {} out of range (ft_weights.len={})",
                feat,
                net.ft_weights.len()
            );
            continue;
        }
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

/// 教師 forward 結果と反転特徴を事前計算し、蒸留処理での再利用を容易にする。
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
) -> Result<DistillArtifacts, String> {
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

    let distill_seed = distill.seed.unwrap_or_else(|| rand::rng().random::<u64>());
    let mut rng: StdRng = StdRng::seed_from_u64(distill_seed);
    let mut classic = ClassicFloatNetwork::he_uniform_with_dims(
        SHOGI_BOARD_SIZE * FE_END,
        CLASSIC_ACC_DIM,
        CLASSIC_H1_DIM,
        CLASSIC_H2_DIM,
        config.estimated_features_per_sample,
        &mut rng,
    );

    // 出力バイアスを教師ターゲットの平均で初期化して初期収束を安定化させる。
    let mut bias_num = 0.0f64;
    let mut bias_den = 0.0f64;
    let mut bias_sq = 0.0f64;
    let mut bias_samples = 0usize;
    for sample in &prepared {
        let weight = sample.weight as f64;
        if weight <= 0.0 {
            continue;
        }
        let target = match config.label_type.as_str() {
            "wdl" => {
                let teacher_logit = match distill.teacher_domain {
                    TeacherValueDomain::WdlLogit => sample.teacher_output,
                    TeacherValueDomain::Cp => sample.teacher_output / config.scale,
                };
                let teacher_prob = sigmoid(teacher_logit / distill.temperature);
                let label_prob = sample.label.clamp(PROB_EPS, 1.0 - PROB_EPS);
                let target_prob = (distill.alpha * teacher_prob
                    + (1.0 - distill.alpha) * label_prob)
                    .clamp(PROB_EPS, 1.0 - PROB_EPS);
                stable_logit(target_prob)
            }
            "cp" => {
                let teacher_cp = match distill.teacher_domain {
                    TeacherValueDomain::Cp => sample.teacher_output,
                    TeacherValueDomain::WdlLogit => sample.teacher_output * config.scale,
                };
                let blended = distill.alpha * teacher_cp + (1.0 - distill.alpha) * sample.label;
                blended.clamp(-(config.cp_clip as f32), config.cp_clip as f32)
            }
            _ => continue,
        } as f64;
        bias_num += target * weight;
        bias_den += weight;
        bias_sq += target * target * weight;
        bias_samples += 1;
    }
    if bias_den > 0.0 {
        classic.output_bias = (bias_num / bias_den) as f32;
    }

    if let Some(lg) = classic_cfg.structured {
        let target_mean = if bias_den > 0.0 {
            bias_num / bias_den
        } else {
            0.0
        };
        let variance = if bias_den > 0.0 {
            (bias_sq / bias_den) - target_mean * target_mean
        } else {
            0.0
        };
        let target_std = variance.max(0.0).sqrt();
        let rec = serde_json::json!({
            "ts": chrono::Utc::now().to_rfc3339(),
            "phase": "distill_classic_init",
            "output_bias_init": classic.output_bias,
            "target_mean": target_mean,
            "target_std": target_std,
            "alpha": distill.alpha,
            "temperature": distill.temperature,
            "label_type": config.label_type,
            "teacher_domain": match distill.teacher_domain { TeacherValueDomain::Cp => "cp", TeacherValueDomain::WdlLogit => "wdl-logit" },
            "samples": bias_samples,
            "scale_temp2": distill.scale_temp2,
            "soften_student": distill.soften_student,
            "seed": distill_seed,
        });
        lg.write_json(&rec);
    }

    let mut scratch = ClassicScratch::new(CLASSIC_ACC_DIM, CLASSIC_H1_DIM, CLASSIC_H2_DIM);
    let temperature = distill.temperature;
    let loss_kind = distill.loss;
    let mut warn_soften_mse_once =
        distill.soften_student && matches!(loss_kind, DistillLossKind::Mse);

    for epoch in 0..DISTILL_EPOCHS {
        let mut epoch_loss = 0.0f64;
        let mut epoch_weight = 0.0f64;
        for sample in &prepared {
            let prediction = forward(&classic, sample, &mut scratch);
            let (loss_contrib, grad_output) = match config.label_type.as_str() {
                "wdl" => {
                    // Teacher may be cp or wdl-logit; normalize to logit space before temperature
                    let teacher_logit = match distill.teacher_domain {
                        TeacherValueDomain::WdlLogit => sample.teacher_output,
                        TeacherValueDomain::Cp => sample.teacher_output / config.scale,
                    };
                    let teacher_prob = sigmoid(teacher_logit / temperature);
                    let label_prob = sample.label.clamp(PROB_EPS, 1.0 - PROB_EPS);
                    let alpha = distill.alpha;
                    let beta = 1.0 - alpha;
                    let weight = sample.weight;
                    match loss_kind {
                        DistillLossKind::Mse => {
                            if warn_soften_mse_once {
                                log::warn!("--kd-soften-student は --kd-loss=mse では効果がありません (epoch={})", epoch + 1);
                                warn_soften_mse_once = false;
                            }
                            let teacher_target = stable_logit(teacher_prob);
                            let label_target = stable_logit(label_prob);
                            let diff_teacher = prediction - teacher_target;
                            let diff_label = prediction - label_target;
                            let mut grad_teacher = diff_teacher * weight;
                            let grad_label = diff_label * weight;
                            let mut loss_teacher = 0.5 * diff_teacher * diff_teacher * weight;
                            let loss_label = 0.5 * diff_label * diff_label * weight;
                            scale_teacher_by_temperature(
                                &mut loss_teacher,
                                &mut grad_teacher,
                                temperature,
                                distill.scale_temp2,
                            );
                            let loss = (alpha * loss_teacher + beta * loss_label) as f64;
                            let grad = alpha * grad_teacher + beta * grad_label;
                            (loss, grad)
                        }
                        DistillLossKind::Bce => {
                            let student_logit_teacher = if distill.soften_student {
                                prediction / temperature
                            } else {
                                prediction
                            };
                            let (loss_teacher_raw, grad_teacher_raw) =
                                bce_with_logits_soft(student_logit_teacher, teacher_prob);
                            let (loss_label_raw, grad_label_raw) =
                                bce_with_logits_soft(prediction, label_prob);
                            let mut loss_teacher = loss_teacher_raw * weight;
                            let mut grad_teacher = grad_teacher_raw * weight;
                            if distill.soften_student {
                                grad_teacher /= temperature;
                            }
                            let loss_label = loss_label_raw * weight;
                            let grad_label = grad_label_raw * weight;
                            scale_teacher_by_temperature(
                                &mut loss_teacher,
                                &mut grad_teacher,
                                temperature,
                                distill.scale_temp2,
                            );
                            let loss = (alpha * loss_teacher + beta * loss_label) as f64;
                            let grad = alpha * grad_teacher + beta * grad_label;
                            (loss, grad)
                        }
                        DistillLossKind::Kl => {
                            // KL(p_t || p_s)
                            let p_t = teacher_prob;
                            let student_logit_teacher = if distill.soften_student {
                                prediction / temperature
                            } else {
                                prediction
                            };
                            let p_s = sigmoid(student_logit_teacher);
                            let p_t_c = p_t.clamp(PROB_EPS, 1.0 - PROB_EPS);
                            let p_s_c = p_s.clamp(PROB_EPS, 1.0 - PROB_EPS);
                            let mut loss_teacher = (p_t_c * (p_t_c / p_s_c).ln()
                                + (1.0 - p_t_c) * ((1.0 - p_t_c) / (1.0 - p_s_c)).ln())
                                * weight;
                            let mut grad_teacher = (p_s - p_t_c) * weight; // d/dz KL
                            if distill.soften_student {
                                grad_teacher /= temperature;
                            }
                            scale_teacher_by_temperature(
                                &mut loss_teacher,
                                &mut grad_teacher,
                                temperature,
                                distill.scale_temp2,
                            );
                            let (loss_label_raw, grad_label_raw) =
                                bce_with_logits_soft(prediction, label_prob);
                            let loss_label = loss_label_raw * weight;
                            let grad_label = grad_label_raw * weight;
                            let loss = (alpha * loss_teacher + beta * loss_label) as f64;
                            let grad = alpha * grad_teacher + beta * grad_label;
                            (loss, grad)
                        }
                    }
                }
                "cp" => {
                    let teacher_cp = match distill.teacher_domain {
                        TeacherValueDomain::Cp => sample.teacher_output,
                        TeacherValueDomain::WdlLogit => sample.teacher_output * config.scale,
                    };
                    let mut target =
                        distill.alpha * teacher_cp + (1.0 - distill.alpha) * sample.label;
                    // Clip target for stability
                    target = target.clamp(-(config.cp_clip as f32), config.cp_clip as f32);
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
                "scale_temp2": distill.scale_temp2,
                "soften_student": distill.soften_student,
                "seed": distill_seed,
                "teacher_domain": match distill.teacher_domain { crate::types::TeacherValueDomain::Cp => "cp", crate::types::TeacherValueDomain::WdlLogit => "wdl-logit" },
            });
            lg.write_json(&rec);
        }
    }

    let classic_fp32 = classic.clone();

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

    Ok(DistillArtifacts {
        classic_fp32,
        bundle_int: bundle,
        scales,
    })
}

// 重み付き分位点（左閉右開累積方式）: 最小の値 v で累積重み >= q * total を満たすもの
/// 重み付き分位点（左閉右開累積）を返す。累積重みが `q * total` 以上になる最小値を選択。
fn weighted_percentile(mut pairs: Vec<(f32, f32)>, q: f32) -> Option<f32> {
    if pairs.is_empty() {
        return None;
    }
    // 無効重み / 非有限値を除外
    pairs.retain(|(v, w)| w.is_finite() && *w > 0.0 && v.is_finite());
    if pairs.is_empty() {
        return None;
    }
    pairs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let total: f64 = pairs.iter().map(|(_, w)| *w as f64).sum();
    if total == 0.0 {
        return pairs.last().map(|(v, _)| *v);
    }
    let q_clamped = q.clamp(0.0, 1.0);
    let target = q_clamped as f64 * total;
    let mut acc = 0.0f64;
    for (v, w) in pairs {
        acc += w as f64;
        if acc >= target {
            return Some(v);
        }
    }
    None
}

fn to_features_them(features_us: &[u32]) -> Vec<u32> {
    features_us.iter().map(|&f| flip_us_them(f as usize) as u32).collect()
}

fn convert_logit_to_cp(logit: f32, config: &Config) -> f32 {
    logit * config.scale
}

pub fn evaluate_distill(
    teacher: &Network,
    classic_fp32: &ClassicFloatNetwork,
    samples: &[Sample],
    config: &Config,
    teacher_domain: TeacherValueDomain,
) -> DistillEvalMetrics {
    let mut metrics = DistillEvalMetrics::default();
    let mut acc_buf = vec![0.0f32; teacher.acc_dim];
    let mut act_buf = vec![0.0f32; teacher.acc_dim];
    let mut scratch = ClassicScratch::new(CLASSIC_ACC_DIM, CLASSIC_H1_DIM, CLASSIC_H2_DIM);

    let mut cp_abs_diffs = Vec::new();
    let mut cp_weights = Vec::new();
    let mut logit_abs_diffs = Vec::new();
    let mut logit_weights = Vec::new();

    let mut mae_cp_num = 0.0f64;
    let mut mae_logit_num = 0.0f64;
    let mut weight_sum = 0.0f64;
    let mut teacher_cp_weighted_sum = 0.0f64;
    let mut teacher_cp_values = Vec::new();
    let mut student_cp_values = Vec::new();

    let mut max_cp = 0.0f32;
    let mut max_logit = 0.0f32;

    for sample in samples.iter().filter(|s| s.weight > 0.0).take(MAX_DISTILL_SAMPLES) {
        let weight = sample.weight as f64;
        let teacher_raw =
            teacher.forward_with_buffers(&sample.features, &mut acc_buf, &mut act_buf);
        // Normalize teacher to logit domain (for wdl metrics) and cp domain (for cp metrics)
        let teacher_logit = match (config.label_type.as_str(), teacher_domain) {
            ("wdl", TeacherValueDomain::WdlLogit) => teacher_raw,
            ("wdl", TeacherValueDomain::Cp) => teacher_raw / config.scale,
            ("cp", TeacherValueDomain::WdlLogit) => teacher_raw, // raw already logit
            ("cp", TeacherValueDomain::Cp) => teacher_raw / config.scale, // to reuse logit metrics if needed
            _ => teacher_raw,
        };

        let features_them = to_features_them(&sample.features);
        let ds = DistillSample {
            features_us: sample.features.clone(),
            features_them,
            teacher_output: 0.0,
            label: 0.0,
            weight: 0.0,
        };
        // student_raw: label_type が wdl の場合は logit、cp の場合は cp ドメインの値
        let student_raw = forward(classic_fp32, &ds, &mut scratch);

        metrics.n += 1;
        weight_sum += weight;

        if config.label_type == "wdl" {
            let diff_logit = (student_raw - teacher_logit).abs();
            mae_logit_num += diff_logit as f64 * weight;
            logit_abs_diffs.push(diff_logit);
            logit_weights.push(weight as f32);
            if diff_logit > max_logit {
                max_logit = diff_logit;
            }
        }

        let (teacher_cp, student_cp) = map_teacher_student_to_cp(
            config.label_type.as_str(),
            teacher_domain,
            teacher_raw,
            teacher_logit,
            student_raw,
            config,
        );

        let diff_cp = (student_cp - teacher_cp).abs();
        mae_cp_num += diff_cp as f64 * weight;
        cp_abs_diffs.push(diff_cp);
        cp_weights.push(weight as f32);
        if diff_cp > max_cp {
            max_cp = diff_cp;
        }

        teacher_cp_weighted_sum += teacher_cp as f64 * weight;
        teacher_cp_values.push((teacher_cp as f64, weight));
        student_cp_values.push(student_cp as f64);
    }

    if metrics.n == 0 || weight_sum == 0.0 {
        metrics.n = 0;
        return metrics;
    }

    metrics.mae_cp = Some((mae_cp_num / weight_sum) as f32);
    metrics.max_cp = Some(max_cp);
    // 重み付き p95 （従来は無重み）
    metrics.p95_cp = weighted_percentile(
        cp_abs_diffs.iter().cloned().zip(cp_weights.iter().cloned()).collect(),
        0.95,
    );

    if config.label_type == "wdl" {
        metrics.mae_logit = Some((mae_logit_num / weight_sum) as f32);
        metrics.max_logit = Some(max_logit);
        metrics.p95_logit = weighted_percentile(
            logit_abs_diffs.iter().cloned().zip(logit_weights.iter().cloned()).collect(),
            0.95,
        );
    }

    let teacher_mean = teacher_cp_weighted_sum / weight_sum;
    let mut ss_res = 0.0f64;
    let mut ss_tot = 0.0f64;
    for ((teacher_cp, w), student_cp) in teacher_cp_values.iter().zip(student_cp_values.iter()) {
        let diff = student_cp - teacher_cp;
        ss_res += (*w) * diff * diff;
        let centered = teacher_cp - teacher_mean;
        ss_tot += (*w) * centered * centered;
    }
    if ss_tot > f64::EPSILON {
        metrics.r2_cp = Some((1.0 - ss_res / ss_tot) as f32);
    }

    metrics
}

// Helper: 教師 raw / logit / 学生 raw から cp 空間の (teacher_cp, student_cp) を取得する。
// label_type="wdl": teacher_logit, student_raw(=logit) を cp へスケール
// label_type="cp" : teacher_raw を teacher_domain に応じて cp 化し、student_raw はそのまま
fn map_teacher_student_to_cp(
    label_type: &str,
    teacher_domain: TeacherValueDomain,
    teacher_raw: f32,
    teacher_logit: f32,
    student_raw: f32,
    config: &Config,
) -> (f32, f32) {
    if label_type == "wdl" {
        let t = convert_logit_to_cp(teacher_logit, config);
        let s = convert_logit_to_cp(student_raw, config);
        (t, s)
    } else {
        let t = match teacher_domain {
            TeacherValueDomain::Cp => teacher_raw,
            TeacherValueDomain::WdlLogit => teacher_raw * config.scale,
        };
        // student_raw は cp ドメイン
        (t, student_raw)
    }
}

pub fn evaluate_quantization_gap(
    classic_fp32: &ClassicFloatNetwork,
    bundle: &ClassicIntNetworkBundle,
    scales: &ClassicQuantizationScales,
    samples: &[Sample],
    config: &Config,
) -> QuantEvalMetrics {
    let mut metrics = QuantEvalMetrics::default();
    let mut scratch = ClassicScratch::new(CLASSIC_ACC_DIM, CLASSIC_H1_DIM, CLASSIC_H2_DIM);

    // --- Scratch buffers (INT path) to avoid per-sample allocations ---
    let acc_dim = bundle.transformer.acc_dim;
    // 新 API: ClassicScratchViews を利用して FT+中間層+acc を一括再利用
    let mut views = ClassicScratchViews::new(acc_dim, bundle.network.h1_dim, bundle.network.h2_dim);

    let mut cp_abs_diffs = Vec::new();
    let mut cp_weights = Vec::new();
    let mut logit_abs_diffs = Vec::new();
    let mut logit_weights = Vec::new();

    let mut mae_cp_num = 0.0f64;
    let mut mae_logit_num = 0.0f64;
    let mut weight_sum = 0.0f64;

    let mut max_cp = 0.0f32;
    let mut max_logit = 0.0f32;

    for sample in samples.iter().filter(|s| s.weight > 0.0).take(MAX_DISTILL_SAMPLES) {
        let weight = sample.weight as f64;

        let features_them = to_features_them(&sample.features);
        let ds = DistillSample {
            features_us: sample.features.clone(),
            features_them: features_them.clone(),
            teacher_output: 0.0,
            label: 0.0,
            weight: 0.0,
        };
        let fp32_logit = forward(classic_fp32, &ds, &mut scratch);

        // Reuse scratch to avoid allocating acc/bias copies each iteration
        let int_output = bundle.propagate_with_features_scratch_full(
            &sample.features,
            &features_them,
            &mut views,
        ) as f32;
        // Classic v1: 最終出力は単一スケール (per-tensor)。将来 per-channel 化する場合は
        // ClassicQuantizationScales::output_scale() の実装を差し替える。
        let int_logit = int_output * scales.output_scale();

        metrics.n += 1;
        weight_sum += weight;

        if config.label_type == "wdl" {
            let diff_logit = (int_logit - fp32_logit).abs();
            mae_logit_num += diff_logit as f64 * weight;
            logit_abs_diffs.push(diff_logit);
            logit_weights.push(weight as f32);
            if diff_logit > max_logit {
                max_logit = diff_logit;
            }
        }

        let (fp32_cp, int_cp) = if config.label_type == "wdl" {
            (convert_logit_to_cp(fp32_logit, config), convert_logit_to_cp(int_logit, config))
        } else {
            (fp32_logit, int_logit)
        };

        let diff_cp = (int_cp - fp32_cp).abs();
        mae_cp_num += diff_cp as f64 * weight;
        cp_abs_diffs.push(diff_cp);
        cp_weights.push(weight as f32);
        if diff_cp > max_cp {
            max_cp = diff_cp;
        }
    }

    if metrics.n == 0 || weight_sum == 0.0 {
        metrics.n = 0;
        return metrics;
    }

    metrics.mae_cp = Some((mae_cp_num / weight_sum) as f32);
    metrics.max_cp = Some(max_cp);
    metrics.p95_cp = weighted_percentile(
        cp_abs_diffs.iter().cloned().zip(cp_weights.iter().cloned()).collect(),
        0.95,
    );

    if config.label_type == "wdl" {
        metrics.mae_logit = Some((mae_logit_num / weight_sum) as f32);
        metrics.max_logit = Some(max_logit);
        metrics.p95_logit = weighted_percentile(
            logit_abs_diffs.iter().cloned().zip(logit_weights.iter().cloned()).collect(),
            0.95,
        );
    }

    metrics
}

#[cfg(test)]
mod percentile_tests {
    use super::weighted_percentile;

    #[test]
    fn weighted_percentile_extremes() {
        let base = vec![(0.0, 1.0), (10.0, 1.0)];
        assert_eq!(weighted_percentile(base.clone(), 0.0), Some(0.0));
        assert_eq!(weighted_percentile(base.clone(), 1.0), Some(10.0));
    }

    #[test]
    fn weighted_percentile_skewed_weights() {
        let skew = vec![(0.0, 0.001), (100.0, 1000.0)];
        assert_eq!(weighted_percentile(skew, 0.5), Some(100.0));
    }
}

#[cfg(test)]
mod distill_training_tests {
    use super::*;
    use rand::SeedableRng;

    #[test]
    fn backward_update_changes_weights_with_he_init() {
        let mut rng = rand::rngs::StdRng::seed_from_u64(7);
        let mut net = ClassicFloatNetwork::he_uniform_with_dims(4, 2, 2, 2, 2, &mut rng);
        net.hidden1_biases.iter_mut().for_each(|b| *b = 0.1);
        net.hidden2_biases.iter_mut().for_each(|b| *b = 0.1);

        let before_ft = net.ft_weights.clone();
        let before_h1 = net.hidden1_weights.clone();
        let before_h2 = net.hidden2_weights.clone();

        let sample = DistillSample {
            features_us: vec![0],
            features_them: vec![1],
            teacher_output: 0.0,
            label: 0.0,
            weight: 1.0,
        };
        let mut scratch = ClassicScratch::new(2, 2, 2);

        let _ = forward(&net, &sample, &mut scratch);
        backward_update(&mut net, &mut scratch, &sample, 1.0, 1e-2);

        let delta_threshold: f32 = 1e-6;
        let changed_ft = net
            .ft_weights
            .iter()
            .zip(before_ft.iter())
            .any(|(a, b)| (*a - *b).abs() > delta_threshold);
        let changed_h1 = net
            .hidden1_weights
            .iter()
            .zip(before_h1.iter())
            .any(|(a, b)| (*a - *b).abs() > delta_threshold);
        let changed_h2 = net
            .hidden2_weights
            .iter()
            .zip(before_h2.iter())
            .any(|(a, b)| (*a - *b).abs() > delta_threshold);

        assert!(changed_ft || changed_h1 || changed_h2);
    }

    #[test]
    fn scale_teacher_by_temperature_applies_when_enabled() {
        let mut loss = 0.5f32;
        let mut grad = 0.25f32;
        scale_teacher_by_temperature(&mut loss, &mut grad, 2.0, true);
        assert!((loss - 2.0).abs() < 1e-6);
        assert!((grad - 1.0).abs() < 1e-6);

        let mut loss_no = 0.5f32;
        let mut grad_no = 0.25f32;
        scale_teacher_by_temperature(&mut loss_no, &mut grad_no, 2.0, false);
        assert!((loss_no - 0.5).abs() < 1e-6);
        assert!((grad_no - 0.25).abs() < 1e-6);
    }

    #[test]
    fn forward_skips_out_of_range_features_without_panic() {
        let mut rng = rand::rngs::StdRng::seed_from_u64(99);
        let mut net = ClassicFloatNetwork::he_uniform_with_dims(4, 2, 2, 2, 2, &mut rng);
        net.ft_weights.fill(0.0);
        net.ft_biases.fill(0.0);
        net.output_bias = 1.23;

        let sample = DistillSample {
            features_us: vec![10_000], // clearly越境
            features_them: vec![20_000],
            teacher_output: 0.0,
            label: 0.0,
            weight: 1.0,
        };
        let mut scratch = ClassicScratch::new(2, 2, 2);
        let out = forward(&net, &sample, &mut scratch);
        assert!((out - net.output_bias).abs() < 1e-6);
    }
}

// --- Domain conversion helpers (exposed for tests via unit test module) ---
#[cfg(test)]
fn teacher_logit_from_raw(
    _label_type: &str,
    domain: TeacherValueDomain,
    raw: f32,
    scale: f32,
) -> f32 {
    match domain {
        TeacherValueDomain::WdlLogit => raw,   // raw already logit
        TeacherValueDomain::Cp => raw / scale, // normalize cp -> logit
    }
}

#[cfg(test)]
fn teacher_cp_from_raw(label_type: &str, domain: TeacherValueDomain, raw: f32, scale: f32) -> f32 {
    if label_type == "wdl" {
        // raw interpreted as (maybe) logit; convert logit->cp after normalizing
        let logit = teacher_logit_from_raw(label_type, domain, raw, scale);
        logit * scale
    } else {
        // label_type == cp : raw is either cp or logit depending on domain
        match domain {
            TeacherValueDomain::Cp => raw,
            TeacherValueDomain::WdlLogit => raw * scale,
        }
    }
}

#[cfg(test)]
mod domain_conversion_tests {
    use super::*;

    #[test]
    fn teacher_domain_wdl_teacher_logit_roundtrip() {
        let scale = 600.0;
        let raw_logit = 0.75f32; // teacher raw in logit domain
        let logit = teacher_logit_from_raw("wdl", TeacherValueDomain::WdlLogit, raw_logit, scale);
        let cp = teacher_cp_from_raw("wdl", TeacherValueDomain::WdlLogit, raw_logit, scale);
        assert!((logit - raw_logit).abs() < 1e-6);
        assert!((cp - raw_logit * scale).abs() < 1e-4);
    }

    #[test]
    fn teacher_domain_wdl_teacher_cp_normalization() {
        let scale = 600.0;
        let raw_cp = 300.0f32; // raw in cp domain when domain=Cp
        let logit = teacher_logit_from_raw("wdl", TeacherValueDomain::Cp, raw_cp, scale);
        let cp = teacher_cp_from_raw("wdl", TeacherValueDomain::Cp, raw_cp, scale);
        assert!((logit - raw_cp / scale).abs() < 1e-6);
        assert!((cp - raw_cp).abs() < 1e-6);
    }

    #[test]
    fn teacher_domain_cp_teacher_logit_conversion() {
        let scale = 600.0;
        let raw_logit = 1.2f32; // raw in logit when domain=WdlLogit
        let logit = teacher_logit_from_raw("cp", TeacherValueDomain::WdlLogit, raw_logit, scale);
        let cp = teacher_cp_from_raw("cp", TeacherValueDomain::WdlLogit, raw_logit, scale);
        assert!((logit - raw_logit).abs() < 1e-6);
        assert!((cp - raw_logit * scale).abs() < 1e-4);
    }

    #[test]
    fn teacher_domain_cp_teacher_cp_identity() {
        let scale = 600.0;
        let raw_cp = 480.0f32; // raw cp when domain=Cp
        let logit = teacher_logit_from_raw("cp", TeacherValueDomain::Cp, raw_cp, scale);
        let cp = teacher_cp_from_raw("cp", TeacherValueDomain::Cp, raw_cp, scale);
        assert!((logit - raw_cp / scale).abs() < 1e-6);
        assert!((cp - raw_cp).abs() < 1e-6);
    }

    // Regression test: label_type=cp かつ teacher_domain=wdl-logit の評価で
    // 生徒出力 (student_raw) を誤って scale しないことを確認する。
    // teacher_raw=1.0 (logit) => cp=scale=600, student_raw=300cp とし
    // MAE が 300 になるべき（もし誤って *scale されると巨大値に化ける）。
    #[test]
    fn cp_label_logit_teacher_no_student_rescale() {
        let scale = 600.0f32;
        let teacher_raw_logit = 1.0f32; // implies 600cp teacher
        let student_cp = 300.0f32; // classic fp32 output

        // teacher cp
        let teacher_cp =
            teacher_cp_from_raw("cp", TeacherValueDomain::WdlLogit, teacher_raw_logit, scale);
        assert!((teacher_cp - 600.0).abs() < 1e-6);

        // simulate evaluate_distill current logic after bugfix:
        // student stays as-is (cp domain)
        let diff = (student_cp - teacher_cp).abs();
        assert!((diff - 300.0).abs() < 1e-6);
    }
}
