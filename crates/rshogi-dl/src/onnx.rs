//! ONNX policy/value 推論と入力形式の検証。

use std::path::{Path, PathBuf};

use anyhow::Result;
use rayon::prelude::*;
use rshogi_core::position::Position;

use ort::memory::{AllocationDevice, AllocatorType, MemoryInfo, MemoryType};
use ort::session::Session;
use ort::value::TensorRef;

use crate::features::{
    FEATURES1_SIZE, FEATURES2_SIZE, INPUT1_CHANNELS, INPUT2_CHANNELS, MAX_MOVE_LABEL_NUM,
    make_input_features,
};
use crate::{EncodedBatch, EncodedInput};

#[cfg(feature = "inference-diagnostics")]
use crate::diagnostics::{ForwardTimer, InferenceStage};

#[cfg(feature = "inference-diagnostics")]
macro_rules! inference_try {
    ($timer:ident, $operation:expr) => {
        match $operation {
            Ok(value) => value,
            Err(error) => {
                $timer.fail();
                return Err(error);
            }
        }
    };
}

#[cfg(not(feature = "inference-diagnostics"))]
macro_rules! inference_try {
    ($timer:ident, $operation:expr) => {
        $operation?
    };
}

#[cfg(feature = "inference-diagnostics")]
macro_rules! inference_ensure {
    ($timer:ident, $condition:expr, $($message:tt)*) => {
        if !($condition) {
            $timer.fail();
            anyhow::bail!($($message)*);
        }
    };
}

#[cfg(not(feature = "inference-diagnostics"))]
macro_rules! inference_ensure {
    ($timer:ident, $condition:expr, $($message:tt)*) => {
        anyhow::ensure!($condition, $($message)*);
    };
}

/// ONNX value evaluator の設定
pub struct OnnxConfig {
    /// dlshogi ONNX モデルのパス
    pub model_path: PathBuf,
    /// CUDA device id。負値で CPU 推論。
    pub gpu_id: i32,
    /// TensorRT EP (FP16) を使う。false なら CUDA EP (FP32)。
    pub use_tensorrt: bool,
    /// TensorRT エンジンキャッシュの保存先（`use_tensorrt` 時のみ有効）
    pub tensorrt_cache: Option<PathBuf>,
    /// 1 回の推論あたりの最大局面数
    pub batch_size: usize,
}

/// dlshogi value head を ONNX Runtime で評価する
pub struct OnnxEvaluator {
    sessions: BasicSessions,
    batch_size: usize,
    f1_buf: Vec<f32>,
    f2_buf: Vec<f32>,
    single_values: Vec<f32>,
    single_policy: Vec<f32>,
    #[cfg(feature = "inference-diagnostics")]
    timings: crate::InferenceTimings,
}

struct BasicSessions {
    main: EvaluatorSession,
    #[cfg(feature = "cuda-basic-root-session")]
    single_row: Option<Box<EvaluatorSession>>,
}

impl BasicSessions {
    #[cfg(feature = "cuda-basic-root-session")]
    fn for_rows(&mut self, rows: usize) -> &mut EvaluatorSession {
        if rows == 1
            && let Some(single_row) = &mut self.single_row
        {
            single_row
        } else {
            &mut self.main
        }
    }

    #[cfg(not(feature = "cuda-basic-root-session"))]
    fn for_rows(&mut self, _: usize) -> &mut EvaluatorSession {
        &mut self.main
    }
}

struct EvaluatorSession {
    inner: Session,
    #[cfg(feature = "cuda-pinned-output")]
    pinned_device: Option<i32>,
}

impl EvaluatorSession {
    fn output_memory(&self) -> Result<MemoryInfo> {
        #[cfg(feature = "cuda-pinned-output")]
        if let Some(device) = self.pinned_device {
            return MemoryInfo::new(
                AllocationDevice::CUDA_PINNED,
                device,
                AllocatorType::Device,
                MemoryType::CPUOutput,
            )
            .map_err(ort_err);
        }
        MemoryInfo::new(AllocationDevice::CPU, 0, AllocatorType::Device, MemoryType::CPUOutput)
            .map_err(ort_err)
    }
}

fn ort_err(e: ort::Error) -> anyhow::Error {
    anyhow::anyhow!("ONNX Runtime error: {e}")
}

/// `ORT_DYLIB_PATH` が指す共有ライブラリが実在することを確認する。
///
/// load-dynamic feature 下では未設定時に ort がシステムパスを探索してハングするため、
/// 推論に入る前に明示的に弾く。
fn ensure_ort_dylib(gpu_id: i32) -> Result<()> {
    match std::env::var("ORT_DYLIB_PATH") {
        Ok(path) if !path.is_empty() => {
            if !Path::new(&path).is_file() {
                anyhow::bail!(
                    "ORT_DYLIB_PATH is set to '{path}' but the file does not exist \
                     (or is not a regular file).\n\
                     Download ONNX Runtime from:\n  \
                     https://github.com/microsoft/onnxruntime/releases"
                );
            }
            eprintln!("ORT_DYLIB_PATH: {path}");
            Ok(())
        }
        _ => {
            let mode_hint = if gpu_id >= 0 {
                "GPU inference requires a GPU-enabled ONNX Runtime (onnxruntime-linux-x64-gpu-*)."
            } else {
                "CPU inference requires an ONNX Runtime library (GPU or CPU build)."
            };
            anyhow::bail!(
                "ORT_DYLIB_PATH environment variable is not set.\n\
                 {mode_hint}\n\
                 Download from: https://github.com/microsoft/onnxruntime/releases\n\
                 Example:\n  \
                 ORT_DYLIB_PATH=/path/to/libonnxruntime.so ..."
            );
        }
    }
}

