//! gensfen が生成した pack を pack_to_psv で展開する round-trip テスト。

#[cfg(unix)]
mod unix {
    use std::fs::Permissions;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    use rshogi_core::position::Position;
    use rshogi_core::types::Move;
    use tempfile::TempDir;
    use tools::packed_sfen::{PackedSfenValue, move_to_psv_move16, pack_position};

    const GENSFEN_BIN: &str = env!("CARGO_BIN_EXE_gensfen");
    const PACK_TO_PSV_BIN: &str = env!("CARGO_BIN_EXE_pack_to_psv");

    #[test]
    fn gensfen_pack_round_trips_through_pack_to_psv() {
        let dir = TempDir::new().unwrap();
        let engine = dir.path().join("engine.sh");
        let out_dir = dir.path().join("out");
        let psv_path = dir.path().join("roundtrip.psv");
        std::fs::write(
            &engine,
            "#!/bin/sh\n\
             go_count=0\n\
             while IFS= read -r line; do\n\
               case \"$line\" in\n\
                 usi) printf 'id name pack-roundtrip-test\\nusiok\\n' ;;\n\
                 isready) printf 'readyok\\n' ;;\n\
                 go*) go_count=$((go_count + 1)); \
                      if [ \"$go_count\" -eq 1 ]; then \
                        printf 'info score cp 123\\nbestmove 7g7f\\n'; \
                      else \
                        printf 'info score cp -45\\nbestmove 3c3d\\n'; \
                      fi ;;\n\
                 quit) exit 0 ;;\n\
               esac\n\
             done\n",
        )
        .unwrap();
        std::fs::set_permissions(&engine, Permissions::from_mode(0o755)).unwrap();

        let gensfen = Command::new(GENSFEN_BIN)
            .args([
                "--native=false",
                "--engine-path",
                engine.to_str().unwrap(),
                "--out-dir",
                out_dir.to_str().unwrap(),
                "--training-data-format",
                "pack",
                "--games",
                "1",
                "--max-moves",
                "2",
                "--hash-mb",
                "1",
                "--dedup-hash-size",
                "0",
                "--startpos-no-repeat=false",
                "--skip-in-check=false",
            ])
            .output()
            .unwrap();
        assert!(
            gensfen.status.success(),
            "stdout={}\nstderr={}",
            String::from_utf8_lossy(&gensfen.stdout),
            String::from_utf8_lossy(&gensfen.stderr)
        );

        let converted = Command::new(PACK_TO_PSV_BIN)
            .args([
                "--input",
                out_dir.join("gensfen.pack").to_str().unwrap(),
                "--output",
                psv_path.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(
            converted.status.success(),
            "stdout={}\nstderr={}",
            String::from_utf8_lossy(&converted.stdout),
            String::from_utf8_lossy(&converted.stderr)
        );
        assert!(
            !String::from_utf8_lossy(&converted.stdout).contains("Move errors:"),
            "pack replay must have zero move errors"
        );

        let bytes = std::fs::read(psv_path).unwrap();
        let records: Vec<_> = bytes
            .as_chunks::<{ PackedSfenValue::SIZE }>()
            .0
            .iter()
            .map(|record| PackedSfenValue::from_bytes(record).unwrap())
            .collect();
        assert_eq!(records.len(), 2);

        let mut pos = Position::new();
        pos.set_hirate();
        let first = Move::from_usi("7g7f").unwrap();
        assert_eq!(records[0].sfen, pack_position(&pos));
        assert_eq!(records[0].score, 123);
        assert_eq!(records[0].move16, move_to_psv_move16(first));
        assert_eq!(records[0].game_ply, 1);
        assert_eq!(records[0].game_result, 0);

        let gives_check = pos.gives_check(first);
        pos.do_move(first, gives_check);
        let second = Move::from_usi("3c3d").unwrap();
        assert_eq!(records[1].sfen, pack_position(&pos));
        assert_eq!(records[1].score, -45);
        assert_eq!(records[1].move16, move_to_psv_move16(second));
        assert_eq!(records[1].game_ply, 2);
        assert_eq!(records[1].game_result, 0);
    }

    #[test]
    fn gensfen_pack_moves_match_played_moves_sidecar_with_multipv_randomization() {
        let dir = TempDir::new().unwrap();
        let engine = dir.path().join("engine.sh");
        let out_dir = dir.path().join("out");
        let psv_path = dir.path().join("multipv-roundtrip.psv");
        std::fs::write(
            &engine,
            "#!/bin/sh\n\
             go_count=0\n\
             while IFS= read -r line; do\n\
               case \"$line\" in\n\
                 usi) printf 'id name pack-multipv-roundtrip-test\\noption name MultiPV type spin default 1 min 1 max 3\\nusiok\\n' ;;\n\
                 isready) printf 'readyok\\n' ;;\n\
                 go*) go_count=$((go_count + 1)); \
                      if [ \"$go_count\" -eq 1 ]; then \
                        printf 'info multipv 1 score cp 123 pv 7g7f\\ninfo multipv 2 score cp 122 pv 2g2f\\ninfo multipv 3 score cp 121 pv 6g6f\\nbestmove 7g7f\\n'; \
                      else \
                        printf 'info multipv 1 score cp -45 pv 3c3d\\ninfo multipv 2 score cp -46 pv 8c8d\\ninfo multipv 3 score cp -47 pv 4c4d\\nbestmove 3c3d\\n'; \
                      fi ;;\n\
                 quit) exit 0 ;;\n\
               esac\n\
             done\n",
        )
        .unwrap();
        std::fs::set_permissions(&engine, Permissions::from_mode(0o755)).unwrap();

        let gensfen = Command::new(GENSFEN_BIN)
            .args([
                "--native=false",
                "--engine-path",
                engine.to_str().unwrap(),
                "--out-dir",
                out_dir.to_str().unwrap(),
                "--training-data-format",
                "pack",
                "--games",
                "1",
                "--max-moves",
                "2",
                "--hash-mb",
                "1",
                "--dedup-hash-size",
                "0",
                "--startpos-no-repeat=false",
                "--skip-in-check=false",
                "--random-multi-pv",
                "3",
                "--random-multi-pv-diff",
                "1000",
                "--shuffle-seed",
                "1",
                "--emit-eval-file",
            ])
            .output()
            .unwrap();
        assert!(
            gensfen.status.success(),
            "stdout={}\nstderr={}",
            String::from_utf8_lossy(&gensfen.stdout),
            String::from_utf8_lossy(&gensfen.stderr)
        );

        let converted = Command::new(PACK_TO_PSV_BIN)
            .args([
                "--input",
                out_dir.join("gensfen.pack").to_str().unwrap(),
                "--output",
                psv_path.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(
            converted.status.success(),
            "stdout={}\nstderr={}",
            String::from_utf8_lossy(&converted.stdout),
            String::from_utf8_lossy(&converted.stderr)
        );
        assert!(
            !String::from_utf8_lossy(&converted.stdout).contains("Move errors:"),
            "pack replay must have zero move errors"
        );

        let eval = std::fs::read_to_string(out_dir.join("gensfen.eval.txt")).unwrap();
        let game_line = eval.lines().find(|line| line.starts_with("game 1: ")).unwrap();
        let (_, moves) = game_line
            .split_once(" moves ")
            .expect("eval sidecar must contain the played move sequence");
        let played_moves: Vec<_> =
            moves.split_whitespace().map(|usi| Move::from_usi(usi).unwrap()).collect();

        let bytes = std::fs::read(psv_path).unwrap();
        let records: Vec<_> = bytes
            .as_chunks::<{ PackedSfenValue::SIZE }>()
            .0
            .iter()
            .map(|record| PackedSfenValue::from_bytes(record).unwrap())
            .collect();
        assert_eq!(records.len(), played_moves.len());
        for (record, played_move) in records.iter().zip(&played_moves) {
            assert_eq!(record.move16, move_to_psv_move16(*played_move));
        }

        // result JSONL の diversions は全手列ではなく、PV1 以外を選んだ手だけを記録する。
        // 固定 seed で発火を保証し、chosen_move が sidecar の同じ ply と一致することも確認する。
        let jsonl = std::fs::read_to_string(out_dir.join("gensfen.jsonl")).unwrap();
        let result: serde_json::Value = jsonl
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .find(|value: &serde_json::Value| value["type"] == "result")
            .unwrap();
        let diversions = result["diversions"].as_array().unwrap();
        assert!(!diversions.is_empty(), "fixed seed must exercise a MultiPV diversion");
        for diversion in diversions {
            let ply = diversion["ply"].as_u64().unwrap() as usize;
            assert_eq!(diversion["chosen_move"].as_str().unwrap(), played_moves[ply - 1].to_usi());
        }
    }
}
