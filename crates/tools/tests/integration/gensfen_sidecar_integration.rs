use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;
use tools::packed_sfen::PackedSfenValue;

const BIN: &str = env!("CARGO_BIN_EXE_gensfen");

#[test]
fn gensfen_rejects_sidecar_collisions_with_normalized_output_paths() {
    let dir = TempDir::new().unwrap();
    let out_dir = dir.path().join("out");

    for (extra_arg, sidecar) in [
        ("--emit-eval-file", out_dir.join("gensfen.eval.txt")),
        ("--log-info", out_dir.join(".").join("gensfen.info.jsonl")),
        ("--emit-metrics", out_dir.join("gensfen.w1.metrics.jsonl")),
        ("--emit-eval-file", out_dir.join(".").join("gensfen.psv")),
    ] {
        let result = Command::new(BIN)
            .args([
                "--out-dir",
                out_dir.to_str().unwrap(),
                "--concurrency",
                "2",
                extra_arg,
                "--emit-game-id-sidecar",
                sidecar.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(
            String::from_utf8_lossy(&result.stderr).contains("conflicts"),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

#[cfg(unix)]
#[test]
fn gensfen_rejects_sidecar_collision_through_symlink_followed_by_parent_dir() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new().unwrap();
    let out_dir = dir.path().join("out");
    let real_dir = dir.path().join("real");
    std::fs::create_dir_all(real_dir.join("sub")).unwrap();
    std::fs::create_dir_all(&out_dir).unwrap();
    symlink(real_dir.join("sub"), out_dir.join("link")).unwrap();

    let training_data = real_dir.join("gensfen.psv");
    let sidecar = out_dir.join("link").join("..").join("gensfen.psv");
    let result = Command::new(BIN)
        .args([
            "--out-dir",
            out_dir.to_str().unwrap(),
            "--output-training-data",
            training_data.to_str().unwrap(),
            "--emit-game-id-sidecar",
            sidecar.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("conflicts"),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

fn write_sparse_zero_halfkp(path: &std::path::Path) {
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
fn real_native_gensfen_concatenates_psv_and_game_id_sidecar_in_lockstep() {
    let dir = TempDir::new().unwrap();
    let out_dir = dir.path().join("out");
    let eval_file = dir.path().join("zero.nnue");
    let sidecar = dir.path().join("game_ids.bin");
    write_sparse_zero_halfkp(&eval_file);

    let result = Command::new(BIN)
        .args([
            "--out-dir",
            out_dir.to_str().unwrap(),
            "--eval-file",
            eval_file.to_str().unwrap(),
            "--games",
            "2",
            "--max-moves",
            "2",
            "--depth",
            "1",
            "--concurrency",
            "2",
            "--hash-mb",
            "1",
            "--dedup-hash-size",
            "0",
            "--startpos-no-repeat=false",
            "--emit-game-id-sidecar",
            sidecar.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));

    let psv = out_dir.join("gensfen.psv");
    let psv_records = std::fs::metadata(psv).unwrap().len() as usize / PackedSfenValue::SIZE;
    let sidecar_bytes = std::fs::read(sidecar).unwrap();
    assert_eq!(sidecar_bytes.len() % 4, 0);
    let game_ids: Vec<u32> = sidecar_bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|bytes| u32::from_le_bytes(*bytes))
        .collect();
    assert!(psv_records > 0);
    assert_eq!(psv_records, game_ids.len());

    let result_ids: HashSet<u32> =
        BufReader::new(File::open(out_dir.join("gensfen.jsonl")).unwrap())
            .lines()
            .map(|line| serde_json::from_str::<Value>(&line.unwrap()).unwrap())
            .filter(|value| value["type"] == "result")
            .map(|value| value["game_id"].as_u64().unwrap() as u32)
            .collect();
    assert_eq!(result_ids, HashSet::from([1, 2]));
    assert!(game_ids.iter().all(|game_id| result_ids.contains(game_id)));

    let meta: Value = serde_json::from_str(
        std::fs::read_to_string(out_dir.join("gensfen.jsonl"))
            .unwrap()
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    let native_sha = meta["fingerprint"]["engine"]["sha256_black"].as_str().unwrap();
    assert_eq!(native_sha.len(), 64);
    assert_eq!(native_sha, meta["fingerprint"]["engine"]["sha256_white"].as_str().unwrap());

    for worker in 0..2 {
        assert!(!out_dir.join(format!("gensfen.w{worker}.jsonl")).exists());
        assert!(!out_dir.join(format!("gensfen.w{worker}.psv")).exists());
        assert!(!out_dir.join(format!("gensfen.w{worker}.game_ids.bin")).exists());
    }
}

#[test]
fn gensfen_tree_sample_cli_requires_both_arguments() {
    for args in [
        vec!["--tree-sample-p", "1"],
        vec!["--tree-sample-out", "tree.psv"],
    ] {
        let result = Command::new(BIN).args(args).output().unwrap();
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("required"));
    }
}

#[cfg(not(feature = "search-tree-sample"))]
#[test]
fn gensfen_tree_sample_requires_feature() {
    let result = Command::new(BIN)
        .args(["--tree-sample-p", "1", "--tree-sample-out", "tree.psv"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("--features search-tree-sample"));
}

#[cfg(feature = "search-tree-sample")]
#[test]
fn gensfen_tree_sample_rejects_non_native_and_multiple_threads() {
    for (option, value, message) in [
        ("--threads", "2", "threads=1"),
        ("--threads-black", "2", "threads=1"),
        ("--threads-white", "2", "threads=1"),
        ("--native", "false", "native mode"),
    ] {
        let result = Command::new(BIN)
            .args([
                "--tree-sample-p",
                "1",
                "--tree-sample-out",
                "tree.psv",
                option,
                value,
            ])
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(
            String::from_utf8_lossy(&result.stderr).contains(message),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

#[cfg(feature = "search-tree-sample")]
#[test]
fn real_native_gensfen_tree_sample_is_reproducible_and_resumable() {
    let dir = TempDir::new().unwrap();
    let eval_file = dir.path().join("zero.nnue");
    write_sparse_zero_halfkp(&eval_file);
    let run = |name: &str, resume: bool, probability: &str, fault: Option<&str>| {
        let out = dir.path().join(name);
        let tree = out.join("samples.psv");
        let mut command = Command::new(BIN);
        command.args([
            "--out-dir",
            out.to_str().unwrap(),
            "--eval-file",
            eval_file.to_str().unwrap(),
            "--games",
            "2",
            "--max-moves",
            "3",
            "--depth",
            "2",
            "--concurrency",
            "1",
            "--hash-mb",
            "1",
            "--dedup-hash-size",
            "0",
            "--startpos-no-repeat=false",
            "--shuffle-seed",
            "123",
            "--tree-sample-p",
            probability,
            "--tree-sample-out",
            tree.to_str().unwrap(),
        ]);
        if resume {
            command.arg("--resume");
        }
        if let Some(fault) = fault {
            command.env("RSHOGI_GENSFEN_FAULT", fault);
        }
        command.output().unwrap()
    };
    for name in ["first", "second"] {
        let result = run(name, false, "1", None);
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
        assert!(String::from_utf8_lossy(&result.stdout).contains("records_written="));
    }
    let first = std::fs::read(dir.path().join("first/samples.psv")).unwrap();
    assert!(!first.is_empty());
    assert_eq!(first.len() % PackedSfenValue::SIZE, 0);
    for bytes in first.as_chunks::<{ PackedSfenValue::SIZE }>().0 {
        let record = PackedSfenValue::from_bytes(bytes).unwrap();
        assert_eq!((record.score, record.move16, record.game_result, record.padding), (0, 0, 0, 2));
        assert!(record.game_ply >= 1);
    }
    assert_eq!(first, std::fs::read(dir.path().join("second/samples.psv")).unwrap());
    let resumed = run("first", true, "1", None);
    assert!(resumed.status.success(), "{}", String::from_utf8_lossy(&resumed.stderr));
    assert_eq!(first, std::fs::read(dir.path().join("first/samples.psv")).unwrap());
    let mismatch = run("first", true, "0.5", None);
    assert!(!mismatch.status.success());
    assert!(String::from_utf8_lossy(&mismatch.stderr).contains("fingerprint mismatch"));

    let failed = run("interrupted", false, "1", Some("before_result:2"));
    assert!(!failed.status.success(), "fault injection did not fail");
    let recovered = run("interrupted", true, "1", None);
    assert!(recovered.status.success(), "{}", String::from_utf8_lossy(&recovered.stderr));
    assert_eq!(first, std::fs::read(dir.path().join("interrupted/samples.psv")).unwrap());
}
