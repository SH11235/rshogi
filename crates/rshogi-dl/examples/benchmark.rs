use std::hint::black_box;
use std::io::{self, BufRead, Read};
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result, bail, ensure};
use rshogi_core::position::Position;
use rshogi_dl::cache::{CacheStats, CachedEvaluator};
use rshogi_dl::features::{
    FEATURES1_SIZE, FEATURES2_SIZE, MAX_MOVE_LABEL_NUM, make_input_features,
};
use rshogi_dl::{EncodedInput, Evaluator, InputFormat, tokens};
use serde_json::{Value, json};

const MAX_POSITIONS: usize = 1024;
const MAX_LINE_BYTES: usize = 4096;
const MAX_REQUESTS: usize = 10_000_000;
const USAGE: &str = "benchmark MODE [--iterations N] [--warmup N] [--batch-size N] [--cache-mib N] [--model FILE] [--device ID] [--cuda-tf32 true|false] [--reuse-encoded true|false]; MODE: encode-basic|encode-tokens|cache-basic|cache-tokens|onnx|onnx-cache; SFEN lines on stdin (empty = start position)";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    EncodeBasic,
    EncodeTokens,
    CacheBasic,
    CacheTokens,
    Onnx,
    OnnxCache,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Self::EncodeBasic => "encode-basic",
            Self::EncodeTokens => "encode-tokens",
            Self::CacheBasic => "cache-basic",
            Self::CacheTokens => "cache-tokens",
            Self::Onnx => "onnx",
            Self::OnnxCache => "onnx-cache",
        }
    }
}

struct Config {
    mode: Mode,
    iterations: usize,
    warmup: usize,
    batch_size: usize,
    cache_mib: usize,
    model: Option<PathBuf>,
    device: i32,
    cuda_tf32: bool,
    reuse_encoded: bool,
}

impl Config {
    fn parse(args: impl IntoIterator<Item = String>) -> Result<Self> {
        let mut args = args.into_iter();
        let mode = match args.next().as_deref() {
            Some("encode-basic") => Mode::EncodeBasic,
            Some("encode-tokens") => Mode::EncodeTokens,
            Some("cache-basic") => Mode::CacheBasic,
            Some("cache-tokens") => Mode::CacheTokens,
            Some("onnx") => Mode::Onnx,
            Some("onnx-cache") => Mode::OnnxCache,
            _ => bail!(USAGE),
        };
        let mut config = Self {
            mode,
            iterations: 100,
            warmup: 10,
            batch_size: 1,
            cache_mib: 64,
            model: None,
            device: -1,
            cuda_tf32: true,
            reuse_encoded: true,
        };
        let mut seen = Vec::new();
        while let Some(flag) = args.next() {
            ensure!(!seen.contains(&flag), "duplicate option: {flag}");
            let value = args.next().with_context(|| format!("missing value for {flag}"))?;
            match flag.as_str() {
                "--iterations" => config.iterations = value.parse()?,
                "--warmup" => config.warmup = value.parse()?,
                "--batch-size" => config.batch_size = value.parse()?,
                "--cache-mib" => config.cache_mib = value.parse()?,
                "--model" => config.model = Some(value.into()),
                "--device" => config.device = value.parse()?,
                "--cuda-tf32" => config.cuda_tf32 = value.parse()?,
                "--reuse-encoded" => config.reuse_encoded = value.parse()?,
                _ => bail!("unknown option: {flag}; {USAGE}"),
            }
            seen.push(flag);
        }
        ensure!(
            (1..=MAX_REQUESTS).contains(&config.iterations),
            "iterations must be 1..={MAX_REQUESTS}"
        );
        ensure!(config.warmup <= MAX_REQUESTS, "warmup must be 0..={MAX_REQUESTS}");
        ensure!((1..=256).contains(&config.batch_size), "batch size must be 1..=256");
        ensure!(config.cache_mib <= 1024, "cache must be 0..=1024 MiB");
        let actual_model = matches!(mode, Mode::Onnx | Mode::OnnxCache);
        ensure!(
            actual_model == config.model.is_some(),
            "--model is required only for ONNX modes"
        );
        ensure!(
            actual_model || (!seen.iter().any(|f| f == "--device" || f == "--cuda-tf32")),
            "device and precision options require an ONNX mode"
        );
        for flag in ["--cache-mib", "--reuse-encoded"] {
            ensure!(
                matches!(mode, Mode::CacheBasic | Mode::CacheTokens | Mode::OnnxCache)
                    || !seen.iter().any(|f| f == flag),
                "{flag} requires a cache mode"
            );
        }
        ensure!(
            mode == Mode::Onnx || config.batch_size == 1,
            "batch sizes above 1 require onnx mode"
        );
        Ok(config)
    }

