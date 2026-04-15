//! Pentanomial 集計と正規化 Elo / logistic Elo 計算。
//!
//! Penta は「同じ開始局面で先後入替えた 2 ゲーム」を 1 ペアとして数える。
//! 視点は challenger (test engine) に固定。`ww` は test が 2 連勝、
//! `ll` は test が 2 連敗、`wl` は先後で 1 勝 1 敗、など。
//!
//! 5 項ベクトルへの射影は `(dd + wl)` をまとめて次の通り:
//! ```text
//! p = [ll, dl, dd+wl, wd, ww] / pair_count
//! score vector s = [0.0, 0.25, 0.5, 0.75, 1.0]
//! score    = Σ p_i * s_i
//! variance = Σ p_i * (s_i - score)^2
//! ```

/// 正規分布 95% 信頼区間で使う 1.96。
pub(crate) const NORM_PPF_0_975: f64 = 1.959_963_984_540_054;

/// 1 ゲームの結果。test engine (challenger) 視点。
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum GameSide {
    Win,
    Draw,
    Loss,
}

/// Pentanomial 集計。
///
/// 視点は challenger (test) 固定。
///
/// - `ww`: test が 2 連勝
/// - `wd`: 1 勝 1 分
/// - `wl`: 1 勝 1 敗
/// - `dd`: 2 分
/// - `dl`: 1 分 1 敗
/// - `ll`: 2 連敗
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Penta {
    pub ll: u64,
    pub dl: u64,
    pub dd: u64,
    pub wl: u64,
    pub wd: u64,
    pub ww: u64,
}

impl Penta {
    pub const ZERO: Penta = Penta {
        ll: 0,
        dl: 0,
        dd: 0,
        wl: 0,
        wd: 0,
        ww: 0,
    };

    /// 2 ゲームの結果から 1 ペア分の Penta を得る（challenger 視点）。
    pub fn from_pair(game_a: GameSide, game_b: GameSide) -> Penta {
        use GameSide::{Draw as D, Loss as L, Win as W};
        match (game_a, game_b) {
            (W, W) => Penta {
                ww: 1,
                ..Penta::ZERO
            },
            (W, D) | (D, W) => Penta {
                wd: 1,
                ..Penta::ZERO
            },
            (W, L) | (L, W) => Penta {
                wl: 1,
                ..Penta::ZERO
            },
            (D, D) => Penta {
                dd: 1,
                ..Penta::ZERO
            },
            (D, L) | (L, D) => Penta {
                dl: 1,
                ..Penta::ZERO
            },
            (L, L) => Penta {
                ll: 1,
                ..Penta::ZERO
            },
        }
    }

    /// 観測したペア数。
    pub fn pair_count(&self) -> u64 {
        self.ll + self.dl + self.dd + self.wl + self.wd + self.ww
    }

    /// 各カテゴリの観測確率を 5 項ベクトルに射影する。
    ///
    /// `pair_count() == 0` の場合は `None` を返す（0 除算回避）。
    pub fn to_probs(self) -> Option<[f64; 5]> {
        let pc = self.pair_count();
        if pc == 0 {
            return None;
        }
        let pc = pc as f64;
        Some([
            self.ll as f64 / pc,
            self.dl as f64 / pc,
            (self.dd + self.wl) as f64 / pc,
            self.wd as f64 / pc,
            self.ww as f64 / pc,
        ])
    }

    /// 期待スコア（0 = 全敗, 0.5 = 互角, 1 = 全勝）。
    pub fn score(&self) -> Option<f64> {
        self.to_probs().map(score_of)
    }

    /// スコアの分散。
    pub fn variance(&self) -> Option<f64> {
        let probs = self.to_probs()?;
        let mu = score_of(probs);
        Some(variance_of(probs, mu))
    }

    /// 正規化 Elo とその 95%CI 半幅を返す（challenger 視点）。
    ///
    /// 分散が 0（全勝/全敗/全引分）の場合は `None` を返す。
    pub fn normalized_elo(&self) -> Option<(f64, f64)> {
        let probs = self.to_probs()?;
        let pc = self.pair_count() as f64;
        let mu = score_of(probs);
        let var = variance_of(probs, mu);
        if var <= f64::EPSILON {
            return None;
        }
        let per_pair_var = var / pc;
        let se = per_pair_var.sqrt();
        let score_lower = mu - NORM_PPF_0_975 * se;
        let score_upper = mu + NORM_PPF_0_975 * se;
        let elo = normalized_elo_from(mu, var);
        let elo_lower = normalized_elo_from(score_lower, var);
        let elo_upper = normalized_elo_from(score_upper, var);
        Some((elo, (elo_upper - elo_lower) / 2.0))
    }

