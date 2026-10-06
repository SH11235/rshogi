use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use rshogi_core::position::Position;
use rshogi_core::types::Move;
use tempfile::TempDir;
use tools::packed_sfen::{PackedSfenValue, move_to_psv_move16, pack_position, unpack_sfen};

const JSONL: &str = env!("CARGO_BIN_EXE_psv_to_jsonl");
const MIGRATE: &str = env!("CARGO_BIN_EXE_migrate_psv_move16");
const PACK: &str = env!("CARGO_BIN_EXE_pack_to_psv");
const FILTER: &str = env!("CARGO_BIN_EXE_filter_teacher_data");
const FIX: &str = env!("CARGO_BIN_EXE_fix_scores");

#[derive(Clone, Copy, Debug)]
enum Alias {
    SamePath,
    Hardlink,
    #[cfg(unix)]
    Symlink,
}

fn aliases() -> Vec<Alias> {
    vec![
        Alias::SamePath,
        Alias::Hardlink,
        #[cfg(unix)]
        Alias::Symlink,
    ]
}

fn link_alias(alias: Alias, input: &Path, output: &Path) {
    match alias {
        Alias::SamePath => assert_eq!(input, output),
        Alias::Hardlink => fs::hard_link(input, output).unwrap(),
        #[cfg(unix)]
        Alias::Symlink => std::os::unix::fs::symlink(input, output).unwrap(),
    }
}

fn record() -> Vec<u8> {
    include_bytes!("fixtures/psv_to_hcpe3_yaneuraou_sample.psv")[..40].to_vec()
}

fn legacy_record() -> Vec<u8> {
    let mut bytes = record();
    bytes[34..36].copy_from_slice(&(37u16 | (82 << 7)).to_le_bytes());
    bytes
}

fn pack_game() -> Vec<u8> {
    // 平手から 7g7f、評価値123、引き分けの終局マーカー。
    vec![1, 0x3b, 0x1e, 123, 0, 0, 0, 0]
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_rejected(output: &Output) {
    assert!(
        !output.status.success(),
        "unexpected success: stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn alias_pair(dir: &Path, alias: Alias, bytes: &[u8]) -> (PathBuf, PathBuf) {
    let input = dir.join("input.psv");
    fs::write(&input, bytes).unwrap();
    let output = match alias {
        Alias::SamePath => input.clone(),
        _ => dir.join("output"),
    };
    link_alias(alias, &input, &output);
    (input, output)
}

#[test]
fn jsonl_rejects_input_aliases_without_changing_source() {
    let bytes = record();
    for alias in aliases() {
        let dir = TempDir::new().unwrap();
        let (input, output) = alias_pair(dir.path(), alias, &bytes);
        let result = Command::new(JSONL)
            .arg("--input")
            .arg(&input)
            .arg("--output")
            .arg(&output)
            .output()
            .unwrap();
        assert_rejected(&result);
        assert_eq!(fs::read(&input).unwrap(), bytes, "{alias:?}");
        assert_eq!(fs::read(&output).unwrap(), bytes);
    }
}

#[test]
fn jsonl_distinct_output_preserves_record_content() {
    let dir = TempDir::new().unwrap();
    let bytes = record();
    fs::write(dir.path().join("input.psv"), &bytes).unwrap();
    let result = Command::new(JSONL)
        .current_dir(dir.path())
        .args(["--input", "input.psv", "--output", "output.jsonl"])
        .output()
        .unwrap();
    assert_success(&result);
    let psv = PackedSfenValue::from_bytes(&bytes).unwrap();
    let json: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.path().join("output.jsonl")).unwrap()).unwrap();
    assert_eq!(json["sfen"], unpack_sfen(&psv.sfen).unwrap());
    assert_eq!(json["score"], 136);
    assert_eq!(json["best_move"], "1g1f");
    assert_eq!(json["depth"], 0);
    assert_eq!(json["nodes"], 0);
    assert_eq!(fs::read(dir.path().join("input.psv")).unwrap(), bytes);
}

