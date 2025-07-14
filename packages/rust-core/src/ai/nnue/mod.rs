//! NNUE (Efficiently Updatable Neural Network) evaluation function
//!
//! Implements HalfKP 256x2-32-32 architecture with incremental updates

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
const FV_SCALE: i32 = 16;

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
        (output * FV_SCALE) >> 16
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
                // Return 0 evaluation on error - this shouldn't happen in normal usage
                eprintln!("Warning: Empty accumulator stack in NNUE evaluation");
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
    fn test_nnue_evaluator_wrapper_empty_stack() {
        let mut wrapper = NNUEEvaluatorWrapper::zero();
        wrapper.accumulator_stack.clear(); // Force empty stack

        let pos = Position::startpos();
        let eval = wrapper.evaluate(&pos);

        // Should return 0 on error
        assert_eq!(eval, 0);
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
