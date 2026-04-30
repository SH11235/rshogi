//! `run_game_session_with_events` / `run_resumed_session_with_events` の
//! end-to-end 進捗通知を mock CSA (TCP loopback) + 簡易 USI mock engine (bash 経由)
//! で確認する integration test。
//!
//! - 通常対局: `Connected → GameSummary → GameStarted → BestMoveSelected →
//!   MoveSent → MoveConfirmed → ... → GameEnded → Disconnected{GameOver}`
//! - resume: `Connected → Resumed{summary,state} → GameStarted → ...`
//!   (履歴 replay は emit しないことを確認)
//! - sink Fatal で `Disconnected{SinkAborted}` + `SessionError::SinkAborted`
//! - shutdown で `Disconnected{Shutdown}` + `SessionError::Shutdown`
//!
//! 対象 OS: Unix (bash 必須)。Windows には現状 mock USI engine 経路がないため
//! `#[cfg(unix)]` でガードする。

#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use rshogi_csa_client::config::CsaClientConfig;
use rshogi_csa_client::engine::UsiEngine;
use rshogi_csa_client::events::{
    DisconnectReason, MovePlayer, ReconnectState, SearchInfoEmitPolicy, SessionError,
    SessionEventSink, SessionProgress, SinkError,
};
use rshogi_csa_client::protocol::CsaConnection;
use rshogi_csa_client::session::{
    run_game_session, run_game_session_with_events, run_resumed_session_with_events,
};

// ────────────────────────────────────────────
// 共通: mock CSA TCP server / mock USI engine 生成
// ────────────────────────────────────────────

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
            stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
            stream.set_write_timeout(Some(Duration::from_secs(10))).ok();
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

/// 1 局分の最小フローで bestmove を 1 度だけ返す USI mock engine スクリプトを
/// 一時ファイルに書き出して path を返す。エンジンは:
///   - `usi` -> `id name mock\nusiok`
///   - `isready` -> `info string ready\nreadyok`
///   - `position ...` + `go ...` -> `info depth 5 score cp 100 nodes 1234 pv 7g7f\nbestmove 7g7f`
///   - `gameover ...` -> 何もしない
///   - `quit` -> 終了
fn mock_usi_engine_script() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = tempfile_root();
    let seq = SEQ.fetch_add(1, AtomicOrdering::SeqCst);
    let path = dir.join(format!("mock_usi_engine_{}_{}.sh", std::process::id(), seq));
    let script = r#"#!/usr/bin/env bash
# mock USI engine for csa-client integration test
while IFS= read -r line; do
    case "$line" in
        usi)
            echo "id name mock"
            echo "usiok"
            ;;
        isready)
            echo "readyok"
            ;;
        usinewgame)
            ;;
        position*)
            ;;
        go*)
            echo "info depth 5 score cp 100 nodes 1234 nps 5000 time 200 pv 7g7f"
            echo "bestmove 7g7f"
            ;;
        ponderhit)
            echo "info depth 5 score cp 100 nodes 1234 nps 5000 time 200 pv 7g7f"
            echo "bestmove 7g7f"
            ;;
        stop)
            echo "bestmove 7g7f"
            ;;
        gameover*)
            ;;
        quit)
            exit 0
            ;;
    esac
done
"#;
    std::fs::write(&path, script).expect("write mock engine script");
    let mut perms = std::fs::metadata(&path).expect("stat").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("set perms");
    path
}

fn tempfile_root() -> PathBuf {
    std::env::temp_dir()
}

/// `CsaClientConfig` を mock 用に組み立てる。
fn mock_config(engine_path: PathBuf, search_info_emit: SearchInfoEmitPolicy) -> CsaClientConfig {
    let mut config = CsaClientConfig::default();
    config.server.id = "alice".to_owned();
    config.server.password = "pw".to_owned();
    config.server.floodgate = false;
    config.server.keepalive.ping_interval_sec = 0;
    config.engine.path = engine_path;
    config.game.ponder = false;
    config.game.search_info_emit = search_info_emit;
    config
}

