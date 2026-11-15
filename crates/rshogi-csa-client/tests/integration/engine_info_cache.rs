#![cfg(unix)]

use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
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
            assert!(!info.has_score());
            assert!(info.pv.is_empty());
        }
        SearchOutcome::ServerInterrupt(_) => panic!("unexpected server interrupt"),
    }
    assert_eq!(callback_count, 0);
}

#[test]
fn shutdown_is_observed_between_info_and_bestmove() {
    let path = write_mock_engine(
        r#"#!/usr/bin/env bash
while IFS= read -r line; do
    case "$line" in
        usi) echo "usiok" ;;
        isready) echo "readyok" ;;
        go*)
            printf 'info depth 1 score cp 10 pv 7g7f\nbestmove 7g7f\n'
            ;;
        quit) exit 0 ;;
    esac
done
"#,
        "mock_shutdown_after_info",
    );
    let mut engine = spawn_engine(&path);
    let shutdown = AtomicBool::new(false);
    let (_tx, rx) = mpsc::channel();
    let mut callback = |_info: &rshogi_csa_client::engine::SearchInfo, _raw: &str| {
        shutdown.store(true, Ordering::SeqCst);
    };
    let outcome = engine
        .go_with_info("position startpos", "go depth 1", &shutdown, &rx, &mut callback)
        .expect("go");
    engine.quit();
    match outcome {
        SearchOutcome::BestMove(result, _) => assert_eq!(result.bestmove, "resign"),
        SearchOutcome::ServerInterrupt(_) => panic!("unexpected server interrupt"),
    }
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
            assert!(!info.has_score());
            assert!(info.pv.is_empty());
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
            assert!(!info.has_score());
            assert!(info.pv.is_empty());
        }
        SearchOutcome::ServerInterrupt(_) => panic!("unexpected server interrupt"),
    }
    assert_eq!(callback_count, 0);
}

fn ponder_info_for_bestmove(before_hit: &str, after_hit: &str) -> rshogi_csa_client::SearchInfo {
    let script = format!(
        r#"#!/usr/bin/env bash
while IFS= read -r line; do
    case "$line" in
        usi) echo usiok ;;
        isready) echo readyok ;;
        'go ponder'*)
            {before_hit}
            ;;
        ponderhit)
            {after_hit}
            echo 'bestmove 2g2f'
            ;;
        quit) exit 0 ;;
    esac
done
"#
    );
    let path = write_mock_engine(&script, "ponder_info_root");
    let mut engine = spawn_engine(&path);
    let shutdown = AtomicBool::new(false);
    let (_tx, rx) = mpsc::channel();
    engine
        .go_ponder("position startpos moves 7g7f 3c3d", "go ponder depth 1")
        .expect("go ponder");
    let mut callback = |_: &rshogi_csa_client::SearchInfo, _: &str| {};
    let outcome = engine.ponderhit_with_info(&shutdown, &rx, &mut callback).expect("ponderhit");
    engine.quit();
    match outcome {
        SearchOutcome::BestMove(result, info) => {
            assert_eq!(result.bestmove, "2g2f");
            info
        }
        SearchOutcome::ServerInterrupt(_) => panic!("unexpected server interrupt"),
    }
}

#[test]
fn ponderhit_info_without_matching_pv_is_not_move_evaluation() {
    for (before, after) in [
        ("echo 'info score cp 81 pv 8c8d'", ":"),
        (":", "echo 'info score cp 81 pv 8c8d'"),
        (":", "echo 'info score cp 81'"),
    ] {
        let info = ponder_info_for_bestmove(before, after);
        if !info.pv.is_empty() {
            assert_eq!(info.score_cp, Some(81));
        }
        assert!(!info.matches_bestmove("2g2f"));
    }
}

#[test]
fn ponderhit_fresh_info_and_normal_ponder_keep_move_evaluation() {
    for (before, after) in [
        ("echo 'info score cp 81 pv 8c8d'", "echo 'info score cp 93 pv 2g2f 8c8d'"),
        ("echo 'info score cp 93 pv 2g2f 8c8d'", ":"),
    ] {
        let info = ponder_info_for_bestmove(before, after);
        assert!(info.matches_bestmove("2g2f"));
        assert_eq!(info.score_cp, Some(93));
        assert_eq!(info.pv, ["2g2f", "8c8d"]);
    }
}

#[test]
fn normal_go_and_ponder_miss_keep_matching_move_evaluation() {
    let path = write_mock_engine(
        r#"#!/usr/bin/env bash
while IFS= read -r line; do
    case "$line" in
        usi) echo usiok ;;
        isready) echo readyok ;;
        'go ponder'*) echo 'info score cp 81 pv 8c8d' ;;
        stop) echo 'bestmove 2g2f' ;;
        go*)
            echo 'info depth 4 score cp 93 pv 2g2f 8c8d'
            echo 'bestmove 2g2f'
            ;;
        quit) exit 0 ;;
    esac
done
"#,
        "normal_go_matching_pv",
    );
    for ponder_miss in [false, true] {
        let mut engine = spawn_engine(&path);
        let shutdown = AtomicBool::new(false);
        let (_tx, rx) = mpsc::channel();
        if ponder_miss {
            engine
                .go_ponder("position startpos moves 7g7f 3c3d", "go ponder depth 1")
                .expect("go ponder");
            engine.stop_and_wait().expect("stop ponder");
        }
        let mut callback = |_: &rshogi_csa_client::SearchInfo, _: &str| {};
        let outcome = engine
            .go_with_info(
                "position startpos moves 7g7f 4c4d",
                "go depth 1",
                &shutdown,
                &rx,
                &mut callback,
            )
            .expect("go");
        engine.quit();
        match outcome {
            SearchOutcome::BestMove(result, info) => {
                assert_eq!(result.bestmove, "2g2f");
                assert!(info.matches_bestmove(&result.bestmove));
                assert_eq!(info.score_cp, Some(93));
                assert_eq!(info.depth, Some(4));
                assert_eq!(info.pv, ["2g2f", "8c8d"]);
            }
            SearchOutcome::ServerInterrupt(_) => panic!("unexpected server interrupt"),
        }
    }
}

#[test]
fn split_score_and_matching_pv_are_not_move_evaluation() {
    for after in ["echo 'info pv 2g2f'", "echo 'info pv 2g2f score cp 93'"] {
        let info = ponder_info_for_bestmove("echo 'info score cp 81 pv 8c8d'", after);
        assert!(!info.has_score());
        assert!(info.pv.is_empty());
    }
}

#[test]
fn final_multipv_two_keeps_primary_evaluation() {
    let info = ponder_info_for_bestmove(
        "echo 'info multipv 1 depth 4 score cp 93 pv 2g2f 8c8d'",
        "echo 'info multipv 2 depth 5 score cp 81 pv 7g7f 3c3d'",
    );
    assert_eq!(info.score_cp, Some(93));
    assert_eq!(info.depth, Some(4));
    assert_eq!(info.pv, ["2g2f", "8c8d"]);
}
