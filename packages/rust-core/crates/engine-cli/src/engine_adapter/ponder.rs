//! Ponder functionality for the engine adapter.
//!
//! This module handles ponder (thinking on opponent's time) operations,
//! including ponder hit handling and ponder state management.

use anyhow::Result;
use log::{debug, info};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::engine_adapter::EngineAdapter;

impl EngineAdapter {
    /// Handle ponder hit (opponent played the expected move)
    pub fn ponder_hit(&mut self) -> Result<()> {
        if let Some(ref flag) = self.active_ponder_hit_flag {
            info!("Ponder hit: Setting flag at {:p} to true", Arc::as_ptr(flag));
            flag.store(true, Ordering::Release);

            // Clear ponder state since we're transitioning to normal search
            self.ponder_state.is_pondering = false;

            // Don't stop the search - let it continue as normal search after ponderhit
            // The SearchContext::process_events() will detect the ponder_hit_flag and
            // convert from ponder to normal search mode internally
            info!("Ponder hit: Converting ponder search to normal search (search continues)");
            Ok(())
        } else {
            debug!("Ponder hit called but no active ponder flag");
            Ok(())
        }
    }
}