/// 1 局分の `Game_Summary` 行群 (平手・自分先手)。`Reconnect_Token` 拡張あり。
fn game_summary_lines(game_id: &str) -> Vec<String> {
    vec![
        "BEGIN Game_Summary".to_owned(),
        "Protocol_Version:1.2".to_owned(),
        format!("Game_ID:{}", game_id),
        "Name+:alice".to_owned(),
        "Name-:bob".to_owned(),
        "Your_Turn:+".to_owned(),
        "To_Move:+".to_owned(),
        "Time_Unit:1sec".to_owned(),
        "Total_Time:600".to_owned(),
        "Byoyomi:10".to_owned(),
        "BEGIN Position".to_owned(),
        "P1-KY-KE-GI-KI-OU-KI-GI-KE-KY".to_owned(),
        "P2 * -HI *  *  *  *  * -KA *".to_owned(),
        "P3-FU-FU-FU-FU-FU-FU-FU-FU-FU".to_owned(),
        "P4 *  *  *  *  *  *  *  *  *".to_owned(),
        "P5 *  *  *  *  *  *  *  *  *".to_owned(),
        "P6 *  *  *  *  *  *  *  *  *".to_owned(),
        "P7+FU+FU+FU+FU+FU+FU+FU+FU+FU".to_owned(),
        "P8 * +KA *  *  *  *  * +HI *".to_owned(),
        "P9+KY+KE+GI+KI+OU+KI+GI+KE+KY".to_owned(),
        "+".to_owned(),
        "END Position".to_owned(),
        "Reconnect_Token:tok-xyz".to_owned(),
        "END Game_Summary".to_owned(),
    ]
}

/// 集計用に sink から取り出す Arc 観測 handle 一式。
type CapturingHandles = (Arc<Mutex<Vec<&'static str>>>, Arc<Mutex<u32>>);

/// Sink 集計用ヘルパー
struct CapturingSink {
    events: Arc<Mutex<Vec<&'static str>>>,
    fatal_after: Option<&'static str>,
    error_calls: Arc<Mutex<u32>>,
}

impl CapturingSink {
    fn new() -> (Self, CapturingHandles) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let error_calls = Arc::new(Mutex::new(0u32));
        let me = CapturingSink {
            events: Arc::clone(&events),
            fatal_after: None,
            error_calls: Arc::clone(&error_calls),
        };
        (me, (events, error_calls))
    }
    fn fatal_on(mut self, label: &'static str) -> Self {
        self.fatal_after = Some(label);
        self
    }
}

impl SessionEventSink for CapturingSink {
    fn on_event(&mut self, event: SessionProgress) -> Result<(), SinkError> {
        let label = label_for(&event);
        self.events.lock().unwrap().push(label);
        if Some(label) == self.fatal_after {
            return Err(SinkError::Fatal(Box::new(std::io::Error::other("test fatal"))));
        }
        Ok(())
    }

    fn on_error(&mut self, _error: &SessionError) -> Result<(), SinkError> {
        *self.error_calls.lock().unwrap() += 1;
        Ok(())
    }
}

fn label_for(event: &SessionProgress) -> &'static str {
    match event {
        SessionProgress::Connected => "Connected",
        SessionProgress::GameSummary(_) => "GameSummary",
        SessionProgress::Resumed { .. } => "Resumed",
        SessionProgress::GameStarted => "GameStarted",
        SessionProgress::BestMoveSelected(_) => "BestMoveSelected",
        SessionProgress::MoveSent(_) => "MoveSent",
        SessionProgress::MoveConfirmed(e) => match e.player {
            MovePlayer::SelfPlayer => "MoveConfirmedSelf",
            MovePlayer::Opponent => "MoveConfirmedOpp",
        },
        SessionProgress::SearchInfo(_) => "SearchInfo",
        SessionProgress::GameEnded(_) => "GameEnded",
        SessionProgress::Disconnected { reason } => match reason {
            DisconnectReason::GameOver => "Disconnected:GameOver",
            DisconnectReason::Shutdown => "Disconnected:Shutdown",
            DisconnectReason::SinkAborted => "Disconnected:SinkAborted",
            DisconnectReason::TransportError(_) => "Disconnected:Transport",
            DisconnectReason::Unknown => "Disconnected:Unknown",
        },
    }
}

