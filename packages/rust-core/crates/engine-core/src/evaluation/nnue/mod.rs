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

#[cfg(debug_assertions)]
use log::warn;

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

impl NNUEEvaluatorWrapper {
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
                let accumulator = match accumulator_stack.last() {
                    Some(acc) => acc,
                    None => {
                        #[cfg(debug_assertions)]
                        warn!("[NNUE] Empty accumulator stack");
                        return 0;
                    }
                };
                evaluator.evaluate_with_accumulator(pos, accumulator)
            }
            Backend::Single { net, .. } => {
                #[cfg(feature = "nnue_single_diff")]
                if let Backend::Single { acc_stack, .. } = &self.backend {
                    if let (Some(acc), Some(h)) = (acc_stack.last(), self.tracked_hash) {
                        if h == pos.zobrist_hash {
                            return net.evaluate_from_accumulator(acc.as_slice());
                        }
                    }
                }
                // フォールバック：同期が取れていない場合は常にフル評価
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
            Backend::Single { .. } => {
                // まだ差分更新は未実装。評価時はフルにフォールバックさせる。
                let _ = (pos, mv); // unused guard
                self.tracked_hash = None;
                Ok(())
            }
        }
    }

    /// Hook: undo_move（増分戻し）。Single は未対応のため安全側にフォールバック。
    pub fn undo_move(&mut self) {
        match &mut self.backend {
            Backend::Classic {
                accumulator_stack, ..
            } => {
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
            b2: 0.0,
        };

        // Start position with both kings
        let mut pos = Position::startpos();

        // Black to move
        pos.side_to_move = Color::Black;
        let acc_b = super::single_state::SingleAcc::refresh(&pos, &net);
        let eval_full_b = net.evaluate(&pos);
        let eval_acc_b = net.evaluate_from_accumulator(acc_b.as_slice());
        assert_eq!(eval_full_b, eval_acc_b);

        // White to move (flip path)
        pos.side_to_move = Color::White;
        let acc_w = super::single_state::SingleAcc::refresh(&pos, &net);
        let eval_full_w = net.evaluate(&pos);
        let eval_acc_w = net.evaluate_from_accumulator(acc_w.as_slice());
        assert_eq!(eval_full_w, eval_acc_w);
    }
}
