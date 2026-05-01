//! USI engine subprocess 死亡時に stderr 末尾 / engine path / exit status を含む
//! 診断 error を返すことを mock engine で検証する host 単体テスト。
//!
//! Issue #593 partial fix の regression guard。fatal communication error 経路
//! (send BrokenPipe / recv Disconnected / wait_bestmove Disconnected) と
//! stderr ring buffer (4 KB cap、CRLF 吸収) の挙動を 5 fixture で pin する。
//!
//! 対象 OS: Unix (bash 必須)。Windows には mock USI engine 経路がないため
//! `#[cfg(unix)]` でガードする。

#![cfg(unix)]

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use rshogi_csa_client::engine::UsiEngine;
use rshogi_csa_client::event::Event;

const SPAWN_TIMEOUT: Duration = Duration::from_secs(5);

static SCRIPT_SEQ: AtomicU64 = AtomicU64::new(0);
static TMPDIR_LOCK: Mutex<()> = Mutex::new(());

/// 与えた bash script を 0o755 の実行可能ファイルとして一時ディレクトリに書き出し、
/// path を返す。test ごとに unique な名前を付与する。
fn write_mock_script(name: &str, body: &str) -> PathBuf {
    let _guard = TMPDIR_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let seq = SCRIPT_SEQ.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!(
        "csa_client_mock_{}_{}_{}.sh",
        std::process::id(),
        name,
        seq,
    ));
    std::fs::write(&path, body).expect("write mock script");
    let mut perms = std::fs::metadata(&path).expect("stat").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod");
    path
}

/// 死亡時の error message が満たすべき共通条件 (path + (exit or status=unknown))
fn assert_diagnostic_prefix(msg: &str, expected_path: &Path) {
    assert!(msg.contains("エンジンプロセスが終了しました"), "missing prefix in: {msg}");
    let path_str = expected_path.display().to_string();
    assert!(msg.contains(&format!("path={path_str}")), "missing path={path_str} in: {msg}");
    assert!(
        msg.contains("exit=") || msg.contains("status=unknown"),
        "missing exit/status in: {msg}"
    );
}

// ───────────────────────────────────────────────
// Fixture 1: spawn 直後 stderr 書き出し → exit 1
// ───────────────────────────────────────────────
#[test]
fn dying_engine_immediate_includes_stderr_tail() {
    let script = r#"#!/usr/bin/env bash
printf 'stderr line 1\n' >&2
printf 'stderr line 2\n' >&2
exec 2>&-
exit 1
"#;
    let path = write_mock_script("dying_immediate", script);
    let opts: HashMap<String, toml::Value> = HashMap::new();
    let err = match UsiEngine::spawn(&path, &opts, false, SPAWN_TIMEOUT) {
        Ok(_) => panic!("spawn 即時死で error が期待される"),
        Err(e) => e,
    };
    let msg = format!("{err:#}");
    assert_diagnostic_prefix(&msg, &path);
    assert!(
        msg.contains("stderr line 1") || msg.contains("stderr line 2"),
        "stderr 末尾 (line 1 / line 2) のいずれかが含まれるはず: {msg}"
    );
}

// ───────────────────────────────────────────────
// Fixture 2: usi/usiok + isready/readyok 後 stderr → exit
//   new_game() で死亡を観測する
// ───────────────────────────────────────────────
#[test]
fn dying_engine_after_first_handshake_includes_stderr_tail() {
    let script = r#"#!/usr/bin/env bash
read line  # usi
echo "id name mock"
echo "usiok"
read line  # isready
echo "readyok"
printf 'engine info on stderr A\n' >&2
printf 'engine info on stderr B\n' >&2
exec 2>&-
exit 1
"#;
    let path = write_mock_script("dying_after_handshake", script);
    let opts: HashMap<String, toml::Value> = HashMap::new();
    let mut engine = UsiEngine::spawn(&path, &opts, false, SPAWN_TIMEOUT)
        .expect("初回 handshake は成功する想定");
    // engine プロセスは usiok+readyok を返した直後に exit。
    // new_game() は usinewgame + isready を送る。BrokenPipe か recv Disconnected
    // のいずれかから engine_exited_error() に合流する。
    let err = engine.new_game().expect_err("engine 死亡で error が期待される");
    let msg = format!("{err:#}");
    assert_diagnostic_prefix(&msg, &path);
    assert!(
        msg.contains("engine info on stderr A") || msg.contains("engine info on stderr B"),
        "stderr 末尾の line A/B が含まれるはず: {msg}"
    );
}

