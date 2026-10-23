//! build.rs のソースの版の材料 (`engine_source_id`・`usi_result_features`) の単体テスト。

include!("../../build/source_version.rs");

use std::path::Path;

fn run(root: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .current_dir(root)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "{args:?}");
}

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn workspace(root: &Path) {
    run(root, &["init", "-q"]);
    for path in [
        "crates/rshogi-core/src/lib.rs",
        "crates/rshogi-book/src/lib.rs",
        "crates/rshogi-usi/src/main.rs",
        "Cargo.toml",
        "Cargo.lock",
        "crates/tools/src/lib.rs",
    ] {
        write(root, path, path);
    }
    run(root, &["add", "."]);
    run(root, &["commit", "-q", "-m", "init"]);
}

#[test]
fn id_ignores_other_crates_and_tracks_engine_sources_and_dirty() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    workspace(root);
    let (first, dirty) = engine_source_id(root).unwrap();
    assert!(!dirty);
    assert_eq!(first.len(), 40);

    write(root, "crates/tools/src/lib.rs", "changed");
    write(root, "README.md", "docs");
    run(root, &["add", "."]);
    run(root, &["commit", "-q", "-m", "tools"]);
    assert_eq!(engine_source_id(root).unwrap(), (first.clone(), false));

    write(root, "crates/rshogi-core/src/lib.rs", "changed");
    assert_eq!(engine_source_id(root).unwrap(), (first.clone(), true));
    write(root, "crates/rshogi-core/src/untracked.rs", "new");
    run(root, &["add", "crates/rshogi-core/src/lib.rs"]);
    assert!(engine_source_id(root).unwrap().1);
    run(root, &["commit", "-q", "-m", "core"]);
    let (second, dirty) = engine_source_id(root).unwrap();
    assert!(!dirty);
    assert_ne!(second, first);
}

#[test]
fn nested_or_missing_checkout_has_no_id() {
    let dir = tempfile::tempdir().unwrap();
    assert!(engine_source_id(dir.path()).is_none());
    workspace(dir.path());
    let nested = dir.path().join("vendor/rshogi");
    for path in ["crates/rshogi-core/src/lib.rs", "Cargo.toml"] {
        write(&nested, path, path);
    }
    assert!(engine_source_id(&nested).is_none());
}

#[test]
fn usi_features_forwarded_to_core_or_neutral_do_not_count() {
    let manifest = r#"
[features]
default = ["edition-universal"]
use-lazy-evaluate = ["rshogi-core/use-lazy-evaluate"]
mimalloc = ["dep:mimalloc"]
allocation-stats = ["rshogi-core/allocation-stats", "dep:tracking-allocator"]
local-only = []
multi = [
  "rshogi-core/deep",
  "dep:other",
]

[[bin]]
name = "x"
"#;
    let all = |_: &str| true;
    assert_eq!(usi_result_features(manifest, &all), ["local-only", "multi"]);
    let real = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap();
    assert!(usi_result_features(&real, &all).is_empty());
}
