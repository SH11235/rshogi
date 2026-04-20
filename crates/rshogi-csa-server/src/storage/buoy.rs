//! `BuoyStorage` のローカルファイル実装。
//!
//! ブイ (途中局面テンプレート) を `<topdir>/buoys/<sanitized_game_name>.json`
//! に JSON として保存する。内容は以下の単純な schema:
//!
//! ```text
//! {
//!   "moves": ["+7776FU", "-3334FU", ...],
//!   "remaining": 3
//! }
//! ```
//!
//! - `set` は原子的に上書きする (`.tmp` に書いてから `rename`)。
//! - `delete` はファイル削除。ファイル未存在は no-op (`Ok(())`)。
//! - `count` は JSON を読んで `remaining` を返す。ファイル未存在なら `Ok(None)`。
//!
//! `tokio-transport` フィーチャ下でのみコンパイルされる (`tokio::fs` が必要)。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::io::AsyncWriteExt;

use crate::error::StorageError;
use crate::port::BuoyStorage;
use crate::types::{CsaMoveToken, GameName};

/// ローカルディレクトリへブイを書き出す `BuoyStorage`。
///
/// 同一プロセス内での並列 `set` / `delete` はファイル単位の `rename` に
/// 依存するため、`set` 中に `set` が来ても最後の書き込みが勝つ (last-writer
/// wins)。バッチ運用で複数プロセスが同一 `topdir/buoys/<name>.json` を
/// 触る想定は現時点では無い。
#[derive(Debug, Clone)]
pub struct FileBuoyStorage {
    topdir: PathBuf,
}

/// ディスク上の JSON schema。serde が直接 roundtrip できる最小形。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct BuoyFile {
    /// 初期局面に差し込む CSA 手列 (生文字列。検証は呼び出し側の責務)。
    moves: Vec<String>,
    /// 残り対局数。0 になると実質 "消費済み"。
    remaining: u32,
}

impl FileBuoyStorage {
    /// `<topdir>/buoys/` 配下に JSON を書き出すストレージを作る。
    pub fn new<P: Into<PathBuf>>(topdir: P) -> Self {
        Self {
            topdir: topdir.into(),
        }
    }

    /// 1 ブイ分のファイルパス `<topdir>/buoys/<sanitized>.json`。
    ///
    /// `game_name` の `/` / `\0` / パス区切りはファイル名で有害なので、
    /// 明示的に `_` へ置換する。CSA の game_name は通常 `floodgate-600-10`
    /// のようなハイフン区切り ASCII のため、現実の衝突はほぼ無いが、
    /// 悪意ある入力を防ぐため常に sanitize する。
    fn path_for(&self, game_name: &GameName) -> PathBuf {
        let sanitized: String = game_name
            .as_str()
            .chars()
            .map(|c| {
                if c == '/' || c == '\\' || c == '\0' || c == '.' {
                    '_'
                } else {
                    c
                }
            })
            .collect();
        self.topdir.join("buoys").join(format!("{sanitized}.json"))
    }
}

impl BuoyStorage for FileBuoyStorage {
    async fn set(
        &self,
        game_name: &GameName,
        moves: Vec<CsaMoveToken>,
        remaining: u32,
    ) -> Result<(), StorageError> {
        let path = self.path_for(game_name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await.map_err(to_storage_err)?;
        }
        let payload = BuoyFile {
            moves: moves.into_iter().map(|t| t.as_str().to_owned()).collect(),
            remaining,
        };
        let bytes = serde_json::to_vec(&payload)
            .map_err(|e| StorageError::Io(format!("serialize buoy: {e}")))?;

        // .tmp → rename で原子的書き換え。中断時に半端な JSON が残らないようにする。
        let tmp = path.with_extension("json.tmp");
        let mut f = fs::File::create(&tmp).await.map_err(to_storage_err)?;
        f.write_all(&bytes).await.map_err(to_storage_err)?;
        f.flush().await.map_err(to_storage_err)?;
        drop(f);
        fs::rename(&tmp, &path).await.map_err(to_storage_err)?;
        Ok(())
    }

    async fn delete(&self, game_name: &GameName) -> Result<(), StorageError> {
        let path = self.path_for(game_name);
        match fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            // 未登録なら no-op。重複削除や idempotent な運用を許容。
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(StorageError::Io(format!("delete buoy: {e}"))),
        }
    }

    async fn count(&self, game_name: &GameName) -> Result<Option<u32>, StorageError> {
        let path = self.path_for(game_name);
        let bytes = match fs::read(&path).await {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(StorageError::Io(format!("read buoy: {e}"))),
        };
        let parsed: BuoyFile = serde_json::from_slice(&bytes)
            .map_err(|e| StorageError::Io(format!("parse buoy: {e}")))?;
        Ok(Some(parsed.remaining))
    }
}

