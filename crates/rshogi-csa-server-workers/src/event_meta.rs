//! イベント (大会) のメタデータ。
//!
//! `games_search_index.event` と同じ文字列を主キーに、正式名称・主催・公式ページ・
//! 棋譜の配布元・利用条件の要約を D1 の `event_meta` テーブルに持つ。対局が 1 つも
//! 無い `event` にも登録できる (棋譜の投入より先にメタを入れられる)。
//!
//! - `PUT /api/v1/admin/events/{event}` で upsert する (管理トークン必須)。
//! - `GET /api/v1/games/events` の各要素に `meta` として付く (登録が無い `event` は
//!   `meta` を省略)。
//!
//! 本モジュールは I/O を持たない (入力検証・SQL 文字列・wire 形状)。D1 への読み書きは
//! wasm32 専用の `event_meta_api` / `viewer_api` が行う。

use serde::{Deserialize, Serialize};

use crate::games_search_index::EventTag;
use crate::kifu_import::normalize_event;

/// `title` / `organizer` の最大文字数。
pub const MAX_NAME_CHARS: usize = 200;
/// `official_url` / `kifu_source_url` の最大文字数。
pub const MAX_URL_CHARS: usize = 500;
/// `terms_note` の最大文字数。
pub const MAX_TERMS_NOTE_CHARS: usize = 1000;

/// メタの upsert。7 個の `?` は `event, title, organizer, official_url,
/// kifu_source_url, terms_note, updated_at_ms` の順。
pub const EVENT_META_UPSERT_SQL: &str = "INSERT INTO event_meta (event, title, organizer, official_url, kifu_source_url, terms_note, updated_at_ms) VALUES (?, ?, ?, ?, ?, ?, ?) ON CONFLICT(event) DO UPDATE SET title=excluded.title, organizer=excluded.organizer, official_url=excluded.official_url, kifu_source_url=excluded.kifu_source_url, terms_note=excluded.terms_note, updated_at_ms=excluded.updated_at_ms";

/// 登録済みメタの全件取得。イベントは大会・企画の単位で、行数は対局数に比例しない
/// (数十〜数百行) ので絞り込まずに読む。
pub const EVENT_META_SELECT_SQL: &str =
    "SELECT event, title, organizer, official_url, kifu_source_url, terms_note FROM event_meta";

/// イベントのメタ (wire 形状)。任意項目は未登録なら `null`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventMeta {
    /// 正式名称。
    pub title: String,
    /// 主催。
    pub organizer: Option<String>,
    /// 大会の公式ページ。
    pub official_url: Option<String>,
    /// 棋譜の配布元ページ。
    pub kifu_source_url: Option<String>,
    /// 利用条件の要約。
    pub terms_note: Option<String>,
}

/// `PUT /api/v1/admin/events/{event}` のリクエスト本文。
#[derive(Debug, Default, Deserialize)]
pub struct EventMetaRequest {
    pub title: Option<String>,
    #[serde(default)]
    pub organizer: Option<String>,
    #[serde(default)]
    pub official_url: Option<String>,
    #[serde(default)]
    pub kifu_source_url: Option<String>,
    #[serde(default)]
    pub terms_note: Option<String>,
}

impl EventMetaRequest {
    /// 入力を検証して保存する形にする。`Err` はクライアントへ返す 400 の本文。
    ///
    /// 各文字列は前後の空白を除く。任意項目は空なら未登録 (`None`) として扱う。
    /// 制御文字 (改行・タブを含む) と長さ上限超えは拒否する。URL は `https://` か
    /// `http://` で始まる絶対 URL だけを受け付ける (フロントがそのままリンクにするので、
    /// `javascript:` などの scheme を入れさせない)。
    pub fn validate(self) -> Result<EventMeta, String> {
        let title = normalize_text("title", self.title, MAX_NAME_CHARS)?
            .ok_or_else(|| "title is required".to_owned())?;
        let organizer = normalize_text("organizer", self.organizer, MAX_NAME_CHARS)?;
        let official_url = normalize_url("official_url", self.official_url)?;
        let kifu_source_url = normalize_url("kifu_source_url", self.kifu_source_url)?;
        let terms_note = normalize_text("terms_note", self.terms_note, MAX_TERMS_NOTE_CHARS)?;
        Ok(EventMeta {
            title,
            organizer,
            official_url,
            kifu_source_url,
            terms_note,
        })
    }
}

