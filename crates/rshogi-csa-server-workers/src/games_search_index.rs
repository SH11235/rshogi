//! D1 を使った終局済棋譜の検索用二次インデックス。
//!
//! R2 の `games-index/*.json` が正本であり、本モジュールの書き込みはすべて
//! best-effort とする。検索レスポンスを R2 一覧と同じ wire format に戻せるよう、
//! 検索カラムに加えて `end_reason` と `clock_json` も保持する。

use serde::{Deserialize, Serialize};

#[cfg(any(target_arch = "wasm32", test))]
use std::collections::{BTreeMap, BTreeSet};

#[cfg(target_arch = "wasm32")]
use crate::games_index::GamesIndexEntry;
use crate::games_index::is_external_source;

pub const DEFAULT_PAGE_SIZE: u32 = 20;
pub const MAX_PAGE_SIZE: u32 = 100;

/// 検索 API の `source` に一度に指定できる値の上限。bind 数を有界に保つ。
pub const MAX_SOURCE_FILTERS: usize = 8;

/// [`SearchRow`] を読み出す SELECT の列リスト。検索 / プレイヤー詳細 /
/// レーティング集計で共有し、列追加時の取りこぼしを防ぐ。
pub const SEARCH_ROW_COLUMNS: &str = "game_id, started_at_ms, ended_at_ms, sente_name, gote_name, black_player_id, white_player_id, wire_result_kind, end_reason, moves_count, clock_json, source, event, origin_url";

/// プレイヤー詳細の対局一覧の絞り込み条件。
///
/// bind は canonical ID を 4 回 (先手・後手それぞれ本人と alias の subquery)。alias の
/// 家族を D1 側で解決するので、rotation を重ねても bind 数は一定。外部棋譜
/// (`wdoor` / `upload`) は内部レーティングの対象外なので除外する。
pub fn player_games_predicate() -> String {
    format!(
        "((black_player_id = ? OR black_player_id IN (SELECT alias_id FROM player_id_aliases WHERE canonical_id = ?)) OR (white_player_id = ? OR white_player_id IN (SELECT alias_id FROM player_id_aliases WHERE canonical_id = ?))) AND {}",
        crate::games_index::NOT_EXTERNAL_SOURCE_SQL
    )
}

/// イベントタグ一覧 (`GET /api/v1/games/events`) の集計 SQL。
///
/// `event` が空でない対局を `(event, source)` ごとに数え、件数の多い順 (同数は
/// `event` 昇順、さらに `source` 昇順) に返す。`event IS NOT NULL` は部分 index
/// `games_search_index_event_source` を使わせるための条件。
pub const EVENT_TAGS_SQL: &str = "SELECT event, source, COUNT(*) AS games FROM games_search_index WHERE event IS NOT NULL AND event <> '' GROUP BY event, source ORDER BY games DESC, event ASC, source ASC";

/// イベントタグ一覧の 1 件。
#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct EventTag {
    pub event: String,
    pub source: String,
    pub games: u64,
}

/// 検索 API の `source` クエリ値 (カンマ区切り) を重複のない値の列に分解する。
///
/// 空要素は無視する。値が [`MAX_SOURCE_FILTERS`] を超える場合は `Err`。
pub fn parse_source_filter(raw: &str) -> Result<Vec<String>, String> {
    let mut sources: Vec<String> = Vec::new();
    for value in raw.split(',').map(str::trim).filter(|value| !value.is_empty()) {
        if !sources.iter().any(|existing| existing == value) {
            sources.push(value.to_owned());
        }
    }
    if sources.len() > MAX_SOURCE_FILTERS {
        return Err(format!("source accepts at most {MAX_SOURCE_FILTERS} values"));
    }
    Ok(sources)
}

#[cfg(any(target_arch = "wasm32", test))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct AliasLink {
    alias_id: String,
    canonical_id: String,
}

/// A new canonical ID absorbs both the freshly derived aliases and every
/// historical alias whose current target is one of those IDs. The canonical
/// ID itself is never retained as an alias, preventing cycles.
#[cfg(any(target_arch = "wasm32", test))]
fn normalized_alias_links(
    existing: &[AliasLink],
    canonical_id: &str,
    aliases: &[String],
) -> BTreeMap<String, String> {
    let targets: BTreeSet<&str> = std::iter::once(canonical_id)
        .chain(aliases.iter().map(String::as_str))
        .collect();
    let mut normalized = BTreeMap::new();
    for link in existing {
        if link.alias_id == canonical_id {
            continue;
        }
        let target = if targets.contains(link.canonical_id.as_str()) {
            canonical_id.to_owned()
        } else {
            link.canonical_id.clone()
        };
        normalized.insert(link.alias_id.clone(), target);
    }
    for alias in aliases {
        if alias != canonical_id {
            normalized.insert(alias.clone(), canonical_id.to_owned());
        }
    }
    normalized
}

