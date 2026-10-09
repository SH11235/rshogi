//! `test = false` を付けた bin に単体テストが紛れ込んでいないことを確認する。
//!
//! `test = false` の bin は `cargo test` の対象から外れるため、そこへ書いたテストは
//! 一度も実行されない。確認はソースの文字列検索で、テスト属性と `cfg(test)` の代表的な
//! 書き方だけを検出する。

use std::fs;
use std::path::Path;

#[test]
fn bins_without_test_target_contain_no_tests() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest: toml::Table =
        fs::read_to_string(manifest_dir.join("Cargo.toml")).unwrap().parse().unwrap();
    let mut checked = 0;
    for bin in manifest["bin"].as_array().unwrap() {
        if bin.get("test").and_then(toml::Value::as_bool) != Some(false) {
            continue;
        }
        let path = bin["path"].as_str().unwrap();
        let source = fs::read_to_string(manifest_dir.join(path)).unwrap();
        for marker in [
            "#[test]",
            "#[tokio::test",
            "cfg(test)",
            "cfg(all(test",
            "cfg(any(test",
        ] {
            assert!(
                !source.contains(marker),
                "{path} は `test = false` だが `{marker}` を含む。Cargo.toml の `test = false` を外すこと"
            );
        }
        checked += 1;
    }
    assert!(checked > 0, "`test = false` の bin が 1 つも見つからない");
}
