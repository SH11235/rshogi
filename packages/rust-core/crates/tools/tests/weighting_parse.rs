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

#[test]
fn canonical_order_even_if_shuffled_and_duplicate() {
    // active の順序・重複に関係なく正規順で適用される
    let cfg_shuffled = wcfg::WeightingConfig {
        active: vec![
            wcfg::WeightingKind::Mate,
            wcfg::WeightingKind::Phase,
            wcfg::WeightingKind::Gap,
            wcfg::WeightingKind::Exact,
            wcfg::WeightingKind::Exact,
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
    let cfg_canon = wcfg::WeightingConfig {
        active: vec![
            wcfg::WeightingKind::Exact,
            wcfg::WeightingKind::Gap,
            wcfg::WeightingKind::Phase,
            wcfg::WeightingKind::Mate,
        ],
        ..cfg_shuffled.clone()
    };
    let out1 = wcfg::apply_weighting(
        1.0,
        &cfg_shuffled,
        Some(true),
        Some(10),
        Some(wcfg::PhaseKind::Endgame),
        Some(true),
    );
    let out2 = wcfg::apply_weighting(
        1.0,
        &cfg_canon,
        Some(true),
        Some(10),
        Some(wcfg::PhaseKind::Endgame),
        Some(true),
    );
    assert!((out1 - out2).abs() < 1e-6);
}

#[test]
fn load_config_file_yaml_and_json() {
    let yaml = "tests/fixtures/weighting_endgame.yaml";
    let json = "tests/fixtures/weighting_endgame.json";
    let y = wcfg::load_config_file(yaml).expect("yaml parse");
    let j = wcfg::load_config_file(json).expect("json parse");
    assert_eq!(y.w_exact, Some(1.2));
    assert_eq!(j.w_exact, Some(1.2));
    assert_eq!(y.w_phase_endgame, Some(1.3));
    assert_eq!(j.w_phase_endgame, Some(1.3));
}

#[test]
fn deny_unknown_fields_in_config() {
    // create a temp YAML with unknown field
    let td = tempfile::tempdir().unwrap();
    let p = td.path().join("bad.yaml");
    std::fs::write(&p, "weighting: [exact]\nunknown_key: 1.0\n").unwrap();
    let err = wcfg::load_config_file(&p).unwrap_err();
    let msg = format!("{}", err);
    assert!(msg.contains("unknown field") || msg.contains("unknown"), "msg={}", msg);
}
