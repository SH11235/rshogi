//! Workers 側 WebSocket path の純粋パーサ。
//!
//! `GET /ws/<room_id>` は対局者、`GET /ws/<room_id>/spectate` は観戦者として扱う。
//! ルーティング規則を worker ランタイムから分離し、host target でも単体テスト
//! できるようにする。

use crate::room_id::is_valid_room_id;

/// WebSocket 接続先の種別。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsRoute {
    /// 対局者セッション。
    Player { room_id: String },
    /// 観戦者セッション。
    Spectator { room_id: String },
}

impl WsRoute {
    /// ルートが参照する room_id。
    pub fn room_id(&self) -> &str {
        match self {
            Self::Player { room_id } | Self::Spectator { room_id } => room_id,
        }
    }

    /// 観戦 route かどうか。
    pub fn is_spectator(&self) -> bool {
        matches!(self, Self::Spectator { .. })
    }
}

/// path 文字列から WebSocket route を解釈する。
///
/// `/ws/<room_id>` と `/ws/<room_id>/spectate` だけを受け付ける。`room_id` は
/// [`is_valid_room_id`] を満たす必要がある。
pub fn parse_ws_route(path: &str) -> Option<WsRoute> {
    let tail = path.strip_prefix("/ws/")?;
    let (room_id, spectator) = match tail.split_once('/') {
        None => (tail, false),
        Some((room_id, "spectate")) => (room_id, true),
        Some(_) => return None,
    };
    if !is_valid_room_id(room_id) {
        return None;
    }
    Some(if spectator {
        WsRoute::Spectator {
            room_id: room_id.to_owned(),
        }
    } else {
        WsRoute::Player {
            room_id: room_id.to_owned(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_player_route() {
        assert_eq!(
            parse_ws_route("/ws/room-1"),
            Some(WsRoute::Player {
                room_id: "room-1".to_owned(),
            })
        );
    }

    #[test]
    fn parses_spectator_route() {
        assert_eq!(
            parse_ws_route("/ws/room_1/spectate"),
            Some(WsRoute::Spectator {
                room_id: "room_1".to_owned(),
            })
        );
    }

    #[test]
    fn rejects_unknown_suffix_and_invalid_room() {
        assert_eq!(parse_ws_route("/ws/room-1/extra"), None);
        assert_eq!(parse_ws_route("/ws/room/1"), None);
        assert_eq!(parse_ws_route("/health"), None);
    }
}
