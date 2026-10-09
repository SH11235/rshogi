//! psv_to_hcpe3 の bit 一致 / 決定性 integration テスト
//!
//! `tests/fixtures/psv_to_hcpe3_yaneuraou_sample.psv` を入力に、cshogi 製オラクル
//! （`psv_to_hcpe3.py` / dlshogi `psv_to_hcpe.py`）の出力 `.hcpe3` / `.hcpe` と
//! byte 完全一致することを確認する。旧形式 fixture は混入ガードの回帰テストに使う。

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_psv_to_hcpe3");

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

fn run(input: &Path, output: &Path, format: &str, threads: &str, chunk: &str) {
    let status = Command::new(BIN)
        .args([
            "--input",
            input.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--format",
            format,
            "--threads",
            threads,
            "--chunk",
            chunk,
        ])
        .status()
        .expect("failed to run psv_to_hcpe3");
    assert!(status.success(), "psv_to_hcpe3 exited with failure");
}

#[test]
fn legacy_move16_fixture_is_rejected() {
    let input = fixture("psv_to_hcpe3_sample.psv");
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("legacy.bin");
    let status = Command::new(BIN)
        .args([
            "--input",
            input.to_str().unwrap(),
            "--output",
            out.to_str().unwrap(),
        ])
        .status()
        .expect("failed to run psv_to_hcpe3");
    assert!(!status.success(), "legacy move16 input must be rejected");
}

#[test]
fn hardlinked_input_aliases_are_rejected_without_modifying_files() {
    for staging_alias in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.psv");
        let output = dir.path().join("output.hcpe3");
        let original = std::fs::read(fixture("psv_to_hcpe3_yaneuraou_sample.psv")).unwrap();
        std::fs::write(&input, &original).unwrap();
        let alias = if staging_alias {
            std::fs::write(&output, b"existing output").unwrap();
            tools::common::io::partial_path(&output)
        } else {
            output.clone()
        };
        std::fs::hard_link(&input, &alias).unwrap();

        let result = Command::new(BIN)
            .args(["--input"])
            .arg(&input)
            .arg("--output")
            .arg(&output)
            .output()
            .unwrap();

        assert!(!result.status.success(), "input alias must be rejected");
        assert_eq!(std::fs::read(&input).unwrap(), original);
        assert_eq!(std::fs::read(&alias).unwrap(), original);
        if staging_alias {
            assert_eq!(std::fs::read(&output).unwrap(), b"existing output");
        } else {
            assert!(!tools::common::io::partial_path(&output).exists());
        }
    }
}

#[test]
fn existing_partial_is_never_overwritten() {
    for linked_output in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.psv");
        let output = dir.path().join("output.hcpe3");
        let partial = tools::common::io::partial_path(&output);
        let original = std::fs::read(fixture("psv_to_hcpe3_yaneuraou_sample.psv")).unwrap();
        std::fs::write(&input, &original).unwrap();
        std::fs::write(&output, b"existing output").unwrap();
        if linked_output {
            std::fs::hard_link(&output, &partial).unwrap();
        } else {
            std::fs::write(&partial, b"another conversion").unwrap();
        }
        let partial_before = std::fs::read(&partial).unwrap();

        let result = Command::new(BIN)
            .arg("--input")
            .arg(&input)
            .arg("--output")
            .arg(&output)
            .arg("--threads")
            .arg("1")
            .output()
            .unwrap();

        assert!(!result.status.success(), "existing partial must be rejected");
        assert!(String::from_utf8_lossy(&result.stderr).contains(".partial"));
        assert_eq!(std::fs::read(&input).unwrap(), original);
        assert_eq!(std::fs::read(&partial).unwrap(), partial_before);
        assert_eq!(std::fs::read(&output).unwrap(), b"existing output");
    }
}

#[cfg(unix)]
#[test]
fn symlink_outputs_are_rejected_without_touching_targets() {
    for staging_alias in [false, true] {
        for dangling in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let input = dir.path().join("input.psv");
            let output = dir.path().join("output.hcpe3");
            let target = dir.path().join("target");
            let original = std::fs::read(fixture("psv_to_hcpe3_yaneuraou_sample.psv")).unwrap();
            std::fs::write(&input, &original).unwrap();
            if !dangling {
                std::fs::write(&target, b"unrelated data").unwrap();
            }
            let alias = if staging_alias {
                std::fs::write(&output, b"existing output").unwrap();
                tools::common::io::partial_path(&output)
            } else {
                output.clone()
            };
            std::os::unix::fs::symlink(&target, &alias).unwrap();

            let result = Command::new(BIN)
                .arg("--input")
                .arg(&input)
                .arg("--output")
                .arg(&output)
                .output()
                .unwrap();

            assert!(!result.status.success(), "symlink output must be rejected");
            assert_eq!(std::fs::read(&input).unwrap(), original);
            assert!(std::fs::symlink_metadata(&alias).unwrap().file_type().is_symlink());
            if dangling {
                assert!(!target.exists());
            } else {
                assert_eq!(std::fs::read(&target).unwrap(), b"unrelated data");
            }
            if staging_alias {
                assert_eq!(std::fs::read(&output).unwrap(), b"existing output");
            }
        }
    }
}

#[test]
fn successful_conversion_replaces_existing_output_and_removes_partial() {
    let dir = tempfile::tempdir().unwrap();
    let input = fixture("psv_to_hcpe3_yaneuraou_sample.psv");
    let output = dir.path().join("output.hcpe3");
    std::fs::write(&output, b"existing output").unwrap();

    run(&input, &output, "hcpe3", "1", "7");

    let expected = std::fs::read(fixture("psv_to_hcpe3_yaneuraou_sample.hcpe3")).unwrap();
    assert_eq!(std::fs::read(&output).unwrap(), expected);
    assert!(!tools::common::io::partial_path(&output).exists());
}

