//! 基本 plane / token 形式の ONNX 推論を手番側 cp として公開する。

use crate::dlshogi_features::winrate_to_cp;
use anyhow::{Context, Result};
use rayon::prelude::*;
use rshogi_core::position::Position;
use rshogi_dl::onnx::{ModelEvaluator, OnnxConfig, OnnxEvaluator};
use std::path::PathBuf;

/// ONNX value evaluator の設定
pub struct OnnxValueConfig {
    /// 基本 plane または stm-tokens-v1 ONNX モデルのパス
    pub model_path: PathBuf,
    /// CUDA device id。負値で CPU 推論。
    pub gpu_id: i32,
    /// 基本 plane 形式に TensorRT EP (FP16) を使う。token 形式は CPU/CUDA のみ。
    pub use_tensorrt: bool,
    /// TensorRT エンジンキャッシュの保存先（`use_tensorrt` 時のみ有効）
    pub tensorrt_cache: Option<PathBuf>,
    /// winrate→cp 変換のスケール（dlshogi の Eval_Coef 逆変換）
    pub eval_scale: f32,
    /// 1 回の推論あたりの最大局面数
    pub batch_size: usize,
}

/// policy/value 推論に既存ツール向けの cp 変換を適用する。
pub struct OnnxValueEvaluator {
    inner: ModelEvaluator,
    eval_scale: f32,
    raw_values: Vec<f32>,
}

impl OnnxValueEvaluator {
    /// モデルと推論 backend を初期化する。
    pub fn new(cfg: &OnnxValueConfig) -> Result<Self> {
        let config = OnnxConfig {
            model_path: cfg.model_path.clone(),
            gpu_id: cfg.gpu_id,
            use_tensorrt: cfg.use_tensorrt,
            tensorrt_cache: cfg.tensorrt_cache.clone(),
            batch_size: cfg.batch_size,
        };
        let inner = if cfg.use_tensorrt {
            ModelEvaluator::Basic(OnnxEvaluator::new(&config).context(
                "TensorRT supports basic plane models only; token models require CPU/CUDA without --onnx-tensorrt",
            )?)
        } else {
            ModelEvaluator::new(&config)?
        };
        Ok(Self {
            inner,
            eval_scale: cfg.eval_scale,
            raw_values: Vec::with_capacity(cfg.batch_size),
        })
    }

    /// 局面ごとに手番側の cp を返す。
    pub fn evaluate(&mut self, positions: &[Position]) -> Result<Vec<i32>> {
        Ok(self
            .inner
            .evaluate_values(positions)?
            .into_iter()
            .map(|v| winrate_to_cp(v, self.eval_scale))
            .collect())
    }

    /// 手番側 cp と、局面順の policy logits を追記する。
    pub fn evaluate_with_policy<'a>(
        &mut self,
        positions: impl IndexedParallelIterator<Item = &'a Position>,
        values: &mut Vec<i32>,
        policy: &mut Vec<f32>,
    ) -> Result<()> {
        self.raw_values.clear();
        self.inner.evaluate_with_policy_iter(positions, &mut self.raw_values, policy)?;
        values.extend(self.raw_values.iter().map(|&v| winrate_to_cp(v, self.eval_scale)));
        Ok(())
    }
}
