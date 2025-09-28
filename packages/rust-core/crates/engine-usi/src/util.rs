use crate::io::usi_println;
use engine_core::search::constants::SEARCH_INF;
use engine_core::usi::{score_view_from_internal, ScoreView};

/// Clamp internal score to USI-friendly bounds while preserving mate information.
pub fn sanitize_score_view(view: ScoreView) -> ScoreView {
    match view {
        ScoreView::Cp(cp) if cp <= -(SEARCH_INF - 1) => ScoreView::Cp(-29_999),
        ScoreView::Cp(cp) if cp >= SEARCH_INF - 1 => ScoreView::Cp(29_999),
        other => other,
    }
}

/// Convert engine internal score to a sanitized ScoreView for output.
pub fn score_view_with_clamp(raw_score: i32) -> ScoreView {
    sanitize_score_view(score_view_from_internal(raw_score))
}

/// Emit bestmove (and optional ponder) using standard USI formatting.
pub fn emit_bestmove(final_usi: &str, ponder: Option<String>) {
    if let Some(p) = ponder {
        usi_println(&format!("bestmove {} ponder {}", final_usi, p));
    } else {
        usi_println(&format!("bestmove {}", final_usi));
    }
}
