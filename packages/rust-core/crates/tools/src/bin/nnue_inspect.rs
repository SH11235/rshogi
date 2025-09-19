use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;
use hex::encode as hex_encode;
use once_cell::sync::Lazy;
use regex::Regex;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tools::classic_roundtrip::ClassicQuantizationScalesData;

#[derive(Parser, Debug)]
#[command(author, version, about = "Inspect Classic NNUE network artifacts")]
struct Cli {
    /// Path to Classic integer network (e.g. nn.bin / nn.classic.nnue)
    #[arg(long, value_name = "FILE")]
    path: PathBuf,

    /// Optional path to quantization scales JSON (defaults to nn.classic.scales.json next to the network)
    #[arg(long, value_name = "FILE")]
    scales: Option<PathBuf>,

    /// Emit JSON instead of human-readable text
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Clone, Serialize)]
struct InspectReport {
    path: String,
    size_bytes: usize,
    format: FormatReport,
    header: HeaderReport,
    metadata: Option<MetadataReport>,
    dimensions: DimensionReport,
    weights: WeightSummary,
    scales: Option<ScalesReport>,
    sha256: String,
}

#[derive(Debug, Clone, Serialize)]
struct FormatReport {
    kind: String,
    description: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct HeaderReport {
    architecture_id: String,
    architecture_label: Option<String>,
    version_raw_u32: Option<String>,
    version_as_f32: Option<f32>,
    declared_size: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
struct MetadataReport {
    length: usize,
    text: String,
    parsed: Option<ParsedMetadata>,
}

#[derive(Debug, Clone, Serialize)]
struct ParsedMetadata {
    features_input_dim: Option<usize>,
    accumulators_per_side: Option<usize>,
    accumulator_sides: Option<usize>,
    hidden_layers: Vec<usize>,
}

#[derive(Debug, Clone, Serialize)]
struct DimensionReport {
    input_dim: usize,
    input_dim_from_metadata: Option<usize>,
    accumulators_per_side: usize,
    accumulator_sides: usize,
    hidden1: usize,
    hidden2: usize,
    output: usize,
    notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct WeightSummary {
    feature_transform: RangeSummary<i16>,
    feature_bias: RangeSummary<i32>,
    hidden1_weight: RangeSummary<i8>,
    hidden1_bias: RangeSummary<i32>,
    hidden2_weight: RangeSummary<i8>,
    hidden2_bias: RangeSummary<i32>,
    output_weight: RangeSummary<i8>,
    output_bias: RangeSummary<i32>,
}

#[derive(Debug, Clone, Serialize)]
struct RangeSummary<T> {
    min: T,
    max: T,
}

#[derive(Debug, Clone, Serialize)]
struct ScalesReport {
    schema_version: u32,
    format_version: String,
    arch: String,
    acc_dim: usize,
    h1_dim: usize,
    h2_dim: usize,
    input_dim: usize,
    bundle_sha256: String,
    quant_scheme: Option<QuantSchemeSummary>,
    s_w0: f32,
    s_w1: F32RangeSummary,
    s_w2: F32RangeSummary,
    s_w3: F32RangeSummary,
    s_in_1: f32,
    s_in_2: f32,
    s_in_3: f32,
}

#[derive(Debug, Clone, Serialize)]
struct QuantSchemeSummary {
    ft: Option<String>,
    h1: Option<String>,
    h2: Option<String>,
    out: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct F32RangeSummary {
    len: usize,
    min: f32,
    max: f32,
    any_non_finite: bool,
}

#[derive(Debug, Clone)]
struct NetworkShape {
    acc_dim: usize,
    hidden1_dim: usize,
    hidden2_dim: usize,
    output_dim: usize,
    sides: usize,
    metadata_input_dim: Option<usize>,
    metadata_hidden: Vec<usize>,
}

enum FormatKindInfo {
    Standard,
    Legacy,
}

struct FormatInfo {
    kind: FormatKindInfo,
    architecture: u32,
    version_u32: Option<u32>,
    version_as_f32: Option<f32>,
    declared_size: Option<u32>,
    metadata: Option<String>,
    payload_offset: usize,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    env_logger::init();

    let data = fs::read(&cli.path)
        .with_context(|| format!("failed to read NNUE network at {}", cli.path.display()))?;
    if data.len() < 16 {
        bail!("network file too small ({} bytes)", data.len());
    }

    let format_info = read_format_info(&data)?;

    let architecture_label = architecture_label(format_info.architecture).map(ToString::to_string);

    let mut shape = NetworkShape {
        acc_dim: 0,
        hidden1_dim: 0,
        hidden2_dim: 0,
        output_dim: 1,
        sides: 2,
        metadata_input_dim: None,
        metadata_hidden: Vec::new(),
    };

    let parsed_metadata = if let Some(text) = format_info.metadata.as_deref() {
        parse_metadata(text, &mut shape).transpose()?
    } else {
        None
    };

    fill_shape_defaults(&mut shape, format_info.architecture);

    let (scales_data, mut scale_read_notes) = read_scales_data(&cli)?;

    let weights = parse_weights(&data, format_info.payload_offset, &shape, scales_data.as_ref())?;

    let (scales_report, mut scale_notes) =
        build_scales_report(scales_data.as_ref(), &shape, &weights.dim);

    let mut dimension_notes = weights.notes;
    if let Some(note) = scale_read_notes.take() {
        dimension_notes.push(note);
    }
    dimension_notes.append(&mut scale_notes);

    if let Some(meta_input) = shape.metadata_input_dim {
        if meta_input != weights.dim.input_dim {
            dimension_notes.push(format!(
                "metadata input_dim {} differs from parsed {}",
                meta_input, weights.dim.input_dim
            ));
        }
    }
    let sha256 = {
        let mut hasher = Sha256::new();
        hasher.update(&data);
        hex_encode(hasher.finalize())
    };

    let dimensions = DimensionReport {
        input_dim: weights.dim.input_dim,
        input_dim_from_metadata: shape.metadata_input_dim,
        accumulators_per_side: weights.dim.acc_dim,
        accumulator_sides: shape.sides,
        hidden1: weights.dim.h1_dim,
        hidden2: weights.dim.h2_dim,
        output: weights.dim.output_dim,
        notes: dimension_notes,
    };

    let metadata_report = format_info.metadata.clone().map(|text| MetadataReport {
        length: text.len(),
        text,
        parsed: parsed_metadata,
    });

    let format_report = match format_info.kind {
        FormatKindInfo::Standard => FormatReport {
            kind: "standard".into(),
            description: Some("NNUEHeader v1".into()),
        },
        FormatKindInfo::Legacy => FormatReport {
            kind: "legacy".into(),
            description: Some("Classic v1 (Suisho nn.bin variant)".into()),
        },
    };

    let header_report = HeaderReport {
        architecture_id: format!("0x{:08X}", format_info.architecture),
        architecture_label,
        version_raw_u32: format_info.version_u32.map(|v| format!("0x{:08X}", v)),
        version_as_f32: format_info.version_as_f32,
        declared_size: format_info.declared_size,
    };

    let report = InspectReport {
        path: cli.path.display().to_string(),
        size_bytes: data.len(),
        format: format_report,
        header: header_report,
        metadata: metadata_report,
        dimensions,
        weights: weights.summary,
        scales: scales_report,
        sha256,
    };

    if cli.json {
        serde_json::to_writer_pretty(std::io::stdout(), &report)?;
        println!();
    } else {
        print_report(&report);
    }

    Ok(())
}

fn read_format_info(data: &[u8]) -> Result<FormatInfo> {
    if data.starts_with(b"NNUE") {
        let version = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let architecture = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
        let declared_size = u32::from_le_bytes([data[12], data[13], data[14], data[15]]);
        let mut payload_offset = 16;
        let mut metadata: Option<String> = None;

        if data.len() >= payload_offset + 4 {
            let len = u32::from_le_bytes([
                data[payload_offset],
                data[payload_offset + 1],
                data[payload_offset + 2],
                data[payload_offset + 3],
            ]) as usize;
            if len <= 1_048_576 && payload_offset + 4 + len <= data.len() {
                payload_offset += 4;
                let slice = &data[payload_offset..payload_offset + len];
                let text = if slice.iter().all(|b| (32..=126).contains(b)) {
                    String::from_utf8(slice.to_vec())
                        .unwrap_or_else(|_| String::from_utf8_lossy(slice).into_owned())
                } else {
                    String::from_utf8_lossy(slice).into_owned()
                };
                metadata = Some(text);
                payload_offset += len;
            }
        }

        Ok(FormatInfo {
            kind: FormatKindInfo::Standard,
            architecture,
            version_u32: Some(version),
            version_as_f32: None,
            declared_size: Some(declared_size),
            metadata,
            payload_offset,
        })
    } else {
        if data.len() < 12 {
            bail!("legacy header truncated ({} bytes)", data.len());
        }
        let architecture = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let raw_version = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let meta_len = u32::from_le_bytes([data[8], data[9], data[10], data[11]]) as usize;
        if 12 + meta_len > data.len() {
            bail!("legacy metadata length {} exceeds file size {}", meta_len, data.len());
        }
        let metadata_slice = &data[12..12 + meta_len];
        let metadata = String::from_utf8_lossy(metadata_slice).into_owned();
        Ok(FormatInfo {
            kind: FormatKindInfo::Legacy,
            architecture,
            version_u32: Some(raw_version),
            version_as_f32: Some(f32::from_bits(raw_version)),
            declared_size: None,
            metadata: Some(metadata),
            payload_offset: 12 + meta_len,
        })
    }
}

fn parse_metadata(text: &str, shape: &mut NetworkShape) -> Option<Result<ParsedMetadata>> {
    static RE_FEATURES: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"Features=\w+\([^)]*\)\[(\d+)->(\d+)x(\d+)\]").expect("valid regex")
    });
    static RE_RELU: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"ClippedReLU\[(\d+)\]").expect("valid regex"));

    let mut parsed = ParsedMetadata {
        features_input_dim: None,
        accumulators_per_side: None,
        accumulator_sides: None,
        hidden_layers: Vec::new(),
    };

    if let Some(cap) = RE_FEATURES.captures(text) {
        if let Ok(val) = cap[1].parse::<usize>() {
            parsed.features_input_dim = Some(val);
            shape.metadata_input_dim = Some(val);
        }
        if let Ok(val) = cap[2].parse::<usize>() {
            parsed.accumulators_per_side = Some(val);
            shape.acc_dim = val;
        }
        if let Ok(val) = cap[3].parse::<usize>() {
            parsed.accumulator_sides = Some(val);
            shape.sides = val;
        }
    }

    for cap in RE_RELU.captures_iter(text) {
        if let Ok(val) = cap[1].parse::<usize>() {
            parsed.hidden_layers.push(val);
        }
    }

    if !parsed.hidden_layers.is_empty() {
        shape.metadata_hidden = parsed.hidden_layers.clone();
        shape.hidden1_dim = parsed.hidden_layers.first().copied().unwrap_or(0);
        shape.hidden2_dim = parsed.hidden_layers.get(1).copied().unwrap_or(parsed.hidden_layers[0]);
    }

    Some(Ok(parsed))
}

