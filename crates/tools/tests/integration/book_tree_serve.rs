use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};
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
    Server(
        serve_command(state, config, max_restarts, multipv, state.parent().unwrap())
            .spawn()
            .unwrap(),
    )
}
fn serve_command(
    state: &Path,
    config: &Path,
    max_restarts: usize,
    multipv: usize,
    out_root: &Path,
) -> Command {
    serve_command_with_engine(state, config, max_restarts, multipv, out_root, &engine_path())
}
fn serve_command_with_engine(
    state: &Path,
    config: &Path,
    max_restarts: usize,
    multipv: usize,
    out_root: &Path,
    engine: &Path,
) -> Command {
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(state.join("server.log"))
        .unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_book_mine"));
    command
        .args(["tree", "serve", "--state"])
        .arg(state)
        .arg("--engine")
        .arg(engine)
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
        .arg("--out-root")
        .arg(out_root)
        .stdout(log.try_clone().unwrap())
        .stderr(log);
    command
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
fn out(state: &Path, name: &str, own: &str) -> PathBuf {
    state.parent().unwrap().join(name).join(own).join("g1")
}
fn book_mine(args: &[&str], state: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_book_mine"))
        .args(["tree"])
        .args(args)
        .arg("--state")
        .arg(state)
        .output()
        .unwrap()
}
fn try_enqueue(state: &Path, lines: &[Value]) -> Output {
    let file = state.parent().unwrap().join("lines.jsonl");
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(&file, text).unwrap();
    book_mine(&["enqueue", "--from", file.to_str().unwrap()], state)
}
fn enqueue(state: &Path, line: Value) -> String {
    let result = try_enqueue(state, &[line]);
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    String::from_utf8(result.stdout).unwrap().trim().to_owned()
}
fn tree_line(name: &str) -> Value {
    json!({"id":name,"root":format!("sfen {ROOT} 1"),"own":"b","depth":1,"game":[["5i4i"]],
        "opts":{"opp_n":2,"stop":300,"rounds":0,"extra":["5i5h"]}})
}
fn add_tree(state: &Path, name: &str) -> String {
    enqueue(state, tree_line(name))
}
fn root_line(name: &str, key: &str) -> Value {
    json!({"id":name,"root":format!("sfen {key} 1"),"own":"w","depth":1,"opts":{"opp_n":2,"stop":300,"rounds":0}})
}
fn add_root(state: &Path, name: &str, key: &str) -> String {
    enqueue(state, root_line(name, key))
}
fn targets_status(state: &Path) -> Value {
    std::fs::read_to_string(state.join("targets.status.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null)
}
fn wait_cmd(state: &Path, receipt: &str, ids: Option<&str>) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_book_mine"));
    command
        .args(["tree", "wait", "--state"])
        .arg(state)
        .arg("--receipt")
        .arg(receipt);
    if let Some(ids) = ids {
        command.arg("--ids").arg(ids);
    }
    command
}
/// serve が書いた記録の手順で、同じ出力先を指す replay 用の定義を作る。
fn replay_toml(state: &Path, name: &str, extra: &str) -> PathBuf {
    let parent = state.parent().unwrap();
    let paths = parent.join(format!("{name}.paths.tsv"));
    std::fs::copy(out(state, name, "b").join("paths.record.tsv"), &paths).unwrap();
    let toml = parent.join(format!("{name}.toml"));
    std::fs::write(
        &toml,
        format!(
            "paths='{}'\nout='{}'\ndepth=1\nopp_n=2\nstop=300\nown='b'\nrounds=0\n{extra}",
            paths.display(),
            out(state, name, "b").display()
        ),
    )
    .unwrap();
    toml
}
/// 起動に失敗するはずの serve を、常駐してしまっても待ち続けないよう期限付きで待つ。
fn exits_with_failure(mut command: Command) -> bool {
    let mut child = Server(command.spawn().unwrap());
    wait_for(|| child.0.try_wait().unwrap().is_some());
    !child.0.wait().unwrap().success()
}
fn searches(state: &Path) -> usize {
    std::fs::read_to_string(state.join("protocol.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains("go nodes"))
        .count()
}

#[test]
fn serve_resizes_drains_resumes_deduplicates_and_reports_stalls_and_crashes() {
    let dir = tempfile::tempdir().unwrap();
    let state = &dir.path().join("state");
    std::fs::create_dir(state).unwrap();
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
        out(state, "a", "b").join("own_nodes.tsv").exists()
            && out(state, "b", "b").join("own_nodes.tsv").exists()
    });
    assert_eq!(
        std::fs::read_to_string(out(state, "a", "b").join("own_nodes.tsv")).unwrap(),
        std::fs::read_to_string(out(state, "b", "b").join("own_nodes.tsv")).unwrap()
    );
    add_tree(state, "c");
    wait_for(|| out(state, "c", "b").join("own_nodes.tsv").exists());
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
    assert!(std::fs::read_to_string(state.join("STATUS")).unwrap().starts_with("STOPPED "));
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
    wait_for(|| out(state, "a", "b").join("own_nodes.tsv").exists());
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
    let config = fixture(state);
    let mut data: Value = serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
    data["crash_always"] = json!(true);
    std::fs::write(&config, serde_json::to_vec(&data).unwrap()).unwrap();
    add_tree(state, "a");
    std::fs::create_dir_all(out(state, "a", "b")).unwrap();
    std::fs::write(out(state, "a", "b").join("own_nodes.tsv"), "stale").unwrap();
    let mut server = start(state, &config);
    wait_for(|| {
        std::fs::read_to_string(out(state, "a", "b").join("STATUS")).is_ok_and(|s| s == "FAIL\n")
    });
    assert!(server.0.try_wait().unwrap().is_none());
    assert!(!out(state, "a", "b").join("own_nodes.tsv").exists());
    // ALERT は同じ周回の終わりに書き出すので、STATUS の後に見えることがある。
    wait_for(|| self::alerts(state).contains("再起動上限"));
    let alerts = std::fs::read_to_string(state.join("ALERT")).unwrap();
    assert_eq!(alerts.lines().filter(|l| l.contains("RESTART worker")).count(), 4);
}

fn fast_fixture(state: &Path) -> std::path::PathBuf {
    let path = fixture(state);
    let mut data: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    data.as_object_mut().unwrap().remove("crash_once");
    data["delay"] = json!(0.05);
    data["stagnant"] = json!(false);
    data["unknown_resign"] = json!(true);
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

#[test]
fn sparse_and_terminal_positions_finish_and_failed_position_is_isolated() {
    let dir = tempfile::tempdir().unwrap();
    let state = &dir.path().join("state");
    std::fs::create_dir(state).unwrap();
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
        status(&out(state, "good", "b")) == "DONE\n"
            && status(&out(state, "short", "w")) == "DONE\n"
            && status(&out(state, "two", "w")) == "DONE\n"
            && status(&out(state, "terminal", "w")) == "DONE\n"
            && status(&out(state, "broken", "w")) == "FAIL\n"
            && status(&out(state, "crash", "w")) == "FAIL\n"
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
fn queue_rejections_partial_lines_and_invalid_workers_are_reported_once() {
    let dir = tempfile::tempdir().unwrap();
    let state = &dir.path().join("state");
    std::fs::create_dir(state).unwrap();
    let config = fast_fixture(state);
    std::fs::write(state.join("workers"), "invalid").unwrap();
    let bad = "invalid\n{\"id\":\"x\",\"root\":\"startpos\",\"own\":\"b\",\"depth\":0}\n";
    std::fs::write(state.join("targets.jsonl"), format!("{bad}{{\"id\":\"partial\"")).unwrap();
    let mut server = start(state, &config);
    wait_for(|| {
        alerts(state).contains("workers が不正")
            && alerts(state).lines().filter(|l| l.contains("キューの行を拒否")).count() == 2
    });
    let before = alerts(state);
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(alerts(state), before);
    let current = targets_status(state);
    assert_eq!(current["processed_bytes"], bad.len());
    assert_eq!(current["rejected"].as_array().unwrap().len(), 2);
    assert_eq!(current["rejected"][1]["id"], "x");
    assert!(current["targets"].as_array().unwrap().is_empty());
    assert!(!try_enqueue(state, &[tree_line("good")]).status.success());
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(state.join("targets.jsonl"))
        .unwrap();
    std::io::Write::write_all(
        &mut file,
        format!(",\"root\":\"sfen {ROOT} 1\",\"own\":\"w\",\"depth\":1}}\n").as_bytes(),
    )
    .unwrap();
    wait_for(|| targets_status(state)["targets"].as_array().is_some_and(|t| t.len() == 1));
    add_tree(state, "good");
    std::fs::write(state.join("workers"), "2").unwrap();
    wait_for(|| status(&out(state, "good", "b")) == "DONE\n");
    assert!(server.0.try_wait().unwrap().is_none());
    stop(state, &mut server);
}

#[test]
fn monitoring_io_failures_recover_and_zero_workers_and_stop_publish_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let state = &dir.path().join("state");
    std::fs::create_dir(state).unwrap();
    for name in ["STATUS", "progress.json", "ALERT"] {
        std::fs::create_dir(state.join(name)).unwrap();
    }
    std::fs::write(state.join("workers"), "0").unwrap();
    let config = fast_fixture(state);
    add_tree(state, "a");
    std::fs::create_dir_all(out(state, "a", "b").join("STATUS")).unwrap();
    let mut queue = std::fs::OpenOptions::new()
        .append(true)
        .open(state.join("targets.jsonl"))
        .unwrap();
    std::io::Write::write_all(&mut queue, b"invalid\n").unwrap();
    let mut server = start(state, &config);
    wait_for(|| state.join("accepted.json").exists());
    std::thread::sleep(Duration::from_millis(250));
    assert!(server.0.try_wait().unwrap().is_none());
    for name in ["STATUS", "progress.json", "ALERT"] {
        std::fs::remove_dir(state.join(name)).unwrap();
    }
    std::fs::remove_dir(out(state, "a", "b").join("STATUS")).unwrap();
    wait_for(|| {
        progress(state)["status"] == "STOPPED" && status(&out(state, "a", "b")) == "STOPPED\n"
    });
    assert!(progress(state)["queued"].as_u64().unwrap() > 0);
    std::fs::write(state.join("workers"), "1").unwrap();
    wait_for(|| status(&out(state, "a", "b")) == "DONE\n");
    assert!(out(state, "a", "b").join("own_nodes.tsv").exists());
    wait_for(|| alerts(state).contains("キューの行を拒否"));
    std::fs::create_dir_all(out(state, "c", "b").join("STATUS")).unwrap();
    add_tree(state, "c");
    wait_for(|| out(state, "c", "b").join("own_nodes.tsv").exists());
    assert!(server.0.try_wait().unwrap().is_none());
    std::fs::remove_dir(out(state, "c", "b").join("STATUS")).unwrap();
    wait_for(|| status(&out(state, "c", "b")) == "DONE\n");
    stop(state, &mut server);
}

#[test]
fn replay_rejects_active_output_even_with_a_different_journal() {
    let dir = tempfile::tempdir().unwrap();
    let state = &dir.path().join("state");
    std::fs::create_dir(state).unwrap();
    let config = fast_fixture(state);
    add_tree(state, "a");
    let mut server = start(state, &config);
    wait_for(|| status(&out(state, "a", "b")) == "DONE\n");
    let own = std::fs::read(out(state, "a", "b").join("own_nodes.tsv")).unwrap();
    for journal in [state.join("journal.jsonl"), state.join("another-journal")] {
        let result = Command::new(env!("CARGO_BIN_EXE_book_mine"))
            .args(["tree", "replay", "--journal"])
            .arg(journal)
            .arg("--tree")
            .arg(replay_toml(state, "a", "extra=['5i5h']\n"))
            .output()
            .unwrap();
        assert!(!result.status.success());
    }
    assert_eq!(status(&out(state, "a", "b")), "DONE\n");
    assert_eq!(std::fs::read(out(state, "a", "b").join("own_nodes.tsv")).unwrap(), own);
    stop(state, &mut server);
}

#[test]
fn explicit_restarts_do_not_exhaust_automatic_limit_and_unused_requests_are_removed() {
    let dir = tempfile::tempdir().unwrap();
    let state = &dir.path().join("state");
    std::fs::create_dir(state).unwrap();
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
    wait_for(|| status(&out(state, "a", "b")) == "DONE\n");
    std::fs::write(state.join("restart/0"), "").unwrap();
    std::fs::write(state.join("restart/7"), "").unwrap();
    wait_for(|| !state.join("restart/0").exists() && !state.join("restart/7").exists());
    wait_for(|| alerts(state).contains("restart 対象の読みがありません"));
    std::fs::write(state.join("workers"), "8").unwrap();
    wait_for(|| progress(state)["workers"].as_array().is_some_and(|w| w.len() == 8));
    add_tree(state, "b");
    wait_for(|| status(&out(state, "b", "b")) == "DONE\n");
    assert!(!alerts(state).contains("再起動上限"));
    stop(state, &mut server);
}

#[test]
fn untouched_tree_stays_queued_and_stop_marks_unfinished_trees_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let state = &dir.path().join("state");
    std::fs::create_dir(state).unwrap();
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
        status(&out(state, "a", "b")) == "RUNNING\n" && status(&out(state, "b", "w")) == "QUEUED\n"
    });
    stop(state, &mut server);
    assert!(status(state).starts_with("STOPPED "));
    assert_eq!(status(&out(state, "a", "b")), "STOPPED\n");
    assert_eq!(status(&out(state, "b", "w")), "STOPPED\n");
}

