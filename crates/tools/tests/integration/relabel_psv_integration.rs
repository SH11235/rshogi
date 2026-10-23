use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use std::process::Command;

use rshogi_core::position::Position;
use serde_json::{Value, json};
use tempfile::TempDir;
use tools::packed_sfen::{PackedSfenValue, pack_position};

const BIN: &str = env!("CARGO_BIN_EXE_relabel_psv");
const GENSFEN_BIN: &str = env!("CARGO_BIN_EXE_gensfen");

fn record(pos: &Position, score: i16, game_ply: u16, game_result: i8) -> PackedSfenValue {
    PackedSfenValue {
        sfen: pack_position(pos),
        score,
        move16: 0,
        game_ply,
        game_result,
        padding: 0,
    }
}

fn write_psv(path: &Path, records: &[PackedSfenValue]) {
    let mut file = File::create(path).unwrap();
    for record in records {
        file.write_all(&record.to_bytes()).unwrap();
    }
}

fn read_psv(path: &Path) -> Vec<PackedSfenValue> {
    std::fs::read(path)
        .unwrap()
        .as_chunks::<{ PackedSfenValue::SIZE }>()
        .0
        .iter()
        .map(|bytes| PackedSfenValue::from_bytes(bytes).unwrap())
        .collect()
}

fn write_game_ids(path: &Path, game_ids: &[u32]) {
    let mut file = File::create(path).unwrap();
    for game_id in game_ids {
        file.write_all(&game_id.to_le_bytes()).unwrap();
    }
}

fn read_stats(stderr: &[u8]) -> Value {
    let line = String::from_utf8_lossy(stderr).lines().last().unwrap().to_owned();
    serde_json::from_str(&line)
        .unwrap_or_else(|error| panic!("invalid stats JSON: {error}: {line}"))
}

fn result_line(game_id: u32, start_ply: u16, reason: &str, diversions: Value) -> Value {
    json!({
        "type": "result",
        "game_id": game_id,
        "start_sfen": format!(
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - {start_ply}"
        ),
        "reason": reason,
        "diversions": diversions,
    })
}

fn write_jsonl(path: &Path, values: &[Value]) {
    let mut file = File::create(path).unwrap();
    for value in values {
        serde_json::to_writer(&mut file, value).unwrap();
        file.write_all(b"\n").unwrap();
    }
}

fn contaminated_command(
    input: &Path,
    output: Option<&Path>,
    sidecar: &Path,
    diversions: &Path,
) -> Command {
    let mut command = Command::new(BIN);
    command.args([
        "--input",
        input.to_str().unwrap(),
        "--deblunder",
        "--deblunder-mode",
        "drop-contaminated",
        "--game-id-sidecar",
        sidecar.to_str().unwrap(),
        "--diversions",
        diversions.to_str().unwrap(),
    ]);
    if let Some(output) = output {
        command.args(["--output", output.to_str().unwrap()]);
    }
    command
}

fn hirate() -> Position {
    let mut pos = Position::new();
    pos.set_hirate();
    pos
}

fn assert_rejected_without_modifying(command: &mut Command, protected: &Path) {
    let original = std::fs::read(protected).unwrap();
    let result = command.output().unwrap();
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("resolves to the same file"),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(std::fs::read(protected).unwrap(), original);
}

#[test]
fn output_with_equivalent_relative_spelling_is_rejected() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("x.psv");
    let pos = hirate();
    write_psv(&input, &[record(&pos, 10, 1, 1)]);

    assert_rejected_without_modifying(
        Command::new(BIN)
            .current_dir(dir.path())
            .args(["--input", "./x.psv", "--output", "x.psv"]),
        &input,
    );
}

#[cfg(unix)]
#[test]
fn output_through_symlink_is_rejected() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new().unwrap();
    let input = dir.path().join("input.psv");
    let output = dir.path().join("output.psv");
    let pos = hirate();
    write_psv(&input, &[record(&pos, 10, 1, 1)]);
    symlink(&input, &output).unwrap();

    assert_rejected_without_modifying(
        Command::new(BIN).args([
            "--input",
            input.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
        ]),
        &input,
    );
}

