use std::io::Write;
use std::process::Command;

/// テスト用の共通USI初期化コマンド（Material評価で動作させる）
const USI_INIT: &str = "usi\nsetoption name MaterialLevel value 9\nisready\n";

/// `go`→`stop`→`quit` で bestmove が返って終了することを確認
#[test]
fn stop_then_quit_outputs_bestmove() {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("rshogi-usi"));
    let mut child = cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn engine");

    {
        let stdin = child.stdin.as_mut().expect("stdin");
        write!(stdin, "{USI_INIT}position startpos\ngo depth 1\nstop\nquit\n").expect("write");
    }

    let output = child.wait_with_output().expect("wait output");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("bestmove"), "stdout:\n{stdout}");
    assert!(output.status.success());
}

/// `go`→`gameover`→`quit` で探索を停止しつつ bestmove を返すこと
#[test]
fn gameover_outputs_bestmove() {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("rshogi-usi"));
    let mut child = cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn engine");

    {
        let stdin = child.stdin.as_mut().expect("stdin");
        write!(stdin, "{USI_INIT}position startpos\ngo depth 1\ngameover lose\nquit\n")
            .expect("write");
    }

    let output = child.wait_with_output().expect("wait output");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("bestmove"), "stdout:\n{stdout}");
    assert!(output.status.success());
}

/// `go`→即`quit` でも bestmove が返って終了すること
#[test]
fn quit_outputs_bestmove() {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("rshogi-usi"));
    let mut child = cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn engine");

    {
        let stdin = child.stdin.as_mut().expect("stdin");
        write!(stdin, "{USI_INIT}position startpos\ngo depth 1\nquit\n").expect("write");
    }

    let output = child.wait_with_output().expect("wait output");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("bestmove"), "stdout:\n{stdout}");
    assert!(output.status.success());
}

/// `go ponder`→`ponderhit`→`quit` で bestmove が返ること
#[test]
fn ponderhit_outputs_bestmove() {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("rshogi-usi"));
    let mut child = cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn engine");

    {
        let stdin = child.stdin.as_mut().expect("stdin");
        write!(stdin, "{USI_INIT}position startpos\ngo ponder depth 2\nponderhit\nquit\n")
            .expect("write");
    }

    let output = child.wait_with_output().expect("wait output");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("bestmove"), "stdout:\n{stdout}");
    assert!(output.status.success());
}

/// `Stochastic_Ponder` 有効時の `ponderhit` で通常探索へ切り替わって bestmove が返ること
#[test]
fn stochastic_ponderhit_restarts_search() {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("rshogi-usi"));
    let mut child = cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn engine");

    {
        let stdin = child.stdin.as_mut().expect("stdin");
        write!(
            stdin,
            "{USI_INIT}setoption name Stochastic_Ponder value true\nposition startpos\ngo ponder depth 2\nponderhit\nquit\n"
        )
        .expect("write");
    }

    let output = child.wait_with_output().expect("wait output");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("bestmove"), "stdout:\n{stdout}");
    assert!(output.status.success());
}
