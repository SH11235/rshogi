use assert_cmd::prelude::*;
use predicates::prelude::*;
use std::process::Command;
use tempfile::TempDir;

#[test]
fn orchestrate_ambiguous_dry_run_prints_full_plan() {
    let tmp = TempDir::new().unwrap();
    let pass1 = tmp.path().join("p1.jsonl");
    // note: pass1 does not need to exist for dry-run
    let final_out = tmp.path().join("final.jsonl");

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
    assert
        .success()
        .stdout(predicate::str::contains("[dry-run]")
            .and(predicate::str::contains("extract_flagged_positions"))
            .and(predicate::str::contains("normalize+unique"))
            .and(predicate::str::contains("generate_nnue_training_data"))
            .and(predicate::str::contains("merge_annotation_results"))
            .and(predicate::str::contains("analyze_teaching_quality"))
        );
}

