use anyhow::{Result, ensure};
use rshogi_core::position::Position;
use rshogi_dl::onnx::{ModelEvaluator, ModelOptions, OnnxConfig};
use std::io::{self, BufRead};

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    ensure!(
        (4..=6).contains(&args.len()),
        "usage: model_probe MODEL DEVICE MAX_BATCH [PROFILE_PREFIX|-] [CUDA_TF32]; SFEN lines on stdin"
    );
    let config = OnnxConfig {
        model_path: args[1].clone().into(),
        gpu_id: args[2].parse()?,
        use_tensorrt: false,
        tensorrt_cache: None,
        batch_size: args[3].parse()?,
    };
    let options = ModelOptions {
        cuda_tf32: args.get(5).map(|value| value.parse()).transpose()?.unwrap_or(true),
        profiling: args.get(4).filter(|path| path.as_str() != "-").map(Into::into),
    };
    let mut evaluator = ModelEvaluator::with_options(&config, &options)?;
    let mut positions = Vec::new();
    for line in io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let mut pos = Position::new();
        pos.set_sfen(&line)?;
        positions.push(pos);
        ensure!(positions.len() <= config.batch_size, "input exceeds MAX_BATCH");
    }
    let mut values = Vec::new();
    let mut policy = Vec::new();
    evaluator.evaluate_with_policy(&positions, &mut values, &mut policy)?;
    let policy: Vec<_> = policy
        .as_chunks::<{ rshogi_dl::features::MAX_MOVE_LABEL_NUM }>()
        .0
        .iter()
        .map(|row| row.as_slice())
        .collect();
    serde_json::to_writer(
        io::stdout().lock(),
        &serde_json::json!({"policy":policy,"values":values}),
    )?;
    if options.profiling.is_some() {
        eprintln!("profile: {}", evaluator.end_profiling()?);
    }
    Ok(())
}
