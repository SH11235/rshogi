//! Ruby shogi-server 互換の `players.yaml` 形式でレートを永続化する
//! [`RateStorage`](crate::port::RateStorage) 実装。
//!
//! ## 互換するフォーマット
//!
//! Ruby `YAML.dump` で書き出される `players.yaml` 形式（[design.md] 1110 行）に
//! 合わせ、トップレベルはプレイヤ名（String）→ レコード（Ruby Symbol キー）の
//! マップで構成する。Ruby の `Symbol` は YAML 上で `":name"` のようにコロン接頭辞
//! つき文字列で表現されるため、本実装でも `:name`/`:rate`/`:win`/`:loss`/
//! `:last_game_id`/`:last_modified` をキーとして読み書きする:
//!
//! ```yaml
//! ---
//! alice:
//!   :name: alice
//!   :rate: 2500
//!   :win: 100
//!   :loss: 50
//!   :last_game_id: 20260426-001
//!   :last_modified: '2026-04-26T12:34:56+00:00'
//! bob:
//!   :name: bob
//!   :rate: 2400
//!   :win: 80
//!   :loss: 60
//!   :last_modified: '2026-04-26T12:34:56+00:00'
//! ```
//!
//! ## クリーンルーム方針
//!
//! Ruby shogi-server / mk_rate / mk_html のソースは参照せず、上記の公開ドキュメント
//! にある形式情報のみから実装する（[Requirement 14.1] / `feedback_no_phase_and_session_refs.md`
//! の OSS 互換ガイドライン）。CI も外部 Ruby ランタイムや shogi-server リポジトリを
//! 引かない（[Task 21.1] 参照）。
//!
//! ## レート値の責務分担
//!
//! `:rate` フィールドは Ruby `mk_rate` バッチが Glicko 系のアルゴリズムで計算する
//! 領域なので、本サーバ側では `record_game_outcome` で **触れない**。サーバが
//! 更新するのは `:win` / `:loss` / `:last_game_id` / `:last_modified` の 4 つだけで、
//! ロード時に取得した `:rate` をそのまま `save` 側に書き戻す。これにより `mk_rate`
//! と本サーバを同居させる運用でも、レート値を踏まないで wins/losses を加算できる。
//!
//! ## アトミック性
//!
//! - **ファイル書き込み**: tmpfile 書き込み + `rename(2)` の POSIX atomic で
//!   `players.yaml` の半端な状態を生まない。
//! - **read-modify-write**: 複数対局が同時に同一プレイヤのレコードを書き換える
//!   ケースを `disk_lock` (async Mutex) 配下で直列化し、`record_game_outcome` の
//!   内部で「キャッシュ更新 → 全件 snapshot → atomic write」を 1 critical section
//!   で完結する。`load` + `save` の 2 段呼び出しではなく `record_game_outcome` を
//!   経由する限り、wins/losses の lost-update は発生しない。

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex as StdMutex;

use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex as AsyncMutex;

use crate::error::StorageError;
use crate::port::{PlayerRateRecord, RateStorage};
use crate::types::{GameId, PlayerName};

/// 1 プレイヤ分のレコードを Ruby Symbol キー（`:name` / `:rate` / ...）で表現する
/// serde スキーマ。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct YamlRecord {
    #[serde(rename = ":name")]
    name: String,
    #[serde(rename = ":rate")]
    rate: i32,
    #[serde(rename = ":win")]
    win: u32,
    #[serde(rename = ":loss")]
    loss: u32,
    #[serde(rename = ":last_game_id", default)]
    last_game_id: Option<String>,
    #[serde(rename = ":last_modified")]
    last_modified: String,
}

impl YamlRecord {
    fn from_record(r: &PlayerRateRecord) -> Self {
        Self {
            name: r.name.as_str().to_owned(),
            rate: r.rate,
            win: r.wins,
            loss: r.losses,
            last_game_id: r.last_game_id.as_ref().map(|g| g.as_str().to_owned()),
            last_modified: r.last_modified.clone(),
        }
    }

    fn into_record(self) -> PlayerRateRecord {
        PlayerRateRecord {
            name: PlayerName::new(self.name),
            rate: self.rate,
            wins: self.win,
            losses: self.loss,
            last_game_id: self.last_game_id.map(GameId::new),
            last_modified: self.last_modified,
        }
    }
}

