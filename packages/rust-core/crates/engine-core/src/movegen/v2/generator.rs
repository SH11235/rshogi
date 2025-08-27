use crate::shogi::{Bitboard, Color, PieceType, Square, Position};
use crate::shogi::moves::Move;

use super::error::MoveGenError;
use super::movelist::MoveList;
use super::tables;

/// Move generator for generating legal moves
pub struct MoveGenerator;

impl MoveGenerator {
    /// Create a new move generator
    pub const fn new() -> Self {
        Self
    }

    /// Generate all legal moves for the given position
    pub fn generate_all(&self, pos: &Position) -> Result<MoveList, MoveGenError> {
        let mut gen = MoveGenImpl::new(pos)?;
        Ok(gen.generate_all())
    }

    /// Check if any legal move exists (early exit optimization)
    pub fn has_legal_moves(&self, pos: &Position) -> Result<bool, MoveGenError> {
        let mut gen = MoveGenImpl::new(pos)?;
        Ok(gen.has_any_legal_move())
    }

    /// Generate only capture moves
    pub fn generate_captures(&self, pos: &Position) -> Result<MoveList, MoveGenError> {
        let mut gen = MoveGenImpl::new(pos)?;
        Ok(gen.generate_captures())
    }

    /// Generate only non-capture moves
    pub fn generate_quiet(&self, pos: &Position) -> Result<MoveList, MoveGenError> {
        let mut gen = MoveGenImpl::new(pos)?;
        Ok(gen.generate_quiet())
    }
}

/// Internal move generation implementation
struct MoveGenImpl<'a> {
    pos: &'a Position,
    king_sq: Square,
    checkers: Bitboard,
    pinned: Bitboard,
    pin_rays: [Bitboard; 81],
    non_king_check_mask: Bitboard,
    drop_block_mask: Bitboard,
    us: Color,
    them: Color,
    our_pieces: Bitboard,
    their_pieces: Bitboard,
    occupied: Bitboard,
}

impl<'a> MoveGenImpl<'a> {
    /// Create a new move generation context
    fn new(pos: &'a Position) -> Result<Self, MoveGenError> {
        let us = pos.side_to_move;
        let them = us.opposite();
        
        // Find king square
        let king_sq = pos.board.king_square(us)
            .ok_or(MoveGenError::KingNotFound(us))?;

        let our_pieces = pos.board.occupied_bb[us as usize];
        let their_pieces = pos.board.occupied_bb[them as usize];
        let occupied = pos.board.all_bb;

        // Calculate checkers and pinned pieces
        let (checkers, pinned) = calculate_pins_and_checkers(pos, king_sq, us);

        let mut gen = Self {
            pos,
            king_sq,
            checkers,
            pinned,
            pin_rays: [Bitboard::EMPTY; 81],
            non_king_check_mask: Bitboard::ALL, // 0チェック時は制約なし
            drop_block_mask: Bitboard::ALL,     // 0チェック時は制約なし
            us,
            them,
            our_pieces,
            their_pieces,
            occupied,
        };

        // Update check masks if in check
        if !gen.checkers.is_empty() {
            // When in check, only moves that block/capture checker are legal (for non-king pieces)
            if gen.checkers.count_ones() == 1 {
                // Single check - can block or capture
                gen.non_king_check_mask = gen.checkers; // Can capture checker
                // TODO: Add blocking squares to non_king_check_mask
                gen.drop_block_mask = Bitboard::EMPTY; // TODO: Calculate blocking squares
            } else {
                // Double check - only king moves allowed
                gen.non_king_check_mask = Bitboard::EMPTY;
                gen.drop_block_mask = Bitboard::EMPTY;
            }
        }

        Ok(gen)
    }

    /// Helper to get captured piece type at a square
    #[inline]
    fn get_captured_type(&self, to: Square) -> Option<PieceType> {
        self.pos.board.piece_on(to).map(|p| p.piece_type)
    }

    /// Generate all legal moves
    fn generate_all(&mut self) -> MoveList {
        let mut moves = MoveList::new();

        // If in double check, only king moves are legal
        if self.checkers.count_ones() > 1 {
            self.generate_king_moves(&mut moves);
            return moves;
        }

        // Generate moves for all piece types
        self.generate_king_moves(&mut moves);
        self.generate_piece_moves(&mut moves);
        
        // Generate drop moves if not in check or single check
        if self.checkers.is_empty() || self.checkers.count_ones() == 1 {
            self.generate_drop_moves(&mut moves);
        }

        moves
    }

