//! ランタイム設定の読み取りヘルパ。
//!
//! Workers の `[vars]` / secret から値を取り出すロジックを worker ランタイムから
//! 分離してテスト可能にする。値取得の実体は wasm32 ビルドでのみ行い、
//! 本モジュールが返すのは「取得結果から導出した純粋データ」に閉じる。

use rshogi_csa_server::ClockSpec;

use crate::origin;

/// 起動時にバインディング名として参照する環境変数キー群。
///
/// # 新規定数を追加するときは
///
/// 個別 const と併せて、用途別の網羅配列のいずれか 1 つに **必ず追加** する:
/// - R2 binding: [`ConfigKeys::ALL_R2_BINDINGS`]
/// - DO binding: [`ConfigKeys::ALL_DO_BINDINGS`]
/// - deploy 対象の全環境（production / staging）で共有する公開 `[vars]` キー:
///   [`ConfigKeys::SHARED_PUBLIC_VARS_KEYS`]
/// - production / staging では Cloudflare secret 経由、local dev では `[vars]`
///   で動かす値: [`ConfigKeys::LOCAL_DEV_ONLY_VARS_KEYS`]
///
/// `tests/wrangler_template_consistency.rs` (template) と
/// `tests/wrangler_environment_toml_consistency.rs` (production / staging) が
/// これら配列と該当 toml ファイルの双方向整合を検証する。配列追加を忘れると
/// template / 各環境 toml 更新忘れも検出できなくなる。
pub struct ConfigKeys;

