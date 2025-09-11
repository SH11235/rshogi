use tools::common::weighting as wcfg;

#[test]
fn precedence_cli_over_config() {
    let file = wcfg::WeightingConfigFile {
        weighting: vec![wcfg::WeightingKind::Gap, wcfg::WeightingKind::Mate],
        w_exact: Some(1.1),
        w_gap: Some(1.2),
        w_phase_endgame: Some(1.3),
        w_phase_opening: None,
        w_phase_middlegame: None,
        w_mate_ring: Some(1.4),
        preset: Some("cfg".into()),
    };
    let merged = wcfg::merge_config(
        Some(file),
        Some(vec![wcfg::WeightingKind::Exact, wcfg::WeightingKind::Phase]),
        Some(1.5),
        None,
        Some(1.7),
        None,
    );
    assert_eq!(merged.active, vec![wcfg::WeightingKind::Exact, wcfg::WeightingKind::Phase]);
    assert!((merged.coeffs.w_exact - 1.5).abs() < 1e-6);
    assert!((merged.coeffs.w_gap - 1.2).abs() < 1e-6);
    assert!((merged.coeffs.w_phase_endgame - 1.7).abs() < 1e-6);
    assert_eq!(merged.preset.as_deref(), Some("cfg"));
}

#[test]
fn apply_order_is_exact_gap_phase_mate() {
    let cfg = wcfg::WeightingConfig {
        active: vec![
            wcfg::WeightingKind::Exact,
            wcfg::WeightingKind::Gap,
            wcfg::WeightingKind::Phase,
            wcfg::WeightingKind::Mate,
        ],
        coeffs: wcfg::WeightingCoefficients {
            w_exact: 2.0,
            w_gap: 3.0,
            w_phase_opening: 5.0,
            w_phase_middlegame: 7.0,
            w_phase_endgame: 11.0,
            w_mate_ring: 13.0,
        },
        preset: None,
    };
    let out = wcfg::apply_weighting(
        1.0,
        &cfg,
        Some(true),
        Some(10),
        Some(wcfg::PhaseKind::Endgame),
        Some(true),
    );
    assert!((out - 858.0).abs() < 1e-6);
}
