//! Temporary dummy unified searcher during migration.
//! Provides the minimal API surface so the engine builds while the new
//! search is being implemented.

use crate::search::tt::TranspositionTable;
use crate::search::types::TeacherProfile;
use crate::search::{SearchLimits, SearchResult, SearchStats};
use crate::Position;
use std::marker::PhantomData;
use std::sync::Arc;

pub struct UnifiedSearcher<E, const USE_TT: bool, const USE_PRUNING: bool> {
    _e: PhantomData<E>,
}

impl<E, const USE_TT: bool, const USE_PRUNING: bool> UnifiedSearcher<E, USE_TT, USE_PRUNING> {
    pub fn new(_evaluator: E) -> Self {
        Self { _e: PhantomData }
    }
    pub fn with_shared_tt<T>(_evaluator: T, _tt: Arc<TranspositionTable>) -> Self {
        Self { _e: PhantomData }
    }
    pub fn set_multi_pv(&mut self, _k: u8) {}
    pub fn set_teacher_profile(&mut self, _profile: TeacherProfile) {}
    pub fn reset_history(&mut self) {}
    pub fn search(&mut self, _pos: &mut Position, _limits: SearchLimits) -> SearchResult {
        SearchResult::new(None, 0, SearchStats::default())
    }
}