    /// Check if any legal move exists
    fn has_any_legal_move(&mut self) -> bool {
        // If in double check, only king moves are possible
        if self.checkers.count_ones() > 1 {
            return self.has_king_escape();
        }

        // Check king moves first (most likely to have moves)
        if self.has_king_escape() {
            return true;
        }

        // Check if any piece can block or capture checker
        if self.has_piece_move() {
            return true;
        }

        // Check drop moves
        if (self.checkers.is_empty() || self.checkers.count_ones() == 1) && self.has_drop_move() {
            return true;
        }

        false
    }

    /// Generate only capture moves
    fn generate_captures(&mut self) -> MoveList {
        let mut moves = MoveList::new();
        
        // Generate captures for all pieces
        self.generate_king_captures(&mut moves);
        self.generate_piece_captures(&mut moves);
        
        moves
    }

    /// Generate only quiet (non-capture) moves
    fn generate_quiet(&mut self) -> MoveList {
        let mut moves = MoveList::new();
        
        // Generate non-captures for all pieces
        self.generate_king_quiet(&mut moves);
        self.generate_piece_quiet(&mut moves);
        
        // Drops are always quiet
        if self.checkers.is_empty() || self.checkers.count_ones() == 1 {
            self.generate_drop_moves(&mut moves);
        }
        
        moves
    }

    /// Generate king moves
    fn generate_king_moves(&mut self, moves: &mut MoveList) {
        let attacks = tables::king_attacks(self.king_sq);
        let valid_targets = attacks & !self.our_pieces;

        for to_sq in valid_targets {
            // Check if king would be safe on this square
            if !self.is_attacked_by(to_sq, self.them) {
                let piece = self.pos.board.piece_on(self.king_sq).unwrap();
                let captured_type = self.pos.board.piece_on(to_sq).map(|p| p.piece_type);
                let mv = Move::normal_with_piece(self.king_sq, to_sq, false, piece.piece_type, captured_type);
                moves.push(mv);
            }
        }
    }

    /// Generate moves for non-king pieces
    fn generate_piece_moves(&mut self, moves: &mut MoveList) {
        // Generate pawn moves
        self.generate_all_pawn_moves(moves);
        // Generate lance moves
        self.generate_all_lance_moves(moves);
        // Generate knight moves
        self.generate_all_knight_moves(moves);
        // Generate silver moves
        self.generate_all_silver_moves(moves);
        // Generate gold moves (including promoted pieces)
        self.generate_all_gold_moves(moves);
        // Generate bishop moves
        self.generate_all_bishop_moves(moves);
        // Generate rook moves
        self.generate_all_rook_moves(moves);
    }

    /// Generate drop moves
    fn generate_drop_moves(&mut self, moves: &mut MoveList) {
        let us = self.pos.side_to_move;
        let empty_squares = !self.pos.board.all_bb;
        
        // Apply drop block mask to determine valid drop targets
        let drop_targets = empty_squares & self.drop_block_mask;
        
        // Early return if no valid drop targets
        if drop_targets.is_empty() {
            return;
        }
        
        // Check each piece type in hand (excluding King)
        for (piece_idx, &piece_type) in tables::HAND_ORDER.iter().enumerate() {
            let count = self.pos.hands[us as usize][piece_idx];
            if count == 0 {
                continue;
            }
            
            // Get valid drop squares for this piece type
            let valid_drops = self.get_valid_drop_squares(piece_type, empty_squares) & drop_targets;
            
            for to in valid_drops {
                moves.push(Move::drop(piece_type, to));
            }
        }
    }
    
