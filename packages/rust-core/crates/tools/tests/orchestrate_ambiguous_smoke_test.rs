use assert_cmd::prelude::*;
use predicates::prelude::*;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

fn write_sfens(path: &Path, n: usize) {
    // Valid Shogi SFEN from engine-core tests
    let sfen = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
    let mut content = String::new();
    for i in 0..n {
        // include a trailing comment to exercise the sfen extractor cut-off
        content.push_str(&format!("sfen {} # {}\n", sfen, i));
    }
    fs::write(path, content).expect("write input");
}

#[test]
fn orchestrate_ambiguous_end_to_end_smoke() {
    if engine_core::util::is_ci_environment() {
        println!("Skipping integration test requiring engine search in CI environment");
        return;
    }
    let tmp = TempDir::new().unwrap();
    let input = tmp.path().join("in.sfen.txt");
    write_sfens(&input, 8);

    // Step 1: generate pass1 with multipv=2 so best2_gap_cp is present
    let pass1 = tmp.path().join("p1.jsonl");
    let mut gen = Command::cargo_bin("generate_nnue_training_data").expect("binary exists");
    let assert = gen
        .args([
            input.to_string_lossy().as_ref(),
            pass1.to_string_lossy().as_ref(),
            "1",
            "4",
            "0",
            "--time-limit-ms",
            "100",
            "--output-format",
            "jsonl",
            "--multipv",
            "2",
            "--hash-mb",
            "16",
        ])
        .assert();
    assert
        .success()
        .stdout(predicate::str::contains("NNUE Training Data Generator"));

    // Step 2: orchestrate ambiguous mining
    let final_out = tmp.path().join("final.jsonl");
    let mut orch = Command::cargo_bin("orchestrate_ambiguous").expect("binary exists");
    let assert = orch
        .args([
            "--pass1",
            pass1.to_string_lossy().as_ref(),
            "--final",
            final_out.to_string_lossy().as_ref(),
            "--gap-threshold",
            "9999",
            "--engine",
            "enhanced",
            "--multipv",
            "3",
            "--hash-mb",
            "16",
            "--split",
            "3",
            "--compress",
            "gz",
        ])
        .assert();
    assert.success();

    // Final outputs exist
    let final_exists = final_out.exists();
    assert!(final_exists, "final output should exist");
    let final_manifest = tmp
        .path()
        .join(format!("{}.manifest.json", final_out.file_stem().unwrap().to_string_lossy()));
    assert!(final_manifest.exists(), "final manifest should exist");

    // Orchestration manifest exists
    let orch_dir = tmp
        .path()
        .join(format!(".{}.ambdig", final_out.file_stem().unwrap().to_string_lossy()));
    let orch_manifest = orch_dir.join("orchestrate_ambiguous.manifest.json");
    assert!(orch_manifest.exists(), "orchestration manifest should exist");

    // Read and lightly validate manifests
    let txt = fs::read_to_string(&final_manifest).expect("read final manifest");
    let v: serde_json::Value = serde_json::from_str(&txt).expect("valid json");
    assert!(v.get("aggregated").is_some(), "final manifest aggregated present");

    let mut s = String::new();
    fs::File::open(&final_out).unwrap().read_to_string(&mut s).unwrap();
    // zero lines is acceptable in smoke test (depending on search outcomes)
    let _lines = s.lines().count();
}