fn create_session(
    cfg: &OnnxConfig,
    profile: Option<&Path>,
    cuda_tf32: bool,
) -> Result<EvaluatorSession> {
    use ort::ep::ExecutionProvider;

    if cfg.batch_size == 0 {
        anyhow::bail!("OnnxConfig.batch_size must be > 0");
    }

    ensure_ort_dylib(cfg.gpu_id)?;

    eprintln!("Loading ONNX model: {}", cfg.model_path.display());

    let mut builder = crate::environment::session_builder()?
        .with_optimization_level(ort::session::builder::GraphOptimizationLevel::All)
        .map_err(|e| anyhow::anyhow!("ORT builder error: {e}"))?
        .with_intra_threads(1)
        .map_err(|e| anyhow::anyhow!("ORT builder error: {e}"))?;

    if let Some(path) = profile {
        builder = builder
            .with_profiling(path)
            .map_err(|e| anyhow::anyhow!("ORT profiling configuration: {e}"))?;
    }
    let session = if cfg.gpu_id >= 0 {
        if cfg.use_tensorrt {
            eprintln!("Using TensorRT (FP16) on GPU {}", cfg.gpu_id);

            let trt_ep = ort::execution_providers::TensorRTExecutionProvider::default()
                .with_device_id(cfg.gpu_id)
                .with_fp16(true)
                .with_engine_cache(cfg.tensorrt_cache.is_some());
            // 形状の範囲を 1..=batch_size に固定する。未指定だと TensorRT EP は見たことのない
            // batch 数が来るたびに engine を作り直し、batch 数が毎回変わる呼び出し側で数秒ずつ止まる。
            let shapes = |n: usize| {
                format!("input1:{n}x{INPUT1_CHANNELS}x9x9,input2:{n}x{INPUT2_CHANNELS}x9x9")
            };
            let trt_ep = trt_ep
                .with_profile_min_shapes(shapes(1))
                .with_profile_opt_shapes(shapes(cfg.batch_size))
                .with_profile_max_shapes(shapes(cfg.batch_size));
            let trt_ep = if let Some(cache_path) = cfg.tensorrt_cache.as_deref() {
                let cache_str = cache_path.to_str().ok_or_else(|| {
                    anyhow::anyhow!(
                        "TensorRT cache path contains non-UTF-8 characters: {}",
                        cache_path.display()
                    )
                })?;
                eprintln!("TensorRT engine cache: {}", cache_path.display());
                trt_ep.with_engine_cache_path(cache_str)
            } else {
                eprintln!("TensorRT engine cache: disabled");
                trt_ep
            };

            match trt_ep.is_available() {
                Ok(true) => eprintln!("TensorRT execution provider: available"),
                Ok(false) => {
                    anyhow::bail!(
                        "TensorRTExecutionProvider is NOT available.\n\
                             Ensure TensorRT (libnvinfer.so.10) is in LD_LIBRARY_PATH.\n\
                             To use CUDA EP instead, omit TensorRT."
                    );
                }
                Err(e) => {
                    eprintln!("WARNING: Failed to check TensorRT EP availability: {e}");
                }
            }

            // TensorRT EP が局面形状をサポートしない場合に備え CUDA EP を後段に登録する
            let cuda_ep = ort::execution_providers::CUDAExecutionProvider::default()
                .with_device_id(cfg.gpu_id)
                .build()
                .error_on_failure();
            let trt_ep = trt_ep.build().error_on_failure();

            builder
                .with_execution_providers([trt_ep, cuda_ep])
                .map_err(|e| anyhow::anyhow!("TensorRT/CUDA EP registration failed: {e}"))?
                .commit_from_file(&cfg.model_path)
                .map_err(ort_err)?
        } else {
            eprintln!("Using CUDA GPU {}", cfg.gpu_id);

            let mut cuda_ep = ort::execution_providers::CUDAExecutionProvider::default()
                .with_device_id(cfg.gpu_id);
            if !cuda_tf32 {
                cuda_ep = cuda_ep.with_tf32(false);
                eprintln!("CUDA TF32: disabled");
            }
            match cuda_ep.is_available() {
                Ok(true) => eprintln!("CUDA execution provider: available"),
                Ok(false) => {
                    anyhow::bail!(
                        "CUDAExecutionProvider is NOT available in the loaded ONNX Runtime library.\n\
                             The library may be a CPU-only build.\n\
                             Check ORT_DYLIB_PATH points to a GPU-enabled onnxruntime."
                    );
                }
                Err(e) => {
                    eprintln!("WARNING: Failed to check CUDA EP availability: {e}");
                }
            }

            let ep = cuda_ep.build().error_on_failure();
            builder
                .with_execution_providers([ep])
                .map_err(|e| {
                    anyhow::anyhow!(
                        "CUDA EP registration failed (is onnxruntime-gpu installed?): {e}"
                    )
                })?
                .commit_from_file(&cfg.model_path)
                .map_err(ort_err)?
        }
    } else {
        eprintln!("Using CPU");
        builder.commit_from_file(&cfg.model_path).map_err(ort_err)?
    };

    Ok(EvaluatorSession {
        inner: session,
        #[cfg(feature = "cuda-pinned-output")]
        pinned_device: (cfg.gpu_id >= 0 && !cfg.use_tensorrt).then_some(cfg.gpu_id),
    })
}

impl OnnxEvaluator {
    /// 基本形式のモデルを読み込む。token形式は受理しない。
    pub fn new(cfg: &OnnxConfig) -> Result<Self> {
        let session = create_session(cfg, None, true)?;
        Self::from_session(
            session,
            cfg,
            #[cfg(feature = "cuda-basic-root-session")]
            &ModelOptions::default(),
        )
    }

    fn from_session(
        session: EvaluatorSession,
        cfg: &OnnxConfig,
        #[cfg(feature = "cuda-basic-root-session")] options: &ModelOptions,
    ) -> Result<Self> {
        let batch_size = cfg.batch_size;
        anyhow::ensure!(
            model_format(&session.inner)? == crate::InputFormat::Basic,
            "legacy evaluator requires basic inputs"
        );
        validate_model(&session.inner, batch_size, crate::InputFormat::Basic)?;
        #[cfg(feature = "cuda-basic-root-session")]
        // CUDA の畳み込み実行計画は入力形状が変わると再作成される。
        let single_row = if cfg.gpu_id >= 0
            && !cfg.use_tensorrt
            && batch_size > 1
            && options.profiling.is_none()
        {
            let session = create_session(cfg, None, options.cuda_tf32)?;
            validate_model(&session.inner, batch_size, crate::InputFormat::Basic)?;
            Some(Box::new(session))
        } else {
            None
        };
        eprintln!("Basic ONNX model loaded. Batch size: {batch_size}");

        Ok(Self {
            sessions: BasicSessions {
                main: session,
                #[cfg(feature = "cuda-basic-root-session")]
                single_row,
            },
            batch_size,
            f1_buf: vec![0.0f32; batch_size * FEATURES1_SIZE],
            f2_buf: vec![0.0f32; batch_size * FEATURES2_SIZE],
            single_values: Vec::with_capacity(1),
            single_policy: Vec::with_capacity(MAX_MOVE_LABEL_NUM),
            #[cfg(feature = "inference-diagnostics")]
            timings: crate::InferenceTimings::default(),
        })
    }

