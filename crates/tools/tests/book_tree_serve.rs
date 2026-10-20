#![cfg(unix)]

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
fn start(state: &Path, config: &Path) -> Server {
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
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/book_tree_engine.py"))
            .arg("--engine-option")
            .arg(format!("FixtureFile={}", config.display()))
            .arg("--engine-option")
            .arg(format!("EvalFile={}", config.display()))
            .args([
                "--nodes",
                "100",
                "--multipv",
                "3",
                "--hash",
                "16",
                "--workers",
                "4",
                "--stall-secs",
                "1",
            ])
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
    std::fs::write(state.join(format!("{name}.paths.tsv")), format!("{ROOT}\t1\t5i4i\tgame\n"))
        .unwrap();
    std::fs::write(state.join("inbox").join(format!("{name}.toml")),format!("paths='../{name}.paths.tsv'\nout='../{name}'\ndepth=1\nopp_n=2\nstop=300\nown='b'\nrounds=0\nextra=['5i5h']\n")).unwrap();
}

#[test]
fn serve_resizes_drains_resumes_deduplicates_and_reports_stalls_and_crashes() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path();
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
    wait_for(|| state.join("a/own_nodes.tsv").exists() && state.join("b/own_nodes.tsv").exists());
    assert_eq!(
        std::fs::read_to_string(state.join("a/own_nodes.tsv")).unwrap(),
        std::fs::read_to_string(state.join("b/own_nodes.tsv")).unwrap()
    );
    add_tree(state, "c");
    wait_for(|| state.join("c/own_nodes.tsv").exists());
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
    let state = dir.path();
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
    wait_for(|| state.join("a/own_nodes.tsv").exists());
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
    let state = dir.path();
    std::fs::create_dir(state.join("inbox")).unwrap();
    let config = fixture(state);
    let mut data: Value = serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
    data["crash_always"] = json!(true);
    std::fs::write(&config, serde_json::to_vec(&data).unwrap()).unwrap();
    add_tree(state, "a");
    std::fs::create_dir(state.join("a")).unwrap();
    std::fs::write(state.join("a/own_nodes.tsv"), "stale").unwrap();
    let mut server = start(state, &config);
    wait_for(|| server.0.try_wait().unwrap().is_some());
    assert!(!server.0.wait().unwrap().success());
    assert_eq!(std::fs::read_to_string(state.join("STATUS")).unwrap(), "FAIL\n");
    assert!(!state.join("a/own_nodes.tsv").exists());
    let alerts = std::fs::read_to_string(state.join("ALERT")).unwrap();
    assert!(alerts.contains("再起動上限"));
    assert_eq!(alerts.lines().filter(|l| l.contains("RESTART worker")).count(), 4);
}
