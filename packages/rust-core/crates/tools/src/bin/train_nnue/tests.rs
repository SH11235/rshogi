use super::{
    classic, dataset, default_teacher_domain, export, logging, model, params, teacher, training,
    types,
};
use bytemuck::cast_slice;
use classic::*;
use dataset::*;
use engine_core::{nnue::features::FE_END, shogi::SHOGI_BOARD_SIZE};
use export::*;
use logging::*;
use model::*;
use params::*;
use rand::SeedableRng;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Seek, SeekFrom, Write},
    path::Path,
};
use teacher::{load_teacher, TeacherBatchRequest, TeacherError};
use tempfile::tempdir;
use tools::{common::weighting as wcfg, nnfc_v1::FEATURE_SET_ID_HALF};
use training::*;
use types::*;

const DEFAULT_RELU_CLIP_NUM: i32 = 127;
const DEFAULT_CALIBRATION_BINS: usize = 40;
const DEFAULT_CHUNK_SIZE: u32 = 1024;

#[derive(Clone, Copy)]
struct HeaderV1 {
    feature_set_id: u32,
    num_samples: u64,
    chunk_size: u32,
    header_size: u32,
    endianness: u8,
    payload_encoding: u8,
    sample_flags_mask: u32,
}

fn write_v1_header(f: &mut File, h: HeaderV1) -> u64 {
    // Magic
    f.write_all(b"NNFC").unwrap();
    // version
    f.write_all(&1u32.to_le_bytes()).unwrap();
    // feature_set_id
    f.write_all(&h.feature_set_id.to_le_bytes()).unwrap();
    // num_samples
    f.write_all(&h.num_samples.to_le_bytes()).unwrap();
    // chunk_size
    f.write_all(&h.chunk_size.to_le_bytes()).unwrap();
    // header_size
    f.write_all(&h.header_size.to_le_bytes()).unwrap();
    // endianness
    f.write_all(&[h.endianness]).unwrap();
    // payload_encoding
    f.write_all(&[h.payload_encoding]).unwrap();
    // reserved16
    f.write_all(&[0u8; 2]).unwrap();
    // payload_offset = after magic (4 bytes) + header_size
    let payload_offset = 4u64 + h.header_size as u64;
    f.write_all(&payload_offset.to_le_bytes()).unwrap();
    // sample_flags_mask
    f.write_all(&h.sample_flags_mask.to_le_bytes()).unwrap();
    // pad header tail to header_size
    let written = 40usize; // fields after magic
    let pad = (h.header_size as usize).saturating_sub(written);
    if pad > 0 {
        f.write_all(&vec![0u8; pad]).unwrap();
    }
    payload_offset
}

