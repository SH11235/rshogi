//! Move generation for shogi
//!
//! Generates all legal moves for a given position

use crate::{
    shogi::{Move, MoveList, ALL_PIECE_TYPES, ATTACK_TABLES},
    Bitboard, Color, PieceType, Position, Square,
};

/// Move generator
pub struct MoveGenImpl<'a> {
    pos: &'a Position,
    moves: MoveList,
    king_sq: Square,
    pub checkers: Bitboard,
    pinned: Bitboard,
    pin_rays: [Bitboard; 81],
}

/// Simple move generator (for compatibility)
pub struct MoveGen;

impl<'a> MoveGenImpl<'a> {
    /// Create new move generator for position
    pub fn new(pos: &'a Position) -> Self {
        let us = pos.side_to_move;
        let king_sq = pos.board.king_square(us).expect("King must exist");

        let mut gen = MoveGenImpl {
            pos,
            moves: MoveList::new(),
            king_sq,
            checkers: Bitboard::EMPTY,
            pinned: Bitboard::EMPTY,
            pin_rays: [Bitboard::EMPTY; 81],
        };

        // Calculate checkers and pins
        gen.calculate_checkers_and_pins();

        gen
    }

    /// Helper to get captured piece type at a square
    #[inline]
    fn get_captured_type(&self, to: Square) -> Option<PieceType> {
        self.pos.board.piece_on(to).map(|p| p.piece_type)
    }

    /// Generate all legal moves
    pub fn generate_all(&mut self) -> MoveList {
        self.moves.clear();

        let us = self.pos.side_to_move;
        let them = us.opposite();
        let _our_pieces = self.pos.board.occupied_bb[us as usize];
        let _their_pieces = self.pos.board.occupied_bb[them as usize];
        let _all_pieces = self.pos.board.all_bb;

        // If in double check, only king moves are legal
        if self.checkers.count_ones() > 1 {
            self.generate_king_moves();
            return std::mem::take(&mut self.moves);
        }

        // Generate moves for each piece type
        for &piece_type_enum in &ALL_PIECE_TYPES {
            let piece_type = piece_type_enum as usize;
            let mut pieces = self.pos.board.piece_bb[us as usize][piece_type];

            while let Some(from) = pieces.pop_lsb() {
                // Check if the piece is promoted
                let piece = self.pos.board.piece_on(from);
                let promoted = piece.map(|p| p.promoted).unwrap_or(false);

                match piece_type_enum {
                    PieceType::King => self.generate_king_moves_from(from),
                    PieceType::Rook => self.generate_sliding_moves(from, piece_type_enum, promoted),
                    PieceType::Bishop => {
                        self.generate_sliding_moves(from, piece_type_enum, promoted)
                    }
                    PieceType::Gold => self.generate_gold_moves(from, promoted),
                    PieceType::Silver => self.generate_silver_moves(from, promoted),
                    PieceType::Knight => self.generate_knight_moves(from, promoted),
                    PieceType::Lance => self.generate_lance_moves(from, promoted),
                    PieceType::Pawn => self.generate_pawn_moves(from, promoted),
                }
            }
        }

        // Generate drop moves
        // When in check, drops can still be legal if they block the check
        self.generate_drop_moves();

        // Note: promoted pieces are already handled in the piece-specific methods

        // Filter out any moves that would capture the enemy king (should not happen)
        let enemy_king_bb = self.pos.board.piece_bb[them as usize][PieceType::King as usize];
        if let Some(enemy_king_sq) = enemy_king_bb.lsb() {
            self.moves.as_mut_slice().retain(|m| m.to() != enemy_king_sq);
        }

        std::mem::take(&mut self.moves)
    }

    /// Calculate checkers and pinned pieces
    fn calculate_checkers_and_pins(&mut self) {
        let us = self.pos.side_to_move;
        let them = us.opposite();
        let king_sq = self.king_sq;
        let our_pieces = self.pos.board.occupied_bb[us as usize];
        let _their_pieces = self.pos.board.occupied_bb[them as usize];

        // Check attacks from each enemy piece type

        // Pawn checks
        let enemy_pawns = self.pos.board.piece_bb[them as usize][PieceType::Pawn as usize];
        let pawn_attacks = ATTACK_TABLES.pawn_attacks(king_sq, them);
        self.checkers |= enemy_pawns & pawn_attacks;

        // Knight checks
        let enemy_knights = self.pos.board.piece_bb[them as usize][PieceType::Knight as usize];
        let knight_attacks = ATTACK_TABLES.knight_attacks(king_sq, them);
        self.checkers |= enemy_knights & knight_attacks;

        // Gold/promoted pieces checks
        let gold_attacks = ATTACK_TABLES.gold_attacks(king_sq, them);
        let enemy_golds = self.pos.board.piece_bb[them as usize][PieceType::Gold as usize];
        self.checkers |= enemy_golds & gold_attacks;

        // Check promoted pieces that move like gold
        let promoted_silvers = self.pos.board.piece_bb[them as usize][PieceType::Silver as usize]
            & self.pos.board.promoted_bb;
        let promoted_knights = self.pos.board.piece_bb[them as usize][PieceType::Knight as usize]
            & self.pos.board.promoted_bb;
        let promoted_lances = self.pos.board.piece_bb[them as usize][PieceType::Lance as usize]
            & self.pos.board.promoted_bb;
        let promoted_pawns = self.pos.board.piece_bb[them as usize][PieceType::Pawn as usize]
            & self.pos.board.promoted_bb;
        self.checkers |=
            (promoted_silvers | promoted_knights | promoted_lances | promoted_pawns) & gold_attacks;

        // Silver checks
        let enemy_silvers = self.pos.board.piece_bb[them as usize][PieceType::Silver as usize]
            & !self.pos.board.promoted_bb;
        let silver_attacks = ATTACK_TABLES.silver_attacks(king_sq, them);
        self.checkers |= enemy_silvers & silver_attacks;

        // Lance checks and pins
        let enemy_lances = self.pos.board.piece_bb[them as usize][PieceType::Lance as usize]
            & !self.pos.board.promoted_bb;
        let mut lance_bb = enemy_lances;
        while let Some(lance_sq) = lance_bb.pop_lsb() {
            // Check if lance can attack in the direction of king
            let can_attack = match them {
                Color::Black => {
                    lance_sq.rank() > king_sq.rank() && lance_sq.file() == king_sq.file()
                }
                Color::White => {
                    lance_sq.rank() < king_sq.rank() && lance_sq.file() == king_sq.file()
                }
            };

            if can_attack {
                let between = self.between_bb(lance_sq, king_sq);
                let blockers = between & self.pos.board.all_bb;

                if blockers.is_empty() {
                    self.checkers.set(lance_sq);
                } else if blockers.count_ones() == 1 {
                    let blocker_sq = blockers.lsb().unwrap();
                    if our_pieces.test(blocker_sq) {
                        self.pinned.set(blocker_sq);
                        self.pin_rays[blocker_sq.0 as usize] =
                            between | Bitboard::from_square(lance_sq);
                    }
                }
            }
        }

        // Sliding pieces (Rook/Bishop) checks and pins
        let enemy_rooks = self.pos.board.piece_bb[them as usize][PieceType::Rook as usize];
        let enemy_bishops = self.pos.board.piece_bb[them as usize][PieceType::Bishop as usize];

        // Dragon (promoted rook) moves like rook + king
        let dragons = enemy_rooks & self.pos.board.promoted_bb;
        let dragon_king_attacks = ATTACK_TABLES.king_attacks(king_sq);
        self.checkers |= dragons & dragon_king_attacks;

        // Horse (promoted bishop) moves like bishop + king
        let horses = enemy_bishops & self.pos.board.promoted_bb;
        self.checkers |= horses & dragon_king_attacks;

        // Check rook/dragon sliding attacks and pins
        let mut rook_bb = enemy_rooks;
        while let Some(rook_sq) = rook_bb.pop_lsb() {
            if self.is_aligned_rook(rook_sq, king_sq) {
                let between = self.between_bb(rook_sq, king_sq);
                let blockers = between & self.pos.board.all_bb;

                if blockers.is_empty() {
                    self.checkers.set(rook_sq);
                } else if blockers.count_ones() == 1 {
                    let blocker_sq = blockers.lsb().unwrap();
                    if our_pieces.test(blocker_sq) {
                        self.pinned.set(blocker_sq);
                        self.pin_rays[blocker_sq.0 as usize] =
                            between | Bitboard::from_square(rook_sq);
                    }
                }
            }
        }

        // Check bishop/horse sliding attacks and pins
        let mut bishop_bb = enemy_bishops;
        while let Some(bishop_sq) = bishop_bb.pop_lsb() {
            if self.is_aligned_bishop(bishop_sq, king_sq) {
                let between = self.between_bb(bishop_sq, king_sq);
                let blockers = between & self.pos.board.all_bb;

                if blockers.is_empty() {
                    self.checkers.set(bishop_sq);
                } else if blockers.count_ones() == 1 {
                    let blocker_sq = blockers.lsb().unwrap();
                    if our_pieces.test(blocker_sq) {
                        self.pinned.set(blocker_sq);
                        self.pin_rays[blocker_sq.0 as usize] =
                            between | Bitboard::from_square(bishop_sq);
                    }
                }
            }
        }
    }

    /// Generate king moves
    fn generate_king_moves(&mut self) {
        self.generate_king_moves_from(self.king_sq);
    }

