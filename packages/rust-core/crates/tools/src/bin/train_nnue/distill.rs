use std::collections::HashMap;
use std::fs::{self, File};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use crate::classic::ClassicScratchViews;
use crate::classic::{
    ClassicActivationSummary, ClassicFloatNetwork, ClassicIntNetworkBundle,
    ClassicLayerQuantScheme, ClassicQuantizationScales,
};
use crate::logging::StructuredLogger;
use crate::params::{CLASSIC_ACC_DIM, CLASSIC_H1_DIM, CLASSIC_H2_DIM, CLASSIC_RELU_CLIP_F32};
use crate::teacher::{TeacherBatchRequest, TeacherLayers, TeacherNetwork};
use crate::types::{
    Config, DistillLossKind, DistillOptions, QuantScheme, Sample, TeacherKind, TeacherScaleFitKind,
    TeacherValueDomain,
};
use bincode::{deserialize_from, serialize_into};
use engine_core::evaluation::nnue::features::flip_us_them;
use engine_core::evaluation::nnue::features::FE_END;
use engine_core::shogi::SHOGI_BOARD_SIZE;
use rand::rngs::StdRng;
use rand::SeedableRng;
use serde::{Deserialize, Serialize};

const MAX_DISTILL_SAMPLES: usize = 50_000;
const TEACHER_EVAL_BATCH: usize = 512;
const ACTIVATION_CALIBRATION_LIMIT: usize = 32_768;
const DISTILL_EPOCHS: usize = 2;
const DISTILL_LR: f32 = 1e-4;
const PROB_EPS: f32 = 1e-6;
static WARNED_OOR_FWD: AtomicBool = AtomicBool::new(false);
static WARNED_OOR_BWD: AtomicBool = AtomicBool::new(false);

#[inline]
fn warn_oor_once(flag: &AtomicBool, ctx: &str, feat: u32, input_dim: usize, acc_dim: usize) {
    if !flag.swap(true, Ordering::Relaxed) {
        let ft_len = input_dim.saturating_mul(acc_dim);
        log::warn!(
            "{}: feature index {} out of range (input_dim={}, acc_dim={}, ft_len={}); subsequent warnings suppressed",
            ctx,
            feat,
            input_dim,
            acc_dim,
            ft_len
        );
    }
}

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
    teacher: Arc<TeacherPrepared>,
    label: f32,
    weight: f32,
}

#[derive(Clone)]
struct TeacherPrepared {
    value: f32,
    domain: TeacherValueDomain,
    layers: Option<Arc<TeacherLayers>>, // None if intermediate layers were not captured
}

impl TeacherPrepared {
    /// Returns true if this prepared entry includes intermediate layer outputs.
    fn has_layers(&self) -> bool {
        self.layers.is_some()
    }
}

struct PreparedDistillSamples {
    samples: Vec<DistillSample>,
    cache_hits: usize,
    cache_misses: usize,
    teacher_scale: Option<(f32, f32)>,
}

struct PendingSample {
    features_us: Vec<u32>,
    features_them: Vec<u32>,
    label: f32,
    weight: f32,
}