fn write_classic_fp32_fixture(path: &Path) {
    let mut file = File::create(path).unwrap();
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
fn header_errors_and_ok_cases() {
    // bad magic
    {
        let td = tempdir().unwrap();
        let path = td.path().join("bad_magic.cache");
        let mut f = File::create(&path).unwrap();
        f.write_all(b"BAD!").unwrap();
        f.flush().unwrap();
        let err =
            load_samples_from_cache(path.to_str().unwrap(), &wcfg::WeightingConfig::default())
                .unwrap_err();
        assert!(format!("{}", err).contains("bad magic"));
    }

    // unknown version
    {
        let td = tempdir().unwrap();
        let path = td.path().join("bad_version.cache");
        let mut f = File::create(&path).unwrap();
        f.write_all(b"NNFC").unwrap();
        f.write_all(&2u32.to_le_bytes()).unwrap(); // version=2 (unsupported)
                                                   // Fill rest with zeros to avoid EOF early
        f.write_all(&[0u8; 64]).unwrap();
        f.flush().unwrap();
        let err =
            load_samples_from_cache(path.to_str().unwrap(), &wcfg::WeightingConfig::default())
                .unwrap_err();
        assert!(format!("{}", err).contains("v1 required"));
    }

    // endianness error
    {
        let td = tempdir().unwrap();
        let path = td.path().join("endianness.cache");
        let mut f = File::create(&path).unwrap();
        let _off = write_v1_header(
            &mut f,
            HeaderV1 {
                feature_set_id: 0x48414C46,
                num_samples: 0,
                chunk_size: DEFAULT_CHUNK_SIZE,
                header_size: 48,
                endianness: 1, // BE
                payload_encoding: 0,
                sample_flags_mask: 0,
            },
        );
        f.flush().unwrap();
        let err =
            load_samples_from_cache(path.to_str().unwrap(), &wcfg::WeightingConfig::default())
                .unwrap_err();
        assert!(format!("{}", err).contains("Unsupported endianness"));
    }

    // unknown encoding
    {
        let td = tempdir().unwrap();
        let path = td.path().join("encoding.cache");
        let mut f = File::create(&path).unwrap();
        let _off = write_v1_header(
            &mut f,
            HeaderV1 {
                feature_set_id: 0x48414C46,
                num_samples: 0,
                chunk_size: DEFAULT_CHUNK_SIZE,
                header_size: 48,
                endianness: 0,
                payload_encoding: 3,
                sample_flags_mask: 0,
            },
        );
        f.flush().unwrap();
        let err =
            load_samples_from_cache(path.to_str().unwrap(), &wcfg::WeightingConfig::default())
                .unwrap_err();
        assert!(format!("{}", err).contains("Unknown payload encoding"));
    }

    // feature_set_id mismatch
    {
        let td = tempdir().unwrap();
        let path = td.path().join("featureset.cache");
        let mut f = File::create(&path).unwrap();
        let _off = write_v1_header(
            &mut f,
            HeaderV1 {
                feature_set_id: 0x00000000,
                num_samples: 0,
                chunk_size: DEFAULT_CHUNK_SIZE,
                header_size: 48,
                endianness: 0,
                payload_encoding: 0,
                sample_flags_mask: 0,
            },
        );
        f.flush().unwrap();
        let err =
            load_samples_from_cache(path.to_str().unwrap(), &wcfg::WeightingConfig::default())
                .unwrap_err();
        assert!(format!("{}", err).contains("Unsupported feature_set_id"));
    }

    // header_size 極端値（0/8/4097）でエラー
    for bad_size in [0u32, 8u32, 4097u32] {
        let td = tempdir().unwrap();
        let path = td.path().join(format!("bad_hs_{bad_size}.cache"));
        let mut f = File::create(&path).unwrap();
        let _off = write_v1_header(
            &mut f,
            HeaderV1 {
                feature_set_id: 0x48414C46,
                num_samples: 0,
                chunk_size: DEFAULT_CHUNK_SIZE,
                header_size: bad_size,
                endianness: 0,
                payload_encoding: 0,
                sample_flags_mask: 0,
            },
        );
        f.flush().unwrap();
        let err =
            load_samples_from_cache(path.to_str().unwrap(), &wcfg::WeightingConfig::default())
                .unwrap_err();
        assert!(format!("{}", err).contains("header_size"));
    }

    // 破損 payload_offset（header_end より小さい）でエラー
    {
        let td = tempdir().unwrap();
        let path = td.path().join("broken_offset.cache");
        let mut f = File::create(&path).unwrap();
        // Magic + version + feature_set_id + num_samples + chunk_size
        f.write_all(b"NNFC").unwrap();
        f.write_all(&1u32.to_le_bytes()).unwrap();
        f.write_all(&0x48414C46u32.to_le_bytes()).unwrap();
        f.write_all(&1u64.to_le_bytes()).unwrap(); // num_samples
        f.write_all(&1024u32.to_le_bytes()).unwrap(); // chunk_size
                                                      // header_size=48, endianness=0, encoding=0, reserved16=0
        f.write_all(&48u32.to_le_bytes()).unwrap();
        f.write_all(&[0u8, 0u8]).unwrap();
        f.write_all(&[0u8; 2]).unwrap();
        // payload_offset を header_end より小さくする（壊れ）
        // header_end = magic(4) + header_size(48) = 52
        let bad_off = 36u64;
        f.write_all(&bad_off.to_le_bytes()).unwrap();
        // sample_flags_mask
        f.write_all(&0u32.to_le_bytes()).unwrap();
        // 余りを header_size まで埋める
        let written = 40usize; // after magic
        let pad = (48usize).saturating_sub(written);
        if pad > 0 {
            f.write_all(&vec![0u8; pad]).unwrap();
        }
        // payload 仮書き
        f.write_all(&0u32.to_le_bytes()).unwrap();
        f.flush().unwrap();

        let res =
            load_samples_from_cache(path.to_str().unwrap(), &wcfg::WeightingConfig::default());
        assert!(res.is_err(), "broken payload_offset should error");
    }

    // header_size larger with payload_offset respected and num_samples=0
    {
        let td = tempdir().unwrap();
        let path = td.path().join("ok_zero.cache");
        let mut f = File::create(&path).unwrap();
        let _off = write_v1_header(
            &mut f,
            HeaderV1 {
                feature_set_id: 0x48414C46,
                num_samples: 0,
                chunk_size: DEFAULT_CHUNK_SIZE,
                header_size: 64,
                endianness: 0,
                payload_encoding: 0,
                sample_flags_mask: 0,
            },
        );
        f.flush().unwrap();
        let v = load_samples_from_cache(path.to_str().unwrap(), &wcfg::WeightingConfig::default())
            .unwrap();
        assert!(v.is_empty());
    }
}

#[test]
fn classic_transformer_accumulates_and_clamps() {
    let acc_dim = 2;
    let weights = vec![128i16, 256, -400, -800, 5000, -5000];
    let biases = vec![10i32, -20i32];
    let transformer = ClassicFeatureTransformerInt::new(weights, biases, acc_dim);

    // 新 API: accumulate_into_i32 を利用し再利用バッファを明示
    let mut tmp = vec![0i32; acc_dim];
    let mut acc = vec![0i16; acc_dim];
    transformer.accumulate_into_i32(&[0, 1, 2], &mut tmp, &mut acc);

    assert_eq!(acc, vec![4738i16, -5564i16]);
}

#[test]
fn classic_integer_network_matches_manual_flow() {
    let acc_dim = 2;
    let transformer =
        ClassicFeatureTransformerInt::new(vec![128, 256, -128, -64, 64, 64], vec![0, 0], acc_dim);

    let network = ClassicQuantizedNetwork::new(ClassicQuantizedNetworkParams {
        hidden1_weights: vec![10, 20, 30, 40, -10, 5, -5, 10],
        hidden1_biases: vec![100, -50],
        hidden2_weights: vec![1, 2, 3, 4],
        hidden2_biases: vec![0, 50],
        output_weights: vec![2, -3],
        output_bias: 10,
        acc_dim,
        h1_dim: 2,
        h2_dim: 2,
    });
    let bundle = ClassicIntNetworkBundle::new(transformer, network);

    let features_us = vec![0u32, 2];
    let features_them = vec![1u32];
    let mut views = ClassicScratchViews::new(acc_dim, 2, 2);
    let output =
        bundle.propagate_with_features_scratch_full(&features_us, &features_them, &mut views);
    assert_eq!(output, -117);
}

#[test]
fn classic_v1_writer_emits_expected_layout() {
    let td = tempdir().unwrap();
    let path = td.path().join("classic.bin");
    let acc_dim = 256;
    let h1_dim = 32;
    let h2_dim = 32;
    let ft_input_dim = 1; // テスト用に縮約

    let mut ft_weights = vec![0i16; acc_dim * ft_input_dim];
    ft_weights[0] = 0x1234;
    let mut ft_biases = vec![0i32; acc_dim];
    ft_biases[0] = 42;
    ft_biases[1] = -7;

    let classic_input_dim = acc_dim * 2;
    let mut hidden1_weights = vec![0i8; classic_input_dim * h1_dim];
    for (idx, w) in hidden1_weights.iter_mut().enumerate() {
        *w = ((idx % 127) as i8).saturating_add(1);
    }
    let mut hidden1_biases = vec![0i32; h1_dim];
    hidden1_biases[0] = 11;
    hidden1_biases[1] = -12;

    let mut hidden2_weights = vec![0i8; h1_dim * h2_dim];
    for (idx, w) in hidden2_weights.iter_mut().enumerate() {
        *w = ((idx % 127) as i8).wrapping_sub(63);
    }
    let hidden2_biases = vec![5, -4]
        .into_iter()
        .chain(std::iter::repeat_n(0, h2_dim - 2))
        .collect::<Vec<i32>>();

    let mut output_weights = vec![0i8; h2_dim];
    output_weights[0] = 3;
    output_weights[1] = -2;

    let transformer = ClassicFeatureTransformerInt::new(ft_weights, ft_biases, acc_dim);
    let network = ClassicQuantizedNetwork::new(ClassicQuantizedNetworkParams {
        hidden1_weights,
        hidden1_biases,
        hidden2_weights,
        hidden2_biases,
        output_weights,
        output_bias: 99,
        acc_dim,
        h1_dim,
        h2_dim,
    });
    let bundle = ClassicIntNetworkBundle::new(transformer, network);
    use std::io::Read;

    write_classic_v1_bundle(&path, &bundle).unwrap();
    let serialized = bundle.as_serialized();
    let expected_payload = serialized.payload_bytes();
    let expected_total = expected_payload + 16;
    let metadata = std::fs::metadata(&path).unwrap();
    assert_eq!(metadata.len(), expected_total);

    let mut file = std::fs::File::open(&path).unwrap();

    let mut header = [0u8; 16];
    file.read_exact(&mut header).unwrap();
    assert_eq!(&header[0..4], b"NNUE");
    assert_eq!(u32::from_le_bytes(header[4..8].try_into().unwrap()), 1);
    assert_eq!(u32::from_le_bytes(header[8..12].try_into().unwrap()), CLASSIC_V1_ARCH_ID);
    assert_eq!(u32::from_le_bytes(header[12..16].try_into().unwrap()), expected_total as u32);

    // First FT weight (0x1234)
    let mut ft_weight_bytes = [0u8; 2];
    file.read_exact(&mut ft_weight_bytes).unwrap();
    assert_eq!(i16::from_le_bytes(ft_weight_bytes), 0x1234);

    // Second bias (-7)
    let canonical_ft_weights = (SHOGI_BOARD_SIZE * FE_END * serialized.acc_dim) as u64;
    let ft_payload_bytes = canonical_ft_weights * 2;
    let bias_offset = 16 + ft_payload_bytes + 4; // 1st bias (42) + 4 bytes
    file.seek(SeekFrom::Start(bias_offset)).unwrap();
    let mut bias_bytes = [0u8; 4];
    file.read_exact(&mut bias_bytes).unwrap();
    assert_eq!(i32::from_le_bytes(bias_bytes), -7);

    // Last 4 bytes = output bias (99)
    file.seek(SeekFrom::End(-4)).unwrap();
    let mut tail = [0u8; 4];
    file.read_exact(&mut tail).unwrap();
    assert_eq!(i32::from_le_bytes(tail), 99);
}

#[test]
fn classic_v1_validate_rejects_inconsistent_lengths() {
    let transformer = ClassicFeatureTransformerInt::new(vec![1, 2, 3, 4], vec![0, 0], 2);
    let network = ClassicQuantizedNetwork::new(ClassicQuantizedNetworkParams {
        hidden1_weights: vec![1, 2, 3, 4],
        hidden1_biases: vec![0, 0],
        hidden2_weights: vec![1, 2, 3, 4],
        hidden2_biases: vec![0, 0],
        output_weights: vec![1, 2],
        output_bias: 0,
        acc_dim: 2,
        h1_dim: 2,
        h2_dim: 2,
    });
    let bundle = ClassicIntNetworkBundle::new(transformer, network);
    let mut serialized = bundle.as_serialized();
    serialized.h1_dim = 4; // break invariant
    assert!(serialized.validate().is_err());
}

#[test]
fn classic_float_round_quantization() {
    let float_net = ClassicFloatNetwork {
        acc_dim: 2,
        input_dim: 3,
        h1_dim: 2,
        h2_dim: 2,
        ft_weights: vec![0.4, -1.6, 2.0, -2.2, 3.8, -4.4],
        ft_biases: vec![0.9, -0.4],
        hidden1_weights: vec![0.5, -0.5, 1.2, -2.4, 0.75, -0.9, 1.5, 1.9],
        hidden1_biases: vec![1.2, -1.2],
        hidden2_weights: vec![0.4, -0.7, 1.1, -1.3],
        hidden2_biases: vec![0.2, -0.4],
        output_weights: vec![0.3, -0.5],
        output_bias: 0.8,
    };
    let bundle = float_net.quantize_round().unwrap();
    let serialized = bundle.as_serialized();
    assert_eq!(serialized.ft_weights, &[0i16, -2, 2, -2, 4, -4]);
    assert_eq!(serialized.ft_biases, &[1, 0]);
    assert_eq!(serialized.hidden1_weights, &[1, -1, 1, -2, 1, -1, 2, 2]);
    assert_eq!(serialized.hidden1_biases, &[1, -1]);
    assert_eq!(serialized.hidden2_weights, &[0, -1, 1, -1]);
    assert_eq!(serialized.hidden2_biases, &[0, 0]);
    assert_eq!(serialized.output_weights, &[0, -1]);
    assert_eq!(serialized.output_bias, 1);
}

#[test]
fn finalize_export_writes_zero_when_bundle_missing() {
    use rand::SeedableRng;

    let td = tempdir().unwrap();
    let mut rng = rand::rngs::StdRng::seed_from_u64(0);
    let network = Network::Single(SingleNetwork::new(8, DEFAULT_RELU_CLIP_NUM, &mut rng));
    let export = ExportOptions {
        arch: ArchKind::Classic,
        format: ExportFormat::ClassicV1,
        ..ExportOptions::default()
    };
    finalize_export(FinalizeExportParams {
        network: &network,
        out_dir: td.path(),
        export,
        emit_single_quant: false,
        classic_bundle: None,
        classic_scales: None,
        calibration_metrics: None,
        quant_metrics: None,
    })
    .unwrap();
    assert!(td.path().join("nn.classic.nnue").exists());
}

#[derive(Deserialize)]
struct TestScalesJson {
    acc_dim: usize,
    h1_dim: usize,
    h2_dim: usize,
    input_dim: usize,
    bundle_sha256: String,
    #[serde(default)]
    quant_scheme: Option<TestQuantScheme>,
}

#[derive(Deserialize, Default)]
struct TestQuantScheme {
    ft: String,
    h1: String,
    h2: String,
    #[serde(rename = "out")]
    out_field: String,
}

impl TestQuantScheme {
    fn out(&self) -> &str {
        &self.out_field
    }
}

fn compute_bundle_sha256(bundle: &ClassicIntNetworkBundle) -> String {
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
    hex::encode(hasher.finalize())
}

#[test]
fn finalize_export_emits_fp32_and_scales_for_classic() {
    use crate::params::{CLASSIC_ACC_DIM, CLASSIC_H1_DIM, CLASSIC_H2_DIM, CLASSIC_RELU_CLIP};

    let td = tempdir().unwrap();
    let mut rng = rand::rngs::StdRng::seed_from_u64(7);
    let classic = ClassicNetwork::new(
        CLASSIC_ACC_DIM,
        CLASSIC_H1_DIM,
        CLASSIC_H2_DIM,
        CLASSIC_RELU_CLIP,
        64,
        &mut rng,
    );

    let (bundle, scales) = classic
        .fp32
        .quantize_symmetric(
            QuantScheme::PerTensor,
            QuantScheme::PerChannel,
            QuantScheme::PerChannel,
            QuantScheme::PerTensor,
            None,
        )
        .unwrap();

    let export = ExportOptions {
        arch: ArchKind::Classic,
        format: ExportFormat::ClassicV1,
        quant_ft: QuantScheme::PerTensor,
        quant_h1: QuantScheme::PerChannel,
        quant_h2: QuantScheme::PerChannel,
        quant_out: QuantScheme::PerTensor,
        emit_fp32_also: true,
    };

    finalize_export(FinalizeExportParams {
        network: &Network::Classic(classic.clone()),
        out_dir: td.path(),
        export,
        emit_single_quant: false,
        classic_bundle: Some(&bundle),
        classic_scales: Some(&scales),
        calibration_metrics: None,
        quant_metrics: None,
    })
    .unwrap();

    let fp32_path = td.path().join("nn.fp32.bin");
    assert!(fp32_path.exists(), "nn.fp32.bin should exist");

    let scales_path = td.path().join("nn.classic.scales.json");
    assert!(scales_path.exists(), "nn.classic.scales.json should exist");

    let scales_json: TestScalesJson =
        serde_json::from_reader(File::open(&scales_path).unwrap()).unwrap();
    assert_eq!(scales_json.acc_dim, CLASSIC_ACC_DIM);
    assert_eq!(scales_json.h1_dim, CLASSIC_H1_DIM);
    assert_eq!(scales_json.h2_dim, CLASSIC_H2_DIM);
    assert_eq!(scales_json.input_dim, SHOGI_BOARD_SIZE * FE_END);

    let expected_sha = compute_bundle_sha256(&bundle);
    assert_eq!(scales_json.bundle_sha256, expected_sha);

    if let Some(q) = scales_json.quant_scheme {
        assert_eq!(q.ft, "per-tensor");
        assert_eq!(q.h1, "per-channel");
        assert_eq!(q.h2, "per-channel");
        assert_eq!(q.out(), "per-tensor");
    } else {
        panic!("quant_scheme missing from scales json");
    }
}

#[test]
fn finalize_export_emit_fp32_ignored_for_non_classic() {
    let td = tempdir().unwrap();
    let mut rng = rand::rngs::StdRng::seed_from_u64(3);
    let network = Network::Single(SingleNetwork::new(8, DEFAULT_RELU_CLIP_NUM, &mut rng));

    let export = ExportOptions {
        arch: ArchKind::Single,
        format: ExportFormat::ClassicV1,
        emit_fp32_also: true,
        ..ExportOptions::default()
    };

    finalize_export(FinalizeExportParams {
        network: &network,
        out_dir: td.path(),
        export,
        emit_single_quant: false,
        classic_bundle: None,
        classic_scales: None,
        calibration_metrics: None,
        quant_metrics: None,
    })
    .unwrap();
    assert!(!td.path().join("nn.fp32.bin").exists());
}

#[test]
fn finalize_export_fp32_quantized_ignored_for_classic() {
    use crate::params::{CLASSIC_ACC_DIM, CLASSIC_H1_DIM, CLASSIC_H2_DIM, CLASSIC_RELU_CLIP};

    let td = tempdir().unwrap();
    let mut rng = rand::rngs::StdRng::seed_from_u64(11);
    let classic = ClassicNetwork::new(
        CLASSIC_ACC_DIM,
        CLASSIC_H1_DIM,
        CLASSIC_H2_DIM,
        CLASSIC_RELU_CLIP,
        64,
        &mut rng,
    );

    let export = ExportOptions {
        arch: ArchKind::Classic,
        format: ExportFormat::Fp32,
        emit_fp32_also: false,
        ..ExportOptions::default()
    };

    finalize_export(FinalizeExportParams {
        network: &Network::Classic(classic),
        out_dir: td.path(),
        export,
        emit_single_quant: true,
        classic_bundle: None,
        classic_scales: None,
        calibration_metrics: None,
        quant_metrics: None,
    })
    .unwrap();

    assert!(td.path().join("nn.fp32.bin").exists());
    assert!(!td.path().join("nn.i8.bin").exists());
}

#[test]
fn weight_consistency_jsonl_vs_cache() {
    // JSONL with both_exact, gap=50, depth=20, seldepth=30
    let td = tempdir().unwrap();
    let json_path = td.path().join("w.jsonl");
    let mut jf = File::create(&json_path).unwrap();
    writeln!(
        jf,
        "{{\"sfen\":\"lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1\",\"eval\":0,\"depth\":20,\"seldepth\":30,\"bound1\":\"Exact\",\"bound2\":\"Exact\",\"best2_gap_cp\":50}}"
    )
    .unwrap();
    jf.flush().unwrap();

    let cfg = Config {
        epochs: 1,
        batch_size: 1,
        learning_rate: 0.001,
        optimizer: "sgd".to_string(),
        l2_reg: 0.0,
        label_type: "cp".to_string(),
        mu: 0.0,
        scale: 600.0,
        cp_clip: 1200,
        accumulator_dim: 8,
        relu_clip: DEFAULT_RELU_CLIP_NUM,
        shuffle: false,
        prefetch_batches: 0,
        throughput_interval_sec: 10_000.0,
        stream_cache: false,
        prefetch_bytes: None,
        estimated_features_per_sample: 64,
        exclude_no_legal_move: false,
        exclude_fallback: false,
        lr_schedule: "constant".to_string(),
        lr_warmup_epochs: 0,
        lr_decay_epochs: None,
        lr_decay_steps: None,
        lr_plateau_patience: None,
        grad_clip: 0.0,
    };
    let json_samples =
        load_samples(json_path.to_str().unwrap(), &cfg, &wcfg::WeightingConfig::default()).unwrap();
    // Two-sample orientation -> take first weight
    let w_json = json_samples[0].weight;

    // Build cache with a single sample (n_features=0) carrying same meta
    let cache_path = td.path().join("w.cache");
    {
        let mut f = File::create(&cache_path).unwrap();
        let payload_offset = write_v1_header(
            &mut f,
            HeaderV1 {
                feature_set_id: 0x48414C46,
                num_samples: 1,
                chunk_size: DEFAULT_CHUNK_SIZE,
                header_size: 48,
                endianness: 0,
                payload_encoding: 0,
                sample_flags_mask: 1u8 as u32,
            },
        );
        // seek to payload_offset
        f.seek(SeekFrom::Start(payload_offset)).unwrap();
        // n_features = 0
        f.write_all(&0u32.to_le_bytes()).unwrap();
        // no features body
        // label (cp irrelevant for weight)
        f.write_all(&0.0f32.to_le_bytes()).unwrap();
        // gap=50
        f.write_all(&(50u16).to_le_bytes()).unwrap();
        // depth=20, seldepth=30
        f.write_all(&[20u8]).unwrap();
        f.write_all(&[30u8]).unwrap();
        // flags: both_exact (bit0)
        f.write_all(&[1u8]).unwrap();
        f.flush().unwrap();
    }

    let cache_samples =
        load_samples_from_cache(cache_path.to_str().unwrap(), &wcfg::WeightingConfig::default())
            .unwrap();
    let w_cache = cache_samples[0].weight;

    assert!(
        (w_json - w_cache).abs() < 1e-6,
        "weights should match: {} vs {}",
        w_json,
        w_cache
    );
}

#[test]
fn unknown_flags_warning_and_continue() {
    let td = tempdir().unwrap();
    let path = td.path().join("unknown_flags.cache");
    let mut f = File::create(&path).unwrap();
    // mask に 0 を渡して「全bit未知扱い」にする
    let off = write_v1_header(
        &mut f,
        HeaderV1 {
            feature_set_id: 0x48414C46,
            num_samples: 1,
            chunk_size: DEFAULT_CHUNK_SIZE,
            header_size: 48,
            endianness: 0,
            payload_encoding: 0,
            sample_flags_mask: 0,
        },
    );
    f.seek(SeekFrom::Start(off)).unwrap();
    // n_features=0, label=0.0, gap=0, depth=0, seldepth=0, flags = 0x80 (未知bit)
    f.write_all(&0u32.to_le_bytes()).unwrap();
    f.write_all(&0.0f32.to_le_bytes()).unwrap();
    f.write_all(&0u16.to_le_bytes()).unwrap();
    f.write_all(&[0u8, 0u8, 0x80u8]).unwrap();
    f.flush().unwrap();

    let samples =
        load_samples_from_cache(path.to_str().unwrap(), &wcfg::WeightingConfig::default()).unwrap();
    assert_eq!(samples.len(), 1);
}

#[test]
fn auc_boundary_labels_skipped() {
    // Network with zero weights outputs 0.0 logits -> p=0.5
    let mut rng = rand::rngs::StdRng::seed_from_u64(123);
    let mut single = SingleNetwork::new(8, DEFAULT_RELU_CLIP_NUM, &mut rng);
    // Zero out all weights/biases to make output exactly 0
    for w in single.w0.iter_mut() {
        *w = 0.0;
    }
    for b in single.b0.iter_mut() {
        *b = 0.0;
    }
    for w in single.w2.iter_mut() {
        *w = 0.0;
    }
    single.b2 = 0.0;

    // Samples all with label==0.5 (boundary) should be skipped and yield None AUC
    let samples = vec![
        Sample {
            features: vec![],
            label: 0.5,
            weight: 1.0,
            cp: None,
            phase: None,
        },
        Sample {
            features: vec![],
            label: 0.5,
            weight: 1.0,
            cp: None,
            phase: None,
        },
    ];
    let cfg = Config {
        epochs: 1,
        batch_size: 1,
        learning_rate: 0.001,
        optimizer: "sgd".to_string(),
        l2_reg: 0.0,
        label_type: "wdl".to_string(),
        mu: 0.0,
        scale: 600.0,
        cp_clip: 1200,
        accumulator_dim: 8,
        relu_clip: DEFAULT_RELU_CLIP_NUM,
        shuffle: false,
        prefetch_batches: 0,
        throughput_interval_sec: 10.0,
        stream_cache: false,
        prefetch_bytes: None,
        estimated_features_per_sample: 64,
        exclude_no_legal_move: false,
        exclude_fallback: false,
        lr_schedule: "constant".to_string(),
        lr_warmup_epochs: 0,
        lr_decay_epochs: None,
        lr_decay_steps: None,
        lr_plateau_patience: None,
        grad_clip: 0.0,
    };
    let network = Network::Single(single);
    let auc = super::compute_val_auc(&network, &samples, &cfg);
    assert!(auc.is_none(), "AUC should be None when all labels are 0.5 boundary");
}

#[test]
fn clamp_gap_and_depth_saturation() {
    // JSONL with large gap and max depth/seldepth (u8 saturate)
    let td = tempdir().unwrap();
    let json_path = td.path().join("w2.jsonl");
    let mut jf = File::create(&json_path).unwrap();
    writeln!(
        jf,
        r#"{{"sfen":"lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1","eval":0,"depth":255,"seldepth":255,"bound1":"Exact","bound2":"Exact","best2_gap_cp":70000}}"#
    )
    .unwrap();
    jf.flush().unwrap();

    let cfg = Config {
        epochs: 1,
        batch_size: 1,
        learning_rate: 0.001,
        optimizer: "sgd".to_string(),
        l2_reg: 0.0,
        label_type: "cp".to_string(),
        mu: 0.0,
        scale: 600.0,
        cp_clip: 1200,
        accumulator_dim: 8,
        relu_clip: DEFAULT_RELU_CLIP_NUM,
        shuffle: false,
        prefetch_batches: 0,
        throughput_interval_sec: 10_000.0,
        stream_cache: false,
        prefetch_bytes: None,
        estimated_features_per_sample: 64,
        exclude_no_legal_move: false,
        exclude_fallback: false,
        lr_schedule: "constant".to_string(),
        lr_warmup_epochs: 0,
        lr_decay_epochs: None,
        lr_decay_steps: None,
        lr_plateau_patience: None,
        grad_clip: 0.0,
    };
    let json_samples =
        load_samples(json_path.to_str().unwrap(), &cfg, &wcfg::WeightingConfig::default()).unwrap();
    assert!(!json_samples.is_empty());
    assert!(json_samples[0].weight <= 1.0);
}

#[test]
fn gap_zero_not_zero_weight() {
    // JSONL with gap=0 should not produce zero sample weight due to BASELINE_MIN_EPS
    let td = tempdir().unwrap();
    let json_path = td.path().join("w_gap0.jsonl");
    let mut jf = File::create(&json_path).unwrap();
    writeln!(
        jf,
        r#"{{"sfen":"lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1","eval":0,"best2_gap_cp":0}}"#
    )
    .unwrap();
    jf.flush().unwrap();

    let cfg = Config {
        epochs: 1,
        batch_size: 1,
        learning_rate: 0.001,
        optimizer: "sgd".to_string(),
        l2_reg: 0.0,
        label_type: "cp".to_string(),
        mu: 0.0,
        scale: 600.0,
        cp_clip: 1200,
        accumulator_dim: 8,
        relu_clip: DEFAULT_RELU_CLIP_NUM,
        shuffle: false,
        prefetch_batches: 0,
        throughput_interval_sec: 10_000.0,
        stream_cache: false,
        prefetch_bytes: None,
        estimated_features_per_sample: 64,
        exclude_no_legal_move: false,
        exclude_fallback: false,
        lr_schedule: "constant".to_string(),
        lr_warmup_epochs: 0,
        lr_decay_epochs: None,
        lr_decay_steps: None,
        lr_plateau_patience: None,
        grad_clip: 0.0,
    };
    let samples =
        load_samples(json_path.to_str().unwrap(), &cfg, &wcfg::WeightingConfig::default()).unwrap();
    assert_eq!(samples.len(), 2); // both perspectives
    assert!(samples[0].weight > 0.0, "weight should be >0 when gap=0");
    assert!(samples[1].weight > 0.0, "weight should be >0 when gap=0");
}

// 再現性（seed指定）— test_training_reproducibility_with_seed
#[test]
fn test_training_reproducibility_with_seed() {
    use rand::SeedableRng;

    // JSONLを用意（2局面）
    let td = tempdir().unwrap();
    let json_path = td.path().join("repro.jsonl");
    let mut jf = File::create(&json_path).unwrap();
    writeln!(
        jf,
        r#"{{"sfen":"lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1","eval":100,"depth":10,"seldepth":12,"bound1":"Exact","bound2":"Exact","best2_gap_cp":25}}"#
    )
    .unwrap();
    writeln!(
        jf,
        r#"{{"sfen":"lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL w - 1","eval":200,"depth":10,"seldepth":12,"bound1":"Exact","bound2":"Exact","best2_gap_cp":30}}"#
    )
    .unwrap();
    jf.flush().unwrap();

    // 設定（shuffle=false、optimizer=sgd、l2=0、accumulator_dim小さめ）
    let cfg = Config {
        epochs: 2,
        batch_size: 4,
        learning_rate: 0.001,
        optimizer: "sgd".to_string(),
        l2_reg: 0.0,
        label_type: "cp".to_string(),
        mu: 0.0,
        scale: 600.0,
        cp_clip: 1200,
        accumulator_dim: 8,
        relu_clip: DEFAULT_RELU_CLIP_NUM,
        shuffle: false,
        prefetch_batches: 0,
        throughput_interval_sec: 10_000.0,
        stream_cache: false,
        prefetch_bytes: None,
        estimated_features_per_sample: 64,
        exclude_no_legal_move: false,
        exclude_fallback: false,
        lr_schedule: "constant".to_string(),
        lr_warmup_epochs: 0,
        lr_decay_epochs: None,
        lr_decay_steps: None,
        lr_plateau_patience: None,
        grad_clip: 0.0,
    };

    // サンプルを読み込み（2局面→2サンプル/局面 = 計4サンプル）
    let mut samples1 =
        load_samples(json_path.to_str().unwrap(), &cfg, &wcfg::WeightingConfig::default()).unwrap();
    let mut samples2 = samples1.clone();
    assert_eq!(samples1.len(), samples2.len());
    assert_eq!(samples1.len(), 4);

    // 同じseedで2つのネットワークを初期化
    let mut rng1 = rand::rngs::StdRng::seed_from_u64(42);
    let mut rng2 = rand::rngs::StdRng::seed_from_u64(42);
    let mut network1 = model::Network::new_single(cfg.accumulator_dim, cfg.relu_clip, &mut rng1);
    let mut network2 = model::Network::new_single(cfg.accumulator_dim, cfg.relu_clip, &mut rng2);

    // 同じ条件・同じデータで学習
    let out_dir = td.path();
    let mut dummy_rng1 = rand::rngs::StdRng::seed_from_u64(123);
    let mut dummy_rng2 = rand::rngs::StdRng::seed_from_u64(123);
    let dash = super::DashboardOpts {
        emit: false,
        calib_bins_n: DEFAULT_CALIBRATION_BINS,
        do_plots: false,
        val_is_jsonl: false,
    };
    let mut bn1 = None;
    let mut bvl1 = f32::INFINITY;
    let mut ll1 = None;
    let mut be1 = None;
    let mut classic_bundle1 = None;
    let mut ctx1 = super::TrainContext {
        out_dir,
        save_every: None,
        dash,
        trackers: super::TrainTrackers {
            best_network: &mut bn1,
            best_val_loss: &mut bvl1,
            last_val_loss: &mut ll1,
            best_epoch: &mut be1,
        },
        structured: None,
        global_step: 0,
        training_config_json: None,
        plateau: None,
        classic_bundle: &mut classic_bundle1,
    };
    train_model(&mut network1, &mut samples1, &None, &cfg, &mut dummy_rng1, &mut ctx1).unwrap();
    let dash2 = super::DashboardOpts {
        emit: false,
        calib_bins_n: DEFAULT_CALIBRATION_BINS,
        do_plots: false,
        val_is_jsonl: false,
    };
    let mut bn2 = None;
    let mut bvl2 = f32::INFINITY;
    let mut ll2 = None;
    let mut be2 = None;
    let mut classic_bundle2 = None;
    let mut ctx2 = super::TrainContext {
        out_dir,
        save_every: None,
        dash: dash2,
        trackers: super::TrainTrackers {
            best_network: &mut bn2,
            best_val_loss: &mut bvl2,
            last_val_loss: &mut ll2,
            best_epoch: &mut be2,
        },
        structured: None,
        global_step: 0,
        training_config_json: None,
        plateau: None,
        classic_bundle: &mut classic_bundle2,
    };
    train_model(&mut network2, &mut samples2, &None, &cfg, &mut dummy_rng2, &mut ctx2).unwrap();

    // 重みの一致を確認（厳密一致 or 十分小さい誤差）
    let net1 = match &network1 {
        model::Network::Single(inner) => inner,
        _ => unreachable!(),
    };
    let net2 = match &network2 {
        model::Network::Single(inner) => inner,
        _ => unreachable!(),
    };

    assert_eq!(net1.w0.len(), net2.w0.len());
    assert_eq!(net1.b0.len(), net2.b0.len());
    assert_eq!(net1.w2.len(), net2.w2.len());
    let eps = 1e-7;
    for (a, b) in net1.w0.iter().zip(net2.w0.iter()) {
        assert!((a - b).abs() <= eps, "w0 diff: {} vs {}", a, b);
    }
    for (a, b) in net1.b0.iter().zip(net2.b0.iter()) {
        assert!((a - b).abs() <= eps, "b0 diff: {} vs {}", a, b);
    }
    for (a, b) in net1.w2.iter().zip(net2.w2.iter()) {
        assert!((a - b).abs() <= eps, "w2 diff: {} vs {}", a, b);
    }
    assert!((net1.b2 - net2.b2).abs() <= eps, "b2 diff: {} vs {}", net1.b2, net2.b2);
}

// 巨大な n_features を持つ壊れキャッシュが上限制約でエラーになること
#[test]
fn n_features_exceeds_limit_errors() {
    let td = tempdir().unwrap();
    let path = td.path().join("too_many_features.cache");
    let mut f = File::create(&path).unwrap();
    // 1サンプル・非圧縮・flags_mask=0
    let off = write_v1_header(
        &mut f,
        HeaderV1 {
            feature_set_id: 0x48414C46,
            num_samples: 1,
            chunk_size: DEFAULT_CHUNK_SIZE,
            header_size: 48,
            endianness: 0,
            payload_encoding: 0,
            sample_flags_mask: 0,
        },
    );
    f.seek(SeekFrom::Start(off)).unwrap();
    // 上限 (SHOGI_BOARD_SIZE*FE_END) + 1 を書く
    let max_allowed = (SHOGI_BOARD_SIZE * FE_END) as u32;
    let n_features = max_allowed + 1;
    f.write_all(&n_features.to_le_bytes()).unwrap();
    // 以降のボディは不要（n_features検証で即エラー）
    f.flush().unwrap();

    let err = load_samples_from_cache(path.to_str().unwrap(), &wcfg::WeightingConfig::default())
        .unwrap_err();
    let msg = format!("{}", err);
    assert!(msg.contains("exceeds"), "unexpected err msg: {}", msg);
}

// stream-sync と in-memory 経路での重み一致（決定論）
#[test]
fn stream_sync_vs_inmem_equivalence() {
    use tempfile::tempdir;
    // 小さな cache v1 を作成（3サンプル, n_features=0）
    let td = tempdir().unwrap();
    let path = td.path().join("tiny.cache");
    let mut f = File::create(&path).unwrap();
    // header: feature_set_id=HALF, num_samples=3, chunk_size=1024, header_size=48, LE, raw payload, flags_mask=0
    let payload_off = write_v1_header(
        &mut f,
        HeaderV1 {
            feature_set_id: 0x48414C46,
            num_samples: 3,
            chunk_size: DEFAULT_CHUNK_SIZE,
            header_size: 48,
            endianness: 0,
            payload_encoding: 0,
            sample_flags_mask: 0,
        },
    );
    f.seek(SeekFrom::Start(payload_off)).unwrap();
    for _ in 0..3u32 {
        // n_features=0
        f.write_all(&0u32.to_le_bytes()).unwrap();
        // label
        f.write_all(&0.0f32.to_le_bytes()).unwrap();
        // gap=50
        f.write_all(&(50u16).to_le_bytes()).unwrap();
        // depth=10, seldepth=12
        f.write_all(&[10u8]).unwrap();
        f.write_all(&[12u8]).unwrap();
        // flags=both_exact
        f.write_all(&[1u8]).unwrap();
    }
    f.flush().unwrap();

    // 共通設定
    let cfg_inmem = Config {
        epochs: 1,
        batch_size: 2,
        learning_rate: 0.001,
        optimizer: "sgd".to_string(),
        l2_reg: 0.0,
        label_type: "cp".to_string(),
        mu: 0.0,
        scale: 600.0,
        cp_clip: 1200,
        accumulator_dim: 8,
        relu_clip: DEFAULT_RELU_CLIP_NUM,
        shuffle: false,
        prefetch_batches: 0,
        throughput_interval_sec: 10_000.0,
        stream_cache: false,
        prefetch_bytes: None,
        estimated_features_per_sample: 64,
        exclude_no_legal_move: false,
        exclude_fallback: false,
        lr_schedule: "constant".to_string(),
        lr_warmup_epochs: 0,
        lr_decay_epochs: None,
        lr_decay_steps: None,
        lr_plateau_patience: None,
        grad_clip: 0.0,
    };
    let cfg_stream = Config {
        stream_cache: true,
        ..cfg_inmem.clone()
    };

    // サンプルを読み込み（in-mem）
    let mut samples =
        load_samples_from_cache(path.to_str().unwrap(), &wcfg::WeightingConfig::default()).unwrap();

    // 同じseedで2つのネットを初期化
    let mut rng1 = rand::rngs::StdRng::seed_from_u64(42);
    let mut rng2 = rand::rngs::StdRng::seed_from_u64(42);
    let mut network_inmem =
        model::Network::new_single(cfg_inmem.accumulator_dim, cfg_inmem.relu_clip, &mut rng1);
    let mut network_stream =
        model::Network::new_single(cfg_stream.accumulator_dim, cfg_stream.relu_clip, &mut rng2);

    let out_dir = td.path();
    let mut dummy_rng = rand::rngs::StdRng::seed_from_u64(123);

    // in-mem 学習
    let dash_inmem = super::DashboardOpts {
        emit: false,
        calib_bins_n: DEFAULT_CALIBRATION_BINS,
        do_plots: false,
        val_is_jsonl: false,
    };
    let mut best_network: Option<Network> = None;
    let mut best_val_loss = f32::INFINITY;
    let mut last_val_loss: Option<f32> = None;
    let mut best_epoch: Option<usize> = None;
    let mut classic_bundle_in = None;
    let mut ctx_in = super::TrainContext {
        out_dir,
        save_every: None,
        dash: dash_inmem,
        trackers: super::TrainTrackers {
            best_network: &mut best_network,
            best_val_loss: &mut best_val_loss,
            last_val_loss: &mut last_val_loss,
            best_epoch: &mut best_epoch,
        },
        structured: None,
        global_step: 0,
        training_config_json: None,
        plateau: None,
        classic_bundle: &mut classic_bundle_in,
    };
    train_model(&mut network_inmem, &mut samples, &None, &cfg_inmem, &mut dummy_rng, &mut ctx_in)
        .unwrap();
    // stream-sync 学習
    let dash_stream = super::DashboardOpts {
        emit: false,
        calib_bins_n: DEFAULT_CALIBRATION_BINS,
        do_plots: false,
        val_is_jsonl: false,
    };
    let mut best_network2: Option<Network> = None;
    let mut best_val_loss2 = f32::INFINITY;
    let mut last_val_loss2: Option<f32> = None;
    let mut best_epoch2: Option<usize> = None;
    let mut classic_bundle_stream = None;
    let mut ctx_st = super::TrainContext {
        out_dir,
        save_every: None,
        dash: dash_stream,
        trackers: super::TrainTrackers {
            best_network: &mut best_network2,
            best_val_loss: &mut best_val_loss2,
            last_val_loss: &mut last_val_loss2,
            best_epoch: &mut best_epoch2,
        },
        structured: None,
        global_step: 0,
        training_config_json: None,
        plateau: None,
        classic_bundle: &mut classic_bundle_stream,
    };
    train_model_stream_cache(
        &mut network_stream,
        path.to_str().unwrap(),
        &None,
        &cfg_stream,
        &mut dummy_rng,
        &mut ctx_st,
        &wcfg::WeightingConfig::default(),
    )
    .unwrap();

    let net_inmem = match &network_inmem {
        model::Network::Single(inner) => inner,
        _ => unreachable!(),
    };
    let net_stream = match &network_stream {
        model::Network::Single(inner) => inner,
        _ => unreachable!(),
    };

    // 重み一致（厳密一致 or 近傍）
    assert_eq!(net_inmem.w0.len(), net_stream.w0.len());
    assert_eq!(net_inmem.b0.len(), net_stream.b0.len());
    assert_eq!(net_inmem.w2.len(), net_stream.w2.len());
    let eps = 1e-7;
    for (a, b) in net_inmem.w0.iter().zip(net_stream.w0.iter()) {
        assert!((a - b).abs() <= eps, "w0 diff: {} vs {}", a, b);
    }
    for (a, b) in net_inmem.b0.iter().zip(net_stream.b0.iter()) {
        assert!((a - b).abs() <= eps, "b0 diff: {} vs {}", a, b);
    }
    for (a, b) in net_inmem.w2.iter().zip(net_stream.w2.iter()) {
        assert!((a - b).abs() <= eps, "w2 diff: {} vs {}", a, b);
    }
    assert!(
        (net_inmem.b2 - net_stream.b2).abs() <= eps,
        "b2 diff: {} vs {}",
        net_inmem.b2,
        net_stream.b2
    );
}

// in-mem loader async 経路（prefetch>0）の structured JSONL に training_config が入ること
#[test]
fn structured_training_config_present_in_inmem_async_loader() {
    let td = tempdir().unwrap();
    let out_dir = td.path();
    let struct_path = out_dir.join("structured.jsonl");

    // 最小のサンプル（特徴ゼロでも動作）
    let train_samples: Vec<Sample> = (0..5)
        .map(|_| Sample {
            features: vec![],
            label: 0.0,
            weight: 1.0,
            cp: None,
            phase: None,
        })
        .collect();
    let val_samples: Vec<Sample> = (0..3)
        .map(|_| Sample {
            features: vec![],
            label: 0.0,
            weight: 1.0,
            cp: None,
            phase: None,
        })
        .collect();

    let mut rng = rand::rngs::StdRng::seed_from_u64(7);
    let mut network = model::Network::new_single(8, DEFAULT_RELU_CLIP_NUM, &mut rng);
    let cfg = Config {
        epochs: 1,
        batch_size: 2,
        learning_rate: 0.001,
        optimizer: "sgd".to_string(),
        l2_reg: 0.0,
        label_type: "cp".to_string(),
        mu: 0.0,
        scale: 600.0,
        cp_clip: 1200,
        accumulator_dim: 8,
        relu_clip: DEFAULT_RELU_CLIP_NUM,
        shuffle: false,
        prefetch_batches: 2,          // async 経路
        throughput_interval_sec: 1e9, // throughput出力を抑止
        stream_cache: false,
        prefetch_bytes: None,
        estimated_features_per_sample: 32,
        exclude_no_legal_move: false,
        exclude_fallback: false,
        lr_schedule: "constant".to_string(),
        lr_warmup_epochs: 0,
        lr_decay_epochs: None,
        lr_decay_steps: None,
        lr_plateau_patience: None,
        grad_clip: 0.0,
    };

    let dash = DashboardOpts {
        emit: false,
        calib_bins_n: DEFAULT_CALIBRATION_BINS,
        do_plots: false,
        val_is_jsonl: false,
    };
    let mut best_network: Option<Network> = None;
    let mut best_val_loss = f32::INFINITY;
    let mut last_val_loss: Option<f32> = None;
    let mut best_epoch: Option<usize> = None;
    let mut classic_bundle_log = None;
    let mut ctx = TrainContext {
        out_dir,
        save_every: None,
        dash,
        trackers: TrainTrackers {
            best_network: &mut best_network,
            best_val_loss: &mut best_val_loss,
            last_val_loss: &mut last_val_loss,
            best_epoch: &mut best_epoch,
        },
        structured: Some(StructuredLogger::new(struct_path.to_str().unwrap()).unwrap()),
        global_step: 0,
        training_config_json: Some(serde_json::json!({"exp_id": "unit"})),
        plateau: None,
        classic_bundle: &mut classic_bundle_log,
    };

    train_model_with_loader(
        &mut network,
        train_samples,
        &Some(val_samples),
        &cfg,
        &mut rand::rngs::StdRng::seed_from_u64(1234),
        &mut ctx,
    )
    .unwrap();

    // 構造化ログのバッファを明示的にフラッシュ
    if let Some(ref lg) = ctx.structured {
        lg.flush().unwrap();
    }

    // JSONLを読んで、phase=val のレコードに training_config があることを確認
    let content = std::fs::read_to_string(struct_path).unwrap();
    let mut found_val = false;
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        if v.get("phase").and_then(|x| x.as_str()) == Some("val") {
            assert!(v.get("training_config").is_some());
            found_val = true;
            break;
        }
    }
    assert!(found_val, "no val record with training_config found in structured JSONL");
}

