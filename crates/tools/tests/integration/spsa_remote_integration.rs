//! `spsa --dispatch async --remote-worker` の結合テスト。worker は同じ機械の別 process
//! (`spsa --worker-stdio`) として起動し、stdin/stdout のやりとり・取りこぼし・再配布を確かめる。

use std::path::Path;

use super::spsa_run_dir_integration::{PoolTest, assert_success};

const SPSA_BIN: &str = env!("CARGO_BIN_EXE_spsa");
const FAKE_ENGINE_BIN: &str = env!("CARGO_BIN_EXE_spsa_test_engine");

/// coordinator と同じ対局条件で worker を起動するコマンド (PoolTest の既定の flag に合わせる)。
fn worker_command(slots: usize, limit: &[&str], extra: &[&str]) -> String {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/spsa");
    let mut words = vec![
        SPSA_BIN.to_owned(),
        "--worker-stdio".into(),
        "--engine-path".into(),
        FAKE_ENGINE_BIN.into(),
        "--startpos-file".into(),
        fixtures.join("startpos.txt").display().to_string(),
        "--threads".into(),
        "1".into(),
        "--hash-mb".into(),
        "16".into(),
        "--concurrency".into(),
        slots.to_string(),
    ];
    words.extend(limit.iter().chain(extra).map(|word| (*word).to_owned()));
    words.iter().map(|word| format!("'{word}'")).collect::<Vec<_>>().join(" ")
}

const NODES: [&str; 2] = ["--nodes", "1000"];

