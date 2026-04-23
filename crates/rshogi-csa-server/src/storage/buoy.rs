//! `BuoyStorage` のローカルファイル実装。
//!
//! ブイ (途中局面テンプレート) を `<topdir>/buoys/<encoded_game_name>.json`
//! に JSON として保存する。内容は以下の単純な schema:
//!
//! ```text
//! {
//!   "moves": ["+7776FU", "-3334FU", ...],
//!   "initial_sfen": "lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 2",
//!   "remaining": 3
//! }
//! ```
//!
//! - `set` は原子的に上書きする (`.tmp` に書いてから `rename`)。tmp ファイル名には
//!   PID + atomic counter を含めて、複数 `set` が同じ buoy に並列に書いても
//!   互いの tmp を踏まない (Codex review PR #470 P3)。
//! - `delete` はファイル削除。ファイル未存在は no-op (`Ok(())`)。
//! - `count` は JSON を読んで `remaining` を返す。ファイル未存在なら `Ok(None)`。
//! - `initial_sfen` は任意。通常の `%%SETBUOY` では CSA 手列から導出した開始局面を
//!   キャッシュし、`%%FORK` では派生局面の SFEN を直接保存する。
//!
//! ## ファイル名エンコーディング
//!
//! `game_name` が `.` `/` `\` 等を含んでも異なるブイが衝突しないよう、
//! **percent-encoding 風の可逆エンコーディング** (`encode_game_name`) を使う。
//! 安全文字 (ASCII alphanumeric と `-` `_`) 以外は `%XX` 形式でエスケープする。
//! これにより `a/b` / `a.b` / `a%b` が異なるファイル名に落ちる (Codex review
//! PR #470 P2)。旧実装 (`/` `\` `.` `\0` を全て `_` に置換) では衝突リスクが
//! あったため修正。
//!
//! `tokio-transport` フィーチャ下でのみコンパイルされる (`tokio::fs` が必要)。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

use crate::error::StorageError;
use crate::port::BuoyStorage;
use crate::types::{CsaMoveToken, GameName};

/// tmp ファイル名生成用の atomic カウンタ。複数 `set` が同時実行されても
/// 各呼び出しで異なるサフィックスが得られるため、tmp ファイルが混ざらない。
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// ローカルディレクトリへブイを書き出す `BuoyStorage`。
///
/// 同一プロセス内での並列 `set` / `delete` はファイル単位の `rename` に
/// 依存するため、`set` 中に `set` が来ても最後の書き込みが勝つ (last-writer
/// wins)。バッチ運用で複数プロセスが同一 `topdir/buoys/<name>.json` を
/// 触る想定は現時点では無い。
#[derive(Debug, Clone)]
pub struct FileBuoyStorage {
    topdir: PathBuf,
    reserve_lock: Arc<Mutex<()>>,
}

/// ディスク上の JSON schema。serde が直接 roundtrip できる最小形。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct BuoyFile {
    /// 初期局面に差し込む CSA 手列 (生文字列。検証は呼び出し側の責務)。
    moves: Vec<String>,
    /// 派生対局の開始局面 SFEN。旧 schema からの後方互換のため省略可。
    #[serde(default)]
    initial_sfen: Option<String>,
    /// 残り対局数。0 になると実質 "消費済み"。
    remaining: u32,
}

/// ストレージから読み出したブイ 1 件分。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredBuoy {
    /// 初期局面に差し込む CSA 手列。
    pub moves: Vec<CsaMoveToken>,
    /// 派生対局の開始局面 SFEN。`Some` ならこちらを優先して使う。
    pub initial_sfen: Option<String>,
    /// 残り対局数。
    pub remaining: u32,
}

impl FileBuoyStorage {
    /// `<topdir>/buoys/` 配下に JSON を書き出すストレージを作る。
    pub fn new<P: Into<PathBuf>>(topdir: P) -> Self {
        Self {
            topdir: topdir.into(),
            reserve_lock: Arc::new(Mutex::new(())),
        }
    }

