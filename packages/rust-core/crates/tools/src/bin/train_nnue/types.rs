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
    #[serde(default)]
    pub teacher_cp: Option<i32>,
    #[serde(default)]
    pub teacher_score: Option<TeacherScore>,
    #[serde(default)]
    pub teacher_weight: Option<f32>,
}

#[derive(Debug, Deserialize)]
pub struct LineInfo {
    #[serde(default)]
    pub score_cp: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct TeacherScore {
    #[serde(rename = "type")]
    pub kind: Option<String>, // "cp" | "mate"
    pub value: Option<i32>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Config {
    pub epochs: usize,
    pub batch_size: usize,
    pub learning_rate: f32,
    pub optimizer: String,
    pub l2_reg: f32,
    pub label_type: String,
    pub mu: f32,
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
    pub grad_clip: f32,
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
    #[clap(name = "huber", alias = "smoothl1")]
    Huber,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TeacherKind {
    Single,
    ClassicFp32,
}

#[derive(Clone, Debug)]
pub struct DistillOptions {
    pub teacher_path: Option<PathBuf>,
    pub teacher_kind: TeacherKind,
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
    pub teacher_scale_fit: TeacherScaleFitKind,
    pub huber_delta: f32,
    pub layer_weight_ft: f32,
    pub layer_weight_h1: f32,
    pub layer_weight_h2: f32,
    pub layer_weight_out: f32,
    pub teacher_batch_size: usize,
    pub teacher_cache: Option<PathBuf>,
}

impl Default for DistillOptions {
    fn default() -> Self {
        Self {
            teacher_path: None,
            teacher_kind: TeacherKind::Single,
            loss: DistillLossKind::Mse,
            temperature: 1.0,
            alpha: 1.0,
            scale_temp2: false,
            soften_student: false,
            seed: None,
            teacher_domain: TeacherValueDomain::Cp,
            teacher_scale_fit: TeacherScaleFitKind::None,
            huber_delta: 1.0,
            layer_weight_ft: 0.0,
            layer_weight_h1: 0.0,
            layer_weight_h2: 0.0,
            layer_weight_out: 1.0,
            teacher_batch_size: 256,
            teacher_cache: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
pub enum TeacherValueDomain {
    #[clap(name = "cp")]
    Cp,
    #[clap(name = "wdl-logit")]
    WdlLogit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum TeacherScaleFitKind {
    #[clap(name = "none")]
    None,
    #[clap(name = "linear")]
    Linear,
}

impl DistillOptions {
    pub fn requires_teacher_layers(&self) -> bool {
        self.layer_weight_ft > 0.0 || self.layer_weight_h1 > 0.0 || self.layer_weight_h2 > 0.0
    }
}

#[derive(Clone, Debug)]
pub struct ExportOptions {
    pub arch: ArchKind,
    pub format: ExportFormat,
    pub quant_ft: QuantScheme,
    pub quant_h1: QuantScheme,
    pub quant_h2: QuantScheme,
    pub quant_out: QuantScheme,
    /// Classic v1 export時に FP32 版も同時に書き出すか（Classic 以外では無視）
    pub emit_fp32_also: bool,
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
            emit_fp32_also: false,
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
