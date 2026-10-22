//! `--source-version` が埋め込んだソースの版を 1 行で出して終わることを確かめる。

use std::process::Command;

#[test]
fn source_version_prints_one_line_and_exits() {
    let out = Command::new(assert_cmd::cargo::cargo_bin!("rshogi-usi"))
        .arg("--source-version")
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    let version = stdout.strip_prefix("source-version ").unwrap().strip_suffix('\n').unwrap();
    assert!(!version.contains('\n'));
    let commit = version.split(['-', '+']).next().unwrap();
    assert!(
        version == "unknown"
            || (commit.len() == 40 && commit.bytes().all(|b| b.is_ascii_hexdigit())),
        "{version}"
    );
}
