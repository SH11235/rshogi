//! Centralized bestmove emission with exactly-once guarantee
//!
//! This module ensures that bestmove emission is attempted exactly once per search.
//! If the emission fails, the sent flag remains true to prevent double emission.
//! Callers must create a new BestmoveEmitter for retry with different content.
//! This design prevents accidental duplicate bestmoves to the GUI.

use crate::types::BestmoveSource;
use crate::usi::{send_info_string, send_response, UsiResponse};
use engine_core::search::types::StopInfo;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

#[cfg(test)]
use once_cell::sync::Lazy;
#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::Mutex;

#[cfg(test)]
/// Test-only tracking of last emitted BestmoveSource by search_id
static LAST_EMIT_SOURCE_BY_ID: Lazy<Mutex<HashMap<u64, BestmoveSource>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

#[cfg(test)]
/// Get the last BestmoveSource for a specific search_id
pub fn last_source_for(search_id: u64) -> Option<BestmoveSource> {
    LAST_EMIT_SOURCE_BY_ID.lock().ok()?.get(&search_id).copied()
}

#[cfg(test)]
/// Clear the last BestmoveSource for a specific search_id
pub fn clear_last_source_for(search_id: u64) {
    if let Ok(mut map) = LAST_EMIT_SOURCE_BY_ID.lock() {
        map.remove(&search_id);
    }
}

#[cfg(test)]
/// Clear all tracked BestmoveSources (for test isolation)
pub fn clear_all_last_sources() {
    if let Ok(mut map) = LAST_EMIT_SOURCE_BY_ID.lock() {
        map.clear();
    }
}

/// Statistics for bestmove emission
#[derive(Debug, Clone)]
pub struct BestmoveStats {
    pub depth: u8,
    pub seldepth: Option<u8>,
    pub score: String,
    pub nodes: u64,
    pub nps: u64,
}

/// Metadata for bestmove emission
#[derive(Debug, Clone)]
pub struct BestmoveMeta {
    /// Source of the bestmove
    pub from: BestmoveSource,
    /// Stop information (required)
    pub stop_info: StopInfo,
    /// Search statistics
    pub stats: BestmoveStats,
}

/// Bestmove emitter with exactly-once guarantee
pub struct BestmoveEmitter {
    /// Flag to ensure exactly-once emission
    sent: AtomicBool,
    /// Search ID for this emitter
    search_id: u64,
    /// Search start time for elapsed calculation
    start_time: Instant,
    /// Test-only flag to force emit() to return an error
    #[cfg(test)]
    force_error: bool,
}

impl BestmoveEmitter {
    /// Create a new bestmove emitter for a search
    pub fn new(search_id: u64) -> Self {
        Self {
            sent: AtomicBool::new(false),
            search_id,
            start_time: Instant::now(),
            #[cfg(test)]
            force_error: false,
        }
    }

    /// Create a new bestmove emitter that will force an error on emit()
    #[cfg(test)]
    pub fn new_with_error(search_id: u64) -> Self {
        Self {
            sent: AtomicBool::new(false),
            search_id,
            start_time: Instant::now(),
            force_error: true,
        }
    }