impl ConfigKeys {
    /// Origin 許可リスト（カンマ区切り）。
    pub const WS_ALLOWED_ORIGINS: &'static str = "WS_ALLOWED_ORIGINS";
    /// Durable Object バインディング名（GameRoom 1 対局 = 1 インスタンス）。
    pub const GAME_ROOM_BINDING: &'static str = "GAME_ROOM";
    /// Durable Object バインディング名（Lobby マッチング待機キュー、1 instance 固定）。
    pub const LOBBY_BINDING: &'static str = "LOBBY";
    /// LobbyDO 内 in-memory queue の総数上限。超過時 LOGIN_LOBBY を `queue_full`
    /// で reject する。未設定時は 100 が既定値。
    pub const LOBBY_QUEUE_SIZE_LIMIT: &'static str = "LOBBY_QUEUE_SIZE_LIMIT";
    /// 私的対局 (`CHALLENGE_LOBBY`) で発行された token の TTL (秒)。期限超過した
    /// 未消費 token は LobbyDO の Alarm ハンドラで `purge_expired` され、保留中の
    /// WS 接続がいれば切断される。未設定時は 3600 秒 (1 時間)。Issue #582 の
    /// Workers 経路で参照する。
    pub const CHALLENGE_TTL_SEC: &'static str = "CHALLENGE_TTL_SEC";
    /// R2 バケットバインディング名（CSA V2 棋譜保存）。
    pub const KIFU_BUCKET_BINDING: &'static str = "KIFU_BUCKET";
    /// R2 バケットバインディング名（Floodgate 履歴保存）。1 対局 = 1 オブジェクト
    /// （単一行 JSON）を `floodgate-history/YYYY/MM/DD/HHMMSS-<game_id>.json` キーで
    /// 保存し、`list_recent` は day shard を新しい順に走査して N 件取得する。
    pub const FLOODGATE_HISTORY_BUCKET_BINDING: &'static str = "FLOODGATE_HISTORY_BUCKET";
    /// 時計方式。`countdown` / `countdown_msec` / `fischer` / `stopwatch`。
    pub const CLOCK_KIND: &'static str = "CLOCK_KIND";
    /// `countdown` / Fischer 用の持ち時間（秒）。
    pub const TOTAL_TIME_SEC: &'static str = "TOTAL_TIME_SEC";
    /// `countdown` の秒読み、または Fischer の増分（秒）。
    pub const BYOYOMI_SEC: &'static str = "BYOYOMI_SEC";
    /// `countdown_msec` 用の持ち時間（ms）。短時間対局（Floodgate 互換ではない拡張）。
    pub const TOTAL_TIME_MS: &'static str = "TOTAL_TIME_MS";
    /// `countdown_msec` の秒読み（ms）。
    pub const BYOYOMI_MS: &'static str = "BYOYOMI_MS";
    /// StopWatch 用の持ち時間（分）。
    pub const TOTAL_TIME_MIN: &'static str = "TOTAL_TIME_MIN";
    /// StopWatch 用の秒読み（分）。
    pub const BYOYOMI_MIN: &'static str = "BYOYOMI_MIN";
    /// 持ち時間プリセット宣言（JSON 配列文字列）。`game_name` 別に `ClockSpec` を
    /// 切り替えるために使う。値の例:
    /// ```jsonc
    /// [
    ///   {"game_name":"byoyomi-600-10","kind":"countdown","total_time_sec":600,"byoyomi_sec":10},
    ///   {"game_name":"fischer-300-10F","kind":"fischer","total_time_sec":300,"increment_sec":10}
    /// ]
    /// ```
    /// 未設定 / 空文字 / 空配列のときは「プリセット未宣言」となり、`CLOCK_KIND`
    /// 等から導出する global clock を全 `game_name` に適用する後方互換動作にとどまる。
    /// 1 件以上登録された場合は strict mode となり、未登録 `game_name` の LOGIN は
    /// `LOGIN_LOBBY:incorrect unknown_game_name` で拒否される。
    pub const CLOCK_PRESETS: &'static str = "CLOCK_PRESETS";
    /// 運営権限を持つハンドル名（`%%SETBUOY` / `%%DELETEBUOY`）。
    ///
    /// **production**: Cloudflare secret として `wrangler secret put ADMIN_HANDLE`
    /// で設定する。OSS repo に handle 名が出ない経路で defense-in-depth を保つ。
    /// **local dev**: `wrangler.toml.example` の `[vars]` に placeholder を残し、
    /// `wrangler dev` を friction なく動かせるようにする。Worker code は
    /// `env.var(ConfigKeys::ADMIN_HANDLE)` で var/secret どちらも読む（Cloudflare
    /// 仕様で同じ namespace に展開される）。
    pub const ADMIN_HANDLE: &'static str = "ADMIN_HANDLE";
    /// 切断時の再接続猶予秒数。`0` または未設定なら再接続プロトコルを無効化し、
    /// WebSocket close を即時 `#ABNORMAL` に流す（保守的既定）。`> 0` を指定する
    /// 構成は `--allow-floodgate-features` (Workers では `ALLOW_FLOODGATE_FEATURES`)
    /// を要求する Floodgate features の opt-in 経路に乗る。
    pub const RECONNECT_GRACE_SECONDS: &'static str = "RECONNECT_GRACE_SECONDS";
    /// Floodgate 機能群を opt-in 有効化するブール変数。`true` / `1` / `yes` / `on`
    /// で有効。`reconnect_protocol` 等の Floodgate 系を要求する構成で必須。
    pub const ALLOW_FLOODGATE_FEATURES: &'static str = "ALLOW_FLOODGATE_FEATURES";
    /// viewer 配信 API (`/api/v1/games*` HTTP, `/ws/<id>/spectate` WS) を opt-in
    /// 有効化するブール変数。`true` / `1` / `yes` / `on` で有効、`false` / `0`
    /// / `no` / `off` または未設定で無効（= 該当 endpoint は 404 で既存ルーティング
    /// にフォールスルー）。production rollout 時の kill-switch を兼ね、本値を
    /// `"0"` に切り替えて redeploy することで viewer API を即時無効化できる。
    /// 設定不正値は安全側に倒し（無効化）、`worker::console_log!` で警告ログを
    /// 出す。
    pub const ALLOW_VIEWER_API: &'static str = "ALLOW_VIEWER_API";

