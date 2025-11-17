use crate::movegen::{MoveGenError, MoveGenerator};
use crate::shogi::board::Square;
use crate::shogi::piece_constants::piece_type_to_hand_index;
use crate::shogi::{Bitboard, Color, Move, Piece, PieceType, Position};
use rand::Rng;
use rand::SeedableRng;
use rand_xoshiro::Xoshiro256PlusPlus;
use std::cmp::Ordering;
use std::collections::HashMap;

const REPETITION_PENALTY: i32 = 1000;
const NOISE_BOUND: i32 = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShogihomeBasicStyle {
    StaticRookV1,
    RangingRookV1,
    Random,
}

#[derive(Clone, Copy, Debug)]
struct PieceValue {
    value: i32,
    capture_gain: i32,
}

impl PieceValue {
    fn from_piece(piece: Piece) -> Self {
        Self {
            value: basic_piece_value(piece.piece_type, piece.promoted),
            capture_gain: capture_gain(piece.piece_type, piece.promoted),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct MovePieceInfo {
    piece_type: PieceType,
    promoted: bool,
}

impl MovePieceInfo {
    fn from_position(pos: &Position, mv: Move) -> Option<Self> {
        if mv.is_drop() {
            return mv.piece_type().map(|piece_type| Self {
                piece_type,
                promoted: false,
            });
        }
        let from = mv.from()?;
        pos.piece_at(from).map(|piece| Self {
            piece_type: piece.piece_type,
            promoted: piece.promoted,
        })
    }
}

#[derive(Clone, Copy)]
struct BoardCoords {
    file: u8,
    rank: u8,
}

impl BoardCoords {
    fn is(&self, file: u8, rank: u8) -> bool {
        self.file == file && self.rank == rank
    }
}

fn oriented_coords(side: Color, sq: Square) -> BoardCoords {
    let oriented = if side == Color::Black { sq } else { sq.flip() };
    BoardCoords {
        file: 9 - oriented.file(),
        rank: oriented.rank() + 1,
    }
}

fn oriented_square(side: Color, file: u8, rank: u8) -> Option<Square> {
    if !(1..=9).contains(&file) || !(1..=9).contains(&rank) {
        return None;
    }
    let file_idx = 9 - file;
    let rank_idx = rank - 1;
    let sq = Square::new(file_idx, rank_idx);
    Some(if side == Color::Black { sq } else { sq.flip() })
}

fn basic_piece_value(piece_type: PieceType, promoted: bool) -> i32 {
    match (piece_type, promoted) {
        (PieceType::Pawn, false) => 100,
        (PieceType::Pawn, true) => 400,
        (PieceType::Lance, false) => 300,
        (PieceType::Lance, true) => 500,
        (PieceType::Knight, false) => 400,
        (PieceType::Knight, true) => 500,
        (PieceType::Silver, false) => 500,
        (PieceType::Silver, true) => 600,
        (PieceType::Gold, _) => 600,
        (PieceType::Bishop, false) => 700,
        (PieceType::Bishop, true) => 1200,
        (PieceType::Rook, false) => 800,
        (PieceType::Rook, true) => 1500,
        (PieceType::King, _) => 0,
    }
}

fn capture_gain(piece_type: PieceType, promoted: bool) -> i32 {
    basic_piece_value(piece_type, promoted) + basic_piece_value(piece_type, false)
}

fn promotion_gain(piece_type: PieceType) -> i32 {
    basic_piece_value(piece_type, true) - basic_piece_value(piece_type, false)
}

pub struct BasicMoveEvaluator {
    style: ShogihomeBasicStyle,
}

impl BasicMoveEvaluator {
    pub fn new(style: ShogihomeBasicStyle) -> Self {
        Self { style }
    }

    pub fn style(&self) -> ShogihomeBasicStyle {
        self.style
    }

    pub fn evaluate_move(&self, pos: &Position, mv: Move) -> i32 {
        if self.style == ShogihomeBasicStyle::Random {
            return 0;
        }
        let piece_info = match MovePieceInfo::from_position(pos, mv) {
            Some(info) => info,
            None => return 0,
        };
        let side = pos.side_to_move;
        let drop = mv.is_drop();
        let from_coords = mv.from().map(|sq| oriented_coords(side, sq));
        let to_coords = oriented_coords(side, mv.to());
        let mut state = MoveEvalState {
            pos,
            side,
            mv,
            piece: piece_info,
            drop,
            from: from_coords,
            to: to_coords,
        };

        state.evaluate(self.style)
    }

    pub fn see_on_square(&self, pos: &Position, target: Square) -> i32 {
        simple_see(pos, target)
    }
}

struct MoveEvalState<'a> {
    pos: &'a Position,
    side: Color,
    mv: Move,
    piece: MovePieceInfo,
    drop: bool,
    from: Option<BoardCoords>,
    to: BoardCoords,
}

impl<'a> MoveEvalState<'a> {
    fn opponent_hand_count(&self, piece_type: PieceType) -> u8 {
        match piece_type_to_hand_index(piece_type) {
            Ok(idx) => self.pos.hands[self.side.opposite() as usize][idx],
            Err(_) => 0,
        }
    }

    fn at(&self, file: u8, rank: u8) -> Option<Piece> {
        let sq = oriented_square(self.side, file, rank)?;
        self.pos.piece_at(sq)
    }

    fn evaluate(&mut self, style: ShogihomeBasicStyle) -> i32 {
        let mut score = 0;
        if let Some(captured) = self.pos.piece_at(self.mv.to()) {
            score += capture_gain(captured.piece_type, captured.promoted);
        }
        if self.mv.is_promote() {
            score += promotion_gain(self.piece.piece_type);
        }

        score += self.evaluate_piece_specific();
        score += self.evaluate_forward_backward();
        score += self.evaluate_drop_into_enemy();

        match style {
            ShogihomeBasicStyle::StaticRookV1 => score + self.evaluate_static_rook(),
            ShogihomeBasicStyle::RangingRookV1 => score + self.evaluate_ranging_rook(),
            ShogihomeBasicStyle::Random => score,
        }
    }

    fn evaluate_piece_specific(&self) -> i32 {
        let mut score = 0;
        match self.piece.piece_type {
            PieceType::Pawn => {
                if self.to.rank == 4 {
                    score += if self.drop { 10 } else { 20 };
                } else if !self.drop && (self.to.file == 1 || self.to.file == 9) {
                    score += 10;
                } else if !self.drop && self.to.is(3, 6) && self.at(4, 6).is_some() {
                    score += 30;
                } else if !self.drop
                    && self.to.is(5, 6)
                    && matches!(self.at(4, 6), Some(piece) if piece.piece_type == PieceType::Bishop)
                {
                    score += 50;
                }
            }
            PieceType::Silver => {
                if let Some(from) = self.from {
                    if self.to.rank < from.rank
                        && self.to.rank >= 7
                        && (2..=8).contains(&self.to.file)
                    {
                        score += 20;
                    }
                }
            }
            PieceType::Bishop => {
                if self.drop
                    && self.to.rank >= 4
                    && !(self.to.file + self.to.rank).is_multiple_of(2)
                {
                    score -= 200;
                } else if self.to.file == 1 || self.to.file == 9 {
                    score -= 500;
                } else if self.drop && self.to.rank == 1 {
                    score -= 50;
                } else if self.to.is(4, 6)
                    && self.at(5, 5).is_none()
                    && self.at(6, 4).is_none()
                    && self.at(7, 3).is_none()
                {
                    score += 100;
                }
            }
            PieceType::Rook => {
                if self.to.rank == 7 {
                    score -= 20;
                }
            }
            _ => {}
        }
        score
    }

    fn evaluate_forward_backward(&self) -> i32 {
        fn eligible(piece: &MovePieceInfo) -> bool {
            match piece.piece_type {
                PieceType::Pawn | PieceType::Silver | PieceType::Gold => true,
                PieceType::Lance | PieceType::Knight => piece.promoted,
                _ => false,
            }
        }

        if self.drop || !eligible(&self.piece) {
            return 0;
        }
        let Some(from) = self.from else {
            return 0;
        };
        if self.to.rank < from.rank {
            return (self.to.rank as i32) - 3;
        } else if self.to.rank > from.rank {
            return 3 - (self.to.rank as i32);
        }
        0
    }

    fn evaluate_drop_into_enemy(&self) -> i32 {
        if !self.drop || self.to.rank > 4 {
            return 0;
        }
        match self.piece.piece_type {
            PieceType::Pawn => (self.to.rank * 3) as i32,
            PieceType::Lance | PieceType::Knight => (self.to.rank * 2) as i32,
            PieceType::Silver | PieceType::Gold => self.to.rank as i32,
            PieceType::Bishop => -100,
            PieceType::Rook => 500,
            _ => 0,
        }
    }

    fn evaluate_static_rook(&self) -> i32 {
        let mut score = 0;
        match self.piece.piece_type {
            PieceType::Pawn => {
                if !self.drop {
                    if self.to.is(2, 6) || self.to.is(2, 5) {
                        score += 50;
                    } else if self.to.is(7, 6) {
                        score += 100;
                    } else if self.to.is(6, 6) || self.to.file == 3 {
                        score += 20;
                    }
                } else if self.to.is(8, 7) {
                    score += 200;
                } else if self.to.is(8, 8) {
                    score += 50;
                }
            }
            PieceType::Lance | PieceType::Knight => {
                score -= 50;
            }
            PieceType::Silver => {
                if let Some(from) = self.from {
                    if from.rank > self.to.rank {
                        if self.to.is(8, 8) || self.to.is(7, 7) {
                            score += 100;
                        } else if self.to.is(6, 8)
                            || self.to.is(6, 7)
                            || self.to.is(3, 8)
                            || self.to.is(3, 7)
                            || self.to.is(3, 5)
                            || self.to.is(4, 6)
                        {
                            score += 20;
                        } else if self.to.is(2, 7) || self.to.is(2, 6) {
                            score += 10;
                        }
                    }
                }
            }
            PieceType::Gold => {
                if !self.drop {
                    if self.to.is(7, 8) {
                        score += 80;
                    } else if self.to.is(5, 8) {
                        score += 20;
                    } else if self.to.is(6, 7) {
                        if let Some(from) = self.from {
                            if from.file <= 6 {
                                score += 30;
                            }
                        }
                    }
                }
            }
            PieceType::Bishop => {
                if let Some(piece) = self.at(self.to.file, self.to.rank) {
                    if piece.piece_type == PieceType::Bishop {
                        score += 200;
                    }
                }
            }
            PieceType::King => {
                if let Some(from) = self.from {
                    if self.to.file == 6 && from.file == 5 {
                        score += 30;
                    } else if self.to.file == 7 && from.file == 6 {
                        score += 100;
                    } else if self.to.file <= 4 {
                        score -= 1000;
                    }
                }
            }
            _ => {}
        }
        score
    }

    fn evaluate_ranging_rook(&self) -> i32 {
        let mut score = 0;
        match self.piece.piece_type {
            PieceType::Pawn => {
                if !self.drop {
                    if self.to.is(7, 6) {
                        score += 100;
                    } else if self.to.is(6, 6) && self.opponent_hand_count(PieceType::Bishop) == 0 {
                        score += 90;
                    } else if self.to.is(6, 5) && self.at(7, 8).is_some() {
                        score += 20;
                        if self.at(7, 5).is_some() {
                            score += 200;
                        }
                    } else if self.to.is(7, 5) {
                        score -= 150;
                    } else if self.to.file == 1 {
                        score += 40;
                        if self.at(1, 4).is_some() {
                            score += 50;
                        }
                    } else if self.to.file == 9 && self.at(9, 4).is_some() {
                        score += 50;
                    }
                } else if self.to.is(8, 7) {
                    score += 200;
                } else if self.to.is(8, 8) {
                    score += 50;
                }
            }
            PieceType::Lance | PieceType::Knight => {
                score -= 50;
            }
            PieceType::Silver => {
                if let Some(from) = self.from {
                    if from.rank > self.to.rank && self.at(7, 6).is_some() {
                        if self.to.is(7, 8) {
                            score += 40;
                        } else if self.to.is(6, 7) {
                            score += 30;
                        } else if self.to.is(5, 6) {
                            score += 10;
                        } else if self.to.is(6, 5) {
                            score += if matches!(self.at(6, 6), Some(piece) if piece.piece_type == PieceType::Pawn)
                            {
                                -10
                            } else {
                                20
                            };
                        } else if self.to.is(4, 5) || self.to.is(3, 8) {
                            score += 10;
                        }
                    }
                }
            }
            PieceType::Gold => {
                if !self.drop && self.to.is(7, 8) {
                    score += 20;
                }
            }
            PieceType::Bishop => {
                if !self.drop && self.to.is(7, 7) {
                    score += 70;
                    if self.at(8, 5).is_some() {
                        score += 50;
                    }
                }
            }
            PieceType::Rook => {
                if !self.drop && self.to.is(6, 8) {
                    score += 80;
                }
            }
            PieceType::King => {
                if self.to.file >= 5 {
                    score -= 1000;
                } else if let Some(from) = self.from {
                    if from.file > self.to.file && self.to.file >= 2 {
                        score += 60 + 5 * (4 - self.to.file as i32);
                    }
                }
            }
        }
        score
    }
}

fn simple_see(pos: &Position, target: Square) -> i32 {
    let Some(occupant) = pos.piece_at(target) else {
        return 0;
    };
    let mut enemy_pieces = vec![PieceValue::from_piece(occupant)];
    let mut my_pieces = Vec::new();
    let side = pos.side_to_move;
    collect_attackers(pos, side, target, &mut my_pieces);
    collect_attackers(pos, side.opposite(), target, &mut enemy_pieces);

    my_pieces.sort_by(piece_value_cmp);
    enemy_pieces.sort_by(piece_value_cmp);

    see_search(0, &my_pieces, 0, &enemy_pieces, 0)
}

fn collect_attackers(pos: &Position, color: Color, target: Square, out: &mut Vec<PieceValue>) {
    let mut bb: Bitboard = pos.get_attackers_to(target, color);
    while let Some(sq) = bb.pop_lsb() {
        if let Some(piece) = pos.piece_at(sq) {
            out.push(PieceValue::from_piece(piece));
        }
    }
}

fn piece_value_cmp(a: &PieceValue, b: &PieceValue) -> Ordering {
    a.value.cmp(&b.value)
}

fn see_search(
    base_score: i32,
    my_pieces: &[PieceValue],
    my_index: usize,
    enemy_pieces: &[PieceValue],
    enemy_index: usize,
) -> i32 {
    if my_index >= my_pieces.len() || enemy_index >= enemy_pieces.len() {
        return 0;
    }
    let mut score = base_score + enemy_pieces[enemy_index].capture_gain;
    if score <= 0 {
        return 0;
    }
    score -= see_search(-score, enemy_pieces, enemy_index + 1, my_pieces, my_index);
    score.max(0)
}

pub struct BasicSearchResult {
    pub best_move: Option<Move>,
    pub score: i32,
}

pub struct BasicEngine {
    evaluator: BasicMoveEvaluator,
    movegen: MoveGenerator,
    rng: Xoshiro256PlusPlus,
    noise: bool,
}

impl BasicEngine {
    pub fn new(style: ShogihomeBasicStyle) -> Self {
        Self {
            evaluator: BasicMoveEvaluator::new(style),
            movegen: MoveGenerator::new(),
            rng: Xoshiro256PlusPlus::seed_from_u64(0x5c09_d1ab_a65e_4311),
            noise: true,
        }
    }

    pub fn with_seed(style: ShogihomeBasicStyle, seed: u64) -> Self {
        Self {
            rng: Xoshiro256PlusPlus::seed_from_u64(seed),
            ..Self::new(style)
        }
    }

    pub fn set_seed(&mut self, seed: u64) {
        self.rng = Xoshiro256PlusPlus::seed_from_u64(seed);
    }

    pub fn enable_noise(&mut self, enabled: bool) {
        self.noise = enabled;
    }

    pub fn style(&self) -> ShogihomeBasicStyle {
        self.evaluator.style()
    }

    pub fn search(
        &mut self,
        root: &Position,
        depth: u8,
        repetition: Option<&RepetitionTable>,
    ) -> Result<BasicSearchResult, MoveGenError> {
        let mut pos = root.clone();
        let moves = self.movegen.generate_all(&pos)?;
        if moves.is_empty() {
            return Ok(BasicSearchResult {
                best_move: None,
                score: 0,
            });
        }
        if self.style() == ShogihomeBasicStyle::Random {
            let idx = self.rng.random_range(0..moves.len());
            return Ok(BasicSearchResult {
                best_move: Some(moves[idx]),
                score: 0,
            });
        }
        let mut best_move = None;
        let mut best_score = i32::MIN;
        for mv in moves.iter().copied() {
            let mut score = self.evaluator.evaluate_move(&pos, mv);
            let undo = pos.do_move(mv);
            if let Some(table) = repetition {
                if table.count(pos.zobrist_hash()) > 0 {
                    score -= REPETITION_PENALTY;
                }
            }
            if depth > 1 {
                score -= self.search_score(&mut pos, depth - 1)?;
            } else {
                score -= self.evaluator.see_on_square(&pos, mv.to());
            }
            pos.undo_move(mv, undo);
            if self.noise {
                score += self.random_noise();
            }
            if score > best_score {
                best_score = score;
                best_move = Some(mv);
            }
        }

        Ok(BasicSearchResult {
            best_move,
            score: best_score,
        })
    }

    fn search_score(&mut self, pos: &mut Position, depth: u8) -> Result<i32, MoveGenError> {
        if self.style() == ShogihomeBasicStyle::Random {
            return Ok(0);
        }
        let moves = self.movegen.generate_all(pos)?;
        if moves.is_empty() {
            return Ok(0);
        }
        let mut best_score = i32::MIN;
        for mv in moves.iter().copied() {
            let mut score = self.evaluator.evaluate_move(pos, mv);
            let undo = pos.do_move(mv);
            if depth > 1 {
                score -= self.search_score(pos, depth - 1)?;
            } else {
                score -= self.evaluator.see_on_square(pos, mv.to());
            }
            pos.undo_move(mv, undo);
            if self.noise {
                score += self.random_noise();
            }
            if score > best_score {
                best_score = score;
            }
        }
        Ok(best_score)
    }

    fn random_noise(&mut self) -> i32 {
        self.rng.random_range(0..NOISE_BOUND)
    }
}

pub struct RepetitionTable {
    counts: HashMap<u64, u32>,
}

impl RepetitionTable {
    pub fn from_position(pos: &Position) -> Self {
        let mut counts = HashMap::new();
        for &hash in &pos.history {
            *counts.entry(hash).or_insert(0) += 1;
        }
        *counts.entry(pos.zobrist_hash()).or_insert(0) += 1;
        Self { counts }
    }

    pub fn count(&self, hash: u64) -> u32 {
        *self.counts.get(&hash).unwrap_or(&0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shogi::position::Position;
    use crate::usi::parse_usi_move;

    #[test]
    fn repetition_table_counts_history() {
        let mut pos = Position::startpos();
        let mv = parse_usi_move("7g7f").unwrap();
        pos.do_move(mv);
        let mv2 = parse_usi_move("3c3d").unwrap();
        pos.do_move(mv2);
        let table = RepetitionTable::from_position(&pos);
        assert!(table.count(pos.zobrist_hash()) >= 1);
    }

    #[test]
    fn evaluator_rewards_pawn_attack() {
        let pos = Position::startpos();
        let evaluator = BasicMoveEvaluator::new(ShogihomeBasicStyle::StaticRookV1);
        let mv = parse_usi_move("7g7f").unwrap();
        let score = evaluator.evaluate_move(&pos, mv);
        assert!(score > 0);
    }

    #[test]
    fn basic_engine_returns_move() {
        let pos = Position::startpos();
        let mut engine = BasicEngine::new(ShogihomeBasicStyle::StaticRookV1);
        engine.enable_noise(false);
        let table = RepetitionTable::from_position(&pos);
        let result = engine.search(&pos, 2, Some(&table)).unwrap();
        assert!(result.best_move.is_some());
    }
}