#[cfg(unix)]
#[test]
fn output_through_hardlink_is_rejected() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("input.psv");
    let output = dir.path().join("output.psv");
    let pos = hirate();
    write_psv(&input, &[record(&pos, 10, 1, 1)]);
    std::fs::hard_link(&input, &output).unwrap();

    assert_rejected_without_modifying(
        Command::new(BIN).args([
            "--input",
            input.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
        ]),
        &input,
    );
}

#[test]
fn output_distinct_from_input_is_accepted() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("input.psv");
    let output = dir.path().join("nested/output.psv");
    let pos = hirate();
    write_psv(&input, &[record(&pos, 10, 1, 1)]);

    let result = Command::new(BIN)
        .args([
            "--input",
            input.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert_eq!(read_psv(&output)[0].score, 2500);
}

#[test]
fn output_same_as_game_id_sidecar_is_rejected() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("input.psv");
    let sidecar = dir.path().join("game_ids.bin");
    let diversions = dir.path().join("games.jsonl");
    let pos = hirate();
    write_psv(&input, &[record(&pos, 10, 1, 1)]);
    std::fs::write(&sidecar, 42u32.to_le_bytes()).unwrap();
    std::fs::write(&diversions, "{\"type\":\"meta\"}\n").unwrap();

    assert_rejected_without_modifying(
        Command::new(BIN).args([
            "--input",
            input.to_str().unwrap(),
            "--output",
            sidecar.to_str().unwrap(),
            "--deblunder",
            "--game-id-sidecar",
            sidecar.to_str().unwrap(),
            "--diversions",
            diversions.to_str().unwrap(),
        ]),
        &sidecar,
    );
}

#[test]
fn output_same_as_diversions_is_rejected() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("input.psv");
    let sidecar = dir.path().join("game_ids.bin");
    let diversions = dir.path().join("games.jsonl");
    let pos = hirate();
    write_psv(&input, &[record(&pos, 10, 1, 1)]);
    write_game_ids(&sidecar, &[42]);
    write_jsonl(&diversions, &[result_line(42, 1, "resign", json!([]))]);

    assert_rejected_without_modifying(
        &mut contaminated_command(&input, Some(&diversions), &sidecar, &diversions),
        &diversions,
    );
}

#[test]
fn verdict_same_as_diversions_is_rejected() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("input.psv");
    let output = dir.path().join("output.psv");
    let sidecar = dir.path().join("game_ids.bin");
    let diversions = dir.path().join("games.jsonl");
    let pos = hirate();
    write_psv(&input, &[record(&pos, 10, 1, 1)]);
    write_game_ids(&sidecar, &[42]);
    write_jsonl(&diversions, &[result_line(42, 1, "resign", json!([]))]);

    assert_rejected_without_modifying(
        contaminated_command(&input, Some(&output), &sidecar, &diversions)
            .args(["--emit-verdict-sidecar", diversions.to_str().unwrap()]),
        &diversions,
    );
    assert!(!output.exists());
}

#[test]
fn score_is_replaced_from_side_to_move_game_result_and_is_deterministic() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("input.psv");
    let output_a = dir.path().join("output-a.psv");
    let output_b = dir.path().join("output-b.psv");
    let pos = hirate();
    write_psv(
        &input,
        &[
            record(&pos, -12, 1, 1),
            record(&pos, 34, 2, -1),
            record(&pos, 56, 3, 0),
        ],
    );

    for output in [&output_a, &output_b] {
        let result = Command::new(BIN)
            .args([
                "--input",
                input.to_str().unwrap(),
                "--output",
                output.to_str().unwrap(),
                "--win-cp",
                "1234",
            ])
            .output()
            .unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
        let stats = read_stats(&result.stderr);
        assert_eq!(stats["input_positions"], 3);
        assert_eq!(stats["wins"], 1);
        assert_eq!(stats["losses"], 1);
        assert_eq!(stats["draws"], 1);
    }

    let scores: Vec<i16> = read_psv(&output_a).iter().map(|record| record.score).collect();
    assert_eq!(scores, [1234, -1234, 0]);
    assert_eq!(std::fs::read(output_a).unwrap(), std::fs::read(output_b).unwrap());
}

