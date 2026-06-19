//! rshogi 内部 build 自動化タスク。
//!
//! `cargo xtask build --edition <preset>[,<preset>...]` で rshogi-core の preset edition
//! feature を有効化した `rshogi-usi` バイナリを build し、`engines/rshogi-usi-<edition>`
//! という命名規則で `engines/` 下に配置する。各 binary には同階層に `<binary>.meta.toml`
//! を書き出し、後から commit / profile / built_at を追跡できるようにする。
//! 設計と命名規則の根拠は ADR `docs/decisions/2026-05-24-build-edition-flavor-design.md`
//! を参照。

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use anyhow::{Context, Result, bail};
use chrono::Local;
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};

/// rshogi-core の Cargo.toml を compile-time に取り込み、preset edition の真値とする。
/// 実行時の current_dir に依存せず list / 検証どちらも安定して動かす目的。
const CORE_CARGO_TOML: &str = include_str!("../../rshogi-core/Cargo.toml");

const USI_PACKAGE: &str = "rshogi-usi";
const USI_BINARY: &str = "rshogi-usi";
const DEFAULT_PROFILE: &str = "production";
const EDITION_PREFIX: &str = "edition-";
const MANIFEST_SUFFIX: &str = ".meta.toml";
const MANIFEST_SCHEMA_VERSION: u32 = 1;

#[derive(Parser, Debug)]
#[command(name = "xtask", version, about = "rshogi 内部 build 自動化タスク")]
struct Cli {
    #[command(subcommand)]
    command: SubCmd,
}

#[derive(Subcommand, Debug)]
enum SubCmd {
    /// preset edition を build して `engines/rshogi-usi-<edition>` に配置する。
    /// 同階層に `<binary>.meta.toml` も書き出し、後追い可能なメタ情報を残す。
    Build {
        /// preset edition 名 (`edition-` 接頭辞省略可、複数指定可)。
        /// カンマ区切り (`a,b,c`) または `--edition` 複数回いずれも受け付ける。
        #[arg(long, value_delimiter = ',', num_args = 1.., conflicts_with = "all_presets")]
        edition: Vec<String>,
        /// `cargo xtask list-editions` の全 preset を順次 build する。
        /// `--edition` と排他。
        #[arg(long, conflicts_with = "edition")]
        all_presets: bool,
        /// cargo profile (`production` / `release` / `dev` / 任意 custom profile)。
        #[arg(long, default_value = DEFAULT_PROFILE)]
        profile: String,
    },
    /// rshogi-core の Cargo.toml に定義された preset edition (`edition-*`) を列挙する。
    ListEditions,
    /// `engines/` 下に配置された binary を manifest と合わせて整形表示する。
    /// manifest が無い旧 binary は `(no manifest)` 表示。
    ListBinaries,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        SubCmd::Build {
            edition,
            all_presets,
            profile,
        } => run_build(edition, all_presets, &profile),
        SubCmd::ListEditions => run_list_editions(),
        SubCmd::ListBinaries => run_list_binaries(),
    }
}

fn run_list_editions() -> Result<()> {
    for ed in preset_editions(CORE_CARGO_TOML)? {
        println!("{ed}");
    }
    Ok(())
}

fn run_build(edition_args: Vec<String>, all_presets: bool, profile: &str) -> Result<()> {
    let available = preset_editions(CORE_CARGO_TOML)?;
    let editions: Vec<String> = if all_presets {
        if !edition_args.is_empty() {
            bail!("`--edition` and `--all-presets` are mutually exclusive");
        }
        available.clone()
    } else {
        if edition_args.is_empty() {
            bail!("at least one `--edition <name>` or `--all-presets` is required");
        }
        edition_args.iter().map(|raw| normalize_edition(raw)).collect()
    };
    for ed in &editions {
        if !available.iter().any(|a| a == ed) {
            bail!(
                "unknown preset edition: `{ed}` (利用可能な preset は `cargo xtask list-editions` で確認できます)"
            );
        }
    }

    let workspace_root = workspace_root()?;
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let target_dir = resolve_target_dir(&workspace_root);
    let rustc_version = warn_on_err("rustc --version", rustc_version(&cargo));
    let commit = warn_on_err("git rev-parse HEAD", git_commit(&workspace_root));
    let commit_dirty = match git_is_dirty(&workspace_root) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("warning: git status check failed ({e:#}); recording commit_dirty=false");
            false
        }
    };

    let ctx = BuildContext {
        cargo: &cargo,
        workspace_root: &workspace_root,
        target_dir: &target_dir,
        profile,
        commit: commit.as_deref(),
        commit_dirty,
        rustc_version: rustc_version.as_deref(),
    };
    for edition_feature in &editions {
        build_one(&ctx, edition_feature)?;
    }

    Ok(())
}