#[test]
fn migration_rejects_final_and_staging_aliases_without_truncation() {
    let bytes = legacy_record();
    for staging in [false, true] {
        for alias in aliases() {
            let dir = TempDir::new().unwrap();
            let output = dir.path().join("output.psv");
            let partial = dir.path().join("output.psv.partial");
            let target = if staging { &partial } else { &output };
            let input = match alias {
                Alias::SamePath => target.clone(),
                _ => dir.path().join("input.psv"),
            };
            fs::write(&input, &bytes).unwrap();
            link_alias(alias, &input, target);
            let result = Command::new(MIGRATE)
                .arg("--input")
                .arg(&input)
                .arg("--output")
                .arg(&output)
                .arg("--verify-legal=false")
                .output()
                .unwrap();
            assert_rejected(&result);
            assert_eq!(fs::read(&input).unwrap(), bytes, "staging={staging}, {alias:?}");
            assert_eq!(fs::read(target).unwrap(), bytes);
            if staging {
                assert!(!output.exists());
            } else {
                assert!(!partial.exists());
            }
        }
    }
}

#[test]
fn migration_preserves_existing_partial_and_converts_to_new_output() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("input.psv");
    let output = dir.path().join("output.psv");
    let partial = dir.path().join("output.psv.partial");
    let bytes = legacy_record();
    fs::write(&input, &bytes).unwrap();
    fs::write(&partial, b"previous partial").unwrap();
    fs::write(&output, b"previous final").unwrap();
    let convert = || {
        Command::new(MIGRATE)
            .arg("--input")
            .arg(&input)
            .arg("--output")
            .arg(&output)
            .arg("--verify-legal=false")
            .output()
            .unwrap()
    };
    assert_rejected(&convert());
    assert_eq!(fs::read(&partial).unwrap(), b"previous partial");
    assert_eq!(fs::read(&input).unwrap(), bytes);
    assert_eq!(fs::read(&output).unwrap(), b"previous final");
    fs::remove_file(&partial).unwrap();
    assert_success(&convert());
    let mut expected = bytes.clone();
    expected[34..36].copy_from_slice(&[0xa5, 0x40]);
    assert_eq!(fs::read(&output).unwrap(), expected);
    assert_eq!(fs::read(&input).unwrap(), bytes);
    assert!(!partial.exists());
}

#[test]
fn pack_rejects_alias_of_any_input_before_creating_output() {
    let bytes = pack_game();
    for alias in aliases() {
        let dir = TempDir::new().unwrap();
        let first = dir.path().join("first.pack");
        fs::write(&first, &bytes).unwrap();
        let (second, output) = alias_pair(dir.path(), alias, &bytes);
        let result = Command::new(PACK)
            .arg("--input")
            .arg(format!("{},{}", first.display(), second.display()))
            .arg("--output")
            .arg(&output)
            .arg("--max-games")
            .arg("1")
            .output()
            .unwrap();
        assert_rejected(&result);
        assert_eq!(fs::read(&first).unwrap(), bytes);
        assert_eq!(fs::read(&second).unwrap(), bytes, "{alias:?}");
        assert_eq!(fs::read(&output).unwrap(), bytes);
    }
}

#[test]
fn pack_distinct_output_contains_played_position() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("input.pack");
    let output = dir.path().join("output.psv");
    let bytes = pack_game();
    fs::write(&input, &bytes).unwrap();
    let result = Command::new(PACK)
        .arg("--input")
        .arg(&input)
        .arg("--output")
        .arg(&output)
        .args(["--max-games", "1"])
        .output()
        .unwrap();
    assert_success(&result);
    let mut pos = Position::new();
    pos.set_hirate();
    let expected = PackedSfenValue {
        sfen: pack_position(&pos),
        score: 123,
        move16: move_to_psv_move16(Move::from_usi("7g7f").unwrap()),
        game_ply: 1,
        game_result: 0,
        padding: 0,
    };
    assert_eq!(fs::read(&output).unwrap(), expected.to_bytes());
    assert_eq!(fs::read(&input).unwrap(), bytes);
}