#[test]
fn declaration_override_uses_side_to_move_declaration_win() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("input.psv");
    let output = dir.path().join("output.psv");
    let mut pos = Position::new();
    pos.set_sfen("KGG6/SS7/PPPPPP3/9/9/9/2pppppp1/1ss1gg1nl/4k2nl b 2R2B3p 1")
        .unwrap();
    write_psv(&input, &[record(&pos, -100, 1, -1)]);

    let result = Command::new(BIN)
        .args([
            "--input",
            input.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--win-cp",
            "2222",
            "--declaration-override",
        ])
        .output()
        .unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert_eq!(read_psv(&output)[0].score, 2222);
    assert_eq!(read_stats(&result.stderr)["declaration_overrides"], 1);
}

#[test]
fn explicit_deblunder_mode_requires_deblunder() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("input.psv");
    let pos = hirate();
    write_psv(&input, &[record(&pos, 10, 1, 1)]);

    for mode in ["drop-before-last", "drop-before-any", "drop-contaminated"] {
        let result = Command::new(BIN)
            .args([
                "--input",
                input.to_str().unwrap(),
                "--dry-run",
                "--deblunder-mode",
                mode,
            ])
            .output()
            .unwrap();
        assert!(!result.status.success());
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(stderr.contains("--deblunder-mode requires --deblunder"), "{stderr}");
    }
}

#[test]
fn deblunder_modes_drop_before_the_selected_diversion_boundary() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("input.psv");
    let sidecar = dir.path().join("game_ids.bin");
    let diversions = dir.path().join("games.jsonl");
    let pos = hirate();
    let records: Vec<_> = (2..=6).map(|ply| record(&pos, 10, ply, 1)).collect();
    write_psv(&input, &records);
    let mut ids = File::create(&sidecar).unwrap();
    for _ in &records {
        ids.write_all(&42u32.to_le_bytes()).unwrap();
    }
    std::fs::write(
        &diversions,
        concat!(
            "{\"type\":\"meta\"}\n",
            "{\"type\":\"result\",\"game_id\":42,",
            "\"start_sfen\":\"lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1\",",
            "\"diversions\":[{\"ply\":5},{\"ply\":3}]}\n"
        ),
    )
    .unwrap();

    for (mode, expected_plies, expected_drops) in [
        ("drop-before-last", vec![6], 4),
        ("drop-before-any", vec![4, 5, 6], 2),
    ] {
        let output = dir.path().join(format!("{mode}.psv"));
        let result = Command::new(BIN)
            .args([
                "--input",
                input.to_str().unwrap(),
                "--output",
                output.to_str().unwrap(),
                "--deblunder",
                "--game-id-sidecar",
                sidecar.to_str().unwrap(),
                "--diversions",
                diversions.to_str().unwrap(),
                "--deblunder-mode",
                mode,
            ])
            .output()
            .unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
        let plies: Vec<u16> = read_psv(&output).iter().map(|record| record.game_ply).collect();
        assert_eq!(plies, expected_plies);
        assert_eq!(read_stats(&result.stderr)["deblunder_dropped_positions"], expected_drops);
    }
}