// ────────────────────────────────────────────
// 通常対局フローの event 順テスト
// ────────────────────────────────────────────

#[test]
fn fresh_session_emits_expected_event_sequence() {
    // 1 手指して即 #WIN を返す mock CSA サーバ
    let port = spawn_mock_tcp_server(|reader, writer| {
        // LOGIN
        let _ = read_line(reader);
        write_lines(writer, &["LOGIN:alice OK"]);
        // Game_Summary
        let lines = game_summary_lines("g-1");
        let line_refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        write_lines(writer, &line_refs);
        // AGREE 受信
        let agree = read_line(reader);
        assert!(agree.starts_with("AGREE"), "expected AGREE, got: {agree}");
        write_lines(writer, &["START:g-1"]);
        // 自分の手 +7776FU を受信
        let mv = read_line(reader);
        assert!(mv.starts_with("+7776FU"), "expected client move +7776FU, got: {mv}");
        // server echo with time
        write_lines(writer, &["+7776FU,T2"]);
        // 終局を即送る (#WIN)
        write_lines(writer, &["#WIN"]);
        // LOGOUT を受け流す (best-effort)
        let _ = read_line(reader);
    });

    let engine_path = mock_usi_engine_script();
    let config = mock_config(engine_path, SearchInfoEmitPolicy::EveryLine);
    let shutdown = Arc::new(AtomicBool::new(false));

    let mut conn = CsaConnection::connect("127.0.0.1", port, false).expect("connect");
    conn.login("alice", "pw").expect("login");

    let mut engine = UsiEngine::spawn(
        &config.engine.path,
        &config.engine.options,
        config.game.ponder,
        Duration::from_secs(5),
    )
    .expect("spawn engine");

    let (sink, (events, _err_calls)) = CapturingSink::new();
    let mut sink = sink;
    let outcome = run_game_session_with_events(
        &config,
        &mut conn,
        &mut engine,
        Arc::clone(&shutdown),
        &mut sink,
    );

    engine.quit();

    let outcome = outcome.expect("session ok");
    let events = events.lock().unwrap().clone();
    eprintln!("captured events: {events:?}");

    // 必要な event がこの順番で全部出ていること (SearchInfo は EveryLine 設定で多発し得るので順番チェック時は除外)
    let filtered: Vec<&str> = events.into_iter().filter(|e| *e != "SearchInfo").collect();
    let expected_prefix = vec![
        "Connected",
        "GameSummary",
        "GameStarted",
        "BestMoveSelected",
        "MoveSent",
        "MoveConfirmedSelf",
        "GameEnded",
        "Disconnected:GameOver",
    ];
    assert_eq!(filtered, expected_prefix, "event 順が不一致");
    assert_eq!(outcome.summary.as_ref().unwrap().game_id, "g-1");
}

// ────────────────────────────────────────────
// resume 経路で history replay が出ないことを確認
// ────────────────────────────────────────────

