//! NNUE (Efficiently Updatable Neural Network) evaluation function
//!
//! Implements NNUE with HalfKP features. Classic weights (v1) use 256x2-32-32-1.
//! Version 2 (v2) supports dynamic dimensions specified in the weight file header.
//!
//! # Architecture Overview
//!
//! The NNUE evaluator consists of two main components:
//!
//! ## 1. Feature Extraction (HalfKP)
//! - **Half**: Features are relative to each side's king position
//! - **K**: King position (81 possible squares)
//! - **P**: All other pieces on the board and in hand
//! - Total features: 81 king squares × 2,182 piece configurations = 176,742 features
//!
//! ## 2. Neural Network (256x2-32-32-1)
//! - **Input Layer**: 512 neurons (256 × 2 perspectives)
//!   - Each side has 256 accumulated feature values
//!   - Features are transformed from sparse HalfKP to dense representation
//! - **Hidden Layer 1**: 32 neurons with ClippedReLU activation
//! - **Hidden Layer 2**: 32 neurons with ClippedReLU activation  
//! - **Output Layer**: 1 neuron (evaluation score in centipawns)
//!
//! ## Key Design Features
//! - **Incremental Updates**: Feature accumulator is updated differentially for efficiency
//! - **Quantization**: 16-bit accumulators are quantized to 8-bit for network input
//! - **SIMD Optimization**: Critical operations use AVX2/SSE4.1 when available
//! - **Memory Efficiency**: Weights are shared across evaluator instances using Arc
//!
//! ## Evaluation Flow
//! 1. Extract active HalfKP features from position
//! 2. Update accumulator incrementally based on move
//! 3. Transform 16-bit accumulated values to 8-bit (quantization)
//! 4. Forward propagate through the neural network
//! 5. Scale output to centipawn units

pub mod accumulator;
pub mod error;
pub mod features;
pub mod network;
pub mod simd;
pub mod single;
pub mod single_state;
pub mod weights;
use crate::shogi::Move;
use crate::{Color, Position};

use super::evaluate::Evaluator;
use accumulator::Accumulator;
use error::{NNUEError, NNUEResult};
use network::Network;
use std::error::Error;
use std::sync::Arc;
use weights::{load_single_weights, load_weights};

/// エンジン側の有効feature一覧（ベンチレポート用）
#[inline]
pub fn enabled_features_str() -> String {
    let mut v = Vec::new();
    if cfg!(feature = "tt_metrics") {
        v.push("tt_metrics");
    }
    if cfg!(feature = "hashfull_filter") {
        v.push("hashfull_filter");
    }
    if cfg!(feature = "ybwc") {
        v.push("ybwc");
    }
    if cfg!(feature = "nnue_telemetry") {
        v.push("nnue_telemetry");
    }
    if cfg!(feature = "nnue_fast_fma") {
        v.push("nnue_fast_fma");
    }
    if cfg!(feature = "diff_agg_hash") {
        v.push("diff_agg_hash");
    }
    if cfg!(feature = "nightly") {
        v.push("nightly");
    }
    // acc_dim の軽い可視化（ランタイム決定: v1=256 / v2=dims）
    v.push("acc_dim=runtime");

    // 補助ログ: x86/x86_64 の SIMD 検出結果
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        let level = crate::simd::dispatch::SimdLevel::detect();
        v.push(match level {
            crate::simd::dispatch::SimdLevel::Avx512f => "simd=avx512f",
            crate::simd::dispatch::SimdLevel::Avx => "simd=avx",
            crate::simd::dispatch::SimdLevel::Sse2 => "simd=sse2",
            crate::simd::dispatch::SimdLevel::Scalar => "simd=scalar",
        });
    }
    // 補助ログ: AArch64（NEON 常時ON）
    #[cfg(target_arch = "aarch64")]
    {
        v.push("simd=neon");
    }
    // 補助ログ: WASM の SIMD 有無
    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    {
        v.push("simd=wasm128");
    }
    #[cfg(all(target_arch = "wasm32", not(target_feature = "simd128")))]
    {
        v.push("simd=wasm-scalar");
    }
    format!("engine-core:{}", v.join(","))
}

// --- Lightweight telemetry counters (optional) ---
// feature = "nnue_telemetry" で有効化。探索中の acc 経路/フォールバック割合などを集計する。
#[cfg(feature = "nnue_telemetry")]
pub mod telemetry {
    use std::sync::atomic::{AtomicU64, Ordering};

    // Evaluate 経路
    pub static ACC_EVAL_COUNT: AtomicU64 = AtomicU64::new(0);
    pub static FB_HASH_MISMATCH: AtomicU64 = AtomicU64::new(0);
    pub static FB_ACC_EMPTY: AtomicU64 = AtomicU64::new(0);
    pub static FB_FEATURE_OFF: AtomicU64 = AtomicU64::new(0);

    // 差分適用が安全側 refresh になった件数（原因別）
    pub static APPLY_REFRESH_KING: AtomicU64 = AtomicU64::new(0);
    pub static APPLY_REFRESH_OTHER: AtomicU64 = AtomicU64::new(0);

    #[inline]
    pub fn record_acc_eval() {
        ACC_EVAL_COUNT.fetch_add(1, Ordering::Relaxed);
    }
    #[inline]
    pub fn record_fb_hash_mismatch() {
        FB_HASH_MISMATCH.fetch_add(1, Ordering::Relaxed);
    }
    #[inline]
    pub fn record_fb_acc_empty() {
        FB_ACC_EMPTY.fetch_add(1, Ordering::Relaxed);
    }
    #[inline]
    pub fn record_fb_feature_off() {
        FB_FEATURE_OFF.fetch_add(1, Ordering::Relaxed);
    }
    #[inline]
    pub fn record_apply_refresh_king() {
        APPLY_REFRESH_KING.fetch_add(1, Ordering::Relaxed);
    }
    #[inline]
    pub fn record_apply_refresh_other() {
        APPLY_REFRESH_OTHER.fetch_add(1, Ordering::Relaxed);
    }

    #[derive(Debug, Clone, Copy)]
    pub struct Snapshot {
        pub acc: u64,
        pub fb_hash_mismatch: u64,
        pub fb_acc_empty: u64,
        pub fb_feature_off: u64,
        pub apply_refresh_king: u64,
        pub apply_refresh_other: u64,
    }

    #[inline]
    pub fn snapshot_and_reset() -> Snapshot {
        let acc = ACC_EVAL_COUNT.swap(0, Ordering::Relaxed);
        let fb_hash_mismatch = FB_HASH_MISMATCH.swap(0, Ordering::Relaxed);
        let fb_acc_empty = FB_ACC_EMPTY.swap(0, Ordering::Relaxed);
        let fb_feature_off = FB_FEATURE_OFF.swap(0, Ordering::Relaxed);
        let apply_refresh_king = APPLY_REFRESH_KING.swap(0, Ordering::Relaxed);
        let apply_refresh_other = APPLY_REFRESH_OTHER.swap(0, Ordering::Relaxed);
        Snapshot {
            acc,
            fb_hash_mismatch,
            fb_acc_empty,
            fb_feature_off,
            apply_refresh_king,
            apply_refresh_other,
        }
    }
}