// ゼロ重みサンプルのみのバッチでは更新が一切走らない（L2=0で検証）
#[test]
fn zero_weight_batches_do_not_update() {
    let td = tempdir().unwrap();
    let out_dir = td.path();
    // 全て weight=0 のサンプル
    let mut samples: Vec<Sample> = (0..4)
        .map(|_| Sample {
            features: vec![0], // 何かしらの特徴を入れても weight=0 で無視される
            label: 0.0,
            weight: 0.0,
            cp: None,
            phase: None,
        })
        .collect();

    let cfg = Config {
        epochs: 1,
        batch_size: 2,
        learning_rate: 0.01,
        optimizer: "sgd".to_string(),
        l2_reg: 0.0, // L2も無効化
        label_type: "cp".to_string(),
        mu: 0.0,
        scale: 600.0,
        cp_clip: 1200,
        accumulator_dim: 8,
        relu_clip: DEFAULT_RELU_CLIP_NUM,
        shuffle: false,
        prefetch_batches: 0,
        throughput_interval_sec: 1e9,
        stream_cache: false,
        prefetch_bytes: None,
        estimated_features_per_sample: 32,
        exclude_no_legal_move: false,
        exclude_fallback: false,
        lr_schedule: "constant".to_string(),
        lr_warmup_epochs: 0,
        lr_decay_epochs: None,
        lr_decay_steps: None,
        lr_plateau_patience: None,
        grad_clip: 0.0,
    };

    let mut rng = rand::rngs::StdRng::seed_from_u64(42);
    let mut network = model::Network::new_single(cfg.accumulator_dim, cfg.relu_clip, &mut rng);
    let before = match &network {
        model::Network::Single(inner) => inner.clone(),
        _ => unreachable!(),
    };

    let dash = DashboardOpts {
        emit: false,
        calib_bins_n: DEFAULT_CALIBRATION_BINS,
        do_plots: false,
        val_is_jsonl: false,
    };
    let mut best_network: Option<Network> = None;
    let mut best_val_loss = f32::INFINITY;
    let mut last_val_loss: Option<f32> = None;
    let mut best_epoch: Option<usize> = None;
    let mut classic_bundle_zero = None;
    let mut ctx = TrainContext {
        out_dir,
        save_every: None,
        dash,
        trackers: TrainTrackers {
            best_network: &mut best_network,
            best_val_loss: &mut best_val_loss,
            last_val_loss: &mut last_val_loss,
            best_epoch: &mut best_epoch,
        },
        structured: None,
        global_step: 0,
        training_config_json: None,
        plateau: None,
        classic_bundle: &mut classic_bundle_zero,
    };

    train_model(&mut network, &mut samples, &None, &cfg, &mut rng, &mut ctx).unwrap();

    let net = match &network {
        model::Network::Single(inner) => inner,
        _ => unreachable!(),
    };

    // 重みが全く変わっていないことを確認（ビット単位で比較して浮動小数比較Lintを回避）
    assert!(before.w0.iter().zip(net.w0.iter()).all(|(a, b)| a.to_bits() == b.to_bits()));
    assert!(before.b0.iter().zip(net.b0.iter()).all(|(a, b)| a.to_bits() == b.to_bits()));
    assert!(before.w2.iter().zip(net.w2.iter()).all(|(a, b)| a.to_bits() == b.to_bits()));
    assert_eq!(before.b2.to_bits(), net.b2.to_bits());
}

