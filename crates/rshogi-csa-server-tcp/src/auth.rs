//! 認証とパスワードハッシュ検証。
//!
//! Ruby shogi-server と互換の players.yaml で提供される plain パスワードを
//! ハッシュ照合できるよう [`PasswordHasher`] を trait として分離する。
//! 現状は [`PlainPasswordHasher`]（equals 比較）のみ実装。
//! bcrypt 等を接続する場合はこの trait を別 crate で実装する。
//!
//! 平文パスワードは [`rshogi_csa_server::types::Secret`] で保持し、
//! ログには一切出さない（Secret の `Debug` は常に `***`）。照合時に `expose()` で
//! 取り出した文字列は比較の直後にスコープを抜けるのでログや永続化には残らない。

use rshogi_csa_server::error::{ServerError, StorageError};
use rshogi_csa_server::port::{PlayerRateRecord, RateStorage};
use rshogi_csa_server::types::{PlayerName, Secret};

/// 認証経路で発生し得るエラー。
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// プレイヤ名に対応するレコードが存在しない。
    ///
    /// 「未登録 = 認証失敗」として扱い、内部的には [`AuthOutcome::Incorrect`]
    /// に畳み込む設計なので、通常この variant は構築側が意図的に使うときだけ返る。
    #[error("unknown player: {0}")]
    UnknownPlayer(String),
    /// 永続化層からのロードに失敗（ストレージ I/O エラー等）。元の [`StorageError`] を
    /// そのまま保持し、[`From<AuthError> for ServerError`] で `ServerError::Storage`
    /// に無損失マップする。
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),
}

impl From<AuthError> for ServerError {
    fn from(e: AuthError) -> Self {
        match e {
            AuthError::Storage(s) => ServerError::Storage(s),
            AuthError::UnknownPlayer(name) => {
                ServerError::Internal(format!("auth: unknown player: {name}"))
            }
        }
    }
}

/// 認証結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthOutcome {
    /// 認証成功。`record` は現在のレコード（ログイン時刻更新等に利用）。
    Ok {
        /// ロードされた既存レコード。
        record: PlayerRateRecord,
    },
    /// 認証失敗（プレイヤ未登録 / パスワード不一致）。
    Incorrect,
}

/// パスワード照合ロジックの抽象。
///
/// Ruby shogi-server の players.yaml と互換のため、既定実装は平文比較
/// [`PlainPasswordHasher`]。将来ハッシュ方式（bcrypt 等）に移行する場合は
/// この trait を実装して差し替え、その際に入力長で分岐しない定数時間比較
/// （`subtle::ConstantTimeEq` 等）を採用する。
pub trait PasswordHasher {
    /// 入力平文 `candidate` と保存ハッシュ `stored_hash` が一致するか判定する。
    fn verify(&self, candidate: &Secret, stored_hash: &str) -> bool;
}

/// players.yaml 互換の平文パスワード照合。
///
/// Ruby shogi-server の players.yaml は平文パスワード保存が既定なので、移行期は
/// 平文比較で互換性を確保する。
#[derive(Debug, Default, Clone, Copy)]
pub struct PlainPasswordHasher;

impl PlainPasswordHasher {
    /// 新しい平文照合ハッシャを返す。
    pub fn new() -> Self {
        Self
    }
}

impl PasswordHasher for PlainPasswordHasher {
    fn verify(&self, candidate: &Secret, stored_hash: &str) -> bool {
        // 注意: 平文比較かつ長さ不一致で即 return するため、処理時間からパスワード長を
        // 推定する攻撃に対して定数時間ではない。本実装は Ruby shogi-server の
        // players.yaml 平文互換のための暫定経路であり、ハッシュ方式への移行時は
        // 長さ分岐ごと `subtle::ConstantTimeEq` 等の完全定数時間比較に置き換える。
        let a = candidate.expose().as_bytes();
        let b = stored_hash.as_bytes();
        if a.len() != b.len() {
            return false;
        }
        let mut diff: u8 = 0;
        for (x, y) in a.iter().zip(b.iter()) {
            diff |= x ^ y;
        }
        diff == 0
    }
}

