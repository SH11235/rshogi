//! rshogi 内部 build 自動化タスク。
//!
//! `cargo xtask build --edition <preset>[,<preset>...]` で rshogi-core の preset edition
//! feature を有効化した `rshogi-usi` バイナリを build し、`engines/rshogi-usi-<edition>`
//! という命名規則で `engines/` 下に配置する。各 binary には同階層に `<binary>.meta.toml`
//! を書き出し、後から commit / profile / built_at を追跡できるようにする。
//! `--features <name>[,<name>...]` で Edition 軸と直交する rshogi-usi の opt-in feature
//! (`mimalloc` 等) を全 edition に追加でき、その場合は binary 名に `+<feature>` が付く。
//! 設計と命名規則の根拠は ADR `docs/decisions/2026-05-24-build-edition-flavor-design.md`
//! を参照。

use std::collections::{BTreeMap, BTreeSet};
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

/// rshogi-usi の Cargo.toml。`--features` で追加できる feature 名の真値とする。
const USI_CARGO_TOML: &str = include_str!("../../rshogi-usi/Cargo.toml");

const USI_PACKAGE: &str = "rshogi-usi";
const USI_BINARY: &str = "rshogi-usi";
const DEFAULT_PROFILE: &str = "production";
const EDITION_PREFIX: &str = "edition-";
/// binary 名で edition slug と追加 feature を区切る文字。Edition 命名は slot 区切りに `-`、
/// slot 内の複合語に `_` を使い、feature 名自体も `-` を含むため、どれとも衝突しない
/// `+` を使う。
const EXTRA_FEATURE_SEPARATOR: char = '+';
/// rshogi-usi の feature 定義が rshogi-core の feature を参照するときの接頭辞。
/// `rshogi-core?/x` は optional dependency 向けの書式で、参照先 feature は同じ。
const CORE_FEATURE_REF_PREFIXES: [&str; 2] = ["rshogi-core/", "rshogi-core?/"];
/// NNUE の構造 (Threat 次元 / EffectBucket の形) を選ぶ feature family の接頭辞。
/// どの preset edition にも束ねられていない member も含め、`--features` では family ごと
/// 拒否する。edition 名と network 構造が食い違う binary を作らないためと、preset の追加で
/// 受理される名前が黙って変わらないようにするため。
const STRUCTURAL_FEATURE_FAMILY_PREFIXES: [&str; 2] = ["threat-profile-", "effect-bucket-"];
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
        /// edition に追加で有効化する rshogi-usi の opt-in feature (`mimalloc` /
        /// `search-stats` 等、Edition 軸と直交するもの)。build 対象の全 edition に付く。
        /// カンマ区切り (`a,b`) または `--features` 複数回いずれも受け付ける。
        /// 指定時は binary 名が `rshogi-usi-<edition>+<feature>[+<feature>...]` になる。
        /// 拒否するもの: `default` / `edition-*` / edition の構成部品 / NNUE 構造の family
        /// (`threat-profile-*` / `effect-bucket-*`)。
        #[arg(long, value_delimiter = ',')]
        features: Vec<String>,
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
            features,
        } => run_build(edition, all_presets, &profile, &features),
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

fn run_build(
    edition_args: Vec<String>,
    all_presets: bool,
    profile: &str,
    feature_args: &[String],
) -> Result<()> {
    let available = preset_editions(CORE_CARGO_TOML)?;
    let extra_features = resolve_extra_features(feature_args, USI_CARGO_TOML, CORE_CARGO_TOML)?;
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
        extra_features: &extra_features,
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
    /// 検証・ソート・重複除去済みの追加 feature (`resolve_extra_features` の結果)。
    extra_features: &'a [String],
    commit: Option<&'a str>,
    commit_dirty: bool,
    rustc_version: Option<&'a str>,
}