#[test]
fn replay_not_ready_has_nonzero_exit_and_preserves_not_ready_status() {
    let dir = tempfile::tempdir().unwrap();
    let state = &dir.path().join("state");
    std::fs::create_dir(state).unwrap();
    let config = fast_fixture(state);
    let mut line = tree_line("a");
    line["opts"].as_object_mut().unwrap().remove("extra");
    enqueue(state, line);
    let mut server = start(state, &config);
    wait_for(|| status(&out(state, "a", "b")) == "NOT_READY\n");
    stop(state, &mut server);
    let tree = replay_toml(state, "a", "");
    let result = Command::new(env!("CARGO_BIN_EXE_book_mine"))
        .args(["tree", "replay", "--journal"])
        .arg(state.join("journal.jsonl"))
        .arg("--tree")
        .arg(tree)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("登録禁止"));
    assert_eq!(status(&out(state, "a", "b")), "NOT_READY\n");
    assert!(!out(state, "a", "b").join("own_nodes.tsv").exists());
}

#[test]
fn out_root_collisions_stop_startup_and_unsafe_ids_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let state = &dir.path().join("state");
    std::fs::create_dir_all(state).unwrap();
    let config = fast_fixture(state);
    for out_root in [state.clone(), state.join("child")] {
        assert!(exits_with_failure(serve_command(state, &config, 3, 3, &out_root)));
        assert!(std::fs::read_to_string(state.join("server.log")).unwrap().contains("衝突"));
        assert!(!state.join("accepted.json").exists());
    }
    for id in ["../x", "a/b", "", &"x".repeat(65)] {
        let mut line = tree_line("good");
        line["id"] = json!(id);
        assert!(!try_enqueue(state, &[line]).status.success());
        assert!(!state.join("targets.jsonl").exists());
    }
    let mut server = start(state, &config);
    let receipt = add_tree(state, "good");
    assert!(wait_cmd(state, &receipt, None).output().unwrap().status.success());
    // 出力の根は state の親でもよいが、state と重なる出力先の id は拒否する。
    let receipt = add_tree(state, "state");
    let result = wait_cmd(state, &receipt, None).output().unwrap();
    assert!(String::from_utf8_lossy(&result.stderr).contains("衝突"));
    let receipt = add_tree(state, "GOOD");
    let result = wait_cmd(state, &receipt, None).output().unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("大小文字"));
    // 同じ出力の根を別の state の serve は使えない。
    let second = dir.path().join("second");
    std::fs::create_dir(&second).unwrap();
    assert!(exits_with_failure(serve_command(&second, &config, 3, 3, dir.path())));
    assert!(std::fs::read_to_string(second.join("ALERT")).unwrap().contains("出力の根"));
    stop(state, &mut server);
    assert!(exits_with_failure(serve_command(
        state,
        &config,
        3,
        3,
        &dir.path().join("other")
    )));
    assert!(std::fs::read_to_string(state.join("server.log")).unwrap().contains("固定"));
    #[cfg(unix)]
    {
        let lock = dir.path().join("serve.lock");
        std::fs::remove_file(&lock).unwrap();
        std::os::unix::fs::symlink(dir.path().join("elsewhere"), &lock).unwrap();
        let result = Command::new(env!("CARGO_BIN_EXE_book_mine"))
            .args(["tree", "replay", "--journal"])
            .arg(state.join("journal.jsonl"))
            .arg("--tree")
            .arg(replay_toml(state, "good", ""))
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("通常のファイル"));
        assert!(exits_with_failure(serve_command(state, &config, 3, 3, dir.path())));
        assert!(
            std::fs::read_to_string(state.join("server.log"))
                .unwrap()
                .contains("通常のファイル")
        );
    }
}