    /// 1 ブイ分のファイルパス `<topdir>/buoys/<encoded>.json`。
    ///
    /// `game_name` は `encode_game_name` で percent-encoding 風の可逆エンコーディング
    /// を施す。ASCII alphanumeric と `-` / `_` はそのまま、それ以外は `%XX` 形式
    /// (大文字 hex) でエスケープ。これにより `a/b` と `a.b` と `a_b` が異なる
    /// ファイル名に落ち、意図せぬ上書きを防ぐ (Codex review PR #470 P2)。
    fn path_for(&self, game_name: &GameName) -> PathBuf {
        let encoded = encode_game_name(game_name.as_str());
        self.topdir.join("buoys").join(format!("{encoded}.json"))
    }

    /// 同一 buoy への並列 `set` が tmp ファイル名で衝突しないよう、毎回
    /// PID + atomic counter で一意な suffix を付けた tmp パスを作る
    /// (Codex review PR #470 P3)。rename 済みの tmp は残らず、rename 失敗時も
    /// ファイルシステム側 cleanup に任せる (tmp ファイルは少量・短命なので
    /// 削除漏れの運用影響は限定的)。
    fn tmp_path_for(&self, final_path: &std::path::Path) -> PathBuf {
        let pid = std::process::id();
        let seq = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let stem = final_path.file_stem().and_then(|s| s.to_str()).unwrap_or("buoy");
        let parent = final_path.parent().unwrap_or(std::path::Path::new("."));
        parent.join(format!("{stem}.{pid}.{seq}.tmp"))
    }

    /// ブイを拡張メタデータ付きで保存する。
    pub async fn store(
        &self,
        game_name: &GameName,
        moves: Vec<CsaMoveToken>,
        remaining: u32,
        initial_sfen: Option<String>,
    ) -> Result<(), StorageError> {
        let path = self.path_for(game_name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await.map_err(to_storage_err)?;
        }
        let payload = BuoyFile {
            moves: moves.into_iter().map(|t| t.as_str().to_owned()).collect(),
            initial_sfen,
            remaining,
        };
        let bytes = serde_json::to_vec(&payload)
            .map_err(|e| StorageError::Io(format!("serialize buoy: {e}")))?;

        // .tmp → rename で原子的書き換え。中断時に半端な JSON が残らないようにする。
        // tmp 名は `<stem>.<pid>.<counter>.tmp` で一意化する。並列 `set` が同じ
        // buoy に走っても互いの tmp を踏まず、rename は last-writer-wins で
        // 確定する (Codex review PR #470 P3)。
        let tmp = self.tmp_path_for(&path);
        let mut f = fs::File::create(&tmp).await.map_err(to_storage_err)?;
        f.write_all(&bytes).await.map_err(to_storage_err)?;
        f.flush().await.map_err(to_storage_err)?;
        drop(f);
        fs::rename(&tmp, &path).await.map_err(to_storage_err)?;
        Ok(())
    }

    /// ブイを丸ごと読み出す。未登録なら `Ok(None)`。
    pub async fn load(&self, game_name: &GameName) -> Result<Option<StoredBuoy>, StorageError> {
        let path = self.path_for(game_name);
        let bytes = match fs::read(&path).await {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(StorageError::Io(format!("read buoy: {e}"))),
        };
        let parsed: BuoyFile = serde_json::from_slice(&bytes)
            .map_err(|e| StorageError::Io(format!("parse buoy: {e}")))?;
        Ok(Some(StoredBuoy {
            moves: parsed.moves.into_iter().map(CsaMoveToken::new).collect(),
            initial_sfen: parsed.initial_sfen,
            remaining: parsed.remaining,
        }))
    }

    /// 残り対局数を 1 減らす。未登録なら `Ok(None)`。
    pub async fn decrement_remaining(
        &self,
        game_name: &GameName,
    ) -> Result<Option<u32>, StorageError> {
        let Some(mut buoy) = self.load(game_name).await? else {
            return Ok(None);
        };
        if buoy.remaining > 0 {
            buoy.remaining -= 1;
        }
        let new_remaining = buoy.remaining;
        self.store(game_name, buoy.moves, new_remaining, buoy.initial_sfen).await?;
        Ok(Some(new_remaining))
    }

