#[cfg(unix)]
mod unix {
    use std::fmt::Write as _;
    use std::fs::Permissions;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};

    use serde_json::Value;
    use tempfile::TempDir;
    use tools::packed_sfen::PackedSfenValue;

    const BIN: &str = env!("CARGO_BIN_EXE_gensfen");

    struct RunResult {
        _dir: TempDir,
        output: Output,
        out_dir: PathBuf,
    }

    fn write_sequence_engine(path: &Path, bestmoves: &[&str], hang: bool) {
        let mut script = String::from(
            "#!/bin/sh\ngo_count=0\nwhile IFS= read -r line; do\n  case \"$line\" in\n    usi) printf 'id name gensfen-worker-test\\nusiok\\n' ;;\n    isready) printf 'readyok\\n' ;;\n",
        );
        script
            .push_str("    go*)\n      go_count=$((go_count + 1))\n      case \"$go_count\" in\n");
        for (index, bestmove) in bestmoves.iter().enumerate() {
            if *bestmove == "<timeout>" {
                writeln!(script, "        {}) : ;;", index + 1).unwrap();
                continue;
            }
            let score = if *bestmove == "win" { -300 } else { 0 };
            writeln!(
                script,
                "        {}) printf 'info score cp {}\\nbestmove {}\\n' ;;",
                index + 1,
                score,
                bestmove
            )
            .unwrap();
        }
        if hang {
            script.push_str("        *) : ;;\n");
        } else {
            script.push_str("        *) printf 'bestmove none\\n' ;;\n");
        }
        script
            .push_str("      esac\n      ;;\n    stop) : ;;\n    quit) exit 0 ;;\n  esac\ndone\n");
        std::fs::write(path, script).unwrap();
        std::fs::set_permissions(path, Permissions::from_mode(0o755)).unwrap();
    }

    fn run_gensfen(
        name: &str,
        bestmoves: &[&str],
        hang: bool,
        max_moves: u32,
        sfen: Option<&str>,
        extra_args: &[&str],
    ) -> RunResult {
        run_gensfen_config(name, bestmoves, hang, max_moves, sfen, 1, 0, extra_args)
    }

    fn run_gensfen_config(
        name: &str,
        bestmoves: &[&str],
        hang: bool,
        max_moves: u32,
        sfen: Option<&str>,
        games: u32,
        dedup_hash_size: usize,
        extra_args: &[&str],
    ) -> RunResult {
        let dir = TempDir::new().unwrap();
        let engine = dir.path().join(format!("{name}-engine.sh"));
        let out_dir = dir.path().join("out");
        write_sequence_engine(&engine, bestmoves, hang);
        let max_moves = max_moves.to_string();
        let games = games.to_string();
        let dedup_hash_size = dedup_hash_size.to_string();

        let mut command = Command::new(BIN);
        command.args([
            "--native=false",
            "--engine-path",
            engine.to_str().unwrap(),
            "--out-dir",
            out_dir.to_str().unwrap(),
            "--games",
            &games,
            "--max-moves",
            &max_moves,
            "--hash-mb",
            "1",
            "--dedup-hash-size",
            &dedup_hash_size,
            "--startpos-no-repeat=false",
            "--skip-in-check=false",
        ]);
        if let Some(sfen) = sfen {
            command.args(["--sfen", sfen]);
        }
        command.args(extra_args);
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "stdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        RunResult {
            _dir: dir,
            output,
            out_dir,
        }
    }

    fn result_json(run: &RunResult) -> Value {
        let jsonl = std::fs::read_to_string(run.out_dir.join("gensfen.jsonl")).unwrap();
        jsonl
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .find(|line| line["type"] == "result")
            .unwrap()
    }

    fn psv_records(run: &RunResult) -> Vec<PackedSfenValue> {
        let bytes = std::fs::read(run.out_dir.join("gensfen.psv")).unwrap();
        assert_eq!(bytes.len() % PackedSfenValue::SIZE, 0);
        bytes
            .as_chunks::<{ PackedSfenValue::SIZE }>()
            .0
            .iter()
            .map(|record| PackedSfenValue::from_bytes(record).unwrap())
            .collect()
    }

    fn assert_result(run: &RunResult, outcome: &str, reason: &str, adopted: bool) {
        let result = result_json(run);
        assert_eq!(result["outcome"], outcome);
        assert_eq!(result["reason"], reason);
        assert_eq!(result["adopted"], adopted);
    }

    #[test]
    fn timeout_discards_whole_game() {
        let run = run_gensfen(
            "timeout",
            &["7g7f"],
            true,
            8,
            None,
            &["--byoyomi", "1", "--timeout-margin-ms", "1"],
        );

        assert_result(&run, "black_win", "timeout", false);
        assert!(psv_records(&run).is_empty());
        let stdout = String::from_utf8_lossy(&run.output.stdout);
        assert!(stdout.contains("timeout=1"));
        assert!(stdout.contains("1 collected positions discarded at game end"));
    }

    #[test]
    fn illegal_normal_move_discards_whole_game() {
        let run = run_gensfen("illegal", &["7g7f", "garbage"], false, 8, None, &[]);

        assert_result(&run, "black_win", "illegal_move", false);
        assert!(psv_records(&run).is_empty());
        assert!(
            String::from_utf8_lossy(&run.output.stdout)
                .contains("1 collected positions discarded at game end")
        );
    }

    #[test]
    fn bestmove_none_with_legal_moves_is_no_bestmove() {
        let run = run_gensfen("none", &["7g7f", "none"], false, 8, None, &[]);

        assert_result(&run, "black_win", "no_bestmove", false);
        assert!(psv_records(&run).is_empty());
        let stdout = String::from_utf8_lossy(&run.output.stdout);
        assert!(stdout.contains("no_bestmove=1"));
        assert!(stdout.contains("1 collected positions discarded at game end"));
    }

    #[test]
    fn abnormal_games_do_not_dedup_positions_from_following_normal_games() {
        for (name, abnormal_move, extra_args) in [
            (
                "timeout-then-normal",
                "<timeout>",
                vec!["--byoyomi", "1", "--timeout-margin-ms", "1"],
            ),
            ("illegal-then-normal", "garbage", vec![]),
            ("none-then-normal", "none", vec![]),
        ] {
            let run = run_gensfen_config(
                name,
                &["7g7f", abnormal_move, "7g7f", "3c3d"],
                false,
                2,
                None,
                2,
                1024,
                &extra_args,
            );

            assert_eq!(psv_records(&run).len(), 2, "{name}");
            assert!(
                !String::from_utf8_lossy(&run.output.stderr).contains("dedup_hits="),
                "{name}: {}",
                String::from_utf8_lossy(&run.output.stderr)
            );
        }
    }

    #[test]
    fn max_moves_adopts_draw_label() {
        let run = run_gensfen("max-moves", &["7g7f"], false, 1, None, &[]);
        let records = psv_records(&run);

        assert_result(&run, "draw", "max_moves", true);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].game_result, 0);
        assert!(!String::from_utf8_lossy(&run.output.stdout).contains("Discarded abnormal games"));
    }

    #[test]
    fn normal_repetition_adopts_all_positions_as_draw() {
        let cycle = ["5i4i", "5a4a", "4i5i", "4a5a"];
        let bestmoves: Vec<&str> = cycle.into_iter().cycle().take(12).collect();
        let run = run_gensfen(
            "sennichite",
            &bestmoves,
            false,
            20,
            Some("4k4/9/9/9/9/9/9/9/4K4 b - 1"),
            &[],
        );
        let records = psv_records(&run);

        assert_result(&run, "draw", "sennichite", true);
        assert_eq!(records.len(), 12);
        assert!(records.iter().all(|record| record.game_result == 0));
    }

    #[test]
    fn long_period_repetition_adopts_all_positions_as_draw() {
        let cycle = [
            "5i4i", "5a4a", "4i3i", "4a3a", "3i3h", "3a3b", "3h3g", "3b3c", "3g4g", "3c4c", "4g5g",
            "4c5c", "5g6g", "5c6c", "6g6h", "6c6b", "6h6i", "6b6a", "6i5i", "6a5a",
        ];
        let bestmoves: Vec<&str> = cycle.into_iter().cycle().take(60).collect();
        let run = run_gensfen(
            "long-sennichite",
            &bestmoves,
            false,
            80,
            Some("4k4/9/9/9/9/9/9/9/4K4 b - 1"),
            &[],
        );
        let records = psv_records(&run);

        assert_result(&run, "draw", "sennichite", true);
        assert_eq!(records.len(), 60);
        assert!(records.iter().all(|record| record.game_result == 0));
    }

    #[test]
    fn perpetual_check_adopts_loser_and_winner_labels() {
        let cycle = ["5a4a", "5b4b", "4a5a", "4b5b"];
        let bestmoves: Vec<&str> = cycle.into_iter().cycle().take(12).collect();
        let run = run_gensfen(
            "perpetual-check",
            &bestmoves,
            false,
            20,
            Some("4k4/4R4/9/9/9/9/9/9/K8 w - 1"),
            &[],
        );
        let records = psv_records(&run);

        assert_result(&run, "white_win", "perpetual_check", true);
        assert_eq!(records.len(), 12);
        let expected: Vec<i8> = [1, -1].into_iter().cycle().take(12).collect();
        assert_eq!(records.iter().map(|record| record.game_result).collect::<Vec<_>>(), expected);
    }

    #[test]
    fn valid_declaration_win_records_terminal_position() {
        let run = run_gensfen(
            "declaration-win",
            &["win"],
            false,
            8,
            Some("KGG6/SS7/PPPPPP3/9/9/9/2pppppp1/1ss1gg1nl/4k2nl b 2R2B3p 1"),
            &[],
        );
        let records = psv_records(&run);

        assert_result(&run, "black_win", "win", true);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].score, 10000);
        assert_eq!(records[0].move16, 0);
        assert_eq!(records[0].game_result, 1);
    }

    #[test]
    fn declaration_win_worker_path_uses_shared_dedup() {
        let run = run_gensfen_config(
            "declaration-win-dedup",
            &["win", "win"],
            false,
            8,
            Some("KGG6/SS7/PPPPPP3/9/9/9/2pppppp1/1ss1gg1nl/4k2nl b 2R2B3p 1"),
            2,
            8,
            &[],
        );

        assert_eq!(psv_records(&run).len(), 1);
        assert!(String::from_utf8_lossy(&run.output.stderr).contains("dedup_hits=1"));
        assert!(
            String::from_utf8_lossy(&run.output.stdout)
                .contains("Declaration-win terminals skipped by dedup: 1 games")
        );
    }

    #[test]
    fn entering_king_rules_without_pseudo_win_are_accepted() {
        for rule in ["NoEnteringKing", "TryRule"] {
            let option = format!("EnteringKingRule={rule}");
            let run = run_gensfen(rule, &["7g7f"], false, 1, None, &["--usi-option", &option]);
            assert_result(&run, "draw", "max_moves", true);
        }
    }

    #[test]
    fn try_rule_declaration_is_played_as_the_returned_king_move() {
        let run = run_gensfen(
            "try-rule-move",
            &["6a5a"],
            false,
            1,
            Some("3K5/9/9/9/9/9/9/9/4k4 b 2r2b4g4s4n4l18p 1"),
            &["--usi-option", "EnteringKingRule=TryRule"],
        );
        let records = psv_records(&run);

        assert_result(&run, "black_win", "win", true);
        assert_eq!(records.len(), 1);
        assert_ne!(records[0].move16, 0);
        assert_eq!(records[0].game_result, 1);
    }

    #[test]
    fn invalid_usi_bestmove_win_discards_whole_game() {
        let run = run_gensfen("invalid-win", &["win"], false, 8, None, &[]);

        assert_result(&run, "white_win", "illegal_move", false);
        assert!(psv_records(&run).is_empty());
    }

    #[test]
    fn unexpected_win_with_non_pseudo_win_rules_is_reported_as_illegal_move() {
        for rule in ["NoEnteringKing", "TryRule"] {
            let option = format!("EnteringKingRule={rule}");
            let run = run_gensfen(
                &format!("unexpected-win-{rule}"),
                &["win"],
                false,
                8,
                None,
                &["--usi-option", &option],
            );
            assert_result(&run, "white_win", "illegal_move", false);
            assert!(psv_records(&run).is_empty());
        }
    }
}
