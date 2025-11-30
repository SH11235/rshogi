//! 局面表現モジュール
//!
//! 将棋の局面を表現し、手の実行・巻き戻しを行う。
//!
//! - `Position`: 局面
//! - `StateInfo`: 局面状態（ハッシュ、王手情報等）
//! - `do_move` / `undo_move`: 手の実行と巻き戻し
//! - SFEN形式の解析・出力

mod pos;
mod sfen;
mod state;
mod zobrist;

pub use pos::Position;
pub use sfen::{SfenError, SFEN_HIRATE};
pub use state::StateInfo;
pub use zobrist::{zobrist_hand, zobrist_psq, zobrist_side, ZOBRIST};
