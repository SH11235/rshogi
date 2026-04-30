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
use crate::types::{Color, PlayerName};
use rand::Rng;
use rand::SeedableRng;
use rand::seq::SliceRandom;
use rand_xoshiro::Xoshiro256PlusPlus;

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

/// レート差・連戦・同一作者ペナルティを最小化するペアリング戦略
/// （Floodgate `least_diff` 相当）。
///
/// # アルゴリズム
///
/// 1. 候補集合を seed 可能な PRNG でシャッフルし、隣接ペアを取って
///    "perfect matching" を作る（候補数が奇数なら最後の 1 名は不成立で残置）
/// 2. 各 trial の "成立ペア数" と "目的関数" を計算し、`(成立ペア数 大, 総コスト 小)`
///    の lexicographic 順で最良の試行を最終結果として採用
/// 3. 既定試行回数 `max_trials = 300`（Requirement 6.2 既定値）
///
/// # 目的関数
///
/// 1 ペア (a, b) のコスト:
///
/// - `(a.rate - b.rate)^2`: レート差の二乗
/// - `back_to_back_penalty`: `a.recent_opponents` に b.name (or 逆) が含まれる場合
///   に加算される連戦ペナルティ
/// - 同一作者ペナルティは player metadata（`:author:`）が現状提供されていない
///   ため本戦略では未実装（必要になった時点で `PairingCandidate` に
///   `author: Option<String>` を追加して対応）
///
/// # 色割り当て
///
/// - 両者が `Some(Color)` で相補的: そのまま割当
/// - 両者が `Some(Color)` で同一: 当該ペアは本 trial では成立としてカウントせず
///   スキップ（trial 全体は破棄しない）。color 偏りのある候補集合（例: Black 3 名 +
///   White 1 名）でも成立可能な組は採用し、残りは待機に戻す
/// - 片方のみ `Some(Color)`: そちらの希望を尊重し、もう片方は反対色
/// - 両者 `None`: シャッフル順序の最初を Black、次を White
///
/// # 決定論性
///
/// 同 seed・同候補集合では同一結果を返す。`with_seed` でテスト固定可。
/// 既定の `new` は OS 乱数で seed する（毎回違う結果）。
#[derive(Debug, Clone)]
pub struct LeastDiffPairingStrategy {
    max_trials: usize,
    back_to_back_penalty: i64,
    seed: Option<u64>,
}

impl Default for LeastDiffPairingStrategy {
    fn default() -> Self {
        Self::new()
    }
}

impl LeastDiffPairingStrategy {
    /// 既定パラメータ（試行 300 回 / 連戦ペナルティ 1_000_000）で構築する。
    /// `seed` は `None`（OS 乱数で seed）。
    pub fn new() -> Self {
        Self {
            max_trials: 300,
            back_to_back_penalty: 1_000_000,
            seed: None,
        }
    }

    /// 試行回数を上書きする（builder スタイル）。
    pub fn with_max_trials(mut self, max_trials: usize) -> Self {
        self.max_trials = max_trials;
        self
    }

    /// 連戦ペナルティの重みを上書きする（builder スタイル）。`0` で連戦無視。
    pub fn with_back_to_back_penalty(mut self, penalty: i64) -> Self {
        self.back_to_back_penalty = penalty;
        self
    }

    /// テスト用: 決定論的な PRNG seed を固定する。
    pub fn with_seed(mut self, seed: u64) -> Self {
        self.seed = Some(seed);
        self
    }

    fn build_rng(&self) -> Xoshiro256PlusPlus {
        match self.seed {
            Some(s) => Xoshiro256PlusPlus::seed_from_u64(s),
            None => Xoshiro256PlusPlus::from_seed(rand::random()),
        }
    }
}