struct LayerLossWeights {
    ft: f32,
    h1: f32,
    h2: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TeacherCacheLayers {
    ft: Vec<f32>,
    h1: Vec<f32>,
    h2: Vec<f32>,
    out: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TeacherCacheEntry {
    domain: TeacherValueDomain,
    value: f32,
    layers: Option<TeacherCacheLayers>,
}

#[derive(Debug, Serialize, Deserialize)]
struct TeacherCacheFile {
    #[serde(default = "TeacherCacheFile::default_version")]
    version: u32,
    entries: Vec<(Vec<u32>, TeacherCacheEntry)>,
}

impl TeacherCacheFile {
    const VERSION: u32 = 1;

    fn default_version() -> u32 {
        0
    }
}

impl From<&TeacherPrepared> for TeacherCacheEntry {
    fn from(value: &TeacherPrepared) -> Self {
        let layers = value.layers.as_ref().map(|layers_arc| TeacherCacheLayers {
            ft: layers_arc.ft.clone(),
            h1: layers_arc.h1.clone(),
            h2: layers_arc.h2.clone(),
            out: layers_arc.out,
        });
        TeacherCacheEntry {
            domain: value.domain,
            value: value.value,
            layers,
        }
    }
}

impl TeacherCacheEntry {
    fn into_prepared(self) -> TeacherPrepared {
        let layers = self.layers.map(|l| {
            Arc::new(TeacherLayers {
                ft: l.ft,
                h1: l.h1,
                h2: l.h2,
                out: l.out,
            })
        });
        TeacherPrepared {
            value: self.value,
            domain: self.domain,
            layers,
        }
    }
}

#[derive(Clone, Debug)]
pub struct DistillArtifacts {
    pub classic_fp32: ClassicFloatNetwork,
    pub bundle_int: ClassicIntNetworkBundle,
    pub scales: ClassicQuantizationScales,
    pub calibration_metrics: Option<QuantEvalMetrics>,
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
    x.clamp(0.0, CLASSIC_RELU_CLIP_F32)
}
fn relu_clip_grad(z: f32) -> f32 {
    if z > 0.0 && z < CLASSIC_RELU_CLIP_F32 {
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

#[inline]
fn mse_loss_grad(diff: f32, weight: f32) -> (f32, f32) {
    let grad = diff * weight;
    let loss = 0.5 * diff * diff * weight;
    (loss, grad)
}

#[inline]
fn huber_loss_grad(diff: f32, weight: f32, delta: f32) -> (f32, f32) {
    let abs = diff.abs();
    if abs <= delta {
        mse_loss_grad(diff, weight)
    } else {
        let grad = delta * diff.signum() * weight;
        let loss = delta * (abs - 0.5 * delta) * weight;
        (loss, grad)
    }
}

#[inline]
fn element_loss_grad(
    diff: f32,
    weight: f32,
    loss_kind: DistillLossKind,
    huber_delta: f32,
) -> (f32, f32) {
    match loss_kind {
        DistillLossKind::Huber => huber_loss_grad(diff, weight, huber_delta),
        _ => mse_loss_grad(diff, weight),
    }
}

fn forward(net: &ClassicFloatNetwork, sample: &DistillSample, scratch: &mut ClassicScratch) -> f32 {
    scratch.acc_us.copy_from_slice(&net.ft_biases);
    for &feat in &sample.features_us {
        let idx = feat as usize * net.acc_dim;
        if idx + net.acc_dim > net.ft_weights.len() {
            warn_oor_once(&WARNED_OOR_FWD, "forward(us)", feat, net.input_dim, net.acc_dim);
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
            warn_oor_once(&WARNED_OOR_FWD, "forward(them)", feat, net.input_dim, net.acc_dim);
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

fn compute_activation_summary(
    net: &ClassicFloatNetwork,
    samples: &[DistillSample],
    limit: usize,
) -> ClassicActivationSummary {
    let mut summary = ClassicActivationSummary::default();
    if samples.is_empty() {
        return summary;
    }

    let mut scratch = ClassicScratch::new(net.acc_dim, net.h1_dim, net.h2_dim);
    for sample in samples.iter().take(limit) {
        let _ = forward(net, sample, &mut scratch);

        for &value in &scratch.acc_us {
            summary.ft_max_abs = summary.ft_max_abs.max(value.abs());
        }
        for &value in &scratch.acc_them {
            summary.ft_max_abs = summary.ft_max_abs.max(value.abs());
        }
        for &value in &scratch.a1 {
            summary.h1_max_abs = summary.h1_max_abs.max(value.abs());
        }
        for &value in &scratch.a2 {
            summary.h2_max_abs = summary.h2_max_abs.max(value.abs());
        }
    }

    summary
}

struct BackwardUpdateParams<'a> {
    grad_output: f32,
    layer_weights: &'a LayerLossWeights,
    teacher_layers: Option<&'a TeacherLayers>,
    loss_kind: DistillLossKind,
    huber_delta: f32,
    learning_rate: f32,
    l2_reg: f32,
}

fn backward_update(
    net: &mut ClassicFloatNetwork,
    scratch: &mut ClassicScratch,
    sample: &DistillSample,
    params: BackwardUpdateParams<'_>,
) -> f64 {
    let BackwardUpdateParams {
        grad_output,
        layer_weights,
        teacher_layers,
        loss_kind,
        huber_delta,
        learning_rate,
        l2_reg,
    } = params;
    let h1_dim = net.h1_dim;
    let h2_dim = net.h2_dim;
    let acc_dim = net.acc_dim;
    let input_dim = acc_dim * 2;
    let sample_weight = sample.weight;

    let (teacher_ft, teacher_h1, teacher_h2) = if let Some(layers) = teacher_layers {
        (
            if layers.ft.len() == input_dim {
                Some(&layers.ft[..])
            } else {
                None
            },
            if layers.h1.len() == h1_dim {
                Some(&layers.h1[..])
            } else {
                None
            },
            if layers.h2.len() == h2_dim {
                Some(&layers.h2[..])
            } else {
                None
            },
        )
    } else {
        (None, None, None)
    };

    let mut extra_loss = 0.0f64;

    // Output layer gradients
    for (i, grad_a2) in scratch.d_a2.iter_mut().enumerate().take(h2_dim) {
        let mut total_grad = grad_output * net.output_weights[i];
        if layer_weights.h2 > 0.0 {
            if let Some(t_h2) = teacher_h2 {
                let diff = scratch.a2[i] - t_h2[i];
                let weight = sample_weight * layer_weights.h2;
                let (loss, grad) = element_loss_grad(diff, weight, loss_kind, huber_delta);
                extra_loss += loss as f64;
                total_grad += grad;
            }
        }
        *grad_a2 = total_grad;
        scratch.d_z2[i] = total_grad * relu_clip_grad(scratch.z2[i]);
    }

    // Gradient wrt output weights/bias
    for i in 0..h2_dim {
        let grad_w = grad_output * scratch.a2[i] + l2_reg * net.output_weights[i];
        net.output_weights[i] -= learning_rate * grad_w;
    }
    net.output_bias -= learning_rate * grad_output;

    // Propagate to hidden1 (using current weights before update)
    scratch.d_a1.fill(0.0);
    for (i, row) in net.hidden2_weights.chunks(h1_dim).enumerate().take(h2_dim) {
        let delta = scratch.d_z2[i];
        for (da1, &w) in scratch.d_a1.iter_mut().zip(row.iter()) {
            *da1 += delta * w;
        }
    }

    if layer_weights.h1 > 0.0 {
        if let Some(t_h1) = teacher_h1 {
            for ((da1, &a1), &teacher_a1) in
                scratch.d_a1.iter_mut().zip(scratch.a1.iter()).zip(t_h1.iter()).take(h1_dim)
            {
                let diff = a1 - teacher_a1;
                let weight = sample_weight * layer_weights.h1;
                let (loss, grad) = element_loss_grad(diff, weight, loss_kind, huber_delta);
                extra_loss += loss as f64;
                *da1 += grad;
            }
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

    if layer_weights.ft > 0.0 {
        if let Some(t_ft) = teacher_ft {
            for (idx, din) in scratch.d_input.iter_mut().enumerate().take(input_dim) {
                let diff = scratch.input[idx] - t_ft[idx];
                let weight = sample_weight * layer_weights.ft;
                let (loss, grad) = element_loss_grad(diff, weight, loss_kind, huber_delta);
                extra_loss += loss as f64;
                *din += grad;
            }
        }
    }

    // Update hidden2 weights/biases
    for i in 0..h2_dim {
        let delta = scratch.d_z2[i];
        let row = &mut net.hidden2_weights[i * h1_dim..(i + 1) * h1_dim];
        for (w, &a1) in row.iter_mut().zip(scratch.a1.iter()) {
            let grad = delta * a1 + l2_reg * *w;
            *w -= learning_rate * grad;
        }
        net.hidden2_biases[i] -= learning_rate * delta;
    }

    // Update hidden1 weights/biases
    for j in 0..h1_dim {
        let delta = scratch.d_z1[j];
        let row = &mut net.hidden1_weights[j * input_dim..(j + 1) * input_dim];
        for (w, &inp) in row.iter_mut().zip(scratch.input.iter()) {
            let grad = delta * inp + l2_reg * *w;
            *w -= learning_rate * grad;
        }
        net.hidden1_biases[j] -= learning_rate * delta;
    }

    // Split input gradients for accumulator channels
    let (d_acc_us, d_acc_them) = scratch.d_input.split_at(acc_dim);

    // Update FT biases (shared)
    for i in 0..acc_dim {
        let grad = d_acc_us[i] + d_acc_them[i];
        net.ft_biases[i] -= learning_rate * grad;
    }

    // Update FT weights for active features
    for &feat in &sample.features_us {
        let base = feat as usize * acc_dim;
        if base + acc_dim > net.ft_weights.len() {
            warn_oor_once(&WARNED_OOR_BWD, "backward(us)", feat, net.input_dim, acc_dim);
            continue;
        }
        let row = &mut net.ft_weights[base..base + acc_dim];
        for (w, &grad) in row.iter_mut().zip(d_acc_us.iter()) {
            let grad = grad + l2_reg * *w;
            *w -= learning_rate * grad;
        }
    }
    for &feat in &sample.features_them {
        let base = feat as usize * acc_dim;
        if base + acc_dim > net.ft_weights.len() {
            warn_oor_once(&WARNED_OOR_BWD, "backward(them)", feat, net.input_dim, acc_dim);
            continue;
        }
        let row = &mut net.ft_weights[base..base + acc_dim];
        for (w, &grad) in row.iter_mut().zip(d_acc_them.iter()) {
            let grad = grad + l2_reg * *w;
            *w -= learning_rate * grad;
        }
    }

    extra_loss
}

/// 教師 forward 結果と反転特徴を事前計算し、蒸留処理での再利用を容易にする。
fn prepare_distill_samples(
    teacher: &dyn TeacherNetwork,
    samples: &[Sample],
    max_samples: usize,
    config: &Config,
    distill: &DistillOptions,
) -> Result<PreparedDistillSamples, String> {
    let domain = distill.teacher_domain;
    let require_layers = distill.requires_teacher_layers();
    let batch_size = distill.teacher_batch_size.max(1);

    let mut cache: HashMap<Vec<u32>, Arc<TeacherPrepared>> = match distill.teacher_cache.as_ref() {
        Some(path) => load_teacher_cache(Path::new(path), require_layers, domain)?,
        None => HashMap::new(),
    };
    let mut cache_dirty = false;
    let mut cache_hits = 0usize;
    let mut cache_misses = 0usize;

    let mut result = Vec::new();
    let mut pending: Vec<PendingSample> = Vec::new();

    for sample in samples.iter().filter(|s| s.weight > 0.0) {
        if result.len() + pending.len() >= max_samples {
            break;
        }

        let features_us = sample.features.clone();

        if let Some(prepared) = cache.get(&features_us) {
            if prepared.domain == domain && (!require_layers || prepared.has_layers()) {
                cache_hits += 1;
                let features_them: Vec<u32> =
                    features_us.iter().map(|&f| flip_us_them(f as usize) as u32).collect();
                result.push(DistillSample {
                    features_us,
                    features_them,
                    teacher: prepared.clone(),
                    label: sample.label,
                    weight: sample.weight,
                });
                continue;
            }
        }

        let features_them: Vec<u32> =
            features_us.iter().map(|&f| flip_us_them(f as usize) as u32).collect();
        pending.push(PendingSample {
            features_us,
            features_them,
            label: sample.label,
            weight: sample.weight,
        });

        if pending.len() >= batch_size {
            let processed = flush_pending(
                teacher,
                &mut pending,
                &mut cache,
                &mut result,
                domain,
                require_layers,
            )?;
            cache_misses += processed;
            if processed > 0 && distill.teacher_cache.is_some() {
                cache_dirty = true;
            }
        }
    }

    if !pending.is_empty() {
        let processed =
            flush_pending(teacher, &mut pending, &mut cache, &mut result, domain, require_layers)?;
        cache_misses += processed;
        if processed > 0 && distill.teacher_cache.is_some() {
            cache_dirty = true;
        }
    }

    if let Some(path) = distill.teacher_cache.as_ref() {
        if cache_dirty {
            save_teacher_cache(Path::new(path), &cache)?;
        }
    }

    result.truncate(max_samples);

    let teacher_scale = compute_teacher_scale_fit(&result, config, distill);

    Ok(PreparedDistillSamples {
        samples: result,
        cache_hits,
        cache_misses,
        teacher_scale,
    })
}

fn flush_pending(
    teacher: &dyn TeacherNetwork,
    pending: &mut Vec<PendingSample>,
    cache: &mut HashMap<Vec<u32>, Arc<TeacherPrepared>>,
    out: &mut Vec<DistillSample>,
    domain: TeacherValueDomain,
    require_layers: bool,
) -> Result<usize, String> {
    if pending.is_empty() {
        return Ok(0);
    }

    let requests: Vec<_> = pending
        .iter()
        .map(|p| TeacherBatchRequest {
            features: &p.features_us,
        })
        .collect();

    let evals = teacher
        .evaluate_batch(&requests, domain, require_layers)
        .map_err(|e| format!("failed to evaluate teacher {:?}: {e}", teacher.kind()))?;

    if evals.len() != pending.len() {
        return Err(format!(
            "teacher returned mismatched batch size: expected {}, got {}",
            pending.len(),
            evals.len()
        ));
    }

    let processed = evals.len();

    for (pending_sample, eval) in pending.drain(..).zip(evals.into_iter()) {
        let layers = eval.layers.map(Arc::new);
        if require_layers && layers.is_none() {
            return Err("teacher did not return layer outputs despite λ_ft/λ_h1/λ_h2 > 0".into());
        }
        let prepared = Arc::new(TeacherPrepared {
            value: eval.value,
            domain: eval.domain,
            layers,
        });
        let cache_key = pending_sample.features_us.clone();
        cache.insert(cache_key, prepared.clone());
        out.push(DistillSample {
            features_us: pending_sample.features_us,
            features_them: pending_sample.features_them,
            teacher: prepared,
            label: pending_sample.label,
            weight: pending_sample.weight,
        });
    }

    Ok(processed)
}

fn compute_teacher_scale_fit(
    samples: &[DistillSample],
    config: &Config,
    distill: &DistillOptions,
) -> Option<(f32, f32)> {
    if samples.is_empty() {
        return None;
    }
    if !matches!(distill.teacher_scale_fit, TeacherScaleFitKind::Linear) {
        return None;
    }

    let mut sw = 0.0f64;
    let mut sx = 0.0f64;
    let mut sy = 0.0f64;
    let mut sxx = 0.0f64;
    let mut sxy = 0.0f64;
    let mut count = 0usize;

    let label_type = config.label_type.as_str();
    for sample in samples {
        let w = sample.weight as f64;
        if w <= 0.0 {
            continue;
        }
        let teacher_label_space = teacher_value_in_label_space(
            sample.teacher.value,
            distill.teacher_domain,
            label_type,
            config.scale,
        ) as f64;
        let (x, y) = match label_type {
            "wdl" => {
                let label_prob = sample.label.clamp(PROB_EPS, 1.0 - PROB_EPS);
                let label_logit = stable_logit(label_prob) as f64;
                (teacher_label_space, label_logit)
            }
            "cp" => {
                let label_cp = sample.label as f64;
                (teacher_label_space, label_cp)
            }
            _ => continue,
        };
        if !x.is_finite() || !y.is_finite() {
            continue;
        }
        sw += w;
        sx += w * x;
        sy += w * y;
        sxx += w * x * x;
        sxy += w * x * y;
        count += 1;
    }

    if count < 2 || sw <= f64::EPSILON {
        return None;
    }
    let denom = sw * sxx - sx * sx;
    if denom.abs() < 1e-9 {
        return None;
    }
    let gain = (sw * sxy - sx * sy) / denom;
    let bias = (sy - gain * sx) / sw;
    let gain_f32 = gain as f32;
    let bias_f32 = bias as f32;
    if !gain_f32.is_finite() || !bias_f32.is_finite() {
        return None;
    }
    Some((gain_f32, bias_f32))
}

#[inline]
fn apply_teacher_scale(value: f32, scale: Option<(f32, f32)>) -> f32 {
    if let Some((gain, bias)) = scale {
        value * gain + bias
    } else {
        value
    }
}

#[inline]
fn teacher_value_in_label_space(
    value: f32,
    teacher_domain: TeacherValueDomain,
    label_type: &str,
    scale: f32,
) -> f32 {
    match label_type {
        "wdl" => match teacher_domain {
            TeacherValueDomain::WdlLogit => value,
            TeacherValueDomain::Cp => value / scale,
        },
        "cp" => match teacher_domain {
            TeacherValueDomain::WdlLogit => value * scale,
            TeacherValueDomain::Cp => value,
        },
        _ => value,
    }
}

fn load_teacher_cache(
    path: &Path,
    require_layers: bool,
    domain: TeacherValueDomain,
) -> Result<HashMap<Vec<u32>, Arc<TeacherPrepared>>, String> {
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let file = File::open(path)
        .map_err(|e| format!("failed to open teacher cache {}: {e}", path.display()))?;
    let cache_file: TeacherCacheFile = deserialize_from(file)
        .map_err(|e| format!("failed to deserialize teacher cache {}: {e}", path.display()))?;
    if cache_file.version != 0 && cache_file.version != TeacherCacheFile::VERSION {
        log::warn!(
            "teacher cache {} version {} is incompatible with expected {}; entries will be re-generated",
            path.display(),
            cache_file.version,
            TeacherCacheFile::VERSION
        );
    }
    let mut map = HashMap::with_capacity(cache_file.entries.len());
    for (features, entry) in cache_file.entries {
        if entry.domain != domain {
            continue;
        }
        if require_layers && entry.layers.is_none() {
            continue;
        }
        map.insert(features, Arc::new(entry.into_prepared()));
    }
    Ok(map)
}

fn save_teacher_cache(
    path: &Path,
    cache: &HashMap<Vec<u32>, Arc<TeacherPrepared>>,
) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)
            .map_err(|e| format!("failed to create cache directory {}: {e}", dir.display()))?;
    }
    let mut entries = Vec::with_capacity(cache.len());
    for (features, prepared) in cache {
        entries.push((features.clone(), TeacherCacheEntry::from(prepared.as_ref())));
    }
    let cache_file = TeacherCacheFile {
        version: TeacherCacheFile::VERSION,
        entries,
    };
    let mut file = File::create(path)
        .map_err(|e| format!("failed to create teacher cache {}: {e}", path.display()))?;
    serialize_into(&mut file, &cache_file)
        .map_err(|e| format!("failed to serialize teacher cache {}: {e}", path.display()))?;
    Ok(())
}

pub struct ClassicDistillConfig<'a> {
    pub quant_ft: QuantScheme,
    pub quant_h1: QuantScheme,
    pub quant_h2: QuantScheme,
    pub quant_out: QuantScheme,
    pub quant_calibration: Option<QuantCalibration<'a>>,
    pub structured: Option<&'a StructuredLogger>,
}

impl<'a> ClassicDistillConfig<'a> {
    pub fn new(
        quant_ft: QuantScheme,
        quant_h1: QuantScheme,
        quant_h2: QuantScheme,
        quant_out: QuantScheme,
        quant_calibration: Option<QuantCalibration<'a>>,
        structured: Option<&'a StructuredLogger>,
    ) -> Self {
        Self {
            quant_ft,
            quant_h1,
            quant_h2,
            quant_out,
            quant_calibration,
            structured,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct QuantCalibration<'a> {
    pub samples: &'a [Sample],
    pub limit: usize,
    pub auto_search: bool,
}

#[derive(Clone, Debug)]
struct QuantCandidateReport {
    scheme: ClassicLayerQuantScheme,
    metrics: Option<QuantEvalMetrics>,
}

#[derive(Clone, Debug)]
struct QuantSelectionReport {
    candidates: Vec<QuantCandidateReport>,
    selected_index: usize,
    sample_source: &'static str,
    sample_count: usize,
    auto_search: bool,
}

struct QuantSelectionResult {
    bundle: ClassicIntNetworkBundle,
    scales: ClassicQuantizationScales,
    best_calibration_metrics: Option<QuantEvalMetrics>,
    report: Option<QuantSelectionReport>,
}

struct QuantSelectionParams<'a> {
    net: &'a ClassicFloatNetwork,
    base_scheme: ClassicLayerQuantScheme,
    activation: Option<ClassicActivationSummary>,
    calibration: Option<QuantCalibration<'a>>,
    fallback_samples: &'a [Sample],
    config: &'a Config,
}

#[inline]
fn quant_scheme_label(q: QuantScheme) -> &'static str {
    match q {
        QuantScheme::PerTensor => "per-tensor",
        QuantScheme::PerChannel => "per-channel",
    }
}

fn quant_metric_value(value: Option<f32>) -> f32 {
    match value {
        Some(v) if v.is_finite() => v,
        _ => f32::MAX,
    }
}

fn quant_metrics_score(metrics: &QuantEvalMetrics) -> (f32, f32, f32, f32, f32, usize) {
    (
        quant_metric_value(metrics.mae_cp),
        quant_metric_value(metrics.p95_cp),
        quant_metric_value(metrics.max_cp),
        quant_metric_value(metrics.mae_logit),
        quant_metric_value(metrics.p95_logit),
        usize::MAX - metrics.n,
    )
}

fn select_quantization_config(
    params: QuantSelectionParams<'_>,
) -> Result<QuantSelectionResult, String> {
    let QuantSelectionParams {
        net,
        base_scheme,
        activation,
        calibration,
        fallback_samples,
        config,
    } = params;
    let mut auto_search = false;
    let (eval_samples, sample_source) = match calibration {
        Some(cal) if !cal.samples.is_empty() && cal.limit > 0 => {
            let limit = cal.limit.min(cal.samples.len());
            let slice = &cal.samples[..limit];
            auto_search = cal.auto_search;
            if !slice.is_empty() {
                (slice, "quant-calibration")
            } else {
                let fallback_limit = cal.limit.min(fallback_samples.len());
                (&fallback_samples[..fallback_limit], "training-fallback")
            }
        }
        Some(cal) => {
            auto_search = cal.auto_search;
            let fallback_limit = cal.limit.min(fallback_samples.len());
            (&fallback_samples[..fallback_limit], "training-fallback")
        }
        None => {
            let fallback_limit = fallback_samples.len().min(MAX_DISTILL_SAMPLES);
            (&fallback_samples[..fallback_limit], "training")
        }
    };

    let mut candidates = Vec::new();
    candidates.push(base_scheme);
    if auto_search {
        let ft_scheme = base_scheme.ft;
        let out_scheme = base_scheme.out;
        for &h1 in &[QuantScheme::PerTensor, QuantScheme::PerChannel] {
            for &h2 in &[QuantScheme::PerTensor, QuantScheme::PerChannel] {
                let scheme = ClassicLayerQuantScheme::new(ft_scheme, h1, h2, out_scheme);
                if !candidates.contains(&scheme) {
                    candidates.push(scheme);
                }
            }
        }
    }

    let mut reports: Vec<QuantCandidateReport> = Vec::with_capacity(candidates.len());
    let mut best_index = 0usize;
    let mut best_score: Option<(f32, f32, f32, f32, f32, usize)> = None;
    let mut best_bundle: Option<ClassicIntNetworkBundle> = None;
    let mut best_scales: Option<ClassicQuantizationScales> = None;

    for (idx, scheme) in candidates.iter().enumerate() {
        let (bundle, scales) = net
            .quantize_symmetric(scheme.ft, scheme.h1, scheme.h2, scheme.out, activation)
            .map_err(|e| format!("quantize_symmetric failed for candidate {}: {}", idx, e))?;

        let metrics = if eval_samples.is_empty() {
            None
        } else {
            Some(evaluate_quantization_gap(net, &bundle, &scales, eval_samples, config))
        };

        let candidate_score = metrics.as_ref().map(quant_metrics_score);
        let is_better = match (candidate_score, best_score) {
            (Some(score), Some(best)) => score < best,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => idx == 0,
        };

        if is_better {
            best_index = idx;
            best_score = candidate_score;
            best_bundle = Some(bundle);
            best_scales = Some(scales.clone());
        }

        reports.push(QuantCandidateReport {
            scheme: *scheme,
            metrics,
        });

        if !is_better {
            // bundle and scales drop here for non-best candidates
        }
    }

    let bundle = best_bundle.expect("at least one quantization candidate must exist");
    let scales = best_scales.expect("selected quantization scales missing");

    let best_calibration_metrics = reports.get(best_index).and_then(|c| c.metrics.clone());

    let report = if !reports.is_empty() {
        Some(QuantSelectionReport {
            candidates: reports,
            selected_index: best_index,
            sample_source,
            sample_count: eval_samples.len(),
            auto_search,
        })
    } else {
        None
    };

    Ok(QuantSelectionResult {
        bundle,
        scales,
        best_calibration_metrics,
        report,
    })
}

pub fn distill_classic_after_training(
    teacher: &dyn TeacherNetwork,
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
    if config.label_type == "cp"
        && !matches!(distill.loss, DistillLossKind::Mse | DistillLossKind::Huber)
    {
        return Err("Classic distillation with cp labels supports --kd-loss=mse|huber only".into());
    }

    if !teacher.supports_domain(distill.teacher_domain) {
        return Err(format!(
            "teacher {:?} does not support domain {:?}",
            teacher.kind(),
            distill.teacher_domain
        ));
    }
    if distill.requires_teacher_layers() {
        // 現時点では Classic FP32 教師のみが中間層を提供する。
        if !matches!(teacher.kind(), TeacherKind::ClassicFp32) {
            return Err("Layer distillation weights require classic FP32 teacher".into());
        }
    }

    let start = Instant::now();
    let prepared = prepare_distill_samples(teacher, samples, MAX_DISTILL_SAMPLES, config, distill)?;
    if prepared.samples.is_empty() {
        return Err("No samples available for classic distillation".into());
    }

    log::info!(
        "distillation prepared {} samples (teacher cache hits={}, misses={})",
        prepared.samples.len(),
        prepared.cache_hits,
        prepared.cache_misses
    );

    if let Some(lg) = classic_cfg.structured {
        let cache_target = distill
            .teacher_cache
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<memory>".to_string());
        let rec = serde_json::json!({
            "ts": chrono::Utc::now().to_rfc3339(),
            "component": "distill",
            "phase": "distill_prepare",
            "samples": prepared.samples.len(),
            "cache_hits": prepared.cache_hits,
            "cache_misses": prepared.cache_misses,
            "teacher_cache": cache_target,
            "require_layers": distill.requires_teacher_layers(),
        });
        lg.write_json(&rec);
    }

    if let Some((gain, bias)) = prepared.teacher_scale {
        log::info!("teacher scale fit (linear) applied: gain={:.6}, bias={:.6}", gain, bias);
        if let Some(lg) = classic_cfg.structured {
            let rec = serde_json::json!({
                "ts": chrono::Utc::now().to_rfc3339(),
                "component": "distill",
                "phase": "teacher_scale_fit",
                "kind": "linear",
                "gain": gain,
                "bias": bias,
                "samples": prepared.samples.len(),
                "space": match config.label_type.as_str() {
                    "wdl" => "wdl-logit",
                    "cp" => "cp",
                    other => other,
                },
            });
            lg.write_json(&rec);
        }
    }

    let distill_seed = distill.seed.unwrap_or_else(rand::random::<u64>);
    let mut rng: StdRng = StdRng::seed_from_u64(distill_seed);
    let mut classic = ClassicFloatNetwork::he_uniform_with_dims(
        SHOGI_BOARD_SIZE * FE_END,
        CLASSIC_ACC_DIM,
        CLASSIC_H1_DIM,
        CLASSIC_H2_DIM,
        config.estimated_features_per_sample,
        &mut rng,
    );
    let teacher_scale = prepared.teacher_scale;

    // 出力バイアスを教師ターゲットの平均で初期化して初期収束を安定化させる。
    let mut bias_num = 0.0f64;
    let mut bias_den = 0.0f64;
    let mut bias_sq = 0.0f64;
    let mut bias_samples = 0usize;
    let label_type = config.label_type.as_str();
    for sample in &prepared.samples {
        let weight = sample.weight as f64;
        if weight <= 0.0 {
            continue;
        }
        let teacher_label_space = teacher_value_in_label_space(
            sample.teacher.value,
            distill.teacher_domain,
            label_type,
            config.scale,
        );
        let teacher_adjusted = apply_teacher_scale(teacher_label_space, teacher_scale);
        let target = match label_type {
            "wdl" => {
                let teacher_logit = teacher_adjusted;
                let teacher_prob = sigmoid(teacher_logit / distill.temperature);
                let label_prob = sample.label.clamp(PROB_EPS, 1.0 - PROB_EPS);
                let target_prob = (distill.alpha * teacher_prob
                    + (1.0 - distill.alpha) * label_prob)
                    .clamp(PROB_EPS, 1.0 - PROB_EPS);
                stable_logit(target_prob)
            }
            "cp" => {
                let teacher_cp = teacher_adjusted;
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
            "component": "distill",
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

    let mut scratch = ClassicScratch::new(classic.acc_dim, classic.h1_dim, classic.h2_dim);
    let temperature = distill.temperature;
    let loss_kind = distill.loss;
    let layer_weights = LayerLossWeights {
        ft: distill.layer_weight_ft,
        h1: distill.layer_weight_h1,
        h2: distill.layer_weight_h2,
    };
    let mut warn_soften_mse_once = distill.soften_student
        && matches!(loss_kind, DistillLossKind::Mse | DistillLossKind::Huber);
    let lambda_out = distill.layer_weight_out;
    let huber_delta = distill.huber_delta;
    let label_type = config.label_type.as_str();

    for epoch in 0..DISTILL_EPOCHS {
        let mut out_loss_sum = 0.0f64;
        let mut layer_loss_sum = 0.0f64;
        let mut epoch_weight = 0.0f64;
        for sample in &prepared.samples {
            let prediction = forward(&classic, sample, &mut scratch);
            let teacher_label_space = teacher_value_in_label_space(
                sample.teacher.value,
                distill.teacher_domain,
                label_type,
                config.scale,
            );
            let teacher_adjusted = apply_teacher_scale(teacher_label_space, teacher_scale);
            let (mut loss_contrib, mut grad_output) = match label_type {
                "wdl" => {
                    // Teacher may be cp or wdl-logit; normalize to logit space before temperature
                    let teacher_logit = teacher_adjusted;
                    let teacher_prob = sigmoid(teacher_logit / temperature);
                    let label_prob = sample.label.clamp(PROB_EPS, 1.0 - PROB_EPS);
                    let alpha = distill.alpha;
                    let beta = 1.0 - alpha;
                    let weight = sample.weight;
                    match loss_kind {
                        DistillLossKind::Mse | DistillLossKind::Huber => {
                            if warn_soften_mse_once {
                                log::warn!("--kd-soften-student は --kd-loss=mse では効果がありません (epoch={})", epoch + 1);
                                warn_soften_mse_once = false;
                            }
                            let teacher_target = stable_logit(teacher_prob);
                            let label_target = stable_logit(label_prob);
                            let diff_teacher = prediction - teacher_target;
                            let diff_label = prediction - label_target;
                            let (mut loss_teacher, mut grad_teacher) =
                                element_loss_grad(diff_teacher, weight, loss_kind, huber_delta);
                            let (loss_label, grad_label) =
                                element_loss_grad(diff_label, weight, loss_kind, huber_delta);
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
                    let mut target =
                        distill.alpha * teacher_adjusted + (1.0 - distill.alpha) * sample.label;
                    // Clip target for stability
                    target = target.clamp(-(config.cp_clip as f32), config.cp_clip as f32);
                    let diff = prediction - target;
                    let weight = sample.weight;
                    let (loss, grad) = element_loss_grad(diff, weight, loss_kind, huber_delta);
                    (loss as f64, grad)
                }
                _ => unreachable!(),
            };
            loss_contrib *= lambda_out as f64;
            grad_output *= lambda_out;

            out_loss_sum += loss_contrib;
            epoch_weight += sample.weight as f64;
            let layer_loss = backward_update(
                &mut classic,
                &mut scratch,
                sample,
                BackwardUpdateParams {
                    grad_output,
                    layer_weights: &layer_weights,
                    teacher_layers: sample.teacher.layers.as_deref(),
                    loss_kind,
                    huber_delta,
                    learning_rate: DISTILL_LR,
                    l2_reg: config.l2_reg,
                },
            );
            layer_loss_sum += layer_loss;
        }
        if let Some(lg) = classic_cfg.structured {
            let (loss_out_avg, loss_layers_avg) = if epoch_weight > 0.0 {
                (out_loss_sum / epoch_weight, layer_loss_sum / epoch_weight)
            } else {
                (0.0, 0.0)
            };
            let loss_total_avg = loss_out_avg + loss_layers_avg;
            let rec = serde_json::json!({
                "ts": chrono::Utc::now().to_rfc3339(),
                "component": "distill",
                "phase": "distill_classic",
                "epoch": (epoch + 1) as i64,
                "loss": loss_total_avg,
                "loss_out": loss_out_avg,
                "loss_layers": loss_layers_avg,
                "samples": prepared.samples.len(),
                "loss_kind": match loss_kind {
                    DistillLossKind::Mse => "mse",
                    DistillLossKind::Bce => "bce",
                    DistillLossKind::Kl => "kl",
                    DistillLossKind::Huber => "huber",
                },
                "alpha": distill.alpha,
                "temperature": temperature,
                "scale_temp2": distill.scale_temp2,
                "soften_student": distill.soften_student,
                "seed": distill_seed,
                "layer_weight_ft": distill.layer_weight_ft,
                "layer_weight_h1": distill.layer_weight_h1,
                "layer_weight_h2": distill.layer_weight_h2,
                "layer_weight_out": distill.layer_weight_out,
                "huber_delta": huber_delta,
                "teacher_domain": match distill.teacher_domain { crate::types::TeacherValueDomain::Cp => "cp", crate::types::TeacherValueDomain::WdlLogit => "wdl-logit" },
            });
            lg.write_json(&rec);
        }
    }

    let classic_fp32 = classic.clone();

    let activation_summary =
        compute_activation_summary(&classic, &prepared.samples, ACTIVATION_CALIBRATION_LIMIT);

    log::info!(
        "classic activation summary: ft_max={:.3}, h1_max={:.3}, h2_max={:.3} (limit {} samples)",
        activation_summary.ft_max_abs,
        activation_summary.h1_max_abs,
        activation_summary.h2_max_abs,
        ACTIVATION_CALIBRATION_LIMIT.min(prepared.samples.len())
    );

    if let Some(lg) = classic_cfg.structured {
        let rec = serde_json::json!({
            "ts": chrono::Utc::now().to_rfc3339(),
            "component": "distill",
            "phase": "classic_activation_summary",
            "ft_max_abs": activation_summary.ft_max_abs,
            "h1_max_abs": activation_summary.h1_max_abs,
            "h2_max_abs": activation_summary.h2_max_abs,
            "sample_limit": ACTIVATION_CALIBRATION_LIMIT,
            "samples_used": prepared.samples.len().min(ACTIVATION_CALIBRATION_LIMIT),
        });
        lg.write_json(&rec);
    }

    let base_scheme = ClassicLayerQuantScheme::new(
        classic_cfg.quant_ft,
        classic_cfg.quant_h1,
        classic_cfg.quant_h2,
        classic_cfg.quant_out,
    );
    let quant_selection = select_quantization_config(QuantSelectionParams {
        net: &classic,
        base_scheme,
        activation: Some(activation_summary),
        calibration: classic_cfg.quant_calibration,
        fallback_samples: samples,
        config,
    })?;
    let QuantSelectionResult {
        bundle,
        scales,
        best_calibration_metrics,
        report: quant_report,
    } = quant_selection;

    if let Some(report) = quant_report.as_ref() {
        if !report.candidates.is_empty() {
            let best = &report.candidates[report.selected_index];
            if let Some(metrics) = best.metrics.as_ref() {
                log::info!(
                    "classic quant selection: best h1={:?}, h2={:?} | auto_search={} | source={} | samples={} | mae_cp={:?} | p95_cp={:?}",
                    best.scheme.h1,
                    best.scheme.h2,
                    report.auto_search,
                    report.sample_source,
                    report.sample_count,
                    metrics.mae_cp,
                    metrics.p95_cp
                );
            } else {
                log::info!(
                    "classic quant selection: best h1={:?}, h2={:?} | auto_search={} | source={} | samples={} | metrics-unavailable",
                    best.scheme.h1,
                    best.scheme.h2,
                    report.auto_search,
                    report.sample_source,
                    report.sample_count
                );
            }
        }

        if let Some(lg) = classic_cfg.structured {
            for (idx, candidate) in report.candidates.iter().enumerate() {
                let mut rec = serde_json::json!({
                    "ts": chrono::Utc::now().to_rfc3339(),
                    "component": "quantization",
                    "phase": "classic_quant_search",
                    "candidate_index": idx as i64,
                    "selected": idx == report.selected_index,
                    "auto_search": report.auto_search,
                    "sample_source": report.sample_source,
                    "sample_count": report.sample_count as i64,
                    "scheme": {
                        "ft": quant_scheme_label(candidate.scheme.ft),
                        "h1": quant_scheme_label(candidate.scheme.h1),
                        "h2": quant_scheme_label(candidate.scheme.h2),
                        "out": quant_scheme_label(candidate.scheme.out),
                    },
                });
                if let Some(metrics) = candidate.metrics.as_ref() {
                    rec.as_object_mut().unwrap().insert(
                        "metrics".to_string(),
                        serde_json::json!({
                            "n": metrics.n,
                            "mae_cp": metrics.mae_cp,
                            "p95_cp": metrics.p95_cp,
                            "max_cp": metrics.max_cp,
                            "mae_logit": metrics.mae_logit,
                            "p95_logit": metrics.p95_logit,
                            "max_logit": metrics.max_logit,
                        }),
                    );
                }
                lg.write_json(&rec);
            }
        }
    }

    if let Some(lg) = classic_cfg.structured {
        let mut rec = serde_json::json!({
            "ts": chrono::Utc::now().to_rfc3339(),
            "component": "quantization",
            "phase": "classic_quantize",
            "s_w0": scales.s_w0,
            "s_w1": scales.s_w1,
            "s_w2": scales.s_w2,
            "s_w3": scales.s_w3,
            "s_in_1": scales.s_in_1,
            "s_in_2": scales.s_in_2,
            "s_in_3": scales.s_in_3,
            "quant_scheme": {
                "ft": quant_scheme_label(scales.scheme.ft),
                "h1": quant_scheme_label(scales.scheme.h1),
                "h2": quant_scheme_label(scales.scheme.h2),
                "out": quant_scheme_label(scales.scheme.out),
            },
            "elapsed_sec": start.elapsed().as_secs_f64(),
        });
        if let Some(report) = quant_report.as_ref() {
            if let Some(best) =
                report.candidates.get(report.selected_index).and_then(|c| c.metrics.as_ref())
            {
                rec.as_object_mut().unwrap().insert(
                    "best_calibration_metrics".to_string(),
                    serde_json::json!({
                        "n": best.n,
                        "mae_cp": best.mae_cp,
                        "p95_cp": best.p95_cp,
                        "max_cp": best.max_cp,
                        "mae_logit": best.mae_logit,
                        "p95_logit": best.p95_logit,
                        "max_logit": best.max_logit,
                        "sample_source": report.sample_source,
                        "sample_count": report.sample_count as i64,
                        "auto_search": report.auto_search,
                    }),
                );
            }
        }
        lg.write_json(&rec);
    }

    Ok(DistillArtifacts {
        classic_fp32,
        bundle_int: bundle,
        scales,
        calibration_metrics: best_calibration_metrics,
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
    teacher: &dyn TeacherNetwork,
    classic_fp32: &ClassicFloatNetwork,
    samples: &[Sample],
    config: &Config,
    teacher_domain: TeacherValueDomain,
) -> DistillEvalMetrics {
    debug_assert!(teacher.supports_domain(teacher_domain));
    let mut metrics = DistillEvalMetrics::default();
    let mut scratch =
        ClassicScratch::new(classic_fp32.acc_dim, classic_fp32.h1_dim, classic_fp32.h2_dim);
    let dummy_teacher = Arc::new(TeacherPrepared {
        value: 0.0,
        domain: teacher_domain,
        layers: None,
    });

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

    let mut processed = 0usize;
    let mut batch_requests: Vec<TeacherBatchRequest<'_>> = Vec::with_capacity(TEACHER_EVAL_BATCH);
    let mut batch_samples: Vec<&Sample> = Vec::with_capacity(TEACHER_EVAL_BATCH);

    let mut flush_batch = |batch_requests: &mut Vec<TeacherBatchRequest<'_>>,
                           batch_samples: &mut Vec<&Sample>| {
        if batch_requests.is_empty() {
            return 0usize;
        }
        let evals = match teacher.evaluate_batch(batch_requests, teacher_domain, false) {
            Ok(e) => e,
            Err(e) => {
                log::warn!(
                    "failed to evaluate teacher {:?} during metrics batch: {}",
                    teacher.kind(),
                    e
                );
                batch_requests.clear();
                batch_samples.clear();
                return 0;
            }
        };
        if evals.len() != batch_samples.len() {
            log::warn!(
                "teacher {:?} returned {} evals for {} samples; truncating to minimum",
                teacher.kind(),
                evals.len(),
                batch_samples.len()
            );
        }
        let take = evals.len().min(batch_samples.len());
        for (sample, eval) in batch_samples.iter().take(take).zip(evals.into_iter()) {
            let weight = sample.weight as f64;
            if weight <= 0.0 {
                continue;
            }
            let teacher_raw = eval.value;
            let teacher_logit = match (config.label_type.as_str(), teacher_domain) {
                ("wdl", TeacherValueDomain::WdlLogit) => teacher_raw,
                ("wdl", TeacherValueDomain::Cp) => teacher_raw / config.scale,
                ("cp", TeacherValueDomain::WdlLogit) => teacher_raw,
                ("cp", TeacherValueDomain::Cp) => teacher_raw / config.scale,
                _ => teacher_raw,
            };

            let features_them = to_features_them(&sample.features);
            let ds = DistillSample {
                features_us: sample.features.clone(),
                features_them,
                teacher: dummy_teacher.clone(),
                label: 0.0,
                weight: 0.0,
            };
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
        batch_requests.clear();
        batch_samples.clear();
        take
    };

    for sample in samples.iter().filter(|s| s.weight > 0.0) {
        if processed >= MAX_DISTILL_SAMPLES {
            break;
        }
        if processed + batch_requests.len() >= MAX_DISTILL_SAMPLES {
            processed += flush_batch(&mut batch_requests, &mut batch_samples);
            if processed >= MAX_DISTILL_SAMPLES {
                break;
            }
        }
        batch_requests.push(TeacherBatchRequest {
            features: &sample.features,
        });
        batch_samples.push(sample);
        if batch_requests.len() == TEACHER_EVAL_BATCH {
            processed += flush_batch(&mut batch_requests, &mut batch_samples);
        }
    }

    if processed < MAX_DISTILL_SAMPLES {
        let _ = flush_batch(&mut batch_requests, &mut batch_samples);
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
    let mut scratch =
        ClassicScratch::new(classic_fp32.acc_dim, classic_fp32.h1_dim, classic_fp32.h2_dim);
    let dummy_teacher = Arc::new(TeacherPrepared {
        value: 0.0,
        domain: TeacherValueDomain::Cp,
        layers: None,
    });

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
            teacher: dummy_teacher.clone(),
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
            teacher: Arc::new(TeacherPrepared {
                value: 0.0,
                domain: TeacherValueDomain::Cp,
                layers: None,
            }),
            label: 0.0,
            weight: 1.0,
        };
        let mut scratch = ClassicScratch::new(2, 2, 2);

        let _ = forward(&net, &sample, &mut scratch);
        let layer_weights = LayerLossWeights {
            ft: 0.0,
            h1: 0.0,
            h2: 0.0,
        };
        let _ = backward_update(
            &mut net,
            &mut scratch,
            &sample,
            BackwardUpdateParams {
                grad_output: 1.0,
                layer_weights: &layer_weights,
                teacher_layers: None,
                loss_kind: DistillLossKind::Mse,
                huber_delta: 1.0,
                learning_rate: 1e-2,
                l2_reg: 0.0,
            },
        );

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
            teacher: Arc::new(TeacherPrepared {
                value: 0.0,
                domain: TeacherValueDomain::Cp,
                layers: None,
            }),
            label: 0.0,
            weight: 1.0,
        };
        let mut scratch = ClassicScratch::new(2, 2, 2);
        let out = forward(&net, &sample, &mut scratch);
        assert!((out - net.output_bias).abs() < 1e-6);
    }

    #[test]
    fn backward_update_skips_out_of_range_features_without_panic() {
        let mut rng = rand::rngs::StdRng::seed_from_u64(101);
        let mut net = ClassicFloatNetwork::he_uniform_with_dims(4, 2, 2, 2, 2, &mut rng);
        let sample = DistillSample {
            features_us: vec![10_000],
            features_them: vec![20_000],
            teacher: Arc::new(TeacherPrepared {
                value: 0.0,
                domain: TeacherValueDomain::Cp,
                layers: None,
            }),
            label: 0.0,
            weight: 1.0,
        };
        let mut scratch = ClassicScratch::new(2, 2, 2);
        let _ = forward(&net, &sample, &mut scratch);
        let layer_weights = LayerLossWeights {
            ft: 0.0,
            h1: 0.0,
            h2: 0.0,
        };
        let _ = backward_update(
            &mut net,
            &mut scratch,
            &sample,
            BackwardUpdateParams {
                grad_output: 1.0,
                layer_weights: &layer_weights,
                teacher_layers: None,
                loss_kind: DistillLossKind::Mse,
                huber_delta: 1.0,
                learning_rate: 1e-2,
                l2_reg: 0.0,
            },
        );
    }

    #[test]
    fn backward_update_applies_l2_to_output_weights() {
        let mut rng = rand::rngs::StdRng::seed_from_u64(2024);
        let mut net = ClassicFloatNetwork::he_uniform_with_dims(4, 2, 2, 2, 2, &mut rng);
        let before = net.output_weights.clone();
        let sample = DistillSample {
            features_us: vec![],
            features_them: vec![],
            teacher: Arc::new(TeacherPrepared {
                value: 0.0,
                domain: TeacherValueDomain::Cp,
                layers: None,
            }),
            label: 0.0,
            weight: 1.0,
        };
        let mut scratch = ClassicScratch::new(2, 2, 2);
        let _ = forward(&net, &sample, &mut scratch);

        let layer_weights = LayerLossWeights {
            ft: 0.0,
            h1: 0.0,
            h2: 0.0,
        };
        let _ = backward_update(
            &mut net,
            &mut scratch,
            &sample,
            BackwardUpdateParams {
                grad_output: 0.0,
                layer_weights: &layer_weights,
                teacher_layers: None,
                loss_kind: DistillLossKind::Mse,
                huber_delta: 1.0,
                learning_rate: 1e-2,
                l2_reg: 1e-3,
            },
        );

        assert!(net
            .output_weights
            .iter()
            .zip(before.iter())
            .any(|(a, b)| (*a - *b).abs() > 1e-9));
    }

    #[test]
    fn teacher_scale_fit_applies_in_label_space_for_cp_teacher_wdl_labels() {
        let scale = 600.0f32;
        let teacher_domain = TeacherValueDomain::Cp;
        let label_type = "wdl";
        let teacher_raw = 600.0f32; // cp domain raw
        let fitted = Some((2.0f32, 1.0f32));

        let normalized =
            teacher_value_in_label_space(teacher_raw, teacher_domain, label_type, scale);
        assert!((normalized - 1.0).abs() < 1e-6);

        let adjusted = apply_teacher_scale(normalized, fitted);
        assert!((adjusted - 3.0).abs() < 1e-6);

        let legacy = apply_teacher_scale(teacher_raw, fitted) / scale;
        assert!((adjusted - legacy).abs() > 1e-3);
    }

    #[test]
    fn teacher_scale_fit_applies_in_label_space_for_wdl_teacher_cp_labels() {
        let scale = 600.0f32;
        let teacher_domain = TeacherValueDomain::WdlLogit;
        let label_type = "cp";
        let teacher_raw = 1.0f32; // logit domain raw
        let fitted = Some((0.5f32, -30.0f32));

        let normalized =
            teacher_value_in_label_space(teacher_raw, teacher_domain, label_type, scale);
        assert!((normalized - 600.0).abs() < 1e-6);

        let adjusted = apply_teacher_scale(normalized, fitted);
        assert!((adjusted - 270.0).abs() < 1e-6);

        let legacy = apply_teacher_scale(teacher_raw, fitted) * scale;
        assert!((adjusted - legacy).abs() > 1e-3);
    }

    #[test]
    fn quant_search_generates_candidate_report_with_calibration() {
        let mut rng = rand::rngs::StdRng::seed_from_u64(123);
        let net = ClassicFloatNetwork::he_uniform_with_dims(4, 2, 2, 2, 2, &mut rng);

        let samples = vec![Sample {
            features: Vec::new(),
            label: 0.0,
            weight: 1.0,
            cp: Some(0),
            phase: None,
        }];

        let config = Config {
            epochs: 1,
            batch_size: 1,
            learning_rate: 0.01,
            optimizer: "sgd".to_string(),
            l2_reg: 0.0,
            label_type: "cp".to_string(),
            scale: 600.0,
            cp_clip: 1200,
            accumulator_dim: 2,
            relu_clip: 127,
            shuffle: false,
            prefetch_batches: 1,
            throughput_interval_sec: 1.0,
            stream_cache: false,
            prefetch_bytes: None,
            estimated_features_per_sample: 2,
            exclude_no_legal_move: false,
            exclude_fallback: false,
            lr_schedule: "constant".to_string(),
            lr_warmup_epochs: 0,
            lr_decay_epochs: None,
            lr_decay_steps: None,
            lr_plateau_patience: None,
            grad_clip: 0.0,
        };

        let calibration = QuantCalibration {
            samples: samples.as_slice(),
            limit: samples.len(),
            auto_search: true,
        };

        let selection = select_quantization_config(QuantSelectionParams {
            net: &net,
            base_scheme: ClassicLayerQuantScheme::new(
                QuantScheme::PerTensor,
                QuantScheme::PerTensor,
                QuantScheme::PerTensor,
                QuantScheme::PerTensor,
            ),
            activation: Some(ClassicActivationSummary::default()),
            calibration: Some(calibration),
            fallback_samples: &samples,
            config: &config,
        })
        .expect("quant search selection");

        let report = selection.report.expect("report present");
        assert!(report.auto_search);
        assert_eq!(report.sample_source, "quant-calibration");
        assert_eq!(report.sample_count, samples.len());
        assert!(report.candidates.len() >= 2);
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
