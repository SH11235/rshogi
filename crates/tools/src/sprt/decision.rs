//! SPRT の判定状態。

use super::llr::SprtParameters;
use super::penta::Penta;

/// SPRT の判定結果。
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// まだ境界に達していない。
    Running,
    /// `LLR <= lower_bound`。H0 採択（差は nelo0 未満）。
    AcceptH0,
    /// `LLR >= upper_bound`。H1 採択（差は nelo1 以上）。
    AcceptH1,
}

impl Decision {
    pub fn as_str(&self) -> &'static str {
        match self {
            Decision::Running => "running",
            Decision::AcceptH0 => "accept_h0",
            Decision::AcceptH1 => "accept_h1",
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Decision::AcceptH0 | Decision::AcceptH1)
    }
}

/// 与えられた pentanomial と SPRT パラメータから判定を行う。
///
/// `pair_count == 0` の場合は `Running` を返す。
pub fn judge(params: &SprtParameters, penta: Penta) -> Decision {
    if penta.pair_count() == 0 {
        return Decision::Running;
    }
    let llr = params.llr(penta);
    let (lo, hi) = params.llr_bounds();
    if llr <= lo {
        Decision::AcceptH0
    } else if llr >= hi {
        Decision::AcceptH1
    } else {
        Decision::Running
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_running() {
        let params = SprtParameters::new(0.0, 5.0, 0.05, 0.05).unwrap();
        assert_eq!(judge(&params, Penta::ZERO), Decision::Running);
    }

    #[test]
    fn heavy_win_accepts_h1_eventually() {
        let params = SprtParameters::new(0.0, 5.0, 0.05, 0.05).unwrap();
        let mut p = Penta::ZERO;
        // test が圧倒的に勝ち越す状況を大量に与える
        p.ww = 400;
        p.wd = 100;
        p.wl = 30;
        p.dd = 20;
        p.dl = 10;
        p.ll = 5;
        assert_eq!(judge(&params, p), Decision::AcceptH1);
    }

    #[test]
    fn heavy_loss_accepts_h0_eventually() {
        let params = SprtParameters::new(0.0, 5.0, 0.05, 0.05).unwrap();
        let mut p = Penta::ZERO;
        p.ll = 400;
        p.dl = 100;
        p.wl = 30;
        p.dd = 20;
        p.wd = 10;
        p.ww = 5;
        assert_eq!(judge(&params, p), Decision::AcceptH0);
    }

    #[test]
    fn close_is_running() {
        let params = SprtParameters::new(0.0, 5.0, 0.05, 0.05).unwrap();
        let mut p = Penta::ZERO;
        // 互角に近い小サンプル
        p.ww = 3;
        p.wd = 2;
        p.wl = 4;
        p.dd = 3;
        p.dl = 2;
        p.ll = 3;
        assert_eq!(judge(&params, p), Decision::Running);
    }
}
