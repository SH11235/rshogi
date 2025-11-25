use assert_cmd::Command;

/// 27点法（CSA ルール）での入玉宣言勝ちが有効な局面。
///
/// - 先手手番。
/// - 先手玉: 5c（敵陣三段目以内）
/// - 敵陣内の先手駒: 歩 10 枚 + 玉 1 枚 = 11 枚以上。
/// - 手駒: 先手の飛車 2 枚 + 角 2 枚を持たせて持点を十分に確保。
///
/// SFEN（盤面）:
///   a段: PPPP1PPPP
///   b段: 7PP
///   c段: 4K4
///   d〜h段: 9
///   i段: 4k4
///
/// → "PPPP1PPPP/7PP/4K4/9/9/9/9/9/4k4 b 2R2B 1"
#[test]
fn declaration_win_csa27_emits_bestmove_win() {
    let mut cmd = Command::cargo_bin("engine-usi").expect("binary available");
    let script = r#"usi
isready
usinewgame
setoption name EngineType value Material
setoption name EnteringKingRule value CSARule27
isready
position sfen PPPP1PPPP/7PP/4K4/9/9/9/9/9/4k4 b 2R2B 1
go depth 1
quit
"#;

    let output = cmd.write_stdin(script).assert().success().get_output().stdout.clone();
    let text = String::from_utf8_lossy(&output);

    assert!(
        text.contains("bestmove win"),
        "expected bestmove win for CSA27 declaration win position, got:\n{text}"
    );
}

/// TRy ルールでのテスト。
///
/// - 先手手番。
/// - 先手玉: 5b
/// - 後手玉: 5c
/// - 5a は空き升で、敵の利きも無い。
///
/// → Position::declaration_win_move(TryRule) は Move::win() を返し、
///    エンジンは `bestmove win` を自動的に返す。
#[test]
fn try_rule_emits_bestmove_win() {
    let mut cmd = Command::cargo_bin("engine-usi").expect("binary available");
    let script = r#"usi
isready
usinewgame
setoption name EngineType value Material
setoption name EnteringKingRule value TryRule
isready
position sfen 9/4K4/4k4/9/9/9/9/9/9 b - 1
go depth 1
quit
"#;

    let output = cmd.write_stdin(script).assert().success().get_output().stdout.clone();
    let text = String::from_utf8_lossy(&output);

    assert!(
        text.contains("bestmove win"),
        "expected bestmove win for TRy rule position, got:\n{text}"
    );
}