/// Scale factor for converting network output to centipawns
///
/// The NNUE network outputs values in a higher resolution internal scale.
/// This factor is used to scale up the network output before final normalization.
/// The value 16 is chosen to provide sufficient precision while avoiding overflow.
// When propagate() returns a Q16 fixed-point like integer, we only need to
// right-shift by 16 to convert to centipawns. Keep the numerator at 1.
const FV_SCALE: i32 = 1;

/// Output scaling shift: right shift by 16 bits to normalize the scaled output
///
/// This shift operation performs the final normalization of the evaluation score.
/// The formula is: final_score = (network_output * FV_SCALE) >> OUTPUT_SCALE_SHIFT
///
/// The 16-bit shift effectively divides by 65536, converting from the internal
/// high-precision scale to standard centipawn units used by the chess engine.
/// This two-step process (multiply then shift) preserves precision during calculation.
const OUTPUT_SCALE_SHIFT: i32 = 16;

/// NNUE evaluator with HalfKP features
///
/// Both feature transformer and network are wrapped in Arc for memory efficiency.
/// This allows multiple evaluator instances to share the same weights (approximately 170MB).
#[derive(Clone)]
pub struct NNUEEvaluator {
    /// Feature transformer for converting HalfKP features to network input
    feature_transformer: Arc<features::FeatureTransformer>,
    /// Neural network for evaluation
    network: Arc<Network>,
}

impl NNUEEvaluator {
    /// Create new NNUE evaluator from weights file (typed error)
    pub fn from_file_typed(path: &str) -> NNUEResult<Self> {
        let (feature_transformer, network) = load_weights(path)?;
        Ok(NNUEEvaluator {
            feature_transformer: Arc::new(feature_transformer),
            network: Arc::new(network),
        })
    }

    /// Create new NNUE evaluator from weights file
    pub fn from_file(path: &str) -> Result<Self, Box<dyn Error>> {
        let (feature_transformer, network) =
            load_weights(path).map_err(|e| -> Box<dyn Error> { Box::new(e) })?;
        Ok(NNUEEvaluator {
            feature_transformer: Arc::new(feature_transformer),
            network: Arc::new(network),
        })
    }

    /// Create new NNUE evaluator with zero weights (for testing)
    pub fn zero() -> Self {
        NNUEEvaluator {
            feature_transformer: Arc::new(features::FeatureTransformer::zero()),
            network: Arc::new(Network::zero()),
        }
    }

    /// Evaluate position using precomputed accumulator
    pub fn evaluate_with_accumulator(&self, pos: &Position, accumulator: &Accumulator) -> i32 {
        // Get perspective-based accumulators
        let (acc_us, acc_them) = if pos.side_to_move == Color::Black {
            (&accumulator.black, &accumulator.white)
        } else {
            (&accumulator.white, &accumulator.black)
        };

        // Run network inference
        let output = self.network.propagate(acc_us, acc_them);

        // Scale to centipawns
        (output * FV_SCALE) >> OUTPUT_SCALE_SHIFT
    }

    /// Get reference to feature transformer
    pub fn feature_transformer(&self) -> &features::FeatureTransformer {
        &self.feature_transformer
    }
}

/// Wrapper for integration with Phase 1 Evaluator trait
// Classic は delta_scratch を by-value で保持し、fork 時の alloc を避ける（NPS 優先）。
#[allow(clippy::large_enum_variant)]
enum Backend {
    Classic {
        evaluator: NNUEEvaluator,
        accumulator_stack: Vec<Accumulator>,
        delta_scratch: accumulator::AccumulatorDelta,
    },
    Single {
        net: std::sync::Arc<single::SingleChannelNet>,
        acc_stack: Vec<single_state::SingleAcc>,
    },
}

pub struct NNUEEvaluatorWrapper {
    backend: Backend,
    /// Position hash tracking for safe fallback in parallel / hookless paths
    tracked_hash: Option<u64>,
}

#[cfg(test)]
static CLASSIC_FALLBACK_HITS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
static SINGLE_FALLBACK_HITS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
#[inline]
pub fn fallback_hits() -> usize {
    CLASSIC_FALLBACK_HITS.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(test)]