    fn check_requests(&self, positions: usize) -> Result<()> {
        let measured = self.iterations.checked_mul(positions).context("request count overflow")?;
        let warmup = self.warmup.checked_mul(positions).context("warmup count overflow")?;
        ensure!(
            measured <= MAX_REQUESTS && warmup <= MAX_REQUESTS,
            "measured and warmup requests must each be <= {MAX_REQUESTS}"
        );
        Ok(())
    }
}

fn read_positions(mut reader: impl BufRead) -> Result<Vec<Position>> {
    let mut positions = Vec::new();
    let mut lines = 0;
    loop {
        let mut line = String::new();
        let bytes = Read::by_ref(&mut reader)
            .take((MAX_LINE_BYTES + 1) as u64)
            .read_line(&mut line)?;
        if bytes == 0 {
            break;
        }
        lines += 1;
        ensure!(bytes <= MAX_LINE_BYTES, "SFEN line exceeds {MAX_LINE_BYTES} bytes");
        ensure!(lines <= MAX_POSITIONS, "input exceeds {MAX_POSITIONS} lines");
        let sfen = line.trim();
        if sfen.is_empty() {
            continue;
        }
        let mut position = Position::new();
        position
            .set_sfen(sfen)
            .with_context(|| format!("invalid SFEN on line {lines}"))?;
        positions.push(position);
    }
    if positions.is_empty() {
        let mut position = Position::new();
        position.set_hirate();
        positions.push(position);
    }
    Ok(positions)
}

fn measure(mut operation: impl FnMut() -> Result<f64>, repetitions: usize) -> Result<Value> {
    let started = Instant::now();
    let mut checksum = 0.0;
    for _ in 0..repetitions {
        checksum += operation()?;
    }
    let seconds = started.elapsed().as_secs_f64();
    ensure!(checksum.is_finite(), "non-finite output checksum");
    Ok(json!({"seconds": seconds, "corpus_passes": repetitions, "checksum": checksum}))
}

fn encode(config: &Config, positions: &[Position]) -> Result<Value> {
    let setup = Instant::now();
    let mut first = vec![0.0; FEATURES1_SIZE];
    let mut second = vec![0.0; FEATURES2_SIZE];
    let setup_seconds = setup.elapsed().as_secs_f64();
    let mut operation = || {
        let mut checksum = 0.0;
        for position in positions {
            if config.mode == Mode::EncodeTokens {
                let input = tokens::encode(black_box(position))?;
                checksum += f64::from(input[0]);
                black_box(input);
            } else {
                first.fill(0.0);
                second.fill(0.0);
                make_input_features(black_box(position), &mut first, &mut second);
                checksum += f64::from(first[0]);
                black_box((&first, &second));
            }
        }
        Ok(checksum)
    };
    let warmup = measure(&mut operation, config.warmup)?;
    let measured = measure(&mut operation, config.iterations)?;
    Ok(
        json!({"backend": "encoder-only", "setup_seconds": setup_seconds, "warmup": warmup, "measured": measured}),
    )
}

struct SyntheticEvaluator(InputFormat);

impl Evaluator for SyntheticEvaluator {
    fn input_format(&self) -> Option<InputFormat> {
        Some(self.0)
    }

    fn evaluate(&mut self, position: &Position, logits: &mut [f32]) -> Result<f32> {
        let marker = if position.in_check() { 1.0 } else { 0.0 };
        logits.fill(marker);
        Ok(0.5)
    }
}

fn stats(stats: CacheStats) -> Value {
    json!({"requests": stats.requests, "nn_calls": stats.nn_calls, "hits": stats.hits})
}

struct EncodedControl<E> {
    inner: E,
    enabled: bool,
}

impl<E: Evaluator> Evaluator for EncodedControl<E> {
    fn input_format(&self) -> Option<InputFormat> {
        self.inner.input_format()
    }

    fn evaluate(&mut self, position: &Position, logits: &mut [f32]) -> Result<f32> {
        self.inner.evaluate(position, logits)
    }

    fn evaluate_encoded(
        &mut self,
        input: EncodedInput<'_>,
        logits: &mut [f32],
    ) -> Result<Option<f32>> {
        if self.enabled {
            self.inner.evaluate_encoded(input, logits)
        } else {
            Ok(None)
        }
    }
}

