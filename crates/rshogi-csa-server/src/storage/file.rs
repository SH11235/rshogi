//! `KifuStorage` のローカルファイル実装。
//!
//! - `<topdir>/YYYY/MM/DD/<game_id>.csa` に CSA V2 棋譜を書き込む。
//!   日付は `game_id` 末尾の 8 桁が `YYYYMMDD` 形式である前提だが、形式が
//!   合わなければ `start_time` から推定する代わりにフォールバックの `unknown/`
//!   ディレクトリへ落とす（現状は緩く扱う方針）。
//! - 00LIST は `<topdir>/00LIST` にスペース区切り 1 行として追記する。
//! - 書き込みは原子的: まず一時ファイルに書いてから `rename` で確定する
//!   （中断してもファイルが半端な状態にならないようにするため）。
//!
//! `tokio-transport` フィーチャ下でのみコンパイルされる（`tokio::fs` が必要なため）。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

use crate::error::StorageError;
use crate::port::{GameSummaryEntry, KifuStorage};
use crate::types::{GameId, StorageKey};

/// ローカルディレクトリへ CSA V2 棋譜と 00LIST を書き出す `KifuStorage`。
///
/// `append_summary` は同一インスタンス内で `Mutex` で直列化される。
/// 同一 `topdir` を別インスタンスから複数プロセスで叩くケースは想定外
/// （TCP サーバーは 1 プロセス・1 ストレージインスタンス）。
#[derive(Debug, Clone)]
pub struct FileKifuStorage {
    topdir: PathBuf,
    /// 00LIST 追記の同一プロセス直列化ロック（複数行の write が交錯しないため）。
    append_lock: Arc<Mutex<()>>,
}

impl FileKifuStorage {
    /// 指定ディレクトリ配下に書き込む新規ストレージを作る。
    ///
    /// `topdir` が存在しなくても OK（書き込み時に再帰生成する）。
    pub fn new<P: Into<PathBuf>>(topdir: P) -> Self {
        Self {
            topdir: topdir.into(),
            append_lock: Arc::new(Mutex::new(())),
        }
    }

    /// 棋譜ファイルの相対パス（`YYYY/MM/DD/<game_id>.csa`）を組み立てる。
    ///
    /// 先頭 8 文字を `chrono::NaiveDate` で厳密に YYYYMMDD として検証し、
    /// 不正日付（例 `20261340...`）や数字以外を含むものは `unknown/` に落とす。
    fn relative_kifu_path(&self, game_id: &GameId) -> PathBuf {
        let id = game_id.as_str();
        if id.len() >= 8 {
            let head = &id[0..8];
            if chrono::NaiveDate::parse_from_str(head, "%Y%m%d").is_ok() {
                let yyyy = &head[0..4];
                let mm = &head[4..6];
                let dd = &head[6..8];
                return PathBuf::from(yyyy).join(mm).join(dd).join(format!("{id}.csa"));
            }
        }
        PathBuf::from("unknown").join(format!("{id}.csa"))
    }

    fn zerozero_list_path(&self) -> PathBuf {
        self.topdir.join("00LIST")
    }
}

impl KifuStorage for FileKifuStorage {
    async fn save(&self, game_id: &GameId, csa_v2_text: &str) -> Result<StorageKey, StorageError> {
        let rel = self.relative_kifu_path(game_id);
        let abs = self.topdir.join(&rel);
        let parent =
            abs.parent().ok_or_else(|| StorageError::Io(format!("no parent for {abs:?}")))?;
        fs::create_dir_all(parent)
            .await
            .map_err(|e| StorageError::Io(format!("create_dir_all {parent:?}: {e}")))?;
        atomic_write(&abs, csa_v2_text.as_bytes()).await?;
        // StorageKey は呼び出し側が次のロード時に再現できる相対パス文字列にする。
        Ok(StorageKey::new(rel.to_string_lossy().into_owned()))
    }

