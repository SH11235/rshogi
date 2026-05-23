//! Cargo feature 整合性チェック (Edition 軸 ADR Phase 1)
//!
//! 設計: `docs/decisions/2026-05-24-build-edition-flavor-design.md`
//!
//! - `mode-{universal,family,specific}` のうち**ちょうど 1 個**有効化されたとき、
//!   その mode に応じた整合性ルール (size / activation / ft の重複、`ls-arch`
//!   の依存、`nnue-progress-diff` の L0=1536 specific 制約等) を panic で fail-fast。
//! - `mode-*` がゼロのときは Phase 1 互換モード扱いで checks を緩和する。
//!   旧 `layerstacks-*` / `layerstack-only` / `nnue-{psqt,threat,progress-diff}` の
//!   atomic 指定の従来 build がそのまま動く。
//!
//! 純粋ロジックは `build/checks.rs` に `validate_feature_combination` として
//! 切り出してあり、`tests/build_rs_checks.rs` から `include!` して単体テストする。

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
