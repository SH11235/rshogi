use crate::shogi::attacks::{
    between_bb, gold_attacks, king_attacks, knight_attacks, lance_attacks, pawn_attacks,
    silver_attacks, sliding_attacks, HAND_ORDER,
};
use crate::shogi::board_constants::{
    FILE_MASKS, RANK_1_2_MASK, RANK_1_MASK, RANK_8_9_MASK, RANK_9_MASK, SHOGI_BOARD_SIZE,
};
use crate::shogi::moves::{Move, MoveList};
use crate::shogi::{Bitboard, Color, PieceType, Position, Square};
use crate::shogi::{BOARD_FILES, BOARD_RANKS};

use super::error::MoveGenError;

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
    king_danger_squares: Bitboard, // Squares where king cannot move
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
            king_danger_squares: Bitboard::EMPTY, // 王が移動できない危険マス
            us,
            them,
            our_pieces,
            their_pieces,
            occupied,
        };

        // Always exclude enemy king square from non_king_check_mask
        if let Some(their_king_sq) = pos.board.king_square(them) {
            gen.non_king_check_mask &= !Bitboard::from_square(their_king_sq);
        }

        // Now calculate pin rays after we have the mutable gen
        gen.calculate_pin_rays();

        // Update check masks if in check
        if !gen.checkers.is_empty() {
            // When in check, only moves that block/capture checker are legal (for non-king pieces)
            if gen.checkers.count_ones() == 1 {
                // Single check - can block or capture
                let checker_sq = gen.checkers.lsb().unwrap();
                gen.non_king_check_mask = gen.checkers; // Can capture checker

                // Exclude enemy king square to prevent illegal king capture
                // (Already excluded in initialization, but we need to re-apply after setting to checkers)
                if let Some(their_king_sq) = gen.pos.board.king_square(them) {
                    gen.non_king_check_mask &= !Bitboard::from_square(their_king_sq);
                }

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

            // Calculate king danger squares for single check from sliding pieces
            // Note: Only mark squares in the direction AWAY from the checker.
            // Capturing the checker on its square may be legal and must NOT be
            // precluded by this mask. Safety is validated by is_attacked_by()
            // during king move generation.
            if gen.checkers.count_ones() == 1 {
                gen.calculate_king_danger_squares();
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
        let attacks = king_attacks(self.king_sq);
        let valid_targets = attacks & !self.our_pieces & !self.king_danger_squares;

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
                // Safety net: validate with full legality (accounts for discovered attacks after moving king)
                if self.pos.is_legal_move(mv) {
                    moves.push(mv);
                }
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
        for (piece_idx, &piece_type) in HAND_ORDER.iter().enumerate() {
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

                // Calculate all occupied files at once using bitboard operations
                let occupied_files = self.get_files_from_squares(our_pawns);
                valid &= !occupied_files;

                // Pawns cannot be dropped on the promotion rank
                match us {
                    Color::Black => valid &= !Bitboard(RANK_1_MASK),
                    Color::White => valid &= !Bitboard(RANK_9_MASK),
                }

                // Check for drop pawn mate (打ち歩詰め)
                // Only need to check squares where pawn would give check
                let them = us.opposite();
                if let Some(their_king_sq) = self.pos.board.king_square(them) {
                    #[cfg(test)]
                    println!("Checking drop pawn mate: their_king at {}", their_king_sq);
                    let potential_check_sq = match us {
                        Color::Black => {
                            // Black pawn attacks toward rank 0, so it checks from one rank above
                            if their_king_sq.rank() < 8 {
                                Some(Square::new(their_king_sq.file(), their_king_sq.rank() + 1))
                            } else {
                                None
                            }
                        }
                        Color::White => {
                            // White pawn attacks toward rank 8, so it checks from one rank below
                            if their_king_sq.rank() > 0 {
                                Some(Square::new(their_king_sq.file(), their_king_sq.rank() - 1))
                            } else {
                                None
                            }
                        }
                    };

                    if let Some(check_sq) = potential_check_sq {
                        #[cfg(test)]
                        println!(
                            "Potential check square: {}, valid: {}",
                            check_sq,
                            valid.test(check_sq)
                        );

                        if valid.test(check_sq) && self.is_drop_pawn_mate(check_sq, them) {
                            valid.clear(check_sq);
                        }
                    }
                }
            }
            PieceType::Lance => {
                // Lances cannot be dropped on the promotion rank
                match us {
                    Color::Black => valid &= !Bitboard(RANK_1_MASK),
                    Color::White => valid &= !Bitboard(RANK_9_MASK),
                }
            }
            PieceType::Knight => {
                // Knights cannot be dropped on the last two ranks
                match us {
                    Color::Black => valid &= !Bitboard(RANK_1_2_MASK),
                    Color::White => valid &= !Bitboard(RANK_8_9_MASK),
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
        let attacks = king_attacks(self.king_sq);
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
                if self.pos.is_legal_move(mv) {
                    moves.push(mv);
                }
            }
        }
    }

    /// Generate only king quiet moves
    fn generate_king_quiet(&mut self, moves: &mut MoveList) {
        let attacks = king_attacks(self.king_sq);
        let quiet = attacks & !self.occupied;

        for to_sq in quiet {
            if !self.is_attacked_by(to_sq, self.them) {
                let piece = self.pos.board.piece_on(self.king_sq).unwrap();
                let mv =
                    Move::normal_with_piece(self.king_sq, to_sq, false, piece.piece_type, None);
                if self.pos.is_legal_move(mv) {
                    moves.push(mv);
                }
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
        let attacks = king_attacks(self.king_sq);
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
                let attacks = pawn_attacks(from, us);
                if !(attacks & target_mask).is_empty() {
                    return true;
                }
            }
        }

        // 2. Lances
        let lances = self.pos.board.piece_bb[us as usize][PieceType::Lance as usize]
            & !self.pos.board.promoted_bb;
        for from in lances {
            // Early exit: pinned lances can only move forward along the pin ray
            if self.pinned.test(from) {
                let pin_ray = self.pin_rays[from.index()];
                // Check if pin direction matches lance movement direction
                let lance_file = from.file();
                let lance_forward_squares = match us {
                    Color::Black => {
                        // Black lance moves toward rank 0 (up)
                        // Get all squares on the file up to (but not including) current rank
                        let file_mask = Bitboard(FILE_MASKS[lance_file as usize]);
                        let rank_mask =
                            Bitboard((1u128 << (from.rank() as usize * BOARD_FILES)) - 1);
                        file_mask & rank_mask
                    }
                    Color::White => {
                        // White lance moves toward rank 8 (down)
                        // Get all squares on the file from rank+1 to rank 8
                        let file_mask = Bitboard(FILE_MASKS[lance_file as usize]);
                        let rank_mask =
                            !Bitboard((1u128 << ((from.rank() as usize + 1) * BOARD_FILES)) - 1);
                        file_mask & rank_mask & Bitboard::ALL
                    }
                };

                // If pin ray doesn't intersect with lance's forward movement, skip
                if (pin_ray & lance_forward_squares).is_empty() {
                    continue; // This lance cannot move
                }
            }

            // Get all potential lance moves (without considering blockers)
            let attacks = lance_attacks(from, us);

            // For each potential move, check if it's actually reachable
            for to in attacks {
                // Check if path is blocked
                let between = between_bb(from, to);
                if !(between & self.occupied).is_empty() {
                    continue; // Blocked by another piece
                }

                // Check if this is a valid target
                let target_bb = Bitboard::from_square(to);
                if self.pinned.test(from) {
                    // Must move along pin ray
                    if !(target_bb & self.pin_rays[from.index()] & target_mask).is_empty() {
                        return true;
                    }
                } else if !(target_bb & target_mask).is_empty() {
                    return true;
                }
            }
        }

        // 3. Knights
        let knights = self.pos.board.piece_bb[us as usize][PieceType::Knight as usize]
            & !self.pos.board.promoted_bb;
        for from in knights {
            // Knights cannot move if pinned (they move in L-shape)
            if !self.pinned.test(from) {
                let attacks = knight_attacks(from, us);
                if !(attacks & target_mask).is_empty() {
                    return true;
                }
            }
        }

        // 4. Silvers
        let silvers = self.pos.board.piece_bb[us as usize][PieceType::Silver as usize]
            & !self.pos.board.promoted_bb;
        for from in silvers {
            let attacks = silver_attacks(from, us);
            let valid_moves = if self.pinned.test(from) {
                attacks & self.pin_rays[from.index()]
            } else {
                attacks
            };
            if !(valid_moves & target_mask).is_empty() {
                return true;
            }
        }

        // 5. Golds (including promoted pieces)
        let golds = self.get_gold_like_pieces(us);
        for from in golds {
            let attacks = gold_attacks(from, us);
            let valid_moves = if self.pinned.test(from) {
                attacks & self.pin_rays[from.index()]
            } else {
                attacks
            };
            if !(valid_moves & target_mask).is_empty() {
                return true;
            }
        }

        // 6. Bishops
        let bishops = self.pos.board.piece_bb[us as usize][PieceType::Bishop as usize];
        for from in bishops {
            let attacks = sliding_attacks(from, self.occupied, PieceType::Bishop);
            let promoted = self.pos.board.promoted_bb.test(from);
            let all_attacks = if promoted {
                attacks | king_attacks(from)
            } else {
                attacks
            };

            let valid_moves = if self.pinned.test(from) {
                all_attacks & self.pin_rays[from.index()]
            } else {
                all_attacks
            };

            if !(valid_moves & target_mask).is_empty() {
                return true;
            }
        }

        // 7. Rooks
        let rooks = self.pos.board.piece_bb[us as usize][PieceType::Rook as usize];
        for from in rooks {
            let attacks = sliding_attacks(from, self.occupied, PieceType::Rook);
            let promoted = self.pos.board.promoted_bb.test(from);
            let all_attacks = if promoted {
                attacks | king_attacks(from)
            } else {
                attacks
            };

            let valid_moves = if self.pinned.test(from) {
                all_attacks & self.pin_rays[from.index()]
            } else {
                all_attacks
            };

            if !(valid_moves & target_mask).is_empty() {
                return true;
            }
        }

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

    /// Calculate danger squares where king cannot move when in check from sliding pieces
    fn calculate_king_danger_squares(&mut self) {
        if let Some(checker_sq) = self.checkers.lsb() {
            if let Some(checker_piece) = self.pos.board.piece_on(checker_sq) {
                match checker_piece.piece_type {
                    PieceType::Rook => {
                        // For rook, only squares along the line AWAY from the checker
                        if self.king_sq.file() == checker_sq.file() {
                            // Same file - move away from checker direction
                            let rank_step =
                                (self.king_sq.rank() as i8 - checker_sq.rank() as i8).signum();
                            let mut r = self.king_sq.rank() as i8 + rank_step;
                            while (0..BOARD_RANKS as i8).contains(&r) {
                                self.king_danger_squares
                                    .set(Square::new(self.king_sq.file(), r as u8));
                                r += rank_step;
                            }
                        } else if self.king_sq.rank() == checker_sq.rank() {
                            // Same rank - move away from checker direction
                            let file_step =
                                (self.king_sq.file() as i8 - checker_sq.file() as i8).signum();
                            let mut f = self.king_sq.file() as i8 + file_step;
                            while (0..BOARD_FILES as i8).contains(&f) {
                                self.king_danger_squares
                                    .set(Square::new(f as u8, self.king_sq.rank()));
                                f += file_step;
                            }
                        }
                    }
                    PieceType::Bishop => {
                        // For bishop, only squares along the diagonal AWAY from the checker
                        let file_step =
                            (self.king_sq.file() as i8 - checker_sq.file() as i8).signum();
                        let rank_step =
                            (self.king_sq.rank() as i8 - checker_sq.rank() as i8).signum();

                        // Move one step away from checker direction
                        let mut file = self.king_sq.file() as i8 + file_step;
                        let mut rank = self.king_sq.rank() as i8 + rank_step;
                        while (0..BOARD_FILES as i8).contains(&file)
                            && (0..BOARD_RANKS as i8).contains(&rank)
                        {
                            self.king_danger_squares.set(Square::new(file as u8, rank as u8));
                            file += file_step;
                            rank += rank_step;
                        }
                    }
                    PieceType::Lance => {
                        // For lance, only squares along the file AWAY from the checker
                        if self.king_sq.file() == checker_sq.file() {
                            let rank_step =
                                (self.king_sq.rank() as i8 - checker_sq.rank() as i8).signum();
                            let mut r = self.king_sq.rank() as i8 + rank_step;
                            while (0..BOARD_RANKS as i8).contains(&r) {
                                self.king_danger_squares
                                    .set(Square::new(self.king_sq.file(), r as u8));
                                r += rank_step;
                            }
                        }
                    }
                    _ => {} // Other pieces don't have ray attacks
                }
            }
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
        let pawn_attacks = pawn_attacks(sq, by.opposite()); // Attack from perspective of defender
        let enemy_pawns = self.pos.board.piece_bb[by as usize][PieceType::Pawn as usize]
            & !self.pos.board.promoted_bb;
        if !(pawn_attacks & enemy_pawns).is_empty() {
            return true;
        }

        // Check knight attacks
        let knight_attacks = knight_attacks(sq, by.opposite());
        let enemy_knights = self.pos.board.piece_bb[by as usize][PieceType::Knight as usize]
            & !self.pos.board.promoted_bb;
        if !(knight_attacks & enemy_knights).is_empty() {
            return true;
        }

        // Check king attacks
        let king_attacks = king_attacks(sq);
        let enemy_king = self.pos.board.piece_bb[by as usize][PieceType::King as usize];
        if !(king_attacks & enemy_king).is_empty() {
            return true;
        }

        // Check gold attacks (including promoted pieces)
        let gold_attacks = gold_attacks(sq, by);
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
        let silver_attacks = silver_attacks(sq, by);
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
            let to_bb = pawn_attacks(from, us) & !our_pieces & self.non_king_check_mask;
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
                // Branchless promotion check
                let black_promote = from.rank() <= 2 || to.rank() <= 2;
                let white_promote = from.rank() >= 6 || to.rank() >= 6;
                let can_promote = [black_promote, white_promote][us as usize];

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

        let to_bb = gold_attacks(from, us) & !our_pieces & self.non_king_check_mask;
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
            let attacks = lance_attacks(from, us);
            let blockers = attacks & self.pos.board.all_bb;

            // Find valid moves for lance
            let to_bb = if !blockers.is_empty() {
                // Find first blocker using bitboard operations
                let blocker_sq = match us {
                    Color::Black => {
                        // Black lance moves up (towards rank 0)
                        // Find the highest rank (closest to from) blocker
                        let blockers_on_path = blockers & attacks;
                        // Get the last bit set (highest rank = closest to from)
                        blockers_on_path.into_iter().last()
                    }
                    Color::White => {
                        // White lance moves down (towards rank 8)
                        // Find the lowest rank (closest to from) blocker
                        let blockers_on_path = blockers & attacks;
                        // Get the first bit set (lowest rank = closest to from)
                        blockers_on_path.lsb()
                    }
                };

                if let Some(blocker_sq) = blocker_sq {
                    // Get all squares between from and blocker
                    let moves_between = between_bb(from, blocker_sq);

                    // Include blocker square only if it contains an enemy piece
                    if self.their_pieces.test(blocker_sq) {
                        moves_between | Bitboard::from_square(blocker_sq)
                    } else {
                        moves_between
                    }
                } else {
                    // Fallback: no valid blocker found, use all attacks
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
                // Branchless promotion check
                let black_promote = from.rank() <= 2 || to.rank() <= 2;
                let white_promote = from.rank() >= 6 || to.rank() >= 6;
                let can_promote = [black_promote, white_promote][us as usize];

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
            let to_bb = knight_attacks(from, us) & !our_pieces & self.non_king_check_mask;
            for to in to_bb {
                if self.pinned.test(from) && !self.pin_rays[from.index()].test(to) {
                    continue;
                }

                let captured_type = self.get_captured_type(to);
                let must_promote = match us {
                    Color::Black => to.rank() <= 1, // ranks 0-1 (a-b)
                    Color::White => to.rank() >= 7, // ranks 7-8 (h-i)
                };
                // Branchless promotion check
                let black_promote = from.rank() <= 2 || to.rank() <= 2;
                let white_promote = from.rank() >= 6 || to.rank() >= 6;
                let can_promote = [black_promote, white_promote][us as usize];

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
            let to_bb = silver_attacks(from, us) & !our_pieces & self.non_king_check_mask;
            for to in to_bb {
                if self.pinned.test(from) && !self.pin_rays[from.index()].test(to) {
                    continue;
                }

                let captured_type = self.get_captured_type(to);
                // Branchless promotion check
                let black_promote = from.rank() <= 2 || to.rank() <= 2;
                let white_promote = from.rank() >= 6 || to.rank() >= 6;
                let can_promote = [black_promote, white_promote][us as usize];

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
                // Branchless promotion check
                let black_promote = from.rank() <= 2 || to.rank() <= 2;
                let white_promote = from.rank() >= 6 || to.rank() >= 6;
                let can_promote = [black_promote, white_promote][us as usize];

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
            let king_attacks = king_attacks(from);
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
                // Branchless promotion check
                let black_promote = from.rank() <= 2 || to.rank() <= 2;
                let white_promote = from.rank() >= 6 || to.rank() >= 6;
                let can_promote = [black_promote, white_promote][us as usize];

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
            let king_attacks = king_attacks(from);
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
        let pawn_attacks = pawn_attacks(sq, by.opposite()); // Attack from perspective of defender
        let enemy_pawns = self.pos.board.piece_bb[by as usize][PieceType::Pawn as usize]
            & !self.pos.board.promoted_bb
            & pieces;
        attackers |= pawn_attacks & enemy_pawns;

        // Knight attacks
        let knight_attacks = knight_attacks(sq, by.opposite());
        let enemy_knights = self.pos.board.piece_bb[by as usize][PieceType::Knight as usize]
            & !self.pos.board.promoted_bb
            & pieces;
        attackers |= knight_attacks & enemy_knights;

        // King attacks
        let king_attacks = king_attacks(sq);
        let enemy_king = self.pos.board.piece_bb[by as usize][PieceType::King as usize] & pieces;
        attackers |= king_attacks & enemy_king;

        // Gold attacks (including promoted pieces)
        let gold_attacks = gold_attacks(sq, by.opposite());
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
        let silver_attacks = silver_attacks(sq, by.opposite());
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
        let pawn_attacks = pawn_attacks(to, us);
        if !pawn_attacks.test(their_king_sq) {
            return false; // Not even a check
        }

        // Debug logging - use println for immediate output
        #[cfg(test)]
        {
            println!(
                "=== is_drop_pawn_mate called for pawn at {} with king at {} ===",
                to, their_king_sq
            );
        }

        // Early return if the pawn is not supported (king can just capture it)
        let pawn_support = self.attackers_to_with_occupancy(to, us, self.pos.board.all_bb);
        if pawn_support.is_empty() {
            #[cfg(test)]
            log::debug!("Pawn at {} is not supported", to);
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
            #[cfg(test)]
            println!("  King can safely capture pawn at {}", to);
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
            #[cfg(test)]
            println!("  Checking defender at {} for pawn at {}", def_sq, to);

            // Quick check: if defender is not aligned with king, it can't be pinned
            if !self.is_aligned(def_sq, their_king_sq) {
                #[cfg(test)]
                println!("    Defender at {} not aligned with king at {}", def_sq, their_king_sq);
                return false; // Not aligned = not pinned
            }

            // Check if removing defender exposes king to check from sliding pieces
            // Use current board occupancy (before pawn drop) to check pins
            let mut occ_without_defender = self.pos.board.all_bb;
            occ_without_defender.clear(def_sq);

            // Check only sliding pieces that could pin
            let attackers = self.sliding_attackers_to(their_king_sq, us, occ_without_defender);

            if attackers.is_empty() {
                #[cfg(test)]
                println!("    Defender at {} is not pinned", def_sq);
                return false; // Defender is not pinned and can capture the pawn
            }

            #[cfg(test)]
            println!("    Defender at {} is pinned by {:?}", def_sq, attackers);
        }

        // 3) Check if king has escape squares
        let king_attacks = king_attacks(their_king_sq);
        let their_pieces = self.pos.board.occupied_bb[them as usize];
        let escape_squares = king_attacks & !their_pieces;

        #[cfg(test)]
        {
            log::debug!(
                "Checking escapes: king at {}, escape_squares: {:?}",
                their_king_sq,
                escape_squares
            );
        }

        #[cfg(test)]
        {
            println!("  Escape checking with occupied_after_drop:");
            // Show key pieces
            if let Some(gold_1d) = self.pos.board.piece_on(Square::new(8, 3)) {
                println!("  Gold at 1d present: {:?}", gold_1d);
            }
        }

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

            #[cfg(test)]
            {
                let piece_on_escape = self.pos.board.piece_on(escape_sq);
                println!("  Checking escape to {}, piece there: {:?}", escape_sq, piece_on_escape);
            }

            let escape_attackers =
                self.attackers_to_with_occupancy(escape_sq, us, occ_after_escape);

            #[cfg(test)]
            {
                if self.pos.board.occupied_bb[us as usize].test(escape_sq) {
                    println!("  King would capture piece at {}", escape_sq);
                }
                // Debug: check if gold at 1d attacks 2c
                if escape_sq == Square::new(7, 2) {
                    // 2c
                    let gold_1d_sq = Square::new(8, 3); // 1d
                    let gold_attacks_2c = gold_attacks(gold_1d_sq, us);
                    println!("  Gold at 1d attacks: {:?}", gold_attacks_2c);
                    println!("  Does it include 2c? {}", gold_attacks_2c.test(escape_sq));

                    // Check if gold is in the bitboards
                    let gold_bb = self.pos.board.piece_bb[us as usize][PieceType::Gold as usize];
                    println!("  Gold bitboard includes 1d? {}", gold_bb.test(gold_1d_sq));

                    // Debug attackers_to calculation step by step
                    println!("  Debugging attackers_to for 2c:");
                    let gold_attackers = gold_attacks_2c & gold_bb;
                    println!("    Gold attackers: {:?}", gold_attackers);
                }
                println!(
                    "  Attackers to {}: {:?} (empty={})",
                    escape_sq,
                    escape_attackers,
                    escape_attackers.is_empty()
                );
            }

            if escape_attackers.is_empty() {
                #[cfg(test)]
                println!("  King has safe escape to {}", escape_sq);
                return false; // Safe escape exists
            }

            #[cfg(test)]
            println!("  Escape to {} is blocked by {:?}", escape_sq, escape_attackers);
        }

        true // No defense - it's mate
    }

    /// Calculate file mask for all squares in the bitboard
    fn get_files_from_squares(&self, squares: Bitboard) -> Bitboard {
        use crate::shogi::board_constants::FILE_MASKS;

        let mut files = Bitboard::EMPTY;

        // Check each file for any pieces
        for &file_mask in &FILE_MASKS {
            // If any square in this file contains a piece
            if (squares.0 & file_mask) != 0 {
                // Add the entire file to the result
                files.0 |= file_mask;
            }
        }

        files
    }

    /// Get all pieces that move like gold (gold + promoted pieces)
    fn get_gold_like_pieces(&self, us: Color) -> Bitboard {
        let golds = self.pos.board.piece_bb[us as usize][PieceType::Gold as usize];
        let promoted_pieces = self.pos.board.promoted_bb & self.our_pieces;
        golds
            | (promoted_pieces
                & (self.pos.board.piece_bb[us as usize][PieceType::Silver as usize]
                    | self.pos.board.piece_bb[us as usize][PieceType::Knight as usize]
                    | self.pos.board.piece_bb[us as usize][PieceType::Lance as usize]
                    | self.pos.board.piece_bb[us as usize][PieceType::Pawn as usize]))
    }

    /// Check if two squares are aligned (same file, rank, or diagonal)
    fn is_aligned(&self, sq1: Square, sq2: Square) -> bool {
        // Same file or rank
        sq1.file() == sq2.file()
            || sq1.rank() == sq2.rank()
            || // Same diagonal
        (sq1.file() as i8 - sq2.file() as i8).abs() == (sq1.rank() as i8 - sq2.rank() as i8).abs()
    }

    /// Get sliding piece attackers to a square with given occupancy
    fn sliding_attackers_to(&self, sq: Square, by: Color, occ: Bitboard) -> Bitboard {
        let mut attackers = Bitboard::EMPTY;

        // Rooks and dragons
        let rooks = self.pos.board.piece_bb[by as usize][PieceType::Rook as usize];
        let rook_attacks = sliding_attacks(sq, occ, PieceType::Rook);
        attackers |= rooks & rook_attacks;

        // Bishops and horses
        let bishops = self.pos.board.piece_bb[by as usize][PieceType::Bishop as usize];
        let bishop_attacks = sliding_attacks(sq, occ, PieceType::Bishop);
        attackers |= bishops & bishop_attacks;

        // Lances (directional) - special handling since they only move in one direction
        let lances = self.pos.board.piece_bb[by as usize][PieceType::Lance as usize]
            & !self.pos.board.promoted_bb;
        if !lances.is_empty() {
            // For each lance, check if it attacks the square through the given occupancy
            for lance_sq in lances {
                // Lance can only attack forward (toward enemy)
                let can_attack = match by {
                    Color::Black => lance_sq.file() == sq.file() && lance_sq.rank() > sq.rank(),
                    Color::White => lance_sq.file() == sq.file() && lance_sq.rank() < sq.rank(),
                };

                if can_attack {
                    // Check if path is clear
                    let between = between_bb(lance_sq, sq);
                    if (between & occ).is_empty() {
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
    let pawn_attacks = pawn_attacks(king_sq, us); // Where our pawns would attack from
    checkers |= enemy_pawns & pawn_attacks;

    // Knight checks
    let enemy_knights =
        pos.board.piece_bb[them as usize][PieceType::Knight as usize] & !pos.board.promoted_bb;
    let knight_attacks = knight_attacks(king_sq, us);
    checkers |= enemy_knights & knight_attacks;

    // Gold checks (including promoted pieces that move like gold)
    let gold_attacks = gold_attacks(king_sq, them);
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
    let silver_attacks = silver_attacks(king_sq, them);
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
            let king_attacks = king_attacks(rook_sq);
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
            let king_attacks = king_attacks(bishop_sq);
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