/// Ruby shogi-server 互換 `players.yaml` をレートストレージとして使う実装。
///
/// 起動時に `load_from_file` でファイル全体を in-memory `HashMap` に取り込み、
/// `load` は cache lookup のみで応答する（disk read を発生させない）。
/// `save` / `record_game_outcome` は cache を更新したあと、全件 snapshot を
/// atomic write で `players.yaml` に書き戻す。
///
/// ファイルが存在しない場合は空のマップから始め、最初の `save` で生成する。
#[derive(Debug)]
pub struct PlayersYamlRateStorage {
    path: PathBuf,
    cache: StdMutex<HashMap<String, PlayerRateRecord>>,
    /// disk write を直列化する async lock。`save` / `record_game_outcome` の
    /// critical section 全体（cache 更新 → snapshot → atomic write）を覆う。
    disk_lock: AsyncMutex<()>,
}

impl PlayersYamlRateStorage {
    /// 既存の `players.yaml` を読み込んでレートストレージを構築する。
    ///
    /// ファイルが存在しない場合は空マップを返す（初回起動の運用シナリオ）。
    /// ファイルが空文字列・空白のみの場合も同様に空マップとして扱う。
    /// パース失敗は [`StorageError::Malformed`] として `Err` を返す。
    pub async fn load_from_file(path: PathBuf) -> Result<Self, StorageError> {
        let map = match fs::read_to_string(&path).await {
            Ok(text) if text.trim().is_empty() => HashMap::new(),
            Ok(text) => parse_document(&text)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(e) => {
                return Err(StorageError::Io(format!("read {}: {}", path.display(), e)));
            }
        };
        Ok(Self {
            path,
            cache: StdMutex::new(map),
            disk_lock: AsyncMutex::new(()),
        })
    }

    /// 指定プレイヤがまだ未登録なら、既定値（rate=`initial_rate`、wins=0、losses=0）
    /// でレコードを補填する。disk への書き戻しは行わず、cache を更新するのみ。
    ///
    /// `players.toml` で定義された全プレイヤを LOGIN 経路で受け付けるための
    /// 起動時補填用。最初に終局して `record_game_outcome` が走った時点で
    /// `players.yaml` に書き戻される。
    pub fn ensure_default_records<I, S>(&self, names: I, initial_rate: i32, now_iso: &str)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut cache = self.cache.lock().expect("rate cache poisoned");
        for raw in names {
            let name: String = raw.into();
            cache.entry(name.clone()).or_insert_with(|| PlayerRateRecord {
                name: PlayerName::new(&name),
                rate: initial_rate,
                wins: 0,
                losses: 0,
                last_game_id: None,
                last_modified: now_iso.to_owned(),
            });
        }
    }

    fn snapshot(&self) -> HashMap<String, PlayerRateRecord> {
        self.cache.lock().expect("rate cache poisoned").clone()
    }

    async fn flush_to_disk(
        &self,
        snapshot: &HashMap<String, PlayerRateRecord>,
    ) -> Result<(), StorageError> {
        let yaml = render_document(snapshot)?;
        atomic_write_yaml(&self.path, &yaml).await
    }
}

impl RateStorage for PlayersYamlRateStorage {
    fn load(
        &self,
        name: &PlayerName,
    ) -> impl std::future::Future<Output = Result<Option<PlayerRateRecord>, StorageError>> {
        let result = self.cache.lock().expect("rate cache poisoned").get(name.as_str()).cloned();
        async move { Ok(result) }
    }

    fn save(
        &self,
        record: &PlayerRateRecord,
    ) -> impl std::future::Future<Output = Result<(), StorageError>> {
        let key = record.name.as_str().to_owned();
        let value = record.clone();
        async move {
            let _guard = self.disk_lock.lock().await;
            {
                let mut cache = self.cache.lock().expect("rate cache poisoned");
                cache.insert(key, value);
            }
            let snapshot = self.snapshot();
            self.flush_to_disk(&snapshot).await
        }
    }

    fn list_all(
        &self,
    ) -> impl std::future::Future<Output = Result<Vec<PlayerRateRecord>, StorageError>> {
        let snapshot: Vec<PlayerRateRecord> =
            self.cache.lock().expect("rate cache poisoned").values().cloned().collect();
        async move { Ok(snapshot) }
    }

