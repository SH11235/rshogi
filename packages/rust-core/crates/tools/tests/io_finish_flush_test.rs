use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::Command;

fn bin_path(name: &str) -> PathBuf {
    // Prefer cargo-provided env var for binaries
    let key = format!("CARGO_BIN_EXE_{}", name);
    if let Ok(p) = std::env::var(&key) {
        return PathBuf::from(p);
    }
    // Try debug then release as fallback
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop(); // crates/tools
    p.pop(); // crates
    let mut cand = p.clone();
    cand.push("target");
    cand.push("debug");
    cand.push(name);
    if cand.exists() {
        return cand;
    }
    let mut cand2 = p;
    cand2.push("target");
    cand2.push("release");
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

fn tmp_dir(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    p.push(format!("tools_test_dir_{}_{}", name, n));
    std::fs::create_dir_all(&p).expect("create temp dir");
    p
}

fn write_text(path: &PathBuf, s: &str) {
    let mut f = File::create(path).expect("create file");
    f.write_all(s.as_bytes()).expect("write file");
}

fn read_gz_to_string(p: &PathBuf) -> String {
    let f = File::open(p).expect("open gz");
    let mut dec = flate2::read::GzDecoder::new(f);
    let mut s = String::new();
    dec.read_to_string(&mut s).expect("read gz content");
    s
}

#[cfg(feature = "zstd")]
fn read_zst_to_string(p: &PathBuf) -> String {
    let f = File::open(p).expect("open zst");
    let mut dec = zstd::Decoder::new(f).expect("zst decoder");
    let mut s = String::new();
    use std::io::Read as _;
    dec.read_to_string(&mut s).expect("read zst content");
    s
}

#[test]
fn test_merge_gz_output_finish_and_readback() {
    let rec1 = "{\"sfen\":\"s1\",\"depth\":1,\"seldepth\":1,\"nodes\":1,\"time_ms\":1,\"bound1\":\"Exact\",\"bound2\":\"Exact\"}";
    let rec2 = "{\"sfen\":\"s2\",\"depth\":2,\"seldepth\":2,\"nodes\":2,\"time_ms\":2,\"bound1\":\"Lower\",\"bound2\":\"Lower\"}";
    let in1 = tmp_path("merge_gz_in1.jsonl");
    let in2 = tmp_path("merge_gz_in2.jsonl");
    write_text(&in1, &(rec1.to_owned() + "\n"));
    write_text(&in2, &(rec2.to_owned() + "\n"));
    let out_dir = tmp_dir("merge_gz");
    let out = out_dir.join("out.jsonl.gz");

    let status = Command::new(bin_path("merge_annotation_results"))
        .args([
            in1.to_str().unwrap(),
            in2.to_str().unwrap(),
            out.to_str().unwrap(),
        ])
        .status()
        .expect("run merge gz output");
    assert!(status.success());

    let content = read_gz_to_string(&out);
    assert!(content.contains("\"sfen\":\"s1\""));
    assert!(content.contains("\"sfen\":\"s2\""));

    let _ = std::fs::remove_file(&in1);
    let _ = std::fs::remove_file(&in2);
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_dir_all(&out_dir);
}

#[test]
fn test_extract_gz_output_finish_and_readback() {
    let rec = "{\"sfen\":\"9/9/9/9/9/9/9/9/9 b - 1\",\"best2_gap_cp\":1}";
    let inp = tmp_path("extract_gz_in.jsonl");
    write_text(&inp, &(rec.to_owned() + "\n"));
    let out_dir = tmp_dir("extract_gz");
    let out = out_dir.join("out.sfens.gz");
    let status = Command::new(bin_path("extract_flagged_positions"))
        .args([
            inp.to_str().unwrap(),
            out.to_str().unwrap(),
            "--gap-threshold",
            "10",
        ])
        .status()
        .expect("run extract gz output");
    assert!(status.success());
    let content = read_gz_to_string(&out);
    assert!(content.lines().count() == 1, "content was: {}", content);
    assert!(content.contains("sfen "));
    let _ = std::fs::remove_file(&inp);
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_dir_all(&out_dir);
}

#[cfg(feature = "zstd")]
#[test]
fn test_merge_zst_output_finish_and_readback() {
    let rec = "{\"sfen\":\"s\",\"depth\":3,\"seldepth\":3,\"nodes\":3,\"time_ms\":3,\"bound1\":\"Exact\",\"bound2\":\"Exact\"}";
    let in1 = tmp_path("merge_zst_in.jsonl");
    write_text(&in1, &(rec.to_owned() + "\n"));
    let out_dir = tmp_dir("merge_zst");
    let out = out_dir.join("out.jsonl.zst");
    let status = Command::new(bin_path("merge_annotation_results"))
        .args([in1.to_str().unwrap(), out.to_str().unwrap()])
        .status()
        .expect("run merge zst output");
    assert!(status.success());
    let content = read_zst_to_string(&out);
    assert!(content.contains("\"sfen\":\"s\""));
    let _ = std::fs::remove_file(&in1);
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_dir_all(&out_dir);
}

#[cfg(feature = "zstd")]
#[test]
fn test_extract_zst_output_finish_and_readback() {
    let rec = "{\"sfen\":\"t\",\"best2_gap_cp\":5}";
    let inp = tmp_path("extract_zst_in.jsonl");
    write_text(&inp, &(rec.to_owned() + "\n"));
    let out_dir = tmp_dir("extract_zst");
    let out = out_dir.join("out.sfens.zst");
    let status = Command::new(bin_path("extract_flagged_positions"))
        .args([
            inp.to_str().unwrap(),
            out.to_str().unwrap(),
            "--gap-threshold",
            "10",
        ])
        .status()
        .expect("run extract zst output");
    assert!(status.success());
    let content = read_zst_to_string(&out);
    assert!(content.contains("sfen "));
    let _ = std::fs::remove_file(&inp);
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_dir_all(&out_dir);
}
