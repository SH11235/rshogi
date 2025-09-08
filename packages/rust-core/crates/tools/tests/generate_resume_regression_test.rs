use assert_cmd::prelude::*;
use std::fs;
use std::io::Read;
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

fn count_lines(path: &Path) -> usize {
    match fs::read_to_string(path) {
        Ok(s) => s.lines().count(),
        Err(_) => 0,
    }
}

#[test]
fn resume_twice_no_duplicate_and_manifest_v2_present() {
    let tmp = TempDir::new().unwrap();
    let input = tmp.path().join("resume_in.sfen.txt");
    write_sfens(&input, 8);
    let out = tmp.path().join("resume_out.jsonl");

    // First run
    let mut cmd1 = Command::cargo_bin("generate_nnue_training_data").expect("binary exists");
    let res1 = cmd1
        .args([
            input.to_string_lossy().as_ref(),
            out.to_string_lossy().as_ref(),
            "1",
            "4",
            "0",
            "--time-limit-ms",
            "100",
            "--engine",
            "material",
            "--output-format",
            "jsonl",
        ])
        .status()
        .expect("run generate first");
    assert!(res1.success());

    // Manifest should exist and contain teacher_engine (v2 provenance)
    let manifest_path = tmp.path().join("resume_out.manifest.json");
    let mtxt = fs::read_to_string(&manifest_path).expect("manifest exists");
    let mv: serde_json::Value = serde_json::from_str(&mtxt).expect("manifest json");
    assert!(
        mv.get("teacher_engine").is_some(),
        "missing teacher_engine in manifest: {}",
        mtxt
    );
    assert!(
        mv.get("generation_command").and_then(|v| v.as_str()).is_some(),
        "missing generation_command"
    );
    assert!(mv.get("seed").is_some(), "missing seed");
    assert_eq!(mv.get("manifest_version").and_then(|v| v.as_str()).unwrap_or(""), "2");
    let input_obj = mv.get("input").and_then(|v| v.as_object()).expect("input provenance");
    assert!(input_obj.get("path").is_some(), "missing input.path");
    assert!(input_obj.get("sha256").is_some(), "missing input.sha256");
    assert!(input_obj.get("bytes").is_some(), "missing input.bytes");

    // Count lines after first run
    let c1 = count_lines(&out);

    // Second run with the same args must not append duplicates
    let mut cmd2 = Command::cargo_bin("generate_nnue_training_data").expect("binary exists");
    let res2 = cmd2
        .args([
            input.to_string_lossy().as_ref(),
            out.to_string_lossy().as_ref(),
            "1",
            "4",
            "0",
            "--time-limit-ms",
            "100",
            "--engine",
            "material",
            "--output-format",
            "jsonl",
        ])
        .status()
        .expect("run generate second");
    assert!(res2.success());

    let c2 = count_lines(&out);
    assert_eq!(c2, c1, "second run should not append lines");

    // Progress file should reflect attempted positions and never be less than the successful lines
    let prog = out.with_extension("progress");
    if let Ok(s) = fs::read_to_string(&prog) {
        if let Ok(v) = s.trim().parse::<usize>() {
            assert!(v >= c2, "progress {} should be >= lines {}", v, c2);
        }
    }
}