    fn record_game_outcome(
        &self,
        black: &PlayerName,
        white: &PlayerName,
        winner: Option<&PlayerName>,
        game_id: &GameId,
        now_iso: &str,
    ) -> impl std::future::Future<Output = Result<(), StorageError>> {
        let black_str = black.as_str().to_owned();
        let white_str = white.as_str().to_owned();
        let winner_str = winner.map(|w| w.as_str().to_owned());
        let game_id_owned = game_id.clone();
        let now_owned = now_iso.to_owned();
        async move {
            let _guard = self.disk_lock.lock().await;
            {
                let mut cache = self.cache.lock().expect("rate cache poisoned");
                for key in [&black_str, &white_str] {
                    if let Some(rec) = cache.get_mut(key) {
                        match winner_str.as_deref() {
                            Some(w) if w == key => rec.wins = rec.wins.saturating_add(1),
                            Some(_) => rec.losses = rec.losses.saturating_add(1),
                            None => {}
                        }
                        rec.last_game_id = Some(game_id_owned.clone());
                        rec.last_modified = now_owned.clone();
                    }
                }
            }
            let snapshot = self.snapshot();
            self.flush_to_disk(&snapshot).await
        }
    }
}

fn parse_document(text: &str) -> Result<HashMap<String, PlayerRateRecord>, StorageError> {
    // 並びを byte-stable にして round-trip 比較しやすくするため、内部表現は
    // BTreeMap で受けてから HashMap に変換する。
    let doc: BTreeMap<String, YamlRecord> = serde_yaml::from_str(text)
        .map_err(|e| StorageError::Malformed(format!("players.yaml: {e}")))?;
    Ok(doc.into_iter().map(|(k, v)| (k, v.into_record())).collect())
}

fn render_document(records: &HashMap<String, PlayerRateRecord>) -> Result<String, StorageError> {
    // `BTreeMap` でキーをソートして書き出すことで、同一データから出力 byte 列が
    // 一致する（運用での diff 比較・自動レビューが安定する）。
    let doc: BTreeMap<String, YamlRecord> =
        records.iter().map(|(k, v)| (k.clone(), YamlRecord::from_record(v))).collect();
    serde_yaml::to_string(&doc)
        .map_err(|e| StorageError::Io(format!("serialize players.yaml: {e}")))
}

