use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

fn bin_path(name: &str) -> PathBuf {
    let key = format!("CARGO_BIN_EXE_{}", name);
    if let Ok(p) = std::env::var(&key) {
        return PathBuf::from(p);
    }
    // Fallback locations
    let mut base = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    base.pop(); // crates/tools
    base.pop(); // crates
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

fn tmp_dir(prefix: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    p.push(format!("tools_test_dir_{}_{}", prefix, n));
    std::fs::create_dir_all(&p).expect("create temp dir");
    p
}

fn write_text(path: &PathBuf, s: &str) {
    let mut f = File::create(path).expect("create file");
    f.write_all(s.as_bytes()).expect("write file");
}

fn sha256_hex(path: &PathBuf) -> (String, u64) {
    use sha2::{Digest, Sha256};
    use std::io::Read as _;
    let mut f = File::open(path).expect("open file");
    let mut h = Sha256::new();
    let mut buf = [0u8; 8192];
    let mut total: u64 = 0;
    loop {
        let n = f.read(&mut buf).expect("read file");
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        total += n as u64;
    }
    (hex::encode(h.finalize()), total)
}

#[test]
fn test_expected_mpv_auto_prefers_manifest_aggregated() {
    let dir = tmp_dir("mpv_auto");
    let data = dir.join("data.jsonl");
    // Minimal record; values mostly unused for this test
    let rec = r#"{"sfen":"9/9/9/9/9/9/9/9/9 b - 1","lines":[{"bound":"Exact","score_cp":0}]}"#;
    write_text(&data, &(rec.to_string() + "\n"));
    let (sha, bytes) = sha256_hex(&data);
    let manifest = dir.join("data.manifest.json");
    let m = serde_json::json!({
        "aggregated": { "multipv": 3, "written_lines": 1 },
        "output_sha256": sha,
        "output_bytes": bytes,
    });
    write_text(&manifest, &serde_json::to_string_pretty(&m).unwrap());

    let out = Command::new(bin_path("analyze_teaching_quality"))
        .arg(data.to_str().unwrap())
        .arg("--summary")
        .arg("--manifest-autoload-mode")
        .arg("strict")
        .output()
        .expect("run analyze_teaching_quality");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("expected_mpv=3"), "stdout was: {}", stdout);

    let _ = std::fs::remove_file(&data);
    let _ = std::fs::remove_file(&manifest);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_expected_mpv_cli_overrides_manifest() {
    let dir = tmp_dir("mpv_cli");
    let data = dir.join("data.jsonl");
    let rec = r#"{"sfen":"9/9/9/9/9/9/9/9/9 b - 1","lines":[]}"#;
    write_text(&data, &(rec.to_string() + "\n"));
    let (sha, bytes) = sha256_hex(&data);
    let manifest = dir.join("data.manifest.json");
    let m = serde_json::json!({
        "aggregated": { "multipv": 3, "written_lines": 1 },
        "output_sha256": sha,
        "output_bytes": bytes,
    });
    write_text(&manifest, &serde_json::to_string_pretty(&m).unwrap());

    let out = Command::new(bin_path("analyze_teaching_quality"))
        .arg(data.to_str().unwrap())
        .arg("--summary")
        .arg("--expected-multipv")
        .arg("1")
        .arg("--manifest-autoload-mode")
        .arg("strict")
        .output()
        .expect("run analyze_teaching_quality");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("expected_mpv=1"), "stdout was: {}", stdout);

    let _ = std::fs::remove_file(&data);
    let _ = std::fs::remove_file(&manifest);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_expected_mpv_auto_fallback_when_manifest_missing() {
    // When manifest is absent and --expected-multipv=auto, analyzer should fall back to default (2).
    let dir = tmp_dir("mpv_auto_fallback");
    let data = dir.join("data.jsonl");
    let rec = r#"{"sfen":"9/9/9/9/9/9/9/9/9 b - 1","lines":[]}"#;
    write_text(&data, &(rec.to_string() + "\n"));

    let out = Command::new(bin_path("analyze_teaching_quality"))
        .arg(data.to_str().unwrap())
        .arg("--summary")
        .arg("--manifest-autoload-mode")
        .arg("permissive") // no manifest available; permissive mode should allow fallback to default expected_mpv
        .output()
        .expect("run analyze_teaching_quality");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("expected_mpv=2"),
        "stdout should show fallback expected_mpv=2, was: {}",
        stdout
    );

    let _ = std::fs::remove_file(&data);
    let _ = std::fs::remove_dir_all(&dir);
}
