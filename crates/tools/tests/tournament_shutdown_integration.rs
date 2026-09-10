#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn run_case(mode: &str, concurrency: u32) -> (tempfile::TempDir, serde_json::Value, String) {
    let dir = tempfile::tempdir().unwrap();
    let engine = dir.path().join("engine.sh");
    fs::write(
        &engine,
        r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    usi)
      if [ "$MODE" = "fail" ]; then exit 1; fi
      printf 'id name shutdown-mock
usiok
' ;;
    isready) printf 'readyok
' ;;
    go*)
      if [ "$MODE" = "interrupt" ]; then
        if mkdir "$GATE/first" 2>/dev/null; then
          if [ "$CONCURRENCY" = "2" ]; then
            attempts=0
            while [ ! -d "$GATE/second" ] && [ "$attempts" -lt 100 ]; do
              sleep 0.02
              attempts=$((attempts + 1))
            done
          fi
          kill -INT "$PPID"
          sleep 0.2
        else
          mkdir "$GATE/second"
          sleep 0.5
        fi
      fi
      printf 'bestmove resign
' ;;
    quit) break ;;
  esac
done
"#,
    )
    .unwrap();
    fs::set_permissions(&engine, fs::Permissions::from_mode(0o755)).unwrap();
    let stdout = dir.path().join("stdout");
    let stderr = dir.path().join("stderr");
    let mut child = OwnedChild(
        Command::new(env!("CARGO_BIN_EXE_tournament"))
            .args(["--engine"])
            .arg(&engine)
            .args(["--engine-label", "a", "--engine"])
            .arg(&engine)
            .args([
                "--engine-label",
                "b",
                "--games",
                "1",
                "--nodes",
                "1",
                "--concurrency",
            ])
            .arg(concurrency.to_string())
            .arg("--out-dir")
            .arg(dir.path().join("out"))
            .env("MODE", mode)
            .env("GATE", dir.path())
            .env("CONCURRENCY", concurrency.to_string())
            .stdout(Stdio::from(fs::File::create(&stdout).unwrap()))
            .stderr(Stdio::from(fs::File::create(&stderr).unwrap()))
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
            "shutdown timed out: {}",
            fs::read_to_string(&stderr).unwrap()
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(status.success(), mode == "complete", "{}", fs::read_to_string(&stderr).unwrap());
    let meta =
        serde_json::from_slice(&fs::read(dir.path().join("out/meta.json")).unwrap()).unwrap();
    let output = fs::read_to_string(stdout).unwrap();
    (dir, meta, output)
}

#[test]
fn interrupt_saves_both_inflight_results_and_marks_run_invalid() {
    let (dir, meta, output) = run_case("interrupt", 2);
    let rows = fs::read_to_string(dir.path().join("out/a-vs-b.jsonl")).unwrap();
    let results = rows
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|row| row["type"] == "result")
        .collect::<Vec<_>>();
    assert_eq!(results.len(), 2, "両 worker の終了を待った結果が保存される");
    assert_eq!(results[0]["pair_index"], results[1]["pair_index"]);
    assert_ne!(results[0]["pair_slot"], results[1]["pair_slot"]);
    assert_eq!(meta["run_status"], "interrupted");
    assert_eq!(meta["invalid"], true);
    assert_eq!(meta["incomplete_pairs"], 0);
    assert_eq!(meta["unreturned_games"], 0);
    assert!(output.contains("Tournament Interrupted"));
    assert!(!output.contains("Tournament Complete"));
}

#[test]
fn interrupt_records_unfinished_pair() {
    let (dir, meta, _) = run_case("interrupt", 1);
    let rows = fs::read_to_string(dir.path().join("out/a-vs-b.jsonl")).unwrap();
    assert_eq!(
        rows.lines()
            .filter(
                |line| serde_json::from_str::<serde_json::Value>(line).unwrap()["type"] == "result"
            )
            .count(),
        1
    );
    assert_eq!(meta["incomplete_pairs"], 1);
    assert_eq!(meta["invalid"], true);
}

#[test]
fn initialization_failure_exits_without_waiting_for_ticket_and_records_failure() {
    let (_, meta, output) = run_case("fail", 1);
    assert_eq!(meta["run_status"], "worker_failed");
    assert_eq!(meta["invalid"], true);
    assert!(!output.contains("Tournament Complete"));
}

#[test]
fn normal_completion_remains_valid() {
    let (_, meta, output) = run_case("complete", 1);
    assert_eq!(meta["run_status"], "completed");
    assert_eq!(meta["invalid"], false);
    assert_eq!(meta["incomplete_pairs"], 0);
    assert_eq!(meta["unreturned_games"], 0);
    assert!(output.contains("Tournament Complete"));
}