    /// 局面を batch に分け、手番側の勝率を返す。
    pub fn evaluate(&mut self, positions: &[Position]) -> Result<Vec<f32>> {
        let mut out = Vec::with_capacity(positions.len());
        for chunk in positions.chunks(self.batch_size) {
            self.run_chunk(chunk.par_iter(), &mut out, None)?;
        }
        Ok(out)
    }

    /// 最大 batch 以下の入力を推論し、手番側勝率と policy logits を追記する。
    pub fn evaluate_with_policy<'a>(
        &mut self,
        positions: impl IndexedParallelIterator<Item = &'a Position>,
        values: &mut Vec<f32>,
        policy: &mut Vec<f32>,
    ) -> Result<()> {
        if positions.len() > self.batch_size {
            anyhow::bail!(
                "evaluate_with_policy: {} positions exceed batch_size {}",
                positions.len(),
                self.batch_size
            );
        }
        self.run_chunk(positions, values, Some(policy))
    }

    fn run_chunk<'a>(
        &mut self,
        positions: impl IndexedParallelIterator<Item = &'a Position>,
        out: &mut Vec<f32>,
        policy_out: Option<&mut Vec<f32>>,
    ) -> Result<()> {
        let n = positions.len();
        debug_assert!(n <= self.batch_size);
        if n == 0 {
            return Ok(());
        }

        self.f1_buf[..n * FEATURES1_SIZE].fill(0.0);
        self.f2_buf[..n * FEATURES2_SIZE].fill(0.0);

        self.f1_buf[..n * FEATURES1_SIZE]
            .par_chunks_mut(FEATURES1_SIZE)
            .zip(self.f2_buf[..n * FEATURES2_SIZE].par_chunks_mut(FEATURES2_SIZE))
            .zip(positions)
            .for_each(|((f1, f2), pos)| make_input_features(pos, f1, f2));

        Self::run_inputs(
            self.sessions.for_rows(n),
            n,
            &self.f1_buf[..n * FEATURES1_SIZE],
            &self.f2_buf[..n * FEATURES2_SIZE],
            out,
            policy_out,
            #[cfg(feature = "inference-diagnostics")]
            &mut self.timings,
        )
    }

    fn run_inputs(
        session: &mut EvaluatorSession,
        n: usize,
        first: &[f32],
        second: &[f32],
        out: &mut Vec<f32>,
        policy_out: Option<&mut Vec<f32>>,
        #[cfg(feature = "inference-diagnostics")] timings: &mut crate::InferenceTimings,
    ) -> Result<()> {
        Self::forward_inputs(
            session,
            n,
            first,
            second,
            policy_out.is_some(),
            |values, policy| {
                out.extend_from_slice(values);
                if let Some(output) = policy_out {
                    output.extend_from_slice(policy.expect("requested policy"));
                }
            },
            #[cfg(feature = "inference-diagnostics")]
            timings,
        )
    }

    fn forward_inputs<R>(
        session: &mut EvaluatorSession,
        n: usize,
        first: &[f32],
        second: &[f32],
        need_policy: bool,
        consume: impl FnOnce(&[f32], Option<&[f32]>) -> R,
        #[cfg(feature = "inference-diagnostics")] timings: &mut crate::InferenceTimings,
    ) -> Result<R> {
        // ORT resource より先に宣言し、逆順の Drop で解放時間も記録する。
        #[cfg(feature = "inference-diagnostics")]
        let mut timer = ForwardTimer::new(timings, n);
        let shape1: [usize; 4] = [n, INPUT1_CHANNELS, 9, 9];
        let input1 = inference_try!(
            timer,
            TensorRef::<f32>::from_array_view((shape1, first)).map_err(ort_err)
        );
        let shape2: [usize; 4] = [n, INPUT2_CHANNELS, 9, 9];
        let input2 = inference_try!(
            timer,
            TensorRef::<f32>::from_array_view((shape2, second)).map_err(ort_err)
        );

        #[cfg(feature = "inference-diagnostics")]
        timer.advance(InferenceStage::BindingSetup);
        let output_mem = inference_try!(timer, session.output_memory());
        let mut binding = inference_try!(timer, session.inner.create_binding().map_err(ort_err));
        #[cfg(feature = "inference-diagnostics")]
        timer.advance(InferenceStage::InputBind);
        inference_try!(timer, binding.bind_input("input1", &input1).map_err(ort_err));
        inference_try!(timer, binding.bind_input("input2", &input2).map_err(ort_err));
        #[cfg(feature = "inference-diagnostics")]
        timer.advance(InferenceStage::OutputBind);
        // value のみでも全出力をバインドし、未バインド出力の runtime 処理を避ける。
        inference_try!(
            timer,
            binding.bind_output_to_device("output_policy", &output_mem).map_err(ort_err)
        );
        inference_try!(
            timer,
            binding.bind_output_to_device("output_value", &output_mem).map_err(ort_err)
        );
        #[cfg(feature = "inference-diagnostics")]
        timer.advance(InferenceStage::RunBinding);
        let outputs = inference_try!(timer, session.inner.run_binding(&binding).map_err(ort_err));
        #[cfg(feature = "inference-diagnostics")]
        timer.advance(InferenceStage::OutputValidationCopy);
        let (value_shape, values) = inference_try!(
            timer,
            outputs["output_value"].try_extract_tensor::<f32>().map_err(ort_err)
        );
        inference_ensure!(
            timer,
            value_shape.as_ref() == [n as i64, 1],
            "value output shape mismatch: {value_shape:?}"
        );
        inference_ensure!(
            timer,
            values.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)),
            "value must be a finite probability"
        );
        let policy = if need_policy {
            let (shape, logits) = inference_try!(
                timer,
                outputs["output_policy"].try_extract_tensor::<f32>().map_err(ort_err)
            );
            inference_ensure!(
                timer,
                shape.as_ref() == [n as i64, MAX_MOVE_LABEL_NUM as i64],
                "policy output shape mismatch: {shape:?}"
            );
            inference_ensure!(
                timer,
                policy_logits_are_finite(logits),
                "policy contains non-finite logits"
            );
            Some(logits)
        } else {
            None
        };
        let result = consume(values, policy);
        #[cfg(feature = "inference-diagnostics")]
        timer.finish();
        Ok(result)
    }

    fn run_single(
        &mut self,
        logits: &mut [f32],
        operation: impl FnOnce(&mut Self, &mut Vec<f32>, &mut Vec<f32>) -> Result<()>,
    ) -> Result<f32> {
        anyhow::ensure!(logits.len() == MAX_MOVE_LABEL_NUM, "incorrect policy buffer length");
        let mut values = std::mem::take(&mut self.single_values);
        let mut policy = std::mem::take(&mut self.single_policy);
        values.clear();
        policy.clear();
        let result = operation(self, &mut values, &mut policy);
        let value = if result.is_ok() {
            logits.copy_from_slice(&policy);
            values[0]
        } else {
            0.0
        };
        self.single_values = values;
        self.single_policy = policy;
        result.map(|()| value)
    }
}

