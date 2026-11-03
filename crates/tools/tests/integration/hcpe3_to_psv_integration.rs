//! hcpe3_to_psv を cshogi 製の hcpe3 fixture で確認する。
//!
//! `psv_to_hcpe3_yaneuraou_sample.hcpe3` は同名 `.psv` を cshogi で hcpe3 にしたもので、
//! 1 局 = 1 局面。指し手の無い PSV レコードは hcpe3 に入らない。

use std::path::PathBuf;
use std::process::Command;

use tools::packed_sfen::{PackedSfenValue, unpack_sfen};

const BIN: &str = env!("CARGO_BIN_EXE_hcpe3_to_psv");

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

fn records(bytes: &[u8]) -> Vec<PackedSfenValue> {
    bytes
        .as_chunks::<{ PackedSfenValue::SIZE }>()
        .0
        .iter()
        .map(|chunk| PackedSfenValue::from_bytes(chunk).unwrap())
        .collect()
}

#[test]
fn cshogi_hcpe3_round_trips_to_the_source_psv() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.psv");
    let status = Command::new(BIN)
        .args([
            "--input",
            fixture("psv_to_hcpe3_yaneuraou_sample.hcpe3").to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
        ])
        .status()
        .expect("failed to run hcpe3_to_psv");
    assert!(status.success());

    let source: Vec<_> =
        records(&std::fs::read(fixture("psv_to_hcpe3_yaneuraou_sample.psv")).unwrap())
            .into_iter()
            .filter(|record| record.move16 != 0)
            .collect();
    let converted = records(&std::fs::read(output).unwrap());
    assert!(!converted.is_empty());
    assert_eq!(converted.len(), source.len());
    for (index, (got, want)) in converted.iter().zip(&source).enumerate() {
        // 持ち駒の並びは packed sfen の生成系ごとに違い得るので、復号した局面で比べる。
        assert_eq!(
            unpack_sfen(&got.sfen).unwrap(),
            unpack_sfen(&want.sfen).unwrap(),
            "position mismatch at {index}"
        );
        assert_eq!(got.score, want.score, "score mismatch at {index}");
        assert_eq!(got.move16, want.move16, "move16 mismatch at {index}");
        assert_eq!(got.game_result, want.game_result, "result mismatch at {index}");
        assert_eq!(got.game_ply, 1, "game_ply at {index}");
    }
}