    /// Get valid squares where a piece can be dropped
    fn get_valid_drop_squares(&self, piece_type: PieceType, empty_squares: Bitboard) -> Bitboard {
        let us = self.pos.side_to_move;
        let mut valid = empty_squares;
        
        match piece_type {
            PieceType::Pawn => {
                // Pawns cannot be dropped on files where we already have a pawn
                let our_pawns = self.pos.board.piece_bb[us as usize][PieceType::Pawn as usize]
                    & !self.pos.board.promoted_bb;
                
                // Remove files with our pawns
                for sq in our_pawns {
                    let file = sq.file();
                    // Remove all squares in this file
                    for rank in 0..9 {
                        let file_sq = Square::new(file, rank);
                        valid.clear(file_sq);
                    }
                }
                
                // Pawns cannot be dropped on the promotion rank
                match us {
                    Color::Black => {
                        // Cannot drop on rank 1
                        for file in 0..9 {
                            valid.clear(Square::new(file, 0));
                        }
                    }
                    Color::White => {
                        // Cannot drop on rank 9
                        for file in 0..9 {
                            valid.clear(Square::new(file, 8));
                        }
                    }
                }
                
                // Check two-pawn mate rule (optional - can be expensive)
                // TODO: Implement two-pawn mate check if needed
            }
            PieceType::Lance => {
                // Lances cannot be dropped on the promotion rank
                match us {
                    Color::Black => {
                        for file in 0..9 {
                            valid.clear(Square::new(file, 0));
                        }
                    }
                    Color::White => {
                        for file in 0..9 {
                            valid.clear(Square::new(file, 8));
                        }
                    }
                }
            }
            PieceType::Knight => {
                // Knights cannot be dropped on the last two ranks
                match us {
                    Color::Black => {
                        for file in 0..9 {
                            valid.clear(Square::new(file, 0));
                            valid.clear(Square::new(file, 1));
                        }
                    }
                    Color::White => {
                        for file in 0..9 {
                            valid.clear(Square::new(file, 7));
                            valid.clear(Square::new(file, 8));
                        }
                    }
                }
            }
            _ => {
                // Rook, Bishop, Gold, Silver can be dropped anywhere
            }
        }
        
        valid
    }

    /// Generate only king captures
    fn generate_king_captures(&mut self, moves: &mut MoveList) {
        let attacks = tables::king_attacks(self.king_sq);
        let captures = attacks & self.their_pieces;

        for to_sq in captures {
            if !self.is_attacked_by(to_sq, self.them) {
                let piece = self.pos.board.piece_on(self.king_sq).unwrap();
                let captured_piece = self.pos.board.piece_on(to_sq).unwrap();
                let mv = Move::normal_with_piece(self.king_sq, to_sq, false, piece.piece_type, Some(captured_piece.piece_type));
                moves.push(mv);
            }
        }
    }

    /// Generate only king quiet moves
    fn generate_king_quiet(&mut self, moves: &mut MoveList) {
        let attacks = tables::king_attacks(self.king_sq);
        let quiet = attacks & !self.occupied;

        for to_sq in quiet {
            if !self.is_attacked_by(to_sq, self.them) {
                let piece = self.pos.board.piece_on(self.king_sq).unwrap();
                let mv = Move::normal_with_piece(self.king_sq, to_sq, false, piece.piece_type, None);
                moves.push(mv);
            }
        }
    }

    /// Generate captures for non-king pieces
    fn generate_piece_captures(&mut self, _moves: &mut MoveList) {
        // TODO: Implement
    }

    /// Generate quiet moves for non-king pieces
    fn generate_piece_quiet(&mut self, _moves: &mut MoveList) {
        // TODO: Implement
    }

    /// Check if king has any escape move
    fn has_king_escape(&self) -> bool {
        let attacks = tables::king_attacks(self.king_sq);
        let valid_targets = attacks & !self.our_pieces;

        for to_sq in valid_targets {
            if !self.is_attacked_by(to_sq, self.them) {
                return true;
            }
        }

        false
    }

    /// Check if any piece can move
    fn has_piece_move(&self) -> bool {
        let us = self.pos.side_to_move;
        let our_pieces = self.pos.board.occupied_bb[us as usize];
        let target_mask = self.non_king_check_mask & !our_pieces;
        
        // Check pawns
        let pawns = self.pos.board.piece_bb[us as usize][PieceType::Pawn as usize]
            & !self.pos.board.promoted_bb & !self.pinned;
        if !pawns.is_empty() {
            for from in pawns {
                let attacks = tables::pawn_attacks(from, us);
                if !(attacks & target_mask).is_empty() {
                    return true;
                }
            }
        }
        
        // Check other pieces similarly...
        // For now, return false to avoid compilation errors
        false
    }

