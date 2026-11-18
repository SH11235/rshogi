#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::process::Command;

use serde_json::Value;

#[test]
fn driver_adjudicates_repetition_and_only_exact_scores() {
    for (score, flag, rule, reason, plies) in [
        // 後手の6回目の優勢評価と千日手が同時に成立すると、千日手が優先される。
        ("white-winning", "--adjudicate-win", "movecount=6,score=600", "sennichite", 12),
        ("cp 600", "--adjudicate-win", "movecount=2,score=600", "adjudication_win", 3),
        // 先手 -600・後手 +600 が両側 2 手ずつ揃う 4 手目で後手勝ち。
        (
            "agree",
            "--adjudicate-agreement",
            "movecount=2,score=600",
            "adjudication_agreement",
            4,
        ),
        ("agree", "--adjudicate-agreement", "movecount=2,score=601", "sennichite", 12),
        ("mate 1", "--adjudicate-win", "movecount=2,score=600", "adjudication_win", 3),
        (
            "cp 600 lowerbound",
            "--adjudicate-win",
            "movecount=2,score=600",
            "sennichite",
            12,
        ),
        (
            "mate 1 upperbound",
            "--adjudicate-win",
            "movecount=2,score=600",
            "sennichite",
            12,
        ),
        ("cp 600", "--adjudicate-win", "movecount=6,score=600", "adjudication_win", 11),
        ("cp 600", "--adjudicate-win", "movecount=7,score=600", "sennichite", 12),
        (
            "cp 0 lowerbound",
            "--adjudicate-draw",
            "movenumber=0,movecount=2,score=20",
            "sennichite",
            12,
        ),
        (
            "cp 0 upperbound",
            "--adjudicate-draw",
            "movenumber=0,movecount=2,score=20",
            "sennichite",
            12,
        ),
        (
            "cp -700 lowerbound",
            "--adjudicate-resign",
            "movecount=2,score=600",
            "sennichite",
            12,
        ),
        (
            "cp -700 upperbound",
            "--adjudicate-resign",
            "movecount=2,score=600",
            "sennichite",
            12,
        ),
        (
            "cp 0",
            "--adjudicate-draw",
            "movenumber=0,movecount=2,score=20",
            "adjudication_draw",
            2,
        ),
        (
            "cp -700",
            "--adjudicate-resign",
            "movecount=2,score=600",
            "adjudication_resign",
            3,
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let engine = dir.path().join("engine.sh");
        // 平手から金を往復。moves の数から手を選び、色交換後も同じ手順を返す。
        fs::write(
            &engine,
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    usi) printf 'id name repetition\nusiok\n' ;;
    isready) printf 'readyok\n' ;;
    position*)
      ply=1
      in_moves=0
      for token in $line; do
        if [ "$in_moves" = 1 ]; then ply=$((ply + 1)); fi
        if [ "$token" = moves ]; then in_moves=1; fi
      done
      ;;
    go*)
      case $((ply % 4)) in
        1) move=3i4h ;; 2) move=7a6b ;; 3) move=4h3i ;; 0) move=6b7a ;;
      esac
      score=$TEST_SCORE
      if [ "$score" = white-winning ]; then
        if [ $((ply % 2)) = 0 ]; then score='cp 600'; else score='cp 0'; fi
      fi
      if [ "$score" = agree ]; then
        if [ $((ply % 2)) = 0 ]; then score='cp 600'; else score='cp -600'; fi
      fi
      printf 'info score %s pv %s\nbestmove %s\n' "$score" "$move" "$move"
      ;;
    quit) break ;;
  esac
done
"#,
        )
        .unwrap();
        fs::set_permissions(&engine, fs::Permissions::from_mode(0o755)).unwrap();
        let out = dir.path().join("out");
        let output = Command::new(env!("CARGO_BIN_EXE_tournament"))
            .env("TEST_SCORE", score)
            .args([
                "--engine",
                engine.to_str().unwrap(),
                "--engine-label",
                "a",
                "--engine",
                engine.to_str().unwrap(),
                "--engine-label",
                "b",
                "--games",
                "1",
                "--nodes",
                "1",
                "--max-moves",
                "16",
                "--out-dir",
                out.to_str().unwrap(),
                flag,
                rule,
            ])
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let jsonl = fs::read_to_string(out.join("pair-0-1__a-vs-b.jsonl")).unwrap();
        let rows: Vec<Value> = jsonl.lines().map(|s| serde_json::from_str(s).unwrap()).collect();
        let results: Vec<_> = rows.iter().filter(|row| row["type"] == "result").collect();
        assert_eq!(results.len(), 2);
        for result in results {
            assert_eq!(result["reason"], reason, "{score}: {result}");
            assert_eq!(result["plies"], plies);
            assert_eq!(
                result["outcome"],
                if reason == "adjudication_resign" || reason == "adjudication_agreement" {
                    "white_win"
                } else if reason == "adjudication_win" {
                    "black_win"
                } else {
                    "draw"
                }
            );
        }
        assert_eq!(rows.iter().filter(|row| row["type"] == "move").count(), plies as usize * 2);
    }
}

