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

/// bestmove を受信するまで quit/stop を送らず、通知だけで探索を終了できることを確認する。
fn assert_ponderhit_returns_before_quit(stochastic: bool) {
    use std::io::{BufRead, BufReader};
    use std::sync::mpsc;
    use std::time::Duration;

    let mut child = Command::new(assert_cmd::cargo::cargo_bin!("rshogi-usi"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn engine");
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let line = line.expect("read engine output");
            if line.starts_with("bestmove ") {
                tx.send(line).ok();
            }
        }
    });
    let stdin = child.stdin.as_mut().unwrap();
    write!(stdin, "{USI_INIT}setoption name Stochastic_Ponder value {stochastic}\nposition startpos\ngo ponder depth 2\nponderhit\n").unwrap();
    stdin.flush().unwrap();
    let bestmove = rx.recv_timeout(Duration::from_secs(10));
    if bestmove.is_err() {
        child.kill().expect("terminate unresponsive engine");
    } else {
        writeln!(child.stdin.as_mut().unwrap(), "quit").unwrap();
    }
    let status = child.wait().unwrap();
    reader.join().unwrap();
    let bestmove = bestmove.expect("ponderhit must return bestmove before quit/stop");
    assert!(!bestmove.starts_with("bestmove resign"), "{bestmove}");
    assert!(status.success());
}

/// 通常 ponder は共有通知の消費で bestmove を返す。
#[test]
fn ponderhit_outputs_bestmove() {
    assert_ponderhit_returns_before_quit(false);
}

/// Stochastic_Ponder は先読みを停止し、通常探索へ切り替えて bestmove を返す。
#[test]
fn stochastic_ponderhit_restarts_search() {
    assert_ponderhit_returns_before_quit(true);
}