#[test]
fn completed_status_is_unchanged_and_deleted_done_and_failed_outputs_stay_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let state = &dir.path().join("state");
    std::fs::create_dir_all(state).unwrap();
    let config = fast_fixture(state);
    let mut data: Value = serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
    let bad = "k8/9/9/9/9/9/9/9/8K w -";
    data["positions"][bad] = json!([{"move":"1a1b","cp":0,"depth":1}]);
    std::fs::write(&config, serde_json::to_vec(&data).unwrap()).unwrap();
    add_tree(state, "done");
    add_root(state, "failed", bad);
    let mut server = start(state, &config);
    let done = out(state, "done", "b");
    let failed = out(state, "failed", "w");
    wait_for(|| status(&done) == "DONE\n" && status(&failed) == "FAIL\n");
    let modified = std::fs::metadata(done.join("STATUS")).unwrap().modified().unwrap();
    std::thread::sleep(Duration::from_millis(350));
    assert_eq!(std::fs::metadata(done.join("STATUS")).unwrap().modified().unwrap(), modified);
    std::fs::remove_dir_all(&done).unwrap();
    std::fs::remove_dir_all(&failed).unwrap();
    add_tree(state, "after");
    wait_for(|| status(&out(state, "after", "b")) == "DONE\n");
    std::thread::sleep(Duration::from_millis(350));
    assert!(!done.exists());
    assert!(!failed.exists());
    std::fs::remove_dir_all(state.join("restart")).unwrap();
    wait_for(|| server.0.try_wait().unwrap().is_some());
    assert!(!server.0.wait().unwrap().success());
    assert!(!done.exists());
    assert!(!failed.exists());
}

#[test]
fn serve_accepts_omitted_nonpromotions_and_win_with_legal_moves_as_leaves() {
    use rshogi_core::movegen::{MoveList, generate_legal_all};
    let bishop = "4k4/9/4B4/9/9/9/9/9/4K4 b -";
    for (win, win_row) in [(false, false), (true, false), (true, true)] {
        let dir = tempfile::tempdir().unwrap();
        let state = &dir.path().join("state");
        std::fs::create_dir_all(state).unwrap();
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
        data["positions"][bishop] = if win_row {
            json!([{"move":"win", "cp":30000, "depth":1}])
        } else if win {
            json!([])
        } else {
            json!(rows)
        };
        if win {
            data["bestmove"] = json!("win");
        }
        std::fs::write(&config, serde_json::to_vec(&data).unwrap()).unwrap();
        let mut line = root_line("leaf", bishop);
        line["depth"] = json!(if win { 3 } else { 1 });
        enqueue(state, line);
        let mut server = start_with_multipv(state, &config, 3, 29);
        wait_for(|| status(&out(state, "leaf", "w")) == "DONE\n");
        let journal = std::fs::read_to_string(state.join("journal.jsonl")).unwrap();
        let rows: Vec<Value> = journal.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        // 宣言勝ちの根は葉、指し手のある根は相手の上位 2 手 (未知の局面は投了の葉) を伸ばす。
        assert_eq!(rows.len(), if win { 1 } else { 3 });
        let root = rows.iter().find(|r| r["reading"]["key"] == bishop).unwrap();
        assert_eq!(
            root["reading"]["multipv"].as_array().unwrap().len(),
            if win_row {
                1
            } else if win {
                0
            } else {
                17
            }
        );
        if win {
            assert_eq!(root["reading"]["bestmove"], "win");
        }
        assert!(alerts(state).is_empty());
        stop(state, &mut server);
    }
}

#[cfg(unix)]
#[test]
fn serve_rejects_symlinked_journal_before_changing_outputs() {
    for exists in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let state = &dir.path().join("state");
        std::fs::create_dir_all(state).unwrap();
        let target = dir.path().join("target.jsonl");
        if exists {
            std::fs::write(&target, "preserve").unwrap();
        }
        std::os::unix::fs::symlink(&target, state.join("journal.jsonl")).unwrap();
        let config = fast_fixture(state);
        add_tree(state, "good");
        let mut server = start(state, &config);
        wait_for(|| server.0.try_wait().unwrap().is_some());
        assert!(!server.0.wait().unwrap().success());
        assert!(!dir.path().join("good").exists());
        assert!(!state.join("targets.status.json").exists());
        assert!(!state.join("lock").exists());
        if exists {
            assert_eq!(std::fs::read_to_string(&target).unwrap(), "preserve");
        } else {
            assert!(!target.exists());
        }
    }
}