    /// `wrangler.toml` の `[[r2_buckets]] binding = "..."` で宣言されるべき名前の
    /// 網羅列挙。新規 R2 binding 定数を追加したら必ず本配列にも追加する。
    pub const ALL_R2_BINDINGS: &'static [&'static str] = &[
        Self::KIFU_BUCKET_BINDING,
        Self::FLOODGATE_HISTORY_BUCKET_BINDING,
    ];

    /// `wrangler.toml` の `[[durable_objects.bindings]] name = "..."` で宣言される
    /// べき名前の網羅列挙。新規 DO binding 定数を追加したら必ず本配列にも追加する。
    pub const ALL_DO_BINDINGS: &'static [&'static str] =
        &[Self::GAME_ROOM_BINDING, Self::LOBBY_BINDING];

    /// **deploy 対象の全環境**（production / staging）の `wrangler.<env>.toml`
    /// `[vars]` テーブルで宣言されるべきキーの網羅列挙。本配列に含まれる定数は
    /// 全 deploy 環境で `[vars]` として平文管理される（公開しても運用上問題ない値）。
    ///
    /// 本配列に含まれない定数（例: [`Self::ADMIN_HANDLE`]）は production / staging
    /// いずれも `wrangler secret put` 経由で設定し、`wrangler.<env>.toml` には書かない。
    /// ただし [`Self::LOCAL_DEV_ONLY_VARS_KEYS`] に含まれていれば
    /// `wrangler.toml.example` の `[vars]` には placeholder として残し、local dev
    /// 経路で `wrangler dev` を friction なく動かせるようにする。
    ///
    /// 新規定数追加時の振り分け基準:
    /// - 公開しても問題ない値 → 本配列 `SHARED_PUBLIC_VARS_KEYS`
    /// - production / staging では secret 経由、local dev は var で動かしたい値 →
    ///   本配列に入れず [`Self::LOCAL_DEV_ONLY_VARS_KEYS`] に入れる
    /// - production / staging / local dev のいずれも完全に secret （local dev
    ///   でも `.dev.vars` で都度設定）の場合 → どちらの配列にも入れない（現状
    ///   そのケースなし）。このケースを追加する際は、`ConfigKeys` 全 const を
    ///   走査して **どの `ALL_*` 配列にも属さない定数を網羅** するための第 3 の
    ///   test (例: `wrangler_secret_only_keys_are_documented`) を新設し、漏れなく
    ///   登録対象を gate する仕組みを併せて整える。
    pub const SHARED_PUBLIC_VARS_KEYS: &'static [&'static str] = &[
        Self::WS_ALLOWED_ORIGINS,
        Self::CLOCK_KIND,
        Self::TOTAL_TIME_SEC,
        Self::BYOYOMI_SEC,
        Self::TOTAL_TIME_MS,
        Self::BYOYOMI_MS,
        Self::TOTAL_TIME_MIN,
        Self::BYOYOMI_MIN,
        Self::CLOCK_PRESETS,
        Self::RECONNECT_GRACE_SECONDS,
        Self::ALLOW_FLOODGATE_FEATURES,
        Self::ALLOW_VIEWER_API,
        Self::LOBBY_QUEUE_SIZE_LIMIT,
        Self::CHALLENGE_TTL_SEC,
    ];

    /// **local dev のみ** の `wrangler.toml.example` `[vars]` テーブルに追加で
    /// 宣言されるキーの網羅列挙。production / staging では Cloudflare secret 経由
    /// で設定するため `wrangler.<env>.toml` には書かない。
    ///
    /// `wrangler.toml.example` には `SHARED_PUBLIC_VARS_KEYS ∪ LOCAL_DEV_ONLY_VARS_KEYS`
    /// 全件を `[vars]` として記載することで、新規メンバーが `cp wrangler.toml.example
    /// wrangler.toml && wrangler dev` で即動作確認できる friction レス運用を維持する。
    pub const LOCAL_DEV_ONLY_VARS_KEYS: &'static [&'static str] = &[Self::ADMIN_HANDLE];
}

/// `CHALLENGE_TTL_SEC` 既定値 (1 時間)。`parse_challenge_ttl_duration` で
/// `None` / 空文字 / パース失敗時のフォールバックとして参照する。
pub(crate) const DEFAULT_CHALLENGE_TTL_SEC: u64 = 3600;

