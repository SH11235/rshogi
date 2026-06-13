//! 標準 dlshogi ONNX モデルによる value head 静的評価
//!
//! `make_input_features` で構築した dlshogi 入力を ONNX Runtime (CUDA / TensorRT EP)
//! へ流し、value head の勝率を手番 (STM) 視点 cp へ変換して返す。
//!
//! `rescore_psv` の ONNX パイプラインから value 推論部分のみを切り出した再利用モジュール
//! （`rescore_psv` 側は独自実装のままで、推論ロジックが重複している）。

use std::path::{Path, PathBuf};

use anyhow::Result;
use rayon::prelude::*;
use rshogi_core::position::Position;

use ort::memory::{AllocationDevice, AllocatorType, MemoryInfo, MemoryType};
use ort::session::Session;
use ort::value::TensorRef;

use crate::dlshogi_features::{
    FEATURES1_SIZE, FEATURES2_SIZE, INPUT1_CHANNELS, INPUT2_CHANNELS, make_input_features,
    winrate_to_cp,
};

/// ONNX value evaluator の設定
pub struct OnnxValueConfig {
    /// dlshogi ONNX モデルのパス
    pub model_path: PathBuf,
    /// CUDA device id。負値で CPU 推論。
    pub gpu_id: i32,
    /// TensorRT EP (FP16) を使う。false なら CUDA EP (FP32)。
    pub use_tensorrt: bool,
    /// TensorRT エンジンキャッシュの保存先（`use_tensorrt` 時のみ有効）
    pub tensorrt_cache: Option<PathBuf>,
    /// winrate→cp 変換のスケール（dlshogi の Eval_Coef 逆変換）
    pub eval_scale: f32,
    /// 1 回の推論あたりの最大局面数
    pub batch_size: usize,
}

/// dlshogi value head を ONNX Runtime で評価する
pub struct OnnxValueEvaluator {
    session: Session,
    eval_scale: f32,
    batch_size: usize,
    /// CPU 出力先メモリ情報。バッチサイズ非依存なので 1 度だけ確保して再利用する。
    output_mem: MemoryInfo,
    f1_buf: Vec<f32>,
    f2_buf: Vec<f32>,
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

impl OnnxValueEvaluator {
    /// 設定から evaluator を初期化する。
    ///
    /// `gpu_id >= 0` のとき CUDA / TensorRT EP の利用可否を明示的にチェックし、
    /// 暗黙の CPU フォールバックを防ぐ。
    pub fn new(cfg: &OnnxValueConfig) -> Result<Self> {
        use ort::ep::ExecutionProvider;

        if cfg.batch_size == 0 {
            anyhow::bail!("OnnxValueConfig.batch_size must be > 0");
        }

        ensure_ort_dylib(cfg.gpu_id)?;

        eprintln!("Loading dlshogi ONNX model: {}", cfg.model_path.display());

        let mut builder = Session::builder()
            .map_err(ort_err)?
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

        eprintln!("dlshogi ONNX model loaded. Batch size: {}", cfg.batch_size);

        let output_mem =
            MemoryInfo::new(AllocationDevice::CPU, 0, AllocatorType::Device, MemoryType::CPUOutput)
                .map_err(ort_err)?;

        Ok(Self {
            session,
            eval_scale: cfg.eval_scale,
            batch_size: cfg.batch_size,
            output_mem,
            f1_buf: vec![0.0f32; cfg.batch_size * FEATURES1_SIZE],
            f2_buf: vec![0.0f32; cfg.batch_size * FEATURES2_SIZE],
        })
    }

    /// `positions` を `batch_size` 以下のチャンクで推論し、各局面の評価値を返す。
    ///
    /// 戻り値は **手番 (STM) 視点 cp**（勝率を `winrate_to_cp` で変換したもの）。
    /// 先手視点へ揃えるのは呼び出し側の責務とする。
    pub fn evaluate(&mut self, positions: &[Position]) -> Result<Vec<i32>> {
        let mut out = Vec::with_capacity(positions.len());
        for chunk in positions.chunks(self.batch_size) {
            self.evaluate_chunk(chunk, &mut out)?;
        }
        Ok(out)
    }

    fn evaluate_chunk(&mut self, chunk: &[Position], out: &mut Vec<i32>) -> Result<()> {
        let n = chunk.len();
        debug_assert!(n <= self.batch_size);

        self.f1_buf[..n * FEATURES1_SIZE].fill(0.0);
        self.f2_buf[..n * FEATURES2_SIZE].fill(0.0);

        let f1_slices: Vec<&mut [f32]> =
            self.f1_buf[..n * FEATURES1_SIZE].chunks_mut(FEATURES1_SIZE).collect();
        let f2_slices: Vec<&mut [f32]> =
            self.f2_buf[..n * FEATURES2_SIZE].chunks_mut(FEATURES2_SIZE).collect();

        f1_slices.into_par_iter().zip(f2_slices).zip(chunk.par_iter()).for_each(
            |((f1, f2), pos)| {
                make_input_features(pos, f1, f2);
            },
        );

        let shape1: [usize; 4] = [n, INPUT1_CHANNELS, 9, 9];
        let input1 =
            TensorRef::<f32>::from_array_view((shape1, &self.f1_buf[..n * FEATURES1_SIZE]))
                .map_err(ort_err)?;
        let shape2: [usize; 4] = [n, INPUT2_CHANNELS, 9, 9];
        let input2 =
            TensorRef::<f32>::from_array_view((shape2, &self.f2_buf[..n * FEATURES2_SIZE]))
                .map_err(ort_err)?;

        let mut binding = self.session.create_binding().map_err(ort_err)?;
        binding.bind_input("input1", &input1).map_err(ort_err)?;
        binding.bind_input("input2", &input2).map_err(ort_err)?;
        // value 推論には output_policy を使わないが、未バインド出力は ORT 内部処理の
        // オーバーヘッドを増やすため全出力をバインドする。
        binding
            .bind_output_to_device("output_policy", &self.output_mem)
            .map_err(ort_err)?;
        binding
            .bind_output_to_device("output_value", &self.output_mem)
            .map_err(ort_err)?;

        let outputs = self.session.run_binding(&binding).map_err(ort_err)?;
        let (_, values) = outputs["output_value"].try_extract_tensor::<f32>().map_err(ort_err)?;

        for &winrate in values.iter().take(n) {
            out.push(winrate_to_cp(winrate, self.eval_scale));
        }
        Ok(())
    }
}
