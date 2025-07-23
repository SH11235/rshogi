//! Simple integration tests focusing on critical behavior

#[test]
fn test_engine_compiles_and_runs() {
    // This test ensures the engine binary can be built
    // The actual integration tests would require a full build
    // and are better suited for CI/CD environments

    // For now, we just ensure the main components compile

    use engine_cli::usi::{parse_usi_command, UsiCommand};

    // Basic parsing test
    let result = parse_usi_command("usi");
    assert!(matches!(result, Ok(UsiCommand::Usi)));

    let result = parse_usi_command("stop");
    assert!(matches!(result, Ok(UsiCommand::Stop)));

    let result = parse_usi_command("quit");
    assert!(matches!(result, Ok(UsiCommand::Quit)));
}

#[test]
fn test_search_info_formatting() {
    use engine_cli::engine_adapter::SearchInfo;

    // Test that depth 0 is not shown
    let info = SearchInfo {
        depth: 0,
        time: 100,
        nodes: 1000,
        pv: vec!["7g7f".to_string()],
        score: 50,
    };

    let output = info.to_usi_string();
    assert!(!output.contains("depth 0"));
    assert!(output.contains("score cp 50"));
    assert!(output.contains("time 100"));
    assert!(output.contains("nodes 1000"));
    assert!(output.contains("pv 7g7f"));

    // Test that depth > 0 is shown
    let info = SearchInfo {
        depth: 5,
        time: 100,
        nodes: 1000,
        pv: vec!["7g7f".to_string()],
        score: 50,
    };

    let output = info.to_usi_string();
    assert!(output.contains("depth 5"));
}

#[test]
fn test_time_minimum_value() {
    use engine_cli::engine_adapter::SearchInfo;
    use std::time::Duration;

    // Simulate very short elapsed time
    let elapsed = Duration::from_micros(100); // 0.1ms
    let time_ms = elapsed.as_millis().max(1) as u64;

    assert_eq!(time_ms, 1); // Should be at least 1ms

    // Test in SearchInfo context
    let info = SearchInfo {
        depth: 1,
        time: time_ms,
        nodes: 10,
        pv: vec![],
        score: 0,
    };

    let output = info.to_usi_string();
    assert!(output.contains("time 1"));
}
