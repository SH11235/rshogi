//! CLI integration tests for train_nnue

use crate::error_messages::*;
use assert_cmd::prelude::*;
use predicates::str::contains;
use std::fs::{self, File};
use std::io::Write;
use std::process::Command;
use tempfile::tempdir;
use tools::nnfc_v1::FEATURE_SET_ID_HALF;

fn write_minimal_nnfc_cache(path: &std::path::Path) {
    let mut f = File::create(path).unwrap();
    let header_size: u32 = 48;
    f.write_all(b"NNFC").unwrap();
    f.write_all(&1u32.to_le_bytes()).unwrap();
    f.write_all(&FEATURE_SET_ID_HALF.to_le_bytes()).unwrap();
    f.write_all(&0u64.to_le_bytes()).unwrap();
    f.write_all(&1024u32.to_le_bytes()).unwrap();
    f.write_all(&header_size.to_le_bytes()).unwrap();
    f.write_all(&[0]).unwrap(); // little-endian
    f.write_all(&[0]).unwrap(); // raw payload
    f.write_all(&[0u8; 2]).unwrap();
    let payload_offset = 4u64 + header_size as u64;
    f.write_all(&payload_offset.to_le_bytes()).unwrap();
    f.write_all(&0u32.to_le_bytes()).unwrap(); // sample_flags_mask
    if header_size as usize > 40 {
        f.write_all(&vec![0u8; header_size as usize - 40]).unwrap();
    }
    f.flush().unwrap();
}