#[cfg(unix)]
#[test]
fn queue_and_restart_links_and_fifos_are_alerted_without_stopping_serve() {
    let dir = tempfile::tempdir().unwrap();
    let state = &dir.path().join("state");
    std::fs::create_dir_all(state).unwrap();
    let config = fast_fixture(state);
    let elsewhere = dir.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    add_root(&elsewhere.join("state"), "valid", ROOT);
    std::os::unix::fs::symlink(elsewhere.join("state/targets.jsonl"), state.join("targets.jsonl"))
        .unwrap();
    // 書き手のいない FIFO を開くと止まるので、キューも restart もリンク・FIFO は開かずに拒否する。
    let fifo = dir.path().join("fifo");
    assert!(Command::new("mkfifo").arg(&fifo).status().unwrap().success());
    std::fs::create_dir_all(state.join("restart")).unwrap();
    std::os::unix::fs::symlink(&fifo, state.join("restart/0")).unwrap();
    assert!(!try_enqueue(state, &[tree_line("good")]).status.success());
    assert!(exits_with_failure(serve_command(state, &config, 3, 3, dir.path())));
    assert!(
        std::fs::read_to_string(state.join("server.log"))
            .unwrap()
            .contains("シンボリックリンク")
    );
    assert!(!state.join("targets.status.json").exists());
    std::fs::remove_file(state.join("targets.jsonl")).unwrap();
    let mut server = start(state, &config);
    wait_for(|| alerts(state).contains("restart/0"));
    assert!(std::fs::symlink_metadata(state.join("restart/0")).is_err());
    assert!(
        Command::new("mkfifo")
            .arg(state.join("targets.jsonl"))
            .status()
            .unwrap()
            .success()
    );
    wait_for(|| alerts(state).contains("通常のファイル"));
    assert!(!try_enqueue(state, &[tree_line("good")]).status.success());
    std::fs::remove_file(state.join("targets.jsonl")).unwrap();
    add_root(state, "valid", ROOT);
    wait_for(|| status(&out(state, "valid", "w")) == "DONE\n");
    // 同じ内容 (長さと mtime) のファイルを戻しても、受付停止の理由を確かめ直して消す。
    wait_for(|| targets_status(state)["queue_error"].is_null());
    let saved = dir.path().join("saved.jsonl");
    std::fs::rename(state.join("targets.jsonl"), &saved).unwrap();
    std::fs::create_dir(state.join("targets.jsonl")).unwrap();
    wait_for(|| !targets_status(state)["queue_error"].is_null());
    std::fs::remove_dir(state.join("targets.jsonl")).unwrap();
    std::fs::rename(&saved, state.join("targets.jsonl")).unwrap();
    wait_for(|| targets_status(state)["queue_error"].is_null());
    assert!(server.0.try_wait().unwrap().is_none());
    stop(state, &mut server);
}