    /// マッチ成立時に buoy を 1 回分予約する。
    ///
    /// 1 プロセス内では `reserve_lock` で `load + decrement + persist` を直列化し、
    /// 「残数 1 を 2 対局が同時に拾う」race を防ぐ。
    pub async fn reserve_for_match(
        &self,
        game_name: &GameName,
    ) -> Result<Option<StoredBuoy>, StorageError> {
        let _guard = self.reserve_lock.lock().await;
        let Some(mut buoy) = self.load(game_name).await? else {
            return Ok(None);
        };
        if buoy.remaining == 0 {
            return Ok(Some(buoy));
        }
        let reserved = buoy.clone();
        buoy.remaining -= 1;
        self.store(game_name, buoy.moves.clone(), buoy.remaining, buoy.initial_sfen.clone())
            .await?;
        Ok(Some(reserved))
    }
}

/// `game_name` をファイル名に安全なエンコーディングに変換する。
///
/// - 安全文字 (ASCII alphanumeric、`-`、`_`) はそのまま出力。
/// - それ以外 (UTF-8 multi-byte を含む) は byte 単位で `%XX` 形式 (大文字 hex)
///   にエスケープ。`%` 自体も `%25` に置換して可逆性を保つ。
///
/// 出力は ASCII のみ、ファイル名として有効で、かつ 1 対 1 の単射 (可逆) である。
fn encode_game_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for b in name.bytes() {
        let is_safe = b.is_ascii_alphanumeric() || b == b'-' || b == b'_';
        if is_safe {
            out.push(b as char);
        } else {
            out.push('%');
            const HEX: &[u8; 16] = b"0123456789ABCDEF";
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0f) as usize] as char);
        }
    }
    out
}

