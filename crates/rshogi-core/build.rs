//! Cargo feature 組合せ整合性チェック。
//!
//! 純粋ロジックは `build/checks.rs` の `validate_feature_combination` に切り出して
//! あり、`tests/integration/build_rs_checks.rs` から `include!` して単体テストする。
//! preset edition の命名規則は `docs/build.md` を参照。

use std::env;

include!("build/checks.rs");
include!("build/result_features.rs");

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

    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=build/result_features.rs");
    let manifest = std::fs::read_to_string(
        std::path::Path::new(&env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
            .join("Cargo.toml"),
    )
    .expect("rshogi-core の Cargo.toml を読めません");
    println!(
        "cargo:rustc-env=RSHOGI_CORE_RESULT_FEATURES={}",
        result_features(&manifest, &has_feature).join(",")
    );
}
