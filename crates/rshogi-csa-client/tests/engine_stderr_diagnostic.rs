//! USI engine subprocess 死亡時に stderr 末尾 / engine path / exit status を含む
//! 診断 error を返すことを mock engine で検証する host 単体テスト。
//!
//! https://github.com/SH11235/rshogi/issues/593 partial fix の regression guard。fatal communication error 経路
//! (send BrokenPipe / recv Disconnected / wait_bestmove Disconnected) と
//! stderr ring buffer (4 KB cap、CRLF 吸収) の挙動を 6 fixture で pin する。
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

use rshogi_csa_client::engine::{SpawnOptions, UsiEngine};
use rshogi_csa_client::event::Event;

const SPAWN_TIMEOUT: Duration = Duration::from_secs(5);

/// 既存テストの 5 引数 spawn 呼び出しを集約するためのヘルパ。`stderr_passthrough` 以外は
/// fixture でほぼ固定値のため、testless な call site を増やさず簡潔に書けるようにする。
fn spawn_opts(stderr_passthrough: bool) -> SpawnOptions {
    SpawnOptions {
        ponder: false,
        startup_timeout: SPAWN_TIMEOUT,
        stderr_passthrough,
    }
}

static SCRIPT_SEQ: AtomicU64 = AtomicU64::new(0);
static TMPDIR_LOCK: Mutex<()> = Mutex::new(());