/// `CHALLENGE_TTL_SEC` 文字列を `Duration` へ解決する。`None` / 空文字 /
/// 非数値 / `u64` 範囲外は [`DEFAULT_CHALLENGE_TTL_SEC`] にフォールバック
/// する (challenge TTL 不正で全 `CHALLENGE_LOBBY` 発行が止まる事態を避ける
/// 安全側挙動)。`= 0` は purge が即時となり全 token が短命になる選択を
/// 運用側に与えるため許容する。
pub fn parse_challenge_ttl_duration(raw: Option<&str>) -> std::time::Duration {
    let trimmed = raw.unwrap_or("").trim();
    if trimmed.is_empty() {
        return std::time::Duration::from_secs(DEFAULT_CHALLENGE_TTL_SEC);
    }
    match trimmed.parse::<u64>() {
        Ok(secs) => std::time::Duration::from_secs(secs),
        Err(_) => std::time::Duration::from_secs(DEFAULT_CHALLENGE_TTL_SEC),
    }
}

/// `RECONNECT_GRACE_SECONDS` 文字列を `Duration` へ解決する。`None` または空文字
/// は `Duration::ZERO`（再接続プロトコル無効化）として扱う。負値・非数値文字列・
/// `u64` の範囲外は `Err` で拒否する（実用上は分〜時間オーダーの設定だけを期待）。
pub fn parse_reconnect_grace_duration(raw: Option<&str>) -> Result<std::time::Duration, String> {
    let trimmed = raw.unwrap_or("").trim();
    if trimmed.is_empty() {
        return Ok(std::time::Duration::ZERO);
    }
    let secs: u64 = trimmed
        .parse()
        .map_err(|e| format!("RECONNECT_GRACE_SECONDS: invalid u64 {trimmed:?}: {e}"))?;
    Ok(std::time::Duration::from_secs(secs))
}

/// Workers `[vars]` 文字列群から時計設定を解決する。
///
/// `CLOCK_KIND` のバリアント別に参照する env 変数:
/// - `countdown`: `TOTAL_TIME_SEC` / `BYOYOMI_SEC` (秒、Floodgate 互換)
/// - `countdown_msec`: `TOTAL_TIME_MS` / `BYOYOMI_MS` (ms、短時間対局向け拡張)
/// - `fischer`: `TOTAL_TIME_SEC` / `BYOYOMI_SEC` (秒、`BYOYOMI_SEC` は Fischer increment)
/// - `stopwatch`: `TOTAL_TIME_MIN` / `BYOYOMI_MIN` (分)
pub fn parse_clock_spec(
    clock_kind: Option<&str>,
    total_time_sec: Option<&str>,
    byoyomi_sec: Option<&str>,
    total_time_ms: Option<&str>,
    byoyomi_ms: Option<&str>,
    total_time_min: Option<&str>,
    byoyomi_min: Option<&str>,
) -> Result<ClockSpec, String> {
    fn parse_u32(name: &str, raw: Option<&str>, default: u32) -> Result<u32, String> {
        match raw {
            Some(s) => s.parse::<u32>().map_err(|e| format!("{name}: invalid u32 {s:?}: {e}")),
            None => Ok(default),
        }
    }

    match clock_kind.unwrap_or("countdown").to_ascii_lowercase().as_str() {
        "countdown" => Ok(ClockSpec::Countdown {
            total_time_sec: parse_u32("TOTAL_TIME_SEC", total_time_sec, 600)?,
            byoyomi_sec: parse_u32("BYOYOMI_SEC", byoyomi_sec, 10)?,
        }),
        "countdown_msec" => Ok(ClockSpec::CountdownMsec {
            total_time_ms: parse_u32("TOTAL_TIME_MS", total_time_ms, 600_000)?,
            byoyomi_ms: parse_u32("BYOYOMI_MS", byoyomi_ms, 10_000)?,
        }),
        "fischer" => Ok(ClockSpec::Fischer {
            total_time_sec: parse_u32("TOTAL_TIME_SEC", total_time_sec, 600)?,
            increment_sec: parse_u32("BYOYOMI_SEC", byoyomi_sec, 10)?,
        }),
        "stopwatch" => Ok(ClockSpec::StopWatch {
            total_time_min: parse_u32("TOTAL_TIME_MIN", total_time_min, 10)?,
            byoyomi_min: parse_u32("BYOYOMI_MIN", byoyomi_min, 1)?,
        }),
        other => Err(format!(
            "CLOCK_KIND: expected countdown|countdown_msec|fischer|stopwatch, got {other:?}"
        )),
    }
}

