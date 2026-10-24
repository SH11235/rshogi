//! `spsa --dispatch async` の USI 配管と永続化の結合テスト。
//!
//! fake USI engine (`spsa_test_engine`) の `value_driven` mode は受け取った設定値だけで勝敗を決めるので、
//! W=1 では中断の有無に関わらず同じ θ になる。crash 境界は `RSHOGI_SPSA_FAULT` で process を落として再現する。

use std::path::Path;
use std::process::Command;

use super::spsa_run_dir_integration::{PoolTest, assert_success};

const ASYNC: [&str; 2] = ["--dispatch", "async"];

fn run_async(
    test: &PoolTest,
    mode: &str,
    total: u32,
    concurrency: usize,
    extra: &[&str],
    env: &[(&str, &Path)],
) -> std::process::Output {
    let mut args = ASYNC.to_vec();
    args.extend_from_slice(extra);
    test.run_with(mode, total, concurrency, &args, env)
}

fn read(test: &PoolTest, file: &str) -> String {
    String::from_utf8(test.bytes(file)).unwrap()
}

fn journal(test: &PoolTest) -> Vec<serde_json::Value> {
    read(test, "journal.jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn kinds(test: &PoolTest, kind: &str) -> Vec<serde_json::Value> {
    journal(test).into_iter().filter(|record| record["kind"] == kind).collect()
}

/// meta が指す最終 snapshot の θ (最短 round-trip 表記の文字列)。6 桁表示の CSV では見えない bit 差を比べる。
fn final_theta(test: &PoolTest) -> serde_json::Value {
    let meta = test.meta();
    let name = meta["snapshot"].as_str().unwrap();
    let snapshot: serde_json::Value =
        serde_json::from_slice(&test.bytes(&format!("snapshots/{name}"))).unwrap();
    snapshot["theta"].clone()
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// value_driven・W=1・6 pair の中断なしの基準 run。
fn reference_run() -> PoolTest {
    let test = PoolTest::new();
    assert_success(&run_async(&test, "value_driven", 6, 1, &["--snapshot-interval", "2"], &[]));
    test
}

#[test]
fn single_worker_async_matches_batch_pairs_one() {
    let batch = PoolTest::new();
    assert_success(&batch.run_with("value_driven", 6, 1, &["--batch-pairs", "1"], &[]));
    let async_run = reference_run();
    assert_eq!(batch.bytes("final.params"), async_run.bytes("final.params"));
    let rows = |test: &PoolTest| -> Vec<String> {
        read(test, "values.csv")
            .lines()
            .skip(1)
            .map(|line| line.split_once(',').unwrap().1.to_owned())
            .collect()
    };
    assert_eq!(rows(&batch), rows(&async_run));
    let meta = async_run.meta();
    assert_eq!(meta["format_version"], 5);
    assert_eq!(meta["status"], "completed");
    assert_eq!(meta["completed_pairs"], 6);
    assert_eq!(kinds(&async_run, "accept").len(), 6);
}

#[test]
fn pair_sends_values_once_and_swaps_colors() {
    let test = PoolTest::new();
    assert_success(&run_async(
        &test,
        "value_driven",
        3,
        1,
        &[
            "--active-only-regex",
            "^SPSA_TEST_INT$",
            "--random-startpos",
            "false",
        ],
        &[("SPSA_TEST_ENGINE_PROTOCOL_LOG", &test.path("protocol"))],
    ));
    let log = std::fs::read_to_string(test.path("protocol")).unwrap();
    let mut engines = std::collections::BTreeMap::<&str, Vec<&str>>::new();
    for line in log.lines() {
        let (pid, command) = line.split_once(' ').unwrap();
        engines.entry(pid).or_default().push(command);
    }
    assert_eq!(engines.len(), 2);
    for commands in engines.values() {
        let relevant: Vec<&str> = commands
            .iter()
            .copied()
            .filter(|command| {
                command.starts_with("setoption name SPSA_TEST_")
                    || matches!(*command, "isready" | "usinewgame")
            })
            .collect();
        // 初期化の後、pair ごとに「全値 → isready → (usinewgame → isready) × 2 局」。
        let first_pair =
            relevant.iter().position(|c| c.starts_with("setoption name SPSA")).unwrap();
        let expected = [
            "INT",
            "FLOAT",
            "isready",
            "usinewgame",
            "isready",
            "usinewgame",
            "isready",
        ];
        let pairs: Vec<Vec<&str>> = relevant[first_pair..]
            .chunks(expected.len())
            .map(|chunk| {
                chunk
                    .iter()
                    .map(|c| {
                        if c.starts_with("setoption name SPSA_TEST_INT") {
                            "INT"
                        } else if c.starts_with("setoption name SPSA_TEST_FLOAT") {
                            "FLOAT"
                        } else {
                            c
                        }
                    })
                    .collect()
            })
            .collect();
        assert_eq!(pairs.len(), 3, "{log}");
        assert!(pairs.iter().all(|pair| pair == &expected), "{log}");
        // regex 対象外の FLOAT は固定値を両側に毎回送る。
        assert!(
            commands
                .iter()
                .filter(|c| c.starts_with("setoption name SPSA_TEST_FLOAT"))
                .all(|c| *c == "setoption name SPSA_TEST_FLOAT value 1.500000"),
            "{log}"
        );
    }
    let stats = read(&test, "stats.csv");
    let rows: Vec<Vec<&str>> =
        stats.lines().skip(1).map(|line| line.split(',').collect()).collect();
    assert_eq!(rows.len(), 3);
    for row in &rows {
        assert_eq!((row[6], row[10]), ("true", "false"), "game1 plus black, game2 plus white");
    }
    // cyclic の開始局面は pair_id 順に周回する。
    let dispatches = kinds(&test, "dispatch");
    let starts: Vec<u64> =
        dispatches.iter().map(|d| d["start_pos_index"].as_u64().unwrap()).collect();
    assert_eq!(starts, vec![0, 1, 0]);
}

#[test]
fn second_game_failure_replays_whole_pair_with_same_values() {
    let test = PoolTest::new();
    let output = run_async(
        &test,
        "value_driven",
        2,
        1,
        &["--engine-retries", "0"],
        &[
            ("SPSA_TEST_ENGINE_FAIL_ONCE_MARKER", &test.path("marker")),
            ("SPSA_TEST_ENGINE_FAIL_ON_NEWGAME", Path::new("3")),
            ("SPSA_TEST_ENGINE_PROTOCOL_LOG", &test.path("protocol")),
        ],
    );
    assert_success(&output);
    let failed = kinds(&test, "attempt_failed");
    assert_eq!(failed.len(), 1);
    assert_eq!(
        (failed[0]["pair_id"].as_u64(), failed[0]["attempt"].as_u64()),
        (Some(0), Some(1))
    );
    let dispatches: Vec<_> =
        kinds(&test, "dispatch").into_iter().filter(|d| d["pair_id"] == 0).collect();
    assert_eq!(dispatches.len(), 2);
    assert_eq!(dispatches[0]["plus"], dispatches[1]["plus"]);
    assert_eq!(dispatches[0]["minus"], dispatches[1]["minus"]);
    assert_eq!(dispatches[0]["start_pos_index"], dispatches[1]["start_pos_index"]);
    // 第 1 局を終えた後の失敗でも、受理されるのは attempt 2 の 2 局だけ。
    let received = kinds(&test, "received");
    assert_eq!(received.len(), 2);
    assert_eq!(received[0]["attempt"], 2);
    assert_eq!(test.meta()["failed_attempts"], 1);
    let reference = reference_run_with_total(2);
    assert_eq!(test.bytes("final.params"), reference.bytes("final.params"));
}

fn reference_run_with_total(total: u32) -> PoolTest {
    let test = PoolTest::new();
    assert_success(&run_async(&test, "value_driven", total, 1, &[], &[]));
    test
}

#[test]
fn initialization_failure_moves_to_next_attempt() {
    let test = PoolTest::new();
    let output = run_async(
        &test,
        "initialize_fail_once",
        1,
        1,
        &["--engine-retries", "0"],
        &[("SPSA_TEST_ENGINE_SCRIPT_GATE", &test.path("gate"))],
    );
    assert_success(&output);
    assert_eq!(kinds(&test, "attempt_failed").len(), 1);
    assert_eq!(kinds(&test, "dispatch").len(), 2);
    assert_eq!(test.meta()["completed_pairs"], 1);
}

#[test]
fn repeated_failures_stop_and_resume_needs_a_higher_limit() {
    let test = PoolTest::new();
    let output = run_async(
        &test,
        "exit_on_go",
        2,
        1,
        &["--engine-retries", "0", "--max-pair-attempts", "2"],
        &[],
    );
    assert!(!output.status.success());
    assert!(stderr(&output).contains("failed 2 attempt(s)"), "{}", stderr(&output));
    let meta = test.meta();
    assert_eq!(meta["status"], "stopped");
    assert_eq!(meta["failed_attempts"], 2);
    assert!(!test.path("run/final.params").exists());
    assert_eq!(kinds(&test, "attempt_failed").len(), 2);
    // 失敗数はリセットされない。同じ上限では指し直さずに止まる。
    let resume = |limit: &str| {
        run_async(
            &test,
            "exit_on_go",
            2,
            1,
            &[
                "--engine-retries",
                "0",
                "--max-pair-attempts",
                limit,
                "--resume",
                "--force-unlock",
            ],
            &[],
        )
    };
    let output = resume("2");
    assert!(!output.status.success());
    assert!(stderr(&output).contains("failed 2 attempt(s)"), "{}", stderr(&output));
    assert_eq!(kinds(&test, "dispatch").len(), 2);
    // 上限を引き上げれば 1 回指し直す。
    let output = resume("3");
    assert!(!output.status.success());
    assert!(stderr(&output).contains("failed 3 attempt(s)"), "{}", stderr(&output));
    let dispatches = kinds(&test, "dispatch");
    assert_eq!(dispatches.len(), 3);
    assert_eq!(dispatches.last().unwrap()["attempt"], 3);
}

#[test]
fn duplicate_result_is_journaled_and_applied_once() {
    let test = PoolTest::new();
    assert_success(&run_async(
        &test,
        "value_driven",
        6,
        1,
        &["--snapshot-interval", "2"],
        &[("RSHOGI_SPSA_FAULT", Path::new("duplicate_result:1"))],
    ));
    let duplicates = kinds(&test, "duplicate");
    assert_eq!(duplicates.len(), 1);
    assert_eq!(duplicates[0]["pair_id"], 1);
    let reference = reference_run();
    assert_eq!(final_theta(&test), final_theta(&reference));
    assert_eq!(test.bytes("values.csv"), reference.bytes("values.csv"));
}

#[test]
fn single_worker_crash_points_resume_to_uninterrupted_result() {
    let reference = reference_run();
    for (point, mode) in [
        ("after_dispatch:3", "value_driven"),
        ("after_received:3", "value_driven"),
        ("after_accept:3", "value_driven"),
        ("snapshot_written:2", "value_driven"),
        ("meta_written:2", "value_driven"),
        ("completed_snapshot:1", "value_driven"),
    ] {
        let test = PoolTest::new();
        let output = run_async(
            &test,
            mode,
            6,
            1,
            &["--snapshot-interval", "2"],
            &[("RSHOGI_SPSA_FAULT", Path::new(point))],
        );
        assert_eq!(output.status.code(), Some(86), "{point}: {}", stderr(&output));
        let output = run_async(
            &test,
            mode,
            6,
            1,
            &["--snapshot-interval", "2", "--resume", "--force-unlock"],
            &[],
        );
        assert_success(&output);
        assert_eq!(final_theta(&test), final_theta(&reference), "{point}");
        assert_eq!(test.bytes("final.params"), reference.bytes("final.params"), "{point}");
        assert_eq!(test.bytes("values.csv"), reference.bytes("values.csv"), "{point}");
        assert_eq!(test.meta()["status"], "completed", "{point}");
    }
}

#[test]
fn second_crash_after_redispatch_replays_superseded_attempt() {
    let reference = reference_run();
    let test = PoolTest::new();
    let args = ["--snapshot-interval", "2"];
    let output = run_async(
        &test,
        "value_driven",
        6,
        1,
        &args,
        &[("RSHOGI_SPSA_FAULT", Path::new("after_dispatch:2"))],
    );
    assert_eq!(output.status.code(), Some(86), "{}", stderr(&output));
    let resume = [args[0], args[1], "--resume", "--force-unlock"];
    let output = run_async(
        &test,
        "value_driven",
        6,
        1,
        &resume,
        &[("RSHOGI_SPSA_FAULT", Path::new("after_received:3"))],
    );
    assert_eq!(output.status.code(), Some(86), "{}", stderr(&output));
    let attempts: Vec<u64> = kinds(&test, "dispatch")
        .iter()
        .filter(|d| d["pair_id"] == 1)
        .map(|d| d["attempt"].as_u64().unwrap())
        .collect();
    assert_eq!(attempts, vec![1, 2], "crash-interrupted attempt is superseded without failure");
    assert_success(&run_async(&test, "value_driven", 6, 1, &resume, &[]));
    assert_eq!(final_theta(&test), final_theta(&reference));
    assert_eq!(test.bytes("values.csv"), reference.bytes("values.csv"));
    assert_eq!(test.meta()["failed_attempts"], 0);
}

#[test]
fn io_errors_and_csv_rebuild_crash_keep_previous_checkpoint() {
    let reference = reference_run();
    for (point, code) in [("snapshot_io:2", Some(1)), ("meta_io:3", Some(1))] {
        let test = PoolTest::new();
        let output = run_async(
            &test,
            "value_driven",
            6,
            1,
            &["--snapshot-interval", "2"],
            &[("RSHOGI_SPSA_FAULT", Path::new(point))],
        );
        assert_eq!(output.status.code(), code, "{point}: {}", stderr(&output));
        assert!(stderr(&output).contains("fault injection"), "{point}");
        assert_eq!(test.meta()["status"], "running", "{point}: meta keeps the previous checkpoint");
        let resume = ["--snapshot-interval", "2", "--resume", "--force-unlock"];
        // CSV の作り直しの途中で落ちても、次の resume で作り直せる。
        let output = run_async(
            &test,
            "value_driven",
            6,
            1,
            &resume,
            &[("RSHOGI_SPSA_FAULT", Path::new("csv_rebuild:1"))],
        );
        assert_eq!(output.status.code(), Some(86), "{point}: {}", stderr(&output));
        assert_success(&run_async(&test, "value_driven", 6, 1, &resume, &[]));
        assert_eq!(final_theta(&test), final_theta(&reference), "{point}");
        assert_eq!(test.bytes("values.csv"), reference.bytes("values.csv"), "{point}");
        assert_eq!(test.bytes("stats.csv"), reference.bytes("stats.csv"), "{point}");
    }
}

#[test]
fn attempt_failed_crash_resumes_without_losing_failure_count() {
    let test = PoolTest::new();
    let output = run_async(
        &test,
        "value_driven",
        2,
        1,
        &["--engine-retries", "0"],
        &[
            ("SPSA_TEST_ENGINE_FAIL_ONCE_MARKER", &test.path("marker")),
            ("RSHOGI_SPSA_FAULT", Path::new("after_attempt_failed:1")),
        ],
    );
    assert_eq!(output.status.code(), Some(86), "{}", stderr(&output));
    let output = run_async(
        &test,
        "value_driven",
        2,
        1,
        &["--engine-retries", "0", "--resume", "--force-unlock"],
        &[],
    );
    assert_success(&output);
    assert_eq!(test.meta()["failed_attempts"], 1);
    assert_eq!(test.bytes("final.params"), reference_run_with_total(2).bytes("final.params"));
}

#[test]
fn multi_worker_crash_resumes_with_consistent_journal() {
    let test = PoolTest::new();
    let output = run_async(
        &test,
        "value_driven",
        12,
        3,
        &["--snapshot-interval", "3"],
        &[("RSHOGI_SPSA_FAULT", Path::new("after_received:7"))],
    );
    assert_eq!(output.status.code(), Some(86), "{}", stderr(&output));
    let output = run_async(
        &test,
        "value_driven",
        12,
        2,
        &["--snapshot-interval", "3", "--resume", "--force-unlock"],
        &[],
    );
    assert_success(&output);
    let accepts: Vec<u64> =
        kinds(&test, "accept").iter().map(|a| a["pair_id"].as_u64().unwrap()).collect();
    let mut sorted = accepts.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, (0..12).collect::<Vec<_>>(), "each pair is applied exactly once");
    let stats = read(&test, "stats.csv");
    let rows: Vec<Vec<&str>> =
        stats.lines().skip(1).map(|line| line.split(',').collect()).collect();
    assert_eq!(rows.len(), 12);
    assert!(rows.iter().all(|row| row[4].parse::<u32>().unwrap() <= 12), "lag <= L\n{stats}");
    let pair_order: Vec<u64> = rows.iter().map(|row| row[1].parse().unwrap()).collect();
    assert_eq!(pair_order, accepts, "stats.csv follows the accept order");
    // 3 回目の起動は journal の全再生だけで終わり、engine を起動しない。
    let spawns_before = std::fs::read_to_string(test.path("spawns")).unwrap().lines().count();
    let output = run_async(&test, "value_driven", 12, 2, &["--resume", "--force-unlock"], &[]);
    assert_success(&output);
    let spawns_after = std::fs::read_to_string(test.path("spawns")).unwrap().lines().count();
    assert_eq!(spawns_before, spawns_after);
    assert_eq!(read(&test, "stats.csv"), stats);
}

#[test]
fn async_and_batch_only_flags_are_rejected_before_touching_run_dir() {
    for (dispatch, extra, flag) in [
        ("async", vec!["--batch-pairs", "2"], "--batch-pairs"),
        ("async", vec!["--early-stop-patience", "0"], "--early-stop-patience"),
        ("async", vec!["--force-schedule"], "--force-schedule"),
        ("async", vec!["--strict-init-check"], "--strict-init-check"),
        ("async", vec!["--stats-csv", "x.csv"], "--stats-csv"),
        ("batch", vec!["--max-lag", "4"], "--max-lag"),
        ("batch", vec!["--snapshot-interval", "4"], "--snapshot-interval"),
    ] {
        let test = PoolTest::new();
        let mut args = vec!["--dispatch", dispatch];
        args.extend(extra);
        let output = test.run_with("value_driven", 2, 1, &args, &[]);
        assert!(!output.status.success());
        assert!(stderr(&output).contains(flag), "{}", stderr(&output));
        assert!(!test.path("run").exists(), "{flag}");
    }
}

#[test]
fn resume_rejects_other_dispatch_mode_and_changed_conditions() {
    let batch = PoolTest::new();
    assert_success(&batch.run_with("value_driven", 2, 1, &["--batch-pairs", "1"], &[]));
    let output = run_async(&batch, "value_driven", 2, 1, &["--resume"], &[]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("batch 専用"), "{}", stderr(&output));

    let async_run = reference_run();
    let output = async_run.run_with("value_driven", 6, 1, &["--batch-pairs", "1", "--resume"], &[]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("--dispatch async"), "{}", stderr(&output));
    for (extra, reason) in [
        (vec!["--byoyomi", "10"], "対局条件が meta と異なります"),
        (vec!["--max-lag", "9"], "--max-lag 9"),
        (vec!["--alpha", "0.6020000000000001"], "schedule"),
    ] {
        let mut args = vec!["--resume"];
        args.extend(extra.iter().copied());
        let output = run_async(&async_run, "value_driven", 6, 1, &args, &[]);
        assert!(!output.status.success(), "{extra:?}");
        assert!(stderr(&output).contains(reason), "{extra:?}: {}", stderr(&output));
    }
    assert_eq!(async_run.meta()["status"], "completed");
}

#[test]
fn downstream_tools_read_async_csv() {
    let test = reference_run();
    let plot = test.path("plot.csv");
    let output = Command::new(env!("CARGO_BIN_EXE_spsa_stats_to_plot_csv"))
        .arg(test.path("run/stats.csv"))
        .arg("--output-csv")
        .arg(&plot)
        .output()
        .unwrap();
    assert_success(&output);
    let data = std::fs::read_to_string(&plot).unwrap();
    let mut lines = data.lines();
    let header: Vec<&str> = lines.next().unwrap().split(',').collect();
    let rows: Vec<Vec<&str>> = lines.map(|line| line.split(',').collect()).collect();
    assert_eq!(rows.len(), 6);
    let column = |name: &str| header.iter().position(|h| *h == name).unwrap();
    assert!(
        rows.iter()
            .all(|row| row[column("mode")] == "async" && row[column("games")] == "2")
    );
    assert!(
        rows.iter()
            .all(|row| row[column("a_t")].is_empty() && row[column("c_t")].is_empty())
    );

    let output = Command::new(env!("CARGO_BIN_EXE_spsa_param_diff"))
        .arg("--current")
        .arg(test.path("run/final.params"))
        .arg("--base")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/spsa/canonical.params"))
        .arg("--param-values-csv")
        .arg(test.path("run/values.csv"))
        .output()
        .unwrap();
    assert_success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("touched="), "{stdout}");
}

