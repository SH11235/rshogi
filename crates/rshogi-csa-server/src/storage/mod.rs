//! 永続化アダプタ実装。現状は TCP 向けのファイルストレージのみ。

#[cfg(feature = "tokio-transport")]
pub mod file;
