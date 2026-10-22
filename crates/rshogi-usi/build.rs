//! エンジンのソースの版の材料を埋め込む (`rshogi-usi --source-version` で出す)。
//!
//! - `RSHOGI_SOURCE_ID`: HEAD のエンジンのソース (`ENGINE_SOURCES`) の git の id をまとめた id。
//!   そこに commit されていない変更があれば `-dirty`。git が無い・失敗する、または git の作業木の最上位で
//!   ビルドしていなければ `unknown`
//! - `RSHOGI_USI_RESULT_FEATURES`: rshogi-usi 自身の feature のうち結果を変えうるもの
//!
//! tools・docs だけの commit では id は変わらない。CPU 向けの最適化 (target-cpu・SIMD・profile) も版に含めない。

use std::env;
use std::path::PathBuf;

include!("build/source_version.rs");

fn has_feature(name: &str) -> bool {
    env::var_os(format!("CARGO_FEATURE_{}", name.to_ascii_uppercase().replace('-', "_"))).is_some()
}

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let root = manifest_dir.join("../..");
    println!("cargo:rerun-if-changed=build/source_version.rs");
    // ソースの内容が変わったときだけ id を作り直す。HEAD や index は見ないので、エンジン以外の commit で
    // rshogi-usi を作り直さない。
    for source in ENGINE_SOURCES {
        println!("cargo:rerun-if-changed={}", root.join(source).display());
    }
    let id = match engine_source_id(&root) {
        Some((id, false)) => id,
        Some((id, true)) => {
            // 変更を commit すると内容は変わらず index が変わるので、そのときに -dirty を外す。
            if let Some(index) =
                git(&root, &["rev-parse", "--path-format=absolute", "--git-path", "index"], None)
            {
                println!("cargo:rerun-if-changed={index}");
            }
            format!("{id}-dirty")
        }
        None => "unknown".to_owned(),
    };
    println!("cargo:rustc-env=RSHOGI_SOURCE_ID={id}");
    let manifest = std::fs::read_to_string(manifest_dir.join("Cargo.toml"))
        .expect("rshogi-usi の Cargo.toml を読めません");
    println!(
        "cargo:rustc-env=RSHOGI_USI_RESULT_FEATURES={}",
        usi_result_features(&manifest, &has_feature).join(",")
    );
}
