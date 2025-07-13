//! Main engine implementation that integrates search and evaluation
//!
//! Provides a simple interface for using different evaluators with the search engine

use super::board::Position;
use super::evaluate::MaterialEvaluator;
use super::nnue::NNUEEvaluatorWrapper;
use super::search::{SearchLimits, SearchResult, Searcher};
use std::sync::Arc;

/// Engine type selection
#[derive(Clone, Copy, Debug)]
pub enum EngineType {
    /// Simple material-based evaluation
    Material,
    /// NNUE evaluation
    Nnue,
}

/// Main engine struct
pub struct Engine {
    engine_type: EngineType,
}

impl Engine {
    /// Create new engine with specified type
    pub fn new(engine_type: EngineType) -> Self {
        Engine { engine_type }
    }

    /// Search for best move in position
    pub fn search(&self, pos: &mut Position, limits: SearchLimits) -> SearchResult {
        match self.engine_type {
            EngineType::Material => {
                let evaluator = Arc::new(MaterialEvaluator);
                let mut searcher = Searcher::new(limits, evaluator);
                searcher.search(pos)
            }
            EngineType::Nnue => {
                // For now, use default NNUE weights (zero-initialized)
                // In production, load from file
                let evaluator = Arc::new(NNUEEvaluatorWrapper::zero());
                let mut searcher = Searcher::new(limits, evaluator);
                searcher.search(pos)
            }
        }
    }

    /// Load NNUE weights from file
    pub fn load_nnue_weights(&mut self, _path: &str) -> Result<(), Box<dyn std::error::Error>> {
        // TODO: Implement weight loading and store in engine
        // For now, just validate that we're using NNUE engine
        if !matches!(self.engine_type, EngineType::Nnue) {
            return Err("Cannot load NNUE weights for non-NNUE engine".into());
        }

        // In a real implementation, we would:
        // 1. Load weights using nnue::weights::load_weights
        // 2. Store the evaluator in the engine
        // 3. Use it in search()

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_material_engine() {
        let mut pos = Position::startpos();
        let engine = Engine::new(EngineType::Material);
        let limits = SearchLimits {
            depth: 3,
            time: Some(Duration::from_secs(1)),
            nodes: None,
        };

        let result = engine.search(&mut pos, limits);

        assert!(result.best_move.is_some());
        assert!(result.stats.nodes > 0);
    }

    #[test]
    fn test_nnue_engine() {
        let mut pos = Position::startpos();
        let engine = Engine::new(EngineType::Nnue);
        let limits = SearchLimits {
            depth: 3,
            time: Some(Duration::from_secs(1)),
            nodes: None,
        };

        let result = engine.search(&mut pos, limits);

        assert!(result.best_move.is_some());
        assert!(result.stats.nodes > 0);
    }
}
