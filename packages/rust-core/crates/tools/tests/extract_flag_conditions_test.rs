use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn bin_path(name: &str) -> PathBuf {
    let key = format!("CARGO_BIN_EXE_{}", name);
    if let Ok(p) = std::env::var(&key) {
        return PathBuf::from(p);
    }
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.pop();
    p.push("target");
    p.push("debug");
    p.push(name);
    p
}

fn tmp_path(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    p.push(format!("tools_test_{}_{}", name, n));
    p
}

fn write_text(path: &PathBuf, s: &str) {
    let mut f = File::create(path).expect("create file");
    f.write_all(s.as_bytes()).expect("write file");
}

fn make_jsonl(records: &[&str]) -> String {
    let mut s = String::new();
    for r in records {
        s.push_str(r);
        s.push('\n');
    }
    s
}

#[test]
fn test_extract_non_exact_fallback_lines_bound() {
    // Record has no bound1/bound2, but lines[0].bound = Lower (non-exact)
    let rec = r#"{"sfen":"fallback/lines b - 1","lines":[{"bound":"Lower"},{"bound":"Exact"}],"best2_gap_cp":100}"#;
    let inp = tmp_path("extract_lines_fallback.jsonl");
    write_text(&inp, &make_jsonl(&[rec]));
    let out = Command::new(bin_path("extract_flagged_positions"))
        .args([inp.to_str().unwrap(), "-", "--include-non-exact"])
        .stdout(Stdio::piped())
        .output()
        .expect("run extract with lines bound fallback");
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("sfen "));
    let _ = std::fs::remove_file(&inp);
}

#[test]
fn test_extract_record_level_mate_boundary() {
    let rec = r#"{"sfen":"mateflag/lines b - 1","mate_boundary":true}"#;
    let inp = tmp_path("extract_mateflag.jsonl");
    write_text(&inp, &make_jsonl(&[rec]));
    let out = Command::new(bin_path("extract_flagged_positions"))
        .args([inp.to_str().unwrap(), "-", "--include-mate-boundary"])
        .stdout(Stdio::piped())
        .output()
        .expect("run extract with record-level mate_boundary");
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("sfen "));
    let _ = std::fs::remove_file(&inp);
}
