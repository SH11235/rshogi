use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WeightingKind {
    Exact,
    Gap,
    Phase,
    Mate,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WeightingCoefficients {
    #[serde(default = "one")]
    pub w_exact: f32,
    #[serde(default = "one")]
    pub w_gap: f32,
    #[serde(default = "one")]
    pub w_phase_opening: f32,
    #[serde(default = "one")]
    pub w_phase_middlegame: f32,
    #[serde(default = "one")]
    pub w_phase_endgame: f32,
    #[serde(default = "one")]
    pub w_mate_ring: f32,
}

fn one() -> f32 {
    1.0
}

impl Default for WeightingCoefficients {
    fn default() -> Self {
        Self {
            w_exact: 1.0,
            w_gap: 1.0,
            w_phase_opening: 1.0,
            w_phase_middlegame: 1.0,
            w_phase_endgame: 1.0,
            w_mate_ring: 1.0,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WeightingConfigFile {
    #[serde(default)]
    pub weighting: Vec<WeightingKind>,
    #[serde(default)]
    pub w_exact: Option<f32>,
    #[serde(default)]
    pub w_gap: Option<f32>,
    #[serde(default)]
    pub w_phase_endgame: Option<f32>,
    #[serde(default)]
    pub w_phase_opening: Option<f32>,
    #[serde(default)]
    pub w_phase_middlegame: Option<f32>,
    #[serde(default)]
    pub w_mate_ring: Option<f32>,
    #[serde(default)]
    pub preset: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WeightingConfig {
    pub active: Vec<WeightingKind>,
    pub coeffs: WeightingCoefficients,
    pub preset: Option<String>,
}
pub fn load_config_file<P: AsRef<std::path::Path>>(
    path: P,
) -> Result<WeightingConfigFile, Box<dyn std::error::Error>> {
    let path = path.as_ref();
    let data = std::fs::read_to_string(path)?;
    let ext = path.extension().and_then(|s| s.to_str()).map(|s| s.to_ascii_lowercase());
    match ext.as_deref() {
        Some("yaml") | Some("yml") => Ok(serde_yaml::from_str(&data)?),
        Some("json") => Ok(serde_json::from_str(&data)?),
        _ => serde_json::from_str(&data)
            .or_else(|_| serde_yaml::from_str(&data))
            .map_err(|e| e.into()),
    }
}

pub(crate) const CANONICAL_ORDER: [WeightingKind; 4] = [
    WeightingKind::Exact,
    WeightingKind::Gap,
    WeightingKind::Phase,
    WeightingKind::Mate,
];

fn normalize_active(v: Vec<WeightingKind>) -> Vec<WeightingKind> {
    use std::collections::HashSet;
    let set: HashSet<_> = v.into_iter().collect();
    CANONICAL_ORDER.iter().copied().filter(|k| set.contains(k)).collect()
}

fn validate_coeffs(c: &mut WeightingCoefficients) {
    fn ok(x: f32) -> bool {
        x.is_finite() && x >= 0.0
    }
    macro_rules! sanitize {
        ($name:ident) => {
            if !ok(c.$name) {
                eprintln!(
                    "Warning: invalid coefficient {}={:?}; reset to 1.0",
                    stringify!($name),
                    c.$name
                );
                c.$name = 1.0;
            }
        };
    }
    sanitize!(w_exact);
    sanitize!(w_gap);
    sanitize!(w_phase_opening);
    sanitize!(w_phase_middlegame);
    sanitize!(w_phase_endgame);
    sanitize!(w_mate_ring);
}

pub fn merge_config(
    file: Option<WeightingConfigFile>,
    cli_active: Option<Vec<WeightingKind>>,
    cli_w_exact: Option<f32>,
    cli_w_gap: Option<f32>,
    cli_w_phase_endgame: Option<f32>,
    cli_w_mate_ring: Option<f32>,
) -> WeightingConfig {
    let mut out = WeightingConfig::default();
    if let Some(ref f) = file {
        out.active = f.weighting.clone();
        if let Some(x) = f.w_exact {
            out.coeffs.w_exact = x;
        }
        if let Some(x) = f.w_gap {
            out.coeffs.w_gap = x;
        }
        if let Some(x) = f.w_phase_opening {
            out.coeffs.w_phase_opening = x;
        }
        if let Some(x) = f.w_phase_middlegame {
            out.coeffs.w_phase_middlegame = x;
        }
        if let Some(x) = f.w_phase_endgame {
            out.coeffs.w_phase_endgame = x;
        }
        if let Some(x) = f.w_mate_ring {
            out.coeffs.w_mate_ring = x;
        }
        out.preset = f.preset.clone();
    }
    if let Some(a) = cli_active {
        out.active = a;
    }
    if let Some(x) = cli_w_exact {
        out.coeffs.w_exact = x;
    }
    if let Some(x) = cli_w_gap {
        out.coeffs.w_gap = x;
    }
    if let Some(x) = cli_w_phase_endgame {
        out.coeffs.w_phase_endgame = x;
    }
    if let Some(x) = cli_w_mate_ring {
        out.coeffs.w_mate_ring = x;
    }
    // 正規化とバリデーション
    out.active = normalize_active(out.active);
    let mut c = out.coeffs.clone();
    validate_coeffs(&mut c);
    out.coeffs = c;
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PhaseKind {
    Opening,
    Middlegame,
    Endgame,
}

/// active の順序を exact→gap→phase→mate として乗算する
#[must_use]
pub fn apply_weighting(
    base: f32,
    cfg: &WeightingConfig,
    both_exact: Option<bool>,
    gap_cp: Option<i32>,
    phase: Option<PhaseKind>,
    mate_ring: Option<bool>,
) -> f32 {
    let mut w = base;
    if cfg.active.is_empty() {
        return w;
    }
    for k in CANONICAL_ORDER {
        if !cfg.active.contains(&k) {
            continue;
        }
        match k {
            WeightingKind::Exact => {
                if both_exact.unwrap_or(false) {
                    w *= cfg.coeffs.w_exact;
                }
            }
            WeightingKind::Gap => {
                if let Some(g) = gap_cp {
                    if g >= 0 {
                        // 小さいほど↑
                        // 閾値 50cp で簡易的に強調（ベースラインへの乗算のみ）
                        if g < 50 {
                            w *= cfg.coeffs.w_gap;
                        }
                    }
                }
            }
            WeightingKind::Phase => {
                if let Some(ph) = phase {
                    match ph {
                        PhaseKind::Opening => {
                            w *= cfg.coeffs.w_phase_opening;
                        }
                        PhaseKind::Middlegame => {
                            w *= cfg.coeffs.w_phase_middlegame;
                        }
                        PhaseKind::Endgame => {
                            w *= cfg.coeffs.w_phase_endgame;
                        }
                    }
                }
            }
            WeightingKind::Mate => {
                if mate_ring.unwrap_or(false) {
                    w *= cfg.coeffs.w_mate_ring;
                }
            }
        }
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_precedence_cli_over_config() {
        let file = WeightingConfigFile {
            weighting: vec![WeightingKind::Gap, WeightingKind::Mate],
            w_exact: Some(1.1),
            w_gap: Some(1.2),
            w_phase_endgame: Some(1.3),
            w_phase_opening: None,
            w_phase_middlegame: None,
            w_mate_ring: Some(1.4),
            preset: Some("cfg".into()),
        };
        let cfg = merge_config(
            Some(file),
            Some(vec![WeightingKind::Exact, WeightingKind::Phase]),
            Some(1.5), // override
            None,
            Some(1.7), // override
            None,
        );
        assert_eq!(cfg.active, vec![WeightingKind::Exact, WeightingKind::Phase]);
        assert!((cfg.coeffs.w_exact - 1.5).abs() < 1e-6);
        assert!((cfg.coeffs.w_gap - 1.2).abs() < 1e-6); // from file
        assert!((cfg.coeffs.w_phase_endgame - 1.7).abs() < 1e-6);
        assert!((cfg.coeffs.w_mate_ring - 1.4).abs() < 1e-6);
        assert_eq!(cfg.preset.as_deref(), Some("cfg"));
    }

    #[test]
    fn apply_order_exact_gap_phase_mate() {
        let cfg = WeightingConfig {
            active: vec![
                WeightingKind::Exact,
                WeightingKind::Gap,
                WeightingKind::Phase,
                WeightingKind::Mate,
            ],
            coeffs: WeightingCoefficients {
                w_exact: 2.0,
                w_gap: 3.0,
                w_phase_opening: 5.0,
                w_phase_middlegame: 7.0,
                w_phase_endgame: 11.0,
                w_mate_ring: 13.0,
            },
            preset: None,
        };
        let base = 1.0f32;
        let out =
            apply_weighting(base, &cfg, Some(true), Some(10), Some(PhaseKind::Endgame), Some(true));
        // 2 * 3 * 11 * 13 = 858
        assert!((out - 858.0).abs() < 1e-6, "out={}", out);
    }

    #[test]
    fn order_is_canonical_even_if_config_is_shuffled() {
        let cfg_shuffled = WeightingConfig {
            active: vec![
                WeightingKind::Mate,
                WeightingKind::Phase,
                WeightingKind::Gap,
                WeightingKind::Exact,
                WeightingKind::Exact,
            ],
            coeffs: WeightingCoefficients {
                w_exact: 2.0,
                w_gap: 3.0,
                w_phase_opening: 5.0,
                w_phase_middlegame: 7.0,
                w_phase_endgame: 11.0,
                w_mate_ring: 13.0,
            },
            preset: None,
        };
        let cfg_canonical = WeightingConfig {
            active: vec![
                WeightingKind::Exact,
                WeightingKind::Gap,
                WeightingKind::Phase,
                WeightingKind::Mate,
            ],
            ..cfg_shuffled.clone()
        };
        let out1 = apply_weighting(
            1.0,
            &cfg_shuffled,
            Some(true),
            Some(10),
            Some(PhaseKind::Endgame),
            Some(true),
        );
        let out2 = apply_weighting(
            1.0,
            &cfg_canonical,
            Some(true),
            Some(10),
            Some(PhaseKind::Endgame),
            Some(true),
        );
        assert!((out1 - out2).abs() < 1e-6);
    }

    #[test]
    fn identity_when_all_one() {
        let cfg = WeightingConfig::default(); // active empty, coeffs all 1.0
        let w =
            apply_weighting(1.0, &cfg, Some(true), Some(0), Some(PhaseKind::Endgame), Some(true));
        assert!((w - 1.0).abs() < 1e-6);
    }
}
