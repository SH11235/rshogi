//! 終局結果モデルと、対局者・観戦者へ送信するメッセージ行列生成。
//!
//! Requirement 4.7 に従い、送信順は `(a) 終局理由コード → (b) 勝敗コード` を徹底する。

use crate::types::Color;

/// `#ILLEGAL_MOVE` が通知される際の補足事由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IllegalReason {
    /// 非合法手（移動先・駒種の不整合）。
    Generic,
    /// 打ち歩詰。
    Uchifuzume,
    /// `%KACHI` 宣言が 24 点法で不成立。
    IllegalKachi,
}

/// 終局理由のモデル。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GameResult {
    /// `%TORYO` → `#RESIGN`。
    Toryo {
        /// 投了負けしなかった側（勝者）。
        winner: Color,
    },
    /// `#TIME_UP`。
    TimeUp {
        /// 時間切れした側（敗者）。
        loser: Color,
    },
    /// `#ILLEGAL_MOVE`（Generic／Uchifuzume／IllegalKachi）。
    IllegalMove {
        /// 非合法手を指した側（敗者）。
        loser: Color,
        /// 反則の種別。
        reason: IllegalReason,
    },
    /// `%KACHI` → `#JISHOGI`（24 点法成立）。
    Kachi {
        /// 入玉宣言が成立した側（勝者）。
        winner: Color,
    },
    /// `#OUTE_SENNICHITE`（連続王手千日手）。王手側の反則負け。
    OuteSennichite {
        /// 王手側（敗者）。
        loser: Color,
    },
    /// `#SENNICHITE` → `#DRAW`（通常千日手）。
    Sennichite,
    /// `#MAX_MOVES` → `#CENSORED`（最大手数到達）。
    MaxMoves,
    /// `#ABNORMAL`（切断・内部エラー）。`winner` は確定不能時 `None`。
    Abnormal {
        /// 確定している勝者（切断側が敗北）。
        winner: Option<Color>,
    },
}

/// 通知対象の種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Audience {
    /// 勝者本人。
    Winner,
    /// 敗者本人。
    Loser,
    /// 観戦者。
    Spectator,
    /// 引き分け・無勝負で全員に同一メッセージを送る際。
    All,
}

/// 関係者ごとに送るメッセージ行列。行単位で順序保持。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultMessages {
    /// `(宛先, 行列)` の並び。並び順がそのまま送信順。
    pub sends: Vec<(Audience, Vec<String>)>,
}

impl GameResult {
    /// 要件 4.7 に従い、終局理由コード → 勝敗コード の順でメッセージ行列を生成する。
    pub fn server_messages(&self) -> ResultMessages {
        match self {
            GameResult::Toryo { .. } => pair_win_lose("#RESIGN"),
            GameResult::TimeUp { .. } => pair_win_lose("#TIME_UP"),
            GameResult::IllegalMove { .. } => pair_win_lose("#ILLEGAL_MOVE"),
            GameResult::Kachi { .. } => pair_win_lose("#JISHOGI"),
            GameResult::OuteSennichite { .. } => pair_win_lose("#OUTE_SENNICHITE"),
            GameResult::Sennichite => ResultMessages {
                sends: vec![(Audience::All, vec!["#SENNICHITE".to_owned(), "#DRAW".to_owned()])],
            },
            GameResult::MaxMoves => ResultMessages {
                sends: vec![(Audience::All, vec!["#MAX_MOVES".to_owned(), "#CENSORED".to_owned()])],
            },
            GameResult::Abnormal { winner } => match winner {
                Some(_) => pair_win_lose("#ABNORMAL"),
                None => ResultMessages {
                    sends: vec![(Audience::All, vec!["#ABNORMAL".to_owned()])],
                },
            },
        }
    }

    /// 終局の勝者（存在すれば）。
    pub fn winner(&self) -> Option<Color> {
        match self {
            GameResult::Toryo { winner } | GameResult::Kachi { winner } => Some(*winner),
            GameResult::Abnormal { winner } => *winner,
            GameResult::TimeUp { loser } => Some(loser.opposite()),
            GameResult::IllegalMove { loser, .. } => Some(loser.opposite()),
            GameResult::OuteSennichite { loser } => Some(loser.opposite()),
            GameResult::Sennichite | GameResult::MaxMoves => None,
        }
    }
}

fn pair_win_lose(reason: &str) -> ResultMessages {
    ResultMessages {
        sends: vec![
            (Audience::Winner, vec![reason.to_owned(), "#WIN".to_owned()]),
            (Audience::Loser, vec![reason.to_owned(), "#LOSE".to_owned()]),
            (Audience::Spectator, vec![reason.to_owned(), "#WIN".to_owned()]),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toryo_emits_resign_then_win_lose_in_order() {
        let r = GameResult::Toryo {
            winner: Color::Black,
        };
        let msg = r.server_messages();
        // 3 宛先すべて #RESIGN が先、勝敗コードが後
        for (_, lines) in msg.sends.iter() {
            assert_eq!(lines.len(), 2);
            assert_eq!(lines[0], "#RESIGN");
            assert!(lines[1] == "#WIN" || lines[1] == "#LOSE");
        }
    }

    #[test]
    fn sennichite_sends_same_message_to_all() {
        let msg = GameResult::Sennichite.server_messages();
        assert_eq!(msg.sends.len(), 1);
        assert_eq!(msg.sends[0].0, Audience::All);
        assert_eq!(msg.sends[0].1, vec!["#SENNICHITE", "#DRAW"]);
    }

    #[test]
    fn max_moves_sends_all_censored() {
        let msg = GameResult::MaxMoves.server_messages();
        assert_eq!(msg.sends[0].0, Audience::All);
        assert_eq!(msg.sends[0].1, vec!["#MAX_MOVES", "#CENSORED"]);
    }

    #[test]
    fn time_up_winner_is_opposite_of_loser() {
        let r = GameResult::TimeUp {
            loser: Color::Black,
        };
        assert_eq!(r.winner(), Some(Color::White));
    }

    #[test]
    fn illegal_move_reason_kept() {
        let r = GameResult::IllegalMove {
            loser: Color::White,
            reason: IllegalReason::Uchifuzume,
        };
        let msg = r.server_messages();
        // Uchifuzume でも生成される行は `#ILLEGAL_MOVE` + `#WIN/#LOSE`
        assert_eq!(msg.sends[0].1[0], "#ILLEGAL_MOVE");
    }

    #[test]
    fn abnormal_with_winner_pairs_messages() {
        let r = GameResult::Abnormal {
            winner: Some(Color::Black),
        };
        let msg = r.server_messages();
        assert_eq!(msg.sends.len(), 3);
    }

    #[test]
    fn abnormal_without_winner_broadcasts_all() {
        let r = GameResult::Abnormal { winner: None };
        let msg = r.server_messages();
        assert_eq!(msg.sends[0].0, Audience::All);
        assert_eq!(msg.sends[0].1, vec!["#ABNORMAL"]);
    }
}
