//! Time management module tests

mod byoyomi_tests;
mod concurrent_tests;
mod integration_tests;
mod monotonic_test;
mod ponder_tests;
mod pv_stability_tests;

pub use crate::time_management::test_utils::{mock_advance_time, mock_set_time};