#[test]
fn legacy_deblunder_writes_dropped_legacy_verdict() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("input.psv");
    let output = dir.path().join("output.psv");
    let verdict = dir.path().join("verdict.bin");
    let sidecar = dir.path().join("ids.bin");
    let diversions = dir.path().join("games.jsonl");
    let pos = hirate();
    write_psv(&input, &[record(&pos, 10, 1, 1), record(&pos, 10, 2, 1)]);
    write_game_ids(&sidecar, &[42, 42]);
    write_jsonl(&diversions, &[result_line(42, 1, "resign", json!([{"ply": 1}]))]);

    let result = Command::new(BIN)
        .args([
            "--input",
            input.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--deblunder",
            "--game-id-sidecar",
            sidecar.to_str().unwrap(),
            "--diversions",
            diversions.to_str().unwrap(),
            "--emit-verdict-sidecar",
            verdict.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert_eq!(std::fs::read(verdict).unwrap(), [5, 0]);
}

#[test]
fn deblunder_converts_relative_diversion_ply_from_midgame_start_sfen() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("input.psv");
    let output = dir.path().join("output.psv");
    let sidecar = dir.path().join("game_ids.bin");
    let diversions = dir.path().join("games.jsonl");
    let pos = hirate();
    let records: Vec<_> = (100..=103).map(|ply| record(&pos, 10, ply, 1)).collect();
    write_psv(&input, &records);
    let mut ids = File::create(&sidecar).unwrap();
    for _ in &records {
        ids.write_all(&7u32.to_le_bytes()).unwrap();
    }
    std::fs::write(
        &diversions,
        concat!(
            "{\"type\":\"result\",\"game_id\":7,",
            "\"start_sfen\":\"lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 100\",",
            "\"diversions\":[{\"ply\":2}]}\n"
        ),
    )
    .unwrap();

    let result = Command::new(BIN)
        .args([
            "--input",
            input.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--deblunder",
            "--game-id-sidecar",
            sidecar.to_str().unwrap(),
            "--diversions",
            diversions.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    let plies: Vec<u16> = read_psv(&output).iter().map(|record| record.game_ply).collect();
    assert_eq!(plies, [102, 103]);
    assert_eq!(read_stats(&result.stderr)["deblunder_dropped_positions"], 2);
}

#[test]
fn buffered_record_error_reports_exact_input_path_and_record() {
    let dir = TempDir::new().unwrap();
    let input_a = dir.path().join("a.psv");
    let input_b = dir.path().join("b.psv");
    let output = dir.path().join("output.psv");
    let sidecar = dir.path().join("ids.bin");
    let diversions = dir.path().join("games.jsonl");
    let pos = hirate();
    write_psv(&input_a, &[record(&pos, 10, 1, 1)]);
    write_psv(&input_b, &[record(&pos, 10, 1, 2)]);
    write_game_ids(&sidecar, &[1, 2]);
    write_jsonl(
        &diversions,
        &[
            result_line(1, 1, "resign", json!([])),
            result_line(2, 1, "resign", json!([])),
        ],
    );
    let inputs = format!("{},{}", input_a.display(), input_b.display());

    let result = contaminated_command(Path::new(&inputs), Some(&output), &sidecar, &diversions)
        .output()
        .unwrap();
    assert!(!result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("invalid game_result 2 at record 1"), "{stderr}");
    assert!(stderr.contains(&input_b.display().to_string()), "{stderr}");
}

#[test]
fn missing_diversion_record_falls_back_to_gap_only() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("input.psv");
    let output = dir.path().join("output.psv");
    let sidecar = dir.path().join("ids.bin");
    let diversions = dir.path().join("games.jsonl");
    let pos = hirate();
    let records = [record(&pos, 500, 10, 1), record(&pos, 500, 20, 1)];
    write_psv(&input, &records);
    write_game_ids(&sidecar, &[1, 2]);
    write_jsonl(
        &diversions,
        &[
            result_line(
                1,
                10,
                "resign",
                json!([{"ply": 2, "kind": "multipv", "score_gap_cp": 150}]),
            ),
            result_line(
                2,
                20,
                "resign",
                json!([{"ply": 2, "kind": "multipv", "score_gap_cp": 50}]),
            ),
        ],
    );

    let result = contaminated_command(&input, Some(&output), &sidecar, &diversions)
        .output()
        .unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert_eq!(read_psv(&output).iter().map(|r| r.game_ply).collect::<Vec<_>>(), [20]);
    let stats = read_stats(&result.stderr);
    assert_eq!(stats["decisions"]["missing_record_contaminated"], 1);
    assert_eq!(stats["decisions"]["missing_record_preserved"], 1);
    assert_eq!(
        stats["gap_histogram"]["boundaries_cp"],
        json!([0, 50, 100, 200, 300, 500, 1000, 3000])
    );
    assert_eq!(stats["gap_histogram"]["counts"], json!([0, 0, 1, 1, 0, 0, 0, 0, 0]));
}

#[test]
fn draw_uses_gap_only_even_when_original_score_is_decisive() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("input.psv");
    let output = dir.path().join("output.psv");
    let sidecar = dir.path().join("ids.bin");
    let diversions = dir.path().join("games.jsonl");
    let pos = hirate();
    write_psv(&input, &[record(&pos, 800, 30, 0), record(&pos, 800, 40, 0)]);
    write_game_ids(&sidecar, &[3, 11]);
    write_jsonl(
        &diversions,
        &[
            result_line(
                3,
                30,
                "max_moves",
                json!([{"ply": 1, "kind": "multipv", "score_gap_cp": 50}]),
            ),
            result_line(
                11,
                40,
                "other_draw",
                json!([{"ply": 1, "kind": "multipv", "score_gap_cp": 150}]),
            ),
        ],
    );

    let result = contaminated_command(&input, Some(&output), &sidecar, &diversions)
        .output()
        .unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert_eq!(read_psv(&output).len(), 1);
    let stats = read_stats(&result.stderr);
    assert_eq!(stats["preserved_games"], 1);
    assert_eq!(stats["contaminated_games"], 1);
    assert_eq!(stats["decisions"]["gap_contaminated"], 1);
    assert_eq!(stats["draw_games_by_reason"]["max_moves"], 1);
    assert_eq!(stats["draw_games_by_reason"]["other"], 1);
}

