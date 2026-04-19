//! 棋譜生成・00LIST 行整形などの I/O 非依存ロジック。
//!
//! Phase 1 では CSA V2 棋譜（`record_v22.html` 準拠）を組み立てる
//! [`kifu::KifuRecord`] と、00LIST 1 行整形 [`kifu::format_zerozero_list_line`] を提供する。
//! 永続化アダプタ（FileKifuStorage 等）は [`crate::storage`] 配下を参照。

pub mod kifu;
