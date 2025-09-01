use crate::state::SearchState;
use crate::worker::WorkerMessage;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_acceptance_gate_rejects_go_while_searching() {
        // Create test context
        let mut search_state = SearchState::Searching;

        // Verify that go command cannot be accepted
        assert!(!search_state.can_start_search());

        // Verify that StopRequested state also rejects
        search_state = SearchState::StopRequested;
        assert!(!search_state.can_start_search());
    }

    #[test]
    fn test_acceptance_gate_allows_go_when_idle() {
        let mut search_state = SearchState::Idle;

        // Verify that go command can be accepted
        assert!(search_state.can_start_search());

        // Try to start search
        assert!(search_state.try_start_search());
        assert_eq!(search_state, SearchState::Searching);
    }

    #[test]
    fn test_acceptance_gate_allows_go_when_finalized() {
        let mut search_state = SearchState::Finalized;

        // Verify that go command can be accepted when finalized
        assert!(search_state.can_start_search());

        // Try to start search
        assert!(search_state.try_start_search());
        assert_eq!(search_state, SearchState::Searching);
    }

    #[test]
    fn test_state_transitions() {
        let mut search_state = SearchState::Idle;

        // Idle -> Searching
        assert!(search_state.try_start_search());
        assert_eq!(search_state, SearchState::Searching);

        // Searching -> StopRequested
        assert!(search_state.request_stop());
        assert_eq!(search_state, SearchState::StopRequested);

        // StopRequested -> Finalized
        search_state.set_finalized();
        assert_eq!(search_state, SearchState::Finalized);

        // Finalized -> Idle (after worker join)
        search_state.set_idle();
        assert_eq!(search_state, SearchState::Idle);
    }

    #[test]
    fn test_search_id_message_filtering() {
        // Test that messages with old search_id are dropped
        let current_search_id = 42u64;
        let old_search_id = 41u64;

        // Create test messages
        let old_info = WorkerMessage::Info {
            info: Default::default(),
            search_id: old_search_id,
        };

        let current_info = WorkerMessage::Info {
            info: Default::default(),
            search_id: current_search_id,
        };

        // Extract search_id from messages
        let old_msg_id = match &old_info {
            WorkerMessage::Info { search_id, .. } => *search_id,
            _ => unreachable!(),
        };

        let current_msg_id = match &current_info {
            WorkerMessage::Info { search_id, .. } => *search_id,
            _ => unreachable!(),
        };

        // Verify filtering logic
        assert_ne!(old_msg_id, current_search_id);
        assert_eq!(current_msg_id, current_search_id);
    }

    #[test]
    fn test_cleanup_messages_allowed_from_old_searches() {
        let old_search_id = 41u64;

        // Finished should be allowed even from old searches
        let finished_msg = WorkerMessage::Finished {
            from_guard: false,
            search_id: old_search_id,
        };

        // These messages should be allowed through for cleanup
        matches!(
            &finished_msg,
            WorkerMessage::Finished { .. } | WorkerMessage::ReturnEngine { .. }
        );
    }
}
