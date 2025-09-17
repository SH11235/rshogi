//! NNUE (Efficiently Updatable Neural Network) trainer
//!
//! このバイナリは JSONL/NFC キャッシュから NNUE モデルを学習し、各種フォーマットで出力します。

pub(crate) mod classic;
pub(crate) mod dataset;
pub(crate) mod distill;
pub(crate) mod export;
pub(crate) mod logging;
pub(crate) mod model;
pub(crate) mod params;
pub(crate) mod training;
pub(crate) mod types;

use std::fs::{create_dir_all, File};
use std::io::Write;
use std::path::PathBuf;
use std::time::{Instant, SystemTime};

use clap::parser::ValueSource;
use clap::{arg, value_parser, Arg, ArgAction, Command, ValueHint};
use engine_core::evaluation::nnue::features::FE_END;
use engine_core::shogi::SHOGI_BOARD_SIZE;
use model::Network;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use tools::common::weighting as wcfg;

use classic::ClassicIntNetworkBundle;
use dataset::{load_samples, load_samples_from_cache};
use distill::distill_classic_after_training;
use export::{finalize_export, save_network};
use logging::StructuredLogger;
use params::{DEFAULT_ACC_DIM, DEFAULT_RELU_CLIP, MAX_PREFETCH_BATCHES};
use training::{
    compute_val_auc, compute_val_auc_and_ece, train_model, train_model_stream_cache,
    train_model_with_loader, DashboardOpts, LrPlateauState, TrainContext, TrainTrackers,
};
use types::{
    ArchKind, Config, DistillLossKind, DistillOptions, ExportFormat, ExportOptions, QuantScheme,
    Sample,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
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
        .arg(arg!(--opt <TYPE> "Optimizer: sgd, adam").default_value("adam"))
        .arg(arg!(--l2 <RATE> "L2 regularization").default_value("0.000001"))
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
                .help("Quantization scheme for feature transformer weights")
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
                .help("Quantization scheme for output layer weights")
                .value_parser(clap::value_parser!(QuantScheme))
                .default_value("per-tensor"),
        )
        .arg(
            Arg::new("distill-from-single")
                .long("distill-from-single")
                .help("Path to teacher Single FP32 weights for knowledge distillation")
                .value_hint(ValueHint::FilePath),
        )
        .arg(
            Arg::new("kd-loss")
                .long("kd-loss")
                .help("Knowledge distillation loss: mse|bce|kl")
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
        .arg(arg!(--seed <SEED> "Random seed for reproducibility"))
        .arg(arg!(-o --out <DIR> "Output directory"))
        .get_matches();

    // Prepare structured logger early for stdout/stderr routing decisions
    let structured_logger: Option<StructuredLogger> = app
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
    let label_type_value = app.get_one::<String>("label").unwrap().to_string();
    let distill_teacher = app.get_one::<String>("distill-from-single").map(PathBuf::from);
    let kd_loss_source = app.value_source("kd-loss");
    let kd_temp_source = app.value_source("kd-temperature");
    let kd_alpha_source = app.value_source("kd-alpha");

    let mut distill_loss =
        *app.get_one::<DistillLossKind>("kd-loss").unwrap_or(&DistillLossKind::Mse);
    let mut distill_temperature = *app.get_one::<f32>("kd-temperature").unwrap();
    let mut distill_alpha = *app.get_one::<f32>("kd-alpha").unwrap();

    if arch == ArchKind::Classic && export_format == ExportFormat::ClassicV1 {
        if distill_teacher.is_none() {
            return Err("Classic export requires --distill-from-single".into());
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

    let config = Config {
        epochs: app.get_one::<String>("epochs").unwrap().parse()?,
        batch_size: app.get_one::<String>("batch-size").unwrap().parse()?,
        learning_rate: app.get_one::<String>("lr").unwrap().parse()?,
        optimizer: app.get_one::<String>("opt").unwrap().to_string(),
        l2_reg: app.get_one::<String>("l2").unwrap().parse()?,
        label_type: label_type_value.clone(),
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
    };

    let export_options = ExportOptions {
        arch,
        format: export_format,
        quant_ft,
        quant_h1,
        quant_h2,
        quant_out,
    };
    let distill_options = DistillOptions {
        teacher_path: distill_teacher.clone(),
        loss: distill_loss,
        temperature: distill_temperature,
        alpha: distill_alpha,
    };

    if config.scale <= 0.0 {
        return Err("Invalid --scale: must be > 0".into());
    }
    if config.throughput_interval_sec <= 0.0 {
        return Err("Invalid --throughput-interval: must be > 0".into());
    }
    if export_options.arch == ArchKind::Single
        && matches!(export_options.format, ExportFormat::ClassicV1)
    {
        return Err("--arch=single does not support --export-format classic-v1".into());
    }
    if export_options.arch == ArchKind::Classic
        && matches!(export_options.format, ExportFormat::SingleI8)
    {
        return Err("--arch=classic does not support --export-format single-i8".into());
    }
    if export_options.arch == ArchKind::Classic
        && export_options.quant_ft == QuantScheme::PerChannel
    {
        return Err("--arch=classic currently supports only --quant-ft=per-tensor".into());
    }
    if export_options.arch == ArchKind::Classic
        && export_options.quant_out == QuantScheme::PerChannel
    {
        return Err("--arch=classic currently supports only --quant-out=per-tensor".into());
    }
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
    }
    if human_to_stderr {
        match &distill_options.teacher_path {
            Some(path) => eprintln!(
                "  Distill: teacher={:?}, loss={:?}, temp={}, alpha={}",
                path, distill_options.loss, distill_options.temperature, distill_options.alpha
            ),
            None => eprintln!(
                "  Distill: teacher=None, loss={:?}, temp={}, alpha={}",
                distill_options.loss, distill_options.temperature, distill_options.alpha
            ),
        }
    } else {
        match &distill_options.teacher_path {
            Some(path) => println!(
                "  Distill: teacher={:?}, loss={:?}, temp={}, alpha={}",
                path, distill_options.loss, distill_options.temperature, distill_options.alpha
            ),
            None => println!(
                "  Distill: teacher=None, loss={:?}, temp={}, alpha={}",
                distill_options.loss, distill_options.temperature, distill_options.alpha
            ),
        }
    }
    if human_to_stderr {
        eprintln!("  Feature dimension (input): {} (HalfKP)", SHOGI_BOARD_SIZE * FE_END);
    } else {
        println!("  Feature dimension (input): {} (HalfKP)", SHOGI_BOARD_SIZE * FE_END);
    }
    if human_to_stderr {
        eprintln!("  Network: {} -> {} -> 1", SHOGI_BOARD_SIZE * FE_END, config.accumulator_dim);
    } else {
        println!("  Network: {} -> {} -> 1", SHOGI_BOARD_SIZE * FE_END, config.accumulator_dim);
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

    // Initialize RNG with seed if provided
    let seed_u64_opt: Option<u64> =
        app.get_one::<String>("seed").and_then(|s| s.parse::<u64>().ok());
    let mut rng: StdRng = if let Some(seed) = seed_u64_opt {
        if human_to_stderr {
            eprintln!("Using random seed (u64): {}", seed);
        } else {
            println!("Using random seed (u64): {}", seed);
        }
        StdRng::seed_from_u64(seed)
    } else {
        let seed_bytes: [u8; 32] = rand::rng().random();
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
    let mut network = Network::new(config.accumulator_dim, config.relu_clip, &mut rng);

    // Train the model
    if human_to_stderr {
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
            structured: structured_logger,
            global_step: 0,
            training_config_json: training_cfg_json,
            plateau: plateau_state,
            export: export_options.clone(),
            distill: distill_options.clone(),
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
                match Network::load(path) {
                    Ok(teacher) => match distill_classic_after_training(
                        &teacher,
                        &train_samples,
                        &config,
                        &distill_options,
                        distill::ClassicDistillConfig::new(
                            export_options.quant_ft,
                            export_options.quant_h1,
                            export_options.quant_h2,
                            export_options.quant_out,
                            ctx.structured.as_ref(),
                        ),
                    ) {
                        Ok((bundle, _scales)) => {
                            *ctx.classic_bundle = Some(bundle);
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
                eprintln!("Classic distillation skipped: --distill-from-single was not provided.");
            } else {
                println!("Classic distillation skipped: --distill-from-single was not provided.");
            }
        }
    }

    // Resolve export format
    finalize_export(
        &network,
        &out_dir,
        export_options,
        app.get_flag("quantized"),
        classic_bundle.as_ref(),
    )?;

    // Save config
    let mut config_file = File::create(out_dir.join("config.json"))?;
    writeln!(config_file, "{}", serde_json::to_string_pretty(&config)?)?;

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

#[cfg(test)]
mod tests;
