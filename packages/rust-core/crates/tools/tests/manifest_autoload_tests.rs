use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::json;
use tempfile::TempDir;
use tools::common::manifest::{resolve_manifest, AutoloadMode};

fn write(path: &Path, s: &str) {
    fs::write(path, s).expect("write");
}

fn sha256_file(path: &Path) -> (String, u64) {
    use sha2::{Digest, Sha256};
    let mut f = fs::File::open(path).unwrap();
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut total: u64 = 0;
    loop {
        let n = std::io::Read::read(&mut f, &mut buf).unwrap();
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    (hex::encode(hasher.finalize()), total)
}

#[test]
fn autoload_non_split_strict_verified() {
    let td = TempDir::new().unwrap();
    let dir = td.path();
    let inp = dir.join("x.jsonl");
    write(&inp, "{\"sfen\":\"s\"}\n");
    let (h, b) = sha256_file(&inp);
    let man = dir.join("x.manifest.json");
    let m = json!({"output_sha256": h, "output_bytes": b});
    write(&man, &serde_json::to_string_pretty(&m).unwrap());

    let res = resolve_manifest(&inp, AutoloadMode::Strict).unwrap().unwrap();
    assert!(res.verified);
    assert_eq!(res.scope, "file");
}

#[test]
fn autoload_part_precedence_strict() {
    let td = TempDir::new().unwrap();
    let dir = td.path();
    let inp = dir.join("out.part-0002.jsonl");
    write(&inp, "{\"sfen\":\"s\"}\n");
    let (h, b) = sha256_file(&inp);
    let part_man = dir.join("out.part-0002.manifest.json");
    let base_man = dir.join("out.manifest.json");
    let mp = json!({"output_sha256": h, "output_bytes": b});
    write(&part_man, &serde_json::to_string_pretty(&mp).unwrap());
    // Base manifest also present but should be ignored due to part precedence
    write(&base_man, "{\"note\":\"base present\"}");

    let res = resolve_manifest(&inp, AutoloadMode::Strict).unwrap().unwrap();
    assert!(res.verified);
    assert_eq!(res.scope, "part");
}

#[test]
fn autoload_parent_fallback_permissive_only() {
    let td = TempDir::new().unwrap();
    let dir = td.path();
    let inp = dir.join("o.part-0001.jsonl");
    write(&inp, "{\"sfen\":\"x\"}\n");
    // Only base manifest without sha/bytes -> Undecidable
    let base = dir.join("o.manifest.json");
    write(&base, "{\"note\":\"v1 manifest\"}");

    let none_strict = resolve_manifest(&inp, AutoloadMode::Strict).unwrap();
    assert!(none_strict.is_none());
    let some_perm = resolve_manifest(&inp, AutoloadMode::Permissive).unwrap();
    assert!(some_perm.is_some());
    let r = some_perm.unwrap();
    assert!(!r.verified);
    assert_eq!(r.scope, "file");
}

#[test]
fn autoload_mismatch_returns_none() {
    let td = TempDir::new().unwrap();
    let dir = td.path();
    let inp = dir.join("y.jsonl");
    write(&inp, "{\"sfen\":\"y\"}\n");
    // Wrong sha/bytes -> Mismatch
    let man = dir.join("y.manifest.json");
    write(
        &man,
        &serde_json::to_string_pretty(&json!({
            "output_sha256": "deadbeef",
            "output_bytes": 99999
        }))
        .unwrap(),
    );
    assert!(resolve_manifest(&inp, AutoloadMode::Strict).unwrap().is_none());
    assert!(resolve_manifest(&inp, AutoloadMode::Permissive).unwrap().is_none());
}

#[test]
fn autoload_old_manifest_permissive_accepts() {
    let td = TempDir::new().unwrap();
    let dir = td.path();
    let inp = dir.join("z.jsonl");
    write(&inp, "{\"sfen\":\"z\"}\n");
    let man = dir.join("z.manifest.json");
    write(&man, "{\"tool\":\"legacy\"}");
    assert!(resolve_manifest(&inp, AutoloadMode::Strict).unwrap().is_none());
    let some = resolve_manifest(&inp, AutoloadMode::Permissive).unwrap();
    assert!(some.is_some());
    assert!(!some.unwrap().verified);
}

#[test]
fn autoload_coexistence_precedence_differs_by_mode() {
    let td = TempDir::new().unwrap();
    let dir = td.path();
    let inp = dir.join("out.part-0003.jsonl");
    write(&inp, "{\"sfen\":\"c\"}\n");
    let (h, b) = sha256_file(&inp);
    // Per-part manifest exists but undecidable (no sha/bytes)
    write(&dir.join("out.part-0003.manifest.json"), "{\"note\":\"undecidable\"}");
    // Base manifest verified
    write(
        &dir.join("out.manifest.json"),
        &serde_json::to_string_pretty(&json!({
            "output_sha256": h,
            "output_bytes": b
        }))
        .unwrap(),
    );
    // strict: should choose base (verified)
    let r_strict = resolve_manifest(&inp, AutoloadMode::Strict).unwrap().unwrap();
    assert!(r_strict.verified);
    assert_eq!(r_strict.scope, "file");
    // permissive: should accept first undecidable (per-part)
    let r_perm = resolve_manifest(&inp, AutoloadMode::Permissive).unwrap().unwrap();
    assert!(!r_perm.verified);
    assert_eq!(r_perm.scope, "part");
}

#[test]
fn autoload_gz_extension_strict_verified() {
    let td = TempDir::new().unwrap();
    let dir = td.path();
    let inp = dir.join("a.jsonl.gz");
    // write any bytes; verify works against gz file bytes (no need to be valid gzip)
    let gz_bytes: Vec<u8> = vec![
        0x1F, 0x8B, 0x08, 0x00, b'f', b'a', b'k', b'e', b'g', b'z', b'\n',
    ];
    fs::write(&inp, &gz_bytes).expect("write gz bytes");
    let (h, b) = sha256_file(&inp);
    // manifest path uses stem without .gz
    let man = dir.join("a.manifest.json");
    write(
        &man,
        &serde_json::to_string_pretty(&json!({
            "output_sha256": h,
            "output_bytes": b
        }))
        .unwrap(),
    );
    let res = resolve_manifest(&inp, AutoloadMode::Strict).unwrap().unwrap();
    assert!(res.verified);
    assert_eq!(res.scope, "file");
}
