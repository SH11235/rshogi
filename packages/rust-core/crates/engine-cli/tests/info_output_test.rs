//! Test for info output during search

use engine_cli::engine_adapter::EngineAdapter;
use engine_cli::usi::output::SearchInfo;
use engine_cli::usi::GoParams;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[test]
fn test_info_output_during_search() {
    let mut adapter = EngineAdapter::new();
    let stop_flag = Arc::new(AtomicBool::new(false));

    // Set position
    adapter.set_position(true, None, &[]).unwrap();

    // Collect info messages
    let info_messages = Arc::new(Mutex::new(Vec::new()));
    let info_messages_clone = info_messages.clone();

    let info_callback = Box::new(move |info: SearchInfo| {
        println!(
            "Info received: depth={:?}, time={:?}, nodes={:?}, score={:?}",
            info.depth, info.time, info.nodes, info.score
        );
        info_messages_clone.lock().unwrap().push(info);
    });

    // Search with depth 5
    let params = GoParams {
        depth: Some(5),
        infinite: false,
        movetime: None,
        nodes: None,
        ..Default::default()
    };

    let result = adapter.search(params, stop_flag, info_callback);
    assert!(result.is_ok());

    // Check we received multiple info messages
    let messages = info_messages.lock().unwrap();
    println!("Total info messages received: {}", messages.len());

    // Should have at least depth 1-5 messages (final may be included)
    assert!(messages.len() >= 5, "Expected at least 5 info messages, got {}", messages.len());

    // Check depths are increasing
    for i in 0..messages.len() - 1 {
        if let (Some(d1), Some(d2)) = (messages[i].depth, messages[i + 1].depth) {
            if d1 > 0 && d2 > 0 {
                assert!(d1 <= d2, "Depths should be non-decreasing");
            }
        }
    }

    // Check time is increasing
    for i in 0..messages.len() - 1 {
        if let (Some(t1), Some(t2)) = (messages[i].time, messages[i + 1].time) {
            assert!(t1 <= t2, "Time should be non-decreasing");
        }
    }
}

#[test]
fn test_info_output_with_early_stop() {
    let mut adapter = EngineAdapter::new();
    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_flag_clone = stop_flag.clone();

    // Set position
    adapter.set_position(true, None, &[]).unwrap();

    // Collect info messages
    let info_messages = Arc::new(Mutex::new(Vec::new()));
    let info_messages_clone = info_messages.clone();

    let info_callback = Box::new(move |info: SearchInfo| {
        let depth = info.depth.unwrap_or(0);
        println!("Info: depth={}, time={:?}, nodes={:?}", depth, info.time, info.nodes);
        info_messages_clone.lock().unwrap().push(info);

        // Stop after depth 3
        if depth >= 3 {
            stop_flag_clone.store(true, Ordering::Release);
        }
    });

    // Search with depth 10 (but will stop early)
    let params = GoParams {
        depth: Some(10),
        infinite: false,
        movetime: None,
        nodes: None,
        ..Default::default()
    };

    let result = adapter.search(params, stop_flag, info_callback);
    assert!(result.is_ok());

    let messages = info_messages.lock().unwrap();
    println!("Info messages before stop: {}", messages.len());

    // Should have info for depths 1, 2, 3, and possibly 4 before stop
    // The final message may show the requested depth (10) but with early termination
    let intermediate_depths: Vec<u32> =
        messages.iter().filter_map(|m| m.depth).filter(|&d| d <= 4).collect();

    println!("Intermediate depths: {intermediate_depths:?}");
    assert!(!intermediate_depths.is_empty(), "Should have intermediate depth info");
    assert!(intermediate_depths.contains(&3), "Should have reached at least depth 3");
}
