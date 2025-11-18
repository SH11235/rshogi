//! Evaluation function for shogi
//!
//! Simple material-based evaluation

use crate::shogi::attacks::sliding_attacks;
use crate::shogi::piece_constants::{APERY_PIECE_VALUES, APERY_PROMOTED_PIECE_VALUES};
use crate::shogi::Square;
use crate::{
    shogi::{ALL_PIECE_TYPES, NUM_HAND_PIECE_TYPES},
    Color, PieceType, Position,
};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::OnceLock;

/// Trait for position evaluation
///
/// Contract:
/// - Returns a score in centipawns from the side-to-move perspective.
/// - Positive values favor the side to move; negative values favor the opponent.
/// - Flipping `side_to_move` on the same board position should approximately flip the sign
///   (exact symmetry is not guaranteed if the evaluator incorporates tempo or king safety asymmetries).
///
/// Implementations should keep this polarity contract to ensure search components
/// (e.g., repetition penalties, pruning rules) behave consistently.
pub trait Evaluator {
    /// Evaluate position from side to move perspective
    fn evaluate(&self, pos: &Position) -> i32;

    /// Notify evaluator that search is starting at this position (root reset)
    /// Default: no-op
    fn on_set_position(&self, _pos: &Position) {}

    /// Notify evaluator before making a real move (called with pre-move position)
    /// Default: no-op
    fn on_do_move(&self, _pre_pos: &Position, _mv: crate::shogi::Move) {}

    /// Notify evaluator after undoing the last real move
    /// Default: no-op
    fn on_undo_move(&self) {}

    /// Notify evaluator before doing a null move (side-to-move flip)
    /// Default: no-op
    fn on_do_null_move(&self, _pre_pos: &Position) {}

    /// Notify evaluator after undoing the last null move
    /// Default: no-op
    fn on_undo_null_move(&self) {}
}

/// Implement Evaluator for Arc<T> where T: Evaluator
impl<T: Evaluator + ?Sized> Evaluator for std::sync::Arc<T> {
    fn evaluate(&self, pos: &Position) -> i32 {
        (**self).evaluate(pos)
    }
    fn on_set_position(&self, pos: &Position) {
        (**self).on_set_position(pos)
    }
    fn on_do_move(&self, pre_pos: &Position, mv: crate::shogi::Move) {
        (**self).on_do_move(pre_pos, mv)
    }
    fn on_undo_move(&self) {
        (**self).on_undo_move()
    }
    fn on_do_null_move(&self, pre_pos: &Position) {
        (**self).on_do_null_move(pre_pos)
    }
    fn on_undo_null_move(&self) {
        (**self).on_undo_null_move()
    }
}

/// Apery 駒価値の純粋な物質評価（軽量ヒューリスティック無し）。
///
/// - 手番側視点のスコアを返す（正なら手番有利）。
/// - 盤上の駒＋持ち駒のみを APERY_PIECE_VALUES/APERY_PROMOTED_PIECE_VALUES で集計する。
fn evaluate_material_apery_only(pos: &Position) -> i32 {
    let us = pos.side_to_move;
    let them = us.opposite();

    let mut score = 0;

    // Material on board
    for &pt in &ALL_PIECE_TYPES {
        let piece_type = pt as usize;

        // Count pieces
        let our_pieces = pos.board.piece_bb[us as usize][piece_type];
        let their_pieces = pos.board.piece_bb[them as usize][piece_type];

        let our_count = our_pieces.count_ones() as i32;
        let their_count = their_pieces.count_ones() as i32;

        let base_value = APERY_PIECE_VALUES[piece_type];
        score += base_value * (our_count - their_count);

        let promoted_delta = APERY_PROMOTED_PIECE_VALUES[piece_type] - base_value;
        if promoted_delta != 0 {
            let our_promoted = our_pieces & pos.board.promoted_bb;
            let their_promoted = their_pieces & pos.board.promoted_bb;

            let our_promoted_count = our_promoted.count_ones() as i32;
            let their_promoted_count = their_promoted.count_ones() as i32;

            score += promoted_delta * (our_promoted_count - their_promoted_count);
        }
    }

    // Material in hand
    for piece_idx in 0..NUM_HAND_PIECE_TYPES {
        let our_hand = pos.hands[us as usize][piece_idx] as i32;
        let their_hand = pos.hands[them as usize][piece_idx] as i32;

        let piece_type = PieceType::from_hand_index(piece_idx).expect("invalid hand index");
        let value = APERY_PIECE_VALUES[piece_type as usize];

        score += value * (our_hand - their_hand);
    }

    score
}

