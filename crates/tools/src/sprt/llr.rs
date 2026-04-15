//! LLR (対数尤度比) 計算と Wald 境界。
//!
//! Michel Van den Bergh "Normalized Elo Practical" §4.1 に基づき、
//! MLE を ITP 法（Oliveira-Takahashi 2020）で解いて LLR を計算する。
//!
//! shogitest の `src/sprt.rs` を参考に Rust 化。式の変更はなし。

use std::num::FpCategory;

use super::penta::Penta;

/// SPRT パラメータ。
///
/// - `nelo0`: H0 (帰無仮説) の正規化 Elo。
/// - `nelo1`: H1 (対立仮説) の正規化 Elo。
/// - `alpha`: 第一種過誤率 (false positive)。
/// - `beta`: 第二種過誤率 (false negative)。
///
/// 視点は challenger (test engine)。`nelo1 > nelo0` のとき
/// 「test が base より強い」方向の検定になる。
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct SprtParameters {
    pub nelo0: f64,
    pub nelo1: f64,
    pub alpha: f64,
    pub beta: f64,
    lower_bound: f64,
    upper_bound: f64,
    t0: f64,
    t1: f64,
}

impl SprtParameters {
    /// SPRT パラメータを構築する。
    pub fn new(nelo0: f64, nelo1: f64, alpha: f64, beta: f64) -> SprtParameters {
        let c_et = 800.0 / f64::ln(10.0);
        let lower_bound = (beta / (1.0 - alpha)).ln();
        let upper_bound = ((1.0 - beta) / alpha).ln();
        SprtParameters {
            nelo0,
            nelo1,
            alpha,
            beta,
            lower_bound,
            upper_bound,
            t0: nelo0 / c_et,
            t1: nelo1 / c_et,
        }
    }

    /// Wald 境界 `(lower, upper)`。
    ///
    /// - `llr <= lower` で H0 採択
    /// - `llr >= upper` で H1 採択
    pub fn llr_bounds(&self) -> (f64, f64) {
        (self.lower_bound, self.upper_bound)
    }

    /// `(nelo0, nelo1)` を返す。
    pub fn nelo_bounds(&self) -> (f64, f64) {
        (self.nelo0, self.nelo1)
    }

    /// 与えられた pentanomial 集計に対する LLR を計算する。
    ///
    /// `pair_count == 0` の場合は `0.0` を返す（情報なし）。
    pub fn llr(&self, penta: Penta) -> f64 {
        let Some(raw_probs) = penta.to_probs() else {
            return 0.0;
        };
        let prob = regularize(raw_probs);
        let count = penta.pair_count() as f64;
        llr(
            count,
            prob,
            [0.0, 0.25, 0.5, 0.75, 1.0],
            self.t0 * f64::sqrt(2.0),
            self.t1 * f64::sqrt(2.0),
        )
    }
}

/// t = t0 vs t = t1 の対数尤度比を計算。
///
/// MLE は `mle` が常に `Some` を返す形に統一されているため
/// （退化ケースでは直前の推定値にフォールバック）、ここでは unwrap できる。
fn llr<const N: usize>(count: f64, prob: [f64; N], score: [f64; N], t0: f64, t1: f64) -> f64 {
    let p0 = match mle(prob, score, 0.5, t0) {
        Some(p) => p,
        None => return 0.0,
    };
    let p1 = match mle(prob, score, 0.5, t1) {
        Some(p) => p,
        None => return 0.0,
    };
    count * mean(std::array::from_fn(|i| p1[i].ln() - p0[i].ln()), prob)
}

