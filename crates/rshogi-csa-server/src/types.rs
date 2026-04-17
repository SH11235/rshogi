//! コアドメイン型（newtype ラッパ）。
//!
//! CSA プロトコルで飛び交う文字列をそのまま `String` として扱うと用途を取り違えやすいため、
//! 意味のある単位に newtype を導入する。全て `AsRef<str>` を実装し `Debug` はそのまま
//! 文字列を出すが、[`Secret`] だけはログ漏洩を避けるため `"***"` 固定で表示する。

use std::fmt;

macro_rules! newtype_str {
    ($(#[$meta:meta])* $vis:vis $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, PartialEq, Eq, Hash)]
        $vis struct $name(String);

        impl $name {
            /// 文字列を受け取り newtype に変換する。
            pub fn new<S: Into<String>>(s: S) -> Self {
                Self(s.into())
            }

            /// 内部表現（`&str`）への参照。
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// 所有 `String` に変換して取り出す。
            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_tuple(stringify!($name)).field(&self.0).finish()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_owned())
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }
    };
}

newtype_str! {
    /// 対局 1 つを識別するサーバー発行 ID（20140101123000 形式等）。
    pub GameId
}

newtype_str! {
    /// CSA LOGIN で使われるプレイヤ名。
    pub PlayerName
}

newtype_str! {
    /// Floodgate の `game_name`（例: `floodgate-600-10`）。
    pub GameName
}

newtype_str! {
    /// 1 行の CSA プロトコル生テキスト（末尾改行は除去済み）。
    pub CsaLine
}

newtype_str! {
    /// CSA 手トークン（例: `+7776FU`、`-3334FU`）。
    pub CsaMoveToken
}

newtype_str! {
    /// デプロイ切断時の再接続を識別するトークン（Requirement 17）。
    pub ReconnectToken
}

newtype_str! {
    /// 運営権限を持つクライアント識別子（`%%SETBUOY` 等で権限判定に用いる）。
    pub AdminId
}

newtype_str! {
    /// 永続化先の抽象的な識別子（ファイルパス／オブジェクトキー／KV キーの共通 key）。
    pub StorageKey
}

newtype_str! {
    /// 配信対象ルームの識別子（通常は [`GameId`] と 1:1）。
    pub RoomId
}

newtype_str! {
    /// レート制限などで使用する IP の文字列表現。
    ///
    /// TCP 版は `SocketAddr::ip().to_string()`、Workers 版は `CF-Connecting-IP` ヘッダの値を渡す。
    pub IpKey
}

/// 機密文字列（パスワード・トークン等）。
///
/// `Debug` 実装は常に `"***"` を返し、誤ってログに平文を残さないようにする。
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// 文字列を Secret として取り込む。
    pub fn new<S: Into<String>>(s: S) -> Self {
        Self(s.into())
    }

    /// 秘匿状態を明示的に解除して生の文字列スライスを取り出す。
    ///
    /// ハッシュ比較やサーバー内部の検証以外では呼ばない。
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

impl From<&str> for Secret {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl From<String> for Secret {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// 手番色。rshogi-core の [`rshogi_core::types::Color`] と意味は同じ。
///
/// コア crate とフロントエンドを疎結合に保つため、サーバー側では独自に再定義する。
/// rshogi-core 側の値との相互変換は `From` / `Into` で提供する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Color {
    /// 先手。
    Black,
    /// 後手。
    White,
}

impl Color {
    /// 相手番を返す。
    pub fn opposite(self) -> Self {
        match self {
            Color::Black => Color::White,
            Color::White => Color::Black,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_debug_is_masked() {
        let s = Secret::new("hunter2");
        let dbg = format!("{:?}", s);
        assert_eq!(dbg, "Secret(***)");
        // expose 経由では元の値が取れる
        assert_eq!(s.expose(), "hunter2");
    }

    #[test]
    fn newtype_display_preserves_content() {
        let n = PlayerName::new("alice");
        assert_eq!(n.to_string(), "alice");
        assert_eq!(n.as_str(), "alice");
    }

    #[test]
    fn color_opposite() {
        assert_eq!(Color::Black.opposite(), Color::White);
        assert_eq!(Color::White.opposite(), Color::Black);
    }
}
