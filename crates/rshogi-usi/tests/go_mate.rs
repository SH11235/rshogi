//! `go mate` (df-pn 詰み探索) の USI 応答

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

/// 将棋図巧 第 1 番 (69 手詰め)。短い制限時間では解けない局面として使う。
const ZUKOU_001: &str = "1pG1B4/Gs+P6/pP7/n1ls5/3k5/nL4+r1b/1+p1p+R4/1S7/2N5K b SP2gn2l11p 1";

struct Engine {
    child: Child,
    lines: Receiver<String>,
}

impl Engine {
    fn start() -> Self {
        let mut child = Command::new(assert_cmd::cargo::cargo_bin!("rshogi-usi"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn engine");
        let stdout = child.stdout.take().unwrap();
        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let mut engine = Self { child, lines };
        engine.send("usi\nsetoption name MaterialLevel value 9\nsetoption name MateHashMB value 16\nisready");
        engine.wait_for(|line| line == "readyok", Duration::from_secs(10));
        engine
    }

    fn send(&mut self, commands: &str) {
        writeln!(self.child.stdin.as_mut().unwrap(), "{commands}").expect("write to engine");
    }

    /// `predicate` を満たす行を待って返す
    fn wait_for(&mut self, predicate: impl Fn(&str) -> bool, timeout: Duration) -> String {
        let deadline = Instant::now() + timeout;
        loop {
            let line = self
                .lines
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("engine output timed out");
            if predicate(&line) {
                return line;
            }
        }
    }

    fn checkmate(&mut self, timeout: Duration) -> String {
        self.wait_for(|line| line.starts_with("checkmate"), timeout)
    }

    /// 応答の後に余分な `checkmate` 行が出ないことを確かめる
    fn assert_no_more_checkmate(&mut self) {
        self.send("isready");
        let line = self.wait_for(
            |line| line == "readyok" || line.starts_with("checkmate"),
            Duration::from_secs(5),
        );
        assert_eq!(line, "readyok", "checkmate が 2 回出力された");
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = writeln!(self.child.stdin.as_mut().unwrap(), "quit");
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
    }
}

#[test]
fn go_mate_returns_mate_sequence() {
    let mut engine = Engine::start();
    engine.send("position sfen 4k4/9/4P4/9/9/9/9/9/4K4 b G 1\ngo mate 5000");
    assert_eq!(engine.checkmate(Duration::from_secs(10)), "checkmate G*5b");
    engine.assert_no_more_checkmate();
}

#[test]
fn go_mate_returns_nomate() {
    let mut engine = Engine::start();
    engine.send("position sfen 4k4/9/9/9/9/9/9/9/4K4 b - 1\ngo mate infinite");
    assert_eq!(engine.checkmate(Duration::from_secs(10)), "checkmate nomate");
    engine.assert_no_more_checkmate();
}

#[test]
fn go_mate_times_out_after_the_limit() {
    let mut engine = Engine::start();
    let start = Instant::now();
    engine.send(&format!("position sfen {ZUKOU_001}\ngo mate 1000"));
    assert_eq!(engine.checkmate(Duration::from_secs(10)), "checkmate timeout");
    let elapsed = start.elapsed();
    assert!(
        elapsed >= Duration::from_millis(900) && elapsed < Duration::from_millis(3000),
        "go mate 1000 の応答まで {elapsed:?}"
    );
    engine.assert_no_more_checkmate();
}

#[test]
fn stop_ends_go_mate_with_timeout() {
    let mut engine = Engine::start();
    engine.send(&format!("position sfen {ZUKOU_001}\ngo mate infinite"));
    std::thread::sleep(Duration::from_millis(300));
    let start = Instant::now();
    engine.send("stop");
    assert_eq!(engine.checkmate(Duration::from_secs(5)), "checkmate timeout");
    assert!(start.elapsed() < Duration::from_secs(2), "stop への応答が遅い");
    engine.assert_no_more_checkmate();
}

#[test]
fn setoption_during_go_mate_does_not_block_commands() {
    let mut engine = Engine::start();
    engine.send(&format!("position sfen {ZUKOU_001}\ngo mate infinite"));
    std::thread::sleep(Duration::from_millis(100));
    // 探索中の setoption は USI の手順違反だが、後続のコマンドを止めてはいけない
    engine.send("setoption name MateHashMB value 2");
    assert_eq!(engine.checkmate(Duration::from_secs(5)), "checkmate timeout");
    engine.send("position sfen 4k4/9/4P4/9/9/9/9/9/4K4 b G 1\ngo mate 5000");
    assert_eq!(engine.checkmate(Duration::from_secs(10)), "checkmate G*5b");
    engine.assert_no_more_checkmate();
}

#[test]
fn position_without_attacker_king_is_not_implemented() {
    let mut engine = Engine::start();
    engine.send("position sfen 4k4/8g/4P4/9/9/9/9/9/9 b G 1\ngo mate 5000");
    assert_eq!(engine.checkmate(Duration::from_secs(10)), "checkmate notimplemented");
    engine.assert_no_more_checkmate();
}

#[test]
fn new_go_cancels_go_mate_without_output() {
    let mut engine = Engine::start();
    engine.send(&format!("position sfen {ZUKOU_001}\ngo mate infinite"));
    std::thread::sleep(Duration::from_millis(100));
    // 停止されずに次の go mate が来たら、前の結果は出さずに新しい問題だけに答える
    engine.send("position sfen 4k4/9/4P4/9/9/9/9/9/4K4 b G 1\ngo mate 5000");
    assert_eq!(engine.checkmate(Duration::from_secs(10)), "checkmate G*5b");
    engine.assert_no_more_checkmate();
}