/// `CLOCK_PRESETS` 環境変数 (JSON 配列文字列) から `game_name → ClockSpec` の
/// マップを構築する。
///
/// `None` / 空文字 / `[]` は空 HashMap を返す（プリセット未宣言モード）。
/// 1 件以上を含む場合は呼び出し側 (lobby / game_room) が strict mode に切り替わり、
/// 未登録 `game_name` の LOGIN を拒否する。
///
/// バリデーション:
/// - JSON パース失敗は `Err`。
/// - 同一 `game_name` の重複は `Err`。
/// - `total_time_*` が 0 の preset は `Err`（少なくとも 1 単位以上の本体時間を要求）。
///   `byoyomi_*` / `increment_sec` の 0 は sudden death として許容。
pub fn parse_clock_presets(
    raw: Option<&str>,
) -> Result<std::collections::HashMap<String, ClockSpec>, String> {
    use serde::Deserialize;
    let trimmed = raw.unwrap_or("").trim();
    if trimmed.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    #[derive(Deserialize)]
    struct Entry {
        game_name: String,
        #[serde(flatten)]
        spec: ClockSpec,
    }
    let entries: Vec<Entry> =
        serde_json::from_str(trimmed).map_err(|e| format!("CLOCK_PRESETS: invalid JSON: {e}"))?;
    let mut out: std::collections::HashMap<String, ClockSpec> =
        std::collections::HashMap::with_capacity(entries.len());
    for entry in entries {
        entry.spec.validate_total_time_nonzero().map_err(|field| {
            format!("CLOCK_PRESETS: clock preset {:?}: {field} must be > 0", entry.game_name)
        })?;
        if out.contains_key(&entry.game_name) {
            return Err(format!(
                "CLOCK_PRESETS: duplicate clock preset entry for game_name {:?}",
                entry.game_name
            ));
        }
        out.insert(entry.game_name, entry.spec);
    }
    Ok(out)
}

/// `resolve_clock_spec_from_presets_map` の戻り値。
/// `parse_clock_presets` の結果から `game_name` の解決結果を 3 状態で表現する。
#[derive(Debug, PartialEq, Eq)]
pub enum PresetResolution {
    /// presets 空 → 呼び出し側が `load_clock_spec_from_env` 等で fallback 解決する
    /// （後方互換モード）。
    Fallback,
    /// 該当 `game_name` の preset を返す。
    Hit(ClockSpec),
    /// presets 宣言済みかつ未登録 → strict mode で `Err` 化されるべき。
    Unknown,
}

/// `parse_clock_presets` で得たマップと `game_name` から `PresetResolution` を返す
/// 純粋ロジック部。env-fetch を持たないため、host target テストから直接呼べる。
pub fn resolve_clock_spec_from_presets_map(
    presets: &std::collections::HashMap<String, ClockSpec>,
    game_name: &str,
) -> PresetResolution {
    if presets.is_empty() {
        return PresetResolution::Fallback;
    }
    match presets.get(game_name) {
        Some(spec) => PresetResolution::Hit(spec.clone()),
        None => PresetResolution::Unknown,
    }
}