fn normalize_text(
    field: &str,
    raw: Option<String>,
    max_chars: usize,
) -> Result<Option<String>, String> {
    let Some(value) = raw.as_deref().map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if value.chars().count() > max_chars {
        return Err(format!("{field} must be at most {max_chars} characters"));
    }
    if value.chars().any(char::is_control) {
        return Err(format!("{field} must not contain control characters"));
    }
    Ok(Some(value.to_owned()))
}

fn normalize_url(field: &str, raw: Option<String>) -> Result<Option<String>, String> {
    let Some(url) = normalize_text(field, raw, MAX_URL_CHARS)? else {
        return Ok(None);
    };
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .ok_or_else(|| format!("{field} must be an absolute https:// or http:// URL"))?;
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    if host.is_empty() || url.chars().any(char::is_whitespace) {
        return Err(format!("{field} must be an absolute https:// or http:// URL"));
    }
    Ok(Some(url))
}

/// `PUT /api/v1/admin/events/{event}` の path 末尾 (`{event}` の部分、percent-encode
/// されたまま) を `event` に直す。
///
/// 複数階層 (`/` を含む) や壊れた percent-encoding は `Err`。decode した後は
/// アップロードの `event` と同じ [`normalize_event`] の規則 (前後の空白除去、空は不可、
/// 長さ上限、制御文字の拒否) を通す。
pub fn event_from_path_segment(segment: &str) -> Result<String, String> {
    if segment.contains('/') {
        return Err("event must be a single path segment".to_owned());
    }
    let decoded = percent_decode(segment)
        .ok_or_else(|| "event is not valid percent-encoded UTF-8".to_owned())?;
    match normalize_event(Some(&decoded))? {
        Some(event) => Ok(event.to_owned()),
        None => Err("event is required".to_owned()),
    }
}

fn percent_decode(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = raw.get(index + 1..index + 3)?;
            if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

/// D1 `event_meta` の 1 行 ([`EVENT_META_SELECT_SQL`] の結果)。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct EventMetaRow {
    pub event: String,
    pub title: String,
    pub organizer: Option<String>,
    pub official_url: Option<String>,
    pub kifu_source_url: Option<String>,
    pub terms_note: Option<String>,
}