pub fn validate_pagination(page: u32, page_size: u32) -> Result<(), String> {
    if page == 0 {
        return Err("page must be a positive integer".into());
    }
    if !(1..=MAX_PAGE_SIZE).contains(&page_size) {
        return Err(format!("pageSize must be 1..={MAX_PAGE_SIZE}"));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchParams {
    pub name: Option<String>,
    pub result: Option<String>,
    /// 空なら絞り込まない。複数あるときはいずれかに一致する対局を返す。
    pub source: Vec<String>,
    /// `event` 列の完全一致。
    pub event: Option<String>,
    pub from: Option<u64>,
    pub to: Option<u64>,
    pub page: u32,
    pub page_size: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum QueryValue {
    Text(String),
    Integer(f64),
}

#[derive(Debug, PartialEq)]
pub struct SearchQuery {
    pub count_sql: String,
    pub rows_sql: String,
    pub filter_values: Vec<QueryValue>,
    pub limit: u32,
    pub offset: u64,
}

/// 検索条件から placeholder 付き SQL と bind 値を組み立てる。
pub fn build_search_query(params: &SearchParams) -> SearchQuery {
    let source_clause = (!params.source.is_empty())
        .then(|| format!("source IN ({})", vec!["?"; params.source.len()].join(", ")));
    let mut clauses: Vec<&str> = Vec::new();
    let mut values = Vec::new();
    if let Some(name) = &params.name {
        clauses.push("(sente_name LIKE ? ESCAPE '\\' COLLATE NOCASE OR gote_name LIKE ? ESCAPE '\\' COLLATE NOCASE)");
        let escaped = name.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
        let pattern = format!("%{escaped}%");
        values.push(QueryValue::Text(pattern.clone()));
        values.push(QueryValue::Text(pattern));
    }
    if let Some(result) = &params.result {
        clauses.push("result_kind = ?");
        values.push(QueryValue::Text(result.clone()));
    }
    if let Some(clause) = &source_clause {
        clauses.push(clause);
        values.extend(params.source.iter().cloned().map(QueryValue::Text));
    }
    if let Some(event) = &params.event {
        clauses.push("event = ?");
        values.push(QueryValue::Text(event.clone()));
    }
    if let Some(from) = params.from {
        clauses.push("ended_at_ms >= ?");
        values.push(QueryValue::Integer(from as f64));
    }
    if let Some(to) = params.to {
        clauses.push("ended_at_ms <= ?");
        values.push(QueryValue::Integer(to as f64));
    }
    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };
    let columns = SEARCH_ROW_COLUMNS;
    SearchQuery {
        count_sql: format!("SELECT COUNT(*) AS total_count FROM games_search_index{where_sql}"),
        rows_sql: format!(
            "SELECT {columns} FROM games_search_index{where_sql} ORDER BY ended_at_ms DESC, game_id ASC LIMIT ? OFFSET ?"
        ),
        filter_values: values,
        limit: params.page_size,
        offset: u64::from(params.page - 1) * u64::from(params.page_size),
    }
}

/// R2 wire の `end_reason` を検索 API の詳細結果分類へ変換する。
pub fn result_kind_for_search(end_reason: &str) -> &'static str {
    match end_reason {
        "RESIGN" => "resignation",
        "TIME_UP" => "time_expired",
        "ILLEGAL" => "abort",
        "JISHOGI" => "jishogi",
        "OUTE_SENNICHITE" => "oute_sennichite",
        "SENNICHITE" => "draw",
        "MAX_MOVES" => "max_moves",
        "ABNORMAL" => "abnormal",
        _ => "abort",
    }
}

/// D1 の `clock_json` 列に保存する文字列を作る。
///
/// 終局時・取り込み時は構造体 ([`crate::games_index::ClockSpec`])、backfill や取り込みの
/// 修復は R2 の meta を読み戻した JSON 値から作る。どちらも一度 `serde_json::Value` に
/// 通してキーを昇順に揃えるので、同じ持ち時間なら同じ文字列になり、経路が変わっても
/// upsert の「内容が違うときだけ書く」判定が空振りする。`None` は JSON の `null`。
pub fn canonical_clock_json<T: Serialize>(clock: &T) -> Result<String, serde_json::Error> {
    serde_json::to_string(&serde_json::to_value(clock)?)
}

#[derive(Debug, Deserialize, Serialize)]
pub struct OwnedGamesIndexEntry {
    pub game_id: String,
    pub started_at_ms: u64,
    pub ended_at_ms: u64,
    pub black_handle: String,
    pub white_handle: String,
    #[serde(default)]
    pub black_player_id: Option<String>,
    #[serde(default)]
    pub white_player_id: Option<String>,
    pub result_kind: String,
    pub end_reason: String,
    pub moves_count: u32,
    /// 外部棋譜で持ち時間を判定できなかった entry では省略される。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock: Option<serde_json::Value>,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SearchRow {
    game_id: String,
    started_at_ms: u64,
    ended_at_ms: u64,
    sente_name: String,
    gote_name: String,
    black_player_id: Option<String>,
    white_player_id: Option<String>,
    wire_result_kind: String,
    end_reason: String,
    moves_count: u32,
    /// `clock` を持たない entry は JSON の `null` を保存する (列は NOT NULL)。
    clock_json: String,
    source: String,
    #[serde(default)]
    event: Option<String>,
    #[serde(default)]
    origin_url: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SearchGameSummary {
    game_id: String,
    started_at_ms: u64,
    ended_at_ms: u64,
    black_handle: String,
    white_handle: String,
    /// 外部棋譜 (`wdoor` / `upload`) では省略する。
    #[serde(skip_serializing_if = "Option::is_none")]
    black_player_id: Option<String>,
    /// 外部棋譜 (`wdoor` / `upload`) では省略する。
    #[serde(skip_serializing_if = "Option::is_none")]
    white_player_id: Option<String>,
    result_kind: String,
    end_reason: String,
    moves_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    clock: Option<serde_json::Value>,
    source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    event: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    origin_url: Option<String>,
}

impl SearchRow {
    pub fn to_player_game(&self) -> crate::player_ratings::PlayerGame {
        crate::player_ratings::PlayerGame {
            game_id: self.game_id.clone(),
            ended_at_ms: self.ended_at_ms,
            black_handle: self.sente_name.clone(),
            white_handle: self.gote_name.clone(),
            black_player_id: self.black_player_id.clone(),
            white_player_id: self.white_player_id.clone(),
            result_kind: self.wire_result_kind.clone(),
        }
    }

    pub fn resolved_player_ids(&self) -> (String, String) {
        let black = self
            .black_player_id
            .clone()
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| crate::player_identity::legacy_player_id(&self.sente_name));
        let white = self
            .white_player_id
            .clone()
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| crate::player_identity::legacy_player_id(&self.gote_name));
        (black, white)
    }

    /// R2 の正準 entry を D1 行と同じ表現へ変換する。
    pub fn from_owned(entry: &OwnedGamesIndexEntry) -> Result<Self, serde_json::Error> {
        Ok(Self {
            game_id: entry.game_id.clone(),
            started_at_ms: entry.started_at_ms,
            ended_at_ms: entry.ended_at_ms,
            sente_name: entry.black_handle.clone(),
            gote_name: entry.white_handle.clone(),
            black_player_id: entry.black_player_id.clone(),
            white_player_id: entry.white_player_id.clone(),
            wire_result_kind: entry.result_kind.clone(),
            end_reason: entry.end_reason.clone(),
            moves_count: entry.moves_count,
            clock_json: canonical_clock_json(&entry.clock)?,
            source: entry.source.clone(),
            event: entry.event.clone(),
            origin_url: entry.origin_url.clone(),
        })
    }

    pub fn into_summary(self) -> Result<SearchGameSummary, serde_json::Error> {
        let (black_player_id, white_player_id) = if is_external_source(&self.source) {
            (None, None)
        } else {
            let (black, white) = self.resolved_player_ids();
            (Some(black), Some(white))
        };
        Ok(SearchGameSummary {
            game_id: self.game_id,
            started_at_ms: self.started_at_ms,
            ended_at_ms: self.ended_at_ms,
            black_handle: self.sente_name,
            white_handle: self.gote_name,
            black_player_id,
            white_player_id,
            result_kind: self.wire_result_kind,
            end_reason: self.end_reason,
            moves_count: self.moves_count,
            clock: serde_json::from_str(&self.clock_json)?,
            source: self.source,
            event: self.event,
            origin_url: self.origin_url,
        })
    }
}

