mod support;

use std::hint::black_box;
use std::io::{self, BufRead, Read, Write};
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
const USAGE: &str = "benchmark MODE [--iterations N] [--warmup N] [--batch-size N] [--cache-mib N] [--model FILE] [--device ID] [--cuda-tf32 true|false] [--reuse-encoded true|false]; MODE: encode-basic|encode-tokens|cache-basic|cache-tokens|onnx|onnx-cache|onnx-batch-cache; SFEN lines on stdin (empty = start position)";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    EncodeBasic,
    EncodeTokens,
    CacheBasic,
    CacheTokens,
    Onnx,
    OnnxCache,
    OnnxBatchCache,
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
            Self::OnnxBatchCache => "onnx-batch-cache",
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
            Some("onnx-batch-cache") => Mode::OnnxBatchCache,
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
        let actual_model = matches!(mode, Mode::Onnx | Mode::OnnxCache | Mode::OnnxBatchCache);
        ensure!(
            actual_model == config.model.is_some(),
            "--model is required only for ONNX modes"
        );
        ensure!(
            actual_model || (!seen.iter().any(|f| f == "--device" || f == "--cuda-tf32")),
            "device and precision options require an ONNX mode"
        );
        ensure!(
            matches!(
                mode,
                Mode::CacheBasic | Mode::CacheTokens | Mode::OnnxCache | Mode::OnnxBatchCache
            ) || !seen.iter().any(|f| f == "--cache-mib"),
            "--cache-mib requires a cache mode"
        );
        ensure!(
            matches!(mode, Mode::CacheBasic | Mode::CacheTokens | Mode::OnnxCache)
                || !seen.iter().any(|f| f == "--reuse-encoded"),
            "--reuse-encoded requires a scalar cache mode"
        );
        ensure!(
            matches!(mode, Mode::Onnx | Mode::OnnxBatchCache) || config.batch_size == 1,
            "batch sizes above 1 require onnx or onnx-batch-cache mode"
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

#[cfg(feature = "inference-diagnostics")]
fn timing_delta(
    after: Option<rshogi_dl::InferenceTimings>,
    before: Option<rshogi_dl::InferenceTimings>,
) -> Result<Value> {
    match (after, before) {
        (Some(after), Some(before)) => {
            let delta =
                after.checked_since(before).context("inference timing counters regressed")?;
            let stages: serde_json::Map<String, Value> = rshogi_dl::InferenceStage::ALL
                .into_iter()
                .map(|stage| {
                    let timing = delta.stages[stage as usize];
                    (stage.name().to_owned(), json!({
                        "calls": timing.calls, "errors": timing.errors, "nanoseconds": timing.nanoseconds,
                    }))
                })
                .collect();
            Ok(json!({"forward_calls": delta.forward_calls,
                "forward_failures": delta.forward_failures, "positions": delta.positions, "stages": stages}))
        }
        (None, None) => Ok(Value::Null),
        _ => bail!("inference timing capability changed"),
    }
}

#[cfg(feature = "inference-diagnostics")]
fn with_inference_timings(mut report: Value, warmup: Value, cold: Value, measured: Value) -> Value {
    report["inference_timings"] = json!({"warmup": warmup, "cold": cold, "measured": measured});
    report
}

struct EncodedControl<E> {
    inner: E,
    enabled: bool,
}

impl<E: Evaluator> Evaluator for EncodedControl<E> {
    #[cfg(feature = "inference-diagnostics")]
    fn inference_timings(&self) -> Option<rshogi_dl::InferenceTimings> {
        self.inner.inference_timings()
    }

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
    #[cfg(feature = "inference-diagnostics")]
    let before = evaluator.inference_timings();
    let warmup = measure(|| pass(&mut evaluator), config.warmup)?;
    #[cfg(feature = "inference-diagnostics")]
    let warmup_timings = timing_delta(evaluator.inference_timings(), before)?;
    evaluator.clear();
    #[cfg(feature = "inference-diagnostics")]
    let before = evaluator.inference_timings();
    let cold = measure(|| pass(&mut evaluator), 1)?;
    #[cfg(feature = "inference-diagnostics")]
    let cold_timings = timing_delta(evaluator.inference_timings(), before)?;
    let cold_stats = stats(evaluator.stats());
    evaluator.reset_stats();
    #[cfg(feature = "inference-diagnostics")]
    let before = evaluator.inference_timings();
    let measured = measure(|| pass(&mut evaluator), config.iterations)?;
    #[cfg(feature = "inference-diagnostics")]
    let measured_timings = timing_delta(evaluator.inference_timings(), before)?;
    let warm_stats = stats(evaluator.stats());
    let report = json!({
        "setup_seconds": setup_seconds,
        "allocated_bytes": evaluator.allocated_bytes(),
        "warmup": warmup,
        "cold": cold,
        "cold_stats": cold_stats,
        "measured": measured,
        "measured_stats": warm_stats
    });
    #[cfg(feature = "inference-diagnostics")]
    let report = with_inference_timings(report, warmup_timings, cold_timings, measured_timings);
    Ok(report)
}

#[cfg(feature = "onnx")]
fn batch_cache<E: Evaluator>(inner: E, config: &Config, positions: &[Position]) -> Result<Value> {
    let setup = Instant::now();
    let format = inner.input_format().context("missing input format")?;
    let mut evaluator =
        CachedEvaluator::with_batch_capacity(inner, config.cache_mib, config.batch_size)?;
    let mut input = rshogi_dl::InputBatchBuffer::new(format, config.batch_size)?;
    let mut values = vec![0.0; config.batch_size];
    let mut policy = vec![0.0; config.batch_size * MAX_MOVE_LABEL_NUM];
    let setup_seconds = setup.elapsed().as_secs_f64();
    let mut pass = |evaluator: &mut CachedEvaluator<E>| {
        let mut checksum = 0.0;
        for positions in positions.chunks(config.batch_size) {
            input.clear();
            for position in positions {
                input.push(black_box(position))?;
            }
            let n = positions.len();
            evaluator.evaluate_encoded_batch(
                input.view(),
                &mut values[..n],
                &mut policy[..n * MAX_MOVE_LABEL_NUM],
            )?;
            checksum += f64::from(values[0]) + f64::from(policy[0]);
            black_box((&values, &policy));
        }
        Ok(checksum)
    };
    #[cfg(feature = "inference-diagnostics")]
    let before = evaluator.inference_timings();
    let warmup = measure(|| pass(&mut evaluator), config.warmup)?;
    #[cfg(feature = "inference-diagnostics")]
    let warmup_timings = timing_delta(evaluator.inference_timings(), before)?;
    evaluator.clear();
    #[cfg(feature = "inference-diagnostics")]
    let before = evaluator.inference_timings();
    let cold = measure(|| pass(&mut evaluator), 1)?;
    #[cfg(feature = "inference-diagnostics")]
    let cold_timings = timing_delta(evaluator.inference_timings(), before)?;
    let cold_stats = stats(evaluator.stats());
    let cold_nn_positions = evaluator.nn_positions();
    evaluator.reset_stats();
    #[cfg(feature = "inference-diagnostics")]
    let before = evaluator.inference_timings();
    let measured = measure(|| pass(&mut evaluator), config.iterations)?;
    #[cfg(feature = "inference-diagnostics")]
    let measured_timings = timing_delta(evaluator.inference_timings(), before)?;
    let report = json!({"setup_seconds":setup_seconds,"allocated_bytes":evaluator.allocated_bytes(),"batch_allocated_bytes":evaluator.batch_allocated_bytes(),"input_allocated_bytes":input.allocated_bytes(),"warmup":warmup,"cold":cold,"cold_stats":cold_stats,"cold_nn_positions":cold_nn_positions,"measured":measured,"measured_stats":stats(evaluator.stats()),"measured_nn_positions":evaluator.nn_positions()});
    #[cfg(feature = "inference-diagnostics")]
    let report = with_inference_timings(report, warmup_timings, cold_timings, measured_timings);
    Ok(report)
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
    if config.mode == Mode::OnnxBatchCache {
        let mut report = batch_cache(evaluator, config, positions)?;
        report["model_setup_seconds"] = json!(model_setup_seconds);
        report["input_format"] = json!(format);
        return Ok(report);
    }
    let mut values = Vec::with_capacity(config.batch_size);
    let mut policy = Vec::with_capacity(config.batch_size * MAX_MOVE_LABEL_NUM);
    let setup_seconds = setup.elapsed().as_secs_f64();
    let mut operation = |evaluator: &mut ModelEvaluator| {
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
    #[cfg(feature = "inference-diagnostics")]
    let before = evaluator.inference_timings();
    let warmup = measure(|| operation(&mut evaluator), config.warmup)?;
    #[cfg(feature = "inference-diagnostics")]
    let warmup_timings = timing_delta(evaluator.inference_timings(), before)?;
    #[cfg(feature = "inference-diagnostics")]
    let before = evaluator.inference_timings();
    let measured = measure(|| operation(&mut evaluator), config.iterations)?;
    #[cfg(feature = "inference-diagnostics")]
    let measured_timings = timing_delta(evaluator.inference_timings(), before)?;
    let requests = config.iterations * positions.len();
    let report = json!({"setup_seconds": setup_seconds, "input_format": format, "warmup": warmup, "measured": measured,
        "measured_stats": {"requests": requests, "nn_calls": config.iterations * positions.len().div_ceil(config.batch_size), "hits": 0}});
    #[cfg(feature = "inference-diagnostics")]
    let report = with_inference_timings(report, warmup_timings, Value::Null, measured_timings);
    Ok(report)
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
        Mode::Onnx | Mode::OnnxCache | Mode::OnnxBatchCache => onnx(config, positions)?,
    };
    report["mode"] = json!(config.mode.name());
    report["backend"] = json!(match config.mode {
        Mode::CacheBasic | Mode::CacheTokens => "synthetic-cache-only",
        Mode::EncodeBasic | Mode::EncodeTokens => "encoder-only",
        Mode::Onnx | Mode::OnnxCache | Mode::OnnxBatchCache => "onnx-runtime",
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
        matches!(
            config.mode,
            Mode::CacheBasic | Mode::CacheTokens | Mode::OnnxCache | Mode::OnnxBatchCache
        )
        .then_some(config.cache_mib)
    );
    let onnx = matches!(config.mode, Mode::Onnx | Mode::OnnxCache | Mode::OnnxBatchCache);
    report["device"] = json!(onnx.then_some(config.device));
    report["cuda_tf32"] = json!(onnx.then_some(config.cuda_tf32));
    if !matches!(config.mode, Mode::Onnx | Mode::OnnxCache | Mode::OnnxBatchCache) {
        report["input_format"] = json!(match config.mode {
            Mode::EncodeBasic | Mode::CacheBasic => "basic",
            _ => "stm-tokens-v1",
        });
    }
    Ok(report)
}

fn main() {
    support::finish(run_command());
}

fn run_command() -> Result<()> {
    let config = Config::parse(std::env::args().skip(1))?;
    let started = Instant::now();
    let positions = read_positions(io::stdin().lock())?;
    let input_seconds = started.elapsed().as_secs_f64();
    let mut report = run(&config, &positions)?;
    report["input_seconds"] = json!(input_seconds);
    serde_json::to_writer(io::stdout().lock(), &report)?;
    writeln!(io::stdout())?;
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
            vec![
                "onnx-batch-cache",
                "--model",
                "model.onnx",
                "--reuse-encoded",
                "false",
            ],
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
    fn encoded_batch_cache_accepts_explicit_capacity_and_cache_off() -> Result<()> {
        let config = config(&[
            "onnx-batch-cache",
            "--model",
            "model.onnx",
            "--batch-size",
            "8",
            "--cache-mib",
            "0",
        ])?;
        assert_eq!(config.mode, Mode::OnnxBatchCache);
        assert_eq!(config.batch_size, 8);
        assert_eq!(config.cache_mib, 0);
        Ok(())
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
    #[cfg(feature = "inference-diagnostics")]
    #[test]
    fn cache_timing_snapshots_are_independent_of_cache_stats_resets() -> Result<()> {
        struct Instrumented(rshogi_dl::InferenceTimings);
        impl Evaluator for Instrumented {
            fn input_format(&self) -> Option<InputFormat> {
                Some(InputFormat::StmTokensV1)
            }
            fn inference_timings(&self) -> Option<rshogi_dl::InferenceTimings> {
                Some(self.0)
            }
            fn evaluate(&mut self, _: &Position, logits: &mut [f32]) -> Result<f32> {
                self.0.forward_calls += 1;
                self.0.positions += 1;
                let phase = &mut self.0.stages[rshogi_dl::InferenceStage::TensorRefs as usize];
                phase.calls += 1;
                phase.nanoseconds += 2;
                logits.fill(0.0);
                Ok(0.5)
            }
        }
        let report = cache(
            Instrumented(rshogi_dl::InferenceTimings::default()),
            &config(&[
                "cache-tokens",
                "--warmup",
                "0",
                "--iterations",
                "3",
                "--cache-mib",
                "1",
            ])?,
            &read_positions(io::Cursor::new(""))?,
        )?;
        let timings = &report["inference_timings"];
        assert_eq!(timings["warmup"]["forward_calls"], 0);
        assert_eq!(timings["cold"]["forward_calls"], 1);
        assert_eq!(timings["cold"]["stages"]["tensor_refs"]["nanoseconds"], 2);
        assert_eq!(timings["measured"]["forward_calls"], 0);
        assert_eq!(timings["measured"]["stages"]["tensor_refs"]["nanoseconds"], 0);
        assert_eq!(report["measured_stats"]["hits"], 3);
        assert!(timing_delta(None, None)?.is_null());
        assert!(timing_delta(None, Some(rshogi_dl::InferenceTimings::default())).is_err());
        Ok(())
    }
}