    /// Generate king moves from a square
    fn generate_king_moves_from(&mut self, from: Square) {
        let us = self.pos.side_to_move;
        let attacks = ATTACK_TABLES.king_attacks(from);
        let targets = attacks & !self.pos.board.occupied_bb[us as usize];

        let mut moves = targets;
        while let Some(to) = moves.pop_lsb() {
            // Check if king would be in check on target square
            if !self.would_be_in_check(from, to) {
                let captured_type = self.get_captured_type(to);
                self.moves.push(Move::normal_with_piece(
                    from,
                    to,
                    false,
                    PieceType::King,
                    captured_type,
                ));
            }
        }
    }

    /// Generate moves for gold (and promoted pieces that move like gold)
    fn generate_gold_moves(&mut self, from: Square, promoted: bool) {
        let us = self.pos.side_to_move;
        let attacks = ATTACK_TABLES.gold_attacks(from, us);
        let targets = attacks & !self.pos.board.occupied_bb[us as usize];

        self.add_moves(from, targets, promoted);
    }

    /// Generate moves for silver
    fn generate_silver_moves(&mut self, from: Square, promoted: bool) {
        if promoted {
            self.generate_gold_moves(from, true);
            return;
        }

        let us = self.pos.side_to_move;
        let attacks = ATTACK_TABLES.silver_attacks(from, us);
        let targets = attacks & !self.pos.board.occupied_bb[us as usize];

        // Never capture enemy king
        let them = us.opposite();
        let enemy_king_bb = self.pos.board.piece_bb[them as usize][PieceType::King as usize];
        let valid_targets = targets & !enemy_king_bb;

        let mut moves = valid_targets;
        while let Some(to) = moves.pop_lsb() {
            let captured_type = self.get_captured_type(to);
            // Check if can promote
            if self.can_promote(from, to, us) {
                self.moves.push(Move::normal_with_piece(
                    from,
                    to,
                    true,
                    PieceType::Silver,
                    captured_type,
                ));
                // Silver promotion is optional
                self.moves.push(Move::normal_with_piece(
                    from,
                    to,
                    false,
                    PieceType::Silver,
                    captured_type,
                ));
            } else {
                self.moves.push(Move::normal_with_piece(
                    from,
                    to,
                    false,
                    PieceType::Silver,
                    captured_type,
                ));
            }
        }
    }

    /// Generate moves for knight
    fn generate_knight_moves(&mut self, from: Square, promoted: bool) {
        if promoted {
            self.generate_gold_moves(from, true);
            return;
        }

        let us = self.pos.side_to_move;
        let attacks = ATTACK_TABLES.knight_attacks(from, us);
        let targets = attacks & !self.pos.board.occupied_bb[us as usize];

        let mut moves = targets;
        while let Some(to) = moves.pop_lsb() {
            let captured_type = self.get_captured_type(to);
            // Knight must promote if it can't move further
            let must_promote = match us {
                Color::Black => to.rank() <= 1, // Black can't move from rank 0-1
                Color::White => to.rank() >= 7, // White can't move from rank 7-8
            };

            if must_promote {
                self.moves.push(Move::normal_with_piece(
                    from,
                    to,
                    true,
                    PieceType::Knight,
                    captured_type,
                ));
            } else if self.can_promote(from, to, us) {
                self.moves.push(Move::normal_with_piece(
                    from,
                    to,
                    true,
                    PieceType::Knight,
                    captured_type,
                ));
                self.moves.push(Move::normal_with_piece(
                    from,
                    to,
                    false,
                    PieceType::Knight,
                    captured_type,
                ));
            } else {
                self.moves.push(Move::normal_with_piece(
                    from,
                    to,
                    false,
                    PieceType::Knight,
                    captured_type,
                ));
            }
        }
    }

    /// Generate moves for lance
    fn generate_lance_moves(&mut self, from: Square, promoted: bool) {
        if promoted {
            self.generate_gold_moves(from, true);
            return;
        }

        let us = self.pos.side_to_move;
        let file = from.file();
        let rank = from.rank() as i8;

        // Lance moves in one direction until blocked
        let (start, end, step) = match us {
            Color::Black => (rank - 1, -1, -1), // Black moves towards rank 0 (up the board)
            Color::White => (rank + 1, 9, 1),   // White moves towards rank 8 (down the board)
        };

        let mut r = start;
        while r != end {
            let sq = Square::new(file, r as u8);

            // Check if square is occupied
            if self.pos.board.all_bb.test(sq) {
                // Can capture enemy piece
                if !self.pos.board.occupied_bb[us as usize].test(sq) {
                    let to = sq;
                    // Lance must promote if it can't move further
                    let must_promote = match us {
                        Color::Black => to.rank() == 0, // Black reaches top (rank 0)
                        Color::White => to.rank() == 8, // White reaches bottom (rank 8)
                    };

                    let captured_type = self.get_captured_type(to);
                    if must_promote {
                        self.moves.push(Move::normal_with_piece(
                            from,
                            to,
                            true,
                            PieceType::Lance,
                            captured_type,
                        ));
                    } else if self.can_promote(from, to, us) {
                        self.moves.push(Move::normal_with_piece(
                            from,
                            to,
                            true,
                            PieceType::Lance,
                            captured_type,
                        ));
                        self.moves.push(Move::normal_with_piece(
                            from,
                            to,
                            false,
                            PieceType::Lance,
                            captured_type,
                        ));
                    } else {
                        self.moves.push(Move::normal_with_piece(
                            from,
                            to,
                            false,
                            PieceType::Lance,
                            captured_type,
                        ));
                    }
                }
                break; // Blocked, can't move further
            } else {
                // Empty square
                let to = sq;
                // Lance must promote if it can't move further
                let must_promote = match us {
                    Color::Black => to.rank() == 0, // Black reaches top (rank 0)
                    Color::White => to.rank() == 8, // White reaches bottom (rank 8)
                };

                let captured_type = self.get_captured_type(to);
                if must_promote {
                    self.moves.push(Move::normal_with_piece(
                        from,
                        to,
                        true,
                        PieceType::Lance,
                        captured_type,
                    ));
                } else if self.can_promote(from, to, us) {
                    self.moves.push(Move::normal_with_piece(
                        from,
                        to,
                        true,
                        PieceType::Lance,
                        captured_type,
                    ));
                    self.moves.push(Move::normal_with_piece(
                        from,
                        to,
                        false,
                        PieceType::Lance,
                        captured_type,
                    ));
                } else {
                    self.moves.push(Move::normal_with_piece(
                        from,
                        to,
                        false,
                        PieceType::Lance,
                        captured_type,
                    ));
                }
            }

            r += step;
        }
    }

    /// Generate moves for pawn
    fn generate_pawn_moves(&mut self, from: Square, promoted: bool) {
        if promoted {
            self.generate_gold_moves(from, true);
            return;
        }

        let us = self.pos.side_to_move;
        let attacks = ATTACK_TABLES.pawn_attacks(from, us);
        let targets = attacks & !self.pos.board.occupied_bb[us as usize];

        let mut moves = targets;
        while let Some(to) = moves.pop_lsb() {
            let captured_type = self.get_captured_type(to);
            // Pawn must promote if it can't move further
            let must_promote = match us {
                Color::Black => to.rank() == 0, // Black reaches top (rank 0)
                Color::White => to.rank() == 8, // White reaches bottom (rank 8)
            };

            if must_promote {
                self.moves.push(Move::normal_with_piece(
                    from,
                    to,
                    true,
                    PieceType::Pawn,
                    captured_type,
                ));
            } else if self.can_promote(from, to, us) {
                self.moves.push(Move::normal_with_piece(
                    from,
                    to,
                    true,
                    PieceType::Pawn,
                    captured_type,
                ));
                self.moves.push(Move::normal_with_piece(
                    from,
                    to,
                    false,
                    PieceType::Pawn,
                    captured_type,
                ));
            } else {
                self.moves.push(Move::normal_with_piece(
                    from,
                    to,
                    false,
                    PieceType::Pawn,
                    captured_type,
                ));
            }
        }
    }

    /// Generate sliding moves (Rook/Bishop)
    fn generate_sliding_moves(&mut self, from: Square, piece_type: PieceType, promoted: bool) {
        let us = self.pos.side_to_move;
        let mut attacks = ATTACK_TABLES.sliding_attacks(from, self.pos.board.all_bb, piece_type);

        // Dragon and Horse have additional king-like moves
        if promoted {
            attacks |= ATTACK_TABLES.king_attacks(from);
        }

        let targets = attacks & !self.pos.board.occupied_bb[us as usize];

        let mut moves = targets;
        while let Some(to) = moves.pop_lsb() {
            let captured_type = self.get_captured_type(to);
            if !promoted && self.can_promote(from, to, us) {
                self.moves
                    .push(Move::normal_with_piece(from, to, true, piece_type, captured_type));
                self.moves.push(Move::normal_with_piece(
                    from,
                    to,
                    false,
                    piece_type,
                    captured_type,
                ));
            } else {
                self.moves.push(Move::normal_with_piece(
                    from,
                    to,
                    false,
                    piece_type,
                    captured_type,
                ));
            }
        }
    }