#[test]
fn resume_does_not_read_missing_or_broken_state_view() {
    let reference = reference_run();
    for damage in ["missing", "broken"] {
        let test = PoolTest::new();
        let output = run_async(
            &test,
            "value_driven",
            6,
            1,
            &["--snapshot-interval", "2"],
            &[("RSHOGI_SPSA_FAULT", Path::new("after_accept:3"))],
        );
        assert_eq!(output.status.code(), Some(86), "{damage}: {}", stderr(&output));
        let state = test.path("run/state.params");
        if damage == "missing" {
            std::fs::remove_file(&state).unwrap();
        } else {
            std::fs::write(&state, "broken\n").unwrap();
        }
        let output = run_async(
            &test,
            "value_driven",
            6,
            1,
            &["--snapshot-interval", "2", "--resume", "--force-unlock"],
            &[],
        );
        assert_success(&output);
        assert_eq!(final_theta(&test), final_theta(&reference), "{damage}");
        assert_eq!(test.bytes("final.params"), reference.bytes("final.params"), "{damage}");
    }
}

#[test]
fn file_valued_usi_option_is_fingerprinted_after_trimming() {
    let test = PoolTest::new();
    let eval = test.path("eval file.bin");
    std::fs::write(&eval, b"first").unwrap();
    let option = format!("EvalFile= {} ", eval.display());
    let args = ["--usi-option", option.as_str()];
    assert_success(&run_async(&test, "value_driven", 2, 1, &args, &[]));
    let files = test.meta()["fingerprint"]["usi_option_files"].clone();
    assert_eq!(files.as_array().map(Vec::len), Some(1), "{files}");
    std::fs::write(&eval, b"second").unwrap();
    let resume = ["--usi-option", option.as_str(), "--resume"];
    let output = run_async(&test, "value_driven", 2, 1, &resume, &[]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("対局条件が meta と異なります"), "{}", stderr(&output));
}