fn to_storage_err(e: std::io::Error) -> StorageError {
    StorageError::Io(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_topdir(tag: &str) -> PathBuf {
        let pid = std::process::id();
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("rshogi_buoy_{tag}_{pid}_{ts}"))
    }

    #[tokio::test(flavor = "current_thread")]
    async fn set_then_count_returns_remaining() {
        let topdir = unique_topdir("set_count");
        let storage = FileBuoyStorage::new(topdir.clone());
        let gn = GameName::new("test-buoy");
        storage.set(&gn, vec![CsaMoveToken::new("+7776FU")], 3).await.unwrap();
        let c = storage.count(&gn).await.unwrap();
        assert_eq!(c, Some(3));
        let _ = fs::remove_dir_all(&topdir).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn set_overwrites_previous_entry() {
        let topdir = unique_topdir("set_overwrite");
        let storage = FileBuoyStorage::new(topdir.clone());
        let gn = GameName::new("test-buoy");
        storage.set(&gn, vec![], 5).await.unwrap();
        storage.set(&gn, vec![], 2).await.unwrap();
        assert_eq!(storage.count(&gn).await.unwrap(), Some(2));
        let _ = fs::remove_dir_all(&topdir).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn count_returns_none_for_unknown_game_name() {
        let topdir = unique_topdir("count_unknown");
        let storage = FileBuoyStorage::new(topdir.clone());
        assert_eq!(storage.count(&GameName::new("never-set")).await.unwrap(), None);
        let _ = fs::remove_dir_all(&topdir).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn delete_removes_entry() {
        let topdir = unique_topdir("delete");
        let storage = FileBuoyStorage::new(topdir.clone());
        let gn = GameName::new("target");
        storage.set(&gn, vec![], 1).await.unwrap();
        assert_eq!(storage.count(&gn).await.unwrap(), Some(1));
        storage.delete(&gn).await.unwrap();
        assert_eq!(storage.count(&gn).await.unwrap(), None);
        let _ = fs::remove_dir_all(&topdir).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn delete_is_idempotent_on_missing_entry() {
        // 未登録 game_name の delete は no-op (Ok)。重複 %%DELETEBUOY の
        // 運用も許容する。
        let topdir = unique_topdir("delete_missing");
        let storage = FileBuoyStorage::new(topdir.clone());
        storage.delete(&GameName::new("never-set")).await.unwrap();
        let _ = fs::remove_dir_all(&topdir).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sanitize_path_for_slash_in_game_name() {
        // `/` / `\` / `.` を含む game_name は `_` に置換されてファイル名に落ちる。
        // これにより topdir 外への escape (`../../etc/passwd` 等) を防ぐ。
        let topdir = unique_topdir("sanitize");
        let storage = FileBuoyStorage::new(topdir.clone());
        let gn = GameName::new("../foo/bar");
        storage.set(&gn, vec![], 1).await.unwrap();
        // `..` の 2 文字 + `/` の 3 文字分が各々 `_` に置換されて `___foo_bar.json`。
        let expected = topdir.join("buoys").join("___foo_bar.json");
        assert!(expected.exists(), "sanitized file not found at {expected:?}");
        // 実際の set/count の round-trip も同じ sanitize 規則で一致する。
        assert_eq!(storage.count(&gn).await.unwrap(), Some(1));
        let _ = fs::remove_dir_all(&topdir).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn moves_roundtrip_through_json() {
        // set で渡した moves が count/内部読みで保たれるか確認する補助テスト。
        // count は直接 moves を返さないが、ファイル内部が正しく書けているかは
        // 次の set の overwrite が壊れないことで間接的に検証される。ここでは
        // ファイルを直接読んで JSON を parse し、moves 配列が戻ることを確認する。
        let topdir = unique_topdir("moves_roundtrip");
        let storage = FileBuoyStorage::new(topdir.clone());
        let gn = GameName::new("rt");
        let original = vec![
            CsaMoveToken::new("+7776FU"),
            CsaMoveToken::new("-3334FU"),
            CsaMoveToken::new("+2726FU"),
        ];
        storage.set(&gn, original.clone(), 7).await.unwrap();
        let bytes = fs::read(&storage.path_for(&gn)).await.unwrap();
        let parsed: BuoyFile = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed.remaining, 7);
        assert_eq!(
            parsed.moves,
            vec![
                "+7776FU".to_owned(),
                "-3334FU".to_owned(),
                "+2726FU".to_owned()
            ]
        );
        let _ = fs::remove_dir_all(&topdir).await;
    }
}
