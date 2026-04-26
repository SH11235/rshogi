//! Cloudflare Workers 環境向け Floodgate 履歴ストレージ実装。
//!
//! `rshogi_csa_server::FloodgateHistoryStorage` trait の Workers 側 backend として、
//! 1 対局 = 1 R2 オブジェクトの JSONL ファイルを
//! `floodgate-history/{YYYY}/{MM}/{DD}/{HHMMSS}-{game_id}.json` 形式で保存する。
//!
//! # 設計判断
//!
//! - **1 対局 = 1 オブジェクト**: R2 は append 操作を持たないため、TCP 側の
//!   JSONL 単一ファイル append-only モデルを直接移植できない。代替として既存
//!   `FileKifuStorage` の `YYYY/MM/DD/<game_id>.csa` パターンに揃え、終局時に
//!   1 PUT で完結させる。並行書き込みのレース処理が不要、`list_recent` は
//!   prefix list の day-shard 走査で実装できる
//! - **キーは sortable**: prefix `floodgate-history/` 配下のキーは時系列で
//!   lexicographic に並ぶため、R2 の昇順 list 結果を逆順に走査するだけで新しい
//!   順の N 件取得ができる
//! - **day-shard 走査**: `list_recent(N)` は当日の day-shard から逆方向に日付を
//!   さかのぼって走査する。1 日あたり数百対局程度を想定すると、典型的 N=10〜100
//!   は当日 1 リストで満たせる。R2 list は 1 ページ最大 1000 オブジェクトなので、
//!   pathological な大量リクエストでもページ分けで処理できる
//! - **DO storage cache は本 PR では入れない**: ホットパス `list_recent` の
//!   キャッシュは将来必要になった時点で追加する（YAGNI）。終局時 1 PUT が
//!   ホットパスではないため、`append` 側にも cache レイヤは不要
//!
//! # 実装範囲
//!
//! 本モジュールは以下を提供する:
//!
//! - **純粋ロジック**: キー生成・JSONL 行 parse・day prefix 計算（host target で
//!   ユニットテスト可能）
//! - **wasm32 R2 アダプタ**: `R2FloodgateHistoryStorage`。`worker::Bucket` を
//!   通じて実 R2 にアクセスする
//! - **テスト用インメモリ実装**: `InMemoryFloodgateHistoryStorage`（`#[cfg(test)]`
//!   配下）。同一の `Arc<Mutex<...>>` backing を 2 つの instance で共有することで
//!   cold start シナリオ（DO instance の破棄 → 再構築 → 永続化済みデータ参照）を
//!   host target 上で再現する
//!
//! 完全な DO 統合（実 R2 + 実 GameRoom DO）は `wrangler dev` (Miniflare) ハーネス
//! で別途検証する。

use chrono::{DateTime, Datelike, NaiveDate, Timelike, Utc};

use rshogi_csa_server::FloodgateHistoryEntry;
use rshogi_csa_server::error::StorageError;

/// R2 オブジェクトキーの共通プレフィックス。`list_recent` は本 prefix 配下を
/// day-shard 単位で走査する。
pub const KEY_PREFIX: &str = "floodgate-history";

/// 1 対局分の R2 オブジェクトキーを生成する。
///
/// キーは `floodgate-history/{YYYY}/{MM}/{DD}/{HHMMSS}-{game_id}.json` 形式で、
/// `entry.start_time` (RFC3339) から日時要素を抽出して埋める。同一 game_id が
/// 同一秒内に複数回 append されることは想定しない（`game_id` がサーバ発行で
/// 一意のため）。
pub fn entry_key(entry: &FloodgateHistoryEntry) -> Result<String, StorageError> {
    let ts = parse_timestamp(&entry.start_time)?;
    Ok(format!(
        "{}/{:04}/{:02}/{:02}/{:02}{:02}{:02}-{}.json",
        KEY_PREFIX,
        ts.year(),
        ts.month(),
        ts.day(),
        ts.hour(),
        ts.minute(),
        ts.second(),
        entry.game_id,
    ))
}

/// 指定日の R2 オブジェクトをすべて含む list prefix を返す。
///
/// `bucket.list().prefix(day_prefix(date))` で当日分のキーを過不足なく取得できる。
pub fn day_prefix(date: NaiveDate) -> String {
    format!("{}/{:04}/{:02}/{:02}/", KEY_PREFIX, date.year(), date.month(), date.day(),)
}

