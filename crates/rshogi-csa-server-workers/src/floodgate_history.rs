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
//!   pathological な大量リクエストでもページ分けで処理できる。なお現状は 1 件 =
//!   1 GET で読み出しており `list_recent(N)` は最大 1+N 回のラウンドトリップに
//!   なる。N が大きい運用で R2 GET レイテンシが目立ってきたら、並列 fetch や
//!   オブジェクト統合での amortize を検討する余地がある
//! - **DO storage cache は現時点では入れない**: ホットパス `list_recent` の
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
//! `InMemoryFloodgateHistoryStorage` は `BTreeMap` を直接舐めるだけなので、
//! wasm32 R2 アダプタ固有のロジック（day-walking ループ・cursor pagination・
//! `pred_opt` フォールバック）は cargo test では検証されない。それらは
//! 完全な DO 統合（実 R2 + 実 GameRoom DO）として `wrangler dev` (Miniflare)
//! ハーネス側で「複数日にまたがる entry」「ページ境界をまたぐ list」「日付走査
//! 上限到達」のシナリオで別途検証する。

use chrono::{DateTime, Datelike, NaiveDate, Timelike, Utc};

use rshogi_csa_server::FloodgateHistoryEntry;
use rshogi_csa_server::error::StorageError;

/// R2 オブジェクトキーの共通プレフィックス。`list_recent` は本 prefix 配下を
/// day-shard 単位で走査する。
pub const KEY_PREFIX: &str = "floodgate-history";

/// 1 対局分の R2 オブジェクトキーを生成する。
///
/// キーは `floodgate-history/{YYYY}/{MM}/{DD}/{HHMMSS}-{game_id}.json` 形式で、
/// `entry.end_time` (RFC3339) から日時要素を抽出して埋める。`start_time` ではなく
/// `end_time` をキー軸に使うのは、`FloodgateHistoryStorage` の TCP 既定実装
/// (`JsonlFloodgateHistoryStorage`) が append 順 = 終局確定順で `list_recent` を
/// 返す契約に揃えるため。`start_time` でキーを切ると長時間対局（数時間続く対局）
/// が短時間対局より古いキーになり、`limit` が小さい `list_recent` で直近終局が
/// 欠落するケースがあり得る。
///
/// `game_id` はサーバ発行で一意（`/` `?` 等の R2 キー予約文字を含まない契約）。
/// 安全側に倒すため、想定外の文字を含む場合は `StorageError::Malformed` を返す。
pub fn entry_key(entry: &FloodgateHistoryEntry) -> Result<String, StorageError> {
    let ts = parse_timestamp(&entry.end_time)?;
    let game_id = validate_key_component(&entry.game_id)?;
    Ok(format!(
        "{}/{:04}/{:02}/{:02}/{:02}{:02}{:02}-{}.json",
        KEY_PREFIX,
        ts.year(),
        ts.month(),
        ts.day(),
        ts.hour(),
        ts.minute(),
        ts.second(),
        game_id,
    ))
}

