//! Tests for game phase module

use super::*;
use crate::usi::parse_sfen;
use crate::Position;

#[test]
fn test_material_signal_calculation() {
    let params = PhaseParameters::for_profile(Profile::Search);

    // Full material (starting position)
    let pos =
        parse_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1").unwrap();
    let signals = compute_signals(&pos, 0, &params.phase_weights, &params);
    assert!(
        (signals.material - 0.0).abs() < 0.001,
        "Starting position should have material signal ~0.0"
    );

    // Kings only
    let pos = parse_sfen("4k4/9/9/9/9/9/9/9/4K4 b - 1").unwrap();
    let signals = compute_signals(&pos, 100, &params.phase_weights, &params);
    assert!(
        (signals.material - 1.0).abs() < 0.001,
        "Kings only should have material signal ~1.0"
    );

    // Some pieces captured
    let pos = parse_sfen("ln1gkg1nl/1r4s2/p1pppp1pp/1p4p2/9/2P6/PP1PPPPPP/7R1/LN1GKGSNL w Bb 30")
        .unwrap();
    let signals = compute_signals(&pos, 60, &params.phase_weights, &params);
    assert!(
        signals.material > 0.0 && signals.material < 0.2,
        "Some captures should give small material signal"
    );
}

#[test]
fn test_ply_signal_calculation() {
    let params = PhaseParameters::for_profile(Profile::Search);
    let pos = Position::startpos();

    // Early game
    let signals = compute_signals(&pos, 0, &params.phase_weights, &params);
    assert_eq!(signals.ply, 0.0, "Ply 0 should give ply signal 0.0");

    let signals = compute_signals(&pos, 20, &params.phase_weights, &params);
    assert_eq!(signals.ply, 0.0, "Ply 20 should still be 0.0 (before opening threshold)");

    // Middle range
    let signals = compute_signals(&pos, 60, &params.phase_weights, &params);
    assert!(signals.ply > 0.0 && signals.ply < 1.0, "Ply 60 should be in middle range");
    assert!((signals.ply - 0.5).abs() < 0.1, "Ply 60 should be around 0.5");

    // Late game
    let signals = compute_signals(&pos, 80, &params.phase_weights, &params);
    assert_eq!(signals.ply, 1.0, "Ply 80+ should give ply signal 1.0");

    let signals = compute_signals(&pos, 200, &params.phase_weights, &params);
    assert_eq!(signals.ply, 1.0, "Very high ply should still be 1.0");
}

#[test]
fn test_profile_differences() {
    let pos = Position::startpos();
    let ply = 50;

    let search_params = PhaseParameters::for_profile(Profile::Search);
    let time_params = PhaseParameters::for_profile(Profile::Time);

    let search_signals = compute_signals(&pos, ply, &search_params.phase_weights, &search_params);
    let time_signals = compute_signals(&pos, ply, &time_params.phase_weights, &time_params);

    // Signals should be the same
    assert_eq!(search_signals.material, time_signals.material);

    // But combined scores differ due to weights
    let search_score = search_signals.combined_score(search_params.w_material, search_params.w_ply);
    let time_score = time_signals.combined_score(time_params.w_material, time_params.w_ply);

    assert!(search_score < time_score, "Time profile should weight ply more heavily");
}

#[test]
fn test_hysteresis() {
    let params = PhaseParameters::for_profile(Profile::Search);

    // Test hysteresis at the boundary between Opening and MiddleGame
    // Score just above endgame_threshold (0.176)
    let boundary_score = params.endgame_threshold + 0.005; // 0.181

    println!(
        "Testing with params: endgame_threshold={}, opening_threshold={}, hysteresis={}",
        params.endgame_threshold, params.opening_threshold, params.hysteresis
    );

    // Create signals that produce this combined score
    // With w_material=0.7, w_ply=0.3, and ply=0:
    // combined = 0.7 * material + 0.3 * 0 = 0.7 * material
    // So material = boundary_score / 0.7
    let material_signal = boundary_score / params.w_material;
    let signals = PhaseSignals {
        material: material_signal,
        ply: 0.0,
    };

    // Without history, should be MiddleGame
    let output = classify(None, &signals, &params);
    assert_eq!(output.phase, GamePhase::MiddleGame, "Without history should be MiddleGame");

    // With Opening history, should stay Opening due to hysteresis
    let output = classify(Some(GamePhase::Opening), &signals, &params);
    assert_eq!(output.phase, GamePhase::Opening, "With Opening history should stay Opening");

    // Need clearer transition to move from Opening to MiddleGame
    let transition_score = params.endgame_threshold + params.hysteresis + 0.001;
    let material_signal = transition_score / params.w_material;
    let signals = PhaseSignals {
        material: material_signal,
        ply: 0.0,
    };
    let output = classify(Some(GamePhase::Opening), &signals, &params);
    assert_eq!(output.phase, GamePhase::MiddleGame, "Should transition to MiddleGame");

    // Test hysteresis at EndGame boundary
    let endgame_boundary = params.opening_threshold + 0.01;
    let material_signal = endgame_boundary / params.w_material;
    let signals = PhaseSignals {
        material: material_signal,
        ply: 0.0,
    };

    // Without history, should be EndGame
    let output = classify(None, &signals, &params);
    assert_eq!(output.phase, GamePhase::EndGame, "Without history should be EndGame");

    // With MiddleGame history, should stay MiddleGame
    let output = classify(Some(GamePhase::MiddleGame), &signals, &params);
    assert_eq!(
        output.phase,
        GamePhase::MiddleGame,
        "With MiddleGame history should stay MiddleGame"
    );
}

#[test]
fn test_compatibility_with_baseline() {
    // Test that Search profile approximates the old controller.rs behavior
    let test_cases = vec![
        // (material_score_old_system, expected_phase)
        (128, GamePhase::Opening),   // Full material
        (100, GamePhase::Opening),   // Above 96
        (96, GamePhase::Opening),    // At threshold
        (95, GamePhase::MiddleGame), // Just below
        (50, GamePhase::MiddleGame), // Clear middle
        (32, GamePhase::MiddleGame), // At endgame threshold
        (31, GamePhase::EndGame),    // Just into endgame
        (0, GamePhase::EndGame),     // No material
    ];

    let params = PhaseParameters::for_profile(Profile::Search);

    for (old_score, expected_phase) in test_cases {
        // Convert old score (0-128) to new signal (0.0-1.0)
        let material_signal = 1.0 - (old_score as f32 / 128.0);
        let signals = PhaseSignals {
            material: material_signal,
            ply: 0.0,
        };

        // With search profile weights (0.7 material, 0.3 ply), and ply=0
        // score = 0.7 * material_signal + 0.3 * 0.0 = 0.7 * material_signal
        let output = classify(None, &signals, &params);

        // For debugging
        let score = signals.combined_score(params.w_material, params.w_ply);
        println!(
            "Old score {} -> signal {:.3} -> combined {:.3} -> {:?}",
            old_score, material_signal, score, output.phase
        );

        assert_eq!(output.phase, expected_phase, "Phase mismatch for old score {}", old_score);
    }
}