/// viewer 配信 API (HTTP `/api/v1/games*` および WS `/ws/<id>/spectate`) が
/// 有効化されているかを `[vars]` `ALLOW_VIEWER_API` から判定する。
///
/// 値の解釈は [`rshogi_csa_server::config::parse_truthy_bool_env`] に委ねる。
/// 設定不正値（`true` / `false` / 数字 / yes/no/on/off 以外）は安全側に倒し
/// （= viewer API 無効化）、`worker::console_log!` で警告ログを 1 回だけ出す。
/// production rollout 時の kill-switch を兼ねる: 値を `"0"` に切り替えて
/// redeploy することで該当 endpoint を即時 404 化できる。
#[cfg(target_arch = "wasm32")]
pub fn is_viewer_api_enabled(env: &worker::Env) -> bool {
    let raw = env.var(ConfigKeys::ALLOW_VIEWER_API).ok().map(|v| v.to_string());
    match rshogi_csa_server::config::parse_truthy_bool_env(raw.as_deref()) {
        Ok(v) => v,
        Err(e) => {
            worker::console_log!("[viewer_api] event=invalid_allow_viewer_api err={e}");
            false
        }
    }
}

/// 取得済みの Origin 許可リスト設定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginAllowList {
    entries: Vec<String>,
}

impl OriginAllowList {
    /// CSV（例: `"https://a.example,https://b.example"`）から構築する。
    pub fn from_csv(csv: &str) -> Self {
        Self {
            entries: origin::parse_allow_list(csv),
        }
    }

    /// 空かどうか。空のときブラウザ経由（Origin 付き）は全拒否、Origin 欠落の
    /// ネイティブクライアントは素通し（[`origin::evaluate`] の仕様）。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 許可リストをイテレートする。
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_csv_yields_empty_list() {
        let list = OriginAllowList::from_csv("");
        assert!(list.is_empty());
    }

    #[test]
    fn csv_parsing_round_trips() {
        let list = OriginAllowList::from_csv("https://a.example, https://b.example");
        let collected: Vec<&str> = list.iter().collect();
        assert_eq!(collected, vec!["https://a.example", "https://b.example"]);
    }

    #[test]
    fn parse_clock_spec_defaults_to_countdown() {
        assert_eq!(
            parse_clock_spec(None, None, None, None, None, None, None).unwrap(),
            ClockSpec::Countdown {
                total_time_sec: 600,
                byoyomi_sec: 10,
            }
        );
    }

    #[test]
    fn parse_clock_spec_accepts_countdown_msec() {
        assert_eq!(
            parse_clock_spec(
                Some("countdown_msec"),
                None,
                None,
                Some("10000"),
                Some("100"),
                None,
                None,
            )
            .unwrap(),
            ClockSpec::CountdownMsec {
                total_time_ms: 10_000,
                byoyomi_ms: 100,
            }
        );
    }

    #[test]
    fn parse_clock_spec_countdown_msec_uses_defaults_when_unset() {
        // CLOCK_KIND=countdown_msec で値未指定なら 600_000 / 10_000 (= 600s / 10s 相当) で
        // production の挙動と整合する。
        assert_eq!(
            parse_clock_spec(Some("countdown_msec"), None, None, None, None, None, None).unwrap(),
            ClockSpec::CountdownMsec {
                total_time_ms: 600_000,
                byoyomi_ms: 10_000,
            }
        );
    }

    #[test]
    fn parse_clock_spec_accepts_fischer() {
        assert_eq!(
            parse_clock_spec(Some("fischer"), Some("300"), Some("5"), None, None, None, None)
                .unwrap(),
            ClockSpec::Fischer {
                total_time_sec: 300,
                increment_sec: 5,
            }
        );
    }

    #[test]
    fn parse_clock_spec_accepts_stopwatch() {
        assert_eq!(
            parse_clock_spec(Some("stopwatch"), None, None, None, None, Some("15"), Some("2"))
                .unwrap(),
            ClockSpec::StopWatch {
                total_time_min: 15,
                byoyomi_min: 2,
            }
        );
    }

    #[test]
    fn parse_clock_spec_rejects_unknown_kind() {
        let err = parse_clock_spec(Some("weird"), None, None, None, None, None, None).unwrap_err();
        assert!(err.contains("countdown|countdown_msec|fischer|stopwatch"));
    }

