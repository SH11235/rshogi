//! 探索スレッドの NUMA node への割り当てと、node ごとの NNUE 重みの写し。
//!
//! CPU が複数ソケットの機械では、全スレッドが 1 つの重みを読むと半分のスレッドが遠い
//! node のメモリを読む。`NumaPolicy=auto` では探索スレッドを node の CPU 集合に固定し、
//! 各 node に置いた重みの写しを読ませる。`none` ではこの module の状態を一切作らない。
//! 固定と topology の検出は Linux だけで行い、他の OS の `auto` は `none` と同じになる。

/// USI オプション `NumaPolicy` の値。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NumaPolicy {
    /// スレッドの固定も重みの写しも行わない。
    #[default]
    None,
    /// 使える CPU が 2 つ以上の NUMA node にまたがるときだけ、node ごとに固定して写しを持つ。
    Auto,
}

impl NumaPolicy {
    /// USI の値 (`none` / `auto`、大文字小文字は区別しない) から変換する。
    pub fn from_usi(value: &str) -> Option<Self> {
        if value.eq_ignore_ascii_case("none") {
            Some(Self::None)
        } else if value.eq_ignore_ascii_case("auto") {
            Some(Self::Auto)
        } else {
            None
        }
    }
}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub(crate) use linux::{
    AffinityGuard, NumaDetection, NumaNetwork, NumaPlan, NumaReplicas, NumaTopology, detect,
    pin_current_thread,
};
#[cfg(all(test, target_os = "linux", feature = "halfkx-arch"))]
pub(crate) use linux::{current_affinity, parse_cpulist};
