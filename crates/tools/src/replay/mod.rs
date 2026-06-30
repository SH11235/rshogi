//! PSV / tournament JSONL 共通の棋譜プレイヤー（`kifu_player` バイナリの実体）。

pub mod jsonl_source;
pub mod model;
pub mod psv_source;
pub mod tui;

pub use jsonl_source::JsonlSource;
pub use model::{
    GameIndex, GameIndexEntry, GameOutcomeView, GameRecord, GameSource, GameSourceRef,
    MoveAnnotation, MoveView, PairFileMeta, display_label,
};
pub use psv_source::PsvSource;
