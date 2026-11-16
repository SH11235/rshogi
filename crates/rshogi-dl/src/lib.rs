//! 将棋の policy/value 推論とモデル入力。

use anyhow::Result;
use rshogi_core::position::Position;

pub mod cache;
#[cfg(feature = "onnx")]
pub mod environment;
pub mod features;
#[cfg(feature = "onnx")]
pub mod onnx;

/// 合法手の生成や終局判定を含まない、局面単独の推論。
pub trait Evaluator {
    /// 手番側の勝率と基本形式の policy logits を返す。
    fn evaluate(&mut self, position: &Position, logits: &mut [f32]) -> Result<f32>;
}