#[test]
fn resumed_session_emits_resumed_event_and_no_history_replay() {
    let port = spawn_mock_tcp_server(|reader, writer| {
        // LOGIN (reconnect)
        let _ = read_line(reader);
        write_lines(writer, &["LOGIN:alice OK"]);
        // Game_Summary (resume 用)
        let lines = game_summary_lines("g-resume");
        let line_refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        write_lines(writer, &line_refs);
        // Reconnect_State
        write_lines(
            writer,
            &[
                "BEGIN Reconnect_State",
                "Current_Turn:+",
                "Black_Time_Remaining_Ms:599500",
                "White_Time_Remaining_Ms:600000",
                "END Reconnect_State",
            ],
        );
        // AGREE は送られない (resume 経路)。即 自分の手番なので bestmove +7776FU を受け取る。
        let mv = read_line(reader);
        assert!(mv.starts_with("+7776FU"), "expected +7776FU, got: {mv}");
        write_lines(writer, &["+7776FU,T1"]);
        // 終局
        write_lines(writer, &["#WIN"]);
        let _ = read_line(reader);
    });

    let engine_path = mock_usi_engine_script();
    let config = mock_config(engine_path, SearchInfoEmitPolicy::Disabled);
    let shutdown = Arc::new(AtomicBool::new(false));

    let mut conn = CsaConnection::connect("127.0.0.1", port, false).expect("connect");
    conn.login_reconnect("alice", "pw", "g-resume", "tok-xyz")
        .expect("login_reconnect");

    let mut engine = UsiEngine::spawn(
        &config.engine.path,
        &config.engine.options,
        config.game.ponder,
        Duration::from_secs(5),
    )
    .expect("spawn engine");

    let (sink, (events, _err_calls)) = CapturingSink::new();
    let mut sink = sink;
    let outcome = run_resumed_session_with_events(
        &config,
        &mut conn,
        &mut engine,
        Arc::clone(&shutdown),
        &mut sink,
    );
    engine.quit();
    let _outcome = outcome.expect("session ok");

    let events = events.lock().unwrap().clone();
    eprintln!("resume events: {events:?}");

    // GameSummary は emit せず Resumed を 1 度だけ emit する
    assert!(events.contains(&"Resumed"));
    assert!(!events.contains(&"GameSummary"));
    let resumed_count = events.iter().filter(|e| **e == "Resumed").count();
    assert_eq!(resumed_count, 1);

    // 履歴 replay は無い (initial_moves が空のため、自分の手 1 つ + その echo
    // の MoveConfirmedSelf 1 つだけが期待される)
    let confirmed_count = events
        .iter()
        .filter(|e| **e == "MoveConfirmedSelf" || **e == "MoveConfirmedOpp")
        .count();
    assert_eq!(confirmed_count, 1, "履歴 replay が emit されています: {events:?}");
}

#[test]
fn resumed_state_last_sfen_matches_summary_position_section() {
    use rshogi_csa::parse_csa_full;
    let port = spawn_mock_tcp_server(|reader, writer| {
        let _ = read_line(reader);
        write_lines(writer, &["LOGIN:alice OK"]);
        let lines = game_summary_lines("g-resume-sfen");
        let line_refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        write_lines(writer, &line_refs);
        write_lines(
            writer,
            &[
                "BEGIN Reconnect_State",
                "Current_Turn:+",
                "Black_Time_Remaining_Ms:599500",
                "White_Time_Remaining_Ms:600000",
                "END Reconnect_State",
            ],
        );
        let mv = read_line(reader);
        assert!(mv.starts_with("+7776FU"));
        write_lines(writer, &["+7776FU,T1", "#WIN"]);
        let _ = read_line(reader);
    });

    let engine_path = mock_usi_engine_script();
    let config = mock_config(engine_path, SearchInfoEmitPolicy::Disabled);
    let shutdown = Arc::new(AtomicBool::new(false));
    let mut conn = CsaConnection::connect("127.0.0.1", port, false).expect("connect");
    conn.login_reconnect("alice", "pw", "g-resume-sfen", "tok-xyz")
        .expect("login_reconnect");
    let mut engine = UsiEngine::spawn(
        &config.engine.path,
        &config.engine.options,
        config.game.ponder,
        Duration::from_secs(5),
    )
    .expect("spawn engine");

    // 期待する SFEN は Game_Summary の Position section から導出する。
    // mock のレイアウトは平手のため `parse_csa_full` 結果は initial_position と等しい。
    let pos_text = lines_position_section();
    let (parsed_pos, _, _) = parse_csa_full(&pos_text).expect("parse csa");
    let expected_sfen = parsed_pos.to_sfen();

    // sink 内で Resumed payload を捕捉
    let captured_sfen: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let captured_clone = Arc::clone(&captured_sfen);
    struct CaptureResumedSink {
        out: Arc<Mutex<Option<String>>>,
    }
    impl SessionEventSink for CaptureResumedSink {
        fn on_event(&mut self, event: SessionProgress) -> Result<(), SinkError> {
            if let SessionProgress::Resumed { state, .. } = &event {
                let _: &ReconnectState = state;
                *self.out.lock().unwrap() = Some(state.last_sfen.clone());
            }
            Ok(())
        }
    }
    let mut sink = CaptureResumedSink {
        out: captured_clone,
    };
    let outcome = run_resumed_session_with_events(
        &config,
        &mut conn,
        &mut engine,
        Arc::clone(&shutdown),
        &mut sink,
    );
    engine.quit();
    let _ = outcome.expect("session ok");

    let captured = captured_sfen.lock().unwrap().clone();
    assert_eq!(
        captured.as_deref(),
        Some(expected_sfen.as_str()),
        "Resumed.state.last_sfen が GameSummary.position_section の SFEN 変換と一致すべき"
    );
}

