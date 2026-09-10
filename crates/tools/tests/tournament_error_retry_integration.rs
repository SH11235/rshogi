#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::process::Command;

use serde_json::Value;

fn write_engine(path: &std::path::Path, script: &str) {
    fs::write(path, script).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

fn run_error_retry_case(isready_action: &str, go_action: &str) {
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
             printf 'launch\\n' >> '{}'\n\
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

    let output = Command::new(env!("CARGO_BIN_EXE_tournament"))
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
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "tournament failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let launch_count = fs::read_to_string(&launches).unwrap().lines().count();
    assert_eq!(launch_count, 2, "error 後に flaky engine が新規起動されるべき");

    let jsonl = fs::read_to_string(out_dir.join("pair-0-1.jsonl")).unwrap();
    let results: Vec<Value> = jsonl
        .lines()
        .map(serde_json::from_str::<Value>)
        .filter_map(Result::ok)
        .filter(|row| row["type"] == "result")
        .collect();
    assert_eq!(results.len(), 4, "初回 2 局と再試行 2 局を出力するべき");
    assert_eq!(results.iter().filter(|row| row["error"] == true).count(), 1);
    assert_eq!(results.iter().filter(|row| row["attempt"] == 1).count(), 2);
    assert!(
        results.iter().filter(|row| row["attempt"] == 1).all(|row| row["error"] != true),
        "新しい process での再試行は成功するべき"
    );

    let meta: Value =
        serde_json::from_slice(&fs::read(out_dir.join("meta.json")).unwrap()).unwrap();
    assert_eq!(meta["error_pairs"], 1);
    assert_eq!(meta["retried_pairs"], 1);
    assert_eq!(meta["exhausted_pairs"], 0);
    assert_eq!(meta["invalid"], false);
}

#[test]
fn communication_error_retires_worker_and_retry_uses_new_processes() {
    run_error_retry_case(
        "printf 'readyok\\n'",
        "if [ ! -e \"$FAILED_ONCE\" ]; then : > \"$FAILED_ONCE\"; exit 1; fi; \
         printf 'bestmove resign\\n'",
    );
}

#[test]
fn new_game_error_retires_worker_and_retry_uses_new_processes() {
    run_error_retry_case(
        "if [ \"$isready_count\" -ge 2 ] && [ ! -e \"$FAILED_ONCE\" ]; then \
         : > \"$FAILED_ONCE\"; exit 1; fi; printf 'readyok\\n'",
        "printf 'bestmove resign\\n'",
    );
}