// ───────────────────────────────────────────────
// Fixture 3: full handshake → go の応答前に exit
// ───────────────────────────────────────────────
#[test]
fn dying_engine_during_go_includes_stderr_tail() {
    let script = r#"#!/usr/bin/env bash
read line  # usi
echo "id name mock"
echo "usiok"
read line  # isready (initialize)
echo "readyok"
read line  # usinewgame
read line  # isready (new_game)
echo "readyok"
read line  # position
read line  # go
printf 'info string about to die\n' >&2
exec 2>&-
exit 1
"#;
    let path = write_mock_script("dying_during_go", script);
    let opts: HashMap<String, toml::Value> = HashMap::new();
    let mut engine =
        UsiEngine::spawn(&path, &opts, false, SPAWN_TIMEOUT).expect("初回 handshake は成功");
    engine.new_game().expect("new_game は成功");
    let shutdown = AtomicBool::new(false);
    let (_tx, server_rx) = mpsc::channel::<Event>();
    let err = match engine.go(
        "position startpos",
        "go btime 1000 wtime 1000 byoyomi 100",
        &shutdown,
        &server_rx,
    ) {
        Ok(_) => panic!("go の応答前に engine が死亡 → error が期待される"),
        Err(e) => e,
    };
    let msg = format!("{err:#}");
    assert_diagnostic_prefix(&msg, &path);
    assert!(
        msg.contains("info string about to die"),
        "stderr 末尾の `info string about to die` が含まれるはず: {msg}"
    );
}

// ───────────────────────────────────────────────
// Fixture 4: 4096 byte cap (1 行 10000 byte → 4096 まで)
// ───────────────────────────────────────────────
#[test]
fn long_stderr_line_is_truncated_to_cap() {
    let script = r#"#!/usr/bin/env bash
read line  # usi
echo "id name mock"
echo "usiok"
head -c 10000 /dev/zero | tr '\0' A >&2
exec 2>&-
exit 1
"#;
    let path = write_mock_script("long_stderr_line", script);
    let opts: HashMap<String, toml::Value> = HashMap::new();
    // initialize は usiok 後 isready を送る → engine 死亡で error
    let err = match UsiEngine::spawn(&path, &opts, false, SPAWN_TIMEOUT) {
        Ok(_) => panic!("isready 送信前後で engine 死亡 → error が期待される"),
        Err(e) => e,
    };
    let msg = format!("{err:#}");
    assert_diagnostic_prefix(&msg, &path);
    // 末尾の最長行は 4096 文字 (`A` * 4096) に truncate されているはず。
    // message 全体長は prefix を加味してもおおよそ < 4096 + 数百 byte。
    let max_line_len = msg.lines().map(|line| line.chars().count()).max().unwrap_or(0);
    assert!(
        max_line_len <= 4096,
        "最長行は 4096 char 以下に truncate されるはず (実測 {max_line_len}): {msg}"
    );
}

// ───────────────────────────────────────────────
// Fixture 5: CRLF 吸収 (`\r` 除去)
// ───────────────────────────────────────────────
#[test]
fn crlf_stderr_is_trimmed() {
    let script = r#"#!/usr/bin/env bash
read line  # usi
echo "id name mock"
echo "usiok"
printf 'CRLF line\r\n' >&2
exec 2>&-
exit 1
"#;
    let path = write_mock_script("crlf_stderr", script);
    let opts: HashMap<String, toml::Value> = HashMap::new();
    let err = match UsiEngine::spawn(&path, &opts, false, SPAWN_TIMEOUT) {
        Ok(_) => panic!("isready 後 engine 死亡 → error が期待される"),
        Err(e) => e,
    };
    let msg = format!("{err:#}");
    assert_diagnostic_prefix(&msg, &path);
    assert!(msg.contains("CRLF line"), "`CRLF line` が含まれるはず: {msg}");
    // `\r` 単独 (CR だけが残る) は出ないはず。`\r\n` の CR を trim しているか確認。
    // message 内に `CRLF line\r` (= 末尾 CR) が現れたら trim 失敗。
    assert!(!msg.contains("CRLF line\r"), "末尾の `\\r` は trim されているはず: {msg}");
}