#[cfg(target_arch = "wasm32")]
pub async fn upsert_entry(env: &worker::Env, entry: &GamesIndexEntry<'_>) -> worker::Result<()> {
    let clock_json = canonical_clock_json(&entry.clock)?;
    upsert_fields(
        env,
        UpsertFields {
            game_id: entry.game_id,
            sente: entry.black_handle,
            gote: entry.white_handle,
            black_player_id: entry.black_player_id,
            white_player_id: entry.white_player_id,
            started_at_ms: entry.started_at_ms,
            ended_at_ms: entry.ended_at_ms,
            wire_result_kind: entry.result_kind,
            end_reason: entry.end_reason,
            moves_count: entry.moves_count,
            clock_json: &clock_json,
            source: entry.source,
            event: entry.event,
            origin_url: entry.origin_url,
        },
    )
    .await
}

#[cfg(target_arch = "wasm32")]
pub async fn upsert_owned(env: &worker::Env, entry: &OwnedGamesIndexEntry) -> worker::Result<()> {
    let clock_json = canonical_clock_json(&entry.clock)?;
    upsert_fields(
        env,
        UpsertFields {
            game_id: &entry.game_id,
            sente: &entry.black_handle,
            gote: &entry.white_handle,
            black_player_id: entry.black_player_id.as_deref(),
            white_player_id: entry.white_player_id.as_deref(),
            started_at_ms: entry.started_at_ms,
            ended_at_ms: entry.ended_at_ms,
            wire_result_kind: &entry.result_kind,
            end_reason: &entry.end_reason,
            moves_count: entry.moves_count,
            clock_json: &clock_json,
            source: &entry.source,
            event: entry.event.as_deref(),
            origin_url: entry.origin_url.as_deref(),
        },
    )
    .await
}

#[cfg(target_arch = "wasm32")]
struct UpsertFields<'a> {
    game_id: &'a str,
    sente: &'a str,
    gote: &'a str,
    black_player_id: Option<&'a str>,
    white_player_id: Option<&'a str>,
    started_at_ms: u64,
    ended_at_ms: u64,
    wire_result_kind: &'a str,
    end_reason: &'a str,
    moves_count: u32,
    clock_json: &'a str,
    source: &'a str,
    event: Option<&'a str>,
    origin_url: Option<&'a str>,
}

