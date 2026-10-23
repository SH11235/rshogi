use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use std::process::Command;

fn write_log(path: &Path, explicit_winner: bool, outcomes: &[(&str, &str)]) {
    let mut lines = vec![json!({
        "type": "meta", "settings": {"games": outcomes.len() * 2},
        "engine_cmd": {"path_black":"base","path_white":"test","label_black":"base","label_white":"test"}
    }).to_string()];
    for (pair, &(a, b)) in outcomes.iter().enumerate() {
        for (slot, outcome) in [a, b].into_iter().enumerate() {
            let mut row =
                json!({"type":"result","outcome":outcome,"pair_index":pair,"pair_slot":slot});
            if explicit_winner {
                let winner = match (outcome, slot) {
                    ("black_win", 0) | ("white_win", 1) => "base",
                    _ => "test",
                };
                row["winner"] = json!(winner);
            }
            lines.push(row.to_string());
        }
    }
    fs::write(
        path,
        lines.join(
            "
",
        ) + "
",
    )
    .unwrap();
}

fn analyze(paths: &[&Path], partial: bool) -> (bool, Value) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_analyze_selfplay"));
    command.args(paths).args([
        "--json",
        "--sprt",
        "--sprt-base-label",
        "base",
        "--sprt-test-label",
        "test",
        "--sprt-nelo0",
        "0",
        "--sprt-nelo1",
        "10",
        "--sprt-alpha",
        "0.05",
        "--sprt-beta",
        "0.05",
    ]);
    if partial {
        command.arg("--allow-partial");
    }
    let output = command.output().unwrap();
    let value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&output.stderr)));
    (output.status.success(), value)
}

#[test]
fn missing_and_corrupt_files_cannot_turn_partial_counts_into_acceptance() {
    let dir = tempfile::tempdir().unwrap();
    let good = dir.path().join("good.jsonl");
    write_log(&good, true, &vec![("white_win", "black_win"); 1000]);
    let (success, control) = analyze(&[&good], false);
    assert!(success);
    assert_eq!(control["sprt"]["decision"], "accept_h1");
    for corrupt in [false, true] {
        let bad = dir.path().join(if corrupt {
            "corrupt.jsonl"
        } else {
            "missing.jsonl"
        });
        if corrupt {
            fs::write(&bad, "{broken JSON").unwrap();
        }
        for partial in [false, true] {
            let (success, output) = analyze(&[&good, &bad], partial);
            assert_eq!(success, partial);
            assert_eq!(output["extra"]["invalid"], true);
            assert!(!output["extra"]["input_issues"].as_array().unwrap().is_empty());
            assert_eq!(output["sprt"]["decision"], "invalid");
            assert_eq!(output["sprt"]["pairs"], 1000);
        }
    }
}

#[test]
fn tournament_run_status_and_metadata_read_errors_are_not_legacy_absence() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("games.jsonl");
    write_log(&log, true, &[("white_win", "black_win")]);
    assert!(analyze(&[&log], false).0); // meta.json が無い旧ログ
    let meta = dir.path().join("meta.json");
    for value in [
        json!({"run_status":"interrupted","invalid":true,"incomplete_pairs":0,"unreturned_games":0}),
        json!({"run_status":"worker_failed","invalid":true}),
        json!({"run_status":"running"}),
        json!({"run_status":"unknown-future-state"}),
        json!({"run_status":"completed","incomplete_pairs":1}),
        json!({"run_status":"completed","unreturned_games":1}),
        json!({"invalid":true}),
    ] {
        fs::write(&meta, value.to_string()).unwrap();
        let (success, output) = analyze(&[&log], false);
        assert!(!success);
        assert_eq!(output["sprt"]["decision"], "invalid");
        assert_eq!(output["extra"]["invalid"], true);
    }
    for valid in [
        json!({"old_schema":1}),
        json!({"run_status":"completed","invalid":false}),
    ] {
        fs::write(&meta, valid.to_string()).unwrap();
        assert!(analyze(&[&log], false).0);
    }
    fs::write(&meta, "{invalid").unwrap();
    assert!(!analyze(&[&log], false).0);
    fs::remove_file(&meta).unwrap();
    fs::create_dir(&meta).unwrap(); // 存在するが通常ファイルとして読めない
    assert!(!analyze(&[&log], false).0);
}

