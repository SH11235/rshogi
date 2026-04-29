//! `wrangler.production.toml` / `wrangler.staging.toml` と `ConfigKeys` 定数の
//! 整合性検証。
//!
//! 各環境向け toml は CI 自動 deploy で `wrangler deploy --config <file>` が読む
//! 設定ファイル。`ConfigKeys` 側で「全環境で `[vars]` で管理する公開値」と分類
//! した定数（[`ConfigKeys::SHARED_PUBLIC_VARS_KEYS`]）が過不足なく宣言されている
//! ことを各環境ファイルについて検証する。
//!
//! **本ファイルが各環境で扱わない値**（[`ConfigKeys::LOCAL_DEV_ONLY_VARS_KEYS`]、
//! 例: `ADMIN_HANDLE`）は production / staging いずれも `wrangler secret put`
//! 経由で設定する仕様。本テストでは各環境 toml の `[vars]` に **これらの値が
//! 含まれていないこと** も検証する。
//!
//! `wrangler.toml.example` (local dev template) は別テスト
//! (`wrangler_template_consistency.rs`) が `SHARED_PUBLIC_VARS_KEYS` ∪
//! `LOCAL_DEV_ONLY_VARS_KEYS` の和集合と整合することを検証する。

use std::sync::LazyLock;

use rshogi_csa_server_workers::config::ConfigKeys;

/// 単一の deploy 環境（production / staging）から抽出したバインディング情報。
/// 比較ロジックを共通化してファイル数だけ test を増やせるようにする。
struct EnvironmentBindings {
    /// 失敗 message に出す環境名（"production" / "staging"）。
    label: &'static str,
    /// 失敗 message に出す toml ファイル名。
    file_name: &'static str,
    r2_bindings: Vec<String>,
    do_bindings: Vec<String>,
    vars_keys: Vec<String>,
    /// `[[migrations]]` 配列を生のまま保持する。`new_sqlite_classes` 等を
    /// 各 test が独自に検査するため、`Vec<toml::Value>` のまま持つ。
    migrations: Vec<toml::Value>,
    /// `[triggers] crons = [...]` の値 (Issue #551)。空配列は未宣言。
    crons: Vec<String>,
}

static PRODUCTION: LazyLock<EnvironmentBindings> =
    LazyLock::new(|| load_environment_bindings("production", "wrangler.production.toml"));
static STAGING: LazyLock<EnvironmentBindings> =
    LazyLock::new(|| load_environment_bindings("staging", "wrangler.staging.toml"));

fn load_environment_bindings(label: &'static str, file_name: &'static str) -> EnvironmentBindings {
    let toml_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(file_name);
    let raw = std::fs::read_to_string(&toml_path).unwrap_or_else(|e| {
        panic!("failed to read {}: {e}", toml_path.display());
    });
    let doc: toml::Value = toml::from_str(&raw).unwrap_or_else(|e| {
        panic!("failed to parse {} as TOML: {e}", toml_path.display());
    });

    let r2_bindings = doc
        .get("r2_buckets")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.get("binding").and_then(|v| v.as_str()).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();

    let do_bindings = doc
        .get("durable_objects")
        .and_then(|v| v.get("bindings"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.get("name").and_then(|v| v.as_str()).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();

    let vars_keys = doc
        .get("vars")
        .and_then(|v| v.as_table())
        .map(|t| t.keys().cloned().collect())
        .unwrap_or_default();

    let migrations = doc.get("migrations").and_then(|v| v.as_array()).cloned().unwrap_or_default();

    let crons = doc
        .get("triggers")
        .and_then(|v| v.get("crons"))
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|t| t.as_str().map(str::to_owned)).collect())
        .unwrap_or_default();

    EnvironmentBindings {
        label,
        file_name,
        r2_bindings,
        do_bindings,
        vars_keys,
        migrations,
        crons,
    }
}

