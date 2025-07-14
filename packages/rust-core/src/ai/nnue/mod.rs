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
pub mod weights;

use super::board::{Color, Position};
use super::evaluate::Evaluator;
use accumulator::Accumulator;
use error::{NNUEError, NNUEResult};
use network::Network;
use std::error::Error;
use std::sync::Arc;
use weights::load_weights;

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
pub struct NNUEEvaluatorWrapper {
    evaluator: NNUEEvaluator,
    accumulator_stack: Vec<Accumulator>,
}

impl NNUEEvaluatorWrapper {
    /// Create new wrapper from weights file
    pub fn new(weights_path: &str) -> Result<Self, Box<dyn Error>> {
        let evaluator = NNUEEvaluator::from_file(weights_path)?;
        let mut initial_acc = Accumulator::new();

        // Initialize with empty position
        let empty_pos = Position::empty();
        initial_acc.refresh(&empty_pos, &evaluator.feature_transformer);

        Ok(NNUEEvaluatorWrapper {
            evaluator,
            accumulator_stack: vec![initial_acc],
        })
    }

    /// Update accumulator when making a move
    pub fn do_move(&mut self, pos: &Position, mv: super::moves::Move) -> NNUEResult<()> {
        let current_acc = self.accumulator_stack.last().ok_or(NNUEError::EmptyAccumulatorStack)?;
        let mut new_acc = current_acc.clone();

        // Calculate differential update
        let update = accumulator::calculate_update(pos, mv)?;

        // Update both perspectives
        new_acc.update(&update, Color::Black, &self.evaluator.feature_transformer);
        new_acc.update(&update, Color::White, &self.evaluator.feature_transformer);

        self.accumulator_stack.push(new_acc);
        Ok(())
    }

    /// Undo last move
    pub fn undo_move(&mut self) {
        if self.accumulator_stack.len() > 1 {
            self.accumulator_stack.pop();
        }
    }

    /// Reset to position
    pub fn set_position(&mut self, pos: &Position) {
        self.accumulator_stack.clear();

        let mut acc = Accumulator::new();
        acc.refresh(pos, &self.evaluator.feature_transformer);

        self.accumulator_stack.push(acc);
    }

    /// Create zero-initialized evaluator (for testing)
    pub fn zero() -> Self {
        let evaluator = NNUEEvaluator::zero();
        let mut initial_acc = Accumulator::new();

        // Initialize with empty position
        let empty_pos = Position::empty();
        initial_acc.refresh(&empty_pos, &evaluator.feature_transformer);

        NNUEEvaluatorWrapper {
            evaluator,
            accumulator_stack: vec![initial_acc],
        }
    }
}

impl Evaluator for NNUEEvaluatorWrapper {
    fn evaluate(&self, pos: &Position) -> i32 {
        // Since Evaluator trait doesn't support Result, we need to handle errors internally
        let accumulator = match self.accumulator_stack.last() {
            Some(acc) => acc,
            None => {
                // This should never happen in normal usage - accumulator stack should always have at least one entry
                debug_assert!(false, "Empty accumulator stack in NNUE evaluation");
                // Return 0 evaluation as fallback
                return 0;
            }
        };
        self.evaluator.evaluate_with_accumulator(pos, accumulator)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::board::{Color, Piece, PieceType, Square};
    use crate::ai::moves::Move;

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
            .put_piece(Square::new(4, 4), Piece::new(PieceType::Pawn, Color::Black));

        // Try to make a move - should fail due to missing kings
        let mv = Move::make_normal(Square::new(4, 4), Square::new(4, 3));
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
}