#[cfg(target_arch = "wasm32")]
fn optional_text(value: Option<&str>) -> worker::wasm_bindgen::JsValue {
    value.map_or(worker::wasm_bindgen::JsValue::NULL, worker::wasm_bindgen::JsValue::from_str)
}

/// 外部棋譜 (`wdoor` / `upload`) の 1 行を upsert する。
///
/// player ID は付けず (`NULL`)、レーティング集計の対象外なので
/// `player_rating_state` の revision / rebuild マーカーにも触れない。過去日時の
/// 棋譜を大量に取り込んでも Elo の全履歴 rebuild を誘発しない。自前対局の upsert と
/// 同じく、内容が変わらない行は書き換えない。保存済み対局への再リクエスト (修復) で
/// 毎回呼ばれるが、`clock_json` は [`canonical_clock_json`] で保存時と同じ文字列に
/// なるので、欠けが無ければ 1 行も書かない。
#[cfg(target_arch = "wasm32")]
async fn upsert_external_fields(
    db: &worker::D1Database,
    fields: UpsertFields<'_>,
) -> worker::Result<()> {
    use worker::wasm_bindgen::JsValue;
    let values = [
        JsValue::from_str(fields.game_id),
        JsValue::from_str(fields.sente),
        JsValue::from_str(fields.gote),
        JsValue::from_f64(fields.started_at_ms as f64),
        JsValue::from_f64(fields.ended_at_ms as f64),
        JsValue::from_str(result_kind_for_search(fields.end_reason)),
        JsValue::from_str(fields.source),
        JsValue::from_f64(f64::from(fields.moves_count)),
        JsValue::from_str(fields.wire_result_kind),
        JsValue::from_str(fields.end_reason),
        JsValue::from_str(fields.clock_json),
        optional_text(fields.event),
        optional_text(fields.origin_url),
    ];
    db.prepare("INSERT INTO games_search_index (game_id, sente_name, gote_name, black_player_id, white_player_id, started_at_ms, ended_at_ms, result_kind, source, moves_count, wire_result_kind, end_reason, clock_json, event, origin_url) VALUES (?, ?, ?, NULL, NULL, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(game_id) DO UPDATE SET sente_name=excluded.sente_name, gote_name=excluded.gote_name, black_player_id=NULL, white_player_id=NULL, started_at_ms=excluded.started_at_ms, ended_at_ms=excluded.ended_at_ms, result_kind=excluded.result_kind, source=excluded.source, moves_count=excluded.moves_count, wire_result_kind=excluded.wire_result_kind, end_reason=excluded.end_reason, clock_json=excluded.clock_json, event=excluded.event, origin_url=excluded.origin_url WHERE sente_name IS NOT excluded.sente_name OR gote_name IS NOT excluded.gote_name OR black_player_id IS NOT NULL OR white_player_id IS NOT NULL OR started_at_ms IS NOT excluded.started_at_ms OR ended_at_ms IS NOT excluded.ended_at_ms OR result_kind IS NOT excluded.result_kind OR source IS NOT excluded.source OR moves_count IS NOT excluded.moves_count OR wire_result_kind IS NOT excluded.wire_result_kind OR end_reason IS NOT excluded.end_reason OR clock_json IS NOT excluded.clock_json OR event IS NOT excluded.event OR origin_url IS NOT excluded.origin_url")
        .bind(&values)?
        .run()
        .await?;
    Ok(())
}

