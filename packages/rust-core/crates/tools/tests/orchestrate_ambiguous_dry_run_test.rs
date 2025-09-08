use assert_cmd::prelude::*;
use std::process::Command;
use tempfile::TempDir;

#[test]
fn orchestrate_ambiguous_dry_run_prints_full_plan() {
    let tmp = TempDir::new().unwrap();
    // create a subdir with space in path to check quoting
    let spaced = tmp.path().join("dir with space");
    std::fs::create_dir_all(&spaced).unwrap();
    let pass1 = spaced.join("p1.jsonl");
    // note: pass1 does not need to exist for dry-run
    // set final to compressed name to verify default manifest naming
    let final_out = spaced.join("final.jsonl.gz");

    let mut cmd = Command::cargo_bin("orchestrate_ambiguous").expect("binary exists");
    let assert = cmd
        .args([
            "--pass1",
            pass1.to_string_lossy().as_ref(),
            "--final",
            final_out.to_string_lossy().as_ref(),
            "--dry-run",
            "--analyze-summary",
            "--merge-mode",
            "depth-first",
        ])
        .assert();

    // Expect extract, normalize, generate, merge, analyze plan lines
    let outp = assert.get_output().stdout.clone();
    let s = String::from_utf8_lossy(&outp);
    assert!(s.contains("[dry-run]"));
    assert!(s.contains("extract_flagged_positions"));
    assert!(s.contains("normalize+unique"));
    assert!(s.contains("generate_nnue_training_data"));
    assert!(s.contains("merge_annotation_results"));
    assert!(s.contains("analyze_teaching_quality"));
    // expect quotes around spaced path in generate/merge lines
    assert!(s.contains("\""));
    // expect default final manifest is final.manifest.json
    assert!(s.contains("--manifest-out") && s.contains("final.manifest.json"));
}