/// 1 ペア分の配色決定ロジック。private match dispatch 等の **shuffling を伴わない
/// 経路** から呼ぶ用途で公開する。
///
/// 双方が `Some` で相補的 → そのまま割当。片方のみ `Some` → そちらの希望を尊重して
/// もう片方は反対色。双方 `None` → `rng.random::<bool>()` で乱択。
/// 双方が同色 (`(Black, Black)` / `(White, White)`) → `None` (色不適合、上位層が
/// CHALLENGE 受理時に弾く想定だが防御的に Option を返す)。
///
/// # `try_pair_with_cost` と統合しない理由
///
/// 既存 [`try_pair_with_cost`] は同形の `match (a_color, b_color)` 表を持つが、
/// `(None, None)` ケースの挙動が異なる:
///
/// - `try_pair_with_cost`: 入力順 `(a, b)` を **deterministic** に Black/White に
///   割当てる。ランダム性は `LeastDiffPairingStrategy::try_pair` が外側で行う
///   `indices.shuffle(&mut rng)` (shuffle ベース) で担保される。
/// - `resolve_color_for_pair`: 入力順がそのままなら配色も固定になるので、本関数
///   内部で `rng.random::<bool>()` を引いて乱択する。private match dispatch は
///   shuffle 経路を持たないため。
///
/// 以上の挙動差を共通 helper に吸収しようとすると引数で動作モードを切り替える
/// 設計になりコストが先に立つため、`(None, None)` の扱いだけが違う 2 関数として
/// 並置する。
pub fn resolve_color_for_pair<R: Rng>(
    a_name: PlayerName,
    a_color: Option<Color>,
    b_name: PlayerName,
    b_color: Option<Color>,
    rng: &mut R,
) -> Option<MatchedPair> {
    match (a_color, b_color) {
        (Some(Color::Black), Some(Color::White)) | (Some(Color::Black), None) => {
            Some(MatchedPair {
                black: a_name,
                white: b_name,
            })
        }
        (Some(Color::White), Some(Color::Black)) | (Some(Color::White), None) => {
            Some(MatchedPair {
                black: b_name,
                white: a_name,
            })
        }
        (None, Some(Color::Black)) => Some(MatchedPair {
            black: b_name,
            white: a_name,
        }),
        (None, Some(Color::White)) => Some(MatchedPair {
            black: a_name,
            white: b_name,
        }),
        (None, None) => {
            if rng.random::<bool>() {
                Some(MatchedPair {
                    black: a_name,
                    white: b_name,
                })
            } else {
                Some(MatchedPair {
                    black: b_name,
                    white: a_name,
                })
            }
        }
        (Some(Color::Black), Some(Color::Black)) | (Some(Color::White), Some(Color::White)) => None,
    }
}

/// 候補ペア (a, b) のコストを計算する。`None` を返した場合は color 不適合で、
/// このペアは本 trial では成立せずスキップ（trial 全体は破棄しない）。
/// `Some(...)` の中身は `(black, white, cost)`。
fn try_pair_with_cost(
    a: &PairingCandidate,
    b: &PairingCandidate,
    back_to_back_penalty: i64,
) -> Option<(MatchedPair, i64)> {
    // 既定 1500 でレート未指定を扱う（design.md / 既存 InMemoryRateStorage 既定値）
    let rate_a = a.rate.unwrap_or(1500);
    let rate_b = b.rate.unwrap_or(1500);
    let rate_diff = (rate_a as i64 - rate_b as i64).abs();
    let mut cost = rate_diff * rate_diff;

    // 連戦ペナルティ: 双方向に history を見る（片側だけ記録されている可能性に対応）。
    let played_recently = a.recent_opponents.iter().any(|n| n == b.name.as_str())
        || b.recent_opponents.iter().any(|n| n == a.name.as_str());
    if played_recently {
        cost = cost.saturating_add(back_to_back_penalty);
    }

    // 色割り当て。色不適合（同一希望）の場合は None を返し、呼び出し側
    // (`LeastDiffPairingStrategy::try_pair`) は当該ペアだけスキップして本 trial を
    // 続行する（trial 全体は破棄しない）。
    let (black, white) = match (a.preferred_color, b.preferred_color) {
        (Some(Color::Black), Some(Color::White)) | (Some(Color::Black), None) => {
            (a.name.clone(), b.name.clone())
        }
        (Some(Color::White), Some(Color::Black)) | (Some(Color::White), None) => {
            (b.name.clone(), a.name.clone())
        }
        (None, Some(Color::Black)) => (b.name.clone(), a.name.clone()),
        (None, Some(Color::White)) => (a.name.clone(), b.name.clone()),
        (None, None) => (a.name.clone(), b.name.clone()),
        // 同一希望の組み合わせは不適合。
        (Some(Color::Black), Some(Color::Black)) | (Some(Color::White), Some(Color::White)) => {
            return None;
        }
    };

    Some((MatchedPair { black, white }, cost))
}