/// プレイヤ名・パスワードを照合して `AuthOutcome` を返す。
///
/// `storage` は [`RateStorage`] 実装で、`name` に対応するレコードの有無だけを問う
/// （PlayerRateRecord にパスワードは含まれないため、`stored_hash` は別経路の
/// players.yaml 等から読んだ値を呼び出し側が渡す）。以下の前提に依存する:
///
/// - **`storage.load(name)` が `None` を返した場合は認証失敗**として
///   [`AuthOutcome::Incorrect`] を返す。新規プレイヤーの登録（未知 handle を
///   `rate_storage` に差し込むような経路）は本関数の責務外で、呼び出し側が
///   起動時または別の管理経路で `RateStorage` を予めその handle で満たしておく必要がある。
///   現在の TCP バイナリは `main.rs` で players.toml の全エントリを初期投入する前提。
/// - 永続化（`load`）での I/O 失敗は [`AuthError::Storage`] として propagate する。
///   呼び出し側は `?` で [`ServerError`] に変換し、セッションを閉じること。
pub async fn authenticate<S>(
    storage: &S,
    hasher: &dyn PasswordHasher,
    name: &PlayerName,
    password: &Secret,
    stored_hash: &str,
) -> Result<AuthOutcome, AuthError>
where
    S: RateStorage,
{
    let record = match storage.load(name).await? {
        Some(r) => r,
        None => return Ok(AuthOutcome::Incorrect),
    };
    if hasher.verify(password, stored_hash) {
        Ok(AuthOutcome::Ok { record })
    } else {
        Ok(AuthOutcome::Incorrect)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rshogi_csa_server::error::StorageError;
    use rshogi_csa_server::types::GameId;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// ロード結果を HashMap で返すだけの RateStorage モック。
    struct MockRateStorage {
        data: Mutex<HashMap<String, PlayerRateRecord>>,
    }

    impl MockRateStorage {
        fn new(records: Vec<PlayerRateRecord>) -> Self {
            let mut map = HashMap::new();
            for r in records {
                map.insert(r.name.as_str().to_owned(), r);
            }
            Self {
                data: Mutex::new(map),
            }
        }
    }

    impl RateStorage for MockRateStorage {
        async fn load(&self, name: &PlayerName) -> Result<Option<PlayerRateRecord>, StorageError> {
            Ok(self.data.lock().unwrap().get(name.as_str()).cloned())
        }

        async fn save(&self, record: &PlayerRateRecord) -> Result<(), StorageError> {
            self.data
                .lock()
                .unwrap()
                .insert(record.name.as_str().to_owned(), record.clone());
            Ok(())
        }

        async fn list_all(&self) -> Result<Vec<PlayerRateRecord>, StorageError> {
            Ok(self.data.lock().unwrap().values().cloned().collect())
        }
    }

    fn rec(name: &str) -> PlayerRateRecord {
        PlayerRateRecord {
            name: PlayerName::new(name),
            rate: 1500,
            wins: 0,
            losses: 0,
            last_game_id: Some(GameId::new("prev")),
            last_modified: "2026-04-17T00:00:00Z".to_owned(),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn plain_hasher_equal_strings_match() {
        let h = PlainPasswordHasher::new();
        assert!(h.verify(&Secret::new("hunter2"), "hunter2"));
        assert!(!h.verify(&Secret::new("hunter2"), "hunter3"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn plain_hasher_different_lengths_do_not_match() {
        let h = PlainPasswordHasher::new();
        assert!(!h.verify(&Secret::new("abc"), "abcd"));
        assert!(!h.verify(&Secret::new("abcd"), "abc"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn authenticate_ok_when_hash_matches_and_record_exists() {
        let store = MockRateStorage::new(vec![rec("alice")]);
        let out = authenticate(
            &store,
            &PlainPasswordHasher::new(),
            &PlayerName::new("alice"),
            &Secret::new("pw"),
            "pw",
        )
        .await
        .unwrap();
        match out {
            AuthOutcome::Ok { record } => assert_eq!(record.name.as_str(), "alice"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn authenticate_incorrect_when_password_mismatches() {
        let store = MockRateStorage::new(vec![rec("alice")]);
        let out = authenticate(
            &store,
            &PlainPasswordHasher::new(),
            &PlayerName::new("alice"),
            &Secret::new("bad"),
            "pw",
        )
        .await
        .unwrap();
        assert_eq!(out, AuthOutcome::Incorrect);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn authenticate_incorrect_when_record_missing() {
        let store = MockRateStorage::new(Vec::new());
        let out = authenticate(
            &store,
            &PlainPasswordHasher::new(),
            &PlayerName::new("ghost"),
            &Secret::new("pw"),
            "pw",
        )
        .await
        .unwrap();
        assert_eq!(out, AuthOutcome::Incorrect);
    }
}
