/// Search state management - tracks the current state of the search
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchState {
    /// No search is active
    Idle,
    /// Search is actively running
    Searching,
    /// Stop has been requested but search is still running
    StopRequested,
}

impl SearchState {
    /// Check if we're in any searching state
    pub fn is_searching(&self) -> bool {
        matches!(self, SearchState::Searching | SearchState::StopRequested)
    }

    /// Check if we can start a new search
    pub fn can_start_search(&self) -> bool {
        matches!(self, SearchState::Idle)
    }

    /// Check if we should accept a bestmove
    pub fn can_accept_bestmove(&self) -> bool {
        matches!(self, SearchState::Searching | SearchState::StopRequested)
    }
}