    /// Check if any drop move is possible
    fn has_drop_move(&self) -> bool {
        let us = self.pos.side_to_move;
        let hand = &self.pos.hands[us as usize];
        
        // Check if any piece is in hand
        let has_pieces = hand.iter().any(|&count| count > 0);
        if !has_pieces {
            return false;
        }
        
        // If there are empty squares that satisfy drop constraints, drops are possible
        let empty_squares = !self.pos.board.all_bb & self.drop_block_mask;
        !empty_squares.is_empty()
    }

    /// Check if a square is attacked by the given side
    fn is_attacked_by(&self, sq: Square, by: Color) -> bool {
        let attackers_bb = self.pos.board.occupied_bb[by as usize];
        
        // Check pawn attacks
        let pawn_attacks = tables::pawn_attacks(sq, by.opposite()); // Attack from perspective of defender
        let enemy_pawns = self.pos.board.piece_bb[by as usize][PieceType::Pawn as usize] 
            & !self.pos.board.promoted_bb;
        if !(pawn_attacks & enemy_pawns).is_empty() {
            return true;
        }
        
        // Check knight attacks
        let knight_attacks = tables::knight_attacks(sq, by.opposite());
        let enemy_knights = self.pos.board.piece_bb[by as usize][PieceType::Knight as usize]
            & !self.pos.board.promoted_bb;
        if !(knight_attacks & enemy_knights).is_empty() {
            return true;
        }
        
        // Check king attacks
        let king_attacks = tables::king_attacks(sq);
        let enemy_king = self.pos.board.piece_bb[by as usize][PieceType::King as usize];
        if !(king_attacks & enemy_king).is_empty() {
            return true;
        }
        
        // Check gold attacks (including promoted pieces)
        let gold_attacks = tables::gold_attacks(sq, by);
        let enemy_golds = self.pos.board.piece_bb[by as usize][PieceType::Gold as usize];
        let promoted_pieces = self.pos.board.promoted_bb & attackers_bb;
        let gold_movers = enemy_golds | (promoted_pieces & 
            (self.pos.board.piece_bb[by as usize][PieceType::Silver as usize] |
             self.pos.board.piece_bb[by as usize][PieceType::Knight as usize] |
             self.pos.board.piece_bb[by as usize][PieceType::Lance as usize] |
             self.pos.board.piece_bb[by as usize][PieceType::Pawn as usize]));
        if !(gold_attacks & gold_movers).is_empty() {
            return true;
        }
        
        // Check silver attacks
        let silver_attacks = tables::silver_attacks(sq, by);
        let enemy_silvers = self.pos.board.piece_bb[by as usize][PieceType::Silver as usize]
            & !self.pos.board.promoted_bb;
        if !(silver_attacks & enemy_silvers).is_empty() {
            return true;
        }
        
        // Check sliding pieces (rook, bishop, lance)
        // For now, simplified check - TODO: implement proper sliding attack detection
        // This requires ray attacks which we haven't implemented yet
        
        false
    }

    /// Generate all pawn moves
    fn generate_all_pawn_moves(&mut self, moves: &mut MoveList) {
        let us = self.us;
        let our_pieces = self.pos.board.occupied_bb[us as usize];
        let _their_pieces = self.pos.board.occupied_bb[self.them as usize];
        let _all_pieces = self.pos.board.all_bb;
        
        let pawns = self.pos.board.piece_bb[us as usize][PieceType::Pawn as usize];
        let unpromoted_pawns = pawns & !self.pos.board.promoted_bb;
        let promoted_pawns = pawns & self.pos.board.promoted_bb;
        
        // Handle unpromoted pawns
        for from in unpromoted_pawns {
            let to_bb = tables::pawn_attacks(from, us) & !our_pieces & self.non_king_check_mask;
            for to in to_bb {
                // Check if pawn is pinned
                if self.pinned.test(from) {
                    // Pinned piece can only move along pin ray
                    if !self.pin_rays[from.index()].test(to) {
                        continue;
                    }
                }
                
                let captured_type = self.get_captured_type(to);
                let must_promote = match us {
                    Color::Black => to.rank() == 0,  // rank 1 (9段)
                    Color::White => to.rank() == 8,  // rank 9 (1段)
                };
                let can_promote = match us {
                    Color::Black => from.rank() <= 2 || to.rank() <= 2,  // ranks 0-2 (a-c)
                    Color::White => from.rank() >= 6 || to.rank() >= 6,  // ranks 6-8 (g-i)
                };
                
                if must_promote {
                    moves.push(Move::normal_with_piece(from, to, true, PieceType::Pawn, captured_type));
                } else if can_promote {
                    // Add both promoting and non-promoting moves
                    moves.push(Move::normal_with_piece(from, to, false, PieceType::Pawn, captured_type));
                    moves.push(Move::normal_with_piece(from, to, true, PieceType::Pawn, captured_type));
                } else {
                    moves.push(Move::normal_with_piece(from, to, false, PieceType::Pawn, captured_type));
                }
            }
        }
        
        // Handle promoted pawns (tokin) - they move like gold
        for from in promoted_pawns {
            self.generate_gold_like_moves(from, PieceType::Pawn, moves);
        }
    }
    
