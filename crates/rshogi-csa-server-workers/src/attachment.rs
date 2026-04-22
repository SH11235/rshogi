//! WebSocket に紐づけるロール情報 (`WsAttachment`)。
//!
//! Cloudflare Workers の WebSocket Hibernation では、各 WebSocket に対して
//! `serialize_attachment` で JSON 互換の値を保存できる。この値は isolate が
//! 凍結されても復帰後に `deserialize_attachment` で取り出せるため、
//! 「この ws がどの対局者か」というマッピングを DO 内の in-memory 変数に
//! 頼らず保持できる。
//!
//! 本モジュールは attachment の形式と (de)serialize 規約だけを定義し、
//! worker ランタイムに依存しない。単体テストはホスト target で走る。

use serde::{Deserialize, Serialize};

/// 先手・後手の別。`rshogi_csa_server::types::Color` が `serde::Serialize` を
/// 実装していないため、attachment 用には独自のタグ付き列挙を使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    /// 先手。
    Black,
    /// 後手。
    White,
}

impl Role {
    /// 相手の手番。
    pub fn opposite(self) -> Self {
        match self {
            Role::Black => Role::White,
            Role::White => Role::Black,
        }
    }

    /// `rshogi_csa_server::types::Color` へ変換する。
    pub fn to_core(self) -> rshogi_csa_server::types::Color {
        match self {
            Role::Black => rshogi_csa_server::types::Color::Black,
            Role::White => rshogi_csa_server::types::Color::White,
        }
    }

    /// `rshogi_csa_server::types::Color` から変換する。
    pub fn from_core(color: rshogi_csa_server::types::Color) -> Self {
        match color {
            rshogi_csa_server::types::Color::Black => Role::Black,
            rshogi_csa_server::types::Color::White => Role::White,
        }
    }
}

/// 1 WebSocket に紐づく attachment 値。
///
/// # バリアント
///
/// - [`WsAttachment::Pending`]: LOGIN 到着前の匿名接続。`websocket_message`
///   ハンドラは最初に受信した行を LOGIN として解釈しようとする。
/// - [`WsAttachment::Player`]: 認証済みプレイヤ。色・ハンドル・game_name を保持する。
/// - [`WsAttachment::Spectator`]: 観戦者。`game_id` で観戦対象の対局を特定する。
///   観戦系メッセージ (`%%MONITOR2ON/OFF`, `%%CHAT`) の経路判定と broadcast
///   fanout の対象判定に使う。
///
/// serde タグ付き形式を使い、新 variant を追加しても既存 attachment を
/// 読み壊さない前方互換性を確保する。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WsAttachment {
    /// LOGIN 未完了の匿名接続。
    Pending,
    /// 認証済みプレイヤ。
    Player {
        /// 割り当てられた手番色。
        role: Role,
        /// CSA LOGIN の `<handle>` 部分。プレイヤ識別子として使う。
        handle: String,
        /// CSA LOGIN の `<game_name>` 部分。マッチング時の同名性チェックに使う。
        game_name: String,
    },
    /// 観戦者。`/ws/<room_id>/spectate` から接続したセッションに付与する。
    ///
    /// Player との違いは「盤面を動かす権限を持たず、broadcast を一方向受信する」点。
    /// `game_id` は観戦対象の対局 ID で、`GameRoom` DO が broadcast fanout 時に
    /// `WsAttachment::Spectator` 持ちセッション全てへ配信する判定で使う。
    Spectator {
        /// 観戦対象の対局 ID。
        game_id: String,
    },
}

impl WsAttachment {
    /// プレイヤ attachment を構築する補助関数。
    pub fn player(role: Role, handle: impl Into<String>, game_name: impl Into<String>) -> Self {
        Self::Player {
            role,
            handle: handle.into(),
            game_name: game_name.into(),
        }
    }

    /// 観戦者 attachment を構築する補助関数。
    pub fn spectator(game_id: impl Into<String>) -> Self {
        Self::Spectator {
            game_id: game_id.into(),
        }
    }
}