/// JSONL 1 行から `FloodgateHistoryEntry` を構築する。
///
/// R2 オブジェクト本文は 1 entry を `serde_json::to_string` で書き込んだ単一行
/// JSON。空行や末尾改行は呼び出し側でトリムする想定（`String::trim` 経由）。
pub fn parse_entry_jsonl(line: &str) -> Result<FloodgateHistoryEntry, StorageError> {
    serde_json::from_str(line.trim())
        .map_err(|e| StorageError::Malformed(format!("parse history entry: {e}")))
}

/// `FloodgateHistoryEntry` を JSONL 1 行（末尾改行なし）にシリアライズする。
pub fn serialize_entry_jsonl(entry: &FloodgateHistoryEntry) -> Result<String, StorageError> {
    serde_json::to_string(entry)
        .map_err(|e| StorageError::Io(format!("serialize history entry: {e}")))
}

fn parse_timestamp(rfc3339: &str) -> Result<DateTime<Utc>, StorageError> {
    DateTime::parse_from_rfc3339(rfc3339)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| StorageError::Malformed(format!("parse timestamp {rfc3339:?}: {e}")))
}

/// `list_recent` の day-shard 走査で 1 度にさかのぼる最大日数。1 年分の history を
/// scan 上限とし、それ以上古い日付に entry が偏在する場合は走査を打ち切る
/// （Floodgate 運用では年単位の rotate を想定）。
pub const MAX_DAYS_LOOKBACK: u32 = 366;

#[cfg(target_arch = "wasm32")]
mod wasm32_impl {
    use super::*;

    use std::future::Future;

    use rshogi_csa_server::FloodgateHistoryStorage;
    use worker::{Bucket, Date, Env};

    /// Cloudflare R2 を backend とする `FloodgateHistoryStorage` 実装。
    ///
    /// `binding` には `wrangler.toml` で宣言した R2 バケットのバインディング名を
    /// 渡す（`config::ConfigKeys::FLOODGATE_HISTORY_BUCKET_BINDING` 推奨）。
    pub struct R2FloodgateHistoryStorage {
        env: Env,
        binding: String,
    }

    impl R2FloodgateHistoryStorage {
        /// `env` から `binding` 名で R2 バケットを参照するストレージを構築する。
        pub fn new(env: Env, binding: impl Into<String>) -> Self {
            Self {
                env,
                binding: binding.into(),
            }
        }

        fn bucket(&self) -> Result<Bucket, StorageError> {
            self.env
                .bucket(&self.binding)
                .map_err(|e| StorageError::Io(format!("R2 binding {}: {e}", self.binding)))
        }

        fn today_utc() -> NaiveDate {
            // wasm32 では `Utc::now()` が `clock` feature 無効のため使えない。
            // 代わりに Workers の `Date::now()` でミリ秒タイムスタンプを取得して
            // chrono に橋渡しする。
            let now_ms = Date::now().as_millis();
            DateTime::<Utc>::from_timestamp_millis(now_ms as i64)
                .map(|dt| dt.date_naive())
                .unwrap_or_else(|| {
                    // タイムスタンプが i64 範囲外になることは現実的にあり得ないが、
                    // 防御的に Unix epoch にフォールバックして安全に進める。
                    NaiveDate::from_ymd_opt(1970, 1, 1).expect("epoch date is valid")
                })
        }
    }

    impl FloodgateHistoryStorage for R2FloodgateHistoryStorage {
        fn append(
            &self,
            entry: &FloodgateHistoryEntry,
        ) -> impl Future<Output = Result<(), StorageError>> {
            let key = entry_key(entry);
            let payload = serialize_entry_jsonl(entry);
            let bucket = self.bucket();
            async move {
                let key = key?;
                let payload = payload?;
                let bucket = bucket?;
                bucket
                    .put(&key, payload.into_bytes())
                    .execute()
                    .await
                    .map_err(|e| StorageError::Io(format!("R2 put {key}: {e}")))?;
                Ok(())
            }
        }

