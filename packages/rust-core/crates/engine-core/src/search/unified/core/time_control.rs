//! Time control utilities for search
//!
//! Manages event polling intervals based on time control settings

use crate::{
    evaluation::evaluate::Evaluator, search::unified::UnifiedSearcher, time_management::TimeControl,
};

// Polling masks grouped for readability (all are (2^k - 1))
mod masks {
    pub(super) const EVERY_NODE: u64 = 0x0; // every node
    pub(super) const N8_MASK: u64 = 0x7; // 8 nodes
    pub(super) const N16_MASK: u64 = 0xF; // 16 nodes
    pub(super) const N32_MASK: u64 = 0x1F; // 32 nodes
    pub(super) const N64_MASK: u64 = 0x3F; // 64 nodes
    pub(super) const N128_MASK: u64 = 0x7F; // 128 nodes
    pub(super) const N256_MASK: u64 = 0xFF; // 256 nodes
    pub(super) const N1024_MASK: u64 = 0x3FF; // 1024 nodes
}
use masks::*;

pub(super) const NEAR_DEADLINE_MASK: u64 = N32_MASK;

#[inline(always)]
fn is_ponder_like<E, const USE_TT: bool, const USE_PRUNING: bool>(
    searcher: &UnifiedSearcher<E, USE_TT, USE_PRUNING>,
) -> bool
where
    E: Evaluator + Send + Sync + 'static,
{
    // Explicit ponder mode signaled via limits
    if matches!(&searcher.context.limits().time_control, TimeControl::Ponder(_)) {
        return true;
    }
    // TimeManager with infinite/ponder-like soft limit (u64::MAX)
    if let Some(tm) = &searcher.time_manager {
        if tm.soft_limit_ms() == u64::MAX {
            return true;
        }
    }
    false
}

