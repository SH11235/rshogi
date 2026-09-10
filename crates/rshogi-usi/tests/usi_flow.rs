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

/// 終局でもponder/infiniteは明示stopまでbestmoveを保留する。
#[test]
fn terminal_positions_obey_usi_output_wait() {
    use std::io::{BufRead, BufReader};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};
    for (sfen, expected) in [
        ("4k4/9/9/9/9/9/4r4/4g4/4K4 b - 1", "resign"),
        ("KGG6/SS7/PPPPPP3/9/9/9/2pppppp1/1ss1gg1nl/4k2nl b 2R2B3p 1", "win"),
    ] {
        for mode in ["depth 1", "ponder depth 1", "infinite"] {
            let mut child = Command::new(assert_cmd::cargo::cargo_bin!("rshogi-usi"))
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .expect("spawn engine");
            let (sender, receiver) = mpsc::channel();
            let stdout = child.stdout.take().unwrap();
            let reader = std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    if sender.send(line.expect("read stdout")).is_err() {
                        break;
                    }
                }
            });
            write!(child.stdin.as_mut().unwrap(), "{USI_INIT}").unwrap();
            let result = (|| -> Result<String, String> {
                let deadline = Instant::now() + Duration::from_secs(10);
                loop {
                    let line = receiver
                        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                        .map_err(|err| err.to_string())?;
                    if line == "readyok" {
                        break;
                    }
                }
                writeln!(child.stdin.as_mut().unwrap(), "position sfen {sfen}\ngo {mode}")
                    .map_err(|err| err.to_string())?;
                if mode != "depth 1" {
                    let deadline = Instant::now() + Duration::from_millis(300);
                    loop {
                        match receiver
                            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                        {
                            Ok(line) if line.starts_with("bestmove ") => {
                                return Err(format!("early {line}"));
                            }
                            Ok(_) => {}
                            Err(mpsc::RecvTimeoutError::Timeout) => break,
                            Err(err) => return Err(err.to_string()),
                        }
                    }
                    // stopはinitで消されないためF01のponderhit競合を混ぜない。
                    writeln!(child.stdin.as_mut().unwrap(), "stop")
                        .map_err(|err| err.to_string())?;
                }
                let deadline = Instant::now() + Duration::from_secs(3);
                loop {
                    let line = receiver
                        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                        .map_err(|err| err.to_string())?;
                    if line.starts_with("bestmove ") {
                        return Ok(line);
                    }
                }
            })();
            if result.is_ok() {
                writeln!(child.stdin.as_mut().unwrap(), "quit").unwrap();
            } else {
                let _ = child.kill();
            }
            let status = child.wait().unwrap();
            reader.join().unwrap();
            let bestmove = result.unwrap_or_else(|err| panic!("{expected} {mode}: {err}"));
            assert_eq!(bestmove.split_whitespace().nth(1), Some(expected));
            assert!(status.success());
        }
    }
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
