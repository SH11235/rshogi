//! Threat accumulator の格納幅。

#[cfg(feature = "threat-width-512")]
pub(crate) type ThreatAccumulation<const L1: usize> = [i16; 512];
#[cfg(not(feature = "threat-width-512"))]
pub(crate) type ThreatAccumulation<const L1: usize> = [i16; L1];

pub(crate) const fn width(l1: usize) -> usize {
    if cfg!(feature = "threat-width-512") {
        512
    } else {
        l1
    }
}

pub(crate) fn zero<const L1: usize>() -> ThreatAccumulation<L1> {
    #[cfg(feature = "threat-width-512")]
    {
        [0; 512]
    }
    #[cfg(not(feature = "threat-width-512"))]
    {
        [0; L1]
    }
}

#[inline]
pub(crate) fn value<const L1: usize>(threat: &ThreatAccumulation<L1>, column: usize) -> i16 {
    let half = L1 / 2;
    let local = column % half;
    if local < threat.len().min(L1) / 2 {
        threat[local + usize::from(column >= half) * (threat.len().min(L1) / 2)]
    } else {
        0
    }
}
