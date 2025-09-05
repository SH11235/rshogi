use std::env;
use std::fs::{self, File};
use std::io::{Write, Read};
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;

fn bin_path(name: &str) -> PathBuf {
    // Prefer cargo-provided env var for binaries
    let key = format!("CARGO_BIN_EXE_{}", name);
    if let Ok(p) = env::var(&key) {
        return PathBuf::from(p);
    }
    // Fallback to target/debug
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop(); // crates/tools
    p.pop(); // crates
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

fn read_text(path: &PathBuf) -> String {
    let mut s = String::new();
    File::open(path).unwrap().read_to_string(&mut s).unwrap();
    s
}

fn make_jsonl(records: &[&str]) -> String {
    let mut s = String::new();
    for r in records { s.push_str(r); s.push('\n'); }
    s
}

#[test]
fn test_merge_dedup_exact_first() {
    let sfen = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
    // rec_a: both exact, depth 10
    let rec_a = format!(
        "{{\"sfen\":\"{}\",\"depth\":10,\"seldepth\":12,\"nodes\":1000,\"time_ms\":5,\"bound1\":\"Exact\",\"bound2\":\"Exact\"}}",
        sfen
    );
    // rec_b: not exact, depth 12 (deeper)
    let rec_b = format!(
        "{{\"sfen\":\"{}\",\"depth\":12,\"seldepth\":14,\"nodes\":2000,\"time_ms\":6,\"bound1\":\"Lower\",\"bound2\":\"Lower\"}}",
        sfen
    );
    let in1 = tmp_path("in1_exact.jsonl");
    let in2 = tmp_path("in2_deeper.jsonl");
    write_text(&in1, &make_jsonl(&[&rec_a]));
    write_text(&in2, &make_jsonl(&[&rec_b]));
    let out = tmp_path("out_exact_first.jsonl");

    let status = Command::new(bin_path("merge_annotation_results"))
        .args([in1.to_str().unwrap(), in2.to_str().unwrap(), out.to_str().unwrap(), "--dedup-by-sfen"])
        .status()
        .expect("run merge");
    assert!(status.success());

    let content = read_text(&out);
    assert!(content.contains("\"depth\":10"), "EXACT-first should choose shallower exact over deeper non-exact: {content}");

    let _ = fs::remove_file(&in1);
    let _ = fs::remove_file(&in2);
    let _ = fs::remove_file(&out);
}

#[test]
fn test_merge_prefer_deeper_mode() {
    let sfen = "9/9/9/9/9/9/9/9/9 b - 1";
    // rec_a: both exact, depth 8
    let rec_a = format!(
        "{{\"sfen\":\"{}\",\"depth\":8,\"seldepth\":10,\"nodes\":900,\"time_ms\":5,\"bound1\":\"Exact\",\"bound2\":\"Exact\"}}",
        sfen
    );
    // rec_b: not exact, depth 12
    let rec_b = format!(
        "{{\"sfen\":\"{}\",\"depth\":12,\"seldepth\":14,\"nodes\":2000,\"time_ms\":6,\"bound1\":\"Lower\",\"bound2\":\"Lower\"}}",
        sfen
    );
    let in1 = tmp_path("in1_exact2.jsonl");
    let in2 = tmp_path("in2_deeper2.jsonl");
    write_text(&in1, &make_jsonl(&[&rec_a]));
    write_text(&in2, &make_jsonl(&[&rec_b]));
    let out = tmp_path("out_deeper.jsonl");

    let status = Command::new(bin_path("merge_annotation_results"))
        .args([
            in1.to_str().unwrap(),
            in2.to_str().unwrap(),
            out.to_str().unwrap(),
            "--dedup-by-sfen",
            "--prefer-deeper",
        ])
        .status()
        .expect("run merge");
    assert!(status.success());

    let content = read_text(&out);
    assert!(content.contains("\"depth\":12"), "Depth-first should choose deeper even if not exact: {content}");

    let _ = fs::remove_file(&in1);
    let _ = fs::remove_file(&in2);
    let _ = fs::remove_file(&out);
}

#[test]
fn test_merge_output_sfen_sorted() {
    // Two sfens out of order across two inputs
    let s1 = "b/9/9/9/9/9/9/9/9 b - 1";
    let s2 = "a/9/9/9/9/9/9/9/9 b - 1";
    let rec1 = format!("{{\"sfen\":\"{}\",\"depth\":5,\"bound1\":\"Exact\",\"bound2\":\"Exact\"}}", s1);
    let rec2 = format!("{{\"sfen\":\"{}\",\"depth\":5,\"bound1\":\"Exact\",\"bound2\":\"Exact\"}}", s2);
    let in1 = tmp_path("order_in1.jsonl");
    let in2 = tmp_path("order_in2.jsonl");
    write_text(&in1, &make_jsonl(&[&rec1]));
    write_text(&in2, &make_jsonl(&[&rec2]));
    let out = tmp_path("order_out.jsonl");

    let status = Command::new(bin_path("merge_annotation_results"))
        .args([in1.to_str().unwrap(), in2.to_str().unwrap(), out.to_str().unwrap(), "--dedup-by-sfen"])
        .status()
        .expect("run merge");
    assert!(status.success());
    let content = read_text(&out);
    // Expect s2 ("a/") line first then s1 ("b/")
    let first_idx = content.find("\"sfen\":\"a/").unwrap_or(usize::MAX);
    let second_idx = content.find("\"sfen\":\"b/").unwrap_or(usize::MAX);
    assert!(first_idx < second_idx, "Output not sorted by sfen: {content}");

    let _ = fs::remove_file(&in1);
    let _ = fs::remove_file(&in2);
    let _ = fs::remove_file(&out);
}

#[test]
fn test_merge_non_dedup_concatenation_order() {
    let rec_a = "{\"sfen\":\"x/9/9/9/9/9/9/9/9 b - 1\"}";
    let rec_b = "{\"sfen\":\"y/9/9/9/9/9/9/9/9 b - 1\"}";
    let in1 = tmp_path("concat_in1.jsonl");
    let in2 = tmp_path("concat_in2.jsonl");
    write_text(&in1, &make_jsonl(&[rec_a]));
    write_text(&in2, &make_jsonl(&[rec_b]));
    let out = tmp_path("concat_out.jsonl");

    let status = Command::new(bin_path("merge_annotation_results"))
        .args([in1.to_str().unwrap(), in2.to_str().unwrap(), out.to_str().unwrap()])
        .status()
        .expect("run merge");
    assert!(status.success());
    let content = read_text(&out);
    let lines: Vec<_> = content.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].contains("\"x/"));
    assert!(lines[1].contains("\"y/"));

    let _ = fs::remove_file(&in1);
    let _ = fs::remove_file(&in2);
    let _ = fs::remove_file(&out);
}