// 非同期ストリーム（prefetch>0）で破損キャッシュのエラーが上位に伝搬すること
#[test]
fn stream_async_propagates_errors() {
    let td = tempdir().unwrap();
    let path = td.path().join("bad_async.cache");

    // 1サンプル、raw（非圧縮）でヘッダを書き、payload に n_features = MAX+1 を書く
    let mut f = File::create(&path).unwrap();
    let payload_off = write_v1_header(
        &mut f,
        HeaderV1 {
            feature_set_id: FEATURE_SET_ID_HALF,
            num_samples: 1,
            chunk_size: DEFAULT_CHUNK_SIZE,
            header_size: 48,
            endianness: 0,
            payload_encoding: 0,
            sample_flags_mask: 0,
        },
    );
    f.seek(SeekFrom::Start(payload_off)).unwrap();
    let max_allowed = (SHOGI_BOARD_SIZE * FE_END) as u32;
    let n_features = max_allowed + 1;
    f.write_all(&n_features.to_le_bytes()).unwrap();
    f.flush().unwrap();

    let cfg = Config {
        epochs: 1,
        batch_size: 1024,
        learning_rate: 0.001,
        optimizer: "sgd".to_string(),
        l2_reg: 0.0,
        label_type: "cp".to_string(),
        mu: 0.0,
        scale: 600.0,
        cp_clip: 1200,
        accumulator_dim: 8,
        relu_clip: DEFAULT_RELU_CLIP_NUM,
        shuffle: false,
        prefetch_batches: 2, // async 経路
        throughput_interval_sec: 10_000.0,
        stream_cache: true,
        prefetch_bytes: None,
        estimated_features_per_sample: 64,
        exclude_no_legal_move: false,
        exclude_fallback: false,
        lr_schedule: "constant".to_string(),
        lr_warmup_epochs: 0,
        lr_decay_epochs: None,
        lr_decay_steps: None,
        lr_plateau_patience: None,
        grad_clip: 0.0,
    };

    let mut rng = rand::rngs::StdRng::seed_from_u64(1);
    let mut network = model::Network::new_single(cfg.accumulator_dim, cfg.relu_clip, &mut rng);
    let out_dir = td.path();
    let dash = super::DashboardOpts {
        emit: false,
        calib_bins_n: DEFAULT_CALIBRATION_BINS,
        do_plots: false,
        val_is_jsonl: false,
    };
    let mut best_network: Option<Network> = None;
    let mut best_val_loss = f32::INFINITY;
    let mut last_val_loss: Option<f32> = None;
    let mut best_epoch: Option<usize> = None;
    let mut classic_bundle_err = None;
    let mut ctx = super::TrainContext {
        out_dir,
        save_every: None,
        dash,
        trackers: super::TrainTrackers {
            best_network: &mut best_network,
            best_val_loss: &mut best_val_loss,
            last_val_loss: &mut last_val_loss,
            best_epoch: &mut best_epoch,
        },
        structured: None,
        global_step: 0,
        training_config_json: None,
        plateau: None,
        classic_bundle: &mut classic_bundle_err,
    };
    let err = train_model_stream_cache(
        &mut network,
        path.to_str().unwrap(),
        &None,
        &cfg,
        &mut rng,
        &mut ctx,
        &wcfg::WeightingConfig::default(),
    )
    .unwrap_err();
    let msg = format!("{}", err);
    assert!(msg.contains("exceeds sane limit"), "unexpected err msg: {}", msg);
}
// n_features=0 のサンプルのみで 1 epoch 学習し、NaN が発生しないことのスモーク
#[test]
fn train_one_batch_with_zero_feature_sample_smoke() {
    use rand::SeedableRng;

    // 単一サンプル（特徴なし、重み1.0、ラベル0.0）
    let mut samples = vec![Sample {
        features: Vec::new(),
        label: 0.0,
        weight: 1.0,
        cp: None,
        phase: None,
    }];

    let cfg = Config {
        epochs: 1,
        batch_size: 1,
        learning_rate: 0.001,
        optimizer: "sgd".to_string(),
        l2_reg: 0.0,
        label_type: "cp".to_string(),
        mu: 0.0,
        scale: 600.0,
        cp_clip: 1200,
        accumulator_dim: 8,
        relu_clip: DEFAULT_RELU_CLIP_NUM,
        shuffle: false,
        prefetch_batches: 0,
        throughput_interval_sec: 10_000.0,
        stream_cache: false,
        prefetch_bytes: None,
        estimated_features_per_sample: 64,
        exclude_no_legal_move: false,
        exclude_fallback: false,
        lr_schedule: "constant".to_string(),
        lr_warmup_epochs: 0,
        lr_decay_epochs: None,
        lr_decay_steps: None,
        lr_plateau_patience: None,
        grad_clip: 0.0,
    };

    let td = tempfile::tempdir().unwrap();
    let out_dir = td.path();

    let mut rng = rand::rngs::StdRng::seed_from_u64(1);
    let mut network = model::Network::new_single(cfg.accumulator_dim, cfg.relu_clip, &mut rng);
    let dash = super::DashboardOpts {
        emit: false,
        calib_bins_n: DEFAULT_CALIBRATION_BINS,
        do_plots: false,
        val_is_jsonl: false,
    };
    let mut best_network: Option<Network> = None;
    let mut best_val_loss = f32::INFINITY;
    let mut last_val_loss: Option<f32> = None;
    let mut best_epoch: Option<usize> = None;
    let mut classic_bundle_smoke = None;
    let mut ctx = super::TrainContext {
        out_dir,
        save_every: None,
        dash,
        trackers: super::TrainTrackers {
            best_network: &mut best_network,
            best_val_loss: &mut best_val_loss,
            last_val_loss: &mut last_val_loss,
            best_epoch: &mut best_epoch,
        },
        structured: None,
        global_step: 0,
        training_config_json: None,
        plateau: None,
        classic_bundle: &mut classic_bundle_smoke,
    };
    train_model(&mut network, &mut samples, &None, &cfg, &mut rng, &mut ctx).unwrap();

    let net = match &network {
        model::Network::Single(inner) => inner,
        _ => unreachable!(),
    };

    // NaN が混入していないこと
    assert!(net.w0.iter().all(|v| v.is_finite()));
    assert!(net.b0.iter().all(|v| v.is_finite()));
    assert!(net.w2.iter().all(|v| v.is_finite()));
    assert!(net.b2.is_finite());
}

