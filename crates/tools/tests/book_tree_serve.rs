use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};
use tools::book_db::{child_position_after_move, strip_ply};

const ROOT: &str = "4k4/9/9/9/9/9/9/9/4K4 b -";
struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn wait_for(mut condition: impl FnMut() -> bool) {
    let start = Instant::now();
    while !condition() {
        assert!(start.elapsed() < Duration::from_secs(35), "serve の状態遷移が期限を超えました");
        std::thread::sleep(Duration::from_millis(30));
    }
}
fn progress(state: &Path) -> Value {
    std::fs::read_to_string(state.join("progress.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null)
}
fn mock_engine() -> &'static Path {
    static MOCK: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    MOCK.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let result = Command::new("rustc")
            .arg("--edition=2024")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/book_tree_engine.rs"))
            .arg("-o")
            .arg(dir.path().join(format!("engine{}", std::env::consts::EXE_SUFFIX)))
            .output()
            .unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
        dir
    })
    .path()
}
fn engine_path() -> std::path::PathBuf {
    mock_engine().join(format!("engine{}", std::env::consts::EXE_SUFFIX))
}
fn prepare_config(config: &Path) -> std::path::PathBuf {
    let data: Value = serde_json::from_slice(&std::fs::read(config).unwrap()).unwrap();
    let mut text = String::new();
    for (name, value) in data.as_object().unwrap() {
        if name == "positions" {
            for (key, rows) in value.as_object().unwrap() {
                let rows: Vec<_> = rows
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|r| format!("{} {} {}", r["move"].as_str().unwrap(), r["cp"], r["depth"]))
                    .collect();
                text.push_str(&format!("{key}\t{}\n", rows.join("|")));
            }
        } else {
            text.push_str(&format!(
                "{name}\t{}\n",
                value.as_str().map(str::to_owned).unwrap_or_else(|| value.to_string())
            ));
        }
    }
    let path = config.with_extension("tsv");
    std::fs::write(&path, text).unwrap();
    path
}
fn start(state: &Path, config: &Path) -> Server {
    start_with_restarts(state, config, 3)
}
fn start_with_restarts(state: &Path, config: &Path, max_restarts: usize) -> Server {
    start_with_multipv(state, config, max_restarts, 3)
}
fn start_with_multipv(state: &Path, config: &Path, max_restarts: usize, multipv: usize) -> Server {
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(state.join("server.log"))
        .unwrap();
    Server(
        Command::new(env!("CARGO_BIN_EXE_book_mine"))
            .args(["tree", "serve", "--state"])
            .arg(state)
            .arg("--engine")
            .arg(engine_path())
            .arg("--engine-option")
            .arg(format!("FixtureFile={}", prepare_config(config).display()))
            .arg("--engine-option")
            .arg(format!("EvalFile={}", config.display()))
            .arg("--multipv")
            .arg(multipv.to_string())
            .args([
                "--nodes",
                "100",
                "--hash",
                "16",
                "--workers",
                "4",
                "--stall-secs",
                "1",
            ])
            .arg("--max-restarts")
            .arg(max_restarts.to_string())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap(),
    )
}
fn fixture(state: &Path) -> std::path::PathBuf {
    let mut positions = BTreeMap::new();
    let root_moves = ["5i4i", "5i6i", "5i5h"];
    positions.insert(
        ROOT.to_string(),
        json!(
            root_moves
                .iter()
                .map(|m| json!({"move":m,"cp":10,"depth":8}))
                .collect::<Vec<_>>()
        ),
    );
    for mv in root_moves {
        let sfen = child_position_after_move(&format!("{ROOT} 1"), mv).unwrap().to_sfen();
        positions.insert(strip_ply(&sfen).to_owned(),json!([{"move":"5a4a","cp":500,"depth":8},{"move":"5a6a","cp":450,"depth":8},{"move":"5a5b","cp":400,"depth":8}]));
    }
    let config = state.join("engine.json");
    std::fs::write(&config,serde_json::to_vec(&json!({"positions":positions,"delay":1.3,"stagnant":true,"crash_once":state.join("crashed"),"log":state.join("protocol.jsonl")})).unwrap()).unwrap();
    config
}
fn add_tree(state: &Path, name: &str) {
    std::fs::write(
        state.parent().unwrap().join(format!("{name}.paths.tsv")),
        format!("{ROOT}\t1\t5i4i\tgame\n"),
    )
    .unwrap();
    std::fs::write(state.join("inbox").join(format!("{name}.toml")),format!("paths='../../{name}.paths.tsv'\nout='../../{name}'\ndepth=1\nopp_n=2\nstop=300\nown='b'\nrounds=0\nextra=['5i5h']\n")).unwrap();
}