fn build_one(ctx: &BuildContext, edition_feature: &str) -> Result<()> {
    let cargo_features = cargo_features_arg(edition_feature, ctx.extra_features);
    println!("==> Building {cargo_features} (profile={})", ctx.profile);
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
            &cargo_features,
        ])
        .status()
        .with_context(|| format!("failed to spawn cargo build (cargo={})", ctx.cargo))?;
    if !status.success() {
        bail!("cargo build exited with {status} for features `{cargo_features}`");
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

    let dst = engines_path(ctx.workspace_root, edition_feature, ctx.extra_features)?;
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create engines dir: {}", parent.display()))?;
    }
    std::fs::copy(&src, &dst)
        .with_context(|| format!("copy {} -> {}", src.display(), dst.display()))?;

    let manifest = Manifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        edition: edition_feature.to_string(),
        features: ctx.extra_features.to_vec(),
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
                (edition_label(m), m.profile.clone(), short_commit, status.to_string())
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
    /// `--features` で追加した feature (ソート・重複除去済み)。追加なしの build では
    /// field ごと省略し、manifest を追加 feature 導入前と同一の内容に保つ。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    features: Vec<String>,
    profile: String,
    commit: String,
    commit_dirty: bool,
    built_at: String,
    rustc: String,
    binary: String,
}

/// `list-binaries` の EDITION 列に出す文字列。追加 feature があれば binary 名と同じ
/// `+<feature>` 形式で後置する。
fn edition_label(manifest: &Manifest) -> String {
    let mut label = manifest.edition.clone();
    push_extra_features(&mut label, &manifest.features);
    label
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
    let features = feature_table(cargo_toml, "rshogi-core")?;
    let mut out: Vec<String> = features
        .keys()
        .filter(|name| name.starts_with(EDITION_PREFIX))
        .cloned()
        .collect();
    out.sort();
    Ok(out)
}

/// Cargo.toml の `[features]` を「feature 名 → その feature が有効化する項目」の表で返す。
/// `crate_name` はエラーメッセージ用。
fn feature_table(cargo_toml: &str, crate_name: &str) -> Result<BTreeMap<String, Vec<String>>> {
    let parsed: toml::Value =
        toml::from_str(cargo_toml).with_context(|| format!("parse {crate_name} Cargo.toml"))?;
    let features = parsed
        .get("features")
        .and_then(|v| v.as_table())
        .with_context(|| format!("{crate_name} Cargo.toml has no [features] section"))?;
    let mut out = BTreeMap::new();
    for (name, entries) in features {
        let entries = entries
            .as_array()
            .with_context(|| format!("{crate_name} feature `{name}` is not an array"))?
            .iter()
            .map(|e| {
                e.as_str().map(str::to_string).with_context(|| {
                    format!("{crate_name} feature `{name}` has a non-string entry")
                })
            })
            .collect::<Result<Vec<String>>>()?;
        out.insert(name.clone(), entries);
    }
    Ok(out)
}

/// `roots` から同一 crate 内の feature 参照を辿って到達できる項目を全て集める。
/// 返り値には `dep:x` / `<crate>/<feature>` 形式の項目もそのまま含まれる。
fn feature_closure<'a>(
    table: &'a BTreeMap<String, Vec<String>>,
    roots: impl IntoIterator<Item = &'a str>,
) -> BTreeSet<&'a str> {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut stack: Vec<&str> = roots.into_iter().collect();
    while let Some(item) = stack.pop() {
        if !seen.insert(item) {
            continue;
        }
        if let Some(entries) = table.get(item) {
            stack.extend(entries.iter().map(String::as_str));
        }
    }
    seen
}

