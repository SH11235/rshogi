//! Utilities for emission metadata and structured logging

use crate::types::BestmoveSource;
use crate::usi::send_info_string;
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
            BestmoveSource::EmergencyFallbackTimeout | BestmoveSource::PartialResultTimeout => {
                TerminationReason::TimeLimit
            }
            // Normal completion cases -> Completed
            BestmoveSource::EmergencyFallbackOnFinish | BestmoveSource::CoreFinalize => {
                TerminationReason::Completed
            }
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
            BestmoveSource::EmergencyFallbackTimeout | BestmoveSource::PartialResultTimeout
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

/// Log helpers for position restore flow
pub fn log_position_restore_try(move_len: usize, age_ms: u128) {
    let _ = send_info_string(log_tsv(&[
        ("kind", "position_restore_try"),
        ("move_len", &move_len.to_string()),
        ("age_ms", &age_ms.to_string()),
    ]));
}

pub fn log_position_restore_success(source: &str) {
    let _ = send_info_string(log_tsv(&[("kind", "position_restore_success"), ("source", source)]));
}

pub fn log_position_restore_fallback(reason: &str) {
    let _ = send_info_string(log_tsv(&[("kind", "position_restore_fallback"), ("reason", reason)]));
}

pub fn log_position_restore_resign(reason: &str, expected: Option<&str>, actual: Option<&str>) {
    let mut pairs = vec![("kind", "position_restore_resign"), ("reason", reason)];
    if let Some(exp) = expected {
        pairs.push(("expected", exp));
    }
    if let Some(act) = actual {
        pairs.push(("actual", act));
    }
    let _ = send_info_string(log_tsv(&pairs));
}

/// Log a unified on_stop_source entry
pub fn log_on_stop_source(src: &str) {
    let _ = send_info_string(log_tsv(&[("kind", "on_stop_source"), ("src", src)]));
}

/// Log unified on_stop diagnostic snapshot
///
/// This standardizes the first-line dump on stop handling for race analysis.
pub fn log_on_stop_snapshot(
    state: &str,
    ponder: bool,
    has_session: bool,
    has_partial: bool,
    has_pre_session: bool,
) {
    let _ = send_info_string(log_tsv(&[
        ("kind", "on_stop"),
        ("state", state),
        ("ponder", if ponder { "1" } else { "0" }),
        ("session", if has_session { "1" } else { "0" }),
        ("partial", if has_partial { "1" } else { "0" }),
        ("pre_session_fallback", if has_pre_session { "1" } else { "0" }),
    ]));
}

/// Log position store snapshot
pub fn log_position_store(root_hash: u64, move_len: usize, sfen_snapshot: &str, stored_ms: u128) {
    let _ = send_info_string(log_tsv(&[
        ("kind", "position_store"),
        ("root_hash", &format!("{:#016x}", root_hash)),
        ("move_len", &move_len.to_string()),
        ("sfen_first_20", &sfen_snapshot.chars().take(20).collect::<String>()),
        ("stored_ms_since_start", &stored_ms.to_string()),
    ]));
}

/// Log go_received with optional pre_session_fallback
pub fn log_go_received(ponder: bool, pre_session_fallback: Option<&str>) {
    let ponder_str = if ponder { "1" } else { "0" };
    let fallback = pre_session_fallback.unwrap_or("none");
    let _ = send_info_string(log_tsv(&[
        ("kind", "go_received"),
        ("ponder", ponder_str),
        ("pre_session_fallback", fallback),
    ]));
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