/// `game_id` を R2 キーに埋め込む前のバリデーション。
///
/// R2 キーで `/` は階層区切りとして扱われ、day-shard prefix での list / sort が
/// 壊れる。空文字や R2 が拒否する制御文字も同様に弾く。CSA プロトコル上 `game_id`
/// は ASCII 英数 + `-` `_` のみを生成するサーバ前提なので、想定外の文字種は
/// バグとして上位に伝える。
///
/// 本関数は `games_index` モジュール (viewer 配信 API) からも参照されるため
/// `pub` で公開する。許可文字集合と空文字拒否のセマンティクスは両者で共有する。
pub fn validate_key_component(s: &str) -> Result<&str, StorageError> {
    if s.is_empty() {
        return Err(StorageError::Malformed("empty game_id in history entry".to_owned()));
    }
    if let Some(bad) = s.chars().find(|c| {
        // R2 キーで安全に使える文字に絞る。サーバ発行 `game_id` の想定文字集合
        // （ASCII 英数 + `-` + `_`）に該当しない場合は弾く。
        !(c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
    }) {
        return Err(StorageError::Malformed(format!(
            "game_id {s:?} contains disallowed character {bad:?} for R2 key"
        )));
    }
    Ok(s)
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
        .map_err(|e| StorageError::Malformed(format!("serialize history entry: {e}")))
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

        fn today_utc() -> Result<NaiveDate, StorageError> {
            // wasm32 では `Utc::now()` が `clock` feature 無効のため使えない。
            // 代わりに Workers の `Date::now()` でミリ秒タイムスタンプを取得して
            // chrono に橋渡しする。`from_timestamp_millis` は ms が i64 表現
            // 可能な範囲外だと `None` を返すが、現実的に発生しない。発生時は
            // 静かにフォールバックすると `list_recent` の結果が空になって診断
            // 困難になるため、エラー化して上位に伝える。
            let now_ms = Date::now().as_millis();
            DateTime::<Utc>::from_timestamp_millis(now_ms as i64)
                .map(|dt| dt.date_naive())
                .ok_or_else(|| {
                    StorageError::Io(format!(
                        "Date::now() ms {now_ms} is out of i64 timestamp range"
                    ))
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
                // `limit` は trait doc 上 `usize::MAX` も許容するため
                // `Vec::with_capacity(limit)` だと OOM panic 経路を生み得る。
                // 段階的成長に任せ、初期確保はゼロにしておく。
                let mut entries: Vec<FloodgateHistoryEntry> = Vec::new();
                let mut day = today?;
                let mut days_scanned: u32 = 0;
                while entries.len() < limit && days_scanned < MAX_DAYS_LOOKBACK {
                    let prefix = day_prefix(day);
                    // R2 list は prefix 内 lexicographic 昇順でページングされるため、
                    // ページ単位で逆順化すると「ページ N の末尾（新しい側）」より
                    // 「ページ N+1 の末尾（さらに新しい）」が後で来る。1 日 >1000 件の
                    // 状況で `limit` が小さいと先頭ページ（古い側）の末尾だけで
                    // limit を満たし、当日最新を返せない契約破綻になる。これを防ぐ
                    // ため、当日分のキーを一旦すべて pagination で集めてから一括逆順で
                    // 走査する。1 日数百対局を想定する Floodgate 運用では 1 ページで
                    // 終わるのが通常で、複数ページに膨らむのはバースト時のみ。
                    let mut day_keys: Vec<String> = Vec::new();
                    let mut cursor: Option<String> = None;
                    loop {
                        let mut builder = bucket.list().prefix(prefix.clone());
                        if let Some(c) = cursor.as_ref() {
                            builder = builder.cursor(c.clone());
                        }
                        let page = builder.execute().await.map_err(|e| {
                            StorageError::Io(format!("R2 list prefix {prefix}: {e}"))
                        })?;
                        day_keys.extend(page.objects().iter().map(|obj| obj.key()));
                        if !page.truncated() {
                            break;
                        }
                        cursor = page.cursor();
                        if cursor.is_none() {
                            break;
                        }
                    }
                    for key in day_keys.into_iter().rev() {
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
                        let raw = body
                            .text()
                            .await
                            .map_err(|e| StorageError::Io(format!("R2 read body {key}: {e}")))?;
                        entries.push(parse_entry_jsonl(&raw)?);
                    }
                    // `pred_opt` は `0001-01-01` でのみ `None` を返す。Floodgate
                    // 運用で起こり得ないが、もし到達したら同じ日付を再走査せずに
                    // 走査を打ち切る（外側の `days_scanned` で抜けるのを待つより
                    // 意図が明示的）。
                    let Some(prev) = day.pred_opt() else {
                        break;
                    };
                    day = prev;
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

    fn entry(game_id: &str, end_time: &str) -> FloodgateHistoryEntry {
        FloodgateHistoryEntry {
            game_id: game_id.to_owned(),
            game_name: "floodgate-600-10".to_owned(),
            black: "alice".to_owned(),
            white: "bob".to_owned(),
            start_time: "2026-04-26T12:00:00+00:00".to_owned(),
            end_time: end_time.to_owned(),
            result_code: "#RESIGN".to_owned(),
            winner: Some(HistoryColor::Black),
        }
    }

    #[test]
    fn entry_key_uses_end_time_components() {
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
        // end_time が JST (+09:00) 表記でも、キーは UTC に変換した日時で生成される
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
    fn entry_key_rejects_game_id_with_slash() {
        // `game_id` に `/` が混入すると R2 キーの階層構造が壊れて day-shard list が
        // 破綻するため、キー生成時点で `Malformed` で弾く（防御層を 1 つ持つ）。
        let e = entry("g1/evil", "2026-04-26T12:00:00+00:00");
        let err = entry_key(&e).unwrap_err();
        assert!(matches!(err, StorageError::Malformed(_)), "got: {err:?}");
    }

    #[test]
    fn entry_key_rejects_empty_game_id() {
        let e = entry("", "2026-04-26T12:00:00+00:00");
        let err = entry_key(&e).unwrap_err();
        assert!(matches!(err, StorageError::Malformed(_)), "got: {err:?}");
    }

    #[test]
    fn entry_key_rejects_game_id_with_non_ascii() {
        let e = entry("g\u{3042}", "2026-04-26T12:00:00+00:00");
        let err = entry_key(&e).unwrap_err();
        assert!(matches!(err, StorageError::Malformed(_)), "got: {err:?}");
    }

    #[test]
    fn entry_key_accepts_game_id_with_underscore_and_dash() {
        // ASCII 英数 + `_` + `-` はサーバ発行 game_id で頻出（タイムスタンプ + 連番
        // の連結等）。これらは R2 キーで安全に扱えるため受理する。
        let e = entry("g_1-abc", "2026-04-26T12:00:00+00:00");
        let key = entry_key(&e).unwrap();
        assert!(key.ends_with("-g_1-abc.json"), "got: {key}");
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
        for (id, end_time) in [
            ("g1", "2026-04-26T12:00:00+00:00"),
            ("g2", "2026-04-26T13:00:00+00:00"),
            ("g3", "2026-04-26T14:00:00+00:00"),
        ] {
            storage.append(&entry(id, end_time)).await.unwrap();
        }
        let recent = storage.list_recent(2).await.unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].game_id, "g3");
        assert_eq!(recent[1].game_id, "g2");
    }

    /// `entry_key` が `end_time` ベースで生成され BTreeMap が lexicographic に
    /// sort することで、複数日にまたがる append でも `list_recent` が正しく
    /// 新しい順を返す。R2 アダプタの day-walking ループ自体は wrangler dev で
    /// 検証するが、キー設計（end_time の年月日要素を埋める）が day 境界を
    /// またいで sortable である事実は本テストで固定する。
    #[tokio::test(flavor = "current_thread")]
    async fn list_recent_orders_entries_across_day_boundaries() {
        let backing = Arc::new(Mutex::new(BTreeMap::new()));
        let storage = InMemoryFloodgateHistoryStorage::new(backing);
        for (id, end_time) in [
            // 意図的に append 順を時系列とは逆に並べて、結果が end_time 順で
            // 揃うことを確認する（append 順そのものに依存しない契約）。
            ("g3", "2026-04-26T15:00:00+00:00"), // Apr 26 昼
            ("g1", "2026-04-25T23:30:00+00:00"), // Apr 25 深夜
            ("g4", "2026-04-27T08:00:00+00:00"), // Apr 27 朝
            ("g2", "2026-04-26T01:00:00+00:00"), // Apr 26 早朝
        ] {
            storage.append(&entry(id, end_time)).await.unwrap();
        }
        let recent = storage.list_recent(3).await.unwrap();
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].game_id, "g4");
        assert_eq!(recent[1].game_id, "g3");
        assert_eq!(recent[2].game_id, "g2");
    }

    /// 同日内の 1 ページ最大（1000 件）を超える append でも、key の lexicographic
    /// sort が「`end_time` 早い → 遅い」の順序を維持することを `entry_key` レベルで
    /// 固定する。R2 の page-boundary を超えるシナリオは wrangler dev で実 R2 を
    /// 使って検証するが、キー設計自体が page 境界をまたいでも単調順序を持つ
    /// （= `Vec::extend` で全ページ収集 → `into_iter().rev()` で正しく新しい順を
    /// 取り出せる）事実は本テストで固定する。
    #[test]
    fn entry_key_sorts_lexicographically_with_end_time() {
        let early = entry("g1", "2026-04-26T08:30:15+00:00");
        let mid = entry("g2", "2026-04-26T12:00:00+00:00");
        let late = entry("g3", "2026-04-26T18:45:30+00:00");
        let key_early = entry_key(&early).unwrap();
        let key_mid = entry_key(&mid).unwrap();
        let key_late = entry_key(&late).unwrap();
        assert!(key_early < key_mid, "{key_early} < {key_mid}");
        assert!(key_mid < key_late, "{key_mid} < {key_late}");
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
