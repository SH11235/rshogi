use assert_cmd::prelude::*;
use jsonschema::{Draft, JSONSchema};
use predicates::prelude::*;
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    // Current file: crates/tools/tests/gauntlet_cli.rs
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

#[test]
fn test_gauntlet_stub_json_schema_and_gate() {
    // Prepare temp outputs
    let tmp = tempfile::tempdir().unwrap();
    let json_path = tmp.path().join("out.json");
    let report_path = tmp.path().join("report.md");

    // Schema path
    let schema_path = repo_root().join("docs/schemas/gauntlet_out.schema.json");
    assert!(schema_path.exists(), "schema not found: {}", schema_path.display());

    // Book path (representative sample)
    let book_path = repo_root().join("docs/reports/fixtures/opening/representative.epd");
    assert!(book_path.exists(), "book not found: {}", book_path.display());

    // Run gauntlet with stub
    let mut cmd = Command::cargo_bin("gauntlet").unwrap();
    cmd.args([
        "--base",
        "baseline.nn",
        "--cand",
        "candidate.nn",
        "--time",
        "0/1+0.1",
        "--games",
        "20",
        "--threads",
        "1",
        "--hash-mb",
        "256",
        "--book",
        book_path.to_str().unwrap(),
        "--multipv",
        "1",
        "--json",
        json_path.to_str().unwrap(),
        "--report",
        report_path.to_str().unwrap(),
        "--stub",
    ]);
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("\"phase\":\"gauntlet\""));

    // Read and validate JSON against schema
    let data: Value = serde_json::from_slice(&fs::read(&json_path).unwrap()).unwrap();
    let mut schema: Value = serde_json::from_slice(&fs::read(schema_path).unwrap()).unwrap();
    // Work around relative $id by injecting an absolute base URI for validation context
    if let Value::Object(ref mut m) = schema {
        if let Some(Value::String(id)) = m.get_mut("$id") {
            if !id.starts_with("http://") && !id.starts_with("https://") {
                *id = "https://example.com/gauntlet_out.schema.json".to_string();
            }
        }
    }
    let compiled = JSONSchema::options().with_draft(Draft::Draft7).compile(&schema).unwrap();
    if let Err(errors) = compiled.validate(&data) {
        for e in errors {
            eprintln!("schema error: {}", e);
        }
        panic!("JSON does not match schema");
    }

    // Check gate decision and metrics
    let summary = &data["summary"];
    let gate = summary["gate"].as_str().unwrap();
    assert_eq!(gate, "pass", "stub should pass gate (winrate>=55% and |nps|<=3%)");
    let pv_p90 = summary["pv_spread_p90_cp"].as_f64().unwrap();
    assert!((pv_p90 - 25.0).abs() < 1e-6);
}

#[test]
fn test_gauntlet_stdout_destinations() {
    // Prepare temp outputs
    let tmp = tempfile::tempdir().unwrap();
    let report_path = tmp.path().join("report.md");

    // Book path (representative sample)
    let book_path = repo_root().join("docs/reports/fixtures/opening/representative.epd");
    assert!(book_path.exists(), "book not found: {}", book_path.display());

    // Run gauntlet with JSON to stdout
    let mut cmd = Command::cargo_bin("gauntlet").unwrap();
    cmd.args([
        "--base",
        "baseline.nn",
        "--cand",
        "candidate.nn",
        "--time",
        "0/1+0.1",
        "--games",
        "20",
        "--threads",
        "1",
        "--hash-mb",
        "256",
        "--book",
        book_path.to_str().unwrap(),
        "--multipv",
        "1",
        "--json",
        "-",
        "--report",
        report_path.to_str().unwrap(),
        "--stub",
    ]);
    cmd.assert()
        .success()
        // JSON should go to stdout
        .stdout(predicate::str::contains("\"env\":"))
        // structured_v1 should be on stderr when stdout is used by JSON
        .stderr(predicate::str::contains("\"phase\":\"gauntlet\""));
}
