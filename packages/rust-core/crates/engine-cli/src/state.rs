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

    /// Transition to searching state if allowed
    pub fn try_start_search(&mut self) -> bool {
        if self.can_start_search() {
            *self = SearchState::Searching;
            true
        } else {
            false
        }
    }

    /// Transition to stop requested state if searching
    pub fn request_stop(&mut self) -> bool {
        if self.is_searching() {
            *self = SearchState::StopRequested;
            true
        } else {
            false
        }
    }

    /// Transition to idle state
    pub fn set_idle(&mut self) {
        *self = SearchState::Idle;
    }
}
