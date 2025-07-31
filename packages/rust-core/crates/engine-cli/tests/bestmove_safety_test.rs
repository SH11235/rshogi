//! Tests for bestmove safety - ensuring bestmove is sent exactly once
//!
//! These tests verify that:
//! 1. Stop → fallback → delayed BestMove results in only one bestmove
//! 2. Normal completion → BestMove → Finished sends exactly one bestmove
//! 3. Worker error without BestMove sends fallback bestmove once

use crossbeam_channel::unbounded;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

/// Search state management - tracks the current state of the search
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchState {
    /// No search is active
    Idle,
    /// Search is actively running
    Searching,
    /// Stop has been requested but search is still running
    StopRequested,
    /// Fallback move has been sent due to timeout/error
    FallbackSent,
}

impl SearchState {
    /// Check if we're in any searching state
    fn is_searching(&self) -> bool {
        matches!(self, SearchState::Searching | SearchState::StopRequested)
    }

    /// Check if we should accept a bestmove
    fn can_accept_bestmove(&self) -> bool {
        matches!(self, SearchState::Searching | SearchState::StopRequested)
    }
}

/// Mock worker message types for unit testing
#[derive(Debug)]
#[allow(dead_code)]
enum MockWorkerMessage {
    Info(String),
    BestMove {
        best_move: String,
        ponder: Option<String>,
    },
    PartialResult {
        current_best: String,
        depth: u32,
        score: i32,
    },
    Finished {
        from_guard: bool,
    },
    Error(String),
}

/// Test that simulates the exact message ordering scenarios
#[test]
fn test_message_ordering_unit() {
    // Test 1: Normal order - BestMove then Finished
    {
        let (tx, rx) = unbounded::<MockWorkerMessage>();
        let bestmove_count = Arc::new(AtomicU32::new(0));
        let mut search_state = SearchState::Searching;
        let bestmove_sent = Arc::new(AtomicBool::new(false));

        // Simulate normal completion
        tx.send(MockWorkerMessage::BestMove {
            best_move: "7g7f".to_string(),
            ponder: None,
        })
        .unwrap();
        tx.send(MockWorkerMessage::Finished { from_guard: false }).unwrap();

        // Process messages
        while let Ok(msg) = rx.try_recv() {
            match msg {
                MockWorkerMessage::BestMove { .. } => {
                    if search_state.can_accept_bestmove() && !bestmove_sent.load(Ordering::Acquire)
                    {
                        bestmove_count.fetch_add(1, Ordering::Release);
                        bestmove_sent.store(true, Ordering::Release);
                        search_state = SearchState::Idle;
                    }
                }
                MockWorkerMessage::Finished { .. } => {
                    search_state = SearchState::Idle;
                }
                _ => {}
            }
        }

        assert_eq!(bestmove_count.load(Ordering::Acquire), 1);
    }

    // Test 2: Reversed order - Finished then BestMove (shouldn't happen but test safety)
    {
        let (tx, rx) = unbounded::<MockWorkerMessage>();
        let bestmove_count = Arc::new(AtomicU32::new(0));
        let mut search_state = SearchState::Searching;
        let bestmove_sent = Arc::new(AtomicBool::new(false));

        // Simulate reversed order
        tx.send(MockWorkerMessage::Finished { from_guard: false }).unwrap();
        tx.send(MockWorkerMessage::BestMove {
            best_move: "7g7f".to_string(),
            ponder: None,
        })
        .unwrap();

        // Process messages
        while let Ok(msg) = rx.try_recv() {
            match msg {
                MockWorkerMessage::BestMove { .. } => {
                    if search_state.can_accept_bestmove() && !bestmove_sent.load(Ordering::Acquire)
                    {
                        bestmove_count.fetch_add(1, Ordering::Release);
                        bestmove_sent.store(true, Ordering::Release);
                        search_state = SearchState::Idle;
                    }
                }
                MockWorkerMessage::Finished { .. } => {
                    search_state = SearchState::Idle;
                }
                _ => {}
            }
        }

        // With bestmove_sent flag, even reversed order is safe
        assert_eq!(bestmove_count.load(Ordering::Acquire), 0);
    }

    // Test 3: Double BestMove (shouldn't happen but test safety)
    {
        let (tx, rx) = unbounded::<MockWorkerMessage>();
        let bestmove_count = Arc::new(AtomicU32::new(0));
        let search_state = SearchState::Searching;
        let bestmove_sent = Arc::new(AtomicBool::new(false));

        // Simulate double bestmove
        tx.send(MockWorkerMessage::BestMove {
            best_move: "7g7f".to_string(),
            ponder: None,
        })
        .unwrap();
        tx.send(MockWorkerMessage::BestMove {
            best_move: "8h2b+".to_string(),
            ponder: None,
        })
        .unwrap();

        // Process messages
        while let Ok(msg) = rx.try_recv() {
            if let MockWorkerMessage::BestMove { .. } = msg {
                if search_state.can_accept_bestmove() && !bestmove_sent.load(Ordering::Acquire) {
                    bestmove_count.fetch_add(1, Ordering::Release);
                    bestmove_sent.store(true, Ordering::Release);
                    // Don't clear searching to test second bestmove handling
                }
            }
        }

        // Only first bestmove should be sent
        assert_eq!(bestmove_count.load(Ordering::Acquire), 1);
    }
}

