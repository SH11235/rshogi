//! JSONL 形式の Floodgate 履歴ストレージ実装（tokio ランタイム前提）。
//!
//! 追記は同一インスタンスの Mutex で直列化する。他プロセスからの同時追記には
//! 対応しない。ローテーションは外部ツールへ任せ、照会は必要な末尾行だけを読む。

use std::io::SeekFrom;
use std::path::PathBuf;

use tokio::fs::OpenOptions;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::Mutex as AsyncMutex;

use crate::error::StorageError;

use super::port::FloodgateHistoryStorage;
use super::types::FloodgateHistoryEntry;

/// JSONL 形式（1 entry = 1 line）でファイルに append-only 記録する `FloodgateHistoryStorage`
/// 実装。
///
/// `append` は内部 `AsyncMutex` で直列化する。同一ファイルへの別インスタンスや
/// 他プロセスからの同時追記には対応しない。
/// `list_recent` は末尾から必要な行だけを読み、履歴全体をメモリへ読み込まない。
#[derive(Debug)]
pub struct JsonlFloodgateHistoryStorage {
    path: PathBuf,
    /// append を直列化する async lock。`list_recent` は別 read 経路なのでロック
    /// 範囲は短い。
    append_lock: AsyncMutex<()>,
}

impl JsonlFloodgateHistoryStorage {
    /// 指定パスをベースに storage を構築する。ファイルは存在しなくてよく、
    /// 最初の `append` で作成される。`load_recent` ではファイル不在を空 Vec で
    /// 返す。
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            append_lock: AsyncMutex::new(()),
        }
    }
}

impl FloodgateHistoryStorage for JsonlFloodgateHistoryStorage {
    fn append(
        &self,
        entry: &FloodgateHistoryEntry,
    ) -> impl std::future::Future<Output = Result<(), StorageError>> {
        // serde_json による serialize は I/O より前に同期で完了するため、async
        // ブロックの外で実行して成否を `Result` に畳み、async ブロック内で `?`
        // 経由で伝播する（早期 return パスと async ブロックパスの戻り型が
        // 異なる問題を回避する）。
        let serialized: Result<String, StorageError> = serde_json::to_string(entry)
            .map_err(|e| StorageError::Io(format!("serialize FloodgateHistoryEntry: {e}")));
        async move {
            let line = serialized?;
            let _guard = self.append_lock.lock().await;
            // 親ディレクトリ未作成時のフェイル連発を防ぐ。`--floodgate-history-jsonl`
            // にデプロイ初回などで存在しないディレクトリ配下のパスを指定された場合、
            // `OpenOptions::open` だけだと毎ゲーム ENOENT で `StorageError::Io` を
            // 返し続けて「履歴 opt-in したのに永続化が常に失敗する」状態になる。
            // `create_dir_all` は idempotent でディレクトリが既にあれば no-op、
            // ファイル作成より遥かに低頻度なので append-lock 内で一度ずつ呼んで OK。
            if let Some(parent) = self.path.parent()
                && !parent.as_os_str().is_empty()
            {
                tokio::fs::create_dir_all(parent).await.map_err(|e| {
                    StorageError::Io(format!("create_dir_all {}: {}", parent.display(), e))
                })?;
            }
            let mut file =
                OpenOptions::new().create(true).append(true).open(&self.path).await.map_err(
                    |e| StorageError::Io(format!("open {} (append): {}", self.path.display(), e)),
                )?;
            // 1 entry につき 1 行 + `\n` を 1 回の write で出す。`write_all` は
            // 内部で複数 syscall に分割し得るが、`O_APPEND` 下では各 write が
            // 末尾に書かれるので順序は保たれる。
            let mut payload = line.into_bytes();
            payload.push(b'\n');
            file.write_all(&payload)
                .await
                .map_err(|e| StorageError::Io(format!("append entry: {e}")))?;
            file.flush().await.map_err(|e| StorageError::Io(format!("flush entry: {e}")))?;
            Ok(())
        }
    }

    fn list_recent(
        &self,
        limit: usize,
    ) -> impl std::future::Future<Output = Result<Vec<FloodgateHistoryEntry>, StorageError>> {
        let path = self.path.clone();
        async move {
            if limit == 0 {
                return Ok(Vec::new());
            }
            let mut file = match tokio::fs::File::open(&path).await {
                Ok(file) => file,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
                Err(e) => return Err(StorageError::Io(format!("read {}: {}", path.display(), e))),
            };
            // 必要な末尾行だけ読む。行を組み立ててから UTF-8 検証し、ブロック境界の分断を許容する。
            let mut offset = file.metadata().await.map_err(history_io_error)?.len();
            let mut buffer = [0_u8; 8192];
            let mut reversed_line = Vec::new();
            let mut entries = Vec::new();
            while offset > 0 {
                let count = offset.min(buffer.len() as u64) as usize;
                offset -= count as u64;
                file.seek(SeekFrom::Start(offset)).await.map_err(history_io_error)?;
                file.read_exact(&mut buffer[..count]).await.map_err(history_io_error)?;
                for &byte in buffer[..count].iter().rev() {
                    if byte == b'\n' {
                        if let Some(entry) = parse_reversed_history_line(&mut reversed_line)? {
                            entries.push(entry);
                            if entries.len() == limit {
                                return Ok(entries);
                            }
                        }
                    } else {
                        reversed_line.push(byte);
                    }
                }
            }
            if let Some(entry) = parse_reversed_history_line(&mut reversed_line)? {
                entries.push(entry);
            }
            Ok(entries)
        }
    }
}

