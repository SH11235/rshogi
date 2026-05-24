//! rshogi 内部 build 自動化タスク。
//!
//! `cargo xtask build --edition <preset>` で rshogi-core の preset edition feature を
//! 有効化した `rshogi-usi` バイナリを build し、`engines/rshogi-usi-<edition>` という
//! 命名規則で `engines/` 下に配置する。設計と命名規則の根拠は ADR
//! `docs/decisions/2026-05-24-build-edition-flavor-design.md` を参照。

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

/// rshogi-core の Cargo.toml を compile-time に取り込み、preset edition の真値とする。
/// 実行時の current_dir に依存せず list / 検証どちらも安定して動かす目的。
const CORE_CARGO_TOML: &str = include_str!("../../rshogi-core/Cargo.toml");

const USI_PACKAGE: &str = "rshogi-usi";
const USI_BINARY: &str = "rshogi-usi";
const DEFAULT_PROFILE: &str = "production";
const DEFAULT_FLAVOR: &str = "default";
const EDITION_PREFIX: &str = "edition-";

#[derive(Parser, Debug)]
#[command(name = "xtask", version, about = "rshogi 内部 build 自動化タスク")]
struct Cli {
    #[command(subcommand)]
    command: SubCmd,
}

#[derive(Subcommand, Debug)]
enum SubCmd {
    /// preset edition を build して `engines/rshogi-usi-<edition>[-flavor-<flavor>]` に配置する。
    Build {
        /// preset edition 名。`edition-` 接頭辞は省略可。
        /// 例: `ls-halfka_hm_merged-1536x16x32-psqt` または `edition-ls-halfka_hm_merged-1536x16x32-psqt`。
        #[arg(long)]
        edition: String,
        /// Flavor 名 (Edition 軸と直交、`default` 以外は binary 名に `-flavor-<flavor>` として付加される)。
        /// Flavor 軸の中身は本 task の対象外で、引数だけ受けて命名規則に従う。
        #[arg(long, default_value = DEFAULT_FLAVOR)]
        flavor: String,
        /// cargo profile (`production` / `release` / `dev` / 任意 custom profile)。
        #[arg(long, default_value = DEFAULT_PROFILE)]
        profile: String,
    },
    /// rshogi-core の Cargo.toml に定義された preset edition (`edition-*`) を列挙する。
    ListEditions,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        SubCmd::Build {
            edition,
            flavor,
            profile,
        } => run_build(&edition, &flavor, &profile),
        SubCmd::ListEditions => run_list_editions(),
    }
}

fn run_list_editions() -> Result<()> {
    for ed in preset_editions(CORE_CARGO_TOML)? {
        println!("{ed}");
    }
    Ok(())
}

fn run_build(edition_arg: &str, flavor: &str, profile: &str) -> Result<()> {
    let edition_feature = normalize_edition(edition_arg);
    let available = preset_editions(CORE_CARGO_TOML)?;
    if !available.iter().any(|e| e == &edition_feature) {
        bail!(
            "unknown preset edition: `{edition_feature}` (利用可能な preset は `cargo xtask list-editions` で確認できます)"
        );
    }
    validate_flavor(flavor)?;

    let workspace_root = workspace_root()?;
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());

    let status = Command::new(&cargo)
        .current_dir(&workspace_root)
        .args([
            "build",
            "--package",
            USI_PACKAGE,
            "--bin",
            USI_BINARY,
            "--profile",
            profile,
            "--no-default-features",
            "--features",
            edition_feature.as_str(),
        ])
        .status()
        .with_context(|| format!("failed to spawn cargo build (cargo={cargo})"))?;
    if !status.success() {
        bail!("cargo build exited with {status}");
    }

    let binary_filename = format!("{USI_BINARY}{}", std::env::consts::EXE_SUFFIX);
    let src = workspace_root.join("target").join(profile_dir(profile)).join(&binary_filename);
    if !src.exists() {
        bail!("expected build artifact not found at {} (profile=`{}`)", src.display(), profile);
    }

    let dst = engines_path(&workspace_root, &edition_feature, flavor)?;
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create engines dir: {}", parent.display()))?;
    }
    std::fs::copy(&src, &dst)
        .with_context(|| format!("copy {} -> {}", src.display(), dst.display()))?;

    println!("Built {}", dst.display());
    Ok(())
}

/// `edition-` 接頭辞を正規化する (与えられていなければ付与)。
fn normalize_edition(input: &str) -> String {
    if input.starts_with(EDITION_PREFIX) {
        input.to_string()
    } else {
        format!("{EDITION_PREFIX}{input}")
    }
}

/// rshogi-core の Cargo.toml `[features]` から `edition-*` 名を抽出してソート返却。
fn preset_editions(cargo_toml: &str) -> Result<Vec<String>> {
    let parsed: toml::Value = toml::from_str(cargo_toml).context("parse rshogi-core Cargo.toml")?;
    let features = parsed
        .get("features")
        .and_then(|v| v.as_table())
        .context("rshogi-core Cargo.toml has no [features] section")?;
    let mut out: Vec<String> = features
        .keys()
        .filter(|name| name.starts_with(EDITION_PREFIX))
        .cloned()
        .collect();
    out.sort();
    Ok(out)
}