#[test]
fn serve_resizes_drains_resumes_deduplicates_and_reports_stalls_and_crashes() {
    let dir = tempfile::tempdir().unwrap();
    let state = &dir.path().join("state");
    std::fs::create_dir(state).unwrap();
    std::fs::create_dir(state.join("inbox")).unwrap();
    let config = fixture(state);
    add_tree(state, "a");
    add_tree(state, "b");
    let mut server = start(state, &config);
    wait_for(|| progress(state)["workers"].as_array().is_some_and(|w| w.len() == 4));
    wait_for(|| state.join("crashed").exists());
    std::fs::write(state.join("workers"), "1").unwrap();
    wait_for(|| progress(state)["workers"].as_array().is_some_and(|w| w.len() == 1));
    std::fs::write(state.join("workers"), "0").unwrap();
    wait_for(|| progress(state)["workers"].as_array().is_some_and(|w| w.is_empty()));
    assert!(server.0.try_wait().unwrap().is_none());
    let alert = std::fs::read_to_string(state.join("ALERT")).unwrap();
    assert!(alert.contains("STALL"));
    assert!(alert.contains("RESTART"));
    let journal_before = std::fs::read_to_string(state.join("journal.jsonl")).unwrap();
    assert!(!journal_before.is_empty());
    std::fs::write(state.join("stop"), "").unwrap();
    wait_for(|| server.0.try_wait().unwrap().is_some());
    assert!(server.0.wait().unwrap().success());
    std::fs::remove_file(state.join("stop")).unwrap();
    std::fs::write(state.join("workers"), "3").unwrap();
    let mut server = start(state, &config);
    wait_for(|| {
        state.parent().unwrap().join("a/own_nodes.tsv").exists()
            && state.parent().unwrap().join("b/own_nodes.tsv").exists()
    });
    assert_eq!(
        std::fs::read_to_string(state.parent().unwrap().join("a/own_nodes.tsv")).unwrap(),
        std::fs::read_to_string(state.parent().unwrap().join("b/own_nodes.tsv")).unwrap()
    );
    add_tree(state, "c");
    wait_for(|| state.parent().unwrap().join("c/own_nodes.tsv").exists());
    wait_for(|| progress(state)["status"] == "IDLE");
    assert!(server.0.try_wait().unwrap().is_none());
    let journal = std::fs::read_to_string(state.join("journal.jsonl")).unwrap();
    let records: Vec<Value> = journal.lines().map(|s| serde_json::from_str(s).unwrap()).collect();
    let keys: BTreeSet<_> = records.iter().map(|r| r["reading"]["key"].as_str().unwrap()).collect();
    assert_eq!(records.len(), keys.len());
    assert_eq!(keys.len(), 4);
    assert!(journal.starts_with(&journal_before));
    std::fs::write(state.join("stop"), "").unwrap();
    wait_for(|| server.0.try_wait().unwrap().is_some());
    assert!(server.0.wait().unwrap().success());
    assert_eq!(std::fs::read_to_string(state.join("STATUS")).unwrap(), "STOPPED\n");
    let log = std::fs::read_to_string(state.join("protocol.jsonl")).unwrap();
    let mut games = BTreeMap::new();
    let mut ready = BTreeMap::new();
    let mut searches = 0;
    for line in log.lines() {
        let row: Value = serde_json::from_str(line).unwrap();
        let pid = row[0].as_u64().unwrap();
        let command = row[1].as_str().unwrap();
        if command == "usinewgame" {
            games.insert(pid, true);
            ready.insert(pid, false);
        }
        if command == "isready" && games.get(&pid) == Some(&true) {
            ready.insert(pid, true);
        }
        if command.starts_with("go ") {
            assert_eq!(games.get(&pid), Some(&true));
            assert_eq!(ready.get(&pid), Some(&true));
            games.insert(pid, false);
            searches += 1;
        }
    }
    assert_eq!(searches, 5);
}