const INPUT_FORMAT_KEY: &str = "rshogi.input_format";
const POLICY_FORMAT: &str = "direction-destination-2187-v1";
const VALUE_FORMAT: &str = "stm-expected-score-sigmoid-v1";

fn model_format(session: &Session) -> Result<crate::InputFormat> {
    let metadata = session.metadata().map_err(ort_err)?;
    // missing key が空文字として返る runtime でも、空文字の宣言と未宣言を区別する。
    let keys = metadata.custom_keys().map_err(ort_err)?;
    let custom = |key: &str| -> Result<Option<String>> {
        if keys.iter().any(|name| name == key) {
            metadata
                .custom(key)
                .map(Some)
                .ok_or_else(|| anyhow::anyhow!("cannot read metadata {key}"))
        } else {
            Ok(None)
        }
    };
    let format = match custom(INPUT_FORMAT_KEY)?.as_deref() {
        None => crate::InputFormat::Basic,
        Some("stm-tokens-v1") => crate::InputFormat::StmTokensV1,
        Some(other) => anyhow::bail!("unsupported input format: {other}"),
    };
    for (key, expected) in [
        ("rshogi.policy_format", POLICY_FORMAT),
        ("rshogi.value_format", VALUE_FORMAT),
    ] {
        let value = custom(key)?;
        anyhow::ensure!(
            value.as_deref() == Some(expected)
                || (value.is_none() && format == crate::InputFormat::Basic),
            "missing or conflicting metadata {key}"
        );
    }
    Ok(format)
}

fn policy_logits_are_finite(logits: &[f32]) -> bool {
    #[cfg(feature = "policy-validation-reduction")]
    {
        // 短絡を避け、連続した logit の有限性検査をコンパイラがベクトル化できるようにする。
        logits.iter().fold(true, |valid, value| valid & value.is_finite())
    }
    #[cfg(not(feature = "policy-validation-reduction"))]
    {
        logits.iter().all(|value| value.is_finite())
    }
}

fn validate_model(session: &Session, max_batch: usize, format: crate::InputFormat) -> Result<()> {
    use ort::value::{TensorElementType, ValueType};
    let inputs = match format {
        crate::InputFormat::Basic => vec![("input1", vec![62, 9, 9]), ("input2", vec![57, 9, 9])],
        crate::InputFormat::StmTokensV1 => {
            vec![("tokens", vec![crate::tokens::TOKEN_COUNT as i64])]
        }
    };
    for (outlets, expected, input) in [
        (session.inputs(), inputs, true),
        (
            session.outputs(),
            vec![("output_policy", vec![2187]), ("output_value", vec![1])],
            false,
        ),
    ] {
        anyhow::ensure!(outlets.len() == expected.len(), "unexpected model input/output count");
        for (name, tail) in expected {
            let outlet = outlets
                .iter()
                .find(|v| v.name() == name)
                .ok_or_else(|| anyhow::anyhow!("missing tensor {name}"))?;
            let ValueType::Tensor { ty, shape, .. } = outlet.dtype() else {
                anyhow::bail!("{name} must be a tensor");
            };
            let expected_type = if input && format == crate::InputFormat::StmTokensV1 {
                TensorElementType::Int64
            } else {
                TensorElementType::Float32
            };
            anyhow::ensure!(*ty == expected_type, "incorrect {name} dtype");
            anyhow::ensure!(
                shape.len() == tail.len() + 1 && shape[1..] == tail,
                "unsupported {name} shape: {shape:?}"
            );
            anyhow::ensure!(
                shape[0] == -1 || (shape[0] == 1 && max_batch == 1),
                "{name} must support dynamic batches or batch 1"
            );
        }
    }
    Ok(())
}

impl crate::Evaluator for OnnxEvaluator {
    #[cfg(feature = "inference-diagnostics")]
    fn inference_timings(&self) -> Option<crate::InferenceTimings> {
        Some(self.timings)
    }

    fn input_format(&self) -> Option<crate::InputFormat> {
        Some(crate::InputFormat::Basic)
    }
    fn encoded_batch_capacity(&self) -> Option<usize> {
        Some(self.batch_size)
    }

    fn evaluate_encoded_batch(
        &mut self,
        input: EncodedBatch<'_>,
        values: &mut [f32],
        policies: &mut [f32],
    ) -> Result<()> {
        let (first, second) = input
            .basic()
            .ok_or_else(|| anyhow::anyhow!("basic evaluator requires basic encoded batch"))?;
        validate_batch_outputs(input.len(), self.batch_size, values, policies)?;
        if input.is_empty() {
            return Ok(());
        }
        Self::forward_inputs(
            self.sessions.for_rows(input.len()),
            input.len(),
            first,
            second,
            true,
            |result, policy| {
                values.copy_from_slice(result);
                policies.copy_from_slice(policy.expect("requested policy"));
            },
            #[cfg(feature = "inference-diagnostics")]
            &mut self.timings,
        )
    }