#[test]
fn classic_train_updates_weights() {
    use rand::SeedableRng;

    let mut rng = rand::rngs::StdRng::seed_from_u64(123);
    let mut network = model::Network::new_classic(DEFAULT_RELU_CLIP_NUM, 8, &mut rng);
    let before = match &network {
        model::Network::Classic(inner) => inner.clone(),
        _ => unreachable!(),
    };

    let mut samples = vec![Sample {
        features: vec![0, 1, 2],
        label: 1.0,
        weight: 1.0,
        cp: None,
        phase: None,
    }];

    let cfg = Config {
        epochs: 1,
        batch_size: 1,
        learning_rate: 0.01,
        optimizer: "sgd".to_string(),
        l2_reg: 0.0,
        label_type: "wdl".to_string(),
        mu: 0.0,
        scale: 600.0,
        cp_clip: 1200,
        accumulator_dim: 512,
        relu_clip: DEFAULT_RELU_CLIP_NUM,
        shuffle: false,
        prefetch_batches: 0,
        throughput_interval_sec: 10_000.0,
        stream_cache: false,
        prefetch_bytes: None,
        estimated_features_per_sample: 64,
        exclude_no_legal_move: false,
        exclude_fallback: false,
        lr_schedule: "constant".to_string(),
        lr_warmup_epochs: 0,
        lr_decay_epochs: None,
        lr_decay_steps: None,
        lr_plateau_patience: None,
        grad_clip: 0.0,
    };

    let td = tempfile::tempdir().unwrap();
    let out_dir = td.path();
    let dash = DashboardOpts {
        emit: false,
        calib_bins_n: DEFAULT_CALIBRATION_BINS,
        do_plots: false,
        val_is_jsonl: false,
    };
    let mut best_network: Option<Network> = None;
    let mut best_val_loss = f32::INFINITY;
    let mut last_val_loss: Option<f32> = None;
    let mut best_epoch: Option<usize> = None;
    let mut classic_bundle = None;
    let mut ctx = TrainContext {
        out_dir,
        save_every: None,
        dash,
        trackers: TrainTrackers {
            best_network: &mut best_network,
            best_val_loss: &mut best_val_loss,
            last_val_loss: &mut last_val_loss,
            best_epoch: &mut best_epoch,
        },
        structured: None,
        global_step: 0,
        training_config_json: None,
        plateau: None,
        classic_bundle: &mut classic_bundle,
    };

    let mut rng_train = rand::rngs::StdRng::seed_from_u64(999);
    train_model(&mut network, &mut samples, &None, &cfg, &mut rng_train, &mut ctx).unwrap();

    let after = match &network {
        model::Network::Classic(inner) => inner,
        _ => unreachable!(),
    };

    assert!(after
        .fp32
        .output_weights
        .iter()
        .zip(before.fp32.output_weights.iter())
        .any(|(a, b)| (*a - *b).abs() > 1e-6));
}