#[test]
fn random_diversion_is_contaminated() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("input.psv");
    let output = dir.path().join("output.psv");
    let verdict = dir.path().join("verdict.bin");
    let sidecar = dir.path().join("ids.bin");
    let diversions = dir.path().join("games.jsonl");
    let pos = hirate();
    write_psv(&input, &[record(&pos, 0, 40, 0), record(&pos, 0, 41, 0)]);
    write_game_ids(&sidecar, &[4, 4]);
    write_jsonl(
        &diversions,
        &[result_line(
            4,
            40,
            "sennichite",
            json!([{"ply": 1, "kind": "random", "score_gap_cp": null}]),
        )],
    );

    let result = contaminated_command(&input, Some(&output), &sidecar, &diversions)
        .args(["--emit-verdict-sidecar", verdict.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert_eq!(read_psv(&output).iter().map(|r| r.game_ply).collect::<Vec<_>>(), [41]);
    assert_eq!(std::fs::read(verdict).unwrap(), [4, 0]);
    let stats = read_stats(&result.stderr);
    assert_eq!(stats["decisions"]["random_contaminated"], 1);
    assert_eq!(stats["draw_games_by_reason"]["sennichite"], 1);
}

#[test]
fn declaration_override_is_counted_when_record_is_dropped() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("input.psv");
    let output = dir.path().join("output.psv");
    let sidecar = dir.path().join("ids.bin");
    let diversions = dir.path().join("games.jsonl");
    let mut pos = Position::new();
    pos.set_sfen("KGG6/SS7/PPPPPP3/9/9/9/2pppppp1/1ss1gg1nl/4k2nl b 2R2B3p 1")
        .unwrap();
    write_psv(&input, &[record(&pos, -100, 1, -1)]);
    write_game_ids(&sidecar, &[12]);
    write_jsonl(
        &diversions,
        &[result_line(
            12,
            1,
            "resign",
            json!([{"ply": 1, "kind": "random"}]),
        )],
    );

    let result = contaminated_command(&input, Some(&output), &sidecar, &diversions)
        .arg("--declaration-override")
        .output()
        .unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert!(read_psv(&output).is_empty());
    let stats = read_stats(&result.stderr);
    assert_eq!(stats["declaration_overrides"], 1);
    assert_eq!(stats["declaration_overrides_dropped"], 1);
}

#[test]
fn multipv_without_score_gap_is_rejected() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("input.psv");
    let output = dir.path().join("output.psv");
    let sidecar = dir.path().join("ids.bin");
    let diversions = dir.path().join("games.jsonl");
    let pos = hirate();
    write_psv(&input, &[record(&pos, 0, 1, 0)]);
    write_game_ids(&sidecar, &[13]);
    write_jsonl(
        &diversions,
        &[result_line(
            13,
            1,
            "max_moves",
            json!([{"ply": 1, "kind": "multipv"}]),
        )],
    );

    let result = contaminated_command(&input, Some(&output), &sidecar, &diversions)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("multipv diversion requires score_gap_cp")
    );
}