    async fn load(&self, game_id: &GameId) -> Result<Option<String>, StorageError> {
        let rel = self.relative_kifu_path(game_id);
        let abs = self.topdir.join(&rel);
        match fs::read_to_string(&abs).await {
            Ok(body) => Ok(Some(body)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(StorageError::Io(format!("read {abs:?}: {e}"))),
        }
    }

    async fn append_summary(&self, entry: &GameSummaryEntry) -> Result<(), StorageError> {
        // 同一 FileKifuStorage インスタンスからの append を直列化する。
        // `tokio::fs::File::write_all` は内部で複数 write を呼び得るため、
        // 同時呼び出しがあると POSIX O_APPEND だけでは行内交錯を防げない。
        // 1 プロセス前提で Mutex 1 つの直列化で十分。複数プロセスから同一
        // ディレクトリを叩く運用が出てきたら flock(2) を併用する想定。
        let _guard = self.append_lock.lock().await;
        let path = self.zerozero_list_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| StorageError::Io(format!("create_dir_all {parent:?}: {e}")))?;
        }
        let line = format!(
            "{} {} {} {} {} {}\n",
            entry.game_id,
            entry.sente,
            entry.gote,
            entry.start_time,
            entry.end_time,
            entry.result_code,
        );
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
            .map_err(|e| StorageError::Io(format!("open {path:?}: {e}")))?;
        f.write_all(line.as_bytes())
            .await
            .map_err(|e| StorageError::Io(format!("write {path:?}: {e}")))?;
        f.flush().await.map_err(|e| StorageError::Io(format!("flush {path:?}: {e}")))
    }
}

/// 原子的書き込み: 一時ファイルに書いた後 `rename` で目的のパスに置き換える。
async fn atomic_write(target: &Path, contents: &[u8]) -> Result<(), StorageError> {
    let tmp_path = tmp_sibling(target);
    {
        let mut f = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp_path)
            .await
            .map_err(|e| StorageError::Io(format!("create tmp {tmp_path:?}: {e}")))?;
        f.write_all(contents)
            .await
            .map_err(|e| StorageError::Io(format!("write tmp {tmp_path:?}: {e}")))?;
        f.sync_all()
            .await
            .map_err(|e| StorageError::Io(format!("sync tmp {tmp_path:?}: {e}")))?;
    }
    fs::rename(&tmp_path, target)
        .await
        .map_err(|e| StorageError::Io(format!("rename {tmp_path:?} -> {target:?}: {e}")))
}

