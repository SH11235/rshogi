use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;
use tools::packed_sfen::PackedSfenValue;

const SELECT_BIN: &str = env!("CARGO_BIN_EXE_psv_select_by_mask");
const SCATTER_BIN: &str = env!("CARGO_BIN_EXE_psv_scatter_by_mask");

fn records(count: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(count * PackedSfenValue::SIZE);
    for row in 0..count {
        let mut sfen = [0u8; 32];
        for (column, byte) in sfen.iter_mut().enumerate() {
            *byte = row.wrapping_mul(41).wrapping_add(column) as u8;
        }
        bytes.extend_from_slice(
            &PackedSfenValue {
                sfen,
                score: row as i16 * 10 - 50,
                move16: row as u16 + 100,
                game_ply: row as u16 + 200,
                game_result: row as i8 % 3 - 1,
                padding: row as u8,
            }
            .to_bytes(),
        );
    }
    bytes
}

fn run_select(input: &Path, mask: &Path, compact: &Path) -> Output {
    Command::new(SELECT_BIN)
        .args([
            "--input",
            input.to_str().unwrap(),
            "--mask",
            mask.to_str().unwrap(),
            "--out",
            compact.to_str().unwrap(),
        ])
        .output()
        .unwrap()
}

fn run_scatter(input: &Path, mask: &Path, compact: &Path, out: &Path) -> Output {
    Command::new(SCATTER_BIN)
        .args([
            "--input",
            input.to_str().unwrap(),
            "--mask",
            mask.to_str().unwrap(),
            "--compact",
            compact.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
        ])
        .output()
        .unwrap()
}

fn assert_stats(output: &Output, expected: &[(&str, u64)]) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout.clone()).unwrap();
    let stats: BTreeMap<_, _> = stdout
        .split_whitespace()
        .filter_map(|field| field.split_once('='))
        .filter_map(|(key, value)| value.parse::<u64>().ok().map(|value| (key, value)))
        .collect();
    for &(key, value) in expected {
        assert_eq!(stats.get(key), Some(&value), "stdout: {stdout}");
    }
}

fn replace_scores(bytes: &mut [u8], scores: &[i16]) {
    assert_eq!(bytes.len(), scores.len() * PackedSfenValue::SIZE);
    for (record, &score) in
        bytes.as_chunks_mut::<{ PackedSfenValue::SIZE }>().0.iter_mut().zip(scores)
    {
        let mut value = PackedSfenValue::from_bytes(record).unwrap();
        value.score = score;
        record.copy_from_slice(&value.to_bytes());
    }
}

#[test]
fn real_binary_roundtrip_preserves_rows_and_counts_only_changed_scores() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("input.psv");
    let mask = dir.path().join("mask.bits");
    let compact = dir.path().join("compact.psv");
    let out = dir.path().join("out.psv");
    let input_bytes = records(9);
    let selected = [0usize, 2, 5, 7, 8];
    fs::write(&input, &input_bytes).unwrap();
    fs::write(&mask, [0b1010_0101, 0b0000_0001]).unwrap();

    let select = run_select(&input, &mask, &compact);
    assert_stats(&select, &[("records", 9), ("selected", 5)]);
    let mut compact_bytes = fs::read(&compact).unwrap();
    for (compact_row, &input_row) in selected.iter().enumerate() {
        let compact_start = compact_row * PackedSfenValue::SIZE;
        let input_start = input_row * PackedSfenValue::SIZE;
        assert_eq!(
            &compact_bytes[compact_start..compact_start + PackedSfenValue::SIZE],
            &input_bytes[input_start..input_start + PackedSfenValue::SIZE]
        );
    }

    let scores = [-300, -30, -301, 20, -302];
    replace_scores(&mut compact_bytes, &scores);
    fs::write(&compact, &compact_bytes).unwrap();
    let scatter = run_scatter(&input, &mask, &compact, &out);
    assert_stats(&scatter, &[("records", 9), ("replaced", 5), ("changed", 3)]);

    let output = fs::read(out).unwrap();
    let mut expected = input_bytes.clone();
    for (&row, &score) in selected.iter().zip(&scores) {
        let start = row * PackedSfenValue::SIZE;
        let mut value =
            PackedSfenValue::from_bytes(&expected[start..start + PackedSfenValue::SIZE]).unwrap();
        value.score = score;
        expected[start..start + PackedSfenValue::SIZE].copy_from_slice(&value.to_bytes());
    }
    assert_eq!(output, expected);
}

#[test]
fn unchanged_compact_is_bit_exact_identity() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("input.psv");
    let mask = dir.path().join("mask.bits");
    let compact = dir.path().join("compact.psv");
    let out = dir.path().join("out.psv");
    let input_bytes = records(9);
    fs::write(&input, &input_bytes).unwrap();
    fs::write(&mask, [0b1010_0110, 0b0000_0001]).unwrap();

    let select = run_select(&input, &mask, &compact);
    assert_stats(&select, &[("records", 9), ("selected", 5)]);
    let scatter = run_scatter(&input, &mask, &compact, &out);
    assert_stats(&scatter, &[("records", 9), ("replaced", 5), ("changed", 0)]);
    assert_eq!(fs::read(out).unwrap(), input_bytes);
}

#[test]
fn empty_input_roundtrip_succeeds_with_zero_stats() {
    let dir = TempDir::new().unwrap();
    let input = dir.path().join("input.psv");
    let mask = dir.path().join("mask.bits");
    let compact = dir.path().join("compact.psv");
    let out = dir.path().join("out.psv");
    fs::write(&input, []).unwrap();
    fs::write(&mask, []).unwrap();

    let select = run_select(&input, &mask, &compact);
    assert_stats(&select, &[("records", 0), ("selected", 0)]);
    assert!(fs::read(&compact).unwrap().is_empty());
    let scatter = run_scatter(&input, &mask, &compact, &out);
    assert_stats(&scatter, &[("records", 0), ("replaced", 0), ("changed", 0)]);
    assert!(fs::read(out).unwrap().is_empty());
}

#[test]
fn all_zero_and_all_one_masks_roundtrip() {
    for (name, mask_bytes, selected) in [("zero", [0, 0], 0u64), ("one", [0xff, 0x01], 9u64)] {
        let dir = TempDir::new().unwrap();
        let input = dir.path().join(format!("{name}-input.psv"));
        let mask = dir.path().join(format!("{name}-mask.bits"));
        let compact = dir.path().join(format!("{name}-compact.psv"));
        let out = dir.path().join(format!("{name}-out.psv"));
        let input_bytes = records(9);
        fs::write(&input, &input_bytes).unwrap();
        fs::write(&mask, mask_bytes).unwrap();

        let select = run_select(&input, &mask, &compact);
        assert_stats(&select, &[("records", 9), ("selected", selected)]);
        assert_eq!(fs::metadata(&compact).unwrap().len(), selected * PackedSfenValue::SIZE as u64);
        let scatter = run_scatter(&input, &mask, &compact, &out);
        assert_stats(&scatter, &[("records", 9), ("replaced", selected), ("changed", 0)]);
        assert_eq!(fs::read(out).unwrap(), input_bytes);
    }
}
