//! `spsa --dispatch async` の Fischer 対局 (NPS による時計の補正・1 局の上限・時間切れの率・
//! 両側の評価の一致による裁定) の結合テスト。fake USI engine の `fischer_script` mode を使う。

use std::path::Path;

use super::spsa_run_dir_integration::{PoolTest, assert_success};

const MODE: &str = "fischer_script";

fn run(
    test: &PoolTest,
    total: u32,
    limit: &[&str],
    extra: &[&str],
    env: &[(&str, &Path)],
) -> std::process::Output {
    let mut args = vec!["--dispatch", "async", "--random-startpos", "false"];
    args.extend_from_slice(extra);
    test.run_with_limit(MODE, total, 1, limit, &args, env)
}

fn records(test: &PoolTest, file: &str) -> Vec<serde_json::Value> {
    String::from_utf8(test.bytes(file))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn kinds(test: &PoolTest, kind: &str) -> Vec<serde_json::Value> {
    records(test, "journal.jsonl")
        .into_iter()
        .filter(|r| r["kind"] == kind)
        .collect()
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// bench が 1 局面あたり 10 ms で返す nodes。基準 350k に対して十分速く、足切りに掛からない。
const FAST_BENCH_NODES: &str = "35000";

fn go_btimes(protocol: &Path) -> Vec<String> {
    std::fs::read_to_string(protocol)
        .unwrap()
        .lines()
        .filter_map(|line| line.split_once(" go btime "))
        .map(|(_, rest)| rest.split_whitespace().next().unwrap().to_owned())
        .collect()
}

#[test]
fn nps_scaled_clock_is_recorded_in_meta_journal_and_used_for_every_game() {
    let test = PoolTest::new();
    let protocol = test.path("protocol");
    assert_success(&run(
        &test,
        2,
        &["--btime", "20000", "--binc", "200"],
        &["--nps-scaling"],
        &[
            ("SPSA_TEST_ENGINE_BENCH_NODES", Path::new(FAST_BENCH_NODES)),
            ("SPSA_TEST_ENGINE_PROTOCOL_LOG", &protocol),
        ],
    ));
    let meta = test.meta();
    assert_eq!(meta["fingerprint"]["nps_scaling"]["reference_nps"], 350_000);
    assert_eq!(meta["fingerprint"]["btime"], 20_000);
    let calibrations = meta["tc_calibrations"].as_array().unwrap();
    assert_eq!(calibrations.len(), 1);
    let calibration = &calibrations[0];
    let scale = calibration["scale"].as_f64().unwrap();
    let measured = calibration["measured_nps"].as_f64().unwrap();
    assert!((scale - 350_000.0 / measured).abs() < 1e-12);
    let btime = calibration["btime"].as_u64().unwrap();
    let binc = calibration["binc"].as_u64().unwrap();
    assert_eq!(btime, (20_000.0 * scale).round() as u64);
    assert_eq!(binc, (200.0 * scale).round() as u64);
    assert_eq!(calibration["reference_btime"], 20_000);
    assert_eq!(calibration["slots"], 1);
    assert_eq!(calibration["bench_version"], "spsa-bench-v1");

    let journal_calibrations = kinds(&test, "calibration");
    assert_eq!(journal_calibrations.len(), 1);
    assert_eq!(journal_calibrations[0]["btime"], btime);
    for dispatch in kinds(&test, "dispatch") {
        assert_eq!(
            dispatch["clock"],
            serde_json::json!({"btime": btime, "binc": binc, "calibration": 0})
        );
    }
    // 各局の初手は倍率を掛けた持ち時間から始まる。
    let starts = go_btimes(&protocol).into_iter().filter(|b| *b == btime.to_string()).count();
    assert!(starts >= 4, "{starts}");

    // journal の received と games.jsonl に終了理由と手ごとの時間・nodes が残る。
    for received in kinds(&test, "received") {
        for game in received["games"].as_array().unwrap() {
            assert_eq!(game["reason"], "resign");
            assert_eq!(game["elapsed_ms"].as_array().unwrap().len(), 2);
            let nodes: Vec<u64> =
                game["nodes"].as_array().unwrap().iter().map(|n| n.as_u64().unwrap()).collect();
            assert!(nodes.iter().all(|n| *n >= 1000), "{nodes:?}");
        }
    }
    let games = records(&test, "games.jsonl");
    assert_eq!(games.len(), 4);
    for game in &games {
        assert_eq!(game["reason"], "resign");
        let plies = game["plies"].as_u64().unwrap() as usize;
        assert_eq!(game["elapsed_ms"].as_array().unwrap().len(), plies);
        assert_eq!(game["nodes"].as_array().unwrap().len(), plies);
    }
    assert_eq!(meta["received_games"], 4);
    assert_eq!(meta["timeout_games"], 0);
}

#[test]
fn nps_scaling_rejects_a_slow_machine_before_creating_async_artifacts() {
    let test = PoolTest::new();
    let output = run(
        &test,
        2,
        &["--btime", "20000", "--binc", "200"],
        &["--nps-scaling"],
        &[("SPSA_TEST_ENGINE_BENCH_NODES", Path::new("10"))],
    );
    assert!(!output.status.success());
    assert!(stderr(&output).contains("下回る"), "{}", stderr(&output));
    assert!(!test.path("run").join("meta.json").exists());
    assert!(!test.path("run").join("journal.jsonl").exists());
}

#[test]
fn resume_measures_again_and_keeps_the_clock_of_redispatched_pairs() {
    let test = PoolTest::new();
    let limit = ["--btime", "20000", "--binc", "200"];
    // pair 1 を dispatch した直後に落とす。
    let output = run(
        &test,
        3,
        &limit,
        &["--nps-scaling"],
        &[
            ("SPSA_TEST_ENGINE_BENCH_NODES", Path::new(FAST_BENCH_NODES)),
            ("RSHOGI_SPSA_FAULT", Path::new("after_dispatch:2")),
        ],
    );
    assert!(!output.status.success());
    // 速さの違う機械で resume する (bench の nodes が 4 倍)。
    let protocol = test.path("protocol");
    assert_success(&run(
        &test,
        3,
        &limit,
        &["--nps-scaling", "--resume", "--force-unlock"],
        &[
            ("SPSA_TEST_ENGINE_BENCH_NODES", Path::new("140000")),
            ("SPSA_TEST_ENGINE_PROTOCOL_LOG", &protocol),
        ],
    ));
    let calibrations = kinds(&test, "calibration");
    assert_eq!(calibrations.len(), 2);
    let clock = |i: usize| {
        serde_json::json!({
            "btime": calibrations[i]["btime"],
            "binc": calibrations[i]["binc"],
            "calibration": i,
        })
    };
    assert_ne!(calibrations[0]["btime"], calibrations[1]["btime"]);
    assert_eq!(test.meta()["tc_calibrations"].as_array().unwrap().len(), 2);
    let dispatches = kinds(&test, "dispatch");
    let by_pair = |pair: u64| -> Vec<&serde_json::Value> {
        dispatches.iter().filter(|d| d["pair_id"] == pair).collect()
    };
    assert_eq!(by_pair(0)[0]["clock"], clock(0));
    // 落ちた時点で実行中だった pair 1 は、指し直しも前の時計で指す。新しい pair 2 は測り直した時計。
    let pair1 = by_pair(1);
    assert_eq!(pair1.len(), 2);
    assert!(pair1.iter().all(|d| d["clock"] == clock(0)));
    assert_eq!(by_pair(2)[0]["clock"], clock(1));
    let btimes = go_btimes(&protocol);
    for (i, calibration) in calibrations.iter().enumerate() {
        let start = calibration["btime"].as_u64().unwrap().to_string();
        assert!(btimes.contains(&start), "calibration {i}: {btimes:?}");
    }
}

#[test]
fn resume_rejects_changed_scaling_or_adjudication_conditions() {
    let test = PoolTest::new();
    let limit = ["--btime", "20000", "--binc", "200"];
    let bench = [("SPSA_TEST_ENGINE_BENCH_NODES", Path::new(FAST_BENCH_NODES))];
    assert_success(&run(&test, 1, &limit, &["--nps-scaling"], &bench));
    for extra in [
        vec!["--resume"],
        vec!["--resume", "--nps-scaling", "--reference-nps", "300000"],
        vec![
            "--resume",
            "--nps-scaling",
            "--adjudicate-agreement",
            "movecount=3,score=800",
        ],
    ] {
        let output = run(&test, 1, &limit, &extra, &bench);
        assert!(!output.status.success(), "{extra:?}");
        assert!(
            stderr(&output).contains("対局条件が meta と異なります"),
            "{extra:?}: {}",
            stderr(&output)
        );
    }
    let output = run(
        &test,
        1,
        &["--btime", "10000", "--binc", "200"],
        &["--resume", "--nps-scaling"],
        &bench,
    );
    assert!(!output.status.success());
    assert!(stderr(&output).contains("対局条件が meta と異なります"));
    assert_eq!(test.meta()["status"], "completed");
}

#[test]
fn game_over_the_wall_clock_limit_is_an_infra_error_on_the_same_pair() {
    // 持ち時間 100 ms・加算 0 の上限は (100 × 3) × 2 = 600 ms。1 手 150 ms で、時計は 0 で止まるが
    // 判定マージン (1000 ms) の内なので時間切れにならず、上限を先に超える。
    let test = PoolTest::new();
    let output = run(
        &test,
        1,
        &["--btime", "100"],
        &["--engine-retries", "0", "--max-pair-attempts", "2"],
        &[("SPSA_TEST_ENGINE_MOVE_SLEEP_MS", Path::new("150"))],
    );
    assert!(!output.status.success());
    let meta = test.meta();
    assert_eq!(meta["status"], "stopped");
    assert!(meta["stop_reason"].as_str().unwrap().contains("failed 2 attempt"), "{meta}");
    let failed = kinds(&test, "attempt_failed");
    assert_eq!(failed.len(), 2);
    assert!(failed.iter().all(|f| f["reason"].as_str().unwrap().contains("game_time_limit")));
    let dispatches = kinds(&test, "dispatch");
    assert_eq!(dispatches.len(), 2);
    for key in ["plus", "minus", "start_pos_index", "dispatch_k"] {
        assert_eq!(dispatches[0][key], dispatches[1][key], "{key}");
    }
    assert!(kinds(&test, "received").is_empty());
}

#[test]
fn timeouts_are_results_and_too_many_stop_the_run() {
    // 1 手 300 ms に対して持ち時間 50 ms・マージン 20 ms なので、各局の初手が時間切れになる。
    let test = PoolTest::new();
    let output = run(
        &test,
        3,
        &["--btime", "50"],
        &[
            "--timeout-margin-ms",
            "20",
            "--max-timeout-rate",
            "0.5",
            "--timeout-rate-min-games",
            "2",
        ],
        &[("SPSA_TEST_ENGINE_MOVE_SLEEP_MS", Path::new("300"))],
    );
    assert!(!output.status.success());
    let meta = test.meta();
    assert_eq!(meta["status"], "stopped");
    assert!(meta["stop_reason"].as_str().unwrap().contains("timeout rate"), "{meta}");
    assert_eq!(meta["received_games"], 2);
    assert_eq!(meta["timeout_games"], 2);
    // 時間切れの 2 局は普通の結果として θ の更新に入る。
    assert_eq!(kinds(&test, "accept").len(), 1);
    let received = kinds(&test, "received");
    let reasons: Vec<&str> = received[0]["games"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["reason"].as_str().unwrap())
        .collect();
    assert_eq!(reasons, ["timeout", "timeout"]);
    let scores: Vec<i64> = received[0]["games"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["plus_score"].as_i64().unwrap())
        .collect();
    assert_eq!(scores, [-1, 1]);
}

#[test]
fn agreement_adjudication_ends_games_and_is_off_by_default() {
    let limit = ["--btime", "10000", "--binc", "100"];
    let score = [("SPSA_TEST_ENGINE_SCORE", Path::new("1000"))];
    let on = PoolTest::new();
    assert_success(&run(
        &on,
        1,
        &limit,
        &["--adjudicate-agreement", "movecount=2,score=800"],
        &score,
    ));
    assert_eq!(
        on.meta()["fingerprint"]["adjudicate_agreement"],
        serde_json::json!({"movecount": 2, "score": 800})
    );
    for game in records(&on, "games.jsonl") {
        // 先手 +1000 / 後手 -1000 が両側 2 手ずつ揃う 4 手目で先手勝ち。
        assert_eq!(game["reason"], "adjudication_agreement");
        assert_eq!(game["plies"], 4);
        assert_eq!(game["outcome"], "black_win");
    }
    let off = PoolTest::new();
    assert_success(&run(&off, 1, &limit, &[], &score));
    assert!(off.meta()["fingerprint"]["adjudicate_agreement"].is_null());
    for game in records(&off, "games.jsonl") {
        assert_eq!(game["reason"], "resign");
    }
}

#[test]
fn async_fischer_only_flags_are_rejected_elsewhere() {
    for (args, limit, needle) in [
        (
            vec!["--batch-pairs", "1", "--nps-scaling"],
            vec!["--btime", "1000"],
            "--nps-scaling",
        ),
        (
            vec![
                "--batch-pairs",
                "1",
                "--adjudicate-agreement",
                "movecount=2,score=800",
            ],
            vec!["--btime", "1000"],
            "--adjudicate-agreement",
        ),
        (
            vec!["--batch-pairs", "1", "--max-timeout-rate", "0.1"],
            vec!["--btime", "1000"],
            "--max-timeout-rate",
        ),
        (vec!["--dispatch", "async", "--nps-scaling"], vec!["--nodes", "1000"], "Fischer"),
        (
            vec!["--dispatch", "async", "--reference-nps", "1"],
            vec!["--btime", "1000"],
            "--nps-scaling",
        ),
        (
            vec!["--dispatch", "async", "--max-timeout-rate", "2"],
            vec!["--btime", "1000"],
            "[0, 1]",
        ),
    ] {
        let test = PoolTest::new();
        let output = test.run_with_limit(MODE, 1, 1, &limit, &args, &[]);
        assert!(!output.status.success(), "{args:?}");
        assert!(stderr(&output).contains(needle), "{args:?}: {}", stderr(&output));
        assert!(!test.path("run").exists(), "{args:?}");
    }
}
