use std::fs;
use std::process::Command;

use sha2::{Digest, Sha256};

const RECORD_SIZE: usize = 40;

fn run_shuffle(
    input: &std::path::Path,
    output: &std::path::Path,
    threads: usize,
    extra_args: &[&str],
) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_shuffle_psv"));
    command
        .args([
            "--input",
            input.to_str().expect("UTF-8 input path"),
            "--output",
            output.to_str().expect("UTF-8 output path"),
            "--seed",
            "20260801",
            "--chunk-size",
            "500",
        ])
        .args(extra_args)
        .env("RAYON_NUM_THREADS", threads.to_string())
        .env("RUST_LOG", "info");
    let result = command.output().expect("shuffle_psv を実行できること");

    assert!(
        result.status.success(),
        "shuffle_psv failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stderr).expect("ログが UTF-8 であること")
}

#[test]
fn chunked_output_is_independent_of_threads_and_batch_size() {
    let dir = tempfile::tempdir().expect("一時ディレクトリを作成できること");
    let input = dir.path().join("input.psv");
    let single_output = dir.path().join("single.psv");
    let multi_output = dir.path().join("multi.psv");

    let mut records = vec![0u8; 3_000 * RECORD_SIZE];
    for (index, record) in records.as_chunks_mut::<RECORD_SIZE>().0.iter_mut().enumerate() {
        record[..8].copy_from_slice(&(index as u64).to_le_bytes());
        for (offset, byte) in record[8..].iter_mut().enumerate() {
            *byte = (index.wrapping_mul(31).wrapping_add(offset) & 0xff) as u8;
        }
    }
    fs::write(&input, records).expect("入力 PSV を作成できること");

    let single_log = run_shuffle(&input, &single_output, 1, &[]);
    let multi_log = run_shuffle(&input, &multi_output, 4, &[]);

    assert!(single_log.contains("Pass 2 batching (threads: 1,"));
    assert!(multi_log.contains("Pass 2 batching (threads: 4,"));
    assert_eq!(
        fs::read(single_output).expect("単一スレッド出力を読めること"),
        fs::read(multi_output).expect("複数スレッド出力を読めること"),
    );
}

#[test]
fn staged_deletion_flags_preserve_output_sha256() {
    let dir = tempfile::tempdir().expect("一時ディレクトリを作成できること");
    let mut records = vec![0u8; 3_000 * RECORD_SIZE];
    for (index, record) in records.as_chunks_mut::<RECORD_SIZE>().0.iter_mut().enumerate() {
        record[..8].copy_from_slice(&(index as u64).to_le_bytes());
        for (offset, byte) in record[8..].iter_mut().enumerate() {
            *byte = (index.wrapping_mul(31).wrapping_add(offset) & 0xff) as u8;
        }
    }

    let configurations: [(&[&str], bool); 4] = [
        (&[], false),
        (&["--delete-chunk-files"], false),
        (&["--delete-input-after-pass1"], true),
        (&["--delete-chunk-files", "--delete-input-after-pass1"], true),
    ];
    let mut expected_sha256 = None;

    for (case_index, (extra_args, deletes_input)) in configurations.into_iter().enumerate() {
        let input = dir.path().join(format!("input_{case_index}.psv"));
        let output = dir.path().join(format!("output_{case_index}.psv"));
        fs::write(&input, &records).expect("入力 PSV を作成できること");

        run_shuffle(&input, &output, 4, extra_args);

        let output_bytes = fs::read(&output).expect("出力 PSV を読めること");
        assert_eq!(output_bytes.len(), records.len());
        let sha256 = format!("{:x}", Sha256::digest(&output_bytes));
        if let Some(expected) = &expected_sha256 {
            assert_eq!(&sha256, expected);
        } else {
            expected_sha256 = Some(sha256);
        }
        assert_eq!(input.exists(), !deletes_input);
    }
}

#[test]
fn pass2_failure_after_input_deletion_exits_with_preserved_chunks() {
    let dir = tempfile::tempdir().expect("一時ディレクトリを作成できること");
    let input = dir.path().join("input.psv");
    let output = dir.path().join("missing-parent").join("output.psv");
    fs::write(&input, [0u8; 4 * RECORD_SIZE]).expect("入力 PSV を作成できること");

    let result = Command::new(env!("CARGO_BIN_EXE_shuffle_psv"))
        .args([
            "--input",
            input.to_str().expect("UTF-8 input path"),
            "--output",
            output.to_str().expect("UTF-8 output path"),
            "--seed",
            "42",
            "--chunk-size",
            "1",
            "--force",
            "--delete-chunk-files",
            "--delete-input-after-pass1",
        ])
        .output()
        .expect("shuffle_psv を実行できること");

    assert!(!result.status.success());
    assert!(!input.exists());
    assert!(!output.exists());

    let stderr = String::from_utf8_lossy(&result.stderr);
    let path_text = stderr
        .split_once("すべてのチャンクは ")
        .and_then(|(_, rest)| rest.split_once(" に保全済み").map(|(path, _)| path))
        .expect("stderr に保全したチャンクのパスが含まれること");
    let preserved_path = std::path::PathBuf::from(path_text);
    assert!(preserved_path.is_dir());
    let chunk_count = fs::read_dir(&preserved_path).expect("保全チャンクを列挙できること").count();
    assert_eq!(chunk_count, 4);

    fs::remove_dir_all(&preserved_path).expect("検証後に保全チャンクを削除できること");
}
