//! 将棋の policy/value 推論とモデル入力。

use anyhow::Result;
use rshogi_core::position::Position;

pub mod cache;
#[cfg(feature = "onnx")]
pub mod environment;
pub mod features;
pub mod input;
pub use input::{EncodedInput, InputBuffer};
#[cfg(feature = "onnx")]
pub mod onnx;
pub mod tokens;

/// モデルへ渡す完全入力の形式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputFormat {
    /// dlshogi 互換の盤面・利き・持駒 plane。
    Basic,
    /// 手番正規化した盤面・正確な持駒枚数・元の先後からなる 96 token。
    StmTokensV1,
}

/// 合法手の生成や終局判定を含まない、局面単独の推論。
pub trait Evaluator {
    /// 宣言した入力だけに依存する決定的な推論に限り、キャッシュを有効にできる。
    fn input_format(&self) -> Option<InputFormat> {
        None
    }

    /// encode 済み入力を推論する。未対応なら出力を変更せず None を返す。
    /// 対応する実装は、形式不一致や推論失敗をエラーにし、Position による再試行を要求しない。
    fn evaluate_encoded(&mut self, _: EncodedInput<'_>, _: &mut [f32]) -> Result<Option<f32>> {
        Ok(None)
    }

    /// 手番側の期待スコア（勝ち 1・引き分け 0.5・負け 0）と共通の 2187 policy logits を返す。
    fn evaluate(&mut self, position: &Position, logits: &mut [f32]) -> Result<f32>;
}