// 実 YaneuraOu PSV 形式（成り=bit15 / 駒打ち=bit14+from=駒種）の move16 を含む fixture。
// 旧 0x1800 減算方式は bit15 の成りを誤変換するため、この fixture は回帰ガードになる。
#[test]
fn hcpe3_matches_cshogi_oracle_yaneuraou_format() {
    let input = fixture("psv_to_hcpe3_yaneuraou_sample.psv");
    let expected = std::fs::read(fixture("psv_to_hcpe3_yaneuraou_sample.hcpe3")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("yo.hcpe3");
    run(&input, &out, "hcpe3", "1", "200000");
    assert_eq!(
        std::fs::read(&out).unwrap(),
        expected,
        "real-YaneuraOu PSV (bit15 promote / bit14 drop) must byte-match the cshogi oracle"
    );
}

#[test]
fn hcpe_matches_cshogi_oracle_yaneuraou_format() {
    let input = fixture("psv_to_hcpe3_yaneuraou_sample.psv");
    let expected = std::fs::read(fixture("psv_to_hcpe3_yaneuraou_sample.hcpe")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("yo.hcpe");
    run(&input, &out, "hcpe", "1", "200000");
    assert_eq!(
        std::fs::read(&out).unwrap(),
        expected,
        "real-YaneuraOu PSV hcpe output must byte-match the cshogi oracle"
    );
}

#[test]
fn trailing_partial_bytes_are_ignored() {
    // 末尾にレコード長未満の半端バイトがあっても、完全なレコードの出力は ground truth と一致し、
    // ツールは成功終了する（半端バイトはスキップ）。
    let expected = std::fs::read(fixture("psv_to_hcpe3_yaneuraou_sample.hcpe3")).unwrap();
    let mut psv = std::fs::read(fixture("psv_to_hcpe3_yaneuraou_sample.psv")).unwrap();
    psv.extend_from_slice(&[0u8; 7]); // 40 バイト境界に満たない末尾
    let dir = tempfile::tempdir().unwrap();
    let truncated_input = dir.path().join("trailing.psv");
    std::fs::write(&truncated_input, &psv).unwrap();
    let out = dir.path().join("trailing.hcpe3");
    run(&truncated_input, &out, "hcpe3", "1", "200000");
    assert_eq!(
        std::fs::read(&out).unwrap(),
        expected,
        "trailing partial bytes must not affect full-record output"
    );
}

#[test]
fn output_path_with_tmp_extension_is_not_truncated() {
    // `--output *.tmp` でも一時ファイル（.partial 付与）と最終パスが衝突せず正しく出力される。
    let input = fixture("psv_to_hcpe3_yaneuraou_sample.psv");
    let expected = std::fs::read(fixture("psv_to_hcpe3_yaneuraou_sample.hcpe3")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("output.tmp");
    run(&input, &out, "hcpe3", "1", "200000");
    assert_eq!(std::fs::read(&out).unwrap(), expected, "output must be correct for *.tmp path");
}

#[test]
fn limit_restricts_output_record_count() {
    // --limit N は先頭 N レコードだけ変換する（出力 = N × 46 バイト）。
    let input = fixture("psv_to_hcpe3_yaneuraou_sample.psv");
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("limited.hcpe3");
    let status = Command::new(BIN)
        .args([
            "--input",
            input.to_str().unwrap(),
            "--output",
            out.to_str().unwrap(),
            "--format",
            "hcpe3",
            "--limit",
            "10",
        ])
        .status()
        .expect("failed to run psv_to_hcpe3");
    assert!(status.success());
    assert_eq!(
        std::fs::metadata(&out).unwrap().len(),
        10 * 46,
        "--limit 10 must emit 10 records"
    );
}

#[test]
fn record_without_move_is_skipped_and_counted() {
    let dir = tempfile::TempDir::new().unwrap();
    let input = dir.path().join("terminal.psv");
    let out = dir.path().join("terminal.hcpe3");
    let fixture_bytes = std::fs::read(fixture("psv_to_hcpe3_yaneuraou_sample.psv")).unwrap();
    let mut terminal = fixture_bytes[..40].to_vec();
    terminal[34..36].copy_from_slice(&0u16.to_le_bytes());
    std::fs::write(&input, terminal).unwrap();

    let output = Command::new(BIN)
        .args([
            "--input",
            input.to_str().unwrap(),
            "--output",
            out.to_str().unwrap(),
            "--format",
            "hcpe3",
        ])
        .output()
        .expect("failed to run psv_to_hcpe3");

    assert!(output.status.success());
    assert_eq!(std::fs::metadata(out).unwrap().len(), 0);
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("有効な着手を持たないレコードをスキップ: 1 レコード (move16=0)")
    );
}

#[test]
fn output_is_thread_count_independent() {
    // 出力はスレッド数・チャンク境界に依らず bit 一致でなければならない。
    let input = fixture("psv_to_hcpe3_yaneuraou_sample.psv");
    let dir = tempfile::tempdir().unwrap();
    let out1 = dir.path().join("thread1.hcpe3");
    let out4 = dir.path().join("thread4.hcpe3");
    run(&input, &out1, "hcpe3", "1", "200000");
    run(&input, &out4, "hcpe3", "4", "7");
    assert_eq!(
        std::fs::read(&out1).unwrap(),
        std::fs::read(&out4).unwrap(),
        "output must be identical regardless of thread count and chunk size"
    );
}
