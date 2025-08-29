use crate::engine::controller::Engine;
use crate::search::SearchLimits;
use crate::shogi::{Move, Position};

/// Run a shallow, time-bounded search and return the best Move if any.
///
/// This helper is intended for quick fallback decisions (e.g., GUI compatibility paths),
/// not for main search strength. It does not modify the original position.
pub fn quick_search_move(
    engine: &mut Engine,
    position: &Position,
    depth: u8,
    time_ms: u64,
) -> Option<Move> {
    // Clone position to avoid mutating caller state
    let mut pos = position.clone();
    let limits = SearchLimits::builder().depth(depth).fixed_time_ms(time_ms).build();
    let result = engine.search(&mut pos, limits);
    result.best_move
}
