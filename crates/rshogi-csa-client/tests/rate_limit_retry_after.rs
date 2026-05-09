//! `rate_limited retry_after=<sec>` honoring (#682) の integration test。
//!
//! mock CSA TCP サーバが `LOGIN:incorrect rate_limited retry_after=<sec>` /
//! `LOGIN_LOBBY:incorrect rate_limited retry_after=<sec>` を返したときの client
//! 側動作を 2 段で pin する:
//!
//! 1. server の raw 応答が `protocol::login` の `bail!("ログイン失敗: {response}")`
//!    を経て `anyhow::Error` の display 文字列に乗っても、
//!    `extract_retry_after_sec` が `<sec>` を抽出できること (round-trip)。
//! 2. 同 Err msg を `compute_effective_retry_delay` (lib pub fn) に流すと、次
//!    sleep が `<sec>` 秒以上になること。
//!
//! 実 sleep 時間そのものを計測する test は flaky になりがちなので避けている。
//! main loop の sleep 配線そのものは `src/main.rs` の `sleep_with_shutdown`
//! unit test で pin している。

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::thread;
use std::time::Duration;

use rshogi_csa_client::protocol::{
    CsaConnection, compute_effective_retry_delay, extract_retry_after_sec,
};

/// 1 接続を受け取り、与えた `handler` を別スレッドで実行する mock CSA TCP サーバ。
/// `csa_reconnect_protocol.rs` と同じ pattern。
fn spawn_mock_tcp_server<F>(handler: F) -> u16
where
    F: FnOnce(&mut BufReader<std::net::TcpStream>, &mut std::net::TcpStream) + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    thread::Builder::new()
        .name("mock-csa-rate-limit".to_string())
        .spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
            stream.set_write_timeout(Some(Duration::from_secs(5))).ok();
            let mut writer = stream.try_clone().expect("clone stream");
            let mut reader = BufReader::new(stream);
            handler(&mut reader, &mut writer);
        })
        .expect("spawn");
    port
}

fn read_line(reader: &mut BufReader<std::net::TcpStream>) -> String {
    let mut buf = String::new();
    reader.read_line(&mut buf).expect("read line");
    buf.trim_end_matches(['\r', '\n']).to_owned()
}

fn write_lines(writer: &mut std::net::TcpStream, lines: &[&str]) {
    for line in lines {
        writeln!(writer, "{}", line).expect("write line");
    }
    writer.flush().expect("flush");
}

#[test]
fn login_rate_limited_response_propagates_retry_after_to_helper() {
    // server が `LOGIN:incorrect rate_limited retry_after=7` を返したとき、
    // `CsaConnection::login` は `bail!("ログイン失敗: ...")` で Err を返す。
    // その Err の display を `extract_retry_after_sec` に流すと 7 が取れる。
    let port = spawn_mock_tcp_server(|reader, writer| {
        let _ = read_line(reader);
        write_lines(writer, &["LOGIN:incorrect rate_limited retry_after=7"]);
    });

    let mut conn = CsaConnection::connect("127.0.0.1", port, false).expect("connect");
    let err = conn.login("alice", "pw").expect_err("expected rate_limited");
    let msg = err.to_string();

    assert!(
        msg.contains("rate_limited"),
        "error msg should contain rate_limited token: {msg}"
    );
    assert_eq!(
        extract_retry_after_sec(&msg),
        Some(7),
        "extract_retry_after_sec should parse 7 from bail msg: {msg}"
    );
}

#[test]
fn login_rate_limited_effective_delay_respects_retry_after() {
    // バックオフ 1s だが server が retry_after=5 を要求 → 次 sleep は 5s 以上。
    let port = spawn_mock_tcp_server(|reader, writer| {
        let _ = read_line(reader);
        write_lines(writer, &["LOGIN:incorrect rate_limited retry_after=5"]);
    });

    let mut conn = CsaConnection::connect("127.0.0.1", port, false).expect("connect");
    let err = conn.login("alice", "pw").expect_err("expected rate_limited");
    let actual = compute_effective_retry_delay(&err.to_string(), Duration::from_secs(1));

    assert!(
        actual >= Duration::from_secs(5),
        "next sleep must be >= retry_after (5s), got {actual:?}"
    );
    assert_eq!(actual, Duration::from_secs(5));
}

#[test]
fn login_rate_limited_keeps_backoff_when_longer() {
    // バックオフ 60s が server retry_after=3 より長いケース。retry_after で
    // バックオフを巻き戻さない契約 (storm 再開防止) を pin する。
    let port = spawn_mock_tcp_server(|reader, writer| {
        let _ = read_line(reader);
        write_lines(writer, &["LOGIN:incorrect rate_limited retry_after=3"]);
    });

    let mut conn = CsaConnection::connect("127.0.0.1", port, false).expect("connect");
    let err = conn.login("alice", "pw").expect_err("expected rate_limited");
    let actual = compute_effective_retry_delay(&err.to_string(), Duration::from_secs(60));

    assert_eq!(actual, Duration::from_secs(60));
}

#[test]
fn login_non_rate_limited_falls_back_to_backoff() {
    // `unknown_game_name` 等の retry_after を伴わない reason ではバックオフ
    // をそのまま使う (既存挙動温存)。
    let port = spawn_mock_tcp_server(|reader, writer| {
        let _ = read_line(reader);
        write_lines(writer, &["LOGIN:incorrect unknown_game_name"]);
    });

    let mut conn = CsaConnection::connect("127.0.0.1", port, false).expect("connect");
    let err = conn.login("alice", "pw").expect_err("expected login failure");
    let actual = compute_effective_retry_delay(&err.to_string(), Duration::from_secs(4));

    assert_eq!(actual, Duration::from_secs(4));
}

#[test]
fn lobby_login_rate_limited_response_round_trips() {
    // `acquire_lobby_match` (private) は raw `LOGIN_LOBBY` を生で送り、応答を
    // `[Lobby] LOGIN_LOBBY 拒否: incorrect rate_limited retry_after=10` の形で
    // bail する。同形式の Err msg が `extract_retry_after_sec` で parse 可能で
    // あることを pin する (raw transport 経由の round-trip 確認)。
    let lobby_err_msg = "[Lobby] LOGIN_LOBBY 拒否: incorrect rate_limited retry_after=10";
    assert_eq!(extract_retry_after_sec(lobby_err_msg), Some(10));
    let actual = compute_effective_retry_delay(lobby_err_msg, Duration::from_secs(2));
    assert_eq!(actual, Duration::from_secs(10));
}