fn fill_shape_defaults(shape: &mut NetworkShape, architecture: u32) {
    if let Some(defaults) = default_dims_for_arch(architecture) {
        if shape.acc_dim == 0 {
            shape.acc_dim = defaults.acc;
        }
        if shape.hidden1_dim == 0 {
            shape.hidden1_dim = defaults.h1;
        }
        if shape.hidden2_dim == 0 {
            shape.hidden2_dim = defaults.h2;
        }
        if shape.output_dim == 0 {
            shape.output_dim = defaults.output;
        }
        if shape.sides == 0 {
            shape.sides = defaults.sides;
        }
    }
}

struct DefaultDims {
    acc: usize,
    h1: usize,
    h2: usize,
    sides: usize,
    output: usize,
}

fn default_dims_for_arch(arch: u32) -> Option<DefaultDims> {
    match arch {
        0x7AF3_2F16 => Some(DefaultDims {
            acc: 256,
            h1: 32,
            h2: 32,
            sides: 2,
            output: 1,
        }),
        0xD15C_A11C => Some(DefaultDims {
            acc: 256,
            h1: 32,
            h2: 32,
            sides: 2,
            output: 1,
        }),
        _ => None,
    }
}

struct WeightParseResult {
    dim: WeightDims,
    summary: WeightSummary,
    notes: Vec<String>,
}