    #[test]
    fn parse_reconnect_grace_duration_defaults_to_zero() {
        assert_eq!(parse_reconnect_grace_duration(None).unwrap(), std::time::Duration::ZERO);
        assert_eq!(parse_reconnect_grace_duration(Some("")).unwrap(), std::time::Duration::ZERO);
        assert_eq!(
            parse_reconnect_grace_duration(Some(" \t ")).unwrap(),
            std::time::Duration::ZERO,
        );
    }

    #[test]
    fn parse_reconnect_grace_duration_accepts_positive_seconds() {
        assert_eq!(
            parse_reconnect_grace_duration(Some("60")).unwrap(),
            std::time::Duration::from_secs(60),
        );
        assert_eq!(
            parse_reconnect_grace_duration(Some(" 30\n")).unwrap(),
            std::time::Duration::from_secs(30),
        );
    }

    #[test]
    fn parse_reconnect_grace_duration_rejects_non_numeric() {
        let err = parse_reconnect_grace_duration(Some("forever")).unwrap_err();
        assert!(err.contains("RECONNECT_GRACE_SECONDS"));
    }

    /// `CHALLENGE_TTL_SEC` が未設定 / 空文字なら既定 3600 秒。
    #[test]
    fn parse_challenge_ttl_duration_defaults_when_unset() {
        assert_eq!(
            parse_challenge_ttl_duration(None),
            std::time::Duration::from_secs(DEFAULT_CHALLENGE_TTL_SEC)
        );
        assert_eq!(
            parse_challenge_ttl_duration(Some("")),
            std::time::Duration::from_secs(DEFAULT_CHALLENGE_TTL_SEC)
        );
        assert_eq!(
            parse_challenge_ttl_duration(Some("  ")),
            std::time::Duration::from_secs(DEFAULT_CHALLENGE_TTL_SEC)
        );
    }

    /// 数値はそのまま秒として採用する。`= 0` も許容 (purge 即時で全 token 短命)。
    #[test]
    fn parse_challenge_ttl_duration_accepts_seconds() {
        assert_eq!(parse_challenge_ttl_duration(Some("60")), std::time::Duration::from_secs(60));
        assert_eq!(parse_challenge_ttl_duration(Some("0")), std::time::Duration::ZERO);
    }

    /// 非数値はパース失敗時のフォールバック (= 既定値) で扱う。
    #[test]
    fn parse_challenge_ttl_duration_falls_back_on_invalid() {
        assert_eq!(
            parse_challenge_ttl_duration(Some("forever")),
            std::time::Duration::from_secs(DEFAULT_CHALLENGE_TTL_SEC)
        );
        assert_eq!(
            parse_challenge_ttl_duration(Some("-1")),
            std::time::Duration::from_secs(DEFAULT_CHALLENGE_TTL_SEC)
        );
    }

    #[test]
    fn parse_clock_presets_empty_inputs_return_empty_map() {
        assert!(parse_clock_presets(None).unwrap().is_empty());
        assert!(parse_clock_presets(Some("")).unwrap().is_empty());
        assert!(parse_clock_presets(Some("   \n  ")).unwrap().is_empty());
        assert!(parse_clock_presets(Some("[]")).unwrap().is_empty());
    }

