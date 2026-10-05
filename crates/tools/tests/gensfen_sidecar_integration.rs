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
