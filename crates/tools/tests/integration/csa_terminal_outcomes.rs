use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command;

use rshogi_csa::{Color, initial_position};
use rshogi_csa_client::config::CsaClientConfig;
use rshogi_csa_client::jsonl::write_game_jsonl;
use rshogi_csa_client::protocol::{GameResult, GameSummary, TimeConfig};
use rshogi_csa_client::record::{GameRecord, JsonlMoveExtra};
use serde_json::Value;

fn fixture(dir: &Path, result: &GameResult, reason: &str) -> PathBuf {
    let mut record = GameRecord::new(&GameSummary {
        game_id: "terminal-test".to_owned(),
        my_color: Color::Black,
        sente_name: "alice".to_owned(),
        gote_name: "bob".to_owned(),
        position: initial_position(),
        initial_moves: Vec::new(),
        black_time: TimeConfig::default(),
        white_time: TimeConfig::default(),
        reconnect_token: None,
        entering_king_rule: None,
    });
    record.result = reason.to_owned();
    record.add_move("+7776FU", 1, None, Color::Black);
    record.add_jsonl_move(JsonlMoveExtra {
        sfen_before: "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1".to_owned(),
        move_usi: "7g7f".to_owned(),
        engine_label: "alice".to_owned(),
        elapsed_ms: 1000,
        think_limit_ms: 1000,
        seldepth: None,
        nodes: None,
        time_ms: None,
        nps: None,
    });
    write_game_jsonl(dir, &record, &CsaClientConfig::default(), result).unwrap()
}

