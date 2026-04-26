//! Floodgate 履歴の永続化（開始時刻・ペア・結果を再起動跨ぎで参照可能にする）。
//!
//! 各対局の終局時に 1 件の [`FloodgateHistoryEntry`] を append-only な JSONL
//! ファイルに追記する。読み込み側（`%%FLOODGATE history` 等）は近傍 N 件を
//! tail で読む利用を想定し、行末改行で 1 entry = 1 line のフォーマットを採用。
//!
//! # 設計判断
//!
//! - **YAML ではなく JSONL**: 大量のエントリを高速に追記するなら 1 entry =
//!   1 line + `\n` の append-only が一番衝突しにくい。Ruby Floodgate の
//!   `floodgate.history.dump` 等の特定フォーマットを引き継ぐ要件は無く、本
//!   サーバ独自フォーマットで `serde_json::to_string` で安定生成する
//! - **append 単位の atomic 性**: ファイル末尾への append は POSIX 上 1 write
//!   == 1 syscall でブロック分の atomic 性が保証される（512B 程度）。1 entry が
//!   PIPE_BUF (= 4096B) を超えない範囲でオープン append-mode 書き込みが atomic
//!   なので、追加の lock は不要（同一プロセス内での同時 append は内部 Mutex で
//!   直列化）
//! - **クロスプロセス並行 append は非対応**: 単一プロセス前提。複数プロセスが
//!   同ファイルを書く運用は想定しない（YAGNI）
//! - **ローテーションは外部任せ**: ファイルサイズ管理は logrotate 等の外部
//!   ツールに任せ、本実装は append のみ責任を持つ

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex as AsyncMutex;

use crate::error::StorageError;
use crate::types::{Color, GameId, GameName, PlayerName};

/// Floodgate 履歴 1 件分のエントリ。`persist_kifu` 経由で終局確定時に
/// `FloodgateHistoryStorage::append` に渡される。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FloodgateHistoryEntry {
    /// 対局識別子（サーバ発行）。
    pub game_id: String,
    /// マッチが帰属する `game_name`（Floodgate スケジュールの分類軸と一致）。
    pub game_name: String,
    /// 先手プレイヤ名。
    pub black: String,
    /// 後手プレイヤ名。
    pub white: String,
    /// 対局開始時刻（UTC、RFC3339）。
    pub start_time: String,
    /// 対局終了時刻（UTC、RFC3339）。
    pub end_time: String,
    /// 終局理由コード（`#RESIGN` / `#TIME_UP` / `#ILLEGAL_MOVE` 等）。
    pub result_code: String,
    /// 勝者の色。引き分け（千日手・最大手数）や勝敗不確定の `#ABNORMAL` では
    /// `None`。シリアライズ時は `Black` / `White` 文字列。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub winner: Option<HistoryColor>,
}

/// `Color` を JSON スキーマ用に文字列シリアライズする小 enum。core の
/// `Color` は serde 派生していないので独立させる（serde を core 全体に拡げる
/// より隔離する方が依存範囲が読みやすい）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum HistoryColor {
    Black,
    White,
}

impl From<Color> for HistoryColor {
    fn from(c: Color) -> Self {
        match c {
            Color::Black => Self::Black,
            Color::White => Self::White,
        }
    }
}

impl FloodgateHistoryEntry {
    /// 業務型から構築するヘルパ。`persist_kifu` 経路から呼ばれる。
    pub fn new(
        game_id: &GameId,
        game_name: &GameName,
        black: &PlayerName,
        white: &PlayerName,
        start_time: DateTime<Utc>,
        end_time: DateTime<Utc>,
        result_code: &str,
        winner: Option<Color>,
    ) -> Self {
        Self {
            game_id: game_id.as_str().to_owned(),
            game_name: game_name.as_str().to_owned(),
            black: black.as_str().to_owned(),
            white: white.as_str().to_owned(),
            start_time: start_time.to_rfc3339(),
            end_time: end_time.to_rfc3339(),
            result_code: result_code.to_owned(),
            winner: winner.map(HistoryColor::from),
        }
    }
}

/// Floodgate 履歴の永続化抽象。`append` で 1 件追加、`list_recent` で末尾 N 件
/// 取得（運用ダッシュボードや x1 拡張コマンドで使う想定）。
pub trait FloodgateHistoryStorage {
    /// 1 件の履歴エントリを末尾に追記する。失敗時は `StorageError` で伝播。
    fn append(
        &self,
        entry: &FloodgateHistoryEntry,
    ) -> impl std::future::Future<Output = Result<(), StorageError>>;

    /// 末尾 N 件を新しい順で取得する。`limit` は 0 で空 `Vec`、`usize::MAX` で
    /// 全件相当（実装依存上限あり）。再起動を跨いだ参照に使う。
    fn list_recent(
        &self,
        limit: usize,
    ) -> impl std::future::Future<Output = Result<Vec<FloodgateHistoryEntry>, StorageError>>;
}

/// JSONL 形式（1 entry = 1 line）でファイルに append-only 記録する `FloodgateHistoryStorage`
/// 実装。`tokio-transport` 配下でのみコンパイルされる。
///
/// `append` は内部 `AsyncMutex` で直列化し、`OpenOptions::append(true)` で
/// 開いて 1 entry を書く。POSIX 上の `O_APPEND` 書き込みは「現在のファイル末尾
/// にカーソルを移動 → write」を 1 syscall で行うため、同一プロセス内の追記は
/// 上述 Mutex で直列化、他プロセスからの追記は OS 任せ（PIPE_BUF 以下なら atomic、
/// 超える場合は interleave 可能だが本実装は単一プロセス前提）。
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
            let raw = match tokio::fs::read_to_string(&path).await {
                Ok(s) => s,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
                Err(e) => return Err(StorageError::Io(format!("read {}: {}", path.display(), e))),
            };
            // 後ろから limit 行までを集めて、新しい順（最後に書かれた順）で返す。
            //
            // malformed 行は strict に `Err` で返す（`tracing::warn!` で skip しない）。
            // 理由は履歴 JSONL が運用ダッシュボード / x1 拡張コマンド経由の参照に
            // 加えて勝敗集計（mk_rate 等の外部バッチ）の入力にも使われ得るため、
            // 不整合行を黙って無視するとプレイヤごとの win/lose が静かに乖離して
            // 後追い切り分けが極めて困難になる。append-lock + write_all で書き込み
            // 中の crash 痕跡が残った場合は、運用が手作業で末尾の半行を切る or
            // logrotate の rotate 境界で物理削除するなど明示的に対処する方針。
            let mut entries: Vec<FloodgateHistoryEntry> = Vec::new();
            for line in raw.lines().rev() {
                if line.trim().is_empty() {
                    continue;
                }
                let entry: FloodgateHistoryEntry = serde_json::from_str(line)
                    .map_err(|e| StorageError::Malformed(format!("history line: {e}")))?;
                entries.push(entry);
                if entries.len() >= limit {
                    break;
                }
            }
            Ok(entries)
        }
    }
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn entry_round_trips_through_json() {
        let e = entry("g1", Some(HistoryColor::Black));
        let s = serde_json::to_string(&e).unwrap();
        let parsed: FloodgateHistoryEntry = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed, e);
    }

    #[test]
    fn entry_omits_winner_when_none() {
        let e = entry("g1", None);
        let s = serde_json::to_string(&e).unwrap();
        // 引き分け（千日手 / 最大手数）では `winner` フィールドが出力に出ない。
        assert!(!s.contains("\"winner\""), "winner must be omitted: {s}");
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
