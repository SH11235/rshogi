//! Floodgate 履歴永続化の抽象ポート。
//!
//! `append` で 1 件追加、`list_recent` で末尾 N 件取得する責務を持つ。具体実装
//! はランタイム別に分かれる: TCP は JSONL ファイル append、Workers (Cloudflare DO)
//! は R2 day-shard + DO storage ring buffer のハイブリッド等。

use crate::error::StorageError;

use super::types::FloodgateHistoryEntry;

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
