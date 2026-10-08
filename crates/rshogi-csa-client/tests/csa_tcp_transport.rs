//! TCP の部分行が timeout や reader thread への移動で欠落しないことを確認する。

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use rshogi_csa_client::event::Event;
use rshogi_csa_client::transport::{ConnectOpts, CsaTransport, TransportTarget};

#[test]
fn drop_transport_closes_socket_and_reader() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let target =
        TransportTarget::from_host_port("127.0.0.1", listener.local_addr().unwrap().port())
            .unwrap();
    let mut transport = CsaTransport::connect(&target, &ConnectOpts::default()).unwrap();
    let (mut server, _) = listener.accept().unwrap();
    server.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let (tx, rx) = mpsc::channel();
    transport.start_reader_thread(tx).unwrap();
    // 受信イベントで起動を確認した後、サーバーは応答も切断もしない。
    server.write_all(b"READY\n").unwrap();
    assert!(
        matches!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), Event::ServerLine(line) if line == "READY")
    );
    drop(transport);
    assert_eq!(server.read(&mut [0; 1]).unwrap(), 0, "サーバーが EOF を受信すること");
    assert!(matches!(
        rx.recv_timeout(Duration::from_secs(2)).unwrap(),
        Event::ServerDisconnected
    ));
    assert!(
        matches!(
            rx.recv_timeout(Duration::from_secs(2)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ),
        "reader が送信側を解放すること"
    );
}

fn partial_line_server() -> (CsaTransport, mpsc::Sender<()>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (release, ready) = mpsc::channel();
    let (written, received) = mpsc::channel();
    let join = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.write_all(b"LOGIN:").unwrap();
        written.send(()).unwrap();
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        stream.write_all(b"OK\r\nNEXT\n").unwrap();
    });
    let target = TransportTarget::from_host_port("127.0.0.1", port).unwrap();
    let transport = CsaTransport::connect(&target, &ConnectOpts::default()).unwrap();
    received.recv_timeout(Duration::from_secs(5)).unwrap();
    (transport, release, join)
}

#[test]
fn nonblocking_timeout_preserves_the_partial_line() {
    let (mut transport, release, join) = partial_line_server();
    assert!(transport.read_line_nonblocking().unwrap().is_none());
    release.send(()).unwrap();
    assert_eq!(transport.read_line_blocking(Duration::from_secs(5)).unwrap(), "LOGIN:OK");
    assert_eq!(transport.read_line_blocking(Duration::from_secs(5)).unwrap(), "NEXT");
    join.join().unwrap();
}

#[test]
fn blocking_deadline_preserves_the_partial_line_for_the_next_call() {
    let (mut transport, release, join) = partial_line_server();
    assert!(transport.read_line_blocking(Duration::from_millis(50)).is_err());
    release.send(()).unwrap();
    assert_eq!(transport.read_line_blocking(Duration::from_secs(5)).unwrap(), "LOGIN:OK");
    join.join().unwrap();
}

#[test]
fn reader_thread_retains_the_inline_partial_line() {
    let (mut transport, release, join) = partial_line_server();
    assert!(transport.read_line_nonblocking().unwrap().is_none());
    let (tx, rx) = mpsc::channel();
    transport.start_reader_thread(tx).unwrap();
    release.send(()).unwrap();
    assert!(
        matches!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), Event::ServerLine(line) if line == "LOGIN:OK")
    );
    assert!(
        matches!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), Event::ServerLine(line) if line == "NEXT")
    );
    assert!(matches!(
        rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        Event::ServerDisconnected
    ));
    join.join().unwrap();
}

#[test]
fn oversized_unterminated_line_is_rejected() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let join = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.write_all(&vec![b'x'; 64 * 1024 + 1]).unwrap();
    });
    let target = TransportTarget::from_host_port("127.0.0.1", port).unwrap();
    let mut transport = CsaTransport::connect(&target, &ConnectOpts::default()).unwrap();
    let error = transport.read_line_blocking(Duration::from_secs(5)).unwrap_err();
    assert!(error.to_string().contains("64 KiB"));
    join.join().unwrap();
}