fn run(
    test: &PoolTest,
    mode: &str,
    total: u32,
    limit: &[&str],
    workers: &[String],
    extra: &[&str],
    env: &[(&str, &Path)],
) -> std::process::Output {
    let mut args = vec!["--dispatch", "async"];
    for command in workers {
        args.extend(["--remote-worker", command.as_str()]);
    }
    args.extend_from_slice(extra);
    test.run_with_limit(mode, total, 1, limit, &args, env)
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

#[test]
fn two_remote_workers_finish_the_run() {
    let test = PoolTest::new();
    let workers = [
        worker_command(2, &NODES, &[]),
        worker_command(2, &NODES, &[]),
    ];
    assert_success(&run(&test, "value_driven", 8, &NODES, &workers, &[], &[]));
    let meta = test.meta();
    assert_eq!(meta["status"], "completed");
    assert_eq!(meta["completed_pairs"], 8);
    // 既定の lag 上限は worker の slot の合計の 4 倍。
    assert_eq!(meta["max_lag"], 16);
    let joined = kinds(&test, "worker_joined");
    let names: Vec<&str> = joined.iter().map(|j| j["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["w1", "w2"]);
    assert!(joined.iter().all(|j| j["slots"] == 2 && j.get("calibration").is_none()));
    assert_eq!(kinds(&test, "accept").len(), 8);
    assert_eq!(records(&test, "games.jsonl").len(), 16);
    assert!(test.path("run").join("remote/w1.log").is_file());
}

#[test]
fn worker_with_different_conditions_is_left_out() {
    let test = PoolTest::new();
    let workers = [
        worker_command(1, &NODES, &[]),
        worker_command(1, &NODES, &["--max-moves", "300"]),
    ];
    let output = run(&test, "value_driven", 4, &NODES, &workers, &[], &[]);
    assert_success(&output);
    let stderr = stderr(&output);
    assert!(stderr.contains("対局条件が coordinator と違います"), "{stderr}");
    assert!(stderr.contains("max_moves"), "{stderr}");
    let joined = kinds(&test, "worker_joined");
    assert_eq!(joined.len(), 1);
    assert_eq!(joined[0]["name"], "w1");
    assert_eq!(test.meta()["completed_pairs"], 4);
}

#[test]
fn no_usable_worker_stops_before_creating_async_artifacts() {
    let test = PoolTest::new();
    let output = run(&test, "value_driven", 2, &NODES, &["exit 3".into()], &[], &[]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("対局を始められる remote worker がありません"));
    assert!(!test.path("run").join("journal.jsonl").exists());
}

#[test]
fn pairs_of_a_lost_worker_are_played_again_elsewhere() {
    let test = PoolTest::new();
    // 2 台目は hello・ready・最初の結果の 3 行だけ届けて切れる。
    let workers = [
        worker_command(2, &NODES, &[]),
        format!(
            "{} | {{ for i in 1 2 3; do IFS= read -r line && printf '%s\\n' \"$line\"; done; }}",
            worker_command(2, &NODES, &[])
        ),
    ];
    let output = run(&test, "value_driven", 8, &NODES, &workers, &[], &[]);
    assert_success(&output);
    assert!(stderr(&output).contains("remote worker w2"), "{}", stderr(&output));
    let failed = kinds(&test, "attempt_failed");
    assert!(!failed.is_empty());
    assert!(
        failed.iter().all(|f| f["reason"].as_str().unwrap().contains("w2 lost")),
        "{failed:?}"
    );
    let meta = test.meta();
    assert_eq!(meta["status"], "completed");
    assert_eq!(meta["completed_pairs"], 8);
    // 失敗した attempt も同じ pair の値のまま 1 台目で指し直される。
    for f in &failed {
        let pair = f["pair_id"].as_u64().unwrap();
        let attempts: Vec<_> = kinds(&test, "dispatch")
            .into_iter()
            .filter(|d| d["pair_id"].as_u64() == Some(pair))
            .collect();
        assert!(attempts.len() >= 2);
        assert!(attempts.windows(2).all(|w| w[0]["plus"] == w[1]["plus"]));
    }
}

#[test]
fn each_worker_gets_its_own_nps_scaled_clock() {
    let test = PoolTest::new();
    let limit = ["--btime", "20000", "--binc", "200"];
    let extra = ["--nps-scaling", "--random-startpos", "false"];
    let workers = [
        worker_command(1, &limit, &extra),
        worker_command(1, &limit, &extra),
    ];
    // bench が 1 局面あたり 10 ms で返す nodes。基準 350k に対して十分速く、足切りに掛からない。
    let env = [("SPSA_TEST_ENGINE_BENCH_NODES", Path::new("35000"))];
    assert_success(&run(&test, "fischer_script", 4, &limit, &workers, &extra, &env));
    let calibrations = kinds(&test, "calibration");
    assert_eq!(calibrations.len(), 2);
    for (index, calibration) in calibrations.iter().enumerate() {
        assert_eq!(calibration["index"], index);
        assert!(calibration["host"].as_str().unwrap().starts_with(&format!("w{} ", index + 1)));
    }
    let joined = kinds(&test, "worker_joined");
    let indexes: Vec<_> = joined.iter().map(|j| j["calibration"].as_u64().unwrap()).collect();
    assert_eq!(indexes, [0, 1]);
    assert_eq!(test.meta()["tc_calibrations"].as_array().unwrap().len(), 2);
    for dispatch in kinds(&test, "dispatch") {
        let index = dispatch["clock"]["calibration"].as_u64().unwrap() as usize;
        assert_eq!(dispatch["clock"]["btime"], calibrations[index]["btime"]);
    }
}

#[test]
fn resume_with_remote_workers_appends_new_calibrations() {
    let test = PoolTest::new();
    let limit = ["--btime", "20000", "--binc", "200"];
    let extra = ["--nps-scaling", "--random-startpos", "false"];
    let workers = [
        worker_command(1, &limit, &extra),
        worker_command(1, &limit, &extra),
    ];
    let bench = Path::new("35000");
    let crash = run(
        &test,
        "fischer_script",
        4,
        &limit,
        &workers,
        &extra,
        &[
            ("SPSA_TEST_ENGINE_BENCH_NODES", bench),
            ("RSHOGI_SPSA_FAULT", Path::new("after_accept:1")),
        ],
    );
    assert_eq!(crash.status.code(), Some(86), "{}", stderr(&crash));
    let mut resume = extra.to_vec();
    resume.extend(["--resume", "--force-unlock"]);
    assert_success(&run(
        &test,
        "fischer_script",
        4,
        &limit,
        &workers,
        &resume,
        &[("SPSA_TEST_ENGINE_BENCH_NODES", bench)],
    ));
    assert_eq!(test.meta()["status"], "completed");
    let indexes: Vec<_> = kinds(&test, "worker_joined")
        .iter()
        .map(|j| j["calibration"].as_u64().unwrap())
        .collect();
    assert_eq!(indexes, [0, 1, 2, 3]);
    assert_eq!(test.meta()["tc_calibrations"].as_array().unwrap().len(), 4);
}
