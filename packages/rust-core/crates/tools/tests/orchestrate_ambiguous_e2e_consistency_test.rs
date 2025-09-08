use assert_cmd::prelude::*;
use serde_json::Value;
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

fn write_pass1_jsonl(path: &PathBuf, n: usize) {
    // Minimal JSONL with sfen + best2_gap_cp to satisfy extractor
    // Use distinct SFENs to avoid dedup collapsing in merge
    let mut s = String::new();
    for i in 0..n {
        let rec = format!("{{\"sfen\":\"9/9/9/9/9/9/9/9/9 b - {}\",\"best2_gap_cp\":5}}\n", i + 1);
        s.push_str(&rec);
    }
    fs::write(path, s).expect("write pass1");
}

#[test]
fn orchestrate_ambiguous_e2e_counts_and_manifest_consistency() {
    // This E2E test avoids heavy search by providing a handcrafted pass1.
    let tmp = TempDir::new().unwrap();
    let pass1 = tmp.path().join("p1.jsonl");
    write_pass1_jsonl(&pass1, 3);

    let final_out = tmp.path().join("final.jsonl");
    let status = Command::cargo_bin("orchestrate_ambiguous")
        .expect("binary exists")
        .args([
            "--pass1",
            pass1.to_string_lossy().as_ref(),
            "--final",
            final_out.to_string_lossy().as_ref(),
            "--gap-threshold",
            "10", // extract all 3
            "--engine",
            "enhanced",
            "--multipv",
            "3",
            "--hash-mb",
            "16",
            "--split",
            "3",
            // keep generate fast
            "--time-limit-ms",
            "10",
        ])
        .status()
        .expect("run orchestrator");
    assert!(status.success());

    // Final outputs exist
    assert!(final_out.exists());
    let final_manifest = tmp
        .path()
        .join(format!("{}.manifest.json", final_out.file_stem().unwrap().to_string_lossy()));
    assert!(final_manifest.exists());

    // Orchestration manifest exists
    let orch_dir = tmp
        .path()
        .join(format!(".{}.ambdig", final_out.file_stem().unwrap().to_string_lossy()));
    let orch_manifest = orch_dir.join("orchestrate_ambiguous.manifest.json");
    assert!(orch_manifest.exists());

    // Check final manifest written_lines equals final file line count
    let mut s = String::new();
    fs::File::open(&final_out).unwrap().read_to_string(&mut s).unwrap();
    let final_lines = s.lines().count() as u64;
    let mani_txt = fs::read_to_string(&final_manifest).expect("read final manifest");
    let v: Value = serde_json::from_str(&mani_txt).expect("valid json");
    let w = v
        .get("aggregated")
        .and_then(|a| a.get("written_lines"))
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    assert_eq!(w, final_lines, "final manifest written_lines != file lines");

    // Check orchestration counts exist and are consistent in the relaxed sense
    let orch_txt = fs::read_to_string(&orch_manifest).expect("read orch manifest");
    let j: Value = serde_json::from_str(&orch_txt).expect("valid orch json");
    let counts = j.get("counts").expect("counts present");
    let pass1_total = counts.get("pass1_total").and_then(|x| x.as_u64()).unwrap_or(0);
    let extracted = counts.get("extracted").and_then(|x| x.as_u64()).unwrap_or(0);
    let final_written = counts.get("final_written").and_then(|x| x.as_u64()).unwrap_or(0);

    assert!(pass1_total >= extracted, "pass1_total should be >= extracted");
    // Merge may include pass1 when pass2 is empty; only check equality with final manifest here
    assert_eq!(final_written, w, "final_written should match final manifest");
}