#[test]
fn explicit_restart_and_process_kill_recover_from_only_saved_readings() {
    let dir = tempfile::tempdir().unwrap();
    let state = &dir.path().join("state");
    std::fs::create_dir(state).unwrap();
    std::fs::create_dir(state.join("inbox")).unwrap();
    let config = fixture(state);
    add_tree(state, "a");
    let mut server = start(state, &config);
    let searches = || {
        std::fs::read_to_string(state.join("protocol.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter(|line| line.contains("go nodes"))
            .count()
    };
    wait_for(|| searches() >= 2);
    std::fs::write(state.join("restart/0"), "").unwrap();
    wait_for(|| {
        std::fs::read_to_string(state.join("ALERT"))
            .unwrap_or_default()
            .contains("制御ファイル")
    });
    wait_for(|| {
        std::fs::read_to_string(state.join("journal.jsonl"))
            .unwrap_or_default()
            .lines()
            .count()
            == 1
    });
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    let before = std::fs::read_to_string(state.join("journal.jsonl")).unwrap();
    std::fs::write(state.join("workers"), "3").unwrap();
    let mut server = start(state, &config);
    wait_for(|| state.parent().unwrap().join("a/own_nodes.tsv").exists());
    let after = std::fs::read_to_string(state.join("journal.jsonl")).unwrap();
    assert!(after.starts_with(&before));
    assert_eq!(after.lines().count(), 4);
    let log = std::fs::read_to_string(state.join("protocol.jsonl")).unwrap();
    let root_positions = log
        .lines()
        .filter(|line| line.contains(&format!("position sfen {ROOT} ")))
        .count();
    assert_eq!(root_positions, 3);
    std::fs::write(state.join("stop"), "").unwrap();
    wait_for(|| server.0.try_wait().unwrap().is_some());
    assert!(server.0.wait().unwrap().success());
}

#[test]
fn repeated_engine_exits_hit_restart_limit_and_leave_no_candidates() {
    let dir = tempfile::tempdir().unwrap();
    let state = &dir.path().join("state");
    std::fs::create_dir(state).unwrap();
    std::fs::create_dir(state.join("inbox")).unwrap();
    let config = fixture(state);
    let mut data: Value = serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
    data["crash_always"] = json!(true);
    std::fs::write(&config, serde_json::to_vec(&data).unwrap()).unwrap();
    add_tree(state, "a");
    std::fs::create_dir(state.parent().unwrap().join("a")).unwrap();
    std::fs::write(state.parent().unwrap().join("a/own_nodes.tsv"), "stale").unwrap();
    let mut server = start(state, &config);
    wait_for(|| {
        std::fs::read_to_string(state.parent().unwrap().join("a/STATUS"))
            .is_ok_and(|s| s == "FAIL\n")
    });
    assert!(server.0.try_wait().unwrap().is_none());
    assert!(!state.parent().unwrap().join("a/own_nodes.tsv").exists());
    let alerts = std::fs::read_to_string(state.join("ALERT")).unwrap();
    assert!(alerts.contains("再起動上限"));
    assert_eq!(alerts.lines().filter(|l| l.contains("RESTART worker")).count(), 4);
}

fn fast_fixture(state: &Path) -> std::path::PathBuf {
    let path = fixture(state);
    let mut data: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    data.as_object_mut().unwrap().remove("crash_once");
    data["delay"] = json!(0.05);
    data["stagnant"] = json!(false);
    std::fs::write(&path, serde_json::to_vec(&data).unwrap()).unwrap();
    path
}
fn status(path: &Path) -> String {
    std::fs::read_to_string(path.join("STATUS")).unwrap_or_default()
}
fn stop(state: &Path, server: &mut Server) {
    std::fs::write(state.join("stop"), "").unwrap();
    wait_for(|| server.0.try_wait().unwrap().is_some());
    assert!(server.0.wait().unwrap().success());
}
fn alerts(state: &Path) -> String {
    std::fs::read_to_string(state.join("ALERT")).unwrap_or_default()
}
fn add_root(state: &Path, name: &str, key: &str) {
    std::fs::write(
        state.parent().unwrap().join(format!("{name}.paths.tsv")),
        format!("{key}\t1\t\troot\n"),
    )
    .unwrap();
    std::fs::write(state.join("inbox").join(format!("{name}.toml")), format!(
        "paths='../../{name}.paths.tsv'\nout='../../{name}'\ndepth=0\nopp_n=2\nstop=300\nown='w'\nrounds=0\n")).unwrap();
}

#[test]
fn sparse_and_terminal_positions_finish_and_failed_position_is_isolated() {
    let dir = tempfile::tempdir().unwrap();
    let state = &dir.path().join("state");
    std::fs::create_dir(state).unwrap();
    std::fs::create_dir(state.join("inbox")).unwrap();
    let config = fast_fixture(state);
    let mut data: Value = serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
    let short = "k8/9/RP7/9/9/9/9/9/8K w -";
    let two = "k8/9/R8/9/9/9/9/9/8K w -";
    let terminal = "k8/1G7/RP7/9/9/9/9/9/8K w -";
    let broken = "k8/9/9/9/9/9/9/9/8K w -";
    let crash = "k8/9/9/9/9/9/9/9/8K b -";
    data["positions"][two] =
        json!([{"move":"9a8a","cp":500,"depth":8},{"move":"9a8b","cp":400,"depth":8}]);
    data["positions"][short] = json!([{"move":"9a8a","cp":500,"depth":8}]);
    data["positions"][terminal] = json!([]);
    data["positions"][broken] =
        json!([{"move":"9a8a","cp":0,"depth":8},{"move":"9a8a","cp":0,"depth":8}]);
    data["crash_key"] = json!(crash);
    std::fs::write(&config, serde_json::to_vec(&data).unwrap()).unwrap();
    for (name, key) in [
        ("short", short),
        ("two", two),
        ("terminal", terminal),
        ("broken", broken),
        ("crash", crash),
    ] {
        add_root(state, name, key);
    }
    add_tree(state, "good");
    let mut server = start(state, &config);
    wait_for(|| {
        status(&state.parent().unwrap().join("good")) == "DONE\n"
            && status(&state.parent().unwrap().join("short")) == "DONE\n"
            && status(&state.parent().unwrap().join("two")) == "DONE\n"
            && status(&state.parent().unwrap().join("terminal")) == "DONE\n"
            && status(&state.parent().unwrap().join("broken")) == "FAIL\n"
            && status(&state.parent().unwrap().join("crash")) == "FAIL\n"
    });
    assert!(server.0.try_wait().unwrap().is_none());
    let journal = std::fs::read_to_string(state.join("journal.jsonl")).unwrap();
    let rows: Vec<Value> = journal.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let terminal_row = rows.iter().find(|r| r["reading"]["key"] == terminal).unwrap();
    assert_eq!(terminal_row["reading"]["multipv"], json!([]));
    assert_eq!(terminal_row["reading"]["bestmove"], "resign");
    let protocol = std::fs::read_to_string(state.join("protocol.jsonl")).unwrap();
    assert_eq!(
        protocol
            .lines()
            .filter(|l| l.contains(&format!("position sfen {broken} ")))
            .count(),
        1
    );
    stop(state, &mut server);
}

#[test]
fn inbox_errors_and_invalid_workers_are_reported_once_until_changed() {
    let dir = tempfile::tempdir().unwrap();
    let state = &dir.path().join("state");
    std::fs::create_dir(state).unwrap();
    std::fs::create_dir(state.join("inbox")).unwrap();
    let config = fast_fixture(state);
    std::fs::write(state.join("workers"), "invalid").unwrap();
    std::fs::write(state.join("inbox/bad.toml"), "invalid").unwrap();
    std::fs::write(state.join("inbox/temporary.tmp"), "").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("absent", state.join("inbox/.#x.toml")).unwrap();
    let mut server = start(state, &config);
    wait_for(|| alerts(state).contains("workers が不正") && alerts(state).contains("bad.toml"));
    let before = alerts(state);
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(alerts(state), before);
    std::fs::write(state.join("inbox/bad.toml"), "different invalid").unwrap();
    wait_for(|| alerts(state).lines().filter(|l| l.contains("bad.toml")).count() == 2);
    std::fs::OpenOptions::new()
        .write(true)
        .open(state.join("inbox/bad.toml"))
        .unwrap()
        .set_modified(std::time::SystemTime::now() + Duration::from_secs(1))
        .unwrap();
    wait_for(|| alerts(state).lines().filter(|l| l.contains("bad.toml")).count() == 3);
    add_tree(state, "good");
    std::fs::write(state.join("workers"), "2").unwrap();
    wait_for(|| status(&state.parent().unwrap().join("good")) == "DONE\n");
    assert!(server.0.try_wait().unwrap().is_none());
    stop(state, &mut server);
}

#[test]
fn monitoring_io_failures_recover_and_zero_workers_and_stop_publish_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let state = &dir.path().join("state");
    std::fs::create_dir(state).unwrap();
    std::fs::create_dir(state.join("inbox")).unwrap();
    for name in ["STATUS", "progress.json", "ALERT"] {
        std::fs::create_dir(state.join(name)).unwrap();
    }
    std::fs::write(state.join("workers"), "0").unwrap();
    let config = fast_fixture(state);
    add_tree(state, "a");
    std::fs::create_dir(state.parent().unwrap().join("a")).unwrap();
    std::fs::create_dir(state.parent().unwrap().join("a/STATUS")).unwrap();
    std::fs::write(state.join("inbox/bad.toml"), "invalid").unwrap();
    let mut server = start(state, &config);
    wait_for(|| state.join("accepted.json").exists());
    std::thread::sleep(Duration::from_millis(250));
    assert!(server.0.try_wait().unwrap().is_none());
    for name in ["STATUS", "progress.json", "ALERT"] {
        std::fs::remove_dir(state.join(name)).unwrap();
    }
    std::fs::remove_dir(state.parent().unwrap().join("a/STATUS")).unwrap();
    wait_for(|| {
        progress(state)["status"] == "STOPPED"
            && status(&state.parent().unwrap().join("a")) == "STOPPED\n"
    });
    assert!(progress(state)["queued"].as_u64().unwrap() > 0);
    std::fs::write(state.join("workers"), "1").unwrap();
    wait_for(|| status(&state.parent().unwrap().join("a")) == "DONE\n");
    assert!(state.parent().unwrap().join("a/own_nodes.tsv").exists());
    wait_for(|| alerts(state).contains("bad.toml"));
    std::fs::create_dir(state.parent().unwrap().join("c")).unwrap();
    std::fs::create_dir(state.parent().unwrap().join("c/STATUS")).unwrap();
    add_tree(state, "c");
    wait_for(|| state.parent().unwrap().join("c/own_nodes.tsv").exists());
    assert!(server.0.try_wait().unwrap().is_none());
    std::fs::remove_dir(state.parent().unwrap().join("c/STATUS")).unwrap();
    wait_for(|| status(&state.parent().unwrap().join("c")) == "DONE\n");
    stop(state, &mut server);
}

#[test]
fn replay_rejects_active_output_even_with_a_different_journal() {
    let dir = tempfile::tempdir().unwrap();
    let state = &dir.path().join("state");
    std::fs::create_dir(state).unwrap();
    std::fs::create_dir(state.join("inbox")).unwrap();
    let config = fast_fixture(state);
    add_tree(state, "a");
    let mut server = start(state, &config);
    wait_for(|| status(&state.parent().unwrap().join("a")) == "DONE\n");
    let own = std::fs::read(state.parent().unwrap().join("a/own_nodes.tsv")).unwrap();
    for journal in [state.join("journal.jsonl"), state.join("another-journal")] {
        let result = Command::new(env!("CARGO_BIN_EXE_book_mine"))
            .args(["tree", "replay", "--journal"])
            .arg(journal)
            .arg("--tree")
            .arg(state.join("inbox/a.toml"))
            .output()
            .unwrap();
        assert!(!result.status.success());
    }
    assert_eq!(status(&state.parent().unwrap().join("a")), "DONE\n");
    assert_eq!(std::fs::read(state.parent().unwrap().join("a/own_nodes.tsv")).unwrap(), own);
    stop(state, &mut server);
}

#[test]
fn explicit_restarts_do_not_exhaust_automatic_limit_and_unused_requests_are_removed() {
    let dir = tempfile::tempdir().unwrap();
    let state = &dir.path().join("state");
    std::fs::create_dir(state).unwrap();
    std::fs::create_dir(state.join("inbox")).unwrap();
    std::fs::write(state.join("workers"), "1").unwrap();
    let config = fast_fixture(state);
    let mut data: Value = serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
    data["delay"] = json!(0.8);
    std::fs::write(&config, serde_json::to_vec(&data).unwrap()).unwrap();
    add_tree(state, "a");
    let mut server = start_with_restarts(state, &config, 0);
    for attempt in 0..5 {
        wait_for(|| {
            std::fs::read_to_string(state.join("protocol.jsonl"))
                .unwrap_or_default()
                .lines()
                .filter(|l| l.contains(&format!("position sfen {ROOT} ")))
                .count()
                > attempt
        });
        std::fs::write(state.join("restart/0"), "").unwrap();
        wait_for(|| alerts(state).lines().filter(|l| l.contains("制御ファイル")).count() > attempt);
    }
    wait_for(|| status(&state.parent().unwrap().join("a")) == "DONE\n");
    std::fs::write(state.join("restart/0"), "").unwrap();
    std::fs::write(state.join("restart/7"), "").unwrap();
    wait_for(|| !state.join("restart/0").exists() && !state.join("restart/7").exists());
    assert!(alerts(state).contains("restart 対象の読みがありません"));
    std::fs::write(state.join("workers"), "8").unwrap();
    wait_for(|| progress(state)["workers"].as_array().is_some_and(|w| w.len() == 8));
    add_tree(state, "b");
    wait_for(|| status(&state.parent().unwrap().join("b")) == "DONE\n");
    assert!(!alerts(state).contains("再起動上限"));
    stop(state, &mut server);
}

#[test]
fn untouched_tree_stays_queued_and_stop_marks_unfinished_trees_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let state = &dir.path().join("state");
    std::fs::create_dir(state).unwrap();
    std::fs::create_dir(state.join("inbox")).unwrap();
    std::fs::write(state.join("workers"), "1").unwrap();
    let config = fast_fixture(state);
    let mut data: Value = serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
    data["delay"] = json!(0.6);
    let other = "k8/9/9/9/9/9/9/9/8K w -";
    data["positions"][other] = json!([{"move":"9a8a","cp":500,"depth":8},{"move":"9a9b","cp":400,"depth":8},{"move":"9a8b","cp":300,"depth":8}]);
    std::fs::write(&config, serde_json::to_vec(&data).unwrap()).unwrap();
    add_tree(state, "a");
    add_root(state, "b", other);
    let mut server = start(state, &config);
    wait_for(|| {
        status(&state.parent().unwrap().join("a")) == "RUNNING\n"
            && status(&state.parent().unwrap().join("b")) == "QUEUED\n"
    });
    stop(state, &mut server);
    assert_eq!(status(state), "STOPPED\n");
    assert_eq!(status(&state.parent().unwrap().join("a")), "STOPPED\n");
    assert_eq!(status(&state.parent().unwrap().join("b")), "STOPPED\n");
}

