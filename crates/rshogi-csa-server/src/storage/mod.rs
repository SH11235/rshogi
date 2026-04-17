//! 永続化アダプタ実装。Phase 1 では TCP 向けのファイルストレージのみを置く。

#[cfg(feature = "tokio-transport")]
pub mod file;
