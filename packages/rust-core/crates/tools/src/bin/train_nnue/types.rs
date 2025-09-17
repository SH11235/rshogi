use clap::ValueEnum;
use engine_core::game_phase::GamePhase;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
pub struct TrainingPosition {
    pub sfen: String,
    #[serde(default)]
    pub lines: Vec<LineInfo>,
    #[serde(default)]
    pub best2_gap_cp: Option<i32>,
    #[serde(default)]
    pub bound1: Option<String>,
    #[serde(default)]
    pub bound2: Option<String>,
    #[serde(default)]
    pub mate_boundary: Option<bool>,
    #[serde(default)]
    pub no_legal_move: Option<bool>,
    #[serde(default)]
    pub fallback_used: Option<bool>,
    #[serde(default)]
    pub eval: Option<i32>,
    #[serde(default)]
    pub depth: Option<u8>,
    #[serde(default)]
    pub seldepth: Option<u8>,
}

#[derive(Debug, Deserialize)]
pub struct LineInfo {
    #[serde(default)]
    pub score_cp: Option<i32>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Config {
    pub epochs: usize,
    pub batch_size: usize,
    pub learning_rate: f32,
    pub optimizer: String,
    pub l2_reg: f32,
    pub label_type: String,
    pub scale: f32,
    pub cp_clip: i32,
    pub accumulator_dim: usize,
    pub relu_clip: i32,
    pub shuffle: bool,
    pub prefetch_batches: usize,
    pub throughput_interval_sec: f32,
    pub stream_cache: bool,
    pub prefetch_bytes: Option<usize>,
    pub estimated_features_per_sample: usize,
    pub exclude_no_legal_move: bool,
    pub exclude_fallback: bool,
    pub lr_schedule: String,
    pub lr_warmup_epochs: u32,
    pub lr_decay_epochs: Option<u32>,
    pub lr_decay_steps: Option<u64>,
    pub lr_plateau_patience: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ArchKind {
    Single,
    Classic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ExportFormat {
    Fp32,
    #[clap(name = "single-i8")]
    SingleI8,
    #[clap(name = "classic-v1")]
    ClassicV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum QuantScheme {
    #[clap(name = "per-tensor")]
    PerTensor,
    #[clap(name = "per-channel")]
    PerChannel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum DistillLossKind {
    #[clap(name = "mse")]
    Mse,
    #[clap(name = "bce")]
    Bce,
    #[clap(name = "kl")]
    Kl,
}

#[derive(Clone, Debug)]
pub struct DistillOptions {
    pub teacher_path: Option<PathBuf>,
    pub loss: DistillLossKind,
    pub temperature: f32,
    pub alpha: f32,
    pub scale_temp2: bool,
    pub soften_student: bool,
    pub seed: Option<u64>,
    /// 教師ネットワーク出力の数値ドメイン
    /// - Cp: 評価値(cp) 例: ±300, ±1200
    /// - WdlLogit: WDLロジット (シグモイド前の値)
    pub teacher_domain: TeacherValueDomain,
}

impl Default for DistillOptions {
    fn default() -> Self {
        Self {
            teacher_path: None,
            loss: DistillLossKind::Mse,
            temperature: 1.0,
            alpha: 1.0,
            scale_temp2: false,
            soften_student: false,
            seed: None,
            teacher_domain: TeacherValueDomain::Cp,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum TeacherValueDomain {
    #[clap(name = "cp")]
    Cp,
    #[clap(name = "wdl-logit")]
    WdlLogit,
}

#[derive(Clone, Debug)]
pub struct ExportOptions {
    pub arch: ArchKind,
    pub format: ExportFormat,
    pub quant_ft: QuantScheme,
    pub quant_h1: QuantScheme,
    pub quant_h2: QuantScheme,
    pub quant_out: QuantScheme,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            arch: ArchKind::Single,
            format: ExportFormat::Fp32,
            quant_ft: QuantScheme::PerTensor,
            quant_h1: QuantScheme::PerChannel,
            quant_h2: QuantScheme::PerChannel,
            quant_out: QuantScheme::PerChannel,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Sample {
    pub features: Vec<u32>,
    pub label: f32,
    pub weight: f32,
    pub cp: Option<i32>,
    pub phase: Option<GamePhase>,
}
