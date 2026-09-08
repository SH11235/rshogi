//! SPSA 統合テスト用 USI mock。設定は子プロセスの環境変数から読む。
//!
//! `SPSA_TEST_ENGINE_MODE` の mode:
//! - `resign`（既定）: 即投了。`value_driven`: INT >= 6 なら win、それ以外は resign。
//! - `hang_on_go`: 探索に無応答。`stop_then_bestmove`: stop 後に resign。
//! - `info_flood`: go 後に info を出し続ける。`exit_on_go`: go で異常終了。
//! - `ignore_quit`: 即投了するが quit を無視する。
//! - `cancel_script`: 最初の go を受けた個体は他 worker の go 到達後に異常終了。
//!   他個体は quit を無視し、失敗側の相方の quit marker を待って、
//!   position の moves 数に応じた合法手を返す。
//! - `initialize_cancel_script`: 最初の usi 受信個体が相方 worker の go を待って
//!   不正 UTF-8 を返し初期化を失敗させる。quit marker で相方の応答を解放する。
//! - `initialize_fail_once`: 最初の usi だけ不正 UTF-8 を返す。再試行時は即投了。
//!
//! 環境変数（すべて `SPSA_TEST_ENGINE_` 接頭辞）:
//! - `SPAWN_LOG`: PID を起動ごとに追記。
//! - `PROTOCOL_LOG`: PID と受信コマンドを1行1 write で追記。
//! - `FAIL_ONCE_MARKER`: create_new に成功した個体だけ go で異常終了。
//! - `FAIL_AFTER_GO`: 上記失敗を各個体の指定回数の go 後まで遅延（既定0）。
//!   仕様 F への追加で、batch 2 失敗・resume テスト用。
//! - `SCRIPT_GATE`: cancel_script 用 marker の基底パス。拡張子 fail は失敗役の選出、
//!   waiting は他 worker の go 到達、quit は失敗側の破棄開始を表す。
use std::fs::OpenOptions;
use std::io::{BufRead, Write};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

fn append_line(path: &std::path::Path, line: &str) {
    let mut file = OpenOptions::new().create(true).append(true).open(path).unwrap();
    let bytes = line.as_bytes();
    // 並行プロセスの行を混ぜないよう、1 行を単一 write で append する。
    assert_eq!(file.write(bytes).unwrap(), bytes.len());
}