/// イベントタグ一覧の各要素に、同じ `event` のメタを付ける。
///
/// 一覧の件数と並び順は変えない。同じ `event` が `source` 違いで複数あれば、それぞれに
/// 同じメタが付く。対局の無い `event` のメタは一覧に出ない。
pub fn attach_event_meta(tags: &mut [EventTag], rows: Vec<EventMetaRow>) {
    let metas: std::collections::HashMap<String, EventMeta> = rows
        .into_iter()
        .map(|row| {
            (
                row.event,
                EventMeta {
                    title: row.title,
                    organizer: row.organizer,
                    official_url: row.official_url,
                    kifu_source_url: row.kifu_source_url,
                    terms_note: row.terms_note,
                },
            )
        })
        .collect();
    for tag in tags {
        tag.meta = metas.get(&tag.event).cloned();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(json: serde_json::Value) -> EventMetaRequest {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn validate_accepts_full_and_title_only_requests() {
        let meta = request(serde_json::json!({
            "title": " 第35回世界コンピュータ将棋選手権 ",
            "organizer": "コンピュータ将棋協会",
            "official_url": "https://example.org/wcsc35/",
            "kifu_source_url": "http://example.org/wcsc35/kifu.html?x=1#top",
            "terms_note": "出典を明記して利用",
        }))
        .validate()
        .unwrap();
        assert_eq!(meta.title, "第35回世界コンピュータ将棋選手権");
        assert_eq!(meta.organizer.as_deref(), Some("コンピュータ将棋協会"));
        assert_eq!(meta.official_url.as_deref(), Some("https://example.org/wcsc35/"));
        assert_eq!(
            meta.kifu_source_url.as_deref(),
            Some("http://example.org/wcsc35/kifu.html?x=1#top")
        );
        assert_eq!(meta.terms_note.as_deref(), Some("出典を明記して利用"));

        let minimal = request(serde_json::json!({ "title": "WCSC35" })).validate().unwrap();
        assert_eq!(
            minimal,
            EventMeta {
                title: "WCSC35".to_owned(),
                organizer: None,
                official_url: None,
                kifu_source_url: None,
                terms_note: None,
            }
        );
        // 空文字・空白だけ・null の任意項目は未登録として扱う。
        let blank = request(serde_json::json!({
            "title": "WCSC35", "organizer": "  ", "official_url": "", "terms_note": null,
        }))
        .validate()
        .unwrap();
        assert_eq!(blank, minimal);
    }

    #[test]
    fn validate_requires_title() {
        for json in [
            serde_json::json!({}),
            serde_json::json!({ "title": "" }),
            serde_json::json!({ "title": "   " }),
            serde_json::json!({ "title": null }),
        ] {
            assert_eq!(request(json).validate().unwrap_err(), "title is required");
        }
    }

    #[test]
    fn validate_rejects_non_http_urls() {
        for url in [
            "javascript:alert(1)",
            "data:text/html,x",
            "//example.org/",
            "/wcsc35/",
            "example.org",
            "ftp://example.org/",
            "https://",
            "https:///path",
            "https://example.org/a b",
            "HTTPS://example.org/",
        ] {
            for field in ["official_url", "kifu_source_url"] {
                let err = request(serde_json::json!({ "title": "t", field: url }))
                    .validate()
                    .unwrap_err();
                assert_eq!(
                    err,
                    format!("{field} must be an absolute https:// or http:// URL"),
                    "{url}"
                );
            }
        }
    }

    #[test]
    fn validate_rejects_control_characters_and_overlong_values() {
        for (field, value) in [
            ("title", "a\nb"),
            ("organizer", "a\tb"),
            ("official_url", "https://example.org/\r\nX: y"),
            ("terms_note", "a\u{0}b"),
            ("terms_note", "1 行目\n2 行目"),
        ] {
            let mut json = serde_json::json!({ "title": "t" });
            json[field] = value.into();
            assert_eq!(
                request(json).validate().unwrap_err(),
                format!("{field} must not contain control characters")
            );
        }
        for (field, max) in [
            ("title", MAX_NAME_CHARS),
            ("organizer", MAX_NAME_CHARS),
            ("terms_note", MAX_TERMS_NOTE_CHARS),
        ] {
            // 上限は byte 数ではなく文字数。
            let mut json = serde_json::json!({ "title": "t" });
            json[field] = "あ".repeat(max).into();
            assert!(request(json).validate().is_ok(), "{field}");
            let mut json = serde_json::json!({ "title": "t" });
            json[field] = "あ".repeat(max + 1).into();
            assert_eq!(
                request(json).validate().unwrap_err(),
                format!("{field} must be at most {max} characters")
            );
        }
        let long_url = format!("https://example.org/{}", "a".repeat(MAX_URL_CHARS));
        assert_eq!(
            request(serde_json::json!({ "title": "t", "official_url": long_url }))
                .validate()
                .unwrap_err(),
            format!("official_url must be at most {MAX_URL_CHARS} characters")
        );
    }

    #[test]
    fn event_path_segment_is_decoded_and_normalized_like_upload_event() {
        assert_eq!(event_from_path_segment("WCSC35").unwrap(), "WCSC35");
        assert_eq!(
            event_from_path_segment("%E9%9B%BB%E7%AB%9C%E6%88%A6%20%E7%AC%AC6%E5%9B%9E").unwrap(),
            "電竜戦 第6回"
        );
        // 前後の空白は upload の event と同じく除く。
        assert_eq!(event_from_path_segment("%20WCSC35%20").unwrap(), "WCSC35");
        // encode された `/` は event の一部として通す (階層は増えない)。
        assert_eq!(event_from_path_segment("a%2Fb").unwrap(), "a/b");

        assert_eq!(event_from_path_segment("").unwrap_err(), "event is required");
        assert_eq!(event_from_path_segment("%20").unwrap_err(), "event is required");
        assert_eq!(
            event_from_path_segment("a/b").unwrap_err(),
            "event must be a single path segment"
        );
        for broken in ["%", "%2", "%zz", "%+1", "%FF", "%E9%9B"] {
            assert_eq!(
                event_from_path_segment(broken).unwrap_err(),
                "event is not valid percent-encoded UTF-8",
                "{broken}"
            );
        }
        assert_eq!(
            event_from_path_segment("a%0Ab").unwrap_err(),
            "event must not contain control characters"
        );
        let long = "a".repeat(crate::kifu_import::MAX_EVENT_CHARS + 1);
        assert!(event_from_path_segment(&long).unwrap_err().contains("at most"));
    }

    #[test]
    fn sql_matches_migration_columns() {
        let migration = include_str!("../migrations/0006_event_meta.sql");
        for column in [
            "event TEXT PRIMARY KEY",
            "title TEXT NOT NULL",
            "organizer TEXT",
            "official_url TEXT",
            "kifu_source_url TEXT",
            "terms_note TEXT",
            "updated_at_ms INTEGER NOT NULL",
        ] {
            assert!(migration.contains(column), "migration missing {column}");
        }
        assert!(EVENT_META_UPSERT_SQL.starts_with(
            "INSERT INTO event_meta (event, title, organizer, official_url, kifu_source_url, terms_note, updated_at_ms) VALUES (?, ?, ?, ?, ?, ?, ?) ON CONFLICT(event) DO UPDATE SET "
        ));
        assert_eq!(EVENT_META_UPSERT_SQL.matches('?').count(), 7);
        // 省略された任意項目は前の値を残さず NULL に戻す (PUT は全体の置き換え)。
        for column in [
            "title",
            "organizer",
            "official_url",
            "kifu_source_url",
            "terms_note",
            "updated_at_ms",
        ] {
            assert!(
                EVENT_META_UPSERT_SQL.contains(&format!("{column}=excluded.{column}")),
                "{column}"
            );
        }
        assert_eq!(
            EVENT_META_SELECT_SQL,
            "SELECT event, title, organizer, official_url, kifu_source_url, terms_note FROM event_meta"
        );
    }

    fn tag(event: &str, source: &str, games: u64) -> EventTag {
        EventTag {
            event: event.to_owned(),
            source: source.to_owned(),
            games,
            meta: None,
        }
    }

    #[test]
    fn attach_keeps_order_and_omits_meta_for_unregistered_events() {
        let mut tags = vec![
            tag("floodgate-300-10F", "wdoor", 500),
            tag("WCSC35", "upload", 289),
            tag("WCSC35", "wdoor", 2),
        ];
        // D1 の行 (未登録の任意項目は null) をそのまま読める。
        let rows: Vec<EventMetaRow> = serde_json::from_value(serde_json::json!([
            {
                "event": "WCSC35", "title": "第35回世界コンピュータ将棋選手権",
                "organizer": "コンピュータ将棋協会", "official_url": "https://example.org/",
                "kifu_source_url": null, "terms_note": null,
            },
            {
                "event": "no-games-yet", "title": "対局の無いイベント", "organizer": null,
                "official_url": null, "kifu_source_url": null, "terms_note": null,
            },
        ]))
        .unwrap();
        attach_event_meta(&mut tags, rows);
        assert_eq!(
            serde_json::to_value(&tags).unwrap(),
            serde_json::json!([
                { "event": "floodgate-300-10F", "source": "wdoor", "games": 500 },
                {
                    "event": "WCSC35", "source": "upload", "games": 289,
                    "meta": {
                        "title": "第35回世界コンピュータ将棋選手権",
                        "organizer": "コンピュータ将棋協会",
                        "official_url": "https://example.org/",
                        "kifu_source_url": null,
                        "terms_note": null,
                    },
                },
                {
                    "event": "WCSC35", "source": "wdoor", "games": 2,
                    "meta": {
                        "title": "第35回世界コンピュータ将棋選手権",
                        "organizer": "コンピュータ将棋協会",
                        "official_url": "https://example.org/",
                        "kifu_source_url": null,
                        "terms_note": null,
                    },
                },
            ])
        );
    }
}
