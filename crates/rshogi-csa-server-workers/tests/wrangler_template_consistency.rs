//! `wrangler.toml.example` と `ConfigKeys` 定数の整合性検証。
//!
//! Workers コードに新しい binding 定数（R2 / DO / `[vars]` キー）を追加した PR が、
//! `wrangler.toml.example` のテンプレート更新を忘れて merge されると、運用者が
//! `cp wrangler.toml.example wrangler.toml` で派生させた本番設定で binding 不足の
//! まま deploy する事故が発生する。逆に、template にだけ binding を追加して
//! `ConfigKeys` への追加を忘れたケースもコード側で参照されない dead binding を
//! 抱え込む原因になる。
//!
//! 過去事例: PR #500 で `R2FloodgateHistoryStorage` を新設し
//! `ConfigKeys::FLOODGATE_HISTORY_BUCKET_BINDING` を追加したが、対応する
//! `[[r2_buckets]]` エントリの template への追加が漏れていた（PR #505 で修正）。
//!
//! 本テストは `wrangler.toml.example` を TOML として parse し、`ConfigKeys` の
//! 網羅配列 (`ALL_R2_BINDINGS` / `ALL_DO_BINDINGS` /
//! `SHARED_PUBLIC_VARS_KEYS` ∪ `LOCAL_DEV_ONLY_VARS_KEYS`) と template の宣言が
//! **双方向に一致** することを assert する。
//!
//! - 順方向: `ConfigKeys::ALL_*` の各要素 ⊆ template 宣言（コード追加 → template 漏れ検出）
//! - 逆方向: template 宣言 ⊆ `ConfigKeys::ALL_*` の各要素（template 追加 → コード参照漏れ検出）

use std::sync::LazyLock;

use rshogi_csa_server_workers::config::ConfigKeys;

/// `wrangler.toml.example` を解析した結果。テンプレートに宣言されている
/// 各種 binding / `[vars]` キーを集約する。
struct TemplateBindings {
    r2_bindings: Vec<String>,
    do_bindings: Vec<String>,
    vars_keys: Vec<String>,
    /// `[triggers] crons = [...]` の値を保持する (Issue #551)。空配列は未宣言。
    crons: Vec<String>,
}

/// テスト 1 本ごとに file I/O + parse を繰り返さないため `LazyLock` で 1 回化する。
/// 失敗時は `panic!` する（dev 経路でのみ動作するテストなので Result 化は不要）。
static TEMPLATE: LazyLock<TemplateBindings> = LazyLock::new(load_template_bindings);

fn load_template_bindings() -> TemplateBindings {
    let template_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("wrangler.toml.example");
    let raw = std::fs::read_to_string(&template_path).unwrap_or_else(|e| {
        panic!("failed to read {}: {e}", template_path.display());
    });
    let doc: toml::Value = toml::from_str(&raw).unwrap_or_else(|e| {
        panic!("failed to parse {} as TOML: {e}", template_path.display());
    });

    // `[[r2_buckets]]` 配列の各エントリから `binding = "..."` を集める。
    let r2_bindings = doc
        .get("r2_buckets")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.get("binding").and_then(|v| v.as_str()).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();

    // `[[durable_objects.bindings]]` 配列の各エントリから `name = "..."` を集める。
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

    // `[vars]` テーブルのキー集合を集める。
    let vars_keys = doc
        .get("vars")
        .and_then(|v| v.as_table())
        .map(|t| t.keys().cloned().collect())
        .unwrap_or_default();

    // `[triggers] crons = [...]` を集める。
    let crons = doc
        .get("triggers")
        .and_then(|v| v.get("crons"))
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|t| t.as_str().map(str::to_owned)).collect())
        .unwrap_or_default();

    TemplateBindings {
        r2_bindings,
        do_bindings,
        vars_keys,
        crons,
    }
}

