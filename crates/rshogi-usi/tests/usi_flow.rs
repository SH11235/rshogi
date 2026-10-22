use std::io::Write;
use std::process::Command;

/// テスト用の共通USI初期化コマンド（Material評価で動作させる）
const USI_INIT: &str = "usi\nsetoption name MaterialLevel value 9\nisready\n";

#[test]
fn position_with_existing_enemy_check_rejects_duplicate_kings_atomically() {
    let mut command = assert_cmd::Command::new(assert_cmd::cargo::cargo_bin!("rshogi-usi"));
    let output = command
        .timeout(std::time::Duration::from_secs(15))
        .write_stdin(
            "position sfen 4k4/9/9/9/4R4/9/9/9/4K4 b G 1 moves G*1h\nd\n\
             position sfen 4K4/r8/9/9/9/9/9/9/4K4 b - 1 moves 5a5b\nd\n\
             position sfen 4k4/R8/9/9/9/9/9/9/4k4 w - 1 moves 5a5b\nd\n\
             position sfen 4k4/9/9/9/4R4/9/9/9/4K4 b G 1 moves G*1h 5a4a 5e4e\nd\n\
             quit\n",
        )
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    let positions: Vec<_> = stdout.lines().filter_map(|line| line.strip_prefix("SFEN: ")).collect();
    let checked = "4k4/9/9/9/4R4/9/9/8G/4K4 w - 2";
    assert_eq!(
        positions,
        [checked, checked, checked, "5k3/9/9/9/5R3/9/9/8G/4K4 w - 4"],
        "{stdout}"
    );
    assert_eq!(
        stderr.lines().filter(|line| line.contains("at most one king per side")).count(),
        2,
        "{stderr}"
    );
}