#[test]
fn matching_decisive_score_preserves_game_even_with_large_gap() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("input.psv");
    let output = dir.path().join("output.psv");
    let sidecar = dir.path().join("ids.bin");
    let diversions = dir.path().join("games.jsonl");
    let pos = hirate();
    write_psv(&input, &[record(&pos, 500, 50, 1), record(&pos, 400, 51, 1)]);
    write_game_ids(&sidecar, &[5, 5]);
    write_jsonl(
        &diversions,
        &[result_line(
            5,
            50,
            "resign",
            json!([{"ply": 1, "kind": "multipv", "score_gap_cp": 9000}]),
        )],
    );

    let result = contaminated_command(&input, Some(&output), &sidecar, &diversions)
        .output()
        .unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert_eq!(read_psv(&output).len(), 2);
    assert_eq!(read_stats(&result.stderr)["preserved_games"], 1);
}

#[test]
fn multiple_diversions_use_maximum_contaminated_ply_in_both_orders() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("input.psv");
    let output = dir.path().join("output.psv");
    let sidecar = dir.path().join("ids.bin");
    let diversions = dir.path().join("games.jsonl");
    let pos = hirate();
    let mut records = Vec::new();
    let mut ids = Vec::new();
    for game_id in [6, 7] {
        for ply in 1..=5 {
            let score = match (game_id, ply) {
                (6, 2) | (7, 4) => 500,
                (6, 4) | (7, 2) => -500,
                _ => 0,
            };
            records.push(record(&pos, score, ply, 1));
            ids.push(game_id);
        }
    }
    write_psv(&input, &records);
    write_game_ids(&sidecar, &ids);
    let ds = json!([
        {"ply": 2, "kind": "multipv", "score_gap_cp": 0},
        {"ply": 4, "kind": "multipv", "score_gap_cp": 0}
    ]);
    write_jsonl(
        &diversions,
        &[
            result_line(6, 1, "resign", ds.clone()),
            result_line(7, 1, "resign", ds),
        ],
    );

    let result = contaminated_command(&input, Some(&output), &sidecar, &diversions)
        .output()
        .unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    let plies: Vec<u16> = read_psv(&output).iter().map(|r| r.game_ply).collect();
    assert_eq!(plies, [5, 3, 4, 5]);
}

#[test]
fn non_contiguous_game_id_is_rejected() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("input.psv");
    let output = dir.path().join("output.psv");
    let sidecar = dir.path().join("ids.bin");
    let diversions = dir.path().join("games.jsonl");
    let pos = hirate();
    write_psv(
        &input,
        &[
            record(&pos, 0, 1, 0),
            record(&pos, 0, 1, 0),
            record(&pos, 0, 2, 0),
        ],
    );
    write_game_ids(&sidecar, &[8, 9, 8]);
    write_jsonl(
        &diversions,
        &[
            result_line(8, 1, "max_moves", json!([])),
            result_line(9, 1, "max_moves", json!([])),
        ],
    );

    let result = contaminated_command(&input, Some(&output), &sidecar, &diversions)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("reappeared non-contiguously"));
}

