//! NNUE (Efficiently Updatable Neural Network) trainer
//!
//! このバイナリは JSONL/NFC キャッシュから NNUE モデルを学習し、各種フォーマットで出力します。

pub(crate) mod classic;
pub(crate) mod dataset;
pub(crate) mod distill;
pub(crate) mod error_messages;
pub(crate) mod export;
pub(crate) mod logging;
pub(crate) mod model;
pub(crate) mod params;
pub(crate) mod teacher;
pub(crate) mod training;
pub(crate) mod types;

use std::fs::{create_dir_all, File};
use std::io::Write;
use std::path::PathBuf;
use std::time::{Instant, SystemTime};

use chrono::Utc;
use clap::parser::ValueSource;
use clap::{arg, value_parser, Arg, ArgAction, Command, ValueHint};
use engine_core::evaluation::nnue::features::FE_END;
use engine_core::shogi::SHOGI_BOARD_SIZE;
use model::{ClassicNetwork, Network};
use rand::rngs::StdRng;
use rand::SeedableRng;
use tools::common::weighting as wcfg;

use classic::{ClassicIntNetworkBundle, ClassicQuantizationScales};
use dataset::{load_samples, load_samples_from_cache};
use distill::{
    distill_classic_after_training, evaluate_distill, evaluate_quantization_gap,
    DistillEvalMetrics, QuantCalibration, QuantEvalMetrics,
};
use error_messages::*;
use export::{finalize_export, save_network, FinalizeExportParams};
use logging::StructuredLogger;
use params::{
    CLASSIC_ACC_DIM, CLASSIC_H1_DIM, CLASSIC_H2_DIM, CLASSIC_RELU_CLIP, CLASSIC_RELU_CLIP_F32,
    DEFAULT_ACC_DIM, DEFAULT_RELU_CLIP, MAX_PREFETCH_BATCHES,
};
use teacher::load_teacher;
use training::{
    compute_val_auc, compute_val_auc_and_ece, train_model, train_model_stream_cache,
    train_model_with_loader, DashboardOpts, LrPlateauState, TrainContext, TrainTrackers,
};
use types::{
    ArchKind, Config, DistillLossKind, DistillOptions, ExportFormat, ExportOptions, QuantScheme,
    Sample, TeacherKind, TeacherScaleFitKind,
};

use crate::types::TeacherValueDomain;

