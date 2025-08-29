use anyhow::Result;

use crate::emit_utils::log_tsv;
use crate::types::ResignReason;
use crate::usi::{send_info_string, send_response, UsiResponse};

/// Log a position-restore failure and emit a resign bestmove.
///
/// This is a small, shared helper that does not depend on CommandContext.
pub(crate) fn resign_on_position_restore_fail(
    reason: ResignReason,
    log_reason: &str,
) -> Result<()> {
    // Distinct log kind to denote that we emit resign due to restore failure
    send_info_string(log_tsv(&[("kind", "position_restore_resign"), ("reason", log_reason)]))?;
    send_info_string(log_tsv(&[("kind", "resign"), ("resign_reason", &reason.to_string())]))?;
    send_response(UsiResponse::BestMove {
        best_move: "resign".to_string(),
        ponder: None,
    })?;
    Ok(())
}
