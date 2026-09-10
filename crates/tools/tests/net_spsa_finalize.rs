use std::process::{Command, Output};

use rshogi_core::nnue::net_bin_layout::LayerStacksBinLayout;
use rshogi_core::nnue::net_delta::test_utils::{
    SyntheticFtEncoding, build_synthetic_layer_stacks_with_ft_encoding,
};
use rshogi_core::nnue::{HALFKA_HM_DIMENSIONS, HALFKP_DIMENSIONS, NetCoefficientId, NetTensorKind};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::tempdir;
use tools::spsa_param_mapping::parse_param_line;

fn run(binary: &str, args: &[&str]) -> Output {
    Command::new(binary).args(args).output().expect("run command")
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "status={}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn generator_apply_generator_round_trip_and_fail_closed_inputs() {
    let temp = tempdir().expect("tempdir");
    let base_path = temp.path().join("base.bin");
    let params_path = temp.path().join("final.params");
    let output_path = temp.path().join("tuned.bin");
    let regenerated_path = temp.path().join("regenerated.params");
    let report_path = temp.path().join("report.json");
    let synthetic = build_synthetic_layer_stacks_with_ft_encoding(
        "HalfKaHmMerged",
        HALFKA_HM_DIMENSIONS,
        512,
        16,
        32,
        4,
        SyntheticFtEncoding::Leb128Split,
    );
    std::fs::write(&base_path, &synthetic.bytes).expect("base net");
    let base_layout = LayerStacksBinLayout::from_bytes(&synthetic.bytes).expect("base layout");
    let tuned_id = NetCoefficientId {
        kind: NetTensorKind::OutputBias,
        bucket: Some(0),
        index: 0,
    };
    let original_base = base_layout.coefficient(&tuned_id).expect("base coefficient");

    let generated = run(
        env!("CARGO_BIN_EXE_generate_net_spsa_params"),
        &[
            "--nnue",
            base_path.to_str().expect("base path"),
            "--output",
            params_path.to_str().expect("params path"),
            "--targets",
            "out_b",
        ],
    );
    assert_success(&generated);
    let params = std::fs::read_to_string(&params_path).expect("params");
    let tuned_params = params.replacen(",int,0,", ",int,1.6,", 1);
    std::fs::write(&params_path, tuned_params).expect("tuned params");

    let applied = run(
        env!("CARGO_BIN_EXE_apply_net_spsa_params"),
        &[
            "--nnue",
            base_path.to_str().expect("base path"),
            "--params",
            params_path.to_str().expect("params path"),
            "--output",
            output_path.to_str().expect("output path"),
            "--report",
            report_path.to_str().expect("report path"),
        ],
    );
    assert_success(&applied);
    assert!(String::from_utf8_lossy(&applied.stdout).contains("applied=4 nonzero=1 clamped=0"));
    let report: Value =
        serde_json::from_slice(&std::fs::read(&report_path).expect("report")).expect("report JSON");
    assert_eq!(report["applied_count"], 4);
    assert_eq!(report["nonzero_delta_count"], 1);
    assert_eq!(report["clamped_count"], 0);

    let regenerated = run(
        env!("CARGO_BIN_EXE_generate_net_spsa_params"),
        &[
            "--nnue",
            output_path.to_str().expect("output path"),
            "--output",
            regenerated_path.to_str().expect("regenerated path"),
            "--targets",
            "out_b",
        ],
    );
    assert_success(&regenerated);
    let regenerated = std::fs::read_to_string(&regenerated_path).expect("regenerated params");
    let first_row = regenerated
        .lines()
        .enumerate()
        .find_map(|(index, line)| parse_param_line(line, index + 1).expect("parse row"))
        .expect("first row");
    assert_eq!(first_row.name, tuned_id.usi_name());
    assert_eq!(first_row.comment, format!("base={}", original_base + 2));

    let mismatched_path = temp.path().join("mismatched.params");
    let mismatch_output = temp.path().join("mismatch.bin");
    let mismatched = std::fs::read_to_string(&params_path)
        .expect("params")
        .replacen(
            params.lines().next().expect("metadata"),
            "# net=other.bin sha256=0000000000000000000000000000000000000000000000000000000000000000 arch=x buckets=4",
            1,
        );
    std::fs::write(&mismatched_path, mismatched).expect("mismatched params");
    let rejected = run(
        env!("CARGO_BIN_EXE_apply_net_spsa_params"),
        &[
            "--nnue",
            base_path.to_str().expect("base path"),
            "--params",
            mismatched_path.to_str().expect("mismatch params"),
            "--output",
            mismatch_output.to_str().expect("mismatch output"),
        ],
    );
    assert!(!rejected.status.success());
    assert!(!mismatch_output.exists());
    let allowed = run(
        env!("CARGO_BIN_EXE_apply_net_spsa_params"),
        &[
            "--nnue",
            base_path.to_str().expect("base path"),
            "--params",
            mismatched_path.to_str().expect("mismatch params"),
            "--output",
            mismatch_output.to_str().expect("mismatch output"),
            "--allow-net-mismatch",
        ],
    );
    assert_success(&allowed);

    let mixed_path = temp.path().join("mixed.params");
    let mixed_output = temp.path().join("mixed.bin");
    let mut mixed = std::fs::read_to_string(&params_path).expect("params");
    mixed.push_str("SPSA_FUTILITY_MARGIN_BASE,int,1,-3,3,1,0.002\n");
    std::fs::write(&mixed_path, mixed).expect("mixed params");
    let rejected = run(
        env!("CARGO_BIN_EXE_apply_net_spsa_params"),
        &[
            "--nnue",
            base_path.to_str().expect("base path"),
            "--params",
            mixed_path.to_str().expect("mixed params"),
            "--output",
            mixed_output.to_str().expect("mixed output"),
        ],
    );
    assert!(!rejected.status.success());
    assert!(!mixed_output.exists());
}

