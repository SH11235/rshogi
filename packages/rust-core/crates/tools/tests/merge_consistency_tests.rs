use assert_cmd::prelude::*;
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::process::Command;

fn tmp_path(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    p.push(format!("{}_{}", n, name));
    p
}

fn write_text(p: &PathBuf, s: &str) {
    fs::write(p, s.as_bytes()).expect("write_text");
}

fn read_text(p: &PathBuf) -> String {
    let mut s = String::new();
    fs::File::open(p).unwrap().read_to_string(&mut s).unwrap();
    s
}

fn make_jsonl(lines: &[&str]) -> String {
    let mut s = String::new();
    for l in lines {
        s.push_str(l);
        s.push('\n');
    }
    s
}

#[test]
fn merge_manifest_written_lines_matches_output() {
    // Prepare small inputs with distinct SFENs so all survive dedup
    let rec1 = "{\"sfen\":\"s1\",\"depth\":1,\"seldepth\":1,\"nodes\":1,\"time_ms\":1,\"bound1\":\"Exact\",\"bound2\":\"Exact\"}";
    let rec2 = "{\"sfen\":\"s2\",\"depth\":2,\"seldepth\":2,\"nodes\":2,\"time_ms\":2,\"bound1\":\"Exact\",\"bound2\":\"Exact\"}";
    let rec3 = "{\"sfen\":\"s3\",\"depth\":3,\"seldepth\":3,\"nodes\":3,\"time_ms\":3,\"bound1\":\"Exact\",\"bound2\":\"Exact\"}";
    let in1 = tmp_path("cons_in1.jsonl");
    let in2 = tmp_path("cons_in2.jsonl");
    write_text(&in1, &make_jsonl(&[rec1, rec2]));
    write_text(&in2, &make_jsonl(&[rec3]));

    let outp = tmp_path("cons_out.jsonl");
    let mani = tmp_path("cons_out.manifest.json");
    let status = Command::cargo_bin("merge_annotation_results")
        .expect("binary exists")
        .args([
            in1.to_str().unwrap(),
            in2.to_str().unwrap(),
            outp.to_str().unwrap(),
            "--dedup-by-sfen",
            "--manifest-out",
            mani.to_str().unwrap(),
        ])
        .status()
        .expect("run merge");
    assert!(status.success());

    // Count output lines
    let out_txt = read_text(&outp);
    let written = out_txt.lines().count();

    // Read manifest aggregated.written_lines
    let mani_txt = read_text(&mani);
    let v: serde_json::Value = serde_json::from_str(&mani_txt).unwrap();
    let w = v
        .get("aggregated")
        .and_then(|a| a.get("written_lines"))
        .and_then(|x| x.as_u64())
        .unwrap_or(0) as usize;
    assert_eq!(w, written, "manifest written_lines must match output rows");

    let _ = fs::remove_file(&in1);
    let _ = fs::remove_file(&in2);
    let _ = fs::remove_file(&outp);
    let _ = fs::remove_file(&mani);
}

#[test]
fn depth_first_prefers_earlier_pass1_on_tie() {
    // Same SFEN three times; metrics equal so file order breaks tie.
    // We tag each record to identify winner.
    let sfen = "l1 b - 1";
    let rec_a = format!("{{\"sfen\":\"{}\",\"tag\":\"A\",\"depth\":5,\"seldepth\":5,\"nodes\":10,\"time_ms\":1,\"bound1\":\"Exact\",\"bound2\":\"Exact\"}}", sfen);
    let rec_b = format!("{{\"sfen\":\"{}\",\"tag\":\"B\",\"depth\":5,\"seldepth\":5,\"nodes\":10,\"time_ms\":1,\"bound1\":\"Exact\",\"bound2\":\"Exact\"}}", sfen);
    let in_a = tmp_path("order_a.jsonl");
    let in_b = tmp_path("order_b.jsonl");
    write_text(&in_a, &make_jsonl(&[&rec_a]));
    write_text(&in_b, &make_jsonl(&[&rec_b]));
    let outp = tmp_path("order_out.jsonl");
    let status = Command::cargo_bin("merge_annotation_results")
        .expect("binary exists")
        .args([
            in_a.to_str().unwrap(),
            in_b.to_str().unwrap(),
            outp.to_str().unwrap(),
            "--dedup-by-sfen",
            "--mode",
            "depth-first",
        ])
        .status()
        .expect("run merge");
    assert!(status.success());
    let out_txt = read_text(&outp);
    assert!(out_txt.contains("\"tag\":\"A\""), "earlier input should win on tie: {out_txt}");
    assert!(!out_txt.contains("\"tag\":\"B\""));
    let _ = fs::remove_file(&in_a);
    let _ = fs::remove_file(&in_b);
    let _ = fs::remove_file(&outp);
}

#[test]
#[cfg(feature = "zstd")]
fn zstd_part_merge_smoke() {
    use std::fs::File;
    use std::io::Write;
    use zstd::stream::write::Encoder as ZstdEncoder;

    let rec1 = "{\"sfen\":\"s1\",\"depth\":1,\"seldepth\":1,\"nodes\":1,\"time_ms\":1,\"bound1\":\"Exact\",\"bound2\":\"Exact\"}";
    let rec2 = "{\"sfen\":\"s2\",\"depth\":1,\"seldepth\":1,\"nodes\":1,\"time_ms\":1,\"bound1\":\"Exact\",\"bound2\":\"Exact\"}";

    // Write files named as pass2.part-*.jsonl.zst
    let p1 = tmp_path("pass2.part-0001.jsonl.zst");
    let p2 = tmp_path("pass2.part-0002.jsonl.zst");
    for (p, rec) in [(&p1, rec1), (&p2, rec2)] {
        let f = File::create(p).unwrap();
        let mut enc = ZstdEncoder::new(f, 0).unwrap();
        enc.write_all(format!("{}\n", rec).as_bytes()).unwrap();
        enc.finish().unwrap();
    }
    let outp = tmp_path("merge_out_part_zst.jsonl");
    let status = Command::cargo_bin("merge_annotation_results")
        .expect("binary exists")
        .args([
            p1.to_str().unwrap(),
            p2.to_str().unwrap(),
            outp.to_str().unwrap(),
            "--dedup-by-sfen",
        ])
        .status()
        .expect("run merge part zstd");
    assert!(status.success());
    let out_txt = read_text(&outp);
    assert!(out_txt.contains("\"sfen\":\"s1\""));
    assert!(out_txt.contains("\"sfen\":\"s2\""));
    let _ = fs::remove_file(&p1);
    let _ = fs::remove_file(&p2);
    let _ = fs::remove_file(&outp);
}