#[test]
fn output_and_verdict_are_deterministic_and_dry_run_matches_stats() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("input.psv");
    let sidecar = dir.path().join("ids.bin");
    let diversions = dir.path().join("games.jsonl");
    let pos = hirate();
    write_psv(&input, &[record(&pos, -500, 1, 1), record(&pos, 0, 2, 1)]);
    write_game_ids(&sidecar, &[10, 10]);
    write_jsonl(
        &diversions,
        &[result_line(
            10,
            1,
            "resign",
            json!([{"ply": 1, "kind": "multipv", "score_gap_cp": 150}]),
        )],
    );

    let mut full_stats = None;
    for suffix in ["a", "b"] {
        let output = dir.path().join(format!("output-{suffix}.psv"));
        let verdict = dir.path().join(format!("verdict-{suffix}.bin"));
        let result = contaminated_command(&input, Some(&output), &sidecar, &diversions)
            .args(["--emit-verdict-sidecar", verdict.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
        full_stats = Some(read_stats(&result.stderr));
    }
    assert_eq!(
        std::fs::read(dir.path().join("output-a.psv")).unwrap(),
        std::fs::read(dir.path().join("output-b.psv")).unwrap()
    );
    assert_eq!(
        std::fs::read(dir.path().join("verdict-a.bin")).unwrap(),
        std::fs::read(dir.path().join("verdict-b.bin")).unwrap()
    );

    let dry_output = dir.path().join("must-not-exist.psv");
    let dry_verdict = dir.path().join("dry-verdict.bin");
    let result = contaminated_command(&input, None, &sidecar, &diversions)
        .args([
            "--dry-run",
            "--emit-verdict-sidecar",
            dry_verdict.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert!(!dry_output.exists());
    assert_eq!(read_stats(&result.stderr), full_stats.unwrap());
    assert_eq!(
        std::fs::read(dry_verdict).unwrap(),
        std::fs::read(dir.path().join("verdict-a.bin")).unwrap()
    );
}

fn write_sparse_zero_halfkp(path: &Path) {
    const VERSION: u32 = 0x7AF3_2F16;
    const ARCH: &[u8] = b"Features=HalfKP(Friend)[125388->256x2],Network=AffineTransform[1<-32](ClippedReLU[32](AffineTransform[32<-32](ClippedReLU[32](AffineTransform[32<-512](InputSlice[512(0:512)])))))";
    const FILE_SIZE: u64 = 64_217_066;

    let mut file = OpenOptions::new().create(true).truncate(true).write(true).open(path).unwrap();
    file.set_len(FILE_SIZE).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(&VERSION.to_le_bytes()).unwrap();
    file.write_all(&0u32.to_le_bytes()).unwrap();
    file.write_all(&(ARCH.len() as u32).to_le_bytes()).unwrap();
    file.write_all(ARCH).unwrap();
}

#[test]
fn real_native_gensfen_midgame_diversion_reads_exact_absolute_ply_score() {
    let dir = TempDir::new().unwrap();
    let out_dir = dir.path().join("gensfen");
    let eval_file = dir.path().join("zero.nnue");
    let generated_sidecar = dir.path().join("generated-ids.bin");
    write_sparse_zero_halfkp(&eval_file);

    let generated = Command::new(GENSFEN_BIN)
        .args([
            "--out-dir",
            out_dir.to_str().unwrap(),
            "--eval-file",
            eval_file.to_str().unwrap(),
            "--sfen",
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 100",
            "--games",
            "1",
            "--max-moves",
            "4",
            "--depth",
            "1",
            "--concurrency",
            "1",
            "--hash-mb",
            "1",
            "--dedup-hash-size",
            "0",
            "--startpos-no-repeat=false",
            "--emit-game-id-sidecar",
            generated_sidecar.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(generated.status.success(), "{}", String::from_utf8_lossy(&generated.stderr));

    let all_records = read_psv(&out_dir.join("gensfen.psv"));
    let all_ids: Vec<u32> = std::fs::read(&generated_sidecar)
        .unwrap()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|bytes| u32::from_le_bytes(*bytes))
        .collect();
    let game_id = *all_ids.first().expect("NativeBackend run must produce PSV records");
    let mut records: Vec<PackedSfenValue> = all_records
        .into_iter()
        .zip(all_ids)
        .filter(|(_, id)| *id == game_id)
        .map(|(record, _)| record)
        .collect();
    records.sort_by_key(|record| record.game_ply);
    assert!(records.len() >= 3, "NativeBackend run must produce at least three PSV records");
    let absolute_ply = records[1].game_ply;
    assert!(records.iter().any(|record| record.game_ply == absolute_ply - 1));
    assert!(records.iter().any(|record| record.game_ply == absolute_ply + 1));
    let relative_ply = absolute_ply - 100 + 1;
    for record in &mut records {
        record.game_result = 1;
        record.score = if record.game_ply == absolute_ply {
            500
        } else {
            -500
        };
    }

    let input = dir.path().join("isolated.psv");
    let sidecar = dir.path().join("isolated-ids.bin");
    let diversions = dir.path().join("isolated.jsonl");
    let output = dir.path().join("relabelled.psv");
    write_psv(&input, &records);
    write_game_ids(&sidecar, &vec![game_id; records.len()]);
    write_jsonl(
        &diversions,
        &[result_line(
            game_id,
            100,
            "resign",
            json!([{
                "ply": relative_ply,
                "kind": "multipv",
                "score_gap_cp": 150
            }]),
        )],
    );

    let result = contaminated_command(&input, Some(&output), &sidecar, &diversions)
        .output()
        .unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert_eq!(read_psv(&output).len(), records.len());
    assert_eq!(read_stats(&result.stderr)["preserved_games"], 1);
}