    /// Generate drop moves
    fn generate_drop_moves(&mut self) {
        let us = self.pos.side_to_move;
        let empty_squares = !self.pos.board.all_bb;

        // If in check, only consider drops that block the check
        let drop_targets = if self.checkers.count_ones() == 1 {
            // Single check - can block
            let checker_sq = self.checkers.lsb().unwrap();
            let king_sq = self.king_sq;

            // For sliding pieces, we can drop on squares between checker and king
            if self.is_sliding_piece(checker_sq) {
                self.between_bb(checker_sq, king_sq) & empty_squares
            } else {
                // Non-sliding pieces can't be blocked
                Bitboard::EMPTY
            }
        } else if self.checkers.count_ones() > 1 {
            // Double check - no drops can help
            Bitboard::EMPTY
        } else {
            // Not in check - can drop anywhere valid
            empty_squares
        };

        // Check each piece type in hand
        for piece_idx in 0..7 {
            let piece_type = match piece_idx {
                0 => PieceType::Rook,
                1 => PieceType::Bishop,
                2 => PieceType::Gold,
                3 => PieceType::Silver,
                4 => PieceType::Knight,
                5 => PieceType::Lance,
                6 => PieceType::Pawn,
                _ => unreachable!(),
            };

            let count = self.pos.hands[us as usize][piece_idx];
            if count == 0 {
                continue;
            }

            // Get valid drop squares for this piece type
            let valid_drops = self.get_valid_drop_squares(piece_type, empty_squares) & drop_targets;

            let mut drops = valid_drops;
            while let Some(to) = drops.pop_lsb() {
                self.moves.push(Move::drop(piece_type, to));
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
                for file in 0..9 {
                    if self.has_pawn_on_file(file, us) {
                        // Remove all squares on this file
                        for rank in 0..9 {
                            valid.clear(Square::new(file, rank));
                        }
                    }
                }

                // Pawns cannot be dropped on last rank
                match us {
                    Color::Black => {
                        for file in 0..9 {
                            valid.clear(Square::new(file, 8)); // Black's last rank
                        }
                    }
                    Color::White => {
                        for file in 0..9 {
                            valid.clear(Square::new(file, 0)); // White's last rank
                        }
                    }
                }

                // Check for illegal pawn drop checkmate
                let them = us.opposite();
                let their_king_sq = self.pos.board.king_square(them);
                if let Some(king_sq) = their_king_sq {
                    // Check if any pawn drop would give check to enemy king
                    // A pawn gives check if it's one square in front of the king (from the pawn's perspective)
                    let pawn_check_sq = match us {
                        Color::Black => {
                            // Black pawns move towards rank 0, so they give check from rank+1
                            if king_sq.rank() < 8 {
                                Some(Square::new(king_sq.file(), king_sq.rank() + 1))
                            } else {
                                None
                            }
                        }
                        Color::White => {
                            // White pawns move towards rank 8, so they give check from rank-1
                            if king_sq.rank() > 0 {
                                Some(Square::new(king_sq.file(), king_sq.rank() - 1))
                            } else {
                                None
                            }
                        }
                    };

                    // If the pawn drop would give check, verify it's not checkmate
                    if let Some(check_sq) = pawn_check_sq {
                        if valid.test(check_sq) {
                            let is_mate = self.is_drop_pawn_mate(check_sq, them);
                            if is_mate {
                                valid.clear(check_sq);
                            }
                        }
                    }
                }
            }
            PieceType::Lance => {
                // Lances cannot be dropped on last rank
                match us {
                    Color::Black => {
                        for file in 0..9 {
                            valid.clear(Square::new(file, 8)); // Black's last rank
                        }
                    }
                    Color::White => {
                        for file in 0..9 {
                            valid.clear(Square::new(file, 0)); // White's last rank
                        }
                    }
                }
            }
            PieceType::Knight => {
                // Knights cannot be dropped on last two ranks
                match us {
                    Color::Black => {
                        for file in 0..9 {
                            valid.clear(Square::new(file, 7)); // Black can't drop on rank 7-8
                            valid.clear(Square::new(file, 8));
                        }
                    }
                    Color::White => {
                        for file in 0..9 {
                            valid.clear(Square::new(file, 0)); // White can't drop on rank 0-1
                            valid.clear(Square::new(file, 1));
                        }
                    }
                }
            }
            _ => {} // Other pieces can be dropped anywhere empty
        }

        valid
    }

    /// Check if we have a pawn on the given file
    fn has_pawn_on_file(&self, file: u8, color: Color) -> bool {
        let pawns = self.pos.board.piece_bb[color as usize][PieceType::Pawn as usize];
        for rank in 0..9 {
            if pawns.test(Square::new(file, rank)) {
                return true;
            }
        }
        false
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
        let pawn_attacks = ATTACK_TABLES.pawn_attacks(to, us);
        if !pawn_attacks.test(their_king_sq) {
            return false; // Not even a check
        }

        // Check if the pawn has support (if not, king can capture it)
        let pawn_supporters = self.attackers_to(to, us);
        if pawn_supporters.is_empty() {
            return false; // No support - king can capture the pawn
        }

        // Check if any piece (except king, pawn, lance) can capture the dropped pawn
        let defenders = self.attackers_to_except_king_pawn_lance(to, them);

        // Check if any unpinned piece can capture
        if defenders.count_ones() > 0 {
            // Calculate pinned pieces for the defending side
            let pinned = self.calculate_pinned_pieces(them);

            // Check each defender individually
            let mut def_bb = defenders;
            while let Some(def_sq) = def_bb.pop_lsb() {
                // If not pinned, can capture
                if !pinned.test(def_sq) {
                    return false; // Can capture the pawn
                }

                // If pinned, the piece cannot capture the pawn
                // In drop pawn mate, pinned pieces are considered unable to defend
            }
        }

        // Check if king has any escape squares
        let king_attacks = ATTACK_TABLES.king_attacks(their_king_sq);
        let their_pieces = self.pos.board.occupied_bb[them as usize];
        let mut escape_squares = king_attacks & !their_pieces;

        // Remove the pawn square from escape squares (can't escape by capturing the pawn)
        escape_squares &= !Bitboard::from_square(to);

        // Simulate position after pawn drop
        let occupied_after_drop = self.pos.board.all_bb | Bitboard::from_square(to);

        let mut escapes = escape_squares;
        while let Some(escape_sq) = escapes.pop_lsb() {
            // Check if escape square is attacked by any enemy piece
            let attackers = self.attackers_to_with_occupancy(escape_sq, us, occupied_after_drop);
            if attackers.is_empty() {
                return false; // King can escape
            }
        }

        // No escapes, no captures - it's mate
        true
    }

    /// Get all pieces (except king, pawn, lance) of given color that can attack a square
    fn attackers_to_except_king_pawn_lance(&self, sq: Square, color: Color) -> Bitboard {
        let occupied = self.pos.board.all_bb;
        let mut attackers = Bitboard::EMPTY;

        // Knights
        let knights = self.pos.board.piece_bb[color as usize][PieceType::Knight as usize];
        let promoted_knights = self.pos.board.promoted_bb & knights;
        let unpromoted_knights = knights & !self.pos.board.promoted_bb;

        attackers |= unpromoted_knights & ATTACK_TABLES.knight_attacks(sq, color.opposite());
        attackers |= promoted_knights & ATTACK_TABLES.gold_attacks(sq, color.opposite()); // Promoted knight moves like gold

        // Silvers
        let silvers = self.pos.board.piece_bb[color as usize][PieceType::Silver as usize];
        let promoted_silvers = self.pos.board.promoted_bb & silvers;
        let unpromoted_silvers = silvers & !self.pos.board.promoted_bb;

        attackers |= unpromoted_silvers & ATTACK_TABLES.silver_attacks(sq, color.opposite());
        attackers |= promoted_silvers & ATTACK_TABLES.gold_attacks(sq, color.opposite()); // Promoted silver moves like gold

        // Golds
        let golds = self.pos.board.piece_bb[color as usize][PieceType::Gold as usize];
        attackers |= golds & ATTACK_TABLES.gold_attacks(sq, color.opposite());

        // Bishops
        let bishops = self.pos.board.piece_bb[color as usize][PieceType::Bishop as usize];
        let bishop_attacks = ATTACK_TABLES.sliding_attacks(sq, occupied, PieceType::Bishop);
        attackers |= bishops & bishop_attacks;

        // Rooks
        let rooks = self.pos.board.piece_bb[color as usize][PieceType::Rook as usize];
        let rook_attacks = ATTACK_TABLES.sliding_attacks(sq, occupied, PieceType::Rook);
        attackers |= rooks & rook_attacks;

        attackers
    }

