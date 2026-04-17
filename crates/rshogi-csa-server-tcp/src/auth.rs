//! 認証とパスワードハッシュ検証（Requirement 13.1, 13.2）。
//!
//! Ruby shogi-server と互換の players.yaml で提供される plain パスワードを
//! ハッシュ照合できるよう [`PasswordHasher`] を trait として分離する。
//! Phase 1 は [`PlainPasswordHasher`]（equals 比較）のみ実装。
//! 将来 bcrypt 等を接続する場合はこの trait を別 crate で実装する。
//!
//! 平文パスワードは [`rshogi_csa_server::types::Secret`] で保持し、
//! ログには一切出さない（Secret の `Debug` は常に `***`）。照合時に `expose()` で
//! 取り出した文字列は比較の直後にスコープを抜けるのでログや永続化には残らない。

use rshogi_csa_server::port::{PlayerRateRecord, RateStorage};
use rshogi_csa_server::types::{PlayerName, Secret};

/// 認証経路で発生し得るエラー。
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AuthError {
    /// プレイヤ名に対応するレコードが存在しない。
    ///
    /// Phase 1 は「未登録 = 認証失敗」として扱い、内部的には [`AuthOutcome::Incorrect`]
    /// に畳み込む設計なので、通常この variant は構築側が意図的に使うときだけ返る。
    #[error("unknown player: {0}")]
    UnknownPlayer(String),
    /// 永続化層からのロードに失敗（ストレージ I/O エラー等）。
    #[error("storage error: {0}")]
    Storage(String),
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
/// Phase 1 は平文比較のみ。bcrypt 等を導入する際はこの trait を実装して差し替える。
pub trait PasswordHasher {
    /// 入力平文 `candidate` と保存ハッシュ `stored_hash` が一致するか判定する。
    ///
    /// 定数時間比較を推奨するが、Phase 1 の平文実装では `String` の等価比較で十分。
    /// 将来 bcrypt 等を導入する場合は [`subtle::ConstantTimeEq`] などを使う。
    fn verify(&self, candidate: &Secret, stored_hash: &str) -> bool;
}

/// players.yaml 互換の平文パスワード照合。
///
/// Ruby shogi-server の players.yaml は平文パスワード保存が既定なので、
/// 移行期は平文比較で互換性を確保する。将来的には bcrypt 等への移行を別 crate で
/// 提供する想定（Phase 5 の運用品質強化に含める）。
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
        // 長さが違う時点で不一致確定。長さ一致時のみ byte ごとの XOR で定数時間比較する。
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
/// `storage` は [`RateStorage`] 実装。Phase 1 では `name == stored` のレコードが
/// なければ `Incorrect` を返す。パスワードは `PlayerRateRecord` に格納されていないため、
/// 別経路（players.yaml）から読んだハッシュを `stored_hash` として渡す。
///
/// 永続化（`load`）での I/O 失敗は [`AuthError::Storage`] にマップする。
/// 呼び出し側は異常終了経路に回し、セッションを閉じること。
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
    let record = match storage.load(name).await {
        Ok(Some(r)) => r,
        Ok(None) => return Ok(AuthOutcome::Incorrect),
        Err(e) => return Err(AuthError::Storage(e.to_string())),
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
