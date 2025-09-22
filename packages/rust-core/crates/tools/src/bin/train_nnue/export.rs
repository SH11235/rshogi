use crate::classic::{
    write_classic_v1_bundle, ClassicFloatNetwork, ClassicIntNetworkBundle,
    ClassicQuantizationScales,
};
use crate::distill::QuantEvalMetrics;
use crate::model::{ClassicNetwork, Network, SingleNetwork};
use crate::params::{
    CLASSIC_ACC_DIM, CLASSIC_H1_DIM, CLASSIC_H2_DIM, KB_TO_MB_DIVISOR, PERCENTAGE_DIVISOR,
    QUANTIZATION_MAX, QUANTIZATION_METADATA_SIZE, QUANTIZATION_MIN,
};
use crate::types::{ArchKind, ExportFormat, ExportOptions, QuantScheme};
use bytemuck::cast_slice;
use chrono::Utc;
use engine_core::evaluation::nnue::features::FE_END;
use engine_core::shogi::SHOGI_BOARD_SIZE;
use hex::encode as hex_encode;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Write;
use std::path::Path;

pub struct QuantizationParams {
    scale: f32,
    zero_point: i32,
}

impl QuantizationParams {
    pub fn from_weights(weights: &[f32]) -> Self {
        if weights.is_empty() {
            return Self {
                scale: 1.0,
                zero_point: 0,
            };
        }
        let min_val = weights.iter().fold(f32::INFINITY, |a, &b| a.min(b));
        let max_val = weights.iter().fold(f32::NEG_INFINITY, |a, &b| a.max(b));
        if !min_val.is_finite() || !max_val.is_finite() || (max_val - min_val).abs() < 1e-12 {
            return Self {
                scale: 1.0,
                zero_point: 0,
            };
        }
        let scale = (max_val - min_val) / 255.0;
        let zero_point = (-min_val / scale - 128.0).round() as i32;
        Self { scale, zero_point }
    }
}

fn quantize_weights(weights: &[f32], params: &QuantizationParams) -> Vec<i8> {
    weights
        .iter()
        .map(|&w| {
            let quantized = (w / params.scale + params.zero_point as f32).round();
            quantized.clamp(QUANTIZATION_MIN, QUANTIZATION_MAX) as i8
        })
        .collect()
}

fn expect_single<'a>(
    network: &'a Network,
    context: &str,
) -> Result<&'a SingleNetwork, std::io::Error> {
    network.as_single().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{context} は Single アーキテクチャでのみサポートされています"),
        )
    })
}

pub fn save_network_quantized(
    network: &Network,
    path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let single = expect_single(network, "量子化書き出し")?;
    use std::io::BufWriter;
    let mut file = BufWriter::new(File::create(path)?);

    writeln!(file, "NNUE")?;
    writeln!(file, "VERSION 3")?;
    writeln!(file, "FEATURES HALFKP")?;
    writeln!(file, "ACC_DIM {}", single.acc_dim)?;
    writeln!(file, "RELU_CLIP {}", single.relu_clip)?;
    writeln!(file, "FORMAT QUANTIZED_I8")?;
    writeln!(file, "END_HEADER")?;

    let params_w0 = QuantizationParams::from_weights(&single.w0);
    // scales/zero-points are little-endian f32/i32, followed by signed i8 payload (two's complement)
    file.write_all(&params_w0.scale.to_le_bytes())?;
    file.write_all(&params_w0.zero_point.to_le_bytes())?;
    let quantized_w0 = quantize_weights(&single.w0, &params_w0);
    file.write_all(cast_slice::<i8, u8>(&quantized_w0))?;

    let params_b0 = QuantizationParams::from_weights(&single.b0);
    file.write_all(&params_b0.scale.to_le_bytes())?;
    file.write_all(&params_b0.zero_point.to_le_bytes())?;
    let quantized_b0 = quantize_weights(&single.b0, &params_b0);
    file.write_all(cast_slice::<i8, u8>(&quantized_b0))?;

    let params_w2 = QuantizationParams::from_weights(&single.w2);
    file.write_all(&params_w2.scale.to_le_bytes())?;
    file.write_all(&params_w2.zero_point.to_le_bytes())?;
    let quantized_w2 = quantize_weights(&single.w2, &params_w2);
    file.write_all(cast_slice::<i8, u8>(&quantized_w2))?;

    file.write_all(&single.b2.to_le_bytes())?;
    file.flush()?;

    let original_size = (single.w0.len() + single.b0.len() + single.w2.len() + 1) * 4;
    let quantized_size =
        (single.w0.len() + single.b0.len() + single.w2.len()) + QUANTIZATION_METADATA_SIZE;
    println!(
        "Quantized model saved. Size: {:.1} MB -> {:.1} MB ({:.1}% reduction)",
        original_size as f32 / KB_TO_MB_DIVISOR / KB_TO_MB_DIVISOR,
        quantized_size as f32 / KB_TO_MB_DIVISOR / KB_TO_MB_DIVISOR,
        (1.0 - quantized_size as f32 / original_size as f32) * PERCENTAGE_DIVISOR
    );

    Ok(())
}