/// Get event polling mask based on time limit and stop conditions
///
/// Returns a bitmask that determines how frequently to check for events (time limit, stop flag, etc).
/// Lower values mean more frequent checks:
/// - 0x0 (0): Check every node (immediate response when already stopped)
/// - 0x1F (31): Check every 32 nodes (responsive stop handling or Byoyomi)
/// - 0x3F (63): Check every 64 nodes (fixed nodes, ponder mode, or stop_flag present)
/// - 0x7F-0x3FF: Check every 128-1024 nodes (time-based controls)
///
/// This unified mask handles all event checking including stop_flag polling,
/// eliminating the need for separate stop_check_interval logic.
#[inline(always)]
pub fn get_event_poll_mask<E, const USE_TT: bool, const USE_PRUNING: bool>(
    searcher: &UnifiedSearcher<E, USE_TT, USE_PRUNING>,
) -> u64
where
    E: Evaluator + Send + Sync + 'static,
{
    // If already stopped, check every node for immediate exit
    if searcher.context.should_stop() || searcher.context.was_time_stopped() {
        return EVERY_NODE; // immediate response
    }

    // Base mask derived from soft limit (or default when TimeManager is absent)
    let mut mask = if let Some(tm) = &searcher.time_manager {
        match tm.soft_limit_ms() {
            0..=50 => N32_MASK,
            51..=100 => N64_MASK,
            101..=200 => N128_MASK,
            201..=500 => N256_MASK,
            _ => N1024_MASK,
        }
    } else {
        N128_MASK
    };

    if let Some(tm) = &searcher.time_manager {
        // Byoyomi-specific tightening
        if let TimeControl::Byoyomi { byoyomi_ms, .. } = tm.time_control() {
            let byoyomi_mask =
                if let Some((_, current_period_ms, in_byoyomi)) = tm.get_byoyomi_state() {
                    if in_byoyomi && byoyomi_ms > 0 {
                        let ratio = current_period_ms as f64 / byoyomi_ms as f64;
                        if ratio < 0.25 {
                            N8_MASK
                        } else {
                            N32_MASK
                        }
                    } else {
                        N32_MASK
                    }
                } else {
                    N32_MASK
                };
            mask = mask.min(byoyomi_mask);
        }

        // Hard limit proximity tightening
        let hard = tm.hard_limit_ms();
        if hard > 0 && hard < u64::MAX {
            let elapsed_ms = tm.elapsed_ms();
            if elapsed_ms >= hard {
                mask = EVERY_NODE;
            } else {
                let remain = hard - elapsed_ms;
                let nd2 = tm.network_delay2_ms();
                if nd2 > 0 && remain <= nd2.saturating_mul(2) {
                    mask = EVERY_NODE;
                }
                let near_mask = match remain {
                    0..=50 => Some(EVERY_NODE),
                    51..=100 => Some(N8_MASK),
                    101..=150 => Some(N16_MASK),
                    151..=300 => Some(N32_MASK),
                    301..=500 => Some(N64_MASK),
                    _ => None,
                };
                if let Some(candidate) = near_mask {
                    mask = mask.min(candidate);
                }
            }
        }
    }

    // Fixed-nodes search enforces at least N64 polling
    if let TimeControl::FixedNodes { .. } = &searcher.context.limits().time_control {
        mask = mask.min(N64_MASK);
    }

    // Ponder (or soft-limit == u64::MAX) should never poll less frequently than every 64 nodes
    if is_ponder_like(searcher) {
        mask = mask.min(N64_MASK);
    }

    // External stop flag wiring guarantees responsiveness; treat as upper bound N64
    if searcher.context.limits().stop_flag.is_some() {
        mask = mask.min(N64_MASK);
    }

    mask
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        evaluation::evaluate::MaterialEvaluator,
        search::{unified::UnifiedSearcher, SearchLimitsBuilder},
    };
    use std::sync::{atomic::AtomicBool, Arc};

    #[test]
    fn test_mask_values_are_correct() {
        // Verify masks are of the form (2^k - 1)
        assert_eq!(EVERY_NODE, 0);
        assert_eq!(N8_MASK, 7);
        assert_eq!(N16_MASK, 15);
        assert_eq!(N32_MASK, 31);
        assert_eq!(N64_MASK, 63);
        assert_eq!(N128_MASK, 127);
        assert_eq!(N256_MASK, 255);
        assert_eq!(N1024_MASK, 1023);
    }

    #[test]
    fn test_stopped_returns_every_node() {
        let evaluator = MaterialEvaluator;
        let mut searcher = UnifiedSearcher::<_, true, false>::new(evaluator);

        // Set up a search that's already stopped
        let stop_flag = Arc::new(AtomicBool::new(true));
        let limits = SearchLimitsBuilder::default().stop_flag(stop_flag).build();
        searcher.context.set_limits(limits);

        assert_eq!(get_event_poll_mask(&searcher), EVERY_NODE);
    }

    #[test]
    fn test_stop_flag_returns_n64() {
        let evaluator = MaterialEvaluator;
        let mut searcher = UnifiedSearcher::<_, true, false>::new(evaluator);

        // Set up with stop_flag but not stopped
        let stop_flag = Arc::new(AtomicBool::new(false));
        let limits = SearchLimitsBuilder::default().stop_flag(stop_flag).build();
        searcher.context.set_limits(limits);

        assert_eq!(get_event_poll_mask(&searcher), N64_MASK);
    }

    #[test]
    fn test_fixed_nodes_returns_n64() {
        let evaluator = MaterialEvaluator;
        let mut searcher = UnifiedSearcher::<_, true, false>::new(evaluator);

        let limits = SearchLimitsBuilder::default().fixed_nodes(10000).build();
        searcher.context.set_limits(limits);

        assert_eq!(get_event_poll_mask(&searcher), N64_MASK);
    }

    #[test]
    fn test_ponder_returns_n64() {
        let evaluator = MaterialEvaluator;
        let mut searcher = UnifiedSearcher::<_, true, false>::new(evaluator);

        // Create a ponder search - first set up a base time control, then convert to ponder
        let limits = SearchLimitsBuilder::default().fixed_time_ms(1000).ponder_with_inner().build();
        searcher.context.set_limits(limits);

        assert_eq!(get_event_poll_mask(&searcher), N64_MASK);
    }

    #[test]
    fn test_byoyomi_returns_n32() {
        let evaluator = MaterialEvaluator;
        let mut searcher = UnifiedSearcher::<_, true, false>::new(evaluator);

        // Set up byoyomi time control
        let limits = SearchLimitsBuilder::default()
            .byoyomi(1000, 500, 1) // main_time_ms, byoyomi_ms, periods
            .build();
        searcher.context.set_limits(limits.clone());

        // Create time manager to simulate byoyomi
        let time_manager = Arc::new(crate::time_management::TimeManager::new(
            &limits.clone().into(),
            crate::shogi::Color::Black,
            0,
            crate::time_management::GamePhase::Opening,
        ));
        time_manager.override_limits_for_test(2000, u64::MAX);
        searcher.time_manager = Some(time_manager);

        assert_eq!(get_event_poll_mask(&searcher), N32_MASK);
    }

    #[test]
    fn test_is_ponder_like_explicit_ponder() {
        let evaluator = MaterialEvaluator;
        let mut searcher = UnifiedSearcher::<_, true, false>::new(evaluator);
        // base tc then wrap ponder with inner to preserve tc after hit
        let limits = SearchLimitsBuilder::default().fixed_time_ms(1000).ponder_with_inner().build();
        searcher.context.set_limits(limits);
        assert!(super::is_ponder_like(&searcher));
        assert_eq!(get_event_poll_mask(&searcher), N64_MASK);
    }

    #[test]
    fn test_is_ponder_like_soft_limit_max() {
        let evaluator = MaterialEvaluator;
        let mut searcher = UnifiedSearcher::<_, true, false>::new(evaluator);
        // Any non-ponder limits
        let limits = SearchLimitsBuilder::default().fixed_time_ms(5000).build();
        searcher.context.set_limits(limits.clone());

        // TimeManager with Infinite -> soft_limit_ms == u64::MAX
        let infinite_limits = SearchLimitsBuilder::default().infinite().build();
        let tm = Arc::new(crate::time_management::TimeManager::new(
            &infinite_limits.clone().into(),
            crate::shogi::Color::Black,
            0,
            crate::time_management::GamePhase::Opening,
        ));
        searcher.time_manager = Some(tm);

        assert!(super::is_ponder_like(&searcher));
        assert_eq!(get_event_poll_mask(&searcher), N64_MASK);
    }

    #[test]
    fn test_soft_limit_thresholds() {
        // Test various soft limit values
        let test_cases = vec![
            (25, N32_MASK),     // 0..=50
            (50, N32_MASK),     // 0..=50
            (51, N64_MASK),     // 51..=100
            (100, N64_MASK),    // 51..=100
            (101, N128_MASK),   // 101..=200
            (200, N128_MASK),   // 101..=200
            (201, N256_MASK),   // 201..=500
            (500, N256_MASK),   // 201..=500
            (501, N1024_MASK),  // default
            (1000, N1024_MASK), // default
        ];

        for (soft_limit_ms, expected_mask) in test_cases {
            let mut searcher = UnifiedSearcher::<_, true, false>::new(MaterialEvaluator);

            let limits = SearchLimitsBuilder::default().fixed_time_ms(1000).build();
            searcher.context.set_limits(limits.clone());

            let time_manager = Arc::new(crate::time_management::TimeManager::new(
                &limits.clone().into(),
                crate::shogi::Color::Black,
                0,
                crate::time_management::GamePhase::Opening,
            ));
            time_manager.override_limits_for_test(soft_limit_ms, u64::MAX);
            searcher.time_manager = Some(time_manager);

            let mask = get_event_poll_mask(&searcher);
            assert_eq!(mask, expected_mask, "Failed for soft_limit_ms={soft_limit_ms}");
        }
    }

    #[test]
    fn test_no_time_manager_returns_n128() {
        let evaluator = MaterialEvaluator;
        let mut searcher = UnifiedSearcher::<_, true, false>::new(evaluator);

        // Depth-only search (no time manager)
        let limits = SearchLimitsBuilder::default().depth(10).build();
        searcher.context.set_limits(limits);

        assert_eq!(get_event_poll_mask(&searcher), N128_MASK);
    }
}