/// `--features` の値を検証し、ソート・重複除去した追加 feature の一覧を返す。
///
/// 追加できるのは rshogi-usi の `[features]` に定義された、Edition 軸と直交する opt-in
/// feature だけ。次は拒否する:
/// - rshogi-usi に存在しない名前
/// - `default` (xtask は `--no-default-features` で edition を 1 つに固定する)
/// - `edition-*` (edition は `--edition` で指定する)
/// - NNUE 構造の family: `STRUCTURAL_FEATURE_FAMILY_PREFIXES` で始まる名前
///   (preset に束ねられているかによらず family ごと)
/// - edition の構成部品: rshogi-core のいずれかの preset edition が bundle する feature
///   (`mode-*` / `layerstack-arch` / `nnue-psqt` / `nnue-progress-diff` 等) を有効化する
///   もの。edition 名と実際の構成が食い違う binary を作らないため。
fn resolve_extra_features(
    raw: &[String],
    usi_cargo_toml: &str,
    core_cargo_toml: &str,
) -> Result<Vec<String>> {
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    let usi = feature_table(usi_cargo_toml, USI_PACKAGE)?;
    let core = feature_table(core_cargo_toml, "rshogi-core")?;
    let edition_parts = feature_closure(
        &core,
        core.keys().map(String::as_str).filter(|name| name.starts_with(EDITION_PREFIX)),
    );

    let mut out: BTreeSet<String> = BTreeSet::new();
    for name in raw {
        let name = name.trim();
        if name.is_empty() {
            bail!("empty feature name in `--features` (空の feature 名は指定できません)");
        }
        if name == "default" {
            bail!(
                "`default` cannot be passed to `--features` (xtask は `--no-default-features` で edition を 1 つに固定します)"
            );
        }
        if name.starts_with(EDITION_PREFIX) {
            bail!(
                "`{name}` is a preset edition, not an extra feature (edition は `--edition` で指定してください)"
            );
        }
        if !usi.contains_key(name) {
            bail!(
                "unknown {USI_PACKAGE} feature: `{name}` (指定できる feature は crates/rshogi-usi/Cargo.toml の [features] で確認できます)"
            );
        }
        if let Some(family) = STRUCTURAL_FEATURE_FAMILY_PREFIXES
            .iter()
            .find(|prefix| name.starts_with(**prefix))
        {
            bail!(
                "`{name}` selects NNUE structure and must come from an edition (`{family}*` は NNUE の構造を選ぶ feature family です。`--edition` で該当 preset を選んでください)"
            );
        }
        let bundled = feature_closure(&usi, [name])
            .into_iter()
            .filter_map(core_feature_ref)
            .find(|core_feature| edition_parts.contains(core_feature));
        if let Some(core_feature) = bundled {
            bail!(
                "`{name}` is a building block of preset editions (rshogi-core `{core_feature}` は preset edition が bundle する feature です。`--edition` で該当 preset を選んでください)"
            );
        }
        out.insert(name.to_string());
    }
    Ok(out.into_iter().collect())
}

/// feature 定義の項目が rshogi-core の feature 参照 (`rshogi-core/x` / `rshogi-core?/x`)
/// なら参照先の feature 名を返す。
fn core_feature_ref(item: &str) -> Option<&str> {
    CORE_FEATURE_REF_PREFIXES.iter().find_map(|prefix| item.strip_prefix(prefix))
}

/// cargo の `--features` に渡す値 (`<edition>[,<extra>...]`) を組み立てる。
fn cargo_features_arg(edition_feature: &str, extra_features: &[String]) -> String {
    let mut arg = edition_feature.to_string();
    for feature in extra_features {
        arg.push(',');
        arg.push_str(feature);
    }
    arg
}

/// `+<feature>` を追加 feature の数だけ後置する。binary 名と `list-binaries` の表示で共用。
fn push_extra_features(target: &mut String, extra_features: &[String]) {
    for feature in extra_features {
        target.push(EXTRA_FEATURE_SEPARATOR);
        target.push_str(feature);
    }
}

