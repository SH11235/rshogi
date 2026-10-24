//! spsa の棋譜 (`games.jsonl`) と batch の摂動 (`batches.jsonl`) の結合テスト。
//!
//! 読み方は crates/tools/docs/spsa.md の規則に従う: 読めない行は捨て、async は journal の `received`、
//! batch は `meta.completed_pairs` で確定した局だけを、番号ごとに後の行で採る。

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::spsa_run_dir_integration::{PoolTest, assert_success};

/// 改行で終わる行だけを返す。改行の無い末尾は JSON として完結していても書き込み途中の残骸として捨てる。
fn lines(test: &PoolTest, file: &str) -> Vec<String> {
    let text = String::from_utf8(test.bytes(file)).unwrap();
    let mut lines: Vec<String> = text.split('\n').map(str::to_owned).collect();
    lines.pop();
    lines
}

/// 読めない行を捨てて JSON にする。
fn records(test: &PoolTest, file: &str) -> Vec<serde_json::Value> {
    lines(test, file)
        .iter()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn run_async(
    test: &PoolTest,
    total: u32,
    extra: &[&str],
    env: &[(&str, &Path)],
) -> std::process::Output {
    let mut args = vec!["--dispatch", "async", "--snapshot-interval", "2"];
    args.extend_from_slice(extra);
    test.run_with("value_driven", total, 1, &args, env)
}

fn assert_game_line(game: &serde_json::Value) {
    for key in [
        "schema",
        "version",
        "mode",
        "pair_id",
        "pair_slot",
        "game_id",
        "plus_is_black",
        "startpos_idx",
        "startpos_sfen",
        "outcome",
        "plus_score",
        "reason",
        "plies",
        "retries",
        "moves",
        "elapsed_ms",
    ] {
        assert!(game.get(key).is_some(), "missing {key}: {game}");
    }
    assert_eq!(game["schema"], "spsa_game");
    assert_eq!(game["version"], 1);
    assert_eq!(game["plus_is_black"], game["pair_slot"] == 0, "{game}");
    let plies = game["plies"].as_u64().unwrap() as usize;
    for column in ["moves", "elapsed_ms", "score_cp", "depth", "nodes"] {
        assert_eq!(game[column].as_array().unwrap().len(), plies, "{column}: {game}");
    }
    // mock は cp と depth・nodes だけを出すので、全 null の列は省かれる。
    assert!(game.get("score_mate").is_none() && game.get("score_bound").is_none(), "{game}");
    assert_eq!(game["depth"][0], 3);
    let plus_score = game["plus_score"].as_i64().unwrap();
    let black_score = if game["plus_is_black"] == true {
        plus_score
    } else {
        -plus_score
    };
    let expected = match black_score {
        1 => "black_win",
        -1 => "white_win",
        _ => "draw",
    };
    assert_eq!(game["outcome"], expected, "{game}");
}

/// async の規則: journal の received と (pair_id, attempt) が一致する行を、slot ごとに後勝ちで採る。
fn confirmed_async_games(test: &PoolTest) -> BTreeMap<(u64, u64), serde_json::Value> {
    let received: BTreeMap<u64, serde_json::Value> = records(test, "journal.jsonl")
        .into_iter()
        .filter(|r| r["kind"] == "received")
        .map(|r| (r["pair_id"].as_u64().unwrap(), r))
        .collect();
    let mut games = BTreeMap::new();
    for game in records(test, "games.jsonl") {
        let pair_id = game["pair_id"].as_u64().unwrap();
        if received.get(&pair_id).is_some_and(|r| r["attempt"] == game["attempt"]) {
            games.insert((pair_id, game["pair_slot"].as_u64().unwrap()), game);
        }
    }
    for (pair_id, record) in &received {
        for slot in 0..2u64 {
            let game = games.get(&(*pair_id, slot)).unwrap_or_else(|| {
                panic!("received pair {pair_id} slot {slot} has no game record")
            });
            let journal_game = &record["games"][slot as usize];
            for key in ["plus_is_black", "plus_score", "reason", "plies"] {
                assert_eq!(game[key], journal_game[key], "pair {pair_id} slot {slot} {key}");
            }
        }
    }
    games
}

/// batch の規則: (pair_id, slot) ごとに後勝ちで採り、pair_id < completed_pairs のものを残す。
fn confirmed_batch_games(test: &PoolTest) -> BTreeMap<(u64, u64), serde_json::Value> {
    let completed = test.meta()["completed_pairs"].as_u64().unwrap();
    let last: BTreeMap<(u64, u64), serde_json::Value> = records(test, "games.jsonl")
        .into_iter()
        .map(|g| ((g["pair_id"].as_u64().unwrap(), g["pair_slot"].as_u64().unwrap()), g))
        .collect();
    last.into_iter().filter(|((pair_id, _), _)| *pair_id < completed).collect()
}

/// batches.jsonl の規則: k_pair_start ごとに後勝ちで採り、k_pair_start + batch_pairs <= completed_pairs を残す。
fn confirmed_batches(test: &PoolTest) -> BTreeMap<u64, serde_json::Value> {
    let completed = test.meta()["completed_pairs"].as_u64().unwrap();
    let last: BTreeMap<u64, serde_json::Value> = records(test, "batches.jsonl")
        .into_iter()
        .map(|b| (b["k_pair_start"].as_u64().unwrap(), b))
        .collect();
    last.into_iter()
        .filter(|(k, b)| k + b["batch_pairs"].as_u64().unwrap() <= completed)
        .collect()
}

#[test]
fn batch_writes_one_line_per_game_and_batch() {
    let test = PoolTest::new();
    assert_success(&test.run("value_driven", 4, 2, 1, &[], &[]));
    let games = records(&test, "games.jsonl");
    assert_eq!(games.len(), 8);
    for (index, game) in games.iter().enumerate() {
        assert_game_line(game);
        assert_eq!(game["mode"], "batch");
        assert_eq!(game["pair_id"], index as u64 / 2);
        assert_eq!(game["iteration"], index as u64 / 4 + 1);
        assert!(game.get("attempt").is_none() && game.get("dispatch_k").is_none());
    }
    for pair in games.chunks(2) {
        assert_eq!(pair[0]["startpos_idx"], pair[1]["startpos_idx"]);
        assert_eq!(pair[0]["startpos_sfen"], pair[1]["startpos_sfen"]);
    }
    let batches = records(&test, "batches.jsonl");
    assert_eq!(batches.len(), 2);
    let active = test.meta()["active_param_count"].as_u64().unwrap() as usize;
    for (index, batch) in batches.iter().enumerate() {
        assert_eq!(batch["schema"], "spsa_batch");
        assert_eq!(batch["iteration"], index as u64 + 1);
        assert_eq!(batch["k_pair_start"], index as u64 * 2);
        for column in ["params", "c_k", "r_k", "flip", "plus", "minus"] {
            assert_eq!(batch[column].as_array().unwrap().len(), active, "{column}");
        }
        assert!(batch["flip"].as_array().unwrap().iter().all(|f| f.as_i64().unwrap().abs() == 1));
    }
    assert_eq!(test.meta()["game_records"], true);
}

#[test]
fn batch_resume_keeps_committed_lines_and_reader_drops_torn_tail() {
    let test = PoolTest::new();
    // 各 engine の 2 回目の go が batch 2。batch 2 の途中で止める。
    let output = test.run(
        "value_driven",
        4,
        1,
        1,
        &["--engine-retries", "0"],
        &[
            ("SPSA_TEST_ENGINE_FAIL_ONCE_MARKER", &test.path("failed")),
            ("SPSA_TEST_ENGINE_FAIL_AFTER_GO", Path::new("1")),
        ],
    );
    assert!(!output.status.success());
    assert_eq!(lines(&test, "games.jsonl").len(), 2);
    assert_eq!(lines(&test, "batches.jsonl").len(), 1);
    // 書き込み途中で落ちた末尾を模す。JSON として完結していても改行が無ければ読む側は捨て、
    // resume は改行を補って次の行と連結させない。
    let path = test.path("run/games.jsonl");
    let mut bytes = std::fs::read(&path).unwrap();
    let last_line = lines(&test, "games.jsonl").pop().unwrap();
    bytes.extend_from_slice(last_line.as_bytes());
    std::fs::write(&path, bytes).unwrap();
    assert_eq!(records(&test, "games.jsonl").len(), 2);
    assert_success(&test.run("value_driven", 4, 1, 1, &["--resume"], &[]));
    assert_eq!(lines(&test, "games.jsonl").len(), 9);
    assert_eq!(records(&test, "games.jsonl").len(), 9);
    let confirmed = confirmed_batch_games(&test);
    assert_eq!(confirmed.len(), 8);
    let iterations: BTreeSet<u64> = records(&test, "batches.jsonl")
        .iter()
        .map(|b| b["iteration"].as_u64().unwrap())
        .collect();
    assert_eq!(iterations, (1..=4).collect());
}

#[test]
fn no_game_records_writes_nothing_and_resume_cannot_toggle() {
    let test = PoolTest::new();
    let output = test.run(
        "value_driven",
        4,
        1,
        1,
        &["--no-game-records", "--engine-retries", "0"],
        &[
            ("SPSA_TEST_ENGINE_FAIL_ONCE_MARKER", &test.path("failed")),
            ("SPSA_TEST_ENGINE_FAIL_AFTER_GO", Path::new("1")),
        ],
    );
    assert!(!output.status.success());
    assert!(!test.path("run/games.jsonl").exists());
    assert!(!test.path("run/batches.jsonl").exists());
    assert_eq!(test.meta()["game_records"], false);
    let output = test.run("value_driven", 4, 1, 1, &["--resume"], &[]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("game records"));
    assert_success(&test.run("value_driven", 4, 1, 1, &["--resume", "--no-game-records"], &[]));
    assert!(!test.path("run/games.jsonl").exists());

    let test = PoolTest::new();
    assert_success(&run_async(&test, 2, &["--no-game-records"], &[]));
    assert!(!test.path("run/games.jsonl").exists());
    assert_eq!(test.meta()["game_records"], false);
}

#[test]
fn async_writes_both_games_of_each_received_pair() {
    let test = PoolTest::new();
    assert_success(&run_async(&test, 6, &[], &[]));
    assert_eq!(records(&test, "games.jsonl").len(), 12);
    let dispatch_k: BTreeMap<u64, serde_json::Value> = records(&test, "journal.jsonl")
        .into_iter()
        .filter(|r| r["kind"] == "dispatch")
        .map(|r| (r["pair_id"].as_u64().unwrap(), r["dispatch_k"].clone()))
        .collect();
    let confirmed = confirmed_async_games(&test);
    assert_eq!(confirmed.len(), 12);
    for ((pair_id, _), game) in &confirmed {
        assert_game_line(game);
        assert_eq!(game["mode"], "async");
        assert_eq!(game["dispatch_k"], dispatch_k[pair_id]);
        assert!(game.get("iteration").is_none());
    }
    assert_eq!(test.meta()["game_records"], true);
}

#[test]
fn async_crash_points_keep_every_received_pair() {
    for point in [
        "after_games:3",
        "after_received:3",
        "after_accept:3",
        "after_dispatch:3",
    ] {
        let test = PoolTest::new();
        let output = run_async(&test, 6, &[], &[("RSHOGI_SPSA_FAULT", Path::new(point))]);
        assert_eq!(output.status.code(), Some(86), "{point}");
        assert_success(&run_async(&test, 6, &["--resume", "--force-unlock"], &[]));
        let confirmed = confirmed_async_games(&test);
        assert_eq!(confirmed.len(), 12, "{point}");
        if point == "after_games:3" {
            // received の前に落ちた attempt の 2 行は残るが、journal と照合して捨てる。
            assert_eq!(records(&test, "games.jsonl").len(), 14, "{point}");
        }
    }
}

#[test]
fn async_game_write_failure_stops_before_received() {
    let test = PoolTest::new();
    let output = run_async(&test, 6, &[], &[("RSHOGI_SPSA_FAULT", Path::new("games_write:2"))]);
    assert!(!output.status.success());
    assert_ne!(output.status.code(), Some(86));
    let received = records(&test, "journal.jsonl")
        .into_iter()
        .filter(|r| r["kind"] == "received")
        .count();
    assert_eq!(received, 1);
    assert_success(&run_async(&test, 6, &["--resume", "--force-unlock"], &[]));
    assert_eq!(confirmed_async_games(&test).len(), 12);
}

#[test]
fn async_resume_rejects_toggled_game_records() {
    let test = PoolTest::new();
    let output = run_async(&test, 4, &[], &[("RSHOGI_SPSA_FAULT", Path::new("after_accept:2"))]);
    assert_eq!(output.status.code(), Some(86));
    let output = run_async(&test, 4, &["--resume", "--force-unlock", "--no-game-records"], &[]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("game records"));
}

#[test]
fn batch_crash_after_records_rewrites_the_iteration() {
    let test = PoolTest::new();
    let output = test.run(
        "value_driven",
        4,
        1,
        1,
        &[],
        &[("RSHOGI_SPSA_FAULT", Path::new("batch_after_records:2"))],
    );
    assert_eq!(output.status.code(), Some(86));
    // batch 2 の棋譜は書いたが checkpoint の前に落ちた。
    assert_eq!(test.meta()["completed_iterations"], 1);
    assert_eq!(records(&test, "games.jsonl").len(), 4);
    assert_eq!(confirmed_batch_games(&test).len(), 2);
    assert_success(&test.run("value_driven", 4, 1, 1, &["--resume", "--force-unlock"], &[]));
    let games = records(&test, "games.jsonl");
    assert_eq!(games.len(), 10);
    let confirmed = confirmed_batch_games(&test);
    assert_eq!(confirmed.len(), 8);
    // 捨てた batch 2 の行より、指し直した batch 2 の行が採られる。
    assert_eq!(confirmed[&(1, 0)], games[4]);
    assert_eq!(confirmed[&(1, 1)], games[5]);
    assert_eq!(confirmed_batches(&test).len(), 4);
    let batches = records(&test, "batches.jsonl");
    assert_eq!(batches.len(), 5);
    assert_eq!(batches.iter().filter(|b| b["iteration"] == 2).count(), 2);
}

/// 記録機能より前の meta を模して game_records を消す (v3 は v4 専用の項目も消す)。
fn make_legacy_meta(test: &PoolTest, v3: bool) {
    let mut meta = test.meta();
    let fields = meta.as_object_mut().unwrap();
    fields.remove("game_records");
    if v3 {
        fields.insert("format_version".into(), serde_json::json!(3));
        for key in ["total_pairs", "batch_pairs", "completed_pairs"] {
            fields.remove(key);
        }
    }
    std::fs::write(test.path("run/meta.json"), serde_json::to_vec_pretty(&meta).unwrap()).unwrap();
}

fn run_bytes(test: &PoolTest, file: &str) -> Option<Vec<u8>> {
    std::fs::read(test.path(&format!("run/{file}"))).ok()
}

#[test]
fn batch_resume_with_smaller_batches_reads_by_pair_id() {
    let test = PoolTest::new();
    // batch 1 (pair 0・1) の棋譜を書いた後、checkpoint の前に落ちる。
    let output = test.run(
        "value_driven",
        4,
        2,
        1,
        &[],
        &[("RSHOGI_SPSA_FAULT", Path::new("batch_after_records:1"))],
    );
    assert_eq!(output.status.code(), Some(86));
    assert_eq!(test.meta()["completed_pairs"], 0);
    // batch の大きさを変えて指し直すと、pair 1 は iteration 2 として書き直される。
    assert_success(&test.run(
        "value_driven",
        4,
        1,
        1,
        &["--resume", "--force-unlock", "--force-schedule"],
        &[],
    ));
    let games = records(&test, "games.jsonl");
    assert_eq!(games.len(), 12);
    let confirmed = confirmed_batch_games(&test);
    assert_eq!(confirmed.len(), 8);
    for ((pair_id, _), game) in &confirmed {
        assert_eq!(game["iteration"], pair_id + 1, "{game}");
    }
    let batches = confirmed_batches(&test);
    assert_eq!(batches.keys().copied().collect::<Vec<_>>(), vec![0, 1, 2, 3]);
    assert!(batches.values().all(|b| b["batch_pairs"] == 1));
}

#[test]
fn legacy_batch_meta_with_records_rejects_disabling_on_resume() {
    for (v3, keep_games) in [(false, true), (true, true), (false, false), (true, false)] {
        let label = format!("v3={v3} keep_games={keep_games}");
        let test = PoolTest::new();
        let output = test.run(
            "value_driven",
            4,
            1,
            1,
            &[],
            &[("RSHOGI_SPSA_FAULT", Path::new("batch_after_records:2"))],
        );
        assert_eq!(output.status.code(), Some(86));
        make_legacy_meta(&test, v3);
        if !keep_games {
            // games.jsonl だけ失われても batches.jsonl で記録していた run と分かる。
            std::fs::remove_file(test.path("run/games.jsonl")).unwrap();
        }
        let before: Vec<_> = ["meta.json", "games.jsonl", "batches.jsonl"]
            .map(|f| run_bytes(&test, f))
            .into();
        let output = test.run(
            "value_driven",
            4,
            1,
            1,
            &["--resume", "--force-unlock", "--no-game-records"],
            &[],
        );
        assert!(!output.status.success(), "{label}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("game records"), "{label}");
        let after: Vec<_> = ["meta.json", "games.jsonl", "batches.jsonl"]
            .map(|f| run_bytes(&test, f))
            .into();
        assert_eq!(after, before, "{label}");
    }
}

#[test]
fn legacy_batch_meta_without_records_allows_either_setting() {
    for v3 in [false, true] {
        let test = PoolTest::new();
        let output = test.run(
            "value_driven",
            4,
            1,
            1,
            &["--no-game-records", "--engine-retries", "0"],
            &[
                ("SPSA_TEST_ENGINE_FAIL_ONCE_MARKER", &test.path("failed")),
                ("SPSA_TEST_ENGINE_FAIL_AFTER_GO", Path::new("1")),
            ],
        );
        assert!(!output.status.success());
        make_legacy_meta(&test, v3);
        // 起動条件の検証で止まる resume は記録ファイルを作らない。
        let output = test.run(
            "value_driven",
            4,
            1,
            1,
            &[
                "--resume",
                "--force-unlock",
                "--active-only-regex",
                "^NO_SUCH_PARAM$",
            ],
            &[],
        );
        assert!(!output.status.success(), "v3={v3}");
        assert!(!test.path("run/games.jsonl").exists(), "v3={v3}");
        assert!(!test.path("run/batches.jsonl").exists(), "v3={v3}");
        let extra: &[&str] = if v3 {
            &[
                "--resume",
                "--force-unlock",
                "--no-game-records",
                "--force-schedule",
            ]
        } else {
            &["--resume", "--force-unlock", "--no-game-records"]
        };
        assert_success(&test.run("value_driven", 4, 1, 1, extra, &[]));
        assert!(!test.path("run/games.jsonl").exists(), "v3={v3}");
    }
}

#[test]
fn async_duplicate_result_is_not_written_twice() {
    let test = PoolTest::new();
    assert_success(&run_async(
        &test,
        4,
        &[],
        &[("RSHOGI_SPSA_FAULT", Path::new("duplicate_result:1"))],
    ));
    let games = records(&test, "games.jsonl");
    assert_eq!(games.iter().filter(|g| g["pair_id"] == 1).count(), 2);
    assert_eq!(games.len(), 8);
    assert_eq!(confirmed_async_games(&test).len(), 8);
}

#[test]
fn fresh_start_replaces_previous_records() {
    let test = PoolTest::new();
    assert_success(&test.run("value_driven", 4, 2, 1, &[], &[]));
    assert_success(&test.run("value_driven", 2, 1, 1, &["--force-init"], &[]));
    assert_eq!(records(&test, "games.jsonl").len(), 4);
    assert_eq!(records(&test, "batches.jsonl").len(), 2);
    // batch の run-dir を async で作り直すと、async は書かない batches.jsonl も消える。
    assert_success(&run_async(&test, 2, &["--force-init"], &[]));
    assert_eq!(records(&test, "games.jsonl").len(), 4);
    assert!(!test.path("run/batches.jsonl").exists());
    assert_success(&test.run("value_driven", 2, 1, 1, &["--force-init", "--no-game-records"], &[]));
    assert!(!test.path("run/games.jsonl").exists());
    assert!(!test.path("run/batches.jsonl").exists());
}