/// Evaluate position from side to move perspective
pub fn evaluate(pos: &Position) -> i32 {
    // まず純粋な物質評価（Apery 駒価値＋持ち駒）を計算し、その上に軽量ヒューリスティックを積む。
    let mut score = evaluate_material_apery_only(pos);

    let us = pos.side_to_move;
    let them = us.opposite();

    // --- Lightweight material-side heuristics (enabled for Material evaluator)
    // 1) Rook mobility (difference)
    let rook_mob_cp = material_rook_mobility_cp();
    if rook_mob_cp != 0 {
        let occupied = pos.board.all_bb;
        let our_rooks = pos.board.piece_bb[us as usize][PieceType::Rook as usize];
        let their_rooks = pos.board.piece_bb[them as usize][PieceType::Rook as usize];
        let our_occ = pos.board.occupied_bb[us as usize];
        let their_occ = pos.board.occupied_bb[them as usize];

        let mut mob_our = 0i32;
        let mut bb = our_rooks;
        while let Some(sq) = bb.pop_lsb() {
            let moves = sliding_attacks(sq, occupied, PieceType::Rook) & !our_occ;
            mob_our += moves.count_ones() as i32;
        }
        let mut mob_their = 0i32;
        let mut bb2 = their_rooks;
        while let Some(sq) = bb2.pop_lsb() {
            let moves = sliding_attacks(sq, occupied, PieceType::Rook) & !their_occ;
            mob_their += moves.count_ones() as i32;
        }
        score += rook_mob_cp * (mob_our - mob_their);
    }

    // 2) Rook trap (0-mobility) penalty (difference)
    let rook_trap_pen = material_rook_trapped_penalty_cp();
    if rook_trap_pen != 0 {
        let occupied = pos.board.all_bb;
        let our_rooks = pos.board.piece_bb[us as usize][PieceType::Rook as usize];
        let their_rooks = pos.board.piece_bb[them as usize][PieceType::Rook as usize];
        let our_occ = pos.board.occupied_bb[us as usize];
        let their_occ = pos.board.occupied_bb[them as usize];

        let mut trapped_our = 0i32;
        let mut bb = our_rooks;
        while let Some(sq) = bb.pop_lsb() {
            let moves = sliding_attacks(sq, occupied, PieceType::Rook) & !our_occ;
            if moves.count_ones() == 0 {
                trapped_our += 1;
            }
        }
        let mut trapped_their = 0i32;
        let mut bb2 = their_rooks;
        while let Some(sq) = bb2.pop_lsb() {
            let moves = sliding_attacks(sq, occupied, PieceType::Rook) & !their_occ;
            if moves.count_ones() == 0 {
                trapped_their += 1;
            }
        }
        // 相手側が閉じ込められていればプラス、自分側が閉じ込められていればマイナス
        score += rook_trap_pen * (trapped_their - trapped_our);
    }

    // 3) Early king move penalty (difference)
    let king_pen = material_king_early_move_penalty_cp();
    let max_ply = material_king_early_move_max_ply();
    if king_pen != 0 && (pos.ply as i32) <= max_ply {
        let (bk_start, wk_start) = king_start_squares();
        let our_king_moved = match us {
            Color::Black => pos.board.king_square(us) != Some(bk_start),
            Color::White => pos.board.king_square(us) != Some(wk_start),
        } as i32;
        let their_king_moved = match them {
            Color::Black => pos.board.king_square(them) != Some(bk_start),
            Color::White => pos.board.king_square(them) != Some(wk_start),
        } as i32;
        // 自分の早期王移動はペナルティ、相手の早期王移動はボーナス
        score += king_pen * (their_king_moved - our_king_moved);
    }

    // 4) Tempo bonus
    score += material_tempo_cp();

    score
}

/// Simple material evaluator implementing Evaluator trait
#[derive(Clone, Copy, Debug)]
pub struct MaterialEvaluator;

impl Evaluator for MaterialEvaluator {
    fn evaluate(&self, pos: &Position) -> i32 {
        evaluate(pos)
    }
}

// --- Runtime knobs for MaterialEvaluator lightweight terms
fn tempo_cp_atomic() -> &'static AtomicI32 {
    static CELL: OnceLock<AtomicI32> = OnceLock::new();
    CELL.get_or_init(|| AtomicI32::new(10))
}

fn rook_mobility_cp_atomic() -> &'static AtomicI32 {
    static CELL: OnceLock<AtomicI32> = OnceLock::new();
    CELL.get_or_init(|| AtomicI32::new(2))
}

fn rook_trapped_penalty_cp_atomic() -> &'static AtomicI32 {
    static CELL: OnceLock<AtomicI32> = OnceLock::new();
    CELL.get_or_init(|| AtomicI32::new(30))
}

