//! Cargo feature 組合せ整合性チェック。
//!
//! 純粋ロジックは `build/checks.rs` の `validate_feature_combination` に切り出して
//! あり、`tests/build_rs_checks.rs` から `include!` して単体テストする。
//! 詳細は `docs/decisions/2026-05-24-build-edition-flavor-design.md` を参照。

use std::env;

include!("build/checks.rs");

fn has_feature(name: &str) -> bool {
    // Cargo は有効化された feature を `CARGO_FEATURE_<UPPER_SNAKE>` 環境変数で
    // build script に渡す (ハイフンは `_` に置換、大文字化)。
    let env_name = format!("CARGO_FEATURE_{}", name.to_ascii_uppercase().replace('-', "_"));
    env::var_os(env_name).is_some()
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=build/checks.rs");

    if let Err(msg) = validate_feature_combination(&has_feature) {
        panic!("rshogi-core build.rs: {msg}");
    }
}