/// 取得失敗時に warning を `eprintln!` で出力し `None` を返す共通ヘルパ。
/// manifest の "unknown" 値が黙って書き込まれないようにする。
fn warn_on_err<T>(what: &str, result: Result<T>) -> Option<T> {
    match result {
        Ok(v) => Some(v),
        Err(e) => {
            eprintln!("warning: {what} failed ({e:#}); recording `unknown` in manifest");
            None
        }
    }
}

struct BuildContext<'a> {
    cargo: &'a str,
    workspace_root: &'a Path,
    target_dir: &'a Path,
    profile: &'a str,
    commit: Option<&'a str>,
    commit_dirty: bool,
    rustc_version: Option<&'a str>,
}

fn build_one(ctx: &BuildContext, edition_feature: &str) -> Result<()> {
    println!("==> Building {edition_feature} (profile={})", ctx.profile);
    let status = Command::new(ctx.cargo)
        .current_dir(ctx.workspace_root)
        .args([
            "build",
            "--package",
            USI_PACKAGE,
            "--bin",
            USI_BINARY,
            "--profile",
            ctx.profile,
            "--no-default-features",
            "--features",
            edition_feature,
        ])
        .status()
        .with_context(|| format!("failed to spawn cargo build (cargo={})", ctx.cargo))?;
    if !status.success() {
        bail!("cargo build exited with {status} for edition `{edition_feature}`");
    }

    let binary_filename = format!("{USI_BINARY}{}", std::env::consts::EXE_SUFFIX);
    let src = ctx.target_dir.join(profile_dir(ctx.profile)).join(&binary_filename);
    if !src.exists() {
        bail!(
            "expected build artifact not found at {} (profile=`{}`, target_dir=`{}`)",
            src.display(),
            ctx.profile,
            ctx.target_dir.display()
        );
    }

    let dst = engines_path(ctx.workspace_root, edition_feature)?;
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create engines dir: {}", parent.display()))?;
    }
    std::fs::copy(&src, &dst)
        .with_context(|| format!("copy {} -> {}", src.display(), dst.display()))?;

    let manifest = Manifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        edition: edition_feature.to_string(),
        profile: ctx.profile.to_string(),
        commit: ctx.commit.unwrap_or("unknown").to_string(),
        commit_dirty: ctx.commit_dirty,
        built_at: chrono_now_local_rfc3339(),
        rustc: ctx.rustc_version.unwrap_or("unknown").to_string(),
        binary: dst.file_name().and_then(|n| n.to_str()).unwrap_or(USI_BINARY).to_string(),
    };
    let manifest_path = manifest_path_for(&dst);
    write_manifest(&manifest, &manifest_path)?;

    println!("    -> {} ({})", dst.display(), manifest_path.display());
    Ok(())
}