#[inline]
pub fn reset_fallback_hits() {
    CLASSIC_FALLBACK_HITS.store(0, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(test)]
#[inline]
pub fn single_fallback_hits() -> usize {
    SINGLE_FALLBACK_HITS.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(test)]
#[inline]
pub fn reset_single_fallback_hits() {
    SINGLE_FALLBACK_HITS.store(0, std::sync::atomic::Ordering::Relaxed);
}

impl NNUEEvaluatorWrapper {
    /// Create new wrapper from weights file (typed error)
    pub fn new_typed(weights_path: &str) -> NNUEResult<Self> {
        match load_weights(weights_path) {
            Ok((ft, net)) => {
                let evaluator = NNUEEvaluator {
                    feature_transformer: std::sync::Arc::new(ft),
                    network: std::sync::Arc::new(net),
                };
                let mut initial_acc = Accumulator::new();
                let empty_pos = Position::empty();
                initial_acc.refresh(&empty_pos, &evaluator.feature_transformer);
                Ok(NNUEEvaluatorWrapper {
                    backend: Backend::Classic {
                        evaluator,
                        accumulator_stack: vec![initial_acc],
                        delta_scratch: accumulator::AccumulatorDelta {
                            removed_b: smallvec::SmallVec::new(),
                            added_b: smallvec::SmallVec::new(),
                            removed_w: smallvec::SmallVec::new(),
                            added_w: smallvec::SmallVec::new(),
                        },
                    },
                    tracked_hash: Some(u64::MAX),
                })
            }
            Err(classic_err) => match load_single_weights(weights_path) {
                Ok(net) => Ok(NNUEEvaluatorWrapper {
                    backend: Backend::Single {
                        net: std::sync::Arc::new(net),
                        acc_stack: Vec::new(),
                    },
                    tracked_hash: Some(u64::MAX),
                }),
                Err(single_err) => Err(NNUEError::BothWeightsLoadFailed {
                    classic: classic_err,
                    single: single_err,
                }),
            },
        }
    }
    /// 現在の重みを共有しつつ、状態（acc_stack/tracked_hash）のない新インスタンスを作る
    pub fn fork_stateless(&self) -> Self {
        match &self.backend {
            Backend::Classic { evaluator, .. } => {
                Self {
                    backend: Backend::Classic {
                        evaluator: evaluator.clone(),
                        accumulator_stack: Vec::new(),
                        delta_scratch: accumulator::AccumulatorDelta {
                            removed_b: smallvec::SmallVec::new(),
                            added_b: smallvec::SmallVec::new(),
                            removed_w: smallvec::SmallVec::new(),
                            added_w: smallvec::SmallVec::new(),
                        },
                    },
                    // 初期状態は None（evaluate はスタック空→フル再構築）
                    tracked_hash: None,
                }
            }
            Backend::Single { net, .. } => Self {
                backend: Backend::Single {
                    net: net.clone(),
                    acc_stack: Vec::new(),
                },
                // 初期状態は None（evaluate は acc 不在→フル経路）
                tracked_hash: None,
            },
        }
    }
    /// Create new wrapper from weights file
    pub fn new(weights_path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        // Try classic first and capture error for later reporting if needed
        match load_weights(weights_path) {
            Ok((ft, net)) => {
                let evaluator = NNUEEvaluator {
                    feature_transformer: std::sync::Arc::new(ft),
                    network: std::sync::Arc::new(net),
                };
                let mut initial_acc = Accumulator::new();
                let empty_pos = Position::empty();
                initial_acc.refresh(&empty_pos, &evaluator.feature_transformer);
                Ok(NNUEEvaluatorWrapper {
                    backend: Backend::Classic {
                        evaluator,
                        accumulator_stack: vec![initial_acc],
                        delta_scratch: accumulator::AccumulatorDelta {
                            removed_b: smallvec::SmallVec::new(),
                            added_b: smallvec::SmallVec::new(),
                            removed_w: smallvec::SmallVec::new(),
                            added_w: smallvec::SmallVec::new(),
                        },
                    },
                    tracked_hash: Some(u64::MAX),
                })
            }
            Err(classic_err) => match load_single_weights(weights_path) {
                Ok(net) => Ok(NNUEEvaluatorWrapper {
                    backend: Backend::Single {
                        net: std::sync::Arc::new(net),
                        acc_stack: Vec::new(),
                    },
                    tracked_hash: Some(u64::MAX),
                }),
                Err(single_err) => Err(Box::new(NNUEError::BothWeightsLoadFailed {
                    classic: classic_err,
                    single: single_err,
                })),
            },
        }
    }

    // NOTE: set_position/do_move/undo_move は下部の実装へ集約

    /// Test-only: construct a wrapper with a provided SINGLE net
    #[cfg(test)]
    pub fn new_with_single_net_for_test(net: single::SingleChannelNet) -> Self {
        NNUEEvaluatorWrapper {
            backend: Backend::Single {
                net: std::sync::Arc::new(net),
                acc_stack: Vec::new(),
            },
            tracked_hash: None,
        }
    }

    /// SINGLE 用: 現在ノードの Acc スナップショットを返す
    #[must_use]
    pub fn snapshot_single(&self) -> Option<single_state::SingleAcc> {
        if let Backend::Single { acc_stack, .. } = &self.backend {
            acc_stack.last().cloned()
        } else {
            None
        }
    }

    /// SINGLE 用: 指定の Acc を現在ノードとして復元し、tracked_hash を pos に合わせる
    #[track_caller]
    pub fn restore_single_at(&mut self, pos: &Position, acc: single_state::SingleAcc) {
        if let Backend::Single { net, acc_stack, .. } = &mut self.backend {
            // 開発時の寸止め検証：異なる net 由来の Acc を誤って渡していないか
            debug_assert_eq!(acc.pre_black.len(), net.acc_dim);
            debug_assert_eq!(acc.pre_white.len(), net.acc_dim);
            // 追加検査：重み UID の一致（不一致なら安全側で refresh）
            if acc.weights_uid == net.uid {
                acc_stack.clear();
                acc_stack.push(acc);
            } else {
                #[cfg(debug_assertions)]
                log::warn!("[NNUE] restore_single_at: weights UID mismatch; refreshing acc");
                acc_stack.clear();
                acc_stack.push(single_state::SingleAcc::refresh(pos, net));
            }
            self.tracked_hash = Some(pos.zobrist_hash());
        }
    }

    /// Create zero-initialized evaluator (for testing)
    pub fn zero() -> Self {
        let evaluator = NNUEEvaluator::zero();
        let mut initial_acc = Accumulator::new();
        let empty_pos = Position::empty();
        initial_acc.refresh(&empty_pos, &evaluator.feature_transformer);
        NNUEEvaluatorWrapper {
            backend: Backend::Classic {
                evaluator,
                accumulator_stack: vec![initial_acc],
                delta_scratch: accumulator::AccumulatorDelta {
                    removed_b: smallvec::SmallVec::new(),
                    added_b: smallvec::SmallVec::new(),
                    removed_w: smallvec::SmallVec::new(),
                    added_w: smallvec::SmallVec::new(),
                },
            },
            // set_position まで評価で Acc を使わない安全側にする
            tracked_hash: Some(u64::MAX),
        }
    }
}

impl Evaluator for NNUEEvaluatorWrapper {
    fn evaluate(&self, pos: &Position) -> i32 {
        match &self.backend {
            Backend::Classic {
                evaluator,
                accumulator_stack,
                ..
            } => {
                // 初期状態（fork_stateless 直後などでスタック空）は常にフル再構築
                if accumulator_stack.is_empty() {
                    #[cfg(feature = "nnue_telemetry")]
                    telemetry::record_fb_acc_empty();
                    let mut tmp = accumulator::Accumulator::new();
                    tmp.refresh(pos, evaluator.feature_transformer());
                    return evaluator.evaluate_with_accumulator(pos, &tmp);
                }

                // 単スレ探索（on_do/undo 経路）では tracked_hash=None とし、acc が常に同期済み
                // 並列探索では tracked_hash=Some(root_hash) で root 以外は不一致→フル評価へフォールバック
                let use_acc = self.tracked_hash.is_none_or(|h| h == pos.zobrist_hash());
                if use_acc {
                    if let Some(acc) = accumulator_stack.last() {
                        #[cfg(feature = "nnue_telemetry")]
                        telemetry::record_acc_eval();
                        return evaluator.evaluate_with_accumulator(pos, acc);
                    } else {
                        #[cfg(feature = "nnue_telemetry")]
                        telemetry::record_fb_acc_empty();
                    }
                } else {
                    #[cfg(feature = "nnue_telemetry")]
                    telemetry::record_fb_hash_mismatch();
                }

                // フォールバック: 一時 Accumulator を構築してフル評価
                #[cfg(test)]
                {
                    CLASSIC_FALLBACK_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                let mut tmp = accumulator::Accumulator::new();
                tmp.refresh(pos, evaluator.feature_transformer());
                evaluator.evaluate_with_accumulator(pos, &tmp)
            }
            Backend::Single { net, acc_stack } => {
                let use_acc = self.tracked_hash.is_none_or(|h| h == pos.zobrist_hash());
                if use_acc {
                    if let Some(acc) = acc_stack.last() {
                        #[cfg(feature = "nnue_telemetry")]
                        telemetry::record_acc_eval();
                        return net.evaluate_from_accumulator_pre(acc.acc_for(pos.side_to_move));
                    } else {
                        #[cfg(feature = "nnue_telemetry")]
                        telemetry::record_fb_acc_empty();
                        // 安全側：直接評価
                        return net.evaluate(pos);
                    }
                } else {
                    #[cfg(feature = "nnue_telemetry")]
                    telemetry::record_fb_hash_mismatch();
                }

                #[cfg(test)]
                {
                    SINGLE_FALLBACK_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                net.evaluate(pos)
            }
        }
    }
}

impl NNUEEvaluatorWrapper {
    /// Hook: set_position（ルート同期）。Single ではここで Acc を構築する。
    pub fn set_position(&mut self, pos: &Position) {
        match &mut self.backend {
            Backend::Classic {
                evaluator,
                accumulator_stack,
                ..
            } => {
                accumulator_stack.clear();
                let mut acc = Accumulator::new();
                acc.refresh(pos, &evaluator.feature_transformer);
                accumulator_stack.push(acc);
                self.tracked_hash = Some(pos.zobrist_hash());
            }
            Backend::Single { net, acc_stack } => {
                acc_stack.clear();
                acc_stack.push(single_state::SingleAcc::refresh(pos, net));
                self.tracked_hash = Some(pos.zobrist_hash());
            }
        }
    }

    /// Hook: do_move（増分）。
    /// - Single は差分更新を行う（常時有効）
    /// - Classic は安全側フォールバックを併用
    pub fn do_move(&mut self, pos: &Position, mv: Move) -> NNUEResult<()> {
        match &mut self.backend {
            Backend::Classic {
                evaluator,
                accumulator_stack,
                delta_scratch,
            } => {
                // Null move: Acc は変更しない（複製して積むだけ）
                if mv.is_null() {
                    if let Some(cur) = accumulator_stack.last().cloned() {
                        accumulator_stack.push(cur);
                    } else {
                        let mut acc = accumulator::Accumulator::new();
                        acc.refresh(pos, &evaluator.feature_transformer);
                        accumulator_stack.push(acc);
                    }
                    self.tracked_hash = None;
                    return Ok(());
                }
                let current_acc =
                    accumulator_stack.last().ok_or(NNUEError::EmptyAccumulatorStack)?;
                match accumulator::calculate_update_into(delta_scratch, pos, mv)? {
                    accumulator::UpdateOp::Delta => {
                        let mut new_acc = current_acc.clone();
                        new_acc.update(delta_scratch, Color::Black, &evaluator.feature_transformer);
                        new_acc.update(delta_scratch, Color::White, &evaluator.feature_transformer);
                        accumulator_stack.push(new_acc);
                    }
                    accumulator::UpdateOp::FullRefresh => {
                        // 安全側: 子局面でフル再構築
                        let mut post = pos.clone();
                        let _u = post.do_move(mv);
                        let mut acc = accumulator::Accumulator::new();
                        acc.refresh(&post, &evaluator.feature_transformer);
                        accumulator_stack.push(acc);
                    }
                }
                // Classic は子局面に同期済みなので、ハッシュを無効化（安全側）。
                self.tracked_hash = None;
                Ok(())
            }
            Backend::Single { net, acc_stack } => {
                // Null move: Acc は変更しない（複製して積むだけ）
                if mv.is_null() {
                    if let Some(cur) = acc_stack.last().cloned() {
                        acc_stack.push(cur);
                    } else {
                        acc_stack.push(single_state::SingleAcc::refresh(pos, net));
                    }
                    self.tracked_hash = None;
                    return Ok(());
                }
                let current = acc_stack.last().cloned();
                if let Some(cur) = current {
                    let next = single_state::SingleAcc::apply_update(&cur, pos, mv, net);
                    acc_stack.push(next);
                } else {
                    // 初回 do_move で acc_stack が空の場合でも、
                    // 子局面の Acc を積む（親局面ではなく post で初期化）。
                    let mut post = pos.clone();
                    let _u = post.do_move(mv);
                    acc_stack.push(single_state::SingleAcc::refresh(&post, net));
                }
                self.tracked_hash = None;
                Ok(())
            }
        }
    }

    /// Hook: undo_move（増分戻し）。
    pub fn undo_move(&mut self) {
        match &mut self.backend {
            Backend::Classic {
                accumulator_stack, ..
            } => {
                #[cfg(debug_assertions)]
                debug_assert!(
                    !accumulator_stack.is_empty(),
                    "undo_move called with empty accumulator_stack"
                );
                if accumulator_stack.len() > 1 {
                    accumulator_stack.pop();
                }
                self.tracked_hash = None;
            }
            Backend::Single { acc_stack, .. } => {
                #[cfg(debug_assertions)]
                debug_assert!(!acc_stack.is_empty(), "undo_move called with empty acc_stack");
                if acc_stack.len() > 1 {
                    acc_stack.pop();
                }
                self.tracked_hash = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{shogi::Move, usi::parse_usi_square, Piece, PieceType};

    use super::*;

    #[test]
    fn test_nnue_evaluator_creation() {
        let evaluator = NNUEEvaluator::zero();
        let pos = Position::startpos();
        let mut acc = Accumulator::new();
        acc.refresh(&pos, &evaluator.feature_transformer);

        let eval = evaluator.evaluate_with_accumulator(&pos, &acc);
        assert_eq!(eval, 0); // Zero weights should give zero eval
    }

    #[test]
    fn test_nnue_evaluator_wrapper_do_move_error() {
        let mut wrapper = NNUEEvaluatorWrapper::zero();
        let mut pos = Position::empty();

        // Add a piece but no kings
        pos.board
            .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::Pawn, Color::Black));

        // Try to make a move - should fail due to missing kings
        let mv =
            Move::make_normal(parse_usi_square("5e").unwrap(), parse_usi_square("5d").unwrap());
        let result = wrapper.do_move(&pos, mv);

        assert!(result.is_err());
        match result {
            Err(error::NNUEError::KingNotFound(_)) => (),
            _ => panic!("Expected KingNotFound error"),
        }
    }

    #[test]
    fn test_nnue_evaluator_arc_sharing() {
        let evaluator1 = NNUEEvaluator::zero();
        let evaluator2 = evaluator1.clone();

        // Both should share the same Arc pointers
        assert!(Arc::ptr_eq(&evaluator1.feature_transformer, &evaluator2.feature_transformer));
        assert!(Arc::ptr_eq(&evaluator1.network, &evaluator2.network));
    }

    #[test]
    fn test_classic_fallback_full_rebuild() {
        // Classic backend (zero weights)
        let mut wrapper = NNUEEvaluatorWrapper::zero();
        let mut pos = Position::startpos();
        wrapper.set_position(&pos);

        // Move without notifying wrapper (simulate HookSuppressor path)
        let mv =
            Move::make_normal(parse_usi_square("3g").unwrap(), parse_usi_square("3f").unwrap());
        let undo = pos.do_move(mv);

        // reset counter before evaluation
        super::reset_fallback_hits();

        // Evaluate should fallback to full rebuild (tracked_hash != pos)
        let s = wrapper.evaluate(&pos);
        // With zero weights, evaluation is zero
        assert_eq!(s, 0);
        // And fallback path must be taken at least once
        assert!(super::fallback_hits() > 0);

        // Clean up
        pos.undo_move(mv, undo);
    }

    #[test]
    fn test_single_refresh_matches_direct_evaluate() {
        use crate::shogi::SHOGI_BOARD_SIZE;
        let n_feat = SHOGI_BOARD_SIZE * super::features::FE_END;
        let d = 8usize; // small acc dim for test

        // Construct a tiny SINGLE network
        let net = super::single::SingleChannelNet {
            n_feat,
            acc_dim: d,
            scale: 600.0,
            w0: vec![0.0; n_feat * d], // zero rows
            b0: Some(vec![0.1; d]),    // bias only
            w2: vec![1.0; d],          // sum all activations
            b2: 1.0,                   // 非ゼロ出力になるように調整
            uid: 1,
        };

        // Start position with both kings
        let mut pos = Position::startpos();

        // Black to move
        pos.side_to_move = Color::Black;
        let acc_b = super::single_state::SingleAcc::refresh(&pos, &net);
        let eval_full_b = net.evaluate(&pos);
        let eval_acc_b = net.evaluate_from_accumulator_pre(acc_b.acc_for(Color::Black));
        assert_eq!(eval_full_b, eval_acc_b);
        assert_ne!(eval_full_b, 0);

        // White to move (flip path)
        pos.side_to_move = Color::White;
        let acc_w = super::single_state::SingleAcc::refresh(&pos, &net);
        let eval_full_w = net.evaluate(&pos);
        let eval_acc_w = net.evaluate_from_accumulator_pre(acc_w.acc_for(Color::White));
        assert_eq!(eval_full_w, eval_acc_w);
        assert_ne!(eval_full_w, 0);
    }

    #[test]
    fn test_single_apply_update_matches_refresh_trivial() {
        use crate::shogi::SHOGI_BOARD_SIZE;
        let n_feat = SHOGI_BOARD_SIZE * super::features::FE_END;
        let d = 4usize;
        // net: w0=0.5, b0=0.2, w2=1, b2=0
        let net = super::single::SingleChannelNet {
            n_feat,
            acc_dim: d,
            scale: 600.0,
            w0: vec![0.5; n_feat * d],
            b0: Some(vec![0.2; d]),
            w2: vec![1.0; d],
            b2: 0.0,
            uid: 2,
        };

        let mut pos = Position::startpos();
        let acc0 = super::single_state::SingleAcc::refresh(&pos, &net);

        // one legal pawn move 7g7f (3g3f in USI coords)
        let mv =
            Move::make_normal(parse_usi_square("3g").unwrap(), parse_usi_square("3f").unwrap());
        let acc1 = super::single_state::SingleAcc::apply_update(&acc0, &pos, mv, &net);
        let undo = pos.do_move(mv);

        // eval via acc (side-to-move has flipped)
        let eval_acc = net.evaluate_from_accumulator_pre(acc1.acc_for(pos.side_to_move));
        // eval via full refresh
        let acc_full = super::single_state::SingleAcc::refresh(&pos, &net);
        let eval_full = net.evaluate_from_accumulator_pre(acc_full.acc_for(pos.side_to_move));
        assert_eq!(eval_acc, eval_full);

        pos.undo_move(mv, undo);
    }

    #[test]
    fn test_single_drop_update_matches_refresh() {
        use crate::usi::parse_usi_square;

        let n_feat = crate::shogi::SHOGI_BOARD_SIZE * super::features::FE_END;
        let d = 4usize;
        let net = super::single::SingleChannelNet {
            n_feat,
            acc_dim: d,
            scale: 600.0,
            w0: vec![0.5; n_feat * d],
            b0: Some(vec![0.2; d]),
            w2: vec![1.0; d],
            b2: 0.0,
            uid: 3,
        };

        let mut pos = Position::startpos();
        // Give black a pawn in hand
        pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()] = 1;
        let acc0 = super::single_state::SingleAcc::refresh(&pos, &net);
        let mv = Move::make_drop(PieceType::Pawn, parse_usi_square("5e").unwrap());
        let acc1 = super::single_state::SingleAcc::apply_update(&acc0, &pos, mv, &net);
        let undo = pos.do_move(mv);

        let eval_acc = net.evaluate_from_accumulator_pre(acc1.acc_for(pos.side_to_move));
        let acc_full = super::single_state::SingleAcc::refresh(&pos, &net);
        let eval_full = net.evaluate_from_accumulator_pre(acc_full.acc_for(pos.side_to_move));
        assert_eq!(eval_acc, eval_full);

        pos.undo_move(mv, undo);
    }

    #[test]
    fn test_single_capture_update_matches_refresh() {
        use crate::usi::parse_usi_square;

        let n_feat = crate::shogi::SHOGI_BOARD_SIZE * super::features::FE_END;
        let d = 4usize;
        let net = super::single::SingleChannelNet {
            n_feat,
            acc_dim: d,
            scale: 600.0,
            w0: vec![0.5; n_feat * d],
            b0: Some(vec![0.2; d]),
            w2: vec![1.0; d],
            b2: 0.0,
            uid: 4,
        };

        let mut pos = Position::empty();
        // Place kings
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));
        // Place black pawn 3g, white pawn 3f (Black to move can capture)
        pos.board
            .put_piece(parse_usi_square("3g").unwrap(), Piece::new(PieceType::Pawn, Color::Black));
        pos.board
            .put_piece(parse_usi_square("3f").unwrap(), Piece::new(PieceType::Pawn, Color::White));
        pos.side_to_move = Color::Black;

        let acc0 = super::single_state::SingleAcc::refresh(&pos, &net);
        let mv =
            Move::make_normal(parse_usi_square("3g").unwrap(), parse_usi_square("3f").unwrap());
        let acc1 = super::single_state::SingleAcc::apply_update(&acc0, &pos, mv, &net);
        let undo = pos.do_move(mv);

        let eval_acc = net.evaluate_from_accumulator_pre(acc1.acc_for(pos.side_to_move));
        let acc_full = super::single_state::SingleAcc::refresh(&pos, &net);
        let eval_full = net.evaluate_from_accumulator_pre(acc_full.acc_for(pos.side_to_move));
        assert_eq!(eval_acc, eval_full);

        pos.undo_move(mv, undo);
    }

    #[test]
    fn test_single_promotion_update_matches_refresh() {
        use crate::usi::parse_usi_square;

        let n_feat = crate::shogi::SHOGI_BOARD_SIZE * super::features::FE_END;
        let d = 4usize;
        let net = super::single::SingleChannelNet {
            n_feat,
            acc_dim: d,
            scale: 600.0,
            w0: vec![0.5; n_feat * d],
            b0: Some(vec![0.2; d]),
            w2: vec![1.0; d],
            b2: 0.0,
            uid: 12,
        };

        let mut pos = Position::empty();
        // Kings only
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));
        // Black pawn ready to promote: 3c -> 3b+
        pos.board
            .put_piece(parse_usi_square("3c").unwrap(), Piece::new(PieceType::Pawn, Color::Black));
        pos.side_to_move = Color::Black;

        let acc0 = super::single_state::SingleAcc::refresh(&pos, &net);
        let mv = Move::normal_with_piece(
            parse_usi_square("3c").unwrap(),
            parse_usi_square("3b").unwrap(),
            true,
            PieceType::Pawn,
            None,
        );
        let acc1 = super::single_state::SingleAcc::apply_update(&acc0, &pos, mv, &net);
        let undo = pos.do_move(mv);

        let eval_acc = net.evaluate_from_accumulator_pre(acc1.acc_for(pos.side_to_move));
        let acc_full = super::single_state::SingleAcc::refresh(&pos, &net);
        let eval_full = net.evaluate_from_accumulator_pre(acc_full.acc_for(pos.side_to_move));
        assert_eq!(eval_acc, eval_full);

        pos.undo_move(mv, undo);
    }

    #[test]
    fn test_single_king_move_refresh_fallback_matches_refresh() {
        use crate::usi::parse_usi_square;

        let n_feat = crate::shogi::SHOGI_BOARD_SIZE * super::features::FE_END;
        let d = 4usize;
        let net = super::single::SingleChannelNet {
            n_feat,
            acc_dim: d,
            scale: 600.0,
            w0: vec![0.0; n_feat * d],
            b0: Some(vec![0.2; d]),
            w2: vec![1.0; d],
            b2: 0.0,
            uid: 5,
        };

        let mut pos = Position::empty();
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));
        pos.side_to_move = Color::Black;

        let acc0 = super::single_state::SingleAcc::refresh(&pos, &net);
        let mv =
            Move::make_normal(parse_usi_square("5i").unwrap(), parse_usi_square("5h").unwrap());
        let acc1 = super::single_state::SingleAcc::apply_update(&acc0, &pos, mv, &net);

        let undo = pos.do_move(mv);
        let eval_acc = net.evaluate_from_accumulator_pre(acc1.acc_for(pos.side_to_move));
        let acc_full = super::single_state::SingleAcc::refresh(&pos, &net);
        let eval_full = net.evaluate_from_accumulator_pre(acc_full.acc_for(pos.side_to_move));
        assert_eq!(eval_acc, eval_full);
        pos.undo_move(mv, undo);
    }

    #[test]
    fn test_single_two_step_update_matches_refresh() {
        use crate::usi::parse_usi_square;

        let n_feat = crate::shogi::SHOGI_BOARD_SIZE * super::features::FE_END;
        let d = 8usize; // small acc dim
                        // 非トリビアル（w0!=0, b0<0）で ReLU 交差の可能性を持たせる
        let net = super::single::SingleChannelNet {
            n_feat,
            acc_dim: d,
            scale: 600.0,
            w0: vec![0.25; n_feat * d],
            b0: Some(vec![-0.1; d]),
            w2: vec![1.0; d],
            b2: 0.5,
            uid: 6,
        };

        // 盤面セットアップ：玉と歩
        let mut pos = Position::empty();
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));
        pos.board
            .put_piece(parse_usi_square("3g").unwrap(), Piece::new(PieceType::Pawn, Color::Black));
        pos.board
            .put_piece(parse_usi_square("7c").unwrap(), Piece::new(PieceType::Pawn, Color::White));
        pos.side_to_move = Color::Black;

        let acc0 = super::single_state::SingleAcc::refresh(&pos, &net);
        // 1手目：3g->3f
        let mv1 =
            Move::make_normal(parse_usi_square("3g").unwrap(), parse_usi_square("3f").unwrap());
        let acc1 = super::single_state::SingleAcc::apply_update(&acc0, &pos, mv1, &net);
        let _u1 = pos.do_move(mv1);
        // 2手目：7c->7d（白）
        let mv2 =
            Move::make_normal(parse_usi_square("7c").unwrap(), parse_usi_square("7d").unwrap());
        let acc2 = super::single_state::SingleAcc::apply_update(&acc1, &pos, mv2, &net);
        let _u2 = pos.do_move(mv2);

        // フル再構築と一致
        let eval_acc = net.evaluate_from_accumulator_pre(acc2.acc_for(pos.side_to_move));
        let acc_full = super::single_state::SingleAcc::refresh(&pos, &net);
        let eval_full = net.evaluate_from_accumulator_pre(acc_full.acc_for(pos.side_to_move));
        assert_eq!(eval_acc, eval_full);
    }

    #[test]
    fn test_single_capture_promoted_to_hand_update_matches_refresh() {
        use crate::usi::parse_usi_square;

        let n_feat = crate::shogi::SHOGI_BOARD_SIZE * super::features::FE_END;
        let d = 8usize; // small acc dim
        let net = super::single::SingleChannelNet {
            n_feat,
            acc_dim: d,
            scale: 600.0,
            w0: vec![0.25; n_feat * d],
            b0: Some(vec![0.05; d]),
            w2: vec![1.0; d],
            b2: 0.0,
            uid: 7,
        };

        let mut pos = Position::empty();
        // Kings
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));
        // Black rook at 3g
        pos.board
            .put_piece(parse_usi_square("3g").unwrap(), Piece::new(PieceType::Rook, Color::Black));
        // White promoted silver at 3f
        let mut ps = Piece::new(PieceType::Silver, Color::White);
        ps.promoted = true;
        pos.board.put_piece(parse_usi_square("3f").unwrap(), ps);
        pos.side_to_move = Color::Black;

        let acc0 = super::single_state::SingleAcc::refresh(&pos, &net);
        let mv =
            Move::make_normal(parse_usi_square("3g").unwrap(), parse_usi_square("3f").unwrap());
        let acc1 = super::single_state::SingleAcc::apply_update(&acc0, &pos, mv, &net);
        let undo = pos.do_move(mv);

        let eval_acc = net.evaluate_from_accumulator_pre(acc1.acc_for(pos.side_to_move));
        let acc_full = super::single_state::SingleAcc::refresh(&pos, &net);
        let eval_full = net.evaluate_from_accumulator_pre(acc_full.acc_for(pos.side_to_move));
        assert_eq!(eval_acc, eval_full);

        pos.undo_move(mv, undo);
    }

    #[test]
    fn test_single_wrapper_chain_matches_direct_eval() {
        use crate::usi::parse_usi_square;

        let n_feat = crate::shogi::SHOGI_BOARD_SIZE * super::features::FE_END;
        let d = 8usize;
        let net = super::single::SingleChannelNet {
            n_feat,
            acc_dim: d,
            scale: 600.0,
            w0: vec![0.2; n_feat * d],
            b0: Some(vec![0.05; d]),
            w2: vec![1.0; d],
            b2: 0.3,
            uid: 8,
        };

        let mut pos = Position::startpos();
        let mut wrapper = NNUEEvaluatorWrapper::new_with_single_net_for_test(net.clone());
        wrapper.set_position(&pos);

        // Do two moves with wrapper and position
        let m1 =
            Move::make_normal(parse_usi_square("3g").unwrap(), parse_usi_square("3f").unwrap());
        let _ = wrapper.do_move(&pos, m1);
        let u1 = pos.do_move(m1);
        let m2 =
            Move::make_normal(parse_usi_square("7c").unwrap(), parse_usi_square("7d").unwrap());
        let _ = wrapper.do_move(&pos, m2);
        let u2 = pos.do_move(m2);

        // Wrapper eval should match direct net.eval（差分 acc 経路でも等価）
        let s_wrap = wrapper.evaluate(&pos);
        let s_dir = net.evaluate(&pos);
        assert_eq!(s_wrap, s_dir);

        // Undo two moves
        wrapper.undo_move();
        wrapper.undo_move();
        pos.undo_move(m2, u2);
        pos.undo_move(m1, u1);

        // Still matches at original position
        let s_wrap0 = wrapper.evaluate(&pos);
        let s_dir0 = net.evaluate(&pos);
        assert_eq!(s_wrap0, s_dir0);
    }

    #[test]
    fn test_single_drop_two_times_matches_refresh() {
        use crate::usi::parse_usi_square;

        let n_feat = crate::shogi::SHOGI_BOARD_SIZE * super::features::FE_END;
        let d = 8usize;
        let net = super::single::SingleChannelNet {
            n_feat,
            acc_dim: d,
            scale: 600.0,
            w0: vec![0.1; n_feat * d],
            b0: Some(vec![0.01; d]),
            w2: vec![1.0; d],
            b2: 0.0,
            uid: 9,
        };

        let mut pos = Position::empty();
        // Kings
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));
        // Give black two pawns in hand, white one pawn for the second ply
        pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()] = 2;
        pos.hands[Color::White as usize][PieceType::Pawn.hand_index().unwrap()] = 1;
        pos.side_to_move = Color::Black;

        // acc0
        let acc0 = super::single_state::SingleAcc::refresh(&pos, &net);
        // 1st drop: 5e
        let mv1 = Move::make_drop(PieceType::Pawn, parse_usi_square("5e").unwrap());
        let acc1 = super::single_state::SingleAcc::apply_update(&acc0, &pos, mv1, &net);
        let u1 = pos.do_move(mv1);
        let eval_acc1 = net.evaluate_from_accumulator_pre(acc1.acc_for(pos.side_to_move));
        let full1 = super::single_state::SingleAcc::refresh(&pos, &net);
        let eval_full1 = net.evaluate_from_accumulator_pre(full1.acc_for(pos.side_to_move));
        assert_eq!(eval_acc1, eval_full1);

        // 2nd drop (now White to move): 3e (different file to avoid ni-fu rule)
        let mv2 = Move::make_drop(PieceType::Pawn, parse_usi_square("3e").unwrap());
        let acc2 = super::single_state::SingleAcc::apply_update(&acc1, &pos, mv2, &net);
        let u2 = pos.do_move(mv2);
        let eval_acc2 = net.evaluate_from_accumulator_pre(acc2.acc_for(pos.side_to_move));
        let full2 = super::single_state::SingleAcc::refresh(&pos, &net);
        let eval_full2 = net.evaluate_from_accumulator_pre(full2.acc_for(pos.side_to_move));
        assert_eq!(eval_acc2, eval_full2);

        // cleanup
        pos.undo_move(mv2, u2);
        pos.undo_move(mv1, u1);
    }

    #[test]
    fn test_single_non_promotion_update_matches_refresh() {
        use crate::usi::parse_usi_square;

        let n_feat = crate::shogi::SHOGI_BOARD_SIZE * super::features::FE_END;
        let d = 8usize;
        let net = super::single::SingleChannelNet {
            n_feat,
            acc_dim: d,
            scale: 600.0,
            w0: vec![0.2; n_feat * d],
            b0: Some(vec![0.05; d]),
            w2: vec![1.0; d],
            b2: 0.0,
            uid: 10,
        };

        let mut pos = Position::empty();
        // Kings
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));
        // Black silver in promotion zone (3c -> 3b can be non-promotion)
        pos.board.put_piece(
            parse_usi_square("3c").unwrap(),
            Piece::new(PieceType::Silver, Color::Black),
        );
        pos.side_to_move = Color::Black;

        let acc0 = super::single_state::SingleAcc::refresh(&pos, &net);
        let mv = Move::normal_with_piece(
            parse_usi_square("3c").unwrap(),
            parse_usi_square("3b").unwrap(),
            false,
            PieceType::Silver,
            None,
        );
        let acc1 = super::single_state::SingleAcc::apply_update(&acc0, &pos, mv, &net);
        let u = pos.do_move(mv);
        let eval_acc = net.evaluate_from_accumulator_pre(acc1.acc_for(pos.side_to_move));
        let full = super::single_state::SingleAcc::refresh(&pos, &net);
        let eval_full = net.evaluate_from_accumulator_pre(full.acc_for(pos.side_to_move));
        assert_eq!(eval_acc, eval_full);
        pos.undo_move(mv, u);
    }

    #[test]
    fn test_single_wrapper_fallback_on_mismatch() {
        use crate::usi::parse_usi_square;

        let n_feat = crate::shogi::SHOGI_BOARD_SIZE * super::features::FE_END;
        let d = 8usize;
        let net = super::single::SingleChannelNet {
            n_feat,
            acc_dim: d,
            scale: 600.0,
            w0: vec![0.2; n_feat * d],
            b0: Some(vec![0.05; d]),
            w2: vec![1.0; d],
            b2: 0.3,
            uid: 13,
        };

        let mut pos = Position::startpos();
        let mut wrapper = NNUEEvaluatorWrapper::new_with_single_net_for_test(net.clone());
        wrapper.set_position(&pos);

        // mutate pos externally without notifying wrapper
        let mv =
            Move::make_normal(parse_usi_square("3g").unwrap(), parse_usi_square("3f").unwrap());
        let u = pos.do_move(mv);

        // wrapper must fallback → equals direct net.evaluate(pos)
        super::reset_single_fallback_hits();
        if let Backend::Single { net, .. } = &wrapper.backend {
            let s_wrap = wrapper.evaluate(&pos);
            let s_dir = net.evaluate(&pos);
            // 子局面に同期した acc（refresh）経由の評価とも一致することを確認
            let s_acc_refreshed = {
                let full = super::single_state::SingleAcc::refresh(&pos, net);
                net.evaluate_from_accumulator_pre(full.acc_for(pos.side_to_move))
            };
            assert_eq!(s_wrap, s_dir);
            assert_eq!(s_wrap, s_acc_refreshed);
            // フォールバックが使われたことを確認
            assert!(super::single_fallback_hits() > 0);
        } else {
            panic!("expected Single backend");
        }

        pos.undo_move(mv, u);
    }

    #[test]
    fn test_single_random_chain_matches_refresh() {
        use crate::movegen::MoveGenerator;
        use rand::{RngCore, SeedableRng};

        let n_feat = crate::shogi::SHOGI_BOARD_SIZE * super::features::FE_END;
        let d = 8usize;
        let net = super::single::SingleChannelNet {
            n_feat,
            acc_dim: d,
            scale: 600.0,
            w0: vec![0.15; n_feat * d],
            b0: Some(vec![-0.02; d]),
            w2: vec![1.0; d],
            b2: 0.1,
            uid: 9,
        };

        let mut pos = Position::startpos();
        let mut acc = super::single_state::SingleAcc::refresh(&pos, &net);
        let mut rng = rand_xoshiro::Xoshiro128Plus::seed_from_u64(0xC0FFEE);
        let gen = MoveGenerator::new();

        for _ply in 0..20 {
            let moves = gen.generate_all(&pos).unwrap_or_default();
            if moves.is_empty() {
                break;
            }
            let idx = (rng.next_u32() as usize) % moves.len();
            let mv = moves[idx];
            let next = super::single_state::SingleAcc::apply_update(&acc, &pos, mv, &net);
            let _u = pos.do_move(mv);

            // Compare
            let eval_acc = net.evaluate_from_accumulator_pre(next.acc_for(pos.side_to_move));
            let full = super::single_state::SingleAcc::refresh(&pos, &net);
            let eval_full = net.evaluate_from_accumulator_pre(full.acc_for(pos.side_to_move));
            let eval_direct = net.evaluate(&pos);
            assert_eq!(eval_acc, eval_full);
            assert_eq!(eval_acc, eval_direct);

            acc = next;
        }
    }

    #[test]
    fn test_single_null_move_keeps_acc_and_matches_refresh() {
        use crate::usi::parse_usi_square;

        let n_feat = crate::shogi::SHOGI_BOARD_SIZE * super::features::FE_END;
        let d = 8usize;
        let net = super::single::SingleChannelNet {
            n_feat,
            acc_dim: d,
            scale: 600.0,
            w0: vec![0.2; n_feat * d],
            b0: Some(vec![0.01; d]),
            w2: vec![1.0; d],
            b2: 0.0,
            uid: 10,
        };

        // Kings only position to keep it simple
        let mut pos = Position::empty();
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));

        let mut wrapper = NNUEEvaluatorWrapper::new_with_single_net_for_test(net.clone());
        wrapper.set_position(&pos);

        // Do null move on both wrapper and position
        let _ = wrapper.do_move(&pos, Move::null());
        let undo_null = pos.do_null_move();

        let s_wrap = wrapper.evaluate(&pos);
        let s_acc = wrapper
            .snapshot_single()
            .map(|acc| net.evaluate_from_accumulator_pre(acc.acc_for(pos.side_to_move)))
            .unwrap_or_else(|| net.evaluate(&pos));
        let s_full = net.evaluate(&pos);
        assert_eq!(s_wrap, s_acc);
        assert_eq!(s_wrap, s_full);

        // Undo null move
        wrapper.undo_move();
        pos.undo_null_move(undo_null);

        let s_wrap0 = wrapper.evaluate(&pos);
        let s_full0 = net.evaluate(&pos);
        assert_eq!(s_wrap0, s_full0);
    }

    #[test]
    fn test_single_thread_local_acc_parallel_smoke() {
        use crate::movegen::MoveGenerator;
        use crossbeam::scope;
        use rand::{RngCore, SeedableRng};

        let n_feat = crate::shogi::SHOGI_BOARD_SIZE * super::features::FE_END;
        let d = 8usize;
        let net = super::single::SingleChannelNet {
            n_feat,
            acc_dim: d,
            scale: 600.0,
            w0: vec![0.2; n_feat * d],
            b0: Some(vec![0.01; d]),
            w2: vec![1.0; d],
            b2: 0.0,
            uid: 11,
        };

        // Root setup
        let root_pos = Position::startpos();
        let mut root_wrap = NNUEEvaluatorWrapper::new_with_single_net_for_test(net.clone());
        root_wrap.set_position(&root_pos);

        // Snapshot at split point
        let acc0 = root_wrap.snapshot_single().expect("acc at root must exist");

        // Parallel workers
        super::reset_single_fallback_hits();
        scope(|s| {
            for seed in [1u64, 2, 3, 4] {
                let net_cl = net.clone();
                let acc_cl = acc0.clone();
                let pos0 = root_pos.clone();
                s.spawn(move |_| {
                    let mut pos = pos0;
                    let net_for_wrap = net_cl.clone();
                    let mut wrap = NNUEEvaluatorWrapper::new_with_single_net_for_test(net_for_wrap);
                    wrap.restore_single_at(&pos, acc_cl);
                    let gen = MoveGenerator::new();
                    let mut rng = rand_xoshiro::Xoshiro128Plus::seed_from_u64(seed);
                    for _ in 0..16 {
                        let moves = gen.generate_all(&pos).unwrap_or_default();
                        if moves.is_empty() {
                            break;
                        }
                        let mv = moves[(rng.next_u32() as usize) % moves.len()];
                        let _ = wrap.do_move(&pos, mv);
                        let u = pos.do_move(mv);

                        // equality between wrapper eval (acc), snapshot-acc eval and full eval
                        let s_w = wrap.evaluate(&pos);
                        let s_d = wrap
                            .snapshot_single()
                            .map(|acc| {
                                net_cl.evaluate_from_accumulator_pre(acc.acc_for(pos.side_to_move))
                            })
                            .unwrap_or_else(|| net_cl.evaluate(&pos));
                        let s_full = net_cl.evaluate(&pos);
                        assert_eq!(s_w, s_d);
                        assert_eq!(s_w, s_full);

                        pos.undo_move(mv, u);
                        wrap.undo_move();
                    }
                });
            }
        })
        .expect("threads joined");

        // NOTE: 他テストと並列実行されるため、グローバルカウンタの厳密値は検証しない。
        // 本スモークでは acc 経由評価とフル評価の一致のみを確認する。
    }

    #[test]
    fn test_single_refresh_with_no_bias_b0_none() {
        use crate::usi::parse_usi_square;

        let n_feat = crate::shogi::SHOGI_BOARD_SIZE * super::features::FE_END;
        let d = 4usize;
        let net = super::single::SingleChannelNet {
            n_feat,
            acc_dim: d,
            scale: 600.0,
            w0: vec![0.1; n_feat * d], // small non-zero
            b0: None,                  // no bias
            w2: vec![1.0; d],
            b2: 0.0,
            uid: 21,
        };

        let mut pos = Position::empty();
        // Place kings only
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));

        let acc = super::single_state::SingleAcc::refresh(&pos, &net);
        let s_acc = net.evaluate_from_accumulator_pre(acc.acc_for(pos.side_to_move));
        let s_dir = net.evaluate(&pos);
        assert_eq!(s_acc, s_dir);
    }

    #[test]
    fn test_single_restore_mismatched_uid_triggers_refresh() {
        // two nets with same shape but different uid
        let n_feat = crate::shogi::SHOGI_BOARD_SIZE * super::features::FE_END;
        let d = 4usize;
        let net1 = super::single::SingleChannelNet {
            n_feat,
            acc_dim: d,
            scale: 600.0,
            w0: vec![0.2; n_feat * d],
            b0: Some(vec![0.01; d]),
            w2: vec![1.0; d],
            b2: 0.0,
            uid: 31,
        };
        let net2 = super::single::SingleChannelNet {
            uid: 32,
            ..net1.clone()
        };
        let pos = Position::startpos();

        // Build wrapper with net1
        let mut wrapper = NNUEEvaluatorWrapper::new_with_single_net_for_test(net1.clone());
        wrapper.set_position(&pos);

        // Make acc from net2 (mismatched uid)
        let acc2 = super::single_state::SingleAcc::refresh(&pos, &net2);
        // Restore into wrapper - must refresh to net1 uid
        wrapper.restore_single_at(&pos, acc2);
        let acc_after = wrapper.snapshot_single().expect("acc present");
        assert_eq!(acc_after.weights_uid, net1.uid);

        // Evaluation must equal direct
        let s_w = wrapper.evaluate(&pos);
        let s_d = net1.evaluate(&pos);
        assert_eq!(s_w, s_d);
    }

    #[test]
    fn test_single_eval_clamps_to_bounds() {
        // Design a tiny net that saturates beyond clip
        let d = 1usize;
        let n_feat = crate::shogi::SHOGI_BOARD_SIZE * super::features::FE_END;
        let net = super::single::SingleChannelNet {
            n_feat,
            acc_dim: d,
            scale: 600.0,
            w0: vec![0.0; n_feat * d], // no feature contribution
            b0: Some(vec![1.0; d]),    // pre = 1.0
            w2: vec![100000.0; d],     // large weight to saturate
            b2: 0.0,
            uid: 41,
        };
        let pos = Position::startpos();
        let acc = super::single_state::SingleAcc::refresh(&pos, &net);
        let s = net.evaluate_from_accumulator_pre(acc.acc_for(pos.side_to_move));
        assert_eq!(s, 32000);
    }
}
