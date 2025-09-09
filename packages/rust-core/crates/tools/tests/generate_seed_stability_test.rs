use assert_cmd::prelude::*;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

fn write_sfens(path: &Path, n: usize) {
    let sfen = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
    let mut content = String::new();
    for i in 0..n {
        content.push_str(&format!("sfen {} # {}\n", sfen, i));
    }
    fs::write(path, content).expect("write input");
}

fn stable_seed_from_args(args: &[&str]) -> u64 {
    let mut hasher = Sha256::new();
    for a in args.iter() {
        hasher.update(a.as_bytes());
        hasher.update([0]);
    }
    let d = hasher.finalize();
    u64::from_le_bytes(d[0..8].try_into().unwrap())
}

#[test]
fn seed_is_stable_from_args_and_independent_of_argv0() {
    if engine_core::util::is_ci_environment() {
        println!("Skipping integration test requiring engine search in CI environment");
        return;
    }
    let tmp = TempDir::new().unwrap();
    let input = tmp.path().join("in.sfen.txt");
    write_sfens(&input, 3);
    let out = tmp.path().join("out.jsonl");

    // Build the exact arg list we pass to the generator (argv[1..])
    let arg_list = vec![
        input.to_string_lossy().to_string(),
        out.to_string_lossy().to_string(),
        "1".into(),
        "4".into(),
        "0".into(),
        "--time-limit-ms".into(),
        "100".into(),
        "--engine".into(),
        "material".into(),
        "--output-format".into(),
        "jsonl".into(),
    ];
    let mut cmd = Command::cargo_bin("generate_nnue_training_data").expect("binary exists");
    let status = cmd.args(arg_list.clone()).status().expect("run");
    assert!(status.success());

    let man = tmp.path().join("out.manifest.json");
    let txt = fs::read_to_string(&man).expect("manifest exists");
    let v: serde_json::Value = serde_json::from_str(&txt).expect("json");
    let seed = v.get("seed").and_then(|x| x.as_u64()).expect("seed");

    let args_ref: Vec<&str> = arg_list.iter().map(|s| s.as_str()).collect();
    let expected = stable_seed_from_args(&args_ref);
    assert_eq!(seed, expected, "seed should be stable and match expected");
}
