use assert_cmd::prelude::*;
use predicates::prelude::*;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::Command;

// Validate a JSON object minimally against docs/schemas/structured_v1.schema.json
fn validate_structured_line(v: &serde_json::Value) -> bool {
    // required
    for key in ["ts", "phase", "global_step", "epoch"] {
        if v.get(key).is_none() {
            return false;
        }
    }
    // ts
    if !v.get("ts").unwrap().is_string() {
        return false;
    }
    // phase
    if let Some(p) = v.get("phase").and_then(|x| x.as_str()) {
        if p != "train" && p != "val" && p != "gauntlet" {
            return false;
        }
    } else {
        return false;
    }
    // global_step / epoch
    if v.get("global_step").and_then(|x| x.as_i64()).is_none() {
        return false;
    }
    if v.get("epoch").and_then(|x| x.as_i64()).is_none() {
        return false;
    }
    // optional numerics if present
    for k in [
        "lr",
        "train_loss",
        "val_loss",
        "val_auc",
        "examples_sec",
        "loader_ratio",
        "wall_time",
    ] {
        if let Some(x) = v.get(k) {
            if !x.is_number() {
                return false;
            }
        }
    }
    // loader_ratio range if present
    if let Some(x) = v.get("loader_ratio").and_then(|x| x.as_f64()) {
        if !(0.0..=1.0).contains(&x) {
            return false;
        }
    }
    true
}

#[test]
fn cli_conflicts_decay_args_exits_2() {
    // Conflicting flags: --lr-decay-epochs and --lr-decay-steps
    let mut cmd = Command::cargo_bin("train_nnue").unwrap();
    let assert = cmd
        .arg("-i")
        .arg("nonexistent.jsonl")
        .arg("-e")
        .arg("1")
        .arg("-b")
        .arg("2")
        .arg("--lr")
        .arg("0.001")
        .arg("--lr-schedule")
        .arg("step")
        .arg("--lr-decay-epochs")
        .arg("2")
        .arg("--lr-decay-steps")
        .arg("100")
        .assert();
    // Clap should exit with code 2 on conflicts
    assert.failure().code(predicate::eq(2));
}

#[test]
fn structured_schema_fixture_valid() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/reports/fixtures/jsonl_sample.jsonl");
    let f = File::open(path).expect("fixture exists");
    let r = BufReader::new(f);
    for (i, line) in r.lines().enumerate() {
        let line = line.expect("line");
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value =
            serde_json::from_str(&line).expect(&format!("valid json at {}", i));
        assert!(
            validate_structured_line(&v),
            "schema-like validation failed at line {}: {}",
            i,
            line
        );
    }
}
