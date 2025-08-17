//! Core Position structure and basic methods
//!
//! This module contains the Position struct definition and basic methods
//! for creating and querying positions.

use crate::shogi::board::{Board, Color, Piece, PieceType, Square};

/// Information needed to undo a move
#[derive(Clone, Debug)]
pub struct UndoInfo {
    /// Captured piece (if any)
    pub captured: Option<Piece>,
    /// Whether the moving piece was promoted before the move
    pub moved_piece_was_promoted: bool,
    /// Previous hash value
    pub previous_hash: u64,
    /// Previous ply count
    pub previous_ply: u16,
}

/// Position structure
#[derive(Clone, Debug)]
pub struct Position {
    /// Board with bitboards (8 piece types: K,R,B,G,S,N,L,P)
    pub board: Board,

    /// Pieces in hand [color][piece_type] (excluding King)
    pub hands: [[u8; 7]; 2],

    /// 手番 (Black or White)
    pub side_to_move: Color,

    /// 手数
    pub ply: u16,

    /// Zobrist hash (full 64 bits)
    pub hash: u64,
    /// Alias for hash (for compatibility)
    pub zobrist_hash: u64,

    /// History for repetition detection
    pub history: Vec<u64>,
}

impl Position {
    /// Create empty position
    pub fn empty() -> Self {
        Position {
            board: Board::empty(),
            hands: [[0; 7]; 2],
            side_to_move: Color::Black,
            ply: 0,
            hash: 0,
            zobrist_hash: 0,
            history: Vec::new(),
        }
    }

    /// Create starting position
    pub fn startpos() -> Self {
        let mut pos = Self::empty();

        // Place pawns
        // Black pawns on rank 6 (7th rank), White pawns on rank 2 (3rd rank)
        for file in 0..9 {
            let white_pawn_sq = Square::from_usi_chars((b'9' - file as u8) as char, 'c').unwrap();
            pos.board.put_piece(white_pawn_sq, Piece::new(PieceType::Pawn, Color::White));
            let black_pawn_sq = Square::from_usi_chars((b'9' - file as u8) as char, 'g').unwrap();
            pos.board.put_piece(black_pawn_sq, Piece::new(PieceType::Pawn, Color::Black));
        }

        // Black pieces on rank 8 (9th rank) and rank 7 (8th rank) - Black is at bottom
        // Lances
        pos.board.put_piece(
            Square::from_usi_chars('9', 'i').unwrap(),
            Piece::new(PieceType::Lance, Color::Black),
        );
        pos.board.put_piece(
            Square::from_usi_chars('1', 'i').unwrap(),
            Piece::new(PieceType::Lance, Color::Black),
        );

        // Knights
        pos.board.put_piece(
            Square::from_usi_chars('8', 'i').unwrap(),
            Piece::new(PieceType::Knight, Color::Black),
        );
        pos.board.put_piece(
            Square::from_usi_chars('2', 'i').unwrap(),
            Piece::new(PieceType::Knight, Color::Black),
        );

        // Silvers
        pos.board.put_piece(
            Square::from_usi_chars('7', 'i').unwrap(),
            Piece::new(PieceType::Silver, Color::Black),
        );
        pos.board.put_piece(
            Square::from_usi_chars('3', 'i').unwrap(),
            Piece::new(PieceType::Silver, Color::Black),
        );

        // Golds
        pos.board.put_piece(
            Square::from_usi_chars('6', 'i').unwrap(),
            Piece::new(PieceType::Gold, Color::Black),
        );
        pos.board.put_piece(
            Square::from_usi_chars('4', 'i').unwrap(),
            Piece::new(PieceType::Gold, Color::Black),
        );

        // King
        pos.board.put_piece(
            Square::from_usi_chars('5', 'i').unwrap(),
            Piece::new(PieceType::King, Color::Black),
        );

        // Rook (at 2h in USI)
        pos.board.put_piece(
            Square::from_usi_chars('2', 'h').unwrap(),
            Piece::new(PieceType::Rook, Color::Black),
        );

        // Bishop (at 8h in USI)
        pos.board.put_piece(
            Square::from_usi_chars('8', 'h').unwrap(),
            Piece::new(PieceType::Bishop, Color::Black),
        );

        // White pieces on rank 0 (1st rank) and rank 1 (2nd rank) - White is at top
        // Lances
        pos.board.put_piece(
            Square::from_usi_chars('9', 'a').unwrap(),
            Piece::new(PieceType::Lance, Color::White),
        );
        pos.board.put_piece(
            Square::from_usi_chars('1', 'a').unwrap(),
            Piece::new(PieceType::Lance, Color::White),
        );

        // Knights
        pos.board.put_piece(
            Square::from_usi_chars('8', 'a').unwrap(),
            Piece::new(PieceType::Knight, Color::White),
        );
        pos.board.put_piece(
            Square::from_usi_chars('2', 'a').unwrap(),
            Piece::new(PieceType::Knight, Color::White),
        );

        // Silvers
        pos.board.put_piece(
            Square::from_usi_chars('7', 'a').unwrap(),
            Piece::new(PieceType::Silver, Color::White),
        );
        pos.board.put_piece(
            Square::from_usi_chars('3', 'a').unwrap(),
            Piece::new(PieceType::Silver, Color::White),
        );

        // Golds
        pos.board.put_piece(
            Square::from_usi_chars('6', 'a').unwrap(),
            Piece::new(PieceType::Gold, Color::White),
        );
        pos.board.put_piece(
            Square::from_usi_chars('4', 'a').unwrap(),
            Piece::new(PieceType::Gold, Color::White),
        );

        // King
        pos.board.put_piece(
            Square::from_usi_chars('5', 'a').unwrap(),
            Piece::new(PieceType::King, Color::White),
        );

        // Rook (at 8b in USI)
        pos.board.put_piece(
            Square::from_usi_chars('8', 'b').unwrap(),
            Piece::new(PieceType::Rook, Color::White),
        );

        // Bishop (at 2b in USI)
        pos.board.put_piece(
            Square::from_usi_chars('2', 'b').unwrap(),
            Piece::new(PieceType::Bishop, Color::White),
        );

        // Calculate hash
        pos.hash = pos.compute_hash();
        pos.zobrist_hash = pos.hash;

        // Set initial ply to 0
        pos.ply = 0;

        pos
    }

