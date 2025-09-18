pub trait DashboardValKind {
    fn is_jsonl(&self) -> bool;
    fn calib_bins(&self) -> usize;
}

#[derive(Clone, Copy)]
pub struct DashboardOpts {
    pub emit: bool,
    pub calib_bins_n: usize,
    pub do_plots: bool,
    pub val_is_jsonl: bool,
}

impl DashboardValKind for DashboardOpts {
    fn is_jsonl(&self) -> bool {
        self.val_is_jsonl
    }
    fn calib_bins(&self) -> usize {
        self.calib_bins_n
    }
}

pub struct TrainTrackers<'a> {
    pub best_network: &'a mut Option<Network>,
    pub best_val_loss: &'a mut f32,
    pub last_val_loss: &'a mut Option<f32>,
    pub best_epoch: &'a mut Option<usize>,
}

pub struct TrainContext<'a> {
    pub out_dir: &'a Path,
    pub save_every: Option<usize>,
    pub dash: DashboardOpts,
    pub trackers: TrainTrackers<'a>,
    pub structured: Option<StructuredLogger>,
    pub global_step: u64,
    pub training_config_json: Option<serde_json::Value>,
    pub plateau: Option<LrPlateauState>,
    pub export: ExportOptions,
    pub distill: DistillOptions,
    pub classic_bundle: &'a mut Option<ClassicIntNetworkBundle>,
}

// LR Plateau state (Spec #11 overlay)
pub struct LrPlateauState {
    pub(crate) best: f32,
    pub(crate) wait: u32,
    pub(crate) patience: u32,
    pub(crate) min_delta: f32,
    pub(crate) gamma: f32,
    pub(crate) multiplier: f32,
}

impl LrPlateauState {
    pub fn new(patience: u32) -> Self {
        Self {
            best: f32::INFINITY,
            wait: 0,
            patience,
            min_delta: 1e-6,
            gamma: 0.5,
            multiplier: 1.0,
        }
    }

    #[inline]
    pub fn factor(&self) -> f32 {
        self.multiplier
    }

    // Returns Some(new_multiplier) if decay triggered
    pub fn update(&mut self, val_loss: f32) -> Option<f32> {
        if !val_loss.is_finite() {
            return None;
        }
        if val_loss + self.min_delta < self.best {
            self.best = val_loss;
            self.wait = 0;
            None
        } else {
            self.wait = self.wait.saturating_add(1);
            if self.patience > 0 && self.wait >= self.patience {
                self.wait = 0;
                self.multiplier *= self.gamma;
                Some(self.multiplier)
            } else {
                None
            }
        }
    }
}

#[inline]
pub(crate) fn lr_base_for(
    epoch: usize,
    global_step: u64,
    cfg: &Config,
    plateau: Option<&LrPlateauState>,
) -> f32 {
    let mut lr_factor = 1.0f32;
    if cfg.lr_warmup_epochs > 0 {
        let e = epoch as u32;
        if e < cfg.lr_warmup_epochs {
            lr_factor = ((e + 1) as f32) / (cfg.lr_warmup_epochs as f32);
        }
    }
    match cfg.lr_schedule.as_str() {
        "constant" => {}
        "step" => {
            let step_gamma: f32 = 0.5;
            if let Some(de) = cfg.lr_decay_epochs {
                if de > 0 {
                    let k = ((epoch as u32) / de) as i32;
                    lr_factor *= step_gamma.powi(k);
                }
            }
            if let Some(ds) = cfg.lr_decay_steps {
                if ds > 0 {
                    let k = (global_step / ds) as i32;
                    lr_factor *= step_gamma.powi(k);
                }
            }
        }
        "cosine" => {
            let mut p = 0.0f32;
            if let Some(de) = cfg.lr_decay_epochs {
                if de > 0 {
                    p = ((epoch as f32) / (de as f32)).clamp(0.0, 1.0);
                }
            }
            if let Some(ds) = cfg.lr_decay_steps {
                if ds > 0 {
                    p = ((global_step as f32) / (ds as f32)).clamp(0.0, 1.0);
                }
            }
            lr_factor *= 0.5 * (1.0 + (std::f32::consts::PI * p).cos());
        }
        _ => {}
    }
    let pl = plateau.map(|p| p.factor()).unwrap_or(1.0);
    (cfg.learning_rate * lr_factor * pl).max(0.0)
}
