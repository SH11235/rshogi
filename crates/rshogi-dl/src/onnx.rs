//! dlshogi 互換 ONNX モデルの policy logits と手番側勝率。

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
    session: Session,
    batch_size: usize,
    f1_buf: Vec<f32>,
    f2_buf: Vec<f32>,
    single_values: Vec<f32>,
    single_policy: Vec<f32>,
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

impl OnnxEvaluator {
    /// 設定から evaluator を初期化する。
    ///
    /// `gpu_id >= 0` のとき CUDA / TensorRT EP の利用可否を明示的にチェックし、
    /// 暗黙の CPU フォールバックを防ぐ。
    pub fn new(cfg: &OnnxConfig) -> Result<Self> {
        use ort::ep::ExecutionProvider;

        if cfg.batch_size == 0 {
            anyhow::bail!("OnnxConfig.batch_size must be > 0");
        }

        ensure_ort_dylib(cfg.gpu_id)?;

        eprintln!("Loading dlshogi ONNX model: {}", cfg.model_path.display());

        let mut builder = crate::environment::session_builder()?
            .with_optimization_level(ort::session::builder::GraphOptimizationLevel::All)
            .map_err(|e| anyhow::anyhow!("ORT builder error: {e}"))?
            .with_intra_threads(1)
            .map_err(|e| anyhow::anyhow!("ORT builder error: {e}"))?;

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

                let cuda_ep = ort::execution_providers::CUDAExecutionProvider::default()
                    .with_device_id(cfg.gpu_id);
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

        validate_model(&session, cfg.batch_size)?;
        eprintln!("dlshogi ONNX model loaded. Batch size: {}", cfg.batch_size);

        Ok(Self {
            session,
            batch_size: cfg.batch_size,
            f1_buf: vec![0.0f32; cfg.batch_size * FEATURES1_SIZE],
            f2_buf: vec![0.0f32; cfg.batch_size * FEATURES2_SIZE],
            single_values: Vec::with_capacity(1),
            single_policy: Vec::with_capacity(MAX_MOVE_LABEL_NUM),
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

        let shape1: [usize; 4] = [n, INPUT1_CHANNELS, 9, 9];
        let input1 =
            TensorRef::<f32>::from_array_view((shape1, &self.f1_buf[..n * FEATURES1_SIZE]))
                .map_err(ort_err)?;
        let shape2: [usize; 4] = [n, INPUT2_CHANNELS, 9, 9];
        let input2 =
            TensorRef::<f32>::from_array_view((shape2, &self.f2_buf[..n * FEATURES2_SIZE]))
                .map_err(ort_err)?;

        let output_mem =
            MemoryInfo::new(AllocationDevice::CPU, 0, AllocatorType::Device, MemoryType::CPUOutput)
                .map_err(ort_err)?;
        let mut binding = self.session.create_binding().map_err(ort_err)?;
        binding.bind_input("input1", &input1).map_err(ort_err)?;
        binding.bind_input("input2", &input2).map_err(ort_err)?;
        // value のみの推論でも、未バインド出力は ORT 内部処理のオーバーヘッドを増やすため
        // 全出力をバインドする。
        binding.bind_output_to_device("output_policy", &output_mem).map_err(ort_err)?;
        binding.bind_output_to_device("output_value", &output_mem).map_err(ort_err)?;

        let outputs = self.session.run_binding(&binding).map_err(ort_err)?;
        let (value_shape, values) =
            outputs["output_value"].try_extract_tensor::<f32>().map_err(ort_err)?;
        anyhow::ensure!(
            value_shape.as_ref() == [n as i64, 1],
            "value output shape mismatch: {value_shape:?}"
        );
        anyhow::ensure!(
            values.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)),
            "value must be a finite probability"
        );

        for &winrate in values.iter().take(n) {
            out.push(winrate);
        }

        if let Some(policy_out) = policy_out {
            let (shape, logits) =
                outputs["output_policy"].try_extract_tensor::<f32>().map_err(ort_err)?;
            if shape.as_ref() != [n as i64, MAX_MOVE_LABEL_NUM as i64] {
                anyhow::bail!(
                    "policy output shape mismatch: expected [{n}, {MAX_MOVE_LABEL_NUM}], got {shape:?}"
                );
            }
            anyhow::ensure!(
                logits.iter().all(|v| v.is_finite()),
                "policy contains non-finite logits"
            );
            policy_out.extend_from_slice(logits);
        }
        Ok(())
    }
}

fn validate_model(session: &Session, max_batch: usize) -> Result<()> {
    use ort::value::{TensorElementType, ValueType};
    for (outlets, expected) in [
        (session.inputs(), vec![("input1", vec![62, 9, 9]), ("input2", vec![57, 9, 9])]),
        (
            session.outputs(),
            vec![("output_policy", vec![2187]), ("output_value", vec![1])],
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
            anyhow::ensure!(*ty == TensorElementType::Float32, "{name} must be float32");
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
    fn evaluate(&mut self, position: &Position, logits: &mut [f32]) -> Result<f32> {
        anyhow::ensure!(logits.len() == MAX_MOVE_LABEL_NUM, "incorrect policy buffer length");
        let mut values = std::mem::take(&mut self.single_values);
        let mut policy = std::mem::take(&mut self.single_policy);
        values.clear();
        policy.clear();
        let result = self.evaluate_with_policy(
            std::slice::from_ref(position).par_iter(),
            &mut values,
            &mut policy,
        );
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