fn cache<E: Evaluator>(inner: E, config: &Config, positions: &[Position]) -> Result<Value> {
    let setup = Instant::now();
    let mut evaluator = CachedEvaluator::new(
        EncodedControl {
            inner,
            enabled: config.reuse_encoded,
        },
        config.cache_mib,
    )?;
    let setup_seconds = setup.elapsed().as_secs_f64();
    let mut logits = [0.0; MAX_MOVE_LABEL_NUM];
    let mut pass = |evaluator: &mut CachedEvaluator<EncodedControl<E>>| {
        let mut checksum = 0.0;
        for position in positions {
            let value = evaluator.evaluate(black_box(position), &mut logits)?;
            checksum += f64::from(value) + f64::from(logits[0]);
            black_box(&logits);
        }
        Ok(checksum)
    };
    let warmup = measure(|| pass(&mut evaluator), config.warmup)?;
    evaluator.clear();
    let cold = measure(|| pass(&mut evaluator), 1)?;
    let cold_stats = stats(evaluator.stats());
    evaluator.reset_stats();
    let measured = measure(|| pass(&mut evaluator), config.iterations)?;
    let warm_stats = stats(evaluator.stats());
    Ok(json!({
        "setup_seconds": setup_seconds,
        "allocated_bytes": evaluator.allocated_bytes(),
        "warmup": warmup,
        "cold": cold,
        "cold_stats": cold_stats,
        "measured": measured,
        "measured_stats": warm_stats
    }))
}

#[cfg(feature = "onnx")]
fn onnx(config: &Config, positions: &[Position]) -> Result<Value> {
    use rshogi_dl::onnx::{ModelEvaluator, ModelOptions, OnnxConfig};

    let setup = Instant::now();
    let mut evaluator = ModelEvaluator::with_options(
        &OnnxConfig {
            model_path: config.model.clone().context("model required")?,
            gpu_id: config.device,
            batch_size: config.batch_size,
            use_tensorrt: false,
            tensorrt_cache: None,
        },
        &ModelOptions {
            cuda_tf32: config.cuda_tf32,
            profiling: None,
        },
    )?;
    let model_setup_seconds = setup.elapsed().as_secs_f64();
    let format = match evaluator.input_format() {
        Some(InputFormat::Basic) => "basic",
        Some(InputFormat::StmTokensV1) => "stm-tokens-v1",
        None => bail!("model has no declared input format"),
    };
    if config.mode == Mode::OnnxCache {
        let mut report = cache(evaluator, config, positions)?;
        report["model_setup_seconds"] = json!(model_setup_seconds);
        report["input_format"] = json!(format);
        return Ok(report);
    }
    let mut values = Vec::with_capacity(config.batch_size);
    let mut policy = Vec::with_capacity(config.batch_size * MAX_MOVE_LABEL_NUM);
    let setup_seconds = setup.elapsed().as_secs_f64();
    let mut operation = || {
        let mut checksum = 0.0;
        for batch in positions.chunks(config.batch_size) {
            values.clear();
            policy.clear();
            evaluator.evaluate_with_policy(black_box(batch), &mut values, &mut policy)?;
            checksum += f64::from(values[0]) + f64::from(policy[0]);
            black_box((&values, &policy));
        }
        Ok(checksum)
    };
    let warmup = measure(&mut operation, config.warmup)?;
    let measured = measure(&mut operation, config.iterations)?;
    let requests = config.iterations * positions.len();
    Ok(
        json!({"setup_seconds": setup_seconds, "input_format": format, "warmup": warmup, "measured": measured,
        "measured_stats": {"requests": requests, "nn_calls": config.iterations * positions.len().div_ceil(config.batch_size), "hits": 0}}),
    )
}

#[cfg(not(feature = "onnx"))]
fn onnx(_: &Config, _: &[Position]) -> Result<Value> {
    bail!("ONNX modes require --features onnx")
}