impl PairingLogic for LeastDiffPairingStrategy {
    fn try_pair(&self, candidates: &[PairingCandidate]) -> Vec<MatchedPair> {
        if candidates.len() < 2 {
            return Vec::new();
        }
        // 決定論性のため名前ソート（既定）。trial 内のシャッフルは PRNG で。
        let mut sorted = candidates.to_vec();
        sorted.sort_by(|a, b| a.name.as_str().cmp(b.name.as_str()));

        let mut rng = self.build_rng();
        let mut indices: Vec<usize> = (0..sorted.len()).collect();
        let mut best: Option<(usize, i64, Vec<MatchedPair>)> = None;

        for _trial in 0..self.max_trials {
            indices.shuffle(&mut rng);
            let mut pairs: Vec<MatchedPair> = Vec::with_capacity(sorted.len() / 2);
            let mut total_cost: i64 = 0;
            for chunk in indices.chunks(2) {
                if chunk.len() < 2 {
                    // 奇数要素: 最後の 1 名は本 trial で不成立（残置）。
                    break;
                }
                let a = &sorted[chunk[0]];
                let b = &sorted[chunk[1]];
                if let Some((pair, cost)) = try_pair_with_cost(a, b, self.back_to_back_penalty) {
                    total_cost = total_cost.saturating_add(cost);
                    pairs.push(pair);
                }
                // 色不適合（同一希望）は当該ペアのみスキップして残置。trial 全体は
                // 破棄しない（色偏り時に成立可能な組まで失わないため）。
            }
            // (成立ペア数 大, 総コスト 小) の lexicographic 順で best を更新する。
            // 同コストでも成立数が多い方を優先することで、色不適合スキップ後でも
            // 残りのペアを取りこぼさない。
            let better = match &best {
                None => true,
                Some((bm, bc, _)) => pairs.len() > *bm || (pairs.len() == *bm && total_cost < *bc),
            };
            if better {
                best = Some((pairs.len(), total_cost, pairs));
            }
        }
        best.map(|(_, _, p)| p).unwrap_or_default()
    }