/// 確率ベクトル `prob` を経験分布とみなし、
/// `t = (mu - mu_ref) / sigma` という条件下での MLE 分布を求める。
///
/// 詳細は Van den Bergh [1] §4.1。
///
/// - 通常ケースでは収束した推定値を `Some` で返す。
/// - `sigma == 0`（退化分布）または `itp` が符号条件違反で解を求められない場合は、
///   panic せず**直前の推定値**で打ち切って `Some(p)` を返す（情報不足扱い）。
/// - 反復内で `f_itp` が NaN になる等、呼び出し側が復旧できない形で失敗した場合のみ `None`。
///   `llr` は `None` を「LLR = 0」にフォールバックする。
fn mle<const N: usize>(
    prob: [f64; N],
    score: [f64; N],
    mu_ref: f64,
    t_star: f64,
) -> Option<[f64; N]> {
    const THETA_EPSILON: f64 = 1e-7;
    const MLE_EPSILON: f64 = 1e-4;
    const MAX_OUTER_ITER: usize = 64;

    let mut p = [1.0 / N as f64; N];

    for _ in 0..MAX_OUTER_ITER {
        let prev_p = p;

        let (mu, variance) = mean_and_variance(score, p);
        let sigma = variance.sqrt();
        if !sigma.is_finite() || sigma <= 0.0 {
            // 退化分布 (1 点集中) なのでこれ以上反復できない。
            // 直前の推定値を返す。
            return Some(p);
        }
        let phi: [f64; N] = std::array::from_fn(|i| {
            let a_i = score[i];
            a_i - mu_ref - 0.5 * t_star * sigma * (1.0 + ((a_i - mu) / sigma).powi(2))
        });

        let u = phi.iter().copied().fold(f64::INFINITY, f64::min);
        let v = phi.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let min_theta = -1.0 / v;
        let max_theta = -1.0 / u;

        // `itp` が符号条件違反で None を返したら、現在の推定値で打ち切る。
        // （MLE 反復のこの段は収束に足らないが panic は避ける）
        let theta = match itp(
            |x: f64| (0..N).map(|i| prob[i] * phi[i] / (1.0 + x * phi[i])).sum(),
            (min_theta, max_theta),
            (f64::INFINITY, f64::NEG_INFINITY),
            0.1,
            2.0,
            0.99,
            THETA_EPSILON,
        ) {
            Some(t) => t,
            None => return Some(p),
        };

        p = std::array::from_fn(|i| prob[i] / (1.0 + theta * phi[i]));

        if (0..N).all(|i| (prev_p[i] - p[i]).abs() < MLE_EPSILON) {
            break;
        }
    }

    Some(p)
}

/// `max(x_i, 1e-3)`。shogitest 仕様に合わせて再正規化はしない。
fn regularize<const N: usize>(x: [f64; N]) -> [f64; N] {
    x.map(|v| v.max(1e-3))
}

fn mean<const N: usize>(x: [f64; N], p: [f64; N]) -> f64 {
    (0..N).map(|i| p[i] * x[i]).sum()
}

fn mean_and_variance<const N: usize>(x: [f64; N], p: [f64; N]) -> (f64, f64) {
    let mu = mean(x, p);
    (mu, (0..N).map(|i| p[i] * (x[i] - mu).powi(2)).sum())
}