    /// Emit bestmove with unified logging
    pub fn emit(
        &self,
        best_move: String,
        ponder: Option<String>,
        mut meta: BestmoveMeta,
    ) -> anyhow::Result<()> {
        // Ensure exactly-once emission
        if self.sent.swap(true, Ordering::AcqRel) {
            log::debug!(
                "Bestmove already sent for search {}, ignoring: {}",
                self.search_id,
                best_move
            );
            return Ok(());
        }

        // Test-only: track the BestmoveSource by search_id
        // This is done AFTER the sent flag check to ensure we only track actually sent moves
        #[cfg(test)]
        {
            if let Ok(mut map) = LAST_EMIT_SOURCE_BY_ID.lock() {
                map.insert(self.search_id, meta.from);
            }
        }

        // Test-only: force error if requested
        #[cfg(test)]
        if self.force_error {
            return Err(anyhow::anyhow!("Test error: forced emit failure"));
        }

        // Complement elapsed_ms and nps if needed
        let actual_elapsed = self.start_time.elapsed();
        let actual_elapsed_ms = actual_elapsed.as_millis() as u64;

        // If elapsed_ms is 0, use actual elapsed time
        if meta.stop_info.elapsed_ms == 0 && actual_elapsed_ms > 0 {
            meta.stop_info.elapsed_ms = actual_elapsed_ms;
        }

        // Recalculate NPS if it's 0 and we have valid data
        if meta.stats.nps == 0 && meta.stop_info.elapsed_ms > 0 && meta.stats.nodes > 0 {
            meta.stats.nps = meta.stats.nodes.saturating_mul(1000) / meta.stop_info.elapsed_ms;
        }

        // Log null move usage for debugging
        if best_move.trim() == "0000" {
            let _ = send_info_string(
                "using null move (0000) - position may be invalid or no legal moves.",
            );
        }

        // Send USI bestmove response
        let result = send_response(UsiResponse::BestMove {
            best_move: best_move.clone(),
            ponder: ponder.clone(),
        });

        // Handle send result
        match result {
            Ok(()) => {
                // Log after successful sending
                log::info!(
                    "[BESTMOVE] sent: {} ponder={:?} (search_id={}, depth={}, nps={}, flush=immediate)",
                    best_move,
                    ponder,
                    self.search_id,
                    meta.stats.depth,
                    meta.stats.nps
                );

                // Debug-only observability info
                #[cfg(debug_assertions)]
                {
                    let _ = send_info_string(format!(
                        "Emitter: search_id={} from={}",
                        self.search_id, meta.from
                    ));
                }

                // Send unified tab-separated key=value log (single line for machine readability)
                // Note: The score field contains spaces (e.g., "cp 150", "mate 7") following USI protocol format.
                // External parsers should use tab as the delimiter, not spaces.
                let stop_reason = meta.stop_info.reason.to_string();
                let ponder_str = ponder.as_deref().unwrap_or("none");

                let seldepth_str =
                    meta.stats.seldepth.map(|v| v.to_string()).unwrap_or_else(|| "-".to_string());

                let info_string = format!(
                    "kind=bestmove_sent\t\
                     search_id={}\t\
                     bestmove_from={}\t\
                     stop_reason={}\t\
                     depth={}\t\
                     seldepth={}\t\
                     depth_reached={}\t\
                     score={}\t\
                     nodes={}\t\
                     nps={}\t\
                     elapsed_ms={}\t\
                     time_soft_ms={}\t\
                     time_hard_ms={}\t\
                     hard_timeout={}\t\
                     bestmove={}\t\
                     ponder={}",
                    self.search_id,
                    meta.from,
                    stop_reason,
                    meta.stats.depth,
                    seldepth_str,
                    meta.stop_info.depth_reached,
                    meta.stats.score,
                    meta.stats.nodes,
                    meta.stats.nps,
                    meta.stop_info.elapsed_ms,
                    meta.stop_info.soft_limit_ms,
                    meta.stop_info.hard_limit_ms,
                    meta.stop_info.hard_timeout,
                    best_move,
                    ponder_str
                );

                if let Err(e) = send_info_string(info_string) {
                    log::warn!("Failed to send tab-separated info after bestmove: {}", e);
                }
                Ok(())
            }
            Err(e) => {
                // Log error if send failed
                log::error!(
                    "Failed to send bestmove: {} (search_id: {}, error: {})",
                    best_move,
                    self.search_id,
                    e
                );
                // Note: We keep sent=true to maintain exactly-once guarantee.
                // If the caller wants to retry with a different bestmove,
                // they should create a new BestmoveEmitter instance.
                Err(anyhow::anyhow!("Failed to send bestmove: {}", e))
            }
        }
    }

    /// Set start time
    pub fn set_start_time(&mut self, start_time: Instant) {
        self.start_time = start_time;
    }

    /// Check if bestmove has been sent
    #[cfg(test)]
    pub fn is_sent(&self) -> bool {
        self.sent.load(Ordering::Acquire)
    }

    /// Set start time for testing
    #[cfg(test)]
    pub fn set_start_time_for_test(&mut self, start_time: Instant) {
        self.start_time = start_time;
    }
}