    fn name(&self) -> &'static str {
        "least_diff"
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
            rate: None,
            recent_opponents: Vec::new(),
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
    fn direct_match_skips_unspecified_color() {
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

    fn rated_cand(
        name: &str,
        color: Option<Color>,
        rate: i32,
        recent: Vec<&str>,
    ) -> PairingCandidate {
        PairingCandidate {
            name: PlayerName::new(name),
            preferred_color: color,
            rate: Some(rate),
            recent_opponents: recent.into_iter().map(String::from).collect(),
        }
    }

    /// LeastDiff: レート差が最も小さくなるペアを選ぶ。
    /// alice (1500) / bob (1700) / carol (1510) で 4 名なら、alice-carol (10),
    /// bob-? のペアを作るが、3 名（奇数）なら 1 ペア + 1 残置になる。
    /// 4 名: alice (1500), bob (1700), carol (1510), dave (1690) →
    /// 最適: alice-carol (10² = 100) + bob-dave (10² = 100) = 200
    /// 非最適: alice-bob (200² = 40000) + carol-dave (180² = 32400) = 72400
    #[test]
    fn least_diff_selects_minimum_rate_diff_pairing() {
        let candidates = vec![
            rated_cand("alice", None, 1500, vec![]),
            rated_cand("bob", None, 1700, vec![]),
            rated_cand("carol", None, 1510, vec![]),
            rated_cand("dave", None, 1690, vec![]),
        ];
        let s = LeastDiffPairingStrategy::new().with_seed(42).with_max_trials(300);
        let pairs = s.try_pair(&candidates);
        assert_eq!(pairs.len(), 2, "must produce 2 pairs from 4 candidates");

        // 最適ペアは {alice, carol} と {bob, dave}（順不同）。
        let mut pair_keys: Vec<(String, String)> = pairs
            .iter()
            .map(|p| {
                let mut x = [p.black.as_str().to_owned(), p.white.as_str().to_owned()];
                x.sort();
                (x[0].clone(), x[1].clone())
            })
            .collect();
        pair_keys.sort();
        assert_eq!(
            pair_keys,
            vec![
                ("alice".to_owned(), "carol".to_owned()),
                ("bob".to_owned(), "dave".to_owned()),
            ]
        );
    }

    /// LeastDiff: 連戦ペナルティが効くと、レート差が多少大きくても直近の対戦相手を
    /// 避けるペアが選ばれる。
    #[test]
    fn least_diff_avoids_back_to_back_pairs() {
        // alice (1500) と carol (1510) は直前に対戦済み。連戦ペナルティが
        // 1_000_000（既定）なので、たとえ rate diff 100²=10000 のコストを払っても
        // alice-bob (1500 vs 1600 → 10000) と carol-dave (1510 vs 1610 → 10000) の
        // 合計 20000 が、alice-carol (rate diff² 100 + 連戦 1_000_000) +
        // bob-dave (rate diff² 100) = 1_000_200 より圧倒的に低コスト。
        let candidates = vec![
            rated_cand("alice", None, 1500, vec!["carol"]),
            rated_cand("bob", None, 1600, vec![]),
            rated_cand("carol", None, 1510, vec!["alice"]),
            rated_cand("dave", None, 1610, vec![]),
        ];
        let s = LeastDiffPairingStrategy::new().with_seed(7).with_max_trials(300);
        let pairs = s.try_pair(&candidates);
        let mut pair_keys: Vec<(String, String)> = pairs
            .iter()
            .map(|p| {
                let mut x = [p.black.as_str().to_owned(), p.white.as_str().to_owned()];
                x.sort();
                (x[0].clone(), x[1].clone())
            })
            .collect();
        pair_keys.sort();
        assert_eq!(
            pair_keys,
            vec![
                ("alice".to_owned(), "bob".to_owned()),
                ("carol".to_owned(), "dave".to_owned()),
            ]
        );
    }

    /// LeastDiff: 候補が 2 名未満では空 Vec を返す（DirectMatch と同じ契約）。
    #[test]
    fn least_diff_returns_empty_for_singleton() {
        let s = LeastDiffPairingStrategy::new();
        assert!(s.try_pair(&[rated_cand("alice", None, 1500, vec![])]).is_empty());
    }

    /// LeastDiff: 同じ seed で同じ入力なら同じ結果を返す（決定論性）。
    #[test]
    fn least_diff_is_deterministic_with_same_seed() {
        let candidates = vec![
            rated_cand("a", None, 1500, vec![]),
            rated_cand("b", None, 1550, vec![]),
            rated_cand("c", None, 1600, vec![]),
            rated_cand("d", None, 1650, vec![]),
            rated_cand("e", None, 1700, vec![]),
            rated_cand("f", None, 1750, vec![]),
        ];
        let s1 = LeastDiffPairingStrategy::new().with_seed(123).with_max_trials(50);
        let s2 = LeastDiffPairingStrategy::new().with_seed(123).with_max_trials(50);
        assert_eq!(s1.try_pair(&candidates), s2.try_pair(&candidates));
    }

    /// LeastDiff: rate 未指定（`None`）は既定 1500 として扱う。
    #[test]
    fn least_diff_treats_missing_rate_as_default_1500() {
        // alice rate=None (defaults to 1500), bob rate=Some(2000) →
        // diff² = 500² = 250000
        let candidates = vec![rated_cand("alice", None, 1500, vec![]), {
            let mut c = rated_cand("bob", None, 2000, vec![]);
            c.rate = Some(2000);
            c
        }];
        let s = LeastDiffPairingStrategy::new().with_seed(0).with_max_trials(10);
        let pairs = s.try_pair(&candidates);
        assert_eq!(pairs.len(), 1);
    }

    /// LeastDiff: 色希望が同一の 2 名しかいない場合（trial discard 累積）→
    /// 結果は空 Vec。
    #[test]
    fn least_diff_returns_empty_when_all_prefer_same_color() {
        let candidates = vec![
            rated_cand("alice", Some(Color::Black), 1500, vec![]),
            rated_cand("bob", Some(Color::Black), 1500, vec![]),
        ];
        let s = LeastDiffPairingStrategy::new().with_seed(0);
        assert!(s.try_pair(&candidates).is_empty());
    }

    /// LeastDiff: 色希望が偏っている候補集合（Black 3 + White 1）でも、
    /// 成立可能な 1 ペアは返り、残りは待機に戻る。色不適合の trial 全体破棄を
    /// やめた挙動の回帰テスト。
    #[test]
    fn least_diff_with_color_imbalance_returns_partial_pairs() {
        // alice (Black 1500) と dave (White 1510) のレート差²=100 が最小。
        // bob, carol は Black 同士で組めず本 trial では待機（残置）。
        let candidates = vec![
            rated_cand("alice", Some(Color::Black), 1500, vec![]),
            rated_cand("bob", Some(Color::Black), 1700, vec![]),
            rated_cand("carol", Some(Color::Black), 1900, vec![]),
            rated_cand("dave", Some(Color::White), 1510, vec![]),
        ];
        let s = LeastDiffPairingStrategy::new().with_seed(42).with_max_trials(300);
        let pairs = s.try_pair(&candidates);
        assert_eq!(pairs.len(), 1, "color 偏りでも成立可能な 1 ペアは返る");
        assert_eq!(pairs[0].black.as_str(), "alice");
        assert_eq!(pairs[0].white.as_str(), "dave");
    }

    /// LeastDiff: 戦略名が `"least_diff"` で安定している契約を固定。
    /// `build_strategy` 経由の dispatch が依存する。
    #[test]
    fn least_diff_strategy_name_is_stable() {
        assert_eq!(LeastDiffPairingStrategy::new().name(), "least_diff");
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

    /// `resolve_color_for_pair`: 双方が `Some` で相補的なら指定通り、片方 `None` なら
    /// 反対色、双方 `None` なら rng による乱択、双方同色は `None` を返す契約を
    /// 1 関数で網羅する。
    #[test]
    fn resolve_color_for_pair_covers_all_cases() {
        let mut rng = Xoshiro256PlusPlus::seed_from_u64(0);
        let alice = PlayerName::new("alice");
        let bob = PlayerName::new("bob");

        // 相補的指定
        let r = resolve_color_for_pair(
            alice.clone(),
            Some(Color::Black),
            bob.clone(),
            Some(Color::White),
            &mut rng,
        )
        .unwrap();
        assert_eq!(r.black.as_str(), "alice");
        assert_eq!(r.white.as_str(), "bob");

        // 片方 None
        let r =
            resolve_color_for_pair(alice.clone(), Some(Color::White), bob.clone(), None, &mut rng)
                .unwrap();
        assert_eq!(r.black.as_str(), "bob");
        assert_eq!(r.white.as_str(), "alice");

        let r =
            resolve_color_for_pair(alice.clone(), None, bob.clone(), Some(Color::Black), &mut rng)
                .unwrap();
        assert_eq!(r.black.as_str(), "bob");
        assert_eq!(r.white.as_str(), "alice");

        // 双方 None: rng による乱択 (シードを固定して経路網羅を検証)。
        let r = resolve_color_for_pair(alice.clone(), None, bob.clone(), None, &mut rng).unwrap();
        assert!(
            (r.black.as_str() == "alice" && r.white.as_str() == "bob")
                || (r.black.as_str() == "bob" && r.white.as_str() == "alice"),
        );

        // 同色希望は None
        assert!(
            resolve_color_for_pair(
                alice.clone(),
                Some(Color::Black),
                bob.clone(),
                Some(Color::Black),
                &mut rng,
            )
            .is_none(),
        );
        assert!(
            resolve_color_for_pair(alice, Some(Color::White), bob, Some(Color::White), &mut rng,)
                .is_none(),
        );
    }
}
