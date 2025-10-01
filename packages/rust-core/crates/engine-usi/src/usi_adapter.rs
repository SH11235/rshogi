use std::time::Duration;

use engine_core::search::types::NodeType;
use engine_core::shogi::Move;
use engine_core::usi::{append_usi_score_and_bound, move_to_usi, score_view_from_internal};

/// Emit a USI-compliant info line for a PV update (legacy bridge).
///
/// This is an adapter over the legacy core callback signature so that
/// engine-usi prints consistently formatted info lines while we migrate
/// to event-driven InfoEvent.
pub fn emit_pv_line(
    depth: u8,
    score_internal: i32,
    nodes: u64,
    elapsed: Duration,
    pv: &[Move],
    bound: NodeType,
    multipv_enabled: bool,
) {
    let elapsed_ms = elapsed.as_millis() as u64;
    let denom = elapsed_ms.max(1);
    let nps = nodes.saturating_mul(1000) / denom;

    let mut line = format!("info depth {} time {} nodes {} nps {}", depth, elapsed_ms, nodes, nps);

    if multipv_enabled {
        line.push_str(" multipv 1");
    }

    let score_view = score_view_from_internal(score_internal);
    append_usi_score_and_bound(&mut line, score_view, bound);

    if !pv.is_empty() {
        line.push_str(" pv ");
        line.push_str(&pv.iter().map(move_to_usi).collect::<Vec<_>>().join(" "));
    }

    println!("{}", line);
}