    fn evaluate(&mut self, position: &Position, logits: &mut [f32]) -> Result<f32> {
        self.run_single(logits, |evaluator, values, policy| {
            evaluator.evaluate_with_policy(
                std::slice::from_ref(position).par_iter(),
                values,
                policy,
            )
        })
    }

    fn evaluate_encoded(
        &mut self,
        input: EncodedInput<'_>,
        logits: &mut [f32],
    ) -> Result<Option<f32>> {
        let (first, second) = input
            .basic()
            .ok_or_else(|| anyhow::anyhow!("basic evaluator requires basic encoded inputs"))?;
        self.run_single(logits, |evaluator, values, policy| {
            Self::run_inputs(
                evaluator.sessions.for_rows(1),
                1,
                first,
                second,
                values,
                Some(policy),
                #[cfg(feature = "inference-diagnostics")]
                &mut evaluator.timings,
            )
        })
        .map(Some)
    }
}

/// 基本 plane / token 形式の policy/value evaluator。
pub enum ModelEvaluator {
    /// dlshogi 互換の基本入力。
    Basic(OnnxEvaluator),
    /// 手番正規化した駒・持駒 token 入力。
    Tokens(TokenEvaluator),
}

/// 形式自動選択の推論精度と診断出力。
pub struct ModelOptions {
    /// false で CUDA TF32 を無効化する。true は runtime の既定設定を保つ。CPU では使わない。
    pub cuda_tf32: bool,
    /// ORT profile の保存先 prefix。
    pub profiling: Option<PathBuf>,
}

impl Default for ModelOptions {
    fn default() -> Self {
        Self {
            cuda_tf32: true,
            profiling: None,
        }
    }
}

impl ModelEvaluator {
    /// モデルの入力形式を検証して読み込む。TensorRT は legacy API で指定する。
    pub fn new(cfg: &OnnxConfig) -> Result<Self> {
        Self::with_options(cfg, &ModelOptions::default())
    }

    /// 精度設定と任意の ORT profile を指定して、モデルを読み込む。
    pub fn with_options(cfg: &OnnxConfig, options: &ModelOptions) -> Result<Self> {
        anyhow::ensure!(!cfg.use_tensorrt, "automatic model dispatch supports CPU/CUDA only");
        let session = create_session(cfg, options.profiling.as_deref(), options.cuda_tf32)?;
        match model_format(&session.inner)? {
            crate::InputFormat::Basic => Ok(Self::Basic(OnnxEvaluator::from_session(
                session,
                cfg,
                #[cfg(feature = "cuda-basic-root-session")]
                options,
            )?)),
            crate::InputFormat::StmTokensV1 => {
                validate_model(&session.inner, cfg.batch_size, crate::InputFormat::StmTokensV1)?;
                Ok(Self::Tokens(TokenEvaluator {
                    session,
                    batch_size: cfg.batch_size,
                    tokens: vec![0; cfg.batch_size * crate::tokens::TOKEN_COUNT],
                    #[cfg(feature = "inference-diagnostics")]
                    timings: crate::InferenceTimings::default(),
                }))
            }
        }
    }

    /// 局面を最大 batch ごとに分け、手番側の期待スコアを返す。policy のコピーは行わない。
    pub fn evaluate_values(&mut self, positions: &[Position]) -> Result<Vec<f32>> {
        match self {
            Self::Basic(evaluator) => evaluator.evaluate(positions),
            Self::Tokens(evaluator) => {
                let mut values = vec![0.0; positions.len()];
                for (chunk, output) in positions
                    .chunks(evaluator.batch_size)
                    .zip(values.chunks_mut(evaluator.batch_size))
                {
                    evaluator.run_iter(chunk.par_iter(), output, None)?;
                }
                Ok(values)
            }
        }
    }

    /// 最大 batch 以下の局面を推論し、value と局面順の policy logits を追記する。
    pub fn evaluate_with_policy(
        &mut self,
        positions: &[Position],
        values: &mut Vec<f32>,
        policy: &mut Vec<f32>,
    ) -> Result<()> {
        match self {
            Self::Basic(evaluator) => {
                evaluator.evaluate_with_policy(positions.par_iter(), values, policy)
            }
            Self::Tokens(evaluator) => {
                anyhow::ensure!(
                    positions.len() <= evaluator.batch_size,
                    "positions exceed batch_size"
                );
                append_token_outputs(positions.len(), values, policy, |values, policy| {
                    evaluator.run(positions, values, policy)
                })
            }
        }
    }

    /// 順序付き並列 iterator を直接 encode し、value と policy を入力順に追記する。
    /// 入力数は最大 batch 以下とし、局面や参照の中間配列は作らない。
    pub fn evaluate_with_policy_iter<'a>(
        &mut self,
        positions: impl IndexedParallelIterator<Item = &'a Position>,
        values: &mut Vec<f32>,
        policy: &mut Vec<f32>,
    ) -> Result<()> {
        match self {
            Self::Basic(evaluator) => evaluator.evaluate_with_policy(positions, values, policy),
            Self::Tokens(evaluator) => {
                let n = positions.len();
                anyhow::ensure!(n <= evaluator.batch_size, "positions exceed batch_size");
                append_token_outputs(n, values, policy, |values, policy| {
                    evaluator.run_iter(positions, values, Some(policy))
                })
            }
        }
    }

    /// profile を閉じ、ORT が保存したファイル名を返す。
    pub fn end_profiling(&mut self) -> Result<String> {
        let session = match self {
            Self::Basic(e) => &mut e.sessions.main,
            Self::Tokens(e) => &mut e.session,
        };
        session.inner.end_profiling().map_err(ort_err)
    }
}

fn append_token_outputs(
    n: usize,
    values: &mut Vec<f32>,
    policy: &mut Vec<f32>,
    run: impl FnOnce(&mut [f32], &mut [f32]) -> Result<()>,
) -> Result<()> {
    let value_start = values.len();
    let policy_start = policy.len();
    let value_end = value_start
        .checked_add(n)
        .ok_or_else(|| anyhow::anyhow!("value output size overflow"))?;
    let policy_end = n
        .checked_mul(MAX_MOVE_LABEL_NUM)
        .and_then(|len| policy_start.checked_add(len))
        .ok_or_else(|| anyhow::anyhow!("policy output size overflow"))?;
    values.try_reserve(n)?;
    policy.try_reserve(policy_end - policy_start)?;
    values.resize(value_end, 0.0);
    policy.resize(policy_end, 0.0);
    let result = run(&mut values[value_start..], &mut policy[policy_start..]);
    if result.is_err() {
        values.truncate(value_start);
        policy.truncate(policy_start);
    }
    result
}

