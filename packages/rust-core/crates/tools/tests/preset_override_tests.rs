use assert_cmd::prelude::*;
use predicates::prelude::*;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

fn make_empty_input(tmp: &TempDir) -> PathBuf {
    let p = tmp.path().join("input.sfen.txt");
    fs::File::create(&p).expect("create input");
    p
}

fn run_gen(args: &[&str], tmp: &TempDir) -> (assert_cmd::assert::Assert, PathBuf) {
    let input = make_empty_input(tmp);
    let out = tmp.path().join("out.jsonl");
    let mut cmd = Command::cargo_bin("generate_nnue_training_data").expect("binary exists");
    let mut full_args: Vec<String> = vec![
        input.to_string_lossy().to_string(),
        out.to_string_lossy().to_string(),
        "2".into(),  // depth
        "10".into(), // batch_size
        "0".into(),  // resume_from
    ];
    full_args.extend(args.iter().map(|s| s.to_string()));
    let assert = cmd.args(&full_args).assert();
    (assert, out)
}

#[test]
fn preset_balanced_with_multipv_override() {
    let tmp = TempDir::new().unwrap();
    let (assert, out) = run_gen(&["--preset", "balanced", "--multipv", "3"], &tmp);
    assert
        .success()
        .stdout(predicate::str::contains("Preset Balanced"))
        .stdout(predicate::str::contains("multipv=3"));
    // manifest should exist and reflect overrides
    let manifest = tmp.path().join("out.manifest.json");
    let txt = fs::read_to_string(&manifest).expect("manifest exists");
    assert!(txt.contains("\"preset\": \"balanced\""));
    assert!(txt.contains("\"multipv\": true")); // overrides.multipv
    let _ = out; // silence unused
}

#[test]
fn preset_high_with_nodes_switches_mode() {
    let tmp = TempDir::new().unwrap();
    let (assert, out) = run_gen(&["--preset", "high", "--nodes", "5000000"], &tmp);
    assert.success().stdout(predicate::str::contains("Preset High")).stdout(
        predicate::str::contains("mode=nodes").and(predicate::str::contains("nodes=5000000")),
    );
    let manifest = tmp.path().join("out.manifest.json");
    let txt = fs::read_to_string(&manifest).expect("manifest exists");
    assert!(txt.contains("\"preset\": \"high\""));
    assert!(txt.contains("\"mode\": \"nodes\""));
    assert!(txt.contains("5000000"));
    assert!(txt.contains("\"nodes\": true")); // overrides.nodes
    let _ = out;
}

#[test]
fn preset_baseline_time_override_order_invariant() {
    let tmp1 = TempDir::new().unwrap();
    let (assert1, _) = run_gen(&["--preset", "baseline", "--time-limit-ms", "150"], &tmp1);
    assert1
        .success()
        .stdout(predicate::str::contains("Preset Baseline"))
        .stdout(predicate::str::contains("mode=time"))
        .stdout(predicate::str::contains("time_ms=150"));

    let tmp2 = TempDir::new().unwrap();
    let (assert2, _) = run_gen(&["--time-limit-ms", "150", "--preset", "baseline"], &tmp2);
    assert2
        .success()
        .stdout(predicate::str::contains("Preset Baseline"))
        .stdout(predicate::str::contains("mode=time"))
        .stdout(predicate::str::contains("time_ms=150"));

    let manifest = tmp2.path().join("out.manifest.json");
    let txt = fs::read_to_string(&manifest).expect("manifest exists");
    assert!(txt.contains("\"preset\": \"baseline\""));
    assert!(txt.contains("\"time\": true")); // overrides.time
}
