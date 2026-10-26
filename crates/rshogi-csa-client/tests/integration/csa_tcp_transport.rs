//! TCP の受信と、実セッションでのサーバー確認後の棋譜保存を検証する。

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

mod live_jsonl {
    use super::*;
    use anyhow::{Result, bail};
    use rshogi_csa_client::config::CsaClientConfig;
    use rshogi_csa_client::engine::{
        BestMoveResult, InfoCallback, SearchInfo, SearchOutcome, UsiEngineDriver,
    };
    use rshogi_csa_client::events::{SessionProgress, SinkError};
    use rshogi_csa_client::jsonl::write_game_jsonl;
    use rshogi_csa_client::protocol::{CsaConnection, GameResult};
    use rshogi_csa_client::session::run_game_session_with_events;
    use serde_json::Value;
    use std::io::{BufRead, BufReader};
    use std::net::TcpStream;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, atomic::AtomicBool};

    struct Engine;

    impl UsiEngineDriver for Engine {
        fn new_game(&mut self) -> Result<()> {
            Ok(())
        }

        fn set_option(&mut self, _: &str, _: &str) -> Result<()> {
            Ok(())
        }

        fn go_with_info(
            &mut self,
            _: &str,
            _: &str,
            _: &AtomicBool,
            _: &mpsc::Receiver<Event>,
            _: &mut InfoCallback<'_>,
        ) -> Result<SearchOutcome> {
            Ok(SearchOutcome::BestMove(
                BestMoveResult {
                    bestmove: "7g7f".to_owned(),
                    ponder_move: None,
                },
                SearchInfo::default(),
            ))
        }

        fn go_ponder(&mut self, _: &str, _: &str) -> Result<()> {
            bail!("ponder is disabled")
        }

        fn ponderhit_with_info(
            &mut self,
            _: &AtomicBool,
            _: &mpsc::Receiver<Event>,
            _: &mut InfoCallback<'_>,
        ) -> Result<SearchOutcome> {
            bail!("ponder is disabled")
        }

        fn stop_and_wait(&mut self) -> Result<()> {
            Ok(())
        }

        fn gameover(&mut self, _: &str) -> Result<()> {
            Ok(())
        }
    }

    fn read_line(reader: &mut BufReader<TcpStream>) -> String {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        line.trim_end().to_owned()
    }

    fn live_entries(directory: &Path) -> (PathBuf, Vec<Value>) {
        let paths: Vec<_> = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "jsonl"))
            .collect();
        assert_eq!(paths.len(), 1);
        let text = std::fs::read_to_string(&paths[0]).unwrap();
        let rows = text.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
        (paths[0].clone(), rows)
    }

    fn terminal_after_move(echo: bool, reason: &'static str) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            stream.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            assert_eq!(read_line(&mut reader), "LOGIN alice pw");
            stream.write_all(b"LOGIN:alice OK\nBEGIN Game_Summary\nProtocol_Version:1.2\nGame_ID:g-live-confirm\nName+:alice\nName-:bob\nYour_Turn:+\nTo_Move:+\nTime_Unit:1sec\nTotal_Time:600\nByoyomi:10\nBEGIN Position\nPI\n+\nEND Position\nEND Game_Summary\n").unwrap();
            assert_eq!(read_line(&mut reader), "AGREE g-live-confirm");
            stream.write_all(b"START:g-live-confirm\n").unwrap();
            assert_eq!(read_line(&mut reader), "+7776FU");
            if echo {
                stream.write_all(b"+7776FU,T2\n").unwrap();
            }
            writeln!(stream, "{reason}\n#LOSE").unwrap();
            assert_eq!(read_line(&mut reader), "LOGOUT");
        });
        let root = std::env::temp_dir().join(format!(
            "csa-live-confirm-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut config = CsaClientConfig::default();
        config.server.floodgate = false;
        config.server.keepalive.ping_interval_sec = 0;
        config.game.ponder = false;
        config.record.dir = root.clone();
        config.record.live_jsonl = true;
        let directory = config.record.jsonl_dir().unwrap();
        let mut conn = CsaConnection::connect("127.0.0.1", port, false).unwrap();
        conn.login("alice", "pw").unwrap();
        let mut observed = Vec::new();
        let mut sink = |event: SessionProgress| -> std::result::Result<(), SinkError> {
            match event {
                SessionProgress::MoveSent(_) => {
                    observed.push(("sent", live_entries(&directory).1.len()))
                }
                SessionProgress::MoveConfirmed(_) => {
                    observed.push(("confirmed", live_entries(&directory).1.len()))
                }
                _ => {}
            }
            Ok(())
        };
        let outcome = run_game_session_with_events(
            &config,
            &mut conn,
            &mut Engine,
            Arc::new(AtomicBool::new(false)),
            &mut sink,
        )
        .unwrap();
        server.join().unwrap();
        assert!(matches!(outcome.result, GameResult::Lose));
        let expected_moves = usize::from(echo);
        assert_eq!(outcome.record.moves.len(), expected_moves);
        assert_eq!(outcome.record.jsonl_moves.len(), expected_moves);
        let (live_path, before) = live_entries(&directory);
        let path = write_game_jsonl(&directory, &outcome.record, &config, &outcome.result)
            .expect("final JSONL replaces its own live prefix");
        assert_eq!(observed.first(), Some(&("sent", 1)));
        if echo {
            assert_eq!(observed.last(), Some(&("confirmed", 2)));
            assert_eq!(outcome.record.moves[0].time_sec, 2);
        }
        assert_eq!(before.len(), expected_moves + 1);
        assert!(before.iter().all(|row| row["type"] != "result"));
        assert_eq!(path, live_path);
        let (_, rows) = live_entries(&directory);
        assert_eq!(rows.iter().filter(|row| row["type"] == "move").count(), expected_moves);
        assert_eq!(rows.last().unwrap()["type"], "result");
        assert_eq!(rows.last().unwrap()["plies"], expected_moves);
        assert_eq!(rows.last().unwrap()["outcome"], "white_win");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn time_up_before_echo_saves_result_without_the_rejected_move() {
        terminal_after_move(false, "#TIME_UP");
    }

    #[test]
    fn illegal_move_before_echo_saves_result_without_the_rejected_move() {
        terminal_after_move(false, "#ILLEGAL_MOVE");
    }

    #[test]
    fn echoed_move_is_live_before_confirmation_and_finalizes_with_result() {
        terminal_after_move(true, "#TIME_UP");
    }
}