    /// Calculate pinned pieces for a given color
    fn calculate_pinned_pieces(&self, color: Color) -> Bitboard {
        let king_sq = match self.pos.board.king_square(color) {
            Some(sq) => sq,
            None => return Bitboard::EMPTY,
        };

        let them = color.opposite();
        let mut pinned = Bitboard::EMPTY;

        // Check for pins by rooks
        let rooks = self.pos.board.piece_bb[them as usize][PieceType::Rook as usize];
        let mut rook_bb = rooks;

        while let Some(rook_sq) = rook_bb.pop_lsb() {
            // Check if rook is on same rank or file as king
            let rank_diff = rook_sq.rank() as i8 - king_sq.rank() as i8;
            let file_diff = rook_sq.file() as i8 - king_sq.file() as i8;

            if rank_diff != 0 && file_diff != 0 {
                continue; // Not on same ray
            }

            // Get the ray between rook and king
            let between = ATTACK_TABLES.between_bb(rook_sq, king_sq);
            let blockers = between & self.pos.board.all_bb;

            // Exactly one blocker means it's pinned
            if blockers.count_ones() == 1 {
                let blocker_sq = blockers.lsb().unwrap();
                let blocker_bb = Bitboard::from_square(blocker_sq);

                // Blocker must be our piece
                if (self.pos.board.occupied_bb[color as usize] & blocker_bb).count_ones() > 0 {
                    pinned |= blocker_bb;
                }
            }
        }

        // Check for pins by bishops
        let bishops = self.pos.board.piece_bb[them as usize][PieceType::Bishop as usize];
        let mut bishop_bb = bishops;

        while let Some(bishop_sq) = bishop_bb.pop_lsb() {
            // Check if bishop is on same diagonal as king
            let rank_diff = bishop_sq.rank() as i8 - king_sq.rank() as i8;
            let file_diff = bishop_sq.file() as i8 - king_sq.file() as i8;

            if rank_diff.abs() != file_diff.abs() {
                continue; // Not on same diagonal
            }

            // Get the ray between bishop and king
            let between = ATTACK_TABLES.between_bb(bishop_sq, king_sq);
            let blockers = between & self.pos.board.all_bb;

            // Exactly one blocker means it's pinned
            if blockers.count_ones() == 1 {
                let blocker_sq = blockers.lsb().unwrap();
                let blocker_bb = Bitboard::from_square(blocker_sq);

                // Blocker must be our piece
                if (self.pos.board.occupied_bb[color as usize] & blocker_bb).count_ones() > 0 {
                    pinned |= blocker_bb;
                }
            }
        }

        pinned
    }

    /// Get all pieces of given color that can attack a square with custom occupancy
    fn attackers_to_with_occupancy(
        &self,
        sq: Square,
        color: Color,
        occupied: Bitboard,
    ) -> Bitboard {
        let mut attackers = Bitboard::EMPTY;

        // Pawns (unpromoted)
        let pawns = self.pos.board.piece_bb[color as usize][PieceType::Pawn as usize]
            & !self.pos.board.promoted_bb;
        let pawn_attacks = ATTACK_TABLES.pawn_attacks(sq, color.opposite());
        attackers |= pawns & pawn_attacks;

        // Knights (unpromoted)
        let knights = self.pos.board.piece_bb[color as usize][PieceType::Knight as usize]
            & !self.pos.board.promoted_bb;
        let knight_attacks = ATTACK_TABLES.knight_attacks(sq, color.opposite());
        attackers |= knights & knight_attacks;

        // Golds and promoted pieces that move like gold
        let gold_attacks = ATTACK_TABLES.gold_attacks(sq, color.opposite());
        let golds = self.pos.board.piece_bb[color as usize][PieceType::Gold as usize];
        let promoted_silvers = self.pos.board.piece_bb[color as usize][PieceType::Silver as usize]
            & self.pos.board.promoted_bb;
        let promoted_knights = self.pos.board.piece_bb[color as usize][PieceType::Knight as usize]
            & self.pos.board.promoted_bb;
        let promoted_lances = self.pos.board.piece_bb[color as usize][PieceType::Lance as usize]
            & self.pos.board.promoted_bb;
        let promoted_pawns = self.pos.board.piece_bb[color as usize][PieceType::Pawn as usize]
            & self.pos.board.promoted_bb;
        attackers |=
            (golds | promoted_silvers | promoted_knights | promoted_lances | promoted_pawns)
                & gold_attacks;

        // Silvers (unpromoted)
        let silvers = self.pos.board.piece_bb[color as usize][PieceType::Silver as usize]
            & !self.pos.board.promoted_bb;
        let silver_attacks = ATTACK_TABLES.silver_attacks(sq, color.opposite());
        attackers |= silvers & silver_attacks;

        // Kings
        let kings = self.pos.board.piece_bb[color as usize][PieceType::King as usize];
        let king_attacks = ATTACK_TABLES.king_attacks(sq);
        attackers |= kings & king_attacks;

        // Lances (unpromoted)
        let lances = self.pos.board.piece_bb[color as usize][PieceType::Lance as usize]
            & !self.pos.board.promoted_bb;
        let mut lance_bb = lances;
        while let Some(lance_sq) = lance_bb.pop_lsb() {
            let can_attack = match color {
                Color::Black => lance_sq.rank() > sq.rank() && lance_sq.file() == sq.file(),
                Color::White => lance_sq.rank() < sq.rank() && lance_sq.file() == sq.file(),
            };
            if can_attack {
                let between = self.between_bb(lance_sq, sq);
                if (between & occupied).is_empty() {
                    attackers.set(lance_sq);
                }
            }
        }

        // Rooks and Dragons with custom occupancy
        let rooks = self.pos.board.piece_bb[color as usize][PieceType::Rook as usize];
        let rook_attacks = ATTACK_TABLES.sliding_attacks(sq, occupied, PieceType::Rook);
        attackers |= rooks & rook_attacks;

        // Dragons also have king-like moves
        let dragons = rooks & self.pos.board.promoted_bb;
        attackers |= dragons & king_attacks;

        // Bishops and Horses with custom occupancy
        let bishops = self.pos.board.piece_bb[color as usize][PieceType::Bishop as usize];
        let bishop_attacks = ATTACK_TABLES.sliding_attacks(sq, occupied, PieceType::Bishop);
        attackers |= bishops & bishop_attacks;

        // Horses also have king-like moves
        let horses = bishops & self.pos.board.promoted_bb;
        attackers |= horses & king_attacks;

        attackers
    }

    /// Get all pieces of given color that can attack a square
    fn attackers_to(&self, sq: Square, color: Color) -> Bitboard {
        let mut attackers = Bitboard::EMPTY;
        let occupied = self.pos.board.all_bb;

        // Pawns (unpromoted)
        let pawns = self.pos.board.piece_bb[color as usize][PieceType::Pawn as usize]
            & !self.pos.board.promoted_bb;
        let pawn_attacks = ATTACK_TABLES.pawn_attacks(sq, color.opposite());
        attackers |= pawns & pawn_attacks;

        // Knights (unpromoted)
        let knights = self.pos.board.piece_bb[color as usize][PieceType::Knight as usize]
            & !self.pos.board.promoted_bb;
        let knight_attacks = ATTACK_TABLES.knight_attacks(sq, color.opposite());
        attackers |= knights & knight_attacks;

        // Golds and promoted pieces that move like gold
        let gold_attacks = ATTACK_TABLES.gold_attacks(sq, color.opposite());
        let golds = self.pos.board.piece_bb[color as usize][PieceType::Gold as usize];
        let promoted_silvers = self.pos.board.piece_bb[color as usize][PieceType::Silver as usize]
            & self.pos.board.promoted_bb;
        let promoted_knights = self.pos.board.piece_bb[color as usize][PieceType::Knight as usize]
            & self.pos.board.promoted_bb;
        let promoted_lances = self.pos.board.piece_bb[color as usize][PieceType::Lance as usize]
            & self.pos.board.promoted_bb;
        let promoted_pawns = self.pos.board.piece_bb[color as usize][PieceType::Pawn as usize]
            & self.pos.board.promoted_bb;
        attackers |=
            (golds | promoted_silvers | promoted_knights | promoted_lances | promoted_pawns)
                & gold_attacks;

        // Silvers (unpromoted)
        let silvers = self.pos.board.piece_bb[color as usize][PieceType::Silver as usize]
            & !self.pos.board.promoted_bb;
        let silver_attacks = ATTACK_TABLES.silver_attacks(sq, color.opposite());
        attackers |= silvers & silver_attacks;

        // Kings
        let kings = self.pos.board.piece_bb[color as usize][PieceType::King as usize];
        let king_attacks = ATTACK_TABLES.king_attacks(sq);
        attackers |= kings & king_attacks;

        // Lances (unpromoted)
        let lances = self.pos.board.piece_bb[color as usize][PieceType::Lance as usize]
            & !self.pos.board.promoted_bb;
        let mut lance_bb = lances;
        while let Some(lance_sq) = lance_bb.pop_lsb() {
            let can_attack = match color {
                Color::Black => lance_sq.rank() > sq.rank() && lance_sq.file() == sq.file(),
                Color::White => lance_sq.rank() < sq.rank() && lance_sq.file() == sq.file(),
            };
            if can_attack {
                let between = self.between_bb(lance_sq, sq);
                if (between & occupied).is_empty() {
                    attackers.set(lance_sq);
                }
            }
        }

        // Rooks and Dragons
        let rooks = self.pos.board.piece_bb[color as usize][PieceType::Rook as usize];
        let rook_attacks = ATTACK_TABLES.sliding_attacks(sq, occupied, PieceType::Rook);
        attackers |= rooks & rook_attacks;

        // Dragons also have king-like moves
        let dragons = rooks & self.pos.board.promoted_bb;
        attackers |= dragons & king_attacks;

        // Bishops and Horses
        let bishops = self.pos.board.piece_bb[color as usize][PieceType::Bishop as usize];
        let bishop_attacks = ATTACK_TABLES.sliding_attacks(sq, occupied, PieceType::Bishop);
        attackers |= bishops & bishop_attacks;

        // Horses also have king-like moves
        let horses = bishops & self.pos.board.promoted_bb;
        attackers |= horses & king_attacks;

        attackers
    }

