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
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use rshogi_csa_client::config::CsaClientConfig;
use rshogi_csa_client::engine::SpawnOptions;

mod common;
use rshogi_csa::{Color, Position, initial_position};
use rshogi_csa_client::events::{
    DisconnectReason, MovePlayer, SearchInfoEmitPolicy, SessionError, SessionEventSink,
    SessionProgress, SinkError,
};
use rshogi_csa_client::jsonl::LiveJsonlWriter;
use rshogi_csa_client::protocol::{CsaConnection, GameSummary, TimeConfig};
use rshogi_csa_client::record::{GameRecord, JsonlMoveExtra, RecordStatus};
use rshogi_csa_client::session::{
    run_game_session, run_game_session_with_events, run_resumed_session_with_events,
    run_resumed_session_with_record,
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
    let script = r#"#!/usr/bin/env bash
# mock USI engine for csa-client integration test
go_count=0
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
            # 2 回目以降の go には応答しない: 即 bestmove を返すと終局通知の
            # 処理とレースし、echo 待ち前の局面適用・record/live JSONL 追記が
            # 走り得る。client は終局検出で stop を送るので、bestmove は下の
            # stop への応答としてのみ返し、読み捨てさせる。
            go_count=$((go_count + 1))
            if [ "$go_count" -eq 1 ]; then
                echo "info depth 5 score cp 100 nodes 1234 nps 5000 time 200 pv 7g7f"
                echo "bestmove 7g7f"
            fi
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
    common::write_mock_script("mock_usi_engine", script)
}

/// ponder miss 後の次 go で info を出さず即 bestmove を返す mock。
/// stop への応答は stale info → bestmove の順で書く: client の読み捨ては
/// 「bestmove がその go の最終出力」という出力契約に依拠しており、bestmove の
/// 後に書くと読み捨てとレースする。
/// ponderhit はこのシナリオでは到達しない想定（到達したら exit 1 でテストを落とす）。
fn mock_usi_engine_stale_ponder_info_script() -> PathBuf {
    let script = r#"#!/usr/bin/env bash
go_count=0
while IFS= read -r line; do
    case "$line" in
        usi)
            echo "id name mock-stale-ponder-info"
            echo "usiok"
            ;;
        isready)
            echo "readyok"
            ;;
        usinewgame)
            ;;
        position*)
            ;;
        "go ponder"*)
            ;;
        gameover*)
            ;;
        go*)
            go_count=$((go_count + 1))
            if [ "$go_count" -eq 1 ]; then
                echo "info depth 5 score cp 100 nodes 1234 nps 5000 time 200 pv 7g7f"
                echo "bestmove 7g7f ponder 3c3d"
            else
                echo "bestmove 2g2f"
            fi
            ;;
        ponderhit)
            exit 1
            ;;
        stop)
            echo "info depth 1 score cp -32001 nodes 0 pv 1c1d"
            echo "bestmove 3c3d"
            ;;
        quit)
            exit 0
            ;;
    esac
done
"#;
    common::write_mock_script("mock_usi_engine_stale_ponder_info", script)
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
    game_summary_lines_for_position(game_id, &initial_position())
}

/// resume 用に、指定した現在局面を Position block に入れた `Game_Summary` を作る。
fn game_summary_lines_for_position(game_id: &str, position: &Position) -> Vec<String> {
    let turn = match position.side_to_move {
        Color::Black => "+",
        Color::White => "-",
    };
    let mut lines = vec![
        "BEGIN Game_Summary".to_owned(),
        "Protocol_Version:1.2".to_owned(),
        format!("Game_ID:{}", game_id),
        "Name+:alice".to_owned(),
        "Name-:bob".to_owned(),
        "Your_Turn:+".to_owned(),
        format!("To_Move:{turn}"),
        "Time_Unit:1sec".to_owned(),
        "Total_Time:600".to_owned(),
        "Byoyomi:10".to_owned(),
        "BEGIN Position".to_owned(),
    ];
    lines.extend(position.to_csa_board().lines().map(str::to_owned));
    lines.extend([
        "END Position".to_owned(),
        "Reconnect_Token:tok-xyz".to_owned(),
        "END Game_Summary".to_owned(),
    ]);
    lines
}

