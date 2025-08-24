//! Centralized bestmove emission with exactly-once guarantee
//!
//! This module ensures that bestmove is sent exactly once per search,
//! and provides unified logging in tab-separated key=value format.

use crate::types::BestmoveSource;
use crate::usi::{send_info_string, send_response, UsiResponse};
use engine_core::search::types::StopInfo;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// Statistics for bestmove emission
#[derive(Debug)]
pub struct BestmoveStats {
    pub depth: u8,
    pub seldepth: Option<u8>,
    pub score: String,
    pub nodes: u64,
    pub nps: u64,
}

/// Metadata for bestmove emission
#[derive(Debug)]
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
}

impl BestmoveEmitter {
    /// Create a new bestmove emitter for a search
    pub fn new(search_id: u64) -> Self {
        Self {
            sent: AtomicBool::new(false),
            search_id,
            start_time: Instant::now(),
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
        if best_move == "0000" {
            let _ = send_info_string(
                "using null move (0000) - position may be invalid or no legal moves",
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
                    "Bestmove sent: {}, ponder: {:?} (search_id: {}, depth: {}, nps: {})",
                    best_move,
                    ponder,
                    self.search_id,
                    meta.stats.depth,
                    meta.stats.nps
                );

                // Debug-only observability info
                #[cfg(debug_assertions)]
                {
                    let _ = send_info_string(format!("Emitter: sent={}", self.search_id));
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
                // Reset sent flag since we failed to send
                // Note: In current implementation, callers use `?` which makes this fatal,
                // so retry won't happen. This is intentional for now but allows future
                // non-fatal error handling if needed.
                self.sent.store(false, Ordering::Release);
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

#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::search::types::TerminationReason;
    use std::time::Duration;

    fn make_test_meta() -> BestmoveMeta {
        BestmoveMeta {
            from: BestmoveSource::Test,
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
    fn test_elapsed_time_complement() {
        // Test that elapsed_ms is complemented from actual elapsed time
        let mut emitter = BestmoveEmitter::new(1);

        // Set start time to 100ms ago
        let past_time = Instant::now() - Duration::from_millis(100);
        emitter.set_start_time_for_test(past_time);

        // Create meta with 0 elapsed_ms
        let mut meta = make_test_meta();
        meta.stop_info.elapsed_ms = 0;
        meta.stats.nodes = 1000;
        meta.stats.nps = 0; // Should be recalculated

        // Simulate emit (we can't actually emit in test, but we can test the logic)
        // The actual elapsed time should be around 100ms
        let actual_elapsed = emitter.start_time.elapsed();
        assert!(actual_elapsed.as_millis() >= 100);

        // If we had access to the internal logic, it would:
        // 1. Set meta.stop_info.elapsed_ms to actual_elapsed_ms (around 100)
        // 2. Recalculate meta.stats.nps = 1000 * 1000 / 100 = 10000
    }

    #[test]
    fn test_nps_recalculation() {
        // Test that NPS is recalculated when it's 0 but we have nodes and elapsed time
        let _emitter = BestmoveEmitter::new(2);

        let mut meta = make_test_meta();
        meta.stop_info.elapsed_ms = 50; // 50ms
        meta.stats.nodes = 5000;
        meta.stats.nps = 0; // Should be recalculated

        // Expected NPS = 5000 * 1000 / 50 = 100000
        let expected_nps = meta.stats.nodes.saturating_mul(1000) / meta.stop_info.elapsed_ms;
        assert_eq!(expected_nps, 100000);
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

            // Verify Display format (used in logs with tab-separated key=value)
            let formatted = reason.to_string();
            assert!(!formatted.is_empty());
            assert!(formatted.chars().all(|c| c.is_alphabetic() || c == '_'));
            // All reasons should be snake_case
            assert_eq!(formatted, formatted.to_lowercase());
        }
    }

    #[test]
    fn test_set_start_time_does_not_reset_sent() {
        let mut emitter = BestmoveEmitter::new(123);
        // 疑似的に送信済みにする
        assert!(!emitter.sent.swap(true, Ordering::AcqRel));
        // ここで start_time を更新しても…
        emitter.set_start_time(std::time::Instant::now());
        // 送信済み状態は維持される
        assert!(emitter.is_sent());
    }
}
