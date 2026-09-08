#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::process::Command;

use serde_json::Value;

#[test]
fn driver_adjudicates_repetition_and_only_exact_scores() {
    for (score, flag, rule, reason, plies) in [
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
        // 平手から金を往復。position の手数から手を選び、色交換後も同じ手順を返す。
        fs::write(
            &engine,
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    usi) printf 'id name repetition\nusiok\n' ;;
    isready) printf 'readyok\n' ;;
    position*) for token in $line; do ply=$token; done ;;
    go*)
      case $((ply % 4)) in
        1) move=3i4h ;; 2) move=7a6b ;; 3) move=4h3i ;; 0) move=6b7a ;;
      esac
      printf 'info score %s pv %s\nbestmove %s\n' "$TEST_SCORE" "$move" "$move"
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
        let jsonl = fs::read_to_string(out.join("a-vs-b.jsonl")).unwrap();
        let rows: Vec<Value> = jsonl.lines().map(|s| serde_json::from_str(s).unwrap()).collect();
        let results: Vec<_> = rows.iter().filter(|row| row["type"] == "result").collect();
        assert_eq!(results.len(), 2);
        for result in results {
            assert_eq!(result["reason"], reason, "{score}: {result}");
            assert_eq!(result["plies"], plies);
            assert_eq!(
                result["outcome"],
                if reason == "adjudication_resign" {
                    "white_win"
                } else {
                    "draw"
                }
            );
        }
        assert_eq!(rows.iter().filter(|row| row["type"] == "move").count(), plies as usize * 2);
    }
}