    /// Generate moves for pieces that move like gold
    fn generate_gold_like_moves(&mut self, from: Square, piece_type: PieceType, moves: &mut MoveList) {
        let us = self.pos.side_to_move;
        let our_pieces = self.pos.board.occupied_bb[us as usize];
        
        let to_bb = tables::gold_attacks(from, us) & !our_pieces & self.non_king_check_mask;
        for to in to_bb {
            if self.pinned.test(from) && !self.pin_rays[from.index()].test(to) {
                continue;
            }
            
            let captured_type = self.get_captured_type(to);
            moves.push(Move::normal_with_piece(from, to, false, piece_type, captured_type));
        }
    }
    
    /// Generate all lance moves
    fn generate_all_lance_moves(&mut self, moves: &mut MoveList) {
        let us = self.pos.side_to_move;
        let our_pieces = self.pos.board.occupied_bb[us as usize];
        
        let lances = self.pos.board.piece_bb[us as usize][PieceType::Lance as usize];
        let unpromoted_lances = lances & !self.pos.board.promoted_bb;
        let promoted_lances = lances & self.pos.board.promoted_bb;
        
        // Handle unpromoted lances
        for from in unpromoted_lances {
            // Lance moves forward until blocked
            let attacks = tables::lance_attacks(from, us);
            let blockers = attacks & self.pos.board.all_bb;
            
            // Find first blocker
            let first_blocker = match us {
                Color::Black => {
                    // For black, find the highest rank (smallest number) blocker
                    let mut min_sq: Option<Square> = None;
                    for sq in blockers {
                        if min_sq.is_none() || sq.rank() < min_sq.unwrap().rank() {
                            min_sq = Some(sq);
                        }
                    }
                    min_sq
                }
                Color::White => {
                    // For white, find the lowest rank (largest number) blocker
                    let mut max_sq: Option<Square> = None;
                    for sq in blockers {
                        if max_sq.is_none() || sq.rank() > max_sq.unwrap().rank() {
                            max_sq = Some(sq);
                        }
                    }
                    max_sq
                }
            };
            
            let to_bb = if let Some(blocker) = first_blocker {
                // Can move to squares up to and including first blocker (if enemy)
                let mut valid_moves = Bitboard::EMPTY;
                
                // Add squares between from and blocker
                for sq in attacks {
                    let sq_rank = sq.rank();
                    let blocker_rank = blocker.rank();
                    
                    let is_before_blocker = match us {
                        Color::Black => sq_rank > blocker_rank,
                        Color::White => sq_rank < blocker_rank,
                    };
                    
                    if is_before_blocker {
                        valid_moves.set(sq);
                    }
                }
                
                // Include blocker square if it has enemy piece
                if !our_pieces.test(blocker) {
                    valid_moves.set(blocker);
                }
                
                valid_moves
            } else {
                attacks
            };
            
            for to in to_bb & self.non_king_check_mask {
                if self.pinned.test(from) && !self.pin_rays[from.index()].test(to) {
                    continue;
                }
                
                let captured_type = self.get_captured_type(to);
                let must_promote = match us {
                    Color::Black => to.rank() == 0,  // rank a = Japanese rank 1
                    Color::White => to.rank() == 8,  // rank i = Japanese rank 9
                };
                let can_promote = match us {
                    Color::Black => from.rank() <= 2 || to.rank() <= 2,  // ranks 0-2 (a-c)
                    Color::White => from.rank() >= 6 || to.rank() >= 6,  // ranks 6-8 (g-i)
                };
                
                if must_promote {
                    moves.push(Move::normal_with_piece(from, to, true, PieceType::Lance, captured_type));
                } else if can_promote {
                    moves.push(Move::normal_with_piece(from, to, false, PieceType::Lance, captured_type));
                    moves.push(Move::normal_with_piece(from, to, true, PieceType::Lance, captured_type));
                } else {
                    moves.push(Move::normal_with_piece(from, to, false, PieceType::Lance, captured_type));
                }
            }
        }
        
        // Handle promoted lances - they move like gold
        for from in promoted_lances {
            self.generate_gold_like_moves(from, PieceType::Lance, moves);
        }
    }
    
