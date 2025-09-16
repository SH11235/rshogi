use super::{classic, dataset, export, logging, model, params, training, types};
use classic::*;
use dataset::*;
use export::*;
use logging::*;
use model::*;
use params::*;
use std::io::{Seek, SeekFrom, Write};
use tempfile::tempdir;
use tools::common::weighting as wcfg;
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

    let mut acc = vec![0i16; acc_dim];
    transformer.accumulate(&[0, 1, 2], &mut acc);

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
    let output = bundle.propagate_with_features(&features_us, &features_them);
    assert_eq!(output, -117);
}

#[test]
fn classic_v1_writer_emits_expected_layout() {
    let td = tempdir().unwrap();
    let path = td.path().join("classic.bin");
    let transformer =
        ClassicFeatureTransformerInt::new(vec![0x1234, -5, 7, 9, -10, 0x2222], vec![42, -7], 2);
    let network = ClassicQuantizedNetwork::new(ClassicQuantizedNetworkParams {
        hidden1_weights: vec![1, 2, 3, 4, 5, 6, 7, 8],
        hidden1_biases: vec![11, -12],
        hidden2_weights: vec![9, 8, 7, 6],
        hidden2_biases: vec![5, -4],
        output_weights: vec![3, -2],
        output_bias: 99,
        acc_dim: 2,
        h1_dim: 2,
        h2_dim: 2,
    });
    let bundle = ClassicIntNetworkBundle::new(transformer, network);
    write_classic_v1_bundle(&path, &bundle).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(bytes.len(), 70);
    assert_eq!(&bytes[0..4], b"NNUE");
    assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 1);
    assert_eq!(u32::from_le_bytes(bytes[8..12].try_into().unwrap()), CLASSIC_V1_ARCH_ID);
    assert_eq!(u32::from_le_bytes(bytes[12..16].try_into().unwrap()), 70u32);

    // First FT weight (0x1234)
    assert_eq!(i16::from_le_bytes(bytes[16..18].try_into().unwrap()), 0x1234);
    // Second bias (-7)
    let serialized = bundle.as_serialized();
    let bias_offset = 16 + serialized.ft_weights.len() * 2 + 4; // first bias consumed
    assert_eq!(i32::from_le_bytes(bytes[bias_offset..bias_offset + 4].try_into().unwrap()), -7);

    // Last 4 bytes = output bias (99)
    let tail = &bytes[bytes.len() - 4..];
    assert_eq!(i32::from_le_bytes(tail.try_into().unwrap()), 99);
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
    let network = Network::new(8, DEFAULT_RELU_CLIP_NUM, &mut rng);
    let mut export = ExportOptions::default();
    export.arch = ArchKind::Classic;
    export.format = ExportFormat::ClassicV1;
    finalize_export(&network, td.path(), export, false, None).unwrap();
    assert!(td.path().join("nn.classic.nnue").exists());
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
    let mut net = Network::new(8, DEFAULT_RELU_CLIP_NUM, &mut rng);
    // Zero out all weights/biases to make output exactly 0
    for w in net.w0.iter_mut() {
        *w = 0.0;
    }
    for b in net.b0.iter_mut() {
        *b = 0.0;
    }
    for w in net.w2.iter_mut() {
        *w = 0.0;
    }
    net.b2 = 0.0;

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
    };
    let auc = super::compute_val_auc(&net, &samples, &cfg);
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
    let mut net1 = Network::new(cfg.accumulator_dim, cfg.relu_clip, &mut rng1);
    let mut net2 = Network::new(cfg.accumulator_dim, cfg.relu_clip, &mut rng2);

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
        export: super::ExportOptions::default(),
        distill: super::DistillOptions::default(),
        classic_bundle: &mut classic_bundle1,
    };
    train_model(&mut net1, &mut samples1, &None, &cfg, &mut dummy_rng1, &mut ctx1).unwrap();
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
        export: super::ExportOptions::default(),
        distill: super::DistillOptions::default(),
        classic_bundle: &mut classic_bundle2,
    };
    train_model(&mut net2, &mut samples2, &None, &cfg, &mut dummy_rng2, &mut ctx2).unwrap();

    // 重みの一致を確認（厳密一致 or 十分小さい誤差）
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
    let mut net_inmem = Network::new(cfg_inmem.accumulator_dim, cfg_inmem.relu_clip, &mut rng1);
    let mut net_stream = Network::new(cfg_stream.accumulator_dim, cfg_stream.relu_clip, &mut rng2);

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
        export: super::ExportOptions::default(),
        distill: super::DistillOptions::default(),
        classic_bundle: &mut classic_bundle_in,
    };
    train_model(&mut net_inmem, &mut samples, &None, &cfg_inmem, &mut dummy_rng, &mut ctx_in)
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
        export: super::ExportOptions::default(),
        distill: super::DistillOptions::default(),
        classic_bundle: &mut classic_bundle_stream,
    };
    train_model_stream_cache(
        &mut net_stream,
        path.to_str().unwrap(),
        &None,
        &cfg_stream,
        &mut dummy_rng,
        &mut ctx_st,
        &wcfg::WeightingConfig::default(),
    )
    .unwrap();

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
    let mut net = Network::new(8, DEFAULT_RELU_CLIP_NUM, &mut rng);
    let cfg = Config {
        epochs: 1,
        batch_size: 2,
        learning_rate: 0.001,
        optimizer: "sgd".to_string(),
        l2_reg: 0.0,
        label_type: "cp".to_string(),
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
        export: ExportOptions::default(),
        distill: DistillOptions::default(),
        classic_bundle: &mut classic_bundle_log,
    };

    train_model_with_loader(
        &mut net,
        train_samples,
        &Some(val_samples),
        &cfg,
        &mut rand::rngs::StdRng::seed_from_u64(1234),
        &mut ctx,
    )
    .unwrap();

    // 構造化ログのバッファを明示的にフラッシュ
    if let Some(ref lg) = ctx.structured {
        if let Some(ref f) = lg.file {
            use std::io::Write as _;
            f.lock().unwrap().flush().unwrap();
        }
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
    };

    let mut rng = rand::rngs::StdRng::seed_from_u64(42);
    let mut net = Network::new(cfg.accumulator_dim, cfg.relu_clip, &mut rng);
    let before = net.clone();

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
        export: ExportOptions::default(),
        distill: DistillOptions::default(),
        classic_bundle: &mut classic_bundle_zero,
    };

    train_model(&mut net, &mut samples, &None, &cfg, &mut rng, &mut ctx).unwrap();

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
    };

    let mut rng = rand::rngs::StdRng::seed_from_u64(1);
    let mut net = Network::new(cfg.accumulator_dim, cfg.relu_clip, &mut rng);
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
        export: super::ExportOptions::default(),
        distill: super::DistillOptions::default(),
        classic_bundle: &mut classic_bundle_err,
    };
    let err = train_model_stream_cache(
        &mut net,
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
    };

    let td = tempfile::tempdir().unwrap();
    let out_dir = td.path();

    let mut rng = rand::rngs::StdRng::seed_from_u64(1);
    let mut net = Network::new(cfg.accumulator_dim, cfg.relu_clip, &mut rng);
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
        export: super::ExportOptions::default(),
        distill: super::DistillOptions::default(),
        classic_bundle: &mut classic_bundle_smoke,
    };
    train_model(&mut net, &mut samples, &None, &cfg, &mut rng, &mut ctx).unwrap();

    // NaN が混入していないこと
    assert!(net.w0.iter().all(|v| v.is_finite()));
    assert!(net.b0.iter().all(|v| v.is_finite()));
    assert!(net.w2.iter().all(|v| v.is_finite()));
    assert!(net.b2.is_finite());
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
