use crate::search::params;

#[derive(Clone, Copy, Debug)]
pub struct PruneToggles {
    pub enable_nmp: bool,
    pub enable_iid: bool,
    pub enable_razor: bool,
    pub enable_probcut: bool,
    pub enable_static_beta_pruning: bool,
}

impl Default for PruneToggles {
    fn default() -> Self {
        Self {
            enable_nmp: true,
            enable_iid: true,
            enable_razor: true,
            enable_probcut: true,
            enable_static_beta_pruning: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchProfileKind {
    BasicMaterial,
    BasicNnue,
    EnhancedMaterial,
    EnhancedNnue,
}

#[derive(Clone, Copy, Debug)]
pub struct SearchTuning {
    pub lmr_k_x100: u32,
    pub lmp_limits: [usize; 3],
    pub hp_threshold: i32,
    pub sbp_margin_d1: i32,
    pub sbp_margin_d2: i32,
    pub probcut_margin_d5: i32,
    pub probcut_margin_d6p: i32,
    pub iid_min_depth: i32,
    pub enable_qs_checks: bool,
    pub enable_razor: bool,
}

impl SearchTuning {
    const fn material_basic() -> Self {
        Self {
            lmr_k_x100: 170,
            lmp_limits: [6, 12, 18],
            hp_threshold: -2000,
            sbp_margin_d1: 200,
            sbp_margin_d2: 300,
            probcut_margin_d5: 250,
            probcut_margin_d6p: 300,
            iid_min_depth: 8,
            enable_qs_checks: true,
            enable_razor: false,
        }
    }

    const fn nnue_basic() -> Self {
        Self {
            lmr_k_x100: 170,
            lmp_limits: [6, 12, 18],
            hp_threshold: -2000,
            sbp_margin_d1: 200,
            sbp_margin_d2: 300,
            probcut_margin_d5: 250,
            probcut_margin_d6p: 300,
            iid_min_depth: 8,
            enable_qs_checks: true,
            enable_razor: false,
        }
    }

    const fn material_enhanced() -> Self {
        Self {
            lmr_k_x100: 170,
            lmp_limits: [6, 12, 18],
            hp_threshold: -2000,
            sbp_margin_d1: 200,
            sbp_margin_d2: 300,
            probcut_margin_d5: 250,
            probcut_margin_d6p: 300,
            iid_min_depth: 6,
            enable_qs_checks: true,
            enable_razor: true,
        }
    }

    const fn nnue_enhanced() -> Self {
        Self {
            lmr_k_x100: 170,
            lmp_limits: [6, 12, 18],
            hp_threshold: -2000,
            sbp_margin_d1: 200,
            sbp_margin_d2: 300,
            probcut_margin_d5: 250,
            probcut_margin_d6p: 300,
            iid_min_depth: 6,
            enable_qs_checks: true,
            enable_razor: true,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SearchProfile {
    pub kind: SearchProfileKind,
    pub prune: PruneToggles,
    pub tuning: SearchTuning,
}

impl SearchProfile {
    pub fn basic_material() -> Self {
        Self {
            kind: SearchProfileKind::BasicMaterial,
            prune: PruneToggles {
                enable_nmp: true,
                enable_iid: false,
                enable_razor: false,
                enable_probcut: false,
                enable_static_beta_pruning: true,
            },
            tuning: SearchTuning::material_basic(),
        }
    }

    pub fn basic_nnue() -> Self {
        Self {
            kind: SearchProfileKind::BasicNnue,
            prune: PruneToggles {
                enable_nmp: true,
                enable_iid: false,
                enable_razor: false,
                enable_probcut: false,
                enable_static_beta_pruning: true,
            },
            tuning: SearchTuning::nnue_basic(),
        }
    }

    pub fn enhanced_material() -> Self {
        Self {
            kind: SearchProfileKind::EnhancedMaterial,
            prune: PruneToggles::default(),
            tuning: SearchTuning::material_enhanced(),
        }
    }

    pub fn enhanced_nnue() -> Self {
        Self {
            kind: SearchProfileKind::EnhancedNnue,
            prune: PruneToggles::default(),
            tuning: SearchTuning::nnue_enhanced(),
        }
    }

    /// Backwards-compatible alias for material basic profile.
    pub fn basic() -> Self {
        Self::basic_material()
    }

    /// Backwards-compatible alias for material enhanced profile.
    pub fn enhanced() -> Self {
        Self::enhanced_material()
    }

    /// Apply this profile's tuning values to the global runtime parameters (`SearchParams`).
    ///
    /// EngineType 切り替え時に既定値をリセットし、必要に応じて USI `setoption`
    /// で再調整できるようにする目的で使用する。
    pub fn apply_runtime_defaults(&self) {
        // Numeric tunables
        params::set_lmr_k_x100(self.tuning.lmr_k_x100);
        params::set_lmp_d1(self.tuning.lmp_limits[0]);
        params::set_lmp_d2(self.tuning.lmp_limits[1]);
        params::set_lmp_d3(self.tuning.lmp_limits[2]);
        params::set_hp_threshold(self.tuning.hp_threshold);
        params::set_sbp_d1(self.tuning.sbp_margin_d1);
        params::set_sbp_d2(self.tuning.sbp_margin_d2);
        params::set_probcut_d5(self.tuning.probcut_margin_d5);
        params::set_probcut_d6p(self.tuning.probcut_margin_d6p);
        params::set_iid_min_depth(self.tuning.iid_min_depth);

        // Boolean toggles (runtime gates)
        params::set_qs_checks_enabled(self.tuning.enable_qs_checks);
        params::set_razor_enabled(self.tuning.enable_razor);
        params::set_nmp_enabled(self.prune.enable_nmp);
        params::set_iid_enabled(self.prune.enable_iid);
        params::set_probcut_enabled(self.prune.enable_probcut);
        params::set_static_beta_enabled(self.prune.enable_static_beta_pruning);
    }
}

impl Default for SearchProfile {
    fn default() -> Self {
        Self::enhanced_material()
    }
}
