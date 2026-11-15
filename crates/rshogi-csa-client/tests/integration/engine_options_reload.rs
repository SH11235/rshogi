#![cfg(all(unix, feature = "cli"))]

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use super::common::write_mock_script;

struct Fixture {
    root: PathBuf,
    script: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.script);
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn config(root: &Path, script: &Path, port: u16, book: &str, games: usize) -> String {
    format!(
        "[server]\nhost='127.0.0.1'\nport={port}\nid='alice'\npassword='pw'\nfloodgate=false\n\
         [server.keepalive]\nping_interval_sec=0\n\
         [engine]\npath='{}'\n[engine.options]\nBookFile='{}'\nNetworkDelay=0\nThreads=1\n\
         [game]\nponder=false\nmax_games={games}\n\
         [record]\ndir='{}'\nsave_csa=false\nsave_sfen=false\n\
         [log]\ndir=''\n",
        script.display(),
        root.join(book).display(),
        root.join("records").display()
    )
}

fn read_line(reader: &mut BufReader<TcpStream>) -> String {
    let mut line = String::new();
    assert!(reader.read_line(&mut line).unwrap() > 0);
    line.trim_end().to_owned()
}

#[test]
fn slow_book_reload_finishes_before_next_login_without_restarting_engine() {
    run_reload_scenario(false);
}

#[test]
fn failed_ready_restarts_old_engine_and_retries_unacknowledged_options_next_game() {
    run_reload_scenario(true);
}

fn run_reload_scenario(fail_first_ready: bool) {
    let expected_moves = if fail_first_ready {
        vec!["+7776FU", "+7776FU", "+2726FU"]
    } else {
        vec!["+7776FU", "+2726FU"]
    };
    let games = expected_moves.len();
    let root = std::env::temp_dir().join(format!(
        "csa-options-e2e-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    let trace = root.join("usi.log");
    let script = write_mock_script(
        "options_reload",
        &format!(
            r#"#!/usr/bin/env bash
set -eu
while IFS= read -r line; do
    printf '%s\n' "$line" >> '{}'
    case "$line" in
        usi) printf 'id name reload-mock\noption name BookFile type string default none\nusiok\n' ;;
        'setoption name BookFile value '*) book="${{line#* value }}" ;;
        isready)
            case "$book" in
                */new.book)
                    if [ "{fail_first_ready}" = true ] && [ ! -e '{failed}' ]; then
                        touch '{failed}'
                        exit 1
                    fi
                    if [ ! -e '{prepared}' ]; then
                        sleep 0.2
                        printf 'reload-ready\n' >> '{trace}'
                        touch '{prepared}'
                    fi ;;
            esac
            echo readyok ;;
        'go '*) read -r chosen < "$book"; echo "bestmove $chosen" ;;
        quit) exit 0 ;;
    esac