impl BuoyStorage for FileBuoyStorage {
    async fn set(
        &self,
        game_name: &GameName,
        moves: Vec<CsaMoveToken>,
        remaining: u32,
    ) -> Result<(), StorageError> {
        self.store(game_name, moves, remaining, None).await
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
        Ok(self.load(game_name).await?.map(|b| b.remaining))
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
    async fn encode_path_for_special_characters_escapes_to_percent_hex() {
        // `..` / `/` を含む game_name は percent-encoding で可逆にエスケープされる。
        // これにより topdir 外への escape (`../../etc/passwd` 等) を防ぎつつ、
        // 異なる game_name が衝突しない (Codex review PR #470 P2)。
        let topdir = unique_topdir("encode_escape");
        let storage = FileBuoyStorage::new(topdir.clone());
        let gn = GameName::new("../foo/bar");
        storage.set(&gn, vec![], 1).await.unwrap();
        // `.` → `%2E`, `/` → `%2F` で `%2E%2E%2Ffoo%2Fbar.json` になる。
        let expected = topdir.join("buoys").join("%2E%2E%2Ffoo%2Fbar.json");
        assert!(expected.exists(), "encoded file not found at {expected:?}");
        assert_eq!(storage.count(&gn).await.unwrap(), Some(1));
        let _ = fs::remove_dir_all(&topdir).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn encoded_game_names_do_not_collide_across_special_chars() {
        // `a.b` / `a/b` / `a_b` / `a-b` が異なるファイル名に落ちることを確認する
        // (Codex review PR #470 P2 の回帰防止: 旧実装は全て `a_b.json` に潰れた)。
        let topdir = unique_topdir("encode_distinct");
        let storage = FileBuoyStorage::new(topdir.clone());
        // 各 game_name を別の remaining で登録し、各 count が独立して観測できることを確認する。
        storage.set(&GameName::new("a.b"), vec![], 10).await.unwrap();
        storage.set(&GameName::new("a/b"), vec![], 20).await.unwrap();
        storage.set(&GameName::new("a_b"), vec![], 30).await.unwrap();
        storage.set(&GameName::new("a-b"), vec![], 40).await.unwrap();
        assert_eq!(storage.count(&GameName::new("a.b")).await.unwrap(), Some(10));
        assert_eq!(storage.count(&GameName::new("a/b")).await.unwrap(), Some(20));
        assert_eq!(storage.count(&GameName::new("a_b")).await.unwrap(), Some(30));
        assert_eq!(storage.count(&GameName::new("a-b")).await.unwrap(), Some(40));
        let _ = fs::remove_dir_all(&topdir).await;
    }

    #[test]
    fn encode_game_name_preserves_safe_ascii_and_escapes_others() {
        // encode_game_name の単純 unit テスト: 安全文字はそのまま、他は %XX。
        assert_eq!(encode_game_name("abc-123_XYZ"), "abc-123_XYZ");
        assert_eq!(encode_game_name("a.b"), "a%2Eb");
        assert_eq!(encode_game_name("a/b"), "a%2Fb");
        assert_eq!(encode_game_name("a%b"), "a%25b");
        // UTF-8 multi-byte (日本語) は各 byte が % エスケープされる。
        assert_eq!(encode_game_name("あ"), "%E3%81%82");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn set_uses_unique_tmp_path_per_invocation() {
        // Codex review PR #470 P3 の回帰防止: tmp ファイル名が PID + counter で
        // 一意化されるため、並列 `set` が共通の `.tmp` を踏まない。同一 buoy に
        // 対して 2 回続けて `set` を走らせ、どちらも成功することで last-writer-wins
        // が壊れないことを確認する (旧実装は tmp を共有していて 2 回目が部分的に
        // 壊れるリスクがあった)。
        let topdir = unique_topdir("tmp_unique");
        let storage = FileBuoyStorage::new(topdir.clone());
        let gn = GameName::new("sequential");
        storage.set(&gn, vec![], 1).await.unwrap();
        storage.set(&gn, vec![], 2).await.unwrap();
        assert_eq!(storage.count(&gn).await.unwrap(), Some(2));
        // buoys ディレクトリに残るのは本ファイル 1 つだけで、tmp 残骸が無いはず
        // (rename 成功時は tmp は消える)。
        let mut entries = fs::read_dir(topdir.join("buoys")).await.unwrap();
        let mut count = 0;
        while let Some(e) = entries.next_entry().await.unwrap() {
            let name = e.file_name().to_string_lossy().to_string();
            assert!(!name.ends_with(".tmp"), "leftover tmp: {name}");
            count += 1;
        }
        assert_eq!(count, 1);
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
        assert_eq!(parsed.initial_sfen, None);
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

    #[tokio::test(flavor = "current_thread")]
    async fn load_returns_initial_sfen_when_present() {
        let topdir = unique_topdir("load_initial_sfen");
        let storage = FileBuoyStorage::new(topdir.clone());
        let gn = GameName::new("forked");
        storage
            .store(
                &gn,
                vec![CsaMoveToken::new("+7776FU")],
                1,
                Some(
                    "lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 2".to_owned(),
                ),
            )
            .await
            .unwrap();
        let buoy = storage.load(&gn).await.unwrap().unwrap();
        assert_eq!(buoy.moves, vec![CsaMoveToken::new("+7776FU")]);
        assert_eq!(
            buoy.initial_sfen.as_deref(),
            Some("lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 2")
        );
        assert_eq!(buoy.remaining, 1);
        let _ = fs::remove_dir_all(&topdir).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn decrement_remaining_updates_stored_value() {
        let topdir = unique_topdir("decrement");
        let storage = FileBuoyStorage::new(topdir.clone());
        let gn = GameName::new("forked");
        storage.set(&gn, vec![CsaMoveToken::new("+7776FU")], 2).await.unwrap();
        assert_eq!(storage.decrement_remaining(&gn).await.unwrap(), Some(1));
        assert_eq!(storage.count(&gn).await.unwrap(), Some(1));
        assert_eq!(storage.decrement_remaining(&gn).await.unwrap(), Some(0));
        assert_eq!(storage.count(&gn).await.unwrap(), Some(0));
        let _ = fs::remove_dir_all(&topdir).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn reserve_for_match_returns_original_entry_and_persists_decremented_count() {
        let topdir = unique_topdir("reserve_for_match");
        let storage = FileBuoyStorage::new(topdir.clone());
        let gn = GameName::new("forked");
        storage.set(&gn, vec![CsaMoveToken::new("+7776FU")], 1).await.unwrap();
        let reserved = storage.reserve_for_match(&gn).await.unwrap().unwrap();
        assert_eq!(reserved.remaining, 1);
        assert_eq!(reserved.moves, vec![CsaMoveToken::new("+7776FU")]);
        assert_eq!(storage.count(&gn).await.unwrap(), Some(0));
        let _ = fs::remove_dir_all(&topdir).await;
    }
}