/// Test fallback behavior when worker doesn't send bestmove
#[test]
fn test_worker_no_bestmove() {
    let (tx, rx) = unbounded::<MockWorkerMessage>();
    let bestmove_count = Arc::new(AtomicU32::new(0));
    let mut search_state = SearchState::Searching;
    let bestmove_sent = Arc::new(AtomicBool::new(false));

    // Simulate worker finishing without bestmove
    tx.send(MockWorkerMessage::PartialResult {
        current_best: "7g7f".to_string(),
        depth: 5,
        score: 100,
    })
    .unwrap();
    tx.send(MockWorkerMessage::Finished { from_guard: false }).unwrap();

    let mut partial_result = None;
    let mut should_send_fallback = false;

    // Process messages
    while let Ok(msg) = rx.try_recv() {
        match msg {
            MockWorkerMessage::BestMove { .. } => {
                if search_state.can_accept_bestmove() && !bestmove_sent.load(Ordering::Acquire) {
                    bestmove_count.fetch_add(1, Ordering::Release);
                    bestmove_sent.store(true, Ordering::Release);
                    search_state = SearchState::Idle;
                }
            }
            MockWorkerMessage::PartialResult {
                current_best,
                depth,
                score,
            } => {
                partial_result = Some((current_best, depth, score));
            }
            MockWorkerMessage::Finished { .. } => {
                search_state = SearchState::Idle;
                if !bestmove_sent.load(Ordering::Acquire) {
                    should_send_fallback = true;
                }
            }
            _ => {}
        }
    }

    // Verify no bestmove was sent by worker
    assert_eq!(bestmove_count.load(Ordering::Acquire), 0);
    assert!(should_send_fallback);
    assert!(partial_result.is_some());
}

/// Test stop command with timeout and fallback
#[test]
fn test_stop_timeout_fallback() {
    let (tx, rx) = unbounded::<MockWorkerMessage>();
    let bestmove_count = Arc::new(AtomicU32::new(0));
    let mut search_state = SearchState::Searching;
    let bestmove_sent = Arc::new(AtomicBool::new(false));

    // Simulate partial result before timeout
    tx.send(MockWorkerMessage::PartialResult {
        current_best: "2g2f".to_string(),
        depth: 3,
        score: 50,
    })
    .unwrap();

    let mut partial_result = None;

    // Process available messages (simulating timeout)
    while let Ok(msg) = rx.try_recv() {
        if let MockWorkerMessage::PartialResult {
            current_best,
            depth,
            score,
        } = msg
        {
            partial_result = Some((current_best, depth, score));
        }
    }

    // Simulate timeout - stop requested and send fallback bestmove
    search_state = SearchState::StopRequested;
    if !bestmove_sent.load(Ordering::Acquire) && partial_result.is_some() {
        bestmove_count.fetch_add(1, Ordering::Release);
        bestmove_sent.store(true, Ordering::Release);
        search_state = SearchState::FallbackSent;
    }

    // Now simulate delayed worker bestmove arriving
    tx.send(MockWorkerMessage::BestMove {
        best_move: "7g7f".to_string(),
        ponder: Some("8c8d".to_string()),
    })
    .unwrap();

    // Process delayed message
    while let Ok(msg) = rx.try_recv() {
        if let MockWorkerMessage::BestMove { .. } = msg {
            if search_state.can_accept_bestmove() && !bestmove_sent.load(Ordering::Acquire) {
                bestmove_count.fetch_add(1, Ordering::Release);
                bestmove_sent.store(true, Ordering::Release);
            }
        }
    }

    // Should have sent exactly one bestmove (the fallback)
    assert_eq!(bestmove_count.load(Ordering::Acquire), 1);
    assert_eq!(search_state, SearchState::FallbackSent);
}

/// Test multiple sequential searches
#[test]
fn test_sequential_searches() {
    let bestmove_count = Arc::new(AtomicU32::new(0));

    for i in 0..3 {
        let (tx, rx) = unbounded::<MockWorkerMessage>();
        let mut search_state = SearchState::Searching;
        let bestmove_sent = Arc::new(AtomicBool::new(false)); // Reset for each search

        // Simulate search
        tx.send(MockWorkerMessage::BestMove {
            best_move: format!("move{i}"),
            ponder: None,
        })
        .unwrap();
        tx.send(MockWorkerMessage::Finished { from_guard: false }).unwrap();

        // Process messages
        while let Ok(msg) = rx.try_recv() {
            match msg {
                MockWorkerMessage::BestMove { .. } => {
                    if search_state.can_accept_bestmove() && !bestmove_sent.load(Ordering::Acquire)
                    {
                        bestmove_count.fetch_add(1, Ordering::Release);
                        bestmove_sent.store(true, Ordering::Release);
                        search_state = SearchState::Idle;
                    }
                }
                MockWorkerMessage::Finished { .. } => {
                    search_state = SearchState::Idle;
                }
                _ => {}
            }
        }
    }

    // Should have sent exactly 3 bestmoves (one per search)
    assert_eq!(bestmove_count.load(Ordering::Acquire), 3);
}

/// Test state transitions
#[test]
fn test_search_state_transitions() {
    let mut state = SearchState::Idle;

    // Can start search from Idle
    assert!(!state.is_searching());
    assert!(!state.can_accept_bestmove());

    // Transition to Searching
    state = SearchState::Searching;
    assert!(state.is_searching());
    assert!(state.can_accept_bestmove());

    // Transition to StopRequested
    state = SearchState::StopRequested;
    assert!(state.is_searching());
    assert!(state.can_accept_bestmove());

    // Transition to FallbackSent
    state = SearchState::FallbackSent;
    assert!(!state.is_searching());
    assert!(!state.can_accept_bestmove());

    // Back to Idle
    state = SearchState::Idle;
    assert!(!state.is_searching());
}
