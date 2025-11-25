use assert_cmd::prelude::*;
use predicates::prelude::*;
use std::process::Command;

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
fn cli_unknown_schedule_exits_2() {
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
        .arg("foo")
        .assert();
    assert.failure().code(predicate::eq(2));
}