/// 与えた bash script を 0o755 の実行可能ファイルとして一時ディレクトリに書き出し、
/// path を返す。test ごとに unique な名前を付与する。
///
/// Linux で `cargo test` を並列実行すると、`std::fs::write` 完了直後の `Command::spawn`
/// で稀に `Text file busy (ETXTBSY)` を踏むため、tmp 書き出し → `sync_all` → chmod →
/// atomic rename の順で kernel に「書き終えた実行可能ファイル」を確実に認識させる
/// (PR #596 review で指摘された flake への対応)。
fn write_mock_script(name: &str, body: &str) -> PathBuf {
    use std::io::Write;
    let _guard = TMPDIR_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let seq = SCRIPT_SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir();
    let final_path =
        dir.join(format!("csa_client_mock_{}_{}_{}.sh", std::process::id(), name, seq,));
    // 書き込みは別 path で行い、close 後に rename することで「open 中の fd」が
    // exec と race する経路を排除する (ETXTBSY 回避)。
    let tmp_path =
        dir.join(format!("csa_client_mock_{}_{}_{}.sh.tmp", std::process::id(), name, seq,));
    {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&tmp_path)
            .expect("open tmp script");
        f.write_all(body.as_bytes()).expect("write mock script");
        f.sync_all().expect("sync_all");
    }
    let mut perms = std::fs::metadata(&tmp_path).expect("stat").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&tmp_path, perms).expect("chmod");
    std::fs::rename(&tmp_path, &final_path).expect("atomic rename");
    final_path
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
    let err = match UsiEngine::spawn(&path, &opts, spawn_opts(false)) {
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
    let mut engine =
        UsiEngine::spawn(&path, &opts, spawn_opts(false)).expect("初回 handshake は成功する想定");
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
        UsiEngine::spawn(&path, &opts, spawn_opts(false)).expect("初回 handshake は成功");
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
    let err = match UsiEngine::spawn(&path, &opts, spawn_opts(false)) {
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
    let err = match UsiEngine::spawn(&path, &opts, spawn_opts(false)) {
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

// ───────────────────────────────────────────────
// Fixture 6: 空行を含む stderr が EOF として誤認されないこと
//   PR #596 codex review で指摘された read_line_capped の bug への regression guard。
//   空行 `\n` を読んだ後に後続行を継続して読めることを pin する。
// ───────────────────────────────────────────────
#[test]
fn empty_stderr_line_is_not_treated_as_eof() {
    let script = r#"#!/usr/bin/env bash
read line  # usi
echo "id name mock"
echo "usiok"
printf 'before empty\n' >&2
printf '\n' >&2
printf 'after empty\n' >&2
exec 2>&-
exit 1
"#;
    let path = write_mock_script("empty_line_not_eof", script);
    let opts: HashMap<String, toml::Value> = HashMap::new();
    let err = match UsiEngine::spawn(&path, &opts, spawn_opts(false)) {
        Ok(_) => panic!("isready 後 engine 死亡 → error が期待される"),
        Err(e) => e,
    };
    let msg = format!("{err:#}");
    assert_diagnostic_prefix(&msg, &path);
    // `before empty` だけでなく `after empty` も含まれていれば、空行を EOF として
    // 誤認していない証拠。reader thread が空行で break した場合は `after empty` が
    // ring buffer に届かない (= bug 再発)。
    assert!(msg.contains("before empty"), "空行前の行が含まれるはず: {msg}");
    assert!(
        msg.contains("after empty"),
        "空行後の行も含まれるはず (空行 EOF 誤認 bug の regression guard): {msg}"
    );
}

// ───────────────────────────────────────────────
// Fixture 7: --engine-stderr-passthrough=true でも既存の ring buffer 末尾捕捉が
//   壊れないことの smoke test。log 多重化 (`log::info!`) 自体の capture は
//   global logger 依存で flake しやすいため本 test では検証せず、push 経路
//   との並行動作 (ring buffer 同等動作) を pin する。
// ───────────────────────────────────────────────
#[test]
fn stderr_passthrough_preserves_ring_buffer() {
    let script = r#"#!/usr/bin/env bash
printf 'passthrough line A\n' >&2
printf 'passthrough line B\n' >&2
exec 2>&-
exit 1
"#;
    let path = write_mock_script("passthrough_smoke", script);
    let opts: HashMap<String, toml::Value> = HashMap::new();
    // SpawnOptions { stderr_passthrough: true } で起動。
    let err = match UsiEngine::spawn(&path, &opts, spawn_opts(true)) {
        Ok(_) => panic!("spawn 即時死で error が期待される"),
        Err(e) => e,
    };
    let msg = format!("{err:#}");
    assert_diagnostic_prefix(&msg, &path);
    // STDERR_TAIL_MAX_LINES = 64 なので 2 行の `passthrough line A` / `passthrough line B`
    // は cap 落ちしない前提で両方とも diagnostic msg に含まれていることを厳格 assert する。
    // ring buffer cap が 1 行に縮まる将来変更があれば本 assertion は再検討が必要 (mock の
    // 出力行数 / cap のいずれかを揃える)。
    assert!(
        msg.contains("passthrough line A") && msg.contains("passthrough line B"),
        "passthrough=true でも ring buffer に末尾 (line A / line B 両方) が積まれているはず: {msg}"
    );
}

// ───────────────────────────────────────────────
// Fixture 8 (Linux only): engine が fork した孫プロセスが stderr fd を継承
//   している状況で、`UsiEngine::drop` の `killpg(pgid, SIGKILL)` により
//   孫プロセスにも SIGKILL が届き (reap は init / systemd 側) 、stderr reader
//   thread が EOF を観測して leak しないこと。
//
//   Issue #618: 既存の `child.kill()` 単独では孫が継承した stderr fd が
//   残り、reader thread が EOF を観測できず leak していた。
//
//   検証手順:
//   1. mock USI engine が fork して長 sleep の孫を生成、孫 pid を tmp file へ書く。
//   2. UsiEngine::spawn → handshake 完了。
//   3. drop(engine) で kill_engine_process_tree() が走る。
//   4. 孫 pid を /proc から polling で確認:
//      - drop 直後に setpgid(0,0) が反映済み: 孫の pgid == child の pid
//      - drop 後 1 秒以内に /proc/<pid>/stat が消滅 / state=Z (= SIGKILL 到達)
//   `/proc` を直接読むため Linux 限定 fixture。Linux 以外 (macOS / *BSD) は skip。
// ───────────────────────────────────────────────
#[cfg(target_os = "linux")]
#[test]
fn descendant_process_killed_via_killpg_on_drop() {
    use std::time::Instant;

    // tmpfile に孫 pid を書く同期チャネル。test 側は read で「孫が起動したか」を待つ。
    let pid_file = std::env::temp_dir().join(format!(
        "csa_client_grandchild_pid_{}_{}.txt",
        std::process::id(),
        SCRIPT_SEQ.fetch_add(1, Ordering::SeqCst),
    ));
    // 既存 file が残っていれば除去 (sequence 衝突の hedge)。
    let _ = std::fs::remove_file(&pid_file);

    // mock USI engine: fork して sleep の孫を background 起動。
    // - 孫は stderr を継承 (`exec 2>&-` を孫側で呼ばない)。
    // - 孫 pid を pid_file へ書き出し、親側 handshake を完了させた上で `read` で待機。
    // - 親 (engine) が SIGKILL されたら孫は孤児化、killpg なら孫も SIGKILL。
    let script = format!(
        r#"#!/usr/bin/env bash
# 孫プロセスを background で fork。stderr は継承 (close しない)。
( exec sleep 60 ) &
GRANDCHILD_PID=$!
printf '%s\n' "$GRANDCHILD_PID" > {pid_file}
read line  # usi
echo "id name mock_grandchild"
echo "usiok"
read line  # isready
echo "readyok"
# 親はこのまま read で待機。Drop で SIGKILL されるのを待つ。
read line
"#,
        pid_file = pid_file.display(),
    );
    let path = write_mock_script("grandchild_killpg", &script);
    let opts: HashMap<String, toml::Value> = HashMap::new();
    let engine =
        UsiEngine::spawn(&path, &opts, spawn_opts(false)).expect("初回 handshake は成功する想定");
    let parent_pid = engine.child_pid_for_test();

    // 孫 pid が pid_file に書かれるまで待つ (最大 2 秒)。
    let grandchild_pid = {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Ok(s) = std::fs::read_to_string(&pid_file)
                && let Ok(pid) = s.trim().parse::<i32>()
            {
                break pid;
            }
            if Instant::now() >= deadline {
                panic!("孫プロセス pid が {pid_file:?} に書かれませんでした");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    };

    // 孫の pgid が parent_pid と一致することを /proc/<pid>/stat から確認。
    // /proc/<pid>/stat の format: pid (comm) state ppid pgrp ...
    // comm にはスペースが含まれ得るため、最後の `)` で分割してから token 化する。
    let stat = std::fs::read_to_string(format!("/proc/{grandchild_pid}/stat"))
        .expect("/proc/<pid>/stat 読み取りに失敗");
    let after_comm =
        stat.rsplit_once(')').expect("stat に ')' がない (format 不正)").1.trim_start();
    let mut tokens = after_comm.split_whitespace();
    let _state = tokens.next().expect("state token");
    let _ppid = tokens.next().expect("ppid token");
    let pgrp: i32 = tokens.next().expect("pgrp token").parse().expect("pgrp parse");
    assert_eq!(
        pgrp, parent_pid as i32,
        "孫 pgid は parent (engine) pid と一致しているはず (setpgid 検証)"
    );

    // drop でプロセスツリー kill。
    drop(engine);

    // 孫 pid が消滅または zombie 状態 (Z) になるのを 2 秒 polling で確認。
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut ok = false;
    while Instant::now() < deadline {
        match std::fs::read_to_string(format!("/proc/{grandchild_pid}/stat")) {
            Err(_) => {
                ok = true;
                break;
            }
            Ok(stat) => {
                let after_comm = stat.rsplit_once(')').map(|s| s.1.trim_start()).unwrap_or("");
                let state = after_comm.split_whitespace().next().unwrap_or("");
                if state == "Z" {
                    ok = true;
                    break;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(ok, "孫プロセス {grandchild_pid} は drop 後 2 秒以内に kill されるはず (#618)");

    // tmp file 掃除 (best-effort)。
    let _ = std::fs::remove_file(&pid_file);
}