#[derive(Clone)]
struct WeightDims {
    input_dim: usize,
    acc_dim: usize,
    h1_dim: usize,
    h2_dim: usize,
    output_dim: usize,
}

fn parse_weights(
    data: &[u8],
    payload_offset: usize,
    shape: &NetworkShape,
    scales: Option<&ClassicQuantizationScalesData>,
) -> Result<WeightParseResult> {
    if payload_offset > data.len() {
        bail!("payload offset {} beyond file size {}", payload_offset, data.len());
    }

    let mut offset = payload_offset;
    let mut notes = Vec::new();

    let acc_dim = shape.acc_dim;
    let h1_dim = shape.hidden1_dim;
    let h2_dim = shape.hidden2_dim;
    let output_dim = shape.output_dim;
    let sides = shape.sides;

    if acc_dim == 0 || h1_dim == 0 || h2_dim == 0 || output_dim == 0 || sides == 0 {
        bail!("insufficient shape information to parse weights");
    }

    let remaining = data.len() - offset;
    let tail_bytes = acc_dim * 4
        + (acc_dim.checked_mul(sides).ok_or_else(|| anyhow!("acc_dim * sides overflow"))? * h1_dim)
        + (h1_dim * 4)
        + (h1_dim * h2_dim)
        + (h2_dim * 4)
        + (h2_dim * output_dim)
        + (output_dim * 4);
    if remaining < tail_bytes {
        bail!(
            "payload too small: remaining {} bytes, fixed tail requires {} bytes",
            remaining,
            tail_bytes
        );
    }

    let mut ft_weights_bytes = remaining - tail_bytes;
    if !ft_weights_bytes.is_multiple_of(2) {
        bail!(
            "feature transformer section has odd byte length {}; expected even",
            ft_weights_bytes
        );
    }
    let mut ft_weight_count = ft_weights_bytes / 2;
    let ft_divisor = acc_dim;
    if !ft_weight_count.is_multiple_of(ft_divisor) {
        if ft_weights_bytes >= 8 {
            let prelude = &data[offset..offset + 8];
            let reduced_bytes = ft_weights_bytes - 8;
            if reduced_bytes.is_multiple_of(2) {
                let reduced_count = reduced_bytes / 2;
                if reduced_count.is_multiple_of(ft_divisor) {
                    let tentative_input_dim = reduced_count / ft_divisor;
                    let word0 =
                        u32::from_le_bytes([prelude[0], prelude[1], prelude[2], prelude[3]]);
                    let word1 =
                        u32::from_le_bytes([prelude[4], prelude[5], prelude[6], prelude[7]]);
                    notes.push(format!(
                        "skipped 8-byte FT header words: 0x{word0:08X}, 0x{word1:08X}"
                    ));
                    if !input_dim_matches_hints(tentative_input_dim, shape, scales) {
                        notes.push(format!(
                            "derived input_dim {} differs from metadata/scales hints",
                            tentative_input_dim
                        ));
                    }
                    offset += 8;
                    ft_weights_bytes = reduced_bytes;
                    ft_weight_count = reduced_count;
                } else {
                    bail!(
                        "feature transformer weight count {} not divisible by acc_dim {}; corrupted network?",
                        reduced_count,
                        ft_divisor
                    );
                }
            } else {
                bail!(
                    "feature transformer weight section misaligned after potential header adjustment"
                );
            }
        } else {
            bail!(
                "feature transformer weight count {} not divisible by acc_dim {}; corrupted network?",
                ft_weight_count,
                ft_divisor
            );
        }
    }
    let input_dim = ft_weight_count / ft_divisor;

    let ft_range = summarize_i16(&data[offset..offset + ft_weights_bytes]);
    offset += ft_weights_bytes;

    let ft_bias_range = summarize_i32(&data[offset..offset + acc_dim * 4]);
    offset += acc_dim * 4;

    let h1_weight_len =
        acc_dim.checked_mul(sides).ok_or_else(|| anyhow!("acc_dim * sides overflow"))? * h1_dim;
    let h1_weight_range = summarize_i8(&data[offset..offset + h1_weight_len]);
    offset += h1_weight_len;

    let h1_bias_range = summarize_i32(&data[offset..offset + h1_dim * 4]);
    offset += h1_dim * 4;

    let h2_weight_range = summarize_i8(&data[offset..offset + h1_dim * h2_dim]);
    offset += h1_dim * h2_dim;

    let h2_bias_range = summarize_i32(&data[offset..offset + h2_dim * 4]);
    offset += h2_dim * 4;

    let out_weight_range = summarize_i8(&data[offset..offset + h2_dim * output_dim]);
    offset += h2_dim * output_dim;

    let out_bias_range = summarize_i32(&data[offset..offset + output_dim * 4]);
    offset += output_dim * 4;

    if offset != data.len() {
        bail!(
            "parsed {} bytes but file size is {}; leftover {} bytes",
            offset,
            data.len(),
            data.len() - offset
        );
    }

    let dims = WeightDims {
        input_dim,
        acc_dim,
        h1_dim,
        h2_dim,
        output_dim,
    };

    let summary = WeightSummary {
        feature_transform: ft_range,
        feature_bias: ft_bias_range,
        hidden1_weight: h1_weight_range,
        hidden1_bias: h1_bias_range,
        hidden2_weight: h2_weight_range,
        hidden2_bias: h2_bias_range,
        output_weight: out_weight_range,
        output_bias: out_bias_range,
    };

    Ok(WeightParseResult {
        dim: dims,
        summary,
        notes,
    })
}