fn encode_token_inputs<'a>(
    positions: impl IndexedParallelIterator<Item = &'a Position>,
    output: &mut [i64],
) -> Result<()> {
    let len = positions
        .len()
        .checked_mul(crate::tokens::TOKEN_COUNT)
        .ok_or_else(|| anyhow::anyhow!("token input size overflow"))?;
    anyhow::ensure!(len <= output.len(), "incorrect token buffer length");
    positions
        .zip(output[..len].par_chunks_mut(crate::tokens::TOKEN_COUNT))
        .try_for_each(|(position, row)| -> Result<()> {
            for (slot, token) in row.iter_mut().zip(crate::tokens::encode(position)?) {
                *slot = i64::from(token);
            }
            Ok(())
        })
}

fn validate_batch_outputs(
    n: usize,
    capacity: usize,
    values: &[f32],
    policies: &[f32],
) -> Result<()> {
    anyhow::ensure!(n <= capacity, "batch capacity exceeded");
    anyhow::ensure!(
        values.len() == n && n.checked_mul(MAX_MOVE_LABEL_NUM) == Some(policies.len()),
        "incorrect output buffers"
    );
    Ok(())
}

/// `stm-tokens-v1` の再利用可能な入力 buffer と ORT session。
pub struct TokenEvaluator {
    session: EvaluatorSession,
    batch_size: usize,
    tokens: Vec<i64>,
    #[cfg(feature = "inference-diagnostics")]
    timings: crate::InferenceTimings,
}

impl TokenEvaluator {
    fn run(
        &mut self,
        positions: &[Position],
        values_out: &mut [f32],
        policy_out: &mut [f32],
    ) -> Result<()> {
        let n = positions.len();
        anyhow::ensure!(n <= self.batch_size, "positions exceed batch_size");
        anyhow::ensure!(
            values_out.len() == n && policy_out.len() == n * MAX_MOVE_LABEL_NUM,
            "incorrect output buffers"
        );
        if n == 0 {
            return Ok(());
        }
        for (position, output) in positions
            .iter()
            .zip(self.tokens.as_chunks_mut::<{ crate::tokens::TOKEN_COUNT }>().0)
        {
            for (slot, token) in output.iter_mut().zip(crate::tokens::encode(position)?) {
                *slot = i64::from(token);
            }
        }
        Self::run_inputs(
            &mut self.session,
            n,
            &self.tokens[..n * crate::tokens::TOKEN_COUNT],
            values_out,
            policy_out,
            #[cfg(feature = "inference-diagnostics")]
            &mut self.timings,
        )
    }

    fn run_iter<'a>(
        &mut self,
        positions: impl IndexedParallelIterator<Item = &'a Position>,
        values_out: &mut [f32],
        policy_out: Option<&mut [f32]>,
    ) -> Result<()> {
        let n = positions.len();
        anyhow::ensure!(n <= self.batch_size, "positions exceed batch_size");
        anyhow::ensure!(
            values_out.len() == n
                && policy_out.as_ref().is_none_or(|policy| policy.len() == n * MAX_MOVE_LABEL_NUM),
            "incorrect output buffers"
        );
        if n == 0 {
            return Ok(());
        }
        encode_token_inputs(positions, &mut self.tokens)?;
        Self::run_inputs_optional_policy(
            &mut self.session,
            n,
            &self.tokens[..n * crate::tokens::TOKEN_COUNT],
            values_out,
            policy_out,
            #[cfg(feature = "inference-diagnostics")]
            &mut self.timings,
        )
    }

    fn run_encoded_batch(
        &mut self,
        input: EncodedBatch<'_>,
        values: &mut [f32],
        policies: &mut [f32],
    ) -> Result<()> {
        let tokens = input
            .tokens()
            .ok_or_else(|| anyhow::anyhow!("token evaluator requires token encoded batch"))?;
        validate_batch_outputs(input.len(), self.batch_size, values, policies)?;
        if input.is_empty() {
            return Ok(());
        }
        for (slot, &token) in self.tokens.iter_mut().zip(tokens) {
            *slot = i64::from(token);
        }
        Self::run_inputs(
            &mut self.session,
            input.len(),
            &self.tokens[..tokens.len()],
            values,
            policies,
            #[cfg(feature = "inference-diagnostics")]
            &mut self.timings,
        )
    }

    fn run_encoded(&mut self, input: EncodedInput<'_>, logits: &mut [f32]) -> Result<f32> {
        anyhow::ensure!(logits.len() == MAX_MOVE_LABEL_NUM, "incorrect policy buffer length");
        let tokens = input
            .tokens()
            .ok_or_else(|| anyhow::anyhow!("token evaluator requires token encoded inputs"))?;
        for (slot, &token) in self.tokens.iter_mut().zip(tokens) {
            *slot = i64::from(token);
        }
        let mut values = [0.0];
        Self::run_inputs(
            &mut self.session,
            1,
            &self.tokens[..crate::tokens::TOKEN_COUNT],
            &mut values,
            logits,
            #[cfg(feature = "inference-diagnostics")]
            &mut self.timings,
        )?;
        Ok(values[0])
    }

    fn run_inputs(
        session: &mut EvaluatorSession,
        n: usize,
        tokens: &[i64],
        values_out: &mut [f32],
        policy_out: &mut [f32],
        #[cfg(feature = "inference-diagnostics")] timings: &mut crate::InferenceTimings,
    ) -> Result<()> {
        Self::run_inputs_optional_policy(
            session,
            n,
            tokens,
            values_out,
            Some(policy_out),
            #[cfg(feature = "inference-diagnostics")]
            timings,
        )
    }

    fn run_inputs_optional_policy(
        session: &mut EvaluatorSession,
        n: usize,
        tokens: &[i64],
        values_out: &mut [f32],
        policy_out: Option<&mut [f32]>,
        #[cfg(feature = "inference-diagnostics")] timings: &mut crate::InferenceTimings,
    ) -> Result<()> {
        // ORT resource より先に宣言し、逆順の Drop で解放時間も記録する。
        #[cfg(feature = "inference-diagnostics")]
        let mut timer = ForwardTimer::new(timings, n);
        let tokens = inference_try!(
            timer,
            TensorRef::<i64>::from_array_view(([n, crate::tokens::TOKEN_COUNT], tokens))
                .map_err(ort_err)
        );
        #[cfg(feature = "inference-diagnostics")]
        timer.advance(InferenceStage::BindingSetup);
        let memory = inference_try!(timer, session.output_memory());
        let mut binding = inference_try!(timer, session.inner.create_binding().map_err(ort_err));
        #[cfg(feature = "inference-diagnostics")]
        timer.advance(InferenceStage::InputBind);
        inference_try!(timer, binding.bind_input("tokens", &tokens).map_err(ort_err));
        #[cfg(feature = "inference-diagnostics")]
        timer.advance(InferenceStage::OutputBind);
        inference_try!(
            timer,
            binding.bind_output_to_device("output_policy", &memory).map_err(ort_err)
        );
        inference_try!(
            timer,
            binding.bind_output_to_device("output_value", &memory).map_err(ort_err)
        );
        #[cfg(feature = "inference-diagnostics")]
        timer.advance(InferenceStage::RunBinding);
        let outputs = inference_try!(timer, session.inner.run_binding(&binding).map_err(ort_err));
        #[cfg(feature = "inference-diagnostics")]
        timer.advance(InferenceStage::OutputValidationCopy);
        let policy = if policy_out.is_some() {
            let (shape, policy) = inference_try!(
                timer,
                outputs["output_policy"].try_extract_tensor::<f32>().map_err(ort_err)
            );
            inference_ensure!(
                timer,
                shape.as_ref() == [n as i64, MAX_MOVE_LABEL_NUM as i64],
                "incorrect policy output shape"
            );
            inference_ensure!(timer, policy_logits_are_finite(policy), "non-finite policy logits");
            Some(policy)
        } else {
            None
        };
        let (shape, values) = inference_try!(
            timer,
            outputs["output_value"].try_extract_tensor::<f32>().map_err(ort_err)
        );
        inference_ensure!(timer, shape.as_ref() == [n as i64, 1], "incorrect value output shape");
        inference_ensure!(
            timer,
            values.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)),
            "invalid expected score"
        );
        inference_ensure!(
            timer,
            policy_out.is_some() == policy.is_some(),
            "policy output request mismatch"
        );
        if let (Some(output), Some(policy)) = (policy_out, policy) {
            output.copy_from_slice(policy);
        }
        values_out.copy_from_slice(values);
        #[cfg(feature = "inference-diagnostics")]
        timer.finish();
        Ok(())
    }
}

