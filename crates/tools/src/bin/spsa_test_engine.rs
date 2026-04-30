//! SPSA integration test 用の最小 USI エンジン mock。
//!
//! 本物のエンジンを呼ばずに `spsa` バイナリの run-dir 整合性 (state.params /
//! meta.json / values.csv の同時生成 + resume append) を検証するためだけの
//! ヘルパーバイナリ。production には使わない。
//!
//! プロトコル動作:
//! - `usi` → `id name spsa-test-engine`, `usiok`
//! - `isready` → `readyok`
//! - `setoption name <X> value <Y>` → 黙って ack
//! - `usinewgame` → 何もしない
//! - `position ...` → 何もしない
//! - `go ...` → `bestmove resign`
//! - `quit` → exit
//!
//! `bestmove resign` で各 game が即座に終わるため、SPSA loop は数 iter が
//! 数秒で完了する。SPSA の数学的妥当性 (摂動応答性) は検証せず、I/O 整合性
//! のみテストする想定。

use std::io::{BufRead, Write};

fn main() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    let stdin_lock = stdin.lock();
    for line in stdin_lock.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let trimmed = line.trim();
        if trimmed == "usi" {
            let _ = writeln!(stdout, "id name spsa-test-engine");
            let _ = writeln!(stdout, "id author claude-code");
            let _ = writeln!(stdout, "usiok");
        } else if trimmed == "isready" {
            let _ = writeln!(stdout, "readyok");
        } else if trimmed.starts_with("go") {
            let _ = writeln!(stdout, "bestmove resign");
        } else if trimmed == "quit" {
            break;
        }
        // setoption / position / usinewgame / その他は黙って受理。
        let _ = stdout.flush();
    }
}
