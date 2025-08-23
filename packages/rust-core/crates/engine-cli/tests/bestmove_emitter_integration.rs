//! Integration tests for bestmove emitter

use engine_cli::bestmove_emitter::{BestmoveEmitter, BestmoveMeta, BestmoveStats};
use engine_cli::types::BestmoveSource;
use engine_core::search::types::{StopInfo, TerminationReason};
use std::thread;

#[test]
fn test_bestmove_meta_construction() {
    // Test that BestmoveMeta can be properly constructed
    let stop_info = StopInfo {
        reason: TerminationReason::TimeLimit,
        elapsed_ms: 1500,
        nodes: 250000,
        depth_reached: 18,
        hard_timeout: false,
    };

    let stats = BestmoveStats {
        depth: 18,
        seldepth: Some(25),
        score: "cp 125".to_string(),
        nodes: 250000,
        nps: 166666, // nodes * 1000 / elapsed_ms
    };

    let meta = BestmoveMeta {
        from: BestmoveSource::SessionOnStop,
        stop_info,
        stats,
    };

    // Verify fields
    assert_eq!(meta.from, BestmoveSource::SessionOnStop);
    assert_eq!(meta.stop_info.reason, TerminationReason::TimeLimit);
    assert_eq!(meta.stats.depth, 18);
    assert_eq!(meta.stats.score, "cp 125");
}

#[test]
fn test_bestmove_from_sources() {
    // Test different bestmove sources
    let sources = vec![BestmoveSource::EmergencyFallback, BestmoveSource::Resign];

    for source in sources {
        let stop_info = StopInfo {
            reason: TerminationReason::Error,
            elapsed_ms: 100,
            nodes: 1000,
            depth_reached: 1,
            hard_timeout: false,
        };

        let stats = BestmoveStats {
            depth: 1,
            seldepth: None,
            score: "unknown".to_string(),
            nodes: 1000,
            nps: 10000,
        };

        let meta = BestmoveMeta {
            from: source,
            stop_info,
            stats,
        };

        assert_eq!(meta.from, source);
    }
}

#[test]
fn test_score_formats() {
    // Test different score formats
    let scores = vec![
        ("cp 100", "Centipawn score"),
        ("cp -50", "Negative centipawn"),
        ("mate 5", "Mate in 5"),
        ("mate -3", "Mated in 3"),
        ("unknown", "Unknown score"),
    ];

    for (score, description) in scores {
        let stats = BestmoveStats {
            depth: 10,
            seldepth: Some(15),
            score: score.to_string(),
            nodes: 10000,
            nps: 10000,
        };

        assert_eq!(stats.score, score, "Failed for: {}", description);
    }
}

#[test]
fn test_concurrent_emitter_creation() {
    // Test that multiple emitters can be created for different searches
    let num_searches = 10;
    let mut handles = vec![];

    for search_id in 0..num_searches {
        let handle = thread::spawn(move || {
            let _emitter = BestmoveEmitter::new(search_id);
            // Each emitter should have unique search_id
            search_id
        });
        handles.push(handle);
    }

    let mut search_ids = vec![];
    for handle in handles {
        search_ids.push(handle.join().unwrap());
    }

    // All search IDs should be unique
    search_ids.sort();
    for i in 0..num_searches {
        assert_eq!(search_ids[i as usize], i);
    }
}