#[cfg(target_arch = "wasm32")]
async fn upsert_fields(env: &worker::Env, fields: UpsertFields<'_>) -> worker::Result<()> {
    use worker::wasm_bindgen::JsValue;
    let db = env.d1(crate::config::ConfigKeys::GAMES_SEARCH_DB_BINDING)?;
    if is_external_source(fields.source) {
        return upsert_external_fields(&db, fields).await;
    }
    let legacy_black;
    let black_player_id = match fields.black_player_id {
        Some(id) => id,
        None => {
            legacy_black = crate::player_identity::legacy_player_id(fields.sente);
            &legacy_black
        }
    };
    let legacy_white;
    let white_player_id = match fields.white_player_id {
        Some(id) => id,
        None => {
            legacy_white = crate::player_identity::legacy_player_id(fields.gote);
            &legacy_white
        }
    };
    let values = [
        JsValue::from_str(fields.game_id),
        JsValue::from_str(fields.sente),
        JsValue::from_str(fields.gote),
        JsValue::from_str(black_player_id),
        JsValue::from_str(white_player_id),
        JsValue::from_f64(fields.started_at_ms as f64),
        JsValue::from_f64(fields.ended_at_ms as f64),
        JsValue::from_str(result_kind_for_search(fields.end_reason)),
        JsValue::from_str(fields.source),
        JsValue::from_f64(f64::from(fields.moves_count)),
        JsValue::from_str(fields.wire_result_kind),
        JsValue::from_str(fields.end_reason),
        JsValue::from_str(fields.clock_json),
        optional_text(fields.event),
        optional_text(fields.origin_url),
    ];
    let dirty_values = vec![
        JsValue::from_f64(fields.ended_at_ms as f64),
        JsValue::from_f64(fields.ended_at_ms as f64),
        JsValue::from_str(fields.game_id),
        JsValue::from_str(fields.game_id),
        JsValue::from_str(fields.game_id),
        JsValue::from_str(fields.sente),
        JsValue::from_str(fields.gote),
        JsValue::from_str(black_player_id),
        JsValue::from_str(white_player_id),
        JsValue::from_f64(fields.started_at_ms as f64),
        JsValue::from_f64(fields.ended_at_ms as f64),
        JsValue::from_str(fields.wire_result_kind),
        JsValue::from_str(fields.end_reason),
        JsValue::from_str(fields.source),
        JsValue::from_f64(f64::from(fields.moves_count)),
        JsValue::from_str(fields.clock_json),
    ];
    // Every real change advances data_revision in the same transaction as the
    // UPSERT. A materializer page loaded from an older revision can therefore
    // discard all derived writes before advancing its cursor. Cursor以前の
    // late insert/updateは同時に全履歴rebuildも要求する。
    let dirty = db
        .prepare("UPDATE player_rating_state SET data_revision = data_revision + 1, rebuild_required = CASE WHEN cursor_ended_at_ms > ? OR (cursor_ended_at_ms = ? AND cursor_game_id >= ?) THEN 1 ELSE rebuild_required END WHERE singleton = 1 AND (NOT EXISTS (SELECT 1 FROM games_search_index WHERE game_id = ?) OR EXISTS (SELECT 1 FROM games_search_index WHERE game_id = ? AND (sente_name IS NOT ? OR gote_name IS NOT ? OR black_player_id IS NOT ? OR white_player_id IS NOT ? OR started_at_ms IS NOT ? OR ended_at_ms IS NOT ? OR wire_result_kind IS NOT ? OR end_reason IS NOT ? OR source IS NOT ? OR moves_count IS NOT ? OR clock_json IS NOT ?)))")
        .bind(&dirty_values)?;
    let upsert = db.prepare("INSERT INTO games_search_index (game_id, sente_name, gote_name, black_player_id, white_player_id, started_at_ms, ended_at_ms, result_kind, source, moves_count, wire_result_kind, end_reason, clock_json, event, origin_url) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(game_id) DO UPDATE SET sente_name=excluded.sente_name, gote_name=excluded.gote_name, black_player_id=excluded.black_player_id, white_player_id=excluded.white_player_id, started_at_ms=excluded.started_at_ms, ended_at_ms=excluded.ended_at_ms, result_kind=excluded.result_kind, source=excluded.source, moves_count=excluded.moves_count, wire_result_kind=excluded.wire_result_kind, end_reason=excluded.end_reason, clock_json=excluded.clock_json, event=excluded.event, origin_url=excluded.origin_url WHERE sente_name IS NOT excluded.sente_name OR gote_name IS NOT excluded.gote_name OR black_player_id IS NOT excluded.black_player_id OR white_player_id IS NOT excluded.white_player_id OR ended_at_ms IS NOT excluded.ended_at_ms OR wire_result_kind IS NOT excluded.wire_result_kind OR end_reason IS NOT excluded.end_reason OR started_at_ms IS NOT excluded.started_at_ms OR source IS NOT excluded.source OR moves_count IS NOT excluded.moves_count OR clock_json IS NOT excluded.clock_json OR event IS NOT excluded.event OR origin_url IS NOT excluded.origin_url")
        .bind(&values)?;
    db.batch(vec![dirty, upsert]).await?;
    Ok(())
}

