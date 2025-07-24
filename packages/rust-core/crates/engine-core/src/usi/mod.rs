//! USI protocol support for the engine

mod notation;

pub use notation::{
    move_to_usi, parse_sfen, parse_usi_move, parse_usi_square, position_to_sfen, UsiParseError,
};