impl crate::Evaluator for ModelEvaluator {
    #[cfg(feature = "inference-diagnostics")]
    fn inference_timings(&self) -> Option<crate::InferenceTimings> {
        Some(match self {
            Self::Basic(evaluator) => evaluator.timings,
            Self::Tokens(evaluator) => evaluator.timings,
        })
    }

    fn input_format(&self) -> Option<crate::InputFormat> {
        Some(match self {
            Self::Basic(_) => crate::InputFormat::Basic,
            Self::Tokens(_) => crate::InputFormat::StmTokensV1,
        })
    }

    fn encoded_batch_capacity(&self) -> Option<usize> {
        Some(match self {
            Self::Basic(e) => e.batch_size,
            Self::Tokens(e) => e.batch_size,
        })
    }

    fn evaluate_encoded_batch(
        &mut self,
        input: EncodedBatch<'_>,
        values: &mut [f32],
        policies: &mut [f32],
    ) -> Result<()> {
        match self {
            Self::Basic(e) => crate::Evaluator::evaluate_encoded_batch(e, input, values, policies),
            Self::Tokens(e) => e.run_encoded_batch(input, values, policies),
        }
    }

    fn evaluate(&mut self, position: &Position, logits: &mut [f32]) -> Result<f32> {
        match self {
            Self::Basic(evaluator) => crate::Evaluator::evaluate(evaluator, position, logits),
            Self::Tokens(evaluator) => {
                let mut values = [0.0];
                evaluator.run(std::slice::from_ref(position), &mut values, logits)?;
                Ok(values[0])
            }
        }
    }