#[test]
fn replay_not_ready_has_nonzero_exit_and_preserves_not_ready_status() {
    let dir = tempfile::tempdir().unwrap();
    let state = &dir.path().join("state");
    std::fs::create_dir(state).unwrap();
    std::fs::create_dir(state.join("inbox")).unwrap();
    let config = fast_fixture(state);
    add_tree(state, "a");
    let tree = state.join("inbox/a.toml");
    let text = std::fs::read_to_string(&tree).unwrap().replace("extra=['5i5h']\n", "");
    std::fs::write(&tree, text).unwrap();
    let mut server = start(state, &config);
    wait_for(|| status(&state.parent().unwrap().join("a")) == "NOT_READY\n");
    stop(state, &mut server);
    let result = Command::new(env!("CARGO_BIN_EXE_book_mine"))
        .args(["tree", "replay", "--journal"])
        .arg(state.join("journal.jsonl"))
        .arg("--tree")
        .arg(tree)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("登録禁止"));
    assert_eq!(status(&state.parent().unwrap().join("a")), "NOT_READY\n");
    assert!(!state.parent().unwrap().join("a/own_nodes.tsv").exists());
}

#[test]
fn inbox_path_collisions_preserve_outputs_and_only_reject_the_new_tree() {
    let dir = tempfile::tempdir().unwrap();
    let state = &dir.path().join("state");
    std::fs::create_dir_all(state.join("inbox")).unwrap();
    let config = fast_fixture(state);
    add_tree(state, "good");
    let mut server = start(state, &config);
    wait_for(|| status(&dir.path().join("good")) == "DONE\n");
    let own = std::fs::read(dir.path().join("good/own_nodes.tsv")).unwrap();
    let good = dir.path().join("good");
    let targets = [
        state.clone(),
        dir.path().to_owned(),
        good.clone(),
        good.join("child"),
    ];
    for (index, out) in targets.iter().enumerate() {
        let path = state.join(format!("inbox/collision{index}.toml"));
        std::fs::write(path, format!("paths='../../good.paths.tsv'\nout='{}'\ndepth=0\nopp_n=1\nstop=300\nown='b'\nrounds=0\n", out.display())).unwrap();
    }
    let input = dir.path().join("input");
    std::fs::create_dir(&input).unwrap();
    std::fs::write(input.join("d0.jsonl"), "protected").unwrap();
    std::fs::write(state.join("inbox/paths-collision.toml"), "paths='../../input/d0.jsonl'\nout='../../input'\ndepth=0\nopp_n=1\nstop=300\nown='b'\nrounds=0\n").unwrap();
    wait_for(|| alerts(state).lines().filter(|l| l.contains("衝突")).count() == 5);
    assert!(!good.join("child").exists());
    assert_eq!(std::fs::read(input.join("d0.jsonl")).unwrap(), b"protected");
    assert_eq!(std::fs::read(good.join("own_nodes.tsv")).unwrap(), own);
    assert_eq!(progress(state)["trees"].as_array().unwrap().len(), 1);
    add_tree(state, "after");
    wait_for(|| status(&dir.path().join("after")) == "DONE\n");
    assert!(server.0.try_wait().unwrap().is_none());
    stop(state, &mut server);
}

