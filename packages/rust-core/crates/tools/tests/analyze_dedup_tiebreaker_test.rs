use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn bin_path(name: &str) -> PathBuf {
    let key = format!("CARGO_BIN_EXE_{}", name);
    if let Ok(p) = std::env::var(&key) {
        return PathBuf::from(p);
    }
    // Fallback: try release first (common in CI), then debug
    let mut base = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    base.pop(); // crates/tools
    base.pop(); // crates
    let mut cand = base.clone();
    cand.push("target");
    cand.push("release");
    cand.push(name);
    if cand.exists() {
        return cand;
    }
    let mut cand2 = base;
    cand2.push("target");
    cand2.push("debug");
    cand2.push(name);
    cand2
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
fn test_analyze_dedup_prefers_earlier_file_on_tie() {
    // same SFEN and identical metrics; only tt_hit_rate differs
    let sfen = "tie/9/9/9/9/9/9/9/9 b - 1";
    let rec_first = format!(
        "{{\"sfen\":\"{}\",\"depth\":10,\"seldepth\":10,\"nodes\":100,\"time_ms\":10,\"bound1\":\"Exact\",\"bound2\":\"Exact\",\"tt_hit_rate\":0.110}}",
        sfen
    );
    let rec_second = format!(
        "{{\"sfen\":\"{}\",\"depth\":10,\"seldepth\":10,\"nodes\":100,\"time_ms\":10,\"bound1\":\"Exact\",\"bound2\":\"Exact\",\"tt_hit_rate\":0.890}}",
        sfen
    );
    let in1 = tmp_path("analyze_dedup_file1.jsonl");
    let in2 = tmp_path("analyze_dedup_file2.jsonl");
    write_text(&in1, &make_jsonl(&[&rec_first]));
    write_text(&in2, &make_jsonl(&[&rec_second]));

    let out = Command::new(bin_path("analyze_teaching_quality"))
        .args([
            in1.to_str().unwrap(),
            "--inputs",
            in2.to_str().unwrap(),
            "--dedup-by-sfen",
            "--report",
            "tt",
        ])
        .stdout(Stdio::piped())
        .output()
        .expect("run analyze_teaching_quality");
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    // Expect the earlier file's tt_hit_rate to be selected (samples=1, mean=0.110)
    assert!(stdout.contains("tt: mean=0.110"), "stdout was:\n{}", stdout);
    assert!(stdout.contains("samples=1"), "stdout was:\n{}", stdout);
    let _ = std::fs::remove_file(&in1);
    let _ = std::fs::remove_file(&in2);
}

#[test]
fn test_analyze_dedup_prefers_earlier_line_on_tie() {
    // same SFEN twice in same file; line1 should win over line2 on complete tie
    let sfen = "tie2/9/9/9/9/9/9/9/9 b - 1";
    let rec1 = format!(
        "{{\"sfen\":\"{}\",\"depth\":15,\"seldepth\":15,\"nodes\":100,\"time_ms\":10,\"bound1\":\"Exact\",\"bound2\":\"Exact\",\"tt_hit_rate\":0.200}}",
        sfen
    );
    let rec2 = format!(
        "{{\"sfen\":\"{}\",\"depth\":15,\"seldepth\":15,\"nodes\":100,\"time_ms\":10,\"bound1\":\"Exact\",\"bound2\":\"Exact\",\"tt_hit_rate\":0.800}}",
        sfen
    );
    let in1 = tmp_path("analyze_dedup_samefile.jsonl");
    write_text(&in1, &make_jsonl(&[&rec1, &rec2]));

    let out = Command::new(bin_path("analyze_teaching_quality"))
        .args([in1.to_str().unwrap(), "--dedup-by-sfen", "--report", "tt"])
        .stdout(Stdio::piped())
        .output()
        .expect("run analyze_teaching_quality same file");
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    // earlier line (rec1) should be selected
    assert!(stdout.contains("tt: mean=0.200"), "stdout was:\n{}", stdout);
    assert!(stdout.contains("samples=1"), "stdout was:\n{}", stdout);
    let _ = std::fs::remove_file(&in1);
}
