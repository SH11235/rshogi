//! CSA 1.2.1 の対局不成立・先後別時計を、実ソケットと in-process engine で検証する。
use anyhow::Result;
use rshogi_csa_client::config::CsaClientConfig;
use rshogi_csa_client::engine::{
    BestMoveResult, InfoCallback, SearchInfo, SearchOutcome, UsiEngineDriver,
};
use rshogi_csa_client::event::Event;
use rshogi_csa_client::protocol::{CsaConnection, StartResponse};
use rshogi_csa_client::session::run_game_session_with_events;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, atomic::AtomicBool, mpsc::Receiver};
use std::thread;
use std::time::Duration;

fn read(reader: &mut BufReader<TcpStream>) -> String {
    let mut line = String::new();
    assert!(reader.read_line(&mut line).unwrap() > 0);
    line.trim().to_owned()
}
fn send(writer: &mut TcpStream, lines: &[&str]) {
    for line in lines {
        writeln!(writer, "{line}").unwrap();
    }
    writer.flush().unwrap();
}
fn server(
    f: impl FnOnce(&mut BufReader<TcpStream>, &mut TcpStream) + Send + 'static,
) -> (u16, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        f(&mut reader, &mut stream);
    });
    (port, handle)
}
fn summary(
    writer: &mut TcpStream,
    color: &str,
    unit: &str,
    black: &str,
    white: &str,
    increment: &str,
    moves: &[&str],
) {
    send(
        writer,
        &[
            "BEGIN Game_Summary",
            "Protocol_Version:1.2.1",
            "Format:Shogi 1.0",
            "Game_ID:g",
            "Name+:b",
            "Name-:w",
        ],
    );
    send(
        writer,
        &[
            &format!("Your_Turn:{color}"),
            "Declaration:Jishogi 1.1",
            "Max_Moves:512",
            "BEGIN Time+",
            &format!("Time_Unit:{unit}"),
            &format!("Total_Time:{black}"),
            &format!("Increment:{increment}"),
            "END Time+",
            "BEGIN Time-",
            &format!("Time_Unit:{unit}"),
            &format!("Total_Time:{white}"),
            &format!("Increment:{increment}"),
            "END Time-",
            "BEGIN Position",
            "PI",
            "+",
        ],
    );
    send(writer, moves);
    send(writer, &["END Position", "END Game_Summary"]);
}
#[derive(Default)]
struct Engine {
    go: Vec<String>,
    new_games: usize,
    options: Vec<(String, String)>,
    declare: bool,
    first_move: Option<String>,
}
impl UsiEngineDriver for Engine {
    fn new_game(&mut self) -> Result<()> {
        self.new_games += 1;
        Ok(())
    }
    fn set_option(&mut self, name: &str, value: &str) -> Result<()> {
        self.options.push((name.into(), value.into()));
        Ok(())
    }
    fn go_with_info(
        &mut self,
        _: &str,
        go: &str,
        _: &AtomicBool,
        _: &Receiver<Event>,
        _: &mut InfoCallback<'_>,
    ) -> Result<SearchOutcome> {
        self.go.push(go.into());
        Ok(SearchOutcome::BestMove(
            BestMoveResult {
                bestmove: self
                    .first_move
                    .take()
                    .unwrap_or_else(|| if self.declare { "win" } else { "resign" }.into()),
                ponder_move: None,
            },
            SearchInfo::default(),
        ))
    }
    fn go_ponder(&mut self, _: &str, _: &str) -> Result<()> {
        unreachable!()
    }
    fn ponderhit_with_info(
        &mut self,
        _: &AtomicBool,
        _: &Receiver<Event>,
        _: &mut InfoCallback<'_>,
    ) -> Result<SearchOutcome> {
        unreachable!()
    }
    fn stop_and_wait(&mut self) -> Result<()> {
        Ok(())
    }
    fn gameover(&mut self, _: &str) -> Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct CandidateSink {
    summaries: usize,
    starts: usize,
}
impl rshogi_csa_client::events::SessionEventSink for CandidateSink {
    fn on_event(
        &mut self,
        event: rshogi_csa_client::events::SessionProgress,
    ) -> std::result::Result<(), rshogi_csa_client::events::SinkError> {
        use rshogi_csa_client::events::SessionProgress;
        match event {
            SessionProgress::GameSummary(_) => self.summaries += 1,
            SessionProgress::GameStarted => self.starts += 1,
            _ => {}
        }
        Ok(())
    }
}
#[test]
fn reject_then_tournament_game_on_same_connection_both_colors() {
    for color in ["+", "-"] {
        let (port, handle) = server(move |reader, writer| {
            summary(writer, color, "1sec", "600", "600", "2", &[]);
            assert_eq!(read(reader), "AGREE g");
            send(writer, &["REJECT:g by opponent"]);
            summary(
                writer,
                color,
                "1sec",
                "600",
                "600",
                "2",
                &["+7776FU,T212", "-3334FU,T2", "+2726FU,T212", "-8384FU,T2"],
            );
            assert_eq!(read(reader), "AGREE g");
            send(writer, &["START:g"]);
            if color == "-" {
                send(writer, &["+2625FU,T1"]);
            }
            assert_eq!(read(reader), if color == "+" { "%TORYO" } else { "%KACHI" });
            send(writer, &["#MAX_MOVES", "#DRAW"]);
            assert_eq!(read(reader), "LOGOUT");
        });
        let mut conn = CsaConnection::connect("127.0.0.1", port, false).unwrap();
        let mut config = CsaClientConfig::default();
        config.game.ponder = false;
        config.time.margin_msec = 0;
        config.record.enabled = false;
        let mut engine = Engine {
            declare: color == "-",
            ..Engine::default()
        };
        let mut sink = CandidateSink::default();
        run_game_session_with_events(
            &config,
            &mut conn,
            &mut engine,
            Arc::new(AtomicBool::new(false)),
            &mut sink,
        )
        .unwrap();
        assert_eq!(engine.new_games, 1);
        assert_eq!((sink.summaries, sink.starts), (2, 1));
        assert_eq!(engine.options, [("EnteringKingRule".into(), "CSARule27".into())]);
        assert_eq!(
            engine.go,
            [if color == "+" {
                "go btime 180000 wtime 600000 binc 2000 winc 2000"
            } else {
                "go btime 181000 wtime 600000 binc 2000 winc 2000"
            }]
        );
        handle.join().unwrap();
    }
}
#[test]
fn minute_and_millisecond_initial_moves_reach_usi_in_ms() {
    for (unit, black, white, increment, moves, expected) in [
        (
            "1min",
            "10",
            "20",
            "1",
            vec!["+7776FU,T2", "-3334FU,T3"],
            "go btime 540000 wtime 1080000 binc 60000 winc 60000",
        ),
        (
            "1msec",
            "10000",
            "20000",
            "500",
            vec!["+7776FU,T1250", "-3334FU,T750"],
            "go btime 9250 wtime 19750 binc 500 winc 500",
        ),
    ] {
        let (port, handle) = server(move |reader, writer| {
            summary(writer, "+", unit, black, white, increment, &moves);
            assert_eq!(read(reader), "AGREE g");
            send(writer, &["START:g"]);
            assert_eq!(read(reader), "%TORYO");
            send(writer, &["#RESIGN", "#LOSE"]);
            assert_eq!(read(reader), "LOGOUT");
        });
        let mut conn = CsaConnection::connect("127.0.0.1", port, false).unwrap();
        let mut config = CsaClientConfig::default();
        config.game.ponder = false;
        config.time.margin_msec = 0;
        config.record.enabled = false;
        let mut engine = Engine::default();
        run_game_session_with_events(
            &config,
            &mut conn,
            &mut engine,
            Arc::new(AtomicBool::new(false)),
            &mut rshogi_csa_client::events::NoopSessionEventSink,
        )
        .unwrap();
        assert_eq!(engine.go, [expected]);
        handle.join().unwrap();
    }
}
#[test]
fn start_wait_keeps_connection_alive_and_can_be_cancelled() {
    let (port, handle) = server(|reader, writer| {
        assert_eq!(read(reader), "AGREE g");
        assert_eq!(read(reader), "");
        send(writer, &["START:g"]);
    });
    let mut conn = CsaConnection::connect("127.0.0.1", port, false).unwrap();
    assert_eq!(
        conn.agree_and_wait_start("g", 1, || true).unwrap(),
        StartResponse::Started("g".into())
    );
    handle.join().unwrap();
    let (port, handle) = server(|reader, _| {
        let mut line = String::new();
        assert_eq!(reader.read_line(&mut line).unwrap(), 0);
    });
    let mut conn = CsaConnection::connect("127.0.0.1", port, false).unwrap();
    assert_eq!(conn.agree_and_wait_start("", 0, || false).unwrap(), StartResponse::Cancelled);
    drop(conn);
    handle.join().unwrap();
}

#[test]
fn mixed_units_and_reordered_fields_apply_live_echo_and_opponent_move() {
    let (port, handle) = server(|reader, writer| {
        send(
            writer,
            &[
                "BEGIN Game_Summary",
                "Game_ID:g",
                "Name+:b",
                "Name-:w",
                "Your_Turn:+",
                "BEGIN Time+",
                "Total_Time:2",
                "Increment:1",
                "Time_Unit:1min",
                "END Time+",
                "BEGIN Time-",
                "Total_Time:10000",
                "Increment:500",
                "Time_Unit:1msec",
                "END Time-",
                "BEGIN Position",
                "PI",
                "+",
                "+7776FU,T1",
                "-3334FU,T250",
                "END Position",
                "END Game_Summary",
            ],
        );
        assert_eq!(read(reader), "AGREE g");
        send(writer, &["START:g"]);
        assert_eq!(read(reader), "+2726FU");
        send(writer, &["+2726FU,T1", "-8384FU,T1250"]);
        assert_eq!(read(reader), "%TORYO");
        send(writer, &["#RESIGN", "#LOSE"]);
        assert_eq!(read(reader), "LOGOUT");
    });
    let mut conn = CsaConnection::connect("127.0.0.1", port, false).unwrap();
    let mut config = CsaClientConfig::default();
    config.game.ponder = false;
    config.server.floodgate = false;
    config.time.margin_msec = 0;
    config.record.enabled = false;
    let mut engine = Engine {
        first_move: Some("2g2f".into()),
        ..Engine::default()
    };
    let outcome = run_game_session_with_events(
        &config,
        &mut conn,
        &mut engine,
        Arc::new(AtomicBool::new(false)),
        &mut rshogi_csa_client::events::NoopSessionEventSink,
    )
    .unwrap();
    assert_eq!(
        engine.go,
        [
            "go btime 120000 wtime 10250 binc 60000 winc 500",
            "go btime 120000 wtime 9500 binc 60000 winc 500"
        ]
    );
    assert_eq!(
        outcome.record.moves.iter().map(|mv| mv.time_sec).collect::<Vec<_>>(),
        [60, 0, 60, 1]
    );
    assert_eq!(outcome.record.jsonl_moves[1].elapsed_ms, 250);
    assert_eq!(outcome.record.jsonl_moves[3].elapsed_ms, 1250);
    handle.join().unwrap();
}

#[test]
fn negotiation_honors_shutdown_without_resigning() {
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_server = Arc::clone(&shutdown);
    let (port, handle) = server(move |reader, writer| {
        summary(writer, "+", "1sec", "180", "600", "2", &[]);
        assert_eq!(read(reader), "AGREE g");
        shutdown_server.store(true, std::sync::atomic::Ordering::SeqCst);
        send(writer, &["REJECT:g"]);
        assert_eq!(read(reader), "LOGOUT");
    });
    let mut conn = CsaConnection::connect("127.0.0.1", port, false).unwrap();
    let mut engine = Engine::default();
    let result = run_game_session_with_events(
        &CsaClientConfig::default(),
        &mut conn,
        &mut engine,
        shutdown,
        &mut rshogi_csa_client::events::NoopSessionEventSink,
    );
    assert!(matches!(result, Err(rshogi_csa_client::events::SessionError::Shutdown)));
    assert_eq!(engine.new_games, 0);
    handle.join().unwrap();
}

#[test]
fn max_moves_after_512th_move_finishes_without_resigning() {
    let (port, handle) = server(|reader, writer| {
        let mut moves = Vec::new();
        for _ in 0..127 {
            moves.extend(["+5958OU,T0", "-5152OU,T0", "+5859OU,T0", "-5251OU,T0"]);
        }
        moves.extend(["+5958OU,T0", "-5152OU,T0", "+5859OU,T0"]);
        summary(writer, "-", "1sec", "180", "600", "0", &moves);
        assert_eq!(read(reader), "AGREE g");
        send(writer, &["START:g"]);
        assert_eq!(read(reader), "-5251OU");
        send(writer, &["-5251OU,T1", "#MAX_MOVES", "#DRAW"]);
        assert_eq!(read(reader), "LOGOUT");
    });
    let mut conn = CsaConnection::connect("127.0.0.1", port, false).unwrap();
    let mut config = CsaClientConfig::default();
    config.game.ponder = false;
    config.server.floodgate = false;
    config.record.enabled = false;
    let mut engine = Engine {
        first_move: Some("5b5a".into()),
        ..Engine::default()
    };
    let outcome = run_game_session_with_events(
        &config,
        &mut conn,
        &mut engine,
        Arc::new(AtomicBool::new(false)),
        &mut rshogi_csa_client::events::NoopSessionEventSink,
    )
    .unwrap();
    assert_eq!(outcome.record.moves.len(), 512);
    assert_eq!(engine.go.len(), 1);
    assert_eq!(outcome.result, rshogi_csa_client::protocol::GameResult::Draw);
    handle.join().unwrap();
}

#[test]
fn separate_successful_games_reset_clock_and_initialize_engine_again() {
    let mut engine = Engine::default();
    for _ in 0..2 {
        let (port, handle) = server(|reader, writer| {
            summary(writer, "+", "1sec", "1", "1", "2", &[]);
            assert_eq!(read(reader), "AGREE g");
            send(writer, &["START:g"]);
            assert_eq!(read(reader), "%TORYO");
            send(writer, &["#RESIGN", "#LOSE"]);
            assert_eq!(read(reader), "LOGOUT");
        });
        let mut conn = CsaConnection::connect("127.0.0.1", port, false).unwrap();
        let mut config = CsaClientConfig::default();
        config.game.ponder = false;
        config.time.margin_msec = 0;
        config.record.enabled = false;
        run_game_session_with_events(
            &config,
            &mut conn,
            &mut engine,
            Arc::new(AtomicBool::new(false)),
            &mut rshogi_csa_client::events::NoopSessionEventSink,
        )
        .unwrap();
        handle.join().unwrap();
    }
    assert_eq!(engine.new_games, 2);
    assert_eq!(engine.go, ["go btime 1000 wtime 1000 binc 2000 winc 2000"; 2]);
}

#[test]
fn common_time_inheritance_retains_ms_when_individual_unit_changes() {
    let (port, handle) = server(|_, writer| {
        send(
            writer,
            &[
                "BEGIN Game_Summary",
                "Game_ID:g",
                "Your_Turn:+",
                "BEGIN Time+",
                "Time_Unit:1msec",
                "Total_Time:600000",
                "END Time+",
                "BEGIN Time-",
                "Increment:1",
                "Time_Unit:1min",
                "END Time-",
                "BEGIN Time",
                "Total_Time:600",
                "Byoyomi:10",
                "Time_Unit:1sec",
                "END Time",
                "BEGIN Position",
                "PI",
                "+",
                "END Position",
                "END Game_Summary",
            ],
        );
    });
    let mut conn = CsaConnection::connect("127.0.0.1", port, false).unwrap();
    let summary = conn.recv_game_summary(0).unwrap();
    assert_eq!(summary.black_time.total_time_ms, 600000);
    assert_eq!(summary.black_time.byoyomi_ms, 10000);
    assert_eq!(summary.black_time.time_unit_ms, 1);
    assert_eq!(summary.white_time.total_time_ms, 600000);
    assert_eq!(summary.white_time.byoyomi_ms, 10000);
    assert_eq!(summary.white_time.increment_ms, 60000);
    assert_eq!(summary.white_time.time_unit_ms, 60000);
    handle.join().unwrap();
}
#[test]
fn start_and_reject_ids_are_normalized_or_fail_explicitly() {
    for (game_id, line, expected) in [
        ("g", "START: g", Some(StartResponse::Started("g".into()))),
        (
            "",
            "START:server-generated-id",
            Some(StartResponse::Started("server-generated-id".into())),
        ),
        ("", "REJECT:server-generated-id by opponent", Some(StartResponse::Rejected)),
        ("g", "START:other", None),
        ("g", "REJECT:other by opponent", None),
    ] {
        let (port, handle) = server(move |reader, writer| {
            assert_eq!(
                read(reader),
                if game_id.is_empty() {
                    "AGREE"
                } else {
                    "AGREE g"
                }
            );
            send(writer, &[line]);
        });
        let mut conn = CsaConnection::connect("127.0.0.1", port, false).unwrap();
        let result = conn.agree_and_wait_start(game_id, 0, || true);
        match expected {
            Some(value) => assert_eq!(result.unwrap(), value),
            None => assert!(result.is_err()),
        }
        handle.join().unwrap();
    }
}
#[test]
fn rejected_game_returns_to_cancellable_summary_wait() {
    let (port, handle) = server(|reader, writer| {
        assert_eq!(read(reader), "AGREE g");
        send(writer, &["REJECT:g"]);
        assert_eq!(read(reader), "LOGOUT");
    });
    let mut conn = CsaConnection::connect("127.0.0.1", port, false).unwrap();
    assert_eq!(conn.agree_and_wait_start("g", 0, || true).unwrap(), StartResponse::Rejected);
    assert!(conn.recv_game_summary_while(0, || false).unwrap().is_none());
    conn.logout().unwrap();
    handle.join().unwrap();
}

#[test]
fn server_generated_start_id_is_saved_in_summary_and_record() {
    #[derive(Default)]
    struct Sink {
        ids: Vec<String>,
        started_ids: Vec<String>,
    }
    impl rshogi_csa_client::events::SessionEventSink for Sink {
        fn on_event(
            &mut self,
            event: rshogi_csa_client::events::SessionProgress,
        ) -> std::result::Result<(), rshogi_csa_client::events::SinkError> {
            match event {
                rshogi_csa_client::events::SessionProgress::GameSummary(summary) => {
                    self.ids.push(summary.game_id.clone())
                }
                rshogi_csa_client::events::SessionProgress::GameStarted => {
                    self.started_ids.push(self.ids.last().unwrap().clone())
                }
                _ => {}
            }
            Ok(())
        }
    }
    let mut sink = Sink::default();
    let (port, handle) = server(|reader, writer| {
        send(
            writer,
            &[
                "BEGIN Game_Summary",
                "Your_Turn:+",
                "Name+:b",
                "Name-:w",
                "Total_Time:600",
                "Reconnect_Token:token",
                "BEGIN Position",
                "PI",
                "+",
                "END Position",
                "END Game_Summary",
            ],
        );
        assert_eq!(read(reader), "AGREE");
        send(writer, &["START:server-generated-id"]);
        assert_eq!(read(reader), "%TORYO");
        send(writer, &["#RESIGN", "#LOSE"]);
        assert_eq!(read(reader), "LOGOUT");
    });
    let mut conn = CsaConnection::connect("127.0.0.1", port, false).unwrap();
    let mut config = CsaClientConfig::default();
    config.game.ponder = false;
    config.record.enabled = false;
    let mut engine = Engine::default();
    let outcome = run_game_session_with_events(
        &config,
        &mut conn,
        &mut engine,
        Arc::new(AtomicBool::new(false)),
        &mut sink,
    )
    .unwrap();
    assert_eq!(outcome.summary.as_ref().unwrap().game_id, "server-generated-id");
    assert_eq!(outcome.record.game_id, "server-generated-id");
    assert_eq!(sink.ids, ["", "server-generated-id"]);
    assert_eq!(sink.started_ids, ["server-generated-id"]);
    assert_eq!(outcome.summary.as_ref().unwrap().reconnect_token.as_deref(), Some("token"));
    handle.join().unwrap();
}

#[test]
fn shutdown_during_summary_does_not_send_agree() {
    use std::cell::Cell;
    use std::sync::{Barrier, atomic::Ordering};
    struct Sink {
        calls: Cell<usize>,
        barrier: Arc<Barrier>,
    }
    impl rshogi_csa_client::events::SessionEventSink for Sink {
        fn on_event(
            &mut self,
            _: rshogi_csa_client::events::SessionProgress,
        ) -> std::result::Result<(), rshogi_csa_client::events::SinkError> {
            Ok(())
        }
        fn should_continue(&self) -> bool {
            let calls = self.calls.get() + 1;
            self.calls.set(calls);
            // Connected 後の確認が1回目、summary待機ループの確認が2回目。
            // shutdown の load 後・summary受信前に停止を立て、AGREE前の再確認を検証。
            if calls == 2 {
                self.barrier.wait();
                self.barrier.wait();
            }
            true
        }
    }
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_server = Arc::clone(&shutdown);
    let barrier = Arc::new(Barrier::new(2));
    let server_barrier = Arc::clone(&barrier);
    let (port, handle) = server(move |reader, writer| {
        server_barrier.wait();
        shutdown_server.store(true, Ordering::SeqCst);
        server_barrier.wait();
        summary(writer, "+", "1sec", "180", "600", "2", &[]);
        assert_eq!(read(reader), "LOGOUT");
    });
    let mut conn = CsaConnection::connect("127.0.0.1", port, false).unwrap();
    let mut engine = Engine::default();
    let mut sink = Sink {
        calls: Cell::new(0),
        barrier,
    };
    let result = run_game_session_with_events(
        &CsaClientConfig::default(),
        &mut conn,
        &mut engine,
        shutdown,
        &mut sink,
    );
    assert!(matches!(result, Err(rshogi_csa_client::events::SessionError::Shutdown)));
    assert_eq!(engine.new_games, 0);
    handle.join().unwrap();
}

/// 実際の GameRoom が生成する通知を TCP 経由で読み、双方の残時間を照合する。
#[test]
fn built_in_server_nonsecond_clocks_match_client_budgets() {
    use rshogi_core::types::EnteringKingRule;
    use rshogi_csa_server::Color;
    use rshogi_csa_server::{
        BroadcastTarget, ClockSpec, CsaLine, GameId, GameRoom, GameRoomConfig, PlayerName,
    };
    for (spec, elapsed_ms, initial_ms, remaining_ms, wire_t) in [
        (
            ClockSpec::CountdownMsec {
                total_time_ms: 10000,
                byoyomi_ms: 0,
            },
            1250,
            10000,
            8750,
            1250,
        ),
        (
            ClockSpec::StopWatch {
                total_time_min: 3,
                byoyomi_min: 0,
            },
            125000,
            180000,
            60000,
            2,
        ),
    ] {
        let (port, handle) = server(move |reader, writer| {
            let mut room = GameRoom::new(
                GameRoomConfig {
                    game_id: GameId::new("g"),
                    black: PlayerName::new("b"),
                    white: PlayerName::new("w"),
                    max_moves: 256,
                    time_margin_ms: 0,
                    entering_king_rule: EnteringKingRule::Point27,
                    initial_sfen: None,
                },
                spec.build_clock(),
            )
            .unwrap();
            send(
                writer,
                &[
                    "BEGIN Game_Summary",
                    "Protocol_Version:1.2.1",
                    "Format:Shogi 1.0",
                    "Game_ID:g",
                    "Name+:b",
                    "Name-:w",
                    "Your_Turn:+",
                ],
            );
            for line in spec.format_time_section().lines() {
                send(writer, &[line]);
            }
            send(
                writer,
                &[
                    "BEGIN Position",
                    "PI",
                    "+",
                    "END Position",
                    "END Game_Summary",
                ],
            );
            assert_eq!(read(reader), "AGREE g");
            room.handle_line(Color::Black, &CsaLine::new("AGREE g"), 0).unwrap();
            let started = room.handle_line(Color::White, &CsaLine::new("AGREE g"), 0).unwrap();
            send(writer, &[started.broadcasts[0].line.as_str()]);
            for (color, token, now) in [
                (Color::Black, "+7776FU", elapsed_ms),
                (Color::White, "-3334FU", 2 * elapsed_ms),
            ] {
                if color == Color::Black {
                    assert_eq!(read(reader), token);
                }
                let result = room.handle_line(color, &CsaLine::new(token), now).unwrap();
                assert_eq!(result.broadcasts[0].line.as_str(), format!("{token},T{wire_t}"));
                assert_eq!(room.clock_remaining_main_ms(color), remaining_ms);
                send(writer, &[result.broadcasts[0].line.as_str()]);
            }
            assert_eq!(read(reader), "%TORYO");
            let result =
                room.handle_line(Color::Black, &CsaLine::new("%TORYO"), 2 * elapsed_ms).unwrap();
            for entry in result.broadcasts {
                if matches!(
                    entry.target,
                    BroadcastTarget::Black | BroadcastTarget::Players | BroadcastTarget::All
                ) {
                    send(writer, &[entry.line.as_str()]);
                }
            }
            assert_eq!(read(reader), "LOGOUT");
        });
        let mut conn = CsaConnection::connect("127.0.0.1", port, false).unwrap();
        let mut config = CsaClientConfig::default();
        config.game.ponder = false;
        config.time.margin_msec = 0;
        config.record.enabled = false;
        let mut engine = Engine {
            first_move: Some("7g7f".into()),
            ..Engine::default()
        };
        run_game_session_with_events(
            &config,
            &mut conn,
            &mut engine,
            Arc::new(AtomicBool::new(false)),
            &mut rshogi_csa_client::events::NoopSessionEventSink,
        )
        .unwrap();
        assert_eq!(
            engine.go,
            [
                format!("go btime {initial_ms} wtime {initial_ms}"),
                format!("go btime {remaining_ms} wtime {remaining_ms}")
            ]
        );
        handle.join().unwrap();
    }
}

#[test]
fn stalled_summary_body_can_be_cancelled_without_waiting_for_end() {
    use std::sync::{atomic::Ordering, mpsc};
    let stopped = Arc::new(AtomicBool::new(false));
    let stopped_server = Arc::clone(&stopped);
    let (consumed_tx, consumed_rx) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (port, handle) = server(move |reader, writer| {
        send(writer, &["BEGIN Game_Summary", "Game_ID:g"]);
        ready_tx.send(()).unwrap();
        consumed_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        stopped_server.store(true, Ordering::SeqCst);
        // END は送らない。停止で受信を抜け、サーバーへの送信は LOGOUT のみ。
        assert_eq!(read(reader), "LOGOUT");
    });
    let mut conn = CsaConnection::connect("127.0.0.1", port, false).unwrap();
    ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let mut polls = 0;
    let start = std::time::Instant::now();
    let result = conn
        .recv_game_summary_while(60, || {
            polls += 1;
            // BEGIN 待ち → 本文先頭 → Game_ID を読んだ後の確認で同期する。
            if polls == 3 {
                consumed_tx.send(()).unwrap();
            }
            !stopped.load(Ordering::SeqCst)
        })
        .unwrap();
    assert!(result.is_none());
    assert!(start.elapsed() < Duration::from_secs(5));
    conn.logout().unwrap();
    handle.join().unwrap();
}
