//! `--sprt-nelo0 -10` のように負値をスペース区切りで渡しても clap が
//! `unexpected argument '-1'` として拒否しないことを確認する回帰テスト。
//! 引数解析を通過した後の失敗（存在しないエンジン / 入力ファイル）は許容する。

use std::process::Command;

fn assert_not_clap_usage_error(output: &std::process::Output, context: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("unexpected argument"),
        "{context}: clap rejected negative nelo value: {stderr}"
    );
    assert!(!stderr.contains("Usage:"), "{context}: clap usage error emitted: {stderr}");
}

#[test]
fn tournament_accepts_negative_nelo_bounds() {
    let dir = tempfile::tempdir().unwrap();
    let missing_engine = dir.path().join("no-such-engine");
    let output = Command::new(env!("CARGO_BIN_EXE_tournament"))
        .args([
            "--engine",
            missing_engine.to_str().unwrap(),
            "--engine-label",
            "a",
            "--engine",
            missing_engine.to_str().unwrap(),
            "--engine-label",
            "b",
            "--sprt",
            "--sprt-base-label",
            "a",
            "--sprt-test-label",
            "b",
            "--sprt-nelo0",
            "-10",
            "--sprt-nelo1",
            "0",
            "--games",
            "1",
            "--out-dir",
        ])
        .arg(dir.path().join("out"))
        .output()
        .unwrap();
    assert_not_clap_usage_error(&output, "tournament");
}

#[test]
fn analyze_selfplay_accepts_negative_nelo_bounds() {
    let dir = tempfile::tempdir().unwrap();
    let missing_log = dir.path().join("no-such-log.jsonl");
    let output = Command::new(env!("CARGO_BIN_EXE_analyze_selfplay"))
        .arg(&missing_log)
        .args([
            "--sprt",
            "--sprt-base-label",
            "base",
            "--sprt-test-label",
            "test",
            "--sprt-nelo0",
            "-10",
            "--sprt-nelo1",
            "0",
        ])
        .output()
        .unwrap();
    assert_not_clap_usage_error(&output, "analyze_selfplay");
}