/// Game_Summary の Position section を string で返す (テスト用)。
fn lines_position_section() -> String {
    [
        "P1-KY-KE-GI-KI-OU-KI-GI-KE-KY",
        "P2 * -HI *  *  *  *  * -KA *",
        "P3-FU-FU-FU-FU-FU-FU-FU-FU-FU",
        "P4 *  *  *  *  *  *  *  *  *",
        "P5 *  *  *  *  *  *  *  *  *",
        "P6 *  *  *  *  *  *  *  *  *",
        "P7+FU+FU+FU+FU+FU+FU+FU+FU+FU",
        "P8 * +KA *  *  *  *  * +HI *",
        "P9+KY+KE+GI+KI+OU+KI+GI+KE+KY",
        "+",
    ]
    .join("\n")
}

// ────────────────────────────────────────────
// Sink Fatal -> SinkAborted で best-effort attempt at clean closure
// ────────────────────────────────────────────

#[test]
fn fatal_sink_triggers_clean_closure_and_returns_sink_aborted() {
    let received_chudan = Arc::new(AtomicBool::new(false));
    let received_logout = Arc::new(AtomicBool::new(false));
    let received_chudan_clone = Arc::clone(&received_chudan);
    let received_logout_clone = Arc::clone(&received_logout);

    let port = spawn_mock_tcp_server(move |reader, writer| {
        let _ = read_line(reader);
        write_lines(writer, &["LOGIN:alice OK"]);
        let lines = game_summary_lines("g-fatal");
        let line_refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        write_lines(writer, &line_refs);
        // AGREE
        let _ = read_line(reader);
        write_lines(writer, &["START:g-fatal"]);
        // GameStarted で sink Fatal が発生する設定にしているため、CSA 上は AGREE/START
        // のあと client が CHUDAN + LOGOUT を送ってくるはず。タイムアウト 3 秒。
        for _ in 0..30 {
            let mut buf = String::new();
            match reader.read_line(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(_) => {
                    let trimmed = buf.trim_end_matches(['\r', '\n']);
                    if trimmed.contains("CHUDAN") {
                        received_chudan_clone.store(true, Ordering::SeqCst);
                    }
                    if trimmed.contains("LOGOUT") {
                        received_logout_clone.store(true, Ordering::SeqCst);
                    }
                    if received_logout_clone.load(Ordering::SeqCst) {
                        return;
                    }
                }
            }
        }
    });

    let engine_path = mock_usi_engine_script();
    let config = mock_config(engine_path, SearchInfoEmitPolicy::Disabled);
    let shutdown = Arc::new(AtomicBool::new(false));

    let mut conn = CsaConnection::connect("127.0.0.1", port, false).expect("connect");
    conn.login("alice", "pw").expect("login");

    let mut engine = UsiEngine::spawn(
        &config.engine.path,
        &config.engine.options,
        config.game.ponder,
        Duration::from_secs(5),
    )
    .expect("spawn engine");

    let (sink, (events, err_calls)) = CapturingSink::new();
    let mut sink = sink.fatal_on("GameStarted");

    let outcome = run_game_session_with_events(
        &config,
        &mut conn,
        &mut engine,
        Arc::clone(&shutdown),
        &mut sink,
    );

    engine.quit();

    // SessionError::SinkAborted が返ること
    match outcome {
        Err(SessionError::SinkAborted(_)) => {}
        other => panic!("expected SessionError::SinkAborted, got {other:?}"),
    }

    let events = events.lock().unwrap().clone();
    eprintln!("fatal events: {events:?}");
    assert!(events.contains(&"Disconnected:SinkAborted"));
    // on_error が 1 度呼ばれている
    assert_eq!(*err_calls.lock().unwrap(), 1, "on_error 呼び出し回数");

    // CHUDAN / LOGOUT が wire に出たか確認
    // (mock サーバスレッドが 3 秒以内に検出するはず)
    thread::sleep(Duration::from_millis(500));
    assert!(received_chudan.load(Ordering::SeqCst), "CHUDAN が送信されていません");
    assert!(received_logout.load(Ordering::SeqCst), "LOGOUT が送信されていません");
}