#[test]
fn completed_status_is_unchanged_and_deleted_done_and_failed_outputs_stay_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let state = &dir.path().join("state");
    std::fs::create_dir_all(state.join("inbox")).unwrap();
    let config = fast_fixture(state);
    let mut data: Value = serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
    let bad = "k8/9/9/9/9/9/9/9/8K w -";
    data["positions"][bad] = json!([{"move":"1a1b","cp":0,"depth":1}]);
    std::fs::write(&config, serde_json::to_vec(&data).unwrap()).unwrap();
    add_tree(state, "done");
    add_root(state, "failed", bad);
    let mut server = start(state, &config);
    let done = dir.path().join("done");
    let failed = dir.path().join("failed");
    wait_for(|| status(&done) == "DONE\n" && status(&failed) == "FAIL\n");
    let modified = std::fs::metadata(done.join("STATUS")).unwrap().modified().unwrap();
    std::thread::sleep(Duration::from_millis(350));
    assert_eq!(std::fs::metadata(done.join("STATUS")).unwrap().modified().unwrap(), modified);
    std::fs::remove_dir_all(&done).unwrap();
    std::fs::remove_dir_all(&failed).unwrap();
    add_tree(state, "after");
    wait_for(|| status(&dir.path().join("after")) == "DONE\n");
    std::thread::sleep(Duration::from_millis(350));
    assert!(!done.exists());
    assert!(!failed.exists());
    std::fs::remove_dir_all(state.join("inbox")).unwrap();
    wait_for(|| server.0.try_wait().unwrap().is_some());
    assert!(!server.0.wait().unwrap().success());
    assert!(!done.exists());
    assert!(!failed.exists());
}