    /// Generate all knight moves
    fn generate_all_knight_moves(&mut self, moves: &mut MoveList) {
        let us = self.pos.side_to_move;
        let our_pieces = self.pos.board.occupied_bb[us as usize];
        
        let knights = self.pos.board.piece_bb[us as usize][PieceType::Knight as usize];
        let unpromoted_knights = knights & !self.pos.board.promoted_bb;
        let promoted_knights = knights & self.pos.board.promoted_bb;
        
        // Handle unpromoted knights
        for from in unpromoted_knights {
            let to_bb = tables::knight_attacks(from, us) & !our_pieces & self.non_king_check_mask;
            for to in to_bb {
                if self.pinned.test(from) && !self.pin_rays[from.index()].test(to) {
                    continue;
                }
                
                let captured_type = self.get_captured_type(to);
                let must_promote = match us {
                    Color::Black => to.rank() <= 1,  // ranks 0-1 (a-b)
                    Color::White => to.rank() >= 7,  // ranks 7-8 (h-i)
                };
                let can_promote = match us {
                    Color::Black => from.rank() <= 2 || to.rank() <= 2,  // ranks 0-2 (a-c)
                    Color::White => from.rank() >= 6 || to.rank() >= 6,  // ranks 6-8 (g-i)
                };
                
                if must_promote {
                    moves.push(Move::normal_with_piece(from, to, true, PieceType::Knight, captured_type));
                } else if can_promote {
                    moves.push(Move::normal_with_piece(from, to, false, PieceType::Knight, captured_type));
                    moves.push(Move::normal_with_piece(from, to, true, PieceType::Knight, captured_type));
                } else {
                    moves.push(Move::normal_with_piece(from, to, false, PieceType::Knight, captured_type));
                }
            }
        }
        
        // Handle promoted knights - they move like gold
        for from in promoted_knights {
            self.generate_gold_like_moves(from, PieceType::Knight, moves);
        }
    }
    
    /// Generate all silver moves
    fn generate_all_silver_moves(&mut self, moves: &mut MoveList) {
        let us = self.pos.side_to_move;
        let our_pieces = self.pos.board.occupied_bb[us as usize];
        
        let silvers = self.pos.board.piece_bb[us as usize][PieceType::Silver as usize];
        let unpromoted_silvers = silvers & !self.pos.board.promoted_bb;
        let promoted_silvers = silvers & self.pos.board.promoted_bb;
        
        // Handle unpromoted silvers
        for from in unpromoted_silvers {
            let to_bb = tables::silver_attacks(from, us) & !our_pieces & self.non_king_check_mask;
            for to in to_bb {
                if self.pinned.test(from) && !self.pin_rays[from.index()].test(to) {
                    continue;
                }
                
                let captured_type = self.get_captured_type(to);
                let can_promote = match us {
                    Color::Black => from.rank() <= 2 || to.rank() <= 2,  // ranks 0-2 (a-c)
                    Color::White => from.rank() >= 6 || to.rank() >= 6,  // ranks 6-8 (g-i)
                };
                
                if can_promote {
                    moves.push(Move::normal_with_piece(from, to, false, PieceType::Silver, captured_type));
                    moves.push(Move::normal_with_piece(from, to, true, PieceType::Silver, captured_type));
                } else {
                    moves.push(Move::normal_with_piece(from, to, false, PieceType::Silver, captured_type));
                }
            }
        }
        
        // Handle promoted silvers - they move like gold
        for from in promoted_silvers {
            self.generate_gold_like_moves(from, PieceType::Silver, moves);
        }
    }
    
