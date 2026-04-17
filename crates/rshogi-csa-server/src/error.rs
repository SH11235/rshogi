//! サーバー全体で使用する型付きエラー。
//!
//! 内部計算でも I/O でも `Result<T, ServerError>` で伝播させ、panic による対局停止を避ける
//! （Requirement 8.5, 12.2）。

use thiserror::Error;

/// CSA プロトコル構文・意味解析エラー。
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProtocolError {
    /// 既知のコマンドに当てはまらない 1 行。
    #[error("unknown CSA command: {0}")]
    Unknown(String),

    /// コマンドは既知だが必要なフィールドが揃っていない／不正形式。
    #[error("malformed CSA command: {0}")]
    Malformed(String),

    /// 現在のセッション状態では受け付けられないコマンド（x1 未確立で `%%` を送る等）。
    #[error("x1 mode is not enabled; {0} is rejected")]
    X1NotEnabled(&'static str),
}

/// プレイヤ状態機械の遷移違反。
#[derive(Debug, Error, PartialEq, Eq)]
pub enum StateError {
    /// 現在状態で受付不能なコマンドが来た。
    #[error("invalid command for current player state ({current:?})")]
    InvalidForState {
        /// 違反時のプレイヤ状態（`Debug` フォーマット）。
        current: String,
    },

    /// 対局 ID が不一致。
    #[error("game_id mismatch: expected={expected}, got={actual}")]
    GameIdMismatch {
        /// サーバー側の対局 ID。
        expected: String,
        /// クライアントが提示した対局 ID。
        actual: String,
    },

    /// 権限エラー（運営でないクライアントが `%%SETBUOY` 等を送った）。
    #[error("permission denied: {0}")]
    PermissionDenied(&'static str),
}

/// 抽象 I/O エラー。[`crate::port::ClientTransport`] / [`crate::port::Broadcaster`] 共通。
#[derive(Debug, Error)]
pub enum TransportError {
    /// 受信待ち中のタイムアウト。
    #[error("connection timed out")]
    Timeout,
    /// 相手からの切断（EOF）を検知。
    #[error("connection closed by peer")]
    Closed,
    /// その他の I/O 失敗。
    #[error("I/O error: {0}")]
    Io(String),
}

impl PartialEq for TransportError {
    fn eq(&self, other: &Self) -> bool {
        use TransportError::*;
        matches!((self, other), (Timeout, Timeout) | (Closed, Closed) | (Io(_), Io(_)))
    }
}

/// 永続化エラー。
#[derive(Debug, Error)]
pub enum StorageError {
    /// 対象キーが存在しない。
    #[error("not found: {0}")]
    NotFound(String),
    /// 書き込み／読み出しの I/O 失敗。
    #[error("backend I/O error: {0}")]
    Io(String),
    /// 形式違反（YAML パース失敗等）。
    #[error("malformed payload: {0}")]
    Malformed(String),
}

/// サーバー全体を通るトップレベルエラー。
#[derive(Debug, Error)]
pub enum ServerError {
    /// プロトコル構文・セマンティクス違反。
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    /// プレイヤ状態機械違反。
    #[error(transparent)]
    State(#[from] StateError),
    /// 抽象 I/O エラー。
    #[error(transparent)]
    Transport(#[from] TransportError),
    /// 永続化エラー。
    #[error(transparent)]
    Storage(#[from] StorageError),
    /// 内部不変条件違反（PanicGuard と連携し、当該対局のみ `#ABNORMAL` 終局にする）。
    #[error("internal invariant broken: {0}")]
    Internal(String),
}
