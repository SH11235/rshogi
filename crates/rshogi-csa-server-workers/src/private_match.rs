//! Lobby から GameRoom へ渡す私的対局の固定条件。
use rshogi_csa_server::ClockSpec;
use rshogi_csa_server::matching::challenge::{ChallengeEntry, ChallengeToken};
use rshogi_csa_server::types::Color;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 再試行でも同じ部屋・配色に収束する私的対局の予約。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrivateMatch {
    /// トークンのハッシュから得る部屋識別子。
    pub room_id: String,
    /// 棋譜・公開索引へ保存する非機密の対局名。
    pub game_name: String,
    /// 先手として招待された handle。
    pub black_handle: String,
    /// 後手として招待された handle。
    pub white_handle: String,
    /// 招待時に確定した時計。
    pub clock: ClockSpec,
    /// 招待時に確定した開始局面。未指定は平手。
    pub initial_sfen: Option<String>,
    /// 新規参加を受理する最終時刻（UNIX epoch ms）。
    pub expires_at_ms: u64,
}

impl PrivateMatch {
    /// token は部屋名へ露出させず、free 配色も再試行間で固定する。
    pub fn from_challenge(token: &ChallengeToken, entry: &ChallengeEntry) -> Self {
        let digest = Sha256::digest(token.as_str().as_bytes());
        let inviter_black = entry
            .inviter_color
            .map(|color| color.to_core() == Color::Black)
            .unwrap_or(digest[0] & 1 == 0);
        let (black_handle, white_handle) = if inviter_black {
            (entry.inviter.clone(), entry.opponent.clone())
        } else {
            (entry.opponent.clone(), entry.inviter.clone())
        };
        Self {
            room_id: format!("private-{:x}", digest),
            game_name: format!("private-{:x}", digest),
            black_handle,
            white_handle,
            clock: entry.clock_spec.clone(),
            initial_sfen: entry.initial_sfen.clone(),
            expires_at_ms: entry.expires_at_ms,
        }
    }

    /// 招待された handle と割り当てた色・対局名だけを受理する。
    pub fn accepts(&self, handle: &str, black: bool, game_name: &str) -> bool {
        game_name.strip_prefix("private-").is_some_and(|token| {
            format!("private-{:x}", Sha256::digest(token.as_bytes())) == self.room_id
        }) && handle
            == if black {
                &self.black_handle
            } else {
                &self.white_handle
            }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rshogi_csa_server::matching::challenge::ColorTag;
    use std::collections::HashMap;

    #[test]
    fn reservation_is_stable_and_binds_token_identity_and_color() {
        let token = ChallengeToken::from_raw("0123456789abcdef01234567");
        for color in [Some(ColorTag::Black), Some(ColorTag::White), None] {
            let entry = ChallengeEntry {
                inviter: "alice".into(),
                opponent: "bob".into(),
                inviter_color: color,
                clock_spec: ClockSpec::default(),
                initial_sfen: None,
                expires_at_ms: 1000,
                pending_ws_attachment_ids: HashMap::new(),
            };
            let plan = PrivateMatch::from_challenge(&token, &entry);
            assert_eq!(plan, PrivateMatch::from_challenge(&token, &entry));
            assert!(crate::room_id::is_valid_room_id(&plan.room_id));
            assert!(!serde_json::to_string(&plan).unwrap().contains(token.as_str()));
            let login_game = format!("private-{}", token.as_str());
            assert!(plan.accepts(&plan.black_handle, true, &login_game));
            assert!(plan.accepts(&plan.white_handle, false, &login_game));
            assert!(!plan.accepts(&plan.white_handle, true, &login_game));
            assert!(!plan.accepts("mallory", false, &login_game));
            assert!(!plan.accepts(&plan.black_handle, true, "private-ffffffffffffffffffffffff"));
            assert!(!plan.accepts(&plan.black_handle, true, &plan.game_name));
            if let Some(color) = color {
                assert_eq!(plan.black_handle == "alice", color == ColorTag::Black);
            }
        }
    }
}