pub(crate) fn save_single_network(
    single: &SingleNetwork,
    path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::BufWriter;
    let mut file = BufWriter::new(File::create(path)?);

    writeln!(file, "NNUE")?;
    writeln!(file, "VERSION 2")?;
    writeln!(file, "FEATURES HALFKP")?;
    writeln!(file, "ARCHITECTURE SINGLE_CHANNEL")?;
    writeln!(file, "ACC_DIM {}", single.acc_dim)?;
    writeln!(file, "RELU_CLIP {}", single.relu_clip)?;
    writeln!(file, "FEATURE_DIM {}", SHOGI_BOARD_SIZE * FE_END)?;
    writeln!(file, "END_HEADER")?;

    file.write_all(&(single.input_dim as u32).to_le_bytes())?;
    file.write_all(&(single.acc_dim as u32).to_le_bytes())?;

    for &w in &single.w0 {
        file.write_all(&w.to_le_bytes())?;
    }
    for &b in &single.b0 {
        file.write_all(&b.to_le_bytes())?;
    }
    for &w in &single.w2 {
        file.write_all(&w.to_le_bytes())?;
    }
    file.write_all(&single.b2.to_le_bytes())?;

    file.flush()?;
    Ok(())
}

pub fn save_network(network: &Network, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    match network {
        Network::Single(single) => save_single_network(single, path),
        Network::Classic(classic) => save_classic_network(classic, path),
    }
}

pub(crate) fn save_classic_network(
    classic: &ClassicNetwork,
    path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::BufWriter;

    let fp32 = &classic.fp32;

    let mut file = BufWriter::new(File::create(path)?);

    writeln!(file, "NNUE")?;
    writeln!(file, "VERSION 1")?;
    writeln!(file, "FEATURES HALFKP")?;
    writeln!(file, "ARCHITECTURE CLASSIC")?;
    writeln!(file, "ACC_DIM {}", fp32.acc_dim)?;
    writeln!(file, "H1_DIM {}", fp32.h1_dim)?;
    writeln!(file, "H2_DIM {}", fp32.h2_dim)?;
    writeln!(file, "RELU_CLIP {}", classic.relu_clip)?;
    writeln!(file, "FEATURE_DIM {}", fp32.input_dim)?;
    writeln!(file, "END_HEADER")?;

    file.write_all(&(fp32.input_dim as u32).to_le_bytes())?;
    file.write_all(&(fp32.acc_dim as u32).to_le_bytes())?;
    file.write_all(&(fp32.h1_dim as u32).to_le_bytes())?;
    file.write_all(&(fp32.h2_dim as u32).to_le_bytes())?;

    for &w in &fp32.ft_weights {
        file.write_all(&w.to_le_bytes())?;
    }
    for &b in &fp32.ft_biases {
        file.write_all(&b.to_le_bytes())?;
    }
    for &w in &fp32.hidden1_weights {
        file.write_all(&w.to_le_bytes())?;
    }
    for &b in &fp32.hidden1_biases {
        file.write_all(&b.to_le_bytes())?;
    }
    for &w in &fp32.hidden2_weights {
        file.write_all(&w.to_le_bytes())?;
    }
    for &b in &fp32.hidden2_biases {
        file.write_all(&b.to_le_bytes())?;
    }
    for &w in &fp32.output_weights {
        file.write_all(&w.to_le_bytes())?;
    }
    file.write_all(&fp32.output_bias.to_le_bytes())?;

    file.flush()?;
    Ok(())
}

pub struct FinalizeExportParams<'a> {
    pub network: &'a Network,
    pub out_dir: &'a Path,
    pub export: ExportOptions,
    pub emit_single_quant: bool,
    pub classic_bundle: Option<&'a ClassicIntNetworkBundle>,
    pub classic_scales: Option<&'a ClassicQuantizationScales>,
    pub calibration_metrics: Option<&'a QuantEvalMetrics>,
    pub quant_metrics: Option<&'a QuantEvalMetrics>,
}

