//! `wrangler.production.toml` と `ConfigKeys` 定数の整合性検証。
//!
//! `wrangler.production.toml` は CI 自動 deploy で `wrangler deploy --config`
//! が読む本番設定ファイル。`ConfigKeys` 側で「production も local dev も var で
//! 管理する公開値」と分類した定数（[`ConfigKeys::PRODUCTION_VARS_KEYS`]）が
//! 過不足なく宣言されていることを検証する。
//!
//! **本ファイルが本番で扱わない値**（[`ConfigKeys::LOCAL_DEV_ONLY_VARS_KEYS`]、
//! 例: `ADMIN_HANDLE`）は production では `wrangler secret put` 経由で設定する
//! 仕様。本テストでは `wrangler.production.toml` の `[vars]` に **これらの値が
//! 含まれていないこと** も検証する。
//!
//! 過去事例の防止対象:
//! - PR #500 で `R2FloodgateHistoryStorage` を新設したが `wrangler.toml.example`
//!   への binding 追加が漏れていた → template 側は task 23.2 で固定化
//! - production toml の整合性は task 23.6 (本テスト) で固定
//!
//! `wrangler.toml.example` (local dev template) は別テスト
//! (`wrangler_template_consistency.rs`) が `PRODUCTION_VARS_KEYS` ∪
//! `LOCAL_DEV_ONLY_VARS_KEYS` の和集合と整合することを検証する。

use std::sync::LazyLock;

use rshogi_csa_server_workers::config::ConfigKeys;

struct ProductionBindings {
    r2_bindings: Vec<String>,
    do_bindings: Vec<String>,
    vars_keys: Vec<String>,
    /// `[[migrations]]` 配列を生のまま保持する。`new_sqlite_classes` 等を
    /// 各 test が独自に検査するため、`Vec<toml::Value>` のまま持つ。
    migrations: Vec<toml::Value>,
}

static PRODUCTION: LazyLock<ProductionBindings> = LazyLock::new(load_production_bindings);

fn load_production_bindings() -> ProductionBindings {
    let toml_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("wrangler.production.toml");
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

    ProductionBindings {
        r2_bindings,
        do_bindings,
        vars_keys,
        migrations,
    }
}

/// 双方向整合 assert。詳細は `wrangler_template_consistency.rs` の同名関数を参照。
fn assert_bidirectional(category: &str, code_side: &[&'static str], production_side: &[String]) {
    let missing_from_production: Vec<_> = code_side
        .iter()
        .filter(|name| !production_side.iter().any(|t| t == **name))
        .collect();
    assert!(
        missing_from_production.is_empty(),
        "wrangler.production.toml missing {category} entries declared in ConfigKeys: \
         {missing_from_production:?}; production currently declares: {production_side:?}",
    );

    let missing_from_code: Vec<_> = production_side
        .iter()
        .filter(|name| !code_side.contains(&name.as_str()))
        .collect();
    assert!(
        missing_from_code.is_empty(),
        "wrangler.production.toml declares {category} entries not present in ConfigKeys: \
         {missing_from_code:?}; ConfigKeys currently lists: {code_side:?}",
    );
}

/// `wrangler.production.toml` の `[[r2_buckets]]` 配列が、`ConfigKeys::ALL_R2_BINDINGS`
/// と双方向に一致することを検証する。
#[test]
fn wrangler_production_r2_bindings_match_config_keys() {
    assert_bidirectional("r2_bindings", ConfigKeys::ALL_R2_BINDINGS, &PRODUCTION.r2_bindings);
}

/// `wrangler.production.toml` の `[[durable_objects.bindings]]` 配列が、
/// `ConfigKeys::ALL_DO_BINDINGS` と双方向に一致することを検証する。
#[test]
fn wrangler_production_do_bindings_match_config_keys() {
    assert_bidirectional("do_bindings", ConfigKeys::ALL_DO_BINDINGS, &PRODUCTION.do_bindings);
}

/// `wrangler.production.toml` の `[vars]` テーブルキーが、
/// `ConfigKeys::PRODUCTION_VARS_KEYS` と双方向に一致することを検証する。
///
/// `LOCAL_DEV_ONLY_VARS_KEYS` (例: `ADMIN_HANDLE`) は production では
/// `wrangler secret put` 経由で設定する仕様のため、本配列に含めない。
#[test]
fn wrangler_production_vars_keys_match_production_only_subset() {
    assert_bidirectional(
        "vars_keys (production-only subset)",
        ConfigKeys::PRODUCTION_VARS_KEYS,
        &PRODUCTION.vars_keys,
    );
}

/// `wrangler.production.toml` の `[vars]` に `LOCAL_DEV_ONLY_VARS_KEYS` の各キーが
/// **含まれていない** ことを検証する。
///
/// production では `LOCAL_DEV_ONLY_VARS_KEYS`（例: `ADMIN_HANDLE`）を Cloudflare
/// secret として `wrangler secret put` で設定する仕様。誤って `[vars]` に書き戻して
/// しまった場合に defense-in-depth の前提が崩れるため、本テストで gate する。
#[test]
fn wrangler_production_vars_must_not_contain_local_dev_only_keys() {
    let leaked: Vec<_> = ConfigKeys::LOCAL_DEV_ONLY_VARS_KEYS
        .iter()
        .filter(|name| PRODUCTION.vars_keys.iter().any(|t| t == **name))
        .collect();
    assert!(
        leaked.is_empty(),
        "wrangler.production.toml [vars] must not declare keys listed in \
         ConfigKeys::LOCAL_DEV_ONLY_VARS_KEYS (these should be set via `wrangler secret put` \
         instead): leaked = {leaked:?}; declared production [vars] keys = {:?}",
        PRODUCTION.vars_keys,
    );
}

/// `[[migrations]]` で `new_sqlite_classes = ["GameRoom"]` が宣言されていることを
/// 検証する。CI deploy で初回適用される SQLite-backed DO の migration が template
/// から抜けていると DO instance の SQLite Storage が利用できない状態で本番化される。
#[test]
fn wrangler_production_declares_sqlite_migration_for_game_room() {
    assert!(
        !PRODUCTION.migrations.is_empty(),
        "wrangler.production.toml must declare [[migrations]]",
    );

    let declares_game_room_sqlite = PRODUCTION.migrations.iter().any(|m| {
        m.get("new_sqlite_classes")
            .and_then(|v| v.as_array())
            .is_some_and(|classes| classes.iter().any(|c| c.as_str() == Some("GameRoom")))
    });
    assert!(
        declares_game_room_sqlite,
        "wrangler.production.toml must declare [[migrations]] new_sqlite_classes = [\"GameRoom\"]; \
         got migrations: {:?}",
        PRODUCTION.migrations,
    );
}