fn resolve_quant_calibration<'a>(
    file_samples: Option<&'a [Sample]>,
    train_samples: &'a [Sample],
    limit: usize,
    quant_search: bool,
) -> Option<QuantCalibration<'a>> {
    if let Some(samples) = file_samples {
        if !samples.is_empty() {
            return Some(QuantCalibration {
                samples,
                limit,
                auto_search: quant_search,
            });
        }
    }
    if quant_search && !train_samples.is_empty() {
        Some(QuantCalibration {
            samples: train_samples,
            limit,
            auto_search: true,
        })
    } else {
        None
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_secs()
        .try_init();

    let app = Command::new("train_nnue")
        .about("Train NNUE model from JSONL data")
        .arg(arg!(-i --input <FILE> "Input JSONL file").required(true))
        .arg(arg!(-v --validation <FILE> "Validation JSONL file"))
        // Weighting (Spec #12)
        .arg(
            Arg::new("config")
                .long("config")
                .help("YAML/JSON config file for weighting and presets")
                .value_hint(ValueHint::FilePath),
        )
        .arg(
            Arg::new("weighting")
                .long("weighting")
                .help("Enable weighting scheme(s): exact|gap|phase|mate (repeatable)")
                .action(ArgAction::Append)
                .value_parser(["exact", "gap", "phase", "mate"]) ,
        )
        .arg(
            Arg::new("w-exact")
                .long("w-exact")
                .help("Coefficient for exact samples")
                .value_parser(value_parser!(f32)),
        )
        .arg(
            Arg::new("w-gap")
                .long("w-gap")
                .help("Coefficient for small-gap samples")
                .value_parser(value_parser!(f32)),
        )
        .arg(
            Arg::new("w-phase-endgame")
                .long("w-phase-endgame")
                .help("Coefficient for endgame phase")
                .value_parser(value_parser!(f32)),
        )
        .arg(
            Arg::new("w-mate-ring")
                .long("w-mate-ring")
                .help("Coefficient for mate-ring samples")
                .value_parser(value_parser!(f32)),
        )
        .arg(arg!(-e --epochs <N> "Number of epochs").default_value("2"))
        .arg(arg!(-b --"batch-size" <N> "Batch size").default_value("8192"))
        .arg(arg!(--lr <RATE> "Learning rate").default_value("0.001"))
        .arg(arg!(--opt <TYPE> "Optimizer: sgd, adam, adamw").default_value("adam"))
        .arg(arg!(--l2 <RATE> "L2 regularization").default_value("0.000001"))
        .arg(
            arg!(--"grad-clip" <N> "Global gradient norm clip (0 disables)")
                .value_parser(clap::value_parser!(f32))
                .default_value("0.0"),
        )
        .arg(
            arg!(-l --label <TYPE> "Label type: wdl, cp")
                .value_parser(["wdl", "cp"]) // strict accepted values
                .default_value("wdl"),
        )
        .arg(
            arg!(--scale <N> "Scale for cp->wdl conversion")
                .value_parser(clap::value_parser!(f32))
                .default_value("600"),
        )
        .arg(
            arg!(--mu <N> "Offset mu for cp->wdl conversion")
                .value_parser(clap::value_parser!(f32))
                .default_value("0"),
        )
        .arg(
            arg!(--"cp-clip" <N> "Clip CP values to this range")
                .value_parser(clap::value_parser!(i32).range(0..))
                .default_value("1200"),
        )
        .arg(arg!(--"acc-dim" <N> "Accumulator dimension").default_value(DEFAULT_ACC_DIM))
        .arg(arg!(--"relu-clip" <N> "ReLU clipping value").default_value(DEFAULT_RELU_CLIP))
        .arg(
            Arg::new("arch")
                .long("arch")
                .help("Training architecture: single or classic")
                .value_parser(clap::value_parser!(ArchKind))
                .default_value("single"),
        )
        .arg(
            Arg::new("export-format")
                .long("export-format")
                .help("Export format: fp32|single-i8|classic-v1")
                .value_parser(clap::value_parser!(ExportFormat))
                .default_value("fp32"),
        )
        .arg(
            Arg::new("quant-ft")
                .long("quant-ft")
                .help("Quantization scheme for feature transformer weights (Classic v1: per-tensor only)")
                .value_parser(clap::value_parser!(QuantScheme))
                .default_value("per-tensor"),
        )
        .arg(
            Arg::new("quant-h1")
                .long("quant-h1")
                .help("Quantization scheme for hidden layer 1 weights")
                .value_parser(clap::value_parser!(QuantScheme))
                .default_value("per-channel"),
        )
        .arg(
            Arg::new("quant-h2")
                .long("quant-h2")
                .help("Quantization scheme for hidden layer 2 weights")
                .value_parser(clap::value_parser!(QuantScheme))
                .default_value("per-channel"),
        )
        .arg(
            Arg::new("quant-out")
                .long("quant-out")
                .help("Quantization scheme for output layer weights (Classic v1: per-tensor only)")
                .value_parser(clap::value_parser!(QuantScheme))
                .default_value("per-tensor"),
        )
        .arg(
            Arg::new("quant-calibration")
                .long("quant-calibration")
                .help("Classic 量子化校正に使用するサンプルファイル (JSONL または cache)。複数指定可")
                .value_hint(ValueHint::FilePath)
                .num_args(1..)
                .action(ArgAction::Append),
        )
        .arg(
            Arg::new("quant-calibration-limit")
                .long("quant-calibration-limit")
                .help("量子化校正に使用する最大サンプル数")
                .value_parser(clap::value_parser!(usize))
                .default_value("40960"),
        )
        .arg(
            Arg::new("quant-search")
                .long("quant-search")
                .help("Classic 量子化で per-tensor / per-channel の組み合わせを校正セットで自動探索")
                .action(ArgAction::SetTrue),
        )
        .arg(
            arg!(--"emit-fp32-also" "Also export Classic FP32 weights when exporting classic-v1 (ignored otherwise)")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("final-cp-gain")
                .long("final-cp-gain")
                .help("Classic v1 export only: multiply final output layer (weights/bias) by this gain to align Q16->cp display range (default: 1.0)")
                .value_parser(clap::value_parser!(f32))
                .default_value("1.0"),
        )
        .arg(
            Arg::new("distill-from-single")
                .long("distill-from-single")
                .help("Path to teacher Single FP32 weights for knowledge distillation")
                .value_hint(ValueHint::FilePath)
                .conflicts_with("distill-from-classic"),
        )
        .arg(
            Arg::new("distill-from-classic")
                .long("distill-from-classic")
                .help("Path to teacher Classic FP32 network (nn.fp32.bin)")
                .value_hint(ValueHint::FilePath)
                .conflicts_with("distill-from-single"),
        )
        .arg(
            Arg::new("teacher-domain")
                .long("teacher-domain")
                .help("Teacher output domain: cp|wdl-logit (default: wdl-logit for all current teachers)")
                .value_parser(clap::value_parser!(TeacherValueDomain)),
        )
        .arg(
            Arg::new("teacher-scale-fit")
                .long("teacher-scale-fit")
                .help("Teacher scale fitting: none|linear (default: none)")
                .value_parser(clap::value_parser!(TeacherScaleFitKind))
                .default_value("none"),
        )
        .arg(
            Arg::new("kd-loss")
                .long("kd-loss")
                .help("Knowledge distillation loss: mse|bce|kl|huber")
                .value_parser(clap::value_parser!(DistillLossKind))
                .default_value("mse"),
        )
        .arg(
            Arg::new("kd-temperature")
                .long("kd-temperature")
                .help("Knowledge distillation softmax temperature")
                .value_parser(clap::value_parser!(f32))
                .default_value("1.0"),
        )
        .arg(
            Arg::new("kd-alpha")
                .long("kd-alpha")
                .help("Knowledge distillation blending coefficient (teacher weight)")
                .value_parser(clap::value_parser!(f32))
                .default_value("1.0"),
        )
        .arg(
            Arg::new("kd-loss-scale-temp2")
                .long("kd-loss-scale-temp2")
                .help("Scale distillation teacher loss/gradient by (temperature)^2 (WDL distillation only;推奨: --kd-soften-student と併用)")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("kd-soften-student")
                .long("kd-soften-student")
                .help("教師BCE/KLで生徒ロジットも温度Tで割る (WDL distillationのみ) ")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("kd-huber-delta")
                .long("kd-huber-delta")
                .help("Huber delta for --kd-loss=huber (SmoothL1)。cp/wdl 共用")
                .value_parser(clap::value_parser!(f32))
                .default_value("1.0"),
        )
        .arg(
            Arg::new("kd-layer-weight-ft")
                .long("kd-layer-weight-ft")
                .help("Feature transformer layer loss weight λ_ft")
                .value_parser(clap::value_parser!(f32))
                .default_value("0.0"),
        )
        .arg(
            Arg::new("kd-layer-weight-h1")
                .long("kd-layer-weight-h1")
                .help("Hidden1 layer loss weight λ_h1")
                .value_parser(clap::value_parser!(f32))
                .default_value("0.0"),
        )
        .arg(
            Arg::new("kd-layer-weight-h2")
                .long("kd-layer-weight-h2")
                .help("Hidden2 layer loss weight λ_h2")
                .value_parser(clap::value_parser!(f32))
                .default_value("0.0"),
        )
        .arg(
            Arg::new("kd-layer-weight-out")
                .long("kd-layer-weight-out")
                .help("Output layer loss weight λ_out (既定値=1.0)")
                .value_parser(clap::value_parser!(f32))
                .default_value("1.0"),
        )
        .arg(
            Arg::new("teacher-batch-size")
                .long("teacher-batch-size")
                .help("Teacher inference batch size for distillation")
                .value_parser(clap::value_parser!(usize))
                .default_value("256"),
        )
        .arg(
            Arg::new("teacher-cache")
                .long("teacher-cache")
                .help("Persist teacher distillation cache to this path (bincode). In-memory cacheは常時 ON")
                .value_hint(ValueHint::FilePath),
        )
        .arg(
            Arg::new("distill-only")
                .long("distill-only")
                .help("Classic distillation のみ実行 (学習をスキップ)。--arch classic --export-format classic-v1 および --distill-from-<single|classic> のいずれかが必要")
                .action(ArgAction::SetTrue),
        )
        .arg(arg!(--shuffle "Shuffle training data"))
        .arg(arg!(--"exclude-no-legal-move" "Exclude positions with no legal moves (JSONL input)"))
        .arg(arg!(--"exclude-fallback" "Exclude positions where fallback was used (JSONL input)"))
        .arg(arg!(--"save-every" <N> "Save checkpoint every N batches"))
        .arg(arg!(--"stream-cache" "Stream cache input without preloading (disables shuffle)"))
        .arg(
            arg!(--"prefetch-batches" <N> "Async prefetch queue depth (cache/stream-cache input)")
                .default_value("2")
                .value_parser(clap::value_parser!(usize)),
        )
        .arg(
            arg!(--"throughput-interval" <SECS> "Seconds between throughput reports")
                .default_value("2.0")
                .value_parser(clap::value_parser!(f32)),
        )
        .arg(
            arg!(--"prefetch-bytes" <BYTES> "Approximate memory cap for prefetched batches (bytes)")
                .value_parser(clap::value_parser!(usize))
        )
        .arg(
            arg!(--"estimated-features-per-sample" <N> "Estimated active features per sample (for prefetch memory cap)")
                .default_value("64")
                .value_parser(clap::value_parser!(usize))
        )
        .arg(arg!(--metrics "Emit per-epoch metrics CSV").action(clap::ArgAction::SetTrue))
        .arg(
            arg!(--"calibration-bins" <N> "Bins for cp calibration (JSONL validation)")
                .value_parser(clap::value_parser!(usize))
                .default_value("40"),
        )
        .arg(
            arg!(--"plots" "Emit PNG plots (requires features=plots)")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            arg!(--"gate-val-loss-non-increase" "Fail if best val_loss not at last epoch")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            arg!(--"gate-min-auc" <N> "Minimum AUC to pass (wdl only)")
                .value_parser(clap::value_parser!(f64)),
        )
        .arg(
            arg!(--"gate-mode" <MODE> "Gate behavior")
                .value_parser(["warn", "fail"]) 
                .default_value("warn"),
        )
        .arg(
            Arg::new("gate-distill-cp-mae")
                .long("gate-distill-cp-mae")
                .value_parser(clap::value_parser!(f32))
                .help("Maximum allowed MAE (cp) between teacher Single FP32 and distilled Classic FP32"),
        )
        .arg(
            Arg::new("gate-distill-cp-p95")
                .long("gate-distill-cp-p95")
                .value_parser(clap::value_parser!(f32))
                .help("Maximum allowed 95th percentile (cp) between teacher Single FP32 and distilled Classic FP32"),
        )
        .arg(
            Arg::new("gate-distill-logit-mae")
                .long("gate-distill-logit-mae")
                .value_parser(clap::value_parser!(f32))
                .help("Maximum allowed MAE (logit) between teacher Single FP32 and distilled Classic FP32 (WDL only)"),
        )
        .arg(
            Arg::new("gate-classic-int-cp-mae")
                .long("gate-classic-int-cp-mae")
                .value_parser(clap::value_parser!(f32))
                .help("Maximum allowed MAE (cp) between Classic FP32 and Classic INT inference"),
        )
        .arg(
            Arg::new("gate-classic-int-cp-p95")
                .long("gate-classic-int-cp-p95")
                .value_parser(clap::value_parser!(f32))
                .help("Maximum allowed 95th percentile (cp) between Classic FP32 and Classic INT inference"),
        )
        .arg(
            Arg::new("gate-classic-int-logit-mae")
                .long("gate-classic-int-logit-mae")
                .value_parser(clap::value_parser!(f32))
                .help("Maximum allowed MAE (logit) between Classic FP32 and Classic INT inference (WDL only)"),
        )
        // LR scheduler (Spec #11)
        .arg(
            arg!(--"lr-schedule" <KIND> "LR scheduler: constant|step|cosine")
                .value_parser(["constant", "step", "cosine"]) 
                .default_value("constant"),
        )
        .arg(
            arg!(--"lr-warmup-epochs" <N> "Warmup epochs for LR")
                .value_parser(clap::value_parser!(u32))
                .default_value("0"),
        )
        .arg(
            arg!(--"lr-decay-epochs" <N> "Decay interval in epochs (step/cosine)")
                .value_parser(clap::value_parser!(u32))
                .conflicts_with("lr-decay-steps"),
        )
        .arg(
            arg!(--"lr-decay-steps" <N> "Decay interval in steps (step/cosine)")
                .value_parser(clap::value_parser!(u64))
                .conflicts_with("lr-decay-epochs"),
        )
        .arg(
            arg!(--"lr-plateau-patience" <N> "Plateau patience in epochs (overlay; requires --validation)")
                .value_parser(clap::value_parser!(u32)),
        )
        .arg(
            arg!(--"structured-log" <PATH> "Structured JSONL log path ('-' for STDOUT)")
        )
        .arg(arg!(--quantized "Save quantized (int8) version of the model"))
        .arg(
            arg!(--seed <SEED> "Random seed for reproducibility")
                .visible_alias("rng-seed"),
        )
        .arg(arg!(-o --out <DIR> "Output directory"))
        .get_matches();

    // Prepare structured logger early for stdout/stderr routing decisions
    let mut structured_logger: Option<StructuredLogger> = app
        .get_one::<String>("structured-log")
        .and_then(|p| match StructuredLogger::new(p) {
            Ok(lg) => Some(lg),
            Err(e) => {
                eprintln!("Warning: failed to open structured log '{}': {}", p, e);
                None
            }
        });
    let human_to_stderr = structured_logger.as_ref().map(|lg| lg.to_stdout).unwrap_or(false);

    // Build weighting config (Spec #12)
    let cfg_file = app.get_one::<String>("config").and_then(|p| match wcfg::load_config_file(p) {
        Ok(v) => Some(v),
        Err(e) => {
            if human_to_stderr {
                eprintln!("Warning: failed to load config '{}': {}", p, e);
            } else {
                println!("Warning: failed to load config '{}': {}", p, e);
            }
            None
        }
    });
    let cli_active = app.get_many::<String>("weighting").map(|vals| {
        vals.map(|s| match s.as_str() {
            "exact" => wcfg::WeightingKind::Exact,
            "gap" => wcfg::WeightingKind::Gap,
            "phase" => wcfg::WeightingKind::Phase,
            "mate" => wcfg::WeightingKind::Mate,
            _ => unreachable!(),
        })
        .collect::<Vec<_>>()
    });
    let weighting_cfg = wcfg::merge_config(
        cfg_file,
        cli_active,
        app.get_one::<f32>("w-exact").copied(),
        app.get_one::<f32>("w-gap").copied(),
        app.get_one::<f32>("w-phase-endgame").copied(),
        app.get_one::<f32>("w-mate-ring").copied(),
    );

    let arch = *app.get_one::<ArchKind>("arch").unwrap_or(&ArchKind::Single);
    let export_format =
        *app.get_one::<ExportFormat>("export-format").unwrap_or(&ExportFormat::Fp32);
    let quant_ft = *app.get_one::<QuantScheme>("quant-ft").unwrap_or(&QuantScheme::PerTensor);
    let quant_h1 = *app.get_one::<QuantScheme>("quant-h1").unwrap_or(&QuantScheme::PerChannel);
    let quant_h2 = *app.get_one::<QuantScheme>("quant-h2").unwrap_or(&QuantScheme::PerChannel);
    let quant_out = *app.get_one::<QuantScheme>("quant-out").unwrap_or(&QuantScheme::PerTensor);
    let quant_calibration_paths: Vec<String> = app
        .get_many::<String>("quant-calibration")
        .map(|vals| vals.map(|s| s.to_owned()).collect())
        .unwrap_or_default();
    let quant_calibration_limit =
        *app.get_one::<usize>("quant-calibration-limit").unwrap_or(&40960usize);
    let quant_search = app.get_flag("quant-search");
    let label_type_value = app.get_one::<String>("label").unwrap().to_string();
    let distill_teacher_single = app.get_one::<String>("distill-from-single").map(PathBuf::from);
    let distill_teacher_classic = app.get_one::<String>("distill-from-classic").map(PathBuf::from);
    if distill_teacher_single.is_some() && distill_teacher_classic.is_some() {
        return Err("cannot specify both --distill-from-single and --distill-from-classic".into());
    }
    let (distill_teacher_path, distill_teacher_kind) = if let Some(path) = distill_teacher_single {
        (Some(path), TeacherKind::Single)
    } else if let Some(path) = distill_teacher_classic {
        (Some(path), TeacherKind::ClassicFp32)
    } else {
        (None, TeacherKind::Single)
    };
    let kd_loss_source = app.value_source("kd-loss");
    let kd_temp_source = app.value_source("kd-temperature");
    let kd_alpha_source = app.value_source("kd-alpha");
    let kd_scale_temp2 = app.get_flag("kd-loss-scale-temp2");
    let kd_soften_student = app.get_flag("kd-soften-student");

    let mut distill_loss =
        *app.get_one::<DistillLossKind>("kd-loss").unwrap_or(&DistillLossKind::Mse);
    let mut distill_temperature = *app.get_one::<f32>("kd-temperature").unwrap();
    let mut distill_alpha = *app.get_one::<f32>("kd-alpha").unwrap();

    if arch == ArchKind::Classic && export_format == ExportFormat::ClassicV1 {
        if distill_teacher_path.is_none() {
            return Err(ERR_CLASSIC_NEEDS_TEACHER.into());
        }
        if kd_loss_source == Some(ValueSource::DefaultValue) {
            distill_loss = DistillLossKind::Mse;
        }
        if kd_temp_source == Some(ValueSource::DefaultValue) && label_type_value == "wdl" {
            distill_temperature = 2.0;
        }
        if kd_alpha_source == Some(ValueSource::DefaultValue) {
            distill_alpha = 1.0;
        }
    }

    let mut config = Config {
        epochs: app.get_one::<String>("epochs").unwrap().parse()?,
        batch_size: app.get_one::<String>("batch-size").unwrap().parse()?,
        learning_rate: app.get_one::<String>("lr").unwrap().parse()?,
        optimizer: app.get_one::<String>("opt").unwrap().to_string(),
        l2_reg: app.get_one::<String>("l2").unwrap().parse()?,
        label_type: label_type_value.clone(),
        mu: *app.get_one::<f32>("mu").unwrap_or(&0.0),
        scale: *app.get_one::<f32>("scale").unwrap(),
        cp_clip: *app.get_one::<i32>("cp-clip").unwrap(),
        accumulator_dim: app.get_one::<String>("acc-dim").unwrap().parse()?,
        relu_clip: app.get_one::<String>("relu-clip").unwrap().parse()?,
        shuffle: app.get_flag("shuffle"),
        prefetch_batches: *app.get_one::<usize>("prefetch-batches").unwrap(),
        throughput_interval_sec: *app.get_one::<f32>("throughput-interval").unwrap(),
        stream_cache: app.get_flag("stream-cache"),
        prefetch_bytes: app.get_one::<usize>("prefetch-bytes").copied(),
        estimated_features_per_sample: *app
            .get_one::<usize>("estimated-features-per-sample")
            .unwrap(),
        exclude_no_legal_move: app.get_flag("exclude-no-legal-move"),
        exclude_fallback: app.get_flag("exclude-fallback"),
        lr_schedule: app
            .get_one::<String>("lr-schedule")
            .map(|s| s.to_string())
            .unwrap_or_else(|| "constant".to_string()),
        lr_warmup_epochs: *app.get_one::<u32>("lr-warmup-epochs").unwrap_or(&0u32),
        lr_decay_epochs: app.get_one::<u32>("lr-decay-epochs").copied(),
        lr_decay_steps: app.get_one::<u64>("lr-decay-steps").copied(),
        lr_plateau_patience: app.get_one::<u32>("lr-plateau-patience").copied(),
        grad_clip: *app.get_one::<f32>("grad-clip").unwrap(),
    };

    let mut export_options = ExportOptions {
        arch,
        format: export_format,
        quant_ft,
        quant_h1,
        quant_h2,
        quant_out,
        emit_fp32_also: app.get_flag("emit-fp32-also"),
    };
    let final_cp_gain: f32 = *app.get_one::<f32>("final-cp-gain").unwrap_or(&1.0);
    // Teacher domain (cp or wdl-logit)。教師種別に応じて既定値を選ぶ。
    let default_teacher_domain = default_teacher_domain(distill_teacher_kind);
    let teacher_domain = app
        .get_one::<TeacherValueDomain>("teacher-domain")
        .copied()
        .unwrap_or(default_teacher_domain);

    let mut distill_options = DistillOptions {
        teacher_path: distill_teacher_path.clone(),
        teacher_kind: distill_teacher_kind,
        loss: distill_loss,
        temperature: distill_temperature,
        alpha: distill_alpha,
        scale_temp2: kd_scale_temp2,
        soften_student: kd_soften_student,
        seed: None,
        teacher_domain,
        teacher_scale_fit: *app
            .get_one::<TeacherScaleFitKind>("teacher-scale-fit")
            .unwrap_or(&TeacherScaleFitKind::None),
        huber_delta: *app.get_one::<f32>("kd-huber-delta").unwrap_or(&1.0),
        layer_weight_ft: *app.get_one::<f32>("kd-layer-weight-ft").unwrap_or(&0.0),
        layer_weight_h1: *app.get_one::<f32>("kd-layer-weight-h1").unwrap_or(&0.0),
        layer_weight_h2: *app.get_one::<f32>("kd-layer-weight-h2").unwrap_or(&0.0),
        layer_weight_out: *app.get_one::<f32>("kd-layer-weight-out").unwrap_or(&1.0),
        teacher_batch_size: *app.get_one::<usize>("teacher-batch-size").unwrap_or(&256usize),
        teacher_cache: app.get_one::<String>("teacher-cache").map(PathBuf::from),
    };

    let seed_u64_opt: Option<u64> =
        app.get_one::<String>("seed").and_then(|s| s.parse::<u64>().ok());
    let distill_seed = seed_u64_opt.map(|s| s ^ 0xC1A5_51C0_5EED_u64);
    distill_options.seed = distill_seed;

    if matches!(distill_options.loss, DistillLossKind::Huber) && distill_options.huber_delta <= 0.0
    {
        return Err("--kd-huber-delta は 0 より大きい値を指定してください".into());
    }
    if matches!(distill_options.loss, DistillLossKind::Bce | DistillLossKind::Kl)
        && distill_options.requires_teacher_layers()
    {
        return Err("--kd-loss=bce/kl とレイヤ別ロス(λ_ft/λ_h1/λ_h2)は同時に指定できません".into());
    }
    if distill_options.layer_weight_out < 0.0
        || distill_options.layer_weight_ft < 0.0
        || distill_options.layer_weight_h1 < 0.0
        || distill_options.layer_weight_h2 < 0.0
    {
        return Err("レイヤ別ロス係数 λ_* は 0 以上を指定してください".into());
    }
    if distill_options.teacher_batch_size == 0 {
        return Err("--teacher-batch-size は 1 以上を指定してください".into());
    }

    if arch == ArchKind::Classic && config.relu_clip != CLASSIC_RELU_CLIP {
        if human_to_stderr {
            eprintln!(
                "Warning: Classic アーキの relu_clip は 127 固定です (--relu-clip={} → 127 に上書き)",
                config.relu_clip
            );
        } else {
            println!(
                "Warning: Classic アーキの relu_clip は 127 固定です (--relu-clip={} → 127 に上書き)",
                config.relu_clip
            );
        }
        config.relu_clip = CLASSIC_RELU_CLIP;
    }

    if config.scale <= 0.0 {
        return Err("Invalid --scale: must be > 0".into());
    }
    if config.throughput_interval_sec <= 0.0 {
        return Err("Invalid --throughput-interval: must be > 0".into());
    }
    if export_options.arch == ArchKind::Single
        && matches!(export_options.format, ExportFormat::ClassicV1)
    {
        return Err(ERR_SINGLE_NO_CLASSIC_V1.into());
    }
    if export_options.arch == ArchKind::Classic
        && matches!(export_options.format, ExportFormat::SingleI8)
    {
        return Err(ERR_CLASSIC_NO_SINGLE_I8.into());
    }
    if export_options.arch == ArchKind::Classic
        && export_options.quant_ft == QuantScheme::PerChannel
    {
        return Err(ERR_CLASSIC_FT_PER_CHANNEL.into());
    }
    if export_options.arch == ArchKind::Classic
        && export_options.quant_out == QuantScheme::PerChannel
    {
        return Err(ERR_CLASSIC_OUT_PER_CHANNEL.into());
    }

    let distill_only_flag = app.get_flag("distill-only");
    let distill_only = if distill_only_flag {
        if arch != ArchKind::Classic {
            return Err("--distill-only を使用するには --arch classic を指定してください".into());
        }
        if export_format != ExportFormat::ClassicV1 {
            return Err(
                "--distill-only を使用するには --export-format classic-v1 を指定してください"
                    .into(),
            );
        }
        if distill_options.teacher_path.is_none() {
            return Err(ERR_CLASSIC_NEEDS_TEACHER.into());
        }
        true
    } else {
        false
    };
    if distill_options.temperature <= 0.0 {
        return Err("--kd-temperature must be > 0".into());
    }
    if config.prefetch_batches > MAX_PREFETCH_BATCHES {
        return Err(format!("Invalid --prefetch-batches: must be <= {MAX_PREFETCH_BATCHES}").into());
    }
    if let Some(0) = config.lr_decay_epochs {
        eprintln!("Error: --lr-decay-epochs must be > 0");
        std::process::exit(2);
    }
    if let Some(0) = config.lr_decay_steps {
        eprintln!("Error: --lr-decay-steps must be > 0");
        std::process::exit(2);
    }

    let input_path = app.get_one::<String>("input").unwrap();
    let validation_path = app.get_one::<String>("validation");
    let emit_metrics = app.get_flag("metrics");
    let calib_bins_n = *app.get_one::<usize>("calibration-bins").unwrap_or(&40usize);
    let do_plots = app.get_flag("plots");
    let gate_last_epoch_best = app.get_flag("gate-val-loss-non-increase");
    let gate_min_auc = app.get_one::<f64>("gate-min-auc").copied();
    let gate_mode_fail = app.get_one::<String>("gate-mode").map(|s| s == "fail").unwrap_or(false);
    let gate_distill_cp_mae = app.get_one::<f32>("gate-distill-cp-mae").copied();
    let gate_distill_cp_p95 = app.get_one::<f32>("gate-distill-cp-p95").copied();
    let gate_distill_logit_mae = app.get_one::<f32>("gate-distill-logit-mae").copied();
    let gate_classic_int_cp_mae = app.get_one::<f32>("gate-classic-int-cp-mae").copied();
    let gate_classic_int_cp_p95 = app.get_one::<f32>("gate-classic-int-cp-p95").copied();
    let gate_classic_int_logit_mae = app.get_one::<f32>("gate-classic-int-logit-mae").copied();
    let save_every: Option<usize> =
        app.get_one::<String>("save-every").map(|s| s.parse()).transpose()?;

    let timestamp = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs();
    let out_dir = app
        .get_one::<String>("out")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("runs/nnue_{}", timestamp)));

    if human_to_stderr {
        eprintln!("Configuration:");
    } else {
        println!("Configuration:");
    }
    if human_to_stderr {
        eprintln!("  Input: {}", input_path);
    } else {
        println!("  Input: {}", input_path);
    }
    if let Some(val_path) = validation_path {
        if human_to_stderr {
            eprintln!("  Validation: {}", val_path);
        } else {
            println!("  Validation: {}", val_path);
        }
    }
    if human_to_stderr {
        eprintln!("  Output: {}", out_dir.display());
    } else {
        println!("  Output: {}", out_dir.display());
    }
    if human_to_stderr {
        eprintln!("  Settings: {:?}", config);
    } else {
        println!("  Settings: {:?}", config);
    }
    if human_to_stderr {
        eprintln!(
            "  Export: arch={:?}, format={:?}, q_ft={:?}, q_h1={:?}, q_h2={:?}, q_out={:?}",
            export_options.arch,
            export_options.format,
            export_options.quant_ft,
            export_options.quant_h1,
            export_options.quant_h2,
            export_options.quant_out
        );
        if export_options.arch == ArchKind::Classic
            && matches!(export_options.format, ExportFormat::ClassicV1)
        {
            eprintln!("  FinalCPGain: {:.3}", final_cp_gain);
        }
    } else {
        println!(
            "  Export: arch={:?}, format={:?}, q_ft={:?}, q_h1={:?}, q_h2={:?}, q_out={:?}",
            export_options.arch,
            export_options.format,
            export_options.quant_ft,
            export_options.quant_h1,
            export_options.quant_h2,
            export_options.quant_out
        );
        if export_options.arch == ArchKind::Classic
            && matches!(export_options.format, ExportFormat::ClassicV1)
        {
            println!("  FinalCPGain: {:.3}", final_cp_gain);
        }
    }
    if human_to_stderr {
        match &distill_options.teacher_path {
            Some(path) => eprintln!(
                "  Distill: teacher={:?}, kind={:?}, domain={:?}, loss={:?}, temp={}, alpha={}, scale_temp2={}, soften_student={}, scale_fit={:?}",
                path,
                distill_options.teacher_kind,
                distill_options.teacher_domain,
                distill_options.loss,
                distill_options.temperature,
                distill_options.alpha,
                distill_options.scale_temp2,
                distill_options.soften_student,
                distill_options.teacher_scale_fit
            ),
            None => eprintln!(
                "  Distill: teacher=None, kind={:?}, domain={:?}, loss={:?}, temp={}, alpha={}, scale_temp2={}, soften_student={}, scale_fit={:?}",
                distill_options.teacher_kind,
                distill_options.teacher_domain,
                distill_options.loss,
                distill_options.temperature,
                distill_options.alpha,
                distill_options.scale_temp2,
                distill_options.soften_student,
                distill_options.teacher_scale_fit
            ),
        }
    } else {
        match &distill_options.teacher_path {
            Some(path) => println!(
                "  Distill: teacher={:?}, kind={:?}, domain={:?}, loss={:?}, temp={}, alpha={}, scale_temp2={}, soften_student={}, scale_fit={:?}",
                path,
                distill_options.teacher_kind,
                distill_options.teacher_domain,
                distill_options.loss,
                distill_options.temperature,
                distill_options.alpha,
                distill_options.scale_temp2,
                distill_options.soften_student,
                distill_options.teacher_scale_fit
            ),
            None => println!(
                "  Distill: teacher=None, kind={:?}, domain={:?}, loss={:?}, temp={}, alpha={}, scale_temp2={}, soften_student={}, scale_fit={:?}",
                distill_options.teacher_kind,
                distill_options.teacher_domain,
                distill_options.loss,
                distill_options.temperature,
                distill_options.alpha,
                distill_options.scale_temp2,
                distill_options.soften_student,
                distill_options.teacher_scale_fit
            ),
        }
    }
    if human_to_stderr {
        eprintln!("  Feature dimension (input): {} (HalfKP)", SHOGI_BOARD_SIZE * FE_END);
    } else {
        println!("  Feature dimension (input): {} (HalfKP)", SHOGI_BOARD_SIZE * FE_END);
    }
    let network_desc = match arch {
        ArchKind::Single => {
            format!("{} -> {} -> 1 (Single)", SHOGI_BOARD_SIZE * FE_END, config.accumulator_dim)
        }
        ArchKind::Classic => format!(
            "HALFKP -> {}x2 -> {} -> {} -> 1 (Classic)",
            CLASSIC_ACC_DIM, CLASSIC_H1_DIM, CLASSIC_H2_DIM
        ),
    };
    if human_to_stderr {
        eprintln!("  Network: {}", network_desc);
    } else {
        println!("  Network: {}", network_desc);
    }

    // Decide input mode
    // Robustly detect NNFC cache (raw/gzip/zstd) by attempting to parse the header.
    // This avoids misclassifying compressed caches as JSONL.
    let is_cache = dataset::is_cache_file(input_path);
    if config.stream_cache && !is_cache {
        eprintln!("Warning: --stream-cache was set but input is not a cache file; ignoring.");
    }

    // Load training data only when not streaming
    let mut train_samples: Vec<Sample> = Vec::new();
    if !(is_cache && config.stream_cache) {
        let start_time = Instant::now();
        if human_to_stderr {
            eprintln!("\nLoading training data...");
        } else {
            println!("\nLoading training data...");
        }
        train_samples = if is_cache {
            if human_to_stderr {
                eprintln!("Loading from cache file...");
            } else {
                println!("Loading from cache file...");
            }
            load_samples_from_cache(input_path, &weighting_cfg)?
        } else {
            load_samples(input_path, &config, &weighting_cfg)?
        };
        if human_to_stderr {
            eprintln!(
                "Loaded {} samples in {:.2}s",
                train_samples.len(),
                start_time.elapsed().as_secs_f32()
            );
        } else {
            println!(
                "Loaded {} samples in {:.2}s",
                train_samples.len(),
                start_time.elapsed().as_secs_f32()
            );
        }
    } else {
        if human_to_stderr {
            eprintln!("\nStreaming training data from cache (no preloading)...");
        } else {
            println!("\nStreaming training data from cache (no preloading)...");
        }
        if config.shuffle {
            eprintln!("Note: shuffle is disabled in --stream-cache mode.");
        }
    }

    if distill_only {
        if train_samples.is_empty() {
            if is_cache {
                if human_to_stderr {
                    eprintln!(
                        "Distill-only モード: stream-cache を無効化してキャッシュを読み込みます"
                    );
                } else {
                    println!(
                        "Distill-only モード: stream-cache を無効化してキャッシュを読み込みます"
                    );
                }
                train_samples = load_samples_from_cache(input_path, &weighting_cfg)?;
            } else {
                if human_to_stderr {
                    eprintln!("Distill-only モード: 訓練データをメモリへ読み込みます");
                } else {
                    println!("Distill-only モード: 訓練データをメモリへ読み込みます");
                }
                train_samples = load_samples(input_path, &config, &weighting_cfg)?;
            }
        }
        if train_samples.is_empty() {
            return Err("Distill-only モードにはメモリ上の訓練サンプルが必要です".into());
        }
    }

    if !distill_only
        && export_options.arch == ArchKind::Classic
        && matches!(export_options.format, ExportFormat::ClassicV1)
        && config.stream_cache
        && is_cache
    {
        return Err(ERR_CLASSIC_STREAM_NEEDS_DISTILL.into());
    }

    // Load validation data if provided
    let mut val_is_jsonl = false;
    let validation_samples = if let Some(val_path) = validation_path {
        if human_to_stderr {
            eprintln!("\nLoading validation data...");
        } else {
            println!("\nLoading validation data...");
        }
        let start_val = Instant::now();

        let is_val_cache = dataset::is_cache_file(val_path);
        let samples = if is_val_cache {
            if human_to_stderr {
                eprintln!("Loading validation from cache file...");
            } else {
                println!("Loading validation from cache file...");
            }
            load_samples_from_cache(val_path, &weighting_cfg)?
        } else {
            val_is_jsonl = true;
            load_samples(val_path, &config, &weighting_cfg)?
        };

        if human_to_stderr {
            eprintln!(
                "Loaded {} validation samples in {:.2}s",
                samples.len(),
                start_val.elapsed().as_secs_f32()
            );
        } else {
            println!(
                "Loaded {} validation samples in {:.2}s",
                samples.len(),
                start_val.elapsed().as_secs_f32()
            );
        }
        Some(samples)
    } else {
        None
    };

    let mut quant_calibration_samples: Option<Vec<Sample>> = None;
    if !quant_calibration_paths.is_empty() {
        let mut collected: Vec<Sample> = Vec::new();
        for path in &quant_calibration_paths {
            if collected.len() >= quant_calibration_limit {
                break;
            }
            let remaining = quant_calibration_limit - collected.len();
            let is_calib_cache = dataset::is_cache_file(path);
            let mut chunk = if is_calib_cache {
                if human_to_stderr {
                    eprintln!("Loading quant calibration from cache: {}", path);
                } else {
                    println!("Loading quant calibration from cache: {}", path);
                }
                load_samples_from_cache(path, &weighting_cfg)?
            } else {
                load_samples(path, &config, &weighting_cfg)?
            };
            if chunk.len() > remaining {
                chunk.truncate(remaining);
            }
            collected.extend(chunk);
        }
        if collected.is_empty() {
            if human_to_stderr {
                eprintln!(
                    "Warning: --quant-calibration で指定したファイルから校正サンプルを取得できませんでした"
                );
            } else {
                println!(
                    "Warning: --quant-calibration で指定したファイルから校正サンプルを取得できませんでした"
                );
            }
        } else {
            if human_to_stderr {
                eprintln!(
                    "Loaded {} quant calibration samples (limit {}): {:?}",
                    collected.len(),
                    quant_calibration_limit,
                    quant_calibration_paths
                );
            } else {
                println!(
                    "Loaded {} quant calibration samples (limit {}): {:?}",
                    collected.len(),
                    quant_calibration_limit,
                    quant_calibration_paths
                );
            }
            quant_calibration_samples = Some(collected);
        }
    } else if quant_search {
        if human_to_stderr {
            eprintln!(
                "Warning: --quant-search を指定しましたが --quant-calibration が未指定です。訓練サンプルで校正します"
            );
        } else {
            println!(
                "Warning: --quant-search を指定しましたが --quant-calibration が未指定です。訓練サンプルで校正します"
            );
        }
    }

    let quant_calibration_slice = quant_calibration_samples.as_deref();

    // Initialize RNG with seed if provided
    let mut rng: StdRng = if let Some(seed) = seed_u64_opt {
        if human_to_stderr {
            eprintln!("Using random seed (u64): {}", seed);
        } else {
            println!("Using random seed (u64): {}", seed);
        }
        StdRng::seed_from_u64(seed)
    } else {
        let seed_bytes: [u8; 32] = rand::random();
        let seed_hex = seed_bytes.iter().map(|b| format!("{:02x}", b)).collect::<String>();
        let u64_proj = u64::from_le_bytes(seed_bytes[0..8].try_into().unwrap());
        if human_to_stderr {
            eprintln!("Generated random seed (32B hex): {} | (u64 proj): {}", seed_hex, u64_proj);
        } else {
            println!("Generated random seed (32B hex): {} | (u64 proj): {}", seed_hex, u64_proj);
        }
        StdRng::from_seed(seed_bytes)
    };

    // Initialize network
    let mut network = match arch {
        ArchKind::Single => Network::new_single(config.accumulator_dim, config.relu_clip, &mut rng),
        ArchKind::Classic => {
            Network::new_classic(config.relu_clip, config.estimated_features_per_sample, &mut rng)
        }
    };

    if distill_only {
        if human_to_stderr {
            eprintln!("\nDistilling Classic model...");
        } else {
            println!("\nDistilling Classic model...");
        }
    } else if human_to_stderr {
        eprintln!("\nTraining...");
    } else {
        println!("\nTraining...");
    }
    create_dir_all(&out_dir)?;
    if emit_metrics {
        let mut w = csv::Writer::from_path(out_dir.join("metrics.csv"))?;
        w.write_record([
            "epoch",
            "train_loss",
            "val_loss",
            "val_auc",
            "val_ece",
            "time_sec",
            "train_weight_sum",
            "val_weight_sum",
            "is_best",
        ])?;
        w.flush()?;
        let mut wp = csv::Writer::from_path(out_dir.join("phase_metrics.csv"))?;
        wp.write_record([
            "epoch",
            "phase",
            "count",
            "weighted_count",
            "logloss",
            "brier",
            "accuracy",
            "mae",
            "mse",
        ])?;
        wp.flush()?;
    }

    // Dashboard options
    let dash = DashboardOpts {
        emit: emit_metrics,
        calib_bins_n,
        do_plots,
        val_is_jsonl,
    };

    // structured_logger is already created above

    // Track best/last for gates and best saving
    let mut best_network: Option<Network> = None;
    let mut best_val_loss: f32 = f32::INFINITY;
    let mut last_val_loss: Option<f32> = None;
    let mut best_epoch: Option<usize> = None;

    let mut classic_bundle: Option<ClassicIntNetworkBundle> = None;
    let mut classic_scales: Option<ClassicQuantizationScales> = None;
    let mut distill_metrics: Option<DistillEvalMetrics> = None;
    let mut quant_metrics: Option<QuantEvalMetrics> = None;
    let mut calibration_metrics: Option<QuantEvalMetrics> = None;

    if distill_only {
        let teacher_path = distill_options.teacher_path.as_ref().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, ERR_CLASSIC_NEEDS_TEACHER)
        })?;
        let teacher = load_teacher(teacher_path, distill_options.teacher_kind).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("Failed to load teacher network '{}': {e}", teacher_path.display()),
            )
        })?;

        let artifacts = distill_classic_after_training(
            teacher.as_ref(),
            &train_samples,
            &config,
            &distill_options,
            distill::ClassicDistillConfig::new(
                export_options.quant_ft,
                export_options.quant_h1,
                export_options.quant_h2,
                export_options.quant_out,
                resolve_quant_calibration(
                    quant_calibration_slice,
                    train_samples.as_slice(),
                    quant_calibration_limit,
                    quant_search,
                ),
                structured_logger.as_ref(),
            ),
        )
        .map_err(std::io::Error::other)?;

        let distill::DistillArtifacts {
            classic_fp32,
            bundle_int,
            scales,
            calibration_metrics: cal_metrics,
        } = artifacts;
        calibration_metrics = cal_metrics.clone();

        export_options.quant_ft = scales.scheme.ft;
        export_options.quant_h1 = scales.scheme.h1;
        export_options.quant_h2 = scales.scheme.h2;
        export_options.quant_out = scales.scheme.out;

        let eval_samples_slice: &[Sample] = validation_samples
            .as_ref()
            .filter(|v| !v.is_empty())
            .map(|v| v.as_slice())
            .unwrap_or_else(|| train_samples.as_slice());

        if !eval_samples_slice.is_empty() {
            distill_metrics = Some(evaluate_distill(
                teacher.as_ref(),
                &classic_fp32,
                eval_samples_slice,
                &config,
                distill_options.teacher_domain,
            ));
            quant_metrics = Some(evaluate_quantization_gap(
                &classic_fp32,
                &bundle_int,
                &scales,
                eval_samples_slice,
                &config,
            ));
        }

        // Apply final cp gain (post-quant) if requested
        let mut bundle_mut = bundle_int;
        if export_options.arch == ArchKind::Classic
            && matches!(export_options.format, ExportFormat::ClassicV1)
            && final_cp_gain != 1.0
        {
            let (sw, sb) = bundle_mut.apply_final_cp_gain(final_cp_gain);
            if human_to_stderr {
                eprintln!(
                    "Applied final-cp-gain={:.3} to Classic INT (sat_w={}, sat_b={})",
                    final_cp_gain, sw, sb
                );
            } else {
                println!(
                    "Applied final-cp-gain={:.3} to Classic INT (sat_w={}, sat_b={})",
                    final_cp_gain, sw, sb
                );
            }
        }
        classic_scales = Some(scales);
        classic_bundle = Some(bundle_mut);

        let relu_clip = match &network {
            Network::Classic(existing) => existing.relu_clip,
            _ => CLASSIC_RELU_CLIP_F32,
        };
        network = Network::Classic(ClassicNetwork {
            fp32: classic_fp32,
            relu_clip,
        });
    } else {
        // Training mode dispatch (scope to release borrows when done)
        {
            // Compose training_config for JSONL: include whether phase weighting was actually applied
            let mut training_cfg_json = serde_json::to_value(&weighting_cfg).ok();
            if let Some(obj) = training_cfg_json.as_mut().and_then(|v| v.as_object_mut()) {
                let phase_applied =
                    !is_cache && weighting_cfg.active.contains(&wcfg::WeightingKind::Phase);
                obj.insert("phase_applied".into(), serde_json::json!(phase_applied));
            }

            // Initialize plateau state if configured and validation is present
            let mut plateau_state = None;
            if let Some(pat) = config.lr_plateau_patience {
                if pat > 0 {
                    if validation_samples.is_some() {
                        plateau_state = Some(LrPlateauState::new(pat));
                    } else {
                        // Warn once if plateau requested but no validation
                        if human_to_stderr {
                            eprintln!(
                                "Warning: --lr-plateau-patience specified but no validation data provided; plateau disabled"
                            );
                        } else {
                            println!(
                                "Warning: --lr-plateau-patience specified but no validation data provided; plateau disabled"
                            );
                        }
                    }
                }
            }

            let mut ctx = TrainContext {
                out_dir: &out_dir,
                save_every,
                dash,
                trackers: TrainTrackers {
                    best_network: &mut best_network,
                    best_val_loss: &mut best_val_loss,
                    last_val_loss: &mut last_val_loss,
                    best_epoch: &mut best_epoch,
                },
                structured: structured_logger.take(),
                global_step: 0,
                training_config_json: training_cfg_json,
                plateau: plateau_state,
                classic_bundle: &mut classic_bundle,
            };
            if is_cache && config.stream_cache {
                train_model_stream_cache(
                    &mut network,
                    input_path,
                    &validation_samples,
                    &config,
                    &mut rng,
                    &mut ctx,
                    &weighting_cfg,
                )?;
            } else if is_cache {
                train_model_with_loader(
                    &mut network,
                    train_samples.clone(),
                    &validation_samples,
                    &config,
                    &mut rng,
                    &mut ctx,
                )?;
            } else {
                train_model(
                    &mut network,
                    &mut train_samples,
                    &validation_samples,
                    &config,
                    &mut rng,
                    &mut ctx,
                )?;
            }

            if arch == ArchKind::Classic && export_format == ExportFormat::ClassicV1 {
                if train_samples.is_empty() {
                    if human_to_stderr {
                        eprintln!(
                            "Classic distillation skipped: training samples not loaded in-memory (stream-cache mode)."
                        );
                    } else {
                        println!(
                            "Classic distillation skipped: training samples not loaded in-memory (stream-cache mode)."
                        );
                    }
                } else if let Some(path) = &distill_options.teacher_path {
                    match load_teacher(path, distill_options.teacher_kind) {
                        Ok(teacher) => match distill_classic_after_training(
                            teacher.as_ref(),
                            &train_samples,
                            &config,
                            &distill_options,
                            distill::ClassicDistillConfig::new(
                                export_options.quant_ft,
                                export_options.quant_h1,
                                export_options.quant_h2,
                                export_options.quant_out,
                                resolve_quant_calibration(
                                    quant_calibration_slice,
                                    train_samples.as_slice(),
                                    quant_calibration_limit,
                                    quant_search,
                                ),
                                ctx.structured.as_ref(),
                            ),
                        ) {
                            Ok(artifacts) => {
                                let distill::DistillArtifacts {
                                    classic_fp32,
                                    bundle_int,
                                    scales,
                                    calibration_metrics: cal_metrics,
                                } = artifacts;
                                calibration_metrics = cal_metrics.clone();

                                let eval_samples_slice: &[Sample] =
                                    if let Some(val) = validation_samples.as_ref() {
                                        if !val.is_empty() {
                                            val.as_slice()
                                        } else {
                                            train_samples.as_slice()
                                        }
                                    } else {
                                        train_samples.as_slice()
                                    };

                                if !eval_samples_slice.is_empty() {
                                    let dm = evaluate_distill(
                                        teacher.as_ref(),
                                        &classic_fp32,
                                        eval_samples_slice,
                                        &config,
                                        distill_options.teacher_domain,
                                    );
                                    let qm = evaluate_quantization_gap(
                                        &classic_fp32,
                                        &bundle_int,
                                        &scales,
                                        eval_samples_slice,
                                        &config,
                                    );
                                    distill_metrics = Some(dm);
                                    quant_metrics = Some(qm);
                                }

                                // Apply final cp gain before saving bundle
                                let mut bundle_m = bundle_int;
                                if export_options.arch == ArchKind::Classic
                                    && matches!(export_options.format, ExportFormat::ClassicV1)
                                    && final_cp_gain != 1.0
                                {
                                    let (sw, sb) = bundle_m.apply_final_cp_gain(final_cp_gain);
                                    if let Some(lg) = ctx.structured.as_ref() {
                                        let rec = serde_json::json!({
                                            "ts": chrono::Utc::now().to_rfc3339(),
                                            "component": "export",
                                            "phase": "final_cp_gain",
                                            "gain": final_cp_gain,
                                            "sat_w": sw,
                                            "sat_b": sb,
                                        });
                                        lg.write_json(&rec);
                                    } else if human_to_stderr {
                                        eprintln!(
                                            "Applied final-cp-gain={:.3} to Classic INT (sat_w={}, sat_b={})",
                                            final_cp_gain, sw, sb
                                        );
                                    } else {
                                        println!(
                                            "Applied final-cp-gain={:.3} to Classic INT (sat_w={}, sat_b={})",
                                            final_cp_gain, sw, sb
                                        );
                                    }
                                }
                                classic_scales = Some(scales.clone());
                                export_options.quant_ft = scales.scheme.ft;
                                export_options.quant_h1 = scales.scheme.h1;
                                export_options.quant_h2 = scales.scheme.h2;
                                export_options.quant_out = scales.scheme.out;
                                *ctx.classic_bundle = Some(bundle_m);
                            }
                            Err(e) => {
                                if human_to_stderr {
                                    eprintln!(
                                        "Classic distillation failed (falling back to zero bundle): {}",
                                        e
                                    );
                                } else {
                                    println!(
                                        "Classic distillation failed (falling back to zero bundle): {}",
                                        e
                                    );
                                }
                            }
                        },
                        Err(e) => {
                            if human_to_stderr {
                                eprintln!(
                                    "Failed to load teacher network for classic distillation: {}",
                                    e
                                );
                            } else {
                                println!(
                                    "Failed to load teacher network for classic distillation: {}",
                                    e
                                );
                            }
                        }
                    }
                } else if human_to_stderr {
                    eprintln!("Classic distillation skipped: teacher network was not provided.");
                } else {
                    println!("Classic distillation skipped: teacher network was not provided.");
                }
            }

            structured_logger = ctx.structured.take();
        }
    }

    // Resolve export format
    finalize_export(FinalizeExportParams {
        network: &network,
        out_dir: &out_dir,
        export: export_options,
        emit_single_quant: app.get_flag("quantized"),
        classic_bundle: classic_bundle.as_ref(),
        classic_scales: classic_scales.as_ref(),
        calibration_metrics: calibration_metrics.as_ref(),
        quant_metrics: quant_metrics.as_ref(),
    })?;

    // Save config
    let mut config_file = File::create(out_dir.join("config.json"))?;
    writeln!(config_file, "{}", serde_json::to_string_pretty(&config)?)?;

    // Emit evaluation metrics & gates for distillation / quantization
    let mut fail_due_to_gate = false;
    let fmt_opt = |v: Option<f32>| -> String {
        v.map(|x| format!("{:.4}", x)).unwrap_or_else(|| "NA".to_string())
    };

    if let Some(metrics) = calibration_metrics.as_ref() {
        if metrics.n == 0 {
            let msg = "Calibration quant metrics: SKIP (no samples)";
            if human_to_stderr {
                eprintln!("{}", msg);
            } else {
                println!("{}", msg);
            }
        } else {
            let summary = format!(
                "Calibration quant metrics: n={} cp_mae={} cp_p95={} cp_max={} logit_mae={} logit_p95={} logit_max={}",
                metrics.n,
                fmt_opt(metrics.mae_cp),
                fmt_opt(metrics.p95_cp),
                fmt_opt(metrics.max_cp),
                fmt_opt(metrics.mae_logit),
                fmt_opt(metrics.p95_logit),
                fmt_opt(metrics.max_logit),
            );
            if human_to_stderr {
                eprintln!("{}", summary);
            } else {
                println!("{}", summary);
            }
        }
    }

    if let Some(metrics) = distill_metrics.as_ref() {
        if metrics.n == 0 {
            let msg = "Distill eval: SKIP (no samples)";
            if human_to_stderr {
                eprintln!("{}", msg);
            } else {
                println!("{}", msg);
            }
        } else {
            let mut evaluated = false;
            let mut pass = true;
            let mut detail_parts = Vec::new();
            let mut gate_map = serde_json::Map::new();

            if let Some(th) = gate_distill_cp_mae {
                evaluated = true;
                gate_map.insert("cp_mae_le".into(), serde_json::json!(th));
                match metrics.mae_cp {
                    Some(val) => {
                        let ok = val <= th;
                        pass &= ok;
                        detail_parts.push(format!(
                            "cp_mae={} (<= {:.3}) {}",
                            fmt_opt(Some(val)),
                            th,
                            if ok { "PASS" } else { "FAIL" }
                        ));
                    }
                    None => {
                        pass = false;
                        detail_parts.push(format!("cp_mae=NA (<= {:.3}) FAIL", th));
                    }
                }
            }
            if let Some(th) = gate_distill_cp_p95 {
                evaluated = true;
                gate_map.insert("cp_p95_le".into(), serde_json::json!(th));
                match metrics.p95_cp {
                    Some(val) => {
                        let ok = val <= th;
                        pass &= ok;
                        detail_parts.push(format!(
                            "cp_p95={} (<= {:.3}) {}",
                            fmt_opt(Some(val)),
                            th,
                            if ok { "PASS" } else { "FAIL" }
                        ));
                    }
                    None => {
                        pass = false;
                        detail_parts.push(format!("cp_p95=NA (<= {:.3}) FAIL", th));
                    }
                }
            }
            if let Some(th) = gate_distill_logit_mae {
                evaluated = true;
                gate_map.insert("logit_mae_le".into(), serde_json::json!(th));
                match metrics.mae_logit {
                    Some(val) => {
                        let ok = val <= th;
                        pass &= ok;
                        detail_parts.push(format!(
                            "logit_mae={} (<= {:.4}) {}",
                            fmt_opt(Some(val)),
                            th,
                            if ok { "PASS" } else { "FAIL" }
                        ));
                    }
                    None => {
                        pass = false;
                        detail_parts.push(format!("logit_mae=NA (<= {:.4}) FAIL", th));
                    }
                }
            }

            gate_map.insert("pass".into(), serde_json::json!(pass));

            if let Some(lg) = structured_logger.as_ref() {
                let mut rec = serde_json::Map::new();
                rec.insert("ts".into(), serde_json::json!(Utc::now().to_rfc3339()));
                rec.insert("phase".into(), serde_json::json!("distill_eval"));
                rec.insert("n".into(), serde_json::json!(metrics.n as i64));
                rec.insert(
                    "mae_cp".into(),
                    metrics.mae_cp.map(|v| serde_json::json!(v)).unwrap_or(serde_json::Value::Null),
                );
                rec.insert(
                    "p95_cp".into(),
                    metrics.p95_cp.map(|v| serde_json::json!(v)).unwrap_or(serde_json::Value::Null),
                );
                rec.insert(
                    "max_cp".into(),
                    metrics.max_cp.map(|v| serde_json::json!(v)).unwrap_or(serde_json::Value::Null),
                );
                rec.insert(
                    "r2_cp".into(),
                    metrics.r2_cp.map(|v| serde_json::json!(v)).unwrap_or(serde_json::Value::Null),
                );
                rec.insert(
                    "mae_logit".into(),
                    metrics
                        .mae_logit
                        .map(|v| serde_json::json!(v))
                        .unwrap_or(serde_json::Value::Null),
                );
                rec.insert(
                    "p95_logit".into(),
                    metrics
                        .p95_logit
                        .map(|v| serde_json::json!(v))
                        .unwrap_or(serde_json::Value::Null),
                );
                rec.insert(
                    "max_logit".into(),
                    metrics
                        .max_logit
                        .map(|v| serde_json::json!(v))
                        .unwrap_or(serde_json::Value::Null),
                );
                rec.insert("gate".into(), serde_json::Value::Object(gate_map.clone()));
                lg.write_json(&serde_json::Value::Object(rec));
            }

            let status = if !evaluated {
                "NO-THRESHOLD"
            } else if pass {
                "PASS"
            } else {
                "FAIL"
            };

            let mut summary = format!(
                "Distill eval: n={} mae_cp={} p95_cp={} max_cp={} r2_cp={}",
                metrics.n,
                fmt_opt(metrics.mae_cp),
                fmt_opt(metrics.p95_cp),
                fmt_opt(metrics.max_cp),
                fmt_opt(metrics.r2_cp)
            );
            if metrics.mae_logit.is_some() {
                summary.push_str(&format!(
                    " mae_logit={} p95_logit={} max_logit={}",
                    fmt_opt(metrics.mae_logit),
                    fmt_opt(metrics.p95_logit),
                    fmt_opt(metrics.max_logit)
                ));
            }
            if !detail_parts.is_empty() {
                summary.push_str(&format!(" | {}", detail_parts.join(", ")));
            }
            summary.push_str(&format!(" -> {}", status));

            if human_to_stderr {
                eprintln!("{}", summary);
            } else {
                println!("{}", summary);
            }

            if evaluated && !pass && gate_mode_fail {
                fail_due_to_gate = true;
            }
        }
    }

    if let Some(metrics) = quant_metrics.as_ref() {
        if metrics.n == 0 {
            let msg = "Quant eval: SKIP (no samples)";
            if human_to_stderr {
                eprintln!("{}", msg);
            } else {
                println!("{}", msg);
            }
        } else {
            let mut evaluated = false;
            let mut pass = true;
            let mut detail_parts = Vec::new();
            let mut gate_map = serde_json::Map::new();

            if let Some(th) = gate_classic_int_cp_mae {
                evaluated = true;
                gate_map.insert("cp_mae_le".into(), serde_json::json!(th));
                match metrics.mae_cp {
                    Some(val) => {
                        let ok = val <= th;
                        pass &= ok;
                        detail_parts.push(format!(
                            "cp_mae={} (<= {:.3}) {}",
                            fmt_opt(Some(val)),
                            th,
                            if ok { "PASS" } else { "FAIL" }
                        ));
                    }
                    None => {
                        pass = false;
                        detail_parts.push(format!("cp_mae=NA (<= {:.3}) FAIL", th));
                    }
                }
            }
            if let Some(th) = gate_classic_int_cp_p95 {
                evaluated = true;
                gate_map.insert("cp_p95_le".into(), serde_json::json!(th));
                match metrics.p95_cp {
                    Some(val) => {
                        let ok = val <= th;
                        pass &= ok;
                        detail_parts.push(format!(
                            "cp_p95={} (<= {:.3}) {}",
                            fmt_opt(Some(val)),
                            th,
                            if ok { "PASS" } else { "FAIL" }
                        ));
                    }
                    None => {
                        pass = false;
                        detail_parts.push(format!("cp_p95=NA (<= {:.3}) FAIL", th));
                    }
                }
            }
            if let Some(th) = gate_classic_int_logit_mae {
                evaluated = true;
                gate_map.insert("logit_mae_le".into(), serde_json::json!(th));
                match metrics.mae_logit {
                    Some(val) => {
                        let ok = val <= th;
                        pass &= ok;
                        detail_parts.push(format!(
                            "logit_mae={} (<= {:.4}) {}",
                            fmt_opt(Some(val)),
                            th,
                            if ok { "PASS" } else { "FAIL" }
                        ));
                    }
                    None => {
                        pass = false;
                        detail_parts.push(format!("logit_mae=NA (<= {:.4}) FAIL", th));
                    }
                }
            }

            gate_map.insert("pass".into(), serde_json::json!(pass));

            if let Some(lg) = structured_logger.as_ref() {
                let mut rec = serde_json::Map::new();
                rec.insert("ts".into(), serde_json::json!(Utc::now().to_rfc3339()));
                rec.insert("phase".into(), serde_json::json!("quant_eval"));
                rec.insert("n".into(), serde_json::json!(metrics.n as i64));
                rec.insert(
                    "mae_cp".into(),
                    metrics.mae_cp.map(|v| serde_json::json!(v)).unwrap_or(serde_json::Value::Null),
                );
                rec.insert(
                    "p95_cp".into(),
                    metrics.p95_cp.map(|v| serde_json::json!(v)).unwrap_or(serde_json::Value::Null),
                );
                rec.insert(
                    "max_cp".into(),
                    metrics.max_cp.map(|v| serde_json::json!(v)).unwrap_or(serde_json::Value::Null),
                );
                rec.insert(
                    "mae_logit".into(),
                    metrics
                        .mae_logit
                        .map(|v| serde_json::json!(v))
                        .unwrap_or(serde_json::Value::Null),
                );
                rec.insert(
                    "p95_logit".into(),
                    metrics
                        .p95_logit
                        .map(|v| serde_json::json!(v))
                        .unwrap_or(serde_json::Value::Null),
                );
                rec.insert(
                    "max_logit".into(),
                    metrics
                        .max_logit
                        .map(|v| serde_json::json!(v))
                        .unwrap_or(serde_json::Value::Null),
                );
                if let Some(scales) = classic_scales.as_ref() {
                    rec.insert(
                        "scales".into(),
                        serde_json::json!({
                            "s_w0": scales.s_w0,
                            "s_w1": scales.s_w1,
                            "s_w2": scales.s_w2,
                            "s_w3": scales.s_w3,
                            "s_in_1": scales.s_in_1,
                            "s_in_2": scales.s_in_2,
                            "s_in_3": scales.s_in_3,
                        }),
                    );
                }
                rec.insert("gate".into(), serde_json::Value::Object(gate_map.clone()));
                lg.write_json(&serde_json::Value::Object(rec));
            }

            let status = if !evaluated {
                "NO-THRESHOLD"
            } else if pass {
                "PASS"
            } else {
                "FAIL"
            };

            let mut summary = format!(
                "Quant eval: n={} mae_cp={} p95_cp={} max_cp={}",
                metrics.n,
                fmt_opt(metrics.mae_cp),
                fmt_opt(metrics.p95_cp),
                fmt_opt(metrics.max_cp)
            );
            if metrics.mae_logit.is_some() {
                summary.push_str(&format!(
                    " mae_logit={} p95_logit={} max_logit={}",
                    fmt_opt(metrics.mae_logit),
                    fmt_opt(metrics.p95_logit),
                    fmt_opt(metrics.max_logit)
                ));
            }
            if !detail_parts.is_empty() {
                summary.push_str(&format!(" | {}", detail_parts.join(", ")));
            }
            summary.push_str(&format!(" -> {}", status));

            if human_to_stderr {
                eprintln!("{}", summary);
            } else {
                println!("{}", summary);
            }

            if evaluated && !pass && gate_mode_fail {
                fail_due_to_gate = true;
            }
        }
    }

    if fail_due_to_gate {
        std::process::exit(1);
    }

    // Save best network and meta when validation present
    if let Some(val_samples) = &validation_samples {
        if let Some(best_net) = &best_network {
            save_network(best_net, &out_dir.join("nn_best.fp32.bin"))?;
            #[derive(serde::Serialize)]
            struct BestMeta {
                best_epoch: usize,
                best_val_loss: f32,
                best_val_auc: Option<f64>,
                best_val_ece: Option<f64>,
                // Repro metadata for reproducibility
                seed: Option<u64>,
                optimizer: String,
                lr: f32,
                l2: f32,
                acc_dim: usize,
                relu_clip: i32,
                label_type: String,
                scale: f32,
                cp_clip: i32,
            }
            let (best_val_auc, best_val_ece) =
                compute_val_auc_and_ece(best_net, val_samples, &config, &dash);
            let meta = BestMeta {
                best_epoch: best_epoch.unwrap_or(0),
                best_val_loss,
                best_val_auc,
                best_val_ece,
                seed: seed_u64_opt,
                optimizer: config.optimizer.clone(),
                lr: config.learning_rate,
                l2: config.l2_reg,
                acc_dim: config.accumulator_dim,
                relu_clip: config.relu_clip,
                label_type: config.label_type.clone(),
                scale: config.scale,
                cp_clip: config.cp_clip,
            };
            let mut mf = File::create(out_dir.join("nn_best.meta.json"))?;
            writeln!(mf, "{}", serde_json::to_string_pretty(&meta)?)?;
            if human_to_stderr {
                eprintln!(
                    "Saved best validation network to {}",
                    out_dir.join("nn_best.fp32.bin").display()
                );
            } else {
                println!(
                    "Saved best validation network to {}",
                    out_dir.join("nn_best.fp32.bin").display()
                );
            }
        }
    }

    // Gating
    if gate_last_epoch_best {
        match (last_val_loss, best_val_loss.is_finite(), validation_samples.is_some()) {
            (Some(last), true, true) => {
                let pass = last <= best_val_loss + 1e-6;
                if human_to_stderr {
                    eprintln!(
                        "GATE val_loss_last_is_best: {} (last={:.6}, best={:.6})",
                        if pass { "PASS" } else { "FAIL" },
                        last,
                        best_val_loss
                    );
                } else {
                    println!(
                        "GATE val_loss_last_is_best: {} (last={:.6}, best={:.6})",
                        if pass { "PASS" } else { "FAIL" },
                        last,
                        best_val_loss
                    );
                }
                if !pass && gate_mode_fail {
                    std::process::exit(1);
                }
            }
            _ => {
                if human_to_stderr {
                    eprintln!("GATE val_loss_last_is_best: SKIP (no validation)")
                } else {
                    println!("GATE val_loss_last_is_best: SKIP (no validation)")
                }
            }
        }
    }
    if let (Some(th), Some(val_samples)) = (gate_min_auc, validation_samples.as_ref()) {
        if config.label_type == "wdl" {
            let auc = compute_val_auc(&network, val_samples, &config);
            match auc {
                Some(v) => {
                    let pass = v >= th;
                    if human_to_stderr {
                        eprintln!(
                            "GATE min_auc {:.4} >= {:.4}: {}",
                            v,
                            th,
                            if pass { "PASS" } else { "FAIL" }
                        );
                    } else {
                        println!(
                            "GATE min_auc {:.4} >= {:.4}: {}",
                            v,
                            th,
                            if pass { "PASS" } else { "FAIL" }
                        );
                    }
                    if !pass && gate_mode_fail {
                        std::process::exit(1);
                    }
                }
                None => {
                    if human_to_stderr {
                        eprintln!("GATE min_auc: SKIP (insufficient positive/negative)")
                    } else {
                        println!("GATE min_auc: SKIP (insufficient positive/negative)")
                    }
                }
            }
        } else if human_to_stderr {
            eprintln!("GATE min_auc: SKIP (label_type!=wdl)");
        } else {
            println!("GATE min_auc: SKIP (label_type!=wdl)");
        }
    }

    if human_to_stderr {
        eprintln!("\nModel saved to: {}", out_dir.display());
    } else {
        println!("\nModel saved to: {}", out_dir.display());
    }

    Ok(())
}

fn default_teacher_domain(kind: TeacherKind) -> TeacherValueDomain {
    match kind {
        TeacherKind::ClassicFp32 => TeacherValueDomain::WdlLogit,
        TeacherKind::Single => TeacherValueDomain::WdlLogit,
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod cli_tests;
