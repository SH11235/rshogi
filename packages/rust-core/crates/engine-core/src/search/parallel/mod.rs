pub mod stop_ctrl;
pub use stop_ctrl::{FinalizeReason, FinalizerMsg, StopController, StopSnapshot};

// Temporary dummy types to satisfy old references while migrating
use crate::evaluation::evaluate::Evaluator;
use crate::search::tt::TranspositionTable;
use crate::search::{SearchLimits, SearchResult, SearchStats};
use crate::Position;
use std::marker::PhantomData;
use std::sync::Arc;

pub struct SharedSearchState; // placeholder

pub struct ParallelSearcher<E> {
    _e: PhantomData<E>,
}

impl<E> ParallelSearcher<E>
where
    E: Evaluator + Send + Sync + 'static,
{
    pub fn new<T>(
        _evaluator: T,
        _tt: Arc<TranspositionTable>,
        _threads: usize,
        _stop_ctrl: Arc<StopController>,
    ) -> Self {
        Self { _e: PhantomData }
    }
    pub fn adjust_thread_count(&mut self, _threads: usize) {}
    pub fn search(&mut self, _pos: &mut Position, _limits: SearchLimits) -> SearchResult {
        SearchResult::new(None, 0, SearchStats::default())
    }
}