/// ITP 法 (Oliveira-Takahashi 2020) による非線形方程式の求解。
///
/// `f(a) < 0 < f(b)` となる区間で `f(x) = 0` を見つける。
/// shogitest と同様、呼び出し側は区間端点の f 値を評価できないとき
/// `(+INF, -INF)` を渡してもよい（根は必ず区間内に存在する前提）。
/// 符号条件が破れた場合は `None` を返す（panic しない）。
fn itp<F>(
    f: F,
    (mut a, mut b): (f64, f64),
    (mut f_a, mut f_b): (f64, f64),
    k_1: f64,
    k_2: f64,
    n_0: f64,
    epsilon: f64,
) -> Option<f64>
where
    F: Fn(f64) -> f64,
{
    if f_a > 0.0 {
        (a, b) = (b, a);
        (f_a, f_b) = (f_b, f_a);
    }

    // 符号条件が崩れていたら panic せず None を返す。
    // 呼び出し側 (`mle`) はそれを「直近の推定値で打ち切る」扱いにする。
    // 注意: `+INF` / `-INF` はセンチネルとして許容する (shogitest 仕様)。
    // NaN だけを弾く。
    if !(f_a < 0.0 && 0.0 < f_b) || f_a.is_nan() || f_b.is_nan() {
        return None;
    }

    let n_half = ((b - a).abs() / (2.0 * epsilon)).log2().ceil();
    let n_max = n_half + n_0;
    let mut i = 0;
    while (b - a).abs() > 2.0 * epsilon {
        let x_half = (a + b) / 2.0;
        let r = epsilon * f64::powf(2.0, n_max - i as f64) - (b - a) / 2.0;
        let delta = k_1 * f64::powf(b - a, k_2);

        let x_f = (f_b * a - f_a * b) / (f_b - f_a);

        let sigma = (x_half - x_f) / (x_half - x_f).abs();
        let x_t = if delta <= (x_half - x_f).abs() {
            x_f + sigma * delta
        } else {
            x_half
        };

        let x_itp = if (x_t - x_half).abs() <= r {
            x_t
        } else {
            x_half - sigma * r
        };

        let f_itp = f(x_itp);
        if f_itp.is_nan() || x_itp.is_nan() {
            // 反復中に数値破綻を検出: 呼び出し側に復旧を任せる。
            return None;
        }
        if f_itp.classify() == FpCategory::Zero {
            a = x_itp;
            b = x_itp;
        } else if f_itp.is_sign_negative() {
            a = x_itp;
            f_a = f_itp;
        } else {
            b = x_itp;
            f_b = f_itp;
        }

        i += 1;
    }

    Some((a + b) / 2.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sprt::penta::{GameSide, Penta};

    fn approx(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() < tol
    }

    #[test]
    fn bounds_are_wald() {
        let p = SprtParameters::new(0.0, 5.0, 0.05, 0.05);
        let (lo, hi) = p.llr_bounds();
        assert!(approx(lo, (0.05_f64 / 0.95).ln(), 1e-12));
        assert!(approx(hi, (0.95_f64 / 0.05).ln(), 1e-12));
    }

    #[test]
    fn zero_penta_llr_is_zero() {
        let p = SprtParameters::new(0.0, 5.0, 0.05, 0.05);
        assert_eq!(p.llr(Penta::ZERO), 0.0);
    }

    #[test]
    fn winning_sample_gives_positive_llr() {
        let params = SprtParameters::new(0.0, 5.0, 0.05, 0.05);
        let mut penta = Penta::ZERO;
        // 明確に test engine 勝ち越し
        penta.ww = 40;
        penta.wd = 20;
        penta.wl = 10;
        penta.dd = 5;
        penta.dl = 3;
        penta.ll = 2;
        let llr = params.llr(penta);
        assert!(llr > 0.0, "expected positive LLR, got {}", llr);
    }

    #[test]
    fn losing_sample_gives_negative_llr() {
        let params = SprtParameters::new(0.0, 5.0, 0.05, 0.05);
        // `Penta::from_pair` を使って対称に反転したペアを作る
        let mut penta = Penta::ZERO;
        penta.ll = 40;
        penta.dl = 20;
        penta.wl = 10;
        penta.dd = 5;
        penta.wd = 3;
        penta.ww = 2;
        let llr = params.llr(penta);
        assert!(llr < 0.0, "expected negative LLR, got {}", llr);
    }

    #[test]
    fn symmetric_flip_flips_sign() {
        // 仮説ペアが 0 周りで対称 (`-5 vs +5`) のときに、
        // Penta を反転すると LLR の符号が厳密に反転することを確認する。
        let params = SprtParameters::new(-5.0, 5.0, 0.05, 0.05);
        let mut win_heavy = Penta::ZERO;
        win_heavy.ww = 30;
        win_heavy.wd = 10;
        win_heavy.wl = 5;
        win_heavy.dd = 3;
        win_heavy.dl = 2;
        win_heavy.ll = 1;

        // 先後反転相当: ww <-> ll, wd <-> dl (dd + wl はそのまま)
        let mut loss_heavy = Penta::ZERO;
        loss_heavy.ll = win_heavy.ww;
        loss_heavy.dl = win_heavy.wd;
        loss_heavy.wl = win_heavy.wl;
        loss_heavy.dd = win_heavy.dd;
        loss_heavy.wd = win_heavy.dl;
        loss_heavy.ww = win_heavy.ll;

        let l1 = params.llr(win_heavy);
        let l2 = params.llr(loss_heavy);
        assert!(approx(l1, -l2, 1e-3), "expected symmetric LLR, got l1={}, l2={}", l1, l2);
        assert!(l1 > 0.0 && l2 < 0.0);
    }

    #[test]
    fn asymmetric_hypothesis_sign_is_still_flipped() {
        // production デフォルト (0/5) でも、ペアを反転したら LLR 符号は必ず逆転する。
        // 厳密な絶対値一致は期待できないが、符号と大体の大きさは確認する。
        let params = SprtParameters::new(0.0, 5.0, 0.05, 0.05);
        let mut win_heavy = Penta::ZERO;
        win_heavy.ww = 30;
        win_heavy.wd = 10;
        win_heavy.wl = 5;
        win_heavy.dd = 3;
        win_heavy.dl = 2;
        win_heavy.ll = 1;
        let mut loss_heavy = Penta::ZERO;
        loss_heavy.ll = win_heavy.ww;
        loss_heavy.dl = win_heavy.wd;
        loss_heavy.wl = win_heavy.wl;
        loss_heavy.dd = win_heavy.dd;
        loss_heavy.wd = win_heavy.dl;
        loss_heavy.ww = win_heavy.ll;
        let l1 = params.llr(win_heavy);
        let l2 = params.llr(loss_heavy);
        assert!(l1 > 0.0 && l2 < 0.0);
        // 相対誤差 5% 以内で反対符号
        let rel = (l1 + l2).abs() / l1.abs().max(l2.abs());
        assert!(rel < 0.05, "l1={}, l2={}, rel={}", l1, l2, rel);
    }

    #[test]
    fn pair_from_pair_all_wins() {
        let p = Penta::from_pair(GameSide::Win, GameSide::Win);
        assert_eq!(p.pair_count(), 1);
        assert_eq!(p.ww, 1);
    }
}