    fn evaluate_encoded(
        &mut self,
        input: EncodedInput<'_>,
        logits: &mut [f32],
    ) -> Result<Option<f32>> {
        match self {
            Self::Basic(evaluator) => crate::Evaluator::evaluate_encoded(evaluator, input, logits),
            Self::Tokens(evaluator) => evaluator.run_encoded(input, logits).map(Some),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_finite_scan_accepts_ieee_finite_values_at_vector_boundaries() {
        assert!(policy_logits_are_finite(&[]));
        let finite = [
            0.0,
            -0.0,
            f32::MAX,
            f32::MIN,
            f32::MIN_POSITIVE,
            f32::from_bits(1),
            f32::from_bits(0x8000_0001),
            1.0,
            -1.0,
        ];
        for length in [
            1,
            7,
            8,
            9,
            31,
            32,
            33,
            MAX_MOVE_LABEL_NUM,
            MAX_MOVE_LABEL_NUM * 128,
        ] {
            for shift in 0..finite.len() {
                let values: Vec<_> =
                    (0..length).map(|index| finite[(index + shift) % finite.len()]).collect();
                assert!(policy_logits_are_finite(&values));
            }
        }
    }

    #[test]
    fn policy_finite_scan_rejects_infinities_and_nan_payloads_in_every_lane() {
        for invalid in [
            0x7f80_0000,
            0xff80_0000,
            0x7f80_0001,
            0xff80_0001,
            0x7fc0_0000,
            0xffc0_0000,
            0x7fff_ffff,
            0xffff_ffff,
        ] {
            for length in [
                1,
                7,
                8,
                9,
                31,
                32,
                33,
                MAX_MOVE_LABEL_NUM,
                MAX_MOVE_LABEL_NUM * 128,
            ] {
                let mut values = vec![1.0; length];
                for index in (0..length.min(64)).chain([length / 2, length - 1]) {
                    values[index] = f32::from_bits(invalid);
                    assert!(!policy_logits_are_finite(&values));
                    values[index] = 1.0;
                }
            }
        }
    }

    #[test]
    fn policy_finite_scan_matches_exponent_bits_on_long_random_inputs() {
        let mut state = 0x5a17_c9e3u32;
        for _ in 0..4 {
            let mut values: Vec<_> = (0..MAX_MOVE_LABEL_NUM * 128)
                .map(|_| {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    f32::from_bits(state)
                })
                .collect();
            let expected = values
                .iter()
                .fold(true, |valid, value| valid & (value.to_bits() & 0x7f80_0000 != 0x7f80_0000));
            assert_eq!(policy_logits_are_finite(&values), expected);
            for value in &mut values {
                *value = f32::from_bits(value.to_bits() & 0xfeff_ffff);
            }
            assert!(policy_logits_are_finite(&values));
        }
    }

    #[test]
    fn parallel_token_inputs_preserve_noncontiguous_order_and_unused_capacity() -> Result<()> {
        let positions: Vec<_> = [
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
            "4k4/9/9/9/9/9/9/9/4K4 w 2r2b4g4s4n4l18p 1",
            "4k4/9/9/9/9/9/9/9/4K4 b 8P 37",
        ]
        .iter()
        .map(|sfen| {
            let mut pos = Position::new();
            pos.set_sfen(sfen)?;
            Ok(pos)
        })
        .collect::<Result<_>>()?;
        let order = [2, 0, 1, 2];
        let mut packed = vec![-1; 5 * crate::tokens::TOKEN_COUNT];
        let capacity = packed.capacity();
        encode_token_inputs(order.par_iter().map(|&index| &positions[index]), &mut packed)?;
        for (row, &index) in order.iter().enumerate() {
            let expected = crate::tokens::encode(&positions[index])?.map(i64::from);
            assert_eq!(
                &packed[row * crate::tokens::TOKEN_COUNT..(row + 1) * crate::tokens::TOKEN_COUNT],
                &expected
            );
        }
        assert!(packed[4 * crate::tokens::TOKEN_COUNT..].iter().all(|&v| v == -1));
        let untouched = packed[crate::tokens::TOKEN_COUNT..].to_vec();
        encode_token_inputs(positions[..1].par_iter(), &mut packed)?;
        assert_eq!(&packed[crate::tokens::TOKEN_COUNT..], untouched);
        encode_token_inputs(positions[..0].par_iter(), &mut packed)?;
        assert_eq!(packed.capacity(), capacity);
        let mut short = [-1; crate::tokens::TOKEN_COUNT];
        assert!(encode_token_inputs(positions.par_iter(), &mut short).is_err());
        assert!(short.iter().all(|&v| v == -1));
        Ok(())
    }

    #[test]
    fn token_policy_append_preserves_prefix_order_capacity_and_rolls_back_failures() -> Result<()> {
        let mut values = Vec::with_capacity(8);
        let mut policy = Vec::with_capacity(8 * MAX_MOVE_LABEL_NUM);
        values.push(17.0);
        policy.push(19.0);
        let capacities = (values.capacity(), policy.capacity());
        append_token_outputs(2, &mut values, &mut policy, |values, policy| {
            values.copy_from_slice(&[0.25, 0.75]);
            policy[..MAX_MOVE_LABEL_NUM].fill(2.0);
            policy[MAX_MOVE_LABEL_NUM..].fill(3.0);
            Ok(())
        })?;
        assert_eq!(values, [17.0, 0.25, 0.75]);
        assert_eq!(policy[0], 19.0);
        assert!(policy[1..1 + MAX_MOVE_LABEL_NUM].iter().all(|&value| value == 2.0));
        assert!(policy[1 + MAX_MOVE_LABEL_NUM..].iter().all(|&value| value == 3.0));
        let previous_values = values.clone();
        let previous_policy = policy.clone();
        assert!(
            append_token_outputs(2, &mut values, &mut policy, |values, policy| {
                values.fill(5.0);
                policy.fill(7.0);
                anyhow::bail!("inference failed")
            })
            .is_err()
        );
        assert_eq!(values, previous_values);
        assert_eq!(policy, previous_policy);
        assert_eq!((values.capacity(), policy.capacity()), capacities);
        append_token_outputs(0, &mut values, &mut policy, |values, policy| {
            assert!(values.is_empty() && policy.is_empty());
            Ok(())
        })?;
        assert!(
            append_token_outputs(usize::MAX, &mut values, &mut policy, |_, _| unreachable!())
                .is_err()
        );
        assert_eq!(values, previous_values);
        assert_eq!(policy, previous_policy);
        Ok(())
    }

    #[cfg(feature = "inference-diagnostics")]
    fn recorded_result(
        totals: &mut crate::InferenceTimings,
        ensure_valid: bool,
        operation_valid: bool,
    ) -> anyhow::Result<()> {
        let mut timer = crate::diagnostics::ForwardTimer::new(totals, 2);
        timer.advance(crate::InferenceStage::BindingSetup);
        inference_ensure!(timer, ensure_valid, "expected validation error");
        let operation: anyhow::Result<()> = if operation_valid {
            Ok(())
        } else {
            Err(anyhow::anyhow!("expected operation error"))
        };
        inference_try!(timer, operation);
        timer.finish();
        Ok(())
    }

    #[cfg(feature = "inference-diagnostics")]
    #[test]
    fn result_errors_and_ensure_errors_finish_their_phase_and_cleanup() {
        for (ensure_valid, operation_valid, failures) in
            [(false, true, 1), (true, false, 1), (true, true, 0)]
        {
            let mut totals = crate::InferenceTimings::default();
            assert_eq!(
                recorded_result(&mut totals, ensure_valid, operation_valid).is_err(),
                failures == 1
            );
            assert_eq!(totals.forward_calls, 1);
            assert_eq!(totals.forward_failures, failures);
            assert_eq!(totals.positions, 2);
            assert_eq!(
                totals.stages[crate::InferenceStage::BindingSetup as usize].errors,
                failures
            );
            assert_eq!(totals.stages[crate::InferenceStage::Cleanup as usize].calls, 1);
        }
    }

    #[test]
    fn model_options_preserve_runtime_precision_by_default() {
        let options = ModelOptions::default();
        assert!(options.cuda_tf32);
        assert!(options.profiling.is_none());
    }
}
