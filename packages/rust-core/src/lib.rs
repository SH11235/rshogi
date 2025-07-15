use wasm_bindgen::prelude::*;

// Keep only WebRTC module for demo purposes
mod simple_webrtc;
pub use simple_webrtc::*;

// Add opening book module
pub mod opening_book;
pub use opening_book::*;

// Add opening book reader module
pub mod opening_book_reader;

// Add AI module
pub mod ai;

// Re-export commonly used types from AI module
pub use ai::board::{Bitboard, Board, Color, Piece, PieceType, Position, Square};
pub use ai::moves::Move;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    fn log(s: &str);
}

#[allow(unused_macros)]
macro_rules! console_log {
    ($($t:tt)*) => (log(&format_args!($($t)*).to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::*;

    wasm_bindgen_test_configure!(run_in_browser);

    #[wasm_bindgen_test]
    #[cfg(target_arch = "wasm32")]
    fn test_piece_creation() {
        let piece = Piece::new(PieceType::Pawn, Color::Black);
        assert_eq!(piece.piece_type, PieceType::Pawn);
        assert_eq!(piece.color, Color::Black);
        assert!(!piece.promoted);
    }

    #[test]
    fn test_color_enum() {
        let black = Color::Black;
        let white = Color::White;
        assert_ne!(black, white);
    }

    #[test]
    fn test_piece_type_enum() {
        let pawn = PieceType::Pawn;
        assert_eq!(pawn, PieceType::Pawn);
    }
}
