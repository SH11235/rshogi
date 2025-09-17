use crate::classic::{write_classic_v1_bundle, ClassicFloatNetwork, ClassicIntNetworkBundle};
use crate::model::Network;
use crate::params::{
    CLASSIC_ACC_DIM, CLASSIC_H1_DIM, CLASSIC_H2_DIM, KB_TO_MB_DIVISOR, PERCENTAGE_DIVISOR,
    QUANTIZATION_MAX, QUANTIZATION_METADATA_SIZE, QUANTIZATION_MIN,
};
use crate::types::{ArchKind, ExportFormat, ExportOptions};
use engine_core::evaluation::nnue::features::FE_END;
use engine_core::shogi::SHOGI_BOARD_SIZE;
use std::fs::File;
use std::io::Write;
use std::path::Path;

pub struct QuantizationParams {
    scale: f32,
    zero_point: i32,
}

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

pub fn save_network_quantized(
    network: &Network,
    path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::BufWriter;
    let mut file = BufWriter::new(File::create(path)?);

    writeln!(file, "NNUE")?;
    writeln!(file, "VERSION 3")?;
    writeln!(file, "FEATURES HALFKP")?;
    writeln!(file, "ACC_DIM {}", network.acc_dim)?;
    writeln!(file, "RELU_CLIP {}", network.relu_clip)?;
    writeln!(file, "FORMAT QUANTIZED_I8")?;
    writeln!(file, "END_HEADER")?;

    let params_w0 = QuantizationParams::from_weights(&network.w0);
    file.write_all(&params_w0.scale.to_le_bytes())?;
    file.write_all(&params_w0.zero_point.to_le_bytes())?;
    let quantized_w0 = quantize_weights(&network.w0, &params_w0);
    file.write_all(&quantized_w0.iter().map(|&x| x as u8).collect::<Vec<_>>())?;

    let params_b0 = QuantizationParams::from_weights(&network.b0);
    file.write_all(&params_b0.scale.to_le_bytes())?;
    file.write_all(&params_b0.zero_point.to_le_bytes())?;
    let quantized_b0 = quantize_weights(&network.b0, &params_b0);
    file.write_all(&quantized_b0.iter().map(|&x| x as u8).collect::<Vec<_>>())?;

    let params_w2 = QuantizationParams::from_weights(&network.w2);
    file.write_all(&params_w2.scale.to_le_bytes())?;
    file.write_all(&params_w2.zero_point.to_le_bytes())?;
    let quantized_w2 = quantize_weights(&network.w2, &params_w2);
    file.write_all(&quantized_w2.iter().map(|&x| x as u8).collect::<Vec<_>>())?;

    file.write_all(&network.b2.to_le_bytes())?;
    file.flush()?;

    let original_size = (network.w0.len() + network.b0.len() + network.w2.len() + 1) * 4;
    let quantized_size =
        (network.w0.len() + network.b0.len() + network.w2.len()) + QUANTIZATION_METADATA_SIZE;
    println!(
        "Quantized model saved. Size: {:.1} MB -> {:.1} MB ({:.1}% reduction)",
        original_size as f32 / KB_TO_MB_DIVISOR / KB_TO_MB_DIVISOR,
        quantized_size as f32 / KB_TO_MB_DIVISOR / KB_TO_MB_DIVISOR,
        (1.0 - quantized_size as f32 / original_size as f32) * PERCENTAGE_DIVISOR
    );

    Ok(())
}

pub fn save_network(network: &Network, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::BufWriter;
    let mut file = BufWriter::new(File::create(path)?);

    writeln!(file, "NNUE")?;
    writeln!(file, "VERSION 2")?;
    writeln!(file, "FEATURES HALFKP")?;
    writeln!(file, "ARCHITECTURE SINGLE_CHANNEL")?;
    writeln!(file, "ACC_DIM {}", network.acc_dim)?;
    writeln!(file, "RELU_CLIP {}", network.relu_clip)?;
    writeln!(file, "FEATURE_DIM {}", SHOGI_BOARD_SIZE * FE_END)?;
    writeln!(file, "END_HEADER")?;

    file.write_all(&(network.input_dim as u32).to_le_bytes())?;
    file.write_all(&(network.acc_dim as u32).to_le_bytes())?;

    for &w in &network.w0 {
        file.write_all(&w.to_le_bytes())?;
    }
    for &b in &network.b0 {
        file.write_all(&b.to_le_bytes())?;
    }
    for &w in &network.w2 {
        file.write_all(&w.to_le_bytes())?;
    }
    file.write_all(&network.b2.to_le_bytes())?;

    file.flush()?;
    Ok(())
}

pub fn finalize_export(
    network: &Network,
    out_dir: &Path,
    export: ExportOptions,
    emit_single_quant: bool,
    classic_bundle: Option<&ClassicIntNetworkBundle>,
) -> Result<(), Box<dyn std::error::Error>> {
    match export.format {
        ExportFormat::Fp32 => {
            save_network(network, &out_dir.join("nn.fp32.bin"))?;
            if emit_single_quant {
                save_network_quantized(network, &out_dir.join("nn.i8.bin"))?;
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
                    .map_err(|e| std::io::Error::other(e.to_string()))?;
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
                    .map_err(|e| std::io::Error::other(e.to_string()))?;
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
        }
    }
    Ok(())
}
