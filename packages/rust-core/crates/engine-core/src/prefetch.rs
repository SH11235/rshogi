use crate::types::Color;

pub(crate) trait TtPrefetch {
    fn prefetch(&self, key: u64, side_to_move: Color);
}

pub(crate) struct NoPrefetch;

impl TtPrefetch for NoPrefetch {
    #[inline]
    fn prefetch(&self, _key: u64, _side_to_move: Color) {}
}
