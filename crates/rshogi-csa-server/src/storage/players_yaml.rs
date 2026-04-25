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
//! alice:
//!   :name: alice
//!   :rate: 2500
//!   :win: 100
//!   :loss: 50
//!   :last_game_id: 20260426-001
//!   :last_modified: 2026-04-26T12:34:56+00:00
//! bob:
//!   :name: bob
//!   :rate: 2400
//!   :win: 80
//!   :loss: 60
//!   :last_modified: 2026-04-26T12:34:56+00:00
//! ```
//!
//! `serde_yaml` の `to_string` は document start (`---`) を出さず、`String` 型の
//! `:last_modified` も quote 無しで bare scalar として出力する。例は
//! `render_document` のテスト golden YAML と一致させる（実出力と差異が出たら
//! `render_document_emits_byte_stable_yaml_with_ruby_symbol_keys` が落ちる）。
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

/// `StdMutex<HashMap<...>>` を poisoning に強くロックする小ヘルパ。
///
/// `current_thread + LocalSet` 運用では他スレッドが panic を伝播させる経路は
/// 実質存在しないが、`poison`化は契約違反ではなく単に「ロック中にどこかで
/// panic が起きた」副作用に過ぎない。CLAUDE.md の「panic は契約違反のみ」
/// 方針に合わせ、poisoning は `into_inner` でデータをそのまま借りる。
fn lock_cache<T>(m: &StdMutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 1 プレイヤ分のレコードを Ruby Symbol キー（`:name` / `:rate` / ...）で表現する
/// serde スキーマ。
///
/// `last_game_id` が `None` の場合は serde の `skip_serializing_if` でキー行ごと
/// 出力から除外する（`null` リテラルや空値を吐かない）。Ruby `YAML.dump` で
/// `Hash.delete(:last_game_id)` 済みの未対局レコードと等価な見え方になる。
/// deserialize 側は `default` で `None` を許容する。
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
    #[serde(
        rename = ":last_game_id",
        default,
        skip_serializing_if = "Option::is_none"
    )]
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
    ///
    /// **fail-fast**: ファイル自体が `NotFound` でも、親ディレクトリが存在しない
    /// 場合は設定ミス（例: `--players-yaml /nonexistent-dir/players.yaml`）として
    /// 起動時に `StorageError::Io` を返す。これを許容すると、サーバが accept ループ
    /// に入ったあと初回終局時に毎回書き込みが失敗する形で運用障害が遅延顕在化
    /// するため、最初の `record_game_outcome` を待たずに起動段階で検知する。
    pub async fn load_from_file(path: PathBuf) -> Result<Self, StorageError> {
        let map = match fs::read_to_string(&path).await {
            Ok(text) if text.trim().is_empty() => HashMap::new(),
            Ok(text) => parse_document(&text)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                ensure_parent_dir_exists(&path).await?;
                HashMap::new()
            }
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

    /// `players.toml` 由来の `PlayerRateRecord` で、まだ YAML 上に未登録のプレイヤ
    /// レコードを補填する。disk への書き戻しは行わず、cache を更新するのみ。
    ///
    /// **契約**: YAML 既存レコードは保護される（同名キーが既にあれば渡された
    /// `PlayerRateRecord` は捨てる）。これにより YAML 側で運用中の `:rate` /
    /// `:win` / `:loss` を上書きせず、TOML 側の値は「YAML 移行時の既存プレイヤ
    /// 補填」目的にのみ使われる。
    ///
    /// `players.toml` で定義された全プレイヤを LOGIN 経路で受け付けるための
    /// 起動時補填用。最初に終局して `record_game_outcome` が走った時点で
    /// `players.yaml` に書き戻される。
    pub fn ensure_default_records<I>(&self, defaults: I)
    where
        I: IntoIterator<Item = PlayerRateRecord>,
    {
        let mut cache = lock_cache(&self.cache);
        for rec in defaults {
            cache.entry(rec.name.as_str().to_owned()).or_insert(rec);
        }
    }

    fn snapshot(&self) -> HashMap<String, PlayerRateRecord> {
        lock_cache(&self.cache).clone()
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
        let result = lock_cache(&self.cache).get(name.as_str()).cloned();
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
                let mut cache = lock_cache(&self.cache);
                cache.insert(key, value);
            }
            let snapshot = self.snapshot();
            self.flush_to_disk(&snapshot).await
        }
    }

    fn list_all(
        &self,
    ) -> impl std::future::Future<Output = Result<Vec<PlayerRateRecord>, StorageError>> {
        let snapshot: Vec<PlayerRateRecord> = lock_cache(&self.cache).values().cloned().collect();
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
        debug_assert_ne!(
            black, white,
            "record_game_outcome: black and white must be distinct players",
        );
        let black_str = black.as_str().to_owned();
        let white_str = white.as_str().to_owned();
        // self-play 防護: 同名を渡した場合は二重加算を防ぐため `for` ループに
        // 入る前に同一性チェックする。trait 既定実装と同じ契約を保つ。
        let same_player = black_str == white_str;
        let winner_str = winner.map(|w| w.as_str().to_owned());
        let game_id_owned = game_id.clone();
        let now_owned = now_iso.to_owned();
        async move {
            if same_player {
                return Ok(());
            }
            let _guard = self.disk_lock.lock().await;
            {
                let mut cache = lock_cache(&self.cache);
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

/// `path` の親ディレクトリが存在することを検証する（[`load_from_file`] の
/// fail-fast 用）。`path` が単純なファイル名（親が空 / `Path::new(".")` 相当）
/// の場合はカレントディレクトリが暗黙的な親なので存在チェックは省略する。
///
/// `tokio::fs::try_exists` は対象が存在しないときに `Ok(false)`、stat 自体が
/// permission denied 等で失敗した場合に `Err` を返す。後者は親が存在しない
/// 設定ミスとは別軸の運用障害（権限不足）なので、エラーメッセージにパスと
/// 原因の両方を残して `Io` で上に伝える。
async fn ensure_parent_dir_exists(path: &Path) -> Result<(), StorageError> {
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => return Ok(()),
    };
    let exists = fs::try_exists(parent).await.map_err(|e| {
        StorageError::Io(format!(
            "stat parent dir {} for {}: {}",
            parent.display(),
            path.display(),
            e
        ))
    })?;
    if !exists {
        return Err(StorageError::Io(format!(
            "players.yaml parent directory does not exist: {} (for {})",
            parent.display(),
            path.display()
        )));
    }
    Ok(())
}

async fn atomic_write_yaml(path: &Path, contents: &str) -> Result<(), StorageError> {
    // `rename(2)` は同一ファイルシステム上で atomic なので tmpfile は隣接ディレクトリ
    // に作る。tmpfile 名衝突防止の構造:
    //
    //   - **異プロセス間**: `pid` で一意化する（PID が同時刻に再利用されるケースは
    //     OS が pid 再利用を防ぐため実質ゼロ）。
    //   - **同プロセス内**: `static AtomicU64` の `seq` が strict monotonic に増え、
    //     同 pid 内では絶対衝突しない。
    //   - `nanos` (subsec) は grep / 障害解析用の時刻ヒントで、衝突防止の根拠では
    //     ない。`pid + seq` で衝突しないので不要にも見えるが、tmp 残骸が事後解析で
    //     見つかった際に「いつ生成されたか」が分かるよう残す。
    //
    // 加えて `create_new(true)` を使うので、運悪く衝突した場合は `AlreadyExists` Err で
    // 検出可能。攻撃者・他テナントが先回りして同名 symlink を張った場合も `create_new`
    // が follow せず `EEXIST` で落ちる。
    //
    // dotfile 接頭辞は ls で隠して "中間ファイル" を示す慣習。
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
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".{stem}.rshogi-tmp.{pid}.{nanos}.{seq}"));

    // tmpfile が rename 完走前に Err 経路へ落ちた場合の残骸を確実に掃除するための
    // RAII ガード。`renamed = true` を立てると Drop で残骸削除をスキップする。
    struct TmpCleanup<'a> {
        tmp: &'a Path,
        renamed: bool,
    }
    impl Drop for TmpCleanup<'_> {
        fn drop(&mut self) {
            if !self.renamed {
                let _ = std::fs::remove_file(self.tmp);
            }
        }
    }
    let mut cleanup = TmpCleanup {
        tmp: &tmp,
        renamed: false,
    };

    // unix では既存 `players.yaml` の mode を継承して tmp を開く（`chmod 0600` 運用で
    // 人手で締めた permission が rename で 0644 に緩む事故を防ぐ）。Windows / その他
    // 非 unix では mode 概念がないので素直に `create_new` のみで開く。
    let mut open_options = fs::OpenOptions::new();
    open_options.write(true).create_new(true);
    // unix では `tokio::fs::OpenOptions::mode` が直接生えており trait import 不要。
    // `Permissions::mode` 側のみ `std::os::unix::fs::PermissionsExt` を要する。
    #[cfg(unix)]
    if let Ok(meta) = std::fs::metadata(path) {
        use std::os::unix::fs::PermissionsExt as _;
        open_options.mode(meta.permissions().mode() & 0o7777);
    }
    let mut file = open_options
        .open(&tmp)
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
    cleanup.renamed = true;
    drop(cleanup);

    // unix では `rename(2)` のディレクトリエントリ更新は親 inode の fsync を行うまで
    // 永続化が保証されない（ext4 / xfs の crash recovery で rename 結果が古い状態に
    // 戻るケースが知られている）。`tokio::fs::OpenOptions::open(dir).sync_all()` で
    // 親ディレクトリを fsync してクラッシュ耐性を確保する。
    //
    // Windows / 非 unix ではディレクトリ open のセマンティクス自体が異なる
    // (`CreateFileW` は `FILE_FLAG_BACKUP_SEMANTICS` 必須等) ため、ディレクトリ fsync
    // は no-op として扱う。本サーバは VPS Linux 運用が一次ターゲットで、Windows は
    // 開発・テスト経路の便宜上のサポート。
    #[cfg(unix)]
    {
        let dir_handle = fs::OpenOptions::new()
            .read(true)
            .open(dir)
            .await
            .map_err(|e| StorageError::Io(format!("open dir {}: {}", dir.display(), e)))?;
        dir_handle
            .sync_all()
            .await
            .map_err(|e| StorageError::Io(format!("fsync dir {}: {}", dir.display(), e)))?;
    }
    #[cfg(not(unix))]
    {
        // 非 unix では no-op。`dir` 引数を未使用警告から逃すため明示的に消費する。
        let _ = dir;
    }
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

    /// `last_game_id` が `None` のレコードは `:last_game_id` 行ごと出力から
    /// 除外される（serde の `skip_serializing_if`）。Ruby 側で `if rec[:last_game_id]`
    /// truthiness チェックする実装でも、`has_key?(:last_game_id)` で存在判定する
    /// 実装でも、どちらにも互換な見え方になる。`null` リテラルは Ruby `YAML.dump`
    /// が出さない形なので避ける。
    #[test]
    fn render_document_omits_last_game_id_when_none() {
        let mut records = HashMap::new();
        records.insert(
            "alice".to_owned(),
            PlayerRateRecord {
                name: PlayerName::new("alice"),
                rate: 1500,
                wins: 0,
                losses: 0,
                last_game_id: None,
                last_modified: "2026-04-26T00:00:00+00:00".to_owned(),
            },
        );
        let yaml = render_document(&records).unwrap();
        let expected = concat!(
            "alice:\n",
            "  :name: alice\n",
            "  :rate: 1500\n",
            "  :win: 0\n",
            "  :loss: 0\n",
            "  :last_modified: 2026-04-26T00:00:00+00:00\n",
        );
        assert_eq!(yaml, expected);
        // round-trip しても同じ shape を保つ（None で deserialize される）。
        let parsed = parse_document(&yaml).unwrap();
        assert_eq!(parsed, records);
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

    /// **P2 回帰固定**: ファイル本体が無くても、親ディレクトリが存在しない
    /// 場合は起動段階で `StorageError::Io` を返して fail-fast する。これを
    /// 許容すると初回終局時の atomic write が必ず失敗する形で運用障害が遅延
    /// 顕在化するため、accept ループに入る前に検知する契約を固定する。
    #[tokio::test(flavor = "current_thread")]
    async fn load_from_file_fails_fast_when_parent_dir_missing() {
        let dir = tempdir();
        // 親ディレクトリ自体が存在しないパスを指す（typo / mount 漏れの設定ミス）。
        let nonexistent_parent = dir.path().join("does-not-exist");
        let path = nonexistent_parent.join("players.yaml");
        let err = PlayersYamlRateStorage::load_from_file(path).await.unwrap_err();
        let msg = format!("{err}");
        assert!(matches!(err, StorageError::Io(_)), "expected Io error, got: {msg}");
        assert!(
            msg.contains("parent directory"),
            "error message should mention parent directory: {msg}"
        );
    }

    /// 親ディレクトリは存在するがファイル本体が無いケースは初回起動の正常経路
    /// なので、空マップで起動できる。`load_from_file_returns_empty_when_file_missing`
    /// と等価だが、parent 検証経路を踏むことで P2 修正の正常系副作用が無いこと
    /// を明示する。
    #[tokio::test(flavor = "current_thread")]
    async fn load_from_file_accepts_missing_file_when_parent_dir_exists() {
        let dir = tempdir();
        let path = dir.path().join("players.yaml");
        // 親 (`dir`) は tempdir で生成済み、ファイル自体は無い状態。
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

    /// `ensure_default_records` 経由で初期レコードを差し込むテストヘルパ。
    /// 旧 API（`names + initial_rate + now_iso`）の呼び出し記述を集約し、
    /// 新 API（`IntoIterator<Item = PlayerRateRecord>`）への移行で
    /// 各テストの差分を最小化する。
    fn seed_default_records(
        storage: &PlayersYamlRateStorage,
        names: &[&str],
        initial_rate: i32,
        now_iso: &str,
    ) {
        let defaults = names.iter().map(|n| PlayerRateRecord {
            name: PlayerName::new(*n),
            rate: initial_rate,
            wins: 0,
            losses: 0,
            last_game_id: None,
            last_modified: now_iso.to_owned(),
        });
        storage.ensure_default_records(defaults);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn record_game_outcome_increments_winner_and_loser_atomically() {
        let dir = tempdir();
        let path = dir.path().join("players.yaml");
        let storage = PlayersYamlRateStorage::load_from_file(path.clone()).await.unwrap();
        seed_default_records(&storage, &["alice", "bob"], 1500, "2026-04-26T00:00:00+00:00");

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
        seed_default_records(&storage, &["alice", "bob"], 1500, "2026-04-26T00:00:00+00:00");

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

    /// `record_game_outcome` の契約: `black == white` が誤って渡されても
    /// `wins`/`losses` の二重加算が発生しない（self-play 防護）。debug ビルドでは
    /// `debug_assert_ne!` が落とすが、release ビルドでも `Ok(())` で早期 return
    /// して整合性を保つ。
    #[cfg(not(debug_assertions))]
    #[tokio::test(flavor = "current_thread")]
    async fn record_game_outcome_rejects_self_play_in_release_build() {
        let dir = tempdir();
        let path = dir.path().join("players.yaml");
        let storage = PlayersYamlRateStorage::load_from_file(path).await.unwrap();
        storage.save(&rec("alice", 1500, 5, 3)).await.unwrap();

        let alice = PlayerName::new("alice");
        let game_id = GameId::new("20260426-self");
        // 同名を black / white 両方に渡しても 2 重加算しない。
        storage
            .record_game_outcome(
                &alice,
                &alice,
                Some(&alice),
                &game_id,
                "2026-04-26T16:00:00+00:00",
            )
            .await
            .unwrap();
        let r = storage.load(&alice).await.unwrap().unwrap();
        // wins/losses は据置（save 時の値そのまま）。
        assert_eq!(r.wins, 5);
        assert_eq!(r.losses, 3);
    }

    /// 日本語ハンドル名 / 数字始まりハンドル名など serde_yaml が quote を要する
    /// 可能性がある文字列で round-trip する契約を固定する。Ruby 互換 YAML として、
    /// UTF-8 名前の対局者を扱える運用を保証する。
    ///
    /// (1) round-trip で意味的同値を保つ
    /// (2) 日本語ハンドルは bare key として出力される（`田中太郎:` の出現を pin）
    /// (3) 数字 only ハンドル `12345` は YAML 1.2 で integer scalar に解釈されうる
    ///     ため `serde_yaml` は string quote を付ける。`'12345':` の出現を pin する
    ///     （quote の有無は仕様変更で破壊的変化があるため byte 列で固定する）。
    #[test]
    fn render_and_parse_round_trip_handles_unicode_and_numeric_names() {
        let mut records = HashMap::new();
        records.insert("田中太郎".to_owned(), rec("田中太郎", 1500, 0, 0));
        records.insert("12345".to_owned(), rec("12345", 1700, 10, 5));
        let yaml = render_document(&records).unwrap();
        let parsed = parse_document(&yaml).unwrap();
        assert_eq!(parsed, records, "Unicode / numeric handles must round-trip");

        // (2): 日本語ハンドルは bare で出力される。
        assert!(yaml.contains("田中太郎:\n"), "non-ASCII handle must be unquoted, got:\n{yaml}",);
        // (3): 数字 only ハンドルは integer 解釈を防ぐため quote される。
        // serde_yaml 0.9 は single quote を採用するが、double quote 採用 minor バージョン
        // 変化に保険を掛けるため両方許容する assertion にする。
        assert!(
            yaml.contains("'12345':\n") || yaml.contains("\"12345\":\n"),
            "numeric handle must be string-quoted to avoid YAML integer coercion, got:\n{yaml}",
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ensure_default_records_does_not_overwrite_existing() {
        let dir = tempdir();
        let path = dir.path().join("players.yaml");
        let storage = PlayersYamlRateStorage::load_from_file(path).await.unwrap();
        storage.save(&rec("alice", 2500, 100, 50)).await.unwrap();

        // alice は既に rate=2500/wins=100 で記録済み。ensure_default_records が
        // これを TOML 由来の既定値で上書きしないことを確認。
        seed_default_records(&storage, &["alice", "bob"], 1500, "2026-04-26T15:00:00+00:00");
        let alice_rec = storage.load(&PlayerName::new("alice")).await.unwrap().unwrap();
        assert_eq!(alice_rec.rate, 2500);
        assert_eq!(alice_rec.wins, 100);
        let bob_rec = storage.load(&PlayerName::new("bob")).await.unwrap().unwrap();
        assert_eq!(bob_rec.rate, 1500);
        assert_eq!(bob_rec.wins, 0);
    }

    /// **P1 回帰固定**: TOML 由来の `PlayerRateRecord`（rate / wins / losses が
    /// 1500 以外）が YAML 未登録プレイヤの補填値として実際に使われることを検証する。
    /// 旧実装は `into_keys()` で名前のみ抽出して固定値 `1500` で補填していたため
    /// TOML 側の運用値を黙って失う bug があった。新 API では `PlayerRateRecord`
    /// 全体を受け取るため、未登録分は TOML 値そのままで in-memory 登録される。
    #[tokio::test(flavor = "current_thread")]
    async fn ensure_default_records_uses_toml_rate_wins_losses_for_unseen_players() {
        let dir = tempdir();
        let path = dir.path().join("players.yaml");
        let storage = PlayersYamlRateStorage::load_from_file(path).await.unwrap();
        // YAML には何もない状態。TOML 側で carol が rate=1800/wins=12/losses=7
        // と運用されているケースを模倣する。
        let toml_seed = vec![PlayerRateRecord {
            name: PlayerName::new("carol"),
            rate: 1800,
            wins: 12,
            losses: 7,
            last_game_id: None,
            last_modified: "2026-04-26T15:00:00+00:00".to_owned(),
        }];
        storage.ensure_default_records(toml_seed);
        let carol = storage.load(&PlayerName::new("carol")).await.unwrap().unwrap();
        assert_eq!(carol.rate, 1800, "TOML rate must be honored for unseen players");
        assert_eq!(carol.wins, 12, "TOML wins must be honored for unseen players");
        assert_eq!(carol.losses, 7, "TOML losses must be honored for unseen players");
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
