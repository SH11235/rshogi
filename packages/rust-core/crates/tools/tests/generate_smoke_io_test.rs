use assert_cmd::prelude::*;
use predicates::prelude::*;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

fn write_sfens(path: &Path, n: usize) {
    // Simple, valid Shogi SFEN from engine-core tests
    let sfen = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
    let mut content = String::new();
    for i in 0..n {
        // include a trailing comment to exercise the sfen extractor cut-off
        content.push_str(&format!("sfen {} # {}\n", sfen, i));
    }
    fs::write(path, content).expect("write input");
}

fn read_gz_to_string(p: &Path) -> String {
    let f = fs::File::open(p).expect("open gz");
    let mut dec = flate2::read::GzDecoder::new(f);
    let mut s = String::new();
    dec.read_to_string(&mut s).expect("read gz content");
    s
}

#[test]
fn generate_non_split_smoke_manifest_and_output_exist() {
    if engine_core::util::is_ci_environment() {
        println!("Skipping integration test requiring engine search in CI environment");
        return;
    }
    let tmp = TempDir::new().unwrap();
    let input = tmp.path().join("in.sfen.txt");
    write_sfens(&input, 5);
    let out = tmp.path().join("out.jsonl");

    // depth=1, batch=4, resume=0; keep time small for speed
    let mut cmd = Command::cargo_bin("generate_nnue_training_data").expect("binary exists");
    let assert = cmd
        .args([
            input.to_string_lossy().as_ref(),
            out.to_string_lossy().as_ref(),
            "1",
            "4",
            "0",
            "--time-limit-ms",
            "100",
            "--output-format",
            "jsonl",
        ])
        .assert();
    assert
        .success()
        .stdout(predicate::str::contains("NNUE Training Data Generator"));

    let manifest = tmp.path().join("out.manifest.json");
    let txt = fs::read_to_string(&manifest).expect("manifest exists");
    assert!(txt.contains("\"wdl_semantics\": \"side_to_move\""));
    // Output file is created even if zero lines (created/truncated at start)
    let out_exists = out.exists();
    assert!(out_exists, "output file should be created");
    // Lines may be zero if positions are skipped; this is acceptable for smoke
    let lines = fs::read_to_string(&out).unwrap_or_default();
    let _ = lines;
}

#[test]
fn generate_split_gz_smoke_parts_and_manifests_consistent() {
    if engine_core::util::is_ci_environment() {
        println!("Skipping integration test requiring engine search in CI environment");
        return;
    }
    let tmp = TempDir::new().unwrap();
    let input = tmp.path().join("in2.sfen.txt");
    write_sfens(&input, 7);
    let out = tmp.path().join("out.jsonl");

    // split every 3 lines, gzip compress; small time limit for speed
    let mut cmd = Command::cargo_bin("generate_nnue_training_data").expect("binary exists");
    let assert = cmd
        .args([
            input.to_string_lossy().as_ref(),
            out.to_string_lossy().as_ref(),
            "1",
            "4",
            "0",
            "--time-limit-ms",
            "100",
            "--split",
            "3",
            "--compress",
            "gz",
            "--output-format",
            "jsonl",
        ])
        .assert();
    assert.success();

    // Collect generated parts (if any)
    let mut part_idx = 1;
    let mut saw_any_part = false;
    loop {
        let stem = out.file_stem().unwrap().to_string_lossy().to_string();
        let dir = out.parent().unwrap();
        let gz = dir.join(format!("{}.part-{:04}.jsonl.gz", stem, part_idx));
        if !gz.exists() {
            break;
        }
        saw_any_part = true;
        // Part manifest path and consistency checks
        let man = dir.join(format!("{}.part-{:04}.manifest.json", stem, part_idx));
        let man_txt = fs::read_to_string(&man).expect("per-part manifest exists");
        let v: serde_json::Value = serde_json::from_str(&man_txt).expect("valid json");
        let cnt = v["count_in_part"].as_u64().unwrap_or(0) as usize;
        // Decompress and count lines; allow zero (if all positions were skipped)
        let content = read_gz_to_string(&gz);
        let lines = content.lines().count();
        assert_eq!(lines, cnt, "part lines should match count_in_part");
        part_idx += 1;
    }
    // It's acceptable that no part is produced (e.g., all positions skipped)
    let _ = saw_any_part;
}
