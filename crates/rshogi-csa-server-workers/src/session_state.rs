//! マッチング・セッション管理の純粋ロジック。
//!
//! Durable Object 上での対局待ち合わせ (pending slot → 2 人揃ったらマッチ) を
//! worker ランタイムから切り離して単体テスト可能にする。SQL / ストレージへの
//! 実体アクセスは [`crate::game_room`] 側が持ち、本モジュールは「与えられた
//! slot 群から match すべきかを判断する」ことだけを担う。

use serde::{Deserialize, Serialize};

use crate::attachment::Role;

/// 1 WebSocket が LOGIN 後に占有するスロット。room_id 内で role はユニーク。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Slot {
    /// 割り当てられた手番色。
    pub role: Role,
    /// CSA LOGIN の `<handle>`。
    pub handle: String,
    /// CSA LOGIN の `<game_name>`。マッチ成立の同一性チェックに使う。
    pub game_name: String,
}

/// マッチング判定の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchResult {
    /// 対局者が揃っていない（slot 数 0 または 1）。
    NotReady,
    /// 2 人揃い、game_name が一致してマッチが成立した。
    Match {
        /// 先手プレイヤのハンドル。
        black_handle: String,
        /// 後手プレイヤのハンドル。
        white_handle: String,
        /// 共通の game_name。
        game_name: String,
    },
    /// 2 人いるが役割衝突や game_name 不一致で成立しない。
    Conflict {
        /// 失敗理由の短いテキスト（ログ用）。
        reason: &'static str,
    },
}

/// 現在のスロット群から match 可否を判定する。
///
/// 呼び出し前提: `slots` は同一 DO インスタンス (= 同一 room_id) の全スロット。
/// 重複 role が混入することは通常想定外だが、防御的に `Conflict` で検出する。
pub fn evaluate_match(slots: &[Slot]) -> MatchResult {
    match slots.len() {
        0 | 1 => MatchResult::NotReady,
        2 => {
            let (a, b) = (&slots[0], &slots[1]);
            let (black, white) = match (a.role, b.role) {
                (Role::Black, Role::White) => (a, b),
                (Role::White, Role::Black) => (b, a),
                _ => {
                    return MatchResult::Conflict {
                        reason: "duplicate role",
                    };
                }
            };
            if black.game_name != white.game_name {
                return MatchResult::Conflict {
                    reason: "game_name mismatch",
                };
            }
            MatchResult::Match {
                black_handle: black.handle.clone(),
                white_handle: white.handle.clone(),
                game_name: black.game_name.clone(),
            }
        }
        _ => MatchResult::Conflict {
            reason: "too many slots",
        },
    }
}

/// Cloudflare Workers に送出する LOGIN 応答のラッパ。プロトコル仕様に準拠し、
/// 成功時は `LOGIN:<handle> OK`、失敗時は `LOGIN:incorrect` を返す
/// (CSA v1.2.1 §Login Sequence)。
pub enum LoginReply {
    /// 認証成功。`name` は LOGIN 時に受理したハンドル全文（`handle+game_name+color`）を使う。
    Ok {
        /// LOGIN 応答で返すハンドル文字列。
        name: String,
    },
    /// 認証失敗、あるいは LOGIN 形式不正。
    Incorrect,
}

impl LoginReply {
    /// 1 行分の CSA 応答テキストに変換する。
    pub fn to_line(&self) -> String {
        match self {
            LoginReply::Ok { name } => format!("LOGIN:{name} OK"),
            LoginReply::Incorrect => "LOGIN:incorrect".to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(role: Role, handle: &str, game: &str) -> Slot {
        Slot {
            role,
            handle: handle.to_owned(),
            game_name: game.to_owned(),
        }
    }

    #[test]
    fn zero_slot_is_not_ready() {
        assert_eq!(evaluate_match(&[]), MatchResult::NotReady);
    }

    #[test]
    fn one_slot_is_not_ready() {
        assert_eq!(evaluate_match(&[slot(Role::Black, "a", "g1")]), MatchResult::NotReady);
    }

    #[test]
    fn two_complementary_slots_with_same_game_name_match() {
        let slots = [slot(Role::Black, "a", "g1"), slot(Role::White, "b", "g1")];
        assert_eq!(
            evaluate_match(&slots),
            MatchResult::Match {
                black_handle: "a".to_owned(),
                white_handle: "b".to_owned(),
                game_name: "g1".to_owned(),
            }
        );
    }

    #[test]
    fn match_is_symmetric_in_slot_order() {
        let forward = [slot(Role::Black, "a", "g1"), slot(Role::White, "b", "g1")];
        let reversed = [slot(Role::White, "b", "g1"), slot(Role::Black, "a", "g1")];
        assert_eq!(evaluate_match(&forward), evaluate_match(&reversed));
    }

    #[test]
    fn duplicate_role_is_conflict() {
        let slots = [slot(Role::Black, "a", "g1"), slot(Role::Black, "b", "g1")];
        assert!(matches!(evaluate_match(&slots), MatchResult::Conflict { .. }));
    }

    #[test]
    fn game_name_mismatch_is_conflict() {
        let slots = [slot(Role::Black, "a", "g1"), slot(Role::White, "b", "g2")];
        assert!(matches!(evaluate_match(&slots), MatchResult::Conflict { .. }));
    }

    #[test]
    fn login_reply_lines_have_expected_format() {
        assert_eq!(
            LoginReply::Ok {
                name: "alice+g1+black".to_owned()
            }
            .to_line(),
            "LOGIN:alice+g1+black OK"
        );
        assert_eq!(LoginReply::Incorrect.to_line(), "LOGIN:incorrect");
    }
}