/// `engines/rshogi-usi-<edition slug>[-flavor-<flavor>]<EXE_SUFFIX>` を組み立てる。
fn engines_path(workspace_root: &Path, edition_feature: &str, flavor: &str) -> Result<PathBuf> {
    let slug = edition_feature
        .strip_prefix(EDITION_PREFIX)
        .with_context(|| format!("edition feature `{edition_feature}` missing prefix"))?;
    let mut name = format!("{USI_BINARY}-{slug}");
    if flavor != DEFAULT_FLAVOR {
        name.push_str(&format!("-flavor-{flavor}"));
    }
    name.push_str(std::env::consts::EXE_SUFFIX);
    Ok(workspace_root.join("engines").join(name))
}

/// flavor 名は engines/ 直下の単一ファイル名として展開されるため、path traversal
/// やシェルメタ文字を許容しない。`default` 含む `[a-z0-9][a-z0-9_-]*` のみ許可。
fn validate_flavor(flavor: &str) -> Result<()> {
    let mut chars = flavor.chars();
    let first = chars.next().with_context(|| "flavor must not be empty".to_string())?;
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        bail!("flavor `{flavor}` must start with a lowercase ASCII letter or digit");
    }
    if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-') {
        bail!(
            "flavor `{flavor}` must match `[a-z0-9][a-z0-9_-]*` (lowercase ASCII, digits, `_`, `-`)"
        );
    }
    Ok(())
}

/// cargo profile 名から `target/<dir>` のディレクトリ名を返す。
/// `dev` profile だけは `target/debug` に書き出される慣習がある。
fn profile_dir(profile: &str) -> &str {
    match profile {
        "dev" => "debug",
        other => other,
    }
}

fn workspace_root() -> Result<PathBuf> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .context("CARGO_MANIFEST_DIR not set (xtask は cargo 経由で起動する必要があります)")?;
    // crates/xtask/Cargo.toml の親 (crates/xtask) の 2 つ上が workspace root。
    let root = Path::new(&manifest_dir)
        .ancestors()
        .nth(2)
        .with_context(|| format!("failed to resolve workspace root from {manifest_dir}"))?
        .to_path_buf();
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_edition_accepts_both_forms() {
        assert_eq!(normalize_edition("ls"), "edition-ls");
        assert_eq!(normalize_edition("edition-ls"), "edition-ls");
        assert_eq!(
            normalize_edition("ls-halfka_hm_merged-1536x16x32-psqt"),
            "edition-ls-halfka_hm_merged-1536x16x32-psqt"
        );
    }

    #[test]
    fn preset_editions_extracts_known_presets() {
        let editions = preset_editions(CORE_CARGO_TOML).expect("preset editions parse");
        for required in [
            "edition-universal",
            "edition-halfkx",
            "edition-ls",
            "edition-ls-halfka_hm_merged-1536x16x32-psqt",
        ] {
            assert!(
                editions.iter().any(|e| e == required),
                "preset {required} not found in rshogi-core Cargo.toml. found: {editions:?}"
            );
        }
    }

    #[test]
    fn engines_path_default_flavor_omits_suffix() {
        let root = PathBuf::from("/tmp/rshogi");
        let p =
            engines_path(&root, "edition-ls-halfka_hm_merged-1536x16x32-psqt", "default").unwrap();
        let expected = format!(
            "/tmp/rshogi/engines/rshogi-usi-ls-halfka_hm_merged-1536x16x32-psqt{}",
            std::env::consts::EXE_SUFFIX
        );
        assert_eq!(p, PathBuf::from(expected));
    }

    #[test]
    fn engines_path_custom_flavor_appends_suffix() {
        let root = PathBuf::from("/tmp/rshogi");
        let p = engines_path(&root, "edition-ls", "tournament").unwrap();
        let expected = format!(
            "/tmp/rshogi/engines/rshogi-usi-ls-flavor-tournament{}",
            std::env::consts::EXE_SUFFIX
        );
        assert_eq!(p, PathBuf::from(expected));
    }

    #[test]
    fn profile_dir_maps_dev_to_debug() {
        assert_eq!(profile_dir("dev"), "debug");
        assert_eq!(profile_dir("release"), "release");
        assert_eq!(profile_dir("production"), "production");
        assert_eq!(profile_dir("profiling"), "profiling");
    }

    #[test]
    fn validate_flavor_accepts_canonical_values() {
        for ok in ["default", "tournament", "pgo", "tournament-2", "abc_def"] {
            validate_flavor(ok).unwrap_or_else(|e| panic!("flavor `{ok}` should be valid: {e}"));
        }
    }

    #[test]
    fn validate_flavor_rejects_path_traversal_and_shell_meta() {
        for bad in [
            "", "..", "../etc", "foo/bar", "FOO", "foo bar", "foo;rm", "-leading",
        ] {
            assert!(
                validate_flavor(bad).is_err(),
                "flavor `{bad}` should be rejected but was accepted"
            );
        }
    }
}
