//! `spsa --dispatch async --remote-worker` の結合テスト。worker は同じ機械の別 process
//! (`spsa --worker-stdio`) として起動し、stdin/stdout のやりとり・取りこぼし・再配布を確かめる。
#![cfg(unix)]

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
    // ssh 先の shell が stdout に書く行 (motd 等) は読み飛ばす。
    let workers = [
        format!("echo welcome; {}", worker_command(2, &NODES, &[])),
        worker_command(2, &NODES, &[]),
    ];
    let output = run(&test, "value_driven", 8, &NODES, &workers, &[], &[]);
    assert_success(&output);
    assert!(stderr(&output).contains("w1: ignoring non-protocol output: welcome"));
    let meta = test.meta();
    assert_eq!(meta["status"], "completed");
    assert_eq!(meta["completed_pairs"], 8);
    // 既定の lag 上限は worker の slot の合計の 4 倍。
    assert_eq!(meta["max_lag"], 16);
    let joined = kinds(&test, "worker_joined");
    let names: Vec<&str> = joined.iter().map(|j| j["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["w1", "w2"]);
    assert!(joined.iter().all(|j| j["slots"] == 2 && j.get("calibration").is_none()));
    // worker の spsa は coordinator と同じ binary。
    let spsa = joined[0]["spsa_sha256"].as_str().unwrap();
    assert_eq!(spsa.len(), 64);
    assert!(joined.iter().all(|j| j["spsa_sha256"] == spsa));
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
    // 2 台目は hello・ready・最初の結果の 3 行だけ届けて、機械ごと落ちる (process group を止める)。
    let workers = [
        worker_command(2, &NODES, &[]),
        format!(
            "{} | {{ for i in 1 2 3; do IFS= read -r line && printf '%s\\n' \"$line\"; done; kill -KILL 0; }}",
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

#[test]
fn worker_that_cannot_start_reports_its_error() {
    let test = PoolTest::new();
    let broken = worker_command(1, &NODES, &[]).replace(FAKE_ENGINE_BIN, "/nonexistent/engine");
    let workers = [worker_command(1, &NODES, &[]), broken];
    let output = run(&test, "value_driven", 2, &NODES, &workers, &[], &[]);
    assert_success(&output);
    let stderr = stderr(&output);
    assert!(stderr.contains("remote worker w2 () lost:"), "{stderr}");
    assert!(stderr.contains("/nonexistent/engine"), "{stderr}");
    assert_eq!(kinds(&test, "worker_joined").len(), 1);
}

#[test]
fn retried_pair_uses_the_clock_of_the_machine_that_plays_it() {
    let test = PoolTest::new();
    let limit = ["--btime", "20000", "--binc", "200"];
    let extra = ["--nps-scaling", "--random-startpos", "false"];
    let workers = [
        worker_command(1, &limit, &extra),
        format!(
            "{} | {{ for i in 1 2 3; do IFS= read -r line && printf '%s\\n' \"$line\"; done; kill -KILL 0; }}",
            worker_command(2, &limit, &extra)
        ),
    ];
    let env = [("SPSA_TEST_ENGINE_BENCH_NODES", Path::new("35000"))];
    assert_success(&run(&test, "fischer_script", 6, &limit, &workers, &extra, &env));
    let failed = kinds(&test, "attempt_failed");
    assert!(!failed.is_empty());
    let dispatches = kinds(&test, "dispatch");
    for f in &failed {
        let calibrations: Vec<_> = dispatches
            .iter()
            .filter(|d| d["pair_id"] == f["pair_id"])
            .map(|d| d["clock"]["calibration"].as_u64().unwrap())
            .collect();
        // w2 (較正 1) で切れた pair は w1 (較正 0) で指し直す。
        assert_eq!(calibrations.first(), Some(&1), "{calibrations:?}");
        assert_eq!(calibrations.last(), Some(&0), "{calibrations:?}");
    }
}

/// heartbeat の途絶による切断そのもの (init 待ち・bench 中・対局中のどれも) は、timeout の下限
/// (60 秒、init 待ちは 300 秒) のため結合テストにしない。
/// 途絶の判定は stdin の終わりと同じ停止経路を通る (`worker_stops_its_bench_when_the_coordinator_goes_away`、
/// `pairs_of_a_lost_worker_are_played_again_elsewhere`)。
#[test]
fn remote_timeout_must_cover_two_heartbeats() {
    let test = PoolTest::new();
    let workers = [worker_command(1, &NODES, &[])];
    let output =
        run(&test, "value_driven", 2, &NODES, &workers, &["--remote-timeout-sec", "59"], &[]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("--remote-timeout-sec must be >= 60"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn worker_stops_its_bench_when_the_coordinator_goes_away() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    // 1 局面 3 秒の bench (8 局面で 24 秒) の途中で stdin を閉じると、探索中の局面を終えた所で止まる。
    let limit = ["--btime", "20000", "--binc", "200"];
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(worker_command(1, &limit, &["--nps-scaling"]))
        .env("SPSA_TEST_ENGINE_MODE", "fischer_script")
        .env("SPSA_TEST_ENGINE_BENCH_SLEEP_MS", "3000")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut hello = String::new();
    stdout.read_line(&mut hello).unwrap();
    assert!(hello.contains(r#""type":"hello""#), "{hello}");
    let mut stdin = child.stdin.take().unwrap();
    let init = r#"{"type":"init","params":[{"name":"SPSA_TEST_INT","is_int":true}],"application_mask":[true],"theta0":["5.0"],"nps_bench":true,"timeout_sec":300}"#;
    writeln!(stdin, "{init}").unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let closed = Instant::now();
    drop(stdin);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if closed.elapsed() > Duration::from_secs(15) {
            child.kill().unwrap();
            panic!("worker kept running its bench after stdin closed");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(!status.success());
    let mut rest = String::new();
    std::io::Read::read_to_string(&mut stdout, &mut rest).unwrap();
    assert!(rest.contains("coordinator が入力を閉じました"), "{rest}");
}

#[test]
fn resuming_a_completed_run_does_not_start_workers() {
    let test = PoolTest::new();
    let workers = [worker_command(1, &NODES, &[])];
    assert_success(&run(&test, "value_driven", 2, &NODES, &workers, &[], &[]));
    let journal = test.bytes("journal.jsonl");
    // 起動されれば失敗する worker でも、完走済みの run の resume は通り、journal も変わらない。
    let resume = ["--resume", "--force-unlock"];
    assert_success(&run(&test, "value_driven", 2, &NODES, &["exit 3".into()], &resume, &[]));
    assert_eq!(test.bytes("journal.jsonl"), journal);
    assert_eq!(test.meta()["status"], "completed");
}

#[test]
fn local_resume_of_a_remote_run_replays_interrupted_pairs_on_the_local_clock() {
    let test = PoolTest::new();
    let limit = ["--btime", "20000", "--binc", "200"];
    let extra = ["--nps-scaling", "--random-startpos", "false"];
    let workers = [
        worker_command(1, &limit, &extra),
        worker_command(1, &limit, &extra),
    ];
    let bench = Path::new("35000");
    // 最初の適用 (= 最初に記録された結果) の直後に落ちるので、もう 1 台に渡した pair は結果が journal に無く、
    // resume で必ず指し直しになる。
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
        &[],
        &resume,
        &[("SPSA_TEST_ENGINE_BENCH_NODES", bench)],
    ));
    // 較正は w1・w2 の 0・1 と、手元の 2。
    assert_eq!(kinds(&test, "calibration").len(), 3);
    let dispatches = kinds(&test, "dispatch");
    let retried: Vec<_> =
        dispatches.iter().filter(|d| d["attempt"].as_u64().unwrap() > 1).collect();
    assert!(!retried.is_empty());
    assert!(retried.iter().all(|d| d["clock"]["calibration"] == 2), "{retried:?}");
}

#[test]
fn worker_stops_when_the_coordinator_stops_reading_its_output() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    // stdin は開いたまま。出力の pipe が閉じたことだけで止まる (止まらないと sh が pipe を持ち続ける)。
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(format!("exec {}", worker_command(1, &NODES, &[])))
        .env("SPSA_TEST_ENGINE_MODE", "value_driven")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut hello = String::new();
    stdout.read_line(&mut hello).unwrap();
    assert!(hello.contains(r#""type":"hello""#), "{hello}");
    drop(stdout);
    let mut stdin = child.stdin.take().unwrap();
    let init = r#"{"type":"init","params":[{"name":"SPSA_TEST_INT","is_int":true}],"application_mask":[true],"theta0":["5.0"],"nps_bench":false,"timeout_sec":300}"#;
    writeln!(stdin, "{init}").unwrap();
    // 閉じた pipe への書き込みで気づく。並列のテストの fork が pipe を一瞬持っている間の `ready` の書き込みは
    // 通ってしまうことがあるので、対局の結果を書かせて確実に書き込みを起こす。
    let task = r#"{"type":"task","pair_id":0,"attempt":1,"start_pos_index":0,"plus":["5.0"],"minus":["5.0"],"tc":{"black_time":0,"white_time":0,"black_inc":0,"white_inc":0,"byoyomi":0},"game_time_limit_ms":null}"#;
    writeln!(stdin, "{task}").unwrap();
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if start.elapsed() > Duration::from_secs(15) {
            child.kill().unwrap();
            panic!("worker kept running after its output closed");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(!status.success());
    drop(stdin);
}