/// `engines/rshogi-usi-<edition slug>[+<feature>...]<EXE_SUFFIX>` を組み立てる。
/// `extra_features` は `resolve_extra_features` でソート・重複除去済みのものを渡す
/// (同じ構成が指定順によらず同じ binary 名になる)。
fn engines_path(
    workspace_root: &Path,
    edition_feature: &str,
    extra_features: &[String],
) -> Result<PathBuf> {
    let slug = edition_feature
        .strip_prefix(EDITION_PREFIX)
        .with_context(|| format!("edition feature `{edition_feature}` missing prefix"))?;
    let mut name = format!("{USI_BINARY}-{slug}");
    push_extra_features(&mut name, extra_features);
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
        let p = engines_path(&root, "edition-layerstacks-halfka_hm_merged-1536x16x32-psqt", &[])
            .unwrap();
        let expected = format!(
            "/tmp/rshogi/engines/rshogi-usi-layerstacks-halfka_hm_merged-1536x16x32-psqt{}",
            std::env::consts::EXE_SUFFIX
        );
        assert_eq!(p, PathBuf::from(expected));
    }

    #[test]
    fn engines_path_appends_extra_features() {
        let root = PathBuf::from("/tmp/rshogi");
        let extras = vec!["mimalloc".to_string(), "search-stats".to_string()];
        let p = engines_path(&root, "edition-universal", &extras).unwrap();
        let expected = format!(
            "/tmp/rshogi/engines/rshogi-usi-universal+mimalloc+search-stats{}",
            std::env::consts::EXE_SUFFIX
        );
        assert_eq!(p, PathBuf::from(expected));
    }

    #[test]
    fn cargo_features_arg_joins_edition_and_extras() {
        assert_eq!(cargo_features_arg("edition-universal", &[]), "edition-universal");
        let extras = vec!["mimalloc".to_string(), "search-stats".to_string()];
        assert_eq!(
            cargo_features_arg("edition-universal", &extras),
            "edition-universal,mimalloc,search-stats"
        );
    }

    fn resolve(raw: &[&str]) -> Result<Vec<String>> {
        let raw: Vec<String> = raw.iter().map(|s| s.to_string()).collect();
        resolve_extra_features(&raw, USI_CARGO_TOML, CORE_CARGO_TOML)
    }

    #[test]
    fn resolve_extra_features_sorts_and_dedups() {
        assert_eq!(resolve(&[]).unwrap(), Vec::<String>::new());
        assert_eq!(resolve(&["mimalloc"]).unwrap(), vec!["mimalloc"]);
        assert_eq!(
            resolve(&["search-stats", " mimalloc", "search-stats"]).unwrap(),
            vec!["mimalloc", "search-stats"]
        );
    }

    #[test]
    fn resolve_extra_features_accepts_orthogonal_opt_in_features() {
        for name in [
            "mimalloc",
            "prepacked-nnue",
            "search-stats",
            "nnue-stats",
            "tt-trace",
            "diagnostics",
            "allocation-stats",
            "tt-write-stats",
            "search-no-pass-rules",
        ] {
            assert_eq!(resolve(&[name]).unwrap(), vec![name], "feature {name} should be accepted");
        }
    }

    #[test]
    fn resolve_extra_features_rejects_invalid_names() {
        let cases = [
            ("no-such-feature", "unknown rshogi-usi feature"),
            ("", "empty feature name"),
            ("default", "`default` cannot be passed"),
            ("edition-universal", "is a preset edition"),
            // rshogi-usi に無い edition 名も unknown ではなく edition として案内する。
            ("edition-no-such", "is a preset edition"),
            ("mode-specific", "building block"),
            ("layerstack-arch", "building block"),
            ("layerstacks-1536x16x32", "building block"),
            ("ft-halfka_hm_merged", "building block"),
            ("nnue-psqt", "building block"),
            ("nnue-progress-diff", "building block"),
            ("nnue-effect-bucket", "building block"),
            // NNUE 構造の family は preset に束ねられているかによらず拒否する。
            ("threat-profile-same-class", "selects NNUE structure"),
            ("threat-profile-cross-side", "selects NNUE structure"),
            ("effect-bucket-2x2-kingfixed", "selects NNUE structure"),
            ("effect-bucket-2x2-kingbucketed", "selects NNUE structure"),
            ("effect-bucket-3x3-kingbucketed", "selects NNUE structure"),
        ];
        for (name, expected) in cases {
            let err = resolve(&[name]).expect_err(name).to_string();
            assert!(err.contains(expected), "feature `{name}`: unexpected error: {err}");
        }
        // 正しい名前と混ぜても全体を拒否する。
        assert!(resolve(&["mimalloc", "no-such-feature"]).is_err());
    }

    #[test]
    fn resolve_extra_features_follows_local_feature_references() {
        // rshogi-usi 内の別 feature 経由で edition の構成部品を有効化するものも拒否する。
        let usi = r#"
[features]
plain = []
indirect = ["plain", "wrapped-arch"]
wrapped-arch = ["rshogi-core/some-arch"]
"#;
        let core = r#"
[features]
edition-x = ["edition-x-any"]
edition-x-any = ["some-arch"]
some-arch = []
"#;
        let run = |name: &str| resolve_extra_features(&[name.to_string()], usi, core);
        assert_eq!(run("plain").unwrap(), vec!["plain"]);
        for name in ["wrapped-arch", "indirect"] {
            let err = run(name).expect_err(name).to_string();
            assert!(err.contains("building block"), "feature `{name}`: {err}");
        }
    }

    #[test]
    fn resolve_extra_features_handles_optional_dependency_references() {
        // `rshogi-core?/x` (optional dependency 向け書式) 経由の構成部品も拒否する。
        let usi = r#"
[features]
weak-arch = ["rshogi-core?/some-arch"]
weak-other = ["rshogi-core?/unrelated"]
"#;
        let core = r#"
[features]
edition-x = ["some-arch"]
some-arch = []
unrelated = []
"#;
        let run = |name: &str| resolve_extra_features(&[name.to_string()], usi, core);
        let err = run("weak-arch").expect_err("weak-arch").to_string();
        assert!(err.contains("building block"), "{err}");
        assert_eq!(run("weak-other").unwrap(), vec!["weak-other"]);
    }

    #[test]
    fn resolve_extra_features_rejects_structural_families_even_if_unbundled() {
        // どの preset にも束ねられていない member でも family 接頭辞で拒否する。
        let usi = r#"
[features]
threat-profile-new = ["rshogi-core/threat-profile-new"]
effect-bucket-new = []
"#;
        let core = r#"
[features]
edition-x = []
threat-profile-new = []
"#;
        for name in ["threat-profile-new", "effect-bucket-new"] {
            let err = resolve_extra_features(&[name.to_string()], usi, core)
                .expect_err(name)
                .to_string();
            assert!(err.contains("selects NNUE structure"), "feature `{name}`: {err}");
        }
    }

    #[test]
    fn cli_features_accepts_comma_and_repeated_forms() {
        let parse = |args: &[&str]| {
            let mut argv = vec!["xtask", "build", "--edition", "universal"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.command {
                SubCmd::Build { features, .. } => features,
                other => panic!("unexpected subcommand: {other:?}"),
            })
        };
        assert_eq!(parse(&[]).unwrap(), Vec::<String>::new());
        assert_eq!(parse(&["--features", "a,b"]).unwrap(), vec!["a", "b"]);
        assert_eq!(parse(&["--features", "a", "--features", "b,c"]).unwrap(), vec!["a", "b", "c"]);
        // 1 回の `--features` が取る引数は 1 つ。空白区切りの 2 つ目は受け付けない。
        assert!(parse(&["--features", "a", "b"]).is_err());
    }

    #[test]
    fn usi_feature_names_do_not_contain_separator() {
        // binary 名の `+` 区切りが一意に読めるよう、feature 名側に `+` が無いことを保証する。
        let usi = feature_table(USI_CARGO_TOML, USI_PACKAGE).unwrap();
        for name in usi.keys() {
            assert!(!name.contains(EXTRA_FEATURE_SEPARATOR), "feature `{name}` contains `+`");
        }
    }

    fn sample_manifest(features: Vec<String>) -> Manifest {
        Manifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            edition: "edition-universal".into(),
            features,
            profile: "production".into(),
            commit: "deadbeef".into(),
            commit_dirty: false,
            built_at: "2026-05-24T22:30:00+09:00".into(),
            rustc: "rustc 1.85.0".into(),
            binary: "rshogi-usi-universal".into(),
        }
    }

    #[test]
    fn manifest_omits_features_field_without_extras() {
        let text = toml::to_string_pretty(&sample_manifest(Vec::new())).unwrap();
        let expected = r#"schema_version = 1
edition = "edition-universal"
profile = "production"
commit = "deadbeef"
commit_dirty = false
built_at = "2026-05-24T22:30:00+09:00"
rustc = "rustc 1.85.0"
binary = "rshogi-usi-universal"
"#;
        assert_eq!(text, expected);
    }

    #[test]
    fn manifest_round_trips_extra_features() {
        let m = sample_manifest(vec!["mimalloc".into(), "search-stats".into()]);
        let text = toml::to_string_pretty(&m).unwrap();
        let parsed: Manifest = toml::from_str(&text).unwrap();
        assert_eq!(parsed.features, m.features);
        assert_eq!(edition_label(&parsed), "edition-universal+mimalloc+search-stats");
        assert_eq!(edition_label(&sample_manifest(Vec::new())), "edition-universal");
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
        let m = sample_manifest(Vec::new());
        write_manifest(&m, &good).unwrap();
        let ManifestStatus::Loaded(loaded) = read_manifest_status(&good) else {
            panic!("valid manifest was not loaded");
        };
        assert_eq!(loaded.schema_version, m.schema_version);
        assert_eq!(loaded.edition, m.edition);
        assert_eq!(loaded.features, m.features);
        assert_eq!(loaded.profile, m.profile);
        assert_eq!(loaded.commit, m.commit);
        assert_eq!(loaded.commit_dirty, m.commit_dirty);
        assert_eq!(loaded.built_at, m.built_at);
        assert_eq!(loaded.rustc, m.rustc);
        assert_eq!(loaded.binary, m.binary);

        std::fs::remove_dir_all(&tmp).ok();
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
        // `features` field を持たない manifest (追加 feature 導入前 / 追加なし build) は空で読む。
        assert!(parsed.features.is_empty());
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
