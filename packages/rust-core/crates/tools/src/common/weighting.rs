use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, Default)]
pub struct WeightingConfig {
    pub active: Vec<WeightingKind>,
    pub coeffs: WeightingCoefficients,
    pub preset: Option<String>,
}

pub fn load_config_file(path: &str) -> Result<WeightingConfigFile, Box<dyn std::error::Error>> {
    let data = std::fs::read_to_string(path)?;
    if path.ends_with(".yaml") || path.ends_with(".yml") {
        let v: WeightingConfigFile = serde_yaml::from_str(&data)?;
        Ok(v)
    } else if path.ends_with(".json") {
        let v: WeightingConfigFile = serde_json::from_str(&data)?;
        Ok(v)
    } else {
        // try JSON then YAML
        if let Ok(v) = serde_json::from_str::<WeightingConfigFile>(&data) {
            return Ok(v);
        }
        let v: WeightingConfigFile = serde_yaml::from_str(&data)?;
        Ok(v)
    }
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
    if let Some(f) = file.clone() {
        out.active = f.weighting;
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
        out.preset = f.preset;
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
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhaseKind {
    Opening,
    Middlegame,
    Endgame,
}

/// active の順序を exact→gap→phase→mate として乗算する
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
    for k in &cfg.active {
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
}
