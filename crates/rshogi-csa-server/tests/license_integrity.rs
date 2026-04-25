//! csa-server 系クレート群の `license` メタデータが GPL-3.0-only に揃っている
//! ことを CI で機械的に検証する。
//!
//! 本サーバ群は GPL-3.0-only を採用するクリーンルーム実装で、生成物
//! （`crates/rshogi-csa-server*` の rlib / cdylib / バイナリ）にも同ライセンス
//! を引き継がせる必要がある。各 `Cargo.toml` の `license` フィールドが手作業で
//! 書き換わって不揃いになる事故を、ビルド時 / cargo test 時に確実に止める。
//!
//! 本テストは workspace ルートの `Cargo.toml` の `members` を動的に走査するため、
//! 将来 `crates/rshogi-csa-server-*` という名前で新クレートが追加された場合も
//! 自動で検査対象になる（追加 PR が license を忘れるとここで落ちる）。

use std::fs;
use std::path::{Path, PathBuf};

/// csa-server 系クレートのライセンスとして許可される唯一の値。
const REQUIRED_LICENSE: &str = "GPL-3.0-only";

/// workspace 内クレートのうち、本テストが整合性を要求する名前接頭辞。
const SCOPED_PREFIX: &str = "rshogi-csa-server";

/// `CARGO_MANIFEST_DIR` から workspace ルート（= `Cargo.toml` に
/// `[workspace]` セクションを持つ最上位ディレクトリ）を探す。
fn workspace_root() -> PathBuf {
    let mut cur = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        let cargo = cur.join("Cargo.toml");
        if cargo.is_file() {
            let text = fs::read_to_string(&cargo).expect("read Cargo.toml");
            let parsed: toml::Value = toml::from_str(&text).expect("parse Cargo.toml");
            if parsed.get("workspace").is_some() {
                return cur;
            }
        }
        if !cur.pop() {
            panic!("could not find workspace root from CARGO_MANIFEST_DIR");
        }
    }
}

/// 1 クレートの `Cargo.toml` から `package.name` と `package.license` を取り出す。
fn read_package_metadata(cargo_toml: &Path) -> (String, Option<String>) {
    let text = fs::read_to_string(cargo_toml)
        .unwrap_or_else(|e| panic!("read {}: {e}", cargo_toml.display()));
    let parsed: toml::Value =
        toml::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", cargo_toml.display()));
    let pkg = parsed
        .get("package")
        .unwrap_or_else(|| panic!("{}: missing [package]", cargo_toml.display()));
    let name = pkg
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("{}: package.name missing", cargo_toml.display()))
        .to_owned();
    let license = pkg.get("license").and_then(|v| v.as_str()).map(|s| s.to_owned());
    (name, license)
}

/// `crates/rshogi-csa-server*` 系メンバーの Cargo.toml を全て検査し、`license`
/// が `GPL-3.0-only` で一貫していることを保証する。
///
/// 失敗時は具体的な crate 名と検出したライセンス値をエラーメッセージに含めるため、
/// CI ログだけで「どの crate の Cargo.toml を直せばよいか」が分かる。
#[test]
fn csa_server_crates_declare_gpl_3_only_license() {
    let root = workspace_root();
    let workspace_toml_text =
        fs::read_to_string(root.join("Cargo.toml")).expect("read workspace Cargo.toml");
    let workspace_toml: toml::Value =
        toml::from_str(&workspace_toml_text).expect("parse workspace Cargo.toml");
    let members = workspace_toml
        .get("workspace")
        .and_then(|w| w.get("members"))
        .and_then(|m| m.as_array())
        .expect("workspace.members must be an array");

    let mut checked: Vec<String> = Vec::new();
    let mut violations: Vec<String> = Vec::new();

    for member in members {
        let path = member.as_str().expect("member must be a string");
        let cargo_toml = root.join(path).join("Cargo.toml");
        let (name, license) = read_package_metadata(&cargo_toml);
        if !name.starts_with(SCOPED_PREFIX) {
            continue;
        }
        match license.as_deref() {
            Some(REQUIRED_LICENSE) => {
                checked.push(name);
            }
            Some(other) => {
                violations
                    .push(format!("{name}: license = {other:?}, expected {REQUIRED_LICENSE:?}"));
            }
            None => {
                violations
                    .push(format!("{name}: license field missing, expected {REQUIRED_LICENSE:?}"));
            }
        }
    }

    assert!(
        !checked.is_empty(),
        "no `{SCOPED_PREFIX}*` crates found in workspace; the prefix or members layout has changed"
    );
    assert!(
        violations.is_empty(),
        "license integrity violations detected:\n  - {}\n\nFix the listed Cargo.toml files to use license = \"{REQUIRED_LICENSE}\".",
        violations.join("\n  - ")
    );

    // 想定リストとの差分を防ぐため、最低限「3 つの代表 crate」が検査されていることを担保。
    // 新しい csa-server* crate が追加された場合は自動で `checked` に乗るため、本 assert は
    // 既存セットの脱落（リネーム漏れ等）を検知する役割を持つ。
    let expected_at_least: &[&str] = &[
        "rshogi-csa-server",
        "rshogi-csa-server-tcp",
        "rshogi-csa-server-workers",
    ];
    for name in expected_at_least {
        assert!(
            checked.iter().any(|c| c == name),
            "expected csa-server crate `{name}` was not checked (workspace layout changed?)"
        );
    }
}
