use wasm_bindgen::prelude::*;

// Keep only WebRTC module for demo purposes
mod simple_webrtc;
pub use simple_webrtc::*;

// Add mate search module
mod mate_search;
pub use mate_search::*;

// Add opening book module
pub mod opening_book;
pub use opening_book::*;

// Add opening book reader module
pub mod opening_book_reader;

// Add AI module
pub mod ai;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    fn log(s: &str);
}

#[allow(unused_macros)]
macro_rules! console_log {
    ($($t:tt)*) => (log(&format_args!($($t)*).to_string()))
}

// Dummy structs for now - will be replaced with actual game logic
#[wasm_bindgen]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PieceType {
    Pawn,
}

#[wasm_bindgen]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Player {
    Black,
    White,
}

#[wasm_bindgen]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Piece {
    pub piece_type: PieceType,
    pub owner: Player,
    pub promoted: bool,
}

#[wasm_bindgen]
impl Piece {
    #[wasm_bindgen(constructor)]
    pub fn new(piece_type: PieceType, owner: Player) -> Piece {
        Piece {
            piece_type,
            owner,
            promoted: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::*;

    wasm_bindgen_test_configure!(run_in_browser);

    #[wasm_bindgen_test]
    #[cfg(target_arch = "wasm32")]
    fn test_piece_creation() {
        let piece = Piece::new(PieceType::Pawn, Player::Black);
        assert_eq!(piece.piece_type, PieceType::Pawn);
        assert_eq!(piece.owner, Player::Black);
        assert!(!piece.promoted);
    }

    #[test]
    fn test_player_enum() {
        let black = Player::Black;
        let white = Player::White;
        assert_ne!(black, white);
    }

    #[test]
    fn test_piece_type_enum() {
        let pawn = PieceType::Pawn;
        assert_eq!(pawn, PieceType::Pawn);
    }
}
