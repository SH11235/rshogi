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
/// `/ws/<room_id>` と `/ws/<id>/spectate` だけを受け付ける。`room_id` は
/// [`is_valid_room_id`] を満たす必要がある。
///
/// 観戦経路の `<id>` は room_id でも game_id 形式 (= `lobby-<game_name>-<32hex>-<13桁以上epoch_ms>`)
/// でも受理する。game_id 形式と判別したら [`extract_room_id_for_spectate`] で
/// suffix の epoch_ms 部を剥がして DO ルーティング用の room_id だけを採用する。
/// 対局者経路 (`/ws/<room_id>`) はこの suffix 剥がしを通さない (URL 由来の値を
/// そのまま使う既存挙動を維持する)。
pub fn parse_ws_route(path: &str) -> Option<WsRoute> {
    let tail = path.strip_prefix("/ws/")?;
    let (id, spectator) = match tail.split_once('/') {
        None => (tail, false),
        Some((room_id, "spectate")) => (room_id, true),
        Some(_) => return None,
    };
    if spectator {
        let room_id = extract_room_id_for_spectate(id);
        if !is_valid_room_id(room_id) {
            return None;
        }
        Some(WsRoute::Spectator {
            room_id: room_id.to_owned(),
        })
    } else {
        if !is_valid_room_id(id) {
            return None;
        }
        Some(WsRoute::Player {
            room_id: id.to_owned(),
        })
    }
}

/// spectate 経路 `<id>` から DO ルーティング用の `room_id` を返す。
///
/// game_id 形式 (= 末尾 `-<13 桁以上の 10 進数字>` で、prefix も
/// `is_valid_room_id` を満たす) と判別したら prefix のみを返す。それ以外は
/// `<id>` 全体をそのまま返す。
///
/// 13 桁以上を採用するのは epoch ミリ秒の桁数 (現代では 13 桁、長期運用で 14
/// 桁も想定) を許容するためで、上限は設けない。room_id 形式の `lobby-<name>-<32 hex>`
/// は末尾 32 hex で a-f を含み得るため、本ヒューリスティックでは判別しない
/// (= 全体を採用する) ことで衝突しない。
fn extract_room_id_for_spectate(id: &str) -> &str {
    if let Some(idx) = id.rfind('-') {
        let suffix = &id[idx + 1..];
        let prefix = &id[..idx];
        if suffix.len() >= 13
            && suffix.chars().all(|c| c.is_ascii_digit())
            && is_valid_room_id(prefix)
        {
            return prefix;
        }
    }
    id
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

    #[test]
    fn extract_room_id_room_only() {
        // game_id 形式ではない (suffix が短い / 数字でない) ので `<id>` 全体を
        // そのまま room_id として返す。
        assert_eq!(extract_room_id_for_spectate("lobby-foo-bar"), "lobby-foo-bar");
    }

    #[test]
    fn extract_room_id_game_form() {
        // 13 桁の epoch ms suffix を剥がして prefix を採用する。
        assert_eq!(extract_room_id_for_spectate("lobby-foo-1777391025209"), "lobby-foo");
    }

    #[test]
    fn extract_room_id_short_suffix() {
        // 12 桁では epoch ms と判別しない (= room_id 全体を保持)。
        assert_eq!(
            extract_room_id_for_spectate("lobby-foo-123456789012"),
            "lobby-foo-123456789012"
        );
    }

    #[test]
    fn extract_room_id_15_digit() {
        // 15 桁 epoch (将来拡張) も上限なしルールで剥がす。
        assert_eq!(extract_room_id_for_spectate("lobby-foo-123456789012345"), "lobby-foo");
    }

    #[test]
    fn extract_room_id_non_digit_suffix() {
        // 数字でない suffix (例: `abc`) は剥がさず room_id 全体を保持。
        assert_eq!(extract_room_id_for_spectate("lobby-foo-abc"), "lobby-foo-abc");
    }

    #[test]
    fn extract_room_id_room_id_with_dash_only() {
        // 内部 `-` を保持する room_id (`lobby-cross-fischer-v2-...`) でも、
        // 最後の `-` が数字 13 桁以上でなければ全体を保持する。
        assert_eq!(
            extract_room_id_for_spectate("lobby-cross-fischer-v2-deadbeef"),
            "lobby-cross-fischer-v2-deadbeef"
        );
    }

    #[test]
    fn parses_spectator_route_with_game_id_form_strips_suffix() {
        // 統合: parse_ws_route の spectate 経路に game_id 形式を渡しても
        // 戻り値は既存形 `WsRoute::Spectator { room_id }`。
        assert_eq!(
            parse_ws_route("/ws/lobby-foo-1777391025209/spectate"),
            Some(WsRoute::Spectator {
                room_id: "lobby-foo".to_owned(),
            })
        );
    }

    #[test]
    fn parses_player_route_does_not_strip_suffix() {
        // player 経路は suffix 剥がしを通さない (= 既存挙動完全維持)。
        // ただし `id_from_name` のキー一致が前提なので、URL に書いたままを
        // room_id として採用する。
        assert_eq!(
            parse_ws_route("/ws/lobby-foo-1777391025209"),
            Some(WsRoute::Player {
                room_id: "lobby-foo-1777391025209".to_owned(),
            })
        );
    }
}
