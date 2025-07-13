//! NNUE (Efficiently Updatable Neural Network) evaluation function
//!
//! Implements HalfKP 256x2-32-32 architecture with incremental updates

pub mod accumulator;
pub mod features;
pub mod network;
pub mod weights;

use super::board::{Color, Position};
use super::evaluate::Evaluator;
use accumulator::Accumulator;
use network::Network;
use std::error::Error;
use std::sync::Arc;
use weights::load_weights;

/// Scale factor for converting network output to centipawns
const FV_SCALE: i32 = 16;

/// NNUE evaluator with HalfKP features
pub struct NNUEEvaluator {
    /// Feature transformer for converting HalfKP features to network input
    feature_transformer: Arc<features::FeatureTransformer>,
    /// Neural network for evaluation
    network: Network,
}

impl NNUEEvaluator {
    /// Create new NNUE evaluator from weights file
    pub fn from_file(path: &str) -> Result<Self, Box<dyn Error>> {
        let (feature_transformer, network) = load_weights(path)?;
        Ok(NNUEEvaluator {
            feature_transformer: Arc::new(feature_transformer),
            network,
        })
    }

    /// Create new NNUE evaluator with zero weights (for testing)
    pub fn zero() -> Self {
        NNUEEvaluator {
            feature_transformer: Arc::new(features::FeatureTransformer::zero()),
            network: Network::zero(),
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
    pub fn do_move(&mut self, pos: &Position, mv: super::moves::Move) {
        let current_acc = self.accumulator_stack.last().unwrap();
        let mut new_acc = current_acc.clone();

        // Calculate differential update
        let update = accumulator::calculate_update(pos, mv);

        // Update both perspectives
        new_acc.update(&update, Color::Black, &self.evaluator.feature_transformer);
        new_acc.update(&update, Color::White, &self.evaluator.feature_transformer);

        self.accumulator_stack.push(new_acc);
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
        let accumulator = self.accumulator_stack.last().unwrap();
        self.evaluator.evaluate_with_accumulator(pos, accumulator)
    }
}

#[cfg(test)]
mod tests {
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
}