fn input_dim_matches_hints(
    input_dim: usize,
    shape: &NetworkShape,
    scales: Option<&ClassicQuantizationScalesData>,
) -> bool {
    if shape.metadata_input_dim.is_none() && scales.is_none() {
        return true;
    }
    if let Some(meta) = shape.metadata_input_dim {
        if meta == input_dim {
            return true;
        }
    }
    if let Some(scales) = scales {
        if scales.input_dim == input_dim {
            return true;
        }
    }
    false
}

fn summarize_i16(data: &[u8]) -> RangeSummary<i16> {
    let mut min = i16::MAX;
    let mut max = i16::MIN;
    for chunk in data.chunks_exact(2) {
        let value = i16::from_le_bytes([chunk[0], chunk[1]]);
        if value < min {
            min = value;
        }
        if value > max {
            max = value;
        }
    }
    RangeSummary { min, max }
}

fn summarize_i32(data: &[u8]) -> RangeSummary<i32> {
    let mut min = i32::MAX;
    let mut max = i32::MIN;
    for chunk in data.chunks_exact(4) {
        let value = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        if value < min {
            min = value;
        }
        if value > max {
            max = value;
        }
    }
    RangeSummary { min, max }
}

fn summarize_i8(data: &[u8]) -> RangeSummary<i8> {
    let mut min = i8::MAX;
    let mut max = i8::MIN;
    for &byte in data {
        let value = byte as i8;
        if value < min {
            min = value;
        }
        if value > max {
            max = value;
        }
    }
    RangeSummary { min, max }
}

