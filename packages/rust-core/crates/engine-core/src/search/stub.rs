use crate::movegen::MoveGenerator;
use crate::search::{SearchLimits, SearchResult};
use crate::shogi::Move;
use crate::Position;
use std::time::Instant;

/// Very small, deterministic stub searcher used during Phase 0 migration.
/// It returns a single legal move (prefer a non-king move if available),
/// or `None` when no legal moves exist (resign).
pub fn run_stub_search(pos: &Position, _limits: &SearchLimits) -> SearchResult {
    let start = Instant::now();
    let mg = MoveGenerator::new();
    let best: Option<Move> =
        mg.generate_all(pos).ok().and_then(|list| list.as_slice().first().copied());

    let elapsed = start.elapsed();
    SearchResult::from_legacy((best, 0), 0, elapsed, Vec::new(), 1)
}