fn run(config: &Config, positions: &[Position]) -> Result<Value> {
    config.check_requests(positions.len())?;
    let mut report = match config.mode {
        Mode::EncodeBasic | Mode::EncodeTokens => encode(config, positions)?,
        Mode::CacheBasic => cache(SyntheticEvaluator(InputFormat::Basic), config, positions)?,
        Mode::CacheTokens => {
            cache(SyntheticEvaluator(InputFormat::StmTokensV1), config, positions)?
        }
        Mode::Onnx | Mode::OnnxCache => onnx(config, positions)?,
    };
    report["mode"] = json!(config.mode.name());
    report["backend"] = json!(match config.mode {
        Mode::CacheBasic | Mode::CacheTokens => "synthetic-cache-only",
        Mode::EncodeBasic | Mode::EncodeTokens => "encoder-only",
        Mode::Onnx | Mode::OnnxCache => "onnx-runtime",
    });
    report["positions"] = json!(positions.len());
    report["iterations"] = json!(config.iterations);
    report["warmup_iterations"] = json!(config.warmup);
    report["batch_size"] = json!(config.batch_size);
    report["reuse_encoded"] = json!(
        matches!(config.mode, Mode::CacheBasic | Mode::CacheTokens | Mode::OnnxCache)
            .then_some(config.reuse_encoded)
    );
    report["cache_mib"] = json!(
        matches!(config.mode, Mode::CacheBasic | Mode::CacheTokens | Mode::OnnxCache)
            .then_some(config.cache_mib)
    );
    let onnx = matches!(config.mode, Mode::Onnx | Mode::OnnxCache);
    report["device"] = json!(onnx.then_some(config.device));
    report["cuda_tf32"] = json!(onnx.then_some(config.cuda_tf32));
    if !matches!(config.mode, Mode::Onnx | Mode::OnnxCache) {
        report["input_format"] = json!(match config.mode {
            Mode::EncodeBasic | Mode::CacheBasic => "basic",
            _ => "stm-tokens-v1",
        });
    }
    Ok(report)
}

fn main() -> Result<()> {
    let config = Config::parse(std::env::args().skip(1))?;
    let started = Instant::now();
    let positions = read_positions(io::stdin().lock())?;
    let input_seconds = started.elapsed().as_secs_f64();
    let mut report = run(&config, &positions)?;
    report["input_seconds"] = json!(input_seconds);
    serde_json::to_writer(io::stdout().lock(), &report)?;
    println!();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(args: &[&str]) -> Result<Config> {
        Config::parse(args.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn invalid_limits_and_incompatible_modes_are_rejected() {
        for args in [
            vec!["cache-tokens", "--iterations", "0"],
            vec!["cache-tokens", "--iterations", "10000001"],
            vec!["cache-tokens", "--warmup", "10000001"],
            vec!["cache-tokens", "--cache-mib", "1025"],
            vec!["cache-tokens", "--batch-size", "2"],
            vec!["onnx"],
            vec!["encode-tokens", "--model", "model.onnx"],
            vec!["encode-tokens", "--device", "0"],
            vec!["encode-tokens", "--cache-mib", "1"],
            vec!["onnx", "--model", "model.onnx", "--reuse-encoded", "false"],
            vec!["cache-tokens", "--reuse-encoded", "invalid"],
            vec!["cache-tokens", "--iterations", "1", "--iterations", "2"],
            vec!["cache-tokens", "--unknown", "1"],
            vec!["cache-tokens", "--warmup"],
        ] {
            assert!(config(&args).is_err(), "{args:?}");
        }
        let config = config(&["cache-tokens", "--iterations", "10000000"])
            .expect("valid single-position iteration count");
        assert!(config.check_requests(2).is_err());
    }

    #[test]
    fn input_is_bounded_and_empty_input_uses_start_position() -> Result<()> {
        assert_eq!(read_positions(io::Cursor::new(""))?.len(), 1);
        assert!(read_positions(io::Cursor::new("x".repeat(MAX_LINE_BYTES + 1))).is_err());
        assert!(read_positions(io::Cursor::new("\n".repeat(MAX_POSITIONS + 1))).is_err());
        assert!(read_positions(io::Cursor::new("invalid sfen\n")).is_err());
        Ok(())
    }

    #[test]
    fn cold_and_warm_cache_measure_actual_backend_calls() -> Result<()> {
        let positions = read_positions(io::Cursor::new(""))?;
        let report = run(
            &config(&[
                "cache-tokens",
                "--iterations",
                "3",
                "--warmup",
                "0",
                "--cache-mib",
                "1",
            ])?,
            &positions,
        )?;
        assert_eq!(report["backend"], "synthetic-cache-only");
        assert_eq!(report["cold_stats"], json!({"requests": 1, "nn_calls": 1, "hits": 0}));
        assert_eq!(report["measured_stats"], json!({"requests": 3, "nn_calls": 0, "hits": 3}));
        assert_eq!(report["measured"]["checksum"], 1.5);
        let report = run(
            &config(&[
                "cache-tokens",
                "--iterations",
                "3",
                "--warmup",
                "0",
                "--cache-mib",
                "0",
            ])?,
            &positions,
        )?;
        assert_eq!(report["measured_stats"], json!({"requests": 3, "nn_calls": 3, "hits": 0}));
        assert_eq!(report["allocated_bytes"], 0);
        Ok(())
    }
}