fn check_readers(path: &Path, unfinished: bool, expected: &str, publish_ok: bool) {
    let outputs = tempfile::tempdir().unwrap();
    let kif = outputs.path().join("game.kif");
    tools::kif::convert_jsonl_to_kif(path, &kif, &tools::kif::GameFilter::default()).unwrap();
    let kif = fs::read_to_string(kif).unwrap();
    let ending = if unfinished {
        "中断"
    } else {
        match expected {
            "black_win" => "先手の勝ち",
            "white_win" => "後手の勝ち",
            _ => "引き分け",
        }
    };
    assert!(kif.contains(&format!("まで1手で{ending}")), "{kif}");
    if unfinished {
        assert!(!kif.contains("引き分け"));
    }

    #[cfg(feature = "csa-replay")]
    {
        use tools::replay::{GameOutcomeView, GameSource, JsonlSource};
        let index = JsonlSource::new(path.parent().unwrap()).build_index().unwrap();
        assert_eq!(index.entries.len(), 1);
        let outcome = if unfinished {
            None
        } else {
            Some(match expected {
                "black_win" => GameOutcomeView::Win(rshogi_core::types::Color::Black),
                "white_win" => GameOutcomeView::Win(rshogi_core::types::Color::White),
                _ => GameOutcomeView::Draw,
            })
        };
        assert_eq!(index.entries[0].outcome, outcome);
    }
    let mut published = Vec::new();
    tools::kifu_publish::convert_jsonl(
        Cursor::new(fs::read(path).unwrap()),
        "test",
        None,
        |_, result| {
            published.push(result.is_ok());
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(published, vec![publish_ok]);

    let psv = outputs.path().join("games.psv");
    let output = Command::new(env!("CARGO_BIN_EXE_jsonl_to_psv"))
        .arg("--input")
        .arg(path)
        .arg("--output")
        .arg(&psv)
        .args(["--missing-score", "zero"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(fs::metadata(psv).unwrap().len(), if unfinished { 0 } else { 40 });
    let bench = outputs.path().join("bench");
    let output = Command::new(env!("CARGO_BIN_EXE_extract_bench_positions"))
        .arg("--jsonl")
        .arg(path)
        .arg("--out-dir")
        .arg(&bench)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let stats: Value =
        serde_json::from_slice(&fs::read(bench.join("stats.json")).unwrap()).unwrap();
    if unfinished {
        assert_eq!(stats["skipped_by_reason"]["jsonl_error_or_in_progress"], 1);
    } else {
        assert_eq!(stats["games_by_source"]["selfplay"], 1);
    }
}

#[test]
fn writer_terminal_results_reach_readers_without_inventing_draws() {
    for (result, expected, reason, unfinished) in [
        (GameResult::Win, "black_win", "resign", false),
        (GameResult::Lose, "white_win", "resign", false),
        (GameResult::Draw, "draw", "sennichite", false),
        (GameResult::Interrupted, "interrupted", "interrupted", true),
        (GameResult::Censored, "censored", "max_moves", true),
    ] {
        for reason in ["", reason] {
            let dir = tempfile::tempdir().unwrap();
            let path = fixture(dir.path(), &result, reason);
            let text = fs::read_to_string(&path).unwrap();
            let row: Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
            assert_eq!(row["outcome"], expected);
            assert_eq!(row["reason"], if reason.is_empty() { expected } else { reason });
            if unfinished {
                assert!(row.get("winner").is_none());
            }
            check_readers(&path, unfinished, expected, !unfinished && !reason.is_empty());
            let analysis = Command::new(env!("CARGO_BIN_EXE_analyze_selfplay"))
                .arg(&path)
                .arg("--json")
                .output()
                .unwrap();
            assert_eq!(analysis.status.success(), !unfinished);
            if !unfinished {
                let value: Value = serde_json::from_slice(&analysis.stdout).unwrap();
                assert_eq!(value["progress"]["done"], 1);
                assert_eq!(value["matchups"][0]["draws"], u32::from(expected == "draw"));
            }
            let flood = Command::new(env!("CARGO_BIN_EXE_floodgate_record"))
                .arg("--dir")
                .arg(dir.path())
                .args(["--me", "alice"])
                .env_remove("CSA_CLIENT_CONFIG")
                .output()
                .unwrap();
            let text = String::from_utf8_lossy(&flood.stdout);
            if unfinished {
                assert!(!flood.status.success());
                assert!(String::from_utf8_lossy(&flood.stderr).contains("完了局の JSONL"));
                assert!(!text.contains("(1局)"), "{text}");
                check_partial_analysis(&path);
            } else {
                assert!(flood.status.success(), "{}", String::from_utf8_lossy(&flood.stderr));
                let counts = match expected {
                    "black_win" => "1勝 0敗 0分",
                    "white_win" => "0勝 1敗 0分",
                    _ => "0勝 0敗 1分",
                };
                assert!(text.contains(counts), "{text}");
            }
        }
    }
}

fn rewrite(path: &Path, source: Option<&str>, reason: &str) {
    let rows: Vec<String> = fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| {
            let mut row: Value = serde_json::from_str(line).unwrap();
            if row["type"] == "meta" {
                if let Some(source) = source {
                    row["source"] = source.into();
                } else {
                    row.as_object_mut().unwrap().remove("source");
                }
            }
            if row["type"] == "result" {
                row["outcome"] = "draw".into();
                row["reason"] = reason.into();
            }
            row.to_string()
        })
        .collect();
    fs::write(path, rows.join("\n") + "\n").unwrap();
}

#[test]
fn legacy_reasons_are_csa_only_and_ambiguous_max_moves_stays_draw() {
    for source in [None, Some("csa_client"), Some("tournament")] {
        for reason in ["interrupted", "censored", "draw", "max_moves"] {
            let dir = tempfile::tempdir().unwrap();
            let path = fixture(dir.path(), &GameResult::Draw, reason);
            rewrite(&path, source, reason);
            let unfinished =
                source != Some("tournament") && matches!(reason, "interrupted" | "censored");
            check_readers(&path, unfinished, "draw", reason == "max_moves");
            let flood = Command::new(env!("CARGO_BIN_EXE_floodgate_record"))
                .arg("--dir")
                .arg(dir.path())
                .args(["--me", "alice"])
                .env_remove("CSA_CLIENT_CONFIG")
                .output()
                .unwrap();
            if unfinished {
                assert!(!flood.status.success());
                assert!(String::from_utf8_lossy(&flood.stderr).contains("完了局の JSONL"));
            } else {
                assert!(flood.status.success(), "{}", String::from_utf8_lossy(&flood.stderr));
                let counted = source == Some("csa_client") || reason == "max_moves";
                let text = String::from_utf8_lossy(&flood.stdout);
                assert!(
                    text.contains(if counted {
                        "0勝 0敗 1分"
                    } else {
                        "0勝 0敗 0分"
                    }),
                    "{text}"
                );
            }
            if source != Some("tournament") {
                let analysis = Command::new(env!("CARGO_BIN_EXE_analyze_selfplay"))
                    .arg(&path)
                    .arg("--json")
                    .output()
                    .unwrap();
                assert_eq!(analysis.status.success(), !unfinished);
            }
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let path = fixture(dir.path(), &GameResult::Draw, "max_moves");
    check_readers(&path, false, "draw", true);
}

fn check_partial_analysis(incomplete: &Path) {
    let dir = tempfile::tempdir().unwrap();
    let good = fixture(dir.path(), &GameResult::Win, "resign");
    for partial in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_analyze_selfplay"));
        command.arg(&good).arg(incomplete).arg("--json");
        if partial {
            command.arg("--allow-partial");
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.success(), partial);
        let parsed: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(parsed["files"], 1);
        assert_eq!(parsed["extra"]["invalid"], true);
        assert_eq!(parsed["extra"]["input_issues"].as_array().unwrap().len(), 1);
        assert_eq!(parsed["extra"]["completed_games"], 1);
        assert_eq!(parsed["extra"]["draws"], 0);
        assert_eq!(parsed["progress"]["done"], 1);
        let output = command.arg("--sprt").output().unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("CSA単局ログは独立した対局ペアではない")
        );
    }
}

#[test]
fn kif_missing_or_unknown_result_is_not_a_draw() {
    for missing in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = fixture(dir.path(), &GameResult::Draw, "sennichite");
        let rows: Vec<String> = fs::read_to_string(&path)
            .unwrap()
            .lines()
            .filter_map(|line| {
                let mut row: Value = serde_json::from_str(line).unwrap();
                if row["type"] == "result" {
                    if missing {
                        return None;
                    }
                    row["outcome"] = "unknown_future_result".into();
                }
                Some(row.to_string())
            })
            .collect();
        fs::write(&path, rows.join("\n") + "\n").unwrap();
        let output = dir.path().join("game.kif");
        tools::kif::convert_jsonl_to_kif(&path, &output, &tools::kif::GameFilter::default())
            .unwrap();
        let text = fs::read_to_string(output).unwrap();
        assert!(text.contains("まで1手で中断"));
        assert!(!text.contains("引き分け"));
    }
}
