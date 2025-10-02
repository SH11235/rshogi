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

#[derive(Clone, Copy, Debug)]
pub struct SearchProfile {
    pub prune: PruneToggles,
}

impl SearchProfile {
    pub fn basic() -> Self {
        Self {
            prune: PruneToggles {
                enable_nmp: true,
                enable_iid: false,
                enable_razor: false,
                enable_probcut: false,
                enable_static_beta_pruning: true,
            },
        }
    }

    pub fn enhanced() -> Self {
        Self {
            prune: PruneToggles::default(),
        }
    }
}

impl Default for SearchProfile {
    fn default() -> Self {
        Self::enhanced()
    }
}