fn main() {
    if let Some(path) = std::env::var_os("SPSA_TEST_ENGINE_SPAWN_LOG") {
        append_line(std::path::Path::new(&path), &format!("{}\n", std::process::id()));
    }
    let mode = std::env::var("SPSA_TEST_ENGINE_MODE").unwrap_or_else(|_| "resign".into());
    let marker = std::env::var_os("SPSA_TEST_ENGINE_FAIL_ONCE_MARKER");
    let fail_after = std::env::var("SPSA_TEST_ENGINE_FAIL_AFTER_GO")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    let mut go_count = 0;
    let mut value = 0;
    let protocol_log = std::env::var_os("SPSA_TEST_ENGINE_PROTOCOL_LOG");
    let script_gate =
        std::env::var_os("SPSA_TEST_ENGINE_SCRIPT_GATE").map(std::path::PathBuf::from);
    let mut moves_count = 0;
    let flooding = Arc::new(AtomicBool::new(false));
    let mut flood_thread = None;
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else {
            break;
        };
        let trimmed = line.trim();
        if let Some(path) = &protocol_log {
            append_line(std::path::Path::new(path), &format!("{} {trimmed}\n", std::process::id()));
        }
        let response = if trimmed == "usi" {
            if matches!(mode.as_str(), "initialize_cancel_script" | "initialize_fail_once")
                && OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(script_gate.as_ref().unwrap().with_extension("fail"))
                    .is_ok()
            {
                if mode == "initialize_cancel_script" {
                    wait_marker(&script_gate.as_ref().unwrap().with_extension("waiting"));
                }
                // reader の UTF-8 エラーで initialize を失敗させ、quit を読む個体は残す。
                let mut stdout = std::io::stdout().lock();
                stdout.write_all(b"\xff\n").unwrap();
                stdout.flush().unwrap();
                None
            } else {
                Some("id name spsa-test-engine\nusiok")
            }
        } else if trimmed == "isready" {
            Some("readyok")
        } else if let Some(v) = trimmed.strip_prefix("setoption name SPSA_TEST_INT value ") {
            value = v.parse::<i64>().unwrap_or(0);
            None
        } else if trimmed.starts_with("position ") {
            moves_count = position_moves_count(trimmed);
            None
        } else if trimmed.starts_with("go") {
            go_count += 1;
            if go_count > fail_after
                && marker.as_ref().is_some_and(|path| {
                    OpenOptions::new().write(true).create_new(true).open(path).is_ok()
                })
            {
                std::process::exit(1);
            }
            match mode.as_str() {
                "cancel_script" | "initialize_cancel_script" => {
                    let gate = script_gate.as_ref().expect("SCRIPT_GATE required");
                    let script_failure = mode == "cancel_script"
                        && OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(gate.with_extension("fail"))
                            .is_ok();
                    if script_failure {
                        wait_marker(&gate.with_extension("waiting"));
                        std::process::exit(1);
                    } else {
                        std::fs::write(gate.with_extension("waiting"), b"").unwrap();
                        wait_marker(&gate.with_extension("quit"));
                        Some(match moves_count {
                            0 => "bestmove 7g7f",
                            1 => "bestmove 3c3d",
                            _ => "bestmove resign",
                        })
                    }
                }
                "exit_on_go" => std::process::exit(1),
                "hang_on_go" | "stop_then_bestmove" => None,
                "info_flood" => {
                    if flood_thread.is_none() {
                        flooding.store(true, Ordering::Release);
                        let active = flooding.clone();
                        flood_thread = Some(std::thread::spawn(move || {
                            while active.load(Ordering::Acquire) {
                                let mut stdout = std::io::stdout().lock();
                                if writeln!(stdout, "info depth 1 nodes 1")
                                    .and_then(|()| stdout.flush())
                                    .is_err()
                                {
                                    break;
                                }
                                drop(stdout);
                                std::thread::sleep(Duration::from_millis(1));
                            }
                        }));
                    }
                    None
                }
                "value_driven" if value >= 6 => Some("bestmove win"),
                _ => Some("bestmove resign"),
            }
        } else if trimmed == "stop" && mode == "stop_then_bestmove" {
            Some("bestmove resign")
        } else if trimmed == "quit"
            && matches!(mode.as_str(), "cancel_script" | "initialize_cancel_script")
        {
            std::fs::write(script_gate.as_ref().unwrap().with_extension("quit"), b"").unwrap();
            None
        } else if trimmed == "quit" && mode != "ignore_quit" {
            break;
        } else {
            None
        };
        if let Some(response) = response {
            let mut stdout = std::io::stdout().lock();
            if writeln!(stdout, "{response}").and_then(|()| stdout.flush()).is_err() {
                break;
            }
        }
    }
    flooding.store(false, Ordering::Release);
    if let Some(handle) = flood_thread {
        let _ = handle.join();
    }
}

fn wait_marker(path: &std::path::Path) {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while !path.exists() {
        assert!(std::time::Instant::now() < deadline, "marker deadline: {}", path.display());
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn position_moves_count(position: &str) -> usize {
    let moves = position.split_whitespace().skip_while(|part| *part != "moves").skip(1).count();
    // run_game の fresh SFEN の手数と、moves 形式の追加入力の両方を扱う。
    let played = if position.starts_with("position sfen ") {
        position
            .split_whitespace()
            .nth(5)
            .and_then(|n| n.parse::<usize>().ok())
            .unwrap_or(1)
            .saturating_sub(1)
    } else {
        0
    };
    played + moves
}

#[cfg(test)]
mod tests {
    use super::position_moves_count;

    #[test]
    fn counts_fresh_sfen_and_appended_moves() {
        assert_eq!(position_moves_count("position sfen board b - 1"), 0);
        assert_eq!(position_moves_count("position sfen board w - 2"), 1);
        assert_eq!(position_moves_count("position sfen board b - 3 moves 7g7f 3c3d"), 4);
        assert_eq!(position_moves_count("position startpos moves 7g7f 3c3d"), 2);
        assert_eq!(position_moves_count("position startpos"), 0);
    }
}