/// keyring rotation で導出した旧 ID → active canonical ID alias を best-effort 登録する。
#[cfg(target_arch = "wasm32")]
pub async fn register_player_aliases(
    env: &worker::Env,
    canonical_id: &str,
    aliases: &[String],
) -> worker::Result<()> {
    use worker::wasm_bindgen::JsValue;

    if aliases.len() > crate::player_identity::MAX_KEYRING_KEYS {
        return Err(worker::Error::RustError("player alias limit exceeded".into()));
    }
    let db = env.d1(crate::config::ConfigKeys::GAMES_SEARCH_DB_BINDING)?;
    let targets: BTreeSet<_> = std::iter::once(canonical_id)
        .chain(aliases.iter().map(String::as_str))
        .collect();
    let placeholders = std::iter::repeat_n("?", targets.len()).collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT alias_id, canonical_id FROM player_id_aliases WHERE alias_id IN ({placeholders}) OR canonical_id IN ({placeholders})"
    );
    let mut lookup_values: Vec<_> = targets.iter().map(|id| JsValue::from_str(id)).collect();
    lookup_values.extend(targets.iter().map(|id| JsValue::from_str(id)));
    #[derive(serde::Deserialize)]
    struct ExistingAliasRow {
        alias_id: String,
        canonical_id: String,
    }
    let existing: Vec<AliasLink> = db
        .prepare(&sql)
        .bind(&lookup_values)?
        .all()
        .await?
        .results::<ExistingAliasRow>()?
        .into_iter()
        .map(|row| AliasLink {
            alias_id: row.alias_id,
            canonical_id: row.canonical_id,
        })
        .collect();
    let current: BTreeMap<_, _> = existing
        .iter()
        .map(|link| (link.alias_id.clone(), link.canonical_id.clone()))
        .collect();
    let normalized = normalized_alias_links(&existing, canonical_id, aliases);
    if current == normalized {
        return Ok(());
    }

    let mut statements = Vec::new();
    let mut reparent_values = vec![JsValue::from_str(canonical_id)];
    reparent_values.extend(targets.iter().map(|id| JsValue::from_str(id)));
    reparent_values.push(JsValue::from_str(canonical_id));
    statements.push(
        db.prepare(&format!(
            "UPDATE player_id_aliases SET canonical_id = ? WHERE canonical_id IN ({placeholders}) AND alias_id <> ?"
        ))
        .bind(&reparent_values)?,
    );
    let delete_values = [JsValue::from_str(canonical_id)];
    statements.push(
        db.prepare("DELETE FROM player_id_aliases WHERE alias_id = ?")
            .bind(&delete_values)?,
    );
    for alias in aliases {
        if alias == canonical_id {
            continue;
        }
        let values = [JsValue::from_str(alias), JsValue::from_str(canonical_id)];
        statements.push(
            db.prepare("INSERT INTO player_id_aliases (alias_id, canonical_id) VALUES (?, ?) ON CONFLICT(alias_id) DO UPDATE SET canonical_id=excluded.canonical_id")
                .bind(&values)?,
        );
    }
    // Reparenting, cycle removal, fresh aliases and rebuild marker commit
    // atomically. A transient failure cannot leave a half-rotated alias graph.
    statements.push(
        db.prepare("UPDATE player_rating_state SET rebuild_required = 1, data_revision = data_revision + 1 WHERE singleton = 1"),
    );
    db.batch(statements).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn base_params() -> SearchParams {
        SearchParams {
            name: None,
            result: None,
            source: Vec::new(),
            event: None,
            from: None,
            to: None,
            page: 1,
            page_size: 20,
        }
    }
    #[test]
    fn query_supports_all_filters() {
        let query = build_search_query(&SearchParams {
            name: Some("藤井".into()),
            result: Some("resignation".into()),
            source: vec!["kifu".into()],
            event: Some("WCSC36".into()),
            from: Some(100),
            to: Some(200),
            ..base_params()
        });
        for clause in [
            "sente_name LIKE ? ESCAPE '\\' COLLATE NOCASE",
            "result_kind = ?",
            "source IN (?)",
            "event = ?",
            "ended_at_ms >= ?",
            "ended_at_ms <= ?",
        ] {
            assert!(query.count_sql.contains(clause));
        }
        assert_eq!(query.filter_values[0], QueryValue::Text("%藤井%".into()));
    }

    #[test]
    fn name_filter_escapes_like_wildcards_as_literals() {
        let query = build_search_query(&SearchParams {
            name: Some(r"a%b_c\d".into()),
            ..base_params()
        });
        assert_eq!(
            query.filter_values,
            vec![
                QueryValue::Text(r"%a\%b\_c\\d%".into()),
                QueryValue::Text(r"%a\%b\_c\\d%".into()),
            ]
        );
        assert!(query.rows_sql.contains("LIKE ? ESCAPE '\\' COLLATE NOCASE"));
    }
    #[test]
    fn query_calculates_one_based_page_offset() {
        let query = build_search_query(&SearchParams {
            page: 3,
            page_size: 25,
            ..base_params()
        });
        assert_eq!((query.limit, query.offset), (25, 50));
    }
    #[test]
    fn result_kind_matches_canonical_mapping_for_all_end_reasons() {
        for (end_reason, expected) in [
            ("RESIGN", "resignation"),
            ("TIME_UP", "time_expired"),
            ("ILLEGAL", "abort"),
            ("JISHOGI", "jishogi"),
            ("OUTE_SENNICHITE", "oute_sennichite"),
            ("SENNICHITE", "draw"),
            ("MAX_MOVES", "max_moves"),
            ("ABNORMAL", "abnormal"),
        ] {
            assert_eq!(result_kind_for_search(end_reason), expected, "{end_reason}");
        }
        assert_eq!(result_kind_for_search("UNKNOWN"), "abort");
    }

    #[test]
    fn search_summary_preserves_games_index_wire_shape() {
        let original = serde_json::json!({
            "game_id": "g-1", "started_at_ms": 100, "ended_at_ms": 200,
            "black_handle": "alice", "white_handle": "bob",
            "result_kind": "WIN_BLACK", "end_reason": "RESIGN", "moves_count": 42,
            "clock": {"kind": "fischer", "total_sec": 300, "increment_sec": 5},
            "source": "kifu"
        });
        let entry: OwnedGamesIndexEntry = serde_json::from_value(original.clone()).unwrap();
        let summary = SearchRow::from_owned(&entry).unwrap().into_summary().unwrap();
        let mut expected = original;
        expected["black_player_id"] =
            serde_json::Value::String(crate::player_identity::legacy_player_id("alice"));
        expected["white_player_id"] =
            serde_json::Value::String(crate::player_identity::legacy_player_id("bob"));
        assert_eq!(serde_json::to_value(summary).unwrap(), expected);
    }

    #[test]
    fn query_binds_each_source_in_order_before_later_filters() {
        let query = build_search_query(&SearchParams {
            source: vec!["kifu".into(), "floodgate".into()],
            event: Some("floodgate-300-10F".into()),
            from: Some(7),
            ..base_params()
        });
        assert!(
            query
                .rows_sql
                .contains("WHERE source IN (?, ?) AND event = ? AND ended_at_ms >= ?"),
            "sql={}",
            query.rows_sql
        );
        assert_eq!(
            query.filter_values,
            vec![
                QueryValue::Text("kifu".into()),
                QueryValue::Text("floodgate".into()),
                QueryValue::Text("floodgate-300-10F".into()),
                QueryValue::Integer(7.0),
            ]
        );
        // bind 数と placeholder 数が一致する (LIMIT / OFFSET の 2 個を除く)。
        assert_eq!(query.count_sql.matches('?').count(), query.filter_values.len());
        assert_eq!(query.rows_sql.matches('?').count(), query.filter_values.len() + 2);
    }

    #[test]
    fn query_without_source_or_event_has_no_such_clause() {
        let query = build_search_query(&base_params());
        assert!(!query.count_sql.contains("WHERE"));
        assert!(query.rows_sql.contains("event, origin_url FROM games_search_index ORDER BY"));
    }

    #[test]
    fn event_tag_serializes_to_contract_shape() {
        let tag = EventTag {
            event: "WCSC36".into(),
            source: "upload".into(),
            games: 123,
        };
        assert_eq!(
            serde_json::to_value(&tag).unwrap(),
            serde_json::json!({"event": "WCSC36", "source": "upload", "games": 123})
        );
        // D1 の行 (COUNT(*) AS games) をそのまま読める。
        let row: EventTag =
            serde_json::from_str(r#"{"event":"WCSC36","source":"upload","games":123}"#).unwrap();
        assert_eq!(row, tag);
        assert!(EVENT_TAGS_SQL.contains("GROUP BY event, source"));
        assert!(EVENT_TAGS_SQL.ends_with("ORDER BY games DESC, event ASC, source ASC"));
    }

    #[test]
    fn player_games_predicate_excludes_external_sources_with_constant_binds() {
        let predicate = player_games_predicate();
        assert_eq!(predicate.matches('?').count(), 4);
        assert!(
            predicate
                .contains("IN (SELECT alias_id FROM player_id_aliases WHERE canonical_id = ?)")
        );
        // 先手・後手の OR 全体を括弧で閉じてから source 条件を AND する。
        assert!(predicate.starts_with("(("));
        assert!(predicate.ends_with("))) AND source NOT IN ('wdoor','upload')"));
    }

    #[test]
    fn clock_json_is_identical_from_struct_and_from_stored_meta() {
        use crate::games_index::{ClockSpec, GamesIndexEntry};
        let server_clocks = [
            rshogi_csa_server::ClockSpec::Countdown {
                total_time_sec: 600,
                byoyomi_sec: 10,
            },
            rshogi_csa_server::ClockSpec::Fischer {
                total_time_sec: 300,
                increment_sec: 10,
            },
            rshogi_csa_server::ClockSpec::CountdownMsec {
                total_time_ms: 60_000,
                byoyomi_ms: 100,
            },
            rshogi_csa_server::ClockSpec::StopWatch {
                total_time_min: 15,
                byoyomi_min: 2,
            },
        ];
        let clocks = server_clocks.iter().map(|clock| Some(ClockSpec::from_server(clock)));
        for clock in clocks.chain(std::iter::once(None)) {
            // 保存経路: 構造体から直接。
            let from_struct = canonical_clock_json(&clock).unwrap();
            // 修復 / backfill 経路: meta JSON を読み戻した値から。
            let entry = GamesIndexEntry {
                game_id: "g",
                started_at_ms: 1,
                ended_at_ms: 2,
                black_handle: "a",
                white_handle: "b",
                black_player_id: None,
                white_player_id: None,
                result_kind: "DRAW",
                end_reason: "SENNICHITE",
                moves_count: 0,
                clock,
                source: "upload",
                event: None,
                origin_url: None,
            };
            let meta = serde_json::to_vec(&entry).unwrap();
            let owned: OwnedGamesIndexEntry = serde_json::from_slice(&meta).unwrap();
            assert_eq!(canonical_clock_json(&owned.clock).unwrap(), from_struct);
            assert_eq!(SearchRow::from_owned(&owned).unwrap().clock_json, from_struct);
        }
        // キーは昇順。構造体の宣言順 (kind が先頭) には依存しない。
        let fischer = ClockSpec::from_server(&rshogi_csa_server::ClockSpec::Fischer {
            total_time_sec: 300,
            increment_sec: 10,
        });
        assert_eq!(
            canonical_clock_json(&Some(fischer)).unwrap(),
            r#"{"increment_sec":10,"kind":"fischer","total_sec":300}"#
        );
        assert_eq!(canonical_clock_json(&None::<ClockSpec<'_>>).unwrap(), "null");
    }

    #[test]
    fn source_filter_splits_dedupes_and_bounds_values() {
        assert_eq!(parse_source_filter("kifu").unwrap(), vec!["kifu"]);
        assert_eq!(
            parse_source_filter(" kifu, floodgate ,,kifu,wdoor").unwrap(),
            vec!["kifu", "floodgate", "wdoor"]
        );
        assert!(parse_source_filter("").unwrap().is_empty());
        assert!(parse_source_filter(",").unwrap().is_empty());
        let at_limit: Vec<_> = (0..MAX_SOURCE_FILTERS).map(|i| format!("s{i}")).collect();
        assert_eq!(parse_source_filter(&at_limit.join(",")).unwrap().len(), MAX_SOURCE_FILTERS);
        let over_limit: Vec<_> = (0..=MAX_SOURCE_FILTERS).map(|i| format!("s{i}")).collect();
        assert!(parse_source_filter(&over_limit.join(",")).is_err());
    }

    #[test]
    fn external_entry_round_trips_optional_fields_without_player_ids() {
        // backfill (R2 meta → games-index → D1) が通る経路: meta JSON を
        // `OwnedGamesIndexEntry` に読み、D1 行表現を経て検索 summary に戻す。
        let original = serde_json::json!({
            "game_id": "wdoor-20261009000001-0123456789",
            "started_at_ms": 100, "ended_at_ms": 200,
            "black_handle": "RAMU", "white_handle": "Panda",
            "result_kind": "WIN_BLACK", "end_reason": "RESIGN", "moves_count": 113,
            "clock": {"kind": "fischer", "total_sec": 300, "increment_sec": 10},
            "source": "wdoor",
            "event": "floodgate-300-10F",
            "origin_url": "https://example.com/x.csa"
        });
        let entry: OwnedGamesIndexEntry = serde_json::from_value(original.clone()).unwrap();
        let summary = SearchRow::from_owned(&entry).unwrap().into_summary().unwrap();
        assert_eq!(serde_json::to_value(summary).unwrap(), original);
    }

    #[test]
    fn external_entry_without_clock_or_event_stays_omitted() {
        let original = serde_json::json!({
            "game_id": "upload-0123456789abcdef",
            "started_at_ms": 100, "ended_at_ms": 200,
            "black_handle": "a", "white_handle": "b",
            "result_kind": "ABORT", "end_reason": "ABNORMAL", "moves_count": 0,
            "source": "upload"
        });
        let entry: OwnedGamesIndexEntry = serde_json::from_value(original.clone()).unwrap();
        let row = SearchRow::from_owned(&entry).unwrap();
        // D1 の clock_json は NOT NULL なので JSON の null を保存する。
        assert_eq!(row.clock_json, "null");
        assert_eq!(serde_json::to_value(row.into_summary().unwrap()).unwrap(), original);
    }

    #[test]
    fn pagination_rejects_zero_and_over_maximum() {
        assert!(validate_pagination(0, 20).is_err());
        assert!(validate_pagination(1, 0).is_err());
        assert!(validate_pagination(1, 101).is_err());
        assert!(validate_pagination(1, 100).is_ok());
    }

    #[test]
    fn second_rotation_reparents_plain_v1_and_v2_urls_to_v3_without_v1_key() {
        const V1: &str = "0123456789abcdef0123456789abcdef";
        const V2: &str = "abcdef0123456789abcdef0123456789";
        const V3: &str = "fedcba9876543210fedcba9876543210";
        let plain_v1 =
            crate::player_identity::derive_player_identity("alice", "pw", Some(V1)).unwrap();
        let v2_ring = format!(r#"{{"active_version":"v2","keys":{{"v1":"{V1}","v2":"{V2}"}}}}"#);
        let v2 =
            crate::player_identity::derive_player_identity("alice", "pw", Some(&v2_ring)).unwrap();
        let after_v2 = normalized_alias_links(&[], &v2.player_id, &v2.aliases);
        assert_eq!(after_v2.get(&plain_v1.player_id), Some(&v2.player_id));

        // v1 has deliberately been removed from the second rotated keyring.
        let v3_ring = format!(r#"{{"active_version":"v3","keys":{{"v2":"{V2}","v3":"{V3}"}}}}"#);
        let v3 =
            crate::player_identity::derive_player_identity("alice", "pw", Some(&v3_ring)).unwrap();
        let existing: Vec<_> = after_v2
            .into_iter()
            .map(|(alias_id, canonical_id)| AliasLink {
                alias_id,
                canonical_id,
            })
            .collect();
        let after_v3 = normalized_alias_links(&existing, &v3.player_id, &v3.aliases);

        // Old detail URLs and games carrying the v2 ID now resolve directly to
        // v3; the materializer therefore rebuilds all ratings under v3 too.
        assert_eq!(after_v3.get(&plain_v1.player_id), Some(&v3.player_id));
        assert_eq!(after_v3.get(&v2.player_id), Some(&v3.player_id));
        assert!(!after_v3.contains_key(&v3.player_id));
        assert!(after_v3.values().all(|target| target == &v3.player_id));
    }
}
