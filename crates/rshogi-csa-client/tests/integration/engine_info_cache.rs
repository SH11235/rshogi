#![cfg(unix)]

use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::time::Duration;

use rshogi_csa_client::engine::{SearchOutcome, SpawnOptions, UsiEngine};

use crate::common;

fn write_mock_engine(script: &str, name: &str) -> PathBuf {
    common::write_mock_script(name, script)
}

fn spawn_engine(path: &Path) -> UsiEngine {
    common::spawn_engine(
        path,
        &HashMap::new(),
        SpawnOptions {
            ponder: false,
            startup_timeout: Duration::from_secs(5),
            stderr_passthrough: false,
        },
    )
    .expect("spawn engine")
}

#[test]
fn go_without_info_returns_empty_search_info() {
    let path = write_mock_engine(
        r#"#!/usr/bin/env bash
while IFS= read -r line; do
    case "$line" in
        usi)
            echo "id name no-info"
            echo "usiok"
            ;;
        isready)
            echo "readyok"
            ;;
        position*)
            ;;
        go*)
            echo "bestmove 7g7f"
            ;;
        quit)
            exit 0
            ;;
    esac
done
"#,
        "mock_no_info",
    );
    let mut engine = spawn_engine(&path);
    let shutdown = AtomicBool::new(false);
    let (_tx, rx) = mpsc::channel();
    let mut callback_count = 0;
    let mut callback = |_info: &rshogi_csa_client::engine::SearchInfo, _raw: &str| {
        callback_count += 1;
    };

    let outcome = engine
        .go_with_info("position startpos", "go depth 1", &shutdown, &rx, &mut callback)
        .expect("go");
    engine.quit();

    match outcome {
        SearchOutcome::BestMove(result, info) => {
            assert_eq!(result.bestmove, "7g7f");
            assert!(!info.has_observation());
        }
        SearchOutcome::ServerInterrupt(_) => panic!("unexpected server interrupt"),
    }
    assert_eq!(callback_count, 0);
}

#[test]
fn pending_info_after_previous_bestmove_is_not_reused_by_next_go() {
    let path = write_mock_engine(
        r#"#!/usr/bin/env bash
go_count=0
while IFS= read -r line; do
    case "$line" in
        usi)
            echo "id name stale-info"
            echo "usiok"
            ;;
        isready)
            echo "readyok"
            ;;
        position*)
            ;;
        go*)
            go_count=$((go_count + 1))
            if [ "$go_count" -eq 1 ]; then
                echo "bestmove 7g7f"
                echo "info depth 1 score cp -32001 nodes 0 pv 1c1d"
            else
                echo "bestmove 2g2f"
            fi
            ;;
        quit)
            exit 0
            ;;
    esac
done
"#,
        "mock_stale_info",
    );
    let mut engine = spawn_engine(&path);
    let shutdown = AtomicBool::new(false);
    let (_tx, rx) = mpsc::channel();
    let mut callback_count = 0;
    let mut callback = |_info: &rshogi_csa_client::engine::SearchInfo, _raw: &str| {
        callback_count += 1;
    };

    let first = engine
        .go_with_info("position startpos", "go depth 1", &shutdown, &rx, &mut callback)
        .expect("first go");
    match first {
        SearchOutcome::BestMove(result, info) => {
            assert_eq!(result.bestmove, "7g7f");
            assert!(!info.has_observation());
        }
        SearchOutcome::ServerInterrupt(_) => panic!("unexpected server interrupt"),
    }

    std::thread::sleep(Duration::from_millis(100));
    let second = engine
        .go_with_info("position startpos moves 7g7f", "go depth 1", &shutdown, &rx, &mut callback)
        .expect("second go");
    engine.quit();

    match second {
        SearchOutcome::BestMove(result, info) => {
            assert_eq!(result.bestmove, "2g2f");
            assert!(!info.has_observation());
        }
        SearchOutcome::ServerInterrupt(_) => panic!("unexpected server interrupt"),
    }
    assert_eq!(callback_count, 0);
}