#[test]
fn filter_rejects_input_aliases_for_data_and_stats() {
    let bytes = record();
    for stats_only in [false, true] {
        for alias in aliases() {
            let dir = TempDir::new().unwrap();
            let (input, output) = alias_pair(dir.path(), alias, &bytes);
            let mut command = Command::new(FILTER);
            command.arg("--input").arg(&input);
            if stats_only {
                command.arg("--stats-only").arg("--stats").arg(&output);
            } else {
                command.arg("--output").arg(&output);
            }
            assert_rejected(&command.output().unwrap());
            assert_eq!(fs::read(&input).unwrap(), bytes, "stats_only={stats_only}, {alias:?}");
            assert_eq!(fs::read(&output).unwrap(), bytes);
        }
    }
}

#[test]
fn filter_rejects_overlapping_outputs_before_writing_either() {
    let bytes = record();
    for alias in aliases() {
        let dir = TempDir::new().unwrap();
        let input = dir.path().join("source.psv");
        fs::write(&input, &bytes).unwrap();
        let sentinel = b"previous output";
        let (output, stats) = alias_pair(dir.path(), alias, sentinel);
        let result = Command::new(FILTER)
            .arg("--input")
            .arg(&input)
            .arg("--output")
            .arg(&output)
            .arg("--stats")
            .arg(&stats)
            .output()
            .unwrap();
        assert_rejected(&result);
        assert_eq!(fs::read(&input).unwrap(), bytes);
        assert_eq!(fs::read(&output).unwrap(), sentinel, "{alias:?}");
        assert_eq!(fs::read(&stats).unwrap(), sentinel);
    }
}

#[test]
fn filter_normal_output_and_stats_only_keep_source_bytes() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("input.psv");
    let output = dir.path().join("output.psv");
    let stats = dir.path().join("stats.json");
    let bytes = record();
    fs::write(&input, &bytes).unwrap();
    let result = Command::new(FILTER)
        .arg("--input")
        .arg(&input)
        .arg("--output")
        .arg(&output)
        .arg("--stats")
        .arg(&stats)
        .output()
        .unwrap();
    assert_success(&result);
    assert_eq!(fs::read(&output).unwrap(), bytes);
    let json: serde_json::Value = serde_json::from_slice(&fs::read(&stats).unwrap()).unwrap();
    assert_eq!(json["total_records"], 1);
    assert_eq!(json["output_records"], 1);
    let result = Command::new(FILTER)
        .arg("--input")
        .arg(&input)
        .arg("--stats-only")
        .arg("--output")
        .arg(&input)
        .arg("--stats")
        .arg(&stats)
        .output()
        .unwrap();
    assert_success(&result);
    assert_eq!(fs::read(&input).unwrap(), bytes);
    let stats_only: serde_json::Value = serde_json::from_slice(&fs::read(&stats).unwrap()).unwrap();
    assert_eq!(stats_only, json);
}

#[test]
fn filter_keeps_new_data_output_when_stats_aliases_it_by_case() {
    let dir = TempDir::new().unwrap();
    let probe = dir.path().join("CaseProbe");
    fs::write(&probe, b"probe").unwrap();
    let case_insensitive = dir.path().join("caseprobe").exists();
    fs::remove_file(probe).unwrap();
    if !case_insensitive {
        return;
    }
    let input = dir.path().join("input.psv");
    let output = dir.path().join("RESULT.PSV");
    let stats = dir.path().join("result.psv");
    let bytes = record();
    fs::write(&input, &bytes).unwrap();
    let result = Command::new(FILTER)
        .arg("--input")
        .arg(&input)
        .arg("--output")
        .arg(&output)
        .arg("--stats")
        .arg(&stats)
        .output()
        .unwrap();
    assert_rejected(&result);
    assert_eq!(fs::read(&input).unwrap(), bytes);
    assert_eq!(fs::read(&output).unwrap(), bytes);
    assert_eq!(fs::read(&stats).unwrap(), bytes);
}

fn fix_command(original: &Path, preprocessed: &Path) -> Command {
    let mut command = Command::new(FIX);
    command.arg("--original").arg(original).arg("--preprocessed").arg(preprocessed);
    command.args(["--sample-count", "0"]);
    command
}

fn preprocessed_record() -> Vec<u8> {
    let mut bytes = record();
    bytes[32..34].copy_from_slice(&1234i16.to_le_bytes());
    bytes
}

