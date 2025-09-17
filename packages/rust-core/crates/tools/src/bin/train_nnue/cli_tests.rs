//! CLI integration tests for train_nnue

use crate::error_messages::*;
use assert_cmd::prelude::*;
use predicates::str::contains;
use std::fs;
use std::process::Command;
use tempfile::tempdir;

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
        .arg("--out")
        .arg(&out_dir);

    // Note: This will succeed (exit code 0) but fail during distillation
    // because dummy teacher file is invalid. The important thing is that
    // it gets past the quantization checks.
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
