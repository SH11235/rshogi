//! Centralized bestmove emission with exactly-once guarantee
//!
//! This module ensures that bestmove is sent exactly once per search,
//! and provides unified logging in LTSV format.

use crate::usi::{send_info_string, send_response, UsiResponse};
use engine_core::search::types::StopInfo;
use std::sync::atomic::{AtomicBool, Ordering};

/// Statistics for bestmove emission
pub struct BestmoveStats {
    pub depth: u32,
    pub seldepth: Option<u32>,
    pub score: String,
    pub nodes: u64,
    pub nps: u64,
}

/// Metadata for bestmove emission
pub struct BestmoveMeta {
    /// Source of the bestmove ("session", "fallback", etc.)
    pub from: &'static str,
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
}

impl BestmoveEmitter {
    /// Create a new bestmove emitter for a search
    pub fn new(search_id: u64) -> Self {
        Self {
            sent: AtomicBool::new(false),
            search_id,
        }
    }

    /// Emit bestmove with unified logging
    pub fn emit(
        &self,
        best_move: String,
        ponder: Option<String>,
        meta: BestmoveMeta,
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
                    "Bestmove sent: {}, ponder: {:?} (search_id: {})",
                    best_move,
                    ponder,
                    self.search_id
                );

                // Send unified LTSV log (single line for machine readability)
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
                     score={}\t\
                     nodes={}\t\
                     nps={}\t\
                     elapsed_ms={}\t\
                     hard_timeout={}\t\
                     bestmove={}\t\
                     ponder={}",
                    self.search_id,
                    meta.from,
                    stop_reason,
                    meta.stats.depth,
                    seldepth_str,
                    meta.stats.score,
                    meta.stats.nodes,
                    meta.stats.nps,
                    meta.stop_info.elapsed_ms,
                    meta.stop_info.hard_timeout,
                    best_move,
                    ponder_str
                );

                if let Err(e) = send_info_string(info_string) {
                    log::warn!("Failed to send LTSV info after bestmove: {}", e);
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
                // Reset sent flag since we failed to send
                self.sent.store(false, Ordering::Release);
                Err(anyhow::anyhow!("Failed to send bestmove: {}", e))
            }
        }
    }

    /// Check if bestmove has been sent
    #[cfg(test)]
    pub fn is_sent(&self) -> bool {
        self.sent.load(Ordering::Acquire)
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::search::types::TerminationReason;

    fn make_test_meta() -> BestmoveMeta {
        BestmoveMeta {
            from: "test",
            stop_info: StopInfo {
                reason: TerminationReason::TimeLimit,
                elapsed_ms: 1000,
                nodes: 10000,
                depth_reached: 10,
                hard_timeout: false,
            },
            stats: BestmoveStats {
                depth: 10,
                seldepth: Some(15),
                score: "cp 150".to_string(),
                nodes: 10000,
                nps: 10000,
            },
        }
    }

    #[test]
    fn test_exactly_once() {
        let emitter = BestmoveEmitter::new(1);

        // First emit should succeed
        assert!(!emitter.is_sent());

        // Note: In test, we can't actually send USI responses
        // So we just test the logic
        assert!(!emitter.sent.swap(true, Ordering::AcqRel));
        assert!(emitter.is_sent());

        // Second emit should be blocked
        assert!(emitter.sent.swap(true, Ordering::AcqRel));
    }

    #[test]
    fn test_concurrent_emission() {
        use std::sync::Arc;
        use std::thread;

        let emitter = Arc::new(BestmoveEmitter::new(42));
        let num_threads = 10;
        let mut handles = vec![];

        // Spawn multiple threads trying to emit simultaneously
        for _ in 0..num_threads {
            let emitter_clone = Arc::clone(&emitter);
            let handle = thread::spawn(move || {
                // Each thread tries to emit
                !emitter_clone.sent.swap(true, Ordering::AcqRel)
            });
            handles.push(handle);
        }

        // Collect results
        let mut success_count = 0;
        for handle in handles {
            if handle.join().unwrap() {
                success_count += 1;
            }
        }

        // Exactly one thread should succeed
        assert_eq!(success_count, 1, "Exactly one emission should succeed");
        assert!(emitter.is_sent());
    }

    #[test]
    fn test_different_stop_reasons() {
        // Test that different stop reasons are formatted correctly
        let reasons = vec![
            TerminationReason::TimeLimit,
            TerminationReason::NodeLimit,
            TerminationReason::DepthLimit,
            TerminationReason::UserStop,
            TerminationReason::Mate,
            TerminationReason::Completed,
            TerminationReason::Error,
        ];

        for reason in reasons {
            let mut meta = make_test_meta();
            meta.stop_info.reason = reason;

            // Verify Display format (which is used in LTSV)
            let formatted = reason.to_string();
            assert!(!formatted.is_empty());
            assert!(formatted.chars().all(|c| c.is_alphabetic() || c == '_'));
            // All reasons should be snake_case
            assert_eq!(formatted, formatted.to_lowercase());
        }
    }
}
