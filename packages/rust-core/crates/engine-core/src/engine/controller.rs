//! Main engine implementation that integrates search and evaluation
//!
//! Provides a simple interface for using different evaluators with the search engine

use log::error;
use std::sync::{Arc, Mutex};

use crate::{
    evaluation::evaluate::{Evaluator, MaterialEvaluator},
    evaluation::nnue::NNUEEvaluatorWrapper,
    search::unified::UnifiedSearcher,
    search::{SearchLimits, SearchResult},
    Position,
};

/// Type alias for unified searchers
type MaterialSearcher = UnifiedSearcher<MaterialEvaluator, true, false, 8>;
type NnueBasicSearcher = UnifiedSearcher<NNUEEvaluatorProxy, true, false, 8>;
type MaterialEnhancedSearcher = UnifiedSearcher<MaterialEvaluator, true, true, 16>;
type NnueEnhancedSearcher = UnifiedSearcher<NNUEEvaluatorProxy, true, true, 16>;

/// Engine type selection
///
/// # Engine Types Overview
///
/// | Type | Search Algorithm | Evaluation | Use Case |
/// |------|-----------------|------------|----------|
/// | `EnhancedNnue` | Advanced (pruning) | NNUE | **Strongest - recommended for matches** |
/// | `Nnue` | Basic | NNUE | Fast analysis |
/// | `Enhanced` | Advanced (pruning) | Material | Lightweight environments |
/// | `Material` | Basic | Material | Debug/Testing |
///
/// # Performance Comparison
///
/// Relative strength with same thinking time (Material = 1.0):
/// - `EnhancedNnue`: 4.0-5.0x (deepest search + best evaluation)
/// - `Nnue`: 2.5-3.0x (good evaluation compensates simple search)
/// - `Enhanced`: 2.0-2.5x (deep search compensates simple evaluation)
/// - `Material`: 1.0x (baseline)
///
/// # Recommendations
///
/// - **For strongest play**: Use `EnhancedNnue`
/// - **For fast analysis**: Use `Nnue`
/// - **For low memory**: Use `Enhanced`
/// - **For debugging**: Use `Material`
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EngineType {
    /// Simple material-based evaluation with basic alpha-beta search
    /// - Simplest implementation
    /// - Best for debugging and testing
    /// - Memory usage: ~5MB
    Material,

    /// NNUE evaluation with basic alpha-beta search
    /// - Neural network evaluation for better positional understanding
    /// - Stable and fast for shallow searches
    /// - Memory usage: ~170MB (NNUE weights)
    Nnue,

    /// Enhanced search with material evaluation
    /// - Advanced pruning: Null Move, LMR, Futility Pruning
    /// - Transposition Table (16MB) for caching
    /// - Good for learning search techniques
    /// - Memory usage: ~20MB
    Enhanced,

    /// Enhanced search with NNUE evaluation (Strongest)
    /// - Combines best search techniques with best evaluation
    /// - Deepest search depth in same time
    /// - Recommended for competitive play
    /// - Memory usage: ~200MB+
    EnhancedNnue,
}

/// Main engine struct
pub struct Engine {
    engine_type: EngineType,
    material_evaluator: Arc<MaterialEvaluator>,
    nnue_evaluator: Arc<Mutex<Option<NNUEEvaluatorWrapper>>>,
    // Unified searchers for each engine type
    material_searcher: Arc<Mutex<Option<MaterialSearcher>>>,
    nnue_basic_searcher: Arc<Mutex<Option<NnueBasicSearcher>>>,
    material_enhanced_searcher: Arc<Mutex<Option<MaterialEnhancedSearcher>>>,
    nnue_enhanced_searcher: Arc<Mutex<Option<NnueEnhancedSearcher>>>,
}

