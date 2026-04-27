//! CSA 再接続プロトコル (`Reconnect_Token` / `LOGIN ... reconnect:` /
//! `BEGIN Reconnect_State`) の client 側 parse / 送出ロジックを TCP loopback で
//! 確認する。
//!
//! `tungstenite` の WS スタックは別スレッドで mock サーバを立てる手間が大きい
//! ため、本テストは TCP 経路を使う。`CsaConnection::login_reconnect` /
//! `recv_reconnect_state` / `recv_game_summary` の `Reconnect_Token` 拡張行は
//! transport 種別非依存で実装されているため TCP / WS 共通の挙動として
//! 妥当に確認できる。

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::thread;
use std::time::Duration;

use rshogi_csa_client::protocol::CsaConnection;

/// 1 接続を受け取り、与えた `handler` を別スレッドで実行する mock CSA TCP サーバ。
fn spawn_mock_tcp_server<F>(handler: F) -> u16
where
    F: FnOnce(&mut BufReader<std::net::TcpStream>, &mut std::net::TcpStream) + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    thread::Builder::new()
        .name("mock-csa-server".to_string())
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
fn login_reconnect_sends_correct_token_format() {
    let port = spawn_mock_tcp_server(|reader, writer| {
        let line = read_line(reader);
        // LOGIN行の reconnect:<game_id>+<token> 形式が期待通りか確認
        assert!(
            line.contains("reconnect:game-42+abc1234"),
            "LOGIN line should contain reconnect:<game_id>+<token>: got {}",
            line
        );
        write_lines(writer, &["LOGIN:alice OK"]);
    });

    let mut conn = CsaConnection::connect("127.0.0.1", port, false).expect("connect");
    conn.login_reconnect("alice", "pw", "game-42", "abc1234")
        .expect("login_reconnect");
}

#[test]
fn login_reconnect_propagates_incorrect_response() {
    let port = spawn_mock_tcp_server(|reader, writer| {
        let _ = read_line(reader);
        write_lines(writer, &["LOGIN:incorrect reconnect_rejected"]);
    });

    let mut conn = CsaConnection::connect("127.0.0.1", port, false).expect("connect");
    let err = conn
        .login_reconnect("alice", "pw", "game-42", "bad-token")
        .expect_err("expected reconnect failure");
    assert!(err.to_string().contains("再接続失敗"));
}

#[test]
fn recv_game_summary_extracts_reconnect_token() {
    let port = spawn_mock_tcp_server(|reader, writer| {
        // LOGIN受信→OK応答
        let _ = read_line(reader);
        write_lines(writer, &["LOGIN:alice OK"]);
        // Game_Summary送出 (Reconnect_Token拡張行付き)
        write_lines(
            writer,
            &[
                "BEGIN Game_Summary",
                "Protocol_Version:1.2",
                "Game_ID:game-42",
                "Name+:black",
                "Name-:white",
                "Your_Turn:+",
                "To_Move:+",
                "Time_Unit:1sec",
                "Total_Time:600",
                "Byoyomi:10",
                "BEGIN Position",
                "PI",
                "+",
                "END Position",
                "Reconnect_Token:black-token-xyz",
                "END Game_Summary",
            ],
        );
    });

    let mut conn = CsaConnection::connect("127.0.0.1", port, false).expect("connect");
    conn.login("alice", "pw").expect("login");
    let summary = conn.recv_game_summary(0).expect("recv_game_summary");
    assert_eq!(summary.game_id, "game-42");
    assert_eq!(summary.reconnect_token.as_deref(), Some("black-token-xyz"));
}

#[test]
fn recv_game_summary_handles_missing_reconnect_token() {
    let port = spawn_mock_tcp_server(|reader, writer| {
        let _ = read_line(reader);
        write_lines(writer, &["LOGIN:alice OK"]);
        write_lines(
            writer,
            &[
                "BEGIN Game_Summary",
                "Protocol_Version:1.2",
                "Game_ID:game-no-token",
                "Name+:black",
                "Name-:white",
                "Your_Turn:+",
                "To_Move:+",
                "Time_Unit:1sec",
                "Total_Time:600",
                "Byoyomi:10",
                "BEGIN Position",
                "PI",
                "+",
                "END Position",
                "END Game_Summary",
            ],
        );
    });

    let mut conn = CsaConnection::connect("127.0.0.1", port, false).expect("connect");
    conn.login("alice", "pw").expect("login");
    let summary = conn.recv_game_summary(0).expect("recv_game_summary");
    assert!(summary.reconnect_token.is_none());
}

#[test]
fn recv_reconnect_state_parses_all_fields() {
    let port = spawn_mock_tcp_server(|reader, writer| {
        let _ = read_line(reader);
        write_lines(writer, &["LOGIN:alice OK"]);
        write_lines(
            writer,
            &[
                "BEGIN Reconnect_State",
                "Current_Turn:-",
                "Black_Time_Remaining_Ms:599500",
                "White_Time_Remaining_Ms:600000",
                "Last_Move:+7776FU",
                "END Reconnect_State",
            ],
        );
    });

    let mut conn = CsaConnection::connect("127.0.0.1", port, false).expect("connect");
    conn.login("alice", "pw").expect("login");
    let state = conn.recv_reconnect_state().expect("recv_reconnect_state");
    assert_eq!(state.black_remaining_ms, 599_500);
    assert_eq!(state.white_remaining_ms, 600_000);
    assert_eq!(state.last_move.as_deref(), Some("+7776FU"));
    assert_eq!(state.current_turn, Some(rshogi_csa::Color::White));
}

#[test]
fn recv_reconnect_state_handles_missing_last_move() {
    let port = spawn_mock_tcp_server(|reader, writer| {
        let _ = read_line(reader);
        write_lines(writer, &["LOGIN:alice OK"]);
        write_lines(
            writer,
            &[
                "BEGIN Reconnect_State",
                "Current_Turn:+",
                "Black_Time_Remaining_Ms:600000",
                "White_Time_Remaining_Ms:600000",
                "END Reconnect_State",
            ],
        );
    });

    let mut conn = CsaConnection::connect("127.0.0.1", port, false).expect("connect");
    conn.login("alice", "pw").expect("login");
    let state = conn.recv_reconnect_state().expect("recv_reconnect_state");
    assert!(state.last_move.is_none());
    assert_eq!(state.current_turn, Some(rshogi_csa::Color::Black));
}
