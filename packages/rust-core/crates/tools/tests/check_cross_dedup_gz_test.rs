use assert_cmd::prelude::*;
use flate2::write::GzEncoder;
use flate2::Compression;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

fn tmp_dir(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    p.push(format!("tools_test_cross_gz_{}_{}", name, n));
    std::fs::create_dir_all(&p).expect("create temp dir");
    p
}

fn write_gz(path: &PathBuf, s: &str) {
    let f = fs::File::create(path).expect("create gz");
    let mut enc = GzEncoder::new(f, Compression::default());
    enc.write_all(s.as_bytes()).expect("write gz");
    enc.finish().expect("finish gz");
}

fn bin_path(name: &str) -> PathBuf {
    if let Ok(p) = std::env::var(format!("CARGO_BIN_EXE_{}", name)) {
        return PathBuf::from(p);
    }
    let mut base = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    base.pop();
    base.pop();
    let mut rel = base.clone();
    rel.push("target");
    rel.push("release");
    rel.push(name);
    if rel.exists() {
        return rel;
    }
    let mut dbg = base;
    dbg.push("target");
    dbg.push("debug");
    dbg.push(name);
    dbg
}

#[test]
fn cross_dedup_accepts_gz_inputs() {
    let dir = tmp_dir("gz");
    let train = dir.join("train.jsonl.gz");
    let valid = dir.join("valid.jsonl.gz");
    let test = dir.join("test.jsonl.gz");
    // No cross duplicates
    write_gz(&train, "{\"sfen\":\"9/9/9/9/9/9/9/9/9 b - 1\"}\n");
    write_gz(&valid, "{\"sfen\":\"9/9/9/9/9/9/9/9/9 w - 1\"}\n");
    write_gz(
        &test,
        "{\"sfen\":\"lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1\"}\n",
    );

    let out = dir.join("leak.csv");
    let status = Command::new(bin_path("check_cross_dedup"))
        .args([
            "--train",
            train.to_str().unwrap(),
            "--valid",
            valid.to_str().unwrap(),
            "--test",
            test.to_str().unwrap(),
            "--report",
            out.to_str().unwrap(),
        ])
        .status()
        .expect("run cross-dedup gz");
    assert!(status.success());
}