#[test]
fn test_extract_flagged_positions_gz() {
    use flate2::write::GzEncoder;
    use flate2::Compression;

    let sfen = "9/9/9/9/9/9/9/9/9 b - 1";
    let rec = format!(
        "{{\"sfen\":\"{}\",\"best2_gap_cp\":10,\"bound1\":\"Exact\",\"bound2\":\"Exact\"}}",
        sfen
    );
    // ensure real .gz extension (ext-based detection)
    let mut input_gz = std::env::temp_dir();
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    input_gz.push(format!("extract_input_{}.jsonl.gz", n));
    {
        let f = File::create(&input_gz).expect("create gz");
        let mut enc = GzEncoder::new(f, Compression::default());
        let payload = make_jsonl(&[&rec]);
        enc.write_all(payload.as_bytes()).unwrap();
        enc.finish().unwrap();
    }
    let out_sfens = tmp_path("extract_out.sfens");
    let status = Command::new(bin_path("extract_flagged_positions"))
        .args([
            input_gz.to_str().unwrap(),
            out_sfens.to_str().unwrap(),
            "--gap-threshold",
            "15", // 10 <= 15 -> include
        ])
        .status()
        .expect("run extract");
    assert!(status.success());
    let content = read_text(&out_sfens);
    assert!(content.contains("sfen "));
    assert!(content.contains(sfen));

    let _ = fs::remove_file(&input_gz);
    let _ = fs::remove_file(&out_sfens);
}

#[test]
fn test_extract_stdout_and_stdin() {
    // Prepare two records, one flagged by gap threshold
    let rec_flag = "{\"sfen\":\"9/9/9/9/9/9/9/9/9 b - 1\",\"best2_gap_cp\":5}";
    let rec_skip = "{\"sfen\":\"a/9/9/9/9/9/9/9/9 b - 1\",\"best2_gap_cp\":100}";
    let payload = format!("{}\n{}\n", rec_flag, rec_skip);

    // STDIN -> file output
    let out_file = tmp_path("extract_stdout_file.sfens");
    let mut child = Command::new(bin_path("extract_flagged_positions"))
        .args(["-", out_file.to_str().unwrap(), "--gap-threshold", "10"]) // 5 <= 10
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .spawn()
        .expect("spawn extract with stdin");
    {
        use std::io::Write as _;
        let stdin = child.stdin.as_mut().unwrap();
        stdin.write_all(payload.as_bytes()).unwrap();
    }
    let status = child.wait().expect("wait child");
    assert!(status.success());
    let content = read_text(&out_file);
    assert!(content.lines().count() == 1);
    assert!(content.contains("sfen "));
    let _ = std::fs::remove_file(&out_file);

    // File input -> STDOUT
    let input_file = tmp_path("extract_stdout_input.jsonl");
    write_text(&input_file, &payload);
    let output = Command::new(bin_path("extract_flagged_positions"))
        .args([input_file.to_str().unwrap(), "-", "--gap-threshold", "10"]) // to stdout
        .output()
        .expect("run extract stdout");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.lines().count() == 1);
    assert!(stdout.contains("sfen "));
    let _ = std::fs::remove_file(&input_file);
}

#[test]
#[cfg(feature = "zstd")]
fn test_zstd_input_merge_and_extract() {
    use zstd::stream::write::Encoder as ZstdEncoder;

    // Prepare input jsonl and compress as .zst for extract
    let rec = "{\"sfen\":\"9/9/9/9/9/9/9/9/9 b - 1\",\"best2_gap_cp\":1}";
    let input_zst = {
        let mut path = std::env::temp_dir();
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        path.push(format!("extract_input_{}.jsonl.zst", n));
        let f = File::create(&path).unwrap();
        let mut enc = ZstdEncoder::new(f, 0).unwrap();
        enc.write_all(format!("{}\n", rec).as_bytes()).unwrap();
        enc.finish().unwrap();
        path
    };
    let out_sfens = tmp_path("extract_out_zst.sfens");
    let status = Command::new(bin_path("extract_flagged_positions"))
        .args([
            input_zst.to_str().unwrap(),
            out_sfens.to_str().unwrap(),
            "--gap-threshold",
            "10",
        ])
        .status()
        .expect("run extract zst");
    assert!(status.success());
    let content = read_text(&out_sfens);
    assert!(content.contains("sfen "));
    let _ = std::fs::remove_file(&input_zst);
    let _ = std::fs::remove_file(&out_sfens);
}