fn king_early_move_penalty_cp_atomic() -> &'static AtomicI32 {
    static CELL: OnceLock<AtomicI32> = OnceLock::new();
    CELL.get_or_init(|| AtomicI32::new(20))
}

fn king_early_move_max_ply_atomic() -> &'static AtomicI32 {
    static CELL: OnceLock<AtomicI32> = OnceLock::new();
    CELL.get_or_init(|| AtomicI32::new(20))
}

#[inline]
pub fn material_tempo_cp() -> i32 {
    tempo_cp_atomic().load(Ordering::Relaxed)
}

#[inline]
pub fn set_material_tempo_cp(v: i32) {
    tempo_cp_atomic().store(v.clamp(-200, 200), Ordering::Relaxed);
}

#[inline]
pub fn material_rook_mobility_cp() -> i32 {
    rook_mobility_cp_atomic().load(Ordering::Relaxed)
}

#[inline]
pub fn set_material_rook_mobility_cp(v: i32) {
    rook_mobility_cp_atomic().store(v.clamp(0, 50), Ordering::Relaxed);
}

#[inline]
pub fn material_rook_trapped_penalty_cp() -> i32 {
    rook_trapped_penalty_cp_atomic().load(Ordering::Relaxed)
}

#[inline]
pub fn set_material_rook_trapped_penalty_cp(v: i32) {
    rook_trapped_penalty_cp_atomic().store(v.clamp(0, 500), Ordering::Relaxed);
}

#[inline]
pub fn material_king_early_move_penalty_cp() -> i32 {
    king_early_move_penalty_cp_atomic().load(Ordering::Relaxed)
}

#[inline]
pub fn set_material_king_early_move_penalty_cp(v: i32) {
    king_early_move_penalty_cp_atomic().store(v.clamp(0, 200), Ordering::Relaxed);
}

#[inline]
pub fn material_king_early_move_max_ply() -> i32 {
    king_early_move_max_ply_atomic().load(Ordering::Relaxed)
}

#[inline]
pub fn set_material_king_early_move_max_ply(v: i32) {
    king_early_move_max_ply_atomic().store(v.clamp(0, 100), Ordering::Relaxed);
}

#[inline]
fn king_start_squares() -> (Square, Square) {
    // Cache computed squares
    static CELL: OnceLock<(Square, Square)> = OnceLock::new();
    *CELL.get_or_init(|| {
        // Black king start: 5i, White king start: 5a
        let bk = Square::from_usi_chars('5', 'i').expect("valid square 5i");
        let wk = Square::from_usi_chars('5', 'a').expect("valid square 5a");
        (bk, wk)
    })
}

#[cfg(test)]
mod tests {
    use crate::{usi::parse_usi_square, Color, Piece};

    use super::*;

    fn place_kings(pos: &mut Position) {
        pos.board
            .put_piece(parse_usi_square("9i").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("1a").unwrap(), Piece::new(PieceType::King, Color::White));
    }

    #[test]
    fn test_evaluate_startpos_is_zero() {
        let pos = Position::startpos();
        // 純粋な駒割りのみを検証するため、テンポや飛車利きなどの軽量項は含めない。
        let score = evaluate_material_apery_only(&pos);
        assert_eq!(score, 0);
    }

    #[test]
    fn test_evaluate_material_apery_values() {
        let mut pos = Position::empty();
        place_kings(&mut pos);
        pos.board
            .put_piece(parse_usi_square("2b").unwrap(), Piece::new(PieceType::Rook, Color::Black));
        pos.board.put_piece(
            parse_usi_square("8h").unwrap(),
            Piece::new(PieceType::Bishop, Color::White),
        );

        // APERY 駒価値のみを検証（R=990, B=855）。
        let score = evaluate_material_apery_only(&pos);
        // 990 (R) - 855 (B) = 135
        assert_eq!(score, 135);
    }

    #[test]
    fn test_promoted_piece_values_match_gold() {
        let mut pos = Position::empty();
        place_kings(&mut pos);

        let mut tokin_black = Piece::new(PieceType::Pawn, Color::Black);
        tokin_black.promoted = true;
        pos.board.put_piece(parse_usi_square("5e").unwrap(), tokin_black);

        let mut tokin_white = Piece::new(PieceType::Pawn, Color::White);
        tokin_white.promoted = true;
        pos.board.put_piece(parse_usi_square("5f").unwrap(), tokin_white);

        // 成り歩は双方とも 540（= 金相当）なので互いに打ち消し合うはず。
        let score = evaluate_material_apery_only(&pos);
        assert_eq!(score, 0, "Tokin vs Tokin should cancel out");
    }