fn run_list_binaries() -> Result<()> {
    let workspace_root = workspace_root()?;
    let current_commit = git_commit(&workspace_root).ok();
    let engines_dir = workspace_root.join("engines");
    if !engines_dir.is_dir() {
        println!("engines directory not found: {}", engines_dir.display());
        return Ok(());
    }

    let mut rows: Vec<BinaryRow> = Vec::new();
    for entry in std::fs::read_dir(&engines_dir)
        .with_context(|| format!("read_dir {}", engines_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        if !is_engine_binary(&path, &name) {
            continue;
        }
        let metadata = entry.metadata().ok();
        let size = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
        let mtime = metadata.as_ref().and_then(|m| m.modified().ok());
        let manifest_path = manifest_path_for(&path);
        let manifest_status = read_manifest_status(&manifest_path);
        rows.push(BinaryRow {
            name,
            size,
            mtime,
            manifest_status,
        });
    }

    rows.sort_by_key(|r| std::cmp::Reverse(r.mtime));

    if rows.is_empty() {
        println!("no engine binaries found under {}", engines_dir.display());
        return Ok(());
    }

    let now = SystemTime::now();
    let header = ("BINARY", "EDITION", "PROFILE", "COMMIT", "AGE", "SIZE", "STATUS");
    let mut formatted: Vec<[String; 7]> = vec![[
        header.0.to_string(),
        header.1.to_string(),
        header.2.to_string(),
        header.3.to_string(),
        header.4.to_string(),
        header.5.to_string(),
        header.6.to_string(),
    ]];
    for row in &rows {
        let (edition, profile, commit, status_flag) = match &row.manifest_status {
            ManifestStatus::Loaded(m) => {
                let short_commit = short_commit(&m.commit);
                let status = if let Some(cur) = current_commit.as_deref() {
                    if cur == m.commit {
                        if m.commit_dirty { "dirty" } else { "current" }
                    } else {
                        "stale"
                    }
                } else {
                    "-"
                };
                (m.edition.clone(), m.profile.clone(), short_commit, status.to_string())
            }
            ManifestStatus::Missing => ("-".into(), "-".into(), "-".into(), "(no manifest)".into()),
            ManifestStatus::Broken => {
                ("-".into(), "-".into(), "-".into(), "(manifest broken)".into())
            }
        };
        formatted.push([
            row.name.clone(),
            edition,
            profile,
            commit,
            format_age(now, row.mtime),
            format_size(row.size),
            status_flag,
        ]);
    }
    print_table(&formatted);
    Ok(())
}

struct BinaryRow {
    name: String,
    size: u64,
    mtime: Option<SystemTime>,
    manifest_status: ManifestStatus,
}

enum ManifestStatus {
    /// `<binary>.meta.toml` 自体が存在しない (xtask 経由 build 前の旧 binary 等)。
    Missing,
    /// `<binary>.meta.toml` は存在するが parse 失敗。
    /// 読み込み時に `eprintln!` で warning を出している (silent fail 回避)。
    Broken,
    Loaded(Manifest),
}

fn read_manifest_status(path: &Path) -> ManifestStatus {
    if !path.exists() {
        return ManifestStatus::Missing;
    }
    match read_manifest(path) {
        Ok(m) => ManifestStatus::Loaded(m),
        Err(e) => {
            eprintln!("warning: failed to read manifest {} ({e:#})", path.display());
            ManifestStatus::Broken
        }
    }
}

// serde 既定で未知フィールドは silently ignore されるため、`flavor` 等の旧 v1
// manifest フィールドが残った既存 binary も parse 失敗せずに読める (backward-compat)。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Manifest {
    schema_version: u32,
    edition: String,
    profile: String,
    commit: String,
    commit_dirty: bool,
    built_at: String,
    rustc: String,
    binary: String,
}

fn write_manifest(manifest: &Manifest, path: &Path) -> Result<()> {
    let toml_text = toml::to_string_pretty(manifest)
        .with_context(|| format!("serialize manifest for {}", path.display()))?;
    std::fs::write(path, toml_text)
        .with_context(|| format!("write manifest {}", path.display()))?;
    Ok(())
}

fn read_manifest(path: &Path) -> Result<Manifest> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read manifest {}", path.display()))?;
    let manifest: Manifest =
        toml::from_str(&text).with_context(|| format!("parse manifest {}", path.display()))?;
    Ok(manifest)
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

/// `engines/rshogi-usi-<edition slug><EXE_SUFFIX>` を組み立てる。
fn engines_path(workspace_root: &Path, edition_feature: &str) -> Result<PathBuf> {
    let slug = edition_feature
        .strip_prefix(EDITION_PREFIX)
        .with_context(|| format!("edition feature `{edition_feature}` missing prefix"))?;
    let mut name = format!("{USI_BINARY}-{slug}");
    name.push_str(std::env::consts::EXE_SUFFIX);
    Ok(workspace_root.join("engines").join(name))
}

fn manifest_path_for(binary_path: &Path) -> PathBuf {
    let mut s = binary_path.as_os_str().to_owned();
    s.push(MANIFEST_SUFFIX);
    PathBuf::from(s)
}

/// cargo profile 名から `target/<dir>` のディレクトリ名を返す。
/// `dev` profile だけは `target/debug` に書き出される慣習がある。
fn profile_dir(profile: &str) -> &str {
    match profile {
        "dev" => "debug",
        other => other,
    }
}

