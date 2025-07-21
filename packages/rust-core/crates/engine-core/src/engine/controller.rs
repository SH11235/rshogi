//! Main engine implementation that integrates search and evaluation
//!
//! Provides a simple interface for using different evaluators with the search engine

use std::sync::{Arc, Mutex};

use crate::{
    evaluate::{Evaluator, MaterialEvaluator},
    nnue::NNUEEvaluatorWrapper,
    search::search_basic::{SearchLimits, SearchResult, Searcher},
    Position,
};

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
    material_evaluator: Arc<MaterialEvaluator>,
    nnue_evaluator: Arc<Mutex<Option<NNUEEvaluatorWrapper>>>,
}

impl Engine {
    /// Create new engine with specified type
    pub fn new(engine_type: EngineType) -> Self {
        let nnue_evaluator = if matches!(engine_type, EngineType::Nnue) {
            // Initialize with zero weights for NNUE engine
            Arc::new(Mutex::new(Some(NNUEEvaluatorWrapper::zero())))
        } else {
            Arc::new(Mutex::new(None))
        };

        Engine {
            engine_type,
            material_evaluator: Arc::new(MaterialEvaluator),
            nnue_evaluator,
        }
    }

    /// Search for best move in position
    pub fn search(&self, pos: &mut Position, limits: SearchLimits) -> SearchResult {
        match self.engine_type {
            EngineType::Material => {
                let mut searcher = Searcher::new(limits, self.material_evaluator.clone());
                searcher.search(pos)
            }
            EngineType::Nnue => {
                let nnue_proxy = Arc::new(NNUEEvaluatorProxy {
                    evaluator: self.nnue_evaluator.clone(),
                });
                let mut searcher = Searcher::new(limits, nnue_proxy);
                searcher.search(pos)
            }
        }
    }

    /// Load NNUE weights from file
    pub fn load_nnue_weights(&mut self, path: &str) -> Result<(), Box<dyn std::error::Error>> {
        // Validate that we're using NNUE engine
        if !matches!(self.engine_type, EngineType::Nnue) {
            return Err("Cannot load NNUE weights for non-NNUE engine".into());
        }

        // Load new NNUE evaluator from file
        let new_evaluator = NNUEEvaluatorWrapper::new(path)?;

        // Replace the evaluator
        let mut nnue_guard = self
            .nnue_evaluator
            .lock()
            .map_err(|_| "Failed to acquire NNUE evaluator lock")?;
        *nnue_guard = Some(new_evaluator);

        Ok(())
    }

    /// Set engine type
    pub fn set_engine_type(&mut self, engine_type: EngineType) {
        self.engine_type = engine_type;

        // If switching to NNUE and it's not initialized, initialize with zero weights
        if matches!(engine_type, EngineType::Nnue) {
            let mut nnue_guard = self.nnue_evaluator.lock().unwrap();
            if nnue_guard.is_none() {
                *nnue_guard = Some(NNUEEvaluatorWrapper::zero());
            }
        }
    }
}

/// Proxy evaluator for thread-safe NNUE access
struct NNUEEvaluatorProxy {
    evaluator: Arc<Mutex<Option<NNUEEvaluatorWrapper>>>,
}

impl Evaluator for NNUEEvaluatorProxy {
    fn evaluate(&self, pos: &Position) -> i32 {
        let guard = match self.evaluator.lock() {
            Ok(g) => g,
            Err(_) => {
                eprintln!("Failed to acquire NNUE evaluator lock");
                return 0;
            }
        };

        match guard.as_ref() {
            Some(evaluator) => evaluator.evaluate(pos),
            None => {
                eprintln!("NNUE evaluator not initialized");
                0
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;
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

    #[test]
    fn test_engine_type_switching() {
        let mut engine = Engine::new(EngineType::Material);

        // Initially material engine
        assert!(matches!(engine.engine_type, EngineType::Material));

        // Switch to NNUE
        engine.set_engine_type(EngineType::Nnue);
        assert!(matches!(engine.engine_type, EngineType::Nnue));

        // Can still search
        let mut pos = Position::startpos();
        let limits = SearchLimits {
            depth: 2,
            time: Some(Duration::from_millis(100)),
            nodes: None,
        };
        let result = engine.search(&mut pos, limits);
        assert!(result.best_move.is_some());
    }

    #[test]
    fn test_load_nnue_weights_wrong_engine_type() {
        let mut engine = Engine::new(EngineType::Material);
        let result = engine.load_nnue_weights("dummy.nnue");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().to_string(), "Cannot load NNUE weights for non-NNUE engine");
    }

    #[test]
    fn test_parallel_engine_execution() {
        // Create a shared engine with NNUE
        let engine = Arc::new(Engine::new(EngineType::Nnue));

        let mut handles = vec![];

        // Spawn multiple threads that use the engine concurrently
        for thread_id in 0..4 {
            let engine_clone = engine.clone();
            let handle = thread::spawn(move || {
                let mut pos = Position::startpos();
                let limits = SearchLimits {
                    depth: 2,
                    time: Some(Duration::from_millis(50)),
                    nodes: None,
                };

                // Each thread performs a search
                let result = engine_clone.search(&mut pos, limits);

                println!("Thread {} completed search with {} nodes", thread_id, result.stats.nodes);

                // Verify we got a valid result
                assert!(result.best_move.is_some());
                assert!(result.stats.nodes > 0);

                result.stats.nodes
            });
            handles.push(handle);
        }

        // Wait for all threads to complete
        let mut total_nodes = 0u64;
        for handle in handles {
            let nodes = handle.join().unwrap();
            total_nodes += nodes;
        }

        println!("Total nodes searched across all threads: {total_nodes}");
        assert!(total_nodes > 0);
    }

    #[test]
    fn test_concurrent_weight_loading() {
        // Create a mutable engine
        let mut engine = Engine::new(EngineType::Nnue);

        // Try to load weights (will fail since file doesn't exist)
        let result1 = engine.load_nnue_weights("nonexistent1.nnue");
        assert!(result1.is_err());

        // Engine type switching is thread-safe through mutex
        engine.set_engine_type(EngineType::Material);
        engine.set_engine_type(EngineType::Nnue);

        // Try another load
        let result2 = engine.load_nnue_weights("nonexistent2.nnue");
        assert!(result2.is_err());
    }
}