    /// Create position from SFEN string
    pub fn from_sfen(sfen: &str) -> Result<Position, String> {
        crate::usi::parse_sfen(sfen).map_err(|e| e.to_string())
    }

    /// Compute Zobrist hash
    pub(super) fn compute_hash(&self) -> u64 {
        use crate::zobrist::ZobristHashing;
        ZobristHashing::zobrist_hash(self)
    }

    /// Get zobrist hash (method for compatibility)
    pub fn zobrist_hash(&self) -> u64 {
        self.zobrist_hash
    }

    /// Get king square for color
    pub fn king_square(&self, color: Color) -> Option<Square> {
        self.board.king_square(color)
    }

    /// Get piece at square
    pub fn piece_at(&self, sq: Square) -> Option<Piece> {
        self.board.piece_on(sq)
    }

    /// Count pieces of given type on board for both colors
    pub fn count_piece_on_board(&self, piece_type: PieceType) -> u16 {
        let black_count =
            self.board.piece_bb[Color::Black as usize][piece_type as usize].count_ones();
        let white_count =
            self.board.piece_bb[Color::White as usize][piece_type as usize].count_ones();
        (black_count + white_count) as u16
    }

    /// Count pieces in hand
    pub fn count_piece_in_hand(&self, color: Color, piece_type: PieceType) -> u16 {
        use crate::shogi::piece_constants::piece_type_to_hand_index;

        match piece_type {
            PieceType::King => 0,
            _ => {
                let hand_idx = piece_type_to_hand_index(piece_type)
                    .expect("Non-King piece type should be valid for hand");
                self.hands[color as usize][hand_idx] as u16
            }
        }
    }

    /// Calculate game phase based on material
    /// Returns a value from 0 (endgame) to 128 (opening)
    pub fn game_phase(&self) -> u8 {
        // Phase weights for each piece type (similar to engine controller)
        const PHASE_WEIGHTS: [(PieceType, u16); 6] = [
            (PieceType::Rook, 4),
            (PieceType::Bishop, 4),
            (PieceType::Gold, 3),
            (PieceType::Silver, 2),
            (PieceType::Knight, 2),
            (PieceType::Lance, 2),
        ];

        // Initial phase total (same as in engine controller)
        const INITIAL_PHASE_TOTAL: u16 = 52; // 2*4 + 2*4 + 4*3 + 4*2 + 4*2 + 4*2

        let mut phase_value = 0u16;

        // Count pieces on board and in hands
        for &(piece_type, weight) in &PHASE_WEIGHTS {
            let on_board = self.count_piece_on_board(piece_type);
            let in_hands = self.count_piece_in_hand(Color::Black, piece_type)
                + self.count_piece_in_hand(Color::White, piece_type);
            phase_value += (on_board + in_hands) * weight;
        }

        // Scale to 0-128 range
        ((phase_value as u32 * 128) / INITIAL_PHASE_TOTAL as u32).min(128) as u8
    }

    /// Check if position is in endgame phase
    pub fn is_endgame(&self) -> bool {
        self.game_phase() < 32 // Same threshold as PHASE_ENDGAME_THRESHOLD
    }

    /// Check if position is in opening phase
    pub fn is_opening(&self) -> bool {
        self.game_phase() > 96 // Same threshold as PHASE_OPENING_THRESHOLD
    }
}