pub fn finalize_export(params: FinalizeExportParams<'_>) -> Result<(), Box<dyn std::error::Error>> {
    let FinalizeExportParams {
        network,
        out_dir,
        export,
        emit_single_quant,
        classic_bundle,
        classic_scales,
        calibration_metrics,
        quant_metrics,
    } = params;

    match export.format {
        ExportFormat::Fp32 => {
            save_network(network, &out_dir.join("nn.fp32.bin"))?;
            if emit_single_quant {
                match network {
                    Network::Single(_) => {
                        save_network_quantized(network, &out_dir.join("nn.i8.bin"))?;
                    }
                    Network::Classic(_) => {
                        log::warn!(
                            "--quantized is ignored when exporting fp32 for Classic architecture"
                        );
                    }
                }
            }
        }
        ExportFormat::SingleI8 => {
            save_network_quantized(network, &out_dir.join("nn.i8.bin"))?;
        }
        ExportFormat::ClassicV1 => {
            let fallback;
            let bundle = match (classic_bundle, export.arch) {
                (Some(b), _) => b,
                (None, ArchKind::Classic) => {
                    log::warn!(
                        "Classic export requested but no Classic bundle was produced; exporting zero weights"
                    );
                    fallback = ClassicFloatNetwork::zeros_with_dims(
                        SHOGI_BOARD_SIZE * FE_END,
                        CLASSIC_ACC_DIM,
                        CLASSIC_H1_DIM,
                        CLASSIC_H2_DIM,
                    )
                    .quantize_round()
                    .map_err(std::io::Error::other)?;
                    &fallback
                }
                (None, ArchKind::Single) => {
                    log::warn!(
                        "Classic export requested for single architecture; generating zero Classic weights"
                    );
                    fallback = ClassicFloatNetwork::zeros_with_dims(
                        SHOGI_BOARD_SIZE * FE_END,
                        CLASSIC_ACC_DIM,
                        CLASSIC_H1_DIM,
                        CLASSIC_H2_DIM,
                    )
                    .quantize_round()
                    .map_err(std::io::Error::other)?;
                    &fallback
                }
            };

            let ser = bundle.as_serialized();
            if ser.acc_dim != CLASSIC_ACC_DIM
                || ser.h1_dim != CLASSIC_H1_DIM
                || ser.h2_dim != CLASSIC_H2_DIM
                || ser.input_dim != SHOGI_BOARD_SIZE * FE_END
            {
                log::warn!(
                    "Classic bundle dimensions unexpected (acc_dim={}, h1_dim={}, h2_dim={}, input_dim={})",
                    ser.acc_dim,
                    ser.h1_dim,
                    ser.h2_dim,
                    ser.input_dim
                );
            }
            write_classic_v1_bundle(&out_dir.join("nn.classic.nnue"), bundle)?;

            if export.emit_fp32_also {
                match network {
                    Network::Classic(classic) => {
                        save_classic_network(classic, &out_dir.join("nn.fp32.bin"))?;
                    }
                    Network::Single(_) => {
                        log::warn!(
                            "--emit-fp32-also specified but export arch {:?} is not Classic; skipping nn.fp32.bin emission",
                            export.arch
                        );
                    }
                }
            }

            if let Some(scales) = classic_scales {
                if let Err(e) = write_classic_scales_json(
                    out_dir,
                    bundle,
                    scales,
                    &export,
                    calibration_metrics,
                    quant_metrics,
                ) {
                    log::warn!("Failed to write nn.classic.scales.json: {}", e);
                }
            } else {
                log::info!(
                    "Classic export completed without quantization scales; nn.classic.scales.json was not emitted"
                );
            }
        }
    }
    Ok(())
}

#[derive(Serialize)]
struct ClassicScalesArtifact {
    schema_version: u32,
    format_version: &'static str,
    arch: String,
    generated_at_utc: String,
    acc_dim: usize,
    h1_dim: usize,
    h2_dim: usize,
    input_dim: usize,
    s_w0: f32,
    s_w1: Vec<f32>,
    s_w2: Vec<f32>,
    s_w3: Vec<f32>,
    s_in_1: f32,
    s_in_2: f32,
    s_in_3: f32,
    bundle_sha256: String,
    quant_scheme: QuantSchemeReport,
    activation: Option<ClassicActivationSummaryArtifact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    calibration_metrics: Option<QuantMetricsArtifact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    eval_metrics: Option<QuantMetricsArtifact>,
}

#[derive(Serialize)]
struct QuantSchemeReport {
    ft: &'static str,
    h1: &'static str,
    h2: &'static str,
    out: &'static str,
}

#[derive(Serialize)]
struct ClassicActivationSummaryArtifact {
    ft_max_abs: f32,
    h1_max_abs: f32,
    h2_max_abs: f32,
}