    /// Add moves from a square to target squares
    fn add_moves(&mut self, from: Square, targets: Bitboard, _promoted: bool) {
        // Get piece type from the board
        let piece = self.pos.board.piece_on(from).expect("Piece must exist at from square");
        let piece_type = piece.piece_type;
        self.add_moves_with_type(from, targets, piece_type);
    }

    /// Add moves from a square to target squares with known piece type
    fn add_moves_with_type(&mut self, from: Square, mut targets: Bitboard, piece_type: PieceType) {
        // If we're in check, only allow moves that block or capture checker
        if !self.checkers.is_empty() && self.checkers.count_ones() == 1 {
            let checker_sq = self.checkers.lsb().unwrap();
            let block_squares = self.between_bb(checker_sq, self.king_sq) | self.checkers;
            targets &= block_squares;
        }

        // If piece is pinned, only allow moves along pin ray
        if self.pinned.test(from) {
            targets &= self.pin_rays[from.0 as usize];
        }

        // Never allow capturing enemy king (should not happen in legal shogi)
        let them = self.pos.side_to_move.opposite();
        let enemy_king_bb = self.pos.board.piece_bb[them as usize][PieceType::King as usize];
        targets &= !enemy_king_bb;

        while let Some(to) = targets.pop_lsb() {
            let captured_type = self.get_captured_type(to);
            self.moves
                .push(Move::normal_with_piece(from, to, false, piece_type, captured_type));
        }
    }

    /// Check if a piece can promote
    fn can_promote(&self, from: Square, to: Square, color: Color) -> bool {
        // A piece can promote if it's moving from or to the promotion zone
        // Promotion zone is the opponent's last 3 ranks
        match color {
            Color::Black => from.rank() <= 2 || to.rank() <= 2, // Ranks 0,1,2 are Black's promotion zone
            Color::White => from.rank() >= 6 || to.rank() >= 6, // Ranks 6,7,8 are White's promotion zone
        }
    }

    /// Check if king would be in check after a move (simplified)
    fn would_be_in_check(&self, from: Square, to: Square) -> bool {
        let us = self.pos.side_to_move;
        let them = us.opposite();

        // Make the move temporarily
        let mut test_occupied = self.pos.board.all_bb;
        test_occupied.clear(from);
        test_occupied.set(to);

        // Check if any enemy piece attacks the destination square
        // (This assumes the move is a king move)
        let king_sq = to;

        // Check sliding attacks with updated occupancy
        let enemy_rooks = self.pos.board.piece_bb[them as usize][PieceType::Rook as usize];
        let enemy_bishops = self.pos.board.piece_bb[them as usize][PieceType::Bishop as usize];

        let rook_attacks = ATTACK_TABLES.sliding_attacks(king_sq, test_occupied, PieceType::Rook);
        if (enemy_rooks & rook_attacks).count_ones() > 0 {
            return true;
        }

        let bishop_attacks =
            ATTACK_TABLES.sliding_attacks(king_sq, test_occupied, PieceType::Bishop);
        if (enemy_bishops & bishop_attacks).count_ones() > 0 {
            return true;
        }

        // Lance attacks need special handling
        let enemy_lances = self.pos.board.piece_bb[them as usize][PieceType::Lance as usize];
        let mut lance_bb = enemy_lances;
        while let Some(lance_sq) = lance_bb.pop_lsb() {
            if self.is_aligned(lance_sq, king_sq) {
                let between = self.between_bb(lance_sq, king_sq);
                if (between & test_occupied).is_empty() {
                    return true;
                }
            }
        }

        false
    }

    /// Check if two squares are aligned (for sliding pieces)
    fn is_aligned(&self, sq1: Square, sq2: Square) -> bool {
        let file_diff = (sq1.file() as i8 - sq2.file() as i8).abs();
        let rank_diff = (sq1.rank() as i8 - sq2.rank() as i8).abs();

        // Same file, rank, or diagonal
        file_diff == 0 || rank_diff == 0 || file_diff == rank_diff
    }

    /// Check if a piece at given square is a sliding piece
    fn is_sliding_piece(&self, sq: Square) -> bool {
        if let Some(piece) = self.pos.board.piece_on(sq) {
            matches!(piece.piece_type, PieceType::Rook | PieceType::Bishop | PieceType::Lance)
                || (piece.promoted
                    && matches!(piece.piece_type, PieceType::Rook | PieceType::Bishop))
        } else {
            false
        }
    }

    /// Check if two squares are aligned for rook movement
    fn is_aligned_rook(&self, sq1: Square, sq2: Square) -> bool {
        sq1.file() == sq2.file() || sq1.rank() == sq2.rank()
    }

    /// Check if two squares are aligned for bishop movement
    fn is_aligned_bishop(&self, sq1: Square, sq2: Square) -> bool {
        let file_diff = (sq1.file() as i8 - sq2.file() as i8).abs();
        let rank_diff = (sq1.rank() as i8 - sq2.rank() as i8).abs();
        file_diff == rank_diff && file_diff != 0
    }

    /// Get bitboard of squares between two aligned squares
    fn between_bb(&self, sq1: Square, sq2: Square) -> Bitboard {
        let mut between = Bitboard::EMPTY;

        let file1 = sq1.file() as i8;
        let rank1 = sq1.rank() as i8;
        let file2 = sq2.file() as i8;
        let rank2 = sq2.rank() as i8;

        let file_step = if file2 > file1 {
            1
        } else if file2 < file1 {
            -1
        } else {
            0
        };
        let rank_step = if rank2 > rank1 {
            1
        } else if rank2 < rank1 {
            -1
        } else {
            0
        };

        let mut f = file1 + file_step;
        let mut r = rank1 + rank_step;

        while f != file2 || r != rank2 {
            between.set(Square::new(f as u8, r as u8));
            f += file_step;
            r += rank_step;
        }

        between
    }
}

// Simple API for enhanced search
impl Default for MoveGen {
    fn default() -> Self {
        Self::new()
    }
}

impl MoveGen {
    /// Create new move generator
    pub fn new() -> Self {
        MoveGen
    }

    /// Generate all legal moves
    pub fn generate_all(&mut self, pos: &Position, moves: &mut MoveList) {
        let mut gen = MoveGenImpl::new(pos);
        let all_moves = gen.generate_all();
        moves.clear();
        for mv in all_moves.as_slice() {
            moves.push(*mv);
        }
    }

    /// Generate only capture moves
    pub fn generate_captures(&mut self, pos: &Position, moves: &mut MoveList) {
        let mut gen = MoveGenImpl::new(pos);
        let all_moves = gen.generate_all();
        moves.clear();

        // Filter captures
        for mv in all_moves.as_slice() {
            if !mv.is_drop() {
                let to = mv.to();
                if pos.board.piece_on(to).is_some() {
                    moves.push(*mv);
                }
            }
        }
    }

    /// Generate evasion moves (when in check)
    pub fn generate_evasions(&mut self, pos: &Position, moves: &mut MoveList) {
        // For now, just generate all moves
        self.generate_all(pos, moves);
    }
}

#[cfg(test)]
mod tests {
    use crate::{usi::parse_usi_square, Piece};

    use super::*;

    #[test]
    fn test_movegen_startpos() {
        let pos = Position::startpos();

        let mut gen = MoveGenImpl::new(&pos);
        let moves = gen.generate_all();

        // Starting position should have exactly 30 legal moves
        // - 9 pawn moves (each pawn can move one square forward)
        // - 2 rook moves (左右の飛車が1マス前進)
        // - 2 bishop moves (左右の角が1マス前進)
        // - 2 gold moves (金が前進)
        // - 2 silver moves (銀が前進)
        // - 4 knight moves (桂馬が跳ねる)
        // - 2 lance moves (香車が前進)
        // - 2 king moves (玉が前進)
        // Total = 30

        assert_eq!(moves.len(), 30);
    }