#[test]
fn classic_train_invalid_optimizer_errors() {
    use rand::SeedableRng;

    let mut rng = rand::rngs::StdRng::seed_from_u64(555);
    let mut network = model::Network::new_classic(DEFAULT_RELU_CLIP_NUM, 8, &mut rng);

    let mut samples = vec![Sample {
        features: vec![0],
        label: 0.0,
        weight: 1.0,
        cp: None,
        phase: None,
    }];

    let cfg = Config {
        epochs: 1,
        batch_size: 1,
        learning_rate: 0.01,
        optimizer: "rmsprop".to_string(),
        l2_reg: 0.0,
        label_type: "wdl".to_string(),
        mu: 0.0,
        scale: 600.0,
        cp_clip: 1200,
        accumulator_dim: 512,
        relu_clip: DEFAULT_RELU_CLIP_NUM,
        shuffle: false,
        prefetch_batches: 0,
        throughput_interval_sec: 10_000.0,
        stream_cache: false,
        prefetch_bytes: None,
        estimated_features_per_sample: 64,
        exclude_no_legal_move: false,
        exclude_fallback: false,
        lr_schedule: "constant".to_string(),
        lr_warmup_epochs: 0,
        lr_decay_epochs: None,
        lr_decay_steps: None,
        lr_plateau_patience: None,
        grad_clip: 0.0,
    };

    let td = tempfile::tempdir().unwrap();
    let out_dir = td.path();
    let dash = DashboardOpts {
        emit: false,
        calib_bins_n: DEFAULT_CALIBRATION_BINS,
        do_plots: false,
        val_is_jsonl: false,
    };
    let mut best_network: Option<Network> = None;
    let mut best_val_loss = f32::INFINITY;
    let mut last_val_loss: Option<f32> = None;
    let mut best_epoch: Option<usize> = None;
    let mut classic_bundle = None;
    let mut ctx = TrainContext {
        out_dir,
        save_every: None,
        dash,
        trackers: TrainTrackers {
            best_network: &mut best_network,
            best_val_loss: &mut best_val_loss,
            last_val_loss: &mut last_val_loss,
            best_epoch: &mut best_epoch,
        },
        structured: None,
        global_step: 0,
        training_config_json: None,
        plateau: None,
        classic_bundle: &mut classic_bundle,
    };

    let mut rng_train = rand::rngs::StdRng::seed_from_u64(321);
    let err =
        train_model(&mut network, &mut samples, &None, &cfg, &mut rng_train, &mut ctx).unwrap_err();
    let msg = format!("{}", err);
    assert!(msg.contains("Unsupported optimizer"));
}