/// 集計用に sink から取り出す Arc 観測 handle 一式。
type CapturingHandles = (Arc<Mutex<Vec<&'static str>>>, Arc<Mutex<u32>>);

/// Sink 集計用ヘルパー
struct CapturingSink {
    resumed_sfen: Option<String>,
    events: Arc<Mutex<Vec<&'static str>>>,
    fatal_after: Option<&'static str>,
    error_calls: Arc<Mutex<u32>>,
}

impl CapturingSink {
    fn new() -> (Self, CapturingHandles) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let error_calls = Arc::new(Mutex::new(0u32));
        let me = CapturingSink {
            resumed_sfen: None,
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
        if let SessionProgress::Resumed { state, .. } = &event {
            self.resumed_sfen = Some(state.last_sfen.clone());
        }

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

    let mut engine = common::spawn_engine(
        &config.engine.path,
        &config.engine.options,
        SpawnOptions {
            ponder: config.game.ponder,
            startup_timeout: Duration::from_secs(5),
            stderr_passthrough: false,
        },
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

#[test]
fn ponder_miss_stale_info_does_not_attach_to_next_instant_bestmove() {
    let second_move_line = Arc::new(Mutex::new(String::new()));
    let observed_second_move = Arc::clone(&second_move_line);
    let port = spawn_mock_tcp_server(move |reader, writer| {
        let _ = read_line(reader);
        write_lines(writer, &["LOGIN:alice OK"]);
        let lines = game_summary_lines("g-stale-ponder-info");
        let line_refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        write_lines(writer, &line_refs);
        let agree = read_line(reader);
        assert!(agree.starts_with("AGREE"), "expected AGREE, got: {agree}");
        write_lines(writer, &["START:g-stale-ponder-info"]);

        let first = read_line(reader);
        assert!(first.starts_with("+7776FU"), "expected first client move +7776FU, got: {first}");
        write_lines(writer, &["+7776FU,T1"]);

        // ponder 予測 (3c3d) と異なる手を指して ponder miss を起こす
        write_lines(writer, &["-8384FU,T1"]);
        let second = read_line(reader);
        *observed_second_move.lock().unwrap() = second.clone();
        assert!(
            second.starts_with("+2726FU"),
            "expected second client move +2726FU, got: {second}"
        );
        assert!(
            !second.contains(",'"),
            "book-like instant move must not include floodgate eval comment: {second}"
        );
        write_lines(writer, &["+2726FU,T1"]);
        write_lines(writer, &["#WIN"]);
        let _ = read_line(reader);
    });

    let engine_path = mock_usi_engine_stale_ponder_info_script();
    let mut config = mock_config(engine_path, SearchInfoEmitPolicy::EveryLine);
    config.game.ponder = true;
    config.server.floodgate = true;
    let shutdown = Arc::new(AtomicBool::new(false));

    let mut conn = CsaConnection::connect("127.0.0.1", port, false).expect("connect");
    conn.login("alice", "pw").expect("login");
    let mut engine = common::spawn_engine(
        &config.engine.path,
        &config.engine.options,
        SpawnOptions {
            ponder: config.game.ponder,
            startup_timeout: Duration::from_secs(5),
            stderr_passthrough: false,
        },
    )
    .expect("spawn engine");

    let (sink, _) = CapturingSink::new();
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

    let moves = &outcome.record.moves;
    assert_eq!(moves.len(), 3);
    assert_eq!(moves[2].csa_move, "+2726FU");
    assert_eq!(moves[2].eval_cp, None);
    assert_eq!(moves[2].eval_mate, None);
    assert_eq!(moves[2].depth, None);
    assert!(moves[2].pv.is_empty());
    assert!(!second_move_line.lock().unwrap().contains(",'"));
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

    let mut engine = common::spawn_engine(
        &config.engine.path,
        &config.engine.options,
        SpawnOptions {
            ponder: config.game.ponder,
            startup_timeout: Duration::from_secs(5),
            stderr_passthrough: false,
        },
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
    assert_eq!(sink.resumed_sfen, Some(initial_position().to_sfen()));
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
fn resumed_session_keeps_pre_disconnect_record_and_start_time() {
    let initial = initial_position();
    let retained_summary = GameSummary {
        game_id: "g-resume-record".to_owned(),
        my_color: Color::Black,
        sente_name: "alice".to_owned(),
        gote_name: "bob".to_owned(),
        position: initial.clone(),
        initial_moves: Vec::new(),
        black_time: TimeConfig {
            total_time_ms: 600_000,
            byoyomi_ms: 10_000,
            increment_ms: 0,
        },
        white_time: TimeConfig {
            total_time_ms: 600_000,
            byoyomi_ms: 10_000,
            increment_ms: 0,
        },
        reconnect_token: Some("tok-xyz".to_owned()),
        entering_king_rule: None,
    };
    let mut retained = GameRecord::new(&retained_summary);
    let original_start_time = retained.start_time;
    let mut current = initial;
    for (csa, usi, side) in [
        ("+2726FU", "2g2f", Color::Black),
        ("-8384FU", "8c8d", Color::White),
    ] {
        let sfen_before = current.to_sfen();
        retained.add_move(csa, 1, None, side);
        retained.add_jsonl_move(JsonlMoveExtra {
            sfen_before,
            move_usi: usi.to_owned(),
            engine_label: if side == Color::Black { "alice" } else { "bob" }.to_owned(),
            elapsed_ms: 10,
            think_limit_ms: 1000,
            seldepth: None,
            nodes: None,
            time_ms: None,
            nps: None,
        });
        current.apply_csa_move(csa).unwrap();
    }
    let resume_lines = game_summary_lines_for_position("g-resume-record", &current);

    let port = spawn_mock_tcp_server(move |reader, writer| {
        let _ = read_line(reader);
        write_lines(writer, &["LOGIN:alice OK"]);
        let line_refs: Vec<&str> = resume_lines.iter().map(String::as_str).collect();
        write_lines(writer, &line_refs);
        write_lines(
            writer,
            &[
                "BEGIN Reconnect_State",
                "Current_Turn:+",
                "Black_Time_Remaining_Ms:599500",
                "White_Time_Remaining_Ms:599000",
                "END Reconnect_State",
            ],
        );
        let mv = read_line(reader);
        assert!(mv.starts_with("+7776FU"), "expected +7776FU, got: {mv}");
        write_lines(writer, &["+7776FU,T1", "#WIN"]);
        let _ = read_line(reader);
    });

    let engine_path = mock_usi_engine_script();
    let config = mock_config(engine_path, SearchInfoEmitPolicy::Disabled);
    let shutdown = Arc::new(AtomicBool::new(false));
    let mut conn = CsaConnection::connect("127.0.0.1", port, false).expect("connect");
    conn.login_reconnect("alice", "pw", "g-resume-record", "tok-xyz")
        .expect("login_reconnect");
    let mut engine = common::spawn_engine(
        &config.engine.path,
        &config.engine.options,
        SpawnOptions {
            ponder: config.game.ponder,
            startup_timeout: Duration::from_secs(5),
            stderr_passthrough: false,
        },
    )
    .expect("spawn engine");
    let outcome = run_resumed_session_with_record(
        &mut conn,
        &mut engine,
        &config,
        shutdown.as_ref(),
        retained,
    )
    .expect("resumed session");
    engine.quit();

    assert_eq!(outcome.outcome.record.start_time, original_start_time);
    assert_eq!(outcome.outcome.record.moves.len(), 3);
    assert_eq!(outcome.outcome.record.jsonl_moves.len(), 3);
    assert_eq!(outcome.outcome.record.moves[0].csa_move, "+2726FU");
    assert_eq!(outcome.outcome.record.moves[1].csa_move, "-8384FU");
    assert_eq!(outcome.outcome.record.moves[2].csa_move, "+7776FU");
    assert_eq!(outcome.status, RecordStatus::Complete);
}

#[test]
fn resumed_session_mismatch_keeps_start_time_and_removes_stale_live_jsonl() {
    let initial = initial_position();
    let retained_summary = GameSummary {
        game_id: "g-resume-fragment".to_owned(),
        my_color: Color::Black,
        sente_name: "alice".to_owned(),
        gote_name: "bob".to_owned(),
        position: initial.clone(),
        initial_moves: Vec::new(),
        black_time: TimeConfig::default(),
        white_time: TimeConfig::default(),
        reconnect_token: Some("tok-xyz".to_owned()),
        entering_king_rule: None,
    };
    let mut retained = GameRecord::new(&retained_summary);
    let original_start_time = retained.start_time;
    retained.add_move("+2726FU", 1, None, Color::Black);
    retained.add_jsonl_move(JsonlMoveExtra {
        sfen_before: initial.to_sfen(),
        move_usi: "2g2f".to_owned(),
        engine_label: "alice".to_owned(),
        elapsed_ms: 10,
        think_limit_ms: 1000,
        seldepth: None,
        nodes: None,
        time_ms: None,
        nps: None,
    });

    let port = spawn_mock_tcp_server(|reader, writer| {
        let _ = read_line(reader);
        write_lines(writer, &["LOGIN:alice OK"]);
        let resume_lines = game_summary_lines("g-resume-fragment");
        let line_refs: Vec<&str> = resume_lines.iter().map(String::as_str).collect();
        write_lines(writer, &line_refs);
        write_lines(
            writer,
            &[
                "BEGIN Reconnect_State",
                "Current_Turn:+",
                "Black_Time_Remaining_Ms:599500",
                "White_Time_Remaining_Ms:599000",
                "END Reconnect_State",
            ],
        );
        let mv = read_line(reader);
        assert!(mv.starts_with("+7776FU"), "expected +7776FU, got: {mv}");
        write_lines(writer, &["+7776FU,T1", "#WIN"]);
        let _ = read_line(reader);
    });

    let engine_path = mock_usi_engine_script();
    let mut config = mock_config(engine_path, SearchInfoEmitPolicy::Disabled);
    let jsonl_dir =
        tempfile_root().join(format!("csa_resume_fragment_live_jsonl_e2e_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&jsonl_dir);
    config.record.jsonl_out = Some(jsonl_dir.clone());
    config.record.live_jsonl = true;
    // 切断前 session が残した通常名 live JSONL を再現する。
    let stale_writer = LiveJsonlWriter::create(&jsonl_dir, &retained, &config).unwrap();
    drop(stale_writer);
    assert_eq!(std::fs::read_dir(&jsonl_dir).unwrap().count(), 1);

    let shutdown = Arc::new(AtomicBool::new(false));
    let mut conn = CsaConnection::connect("127.0.0.1", port, false).expect("connect");
    conn.login_reconnect("alice", "pw", "g-resume-fragment", "tok-xyz")
        .expect("login_reconnect");
    let mut engine = common::spawn_engine(
        &config.engine.path,
        &config.engine.options,
        SpawnOptions {
            ponder: config.game.ponder,
            startup_timeout: Duration::from_secs(5),
            stderr_passthrough: false,
        },
    )
    .expect("spawn engine");
    let outcome = run_resumed_session_with_record(
        &mut conn,
        &mut engine,
        &config,
        shutdown.as_ref(),
        retained,
    )
    .expect("resumed session");
    engine.quit();

    assert_eq!(outcome.outcome.record.start_time, original_start_time);
    assert_eq!(outcome.outcome.record.moves.len(), 1);
    let RecordStatus::ReconnectFragment { reason } = &outcome.status else {
        panic!("局面不一致が fragment 扱いになっていません");
    };
    assert!(reason.contains("局面不一致"), "reason={reason}");
    let paths: Vec<_> = std::fs::read_dir(&jsonl_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(paths.len(), 1, "通常名 live JSONL が残っています: {paths:?}");
    let filename = paths[0].file_name().unwrap().to_string_lossy();
    assert!(filename.contains("_reconnect_fragment.jsonl"), "filename={filename}");
    assert!(
        filename.starts_with(&original_start_time.format("%Y%m%d_%H%M%S").to_string()),
        "filename={filename}"
    );
    let _ = std::fs::remove_dir_all(&jsonl_dir);
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

    let mut engine = common::spawn_engine(
        &config.engine.path,
        &config.engine.options,
        SpawnOptions {
            ponder: config.game.ponder,
            startup_timeout: Duration::from_secs(5),
            stderr_passthrough: false,
        },
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

    let mut engine = common::spawn_engine(
        &config.engine.path,
        &config.engine.options,
        SpawnOptions {
            ponder: config.game.ponder,
            startup_timeout: Duration::from_secs(5),
            stderr_passthrough: false,
        },
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

    let mut engine = common::spawn_engine(
        &config.engine.path,
        &config.engine.options,
        SpawnOptions {
            ponder: config.game.ponder,
            startup_timeout: Duration::from_secs(5),
            stderr_passthrough: false,
        },
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

    let mut engine = common::spawn_engine(
        &config.engine.path,
        &config.engine.options,
        SpawnOptions {
            ponder: config.game.ponder,
            startup_timeout: Duration::from_secs(5),
            stderr_passthrough: false,
        },
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

// ────────────────────────────────────────────
// live JSONL (`[record] live_jsonl`) の対局中追記
// ────────────────────────────────────────────

/// jsonl_dir 内の唯一の `.jsonl` の行数を返す (無ければ 0)。
fn live_jsonl_line_count(dir: &std::path::Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("jsonl"))
        .map(|e| std::fs::read_to_string(e.path()).map(|t| t.lines().count()).unwrap_or(0))
        .sum()
}

/// MoveConfirmed のたびに live JSONL の行数を観測する sink。
struct LiveWatchSink {
    jsonl_dir: PathBuf,
    observed: Arc<Mutex<Vec<(&'static str, usize)>>>,
}

impl SessionEventSink for LiveWatchSink {
    fn on_event(&mut self, event: SessionProgress) -> Result<(), SinkError> {
        let label = label_for(&event);
        if label.starts_with("MoveConfirmed") {
            self.observed
                .lock()
                .unwrap()
                .push((label, live_jsonl_line_count(&self.jsonl_dir)));
        }
        Ok(())
    }

    fn on_error(&mut self, _error: &SessionError) -> Result<(), SinkError> {
        Ok(())
    }
}

#[test]
fn live_jsonl_grows_during_game() {
    // 自分 +7776FU → 相手 -3334FU → #WIN の最小 2 手対局。
    let port = spawn_mock_tcp_server(|reader, writer| {
        let _ = read_line(reader);
        write_lines(writer, &["LOGIN:alice OK"]);
        let lines = game_summary_lines("g-live");
        let line_refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        write_lines(writer, &line_refs);
        let agree = read_line(reader);
        assert!(agree.starts_with("AGREE"), "expected AGREE, got: {agree}");
        write_lines(writer, &["START:g-live"]);
        let mv = read_line(reader);
        assert!(mv.starts_with("+7776FU"), "expected +7776FU, got: {mv}");
        write_lines(writer, &["+7776FU,T2"]);
        write_lines(writer, &["-3334FU,T1"]);
        write_lines(writer, &["#WIN"]);
        let _ = read_line(reader);
    });

    let record_root = tempfile_root().join(format!("csa_live_jsonl_e2e_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&record_root);
    let engine_path = mock_usi_engine_script();
    let mut config = mock_config(engine_path, SearchInfoEmitPolicy::EveryLine);
    config.record.enabled = true;
    config.record.save_jsonl = true;
    config.record.live_jsonl = true;
    config.record.dir = record_root.clone();
    let jsonl_dir = config.record.jsonl_dir().expect("jsonl_dir");

    let shutdown = Arc::new(AtomicBool::new(false));
    let mut conn = CsaConnection::connect("127.0.0.1", port, false).expect("connect");
    conn.login("alice", "pw").expect("login");
    let mut engine = common::spawn_engine(
        &config.engine.path,
        &config.engine.options,
        SpawnOptions {
            ponder: config.game.ponder,
            startup_timeout: Duration::from_secs(5),
            stderr_passthrough: false,
        },
    )
    .expect("spawn engine");

    let observed = Arc::new(Mutex::new(Vec::new()));
    let mut sink = LiveWatchSink {
        jsonl_dir: jsonl_dir.clone(),
        observed: Arc::clone(&observed),
    };
    let outcome = run_game_session_with_events(
        &config,
        &mut conn,
        &mut engine,
        Arc::clone(&shutdown),
        &mut sink,
    );
    engine.quit();
    let outcome = outcome.expect("session outcome");

    // 対局中の観測: 自手確定時点で meta+1 手、相手手確定時点で meta+2 手が
    // 既にディスク上で読める (= 手単位のリアルタイム追記)。
    let observed = observed.lock().unwrap();
    assert_eq!(
        observed.as_slice(),
        &[("MoveConfirmedSelf", 2), ("MoveConfirmedOpp", 3)],
        "observed: {observed:?}"
    );

    // セッション終了直後 (main の write_game_jsonl 前) の live ファイルは
    // meta + move x2 のまま result 行を持たない。
    assert_eq!(live_jsonl_line_count(&jsonl_dir), 3);
    // main 相当の最終書き出しで canonical (result 行込み) に置き換わる。
    let path = rshogi_csa_client::jsonl::write_game_jsonl(
        &jsonl_dir,
        &outcome.record,
        &config,
        &outcome.result,
    )
    .expect("final write");
    let text = std::fs::read_to_string(&path).expect("read final");
    assert_eq!(text.lines().count(), 4);
    assert!(text.lines().last().unwrap().contains("\"type\":\"result\""));
    let _ = std::fs::remove_dir_all(&record_root);
}