#[test]
fn pair_boundary_interruption_cannot_become_formal_acceptance() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("games.jsonl");
    write_log(&log, true, &vec![("white_win", "black_win"); 1000]);
    let text = fs::read_to_string(&log).unwrap();
    let (_, results) = text.split_once('\n').unwrap();
    let mut header: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    header["settings"]["games"] = json!(10_000);
    fs::write(&log, format!("{header}\n{results}")).unwrap();
    let meta = dir.path().join("meta.json");
    for status in ["running", "interrupted", "worker_failed"] {
        fs::write(
            &meta,
            json!({
                "run_status": status, "invalid": false,
                "incomplete_pairs": 0, "unreturned_games": 0
            })
            .to_string(),
        )
        .unwrap();
        for partial in [false, true] {
            let (success, output) = analyze(&[&log], partial);
            assert_eq!(success, partial, "{status}");
            assert_eq!(output["sprt"]["pairs"], 1000);
            assert_eq!(output["sprt"]["decision"], "invalid");
            assert_eq!(output["extra"]["invalid"], true);
        }
    }
    // SPRT や動的目標変更による正常な早期終了は、起動時の予定局数未満でも有効。
    fs::write(
        &meta,
        json!({
            "run_status": "completed", "invalid": false,
            "incomplete_pairs": 0, "unreturned_games": 0
        })
        .to_string(),
    )
    .unwrap();
    let (success, output) = analyze(&[&log], false);
    assert!(success);
    assert_eq!(output["sprt"]["decision"], "accept_h1");
}

#[test]
fn explicit_and_legacy_winners_agree_for_both_slots_and_draws() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("games.jsonl");
    let outcomes = [
        ("white_win", "black_win"),
        ("draw", "draw"),
        ("black_win", "white_win"),
    ];
    write_log(&log, true, &outcomes);
    let (success, explicit) = analyze(&[&log], false);
    assert!(success);
    write_log(&log, false, &outcomes);
    let (success, legacy) = analyze(&[&log], false);
    assert!(success);
    assert_eq!(explicit, legacy);
    assert_eq!(legacy["matchups"][0]["black_wins"], 2);
    assert_eq!(legacy["matchups"][0]["white_wins"], 2);
    assert_eq!(legacy["matchups"][0]["draws"], 2);
    assert_eq!(legacy["sprt"]["penta"]["ww"], 1);
    assert_eq!(legacy["sprt"]["penta"]["dd"], 1);
    assert_eq!(legacy["sprt"]["penta"]["ll"], 1);
}

#[test]
fn complete_json_line_with_missing_partner_is_still_partial_input() {
    use std::io::Write as _;
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("truncated.jsonl");
    write_log(&log, true, &vec![("white_win", "black_win"); 1000]);
    let mut file = fs::OpenOptions::new().append(true).open(&log).unwrap();
    writeln!(file, "{}", json!({"type":"result","outcome":"white_win","winner":"test","pair_index":1000,"pair_slot":0})).unwrap();
    drop(file);
    for partial in [false, true] {
        let (success, output) = analyze(&[&log], partial);
        assert_eq!(success, partial);
        assert_eq!(output["extra"]["invalid"], true);
        assert_eq!(output["sprt"]["decision"], "invalid");
        assert_eq!(output["sprt"]["pairs"], 1000);
    }
}

#[test]
fn by_startpos_cli_merges_directories_and_reports_legacy_exclusions() {
    let dirs = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
    for dir in &dirs {
        fs::write(
            dir.path().join("control_history.jsonl"),
            json!({"type":"control","target_games":2}).to_string(),
        )
        .unwrap();
        let log = dir.path().join("games.jsonl");
        write_log(&log, true, &[("black_win", "white_win"), ("draw", "draw")]);
        let lines: Vec<String> = fs::read_to_string(&log)
            .unwrap()
            .lines()
            .enumerate()
            .map(|(i, line)| {
                let mut row: Value = serde_json::from_str(line).unwrap();
                if i == 1 || i == 2 {
                    row["startpos_idx"] = json!(0);
                }
                row.to_string()
            })
            .collect();
        fs::write(&log, lines.join("\n")).unwrap();
        fs::write(
            dir.path().join("meta.json"),
            json!({"start_positions":["position startpos moves 7g7f"]}).to_string(),
        )
        .unwrap();
    }
    for json_output in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_analyze_selfplay"));
        command.arg("--by-startpos").args(dirs.iter().map(|dir| dir.path()));
        if json_output {
            command.arg("--json");
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("開始局面別集計から 4 局を除外"), "{stderr}");
        if json_output {
            let value: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(value["by_startpos"]["excluded_games"], 4);
            let rows = value["by_startpos"]["rows"].as_array().unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0]["games"], 4);
            assert_eq!(rows[0]["black_wins"], 2);
            assert_eq!(rows[0]["white_wins"], 2);
        } else {
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(stdout.contains("1: position startpos moves 7g7f"));
            assert!(stdout.contains("1 4 2 2 0 0.5000"));
            assert!(stdout.contains("unknown=4"));
        }
    }
    let output = Command::new(env!("CARGO_BIN_EXE_analyze_selfplay"))
        .arg("--json")
        .arg(dirs[0].path())
        .output()
        .unwrap();
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(value.get("by_startpos").is_none());
}