#[derive(Serialize)]
struct QuantMetricsArtifact {
    n: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    mae_cp: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    p95_cp: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_cp: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mae_logit: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    p95_logit: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_logit: Option<f32>,
}

fn write_classic_scales_json(
    out_dir: &Path,
    bundle: &ClassicIntNetworkBundle,
    scales: &ClassicQuantizationScales,
    _export: &ExportOptions,
    calibration_metrics: Option<&QuantEvalMetrics>,
    eval_metrics: Option<&QuantEvalMetrics>,
) -> Result<(), Box<dyn std::error::Error>> {
    let serialized = bundle.as_serialized();
    if serialized.acc_dim != CLASSIC_ACC_DIM
        || serialized.h1_dim != CLASSIC_H1_DIM
        || serialized.h2_dim != CLASSIC_H2_DIM
    {
        log::warn!(
            "Classic bundle dims mismatch (acc={}, h1={}, h2={})",
            serialized.acc_dim,
            serialized.h1_dim,
            serialized.h2_dim
        );
    }
    if serialized.input_dim != SHOGI_BOARD_SIZE * FE_END {
        log::warn!(
            "Classic bundle input_dim mismatch ({} != {})",
            serialized.input_dim,
            SHOGI_BOARD_SIZE * FE_END
        );
    }
    let arch_string =
        format!("HALFKP_{}X2_{}_{}", serialized.acc_dim, serialized.h1_dim, serialized.h2_dim);
    let payload = ClassicScalesArtifact {
        schema_version: 1,
        format_version: "classic-v1",
        arch: arch_string,
        generated_at_utc: Utc::now().to_rfc3339(),
        acc_dim: serialized.acc_dim,
        h1_dim: serialized.h1_dim,
        h2_dim: serialized.h2_dim,
        input_dim: SHOGI_BOARD_SIZE * FE_END,
        s_w0: scales.s_w0,
        s_w1: scales.s_w1.clone(),
        s_w2: scales.s_w2.clone(),
        s_w3: scales.s_w3.clone(),
        s_in_1: scales.s_in_1,
        s_in_2: scales.s_in_2,
        s_in_3: scales.s_in_3,
        bundle_sha256: bundle_sha256(bundle),
        quant_scheme: QuantSchemeReport {
            ft: quant_scheme_label(scales.scheme.ft),
            h1: quant_scheme_label(scales.scheme.h1),
            h2: quant_scheme_label(scales.scheme.h2),
            out: quant_scheme_label(scales.scheme.out),
        },
        activation: scales.activation.map(|summary| ClassicActivationSummaryArtifact {
            ft_max_abs: summary.ft_max_abs,
            h1_max_abs: summary.h1_max_abs,
            h2_max_abs: summary.h2_max_abs,
        }),
        calibration_metrics: calibration_metrics.and_then(make_metrics_artifact),
        eval_metrics: eval_metrics.and_then(make_metrics_artifact),
    };

    let path = out_dir.join("nn.classic.scales.json");
    let file = File::create(path)?;
    serde_json::to_writer_pretty(file, &payload)?;
    Ok(())
}

fn make_metrics_artifact(metrics: &QuantEvalMetrics) -> Option<QuantMetricsArtifact> {
    if metrics.n == 0 {
        return None;
    }
    Some(QuantMetricsArtifact {
        n: metrics.n,
        mae_cp: metrics.mae_cp,
        p95_cp: metrics.p95_cp,
        max_cp: metrics.max_cp,
        mae_logit: metrics.mae_logit,
        p95_logit: metrics.p95_logit,
        max_logit: metrics.max_logit,
    })
}

fn quant_scheme_label(q: QuantScheme) -> &'static str {
    match q {
        QuantScheme::PerTensor => "per-tensor",
        QuantScheme::PerChannel => "per-channel",
    }
}

fn bundle_sha256(bundle: &ClassicIntNetworkBundle) -> String {
    let mut hasher = Sha256::new();

    for &w in &bundle.transformer.weights {
        hasher.update(w.to_le_bytes());
    }
    for &b in &bundle.transformer.biases {
        hasher.update(b.to_le_bytes());
    }
    hasher.update(cast_slice::<i8, u8>(&bundle.network.hidden1_weights));
    for &b in &bundle.network.hidden1_biases {
        hasher.update(b.to_le_bytes());
    }
    hasher.update(cast_slice::<i8, u8>(&bundle.network.hidden2_weights));
    for &b in &bundle.network.hidden2_biases {
        hasher.update(b.to_le_bytes());
    }
    hasher.update(cast_slice::<i8, u8>(&bundle.network.output_weights));
    hasher.update(bundle.network.output_bias.to_le_bytes());

    hex_encode(hasher.finalize())
}
