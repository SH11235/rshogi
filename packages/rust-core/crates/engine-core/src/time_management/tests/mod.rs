//! Time management module tests

mod byoyomi_tests;
mod concurrent_tests;
mod integration_tests;
mod phase_integration_tests;
mod ponder_tests;
mod rounding_tests;
mod scheduled_stop_tests;

pub use crate::time_management::test_utils::{mock_advance_time, mock_set_time};
