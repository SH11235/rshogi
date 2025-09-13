//! NNUE (Efficiently Updatable Neural Network) evaluation function
//!
//! Implements HalfKP 256x2-32-32 architecture with incremental updates
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
#[cfg(feature = "nnue_single_diff")]
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

/// Scale factor for converting network output to centipawns
///
/// The NNUE network outputs values in a higher resolution internal scale.
/// This factor is used to scale up the network output before final normalization.
/// The value 16 is chosen to provide sufficient precision while avoiding overflow.
const FV_SCALE: i32 = 16;

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
    /// Create new NNUE evaluator from weights file
    pub fn from_file(path: &str) -> Result<Self, Box<dyn Error>> {
        let (feature_transformer, network) = load_weights(path)?;
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
enum Backend {
    Classic {
        evaluator: NNUEEvaluator,
        accumulator_stack: Vec<Accumulator>,
    },
    Single {
        net: std::sync::Arc<single::SingleChannelNet>,
        #[cfg(feature = "nnue_single_diff")]
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
    /// 現在の重みを共有しつつ、状態（acc_stack/tracked_hash）のない新インスタンスを作る
    pub fn fork_stateless(&self) -> Self {
        match &self.backend {
            Backend::Classic { evaluator, .. } => {
                Self {
                    backend: Backend::Classic {
                        evaluator: evaluator.clone(),
                        accumulator_stack: Vec::new(),
                    },
                    // 初期状態は None（evaluate はスタック空→フル再構築）
                    tracked_hash: None,
                }
            }
            Backend::Single { net, .. } => Self {
                backend: Backend::Single {
                    net: net.clone(),
                    #[cfg(feature = "nnue_single_diff")]
                    acc_stack: Vec::new(),
                },
                // 初期状態は None（evaluate は acc 不在→フル経路）
                tracked_hash: None,
            },
        }
    }
    /// Create new wrapper from weights file
    pub fn new(weights_path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        if let Ok((ft, net)) = load_weights(weights_path) {
            let evaluator = NNUEEvaluator {
                feature_transformer: std::sync::Arc::new(ft),
                network: std::sync::Arc::new(net),
            };
            let mut initial_acc = Accumulator::new();
            let empty_pos = Position::empty();
            initial_acc.refresh(&empty_pos, &evaluator.feature_transformer);
            return Ok(NNUEEvaluatorWrapper {
                backend: Backend::Classic {
                    evaluator,
                    accumulator_stack: vec![initial_acc],
                },
                tracked_hash: None,
            });
        }
        if let Ok(net) = load_single_weights(weights_path) {
            return Ok(NNUEEvaluatorWrapper {
                backend: Backend::Single {
                    net: std::sync::Arc::new(net),
                    #[cfg(feature = "nnue_single_diff")]
                    acc_stack: Vec::new(),
                },
                tracked_hash: None,
            });
        }
        Err("Failed to load NNUE weights (unsupported format)".into())
    }

    // NOTE: set_position/do_move/undo_move は下部の実装へ集約

    /// Test-only: construct a wrapper with a provided SINGLE net
    #[cfg(test)]
    pub fn new_with_single_net_for_test(net: single::SingleChannelNet) -> Self {
        NNUEEvaluatorWrapper {
            backend: Backend::Single {
                net: std::sync::Arc::new(net),
                #[cfg(feature = "nnue_single_diff")]
                acc_stack: Vec::new(),
            },
            tracked_hash: None,
        }
    }

    /// SINGLE 用: 現在ノードの Acc スナップショットを返す
    #[cfg(feature = "nnue_single_diff")]
    #[must_use]
    pub fn snapshot_single(&self) -> Option<single_state::SingleAcc> {
        if let Backend::Single { acc_stack, .. } = &self.backend {
            acc_stack.last().cloned()
        } else {
            None
        }
    }

    /// SINGLE 用: 指定の Acc を現在ノードとして復元し、tracked_hash を pos に合わせる
    #[cfg(feature = "nnue_single_diff")]
    pub fn restore_single_at(&mut self, pos: &Position, acc: single_state::SingleAcc) {
        if let Backend::Single { net, acc_stack, .. } = &mut self.backend {
            // 開発時の寸止め検証：異なる net 由来の Acc を誤って渡していないか
            debug_assert_eq!(acc.post_black.len(), net.acc_dim);
            debug_assert_eq!(acc.post_white.len(), net.acc_dim);
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
            self.tracked_hash = Some(pos.zobrist_hash);
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
            },
            tracked_hash: None,
        }
    }
}

impl Evaluator for NNUEEvaluatorWrapper {
    fn evaluate(&self, pos: &Position) -> i32 {
        match &self.backend {
            Backend::Classic {
                evaluator,
                accumulator_stack,
            } => {
                // 初期状態（set_position 前）はスタック空のため、常にフル再構築
                if accumulator_stack.is_empty() {
                    let mut tmp = accumulator::Accumulator::new();
                    tmp.refresh(pos, evaluator.feature_transformer());
                    return evaluator.evaluate_with_accumulator(pos, &tmp);
                }

                // 単スレ探索（on_do/undo 経路）では tracked_hash=None とし、acc が常に同期済み
                // 並列探索では tracked_hash=Some(root_hash) で root 以外は不一致→フル評価へフォールバック
                let use_acc = self.tracked_hash.is_none_or(|h| h == pos.zobrist_hash);
                if use_acc {
                    if let Some(acc) = accumulator_stack.last() {
                        return evaluator.evaluate_with_accumulator(pos, acc);
                    }
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
            Backend::Single { net, .. } => {
                #[cfg(feature = "nnue_single_diff")]
                if let Backend::Single { acc_stack, .. } = &self.backend {
                    let use_acc = self.tracked_hash.is_none_or(|h| h == pos.zobrist_hash);
                    if use_acc {
                        if let Some(acc) = acc_stack.last() {
                            return net.evaluate_from_accumulator(acc.acc_for(pos.side_to_move));
                        }
                    }
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
            } => {
                accumulator_stack.clear();
                let mut acc = Accumulator::new();
                acc.refresh(pos, &evaluator.feature_transformer);
                accumulator_stack.push(acc);
                self.tracked_hash = Some(pos.zobrist_hash);
            }
            Backend::Single {
                net,
                #[cfg(feature = "nnue_single_diff")]
                acc_stack,
            } => {
                #[cfg(not(feature = "nnue_single_diff"))]
                let _ = net; // silence unused when feature disabled
                #[cfg(feature = "nnue_single_diff")]
                {
                    acc_stack.clear();
                    acc_stack.push(single_state::SingleAcc::refresh(pos, net));
                }
                self.tracked_hash = Some(pos.zobrist_hash);
            }
        }
    }

    /// Hook: do_move（増分）。Single は未対応のため安全側にフォールバック。
    pub fn do_move(&mut self, pos: &Position, mv: Move) -> NNUEResult<()> {
        match &mut self.backend {
            Backend::Classic {
                evaluator,
                accumulator_stack,
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
                let mut new_acc = current_acc.clone();
                let update = accumulator::calculate_update(pos, mv)?;
                new_acc.update(&update, Color::Black, &evaluator.feature_transformer);
                new_acc.update(&update, Color::White, &evaluator.feature_transformer);
                accumulator_stack.push(new_acc);
                // Classic は子局面に同期済みなので、ハッシュを無効化（安全側）。
                self.tracked_hash = None;
                Ok(())
            }
            Backend::Single {
                net,
                #[cfg(feature = "nnue_single_diff")]
                acc_stack,
            } => {
                #[cfg(not(feature = "nnue_single_diff"))]
                let _ = net; // silence unused when feature disabled
                #[cfg(feature = "nnue_single_diff")]
                {
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
                {
                    if accumulator_stack.is_empty() {
                        debug_assert!(false, "undo_move called with empty accumulator_stack");
                    }
                }
                if accumulator_stack.len() > 1 {
                    accumulator_stack.pop();
                }
                self.tracked_hash = None;
            }
            Backend::Single {
                #[cfg(feature = "nnue_single_diff")]
                acc_stack,
                ..
            } => {
                #[cfg(feature = "nnue_single_diff")]
                {
                    #[cfg(debug_assertions)]
                    {
                        if acc_stack.is_empty() {
                            debug_assert!(false, "undo_move called with empty acc_stack");
                        }
                    }
                    if acc_stack.len() > 1 {
                        acc_stack.pop();
                    }
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
    #[cfg(feature = "nnue_single_diff")]
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
        let eval_acc_b = net.evaluate_from_accumulator(acc_b.acc_for(Color::Black));
        assert_eq!(eval_full_b, eval_acc_b);
        assert_ne!(eval_full_b, 0);

        // White to move (flip path)
        pos.side_to_move = Color::White;
        let acc_w = super::single_state::SingleAcc::refresh(&pos, &net);
        let eval_full_w = net.evaluate(&pos);
        let eval_acc_w = net.evaluate_from_accumulator(acc_w.acc_for(Color::White));
        assert_eq!(eval_full_w, eval_acc_w);
        assert_ne!(eval_full_w, 0);
    }

    #[test]
    #[cfg(feature = "nnue_single_diff")]
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
        let eval_acc = net.evaluate_from_accumulator(acc1.acc_for(pos.side_to_move));
        // eval via full refresh
        let acc_full = super::single_state::SingleAcc::refresh(&pos, &net);
        let eval_full = net.evaluate_from_accumulator(acc_full.acc_for(pos.side_to_move));
        assert_eq!(eval_acc, eval_full);

        pos.undo_move(mv, undo);
    }

    #[test]
    #[cfg(feature = "nnue_single_diff")]
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

        let eval_acc = net.evaluate_from_accumulator(acc1.acc_for(pos.side_to_move));
        let acc_full = super::single_state::SingleAcc::refresh(&pos, &net);
        let eval_full = net.evaluate_from_accumulator(acc_full.acc_for(pos.side_to_move));
        assert_eq!(eval_acc, eval_full);

        pos.undo_move(mv, undo);
    }

    #[test]
    #[cfg(feature = "nnue_single_diff")]
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

        let eval_acc = net.evaluate_from_accumulator(acc1.acc_for(pos.side_to_move));
        let acc_full = super::single_state::SingleAcc::refresh(&pos, &net);
        let eval_full = net.evaluate_from_accumulator(acc_full.acc_for(pos.side_to_move));
        assert_eq!(eval_acc, eval_full);

        pos.undo_move(mv, undo);
    }

    #[test]
    #[cfg(feature = "nnue_single_diff")]
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

        let eval_acc = net.evaluate_from_accumulator(acc1.acc_for(pos.side_to_move));
        let acc_full = super::single_state::SingleAcc::refresh(&pos, &net);
        let eval_full = net.evaluate_from_accumulator(acc_full.acc_for(pos.side_to_move));
        assert_eq!(eval_acc, eval_full);

        pos.undo_move(mv, undo);
    }

    #[test]
    #[cfg(feature = "nnue_single_diff")]
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
        let eval_acc = net.evaluate_from_accumulator(acc1.acc_for(pos.side_to_move));
        let acc_full = super::single_state::SingleAcc::refresh(&pos, &net);
        let eval_full = net.evaluate_from_accumulator(acc_full.acc_for(pos.side_to_move));
        assert_eq!(eval_acc, eval_full);
        pos.undo_move(mv, undo);
    }

    #[test]
    #[cfg(feature = "nnue_single_diff")]
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
        let eval_acc = net.evaluate_from_accumulator(acc2.acc_for(pos.side_to_move));
        let acc_full = super::single_state::SingleAcc::refresh(&pos, &net);
        let eval_full = net.evaluate_from_accumulator(acc_full.acc_for(pos.side_to_move));
        assert_eq!(eval_acc, eval_full);
    }

    #[test]
    #[cfg(feature = "nnue_single_diff")]
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

        let eval_acc = net.evaluate_from_accumulator(acc1.acc_for(pos.side_to_move));
        let acc_full = super::single_state::SingleAcc::refresh(&pos, &net);
        let eval_full = net.evaluate_from_accumulator(acc_full.acc_for(pos.side_to_move));
        assert_eq!(eval_acc, eval_full);

        pos.undo_move(mv, undo);
    }

    #[test]
    #[cfg(feature = "nnue_single_diff")]
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
    #[cfg(feature = "nnue_single_diff")]
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
        let eval_acc1 = net.evaluate_from_accumulator(acc1.acc_for(pos.side_to_move));
        let full1 = super::single_state::SingleAcc::refresh(&pos, &net);
        let eval_full1 = net.evaluate_from_accumulator(full1.acc_for(pos.side_to_move));
        assert_eq!(eval_acc1, eval_full1);

        // 2nd drop (now White to move): 3e (different file to avoid ni-fu rule)
        let mv2 = Move::make_drop(PieceType::Pawn, parse_usi_square("3e").unwrap());
        let acc2 = super::single_state::SingleAcc::apply_update(&acc1, &pos, mv2, &net);
        let u2 = pos.do_move(mv2);
        let eval_acc2 = net.evaluate_from_accumulator(acc2.acc_for(pos.side_to_move));
        let full2 = super::single_state::SingleAcc::refresh(&pos, &net);
        let eval_full2 = net.evaluate_from_accumulator(full2.acc_for(pos.side_to_move));
        assert_eq!(eval_acc2, eval_full2);

        // cleanup
        pos.undo_move(mv2, u2);
        pos.undo_move(mv1, u1);
    }

    #[test]
    #[cfg(feature = "nnue_single_diff")]
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
        let eval_acc = net.evaluate_from_accumulator(acc1.acc_for(pos.side_to_move));
        let full = super::single_state::SingleAcc::refresh(&pos, &net);
        let eval_full = net.evaluate_from_accumulator(full.acc_for(pos.side_to_move));
        assert_eq!(eval_acc, eval_full);
        pos.undo_move(mv, u);
    }

    #[test]
    #[cfg(feature = "nnue_single_diff")]
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
            let s_acc = wrapper
                .snapshot_single()
                .map(|acc| net.evaluate_from_accumulator(acc.acc_for(pos.side_to_move)))
                .unwrap_or_else(|| net.evaluate(&pos));
            let s_dir = net.evaluate(&pos);
            assert_eq!(s_wrap, s_acc);
            assert_eq!(s_wrap, s_dir);
            // フォールバックが使われたことを確認
            assert!(super::single_fallback_hits() > 0);
        } else {
            panic!("expected Single backend");
        }

        pos.undo_move(mv, u);
    }

    #[test]
    #[cfg(feature = "nnue_single_diff")]
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
            let eval_acc = net.evaluate_from_accumulator(next.acc_for(pos.side_to_move));
            let full = super::single_state::SingleAcc::refresh(&pos, &net);
            let eval_full = net.evaluate_from_accumulator(full.acc_for(pos.side_to_move));
            let eval_direct = net.evaluate(&pos);
            assert_eq!(eval_acc, eval_full);
            assert_eq!(eval_acc, eval_direct);

            acc = next;
        }
    }

    #[test]
    #[cfg(feature = "nnue_single_diff")]
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
            .map(|acc| net.evaluate_from_accumulator(acc.acc_for(pos.side_to_move)))
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
    #[cfg(feature = "nnue_single_diff")]
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
        let mut root_pos = Position::startpos();
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
                                net_cl.evaluate_from_accumulator(acc.acc_for(pos.side_to_move))
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
}
