#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

fn write_engine(path: &std::path::Path, script: &str) {
    fs::write(path, script).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

struct OwnedChild(Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn run_error_retry_case(isready_action: &str, go_action: &str, exhausted: bool) {
    let dir = tempfile::tempdir().unwrap();
    let flaky = dir.path().join("flaky-engine.sh");
    let steady = dir.path().join("steady-engine.sh");
    let failed_once = dir.path().join("failed-once");
    let launches = dir.path().join("flaky-launches");
    let out_dir = dir.path().join("out");

    write_engine(
        &flaky,
        &format!(
            "#!/bin/sh\n\
             printf '%s\\n' \"$$\" >> '{}'\n\
             isready_count=0\n\
             while IFS= read -r line; do\n\
               case \"$line\" in\n\
                 usi) printf 'id name flaky\\nusiok\\n' ;;\n\
                 isready)\n\
                   isready_count=$((isready_count + 1))\n\
                   {isready_action}\n\
                   ;;\n\
                 go*)\n\
                   {go_action}\n\
                   ;;\n\
                 quit) break ;;\n\
               esac\n\
             done\n",
            launches.display(),
        ),
    );
    write_engine(
        &steady,
        "#!/bin/sh\n\
         while IFS= read -r line; do\n\
           case \"$line\" in\n\
             usi) printf 'id name steady\\nusiok\\n' ;;\n\
             isready) printf 'readyok\\n' ;;\n\
             go*) printf 'bestmove resign\\n' ;;\n\
             quit) break ;;\n\
           esac\n\
         done\n",
    );

    let stdout_path = dir.path().join("stdout");
    let stderr_path = dir.path().join("stderr");
    let mut child = OwnedChild(
        Command::new(env!("CARGO_BIN_EXE_tournament"))
            .env("FAILED_ONCE", &failed_once)
            .args([
                "--engine",
                flaky.to_str().unwrap(),
                "--engine-label",
                "flaky",
                "--engine",
                steady.to_str().unwrap(),
                "--engine-label",
                "steady",
                "--games",
                "1",
                "--concurrency",
                "1",
                "--nodes",
                "1",
                "--out-dir",
                out_dir.to_str().unwrap(),
            ])
            .stdout(Stdio::from(fs::File::create(&stdout_path).unwrap()))
            .stderr(Stdio::from(fs::File::create(&stderr_path).unwrap()))
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "tournament timed out: {}",
            fs::read_to_string(&stderr_path).unwrap()
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(
        status.success(),
        !exhausted,
        "tournament failed:\nstdout:\n{}\nstderr:\n{}",
        fs::read_to_string(&stdout_path).unwrap(),
        fs::read_to_string(&stderr_path).unwrap(),
    );

    let launches = fs::read_to_string(&launches).unwrap();
    let pids: Vec<_> = launches.lines().collect();
    assert_eq!(pids.len(), if exhausted { 4 } else { 2 });
    assert_eq!(pids.iter().collect::<std::collections::HashSet<_>>().len(), pids.len());
    #[cfg(target_os = "linux")]
    for pid in &pids {
        assert!(pid.parse::<u32>().is_ok());
        assert!(!std::path::Path::new("/proc").join(pid).exists(), "mock child {pid} remains");
    }

    let jsonl = fs::read_to_string(out_dir.join("pair-0-1__flaky-vs-steady.jsonl")).unwrap();
    let results: Vec<Value> = jsonl
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|row| row["type"] == "result")
        .collect();
    assert_eq!(results.len(), if exhausted { 6 } else { 4 });
    assert_eq!(
        results.iter().filter(|row| row["error"] == true).count(),
        if exhausted { 3 } else { 1 }
    );
    assert_eq!(results.iter().filter(|row| row["attempt"] == 1).count(), 2);
    assert!(
        exhausted
            || results.iter().filter(|row| row["attempt"] == 1).all(|row| row["error"] != true),
        "新しい process での再試行は成功するべき"
    );

    let meta: Value =
        serde_json::from_slice(&fs::read(out_dir.join("meta.json")).unwrap()).unwrap();
    assert_eq!(meta["error_pairs"], 1);
    assert_eq!(meta["retried_pairs"], 1);
    assert_eq!(meta["exhausted_pairs"], u64::from(exhausted));
    assert_eq!(meta["invalid"], exhausted);
    if go_action.contains("info string Error:") {
        assert!(results.iter().filter(|row| row["error"] == true).all(|row| {
            row["reason"].as_str().unwrap().contains("info string Error: inference failed")
        }));
    }
    for attempt in 0..=if exhausted { 2 } else { 1 } {
        let pair: Vec<_> = results.iter().filter(|row| row["attempt"] == attempt).collect();
        assert_eq!(pair.len(), 2);
        assert_eq!(pair[0]["pair_index"], results[0]["pair_index"]);
        assert_eq!(pair[1]["pair_index"], results[0]["pair_index"]);
        assert_ne!(pair[0]["pair_slot"], pair[1]["pair_slot"]);
        for row in pair.iter() {
            assert!(row["startpos_idx"].is_u64());
            assert_eq!(row["startpos_idx"], results[0]["startpos_idx"]);
            assert_eq!(
                row["pair_slot"],
                results[row["pair_slot"].as_u64().unwrap() as usize]["pair_slot"]
            );
        }
        assert_eq!(
            pair[0]["reason"].as_str().unwrap().starts_with("error:"),
            pair[0]["error"] == true
        );
    }
}

#[test]
fn communication_error_retires_worker_and_retry_uses_new_processes() {
    run_error_retry_case(
        "printf 'readyok\\n'",
        "if [ ! -e \"$FAILED_ONCE\" ]; then : > \"$FAILED_ONCE\"; exit 1; fi; \
         printf 'bestmove resign\\n'",
        false,
    );
}

#[test]
fn new_game_error_retires_worker_and_retry_uses_new_processes() {
    run_error_retry_case(
        "if [ \"$isready_count\" -ge 2 ] && [ ! -e \"$FAILED_ONCE\" ]; then \
         : > \"$FAILED_ONCE\"; exit 1; fi; printf 'readyok\\n'",
        "printf 'bestmove resign\\n'",
        false,
    );
}

#[test]
fn reported_error_before_resign_retries_in_new_processes() {
    run_error_retry_case(
        "printf 'readyok\\n'",
        "if [ ! -e \"$FAILED_ONCE\" ]; then : > \"$FAILED_ONCE\"; printf 'info string Error: inference failed\\n'; fi; printf 'bestmove resign\\n'",
        false,
    );
}

#[test]
fn reported_error_before_legal_move_retries_in_new_processes() {
    run_error_retry_case(
        "printf 'readyok\\n'",
        "if [ ! -e \"$FAILED_ONCE\" ]; then : > \"$FAILED_ONCE\"; printf 'info string Error: inference failed\\nbestmove 7g7f\\n'; else printf 'bestmove resign\\n'; fi",
        false,
    );
}

#[test]
fn permanent_reported_error_exhausts_pairs_without_counting_resign_as_loss() {
    run_error_retry_case(
        "printf 'readyok\\n'",
        "printf 'info string Error: inference failed\\nbestmove resign\\n'",
        true,
    );
}

#[test]
fn cycle_tournament_records_positions_and_analyzer_uses_result_fields() {
    let dir = tempfile::tempdir().unwrap();
    let engine = dir.path().join("engine.sh");
    write_engine(
        &engine,
        "#!/bin/sh\nwhile IFS= read -r line; do\ncase \"$line\" in\nusi) printf 'usiok\\n' ;;\nisready) printf 'readyok\\n' ;;\ngo*) printf 'bestmove resign\\n' ;;\nquit) break ;;\nesac\ndone\n",
    );
    let positions = dir.path().join("positions");
    fs::write(&positions, "first\tposition startpos\nsecond\tposition startpos moves 7g7f\nthird\tposition startpos moves 2g2f\n").unwrap();
    let out = dir.path().join("out");
    let output = Command::new(env!("CARGO_BIN_EXE_tournament"))
        .arg("--engine")
        .arg(&engine)
        .args(["--engine-label", "a", "--engine"])
        .arg(&engine)
        .args([
            "--engine-label",
            "b",
            "--games",
            "9",
            "--concurrency",
            "2",
            "--nodes",
            "1",
            "--startpos-order",
            "cycle",
            "--startpos-file",
        ])
        .arg(&positions)
        .arg("--out-dir")
        .arg(&out)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let meta: Value = serde_json::from_slice(&fs::read(out.join("meta.json")).unwrap()).unwrap();
    assert_eq!(meta["settings"]["startpos_order"], "cycle");
    assert_eq!(meta["start_positions"].as_array().unwrap().len(), 3);
    let log = out.join("pair-0-1__a-vs-b.jsonl");
    let lines: Vec<Value> = fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect();
    for name in ["first", "second", "third"] {
        assert_eq!(
            lines
                .iter()
                .filter(|line| line["type"] == "result" && line["startpos_name"] == name)
                .count(),
            6
        );
    }
    // 読み直し後の index は起動時の meta と対応しない。
    let lines: Vec<String> = lines
        .into_iter()
        .map(|mut line| {
            if line["type"] == "result" {
                line["startpos_idx"] = 0.into();
            }
            line.to_string()
        })
        .collect();
    fs::write(&log, lines.join("\n")).unwrap();
    for json in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_analyze_selfplay"));
        command.arg("--by-startpos").arg(&out);
        if json {
            command.arg("--json");
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        if json {
            let value: Value = serde_json::from_slice(&output.stdout).unwrap();
            let rows = value["by_startpos"]["rows"].as_array().unwrap();
            assert_eq!(rows.len(), 3);
            assert!(rows.iter().all(|row| row["games"] == 6 && row["startpos_name"].is_string()));
        } else {
            let text = String::from_utf8_lossy(&output.stdout);
            for name in ["first", "second", "third"] {
                assert!(text.contains(&format!("[{name}]")));
            }
        }
    }
}