    #[test]
    fn test_hand_material_consistency() {
        let mut pos = Position::empty();
        place_kings(&mut pos);
        pos.side_to_move = Color::Black;

        // Black has 1 rook in hand, White has 2 pawns in hand
        pos.hands[Color::Black as usize][0] = 1; // Rook
        pos.hands[Color::White as usize][6] = 2; // Pawns

        // 盤上は対称なので 0、持ち駒だけで 990 - 2 * 90 = 810 となるはず。
        let score = evaluate_material_apery_only(&pos);
        assert_eq!(score, 990 - 2 * 90);
    }

    /// 飛車の機動力評価が「利きの多い側を優先する」向きに働くことを確認する。
    #[test]
    fn rook_mobility_bonus_prefers_more_mobility() {
        // 既存ノブを退避し、飛車機動力以外の軽量項は 0 にしておく
        let tempo_old = material_tempo_cp();
        let mob_old = material_rook_mobility_cp();
        let trap_old = material_rook_trapped_penalty_cp();
        let king_pen_old = material_king_early_move_penalty_cp();

        set_material_tempo_cp(0);
        set_material_rook_trapped_penalty_cp(0);
        set_material_king_early_move_penalty_cp(0);
        set_material_rook_mobility_cp(10);

        // 単純な局面: 自玉の飛車のみを配置し、敵側には飛車を置かない。
        // Kings: 5i / 5a, Black rook: 5e
        let mut pos = Position::empty();
        place_kings(&mut pos);
        pos.board
            .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::Rook, Color::Black));
        pos.side_to_move = Color::Black;

        // 純粋な駒割りとの差分として、飛車機動力項が正のボーナスになっていることを確認する。
        let base = evaluate_material_apery_only(&pos);
        let full = MaterialEvaluator.evaluate(&pos);
        let mob = full - base;

        assert!(
            mob > 0,
            "rook mobility term should give positive bonus when only our rook is present (mob={mob})"
        );

        // ノブを元に戻す
        set_material_tempo_cp(tempo_old);
        set_material_rook_mobility_cp(mob_old);
        set_material_rook_trapped_penalty_cp(trap_old);
        set_material_king_early_move_penalty_cp(king_pen_old);
    }

    /// 早期玉移動ペナルティが「自玉だけが初期位置から動いた場合」にマイナス、
    /// 「相手玉だけが動いた場合」にプラスとして働くことを確認する。
    #[test]
    fn early_king_move_penalty_applies_symmetrically() {
        let tempo_old = material_tempo_cp();
        let mob_old = material_rook_mobility_cp();
        let trap_old = material_rook_trapped_penalty_cp();
        let king_pen_old = material_king_early_move_penalty_cp();
        let king_max_old = material_king_early_move_max_ply();

        set_material_tempo_cp(0);
        set_material_rook_mobility_cp(0);
        set_material_rook_trapped_penalty_cp(0);
        set_material_king_early_move_penalty_cp(50);
        set_material_king_early_move_max_ply(10);

        // ベース: 初期局面（早期判定が効くように ply を 1 に揃える）
        let mut base = Position::startpos();
        base.ply = 1;
        let base_eval = MaterialEvaluator.evaluate(&base);

        // 自玉のみ動かした局面（先手 5i→4i に移動）
        let mut pos_self_move = base.clone();
        pos_self_move.board.remove_piece(parse_usi_square("5i").unwrap());
        pos_self_move
            .board
            .put_piece(parse_usi_square("4i").unwrap(), Piece::new(PieceType::King, Color::Black));

        let eval_self = MaterialEvaluator.evaluate(&pos_self_move);

        // 相手玉のみ動かした局面（後手 5a→6a に移動）
        let mut pos_opp_move = base.clone();
        pos_opp_move.board.remove_piece(parse_usi_square("5a").unwrap());
        pos_opp_move
            .board
            .put_piece(parse_usi_square("6a").unwrap(), Piece::new(PieceType::King, Color::White));
        let eval_opp = MaterialEvaluator.evaluate(&pos_opp_move);

        assert!(
            eval_self < base_eval,
            "early king move by side-to-move should be penalized (eval_self={eval_self}, base={base_eval})"
        );
        assert!(
            eval_opp > base_eval,
            "early king move by opponent should be rewarded (eval_opp={eval_opp}, base={base_eval})"
        );

        // ノブを元に戻す
        set_material_tempo_cp(tempo_old);
        set_material_rook_mobility_cp(mob_old);
        set_material_rook_trapped_penalty_cp(trap_old);
        set_material_king_early_move_penalty_cp(king_pen_old);
        set_material_king_early_move_max_ply(king_max_old);
    }
}