#[test]
fn serve_accepts_omitted_nonpromotions_and_win_with_legal_moves_as_leaves() {
    use rshogi_core::movegen::{MoveList, generate_legal_all};
    let bishop = "4k4/9/4B4/9/9/9/9/9/4K4 b -";
    for win in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let state = &dir.path().join("state");
        std::fs::create_dir_all(state.join("inbox")).unwrap();
        let config = fast_fixture(state);
        let mut data: Value = serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
        let pos = tools::book_db::position_from_sfen(&format!("{bishop} 1")).unwrap();
        let mut legal = MoveList::new();
        generate_legal_all(&pos, &mut legal);
        assert_eq!(legal.len(), 29);
        let rows: Vec<_> = legal
            .as_slice()
            .iter()
            .take(17)
            .map(|m| json!({"move":m.to_usi(),"cp":500,"depth":8}))
            .collect();
        data["positions"][bishop] = if win { json!([]) } else { json!(rows) };
        if win {
            data["bestmove"] = json!("win");
        }
        std::fs::write(&config, serde_json::to_vec(&data).unwrap()).unwrap();
        add_root(state, "leaf", bishop);
        if win {
            let path = state.join("inbox/leaf.toml");
            let text = std::fs::read_to_string(&path).unwrap().replace("depth=0", "depth=3");
            std::fs::write(path, text).unwrap();
        }
        let mut server = start_with_multipv(state, &config, 3, 29);
        let out = dir.path().join("leaf");
        wait_for(|| status(&out) == "DONE\n");
        let journal = std::fs::read_to_string(state.join("journal.jsonl")).unwrap();
        let rows: Vec<Value> = journal.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0]["reading"]["multipv"].as_array().unwrap().len(),
            if win { 0 } else { 17 }
        );
        if win {
            assert_eq!(rows[0]["reading"]["bestmove"], "win");
        }
        assert!(alerts(state).is_empty());
        stop(state, &mut server);
    }
}