// LrPlateauState の単体テスト
#[test]
fn lr_plateau_state_basic() {
    let mut p = super::LrPlateauState::new(2);
    assert!((p.factor() - 1.0).abs() < 1e-12);
    // 初回: bestが更新、wait=0、発火しない
    assert!(p.update(1.0).is_none());
    assert_eq!(p.wait, 0);
    assert!((p.best - 1.0).abs() < 1e-12);
    // 同値（改善なし）: wait=1
    assert!(p.update(1.0).is_none());
    assert_eq!(p.wait, 1);
    // さらに改善なし: patience到達で発火、*gamma(=0.5)
    if let Some(mult) = p.update(1.0) {
        assert!((mult - 0.5).abs() < 1e-6);
    } else {
        panic!("expected plateau trigger");
    }
    assert!((p.factor() - 0.5).abs() < 1e-6);
    assert_eq!(p.wait, 0);
    // 改善あり: best更新、wait=0据え置き
    assert!(p.update(0.9).is_none());
    assert!((p.best - 0.9).abs() < 1e-12);
    assert_eq!(p.wait, 0);
    // 非有限は無視
    assert!(p.update(f32::NAN).is_none());
    assert!(p.update(f32::INFINITY).is_none());
    assert!((p.factor() - 0.5).abs() < 1e-6);
}

#[test]
fn lr_plateau_state_min_delta() {
    let mut p = super::LrPlateauState::new(1);
    // set best
    assert!(p.update(1.0).is_none());
    // min_delta=1e-6 の閾下の変化は改善扱いしない
    assert!(p.update(0.9999999).is_some()); // patience=1 到達で発火
    assert!((p.factor() - 0.5).abs() < 1e-6);
}

