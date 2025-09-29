use crate::io::{info_string, usi_println};
use crate::state::{EngineState, ReaperJob};
use engine_core::search::constants::SEARCH_INF;
use engine_core::usi::{score_view_from_internal, ScoreView};
use log::warn;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

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

/// Join the search thread and emit diagnostics if it takes noticeable time.
pub fn join_search_handle(handle: thread::JoinHandle<()>, label: &str) {
    let start = Instant::now();
    match handle.join() {
        Ok(()) => {
            let elapsed = start.elapsed();
            if elapsed >= Duration::from_millis(20) {
                info_string(format!(
                    "worker_join label={} waited_ms={}",
                    label,
                    elapsed.as_millis()
                ));
            }
        }
        Err(err) => {
            warn!("search thread join failed label={} err={:?}", label, err);
            info_string(format!("worker_join_error label={} err={:?}", label, err));
        }
    }
}

const REAPER_QUEUE_SOFT_MAX: usize = 128;

/// Enqueue a search thread join to the background reaper. Falls back to direct join if unavailable.
pub fn enqueue_reaper(state: &EngineState, handle: thread::JoinHandle<()>, label: &str) {
    if let Some(tx) = &state.reaper_tx {
        let queued_len = state.reaper_queue_len.fetch_add(1, Ordering::SeqCst).saturating_add(1);
        if queued_len > REAPER_QUEUE_SOFT_MAX {
            info_string(format!("reaper_queue_len_high label={} len={}", label, queued_len));
        } else {
            info_string(format!("reaper_enqueue label={} len={}", label, queued_len));
        }
        let job = ReaperJob {
            handle,
            label: label.to_string(),
        };
        if let Err(send_err) = tx.send(job) {
            info_string(format!("reaper_enqueue_failed label={}", label));
            state.reaper_queue_len.fetch_sub(1, Ordering::SeqCst);
            let failed_job = send_err.0;
            join_search_handle(failed_job.handle, &failed_job.label);
        }
    } else {
        join_search_handle(handle, label);
    }
    state.notify_idle();
}