#[test]
fn invalid_position_preserves_previous_position_and_search() {
    use std::io::{BufRead, BufReader, Read};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let invalid_commands = [
        "position sfen invalid b - 1",
        "position startpos moves 7g7e",
        "position startpos moves 5e5d",
        "position startpos moves 3c3d",
        "position startpos moves 7g7f+",
        "position startpos moves 6i6h+",
        "position startpos moves P*5e",
        "position startpos moves 7g7f 3c3d 5e5d",
        "position startpos moves 7g7f 3c3d invalid",
        "position startpos moves 7g7fextra",
        "position startpos moves 7g7fあ",
        "position sfen 3pk3R/5g3/4G4/9/9/9/9/9/4K4 b P 1 moves P*5b",
    ];
    let expected = "lnsgkgsnl/1r5b1/pppppp1pp/6p2/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL b - 3";
    let mut input = String::from(
        "usi\nsetoption name MaterialLevel value 9\nsetoption name USI_Hash value 1\nsetoption name EvalHash value 0\nisready\nposition startpos moves 7g7f 3c3d\nd\n",
    );
    for command in invalid_commands {
        input.push_str(command);
        input.push_str("\nd\n");
    }
    input.push_str("go depth 1\n");

    let mut child = Command::new(assert_cmd::cargo::cargo_bin!("rshogi-usi"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn engine");
    let (sender, receiver) = mpsc::channel();
    let stdout = child.stdout.take().unwrap();
    let stdout_reader = std::thread::spawn(move || -> std::io::Result<String> {
        let mut output = String::new();
        for line in BufReader::new(stdout).lines() {
            let line = line?;
            output.push_str(&line);
            output.push('\n');
            if line.starts_with("bestmove ") && sender.send(()).is_err() {
                break;
            }
        }
        Ok(output)
    });
    let mut stderr = child.stderr.take().unwrap();
    let stderr_reader = std::thread::spawn(move || -> std::io::Result<String> {
        let mut output = String::new();
        stderr.read_to_string(&mut output)?;
        Ok(output)
    });
    let result = (|| -> Result<(), String> {
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(input.as_bytes())
            .map_err(|err| err.to_string())?;
        receiver.recv_timeout(Duration::from_secs(15)).map_err(|err| err.to_string())?;
        writeln!(child.stdin.as_mut().unwrap(), "quit").map_err(|err| err.to_string())?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while child.try_wait().map_err(|err| err.to_string())?.is_none() {
            if Instant::now() >= deadline {
                return Err("engine did not exit after quit".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    })();
    if result.is_err()
        && let Err(err) = child.kill()
    {
        eprintln!("failed to terminate engine: {err}");
    }
    let status = child.wait().expect("wait engine");
    let stdout = stdout_reader.join().expect("stdout reader").expect("read stdout");
    let stderr = stderr_reader.join().expect("stderr reader").expect("read stderr");
    assert!(result.is_ok(), "{result:?}\n{stdout}\n{stderr}");
    assert!(status.success(), "{status}\n{stdout}\n{stderr}");
    assert!(stdout.lines().any(|line| line.starts_with("info depth 1 ")), "{stdout}");
    let positions: Vec<_> = stdout.lines().filter_map(|line| line.strip_prefix("SFEN: ")).collect();
    assert_eq!(positions, vec![expected; invalid_commands.len() + 1], "{stdout}");
    assert_eq!(
        stderr
            .lines()
            .filter(|line| line.starts_with("info string Error parsing position:"))
            .count(),
        invalid_commands.len(),
        "{stderr}"
    );
    let bestmove = stdout
        .lines()
        .find_map(|line| line.strip_prefix("bestmove "))
        .expect("bestmove");
    let mv = rshogi_core::types::Move::from_usi_strict(bestmove.split_whitespace().next().unwrap())
        .expect("a move, not resign");
    let mut position = rshogi_core::position::Position::new();
    position.set_sfen(expected).unwrap();
    assert!(position.pseudo_legal(mv) && position.is_legal(mv), "{bestmove}");
}

/// 探索中の isready は infinite / ponder を停止せず readyok を返す。
#[test]
fn isready_during_search_keeps_search_running() {
    use std::io::{BufRead, BufReader};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    for threads in [1, 2] {
        for mode in ["infinite", "ponder depth 1"] {
            let mut child = Command::new(assert_cmd::cargo::cargo_bin!("rshogi-usi"))
                .env_remove("RSHOGI_DISABLE_HELPER_SEARCH")
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
            let result = (|| -> Result<(), String> {
                let wait_for = |predicate: fn(&str) -> bool, reject_bestmove, timeout| {
                    let deadline = Instant::now() + timeout;
                    loop {
                        let line = receiver
                            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                            .map_err(|err| format!("waiting for engine output: {err}"))?;
                        if reject_bestmove && line.starts_with("bestmove ") {
                            return Err(format!("search ended before stop: {line}"));
                        }
                        if predicate(&line) {
                            return Ok(());
                        }
                    }
                };
                writeln!(
                    child.stdin.as_mut().unwrap(),
                    "usi\nsetoption name MaterialLevel value 9\nsetoption name Threads value {threads}\nsetoption name USI_Hash value 1\nsetoption name EvalHash value 1\nisready"
                )
                .map_err(|err| err.to_string())?;
                wait_for(|line| line == "readyok", false, Duration::from_secs(10))?;
                writeln!(child.stdin.as_mut().unwrap(), "position startpos\ngo {mode}")
                    .map_err(|err| err.to_string())?;
                wait_for(|line| line.starts_with("info depth "), true, Duration::from_secs(10))?;
                // 応答後も再度問い合わせでき、stop まで bestmove を保留する。
                for _ in 0..2 {
                    writeln!(child.stdin.as_mut().unwrap(), "isready")
                        .map_err(|err| err.to_string())?;
                    wait_for(|line| line == "readyok", true, Duration::from_secs(3))?;
                }
                let deadline = Instant::now() + Duration::from_millis(200);
                loop {
                    match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    {
                        Ok(line) if line.starts_with("bestmove ") => {
                            return Err(format!("isready stopped the search: {line}"));
                        }
                        Ok(_) => {}
                        Err(mpsc::RecvTimeoutError::Timeout) => break,
                        Err(err) => return Err(err.to_string()),
                    }
                }
                writeln!(child.stdin.as_mut().unwrap(), "stop").map_err(|err| err.to_string())?;
                wait_for(|line| line.starts_with("bestmove "), false, Duration::from_secs(3))?;
                writeln!(child.stdin.as_mut().unwrap(), "quit").map_err(|err| err.to_string())?;
                let deadline = Instant::now() + Duration::from_secs(3);
                loop {
                    if let Some(status) = child.try_wait().map_err(|err| err.to_string())? {
                        return if status.success() {
                            Ok(())
                        } else {
                            Err(format!("engine failed: {status}"))
                        };
                    }
                    if Instant::now() >= deadline {
                        return Err("engine did not quit after stop".into());
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            })();
            if result.is_err() {
                let _ = child.kill();
            }
            child.wait().unwrap();
            reader.join().unwrap();
            result.unwrap_or_else(|err| panic!("Threads={threads}, go {mode}: {err}"));
        }
    }
}

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
#[cfg(feature = "search-pass-rules")]
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
        for (threads, hit) in [(1, true), (1, false), (4, true), (4, false)] {
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
            write!(child.stdin.as_mut().unwrap(), "{USI_INIT}setoption name Threads value {threads}\nsetoption name Stochastic_Ponder value false\nsetoption name MinimumThinkingTime value 1000\nsetoption name NetworkDelay value 0\nsetoption name NetworkDelay2 value 0\nposition startpos\ngo ponder {budget}\n").unwrap();
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
                // main初期化後に、movetime/rtime上限より長く待つ（初期化時の通知競合と分離）。
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
            let elapsed =
                result.unwrap_or_else(|err| panic!("{budget} threads={threads} hit={hit}: {err}"));
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