// ────────────────────────────────────────────
// shutdown -> SessionError::Shutdown + Disconnected{Shutdown}
// ────────────────────────────────────────────

#[test]
fn external_shutdown_emits_shutdown_disconnected_and_returns_shutdown_error() {
    let port = spawn_mock_tcp_server(|reader, writer| {
        let _ = read_line(reader);
        write_lines(writer, &["LOGIN:alice OK"]);
        let lines = game_summary_lines("g-sd");
        let line_refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        write_lines(writer, &line_refs);
        let _ = read_line(reader); // AGREE
        write_lines(writer, &["START:g-sd"]);
        // 自分の手を待たずに何も送らない。client 側で shutdown が立つと
        // `%CHUDAN` / `LOGOUT` が来るはず。
        loop {
            let mut buf = String::new();
            match reader.read_line(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(_) => {
                    let trimmed = buf.trim_end_matches(['\r', '\n']);
                    if trimmed.contains("LOGOUT") {
                        return;
                    }
                }
            }
        }
    });

    let engine_path = mock_usi_engine_script();
    let config = mock_config(engine_path, SearchInfoEmitPolicy::Disabled);
    let shutdown = Arc::new(AtomicBool::new(false));

    let mut conn = CsaConnection::connect("127.0.0.1", port, false).expect("connect");
    conn.login("alice", "pw").expect("login");

    let mut engine = UsiEngine::spawn(
        &config.engine.path,
        &config.engine.options,
        config.game.ponder,
        Duration::from_secs(5),
    )
    .expect("spawn engine");

    // 別スレッドで 200ms 後に shutdown を立てる
    let shutdown_signal = Arc::clone(&shutdown);
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(300));
        shutdown_signal.store(true, Ordering::SeqCst);
    });

    let (sink, (events, _err_calls)) = CapturingSink::new();
    let mut sink = sink;
    let outcome = run_game_session_with_events(
        &config,
        &mut conn,
        &mut engine,
        Arc::clone(&shutdown),
        &mut sink,
    );

    engine.quit();

    match outcome {
        Err(SessionError::Shutdown) => {}
        other => panic!("expected SessionError::Shutdown, got {other:?}"),
    }
    let events = events.lock().unwrap().clone();
    assert!(events.contains(&"Disconnected:Shutdown"), "events: {events:?}");
}