/// 双方向整合 assert。詳細は `wrangler_template_consistency.rs` の同名関数を参照。
fn assert_bidirectional(
    env: &EnvironmentBindings,
    category: &str,
    code_side: &[&'static str],
    env_side: &[String],
) {
    let missing_from_env: Vec<_> =
        code_side.iter().filter(|name| !env_side.iter().any(|t| t == **name)).collect();
    assert!(
        missing_from_env.is_empty(),
        "{file} ({label}) missing {category} entries declared in ConfigKeys: \
         {missing_from_env:?}; {label} currently declares: {env_side:?}",
        file = env.file_name,
        label = env.label,
    );

    let missing_from_code: Vec<_> =
        env_side.iter().filter(|name| !code_side.contains(&name.as_str())).collect();
    assert!(
        missing_from_code.is_empty(),
        "{file} ({label}) declares {category} entries not present in ConfigKeys: \
         {missing_from_code:?}; ConfigKeys currently lists: {code_side:?}",
        file = env.file_name,
        label = env.label,
    );
}

fn assert_r2_bindings_match(env: &EnvironmentBindings) {
    assert_bidirectional(env, "r2_bindings", ConfigKeys::ALL_R2_BINDINGS, &env.r2_bindings);
}

fn assert_do_bindings_match(env: &EnvironmentBindings) {
    assert_bidirectional(env, "do_bindings", ConfigKeys::ALL_DO_BINDINGS, &env.do_bindings);
}

/// `ConfigKeys::SHARED_PUBLIC_VARS_KEYS` は **deploy 対象の全環境（production /
/// staging）で共有する公開 `[vars]` キー集合**。production / staging いずれも
/// 同じ集合を `[vars]` として持つことを保証する。
fn assert_vars_keys_match_shared_public_vars(env: &EnvironmentBindings) {
    assert_bidirectional(
        env,
        "vars_keys (shared public vars across deployed environments)",
        ConfigKeys::SHARED_PUBLIC_VARS_KEYS,
        &env.vars_keys,
    );
}

fn assert_no_local_dev_only_keys(env: &EnvironmentBindings) {
    let leaked: Vec<_> = ConfigKeys::LOCAL_DEV_ONLY_VARS_KEYS
        .iter()
        .filter(|name| env.vars_keys.iter().any(|t| t == **name))
        .collect();
    assert!(
        leaked.is_empty(),
        "{file} ({label}) [vars] must not declare keys listed in \
         ConfigKeys::LOCAL_DEV_ONLY_VARS_KEYS (these should be set via `wrangler secret put` \
         instead): leaked = {leaked:?}; declared [vars] keys = {keys:?}",
        file = env.file_name,
        label = env.label,
        keys = env.vars_keys,
    );
}

/// Issue #551 で追加した `[triggers] crons` が各 deploy 環境に宣言されていることを
/// 固定する。`[event(scheduled)]` ハンドラは production / staging 両方で稼働させる
/// 契約 (片方だけ宣言だと backfill / orphan sweep が動かず orphan が滞留する)。
fn assert_declares_backfill_cron_trigger(env: &EnvironmentBindings) {
    assert!(
        !env.crons.is_empty(),
        "{file} ({label}) must declare [triggers] crons = [...] for the backfill scheduled handler; \
         got: {crons:?}",
        file = env.file_name,
        label = env.label,
        crons = env.crons,
    );
}

fn assert_declares_sqlite_migration_for_game_room(env: &EnvironmentBindings) {
    assert!(
        !env.migrations.is_empty(),
        "{file} ({label}) must declare [[migrations]]",
        file = env.file_name,
        label = env.label,
    );

    let declares_game_room_sqlite = env.migrations.iter().any(|m| {
        m.get("new_sqlite_classes")
            .and_then(|v| v.as_array())
            .is_some_and(|classes| classes.iter().any(|c| c.as_str() == Some("GameRoom")))
    });
    assert!(
        declares_game_room_sqlite,
        "{file} ({label}) must declare [[migrations]] new_sqlite_classes = [\"GameRoom\"]; \
         got migrations: {migrations:?}",
        file = env.file_name,
        label = env.label,
        migrations = env.migrations,
    );
}

// --- production ----------------------------------------------------------

#[test]
fn wrangler_production_r2_bindings_match_config_keys() {
    assert_r2_bindings_match(&PRODUCTION);
}

#[test]
fn wrangler_production_do_bindings_match_config_keys() {
    assert_do_bindings_match(&PRODUCTION);
}

#[test]
fn wrangler_production_vars_keys_match_shared_public_vars() {
    assert_vars_keys_match_shared_public_vars(&PRODUCTION);
}

#[test]
fn wrangler_production_vars_must_not_contain_local_dev_only_keys() {
    assert_no_local_dev_only_keys(&PRODUCTION);
}

#[test]
fn wrangler_production_declares_sqlite_migration_for_game_room() {
    assert_declares_sqlite_migration_for_game_room(&PRODUCTION);
}

#[test]
fn wrangler_production_declares_backfill_cron_trigger() {
    assert_declares_backfill_cron_trigger(&PRODUCTION);
}

// --- staging -------------------------------------------------------------

#[test]
fn wrangler_staging_r2_bindings_match_config_keys() {
    assert_r2_bindings_match(&STAGING);
}

#[test]
fn wrangler_staging_do_bindings_match_config_keys() {
    assert_do_bindings_match(&STAGING);
}

#[test]
fn wrangler_staging_vars_keys_match_shared_public_vars() {
    assert_vars_keys_match_shared_public_vars(&STAGING);
}

#[test]
fn wrangler_staging_vars_must_not_contain_local_dev_only_keys() {
    assert_no_local_dev_only_keys(&STAGING);
}

#[test]
fn wrangler_staging_declares_sqlite_migration_for_game_room() {
    assert_declares_sqlite_migration_for_game_room(&STAGING);
}

#[test]
fn wrangler_staging_declares_backfill_cron_trigger() {
    assert_declares_backfill_cron_trigger(&STAGING);
}