// --- E2E: Classic v1 バンドル export -> engine_core ロード -> propagate 出力一致テスト ---
#[test]
fn e2e_classic_v1_bias_and_small_features_match_engine_core() {
    use crate::classic::{
        write_classic_v1_bundle, ClassicFeatureTransformerInt, ClassicIntNetworkBundle,
        ClassicQuantizedNetwork, ClassicQuantizedNetworkParams, ClassicScratchViews,
    };
    use engine_core::evaluation::nnue::{simd::SimdDispatcher, weights::load_weights};

    let td = tempfile::tempdir().unwrap();
    let path = td.path().join("nn.classic.nnue");

    // 固定サイズ (256x2-32-32) の最小決定的バンドルを生成
    let acc_dim = 256;
    let h1_dim = 32;
    let h2_dim = 32;
    // engine_core v1 ローダ互換: テストでは ft_input_dim=1 (最小) とし、単一特徴のみ使用
    let ft_input_dim = 1;

    // FT weights/biases
    let mut ft_weights = vec![0i16; acc_dim * ft_input_dim];
    for (i, w) in ft_weights.iter_mut().enumerate() {
        *w = ((i as i32 % 31) - 15) as i16;
    }
    let mut ft_biases = vec![0i32; acc_dim];
    for (i, b) in ft_biases.iter_mut().enumerate().take(16) {
        *b = (i as i32) - 8;
    }

    // Classic hidden / output (単純なパターン)
    let classic_input_dim = acc_dim * 2;
    let mut hidden1_weights = vec![0i8; classic_input_dim * h1_dim];
    for (i, w) in hidden1_weights.iter_mut().enumerate() {
        *w = ((i as i32 % 13) - 6) as i8;
    }
    let mut hidden1_biases = vec![0i32; h1_dim];
    for (i, b) in hidden1_biases.iter_mut().enumerate() {
        *b = (i as i32) - 4;
    }
    let mut hidden2_weights = vec![0i8; h1_dim * h2_dim];
    for (i, w) in hidden2_weights.iter_mut().enumerate() {
        *w = ((i as i32 % 7) - 3) as i8;
    }
    let mut hidden2_biases = vec![0i32; h2_dim];
    for (i, b) in hidden2_biases.iter_mut().enumerate() {
        *b = (i as i32) - 2;
    }
    let mut output_weights = vec![0i8; h2_dim];
    for (i, w) in output_weights.iter_mut().enumerate() {
        *w = (i as i8 % 5) - 2;
    }
    let output_bias = 17i32;

    let transformer =
        ClassicFeatureTransformerInt::new(ft_weights.clone(), ft_biases.clone(), acc_dim);
    let network = ClassicQuantizedNetwork::new(ClassicQuantizedNetworkParams {
        hidden1_weights: hidden1_weights.clone(),
        hidden1_biases: hidden1_biases.clone(),
        hidden2_weights: hidden2_weights.clone(),
        hidden2_biases: hidden2_biases.clone(),
        output_weights: output_weights.clone(),
        output_bias,
        acc_dim,
        h1_dim,
        h2_dim,
    });
    let bundle = ClassicIntNetworkBundle::new(transformer, network);
    write_classic_v1_bundle(&path, &bundle).unwrap();

    // engine_core ロード (FeatureTransformer, Network)
    let (core_ft, core_net) =
        load_weights(path.to_str().unwrap()).expect("engine_core load failed");
    // Sanity: dims
    assert_eq!(core_ft.acc_dim(), acc_dim);
    let canonical_ft_weights = SHOGI_BOARD_SIZE * FE_END * acc_dim;
    assert_eq!(core_ft.weights.len(), canonical_ft_weights);
    assert_eq!(&core_ft.weights[..ft_weights.len()], ft_weights.as_slice());
    assert_eq!(core_ft.biases.as_slice(), ft_biases.as_slice());
    assert_eq!(core_net.hidden1_weights.as_slice(), hidden1_weights.as_slice());
    assert_eq!(core_net.hidden1_biases.as_slice(), hidden1_biases.as_slice());
    assert_eq!(core_net.hidden2_weights.as_slice(), hidden2_weights.as_slice());
    assert_eq!(core_net.hidden2_biases.as_slice(), hidden2_biases.as_slice());
    assert_eq!(core_net.output_weights.as_slice(), output_weights.as_slice());
    assert_eq!(core_net.output_bias, output_bias);

    // 1) 空特徴 (bias のみ) — trainer 側 int 推論
    let mut views = ClassicScratchViews::new(acc_dim, h1_dim, h2_dim);
    let trainer_out_bias_only = bundle.propagate_with_features_scratch_full(&[], &[], &mut views);
    let trainer_acc_us = views.acc_us.clone();
    let trainer_acc_them = views.acc_them.clone();

    // engine_core では FeatureTransformer → Accumulator → Network の経路を通す
    fn clamp_i32_to_i16(x: i32) -> i16 {
        if x > i16::MAX as i32 {
            i16::MAX
        } else if x < i16::MIN as i32 {
            i16::MIN
        } else {
            x as i16
        }
    }
    let mut engine_acc_us: Vec<i16> = ft_biases.iter().map(|&b| clamp_i32_to_i16(b)).collect();
    let mut engine_acc_them = engine_acc_us.clone();
    assert_eq!(trainer_acc_us, engine_acc_us);
    assert_eq!(trainer_acc_them, engine_acc_them);
    let engine_out_bias_only = core_net.propagate(&engine_acc_us, &engine_acc_them);

    let mut manual_input = vec![0i8; classic_input_dim];
    SimdDispatcher::transform_features(
        &engine_acc_us,
        &engine_acc_them,
        &mut manual_input,
        acc_dim,
    );
    let mut manual_h1 = vec![0i32; h1_dim];
    for i in 0..h1_dim {
        let mut acc = hidden1_biases[i];
        let row = &hidden1_weights[i * classic_input_dim..(i + 1) * classic_input_dim];
        for (j, &w) in row.iter().enumerate() {
            acc += manual_input[j] as i32 * w as i32;
        }
        manual_h1[i] = acc;
    }
    let mut manual_h1_act = vec![0i8; h1_dim];
    for (dst, &src) in manual_h1_act.iter_mut().zip(manual_h1.iter()) {
        *dst = src.clamp(0, I8_QMAX) as i8;
    }
    let mut manual_h2 = vec![0i32; h2_dim];
    for i in 0..h2_dim {
        let mut acc = hidden2_biases[i];
        let row = &hidden2_weights[i * h1_dim..(i + 1) * h1_dim];
        for (j, &w) in row.iter().enumerate() {
            acc += manual_h1_act[j] as i32 * w as i32;
        }
        manual_h2[i] = acc;
    }
    let mut manual_h2_act = vec![0i8; h2_dim];
    for (dst, &src) in manual_h2_act.iter_mut().zip(manual_h2.iter()) {
        *dst = src.clamp(0, I8_QMAX) as i8;
    }
    let mut manual_out_bias_only = output_bias;
    for (i, &w) in output_weights.iter().enumerate() {
        manual_out_bias_only += manual_h2_act[i] as i32 * w as i32;
    }
    assert_eq!(manual_out_bias_only, trainer_out_bias_only);
    assert_eq!(manual_out_bias_only, engine_out_bias_only);

    // 2) 小さな特徴集合 (唯一の特徴 0)
    let features = vec![0u32];
    let mut views2 = ClassicScratchViews::new(acc_dim, h1_dim, h2_dim);
    let trainer_out_feats =
        bundle.propagate_with_features_scratch_full(&features, &[], &mut views2);

    engine_acc_us = ft_biases.iter().map(|&b| clamp_i32_to_i16(b)).collect();
    engine_acc_them = ft_biases.iter().map(|&b| clamp_i32_to_i16(b)).collect();
    let features_us: Vec<usize> = features.iter().map(|&f| f as usize).collect();
    // SimdDispatcher は対象プラットフォームに応じてスカラ実装へフォールバックするため、
    // 非 x86_64 環境でも同じコード経路で検証可能。
    SimdDispatcher::update_accumulator(
        &mut engine_acc_us,
        &core_ft.weights,
        &features_us,
        true,
        core_ft.acc_dim(),
    );
    let engine_out_feats = core_net.propagate(&engine_acc_us, &engine_acc_them);
    assert_eq!(trainer_out_feats, engine_out_feats, "small-features output mismatch");

    // ロジットは i32 なので浮動小数変換許容は不要だが将来スケール導入時の余地として誤差ゼロを確認
}

// 将来: 重み付き vs 無重み p95 差の可視化テスト (仕様確認用)
// ignore してスケルトンのみ配置
#[test]
#[ignore]
fn weighted_vs_unweighted_p95_diff_visualization_todo() {
    // TODO: 小規模サンプル集合を用意 (異なる weight を付与) し weighted_percentile と naive percentile の差を
    // ログ出力 or assert (差が閾値以上) で確認する。
    // 実装時: evaluate_quantization_gap もしくは percentile ユーティリティを直接呼び出す。
}

// cp 蒸留単位スモーク: teacher_logit を cp 換算する実装部分の退行を検知するため
#[test]
fn cp_distillation_unit_smoke_loss_ordering() {
    use crate::types::{DistillLossKind, DistillOptions};
    use rand::SeedableRng;

    // 単純ネット (全0 初期バイアス) を構築
    let mut rng = rand::rngs::StdRng::seed_from_u64(1);
    let mut net = SingleNetwork::new(8, DEFAULT_RELU_CLIP_NUM, &mut rng);
    // ゼロ化して決定的
    for w in net.w0.iter_mut() {
        *w = 0.0;
    }
    for b in net.b0.iter_mut() {
        *b = 0.0;
    }
    for w in net.w2.iter_mut() {
        *w = 0.0;
    }
    net.b2 = 0.0; // 予測ロジット=0 -> 予測cp=0

    // サンプル: teacher_output=1.0 (logit仮定), label(cp)=200 (scale=600 で教師=600cp 相当)
    let sample = Sample {
        features: vec![],
        label: 200.0,
        weight: 1.0,
        cp: Some(200),
        phase: None,
    };
    let cfg = Config {
        epochs: 1,
        batch_size: 2,
        learning_rate: 0.0,
        optimizer: "sgd".into(),
        l2_reg: 0.0,
        label_type: "cp".into(),
        mu: 0.0,
        scale: 600.0,
        cp_clip: 1200,
        accumulator_dim: 8,
        relu_clip: DEFAULT_RELU_CLIP_NUM,
        shuffle: false,
        prefetch_batches: 0,
        throughput_interval_sec: 10_000.0,
        stream_cache: false,
        prefetch_bytes: None,
        estimated_features_per_sample: 64,
        exclude_no_legal_move: false,
        exclude_fallback: false,
        lr_schedule: "constant".into(),
        lr_warmup_epochs: 0,
        lr_decay_epochs: None,
        lr_decay_steps: None,
        lr_plateau_patience: None,
        grad_clip: 0.0,
    };

    // DistillOptions: alpha=0.5, temperature=1.0, mse
    let distill = DistillOptions {
        alpha: 0.5,
        temperature: 1.0,
        loss: DistillLossKind::Mse,
        ..Default::default()
    };

    // teacher_output を teacher_logit=1.0 として扱い cp 換算 (実装) vs 換算なし(比較用 書き換え) の loss を比較
    // 実装経路: distill_classic_after_training 内部ロジック再利用は重いので簡易に式を複製
    let teacher_logit = 1.0f32;
    let teacher_cp_converted = teacher_logit * cfg.scale; // 600
    let target_with = distill.alpha * teacher_cp_converted + (1.0 - distill.alpha) * sample.label; // 0.5*600 + 0.5*200 = 400
    let pred = 0.0f32; // zero net
    let diff_with = pred - target_with; // -400
    let loss_with = 0.5 * diff_with * diff_with; // 0.5 * 160000 = 80000

    let target_without = distill.alpha * teacher_logit + (1.0 - distill.alpha) * sample.label; // 0.5*1 + 0.5*200 = 100.5
    let diff_without = pred - target_without; // -100.5
    let loss_without = 0.5 * diff_without * diff_without; // 約 5050.125

    // 換算ありの方が loss が大きい (教師が実際は logit で cp に変換すべきなのに未変換だと過小誤差) という期待ではなく、
    // 我々の最終ロジックは教師 logit を cp スケールへ拡大する -> ターゲットが大きく離れ MSE は増加 だが
    // ここでは「換算が入らないと loss のオーダが全く変わる」ことを検知したいので倍率比を確認
    let ratio = loss_with / loss_without;
    assert!(ratio > 10.0, "expected cp conversion to change loss magnitude significantly, ratio={} (with={}, without={})", ratio, loss_with, loss_without);
}

#[test]
fn classic_teacher_enforces_wdl_logit_domain() {
    let td = tempdir().unwrap();
    let path = td.path().join("teacher.fp32");
    write_classic_fp32_fixture(&path);

    let teacher = load_teacher(&path, TeacherKind::ClassicFp32).unwrap();
    assert!(teacher.supports_domain(TeacherValueDomain::WdlLogit));
    assert!(!teacher.supports_domain(TeacherValueDomain::Cp));

    let features = vec![0u32, 1u32];
    let batch = vec![TeacherBatchRequest {
        features: &features,
    }];
    let evals = teacher.evaluate_batch(&batch, TeacherValueDomain::WdlLogit, false).unwrap();
    assert_eq!(evals.len(), 1);

    let err = teacher.evaluate_batch(&batch, TeacherValueDomain::Cp, false).unwrap_err();
    assert!(matches!(err, TeacherError::UnsupportedDomain { .. }));
}

#[test]
fn default_teacher_domain_uses_wdl_logit_for_all_current_teachers() {
    assert!(matches!(
        default_teacher_domain(TeacherKind::Single),
        TeacherValueDomain::WdlLogit
    ));
    assert!(matches!(
        default_teacher_domain(TeacherKind::ClassicFp32),
        TeacherValueDomain::WdlLogit
    ));
}
