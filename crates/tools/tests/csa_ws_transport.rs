//! CSA-over-WebSocket transport の疎通テスト。
//!
//! `tungstenite` の sync server を loopback ポートで立て、`CsaTransport` の
//! WebSocket 経路が 1 line = 1 text frame の対応で送受信できることを確認する。

use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use tools::csa_client::event::Event;
use tools::csa_client::transport::{ConnectOpts, CsaTransport, TransportTarget};
use tungstenite::{Message, accept};

/// 1 接続を受け取り、与えた `script` のスクリプトを順次実行する mock WebSocket
/// サーバを別スレッドで起動して、選んだポートと join handle を返す。
fn spawn_mock_ws_server<F>(handler: F) -> (u16, thread::JoinHandle<()>)
where
    F: FnOnce(&mut tungstenite::WebSocket<std::net::TcpStream>) + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().unwrap().port();
    let join = thread::Builder::new()
        .name("mock-ws-server".to_string())
        .spawn(move || {
            let (stream, _) = listener.accept().expect("mock accept");
            let mut ws = accept(stream).expect("mock ws handshake");
            handler(&mut ws);
            // Close は呼び出し側が自身で投げるので、ここではフレーム close まで実行。
            let _ = ws.close(None);
            let _ = ws.flush();
        })
        .expect("spawn mock");
    (port, join)
}

#[test]
fn ws_transport_send_then_recv_line() {
    let (port, join) = spawn_mock_ws_server(|ws| {
        let msg = ws.read().expect("read text");
        match msg {
            Message::Text(t) => assert_eq!(t.as_str(), "LOGIN alice pw"),
            other => panic!("expected text, got {other:?}"),
        }
        ws.send(Message::Text("LOGIN:OK".into())).expect("send response");
    });

    let target = TransportTarget::from_host_port(&format!("ws://127.0.0.1:{port}/"), 0);
    let mut transport = CsaTransport::connect(
        &target,
        &ConnectOpts {
            tcp_keepalive: false,
            ws_origin: Some("http://localhost".to_owned()),
        },
    )
    .expect("connect");

    transport.write_line("LOGIN alice pw").expect("write");
    let line = transport.read_line_blocking(Duration::from_secs(5)).expect("read response");
    assert_eq!(line, "LOGIN:OK");

    drop(transport);
    join.join().expect("server thread");
}

#[test]
fn ws_transport_reader_thread_delivers_multiple_lines() {
    let (port, join) = spawn_mock_ws_server(|ws| {
        // クライアントの「READY」を待ってから 3 行 push する。
        let _ = ws.read();
        for line in ["LINE_A", "LINE_B", "LINE_C"] {
            ws.send(Message::Text(line.into())).expect("send");
        }
        // 0.5 秒余裕を持って close 前に flush。
        thread::sleep(Duration::from_millis(50));
    });

    let target = TransportTarget::from_host_port(&format!("ws://127.0.0.1:{port}/"), 0);
    let mut transport = CsaTransport::connect(
        &target,
        &ConnectOpts {
            tcp_keepalive: false,
            ws_origin: Some("http://localhost".to_owned()),
        },
    )
    .expect("connect");

    transport.write_line("READY").expect("write ready");

    let (tx, rx) = mpsc::channel::<Event>();
    transport.start_reader_thread(tx).expect("reader thread");

    let mut received = Vec::new();
    while received.len() < 3 {
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Event::ServerLine(s)) => received.push(s),
            Ok(Event::ServerDisconnected) => break,
            Err(e) => panic!("recv timeout: {e:?}"),
        }
    }
    assert_eq!(received, vec!["LINE_A".to_string(), "LINE_B".into(), "LINE_C".into()]);

    drop(transport);
    join.join().expect("server thread");
}

#[test]
fn ws_transport_empty_text_frame_treated_as_keepalive() {
    let (port, join) = spawn_mock_ws_server(|ws| {
        let _ = ws.read(); // wait for "PING"
        ws.send(Message::Text("".into())).expect("empty");
        ws.send(Message::Text("AFTER_KEEPALIVE".into())).expect("after");
    });

    let target = TransportTarget::from_host_port(&format!("ws://127.0.0.1:{port}/"), 0);
    let mut transport = CsaTransport::connect(
        &target,
        &ConnectOpts {
            tcp_keepalive: false,
            ws_origin: Some("http://localhost".to_owned()),
        },
    )
    .expect("connect");

    transport.write_line("PING").expect("write ping");
    let line = transport.read_line_blocking(Duration::from_secs(5)).expect("read");
    assert_eq!(line, "AFTER_KEEPALIVE");

    drop(transport);
    join.join().expect("server thread");
}