/// 環境変数 `CARGO_TARGET_DIR` が設定されていればそれを、無ければ
/// `<workspace_root>/target` を返す。`.cargo/config.toml` の `[build] target-dir`
/// は本 tool では未対応 (rshogi リポでは未使用)。
///
/// 相対 path が指定された場合、cargo は `current_dir(workspace_root)` で起動される
/// ため workspace_root 相対で artifact を置く。xtask 側もそれに合わせて
/// `workspace_root.join(rel)` に正規化し、xtask 起動時の cwd 依存を排除する。
/// 空文字は未設定として扱う。
fn resolve_target_dir(workspace_root: &Path) -> PathBuf {
    let raw = std::env::var_os("CARGO_TARGET_DIR").filter(|s| !s.is_empty());
    match raw {
        Some(val) => {
            let p = PathBuf::from(val);
            if p.is_absolute() {
                p
            } else {
                workspace_root.join(p)
            }
        }
        None => workspace_root.join("target"),
    }
}

fn workspace_root() -> Result<PathBuf> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .context("CARGO_MANIFEST_DIR not set (xtask は cargo 経由で起動する必要があります)")?;
    // crates/xtask/Cargo.toml の親 (crates/xtask) の 2 つ上が workspace root。
    // crate の物理 path を変えた場合はこの ancestors().nth(2) も同期する必要がある。
    let root = Path::new(&manifest_dir)
        .ancestors()
        .nth(2)
        .with_context(|| format!("failed to resolve workspace root from {manifest_dir}"))?
        .to_path_buf();
    Ok(root)
}