#[test]
fn applies_spsa_final_params_without_comments_using_expected_sha256() {
    let temp = tempdir().expect("tempdir");
    let base_path = temp.path().join("base.bin");
    let params_path = temp.path().join("final.params");
    let output_path = temp.path().join("tuned.bin");
    let synthetic = build_synthetic_layer_stacks_with_ft_encoding(
        "HalfKP",
        HALFKP_DIMENSIONS,
        2,
        2,
        1,
        2,
        SyntheticFtEncoding::Leb128Combined,
    );
    std::fs::write(&base_path, &synthetic.bytes).expect("base net");
    std::fs::write(&params_path, "SPSA_NET_out_b_b0_0,int,1.600000,-3,3,1,0.002\n")
        .expect("final params");
    let expected_sha256 = format!("{:x}", Sha256::digest(&synthetic.bytes));

    let applied = run(
        env!("CARGO_BIN_EXE_apply_net_spsa_params"),
        &[
            "--nnue",
            base_path.to_str().expect("base path"),
            "--params",
            params_path.to_str().expect("params path"),
            "--output",
            output_path.to_str().expect("output path"),
            "--expected-net-sha256",
            &expected_sha256,
        ],
    );
    assert_success(&applied);

    let input_layout = LayerStacksBinLayout::from_bytes(&synthetic.bytes).expect("input layout");
    let output = std::fs::read(&output_path).expect("output");
    let output_layout = LayerStacksBinLayout::from_bytes(&output).expect("output layout");
    let id = NetCoefficientId {
        kind: NetTensorKind::OutputBias,
        bucket: Some(0),
        index: 0,
    };
    assert_eq!(
        output_layout.coefficient(&id).expect("output coefficient"),
        input_layout.coefficient(&id).expect("input coefficient") + 2
    );
    // 同じ ID の別表記を追加しても、既存の確定 net を上書きしない。
    std::fs::write(&params_path, "# fixture\nSPSA_NET_out_b_b0_0,int,1,-3,3,1,0.002\nSPSA_NET_out_b_b00_00,int,2,-3,3,1,0.002\n").unwrap();
    let rejected = run(
        env!("CARGO_BIN_EXE_apply_net_spsa_params"),
        &[
            "--nnue",
            base_path.to_str().unwrap(),
            "--params",
            params_path.to_str().unwrap(),
            "--output",
            output_path.to_str().unwrap(),
            "--expected-net-sha256",
            &expected_sha256,
        ],
    );
    assert!(!rejected.status.success());
    let error = String::from_utf8_lossy(&rejected.stderr);
    assert!(error.contains("line 3") && error.contains("line 2"), "{error}");
    assert_eq!(std::fs::read(&output_path).unwrap(), output);
}

