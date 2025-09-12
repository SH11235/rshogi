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

#[cfg(test)]
static CLASSIC_FALLBACK_HITS: std::sync::atomic::AtomicUsize =
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
                // 単スレ探索（on_do/undo 経路）では tracked_hash=None とし、acc が常に同期済み
                // 並列探索（HookSuppressor 経路）では tracked_hash=Some(root_hash) で root 以外は不一致→フル評価へフォールバック
                let use_acc = match self.tracked_hash {
                    None => true,
                    Some(h) => h == pos.zobrist_hash,
                };

                if use_acc {
                    if let Some(acc) = accumulator_stack.last() {
                        return evaluator.evaluate_with_accumulator(pos, acc);
                    } else {
                        #[cfg(debug_assertions)]
                        warn!("[NNUE] Empty accumulator stack");
                        // fall through to full rebuild
                    }
                }

                // フォールバック: 一時 Accumulator を構築してフル評価
                #[cfg(test)]
                {
                    CLASSIC_FALLBACK_HITS
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                let mut tmp = accumulator::Accumulator::new();
                tmp.refresh(pos, evaluator.feature_transformer());
                evaluator.evaluate_with_accumulator(pos, &tmp)
            }
            Backend::Single { net, .. } => {
                #[cfg(feature = "nnue_single_diff")]
                if let Backend::Single { acc_stack, .. } = &self.backend {
                    let use_acc = match self.tracked_hash {
                        None => true,
                        Some(h) => h == pos.zobrist_hash,
                    };
                    if use_acc {
                        if let Some(acc) = acc_stack.last() {
                            return net.evaluate_from_accumulator(acc.acc_for(pos.side_to_move));
                        }
                    }
                }
                // フォールバック：同期が取れていない場合や acc 不在時はフル評価
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
                net: _net,
                #[cfg(feature = "nnue_single_diff")]
                acc_stack,
            } => {
                #[cfg(feature = "nnue_single_diff")]
                {
                    acc_stack.clear();
                    acc_stack.push(single_state::SingleAcc::refresh(pos, _net));
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
            Backend::Single { net, #[cfg(feature = "nnue_single_diff")] acc_stack } => {
                #[cfg(feature = "nnue_single_diff")]
                {
                    let current = acc_stack.last().cloned();
                    if let Some(cur) = current {
                        let next = single_state::SingleAcc::apply_update(&cur, pos, mv, net);
                        acc_stack.push(next);
                    } else {
                        acc_stack.push(single_state::SingleAcc::refresh(pos, net));
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
                if accumulator_stack.len() > 1 {
                    accumulator_stack.pop();
                }
                self.tracked_hash = None;
            }
            Backend::Single { #[cfg(feature = "nnue_single_diff")] acc_stack, .. } => {
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
        // trivial net: w0=0, b0=0.2, w2=1, b2=0
        let net = super::single::SingleChannelNet {
            n_feat,
            acc_dim: d,
            scale: 600.0,
            w0: vec![0.0; n_feat * d],
            b0: Some(vec![0.2; d]),
            w2: vec![1.0; d],
            b2: 0.0,
        };

        let mut pos = Position::startpos();
        let acc0 = super::single_state::SingleAcc::refresh(&pos, &net);

        // one legal pawn move 7g7f (3g3f in USI coords)
        let mv = Move::make_normal(parse_usi_square("3g").unwrap(), parse_usi_square("3f").unwrap());
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
}
