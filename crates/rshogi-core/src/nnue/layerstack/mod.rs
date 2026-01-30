//! LayerStack NNUE 推論実装
//!
//! LayerStack アーキテクチャの推論部実装。
//! この実装が integer forward の「正」となり、学習側（bullet）の bit-exact 検証基準になる。
//!
//! # アーキテクチャ概要
//!
//! ```text
//! HalfKA → FT [1536] × 2視点
//!          ↓
//!     Perspective結合 [3072]
//!          ↓
//!     ClippedReLU [3072]
//!          ↓
//!     Product Pooling [1536]  ← Golden Forward 入力点
//!          ↓
//!     LayerStacks[bucket]
//!       L1 [1536→16] → split([15],[1])
//!       Dual Act [15→30]
//!       L2 [30→64]
//!       Output [64→1] + bypass
//!          ↓
//!       評価値
//! ```
//!
mod bucket;
mod constants;
mod forward;
mod io;
mod network;
mod weights;

pub use bucket::{bucket_index, BucketDivision};
pub use constants::*;
pub use forward::{
    dual_activation, internal_to_cp, layer_stack_forward, layer_stack_l1, layer_stack_l2,
    layer_stack_output, product_pooling,
};
pub use io::{read_lsnn, LsnnHeader, LSNN_MAGIC};
pub use network::{LayerStackNetwork, LayerStackStack};
pub use weights::LayerStackWeights;

#[cfg(test)]
mod tests;
