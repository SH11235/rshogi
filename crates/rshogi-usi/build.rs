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

#[path = "src/spsa_params.rs"]
mod spsa_params;

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
    let mut result_features = usi_result_features(&manifest, &has_feature);
    if let Some(id) = embed_spsa_params() {
        result_features.push(id);
    }
    println!("cargo:rustc-env=RSHOGI_USI_RESULT_FEATURES={}", result_features.join(","));
    println!("cargo:rustc-env=RSHOGI_ENGINE_NAME={}", engine_name());
}

/// `usi` に返すエンジン名 (`id name`)。`RSHOGI_ENGINE_NAME` で変えられ、指定が無ければ `RAMU`。
/// 評価関数の大きさ違いを同じ GUI に並べるときに区別するためのもので、探索の結果は変えないので版の材料にしない。
fn engine_name() -> String {
    const VAR: &str = "RSHOGI_ENGINE_NAME";
    println!("cargo:rerun-if-env-changed={VAR}");
    let name = env::var(VAR).unwrap_or_default();
    let name = name.trim();
    if name.is_empty() {
        return "RAMU".to_owned();
    }
    // `id name` は 1 行で返すので、改行などの制御文字は受け付けない
    assert!(!name.chars().any(char::is_control), "{VAR} に制御文字は使えません: {name:?}");
    name.to_owned()
}

/// `RSHOGI_EMBED_SPSA_PARAMS` (`.params` の絶対パス) の名前と値を `OUT_DIR/embedded_spsa.rs` に
/// Rust の配列として書く。値をテキストのまま実行ファイルに残さないため、build 時に数値にする。
/// 指定が無ければ空の配列を書く。焼き込んだ値は結果を変えるので、中身の hash を版の材料として返す。
fn embed_spsa_params() -> Option<String> {
    const VAR: &str = "RSHOGI_EMBED_SPSA_PARAMS";
    println!("cargo:rerun-if-env-changed={VAR}");
    println!("cargo:rerun-if-changed=src/spsa_params.rs");
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("embedded_spsa.rs");
    let specified = env::var_os(VAR).is_some_and(|v| !v.is_empty());
    let content = match env::var_os(VAR).filter(|v| !v.is_empty()) {
        Some(path) => {
            let path = PathBuf::from(path);
            // build script の作業ディレクトリは crate の dir なので、相対パスは呼び出し側の意図とずれる
            assert!(path.is_absolute(), "{VAR} は絶対パスで指定してください: {}", path.display());
            println!("cargo:rerun-if-changed={}", path.display());
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{VAR} を読めません ({}): {e}", path.display()));
            text.replace("\r\n", "\n")
        }
        None => String::new(),
    };
    let entries = spsa_params::parse_spsa_params(&content);
    assert!(
        !specified || !entries.is_empty(),
        "{VAR} のファイルに SPSA の値の行がありません"
    );
    let mut code = String::from("&[\n");
    for (name, value) in &entries {
        code.push_str(&format!("    ({name:?}, {value}),\n"));
    }
    code.push_str("]\n");
    std::fs::write(&out, code).expect("embedded_spsa.rs を書けません");
    (!entries.is_empty()).then(|| format!("embedded-spsa-{:016x}", fnv1a64(content.as_bytes())))
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3))
}