    /// Generate all gold moves
    fn generate_all_gold_moves(&mut self, moves: &mut MoveList) {
        let us = self.pos.side_to_move;
        let golds = self.pos.board.piece_bb[us as usize][PieceType::Gold as usize];
        
        for from in golds {
            self.generate_gold_like_moves(from, PieceType::Gold, moves);
        }
    }
    
    /// Generate all bishop moves
    fn generate_all_bishop_moves(&mut self, _moves: &mut MoveList) {
        let us = self.pos.side_to_move;
        let bishops = self.pos.board.piece_bb[us as usize][PieceType::Bishop as usize];
        
        for _from in bishops {
            // TODO: Implement sliding bishop moves
            // For now, just generate basic moves as placeholder
        }
    }
    
    /// Generate all rook moves
    fn generate_all_rook_moves(&mut self, _moves: &mut MoveList) {
        let us = self.pos.side_to_move;
        let rooks = self.pos.board.piece_bb[us as usize][PieceType::Rook as usize];
        
        for _from in rooks {
            // TODO: Implement sliding rook moves
            // For now, just generate basic moves as placeholder
        }
    }
}

/// Calculate pinned pieces and checkers
fn calculate_pins_and_checkers(pos: &Position, king_sq: Square, us: Color) -> (Bitboard, Bitboard) {
    let them = us.opposite();
    let _our_pieces = pos.board.occupied_bb[us as usize];
    let _their_pieces = pos.board.occupied_bb[them as usize];
    let mut checkers = Bitboard::EMPTY;
    let pinned = Bitboard::EMPTY;
    
    // Check attacks from non-sliding pieces
    
    // Pawn checks
    let enemy_pawns = pos.board.piece_bb[them as usize][PieceType::Pawn as usize]
        & !pos.board.promoted_bb;
    let pawn_attacks = tables::pawn_attacks(king_sq, us); // Where our pawns would attack from
    checkers |= enemy_pawns & pawn_attacks;
    
    // Knight checks
    let enemy_knights = pos.board.piece_bb[them as usize][PieceType::Knight as usize]
        & !pos.board.promoted_bb;
    let knight_attacks = tables::knight_attacks(king_sq, us);
    checkers |= enemy_knights & knight_attacks;
    
    // Gold checks (including promoted pieces that move like gold)
    let gold_attacks = tables::gold_attacks(king_sq, them);
    let enemy_golds = pos.board.piece_bb[them as usize][PieceType::Gold as usize];
    checkers |= enemy_golds & gold_attacks;
    
    // Promoted pieces that move like gold
    let promoted_silvers = pos.board.piece_bb[them as usize][PieceType::Silver as usize]
        & pos.board.promoted_bb;
    let promoted_knights = pos.board.piece_bb[them as usize][PieceType::Knight as usize]
        & pos.board.promoted_bb;
    let promoted_lances = pos.board.piece_bb[them as usize][PieceType::Lance as usize]
        & pos.board.promoted_bb;
    let promoted_pawns = pos.board.piece_bb[them as usize][PieceType::Pawn as usize]
        & pos.board.promoted_bb;
    checkers |= (promoted_silvers | promoted_knights | promoted_lances | promoted_pawns) & gold_attacks;
    
    // Silver checks
    let enemy_silvers = pos.board.piece_bb[them as usize][PieceType::Silver as usize]
        & !pos.board.promoted_bb;
    let silver_attacks = tables::silver_attacks(king_sq, them);
    checkers |= enemy_silvers & silver_attacks;
    
    // TODO: Implement sliding piece (rook, bishop, lance) checks and pins
    // This requires ray attacks and between bitboards
    
    (checkers, pinned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_move_generator_creation() {
        let gen = MoveGenerator::new();
        let pos = Position::startpos();
        
        // Should be able to generate moves for starting position
        let result = gen.generate_all(&pos);
        assert!(result.is_ok());
        
        let moves = result.unwrap();
        // Starting position should have legal moves
        assert!(!moves.is_empty());
    }
}