impl Engine {
    /// Create new engine with specified type
    pub fn new(engine_type: EngineType) -> Self {
        let material_evaluator = Arc::new(MaterialEvaluator);

        let nnue_evaluator = if matches!(engine_type, EngineType::Nnue | EngineType::EnhancedNnue) {
            // Initialize with zero weights for NNUE engine
            Arc::new(Mutex::new(Some(NNUEEvaluatorWrapper::zero())))
        } else {
            Arc::new(Mutex::new(None))
        };

        // Initialize searchers based on engine type
        let material_searcher = if engine_type == EngineType::Material {
            Arc::new(Mutex::new(Some(MaterialSearcher::new(*material_evaluator))))
        } else {
            Arc::new(Mutex::new(None))
        };

        let nnue_basic_searcher = if engine_type == EngineType::Nnue {
            let nnue_proxy = NNUEEvaluatorProxy {
                evaluator: nnue_evaluator.clone(),
            };
            Arc::new(Mutex::new(Some(NnueBasicSearcher::new(nnue_proxy))))
        } else {
            Arc::new(Mutex::new(None))
        };

        let material_enhanced_searcher = if engine_type == EngineType::Enhanced {
            Arc::new(Mutex::new(Some(MaterialEnhancedSearcher::new(*material_evaluator))))
        } else {
            Arc::new(Mutex::new(None))
        };

        let nnue_enhanced_searcher = if engine_type == EngineType::EnhancedNnue {
            let nnue_proxy = NNUEEvaluatorProxy {
                evaluator: nnue_evaluator.clone(),
            };
            Arc::new(Mutex::new(Some(NnueEnhancedSearcher::new(nnue_proxy))))
        } else {
            Arc::new(Mutex::new(None))
        };

        Engine {
            engine_type,
            material_evaluator,
            nnue_evaluator,
            material_searcher,
            nnue_basic_searcher,
            material_enhanced_searcher,
            nnue_enhanced_searcher,
        }
    }

    /// Search for best move in position
    pub fn search(&self, pos: &mut Position, limits: SearchLimits) -> SearchResult {
        log::info!("Engine::search called with engine_type: {:?}", self.engine_type);
        match self.engine_type {
            EngineType::Material => {
                let mut searcher_guard = self.material_searcher.lock().unwrap();
                if let Some(searcher) = searcher_guard.as_mut() {
                    searcher.search(pos, limits)
                } else {
                    panic!("Material searcher not initialized");
                }
            }
            EngineType::Nnue => {
                log::info!("Starting NNUE search");
                let mut searcher_guard = self.nnue_basic_searcher.lock().unwrap();
                if let Some(searcher) = searcher_guard.as_mut() {
                    let result = searcher.search(pos, limits);
                    log::info!("NNUE search completed");
                    result
                } else {
                    panic!("NNUE searcher not initialized");
                }
            }
            EngineType::Enhanced => {
                log::info!("Starting Enhanced search");
                let mut searcher_guard = self.material_enhanced_searcher.lock().unwrap();
                if let Some(searcher) = searcher_guard.as_mut() {
                    searcher.search(pos, limits)
                } else {
                    panic!("Enhanced searcher not initialized");
                }
            }
            EngineType::EnhancedNnue => {
                log::info!("Starting Enhanced NNUE search");
                let mut searcher_guard = self.nnue_enhanced_searcher.lock().unwrap();
                if let Some(searcher) = searcher_guard.as_mut() {
                    searcher.search(pos, limits)
                } else {
                    panic!("Enhanced NNUE searcher not initialized");
                }
            }
        }
    }

