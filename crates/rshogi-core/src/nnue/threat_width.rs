//! Threat accumulator の格納幅。

#[cfg(any(feature = "threat-width-512", feature = "threat-width-768"))]
pub(crate) type ThreatAccumulation<const L1: usize> = [i16; width(0)];
#[cfg(not(any(feature = "threat-width-512", feature = "threat-width-768")))]
pub(crate) type ThreatAccumulation<const L1: usize> = [i16; L1];

pub(crate) const fn width(l1: usize) -> usize {
    if cfg!(feature = "threat-width-768") {
        768
    } else if cfg!(feature = "threat-width-512") {
        512
    } else {
        l1
    }
}

pub(crate) fn zero<const L1: usize>() -> ThreatAccumulation<L1> {
    [0; _]
}

#[inline]
pub(crate) fn value<const L1: usize>(threat: &ThreatAccumulation<L1>, column: usize) -> i16 {
    let half = L1 / 2;
    let local = column % half;
    let threat_half = threat.len().min(L1) / 2;
    if local < threat_half {
        threat[local + usize::from(column >= half) * threat_half]
    } else {
        0
    }
}