#[test]
fn enqueue_checks_everything_before_writing_and_repair_truncates_only_the_partial_tail() {
    let dir = tempfile::tempdir().unwrap();
    let state = &dir.path().join("state");
    let queue = state.join("targets.jsonl");
    let mut illegal = tree_line("bad");
    illegal["game"] = json!([["5i4i", "5i4i"]]);
    let mut unknown = tree_line("bad");
    unknown["unknown"] = json!(1);
    for lines in [vec![illegal.clone()], vec![tree_line("good"), unknown]] {
        assert!(!try_enqueue(state, &lines).status.success());
        assert!(std::fs::read(&queue).unwrap_or_default().is_empty());
    }
    let receipt: Value = serde_json::from_str(&add_tree(state, "a")).unwrap();
    let first = std::fs::metadata(&queue).unwrap().len();
    assert_eq!(receipt, json!({"start":0,"end":first,"lines":1}));
    let result = try_enqueue(state, &[tree_line("b"), tree_line("c")]);
    let receipt: Value = serde_json::from_slice(&result.stdout).unwrap();
    let second = std::fs::metadata(&queue).unwrap().len();
    assert_eq!(receipt, json!({"start":first,"end":second,"lines":2}));
    let mut file = std::fs::OpenOptions::new().append(true).open(&queue).unwrap();
    std::io::Write::write_all(&mut file, b"{\"id\":\"half").unwrap();
    let result = try_enqueue(state, &[tree_line("d")]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("--repair"));
    // 1 行用の引数は --from / --repair と一緒に使えない (黙って無視しない)。
    assert!(!book_mine(&["enqueue", "--repair", "--memo", "x"], state).status.success());
    let lines = dir.path().join("one.jsonl");
    std::fs::write(&lines, format!("{}\n", tree_line("d"))).unwrap();
    let from = book_mine(
        &[
            "enqueue",
            "--from",
            lines.to_str().unwrap(),
            "--priority",
            "3",
        ],
        state,
    );
    assert!(!from.status.success());
    let result = book_mine(&["enqueue", "--repair"], state);
    assert!(result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("half"));
    assert_eq!(std::fs::metadata(&queue).unwrap().len(), second);
    // 書き手が同時に来ても行は混ざらず、受領の範囲は重ならない。
    let writers: Vec<_> = (0..8)
        .map(|i| {
            let file = dir.path().join(format!("w{i}.jsonl"));
            std::fs::write(
                &file,
                format!("{}\n{}\n", tree_line(&format!("w{i}a")), tree_line(&format!("w{i}b"))),
            )
            .unwrap();
            Command::new(env!("CARGO_BIN_EXE_book_mine"))
                .args(["tree", "enqueue", "--state"])
                .arg(state)
                .arg("--from")
                .arg(file)
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let mut ranges: Vec<(u64, u64)> = writers
        .into_iter()
        .map(|w| {
            let output = w.wait_with_output().unwrap();
            assert!(output.status.success());
            let r: Value = serde_json::from_slice(&output.stdout).unwrap();
            (r["start"].as_u64().unwrap(), r["end"].as_u64().unwrap())
        })
        .collect();
    ranges.sort();
    assert_eq!(ranges[0].0, second);
    assert!(ranges.windows(2).all(|w| w[0].1 == w[1].0));
    let text = std::fs::read_to_string(&queue).unwrap();
    assert_eq!(ranges.last().unwrap().1, text.len() as u64);
    assert_eq!(text.lines().count(), 3 + 16);
    for line in text.lines() {
        serde_json::from_str::<Value>(line).unwrap();
    }
}

#[test]
fn generations_follow_the_resolved_definition_and_reuse_the_journal() {
    let dir = tempfile::tempdir().unwrap();
    let state = &dir.path().join("state");
    std::fs::create_dir(state).unwrap();
    let config = fast_fixture(state);
    let mut server = start(state, &config);
    let generation = |id: &str, own: &str| {
        targets_status(state)["targets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == id && t["own"] == own)
            .map(|t| t["generation"].as_u64().unwrap())
    };
    let receipt = add_tree(state, "a");
    assert!(wait_cmd(state, &receipt, None).output().unwrap().status.success());
    let g1 = state.parent().unwrap().join("a/b/g1");
    let own = std::fs::read(g1.join("own_nodes.tsv")).unwrap();
    let mut line = tree_line("a");
    line["priority"] = json!(5);
    line["memo"] = json!("priority only");
    let receipt = enqueue(state, line.clone());
    assert!(wait_cmd(state, &receipt, None).output().unwrap().status.success());
    assert_eq!(generation("a", "b"), Some(1));
    assert!(!state.parent().unwrap().join("a/b/g2").exists());
    line["depth"] = json!(2);
    let receipt = enqueue(state, line.clone());
    let result = wait_cmd(state, &receipt, None).output().unwrap();
    assert!(result.status.success());
    let waited: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(waited["targets"][0]["generation"], 2);
    assert_eq!(waited["targets"][0]["ready"], true);
    assert_eq!(status(&g1), "SUPERSEDED\n");
    assert_eq!(std::fs::read(g1.join("own_nodes.tsv")).unwrap(), own);
    let before = searches(state);
    line["depth"] = json!(1);
    let receipt = enqueue(state, line.clone());
    assert!(wait_cmd(state, &receipt, None).output().unwrap().status.success());
    let g3 = state.parent().unwrap().join("a/b/g3");
    assert_eq!(generation("a", "b"), Some(3));
    assert_eq!(std::fs::read(g3.join("own_nodes.tsv")).unwrap(), own);
    assert_eq!(status(&state.parent().unwrap().join("a/b/g2")), "SUPERSEDED\n");
    assert_eq!(searches(state), before);
    // 同じ段数で opts だけを変えても新しい世代になる。
    line["opts"]["opp_n"] = json!(1);
    let receipt = enqueue(state, line);
    assert!(wait_cmd(state, &receipt, None).output().unwrap().status.success());
    assert_eq!(generation("a", "b"), Some(4));
    // opts を省いた行は起動時の既定で解決し、既定を変えると priority だけの行でも新しい世代になる。
    let mut implicit = root_line("c", "k8/9/9/9/9/9/9/9/8K w -");
    implicit["opts"] = json!({});
    let receipt = enqueue(state, implicit.clone());
    assert!(wait_cmd(state, &receipt, None).output().unwrap().status.success());
    stop(state, &mut server);
    std::fs::remove_file(state.join("stop")).unwrap();
    let mut command = serve_command(state, &config, 3, 3, state.parent().unwrap());
    let mut server = Server(command.args(["--opp-n", "3"]).spawn().unwrap());
    implicit["priority"] = json!(7);
    let receipt = enqueue(state, implicit);
    assert!(wait_cmd(state, &receipt, None).output().unwrap().status.success());
    assert_eq!(generation("c", "w"), Some(2));
    assert_eq!(generation("a", "b"), Some(4));
    // root と own は id ごとに固定。拒否しても有効な定義と出力は前のまま。
    let mut moved = tree_line("a");
    moved["root"] = json!("startpos");
    moved.as_object_mut().unwrap().remove("game");
    moved["opts"].as_object_mut().unwrap().remove("extra");
    let receipt = enqueue(state, moved);
    assert!(!wait_cmd(state, &receipt, None).output().unwrap().status.success());
    assert_eq!(generation("a", "b"), Some(4));
    assert!(state.parent().unwrap().join("a/b/g4/own_nodes.tsv").exists());
    stop(state, &mut server);
}

#[test]
fn priority_orders_the_queue_hold_pauses_and_bw_reads_both_sides() {
    let dir = tempfile::tempdir().unwrap();
    let state = &dir.path().join("state");
    std::fs::create_dir(state).unwrap();
    std::fs::write(state.join("workers"), "1").unwrap();
    let config = fast_fixture(state);
    let low = "k8/9/9/9/9/9/9/9/8K w -";
    let high = "8k/9/9/9/9/9/9/9/K8 w -";
    let mut line = root_line("low", low);
    line["priority"] = json!(1000);
    enqueue(state, line);
    let mut line = root_line("high", high);
    line["priority"] = json!(0);
    enqueue(state, line);
    let mut held = root_line("held", "4k4/9/9/9/9/9/9/9/K8 w -");
    held["hold"] = json!(true);
    let held_receipt = enqueue(state, held.clone());
    let mut server = start(state, &config);
    wait_for(|| {
        status(&out(state, "low", "w")) == "DONE\n" && status(&out(state, "high", "w")) == "DONE\n"
    });
    let log = std::fs::read_to_string(state.join("protocol.jsonl")).unwrap();
    let first = log.lines().find(|l| l.contains("position sfen")).unwrap();
    assert!(first.contains(high), "{first}");
    assert_eq!(status(&out(state, "held", "w")), "HELD\n");
    assert!(!log.contains("4k4/9/9/9/9/9/9/9/K8 w -"));
    let result = wait_cmd(state, &held_receipt, None).output().unwrap();
    assert!(result.status.success());
    let waited: Value = serde_json::from_slice(&result.stdout).unwrap();
    let held_row = waited["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "held")
        .unwrap();
    assert_eq!(held_row["state"], "HELD");
    assert_eq!(held_row["ready"], false);
    held["hold"] = json!(false);
    let receipt = enqueue(state, held);
    assert!(wait_cmd(state, &receipt, Some("held")).output().unwrap().status.success());
    assert_eq!(status(&out(state, "held", "w")), "DONE\n");
    let mut both = tree_line("both");
    both["own"] = json!("bw");
    let receipt = enqueue(state, both);
    let result = wait_cmd(state, &receipt, Some("both")).output().unwrap();
    assert!(result.status.success());
    let waited: Value = serde_json::from_slice(&result.stdout).unwrap();
    let sides: BTreeSet<_> = waited["targets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["own"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(sides, BTreeSet::from(["b".to_owned(), "w".to_owned()]));
    assert!(out(state, "both", "b").join("own_nodes.tsv").exists());
    assert!(out(state, "both", "w").join("STATUS").exists());
    stop(state, &mut server);
}

#[test]
fn wait_checks_the_receipt_range_fresh_status_and_serve_liveness() {
    let dir = tempfile::tempdir().unwrap();
    let state = &dir.path().join("state");
    std::fs::create_dir(state).unwrap();
    let config = fast_fixture(state);
    let mut server = start(state, &config);
    let receipt = add_tree(state, "a");
    assert!(wait_cmd(state, &receipt, None).output().unwrap().status.success());
    let mut moved = tree_line("a");
    moved["root"] = json!("startpos");
    moved.as_object_mut().unwrap().remove("game");
    moved["opts"].as_object_mut().unwrap().remove("extra");
    let rejected = enqueue(state, moved);
    let result = wait_cmd(state, &rejected, None).output().unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("root と own"));
    // 範囲の外の拒否や同じ id の過去の拒否では落ちない。
    let mut again = tree_line("a");
    again["priority"] = json!(3);
    let receipt = enqueue(state, again);
    assert!(wait_cmd(state, &receipt, None).output().unwrap().status.success());
    assert!(wait_cmd(state, &add_tree(state, "b"), None).output().unwrap().status.success());
    // 全部が終わった後に足した対象を、古い all_ended で終わったと見ない。
    std::fs::write(state.join("workers"), "0").unwrap();
    wait_for(|| progress(state)["workers"].as_array().is_some_and(|w| w.is_empty()));
    let receipt = add_root(state, "c", "k8/9/9/9/9/9/9/9/8K w -");
    let mut waiting = wait_cmd(state, &receipt, Some("a")).spawn().unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    assert!(waiting.try_wait().unwrap().is_none());
    assert_eq!(targets_status(state)["all_ended"], false);
    stop(state, &mut server);
    wait_for(|| waiting.try_wait().unwrap().is_some());
    assert!(!waiting.wait().unwrap().success());
    let result = wait_cmd(state, &receipt, Some("a")).output().unwrap();
    assert!(String::from_utf8_lossy(&result.stderr).contains("serve が動いていません"));
}

#[test]
fn restart_hides_old_ready_processes_lines_once_and_recovers_status_writes() {
    let dir = tempfile::tempdir().unwrap();
    let state = &dir.path().join("state");
    std::fs::create_dir(state).unwrap();
    let config = fast_fixture(state);
    std::fs::write(state.join("targets.jsonl"), "invalid\n").unwrap();
    let receipt_a = add_tree(state, "a");
    let mut server = start(state, &config);
    wait_for(|| targets_status(state)["targets"][0]["ready"] == true);
    let checkpoint = std::fs::read_to_string(state.join("accepted.json")).unwrap();
    let first_run = targets_status(state)["run_id"].clone();
    stop(state, &mut server);
    std::fs::remove_file(state.join("stop")).unwrap();
    // 止まった serve の状態は ready=true のままでも、wait はそれを使わない。
    let result = wait_cmd(state, &receipt_a, None).output().unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("serve が動いていません"));
    // 台帳を別の serve が持っている間は起動に失敗するが、その前に古い ready を消す。
    let other = dir.path().join("other");
    std::fs::create_dir(&other).unwrap();
    let mut holder = Server(
        serve_command(&other, &config, 3, 3, &dir.path().join("other-out"))
            .arg("--journal")
            .arg(state.join("journal.jsonl"))
            .spawn()
            .unwrap(),
    );
    // progress.json は台帳を開いた後の周回で書かれる。
    wait_for(|| other.join("progress.json").exists());
    assert!(exits_with_failure(serve_command(state, &config, 3, 3, state.parent().unwrap())));
    let resumed = targets_status(state);
    assert_ne!(resumed["run_id"], first_run);
    assert_eq!(resumed["targets"][0]["state"], "RESUMING");
    assert_eq!(resumed["targets"][0]["ready"], false);
    stop(&other, &mut holder);
    let mut server = start(state, &config);
    wait_for(|| targets_status(state)["targets"][0]["ready"] == true);
    assert_eq!(std::fs::read_to_string(state.join("accepted.json")).unwrap(), checkpoint);
    assert_eq!(targets_status(state)["rejected"].as_array().unwrap().len(), 1);
    // 状態のファイルを書けない間は ALERT にして、書けるようになったら作り直す。
    std::fs::remove_file(state.join("targets.status.json")).unwrap();
    std::fs::create_dir(state.join("targets.status.json")).unwrap();
    add_tree(state, "b");
    wait_for(|| alerts(state).contains("状態のファイルを書けません"));
    std::fs::remove_dir(state.join("targets.status.json")).unwrap();
    wait_for(|| targets_status(state)["targets"].as_array().is_some_and(|t| t.len() == 2));
    // 処理済みの範囲が変わったら受付を止め、読みは続ける。
    let mut queue = std::fs::read(state.join("targets.jsonl")).unwrap();
    queue[0] = b'X';
    std::fs::write(state.join("targets.jsonl"), &queue).unwrap();
    let receipt = add_tree(state, "c");
    wait_for(|| alerts(state).contains("処理済みの範囲が変わりました"));
    let result = wait_cmd(state, &receipt, None).output().unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("受け付けていません"));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(targets_status(state)["targets"].as_array().unwrap().len(), 2);
    assert!(server.0.try_wait().unwrap().is_none());
    // 戻せば止まっていた行から受付を再開し、受付停止の理由も消える。
    let mut queue = std::fs::read(state.join("targets.jsonl")).unwrap();
    queue[0] = b'i';
    std::fs::write(state.join("targets.jsonl"), &queue).unwrap();
    assert!(wait_cmd(state, &receipt, Some("c")).output().unwrap().status.success());
    assert!(targets_status(state)["queue_error"].is_null());
    stop(state, &mut server);
}

#[test]
fn restart_redoes_side_effects_after_the_checkpoint_and_lines_before_it_once() {
    let dir = tempfile::tempdir().unwrap();
    let state = &dir.path().join("state");
    std::fs::create_dir(state).unwrap();
    std::fs::write(state.join("workers"), "0").unwrap();
    let config = fast_fixture(state);
    std::fs::write(state.join("targets.jsonl"), "invalid\n").unwrap();
    add_tree(state, "a");
    let mut server = start(state, &config);
    wait_for(|| out(state, "a", "b").join("paths.record.tsv").exists());
    stop(state, &mut server);
    std::fs::remove_file(state.join("stop")).unwrap();
    let first = std::fs::read_to_string(state.join("accepted.json")).unwrap();
    // チェックポイントの後、副作用の前に落ちた: 出力先が無くても再開で作り直す。
    std::fs::remove_dir_all(state.parent().unwrap().join("a")).unwrap();
    let mut server = start(state, &config);
    wait_for(|| status(&out(state, "a", "b")) == "STOPPED\n");
    assert!(out(state, "a", "b").join("paths.record.tsv").exists());
    assert_eq!(std::fs::read_to_string(state.join("accepted.json")).unwrap(), first);
    let mut queue = std::fs::OpenOptions::new()
        .append(true)
        .open(state.join("targets.jsonl"))
        .unwrap();
    std::io::Write::write_all(&mut queue, b"invalid again\n").unwrap();
    add_tree(state, "b");
    wait_for(|| targets_status(state)["targets"].as_array().is_some_and(|t| t.len() == 2));
    stop(state, &mut server);
    std::fs::remove_file(state.join("stop")).unwrap();
    let second = std::fs::read_to_string(state.join("accepted.json")).unwrap();
    // チェックポイントの前に落ちた: 行は未処理のまま残り、次の起動で 1 回だけ処理する。
    std::fs::write(state.join("accepted.json"), &first).unwrap();
    std::fs::remove_dir_all(state.parent().unwrap().join("b")).unwrap();
    let before = targets_status(state)["run_id"].clone();
    let mut server = start(state, &config);
    wait_for(|| {
        let current = targets_status(state);
        current["run_id"] != before && current["targets"].as_array().is_some_and(|t| t.len() == 2)
    });
    assert_eq!(std::fs::read_to_string(state.join("accepted.json")).unwrap(), second);
    assert_eq!(targets_status(state)["rejected"].as_array().unwrap().len(), 2);
    wait_for(|| status(&out(state, "b", "b")) == "STOPPED\n");
    stop(state, &mut server);
}

#[cfg(unix)]
#[test]
fn engine_that_cannot_start_is_alerted_once_without_failing_the_tree() {
    let dir = tempfile::tempdir().unwrap();
    let state = &dir.path().join("state");
    std::fs::create_dir(state).unwrap();
    std::fs::write(state.join("workers"), "1").unwrap();
    let eval = dir.path().join("eval");
    std::fs::write(&eval, "eval").unwrap();
    add_tree(state, "a");
    let log = std::fs::File::create(state.join("server.log")).unwrap();
    let mut server = Server(
        Command::new(env!("CARGO_BIN_EXE_book_mine"))
            .args(["tree", "serve", "--state"])
            .arg(state)
            .args(["--engine", "/bin/false", "--engine-option"])
            .arg(format!("EvalFile={}", eval.display()))
            .args(["--nodes", "100", "--hash", "16", "--out-root"])
            .arg(dir.path())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap(),
    );
    wait_for(|| alerts(state).contains("engine を起動できません"));
    std::thread::sleep(Duration::from_millis(1000));
    assert_eq!(
        alerts(state).lines().filter(|l| l.contains("engine を起動できません")).count(),
        1
    );
    assert!(!alerts(state).contains("再起動上限"));
    assert!(["QUEUED\n", "RUNNING\n"].contains(&status(&out(state, "a", "b")).as_str()));
    assert!(server.0.try_wait().unwrap().is_none());
    stop(state, &mut server);
}

#[test]
fn replay_started_before_serve_keeps_its_output_until_it_ends() {
    let dir = tempfile::tempdir().unwrap();
    let state = &dir.path().join("state");
    std::fs::create_dir(state).unwrap();
    let config = fast_fixture(state);
    // serve.lock が無い間に始まった replay の代わりに、出力先の tree.lock を持っておく。
    std::fs::create_dir_all(out(state, "a", "b")).unwrap();
    let replay = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(out(state, "a", "b").join("tree.lock"))
        .unwrap();
    replay.try_lock().unwrap();
    let receipt = add_tree(state, "a");
    let mut server = start(state, &config);
    wait_for(|| alerts(state).contains("出力先を用意できません"));
    std::thread::sleep(Duration::from_millis(300));
    assert!(!out(state, "a", "b").join("STATUS").exists());
    assert!(!out(state, "a", "b").join("paths.record.tsv").exists());
    replay.unlock().unwrap();
    assert!(wait_cmd(state, &receipt, None).output().unwrap().status.success());
    assert_eq!(status(&out(state, "a", "b")), "DONE\n");
    stop(state, &mut server);
}

fn start_traced(state: &Path, config: &Path, trace: &Path) -> Server {
    let mut command = serve_command(state, config, 3, 3, state.parent().unwrap());
    command.arg("--trace-dir").arg(trace);
    Server(command.spawn().unwrap())
}
/// 記録から MultiPV を組み直す。`limit` が無ければ台帳と同じ読み、あれば nodes がそれ以下の行に限る。
fn multipv_at(lines: &[&str], limit: Option<u64>) -> Vec<Value> {
    let mut last = BTreeMap::new();
    for line in lines {
        let tokens: Vec<_> = line.split_whitespace().collect();
        if !line.contains(" pv ")
            || !line.contains(" score ")
            || tokens.contains(&"lowerbound")
            || tokens.contains(&"upperbound")
        {
            continue;
        }
        let get = |name| tokens.iter().position(|t| *t == name).map(|i| tokens[i + 1]);
        let nodes = get("nodes").map(|n| n.parse::<u64>().unwrap());
        if limit.is_some_and(|limit| nodes.is_none_or(|n| n > limit)) {
            continue;
        }
        let index: usize = get("multipv").unwrap_or("1").parse().unwrap();
        assert_eq!(get("score"), Some("cp"));
        last.insert(
            index,
            json!({"move": get("pv").unwrap(), "cp": get("cp").unwrap().parse::<i32>().unwrap(),
                "depth": get("depth").unwrap().parse::<u32>().unwrap()}),
        );
    }
    last.into_values().collect()
}
fn trace_records(trace: &Path) -> Vec<(PathBuf, Vec<Value>)> {
    let mut files: Vec<_> = std::fs::read_dir(trace).unwrap().map(|e| e.unwrap().path()).collect();
    files.sort();
    files
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path).unwrap();
            let records = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
            (path, records)
        })
        .collect()
}

