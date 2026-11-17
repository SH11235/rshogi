use anyhow::{Result, ensure};
use rshogi_core::position::Position;
use rshogi_dl::onnx::{ModelEvaluator, ModelOptions, OnnxConfig};
use rshogi_dl::{Evaluator, InputBuffer, InputFormat};
use std::io::{self, BufRead};

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    ensure!(
        (4..=8).contains(&args.len()),
        "usage: model_probe MODEL DEVICE MAX_BATCH [PROFILE_PREFIX|-] [CUDA_TF32] [batch|encoded] [auto|basic|tokens]; SFEN lines on stdin"
    );
    let api = args.get(6).map(String::as_str).unwrap_or("batch");
    let input_format = args.get(7).map(String::as_str).unwrap_or("auto");
    ensure!(matches!(api, "batch" | "encoded"), "API must be batch or encoded");
    ensure!(matches!(input_format, "auto" | "basic" | "tokens"), "invalid input format");
    ensure!(
        api == "encoded" || input_format == "auto",
        "input format override requires encoded API"
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
    let mut values = Vec::with_capacity(positions.len());
    let mut policy = Vec::with_capacity(positions.len() * rshogi_dl::features::MAX_MOVE_LABEL_NUM);
    if api == "encoded" {
        let format = match input_format {
            "basic" => InputFormat::Basic,
            "tokens" => InputFormat::StmTokensV1,
            _ => evaluator
                .input_format()
                .ok_or_else(|| anyhow::anyhow!("missing input format"))?,
        };
        let mut buffer = InputBuffer::new(format)?;
        for position in &positions {
            let start = policy.len();
            policy.resize(start + rshogi_dl::features::MAX_MOVE_LABEL_NUM, 0.0);
            let value = evaluator
                .evaluate_encoded(buffer.encode(position)?, &mut policy[start..])?
                .ok_or_else(|| anyhow::anyhow!("encoded scalar evaluation is unsupported"))?;
            values.push(value);
        }
    } else {
        evaluator.evaluate_with_policy(&positions, &mut values, &mut policy)?;
    }
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