#[test]
fn single_game_keeps_engine_order_and_can_preserve_start_history() {
    let dir = tempfile::tempdir().unwrap();
    let engine = dir.path().join("engine.sh");
    fs::write(
        &engine,
        r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    usi) printf 'id name repetition\nusiok\n' ;;
    isready) printf 'readyok\n' ;;
    position*)
      set -- $line
      ply=$6
      if [ "$ply" -ne "$BASE_PLY" ]; then exit 1; fi
      if [ "$BASE_PLY" = 1 ] && [ "$8" != 3i4h ]; then exit 1; fi
      if [ "$#" -gt 6 ]; then ply=$((ply + $# - 7)); fi
      ;;
    'go nodes 1')
      case $((ply % 4)) in
        1) move=3i4h ;; 2) move=7a6b ;; 3) move=4h3i ;; 0) move=6b7a ;;
      esac
      printf 'bestmove %s\n' "$move"
      ;;
    quit) break ;;
  esac
done
"#,
    )
    .unwrap();
    fs::set_permissions(&engine, fs::Permissions::from_mode(0o755)).unwrap();
    let start = dir.path().join("start.txt");
    fs::write(&start, "startpos moves 3i4hignored 7a6b 4h3i 6b7a 3i4h 7a6b 4h3i\n").unwrap();

    for (name, preserve, max_moves, reason, plies) in [
        ("history", true, "12", "sennichite", 5),
        ("default", false, "12", "max_moves", 5),
        ("cap", true, "11", "max_moves", 4),
    ] {
        let out = dir.path().join(name);
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_tournament"));
        cmd.env("BASE_PLY", if preserve { "1" } else { "8" }).args([
            "--engine",
            engine.to_str().unwrap(),
            "--engine-label",
            "black",
            "--engine",
            engine.to_str().unwrap(),
            "--engine-label",
            "white",
            "--single-game",
            "--nodes",
            "1",
            "--max-moves",
            max_moves,
            "--startpos-file",
            start.to_str().unwrap(),
            "--out-dir",
            out.to_str().unwrap(),
        ]);
        if preserve {
            cmd.arg("--preserve-start-history");
        }
        let output = cmd.output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let path = out.join("pair-0-1__black-vs-white.jsonl");
        let jsonl = fs::read_to_string(&path).unwrap();
        let rows: Vec<Value> = jsonl.lines().map(|s| serde_json::from_str(s).unwrap()).collect();
        assert_eq!(rows.len(), plies + 2);
        assert_eq!(rows[0]["settings"]["games"], 1);
        assert_eq!(rows[0]["settings"]["single_game"], true);
        assert_eq!(
            rows[0]["settings"]["preserve_start_history"].as_bool(),
            preserve.then_some(true)
        );
        assert_eq!(rows[1]["move_usi"], "6b7a");
        assert_eq!(rows[1]["engine"], "white");
        assert_eq!(rows[2]["engine"], "black");

        let result = rows.last().unwrap();
        assert_eq!(result["type"], "result");
        assert_eq!(result["reason"], reason);
        assert_eq!(result["outcome"], "draw");
        assert_eq!(result["plies"], plies);
        assert!(result.get("pair_slot").is_none() && result.get("pair_index").is_none());
        let meta: Value =
            serde_json::from_slice(&fs::read(out.join("meta.json")).unwrap()).unwrap();
        assert_eq!(meta["run_status"], "completed");
        assert_eq!(meta["invalid"], false);
        assert_eq!(meta["incomplete_pairs"], 0);

        let analysis = Command::new(env!("CARGO_BIN_EXE_analyze_selfplay"))
            .arg(&path)
            .output()
            .unwrap();
        assert!(analysis.status.success(), "{}", String::from_utf8_lossy(&analysis.stderr));
        let sprt = Command::new(env!("CARGO_BIN_EXE_analyze_selfplay"))
            .arg("--sprt")
            .arg(path)
            .output()
            .unwrap();
        assert!(!sprt.status.success());
        assert!(String::from_utf8_lossy(&sprt.stderr).contains("単局ログ"));
    }
}
