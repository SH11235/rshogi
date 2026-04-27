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

    /// `wrangler.toml` の `[[r2_buckets]] binding = "..."` で宣言されるべき名前の
    /// 網羅列挙。新規 R2 binding 定数を追加したら必ず本配列にも追加する。
    pub const ALL_R2_BINDINGS: &'static [&'static str] = &[
        Self::KIFU_BUCKET_BINDING,
        Self::FLOODGATE_HISTORY_BUCKET_BINDING,
    ];

    /// `wrangler.toml` の `[[durable_objects.bindings]] name = "..."` で宣言される
    /// べき名前の網羅列挙。新規 DO binding 定数を追加したら必ず本配列にも追加する。
    pub const ALL_DO_BINDINGS: &'static [&'static str] = &[Self::GAME_ROOM_BINDING];

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
        Self::RECONNECT_GRACE_SECONDS,
        Self::ALLOW_FLOODGATE_FEATURES,
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

    /// 空かどうか。本番運用で空は実質全拒否となる（[`origin::evaluate`] の仕様）。
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
}
