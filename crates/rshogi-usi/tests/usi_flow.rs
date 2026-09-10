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

/// パス探索を有効にしたビルドで、searchmovesの制限が実際のbestmoveに届く。
#[cfg(not(feature = "search-no-pass-rules"))]
#[test]
fn searchmoves_pass_returns_pass_without_stop() {
    use std::io::{BufRead, BufReader};
    use std::sync::mpsc;
    use std::time::Duration;

    let mut child = Command::new(assert_cmd::cargo::cargo_bin!("rshogi-usi"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn engine");
    let stdout = child.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let line = line.expect("read stdout");
            if line.starts_with("bestmove ") {
                let _ = sender.send(line);
                break;
            }
        }
    });
    write!(child.stdin.as_mut().unwrap(), "{USI_INIT}setoption name PassRights value true\nsetoption name InitialPassCount value 1\nposition startpos\ngo depth 1 searchmoves pass\n").unwrap();
    let result = receiver.recv_timeout(Duration::from_secs(15));
    if result.is_ok() {
        writeln!(child.stdin.as_mut().unwrap(), "quit").unwrap();
    } else {
        let _ = child.kill();
    }
    let status = child.wait().unwrap();
    reader.join().unwrap();
    let bestmove = result.expect("bestmove should arrive before stop/quit");
    assert_eq!(bestmove.split_whitespace().nth(1), Some("pass"));
    assert!(status.success());
}