done
"#,
            trace.display(),
            trace = trace.display(),
            failed = root.join("failed-ready").display(),
            prepared = root.join("prepared-book").display(),
        ),
    );
    let fixture = Fixture { root, script };
    std::fs::write(fixture.root.join("old.book"), "7g7f\n").unwrap();
    std::fs::write(fixture.root.join("new.book"), "2g2f\n").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let config_path = fixture.root.join("client.toml");
    std::fs::write(&config_path, config(&fixture.root, &fixture.script, port, "old.book", games))
        .unwrap();
    let server_root = fixture.root.clone();
    let server_script = fixture.script.clone();
    let server_config = config_path.clone();
    let server = thread::spawn(move || {
        for (game, expected) in expected_moves.iter().enumerate() {
            let deadline = Instant::now() + Duration::from_secs(15);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "client did not connect");
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("accept: {error}"),
                }
            };
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            stream.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            assert_eq!(read_line(&mut reader), "LOGIN alice pw");
            if *expected == "+2726FU" {
                let trace = server_root.join("usi.log");
                assert!(std::fs::read_to_string(&trace).unwrap().contains("reload-ready\n"));
                writeln!(
                    std::fs::OpenOptions::new().append(true).open(trace).unwrap(),
                    "server-login-ready"
                )
                .unwrap();
            }
            write!(stream, "LOGIN:alice OK\nBEGIN Game_Summary\nProtocol_Version:1.2\nGame_ID:reload-{game}\nName+:alice\nName-:bob\nYour_Turn:+\nTo_Move:+\nTime_Unit:1sec\nTotal_Time:600\nByoyomi:10\nBEGIN Position\nPI\n+\nEND Position\nEND Game_Summary\n").unwrap();
            assert_eq!(read_line(&mut reader), format!("AGREE reload-{game}"));
            writeln!(stream, "START:reload-{game}").unwrap();
            assert_eq!(read_line(&mut reader), *expected);
            if game == 0 {
                let mut next = config(&server_root, &server_script, port, "new.book", games);
                next = next.replace("NetworkDelay=0", "NetworkDelay=123");
                let temporary = server_config.with_extension("tmp");
                std::fs::write(&temporary, next).unwrap();
                std::fs::rename(temporary, &server_config).unwrap();
            }
            writeln!(stream, "{expected},T0\n#RESIGN\n#WIN").unwrap();
            assert_eq!(read_line(&mut reader), "LOGOUT");
        }
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_csa_client"))
        .arg(&config_path)
        .env_remove("CSA_HOST")
        .env_remove("CSA_PORT")
        .env_remove("CSA_ID")
        .env_remove("CSA_PASSWORD")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let timed_out = loop {
        if child.try_wait().unwrap().is_some() {
            break false;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            break true;
        }
        thread::sleep(Duration::from_millis(20));
    };
    let output = child.wait_with_output().unwrap();
    assert!(!timed_out, "client timed out: {}", String::from_utf8_lossy(&output.stderr));
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    server.join().unwrap();
    let commands = std::fs::read_to_string(trace).unwrap();
    assert_eq!(
        commands.lines().filter(|line| *line == "usi").count(),
        if fail_first_ready { 2 } else { 1 },
        "{commands}"
    );
    assert_eq!(
        commands.lines().filter(|line| *line == "usinewgame").count(),
        games,
        "{commands}"
    );
    assert_eq!(
        commands.lines().filter(|line| *line == "isready").count(),
        if fail_first_ready { 7 } else { 4 },
        "{commands}"
    );
    assert_eq!(
        commands
            .lines()
            .filter(|line| *line == "setoption name Threads value 1")
            .count(),
        if fail_first_ready { 2 } else { 1 },
        "{commands}"
    );
    let book_update =
        format!("setoption name BookFile value {}", fixture.root.join("new.book").display());
    let update_index = commands.rfind(&book_update).unwrap();
    let first_go = commands.find("go ").unwrap();
    let next_ready = commands.rfind("isready").unwrap();
    let prepared_index = commands.find("reload-ready\n").unwrap();
    let login_index = commands.find("server-login-ready\n").unwrap();
    let next_game = commands.rfind("usinewgame").unwrap();
    assert!(first_go < update_index && update_index < prepared_index, "{commands}");
    assert!(
        prepared_index < login_index && login_index < next_game && next_game < next_ready,
        "{commands}"
    );
    assert!(commands.contains("setoption name NetworkDelay value 123"), "{commands}");
    let jsonl: Vec<_> = std::fs::read_dir(fixture.root.join("records/jsonl"))
        .unwrap()
        .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect();
    assert_eq!(jsonl.len(), games);
    for game in 0..games {
        let book = if game == games - 1 {
            "new.book"
        } else {
            "old.book"
        };
        let id = format!("reload-{game}");
        let record = jsonl.iter().find(|text| text.contains(&id)).unwrap();
        assert!(record.contains(book), "{record}");
    }
}

#[test]
fn ponder_setting_keeps_startup_precedence_when_options_are_updated() {
    use super::common::spawn_engine;
    use rshogi_csa_client::engine::SpawnOptions;
    use std::collections::HashMap;

    for name in ["USI_Ponder", "Ponder"] {
        let path = write_mock_script(
            "reload_ponder",
            &format!(
                r#"#!/usr/bin/env bash
set -eu
ponder=false
while IFS= read -r line; do
    case "$line" in
        usi) printf 'option name {name} type check default false\nusiok\n' ;;
        'setoption name {name} value '*) ponder="${{line##* }}" ;;
        isready) if [ "$ponder" != true ]; then exit 1; fi; echo readyok ;;
        quit) exit 0 ;;
    esac
done
"#
            ),
        );
        let options = HashMap::from([(name.to_owned(), toml::Value::Boolean(false))]);
        let mut engine = spawn_engine(
            &path,
            &options,
            SpawnOptions {
                ponder: true,
                startup_timeout: Duration::from_secs(5),
                stderr_passthrough: false,
            },
        )
        .unwrap();
        engine.update_options(&options, true, Duration::from_secs(5)).unwrap();
        engine.new_game().unwrap();
        engine.quit();
        std::fs::remove_file(path).unwrap();
    }
}

#[test]
fn startup_keeps_per_line_timeout_while_reload_bounds_total_ready_wait() {
    use super::common::spawn_engine;
    use rshogi_csa_client::engine::SpawnOptions;
    use std::collections::HashMap;

    let path = write_mock_script(
        "reload_ready_timeout",
        r#"#!/usr/bin/env bash
set -eu
while IFS= read -r line; do
    case "$line" in
        usi) echo usiok ;;
        isready)
            for chunk in 1 2 3 4 5 6; do
                echo 'info string loading book'
                sleep 0.3
            done
            echo readyok ;;
        quit) exit 0 ;;
    esac
done
"#,
    );
    let startup = Instant::now();
    let mut engine = spawn_engine(
        &path,
        &HashMap::new(),
        SpawnOptions {
            ponder: false,
            startup_timeout: Duration::from_secs(1),
            stderr_passthrough: false,
        },
    )
    .unwrap();
    assert!(startup.elapsed() > Duration::from_secs(1));
    let start = Instant::now();
    let error = engine
        .update_options(&HashMap::new(), false, Duration::from_secs(1))
        .unwrap_err();
    assert!(error.to_string().contains("タイムアウト"), "{error:#}");
    assert!(start.elapsed() < Duration::from_secs(5), "{error:#}");
    drop(engine);
    std::fs::remove_file(path).unwrap();
}