#[cfg(unix)]
#[test]
fn inbox_rejects_output_containing_symlinked_journal_without_losing_readings() {
    let dir = tempfile::tempdir().unwrap();
    let state = &dir.path().join("state");
    std::fs::create_dir_all(state.join("inbox")).unwrap();
    let out = dir.path().join("journal-output");
    std::fs::create_dir(&out).unwrap();
    std::fs::write(out.join("d0.jsonl"), "").unwrap();
    std::fs::write(out.join("own_nodes.tsv"), "protected").unwrap();
    std::os::unix::fs::symlink(out.join("d0.jsonl"), state.join("journal.jsonl")).unwrap();
    let config = fast_fixture(state);
    add_tree(state, "good");
    let mut server = start(state, &config);
    wait_for(|| status(&dir.path().join("good")) == "DONE\n");
    let before = std::fs::read(out.join("d0.jsonl")).unwrap();
    assert!(!before.is_empty());
    std::fs::write(state.join("inbox/collision.toml"), "paths='../../good.paths.tsv'\nout='../../journal-output'\ndepth=0\nopp_n=1\nstop=300\nown='b'\nrounds=0\n").unwrap();
    wait_for(|| alerts(state).contains("衝突"));
    assert_eq!(std::fs::read(out.join("d0.jsonl")).unwrap(), before);
    assert_eq!(std::fs::read(out.join("own_nodes.tsv")).unwrap(), b"protected");
    assert!(!out.join("tree.lock").exists());
    assert!(server.0.try_wait().unwrap().is_none());
    stop(state, &mut server);
}
