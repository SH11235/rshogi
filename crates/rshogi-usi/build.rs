//! エンジンのソースの版をバイナリに埋め込む (`rshogi-usi --source-version` で出す)。
//!
//! 版は git の commit で、探索・評価の結果を変える feature を `+` 以降に付ける。
//! エンジンを構成する crate に commit されていない変更があれば `-dirty` を付け、
//! git が使えなければ `unknown` にする。CPU 向けの最適化 (target-cpu・SIMD・profile) は版に含めない。

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

/// エンジンのバイナリに入るソース。変更の有無 (`-dirty`) はこの範囲だけで判定する。
const SOURCES: &[&str] = &[
    "crates/rshogi-core",
    "crates/rshogi-book",
    "crates/rshogi-usi",
    "Cargo.toml",
    "Cargo.lock",
];

/// 同じソースでも探索・評価の結果を変える feature (それ以外はモデルの形式の選択か計測用)。
const RESULT_FEATURES: &[&str] = &[
    "use-lazy-evaluate",
    "search-pass-rules",
    "threat-profile-same-class",
    "threat-profile-same-class-major-pawn",
    "threat-profile-step-attacker",
    "threat-profile-full-symdedup",
    "threat-profile-cross-side",
];

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").current_dir(root).args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let root = manifest.join("../..");
    for source in SOURCES {
        println!("cargo:rerun-if-changed={}", root.join(source).display());
    }
    let version = source_version(&root).unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=RSHOGI_SOURCE_VERSION={version}");
}

fn source_version(root: &Path) -> Option<String> {
    // crate を単体で展開したビルド (crates.io 等) では、外側の別の repo の commit を拾わない。
    if !root.join("crates/rshogi-core").is_dir() {
        return None;
    }
    let commit = git(root, &["rev-parse", "HEAD"])?;
    if commit.len() != 40 || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    // commit の移動 (checkout・commit・stage) でも埋め込み直す。
    let mut watched = vec![
        "HEAD".to_owned(),
        "packed-refs".to_owned(),
        "index".to_owned(),
    ];
    watched.extend(git(root, &["symbolic-ref", "-q", "HEAD"]));
    for name in watched {
        if let Some(path) = git(root, &["rev-parse", "--path-format=absolute", "--git-path", &name])
        {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    let mut status = vec!["status", "--porcelain", "--untracked-files=no", "--"];
    status.extend(SOURCES);
    let mut version = commit;
    if !git(root, &status)?.is_empty() {
        version.push_str("-dirty");
    }
    let features: Vec<&str> = RESULT_FEATURES
        .iter()
        .copied()
        .filter(|f| {
            env::var_os(format!("CARGO_FEATURE_{}", f.to_ascii_uppercase().replace('-', "_")))
                .is_some()
        })
        .collect();
    if !features.is_empty() {
        version.push('+');
        version.push_str(&features.join(","));
    }
    Some(version)
}