/// 既存 `run_game_session(&AtomicBool)` 経路でも、外部から立てた shutdown が
/// `drive_session` 内のループで観測されて `SessionError::Shutdown` で抜けることを
/// 確認する regression test。`Arc<AtomicBool>` 化に伴う snapshot 問題を起こさない
/// ための再発防止 guard。
#[test]
fn external_shutdown_observed_through_legacy_run_game_session() {
    let port = spawn_mock_tcp_server(|reader, writer| {
        let _ = read_line(reader);
        write_lines(writer, &["LOGIN:alice OK"]);
        let lines = game_summary_lines("g-legacy-sd");
        let line_refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        write_lines(writer, &line_refs);
        let _ = read_line(reader); // AGREE
        write_lines(writer, &["START:g-legacy-sd"]);
        loop {
            let mut buf = String::new();
            match reader.read_line(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(_) => {
                    let trimmed = buf.trim_end_matches(['\r', '\n']);
                    if trimmed.contains("LOGOUT") {
                        return;
                    }
                }
            }
        }
    });

    let engine_path = mock_usi_engine_script();
    let config = mock_config(engine_path, SearchInfoEmitPolicy::Disabled);
    let shutdown = Arc::new(AtomicBool::new(false));

    let mut conn = CsaConnection::connect("127.0.0.1", port, false).expect("connect");
    conn.login("alice", "pw").expect("login");

    let mut engine = UsiEngine::spawn(
        &config.engine.path,
        &config.engine.options,
        config.game.ponder,
        Duration::from_secs(5),
    )
    .expect("spawn engine");

    let shutdown_signal = Arc::clone(&shutdown);
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(300));
        shutdown_signal.store(true, Ordering::SeqCst);
    });

    // 既存 API: `&AtomicBool` (Arc 介さず参照を直接渡す)
    let outcome = run_game_session(&mut conn, &mut engine, &config, shutdown.as_ref());

    engine.quit();

    match outcome {
        Err(SessionError::Shutdown) => {}
        other => panic!("expected SessionError::Shutdown via legacy API, got {other:?}"),
    }
}

// ────────────────────────────────────────────
// NonFatal は対局継続
// ────────────────────────────────────────────

#[test]
fn nonfatal_sink_does_not_invoke_on_error_and_session_continues() {
    let port = spawn_mock_tcp_server(|reader, writer| {
        let _ = read_line(reader);
        write_lines(writer, &["LOGIN:alice OK"]);
        let lines = game_summary_lines("g-nonfatal");
        let line_refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        write_lines(writer, &line_refs);
        let _ = read_line(reader); // AGREE
        write_lines(writer, &["START:g-nonfatal"]);
        let mv = read_line(reader);
        assert!(mv.starts_with("+7776FU"));
        write_lines(writer, &["+7776FU,T1", "#WIN"]);
        let _ = read_line(reader);
    });

    let engine_path = mock_usi_engine_script();
    let config = mock_config(engine_path, SearchInfoEmitPolicy::Disabled);
    let shutdown = Arc::new(AtomicBool::new(false));

    let mut conn = CsaConnection::connect("127.0.0.1", port, false).expect("connect");
    conn.login("alice", "pw").expect("login");

    let mut engine = UsiEngine::spawn(
        &config.engine.path,
        &config.engine.options,
        config.game.ponder,
        Duration::from_secs(5),
    )
    .expect("spawn engine");

    // 全 event を NonFatal で返す sink。結果は対局完了 (Ok(SessionOutcome))
    // となり、on_error は呼ばれない。
    let error_calls = Arc::new(Mutex::new(0u32));
    let error_calls_clone = Arc::clone(&error_calls);
    struct NonFatalEverywhere {
        on_error_calls: Arc<Mutex<u32>>,
    }
    impl SessionEventSink for NonFatalEverywhere {
        fn on_event(&mut self, _event: SessionProgress) -> Result<(), SinkError> {
            Err(SinkError::NonFatal(Box::new(std::io::Error::other("transient warn"))))
        }
        fn on_error(&mut self, _err: &SessionError) -> Result<(), SinkError> {
            *self.on_error_calls.lock().unwrap() += 1;
            Ok(())
        }
    }
    let mut sink = NonFatalEverywhere {
        on_error_calls: error_calls_clone,
    };
    let outcome = run_game_session_with_events(
        &config,
        &mut conn,
        &mut engine,
        Arc::clone(&shutdown),
        &mut sink,
    );
    engine.quit();
    assert!(outcome.is_ok(), "対局は NonFatal でも完了するはず: {outcome:?}");
    assert_eq!(*error_calls.lock().unwrap(), 0, "NonFatal 時は on_error が呼ばれないこと");
}
