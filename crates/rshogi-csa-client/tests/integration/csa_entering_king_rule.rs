//! Game_Summary の入玉ルール広告 (`Entering_King_Rule:` 拡張行 / CSA 標準
//! `Declaration:Jishogi 1.1`) を client が正しく解釈するか TCP loopback で確認する。
//! transport 種別非依存の parse 経路のため TCP / WS 共通の挙動として妥当に確認できる。

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::thread;

use rshogi_core::types::EnteringKingRule;
use rshogi_csa_client::protocol::CsaConnection;

/// 1 接続を受け取り、与えた `handler` を別スレッドで実行する mock CSA TCP サーバ。
fn spawn_mock_tcp_server<F>(handler: F) -> u16
where
    F: FnOnce(&mut dyn BufRead, &mut dyn Write) + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("local_addr").port();
    thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept");
        let mut reader = BufReader::new(stream.try_clone().expect("clone"));
        let mut writer = stream;
        handler(&mut reader, &mut writer);
    });
    port
}

fn read_line(reader: &mut dyn BufRead) -> String {
    let mut line = String::new();
    reader.read_line(&mut line).expect("read_line");
    line.trim_end().to_owned()
}

fn write_lines(writer: &mut dyn Write, lines: &[&str]) {
    for line in lines {
        writer.write_all(line.as_bytes()).expect("write");
        writer.write_all(b"\n").expect("write");
    }
    writer.flush().expect("flush");
}

/// 最小 Game_Summary を送出する mock を立て、login + recv_game_summary した結果を
/// 返す。`header_extra` はヘッダフィールド部 (rshogi サーバーの `Declaration:` 位置)、
/// `tail_extra` は `END Position` 後 (同 `Entering_King_Rule:` 拡張行の位置) に挿入する。
fn recv_summary_with(
    header_extra: &'static [&'static str],
    tail_extra: &'static [&'static str],
) -> rshogi_csa_client::GameSummary {
    let port = spawn_mock_tcp_server(move |reader, writer| {
        let _ = read_line(reader);
        write_lines(writer, &["LOGIN:alice OK"]);
        let mut lines = vec!["BEGIN Game_Summary", "Protocol_Version:1.2"];
        lines.extend_from_slice(header_extra);
        lines.extend_from_slice(&[
            "Game_ID:game-ekr",
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
        ]);
        lines.extend_from_slice(tail_extra);
        lines.push("END Game_Summary");
        write_lines(writer, &lines);
    });

    let mut conn = CsaConnection::connect("127.0.0.1", port, false).expect("connect");
    conn.login("alice", "pw").expect("login");
    conn.recv_game_summary(0).expect("recv_game_summary")
}

#[test]
fn extension_line_maps_usi_token() {
    let summary = recv_summary_with(&[], &["Entering_King_Rule:CSARule24"]);
    assert_eq!(summary.entering_king_rule, Some(EnteringKingRule::Point24));
}

#[test]
fn declaration_jishogi_maps_to_point27() {
    let summary = recv_summary_with(&["Declaration:Jishogi 1.1"], &[]);
    assert_eq!(summary.entering_king_rule, Some(EnteringKingRule::Point27));
}

#[test]
fn extension_line_wins_over_declaration() {
    // 本リポサーバーが将来 Point24 のまま `Declaration:` も出すような構成でも
    // 拡張行 (正確なルール) を優先する。
    let summary =
        recv_summary_with(&["Declaration:Jishogi 1.1"], &["Entering_King_Rule:CSARule24"]);
    assert_eq!(summary.entering_king_rule, Some(EnteringKingRule::Point24));
}

#[test]
fn absent_advertisement_yields_none() {
    let summary = recv_summary_with(&[], &[]);
    assert_eq!(summary.entering_king_rule, None);
}

#[test]
fn unknown_token_yields_none() {
    let summary = recv_summary_with(&[], &["Entering_King_Rule:BogusRule"]);
    assert_eq!(summary.entering_king_rule, None);
}
