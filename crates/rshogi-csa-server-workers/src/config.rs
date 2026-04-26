//! ランタイム設定の読み取りヘルパ。
//!
//! Workers の `[vars]` / secret から値を取り出すロジックを worker ランタイムから
//! 分離してテスト可能にする。値取得の実体は wasm32 ビルドでのみ行い、
//! 本モジュールが返すのは「取得結果から導出した純粋データ」に閉じる。

use rshogi_csa_server::ClockSpec;

use crate::origin;

/// 起動時にバインディング名として参照する環境変数キー群。
pub struct ConfigKeys;

impl ConfigKeys {
    /// Origin 許可リスト（カンマ区切り）。
    pub const CORS_ORIGINS: &'static str = "CORS_ORIGINS";
    /// Durable Object バインディング名（GameRoom 1 対局 = 1 インスタンス）。
    pub const GAME_ROOM_BINDING: &'static str = "GAME_ROOM";
    /// R2 バケットバインディング名（CSA V2 棋譜保存）。
    pub const KIFU_BUCKET_BINDING: &'static str = "KIFU_BUCKET";
    /// R2 バケットバインディング名（Floodgate 履歴保存）。1 対局 = 1 オブジェクトの
    /// JSONL を `floodgate-history/YYYY/MM/DD/HHMMSS-<game_id>.json` キーで保存し、
    /// `list_recent` は day shard を新しい順に走査して N 件取得する。
    pub const FLOODGATE_HISTORY_BUCKET_BINDING: &'static str = "FLOODGATE_HISTORY_BUCKET";
    /// 時計方式。`countdown` / `fischer` / `stopwatch`。
    pub const CLOCK_KIND: &'static str = "CLOCK_KIND";
    /// 秒読み / Fischer 用の持ち時間（秒）。
    pub const TOTAL_TIME_SEC: &'static str = "TOTAL_TIME_SEC";
    /// 秒読みの秒読み、または Fischer の増分（秒）。
    pub const BYOYOMI_SEC: &'static str = "BYOYOMI_SEC";
    /// StopWatch 用の持ち時間（分）。
    pub const TOTAL_TIME_MIN: &'static str = "TOTAL_TIME_MIN";
    /// StopWatch 用の秒読み（分）。
    pub const BYOYOMI_MIN: &'static str = "BYOYOMI_MIN";
    /// 運営権限を持つハンドル名（`%%SETBUOY` / `%%DELETEBUOY`）。
    pub const ADMIN_HANDLE: &'static str = "ADMIN_HANDLE";
}

/// Workers `[vars]` 文字列群から時計設定を解決する。
pub fn parse_clock_spec(
    clock_kind: Option<&str>,
    total_time_sec: Option<&str>,
    byoyomi_sec: Option<&str>,
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
        "fischer" => Ok(ClockSpec::Fischer {
            total_time_sec: parse_u32("TOTAL_TIME_SEC", total_time_sec, 600)?,
            increment_sec: parse_u32("BYOYOMI_SEC", byoyomi_sec, 10)?,
        }),
        "stopwatch" => Ok(ClockSpec::StopWatch {
            total_time_min: parse_u32("TOTAL_TIME_MIN", total_time_min, 10)?,
            byoyomi_min: parse_u32("BYOYOMI_MIN", byoyomi_min, 1)?,
        }),
        other => Err(format!("CLOCK_KIND: expected countdown|fischer|stopwatch, got {other:?}")),
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
            parse_clock_spec(None, None, None, None, None).unwrap(),
            ClockSpec::Countdown {
                total_time_sec: 600,
                byoyomi_sec: 10,
            }
        );
    }

    #[test]
    fn parse_clock_spec_accepts_fischer() {
        assert_eq!(
            parse_clock_spec(Some("fischer"), Some("300"), Some("5"), None, None).unwrap(),
            ClockSpec::Fischer {
                total_time_sec: 300,
                increment_sec: 5,
            }
        );
    }

    #[test]
    fn parse_clock_spec_accepts_stopwatch() {
        assert_eq!(
            parse_clock_spec(Some("stopwatch"), None, None, Some("15"), Some("2")).unwrap(),
            ClockSpec::StopWatch {
                total_time_min: 15,
                byoyomi_min: 2,
            }
        );
    }

    #[test]
    fn parse_clock_spec_rejects_unknown_kind() {
        let err = parse_clock_spec(Some("weird"), None, None, None, None).unwrap_err();
        assert!(err.contains("countdown|fischer|stopwatch"));
    }
}
