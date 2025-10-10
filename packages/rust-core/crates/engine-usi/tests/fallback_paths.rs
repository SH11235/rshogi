use assert_cmd::Command;

// 強制panic経路: go直前でpanicを発生させ、go_panic_caughtとfallback_bestmove_emitを確認
#[test]
fn go_forced_panic_triggers_fallback_bestmove() {
    let mut cmd = Command::cargo_bin("engine-usi").expect("binary available");
    cmd.env("USI_TEST_GO_PANIC", "1");
    let script = r#"usi
isready
position startpos
go depth 1
quit
"#;

    let output = cmd.write_stdin(script).assert().success().get_output().stdout.clone();
    let text = String::from_utf8_lossy(&output);

    assert!(text.contains("go_panic_caught=1"), "expected panic catch log, got: {}", text);
    assert!(
        text.contains("fallback_bestmove_emit=1 reason=go_panic"),
        "expected fallback emission log, got: {}",
        text
    );
    assert!(text.contains("bestmove "), "bestmove must be emitted: {}", text);
}

// Poison経路: エンジンMutexをPoisonさせてからgo。panicは捕捉され、フォールバックbestmoveが出ることを確認。
#[test]
fn engine_mutex_poison_then_go_emits_bestmove() {
    let mut cmd = Command::cargo_bin("engine-usi").expect("binary available");
    let script = r#"usi
isready
position startpos
debug_poison_engine
go depth 1
quit
"#;

    let output = cmd.write_stdin(script).assert().success().get_output().stdout.clone();
    let text = String::from_utf8_lossy(&output);

    // goはpanicするがcatch_unwindで吸収され、fallbackが出る
    assert!(
        text.contains("go_panic_caught=1"),
        "expected panic-caught after poison: {}",
        text
    );
    assert!(text.contains("bestmove "), "bestmove must be emitted: {}", text);
}