fn history_io_error(error: std::io::Error) -> StorageError {
    StorageError::Io(format!("read history: {error}"))
}

fn parse_reversed_history_line(
    bytes: &mut Vec<u8>,
) -> Result<Option<FloodgateHistoryEntry>, StorageError> {
    bytes.reverse();
    let line = std::str::from_utf8(bytes)
        .map_err(|error| StorageError::Io(format!("read history UTF-8: {error}")))?;
    // 不整合行を黙って読み飛ばすと、履歴を使う対戦集計が欠落する。
    let entry = if line.trim().is_empty() {
        None
    } else {
        Some(
            serde_json::from_str(line)
                .map_err(|error| StorageError::Malformed(format!("history line: {error}")))?,
        )
    };
    bytes.clear();
    Ok(entry)
}

#[cfg(test)]
mod tests {
    use super::super::types::HistoryColor;
    use super::*;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn entry(game_id: &str, winner: Option<HistoryColor>) -> FloodgateHistoryEntry {
        FloodgateHistoryEntry {
            game_id: game_id.to_owned(),
            game_name: "floodgate-600-10".to_owned(),
            black: "alice".to_owned(),
            white: "bob".to_owned(),
            start_time: "2026-04-26T12:00:00+00:00".to_owned(),
            end_time: "2026-04-26T12:30:00+00:00".to_owned(),
            result_code: "#RESIGN".to_owned(),
            winner,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn list_recent_returns_empty_when_file_missing() {
        let dir = tempdir();
        let path = dir.path().join("history.jsonl");
        let storage = JsonlFloodgateHistoryStorage::new(path);
        let entries = storage.list_recent(10).await.unwrap();
        assert!(entries.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn append_then_list_recent_returns_newest_first() {
        let dir = tempdir();
        let path = dir.path().join("history.jsonl");
        let storage = JsonlFloodgateHistoryStorage::new(path.clone());
        for n in 0..5 {
            storage
                .append(&entry(&format!("g{n}"), Some(HistoryColor::Black)))
                .await
                .unwrap();
        }
        // limit 3 で末尾 3 件（g4 / g3 / g2）を新しい順で取る契約。
        let recent = storage.list_recent(3).await.unwrap();
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].game_id, "g4");
        assert_eq!(recent[1].game_id, "g3");
        assert_eq!(recent[2].game_id, "g2");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn append_persists_across_storage_instances() {
        let dir = tempdir();
        let path = dir.path().join("history.jsonl");
        {
            let storage = JsonlFloodgateHistoryStorage::new(path.clone());
            storage.append(&entry("g1", None)).await.unwrap();
        }
        // 新しい instance で list_recent を呼んでも前回 append 内容が読める
        // （永続化要件 11.4 の本質）。
        let storage2 = JsonlFloodgateHistoryStorage::new(path);
        let recent = storage2.list_recent(10).await.unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].game_id, "g1");
        assert!(recent[0].winner.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn list_recent_rejects_malformed_lines() {
        let dir = tempdir();
        let path = dir.path().join("history.jsonl");
        // 不正な JSON 行を 1 行だけ書いた状態を作る。
        tokio::fs::write(&path, b"{not json}\n").await.unwrap();
        let storage = JsonlFloodgateHistoryStorage::new(path);
        let err = storage.list_recent(5).await.unwrap_err();
        assert!(matches!(err, StorageError::Malformed(_)), "got: {err:?}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn append_returns_err_when_parent_path_is_regular_file() {
        // 親ディレクトリ作成が失敗する経路（同名の regular file が居座っている）でも
        // panic せず `StorageError::Io` で上に返す契約を固定する。これにより
        // `persist_kifu` 側の `if let Err(e) = ... return Err(...)` 経路が
        // 型システム上だけでなく実 I/O 失敗で踏まれることを保証し、将来 silent な
        // best-effort 化に戻された場合に CI で気付ける。
        let dir = tempdir();
        let blocker = dir.path().join("blocker");
        // ファイルを作って、その配下にパスを作ろうとすると create_dir_all が
        // ENOTDIR で落ちる（regular file 上にディレクトリは作れない）。
        std::fs::write(&blocker, b"").unwrap();
        let bad_path = blocker.join("history.jsonl");
        let storage = JsonlFloodgateHistoryStorage::new(bad_path);
        let err = storage.append(&entry("g1", None)).await.unwrap_err();
        assert!(matches!(err, StorageError::Io(_)), "expected Io error, got: {err:?}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn append_creates_missing_parent_directories() {
        // `--floodgate-history-jsonl` にデプロイ初回で未作成のサブディレクトリを
        // 含むパスを指定された場合でも、append が ENOENT で失敗し続けず、ディレクトリ
        // ごと作って成功する契約。
        let dir = tempdir();
        let nested = dir.path().join("a").join("b").join("c").join("history.jsonl");
        let storage = JsonlFloodgateHistoryStorage::new(nested.clone());
        storage.append(&entry("g1", Some(HistoryColor::Black))).await.unwrap();
        let recent = storage.list_recent(5).await.unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].game_id, "g1");
        assert!(nested.exists(), "history file should be created at {nested:?}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn list_recent_skips_blank_lines() {
        // 末尾改行や空行が混ざっていてもパース継続する（外部 logrotate などで
        // 末尾に空行が挿入される運用を想定）。
        let dir = tempdir();
        let path = dir.path().join("history.jsonl");
        let storage = JsonlFloodgateHistoryStorage::new(path.clone());
        storage.append(&entry("g1", Some(HistoryColor::White))).await.unwrap();
        // 手で空行 + entry を追記
        let mut file = OpenOptions::new().append(true).open(&path).await.unwrap();
        file.write_all(b"\n").await.unwrap();
        let line = serde_json::to_string(&entry("g2", None)).unwrap();
        file.write_all(line.as_bytes()).await.unwrap();
        file.write_all(b"\n").await.unwrap();
        file.flush().await.unwrap();
        drop(file);
        let recent = storage.list_recent(10).await.unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].game_id, "g2");
        assert_eq!(recent[1].game_id, "g1");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn list_recent_matches_reverse_lines_across_blocks_and_utf8_boundaries() {
        let dir = tempdir();
        let path = dir.path().join("history.jsonl");
        let mut raw = String::new();
        for n in 0..32 {
            let mut record = entry(&format!("g{n}"), None);
            if n % 4 == 0 {
                record.black = "日本語".repeat(2000);
            }
            raw.push_str(&serde_json::to_string(&record).unwrap());
            raw.push_str(if n % 2 == 0 {
                "\r\n\u{3000}\r\n"
            } else {
                "\n\n"
            });
        }
        raw.push_str(&serde_json::to_string(&entry("last", None)).unwrap());
        tokio::fs::write(&path, &raw).await.unwrap();
        let storage = JsonlFloodgateHistoryStorage::new(path);
        for limit in [0, 1, 2, 7, 33, 100] {
            let expected: Vec<FloodgateHistoryEntry> = raw
                .lines()
                .rev()
                .filter(|line| !line.trim().is_empty())
                .take(limit)
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(storage.list_recent(limit).await.unwrap(), expected, "limit {limit}");
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn list_recent_reassembles_multibyte_character_split_at_block_boundary() {
        let dir = tempdir();
        let path = dir.path().join("history.jsonl");
        let mut record = entry("utf8", None);
        record.black = "あ".repeat(4000);
        let mut raw = serde_json::to_string(&record).unwrap();
        while raw.is_char_boundary(raw.len() - 8192) {
            record.white.push('x');
            raw = serde_json::to_string(&record).unwrap();
        }
        tokio::fs::write(&path, raw).await.unwrap();
        let storage = JsonlFloodgateHistoryStorage::new(path);
        assert_eq!(storage.list_recent(1).await.unwrap(), vec![record]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn list_recent_rejects_only_malformed_lines_inside_requested_tail() {
        let dir = tempdir();
        let path = dir.path().join("history.jsonl");
        let record = entry("last", None);
        let raw = format!("{{not json}}\r\n{}", serde_json::to_string(&record).unwrap());
        tokio::fs::write(&path, raw).await.unwrap();
        let storage = JsonlFloodgateHistoryStorage::new(path);
        assert_eq!(storage.list_recent(1).await.unwrap(), vec![record]);
        assert!(matches!(storage.list_recent(2).await, Err(StorageError::Malformed(_))));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn list_recent_rejects_unterminated_malformed_or_invalid_utf8_tail() {
        let dir = tempdir();
        let path = dir.path().join("history.jsonl");
        let storage = JsonlFloodgateHistoryStorage::new(path.clone());
        tokio::fs::write(&path, b"{not json}").await.unwrap();
        assert!(matches!(storage.list_recent(1).await, Err(StorageError::Malformed(_))));
        tokio::fs::write(&path, [0xff, b'\n']).await.unwrap();
        assert!(matches!(storage.list_recent(1).await, Err(StorageError::Io(_))));
        assert!(storage.list_recent(0).await.unwrap().is_empty());
    }

    /// テスト専用 RAII tempdir（`tempfile` クレート依存を避ける）。
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
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
    fn tempdir() -> TempDir {
        let base = std::env::temp_dir();
        let pid = std::process::id();
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = base.join(format!("rshogi-floodgate-history-{pid}-{n}"));
        std::fs::create_dir_all(&path).expect("create tempdir");
        TempDir { path }
    }
}
