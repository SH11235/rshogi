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

/// 通常ponderの固定期限・hit後予算・明示stopを、初期化後のinfoを同期点にして検証する。
#[test]
fn ponder_time_budgets_wait_for_hit_or_stop() {
    use std::io::{BufRead, BufReader};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    for (budget, fixed) in [
        ("movetime 300", true),
        ("rtime 300", true),
        ("btime 10000 wtime 10000 byoyomi 100", false),
    ] {
        for hit in [true, false] {
            let mut child = Command::new(assert_cmd::cargo::cargo_bin!("rshogi-usi"))
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .expect("spawn engine");
            let stdout = child.stdout.take().unwrap();
            let (sender, receiver) = mpsc::channel();
            let reader = std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    if sender.send(line.expect("read stdout")).is_err() {
                        break;
                    }
                }
            });
            write!(child.stdin.as_mut().unwrap(), "{USI_INIT}setoption name Threads value 1\nsetoption name Stochastic_Ponder value false\nsetoption name MinimumThinkingTime value 1000\nsetoption name NetworkDelay value 0\nsetoption name NetworkDelay2 value 0\nposition startpos\ngo ponder {budget}\n").unwrap();
            let result = (|| -> Result<Duration, String> {
                let deadline = Instant::now() + Duration::from_secs(10);
                loop {
                    let line = receiver
                        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                        .map_err(|err| err.to_string())?;
                    if line.starts_with("bestmove ") {
                        return Err(format!("early {line}"));
                    }
                    if line.starts_with("info depth ") {
                        break;
                    }
                }
                // main初期化後に、movetime/rtime上限より長く待つ（F01と分離）。
                let deadline = Instant::now() + Duration::from_millis(650);
                loop {
                    match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    {
                        Ok(line) if line.starts_with("bestmove ") => {
                            return Err(format!("before signal: {line}"));
                        }
                        Ok(_) => {}
                        Err(mpsc::RecvTimeoutError::Timeout) => break,
                        Err(err) => return Err(err.to_string()),
                    }
                }
                let start = Instant::now();
                writeln!(
                    child.stdin.as_mut().unwrap(),
                    "{}",
                    if hit { "ponderhit" } else { "stop" }
                )
                .map_err(|err| err.to_string())?;
                let deadline = start + Duration::from_secs(4);
                loop {
                    let line = receiver
                        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                        .map_err(|err| err.to_string())?;
                    if line.starts_with("bestmove ") {
                        return Ok(start.elapsed());
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
            let elapsed = result.unwrap_or_else(|err| panic!("{budget} hit={hit}: {err}"));
            eprintln!("ponder budget={budget} hit={hit} response_ms={}", elapsed.as_millis());
            assert!(status.success());
            if hit && fixed {
                assert!(
                    elapsed >= Duration::from_millis(200),
                    "hit後の固定予算を使う: {budget} {elapsed:?}"
                );
            }
            if !hit {
                assert!(elapsed < Duration::from_secs(2), "stopは予算を待たない: {elapsed:?}");
            }
        }
    }
}
