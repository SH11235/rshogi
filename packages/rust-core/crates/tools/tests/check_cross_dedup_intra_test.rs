use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn tmp_dir(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    p.push(format!("tools_test_cross_intra_{}_{}", name, n));
    std::fs::create_dir_all(&p).expect("create temp dir");
    p
}

fn bin_path_prefer_debug() -> PathBuf {
    if let Ok(p) = std::env::var("CARGO_BIN_EXE_check_cross_dedup") {
        return PathBuf::from(p);
    }
    let mut base = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    base.pop();
    base.pop();
    let mut dbg = base.clone();
    dbg.push("target");
    dbg.push("debug");
    dbg.push("check_cross_dedup");
    if dbg.exists() {
        return dbg;
    }
    let mut rel = base;
    rel.push("target");
    rel.push("release");
    rel.push("check_cross_dedup");
    rel
}

#[test]
fn cross_dedup_reports_intra_with_flag() {
    let dir = tmp_dir("intra");
    let train = dir.join("train.jsonl");
    let valid = dir.join("valid.jsonl");
    let test = dir.join("test.jsonl");
    // train に同一SFENを2回（intra leak）
    fs::write(
        &train,
        "{\"sfen\":\"9/9/9/9/9/9/9/9/9 b - 1\"}\n{\"sfen\":\"9/9/9/9/9/9/9/9/9 b - 1\"}\n",
    )
    .unwrap();
    fs::write(&valid, "{\"sfen\":\"9/9/9/9/9/9/9/9/9 w - 1\"}\n").unwrap();
    fs::write(
        &test,
        "{\"sfen\":\"lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1\"}\n",
    )
    .unwrap();

    let report = dir.join("leak.csv");
    let status = Command::new(bin_path_prefer_debug())
        .args([
            "--train",
            train.to_str().unwrap(),
            "--valid",
            valid.to_str().unwrap(),
            "--test",
            test.to_str().unwrap(),
            "--report",
            report.to_str().unwrap(),
            "--include-intra",
        ])
        .status()
        .expect("run cross-dedup intra");
    assert!(!status.success(), "should fail due to intra duplicates");
    let csv = fs::read_to_string(&report).expect("read report");
    assert!(csv.lines().count() >= 2, "csv should contain header + at least 1 row");
    assert!(
        csv.contains(",train,train,"),
        "csv should indicate intra train duplicates: {}",
        csv
    );
}