    #[test]
    fn parse_clock_presets_accepts_three_variants() {
        let raw = r#"[
            {"game_name":"byoyomi-600-10","kind":"countdown","total_time_sec":600,"byoyomi_sec":10},
            {"game_name":"byoyomi-60-5","kind":"countdown","total_time_sec":60,"byoyomi_sec":5},
            {"game_name":"fischer-300-10F","kind":"fischer","total_time_sec":300,"increment_sec":10}
        ]"#;
        let map = parse_clock_presets(Some(raw)).unwrap();
        assert_eq!(map.len(), 3);
        assert!(matches!(
            map.get("byoyomi-600-10"),
            Some(ClockSpec::Countdown {
                total_time_sec: 600,
                byoyomi_sec: 10
            })
        ));
        assert!(matches!(
            map.get("fischer-300-10F"),
            Some(ClockSpec::Fischer {
                total_time_sec: 300,
                increment_sec: 10
            })
        ));
    }

    #[test]
    fn parse_clock_presets_rejects_duplicate_game_name() {
        let raw = r#"[
            {"game_name":"x","kind":"countdown","total_time_sec":600,"byoyomi_sec":10},
            {"game_name":"x","kind":"fischer","total_time_sec":60,"increment_sec":5}
        ]"#;
        let err = parse_clock_presets(Some(raw)).unwrap_err();
        assert!(err.contains("duplicate"), "error must mention duplicate: {err}");
        assert!(err.contains("\"x\""), "error must mention game_name: {err}");
    }

    /// `total_time_sec = 0` を loader 経由で弾き、ラッパーが組み立てるメッセージ
    /// (`CLOCK_PRESETS: clock preset "x": <field> must be > 0`) の prefix と
    /// phrase が崩れないことを回帰防止する E2E 1 件。`ClockSpec` 全 variant は
    /// core 側 table テストでカバー。
    #[test]
    fn parse_clock_presets_rejects_zero_total_time() {
        let raw = r#"[
            {"game_name":"broken","kind":"countdown","total_time_sec":0,"byoyomi_sec":10}
        ]"#;
        let err = parse_clock_presets(Some(raw)).unwrap_err();
        assert!(err.starts_with("CLOCK_PRESETS:"), "error must keep prefix: {err}");
        assert!(err.contains("total_time_sec"), "error must mention field: {err}");
        assert!(err.contains("broken"), "error must mention game_name: {err}");
        assert!(err.contains("must be > 0"), "error must include comparison phrase: {err}");
    }

    #[test]
    fn parse_clock_presets_rejects_invalid_json() {
        let err = parse_clock_presets(Some("not json")).unwrap_err();
        assert!(err.contains("CLOCK_PRESETS"), "error must mention env name: {err}");
    }

    #[test]
    fn parse_clock_presets_allows_zero_byoyomi() {
        let raw = r#"[
            {"game_name":"sd","kind":"countdown","total_time_sec":600,"byoyomi_sec":0}
        ]"#;
        let map = parse_clock_presets(Some(raw)).unwrap();
        assert_eq!(map.len(), 1);
    }

    /// presets が空 (= 未宣言モード) のとき `Fallback` を返し、呼び出し側に
    /// global clock 解決を委ねる。
    #[test]
    fn resolve_returns_fallback_when_presets_empty() {
        let presets: std::collections::HashMap<String, ClockSpec> =
            std::collections::HashMap::new();
        assert_eq!(
            resolve_clock_spec_from_presets_map(&presets, "anything"),
            PresetResolution::Fallback
        );
    }

    /// 登録済 `game_name` は該当 spec を `Hit` で返す。
    #[test]
    fn resolve_hits_registered_game_name() {
        let mut presets = std::collections::HashMap::new();
        presets.insert(
            "byoyomi-600-10".to_owned(),
            ClockSpec::Countdown {
                total_time_sec: 600,
                byoyomi_sec: 10,
            },
        );
        assert_eq!(
            resolve_clock_spec_from_presets_map(&presets, "byoyomi-600-10"),
            PresetResolution::Hit(ClockSpec::Countdown {
                total_time_sec: 600,
                byoyomi_sec: 10
            })
        );
    }

    /// presets 宣言済みかつ未登録 `game_name` は `Unknown` を返し、
    /// `resolve_clock_spec_for_game` 側で strict mode の Err に変換される。
    #[test]
    fn resolve_unknown_game_name_when_presets_declared() {
        let mut presets = std::collections::HashMap::new();
        presets.insert(
            "byoyomi-600-10".to_owned(),
            ClockSpec::Countdown {
                total_time_sec: 600,
                byoyomi_sec: 10,
            },
        );
        assert_eq!(
            resolve_clock_spec_from_presets_map(&presets, "unregistered"),
            PresetResolution::Unknown
        );
    }
}