#[test]
fn cli_errors_on_classic_out_per_channel() {
    let td = tempdir().unwrap();

    // Create minimal JSONL input
    let input = td.path().join("one.jsonl");
    fs::write(&input, r#"{"sfen":"lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1","eval":0,"depth":10,"seldepth":10,"bound1":"Exact"}"#).unwrap();

    // Create dummy teacher file (required for classic export)
    let teacher = td.path().join("teacher.fp32.bin");
    fs::write(&teacher, b"dummy").unwrap();

    let mut cmd = Command::cargo_bin("train_nnue").unwrap();
    cmd.arg("--input")
        .arg(&input)
        .arg("--arch")
        .arg("classic")
        .arg("--export-format")
        .arg("classic-v1")
        .arg("--distill-from-single")
        .arg(&teacher)
        .arg("--quant-out")
        .arg("per-channel")
        .arg("--epochs")
        .arg("1")
        .arg("--batch-size")
        .arg("1");

    cmd.assert().failure().stderr(contains(ERR_CLASSIC_OUT_PER_CHANNEL));
}

#[test]
fn cli_errors_on_classic_ft_per_channel() {
    let td = tempdir().unwrap();

    // Create minimal JSONL input
    let input = td.path().join("one.jsonl");
    fs::write(&input, r#"{"sfen":"lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1","eval":0,"depth":10,"seldepth":10,"bound1":"Exact"}"#).unwrap();

    // Create dummy teacher file
    let teacher = td.path().join("teacher.fp32.bin");
    fs::write(&teacher, b"dummy").unwrap();

    let mut cmd = Command::cargo_bin("train_nnue").unwrap();
    cmd.arg("--input")
        .arg(&input)
        .arg("--arch")
        .arg("classic")
        .arg("--export-format")
        .arg("classic-v1")
        .arg("--distill-from-single")
        .arg(&teacher)
        .arg("--quant-ft")
        .arg("per-channel")
        .arg("--epochs")
        .arg("1")
        .arg("--batch-size")
        .arg("1");

    cmd.assert().failure().stderr(contains(ERR_CLASSIC_FT_PER_CHANNEL));
}

#[test]
fn cli_accepts_classic_per_tensor() {
    let td = tempdir().unwrap();

    // Create minimal JSONL input
    let input = td.path().join("one.jsonl");
    fs::write(&input, r#"{"sfen":"lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1","eval":0,"depth":10,"seldepth":10,"bound1":"Exact"}"#).unwrap();

    // Create dummy teacher file
    let teacher = td.path().join("teacher.fp32.bin");
    fs::write(&teacher, b"dummy").unwrap();

    let out_dir = td.path().join("output");

    let mut cmd = Command::cargo_bin("train_nnue").unwrap();
    cmd.arg("--input")
        .arg(&input)
        .arg("--arch")
        .arg("classic")
        .arg("--export-format")
        .arg("classic-v1")
        .arg("--distill-from-single")
        .arg(&teacher)
        .arg("--quant-ft")
        .arg("per-tensor")
        .arg("--quant-out")
        .arg("per-tensor")
        .arg("--epochs")
        .arg("1")
        .arg("--batch-size")
        .arg("1")
        .arg("--opt")
        .arg("sgd")
        .arg("--out")
        .arg(&out_dir);

    // This should advance past quantization validation, then fail when
    // attempting to load the dummy teacher network. Validate that the
    // failure reason mentions the teacher load rather than quantization.
    cmd.assert()
        .success()
        .stdout(contains("Failed to load teacher network for classic distillation"));
}

#[test]
fn cli_errors_on_classic_without_teacher() {
    let td = tempdir().unwrap();

    // Create minimal JSONL input
    let input = td.path().join("one.jsonl");
    fs::write(&input, r#"{"sfen":"lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1","eval":0,"depth":10,"seldepth":10,"bound1":"Exact"}"#).unwrap();

    let mut cmd = Command::cargo_bin("train_nnue").unwrap();
    cmd.arg("--input")
        .arg(&input)
        .arg("--arch")
        .arg("classic")
        .arg("--export-format")
        .arg("classic-v1")
        .arg("--epochs")
        .arg("1")
        .arg("--batch-size")
        .arg("1");

    cmd.assert().failure().stderr(contains(ERR_CLASSIC_NEEDS_TEACHER));
}

#[test]
fn cli_errors_on_single_with_classic_v1_format() {
    let td = tempdir().unwrap();

    // Create minimal JSONL input
    let input = td.path().join("one.jsonl");
    fs::write(&input, r#"{"sfen":"lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1","eval":0,"depth":10,"seldepth":10,"bound1":"Exact"}"#).unwrap();

    let mut cmd = Command::cargo_bin("train_nnue").unwrap();
    cmd.arg("--input")
        .arg(&input)
        .arg("--arch")
        .arg("single")
        .arg("--export-format")
        .arg("classic-v1")
        .arg("--epochs")
        .arg("1")
        .arg("--batch-size")
        .arg("1");

    cmd.assert().failure().stderr(contains(ERR_SINGLE_NO_CLASSIC_V1));
}

#[test]
fn cli_errors_on_classic_v1_stream_cache_without_distill() {
    let td = tempdir().unwrap();

    let cache_path = td.path().join("stream.cache");
    write_minimal_nnfc_cache(&cache_path);

    let teacher = td.path().join("teacher.fp32.bin");
    fs::write(&teacher, b"dummy").unwrap();

    let out_dir = td.path().join("output");

    let mut cmd = Command::cargo_bin("train_nnue").unwrap();
    cmd.arg("--input")
        .arg(&cache_path)
        .arg("--arch")
        .arg("classic")
        .arg("--export-format")
        .arg("classic-v1")
        .arg("--distill-from-single")
        .arg(&teacher)
        .arg("--stream-cache")
        .arg("--epochs")
        .arg("1")
        .arg("--batch-size")
        .arg("1")
        .arg("--opt")
        .arg("sgd")
        .arg("--out")
        .arg(&out_dir);

    cmd.assert().failure().stderr(contains(ERR_CLASSIC_STREAM_NEEDS_DISTILL));
}

#[test]
fn cli_errors_on_classic_with_single_i8_format() {
    let td = tempdir().unwrap();

    // Create minimal JSONL input
    let input = td.path().join("one.jsonl");
    fs::write(&input, r#"{"sfen":"lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1","eval":0,"depth":10,"seldepth":10,"bound1":"Exact"}"#).unwrap();

    // Create dummy teacher file
    let teacher = td.path().join("teacher.fp32.bin");
    fs::write(&teacher, b"dummy").unwrap();

    let mut cmd = Command::cargo_bin("train_nnue").unwrap();
    cmd.arg("--input")
        .arg(&input)
        .arg("--arch")
        .arg("classic")
        .arg("--export-format")
        .arg("single-i8")
        .arg("--distill-from-single")
        .arg(&teacher)
        .arg("--epochs")
        .arg("1")
        .arg("--batch-size")
        .arg("1");

    cmd.assert().failure().stderr(contains(ERR_CLASSIC_NO_SINGLE_I8));
}