async fn atomic_write_yaml(path: &Path, contents: &str) -> Result<(), StorageError> {
    // `rename(2)` は同一ファイルシステム上で atomic なので tmpfile は隣接ディレクトリ
    // に作る。dotfile 接頭辞でユーザに対しても "中間ファイル" であることを示す。
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let stem = path
        .file_name()
        .ok_or_else(|| {
            StorageError::Io(format!("players.yaml path has no file name: {}", path.display()))
        })?
        .to_string_lossy();
    let tmp = dir.join(format!(".{stem}.rshogi-tmp"));
    let mut file = fs::File::create(&tmp)
        .await
        .map_err(|e| StorageError::Io(format!("create {}: {}", tmp.display(), e)))?;
    file.write_all(contents.as_bytes())
        .await
        .map_err(|e| StorageError::Io(format!("write {}: {}", tmp.display(), e)))?;
    file.sync_all()
        .await
        .map_err(|e| StorageError::Io(format!("sync {}: {}", tmp.display(), e)))?;
    drop(file);
    fs::rename(&tmp, path).await.map_err(|e| {
        StorageError::Io(format!("rename {} -> {}: {}", tmp.display(), path.display(), e))
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn rec(name: &str, rate: i32, wins: u32, losses: u32) -> PlayerRateRecord {
        PlayerRateRecord {
            name: PlayerName::new(name),
            rate,
            wins,
            losses,
            last_game_id: Some(GameId::new("20260426-001")),
            last_modified: "2026-04-26T12:34:56+00:00".to_owned(),
        }
    }

    #[test]
    fn render_document_emits_byte_stable_yaml_with_ruby_symbol_keys() {
        let mut records = HashMap::new();
        records.insert("alice".to_owned(), rec("alice", 2500, 100, 50));
        records.insert("bob".to_owned(), rec("bob", 2400, 80, 60));

        let yaml = render_document(&records).unwrap();
        // Ruby `YAML.dump` のキー順は内部 Hash 挿入順だが、本実装では BTreeMap で
        // 名前昇順に正規化する。アルファベット順で alice → bob が確定。
        // `:name`/`:rate`/`:win`/`:loss`/`:last_game_id`/`:last_modified` の
        // Ruby Symbol キーが quote 無しで出力されることを byte 比較で固定する。
        // `serde_yaml` は ASCII の `:` を quote 不要と判断するため、Ruby
        // `YAML.dump` と同様にコロン接頭辞のみで bare key として出力される。
        // diff 検証や grep で扱いやすくする上でも quote 無しの形を期待する。
        let expected = concat!(
            "alice:\n",
            "  :name: alice\n",
            "  :rate: 2500\n",
            "  :win: 100\n",
            "  :loss: 50\n",
            "  :last_game_id: 20260426-001\n",
            "  :last_modified: 2026-04-26T12:34:56+00:00\n",
            "bob:\n",
            "  :name: bob\n",
            "  :rate: 2400\n",
            "  :win: 80\n",
            "  :loss: 60\n",
            "  :last_game_id: 20260426-001\n",
            "  :last_modified: 2026-04-26T12:34:56+00:00\n",
        );
        assert_eq!(yaml, expected);
    }

    #[test]
    fn parse_document_round_trips_ruby_symbol_keys() {
        let mut records = HashMap::new();
        records.insert("alice".to_owned(), rec("alice", 2500, 100, 50));
        records.insert("bob".to_owned(), rec("bob", 2400, 80, 60));

        let yaml = render_document(&records).unwrap();
        let parsed = parse_document(&yaml).unwrap();
        assert_eq!(parsed, records);
    }

    #[test]
    fn parse_document_accepts_optional_last_game_id_omitted() {
        // Ruby YAML で `:last_game_id:` 行ごと省略するケース（新規プレイヤで
        // 一度も対局していない状態）を許容する。
        let yaml = "alice:\n  ':name': alice\n  ':rate': 1500\n  ':win': 0\n  ':loss': 0\n  ':last_modified': '2026-04-26T00:00:00+00:00'\n";
        let parsed = parse_document(yaml).unwrap();
        assert_eq!(parsed.len(), 1);
        let r = parsed.get("alice").unwrap();
        assert!(r.last_game_id.is_none());
        assert_eq!(r.rate, 1500);
        assert_eq!(r.wins, 0);
        assert_eq!(r.losses, 0);
    }

    #[test]
    fn parse_document_rejects_malformed_yaml() {
        let err = parse_document(":not a mapping").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("players.yaml"), "expected error to mention players.yaml: {msg}");
    }

    #[test]
    fn parse_document_treats_empty_input_as_empty_map() {
        // 上位 `load_from_file` は trim 済みの空文字列を直接 `HashMap::new()` に
        // 落とす経路だが、`parse_document` 単体でも空 mapping `{}` は受理する。
        let parsed = parse_document("{}").unwrap();
        assert!(parsed.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn load_from_file_returns_empty_when_file_missing() {
        let dir = tempdir();
        let path = dir.path().join("players.yaml");
        let storage = PlayersYamlRateStorage::load_from_file(path).await.unwrap();
        assert!(storage.list_all().await.unwrap().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn save_writes_atomic_yaml_and_round_trips() {
        let dir = tempdir();
        let path = dir.path().join("players.yaml");
        let storage = PlayersYamlRateStorage::load_from_file(path.clone()).await.unwrap();
        storage.save(&rec("alice", 2500, 100, 50)).await.unwrap();
        storage.save(&rec("bob", 2400, 80, 60)).await.unwrap();

        // Reload from disk and confirm same records exist.
        let reloaded = PlayersYamlRateStorage::load_from_file(path).await.unwrap();
        let mut names: Vec<String> = reloaded
            .list_all()
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.name.as_str().to_owned())
            .collect();
        names.sort();
        assert_eq!(names, vec!["alice".to_owned(), "bob".to_owned()]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn record_game_outcome_increments_winner_and_loser_atomically() {
        let dir = tempdir();
        let path = dir.path().join("players.yaml");
        let storage = PlayersYamlRateStorage::load_from_file(path.clone()).await.unwrap();
        storage.ensure_default_records(
            ["alice".to_owned(), "bob".to_owned()],
            1500,
            "2026-04-26T00:00:00+00:00",
        );

        let alice = PlayerName::new("alice");
        let bob = PlayerName::new("bob");
        let game_id = GameId::new("20260426-001");
        storage
            .record_game_outcome(&alice, &bob, Some(&alice), &game_id, "2026-04-26T12:34:56+00:00")
            .await
            .unwrap();

        let alice_rec = storage.load(&alice).await.unwrap().unwrap();
        let bob_rec = storage.load(&bob).await.unwrap().unwrap();
        assert_eq!(alice_rec.wins, 1);
        assert_eq!(alice_rec.losses, 0);
        assert_eq!(bob_rec.wins, 0);
        assert_eq!(bob_rec.losses, 1);
        assert_eq!(alice_rec.last_game_id.as_ref().map(|g| g.as_str()), Some("20260426-001"));
        assert_eq!(bob_rec.last_modified, "2026-04-26T12:34:56+00:00");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn record_game_outcome_draw_updates_last_fields_only() {
        let dir = tempdir();
        let path = dir.path().join("players.yaml");
        let storage = PlayersYamlRateStorage::load_from_file(path.clone()).await.unwrap();
        storage.ensure_default_records(
            ["alice".to_owned(), "bob".to_owned()],
            1500,
            "2026-04-26T00:00:00+00:00",
        );

        let alice = PlayerName::new("alice");
        let bob = PlayerName::new("bob");
        let game_id = GameId::new("20260426-002");
        storage
            .record_game_outcome(&alice, &bob, None, &game_id, "2026-04-26T13:00:00+00:00")
            .await
            .unwrap();

        let alice_rec = storage.load(&alice).await.unwrap().unwrap();
        let bob_rec = storage.load(&bob).await.unwrap().unwrap();
        // 千日手・最大手数では wins/losses は据置。last_* のみ更新される。
        assert_eq!(alice_rec.wins, 0);
        assert_eq!(alice_rec.losses, 0);
        assert_eq!(bob_rec.wins, 0);
        assert_eq!(bob_rec.losses, 0);
        assert_eq!(alice_rec.last_modified, "2026-04-26T13:00:00+00:00");
        assert_eq!(bob_rec.last_modified, "2026-04-26T13:00:00+00:00");
        assert_eq!(alice_rec.last_game_id.as_ref().map(|g| g.as_str()), Some("20260426-002"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn record_game_outcome_does_not_mutate_rate_value() {
        // `:rate` は外部バッチ（mk_rate）の責務。本サーバの終局処理では
        // 触れないことを契約として固定する（同居運用で踏まないために重要）。
        let dir = tempdir();
        let path = dir.path().join("players.yaml");
        let storage = PlayersYamlRateStorage::load_from_file(path.clone()).await.unwrap();
        storage.save(&rec("alice", 2500, 0, 0)).await.unwrap();
        storage.save(&rec("bob", 2400, 0, 0)).await.unwrap();

        let alice = PlayerName::new("alice");
        let bob = PlayerName::new("bob");
        let game_id = GameId::new("20260426-003");
        storage
            .record_game_outcome(&alice, &bob, Some(&alice), &game_id, "2026-04-26T14:00:00+00:00")
            .await
            .unwrap();

        let alice_rec = storage.load(&alice).await.unwrap().unwrap();
        let bob_rec = storage.load(&bob).await.unwrap().unwrap();
        assert_eq!(alice_rec.rate, 2500, "rate must be preserved verbatim");
        assert_eq!(bob_rec.rate, 2400, "rate must be preserved verbatim");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn save_recovers_from_corrupted_file_with_explicit_error() {
        let dir = tempdir();
        let path = dir.path().join("players.yaml");
        // 不正な YAML を直接書く（mid-write クラッシュ等のシミュレーション）。
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b":invalid: [unterminated").unwrap();
        drop(f);

        let err = PlayersYamlRateStorage::load_from_file(path).await.unwrap_err();
        let msg = format!("{err}");
        assert!(
            matches!(err, StorageError::Malformed(_)),
            "expected Malformed error, got: {msg}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ensure_default_records_does_not_overwrite_existing() {
        let dir = tempdir();
        let path = dir.path().join("players.yaml");
        let storage = PlayersYamlRateStorage::load_from_file(path).await.unwrap();
        storage.save(&rec("alice", 2500, 100, 50)).await.unwrap();

        // alice は既に rate=2500/wins=100 で記録済み。ensure_default_records が
        // これを既定値で上書きしないことを確認。
        storage.ensure_default_records(
            ["alice".to_owned(), "bob".to_owned()],
            1500,
            "2026-04-26T15:00:00+00:00",
        );
        let alice_rec = storage.load(&PlayerName::new("alice")).await.unwrap().unwrap();
        assert_eq!(alice_rec.rate, 2500);
        assert_eq!(alice_rec.wins, 100);
        let bob_rec = storage.load(&PlayerName::new("bob")).await.unwrap().unwrap();
        assert_eq!(bob_rec.rate, 1500);
        assert_eq!(bob_rec.wins, 0);
    }

    /// `tempfile` クレートを使わずに済ませるため、テスト専用の薄い RAII
    /// ディレクトリを定義する。`Drop` で再帰削除する。
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            // テスト失敗時のクリーンアップ漏れは許容（系列番号衝突は確率的に低い）
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn tempdir() -> TempDir {
        let base = std::env::temp_dir();
        let pid = std::process::id();
        // counter を使って同 pid 内のテスト間衝突を避ける
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = base.join(format!("rshogi-players-yaml-{pid}-{n}"));
        std::fs::create_dir_all(&path).expect("create tempdir");
        TempDir { path }
    }
}