/// `LOGIN <handle>+<game_name>+<color> <password>` 形式の LOGIN 名を分解する。
///
/// TCP 版 (`crates/rshogi-csa-server-tcp/src/server.rs::parse_handle`) と
/// 同一のコンベンションを採用する。Floodgate 以来の慣習で、クライアントが
/// 希望する手番色まで名前に埋めてくる。
///
/// # 戻り値
/// `(handle, game_name, role)` のタプルを返す。形式が崩れていれば `None`。
pub fn parse_login_handle(raw: &str) -> Option<(String, String, Role)> {
    let mut it = raw.split('+');
    let handle = it.next()?.to_owned();
    let game_name = it.next()?.to_owned();
    let color_s = it.next()?;
    if it.next().is_some() {
        return None;
    }
    let role = match color_s.to_ascii_lowercase().as_str() {
        "black" | "b" | "sente" => Role::Black,
        "white" | "w" | "gote" => Role::White,
        _ => return None,
    };
    if handle.is_empty() || game_name.is_empty() {
        return None;
    }
    Some((handle, game_name, role))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_roundtrips_via_json() {
        let att = WsAttachment::Pending;
        let s = serde_json::to_string(&att).unwrap();
        let back: WsAttachment = serde_json::from_str(&s).unwrap();
        assert_eq!(att, back);
    }

    #[test]
    fn player_roundtrips_via_json() {
        let att = WsAttachment::player(Role::Black, "alice", "floodgate-600-10");
        let s = serde_json::to_string(&att).unwrap();
        let back: WsAttachment = serde_json::from_str(&s).unwrap();
        assert_eq!(att, back);
    }

    #[test]
    fn player_json_has_expected_shape() {
        let att = WsAttachment::player(Role::White, "bob", "gamename");
        let s = serde_json::to_string(&att).unwrap();
        // `#[serde(tag = "type")]` により `type` フィールドが付く想定。
        assert!(s.contains("\"type\":\"Player\""));
        assert!(s.contains("\"role\":\"White\""));
        assert!(s.contains("\"handle\":\"bob\""));
        assert!(s.contains("\"game_name\":\"gamename\""));
    }

    #[test]
    fn role_conversion_is_bijective() {
        for r in [Role::Black, Role::White] {
            assert_eq!(Role::from_core(r.to_core()), r);
            assert_eq!(r.opposite().opposite(), r);
        }
    }

    #[test]
    fn parse_login_handle_basic() {
        assert_eq!(
            parse_login_handle("alice+game1+black"),
            Some(("alice".to_owned(), "game1".to_owned(), Role::Black))
        );
        assert_eq!(
            parse_login_handle("bob+game1+W"),
            Some(("bob".to_owned(), "game1".to_owned(), Role::White))
        );
        assert_eq!(
            parse_login_handle("charlie+floodgate-600-10+SENTE"),
            Some(("charlie".to_owned(), "floodgate-600-10".to_owned(), Role::Black))
        );
    }

    #[test]
    fn parse_login_handle_rejects_malformed() {
        assert!(parse_login_handle("alice").is_none());
        assert!(parse_login_handle("alice+game1").is_none());
        assert!(parse_login_handle("alice+game1+purple").is_none());
        assert!(parse_login_handle("+game1+black").is_none());
        assert!(parse_login_handle("alice++black").is_none());
        assert!(parse_login_handle("alice+game1+black+extra").is_none());
    }

    #[test]
    fn spectator_roundtrips_via_json() {
        let att = WsAttachment::spectator("room-20260101-0001");
        let s = serde_json::to_string(&att).unwrap();
        let back: WsAttachment = serde_json::from_str(&s).unwrap();
        assert_eq!(att, back);
    }

    #[test]
    fn spectator_json_has_expected_shape() {
        let att = WsAttachment::spectator("room-xyz");
        let s = serde_json::to_string(&att).unwrap();
        // `#[serde(tag = "type")]` の下では variant 名が `type` 値に入る。
        assert!(s.contains("\"type\":\"Spectator\""));
        assert!(s.contains("\"game_id\":\"room-xyz\""));
    }

    #[test]
    fn player_and_spectator_are_distinct_types() {
        // 同一ハンドル / ID でも Player と Spectator は別 variant として比較される。
        let player = WsAttachment::player(Role::Black, "alice", "room-1");
        let spec = WsAttachment::spectator("room-1");
        assert_ne!(player, spec);
    }
}