    #[test]
    fn test_movegen_king_moves() {
        let mut pos = Position::empty();
        pos.board
            .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::King, Color::Black));

        let mut gen = MoveGenImpl::new(&pos);
        let moves = gen.generate_all();

        // King in center has 8 moves
        assert_eq!(moves.len(), 8);
    }

    #[test]
    fn test_movegen_pawn_moves() {
        let mut pos = Position::empty();
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));
        // Black pawn on rank 5 (not in promotion zone)
        pos.board
            .put_piece(parse_usi_square("5f").unwrap(), Piece::new(PieceType::Pawn, Color::Black));

        let mut gen = MoveGenImpl::new(&pos);
        let moves = gen.generate_all();

        // Should include only one pawn move (not in promotion zone)
        let pawn_moves: Vec<_> = moves
            .as_slice()
            .iter()
            .filter(|m| m.from() == Some(parse_usi_square("5f").unwrap()))
            .collect();
        assert_eq!(pawn_moves.len(), 1);
        assert!(!pawn_moves[0].is_promote());
    }

    #[test]
    fn test_movegen_pawn_promotion() {
        let mut pos = Position::empty();
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));
        // Black pawn in promotion zone (rank 2, can move to rank 1)
        pos.board
            .put_piece(parse_usi_square("5c").unwrap(), Piece::new(PieceType::Pawn, Color::Black));

        let mut gen = MoveGenImpl::new(&pos);
        let moves = gen.generate_all();

        // Should include pawn moves (both promoted and unpromoted since it's in promotion zone)
        let pawn_moves: Vec<_> = moves
            .as_slice()
            .iter()
            .filter(|m| m.from() == Some(parse_usi_square("5c").unwrap()))
            .collect();
        assert_eq!(pawn_moves.len(), 2); // One promoted, one unpromoted

        // Check that we have both promoted and unpromoted moves
        let promoted_count = pawn_moves.iter().filter(|m| m.is_promote()).count();
        let unpromoted_count = pawn_moves.iter().filter(|m| !m.is_promote()).count();
        assert_eq!(promoted_count, 1);
        assert_eq!(unpromoted_count, 1);
    }

    #[test]
    fn test_movegen_in_check() {
        let mut pos = Position::empty();
        // Black king in check from white rook
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::White));
        pos.board
            .put_piece(parse_usi_square("5d").unwrap(), Piece::new(PieceType::Rook, Color::White));
        pos.board
            .put_piece(parse_usi_square("6b").unwrap(), Piece::new(PieceType::Gold, Color::Black));

        let mut gen = MoveGenImpl::new(&pos);
        let moves = gen.generate_all();

        // In check, only king moves and blocking moves are legal
        assert!(gen.checkers.count_ones() > 0); // Verify we detect check

        // King should be able to move to escape
        let king_moves: Vec<_> = moves
            .as_slice()
            .iter()
            .filter(|m| m.from() == Some(parse_usi_square("5a").unwrap()))
            .collect();
        assert!(!king_moves.is_empty());

        // Gold can block the check
        let gold_moves: Vec<_> = moves
            .as_slice()
            .iter()
            .filter(|m| m.from() == Some(parse_usi_square("6b").unwrap()))
            .collect();
        let block_move = gold_moves.iter().find(|m| {
            m.to() == parse_usi_square("5b").unwrap() || m.to() == parse_usi_square("5c").unwrap()
        });
        assert!(block_move.is_some());
    }

    #[test]
    fn test_movegen_pinned_piece() {
        let mut pos = Position::empty();
        // Black gold is pinned by white rook
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::White));
        pos.board
            .put_piece(parse_usi_square("5c").unwrap(), Piece::new(PieceType::Gold, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5f").unwrap(), Piece::new(PieceType::Rook, Color::White));

        let mut gen = MoveGenImpl::new(&pos);
        let moves = gen.generate_all();

        // Pinned gold can only move along the pin ray (file 4)
        let gold_moves: Vec<_> = moves
            .as_slice()
            .iter()
            .filter(|m| m.from() == Some(parse_usi_square("5c").unwrap()))
            .collect();

        // All gold moves should be on file 4
        for m in &gold_moves {
            assert_eq!(m.to().file(), 4);
        }
    }

    #[test]
    fn test_movegen_drop_pawn_mate() {
        let mut pos = Position::empty();
        // White king with no escape squares - White is at top (rank 0)
        pos.board
            .put_piece(parse_usi_square("9i").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));
        pos.board
            .put_piece(parse_usi_square("6a").unwrap(), Piece::new(PieceType::Gold, Color::Black));
        pos.board
            .put_piece(parse_usi_square("4a").unwrap(), Piece::new(PieceType::Gold, Color::Black));
        pos.board
            .put_piece(parse_usi_square("6b").unwrap(), Piece::new(PieceType::Gold, Color::Black));
        pos.board
            .put_piece(parse_usi_square("4b").unwrap(), Piece::new(PieceType::Gold, Color::Black));

        // Black has a pawn in hand
        pos.hands[Color::Black as usize][6] = 1; // Pawn

        let mut gen = MoveGenImpl::new(&pos);
        let moves = gen.generate_all();

        // Pawn drop at 5b would be checkmate - should not be allowed
        let sq_5b = parse_usi_square("5b").unwrap(); // 5b = file 5 (index 4), rank b (index 1)
        let illegal_drop = moves.as_slice().iter().find(|m| m.is_drop() && m.to() == sq_5b);
        assert!(illegal_drop.is_none(), "Drop pawn mate should not be allowed");
    }

    #[test]
    fn test_drop_pawn_mate_with_escape() {
        let mut pos = Position::empty();
        // White king with escape square - White is at top
        pos.board
            .put_piece(parse_usi_square("9i").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));
        pos.board
            .put_piece(parse_usi_square("6a").unwrap(), Piece::new(PieceType::Gold, Color::Black));
        // No piece at (5, 0) - king can escape there

        // Black has a pawn in hand
        pos.hands[Color::Black as usize][6] = 1; // Pawn

        let mut gen = MoveGenImpl::new(&pos);
        let moves = gen.generate_all();

        let sq_5b = parse_usi_square("5b").unwrap();
        // Pawn drop at 5b gives check but king can escape - should be allowed
        let legal_drop = moves.as_slice().iter().find(|m| m.is_drop() && m.to() == sq_5b);
        assert!(legal_drop.is_some(), "Non-mate pawn drop should be allowed");
    }

    #[test]
    fn test_drop_pawn_mate_with_capture() {
        let mut pos = Position::empty();
        // White king trapped - White is at top
        pos.board
            .put_piece(parse_usi_square("9i").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));
        pos.board
            .put_piece(parse_usi_square("6a").unwrap(), Piece::new(PieceType::Gold, Color::Black));
        pos.board
            .put_piece(parse_usi_square("4a").unwrap(), Piece::new(PieceType::Gold, Color::Black));
        // White gold that can capture the pawn
        pos.board
            .put_piece(parse_usi_square("5c").unwrap(), Piece::new(PieceType::Gold, Color::White));

        // Black has a pawn in hand
        pos.hands[Color::Black as usize][6] = 1; // Pawn

        let mut gen = MoveGenImpl::new(&pos);
        let moves = gen.generate_all();

        let sq_5b = parse_usi_square("5b").unwrap();
        // Pawn drop at 5b can be captured - should be allowed
        let legal_drop = moves.as_slice().iter().find(|m| m.is_drop() && m.to() == sq_5b);
        assert!(legal_drop.is_some(), "Capturable pawn drop should be allowed");
    }

    #[test]
    fn test_drop_pawn_mate_without_support() {
        let mut pos = Position::empty();
        // Black king far away - pawn has no support
        pos.board
            .put_piece(parse_usi_square("1a").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::White));

        // Black has a pawn in hand
        pos.hands[Color::Black as usize][6] = 1; // Pawn

        let mut gen = MoveGenImpl::new(&pos);
        let moves = gen.generate_all();

        let sq_4g = parse_usi_square("5h").unwrap();
        // Pawn drop at 4g has no support - king can capture it - should be allowed
        let legal_drop = moves.as_slice().iter().find(|m| m.is_drop() && m.to() == sq_4g);
        assert!(legal_drop.is_some(), "Unsupported pawn drop should be allowed");
    }

    #[test]
    fn test_drop_pawn_mate_pinned_defender() {
        let mut pos = Position::empty();

        // Black to move - testing Black's pawn drop
        pos.side_to_move = Color::Black;

        // Create a scenario where:
        // - White king is trapped with no escape squares
        // - A pawn drop would give check
        // - The only defender (silver) is pinned

        // White pieces (now at top of board)
        pos.board
            .put_piece(parse_usi_square("1b").unwrap(), Piece::new(PieceType::King, Color::White)); // 1b
        pos.board.put_piece(
            parse_usi_square("2b").unwrap(),
            Piece::new(PieceType::Silver, Color::White),
        ); // 2b - will be pinned

        // Black pieces
        pos.board
            .put_piece(parse_usi_square("5b").unwrap(), Piece::new(PieceType::Rook, Color::Black)); // 5b - pins silver
        pos.board
            .put_piece(parse_usi_square("1d").unwrap(), Piece::new(PieceType::Gold, Color::Black)); // 1d - supports pawn
        pos.board
            .put_piece(parse_usi_square("9i").unwrap(), Piece::new(PieceType::King, Color::Black)); // 9i - far away

        // Block escape squares for White king
        pos.board
            .put_piece(parse_usi_square("2a").unwrap(), Piece::new(PieceType::Gold, Color::Black)); // 2a
        pos.board
            .put_piece(parse_usi_square("1a").unwrap(), Piece::new(PieceType::Gold, Color::Black)); // 1a

        // Black has a pawn in hand
        pos.hands[Color::Black as usize][6] = 1; // Pawn

        let mut gen = MoveGenImpl::new(&pos);

        // Try to drop pawn at 1c (would give check to king at 1b)
        let sq_1c = parse_usi_square("1c").unwrap(); // 1c = file 1 (index 8), rank c (index 2)

        // Verify that drop pawn mate is detected
        assert!(gen.is_drop_pawn_mate(sq_1c, Color::White), "Drop pawn mate should be detected");

        let moves = gen.generate_all();

        // Pawn drop at 1c - silver is pinned and cannot capture - should not be allowed
        let illegal_drop = moves.as_slice().iter().find(|m| m.is_drop() && m.to() == sq_1c);
        assert!(
            illegal_drop.is_none(),
            "Drop pawn mate with pinned defender should not be allowed"
        );
    }

    #[test]
    fn test_drop_pawn_not_mate_with_escape() {
        let mut pos = Position::empty();

        // 玉に逃げ道があるケース（打ち歩詰めではない）
        pos.side_to_move = Color::Black;

        // 後手の配置
        pos.board
            .put_piece(parse_usi_square("5h").unwrap(), Piece::new(PieceType::King, Color::White)); // 5h
        pos.board.put_piece(
            parse_usi_square("6h").unwrap(),
            Piece::new(PieceType::Silver, Color::White),
        ); // 6h

        // 先手の配置
        pos.board
            .put_piece(parse_usi_square("5f").unwrap(), Piece::new(PieceType::Gold, Color::Black)); // 5f - 歩を支える
        pos.board
            .put_piece(parse_usi_square("9a").unwrap(), Piece::new(PieceType::King, Color::Black)); // 9a
                                                                                                    // 逃げ場の一部をブロック（でも完全ではない）
        pos.board
            .put_piece(parse_usi_square("6i").unwrap(), Piece::new(PieceType::Gold, Color::Black)); // 6i

        // 先手が歩を持っている
        pos.hands[Color::Black as usize][6] = 1;

        let mut gen = MoveGenImpl::new(&pos);

        // 5gに歩を打つ
        let sq_5g = parse_usi_square("5g").unwrap();

        // 打ち歩詰めではないことを確認（5iに逃げられる）
        assert!(
            !gen.is_drop_pawn_mate(sq_5g, Color::White),
            "Should not be drop pawn mate when king has escape squares"
        );

        let moves = gen.generate_all();
        let legal_drop = moves.as_slice().iter().find(|m| m.is_drop() && m.to() == sq_5g);
        assert!(legal_drop.is_some(), "Pawn drop should be allowed when king can escape");
    }

    #[test]
    fn test_drop_pawn_not_mate_can_capture_with_promoted() {
        let mut pos = Position::empty();

        // 成り駒が歩を取れるケース
        pos.side_to_move = Color::Black;

        // 後手の配置
        pos.board
            .put_piece(parse_usi_square("5h").unwrap(), Piece::new(PieceType::King, Color::White)); // 5h
        pos.board.put_piece(
            parse_usi_square("6g").unwrap(),
            Piece::promoted(PieceType::Silver, Color::White),
        ); // 6g - 成銀

        // 先手の配置
        pos.board
            .put_piece(parse_usi_square("5f").unwrap(), Piece::new(PieceType::Gold, Color::Black)); // 5f - 歩を支える
        pos.board
            .put_piece(parse_usi_square("9a").unwrap(), Piece::new(PieceType::King, Color::Black)); // 9a

        // 玉の逃げ場をブロック
        pos.board
            .put_piece(parse_usi_square("6h").unwrap(), Piece::new(PieceType::Gold, Color::Black)); // 6h
        pos.board
            .put_piece(parse_usi_square("6i").unwrap(), Piece::new(PieceType::Gold, Color::Black)); // 6i
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::Gold, Color::Black)); // 5i
        pos.board
            .put_piece(parse_usi_square("4i").unwrap(), Piece::new(PieceType::Gold, Color::Black)); // 4i
        pos.board
            .put_piece(parse_usi_square("4h").unwrap(), Piece::new(PieceType::Gold, Color::Black)); // 4h

        // 先手が歩を持っている
        pos.hands[Color::Black as usize][6] = 1;

        let mut gen = MoveGenImpl::new(&pos);

        // 5gに歩を打つ
        let sq_5g = parse_usi_square("5g").unwrap();

        // 打ち歩詰めではないことを確認（成銀が取れる）
        assert!(
            !gen.is_drop_pawn_mate(sq_5g, Color::White),
            "Should not be drop pawn mate when promoted piece can capture"
        );

        let moves = gen.generate_all();
        let legal_drop = moves.as_slice().iter().find(|m| m.is_drop() && m.to() == sq_5g);
        assert!(
            legal_drop.is_some(),
            "Pawn drop should be allowed when promoted piece can capture"
        );
    }

    #[test]
    fn test_drop_pawn_not_mate_long_range_defender() {
        let mut pos = Position::empty();

        // 遠距離からの守りのケース（飛車）
        pos.side_to_move = Color::Black;

        // 後手の配置
        pos.board
            .put_piece(parse_usi_square("5h").unwrap(), Piece::new(PieceType::King, Color::White)); // 5h
        pos.board
            .put_piece(parse_usi_square("5d").unwrap(), Piece::new(PieceType::Rook, Color::White)); // 5d - 遠くから守る

        // 先手の配置
        pos.board
            .put_piece(parse_usi_square("5f").unwrap(), Piece::new(PieceType::Gold, Color::Black)); // 5f - 歩を支える
        pos.board
            .put_piece(parse_usi_square("9a").unwrap(), Piece::new(PieceType::King, Color::Black)); // 9a

        // 先手が歩を持っている
        pos.hands[Color::Black as usize][6] = 1;

        let mut gen = MoveGenImpl::new(&pos);

        // 5gに歩を打つ
        let sq_5g = parse_usi_square("5g").unwrap();

        // 打ち歩詰めではないことを確認（飛車が取れる）
        assert!(
            !gen.is_drop_pawn_mate(sq_5g, Color::White),
            "Should not be drop pawn mate when rook can capture from distance"
        );

        let moves = gen.generate_all();
        let legal_drop = moves.as_slice().iter().find(|m| m.is_drop() && m.to() == sq_5g);
        assert!(legal_drop.is_some(), "Pawn drop should be allowed when rook can capture");
    }

    #[test]
    fn test_drop_pawn_mate_at_edge() {
        let mut pos = Position::empty();

        // エッジケース：盤端（1筋）での打ち歩詰め
        pos.side_to_move = Color::Black;

        // 後手の配置（1筋の端、rank 0）
        pos.board
            .put_piece(parse_usi_square("1a").unwrap(), Piece::new(PieceType::King, Color::White)); // 1a
        pos.board.put_piece(
            parse_usi_square("2a").unwrap(),
            Piece::new(PieceType::Silver, Color::White),
        ); // 2a - ピンされる

        // 先手の配置
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::Rook, Color::Black)); // 5a - 銀をピン
        pos.board
            .put_piece(parse_usi_square("1c").unwrap(), Piece::new(PieceType::Gold, Color::Black)); // 1c - 歩を支える
        pos.board
            .put_piece(parse_usi_square("9i").unwrap(), Piece::new(PieceType::King, Color::Black)); // 9i

        // 玉の逃げ場をブロック（盤端なので元々限定的）
        pos.board
            .put_piece(parse_usi_square("2b").unwrap(), Piece::new(PieceType::Gold, Color::Black)); // 2b

        // 先手が歩を持っている
        pos.hands[Color::Black as usize][6] = 1;

        let mut gen = MoveGenImpl::new(&pos);

        // 1bに歩を打つ
        let sq_1b = parse_usi_square("1b").unwrap();

        // 打ち歩詰めが検出されることを確認
        assert!(
            gen.is_drop_pawn_mate(sq_1b, Color::White),
            "Drop pawn mate at board edge should be detected"
        );

        let moves = gen.generate_all();
        let illegal_drop = moves.as_slice().iter().find(|m| m.is_drop() && m.to() == sq_1b);
        assert!(illegal_drop.is_none(), "Drop pawn mate at board edge should not be allowed");
    }

    #[test]
    fn test_drop_pawn_false_positive_cases() {
        let mut pos = Position::empty();

        // 偽陽性防止：歩を支える駒がピンされている
        pos.side_to_move = Color::Black;

        // 後手の配置
        pos.board
            .put_piece(parse_usi_square("5h").unwrap(), Piece::new(PieceType::King, Color::White)); // 5h
        pos.board
            .put_piece(parse_usi_square("8h").unwrap(), Piece::new(PieceType::Rook, Color::White)); // 8h

        // 先手の配置
        pos.board
            .put_piece(parse_usi_square("5f").unwrap(), Piece::new(PieceType::Gold, Color::Black)); // 5f - 歩を支えるが...
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::Black)); // 5a - 金がピンされている！

        // 先手が歩を持っている
        pos.hands[Color::Black as usize][6] = 1;

        let mut gen = MoveGenImpl::new(&pos);

        // 5gに歩を打つ
        let sq_5g = parse_usi_square("5g").unwrap();

        // 打ち歩詰めではないことを確認（歩に紐がついていない - 金がピンされているため）
        assert!(
            !gen.is_drop_pawn_mate(sq_5g, Color::White),
            "Should not be drop pawn mate when supporting piece is pinned"
        );

        let moves = gen.generate_all();
        let legal_drop = moves.as_slice().iter().find(|m| m.is_drop() && m.to() == sq_5g);
        assert!(
            legal_drop.is_some(),
            "Pawn drop should be allowed when support is invalid due to pin"
        );
    }

    #[test]
    fn test_no_king_capture() {
        // 将棋では玉を取る手は禁止されているため、手生成時に除外される必要がある
        // このテストは、玉を取れる位置関係でも、そのような手が生成されないことを検証する
        let mut pos = Position::empty();

        // テスト局面の設定：
        // - 先手の銀(5b)が後手の玉(6c)を取れる位置関係
        // - 正しい実装では、この銀→玉の手は生成されないはず
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::Black)); // 先手玉: 5a
        pos.board.put_piece(
            parse_usi_square("5b").unwrap(),
            Piece::new(PieceType::Silver, Color::Black),
        ); // 先手銀: 5b
        pos.board
            .put_piece(parse_usi_square("6c").unwrap(), Piece::new(PieceType::King, Color::White)); // 後手玉: 6c

        let mut gen = MoveGenImpl::new(&pos);
        let moves = gen.generate_all();

        // 生成された全ての手をチェックし、玉を取る手が含まれていないことを確認
        for m in moves.as_slice() {
            if !m.is_drop() {
                if let Some(from) = m.from() {
                    let to = m.to();
                    if from == parse_usi_square("5b").unwrap()
                        && to == parse_usi_square("6c").unwrap()
                    {
                        panic!("Generated illegal move: silver captures king!");
                    }
                }
            }
        }

        println!("OK: No king capture moves generated");
    }

    #[test]
    fn test_check_evasion_king_moves() {
        // 王手回避：玉の移動で逃げる
        let mut pos = Position::empty();

        // 先手玉が5五、後手飛車が5八で王手
        pos.board
            .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5b").unwrap(), Piece::new(PieceType::Rook, Color::White));

        let mut gen = MoveGenImpl::new(&pos);
        let moves = gen.generate_all();

        // 王手されているので、玉が移動するか、飛車の利きを遮るしかない
        // 玉は飛車の利きから逃げる必要がある
        let king_moves: Vec<_> = moves
            .as_slice()
            .iter()
            .filter(|m| !m.is_drop() && m.from() == Some(parse_usi_square("5e").unwrap()))
            .collect();

        // 玉の逃げ場所を確認（5筋以外）
        for m in &king_moves {
            assert_ne!(m.to().file(), 4, "King should not move on the same file as the rook");
        }
    }

    #[test]
    fn test_check_evasion_block() {
        // 王手回避：合駒で防ぐ
        let mut pos = Position::empty();

        // 先手玉が5一、後手飛車が5八で王手、先手は金を持っている
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5b").unwrap(), Piece::new(PieceType::Rook, Color::White));
        pos.hands[Color::Black as usize][2] = 1; // 金を持っている

        let mut gen = MoveGenImpl::new(&pos);
        let moves = gen.generate_all();

        // 5筋に金を打って飛車の利きを遮る手があるはず
        let block_drops: Vec<_> =
            moves.as_slice().iter().filter(|m| m.is_drop() && m.to().file() == 4).collect();

        assert!(!block_drops.is_empty(), "Should be able to block with a drop");
    }

    #[test]
    fn test_check_evasion_capture() {
        // 王手回避：王手している駒を取る
        let mut pos = Position::empty();

        // 先手玉が5五、後手金が4四で王手、先手銀が3三
        pos.board
            .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("4f").unwrap(), Piece::new(PieceType::Gold, Color::White));
        pos.board.put_piece(
            parse_usi_square("3g").unwrap(),
            Piece::new(PieceType::Silver, Color::Black),
        );

        let mut gen = MoveGenImpl::new(&pos);
        let moves = gen.generate_all();

        // 銀で金を取る手があるはず
        let capture_move = moves.as_slice().iter().find(|m| {
            !m.is_drop()
                && m.from() == Some(parse_usi_square("3g").unwrap())
                && m.to() == parse_usi_square("4f").unwrap()
        });

        assert!(capture_move.is_some(), "Should be able to capture the checking piece");
    }

    #[test]
    fn test_double_check_only_king_moves() {
        // 両王手の場合は玉が逃げるしかない
        let mut pos = Position::empty();

        // 先手玉が5五、後手飛車が5八と角が1九で両王手
        pos.board
            .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5b").unwrap(), Piece::new(PieceType::Rook, Color::White));
        pos.board.put_piece(
            parse_usi_square("1a").unwrap(),
            Piece::new(PieceType::Bishop, Color::White),
        );

        let mut gen = MoveGenImpl::new(&pos);
        let moves = gen.generate_all();

        // 全ての手が玉の移動であることを確認
        for m in moves.as_slice() {
            if !m.is_drop() {
                assert_eq!(
                    m.from(),
                    Some(parse_usi_square("5e").unwrap()),
                    "Only king moves allowed in double check"
                );
            } else {
                panic!("No drops allowed in double check");
            }
        }
    }

    #[test]
    fn test_pin_restriction() {
        // ピンされた駒の移動制限
        let mut pos = Position::empty();

        // 先手玉5一、先手金5五、後手飛車5九でピン
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::Gold, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::Rook, Color::White));

        let mut gen = MoveGenImpl::new(&pos);
        let moves = gen.generate_all();

        // 金の動きは5筋のみ（ピンの方向）
        let gold_moves: Vec<_> = moves
            .as_slice()
            .iter()
            .filter(|m| !m.is_drop() && m.from() == Some(parse_usi_square("5e").unwrap()))
            .collect();

        for m in &gold_moves {
            assert_eq!(m.to().file(), 4, "Pinned piece can only move along pin ray");
        }
    }

    #[test]
    fn test_board_edge_knight_moves() {
        // 盤端での桂馬の動き
        let mut pos = Position::empty();

        // 桂馬を1筋と9筋に配置 (Black knights at rank 8)
        pos.board.put_piece(
            parse_usi_square("1i").unwrap(),
            Piece::new(PieceType::Knight, Color::Black),
        ); // 1i
        pos.board.put_piece(
            parse_usi_square("9i").unwrap(),
            Piece::new(PieceType::Knight, Color::Black),
        ); // 9i
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));

        let mut gen = MoveGenImpl::new(&pos);
        let moves = gen.generate_all();

        // 1iの桂馬は2gにしか行けない (file 7, rank 6)
        let knight1_moves: Vec<_> = moves
            .as_slice()
            .iter()
            .filter(|m| !m.is_drop() && m.from() == Some(parse_usi_square("1i").unwrap()))
            .collect();

        assert_eq!(knight1_moves.len(), 1);
        assert_eq!(knight1_moves[0].to(), parse_usi_square("2g").unwrap()); // Black knight jumps to rank 6

        // 9iの桂馬は8gにしか行けない
        let knight9_moves: Vec<_> = moves
            .as_slice()
            .iter()
            .filter(|m| !m.is_drop() && m.from() == Some(parse_usi_square("9i").unwrap()))
            .collect();

        assert_eq!(knight9_moves.len(), 1);
        assert_eq!(knight9_moves[0].to(), parse_usi_square("8g").unwrap()); // Black knight jumps to rank 6
    }

    #[test]
    fn test_forced_promotion_pawn() {
        // 歩の1段目成り強制
        let mut pos = Position::empty();

        // 先手歩を2段目に配置 (Black pawn on rank 1, moving to rank 0)
        pos.board
            .put_piece(parse_usi_square("5b").unwrap(), Piece::new(PieceType::Pawn, Color::Black));
        pos.board
            .put_piece(parse_usi_square("9i").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("1a").unwrap(), Piece::new(PieceType::King, Color::White));

        let mut gen = MoveGenImpl::new(&pos);
        let moves = gen.generate_all();

        // 歩が1段目に進む手は必ず成り (Black pawn moving to rank 0 must promote)
        let pawn_moves: Vec<_> = moves
            .as_slice()
            .iter()
            .filter(|m| {
                !m.is_drop()
                    && m.from() == Some(parse_usi_square("5b").unwrap())
                    && m.to() == parse_usi_square("5a").unwrap()
            })
            .collect();

        assert_eq!(pawn_moves.len(), 1);
        assert!(pawn_moves[0].is_promote(), "Black pawn must promote on rank 0");
    }

    #[test]
    fn test_forced_promotion_lance() {
        // 香車の1段目成り強制
        let mut pos = Position::empty();

        // 先手香を2段目に配置 (Black lance on rank 1, moving to rank 0)
        pos.board
            .put_piece(parse_usi_square("9b").unwrap(), Piece::new(PieceType::Lance, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));

        let mut gen = MoveGenImpl::new(&pos);
        let moves = gen.generate_all();

        // 香が1段目に進む手は必ず成り (Black lance moving to rank 0 must promote)
        // Find all lance moves and check they properly handle forced promotion
        let lance_moves: Vec<_> = moves
            .as_slice()
            .iter()
            .filter(|m| !m.is_drop() && m.from() == Some(parse_usi_square("9b").unwrap()))
            .collect();

        // At least one move should exist
        assert!(!lance_moves.is_empty(), "Lance should have at least one move");

        // Any move to rank 0 must be promoted
        for mv in &lance_moves {
            if mv.to() == parse_usi_square("9a").unwrap() {
                assert!(mv.is_promote(), "Black lance must promote when moving to rank 0");
            }
        }
    }

    #[test]
    fn test_forced_promotion_knight() {
        // 桂馬の2段目成り強制
        let mut pos = Position::empty();

        // 先手桂を3段目に配置 (Black knight on rank 2)
        pos.board.put_piece(
            parse_usi_square("8c").unwrap(),
            Piece::new(PieceType::Knight, Color::Black),
        );
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));

        let mut gen = MoveGenImpl::new(&pos);
        let moves = gen.generate_all();

        // 桂が1段目に進む手は必ず成り (Black knight moving to rank 0)
        let knight_moves: Vec<_> = moves
            .as_slice()
            .iter()
            .filter(|m| !m.is_drop() && m.from() == Some(parse_usi_square("8c").unwrap()))
            .collect();

        // Black knight jumps 2 ranks forward (toward rank 0)
        for m in &knight_moves {
            if m.to().rank() == 0 {
                assert!(m.is_promote(), "Black knight must promote on rank 0");
            }
        }
    }
}