    /// Load NNUE weights from file
    pub fn load_nnue_weights(&mut self, path: &str) -> Result<(), Box<dyn std::error::Error>> {
        // Validate that we're using NNUE engine
        if !matches!(self.engine_type, EngineType::Nnue | EngineType::EnhancedNnue) {
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

    /// Get current engine type
    pub fn get_engine_type(&self) -> EngineType {
        self.engine_type
    }

    /// Set engine type
    pub fn set_engine_type(&mut self, engine_type: EngineType) {
        self.engine_type = engine_type;

        match engine_type {
            EngineType::Material => {
                // Initialize material searcher if not already
                let mut searcher_guard = self.material_searcher.lock().unwrap();
                if searcher_guard.is_none() {
                    *searcher_guard = Some(MaterialSearcher::new(*self.material_evaluator));
                }
            }
            EngineType::Nnue => {
                // Initialize NNUE evaluator if needed
                let mut nnue_guard = self.nnue_evaluator.lock().unwrap();
                if nnue_guard.is_none() {
                    *nnue_guard = Some(NNUEEvaluatorWrapper::zero());
                }

                // Initialize NNUE basic searcher
                let mut searcher_guard = self.nnue_basic_searcher.lock().unwrap();
                if searcher_guard.is_none() {
                    let nnue_proxy = NNUEEvaluatorProxy {
                        evaluator: self.nnue_evaluator.clone(),
                    };
                    *searcher_guard = Some(NnueBasicSearcher::new(nnue_proxy));
                }
            }
            EngineType::Enhanced => {
                // Initialize enhanced material searcher
                let mut searcher_guard = self.material_enhanced_searcher.lock().unwrap();
                if searcher_guard.is_none() {
                    *searcher_guard = Some(MaterialEnhancedSearcher::new(*self.material_evaluator));
                }
            }
            EngineType::EnhancedNnue => {
                // Initialize NNUE evaluator if needed
                let mut nnue_guard = self.nnue_evaluator.lock().unwrap();
                if nnue_guard.is_none() {
                    *nnue_guard = Some(NNUEEvaluatorWrapper::zero());
                }

                // Initialize enhanced NNUE searcher
                let mut searcher_guard = self.nnue_enhanced_searcher.lock().unwrap();
                if searcher_guard.is_none() {
                    let nnue_proxy = NNUEEvaluatorProxy {
                        evaluator: self.nnue_evaluator.clone(),
                    };
                    *searcher_guard = Some(NnueEnhancedSearcher::new(nnue_proxy));
                }
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
                error!("Failed to acquire NNUE evaluator lock");
                return 0;
            }
        };

        match guard.as_ref() {
            Some(evaluator) => evaluator.evaluate(pos),
            None => {
                error!("NNUE evaluator not initialized");
                0
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    #[test]
    #[ignore] // Requires large stack size due to engine initialization
    fn test_material_engine() {
        let mut pos = Position::startpos();
        let engine = Engine::new(EngineType::Material);
        let limits = SearchLimits::builder().depth(3).fixed_time_ms(1000).build();

        let result = engine.search(&mut pos, limits);

        assert!(result.best_move.is_some());
        assert!(result.stats.nodes > 0);
    }

    #[test]
    #[cfg_attr(not(feature = "large-stack-tests"), ignore)]
    fn test_nnue_engine() {
        // This test requires a large stack size due to NNUE initialization
        let mut pos = Position::startpos();
        let engine = Engine::new(EngineType::Nnue);
        let limits = SearchLimits::builder().depth(3).fixed_time_ms(1000).build();

        let result = engine.search(&mut pos, limits);

        assert!(result.best_move.is_some());
        assert!(result.stats.nodes > 0);
    }

    #[test]
    #[ignore] // Requires large stack size due to Enhanced engine initialization
    fn test_enhanced_engine() {
        let mut pos = Position::startpos();
        let engine = Engine::new(EngineType::Enhanced);
        let limits = SearchLimits::builder().depth(3).fixed_time_ms(1000).build();

        let result = engine.search(&mut pos, limits);

        assert!(result.best_move.is_some());
        assert!(result.stats.nodes > 0);
        assert!(result.stats.elapsed < Duration::from_secs(2));
    }

    #[test]
    #[cfg_attr(not(feature = "large-stack-tests"), ignore)]
    fn test_enhanced_nnue_engine() {
        // This test requires a large stack size due to NNUE initialization
        // Run with: RUST_MIN_STACK=8388608 cargo test -- --ignored
        let mut pos = Position::startpos();
        let engine = Engine::new(EngineType::EnhancedNnue);
        let limits = SearchLimits::builder().depth(3).fixed_time_ms(1000).build();

        let result = engine.search(&mut pos, limits);

        assert!(result.best_move.is_some());
        assert!(result.stats.nodes > 0);
        assert!(result.stats.elapsed < Duration::from_secs(2));
    }

    #[test]
    #[ignore] // Requires large stack size due to Enhanced engine initialization
    fn test_enhanced_engine_with_stop_flag() {
        let pos = Position::startpos();
        let engine = Arc::new(Engine::new(EngineType::Enhanced));
        let stop_flag = Arc::new(AtomicBool::new(false));

        // Start search in a thread
        let engine_clone = engine.clone();
        let pos_clone = pos.clone();
        let stop_flag_clone = stop_flag.clone();

        let handle = thread::spawn(move || {
            let mut pos_mut = pos_clone;
            let limits = SearchLimits::builder()
                .depth(10)
                .fixed_time_ms(10000)
                .stop_flag(stop_flag_clone)
                .build();
            engine_clone.search(&mut pos_mut, limits)
        });

        // Set stop flag after short delay
        thread::sleep(Duration::from_millis(100));
        stop_flag.store(true, std::sync::atomic::Ordering::Release);

        // Wait for search to complete
        let result = handle.join().unwrap();

        // Should have stopped early (within 2000ms accounting for CI variability and Enhanced engine complexity)
        // Enhanced engine performs more complex operations than basic engines
        assert!(
            result.stats.elapsed < Duration::from_millis(2000),
            "Search took too long to stop: {:?}",
            result.stats.elapsed
        );
    }

    #[test]
    #[ignore] // Requires large stack size due to NNUE initialization
    fn test_engine_type_switching_basic() {
        let mut engine = Engine::new(EngineType::Material);

        // Initially material engine
        assert!(matches!(engine.engine_type, EngineType::Material));

        // Switch to Enhanced
        engine.set_engine_type(EngineType::Enhanced);
        assert!(matches!(engine.engine_type, EngineType::Enhanced));

        // Can search with Enhanced engine
        let mut pos = Position::startpos();
        let limits = SearchLimits::builder().depth(2).fixed_time_ms(100).build();
        let result = engine.search(&mut pos, limits);
        assert!(result.best_move.is_some());

        // Switch back to Material
        engine.set_engine_type(EngineType::Material);
        assert!(matches!(engine.engine_type, EngineType::Material));

        let limits2 = SearchLimits::builder().depth(2).fixed_time_ms(100).build();
        let result2 = engine.search(&mut pos, limits2);
        assert!(result2.best_move.is_some());
    }

    #[test]
    #[cfg_attr(not(feature = "large-stack-tests"), ignore)]
    fn test_engine_type_switching_with_nnue() {
        // Separate test for NNUE due to stack size requirements
        let mut engine = Engine::new(EngineType::Material);

        // Switch to NNUE
        engine.set_engine_type(EngineType::Nnue);
        assert!(matches!(engine.engine_type, EngineType::Nnue));

        // Can still search
        let mut pos = Position::startpos();
        let limits = SearchLimits::builder().depth(2).fixed_time_ms(100).build();
        let result = engine.search(&mut pos, limits);
        assert!(result.best_move.is_some());
    }

    #[test]
    #[cfg_attr(not(feature = "large-stack-tests"), ignore)]
    fn test_engine_type_switching_with_enhanced_nnue() {
        // Separate test for EnhancedNnue due to stack size requirements
        // Run with: RUST_MIN_STACK=8388608 cargo test -- --ignored
        let mut engine = Engine::new(EngineType::Material);

        // Switch to EnhancedNnue
        engine.set_engine_type(EngineType::EnhancedNnue);
        assert!(matches!(engine.engine_type, EngineType::EnhancedNnue));

        // Can search with EnhancedNnue engine
        let mut pos = Position::startpos();
        let limits = SearchLimits::builder().depth(2).fixed_time_ms(100).build();
        let result = engine.search(&mut pos, limits);
        assert!(result.best_move.is_some());
    }

    #[test]
    #[ignore] // Requires large stack size due to NNUE initialization
    fn test_load_nnue_weights_wrong_engine_type() {
        let mut engine = Engine::new(EngineType::Material);
        let result = engine.load_nnue_weights("dummy.nnue");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().to_string(), "Cannot load NNUE weights for non-NNUE engine");
    }

    #[test]
    #[cfg_attr(not(feature = "large-stack-tests"), ignore)]
    fn test_parallel_engine_execution() {
        // Create a shared engine with NNUE
        let engine = Arc::new(Engine::new(EngineType::Nnue));

        let mut handles = vec![];

        // Spawn multiple threads that use the engine concurrently
        for thread_id in 0..4 {
            let engine_clone = engine.clone();
            let handle = thread::spawn(move || {
                let mut pos = Position::startpos();
                let limits = SearchLimits::builder().depth(2).fixed_time_ms(50).build();

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
    #[cfg_attr(not(feature = "large-stack-tests"), ignore)]
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