#[test]
fn trace_keeps_info_lines_of_completed_searches_only_and_can_change_across_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let state = &dir.path().join("state");
    std::fs::create_dir(state).unwrap();
    // 出力の根 (state の親) の外に置く。
    let trace_root = tempfile::tempdir().unwrap();
    let trace = trace_root.path().join("trace");
    let config = fixture(state);
    let mut data: Value = serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
    data["noisy"] = json!(true);
    std::fs::write(&config, serde_json::to_vec(&data).unwrap()).unwrap();
    add_tree(state, "a");
    let mut server = start_traced(state, &config, &trace);
    wait_for(|| out(state, "a", "b").join("own_nodes.tsv").exists());
    stop(state, &mut server);
    // crash_once で 1 回の探索が途中で落ち、その局面は読み直される。
    assert_eq!(searches(state), 5);
    let journal: Vec<Value> = std::fs::read_to_string(state.join("journal.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let files = trace_records(&trace);
    assert_eq!(files.len(), 1);
    let records = &files[0].1;
    assert_eq!(records.len(), journal.len());
    for record in &journal {
        let reading = &record["reading"];
        let traced: Vec<_> = records.iter().filter(|t| t["key"] == reading["key"]).collect();
        assert_eq!(traced.len(), 1, "{reading}");
        let t = traced[0];
        assert_eq!(t["ply"], reading["ply"]);
        assert_eq!(t["bestmove"], reading["bestmove"]);
        assert_eq!(t["fingerprint"], record["settings"]["fingerprint"]);
        assert!(t["worker"].as_u64().unwrap() < 4);
        let started = chrono::DateTime::parse_from_rfc3339(t["started"].as_str().unwrap()).unwrap();
        let finished =
            chrono::DateTime::parse_from_rfc3339(t["finished"].as_str().unwrap()).unwrap();
        assert!(started <= finished);
        let lines: Vec<_> =
            t["lines"].as_array().unwrap().iter().map(|l| l.as_str().unwrap()).collect();
        assert!(lines.iter().all(|l| l.starts_with("info")));
        assert!(lines.contains(&"info string alive"));
        assert!(lines.iter().any(|l| l.contains(" lowerbound ")));
        assert_eq!(reading["multipv"], json!(multipv_at(&lines, None)));
        // 停滞中の行 (nodes 1) だけが上限 100 以下で、最後の行 (nodes 101) は上限を超える。
        assert_eq!(multipv_at(&lines, Some(100))[0]["cp"], 0);
    }
    // 台帳の読みだけで済む起動は探索しないので記録を書かず、付け外しで起動を拒否しない。
    std::fs::remove_file(state.join("stop")).unwrap();
    let mut server = start_traced(state, &config, &trace);
    add_tree(state, "b");
    wait_for(|| out(state, "b", "b").join("own_nodes.tsv").exists());
    stop(state, &mut server);
    std::fs::remove_file(state.join("stop")).unwrap();
    let mut server = start(state, &config);
    wait_for(|| progress(state)["status"] == "IDLE");
    stop(state, &mut server);
    assert_eq!(searches(state), 5);
    assert_eq!(trace_records(&trace).iter().map(|(_, r)| r.len()).sum::<usize>(), journal.len());
}

#[test]
fn trace_failures_are_alerted_once_and_readings_continue_until_the_trace_can_be_written() {
    let dir = tempfile::tempdir().unwrap();
    let state = &dir.path().join("state");
    std::fs::create_dir(state).unwrap();
    // trace-dir の場所に通常のファイルがあるので、dir を作れない。
    let trace_root = tempfile::tempdir().unwrap();
    let trace = trace_root.path().join("trace");
    std::fs::write(&trace, "").unwrap();
    let config = fast_fixture(state);
    add_tree(state, "a");
    let mut server = start_traced(state, &config, &trace);
    wait_for(|| out(state, "a", "b").join("own_nodes.tsv").exists());
    wait_for(|| alerts(state).contains("探索の記録を書けません"));
    assert_eq!(std::fs::read_to_string(state.join("journal.jsonl")).unwrap().lines().count(), 4);
    std::fs::remove_file(&trace).unwrap();
    let child = child_position_after_move(&format!("{ROOT} 1"), "5i4i").unwrap().to_sfen();
    add_root(state, "r", strip_ply(&child));
    // 子は未知の局面なので投了の葉になり、木は NOT_READY で終わる。
    wait_for(|| status(&out(state, "r", "w")) == "NOT_READY\n");
    stop(state, &mut server);
    let journal = std::fs::read_to_string(state.join("journal.jsonl")).unwrap();
    let files = trace_records(&trace);
    assert_eq!(files.len(), 1);
    assert!(!files[0].1.is_empty());
    assert_eq!(files[0].1.len(), journal.lines().count() - 4);
    assert_eq!(
        alerts(state).lines().filter(|l| l.contains("探索の記録を書けません")).count(),
        1
    );
}

fn protocol(state: &Path) -> Vec<(u32, String)> {
    std::fs::read_to_string(state.join("protocol.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn worker_recreation_rejects_changed_or_missing_inputs_without_appending() {
    for case in [
        "model",
        "model-missing",
        "engine",
        "engine-missing",
        "implicit-new",
        "implicit-missing",
        "explicit",
        "explicit-missing",
        "unchanged",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let state = &dir.path().join("state");
        std::fs::create_dir(state).unwrap();
        std::fs::write(state.join("workers"), "1").unwrap();
        let config = fast_fixture(state);
        let engine = dir.path().join(format!("engine{}", std::env::consts::EXE_SUFFIX));
        std::fs::copy(engine_path(), &engine).unwrap();
        let implicit = dir.path().join("spsa.params");
        let explicit = dir.path().join("coeff.bin");
        std::fs::write(&explicit, "A").unwrap();
        if case == "implicit-missing" {
            std::fs::write(&implicit, "A").unwrap();
        }
        let mut command = serve_command_with_engine(state, &config, 3, 3, dir.path(), &engine);
        command
            .arg("--engine-option")
            .arg(format!("LS_PROGRESS_COEFF={}", explicit.display()));
        add_tree(state, "a");
        let mut server = Server(command.spawn().unwrap());
        wait_for(|| out(state, "a", "b").join("own_nodes.tsv").exists());
        std::fs::write(state.join("workers"), "0").unwrap();
        wait_for(|| progress(state)["workers"].as_array().is_some_and(Vec::is_empty));
        let before = std::fs::metadata(state.join("journal.jsonl")).unwrap().len();
        assert!(before > 0);
        let old_log = protocol(state);
        let pids: BTreeSet<_> = old_log.iter().map(|(pid, _)| *pid).collect();
        assert!(!pids.is_empty());
        for pid in pids {
            assert!(old_log.contains(&(pid, "quit".into())), "{case}");
        }
        match case {
            "model" => std::fs::write(&config, "B").unwrap(),
            "model-missing" => std::fs::remove_file(&config).unwrap(),
            "engine" => std::fs::write(&engine, "B").unwrap(),
            "engine-missing" => std::fs::remove_file(&engine).unwrap(),
            "implicit-new" => std::fs::write(&implicit, "B").unwrap(),
            "implicit-missing" => std::fs::remove_file(&implicit).unwrap(),
            "explicit" => std::fs::write(&explicit, "B").unwrap(),
            "explicit-missing" => std::fs::remove_file(&explicit).unwrap(),
            "unchanged" => std::fs::write(&explicit, "A").unwrap(),
            _ => unreachable!(),
        }
        add_root(state, "b", "4k4/9/9/9/9/9/9/9/2K6 w -");
        std::fs::write(state.join("workers"), "1").unwrap();
        if case == "unchanged" {
            wait_for(|| out(state, "b", "w").join("own_nodes.tsv").exists());
            stop(state, &mut server);
            assert!(std::fs::read(state.join("journal.jsonl")).unwrap().len() as u64 > before);
            let log = protocol(state);
            let pids: BTreeSet<_> = log.iter().map(|(pid, _)| *pid).collect();
            assert_eq!(pids.len(), 2);
            for pid in pids {
                assert!(log.contains(&(pid, "quit".into())));
            }
        } else {
            wait_for(|| server.0.try_wait().unwrap().is_some());
            assert!(!server.0.wait().unwrap().success(), "{case}");
            assert!(alerts(state).contains("入力の指紋"), "{case}");
            assert_eq!(
                std::fs::read(state.join("journal.jsonl")).unwrap().len() as u64,
                before,
                "{case}"
            );
            assert_eq!(protocol(state), old_log, "{case}");
        }
    }
}

#[test]
fn persistent_replacement_during_initialization_stops_before_search_and_reaps_engine() {
    for (phase, cleanup_failure) in [("initial", false), ("newgame", false), ("initial", true)] {
        let worker_count = if cleanup_failure { 2 } else { 1 };
        let dir = tempfile::tempdir().unwrap();
        let state = &dir.path().join("state");
        std::fs::create_dir(state).unwrap();
        std::fs::write(state.join("workers"), worker_count.to_string()).unwrap();
        let config = fast_fixture(state);
        let marker = state.join("ready-entered");
        let release = state.join("ready-release");
        let mut data: Value = serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
        data["ready_phase"] = json!(phase);
        data["ready_marker"] = json!(marker);
        data["ready_release"] = json!(release);
        std::fs::write(&config, serde_json::to_vec(&data).unwrap()).unwrap();
        add_tree(state, "a");
        if cleanup_failure {
            add_root(state, "b", "4k4/9/9/9/9/9/9/9/2K6 w -");
        }
        let mut server = start(state, &config);
        wait_for(|| {
            marker.exists()
                && protocol(state)
                    .iter()
                    .filter(|(_, line)| line == "isready")
                    .map(|(pid, _)| *pid)
                    .collect::<BTreeSet<_>>()
                    .len()
                    == worker_count
        });
        if cleanup_failure {
            std::fs::create_dir(out(state, "a", "b").join("own_nodes.tsv")).unwrap();
        }
        let before = std::fs::metadata(state.join("journal.jsonl")).unwrap().len();
        std::fs::write(&config, "B").unwrap();
        std::fs::write(&release, "").unwrap();
        wait_for(|| server.0.try_wait().unwrap().is_some());
        assert!(!server.0.wait().unwrap().success());
        assert_eq!(std::fs::read(state.join("journal.jsonl")).unwrap().len() as u64, before);
        assert!(alerts(state).contains("入力の指紋"));
        if cleanup_failure {
            assert!(alerts(state).contains("候補の削除失敗"));
        }
        let log = protocol(state);
        assert!(!log.iter().any(|(_, line)| line.starts_with("go ")));
        let pids: BTreeSet<_> = log.iter().map(|(pid, _)| *pid).collect();
        assert_eq!(pids.len(), worker_count);
        for pid in pids {
            assert!(log.contains(&(pid, "quit".into())));
        }
    }
}