fn read_scales_data(cli: &Cli) -> Result<(Option<ClassicQuantizationScalesData>, Option<String>)> {
    let path = if let Some(p) = &cli.scales {
        p.clone()
    } else {
        default_scales_path(&cli.path)
    };

    if !path.exists() {
        return Ok((
            None,
            Some(format!("scales not found at {}; skipping scale summary", path.display())),
        ));
    }

    let file = fs::File::open(&path)
        .with_context(|| format!("failed to open scales file at {}", path.display()))?;
    let scales: ClassicQuantizationScalesData = serde_json::from_reader(file)
        .with_context(|| format!("failed to parse scales JSON at {}", path.display()))?;

    Ok((Some(scales), None))
}

fn build_scales_report(
    scales: Option<&ClassicQuantizationScalesData>,
    shape: &NetworkShape,
    dims: &WeightDims,
) -> (Option<ScalesReport>, Vec<String>) {
    let Some(scales) = scales else {
        return (None, Vec::new());
    };

    let quant_scheme = scales.quant_scheme.as_ref().map(|qs| QuantSchemeSummary {
        ft: qs.ft.clone(),
        h1: qs.h1.clone(),
        h2: qs.h2.clone(),
        out: qs.out.clone(),
    });

    let report = ScalesReport {
        schema_version: scales.schema_version,
        format_version: scales.format_version.clone(),
        arch: scales.arch.clone(),
        acc_dim: scales.acc_dim,
        h1_dim: scales.h1_dim,
        h2_dim: scales.h2_dim,
        input_dim: scales.input_dim,
        bundle_sha256: scales.bundle_sha256.clone(),
        quant_scheme,
        s_w0: scales.s_w0,
        s_w1: summarize_f32_vec(&scales.s_w1),
        s_w2: summarize_f32_vec(&scales.s_w2),
        s_w3: summarize_f32_vec(&scales.s_w3),
        s_in_1: scales.s_in_1,
        s_in_2: scales.s_in_2,
        s_in_3: scales.s_in_3,
    };

    let mut notes = Vec::new();
    if let Some(meta_input) = shape.metadata_input_dim {
        if meta_input != scales.input_dim {
            notes.push(format!(
                "metadata input_dim {} differs from scales input_dim {}",
                meta_input, scales.input_dim
            ));
        }
    }
    if dims.input_dim != scales.input_dim {
        notes.push(format!(
            "parsed input_dim {} differs from scales input_dim {}",
            dims.input_dim, scales.input_dim
        ));
    }
    if dims.acc_dim != scales.acc_dim {
        notes.push(format!(
            "parsed acc_dim {} differs from scales acc_dim {}",
            dims.acc_dim, scales.acc_dim
        ));
    }
    if dims.h1_dim != scales.h1_dim {
        notes.push(format!(
            "parsed h1_dim {} differs from scales h1_dim {}",
            dims.h1_dim, scales.h1_dim
        ));
    }
    if dims.h2_dim != scales.h2_dim {
        notes.push(format!(
            "parsed h2_dim {} differs from scales h2_dim {}",
            dims.h2_dim, scales.h2_dim
        ));
    }

    (Some(report), notes)
}