/// 双方向整合 assert ヘルパ。
///
/// `code_side` (= `ConfigKeys::ALL_*`) と `template_side` (= `wrangler.toml.example`
/// から抽出したリスト) が同一集合であることを検証する。
///
/// - `code_side` にあって `template_side` に無い要素 → template 更新忘れ
///   (`ConfigKeys` に定数追加したが template 更新を怠った)
/// - `template_side` にあって `code_side` に無い要素 → `ConfigKeys::ALL_*` 登録忘れ
///   (template に binding を入れたが `ConfigKeys::ALL_*` への登録を怠った)
fn assert_bidirectional(category: &str, code_side: &[&'static str], template_side: &[String]) {
    let missing_from_template: Vec<_> = code_side
        .iter()
        .filter(|name| !template_side.iter().any(|t| t == **name))
        .collect();
    assert!(
        missing_from_template.is_empty(),
        "wrangler.toml.example missing {category} entries declared in ConfigKeys::ALL_{cat_upper}: \
         {missing_from_template:?}; template currently declares: {template_side:?}",
        cat_upper = category.to_ascii_uppercase(),
    );

    let missing_from_code: Vec<_> = template_side
        .iter()
        .filter(|name| !code_side.contains(&name.as_str()))
        .collect();
    assert!(
        missing_from_code.is_empty(),
        "wrangler.toml.example declares {category} entries not present in ConfigKeys::ALL_{cat_upper}: \
         {missing_from_code:?}; ConfigKeys::ALL_{cat_upper} currently lists: {code_side:?}",
        cat_upper = category.to_ascii_uppercase(),
    );
}

/// `wrangler.toml.example` の `[[r2_buckets]]` 配列が、`ConfigKeys::ALL_R2_BINDINGS`
/// と双方向に一致することを検証する。
#[test]
fn wrangler_template_r2_bindings_match_config_keys() {
    assert_bidirectional("r2_bindings", ConfigKeys::ALL_R2_BINDINGS, &TEMPLATE.r2_bindings);
}

/// `wrangler.toml.example` の `[[durable_objects.bindings]]` 配列が、
/// `ConfigKeys::ALL_DO_BINDINGS` と双方向に一致することを検証する。
#[test]
fn wrangler_template_do_bindings_match_config_keys() {
    assert_bidirectional("do_bindings", ConfigKeys::ALL_DO_BINDINGS, &TEMPLATE.do_bindings);
}

/// `wrangler.toml.example` の `[vars]` テーブルキーが、`ConfigKeys::SHARED_PUBLIC_VARS_KEYS`
/// + `LOCAL_DEV_ONLY_VARS_KEYS` の和集合と双方向に一致することを検証する。
///
/// **template (`wrangler.toml.example`)** は local dev / 新規メンバー向け。
/// `wrangler dev` を friction なく動かすため、production では secret 化する値も
/// placeholder として `[vars]` に書く。各 deploy 環境の `wrangler.<env>.toml` は
/// 別 test (`wrangler_environment_toml_consistency.rs`) が `SHARED_PUBLIC_VARS_KEYS`
/// 単独と整合することを検証する。
///
/// `[vars]` 値は運用側で書き換える前提なので空文字や placeholder で構わない。
/// 本テストは「キーの集合が一致すること」のみを検証し、値の内容には関与しない。
#[test]
fn wrangler_template_vars_keys_match_config_keys() {
    let expected: Vec<&'static str> = ConfigKeys::SHARED_PUBLIC_VARS_KEYS
        .iter()
        .chain(ConfigKeys::LOCAL_DEV_ONLY_VARS_KEYS.iter())
        .copied()
        .collect();
    assert_bidirectional("vars_keys", &expected, &TEMPLATE.vars_keys);
}

/// Issue #551 で追加した `[triggers] crons` が template に宣言されていることを
/// 固定する。`[event(scheduled)]` ハンドラと cron trigger は同 PR で導入したので、
/// 片方だけが残ったまま運用者が `cp wrangler.toml.example wrangler.toml` した場合
/// に handler が永久 dormant にならないよう、template 側で必須化する。
#[test]
fn wrangler_template_declares_backfill_cron_trigger() {
    assert!(
        !TEMPLATE.crons.is_empty(),
        "wrangler.toml.example must declare [triggers] crons = [...] for the backfill scheduled handler",
    );
}
