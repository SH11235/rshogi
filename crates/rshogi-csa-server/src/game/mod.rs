//! 対局 1 つの進行ロジックをまとめるモジュール。

pub mod clock;
pub mod result;
pub mod room;
#[cfg(feature = "tokio-transport")]
pub mod run_loop;
pub mod validator;