        fn list_recent(
            &self,
            limit: usize,
        ) -> impl Future<Output = Result<Vec<FloodgateHistoryEntry>, StorageError>> {
            let bucket = self.bucket();
            let today = Self::today_utc();
            async move {
                if limit == 0 {
                    return Ok(Vec::new());
                }
                let bucket = bucket?;
                let mut entries: Vec<FloodgateHistoryEntry> = Vec::with_capacity(limit);
                let mut day = today;
                let mut days_scanned: u32 = 0;
                while entries.len() < limit && days_scanned < MAX_DAYS_LOOKBACK {
                    let prefix = day_prefix(day);
                    let mut cursor: Option<String> = None;
                    loop {
                        let mut builder = bucket.list().prefix(prefix.clone());
                        if let Some(c) = cursor.as_ref() {
                            builder = builder.cursor(c.clone());
                        }
                        let page = builder.execute().await.map_err(|e| {
                            StorageError::Io(format!("R2 list prefix {prefix}: {e}"))
                        })?;
                        // R2 は昇順に返すので、当日内の新しい順は逆走査で取り出す。
                        // ページ境界をまたぐと「今のページの末尾（=新しい）」を全部
                        // 取り終わってから次ページへ進むため、まず当ページの全 key を
                        // 集めてから逆順で読み込む。
                        let keys: Vec<String> =
                            page.objects().iter().map(|obj| obj.key()).collect();
                        for key in keys.into_iter().rev() {
                            if entries.len() >= limit {
                                break;
                            }
                            let obj = bucket
                                .get(&key)
                                .execute()
                                .await
                                .map_err(|e| StorageError::Io(format!("R2 get {key}: {e}")))?;
                            let Some(obj) = obj else { continue };
                            let Some(body) = obj.body() else { continue };
                            let raw = body.text().await.map_err(|e| {
                                StorageError::Io(format!("R2 read body {key}: {e}"))
                            })?;
                            entries.push(parse_entry_jsonl(&raw)?);
                        }
                        if entries.len() >= limit || !page.truncated() {
                            break;
                        }
                        cursor = page.cursor();
                        if cursor.is_none() {
                            break;
                        }
                    }
                    day = day.pred_opt().unwrap_or(day);
                    days_scanned += 1;
                }
                Ok(entries)
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub use wasm32_impl::R2FloodgateHistoryStorage;

#[cfg(test)]
mod test_fixture {
    use std::collections::BTreeMap;
    use std::future::Future;
    use std::sync::{Arc, Mutex};

    use rshogi_csa_server::FloodgateHistoryStorage;

    use super::*;

    /// host target でのテスト用インメモリ実装。
    ///
    /// `Arc<Mutex<BTreeMap<String, FloodgateHistoryEntry>>>` を共有 backing
    /// storage として持ち、複数の instance が同じ backing を参照することで
    /// cold start（DO instance の破棄 → 再構築 → 永続化データ参照）を再現できる。
    /// キーは `entry_key` で生成するので R2 アダプタと同じ並びになる。
    pub(super) struct InMemoryFloodgateHistoryStorage {
        backing: Arc<Mutex<BTreeMap<String, FloodgateHistoryEntry>>>,
    }

    impl InMemoryFloodgateHistoryStorage {
        pub(super) fn new(backing: Arc<Mutex<BTreeMap<String, FloodgateHistoryEntry>>>) -> Self {
            Self { backing }
        }
    }

    impl FloodgateHistoryStorage for InMemoryFloodgateHistoryStorage {
        fn append(
            &self,
            entry: &FloodgateHistoryEntry,
        ) -> impl Future<Output = Result<(), StorageError>> {
            let key = entry_key(entry);
            let entry_owned = entry.clone();
            let backing = self.backing.clone();
            async move {
                let key = key?;
                let mut guard = backing.lock().expect("in-memory backing poisoned");
                guard.insert(key, entry_owned);
                Ok(())
            }
        }

        fn list_recent(
            &self,
            limit: usize,
        ) -> impl Future<Output = Result<Vec<FloodgateHistoryEntry>, StorageError>> {
            let backing = self.backing.clone();
            async move {
                if limit == 0 {
                    return Ok(Vec::new());
                }
                let guard = backing.lock().expect("in-memory backing poisoned");
                Ok(guard.values().rev().take(limit).cloned().collect())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use rshogi_csa_server::{FloodgateHistoryEntry, FloodgateHistoryStorage, HistoryColor};

    use super::test_fixture::InMemoryFloodgateHistoryStorage;
    use super::*;

    fn entry(game_id: &str, start_time: &str) -> FloodgateHistoryEntry {
        FloodgateHistoryEntry {
            game_id: game_id.to_owned(),
            game_name: "floodgate-600-10".to_owned(),
            black: "alice".to_owned(),
            white: "bob".to_owned(),
            start_time: start_time.to_owned(),
            end_time: "2026-04-26T13:00:00+00:00".to_owned(),
            result_code: "#RESIGN".to_owned(),
            winner: Some(HistoryColor::Black),
        }
    }

    #[test]
    fn entry_key_uses_start_time_components() {
        let e = entry("g42", "2026-04-26T12:34:56+00:00");
        let key = entry_key(&e).unwrap();
        assert_eq!(key, "floodgate-history/2026/04/26/123456-g42.json");
    }

    #[test]
    fn entry_key_pads_single_digit_components() {
        let e = entry("g7", "2026-01-02T03:04:05+00:00");
        let key = entry_key(&e).unwrap();
        assert_eq!(key, "floodgate-history/2026/01/02/030405-g7.json");
    }

    #[test]
    fn entry_key_normalizes_offset_to_utc() {
        // start_time が JST (+09:00) 表記でも、キーは UTC に変換した日時で生成される
        // （day-shard が UTC 基準で揃うため）。
        let e = entry("g1", "2026-04-26T09:00:00+09:00");
        let key = entry_key(&e).unwrap();
        assert_eq!(key, "floodgate-history/2026/04/26/000000-g1.json");
    }

    #[test]
    fn entry_key_rejects_malformed_timestamp() {
        let e = entry("g1", "not a timestamp");
        let err = entry_key(&e).unwrap_err();
        assert!(matches!(err, StorageError::Malformed(_)), "got: {err:?}");
    }

    #[test]
    fn day_prefix_formats_components() {
        let prefix = day_prefix(NaiveDate::from_ymd_opt(2026, 4, 26).unwrap());
        assert_eq!(prefix, "floodgate-history/2026/04/26/");
    }

    #[test]
    fn parse_and_serialize_round_trip() {
        let original = entry("g1", "2026-04-26T12:00:00+00:00");
        let line = serialize_entry_jsonl(&original).unwrap();
        let parsed = parse_entry_jsonl(&line).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn parse_jsonl_trims_trailing_whitespace() {
        let original = entry("g1", "2026-04-26T12:00:00+00:00");
        let line = serialize_entry_jsonl(&original).unwrap();
        let with_newline = format!("{line}\n");
        let parsed = parse_entry_jsonl(&with_newline).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn parse_jsonl_rejects_malformed() {
        let err = parse_entry_jsonl("{not json}").unwrap_err();
        assert!(matches!(err, StorageError::Malformed(_)), "got: {err:?}");
    }

    /// cold start を再現する受入シナリオ: 1 instance で append → drop し、新規
    /// instance を同じ backing storage で構築して `list_recent` が永続化された
    /// entry を返すことを確認する。`InMemoryFloodgateHistoryStorage` は R2 アダプタ
    /// と同じ trait を実装し、同じ key 生成ロジック (`entry_key`) を共有するため、
    /// 本テストの pass は trait の cold-start 契約を host target 上で固定する。
    #[tokio::test(flavor = "current_thread")]
    async fn cold_start_then_list_recent_returns_persisted_entry() {
        let backing = Arc::new(Mutex::new(BTreeMap::new()));
        {
            let instance1 = InMemoryFloodgateHistoryStorage::new(backing.clone());
            instance1.append(&entry("g1", "2026-04-26T12:00:00+00:00")).await.unwrap();
            // instance1 を drop（DO の cold shutdown 相当）。
        }
        // 新規 instance（DO が再構築されたとき相当）で読み出す。
        let instance2 = InMemoryFloodgateHistoryStorage::new(backing.clone());
        let recent = instance2.list_recent(10).await.unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].game_id, "g1");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn list_recent_returns_newest_first_within_single_instance() {
        let backing = Arc::new(Mutex::new(BTreeMap::new()));
        let storage = InMemoryFloodgateHistoryStorage::new(backing);
        for (id, start) in [
            ("g1", "2026-04-26T12:00:00+00:00"),
            ("g2", "2026-04-26T13:00:00+00:00"),
            ("g3", "2026-04-26T14:00:00+00:00"),
        ] {
            storage.append(&entry(id, start)).await.unwrap();
        }
        let recent = storage.list_recent(2).await.unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].game_id, "g3");
        assert_eq!(recent[1].game_id, "g2");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn list_recent_zero_returns_empty() {
        let backing = Arc::new(Mutex::new(BTreeMap::new()));
        let storage = InMemoryFloodgateHistoryStorage::new(backing);
        storage.append(&entry("g1", "2026-04-26T12:00:00+00:00")).await.unwrap();
        let recent = storage.list_recent(0).await.unwrap();
        assert!(recent.is_empty());
    }
}