/// 同じディレクトリ内で衝突しにくい一時ファイル名を作る。
fn tmp_sibling(target: &Path) -> PathBuf {
    let mut tmp = target.to_path_buf();
    let mut name = target.file_name().map(|s| s.to_owned()).unwrap_or_default();
    // PID とナノ秒タイムスタンプで一意化（POSIX 上で同一プロセス内の衝突は実質起きない）。
    let ts_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    name.push(format!(".tmp.{pid}.{ts_ns}"));
    tmp.set_file_name(name);
    tmp
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::PlayerName;

    fn unique_topdir(tag: &str) -> PathBuf {
        let pid = std::process::id();
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("rshogi_csa_server_test_{tag}_{pid}_{ts}"))
    }

    #[tokio::test(flavor = "current_thread")]
    async fn save_writes_kifu_under_dated_directory() {
        let dir = unique_topdir("save_dated");
        let store = FileKifuStorage::new(&dir);
        let game_id = GameId::new("20260417120000");
        let key = store.save(&game_id, "V2.2\nN+alice\n").await.unwrap();
        assert_eq!(key.as_str(), "2026/04/17/20260417120000.csa");
        let abs = dir.join(key.as_str());
        let body = fs::read_to_string(&abs).await.unwrap();
        assert_eq!(body, "V2.2\nN+alice\n");
        // テストの後始末。
        let _ = fs::remove_dir_all(&dir).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn load_returns_saved_kifu_text() {
        let dir = unique_topdir("load_saved");
        let store = FileKifuStorage::new(&dir);
        let game_id = GameId::new("20260417120000");
        store.save(&game_id, "V2.2\nN+alice\n").await.unwrap();
        let body = store.load(&game_id).await.unwrap();
        assert_eq!(body.as_deref(), Some("V2.2\nN+alice\n"));
        let _ = fs::remove_dir_all(&dir).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn load_returns_none_for_unknown_game() {
        let dir = unique_topdir("load_unknown");
        let store = FileKifuStorage::new(&dir);
        let body = store.load(&GameId::new("20260417120000")).await.unwrap();
        assert_eq!(body, None);
        let _ = fs::remove_dir_all(&dir).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn save_falls_back_to_unknown_for_non_dated_id() {
        let dir = unique_topdir("save_unknown");
        let store = FileKifuStorage::new(&dir);
        let game_id = GameId::new("buoy-123");
        let key = store.save(&game_id, "V2.2\n").await.unwrap();
        assert_eq!(key.as_str(), "unknown/buoy-123.csa");
        let _ = fs::remove_dir_all(&dir).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn save_falls_back_to_unknown_for_invalid_calendar_date() {
        // 13 月や 40 日のような数字だけど不正な日付は unknown へ落とす。
        let dir = unique_topdir("save_invalid_date");
        let store = FileKifuStorage::new(&dir);
        let game_id = GameId::new("20261340abcd");
        let key = store.save(&game_id, "V2.2\n").await.unwrap();
        assert_eq!(key.as_str(), "unknown/20261340abcd.csa");
        let _ = fs::remove_dir_all(&dir).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn save_overwrites_existing_file_atomically() {
        let dir = unique_topdir("save_overwrite");
        let store = FileKifuStorage::new(&dir);
        let game_id = GameId::new("20260417120000");
        store.save(&game_id, "first\n").await.unwrap();
        // 2 度目の save は rename で上書き成功する。
        store.save(&game_id, "second\n").await.unwrap();
        let abs = dir.join("2026/04/17/20260417120000.csa");
        let body = fs::read_to_string(&abs).await.unwrap();
        assert_eq!(body, "second\n");
        let _ = fs::remove_dir_all(&dir).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn append_summary_smoke_test_under_concurrent_load() {
        // Multi-thread runtime + JoinSet で 50 件並列 append を発火する。
        // OS の write(2) アトミック性に依存しなくても 50 行が壊れず保存されることを
        // 確認する「スモークテスト」。`append_lock` が無くても OS の write 追記が
        // 単一 syscall で完了するサイズなら通ってしまうため、本テストはロック欠落の
        // 回帰検出までは保証しない。ロック欠落を決定的に検出したい場合は
        // [`append_lock_serializes_critical_section`] を参照。
        let dir = unique_topdir("append_smoke");
        let store = FileKifuStorage::new(&dir);
        let mut set = tokio::task::JoinSet::new();
        for i in 0..50_u32 {
            let s = store.clone();
            let id = format!("g{i:03}");
            set.spawn(async move {
                s.append_summary(&GameSummaryEntry {
                    game_id: GameId::new(&id),
                    sente: PlayerName::new("alice"),
                    gote: PlayerName::new("bob"),
                    start_time: "2026-04-17T12:00:00Z".to_owned(),
                    end_time: "2026-04-17T12:10:00Z".to_owned(),
                    result_code: "#RESIGN".to_owned(),
                })
                .await
            });
        }
        while let Some(r) = set.join_next().await {
            r.expect("join").expect("append_summary");
        }
        let body = fs::read_to_string(dir.join("00LIST")).await.unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 50);
        for line in lines {
            assert_eq!(line.split(' ').count(), 6, "bad line: {line:?}");
        }
        let _ = fs::remove_dir_all(&dir).await;
    }

    /// `append_summary` が確かに `append_lock` を取得することを、外側で同じロックを
    /// 保持して append_summary が完了しないことから直接検証する回帰テスト。
    ///
    /// `append_summary` から `let _guard = self.append_lock.lock().await;` を消すと、
    /// 外側ロック保持中でも spawned task の append_summary が完了し、
    /// 以下のいずれかの assert で必ず失敗する。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn append_summary_acquires_append_lock_for_critical_section() {
        let dir = unique_topdir("lock_held");
        let store = FileKifuStorage::new(&dir);
        // 1. 外側でロックを保持。
        let outer_guard = store.append_lock.clone().lock_owned().await;

        // 2. spawned task が append_summary を呼ぶ前に oneshot で signal。
        //    こうすることで「task が起動済み・実体に到達済み」が確定する。
        let (entry_tx, entry_rx) = tokio::sync::oneshot::channel::<()>();
        let s = store.clone();
        let join = tokio::spawn(async move {
            let _ = entry_tx.send(());
            s.append_summary(&GameSummaryEntry {
                game_id: GameId::new("g1"),
                sente: PlayerName::new("alice"),
                gote: PlayerName::new("bob"),
                start_time: "2026-04-17T12:00:00Z".to_owned(),
                end_time: "2026-04-17T12:10:00Z".to_owned(),
                result_code: "#RESIGN".to_owned(),
            })
            .await
        });
        entry_rx.await.expect("spawned task started");

        // 3. ロック保持中は append_summary が完了してはいけない。
        //    通常の I/O 完了は数 ms 以内。1 秒間ポーリングして「一度も完了しない」ことを
        //    決定的に検証する。途中で完了したら panic（ロック取得が外されている可能性）。
        for _ in 0..100 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            assert!(
                !join.is_finished(),
                "append_summary が外側ロック中に完了。production の append_lock 取得が外れた疑い。"
            );
        }

        // 4. ロック解放後は append_summary が完了するはず。
        drop(outer_guard);
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), join)
            .await
            .expect("append_summary should complete after lock release");
        result.expect("join").expect("append_summary");
        let _ = fs::remove_dir_all(&dir).await;
    }

    /// `FileKifuStorage` の内部 `Mutex` 自体が直列化として正しく機能することを、
    /// 競合カウンタで補強的に確認する（`Mutex` の使い方の正しさを担保）。
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn append_lock_serializes_critical_section() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let store = FileKifuStorage::new(unique_topdir("lock_probe"));
        let active = std::sync::Arc::new(AtomicU32::new(0));
        let max_observed = std::sync::Arc::new(AtomicU32::new(0));

        let mut set = tokio::task::JoinSet::new();
        for _ in 0..32 {
            let s = store.clone();
            let active = active.clone();
            let max_observed = max_observed.clone();
            set.spawn(async move {
                // 同じ `append_lock` を取って小休止を挟む。
                let _guard = s.append_lock.lock().await;
                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                let mut prev = max_observed.load(Ordering::SeqCst);
                while now > prev
                    && let Err(e) =
                        max_observed.compare_exchange(prev, now, Ordering::SeqCst, Ordering::SeqCst)
                {
                    prev = e;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                active.fetch_sub(1, Ordering::SeqCst);
            });
        }
        while let Some(r) = set.join_next().await {
            r.expect("join");
        }
        // クリティカルセクション内に同時に存在したタスクは常に 1 を超えてはならない。
        assert_eq!(max_observed.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn append_summary_writes_one_line_per_call() {
        let dir = unique_topdir("append_summary");
        let store = FileKifuStorage::new(&dir);
        let entry = |id: &str| GameSummaryEntry {
            game_id: GameId::new(id),
            sente: PlayerName::new("alice"),
            gote: PlayerName::new("bob"),
            start_time: "2026-04-17T12:00:00Z".to_owned(),
            end_time: "2026-04-17T12:10:00Z".to_owned(),
            result_code: "#RESIGN".to_owned(),
        };
        store.append_summary(&entry("g1")).await.unwrap();
        store.append_summary(&entry("g2")).await.unwrap();
        let body = fs::read_to_string(dir.join("00LIST")).await.unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("g1 alice bob"));
        assert!(lines[1].starts_with("g2 alice bob"));
        assert!(lines[0].ends_with("#RESIGN"));
        let _ = fs::remove_dir_all(&dir).await;
    }
}
