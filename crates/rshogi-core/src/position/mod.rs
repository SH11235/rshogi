//! 局面表現モジュール
//!
//! 将棋の局面を表現し、手の実行・巻き戻しを行う。
//!
//! - `Position`: 局面本体（盤面配列・Bitboard・手駒・手番・手数）
//! - `StateInfo`: 局面状態（Zobristハッシュ、王手情報、pin情報、直前の手など）
//! - `Zobrist`: Zobristハッシュ乱数テーブル（手番・駒×升・手駒）
//! - `do_move` / `undo_move` / `do_null_move`: 手の実行と巻き戻し（`StateInfo` をスタックとして管理）
//! - SFEN形式の解析・出力
//!
//! 盤面配列・Bitboard・手駒・Zobristキーは `Position` のメソッド
//! （`put_piece` / `remove_piece` / `do_move` 系）を通じて更新されることを前提とし、
//! 常に互いに整合しているように保つ。

mod board_effect;
pub mod json_conversion;
#[cfg(feature = "move-features")]
mod move_features;
mod movepicker_support;
mod pos;
mod sfen;
mod state;
mod zobrist;

pub(crate) use board_effect::BoardEffects;
#[cfg(feature = "move-features")]
pub use move_features::MoveFeatures;
pub use pos::Position;
pub use sfen::{SfenError, SFEN_HIRATE};
pub use state::StateInfo;
pub use zobrist::{zobrist_hand, zobrist_no_pawns, zobrist_psq, zobrist_side, ZOBRIST};
