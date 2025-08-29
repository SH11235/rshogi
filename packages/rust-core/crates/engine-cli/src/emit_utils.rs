//! Utilities for emission metadata and structured logging

use crate::types::BestmoveSource;
use engine_core::search::types::{StopInfo, TerminationReason};

/// Create a TSV-formatted log string from key-value pairs
/// Values are sanitized to prevent TSV format corruption
pub fn log_tsv(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| {
            // Sanitize value by replacing tabs and newlines with spaces
            let sanitized = v.replace(['\t', '\n', '\r'], " ");
            format!("{k}={sanitized}")
        })
        .collect::<Vec<_>>()
        .join("\t")
}

/// Build BestmoveMeta from common parameters
/// This reduces duplication of BestmoveMeta construction across the codebase
pub fn build_meta(
    from: BestmoveSource,
    depth: u8,
    seldepth: Option<u8>,
    score: Option<String>,
    stop_info: Option<StopInfo>,
) -> crate::bestmove_emitter::BestmoveMeta {
    // Use provided stop_info or create default one
    let si = stop_info.unwrap_or(StopInfo {
        reason: match from {
            // Timeout cases -> TimeLimit
            BestmoveSource::ResignTimeout
            | BestmoveSource::EmergencyFallbackTimeout
            | BestmoveSource::PartialResultTimeout => TerminationReason::TimeLimit,
            // Normal completion cases -> Completed
            BestmoveSource::EmergencyFallback
            | BestmoveSource::EmergencyFallbackOnFinish
            | BestmoveSource::SessionInSearchFinished => TerminationReason::Completed,
            // User stop cases -> UserStop
            BestmoveSource::SessionOnStop => TerminationReason::UserStop,
            // Error cases -> Error
            BestmoveSource::Resign | BestmoveSource::ResignOnFinish => TerminationReason::Error,
        },
        elapsed_ms: 0, // Complement later if available
        nodes: 0,      // Complement later if available
        depth_reached: depth,
        hard_timeout: matches!(
            from,
            BestmoveSource::EmergencyFallbackTimeout
                | BestmoveSource::PartialResultTimeout
                | BestmoveSource::ResignTimeout
        ),
        soft_limit_ms: 0,
        hard_limit_ms: 0,
    });

    let nodes = si.nodes;
    let elapsed_ms = si.elapsed_ms;

    crate::bestmove_emitter::BestmoveMeta {
        from,
        stop_info: si,
        stats: crate::bestmove_emitter::BestmoveStats {
            depth,
            seldepth,
            score: score.unwrap_or_else(|| "none".to_string()),
            nodes,
            nps: if elapsed_ms > 0 {
                nodes.saturating_mul(1000) / elapsed_ms
            } else {
                0
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_tsv_sanitizes() {
        let s = log_tsv(&[("k1", "a\tb\nc"), ("k2", "v2")]);
        assert_eq!(s, "k1=a b c\tk2=v2");
    }
}