fn summarize_f32_vec(values: &[f32]) -> F32RangeSummary {
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    let mut any_non_finite = false;
    for &v in values {
        if !v.is_finite() {
            any_non_finite = true;
            continue;
        }
        if v < min {
            min = v;
        }
        if v > max {
            max = v;
        }
    }
    if min == f32::INFINITY {
        min = f32::NAN;
    }
    if max == f32::NEG_INFINITY {
        max = f32::NAN;
    }
    F32RangeSummary {
        len: values.len(),
        min,
        max,
        any_non_finite,
    }
}

fn default_scales_path(nnue_path: &Path) -> PathBuf {
    let mut candidate = nnue_path.to_path_buf();
    if let Some(parent) = candidate.parent() {
        if let Some(stem) = nnue_path.file_stem() {
            if stem == "nn" {
                return parent.join("nn.classic.scales.json");
            }
        }
    }
    candidate.set_file_name("nn.classic.scales.json");
    candidate
}

fn architecture_label(id: u32) -> Option<&'static str> {
    match id {
        0x7AF3_2F16 => Some("HALFKP_256X2_32_32_1"),
        0xD15C_A11C => Some("HALFKP_X2_DYNAMIC"),
        _ => None,
    }
}

fn print_report(report: &InspectReport) {
    println!("File: {} ({} bytes)", report.path, report.size_bytes);
    println!("Format: {}", report.format.kind);
    if let Some(desc) = &report.format.description {
        println!("  {}", desc);
    }
    println!("Architecture ID: {}", report.header.architecture_id);
    if let Some(label) = &report.header.architecture_label {
        println!("  Label: {}", label);
    }
    if let Some(version) = &report.header.version_raw_u32 {
        println!("Version (raw u32): {}", version);
    }
    if let Some(vf) = report.header.version_as_f32 {
        println!("  as f32: {:.6}", vf);
    }
    if let Some(size) = report.header.declared_size {
        println!("Declared size: {} bytes", size);
    }
    println!("SHA256: {}", report.sha256);

    println!("Dimensions:");
    println!(
        "  input_dim: {} (metadata: {:?})",
        report.dimensions.input_dim, report.dimensions.input_dim_from_metadata
    );
    println!(
        "  accumulators/side: {} (sides: {})",
        report.dimensions.accumulators_per_side, report.dimensions.accumulator_sides
    );
    println!("  hidden1: {}", report.dimensions.hidden1);
    println!("  hidden2: {}", report.dimensions.hidden2);
    println!("  output: {}", report.dimensions.output);
    for note in &report.dimensions.notes {
        println!("  note: {}", note);
    }

    println!("Weight ranges:");
    println!(
        "  ft weights: {} .. {}",
        report.weights.feature_transform.min, report.weights.feature_transform.max
    );
    println!(
        "  ft bias: {} .. {}",
        report.weights.feature_bias.min, report.weights.feature_bias.max
    );
    println!(
        "  h1 weights: {} .. {}",
        report.weights.hidden1_weight.min, report.weights.hidden1_weight.max
    );
    println!(
        "  h1 bias: {} .. {}",
        report.weights.hidden1_bias.min, report.weights.hidden1_bias.max
    );
    println!(
        "  h2 weights: {} .. {}",
        report.weights.hidden2_weight.min, report.weights.hidden2_weight.max
    );
    println!(
        "  h2 bias: {} .. {}",
        report.weights.hidden2_bias.min, report.weights.hidden2_bias.max
    );
    println!(
        "  out weights: {} .. {}",
        report.weights.output_weight.min, report.weights.output_weight.max
    );
    println!(
        "  out bias: {} .. {}",
        report.weights.output_bias.min, report.weights.output_bias.max
    );

    if let Some(scales) = &report.scales {
        println!("Scales ({}):", scales.as_tuple());
        println!(
            "  s_w0: {:.6} | s_in_1: {:.6} | s_in_2: {:.6} | s_in_3: {:.6}",
            scales.s_w0, scales.s_in_1, scales.s_in_2, scales.s_in_3
        );
        println!(
            "  s_w1 len={} range=[{:.6}, {:.6}] non_finite={}",
            scales.s_w1.len, scales.s_w1.min, scales.s_w1.max, scales.s_w1.any_non_finite
        );
        println!(
            "  s_w2 len={} range=[{:.6}, {:.6}] non_finite={}",
            scales.s_w2.len, scales.s_w2.min, scales.s_w2.max, scales.s_w2.any_non_finite
        );
        println!(
            "  s_w3 len={} range=[{:.6}, {:.6}] non_finite={}",
            scales.s_w3.len, scales.s_w3.min, scales.s_w3.max, scales.s_w3.any_non_finite
        );
        if let Some(qs) = &scales.quant_scheme {
            println!(
                "  quant_scheme: ft={:?} h1={:?} h2={:?} out={:?}",
                qs.ft, qs.h1, qs.h2, qs.out
            );
        }
    } else {
        println!("Scales: (not available)");
    }

    if let Some(meta) = &report.metadata {
        println!("Metadata ({} bytes):", meta.length);
        for line in wrap_metadata(&meta.text) {
            println!("  {}", line);
        }
        if let Some(parsed) = &meta.parsed {
            println!(
                "  Parsed: features={:?} acc_per_side={:?} sides={:?} hidden={:?}",
                parsed.features_input_dim,
                parsed.accumulators_per_side,
                parsed.accumulator_sides,
                parsed.hidden_layers
            );
        }
    }
}

fn wrap_metadata(text: &str) -> Vec<String> {
    const WIDTH: usize = 80;
    let mut out = Vec::new();
    let mut current = String::new();
    for token in text.split_whitespace() {
        if current.is_empty() {
            current.push_str(token);
        } else if current.len() + 1 + token.len() <= WIDTH {
            current.push(' ');
            current.push_str(token);
        } else {
            out.push(current);
            current = token.to_string();
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

trait ScalePretty {
    fn as_tuple(&self) -> String;
}

impl ScalePretty for ScalesReport {
    fn as_tuple(&self) -> String {
        format!(
            "schema={} format={} acc={} h1={} h2={} input={} bundle_sha256={}",
            self.schema_version,
            self.format_version,
            self.acc_dim,
            self.h1_dim,
            self.h2_dim,
            self.input_dim,
            self.bundle_sha256
        )
    }
}