#[test]
fn fix_rejects_separate_output_aliases_of_both_inputs() {
    let original_bytes = record();
    let prep_bytes = preprocessed_record();
    for target_original in [false, true] {
        for alias in aliases() {
            if !target_original && matches!(alias, Alias::SamePath) {
                continue; // 同一パスの preprocessed は意図したインプレース更新。
            }
            let dir = TempDir::new().unwrap();
            let original = dir.path().join("original.psv");
            let prep = dir.path().join("preprocessed.psv");
            fs::write(&original, &original_bytes).unwrap();
            fs::write(&prep, &prep_bytes).unwrap();
            let target = if target_original { &original } else { &prep };
            let output = match alias {
                Alias::SamePath => target.clone(),
                _ => dir.path().join("output.psv"),
            };
            link_alias(alias, target, &output);
            assert_success(
                &fix_command(&original, &prep).arg("--output").arg(&output).output().unwrap(),
            );
            assert_eq!(fs::read(&original).unwrap(), original_bytes);
            assert_eq!(fs::read(&prep).unwrap(), prep_bytes);
            let result = fix_command(&original, &prep)
                .arg("--output")
                .arg(&output)
                .arg("--yes")
                .output()
                .unwrap();
            assert_rejected(&result);
            assert_eq!(fs::read(&original).unwrap(), original_bytes);
            assert_eq!(fs::read(&prep).unwrap(), prep_bytes);
        }
    }
}

#[test]
fn fix_preserves_preview_and_supports_distinct_and_in_place_outputs() {
    let original_bytes = record();
    let prep_bytes = preprocessed_record();
    for in_place in [false, true] {
        for explicit in [false, true] {
            if !in_place && !explicit {
                continue;
            }
            let dir = TempDir::new().unwrap();
            let original = dir.path().join("original.psv");
            let prep = dir.path().join("preprocessed.psv");
            let output = if in_place {
                prep.clone()
            } else {
                dir.path().join("output.psv")
            };
            fs::write(&original, &original_bytes).unwrap();
            fs::write(&prep, &prep_bytes).unwrap();
            let mut preview = fix_command(&original, &prep);
            if explicit {
                preview.arg("--output").arg(&output);
            }
            assert_success(&preview.output().unwrap());
            assert_eq!(fs::read(&prep).unwrap(), prep_bytes);
            if !in_place {
                assert!(!output.exists());
            }
            let mut fix = fix_command(&original, &prep);
            fix.arg("--yes");
            if explicit {
                fix.arg("--output").arg(&output);
            }
            assert_success(&fix.output().unwrap());
            assert_eq!(fs::read(&output).unwrap(), original_bytes);
            assert_eq!(fs::read(&original).unwrap(), original_bytes);
            if !in_place {
                assert_eq!(fs::read(&prep).unwrap(), prep_bytes);
            }
        }
    }
}

#[test]
fn fix_in_place_rejects_original_alias_but_preview_remains_read_only() {
    let bytes = record();
    for alias in aliases() {
        let dir = TempDir::new().unwrap();
        let (original, prep) = alias_pair(dir.path(), alias, &bytes);
        assert_success(&fix_command(&original, &prep).output().unwrap());
        assert_rejected(&fix_command(&original, &prep).arg("--yes").output().unwrap());
        assert_eq!(fs::read(&original).unwrap(), bytes);
        assert_eq!(fs::read(&prep).unwrap(), bytes);
    }
}

#[cfg(unix)]
#[test]
fn fix_supports_in_place_preprocessed_symlink() {
    let bytes = record();
    for explicit in [false, true] {
        let dir = TempDir::new().unwrap();
        let original = dir.path().join("original.psv");
        let prep = dir.path().join("preprocessed.psv");
        let link = dir.path().join("preprocessed-link.psv");
        fs::write(&original, &bytes).unwrap();
        fs::write(&prep, preprocessed_record()).unwrap();
        std::os::unix::fs::symlink(&prep, &link).unwrap();
        let mut command = fix_command(&original, &link);
        command.arg("--yes");
        if explicit {
            command.arg("--output").arg(&link);
        }
        assert_success(&command.output().unwrap());
        assert_eq!(fs::read(&prep).unwrap(), bytes);
        assert_eq!(fs::read(&original).unwrap(), bytes);
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
    }
}