fn rustc_version(cargo: &str) -> Result<String> {
    let mut path = PathBuf::from(cargo);
    path.set_file_name("rustc");
    let rustc_bin = if path.exists() {
        path.into_os_string()
    } else {
        OsStr::new("rustc").to_owned()
    };
    let out = Command::new(&rustc_bin)
        .arg("--version")
        .output()
        .with_context(|| format!("spawn {}", rustc_bin.to_string_lossy()))?;
    if !out.status.success() {
        bail!("rustc --version exited with {}", out.status);
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn git_commit(workspace_root: &Path) -> Result<String> {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(workspace_root)
        .output()
        .context("spawn git rev-parse")?;
    if !out.status.success() {
        bail!("git rev-parse exited with {}", out.status);
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn git_is_dirty(workspace_root: &Path) -> Result<bool> {
    let out = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(workspace_root)
        .output()
        .context("spawn git status")?;
    if !out.status.success() {
        bail!("git status exited with {}", out.status);
    }
    Ok(!out.stdout.is_empty())
}

fn short_commit(commit: &str) -> String {
    commit.chars().take(8).collect()
}

fn chrono_now_local_rfc3339() -> String {
    Local::now().to_rfc3339()
}

fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

fn format_age(now: SystemTime, mtime: Option<SystemTime>) -> String {
    let Some(mtime) = mtime else {
        return "-".to_string();
    };
    let Ok(elapsed) = now.duration_since(mtime) else {
        return "future".to_string();
    };
    let secs = elapsed.as_secs();
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    if secs < MINUTE {
        format!("{secs}s")
    } else if secs < HOUR {
        format!("{}m", secs / MINUTE)
    } else if secs < DAY {
        format!("{}h", secs / HOUR)
    } else {
        format!("{}d", secs / DAY)
    }
}

fn is_engine_binary(path: &Path, name: &str) -> bool {
    if !path.is_file() {
        return false;
    }
    if !name.starts_with(USI_BINARY) {
        return false;
    }
    if name.ends_with(MANIFEST_SUFFIX) {
        return false;
    }
    // README / .gitkeep / .bak* 等を除外
    if name.starts_with('.') || name.contains(".bak") || name.ends_with(".md") {
        return false;
    }
    true
}

fn print_table(rows: &[[String; 7]]) {
    let mut widths = [0usize; 7];
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }
    for row in rows {
        let line: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(i, cell)| format!("{:<width$}", cell, width = widths[i]))
            .collect();
        println!("{}", line.join("  "));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_edition_accepts_both_forms() {
        assert_eq!(normalize_edition("layerstacks"), "edition-layerstacks");
        assert_eq!(normalize_edition("edition-layerstacks"), "edition-layerstacks");
        assert_eq!(
            normalize_edition("layerstacks-halfka_hm_merged-1536x16x32-psqt"),
            "edition-layerstacks-halfka_hm_merged-1536x16x32-psqt"
        );
    }

    #[test]
    fn preset_editions_extracts_known_presets() {
        let editions = preset_editions(CORE_CARGO_TOML).expect("preset editions parse");
        for required in [
            "edition-universal",
            "edition-halfkx",
            "edition-layerstacks",
            "edition-layerstacks-halfka_hm_merged-1536x16x32-psqt",
        ] {
            assert!(
                editions.iter().any(|e| e == required),
                "preset {required} not found in rshogi-core Cargo.toml. found: {editions:?}"
            );
        }
    }

    #[test]
    fn engines_path_strips_edition_prefix() {
        let root = PathBuf::from("/tmp/rshogi");
        let p =
            engines_path(&root, "edition-layerstacks-halfka_hm_merged-1536x16x32-psqt").unwrap();
        let expected = format!(
            "/tmp/rshogi/engines/rshogi-usi-layerstacks-halfka_hm_merged-1536x16x32-psqt{}",
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
    fn resolve_target_dir_respects_env_var() {
        let key = "CARGO_TARGET_DIR";
        let prev = std::env::var_os(key);
        let workspace = Path::new("/tmp/rshogi");

        // absolute path: そのまま使う
        let abs = PathBuf::from("/tmp/rshogi-xtask-test-target-dir-probe");
        // SAFETY: 本プロセス内のみで完結する env 書き換え。restore は下記で実施する。
        unsafe { std::env::set_var(key, &abs) };
        assert_eq!(resolve_target_dir(workspace), abs);

        // relative path: workspace_root 相対に正規化する (cargo の `current_dir(workspace_root)`
        // 起動と整合させる)
        // SAFETY: 同上。
        unsafe { std::env::set_var(key, "target-alt") };
        assert_eq!(resolve_target_dir(workspace), PathBuf::from("/tmp/rshogi/target-alt"));

        // 空文字: 未設定扱い
        // SAFETY: 同上。
        unsafe { std::env::set_var(key, "") };
        assert_eq!(resolve_target_dir(workspace), PathBuf::from("/tmp/rshogi/target"));

        // restore
        // SAFETY: 同上。
        unsafe {
            match prev {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }

    #[test]
    fn read_manifest_status_distinguishes_missing_and_broken() {
        let tmp = std::env::temp_dir().join(format!("xtask-manifest-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();

        let missing = tmp.join("does-not-exist.meta.toml");
        assert!(matches!(read_manifest_status(&missing), ManifestStatus::Missing));

        let broken = tmp.join("broken.meta.toml");
        std::fs::write(&broken, "this is not valid toml = = =").unwrap();
        assert!(matches!(read_manifest_status(&broken), ManifestStatus::Broken));

        let good = tmp.join("good.meta.toml");
        let m = Manifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            edition: "edition-universal".into(),
            profile: "production".into(),
            commit: "deadbeef".into(),
            commit_dirty: false,
            built_at: "2026-05-24T22:30:00+09:00".into(),
            rustc: "rustc 1.85.0".into(),
            binary: "rshogi-usi-universal".into(),
        };
        write_manifest(&m, &good).unwrap();
        assert!(matches!(read_manifest_status(&good), ManifestStatus::Loaded(_)));

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn manifest_round_trip_preserves_fields() {
        let manifest = Manifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            edition: "edition-layerstacks-halfka_hm_merged-1536x16x32-psqt".into(),
            profile: "production".into(),
            commit: "5616ea7c056ff21b6705c0ef00ca7266b7b2849f".into(),
            commit_dirty: false,
            built_at: "2026-05-24T22:30:00+09:00".into(),
            rustc: "rustc 1.85.0 (abc 2026-01-01)".into(),
            binary: "rshogi-usi-layerstacks-halfka_hm_merged-1536x16x32-psqt".into(),
        };
        let text = toml::to_string_pretty(&manifest).unwrap();
        let parsed: Manifest = toml::from_str(&text).unwrap();
        assert_eq!(parsed.schema_version, manifest.schema_version);
        assert_eq!(parsed.edition, manifest.edition);
        assert_eq!(parsed.profile, manifest.profile);
        assert_eq!(parsed.commit, manifest.commit);
        assert_eq!(parsed.commit_dirty, manifest.commit_dirty);
        assert_eq!(parsed.built_at, manifest.built_at);
        assert_eq!(parsed.rustc, manifest.rustc);
        assert_eq!(parsed.binary, manifest.binary);
    }

    #[test]
    fn manifest_parses_legacy_flavor_field() {
        // 旧 v1 manifest は `flavor` フィールドを含む。schema_version 据置のまま
        // field 削除しても serde が unknown を ignore して parse 成功すること
        // (backward-compat: 既存 binary の `(manifest broken)` 化を防ぐ)。
        let legacy = r#"
schema_version = 1
edition = "edition-universal"
flavor = "default"
profile = "production"
commit = "deadbeef"
commit_dirty = false
built_at = "2026-05-24T22:30:00+09:00"
rustc = "rustc 1.85.0"
binary = "rshogi-usi-universal"
"#;
        let parsed: Manifest = toml::from_str(legacy).unwrap_or_else(|e| {
            panic!("legacy manifest with `flavor` field should parse, got: {e:#}")
        });
        assert_eq!(parsed.schema_version, 1);
        assert_eq!(parsed.edition, "edition-universal");
        assert_eq!(parsed.profile, "production");
        assert_eq!(parsed.binary, "rshogi-usi-universal");
    }

    #[test]
    fn manifest_path_appends_meta_toml() {
        let p = manifest_path_for(Path::new("/tmp/engines/rshogi-usi-ls"));
        assert_eq!(p, PathBuf::from("/tmp/engines/rshogi-usi-ls.meta.toml"));
        let p_exe = manifest_path_for(Path::new("/tmp/engines/rshogi-usi-ls.exe"));
        assert_eq!(p_exe, PathBuf::from("/tmp/engines/rshogi-usi-ls.exe.meta.toml"));
    }

    #[test]
    fn format_size_uses_binary_units() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(2048), "2.0 KB");
        assert_eq!(format_size(2 * 1024 * 1024), "2.0 MB");
        assert_eq!(format_size(3 * 1024 * 1024 * 1024), "3.0 GB");
    }

    #[test]
    fn format_age_buckets_into_units() {
        let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(10_000_000);
        let make = |secs_ago: u64| Some(now - std::time::Duration::from_secs(secs_ago));
        assert_eq!(format_age(now, make(5)), "5s");
        assert_eq!(format_age(now, make(120)), "2m");
        assert_eq!(format_age(now, make(2 * 3600)), "2h");
        assert_eq!(format_age(now, make(3 * 86400)), "3d");
        assert_eq!(format_age(now, None), "-");
        // future
        assert_eq!(format_age(now, Some(now + std::time::Duration::from_secs(60))), "future");
    }

    #[test]
    fn is_engine_binary_filters_supporting_files() {
        // ファイル存在チェックを伴うので、tempdir で実ファイルを作成して判定する。
        let tmp = std::env::temp_dir().join(format!("xtask-isengine-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let make = |name: &str| {
            let p = tmp.join(name);
            std::fs::write(&p, b"x").unwrap();
            p
        };
        let bin = make("rshogi-usi-edition-x");
        assert!(is_engine_binary(&bin, "rshogi-usi-edition-x"));
        let meta = make("rshogi-usi-edition-x.meta.toml");
        assert!(!is_engine_binary(&meta, "rshogi-usi-edition-x.meta.toml"));
        let readme = make("README.md");
        assert!(!is_engine_binary(&readme, "README.md"));
        let bak = make("rshogi-usi-edition-x.bak-20260511");
        assert!(!is_engine_binary(&bak, "rshogi-usi-edition-x.bak-20260511"));
        let hidden = make(".gitkeep");
        assert!(!is_engine_binary(&hidden, ".gitkeep"));
        // unrelated prefix
        let other = make("some-other-binary");
        assert!(!is_engine_binary(&other, "some-other-binary"));
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn short_commit_truncates_to_eight_chars() {
        assert_eq!(short_commit("5616ea7c056ff21b6705c0ef00ca7266b7b2849f"), "5616ea7c");
        assert_eq!(short_commit("abc"), "abc");
    }
}
