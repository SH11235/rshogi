use assert_cmd::Command;

// 非詰（適当局面）では checkmate nomate を返す
#[test]
fn go_mate_nomate_on_non_mate_position() {
    let mut cmd = Command::cargo_bin("engine-usi").expect("binary available");
    let script = r#"usi
isready
usinewgame
position startpos
go mate 100
quit
"#;

    let output = cmd.write_stdin(script).assert().success().get_output().stdout.clone();
    let text = String::from_utf8_lossy(&output);
    assert!(text.contains("checkmate nomate"), "expected nomate: {}", text);
    assert!(!text.contains("bestmove"), "must not emit bestmove in mate mode: {}", text);
}