    /// logistic (BayesElo 互換) Elo とその 95%CI 半幅。
    pub fn logistic_elo(&self) -> Option<(f64, f64)> {
        let probs = self.to_probs()?;
        let pc = self.pair_count() as f64;
        let mu = score_of(probs);
        let var = variance_of(probs, mu);
        if var <= f64::EPSILON {
            return None;
        }
        let per_pair_var = var / pc;
        let se = per_pair_var.sqrt();
        let score_lower = mu - NORM_PPF_0_975 * se;
        let score_upper = mu + NORM_PPF_0_975 * se;
        let elo = logistic_elo_of(mu);
        let elo_lower = logistic_elo_of(score_lower);
        let elo_upper = logistic_elo_of(score_upper);
        Some((elo, (elo_upper - elo_lower) / 2.0))
    }
}

impl std::ops::Add for Penta {
    type Output = Penta;
    fn add(self, other: Penta) -> Penta {
        Penta {
            ll: self.ll + other.ll,
            dl: self.dl + other.dl,
            dd: self.dd + other.dd,
            wl: self.wl + other.wl,
            wd: self.wd + other.wd,
            ww: self.ww + other.ww,
        }
    }
}

impl std::ops::AddAssign for Penta {
    fn add_assign(&mut self, other: Penta) {
        *self = *self + other;
    }
}

impl std::iter::Sum for Penta {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Penta::ZERO, |a, b| a + b)
    }
}

impl std::fmt::Display for Penta {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}, {}, {}, {}, {}]", self.ll, self.dl, self.dd + self.wl, self.wd, self.ww)
    }
}

fn score_of(probs: [f64; 5]) -> f64 {
    let s = [0.0, 0.25, 0.5, 0.75, 1.0];
    (0..5).map(|i| probs[i] * s[i]).sum()
}

fn variance_of(probs: [f64; 5], mu: f64) -> f64 {
    let s = [0.0, 0.25, 0.5, 0.75, 1.0];
    (0..5).map(|i| probs[i] * (s[i] - mu).powi(2)).sum()
}

fn normalized_elo_from(score: f64, variance: f64) -> f64 {
    let c_et = 800.0 / f64::ln(10.0);
    (score - 0.5) / (2.0 * variance).sqrt() * c_et
}

fn logistic_elo_of(score: f64) -> f64 {
    let s = score.clamp(1e-6, 1.0 - 1e-6);
    -400.0 * (1.0 / s - 1.0).log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_penta_returns_none() {
        let p = Penta::ZERO;
        assert_eq!(p.pair_count(), 0);
        assert!(p.to_probs().is_none());
        assert!(p.score().is_none());
        assert!(p.normalized_elo().is_none());
    }

    #[test]
    fn pair_from_pair_categorization() {
        use GameSide::{Draw, Loss, Win};
        assert_eq!(Penta::from_pair(Win, Win).ww, 1);
        assert_eq!(Penta::from_pair(Win, Draw).wd, 1);
        assert_eq!(Penta::from_pair(Draw, Win).wd, 1);
        assert_eq!(Penta::from_pair(Win, Loss).wl, 1);
        assert_eq!(Penta::from_pair(Draw, Draw).dd, 1);
        assert_eq!(Penta::from_pair(Draw, Loss).dl, 1);
        assert_eq!(Penta::from_pair(Loss, Loss).ll, 1);
    }

    #[test]
    fn penta_sum() {
        let a = Penta::from_pair(GameSide::Win, GameSide::Win);
        let b = Penta::from_pair(GameSide::Loss, GameSide::Loss);
        let c = a + b;
        assert_eq!(c.ww, 1);
        assert_eq!(c.ll, 1);
        assert_eq!(c.pair_count(), 2);
    }

    #[test]
    fn probs_sum_to_one() {
        let mut p = Penta::ZERO;
        p.ww = 3;
        p.wd = 2;
        p.wl = 1;
        p.dd = 4;
        p.dl = 1;
        p.ll = 1;
        let probs = p.to_probs().unwrap();
        let sum: f64 = probs.iter().sum();
        assert!((sum - 1.0).abs() < 1e-12);
    }

    #[test]
    fn score_half_when_symmetric() {
        let mut p = Penta::ZERO;
        p.ww = 5;
        p.ll = 5;
        let s = p.score().unwrap();
        assert!((s - 0.5).abs() < 1e-12);
    }

    #[test]
    fn normalized_elo_zero_variance_returns_none() {
        // 全ペアが dd+wl（スコア 0.5 固定）で variance は 0。
        let mut p = Penta::ZERO;
        p.dd = 10;
        assert_eq!(p.score(), Some(0.5));
        assert_eq!(p.variance(), Some(0.0));
        assert!(p.normalized_elo().is_none());
    }

    #[test]
    fn normalized_elo_positive_when_winning() {
        let mut p = Penta::ZERO;
        p.ww = 20;
        p.wd = 5;
        p.wl = 2;
        p.dd = 3;
        p.dl = 1;
        p.ll = 1;
        let (elo, _ci) = p.normalized_elo().unwrap();
        assert!(elo > 0.0, "expected positive nelo, got {}", elo);
    }
}