#[test]
fn rejects_params_without_metadata_or_source_hash_option() {
    let temp = tempdir().expect("tempdir");
    let base_path = temp.path().join("base.bin");
    let params_path = temp.path().join("final.params");
    let output_path = temp.path().join("tuned.bin");
    let synthetic = build_synthetic_layer_stacks_with_ft_encoding(
        "HalfKP",
        HALFKP_DIMENSIONS,
        2,
        2,
        1,
        2,
        SyntheticFtEncoding::Leb128Combined,
    );
    std::fs::write(&base_path, &synthetic.bytes).expect("base net");
    std::fs::write(&params_path, "SPSA_NET_out_b_b0_0,int,1.600000,-3,3,1,0.002\n")
        .expect("final params");

    let rejected = run(
        env!("CARGO_BIN_EXE_apply_net_spsa_params"),
        &[
            "--nnue",
            base_path.to_str().expect("base path"),
            "--params",
            params_path.to_str().expect("params path"),
            "--output",
            output_path.to_str().expect("output path"),
        ],
    );
    assert!(!rejected.status.success());
    assert!(!output_path.exists());
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert!(stderr.contains("--expected-net-sha256"));
    assert!(stderr.contains("--allow-net-mismatch"));
}

#[test]
fn keeps_finalized_net_and_removes_staged_files_when_report_rename_fails() {
    let temp = tempdir().expect("tempdir");
    let base_path = temp.path().join("base.bin");
    let params_path = temp.path().join("final.params");
    let output_path = temp.path().join("tuned.bin");
    let report_path = temp.path().join("report-dir");
    let synthetic = build_synthetic_layer_stacks_with_ft_encoding(
        "HalfKP",
        HALFKP_DIMENSIONS,
        2,
        2,
        1,
        2,
        SyntheticFtEncoding::Leb128Combined,
    );
    std::fs::write(&base_path, &synthetic.bytes).expect("base net");
    std::fs::write(&params_path, "SPSA_NET_out_b_b0_0,int,1.000000,-3,3,1,0.002\n")
        .expect("final params");
    std::fs::create_dir(&report_path).expect("report directory");
    let expected_sha256 = format!("{:x}", Sha256::digest(&synthetic.bytes));

    let rejected = run(
        env!("CARGO_BIN_EXE_apply_net_spsa_params"),
        &[
            "--nnue",
            base_path.to_str().expect("base path"),
            "--params",
            params_path.to_str().expect("params path"),
            "--output",
            output_path.to_str().expect("output path"),
            "--expected-net-sha256",
            &expected_sha256,
            "--report",
            report_path.to_str().expect("report path"),
        ],
    );
    assert!(!rejected.status.success());
    assert!(output_path.exists(), "net is finalized before the report");
    assert!(std::fs::read_dir(&report_path).expect("report dir").next().is_none());
    let leftovers: Vec<_> = std::fs::read_dir(temp.path())
        .expect("temp contents")
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(".apply_net_spsa_params_"))
        .collect();
    assert!(leftovers.is_empty(), "staged files remain: {leftovers:?}");
}
