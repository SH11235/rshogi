use crate::shogi::attacks::{between_bb, sliding_attacks};
use crate::shogi::moves::Move;
use crate::shogi::{Bitboard, Color, PieceType, Position, Square};

use super::error::MoveGenError;
use super::movelist::MoveList;
use super::tables;

/// Number of squares on a shogi board (9x9)
const SHOGI_BOARD_SIZE: usize = 81;

/// Move generator for generating legal moves
pub struct MoveGenerator;

impl Default for MoveGenerator {
    fn default() -> Self {
        Self::new()
    }
}

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
    pin_rays: [Bitboard; SHOGI_BOARD_SIZE],
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
        let king_sq = pos.board.king_square(us).ok_or(MoveGenError::KingNotFound(us))?;

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
            pin_rays: [Bitboard::EMPTY; SHOGI_BOARD_SIZE],
            non_king_check_mask: Bitboard::ALL, // 0チェック時は制約なし
            drop_block_mask: Bitboard::ALL,     // 0チェック時は制約なし
            us,
            them,
            our_pieces,
            their_pieces,
            occupied,
        };

        // Now calculate pin rays after we have the mutable gen
        gen.calculate_pin_rays();

        // Update check masks if in check
        if !gen.checkers.is_empty() {
            // When in check, only moves that block/capture checker are legal (for non-king pieces)
            if gen.checkers.count_ones() == 1 {
                // Single check - can block or capture
                let checker_sq = gen.checkers.lsb().unwrap();
                gen.non_king_check_mask = gen.checkers; // Can capture checker

                // Add blocking squares if checker is a sliding piece
                if gen.is_sliding_piece(checker_sq) {
                    let blocking_squares = between_bb(checker_sq, king_sq);
                    gen.non_king_check_mask |= blocking_squares;
                    gen.drop_block_mask = blocking_squares; // Drops can only block
                } else {
                    gen.drop_block_mask = Bitboard::EMPTY; // Non-sliding pieces can't be blocked
                }
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
                let mv = Move::normal_with_piece(
                    self.king_sq,
                    to_sq,
                    false,
                    piece.piece_type,
                    captured_type,
                );
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

                // Remove files with our pawns using bitboard operations
                for pawn_sq in our_pawns {
                    let file = pawn_sq.file();
                    valid &= !crate::shogi::attacks::file_mask(file);
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

                // Check for drop pawn mate (打ち歩詰め)
                // Only need to check squares where pawn would give check
                let them = us.opposite();
                if let Some(their_king_sq) = self.pos.board.king_square(them) {
                    let potential_check_sq = match us {
                        Color::Black => {
                            // Black pawn checks from one rank below (toward white)
                            if their_king_sq.rank() > 0 {
                                Some(Square::new(their_king_sq.file(), their_king_sq.rank() - 1))
                            } else {
                                None
                            }
                        }
                        Color::White => {
                            // White pawn checks from one rank above (toward black)
                            if their_king_sq.rank() < 8 {
                                Some(Square::new(their_king_sq.file(), their_king_sq.rank() + 1))
                            } else {
                                None
                            }
                        }
                    };

                    if let Some(check_sq) = potential_check_sq {
                        if valid.test(check_sq) && self.is_drop_pawn_mate(check_sq, them) {
                            valid.clear(check_sq);
                        }
                    }
                }
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
                let mv = Move::normal_with_piece(
                    self.king_sq,
                    to_sq,
                    false,
                    piece.piece_type,
                    Some(captured_piece.piece_type),
                );
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
                let mv =
                    Move::normal_with_piece(self.king_sq, to_sq, false, piece.piece_type, None);
                moves.push(mv);
            }
        }
    }

    /// Generate captures for non-king pieces
    fn generate_piece_captures(&mut self, moves: &mut MoveList) {
        // Save current check mask to temporarily modify it
        let original_check_mask = self.non_king_check_mask;

        // Only allow captures of enemy pieces
        self.non_king_check_mask &= self.their_pieces;

        // Generate moves for all piece types - they will be filtered by the check mask
        if !self.non_king_check_mask.is_empty() {
            self.generate_all_pawn_moves(moves);
            self.generate_all_lance_moves(moves);
            self.generate_all_knight_moves(moves);
            self.generate_all_silver_moves(moves);
            self.generate_all_gold_moves(moves);
            self.generate_all_bishop_moves(moves);
            self.generate_all_rook_moves(moves);
        }

        // Restore original check mask
        self.non_king_check_mask = original_check_mask;
    }

    /// Generate quiet moves for non-king pieces
    fn generate_piece_quiet(&mut self, moves: &mut MoveList) {
        // Save current check mask to temporarily modify it
        let original_check_mask = self.non_king_check_mask;

        // Only allow moves to empty squares
        self.non_king_check_mask &= !self.occupied;

        // Generate moves for all piece types - they will be filtered by the check mask
        if !self.non_king_check_mask.is_empty() {
            self.generate_all_pawn_moves(moves);
            self.generate_all_lance_moves(moves);
            self.generate_all_knight_moves(moves);
            self.generate_all_silver_moves(moves);
            self.generate_all_gold_moves(moves);
            self.generate_all_bishop_moves(moves);
            self.generate_all_rook_moves(moves);
        }

        // Restore original check mask
        self.non_king_check_mask = original_check_mask;
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
            & !self.pos.board.promoted_bb
            & !self.pinned;
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

    /// Check if two squares are aligned for rook movement (same rank or file)
    #[inline]
    fn is_aligned_rook(&self, sq1: Square, sq2: Square) -> bool {
        sq1.file() == sq2.file() || sq1.rank() == sq2.rank()
    }

    /// Check if two squares are aligned for bishop movement (diagonal)
    #[inline]
    fn is_aligned_bishop(&self, sq1: Square, sq2: Square) -> bool {
        let file_diff = (sq1.file() as i8 - sq2.file() as i8).abs();
        let rank_diff = (sq1.rank() as i8 - sq2.rank() as i8).abs();
        file_diff == rank_diff && file_diff != 0
    }

    /// Check if the piece at the given square is a sliding piece
    fn is_sliding_piece(&self, sq: Square) -> bool {
        if let Some(piece) = self.pos.board.piece_on(sq) {
            matches!(piece.piece_type, PieceType::Rook | PieceType::Bishop | PieceType::Lance)
        } else {
            false
        }
    }

    /// Calculate pin rays for all pinned pieces
    fn calculate_pin_rays(&mut self) {
        let them = self.them;
        let king_sq = self.king_sq;

        // Check rook pins
        let enemy_rooks = self.pos.board.piece_bb[them as usize][PieceType::Rook as usize];
        for rook_sq in enemy_rooks {
            if self.is_aligned_rook(rook_sq, king_sq) {
                let between = between_bb(rook_sq, king_sq);
                let blockers = between & self.occupied;

                if blockers.count_ones() == 1 {
                    let blocker_sq = blockers.lsb().unwrap();
                    if self.pinned.test(blocker_sq) {
                        // Include the attacker and all squares between
                        self.pin_rays[blocker_sq.index()] = between
                            | Bitboard::from_square(rook_sq)
                            | Bitboard::from_square(king_sq);
                    }
                }
            }
        }

        // Check bishop pins
        let enemy_bishops = self.pos.board.piece_bb[them as usize][PieceType::Bishop as usize];
        for bishop_sq in enemy_bishops {
            if self.is_aligned_bishop(bishop_sq, king_sq) {
                let between = between_bb(bishop_sq, king_sq);
                let blockers = between & self.occupied;

                if blockers.count_ones() == 1 {
                    let blocker_sq = blockers.lsb().unwrap();
                    if self.pinned.test(blocker_sq) {
                        // Include the attacker and all squares between
                        self.pin_rays[blocker_sq.index()] = between
                            | Bitboard::from_square(bishop_sq)
                            | Bitboard::from_square(king_sq);
                    }
                }
            }
        }

        // Check lance pins
        let enemy_lances = self.pos.board.piece_bb[them as usize][PieceType::Lance as usize]
            & !self.pos.board.promoted_bb;
        for lance_sq in enemy_lances {
            let can_attack = match them {
                Color::Black => {
                    lance_sq.rank() > king_sq.rank() && lance_sq.file() == king_sq.file()
                }
                Color::White => {
                    lance_sq.rank() < king_sq.rank() && lance_sq.file() == king_sq.file()
                }
            };

            if can_attack {
                let between = between_bb(lance_sq, king_sq);
                let blockers = between & self.occupied;

                if blockers.count_ones() == 1 {
                    let blocker_sq = blockers.lsb().unwrap();
                    if self.pinned.test(blocker_sq) {
                        // Include the attacker and all squares between
                        self.pin_rays[blocker_sq.index()] = between
                            | Bitboard::from_square(lance_sq)
                            | Bitboard::from_square(king_sq);
                    }
                }
            }
        }
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
        let gold_movers = enemy_golds
            | (promoted_pieces
                & (self.pos.board.piece_bb[by as usize][PieceType::Silver as usize]
                    | self.pos.board.piece_bb[by as usize][PieceType::Knight as usize]
                    | self.pos.board.piece_bb[by as usize][PieceType::Lance as usize]
                    | self.pos.board.piece_bb[by as usize][PieceType::Pawn as usize]));
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

        // Rook attacks
        let enemy_rooks = self.pos.board.piece_bb[by as usize][PieceType::Rook as usize];
        if !enemy_rooks.is_empty() {
            let rook_attacks = sliding_attacks(sq, self.occupied, PieceType::Rook);
            if !(rook_attacks & enemy_rooks).is_empty() {
                return true;
            }

            // Check promoted rooks (Dragon) king-like moves
            let dragons = enemy_rooks & self.pos.board.promoted_bb;
            if !(king_attacks & dragons).is_empty() {
                return true;
            }
        }

        // Bishop attacks
        let enemy_bishops = self.pos.board.piece_bb[by as usize][PieceType::Bishop as usize];
        if !enemy_bishops.is_empty() {
            let bishop_attacks = sliding_attacks(sq, self.occupied, PieceType::Bishop);
            if !(bishop_attacks & enemy_bishops).is_empty() {
                return true;
            }

            // Check promoted bishops (Horse) king-like moves
            let horses = enemy_bishops & self.pos.board.promoted_bb;
            if !(king_attacks & horses).is_empty() {
                return true;
            }
        }

        // Lance attacks - only check in the correct direction
        let enemy_lances = self.pos.board.piece_bb[by as usize][PieceType::Lance as usize]
            & !self.pos.board.promoted_bb;
        for lance_sq in enemy_lances {
            // Check if lance can attack in the direction of sq
            let can_attack = match by {
                Color::Black => lance_sq.rank() > sq.rank() && lance_sq.file() == sq.file(),
                Color::White => lance_sq.rank() < sq.rank() && lance_sq.file() == sq.file(),
            };

            if can_attack {
                let between = between_bb(lance_sq, sq);
                if (between & self.occupied).is_empty() {
                    return true;
                }
            }
        }

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
                    Color::Black => to.rank() == 0, // rank 1 (9段)
                    Color::White => to.rank() == 8, // rank 9 (1段)
                };
                let can_promote = match us {
                    Color::Black => from.rank() <= 2 || to.rank() <= 2, // ranks 0-2 (a-c)
                    Color::White => from.rank() >= 6 || to.rank() >= 6, // ranks 6-8 (g-i)
                };

                if must_promote {
                    moves.push(Move::normal_with_piece(
                        from,
                        to,
                        true,
                        PieceType::Pawn,
                        captured_type,
                    ));
                } else if can_promote {
                    // Add both promoting and non-promoting moves
                    moves.push(Move::normal_with_piece(
                        from,
                        to,
                        false,
                        PieceType::Pawn,
                        captured_type,
                    ));
                    moves.push(Move::normal_with_piece(
                        from,
                        to,
                        true,
                        PieceType::Pawn,
                        captured_type,
                    ));
                } else {
                    moves.push(Move::normal_with_piece(
                        from,
                        to,
                        false,
                        PieceType::Pawn,
                        captured_type,
                    ));
                }
            }
        }

        // Handle promoted pawns (tokin) - they move like gold
        for from in promoted_pawns {
            self.generate_gold_like_moves(from, PieceType::Pawn, moves);
        }
    }

    /// Generate moves for pieces that move like gold
    fn generate_gold_like_moves(
        &mut self,
        from: Square,
        piece_type: PieceType,
        moves: &mut MoveList,
    ) {
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

        let lances = self.pos.board.piece_bb[us as usize][PieceType::Lance as usize];
        let unpromoted_lances = lances & !self.pos.board.promoted_bb;
        let promoted_lances = lances & self.pos.board.promoted_bb;

        // Handle unpromoted lances
        for from in unpromoted_lances {
            // Lance moves forward until blocked
            let attacks = tables::lance_attacks(from, us);
            let blockers = attacks & self.pos.board.all_bb;

            // Find valid moves for lance
            let to_bb = if !blockers.is_empty() {
                // Find first blocker efficiently
                // Since lance moves only vertically on same file, we can optimize
                let from_file = from.file();
                let from_rank = from.rank();
                let mut first_blocker = None;

                // Find the closest blocker in the direction of movement
                match us {
                    Color::Black => {
                        // Black lance moves up (decreasing rank)
                        for rank in (0..from_rank).rev() {
                            let sq = Square::new(from_file, rank);
                            if blockers.test(sq) {
                                first_blocker = Some(sq);
                                break;
                            }
                        }
                    }
                    Color::White => {
                        // White lance moves down (increasing rank)
                        for rank in (from_rank + 1)..9 {
                            let sq = Square::new(from_file, rank);
                            if blockers.test(sq) {
                                first_blocker = Some(sq);
                                break;
                            }
                        }
                    }
                }

                if let Some(blocker_sq) = first_blocker {
                    // Get all squares between from and blocker using between_bb
                    let moves_between = between_bb(from, blocker_sq);

                    // Include blocker square only if it contains an enemy piece
                    if self.their_pieces.test(blocker_sq) {
                        moves_between | Bitboard::from_square(blocker_sq)
                    } else {
                        moves_between
                    }
                } else {
                    // This shouldn't happen if blockers is not empty, but handle gracefully
                    attacks
                }
            } else {
                // No blockers, can move to all attacked squares
                attacks
            };

            for to in to_bb & self.non_king_check_mask {
                if self.pinned.test(from) && !self.pin_rays[from.index()].test(to) {
                    continue;
                }

                let captured_type = self.get_captured_type(to);
                let must_promote = match us {
                    Color::Black => to.rank() == 0, // rank a = Japanese rank 1
                    Color::White => to.rank() == 8, // rank i = Japanese rank 9
                };
                let can_promote = match us {
                    Color::Black => from.rank() <= 2 || to.rank() <= 2, // ranks 0-2 (a-c)
                    Color::White => from.rank() >= 6 || to.rank() >= 6, // ranks 6-8 (g-i)
                };

                if must_promote {
                    moves.push(Move::normal_with_piece(
                        from,
                        to,
                        true,
                        PieceType::Lance,
                        captured_type,
                    ));
                } else if can_promote {
                    moves.push(Move::normal_with_piece(
                        from,
                        to,
                        false,
                        PieceType::Lance,
                        captured_type,
                    ));
                    moves.push(Move::normal_with_piece(
                        from,
                        to,
                        true,
                        PieceType::Lance,
                        captured_type,
                    ));
                } else {
                    moves.push(Move::normal_with_piece(
                        from,
                        to,
                        false,
                        PieceType::Lance,
                        captured_type,
                    ));
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
                    Color::Black => to.rank() <= 1, // ranks 0-1 (a-b)
                    Color::White => to.rank() >= 7, // ranks 7-8 (h-i)
                };
                let can_promote = match us {
                    Color::Black => from.rank() <= 2 || to.rank() <= 2, // ranks 0-2 (a-c)
                    Color::White => from.rank() >= 6 || to.rank() >= 6, // ranks 6-8 (g-i)
                };

                if must_promote {
                    moves.push(Move::normal_with_piece(
                        from,
                        to,
                        true,
                        PieceType::Knight,
                        captured_type,
                    ));
                } else if can_promote {
                    moves.push(Move::normal_with_piece(
                        from,
                        to,
                        false,
                        PieceType::Knight,
                        captured_type,
                    ));
                    moves.push(Move::normal_with_piece(
                        from,
                        to,
                        true,
                        PieceType::Knight,
                        captured_type,
                    ));
                } else {
                    moves.push(Move::normal_with_piece(
                        from,
                        to,
                        false,
                        PieceType::Knight,
                        captured_type,
                    ));
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
                    Color::Black => from.rank() <= 2 || to.rank() <= 2, // ranks 0-2 (a-c)
                    Color::White => from.rank() >= 6 || to.rank() >= 6, // ranks 6-8 (g-i)
                };

                if can_promote {
                    moves.push(Move::normal_with_piece(
                        from,
                        to,
                        false,
                        PieceType::Silver,
                        captured_type,
                    ));
                    moves.push(Move::normal_with_piece(
                        from,
                        to,
                        true,
                        PieceType::Silver,
                        captured_type,
                    ));
                } else {
                    moves.push(Move::normal_with_piece(
                        from,
                        to,
                        false,
                        PieceType::Silver,
                        captured_type,
                    ));
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
    fn generate_all_bishop_moves(&mut self, moves: &mut MoveList) {
        let us = self.pos.side_to_move;
        let our_pieces = self.pos.board.occupied_bb[us as usize];

        let bishops = self.pos.board.piece_bb[us as usize][PieceType::Bishop as usize];
        let unpromoted_bishops = bishops & !self.pos.board.promoted_bb;
        let promoted_bishops = bishops & self.pos.board.promoted_bb;

        // Handle unpromoted bishops
        for from in unpromoted_bishops {
            let attacks = sliding_attacks(from, self.occupied, PieceType::Bishop);
            let to_bb = attacks & !our_pieces & self.non_king_check_mask;

            for to in to_bb {
                if self.pinned.test(from) && !self.pin_rays[from.index()].test(to) {
                    continue;
                }

                let captured_type = self.get_captured_type(to);
                let can_promote = match us {
                    Color::Black => from.rank() <= 2 || to.rank() <= 2, // ranks 0-2 (a-c)
                    Color::White => from.rank() >= 6 || to.rank() >= 6, // ranks 6-8 (g-i)
                };

                if can_promote {
                    moves.push(Move::normal_with_piece(
                        from,
                        to,
                        false,
                        PieceType::Bishop,
                        captured_type,
                    ));
                    moves.push(Move::normal_with_piece(
                        from,
                        to,
                        true,
                        PieceType::Bishop,
                        captured_type,
                    ));
                } else {
                    moves.push(Move::normal_with_piece(
                        from,
                        to,
                        false,
                        PieceType::Bishop,
                        captured_type,
                    ));
                }
            }
        }

        // Handle promoted bishops (Horse) - they move like bishop + king
        for from in promoted_bishops {
            // Bishop sliding moves
            let bishop_attacks = sliding_attacks(from, self.occupied, PieceType::Bishop);
            let to_bb = bishop_attacks & !our_pieces & self.non_king_check_mask;

            for to in to_bb {
                if self.pinned.test(from) && !self.pin_rays[from.index()].test(to) {
                    continue;
                }

                let captured_type = self.get_captured_type(to);
                moves.push(Move::normal_with_piece(
                    from,
                    to,
                    false,
                    PieceType::Bishop,
                    captured_type,
                ));
            }

            // King-like moves
            let king_attacks = tables::king_attacks(from);
            let king_targets = king_attacks & !our_pieces & self.non_king_check_mask;

            for to in king_targets {
                if self.pinned.test(from) && !self.pin_rays[from.index()].test(to) {
                    continue;
                }

                let captured_type = self.get_captured_type(to);
                moves.push(Move::normal_with_piece(
                    from,
                    to,
                    false,
                    PieceType::Bishop,
                    captured_type,
                ));
            }
        }
    }

    /// Generate all rook moves
    fn generate_all_rook_moves(&mut self, moves: &mut MoveList) {
        let us = self.pos.side_to_move;
        let our_pieces = self.pos.board.occupied_bb[us as usize];

        let rooks = self.pos.board.piece_bb[us as usize][PieceType::Rook as usize];
        let unpromoted_rooks = rooks & !self.pos.board.promoted_bb;
        let promoted_rooks = rooks & self.pos.board.promoted_bb;

        // Handle unpromoted rooks
        for from in unpromoted_rooks {
            let attacks = sliding_attacks(from, self.occupied, PieceType::Rook);
            let to_bb = attacks & !our_pieces & self.non_king_check_mask;

            for to in to_bb {
                if self.pinned.test(from) && !self.pin_rays[from.index()].test(to) {
                    continue;
                }

                let captured_type = self.get_captured_type(to);
                let can_promote = match us {
                    Color::Black => from.rank() <= 2 || to.rank() <= 2, // ranks 0-2 (a-c)
                    Color::White => from.rank() >= 6 || to.rank() >= 6, // ranks 6-8 (g-i)
                };

                if can_promote {
                    moves.push(Move::normal_with_piece(
                        from,
                        to,
                        false,
                        PieceType::Rook,
                        captured_type,
                    ));
                    moves.push(Move::normal_with_piece(
                        from,
                        to,
                        true,
                        PieceType::Rook,
                        captured_type,
                    ));
                } else {
                    moves.push(Move::normal_with_piece(
                        from,
                        to,
                        false,
                        PieceType::Rook,
                        captured_type,
                    ));
                }
            }
        }

        // Handle promoted rooks (Dragon) - they move like rook + king
        for from in promoted_rooks {
            // Rook sliding moves
            let rook_attacks = sliding_attacks(from, self.occupied, PieceType::Rook);
            let to_bb = rook_attacks & !our_pieces & self.non_king_check_mask;

            for to in to_bb {
                if self.pinned.test(from) && !self.pin_rays[from.index()].test(to) {
                    continue;
                }

                let captured_type = self.get_captured_type(to);
                moves.push(Move::normal_with_piece(
                    from,
                    to,
                    false,
                    PieceType::Rook,
                    captured_type,
                ));
            }

            // King-like moves
            let king_attacks = tables::king_attacks(from);
            let king_targets = king_attacks & !our_pieces & self.non_king_check_mask;

            for to in king_targets {
                if self.pinned.test(from) && !self.pin_rays[from.index()].test(to) {
                    continue;
                }

                let captured_type = self.get_captured_type(to);
                moves.push(Move::normal_with_piece(
                    from,
                    to,
                    false,
                    PieceType::Rook,
                    captured_type,
                ));
            }
        }
    }

    /// Get all attackers to a square with custom occupancy
    fn attackers_to_with_occupancy(&self, sq: Square, by: Color, occupancy: Bitboard) -> Bitboard {
        // Only consider pieces that exist in the given occupancy
        let pieces = self.pos.board.occupied_bb[by as usize] & occupancy;

        // Early return if no pieces exist
        if pieces.is_empty() {
            return Bitboard::EMPTY;
        }

        let mut attackers = Bitboard::EMPTY;

        // Pawn attacks
        let pawn_attacks = tables::pawn_attacks(sq, by.opposite()); // Attack from perspective of defender
        let enemy_pawns = self.pos.board.piece_bb[by as usize][PieceType::Pawn as usize]
            & !self.pos.board.promoted_bb
            & pieces;
        attackers |= pawn_attacks & enemy_pawns;

        // Knight attacks
        let knight_attacks = tables::knight_attacks(sq, by.opposite());
        let enemy_knights = self.pos.board.piece_bb[by as usize][PieceType::Knight as usize]
            & !self.pos.board.promoted_bb
            & pieces;
        attackers |= knight_attacks & enemy_knights;

        // King attacks
        let king_attacks = tables::king_attacks(sq);
        let enemy_king = self.pos.board.piece_bb[by as usize][PieceType::King as usize] & pieces;
        attackers |= king_attacks & enemy_king;

        // Gold attacks (including promoted pieces)
        let gold_attacks = tables::gold_attacks(sq, by);
        let enemy_golds = self.pos.board.piece_bb[by as usize][PieceType::Gold as usize] & pieces;
        let promoted_pieces = self.pos.board.promoted_bb & pieces;
        let gold_movers = enemy_golds
            | (promoted_pieces
                & (self.pos.board.piece_bb[by as usize][PieceType::Silver as usize]
                    | self.pos.board.piece_bb[by as usize][PieceType::Knight as usize]
                    | self.pos.board.piece_bb[by as usize][PieceType::Lance as usize]
                    | self.pos.board.piece_bb[by as usize][PieceType::Pawn as usize]));
        attackers |= gold_attacks & gold_movers;

        // Silver attacks
        let silver_attacks = tables::silver_attacks(sq, by);
        let enemy_silvers = self.pos.board.piece_bb[by as usize][PieceType::Silver as usize]
            & !self.pos.board.promoted_bb
            & pieces;
        attackers |= silver_attacks & enemy_silvers;

        // Sliding pieces with custom occupancy

        // Rook attacks
        let enemy_rooks = self.pos.board.piece_bb[by as usize][PieceType::Rook as usize] & pieces;
        if !enemy_rooks.is_empty() {
            let rook_attacks = sliding_attacks(sq, occupancy, PieceType::Rook);
            attackers |= rook_attacks & enemy_rooks;

            // Promoted rooks (Dragon) king-like moves
            let dragons = enemy_rooks & self.pos.board.promoted_bb;
            attackers |= king_attacks & dragons;
        }

        // Bishop attacks
        let enemy_bishops =
            self.pos.board.piece_bb[by as usize][PieceType::Bishop as usize] & pieces;
        if !enemy_bishops.is_empty() {
            let bishop_attacks = sliding_attacks(sq, occupancy, PieceType::Bishop);
            attackers |= bishop_attacks & enemy_bishops;

            // Promoted bishops (Horse) king-like moves
            let horses = enemy_bishops & self.pos.board.promoted_bb;
            attackers |= king_attacks & horses;
        }

        // Lance attacks
        let enemy_lances = self.pos.board.piece_bb[by as usize][PieceType::Lance as usize]
            & !self.pos.board.promoted_bb
            & pieces;
        for lance_sq in enemy_lances {
            // Check if lance can attack in the direction of sq
            let can_attack = match by {
                Color::Black => lance_sq.rank() > sq.rank() && lance_sq.file() == sq.file(),
                Color::White => lance_sq.rank() < sq.rank() && lance_sq.file() == sq.file(),
            };

            if can_attack {
                let between = between_bb(lance_sq, sq);
                if (between & occupancy).is_empty() {
                    attackers.set(lance_sq);
                }
            }
        }

        attackers
    }

    /// Check if dropping a pawn at 'to' would be checkmate (illegal)
    fn is_drop_pawn_mate(&self, to: Square, them: Color) -> bool {
        // Get the enemy king square
        let their_king_sq = match self.pos.board.king_square(them) {
            Some(sq) => sq,
            None => return false, // No king?
        };

        // Check if the pawn drop gives check
        let us = them.opposite();
        let pawn_attacks = tables::pawn_attacks(to, us);
        if !pawn_attacks.test(their_king_sq) {
            return false; // Not even a check
        }

        // Early return if the pawn is not supported (king can just capture it)
        let pawn_support = self.attackers_to_with_occupancy(to, us, self.pos.board.all_bb);
        if pawn_support.is_empty() {
            return false;
        }

        // Simulate position after pawn drop
        let occupied_after_drop = self.pos.board.all_bb | Bitboard::from_square(to);

        // 1) Check if king can capture the pawn safely
        // If the king captures, check if 'to' is still attacked
        let mut occ_after_king_capture = occupied_after_drop;
        occ_after_king_capture.clear(their_king_sq);
        occ_after_king_capture.set(to); // King is now on 'to'

        let king_capturer_attackers =
            self.attackers_to_with_occupancy(to, us, occ_after_king_capture);
        if king_capturer_attackers.is_empty() {
            return false; // King can safely capture
        }

        // 2) Check if any non-king piece can capture the pawn
        // Use bitboard operations to check defenders more efficiently
        let non_king_defenders = self.attackers_to_with_occupancy(to, them, occupied_after_drop)
            & !self.pos.board.piece_bb[them as usize][PieceType::King as usize];

        // For each defender, check if it's pinned
        // A piece is pinned if it's on the line between king and an attacker
        let mut defenders = non_king_defenders;
        while let Some(def_sq) = defenders.pop_lsb() {
            // Quick check: if defender is not on any line to the king, it can't be pinned
            let between = between_bb(def_sq, their_king_sq);
            if between.is_empty() {
                // Not on same line as king, definitely not pinned
                return false;
            }

            // Check if removing defender exposes king to check
            let mut occ_test = self.pos.board.all_bb;
            occ_test.clear(def_sq);

            // Only check attackers that could pin through this defender
            let potential_pinners =
                self.pos.board.occupied_bb[us as usize] & !Bitboard::from_square(to);
            let through_defender_attackers = self.check_attacks_through_square(
                their_king_sq,
                def_sq,
                us,
                potential_pinners,
                occ_test,
            );

            if through_defender_attackers.is_empty() {
                return false; // Defender can capture
            }
        }

        // 3) Check if king has escape squares
        let king_attacks = tables::king_attacks(their_king_sq);
        let their_pieces = self.pos.board.occupied_bb[them as usize];
        let escape_squares = king_attacks & !their_pieces;

        let mut escapes = escape_squares;
        while let Some(escape_sq) = escapes.pop_lsb() {
            // Optimize: only check squares not attacked by the dropped pawn
            if pawn_attacks.test(escape_sq) {
                continue; // This escape is blocked by the pawn
            }

            let mut occ_after_escape = occupied_after_drop;
            occ_after_escape.clear(their_king_sq);
            occ_after_escape.set(escape_sq);

            // If king captures on escape, remove the captured piece
            if self.pos.board.occupied_bb[us as usize].test(escape_sq) {
                occ_after_escape.clear(escape_sq);
                occ_after_escape.set(escape_sq); // King takes its place
            }

            let escape_attackers =
                self.attackers_to_with_occupancy(escape_sq, us, occ_after_escape);
            if escape_attackers.is_empty() {
                return false; // Safe escape exists
            }
        }

        true // No defense - it's mate
    }

    /// Check if there are attackers on the line through a square
    fn check_attacks_through_square(
        &self,
        target: Square,
        through: Square,
        by: Color,
        potential_attackers: Bitboard,
        occupancy: Bitboard,
    ) -> Bitboard {
        // This is a helper to check sliding attacks that go through a square
        let mut attackers = Bitboard::EMPTY;

        // Check rooks and queens (promoted rooks) on same rank/file
        if target.rank() == through.rank() || target.file() == through.file() {
            let rooks = self.pos.board.piece_bb[by as usize][PieceType::Rook as usize]
                & potential_attackers;
            for rook_sq in rooks {
                if between_bb(rook_sq, target).test(through) {
                    let attacks = sliding_attacks(rook_sq, occupancy, PieceType::Rook);
                    if attacks.test(target) {
                        attackers.set(rook_sq);
                    }
                }
            }
        }

        // Check bishops and horses (promoted bishops) on diagonals
        let rank_diff = (target.rank() as i8 - through.rank() as i8).abs();
        let file_diff = (target.file() as i8 - through.file() as i8).abs();
        if rank_diff == file_diff {
            let bishops = self.pos.board.piece_bb[by as usize][PieceType::Bishop as usize]
                & potential_attackers;
            for bishop_sq in bishops {
                if between_bb(bishop_sq, target).test(through) {
                    let attacks = sliding_attacks(bishop_sq, occupancy, PieceType::Bishop);
                    if attacks.test(target) {
                        attackers.set(bishop_sq);
                    }
                }
            }
        }

        // Check lances on same file
        if target.file() == through.file() {
            let lances = self.pos.board.piece_bb[by as usize][PieceType::Lance as usize]
                & !self.pos.board.promoted_bb
                & potential_attackers;
            for lance_sq in lances {
                let can_attack = match by {
                    Color::Black => lance_sq.rank() > target.rank(),
                    Color::White => lance_sq.rank() < target.rank(),
                };
                if can_attack && between_bb(lance_sq, target).test(through) {
                    let between = between_bb(lance_sq, target);
                    if (between & occupancy).is_empty() {
                        attackers.set(lance_sq);
                    }
                }
            }
        }

        attackers
    }
}

/// Calculate pinned pieces and checkers
fn calculate_pins_and_checkers(pos: &Position, king_sq: Square, us: Color) -> (Bitboard, Bitboard) {
    let them = us.opposite();
    let _our_pieces = pos.board.occupied_bb[us as usize];
    let _their_pieces = pos.board.occupied_bb[them as usize];
    let mut checkers = Bitboard::EMPTY;
    let mut pinned = Bitboard::EMPTY;

    // Check attacks from non-sliding pieces

    // Pawn checks
    let enemy_pawns =
        pos.board.piece_bb[them as usize][PieceType::Pawn as usize] & !pos.board.promoted_bb;
    let pawn_attacks = tables::pawn_attacks(king_sq, us); // Where our pawns would attack from
    checkers |= enemy_pawns & pawn_attacks;

    // Knight checks
    let enemy_knights =
        pos.board.piece_bb[them as usize][PieceType::Knight as usize] & !pos.board.promoted_bb;
    let knight_attacks = tables::knight_attacks(king_sq, us);
    checkers |= enemy_knights & knight_attacks;

    // Gold checks (including promoted pieces that move like gold)
    let gold_attacks = tables::gold_attacks(king_sq, them);
    let enemy_golds = pos.board.piece_bb[them as usize][PieceType::Gold as usize];
    checkers |= enemy_golds & gold_attacks;

    // Promoted pieces that move like gold
    let promoted_silvers =
        pos.board.piece_bb[them as usize][PieceType::Silver as usize] & pos.board.promoted_bb;
    let promoted_knights =
        pos.board.piece_bb[them as usize][PieceType::Knight as usize] & pos.board.promoted_bb;
    let promoted_lances =
        pos.board.piece_bb[them as usize][PieceType::Lance as usize] & pos.board.promoted_bb;
    let promoted_pawns =
        pos.board.piece_bb[them as usize][PieceType::Pawn as usize] & pos.board.promoted_bb;
    checkers |=
        (promoted_silvers | promoted_knights | promoted_lances | promoted_pawns) & gold_attacks;

    // Silver checks
    let enemy_silvers =
        pos.board.piece_bb[them as usize][PieceType::Silver as usize] & !pos.board.promoted_bb;
    let silver_attacks = tables::silver_attacks(king_sq, them);
    checkers |= enemy_silvers & silver_attacks;

    // Check sliding pieces for checks and pins

    // Rook checks and pins
    let enemy_rooks = pos.board.piece_bb[them as usize][PieceType::Rook as usize];
    for rook_sq in enemy_rooks {
        // Check if rook and king are aligned
        if rook_sq.file() == king_sq.file() || rook_sq.rank() == king_sq.rank() {
            let between = between_bb(rook_sq, king_sq);
            let blockers = between & pos.board.all_bb;

            if blockers.is_empty() {
                // Direct check
                checkers.set(rook_sq);
            } else if blockers.count_ones() == 1 {
                // Potential pin
                let blocker_sq = blockers.lsb().unwrap();
                if pos.board.occupied_bb[us as usize].test(blocker_sq) {
                    pinned.set(blocker_sq);
                    // Pin rays would be set here but we don't have mutable access
                }
            }
        }

        // Check promoted rook (Dragon) king-like attacks
        if pos.board.promoted_bb.test(rook_sq) {
            let king_attacks = tables::king_attacks(rook_sq);
            if king_attacks.test(king_sq) {
                checkers.set(rook_sq);
            }
        }
    }

    // Bishop checks and pins
    let enemy_bishops = pos.board.piece_bb[them as usize][PieceType::Bishop as usize];
    for bishop_sq in enemy_bishops {
        // Check if bishop and king are diagonally aligned
        let file_diff = (bishop_sq.file() as i8 - king_sq.file() as i8).abs();
        let rank_diff = (bishop_sq.rank() as i8 - king_sq.rank() as i8).abs();

        if file_diff == rank_diff && file_diff != 0 {
            let between = between_bb(bishop_sq, king_sq);
            let blockers = between & pos.board.all_bb;

            if blockers.is_empty() {
                // Direct check
                checkers.set(bishop_sq);
            } else if blockers.count_ones() == 1 {
                // Potential pin
                let blocker_sq = blockers.lsb().unwrap();
                if pos.board.occupied_bb[us as usize].test(blocker_sq) {
                    pinned.set(blocker_sq);
                }
            }
        }

        // Check promoted bishop (Horse) king-like attacks
        if pos.board.promoted_bb.test(bishop_sq) {
            let king_attacks = tables::king_attacks(bishop_sq);
            if king_attacks.test(king_sq) {
                checkers.set(bishop_sq);
            }
        }
    }

    // Lance checks and pins
    let enemy_lances =
        pos.board.piece_bb[them as usize][PieceType::Lance as usize] & !pos.board.promoted_bb;
    for lance_sq in enemy_lances {
        // Check if lance can attack in the direction of king
        let can_attack = match them {
            Color::Black => lance_sq.rank() > king_sq.rank() && lance_sq.file() == king_sq.file(),
            Color::White => lance_sq.rank() < king_sq.rank() && lance_sq.file() == king_sq.file(),
        };

        if can_attack {
            let between = between_bb(lance_sq, king_sq);
            let blockers = between & pos.board.all_bb;

            if blockers.is_empty() {
                checkers.set(lance_sq);
            } else if blockers.count_ones() == 1 {
                let blocker_sq = blockers.lsb().unwrap();
                if pos.board.occupied_bb[us as usize].test(blocker_sq) {
                    pinned.set(blocker_sq);
                }
            }
        }
    }

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

    #[test]
    fn test_drop_pawn_mate() {
        // Test position where dropping a pawn would be checkmate (illegal)
        // White king at 1i (0,8), surrounded by black pieces
        // Black can drop pawn at 1h (0,7) which would be checkmate
        let sfen = "k8/9/9/9/9/9/9/blr6/K8 b P 1";
        let pos = Position::from_sfen(sfen).unwrap();

        let gen = MoveGenerator::new();
        let result = gen.generate_all(&pos);
        assert!(result.is_ok());

        let moves = result.unwrap();

        // Check that P*1h (pawn drop at (0,7)) is NOT in the move list
        // This would be checkmate which is illegal
        let illegal_drop = Move::drop(PieceType::Pawn, Square::new(0, 7));
        assert!(
            !moves.iter().any(|&m| m == illegal_drop),
            "Drop pawn mate should not be generated"
        );
    }

    #[test]
    fn test_drop_pawn_check_allowed_if_not_mate() {
        // Test position where dropping a pawn gives check but is NOT mate (legal)
        // Black king at 9a, White king at 1i, Black has pawn in hand
        // White has no defending pieces, but king can escape
        let sfen = "k8/9/9/9/9/9/9/9/K8 b P 1";
        let pos = Position::from_sfen(sfen).unwrap();

        let gen = MoveGenerator::new();
        let result = gen.generate_all(&pos);
        assert!(result.is_ok());

        let moves = result.unwrap();

        // Check that P*1h (pawn drop at (0,7)) IS in the move list
        // This gives check but king can escape to 2i, so it's legal
        let legal_drop = Move::drop(PieceType::Pawn, Square::new(0, 7));
        assert!(
            moves.iter().any(|&m| m == legal_drop),
            "Drop pawn check should be allowed when not mate"
        );
    }
}
