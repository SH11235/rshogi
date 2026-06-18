//! psv_to_hcpe3 の bit 一致 / 決定性 integration テスト
//!
//! `tests/fixtures/psv_to_hcpe3_sample.psv` を入力に、cshogi 製オラクル
//! （`psv_to_hcpe3.py` / dlshogi `psv_to_hcpe.py`）の出力 `.hcpe3` / `.hcpe` と
//! byte 完全一致することを確認する。fixture は rshogi 自前の gensfen 自己対局 PSV から
//! 通常手・駒打ち・成り × 先後 × 勝敗 を網羅するよう抽出した 56 局面。

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
fn hcpe3_matches_cshogi_oracle() {
    let input = fixture("psv_to_hcpe3_sample.psv");
    let expected = std::fs::read(fixture("psv_to_hcpe3_sample.hcpe3")).unwrap();
    let out = std::env::temp_dir().join("psv_to_hcpe3_it_hcpe3.bin");
    run(&input, &out, "hcpe3", "1", "200000");
    let got = std::fs::read(&out).unwrap();
    assert_eq!(got, expected, "hcpe3 output must byte-match the cshogi oracle");
}

#[test]
fn hcpe_matches_cshogi_oracle() {
    let input = fixture("psv_to_hcpe3_sample.psv");
    let expected = std::fs::read(fixture("psv_to_hcpe3_sample.hcpe")).unwrap();
    let out = std::env::temp_dir().join("psv_to_hcpe3_it_hcpe.bin");
    run(&input, &out, "hcpe", "1", "200000");
    let got = std::fs::read(&out).unwrap();
    assert_eq!(got, expected, "hcpe output must byte-match the cshogi oracle");
}

#[test]
fn trailing_partial_bytes_are_ignored() {
    // 末尾にレコード長未満の半端バイトがあっても、完全なレコードの出力は ground truth と一致し、
    // ツールは成功終了する（半端バイトはスキップ）。
    let expected = std::fs::read(fixture("psv_to_hcpe3_sample.hcpe3")).unwrap();
    let mut psv = std::fs::read(fixture("psv_to_hcpe3_sample.psv")).unwrap();
    psv.extend_from_slice(&[0u8; 7]); // 40 バイト境界に満たない末尾
    let truncated_input = std::env::temp_dir().join("psv_to_hcpe3_it_trailing.psv");
    std::fs::write(&truncated_input, &psv).unwrap();
    let out = std::env::temp_dir().join("psv_to_hcpe3_it_trailing.bin");
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
    let input = fixture("psv_to_hcpe3_sample.psv");
    let expected = std::fs::read(fixture("psv_to_hcpe3_sample.hcpe3")).unwrap();
    let out = std::env::temp_dir().join("psv_to_hcpe3_it_out.tmp");
    run(&input, &out, "hcpe3", "1", "200000");
    assert_eq!(std::fs::read(&out).unwrap(), expected, "output must be correct for *.tmp path");
}

#[test]
fn limit_restricts_output_record_count() {
    // --limit N は先頭 N レコードだけ変換する（出力 = N × 46 バイト）。
    let input = fixture("psv_to_hcpe3_sample.psv");
    let out = std::env::temp_dir().join("psv_to_hcpe3_it_limit.bin");
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
fn output_is_thread_count_independent() {
    // 出力はスレッド数・チャンク境界に依らず bit 一致でなければならない。
    let input = fixture("psv_to_hcpe3_sample.psv");
    let out1 = std::env::temp_dir().join("psv_to_hcpe3_it_t1.bin");
    let out4 = std::env::temp_dir().join("psv_to_hcpe3_it_t4.bin");
    run(&input, &out1, "hcpe3", "1", "200000");
    run(&input, &out4, "hcpe3", "4", "7");
    assert_eq!(
        std::fs::read(&out1).unwrap(),
        std::fs::read(&out4).unwrap(),
        "output must be identical regardless of thread count and chunk size"
    );
}
