//! ペアリング戦略の抽象。
//!
//! 現状は [`DirectMatchStrategy`]（同一 `game_name` × 相補手番の 1 ペア）のみ
//! 提供する。Floodgate 既定／Swiss／Random／駒落ちなどを同じ trait で差し替え
//! られるように、戦略は `&[PairingCandidate]` を受けて `Vec<MatchedPair>` を返す
//! 純関数 `try_pair` として揃える。
//!
//! 副作用（プレイヤ状態の AgreeWaiting 遷移など）は呼び出し側
//! ([`crate::matching::league::League::confirm_match`]) が担う。

use crate::matching::league::{MatchedPair, PairingCandidate};
use crate::types::Color;

/// ペアリング戦略の共通インタフェース。
///
/// 1 回の起動で 0 件以上の `MatchedPair` を返す。複数戦略をチェーンする場合は
/// [`PairingChain`] のような上位コンテナで順番に呼び出し、確定したペアを
/// 候補リストから除いて次戦略に渡す（複数戦略チェーンの導入予定経路）。
pub trait PairingLogic {
    /// 成立したペア一覧を返す。0 件もあり得る。
    ///
    /// `candidates` は呼び出し側で必要な絞り込み（同一 `game_name` 等）を済ませた
    /// 状態で渡されることを期待する。
    fn try_pair(&self, candidates: &[PairingCandidate]) -> Vec<MatchedPair>;

    /// 戦略名（ログ・メトリクス用の識別子）。
    fn name(&self) -> &'static str;
}

/// 直接マッチ戦略：相補的手番（Black × White）が揃った最小ペアを 1 組返す。
///
/// 既定戦略。手番未指定（`preferred_color = None`）のプレイヤは対象外で、
/// 任意手番配分が必要になったら上位の戦略で補完する。
#[derive(Debug, Default, Clone, Copy)]
pub struct DirectMatchStrategy;

impl DirectMatchStrategy {
    /// 新しい戦略インスタンス。
    pub fn new() -> Self {
        Self
    }
}

impl PairingLogic for DirectMatchStrategy {
    fn try_pair(&self, candidates: &[PairingCandidate]) -> Vec<MatchedPair> {
        if candidates.len() < 2 {
            return Vec::new();
        }
        // 候補は呼び出し側で名前順ソート済みの想定だが、戦略単体テストの安定性を
        // 担保するため念のため複製して再ソートする。
        let mut sorted = candidates.to_vec();
        sorted.sort_by(|a, b| a.name.as_str().cmp(b.name.as_str()));

        for i in 0..sorted.len() {
            for j in (i + 1)..sorted.len() {
                let ci = sorted[i].preferred_color;
                let cj = sorted[j].preferred_color;
                match (ci, cj) {
                    (Some(Color::Black), Some(Color::White)) => {
                        return vec![MatchedPair {
                            black: sorted[i].name.clone(),
                            white: sorted[j].name.clone(),
                        }];
                    }
                    (Some(Color::White), Some(Color::Black)) => {
                        return vec![MatchedPair {
                            black: sorted[j].name.clone(),
                            white: sorted[i].name.clone(),
                        }];
                    }
                    _ => {}
                }
            }
        }
        Vec::new()
    }

    fn name(&self) -> &'static str {
        "direct"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::PlayerName;

    fn cand(name: &str, color: Option<Color>) -> PairingCandidate {
        PairingCandidate {
            name: PlayerName::new(name),
            preferred_color: color,
        }
    }

    #[test]
    fn direct_match_returns_complementary_pair() {
        let s = DirectMatchStrategy::new();
        let pairs = s.try_pair(&[
            cand("alice", Some(Color::Black)),
            cand("bob", Some(Color::White)),
        ]);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].black.as_str(), "alice");
        assert_eq!(pairs[0].white.as_str(), "bob");
    }

    #[test]
    fn direct_match_swaps_when_first_is_white() {
        let s = DirectMatchStrategy::new();
        let pairs = s.try_pair(&[
            cand("alice", Some(Color::White)),
            cand("bob", Some(Color::Black)),
        ]);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].black.as_str(), "bob");
        assert_eq!(pairs[0].white.as_str(), "alice");
    }

    #[test]
    fn direct_match_returns_empty_when_only_one_candidate() {
        let s = DirectMatchStrategy::new();
        assert!(s.try_pair(&[cand("alice", Some(Color::Black))]).is_empty());
    }

    #[test]
    fn direct_match_skips_same_color_pair() {
        let s = DirectMatchStrategy::new();
        let pairs = s.try_pair(&[
            cand("alice", Some(Color::Black)),
            cand("bob", Some(Color::Black)),
        ]);
        assert!(pairs.is_empty());
    }

    #[test]
    fn direct_match_skips_unspecified_color_in_phase1() {
        let s = DirectMatchStrategy::new();
        let pairs = s.try_pair(&[cand("alice", None), cand("bob", Some(Color::Black))]);
        assert!(pairs.is_empty());
    }

    #[test]
    fn strategy_name_is_stable() {
        assert_eq!(DirectMatchStrategy::new().name(), "direct");
    }

    /// 戦略は trait オブジェクトとしても扱える（差し替え可能性の確認）。
    #[test]
    fn strategy_is_usable_via_trait_object() {
        let s: Box<dyn PairingLogic> = Box::new(DirectMatchStrategy::new());
        assert_eq!(s.name(), "direct");
        let pairs = s.try_pair(&[
            cand("alice", Some(Color::Black)),
            cand("bob", Some(Color::White)),
        ]);
        assert_eq!(pairs.len(), 1);
    }

    /// League → waiting_candidates → PairingLogic → confirm_match の一連の経路。
    #[test]
    fn end_to_end_pair_flow_with_league_confirms_agree_waiting() {
        use crate::matching::league::{League, PlayerStatus};
        use crate::types::{GameId, GameName};

        let mut league = League::new();
        league.login(&PlayerName::new("alice"), false);
        league.login(&PlayerName::new("bob"), false);
        for (n, c) in [("alice", Color::Black), ("bob", Color::White)] {
            league
                .transition(
                    &PlayerName::new(n),
                    PlayerStatus::GameWaiting {
                        game_name: GameName::new("g1"),
                        preferred_color: Some(c),
                    },
                )
                .unwrap();
        }

        let strategy: Box<dyn PairingLogic> = Box::new(DirectMatchStrategy::new());
        let candidates = league.waiting_candidates(&GameName::new("g1"));
        let pairs = strategy.try_pair(&candidates);
        assert_eq!(pairs.len(), 1);

        league.confirm_match(&pairs[0], GameId::new("g1-001")).unwrap();
        for n in ["alice", "bob"] {
            assert!(matches!(
                league.status(&PlayerName::new(n)),
                Some(PlayerStatus::AgreeWaiting { .. })
            ));
        }
    }
}
