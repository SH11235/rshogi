//! NNUE学習モジュール
//!
//! HalfKP 256x2-32-32 アーキテクチャのNNUE学習を実装する。
//!
//! # 構成
//! - `network`: 学習可能なネットワーク構造（f32重み）
//! - `dataset`: 教師データの読み込み
//! - `optimizer`: Adam/SGDオプティマイザ
//! - `trainer`: 学習ループ

pub mod dataset;
pub mod network;
pub mod optimizer;
pub mod trainer;

pub use dataset::{TrainingBatch, TrainingDataset, TrainingSample};
pub use network::TrainableNetwork;
pub use optimizer::{Adam, Optimizer};
pub use trainer::{LearningRateScheduler, LossType, NewbobState, TrainConfig, Trainer};
