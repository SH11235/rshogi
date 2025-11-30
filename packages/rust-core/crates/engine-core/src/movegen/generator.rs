//! 指し手生成器

use crate::bitboard::{
    between_bb, bishop_effect, dragon_effect, gold_effect, horse_effect, king_effect,
    knight_effect, lance_effect, line_bb, pawn_effect, rook_effect, silver_effect, Bitboard,
    FILE_BB, RANK_BB,
};
use crate::position::Position;
use crate::types::{Color, Move, PieceType, Square};

use super::movelist::MoveList;

/// 敵陣のBitboard（成れる領域）
fn enemy_field(us: Color) -> Bitboard {
    match us {
        Color::Black => RANK_BB[0] | RANK_BB[1] | RANK_BB[2], // 1-3段目
        Color::White => RANK_BB[6] | RANK_BB[7] | RANK_BB[8], // 7-9段目
    }
}

/// 行き所のない歩・香が進めない段
fn rank1_bb(us: Color) -> Bitboard {
    match us {
        Color::Black => RANK_BB[0], // 1段目
        Color::White => RANK_BB[8], // 9段目
    }
}

/// 行き所のない桂が進めない段
fn rank12_bb(us: Color) -> Bitboard {
    match us {
        Color::Black => RANK_BB[0] | RANK_BB[1], // 1-2段目
        Color::White => RANK_BB[7] | RANK_BB[8], // 8-9段目
    }
}

/// 指し手を追加
#[inline]
fn add_move(moves: &mut [Move], idx: &mut usize, mv: Move) {
    moves[*idx] = mv;
    *idx += 1;
}

// ============================================================================
// 駒種別の移動生成
// ============================================================================

/// 歩の移動による指し手を生成
fn generate_pawn_moves(pos: &Position, target: Bitboard, moves: &mut [Move], idx: &mut usize) {
    let us = pos.side_to_move();
    let pawns = pos.pieces(us, PieceType::Pawn);

    if pawns.is_empty() {
        return;
    }

    let promo_ranks = enemy_field(us);
    let rank1 = rank1_bb(us);

    for from in pawns.iter() {
        // 歩の利きを計算
        let attacks = pawn_effect(us, from) & target;
        let moved_pc = pos.piece_on(from);

        for to in attacks.iter() {
            if promo_ranks.contains(to) {
                // 成る手を生成
                let promoted_pc = moved_pc.promote().unwrap();
                add_move(moves, idx, Move::new_move_with_piece(from, to, true, promoted_pc));

                // 不成も生成（1段目でないとき）
                if !rank1.contains(to) {
                    add_move(moves, idx, Move::new_move_with_piece(from, to, false, moved_pc));
                }
            } else {
                // 成れない → 不成のみ
                add_move(moves, idx, Move::new_move_with_piece(from, to, false, moved_pc));
            }
        }
    }
}

/// 香の移動による指し手を生成
fn generate_lance_moves(pos: &Position, target: Bitboard, moves: &mut [Move], idx: &mut usize) {
    let us = pos.side_to_move();
    let lances = pos.pieces(us, PieceType::Lance);

    if lances.is_empty() {
        return;
    }

    let promo_ranks = enemy_field(us);
    let rank1 = rank1_bb(us);
    let occupied = pos.occupied();

    for from in lances.iter() {
        let attacks = lance_effect(us, from, occupied) & target;
        let moved_pc = pos.piece_on(from);

        for to in attacks.iter() {
            if promo_ranks.contains(to) {
                // 成る手を生成
                let promoted_pc = moved_pc.promote().unwrap();
                add_move(moves, idx, Move::new_move_with_piece(from, to, true, promoted_pc));

                // 不成（1段目でないとき）
                if !rank1.contains(to) {
                    add_move(moves, idx, Move::new_move_with_piece(from, to, false, moved_pc));
                }
            } else {
                // 敵陣外 → 不成のみ
                add_move(moves, idx, Move::new_move_with_piece(from, to, false, moved_pc));
            }
        }
    }
}

/// 桂の移動による指し手を生成
fn generate_knight_moves(pos: &Position, target: Bitboard, moves: &mut [Move], idx: &mut usize) {
    let us = pos.side_to_move();
    let knights = pos.pieces(us, PieceType::Knight);

    if knights.is_empty() {
        return;
    }

    let promo_ranks = enemy_field(us);
    let rank12 = rank12_bb(us);

    for from in knights.iter() {
        let attacks = knight_effect(us, from) & target;
        let moved_pc = pos.piece_on(from);

        for to in attacks.iter() {
            if promo_ranks.contains(to) {
                // 成る手を生成
                let promoted_pc = moved_pc.promote().unwrap();
                add_move(moves, idx, Move::new_move_with_piece(from, to, true, promoted_pc));
            }

            // 不成（1,2段目でないとき）
            if !rank12.contains(to) {
                add_move(moves, idx, Move::new_move_with_piece(from, to, false, moved_pc));
            }
        }
    }
}

/// 銀の移動による指し手を生成
fn generate_silver_moves(pos: &Position, target: Bitboard, moves: &mut [Move], idx: &mut usize) {
    let us = pos.side_to_move();
    let silvers = pos.pieces(us, PieceType::Silver);

    if silvers.is_empty() {
        return;
    }

    let promo_ranks = enemy_field(us);

    for from in silvers.iter() {
        let attacks = silver_effect(us, from) & target;
        let from_in_promo = promo_ranks.contains(from);
        let moved_pc = pos.piece_on(from);

        for to in attacks.iter() {
            let to_in_promo = promo_ranks.contains(to);

            // 成る手（移動元または移動先が敵陣）
            if from_in_promo || to_in_promo {
                let promoted_pc = moved_pc.promote().unwrap();
                add_move(moves, idx, Move::new_move_with_piece(from, to, true, promoted_pc));
            }

            // 不成
            add_move(moves, idx, Move::new_move_with_piece(from, to, false, moved_pc));
        }
    }
}

/// 角の移動による指し手を生成
fn generate_bishop_moves(pos: &Position, target: Bitboard, moves: &mut [Move], idx: &mut usize) {
    let us = pos.side_to_move();
    let bishops = pos.pieces(us, PieceType::Bishop);

    if bishops.is_empty() {
        return;
    }

    let promo_ranks = enemy_field(us);
    let occupied = pos.occupied();

    for from in bishops.iter() {
        let attacks = bishop_effect(from, occupied) & target;
        let from_in_promo = promo_ranks.contains(from);
        let moved_pc = pos.piece_on(from);

        for to in attacks.iter() {
            let to_in_promo = promo_ranks.contains(to);

            if from_in_promo || to_in_promo {
                // 成る手を生成
                let promoted_pc = moved_pc.promote().unwrap();
                add_move(moves, idx, Move::new_move_with_piece(from, to, true, promoted_pc));
                // 不成も生成
                add_move(moves, idx, Move::new_move_with_piece(from, to, false, moved_pc));
            } else {
                // 敵陣に関係ない → 不成のみ
                add_move(moves, idx, Move::new_move_with_piece(from, to, false, moved_pc));
            }
        }
    }
}

/// 飛車の移動による指し手を生成
fn generate_rook_moves(pos: &Position, target: Bitboard, moves: &mut [Move], idx: &mut usize) {
    let us = pos.side_to_move();
    let rooks = pos.pieces(us, PieceType::Rook);

    if rooks.is_empty() {
        return;
    }

    let promo_ranks = enemy_field(us);
    let occupied = pos.occupied();

    for from in rooks.iter() {
        let attacks = rook_effect(from, occupied) & target;
        let from_in_promo = promo_ranks.contains(from);
        let moved_pc = pos.piece_on(from);

        for to in attacks.iter() {
            let to_in_promo = promo_ranks.contains(to);

            if from_in_promo || to_in_promo {
                // 成る手を生成
                let promoted_pc = moved_pc.promote().unwrap();
                add_move(moves, idx, Move::new_move_with_piece(from, to, true, promoted_pc));
                // 不成も生成
                add_move(moves, idx, Move::new_move_with_piece(from, to, false, moved_pc));
            } else {
                // 敵陣に関係ない → 不成のみ
                add_move(moves, idx, Move::new_move_with_piece(from, to, false, moved_pc));
            }
        }
    }
}

/// 金相当の駒の移動を生成
fn generate_gold_moves(pos: &Position, target: Bitboard, moves: &mut [Move], idx: &mut usize) {
    let us = pos.side_to_move();

    // 金相当の駒（金、と、成香、成桂、成銀）
    let golds = pos.pieces(us, PieceType::Gold)
        | pos.pieces(us, PieceType::ProPawn)
        | pos.pieces(us, PieceType::ProLance)
        | pos.pieces(us, PieceType::ProKnight)
        | pos.pieces(us, PieceType::ProSilver);

    for from in golds.iter() {
        let attacks = gold_effect(us, from) & target;
        let moved_pc = pos.piece_on(from);
        for to in attacks.iter() {
            add_move(moves, idx, Move::new_move_with_piece(from, to, false, moved_pc));
        }
    }
}

/// 馬の移動を生成
fn generate_horse_moves(pos: &Position, target: Bitboard, moves: &mut [Move], idx: &mut usize) {
    let us = pos.side_to_move();
    let horses = pos.pieces(us, PieceType::Horse);
    let occupied = pos.occupied();

    for from in horses.iter() {
        let attacks = horse_effect(from, occupied) & target;
        let moved_pc = pos.piece_on(from);
        for to in attacks.iter() {
            add_move(moves, idx, Move::new_move_with_piece(from, to, false, moved_pc));
        }
    }
}

/// 龍の移動を生成
fn generate_dragon_moves(pos: &Position, target: Bitboard, moves: &mut [Move], idx: &mut usize) {
    let us = pos.side_to_move();
    let dragons = pos.pieces(us, PieceType::Dragon);
    let occupied = pos.occupied();

    for from in dragons.iter() {
        let attacks = dragon_effect(from, occupied) & target;
        let moved_pc = pos.piece_on(from);
        for to in attacks.iter() {
            add_move(moves, idx, Move::new_move_with_piece(from, to, false, moved_pc));
        }
    }
}

/// 玉の移動を生成
fn generate_king_moves(pos: &Position, target: Bitboard, moves: &mut [Move], idx: &mut usize) {
    let us = pos.side_to_move();
    let king_sq = pos.king_square(us);

    let attacks = king_effect(king_sq) & target;
    let moved_pc = pos.piece_on(king_sq);
    for to in attacks.iter() {
        add_move(moves, idx, Move::new_move_with_piece(king_sq, to, false, moved_pc));
    }
}

// ============================================================================
// 駒打ち生成
// ============================================================================

/// 二歩にならない升のBitboardを返す
fn pawn_drop_mask(_us: Color, our_pawns: Bitboard) -> Bitboard {
    let mut mask = Bitboard::ALL;

    for file_bb in &FILE_BB {
        if !(our_pawns & *file_bb).is_empty() {
            // この筋には歩があるので打てない
            mask &= !*file_bb;
        }
    }

    mask
}

/// 歩の駒打ちを生成
fn generate_pawn_drops(pos: &Position, target: Bitboard, moves: &mut [Move], idx: &mut usize) {
    let us = pos.side_to_move();

    // 手駒に歩がなければ終了
    if !pos.hand(us).has(PieceType::Pawn) {
        return;
    }

    // 1段目を除外
    let rank1 = rank1_bb(us);
    let valid_targets = target & !rank1;

    // 二歩のチェック
    let our_pawns = pos.pieces(us, PieceType::Pawn);
    let valid_targets = valid_targets & pawn_drop_mask(us, our_pawns);

    // 打ち歩詰めチェックは後でis_legalで行う
    let dropped_pc = crate::types::Piece::make(us, PieceType::Pawn);
    for to in valid_targets.iter() {
        add_move(moves, idx, Move::new_drop_with_piece(PieceType::Pawn, to, dropped_pc));
    }
}

/// 歩以外の駒打ちを生成
fn generate_non_pawn_drops(pos: &Position, target: Bitboard, moves: &mut [Move], idx: &mut usize) {
    let us = pos.side_to_move();
    let hand = pos.hand(us);

    // 行き所のない駒の制約
    let rank1 = rank1_bb(us);
    let rank12 = rank12_bb(us);

    // 香（1段目には打てない）
    if hand.has(PieceType::Lance) {
        let dropped_pc = crate::types::Piece::make(us, PieceType::Lance);
        for to in (target & !rank1).iter() {
            add_move(moves, idx, Move::new_drop_with_piece(PieceType::Lance, to, dropped_pc));
        }
    }

    // 桂（1,2段目には打てない）
    if hand.has(PieceType::Knight) {
        let dropped_pc = crate::types::Piece::make(us, PieceType::Knight);
        for to in (target & !rank12).iter() {
            add_move(moves, idx, Move::new_drop_with_piece(PieceType::Knight, to, dropped_pc));
        }
    }

    // 銀・金・角・飛（どこでも打てる）
    for pt in [
        PieceType::Silver,
        PieceType::Gold,
        PieceType::Bishop,
        PieceType::Rook,
    ] {
        if hand.has(pt) {
            let dropped_pc = crate::types::Piece::make(us, pt);
            for to in target.iter() {
                add_move(moves, idx, Move::new_drop_with_piece(pt, to, dropped_pc));
            }
        }
    }
}

// ============================================================================
// メイン生成関数
// ============================================================================

/// 王手がかかっていないときの全ての指し手を生成（pseudo-legal）
pub fn generate_non_evasions(pos: &Position, moves: &mut [Move]) -> usize {
    let us = pos.side_to_move();
    let target = !pos.pieces_c(us); // 自駒のない場所

    let mut idx = 0;

    // 駒の移動
    generate_pawn_moves(pos, target, moves, &mut idx);
    generate_lance_moves(pos, target, moves, &mut idx);
    generate_knight_moves(pos, target, moves, &mut idx);
    generate_silver_moves(pos, target, moves, &mut idx);
    generate_bishop_moves(pos, target, moves, &mut idx);
    generate_rook_moves(pos, target, moves, &mut idx);
    generate_gold_moves(pos, target, moves, &mut idx);
    generate_horse_moves(pos, target, moves, &mut idx);
    generate_dragon_moves(pos, target, moves, &mut idx);
    generate_king_moves(pos, target, moves, &mut idx);

    // 駒打ち（空いている場所のみ）
    let empties = !pos.occupied();
    generate_pawn_drops(pos, empties, moves, &mut idx);
    generate_non_pawn_drops(pos, empties, moves, &mut idx);

    idx
}

/// 王手回避手を生成（pseudo-legal）
pub fn generate_evasions(pos: &Position, moves: &mut [Move]) -> usize {
    debug_assert!(pos.in_check());

    let us = pos.side_to_move();
    let them = !us;
    let king_sq = pos.king_square(us);
    let checkers = pos.checkers();
    let occupied = pos.occupied();

    let mut idx = 0;

    // 王手している駒の利きを集める（玉を除いた盤面で計算）
    let occ_without_king = occupied & !Bitboard::from_square(king_sq);
    let mut slider_attacks = Bitboard::EMPTY;
    let mut checker_count = 0;
    let mut checker_sq = Square::SQ_11; // ダミー

    for sq in checkers.iter() {
        checker_count += 1;
        checker_sq = sq;
        let pc = pos.piece_on(sq);
        let pt = pc.piece_type();

        // 遠方駒の利きを計算（玉を除いた盤面で）
        match pt {
            PieceType::Lance => {
                slider_attacks |= lance_effect(them, sq, occ_without_king);
            }
            PieceType::Bishop | PieceType::Horse => {
                slider_attacks |= bishop_effect(sq, occ_without_king);
            }
            PieceType::Rook | PieceType::Dragon => {
                slider_attacks |= rook_effect(sq, occ_without_king);
            }
            _ => {}
        }
    }

    // 玉の移動先（自駒でなく、王手駒の利きでもない場所）
    let king_targets = king_effect(king_sq) & !pos.pieces_c(us) & !slider_attacks;

    for to in king_targets.iter() {
        // 移動先に敵の利きがないかは後でis_legalでチェック
        add_move(moves, &mut idx, Move::new_move(king_sq, to, false));
    }

    // 両王手なら玉移動のみ
    if checker_count >= 2 {
        return idx;
    }

    // 単王手の場合：合駒・取り返しを生成
    let between = between_bb(checker_sq, king_sq);
    let drop_target = between; // 合駒は間の升
    let move_target = between | Bitboard::from_square(checker_sq); // 移動は間 + 王手駒

    // 玉以外の駒による移動（targetを制限）
    generate_pawn_moves(pos, move_target, moves, &mut idx);
    generate_lance_moves(pos, move_target, moves, &mut idx);
    generate_knight_moves(pos, move_target, moves, &mut idx);
    generate_silver_moves(pos, move_target, moves, &mut idx);
    generate_bishop_moves(pos, move_target, moves, &mut idx);
    generate_rook_moves(pos, move_target, moves, &mut idx);
    generate_gold_moves(pos, move_target, moves, &mut idx);
    generate_horse_moves(pos, move_target, moves, &mut idx);
    generate_dragon_moves(pos, move_target, moves, &mut idx);

    // 駒打ち（合駒のみ）
    if !drop_target.is_empty() {
        generate_pawn_drops(pos, drop_target, moves, &mut idx);
        generate_non_pawn_drops(pos, drop_target, moves, &mut idx);
    }

    idx
}

/// 全ての指し手を生成（王手の有無で分岐）
pub fn generate_all(pos: &Position, moves: &mut [Move]) -> usize {
    if pos.in_check() {
        generate_evasions(pos, moves)
    } else {
        generate_non_evasions(pos, moves)
    }
}

/// 合法手を生成
pub fn generate_legal(pos: &Position, list: &mut MoveList) {
    let mut moves = [Move::NONE; super::types::MAX_MOVES];
    let count = generate_all(pos, &mut moves);

    for mv in moves.iter().take(count) {
        if pos.is_legal(*mv) {
            list.push(*mv);
        }
    }
}

// ============================================================================
// Position に合法性チェックを追加
// ============================================================================

impl Position {
    /// pseudo-legal手が本当に合法かどうかをチェック
    pub fn is_legal(&self, mv: Move) -> bool {
        let us = self.side_to_move();
        let king_sq = self.king_square(us);

        if mv.is_drop() {
            // 駒打ちは打ち歩詰め以外は常に合法
            if mv.drop_piece_type() == PieceType::Pawn {
                return self.is_legal_pawn_drop(mv.to());
            }
            return true;
        }

        let from = mv.from();
        let to = mv.to();

        // 玉の移動
        if from == king_sq {
            // 移動先に敵の利きがないことを確認
            let occ = self.occupied() ^ Bitboard::from_square(from);
            return !self.is_attacked_by(!us, to, occ);
        }

        // pinされている駒
        let pinned = self.blockers_for_king(us);
        if pinned.contains(from) {
            // pinライン上の移動のみ許可
            return line_bb(king_sq, from).contains(to);
        }

        true
    }

    /// 打ち歩詰めかどうかをチェック
    fn is_legal_pawn_drop(&self, to: Square) -> bool {
        let us = self.side_to_move();
        let them = !us;
        let them_king = self.king_square(them);

        // 歩を打つ升が敵玉の頭でなければOK
        let pawn_attack = pawn_effect(us, to);
        if !pawn_attack.contains(them_king) {
            return true;
        }

        // 敵玉の頭に歩を打つ → 打ち歩詰めチェック
        // 簡易実装：敵玉が逃げられるか、または歩を取れるかをチェック

        // 1. 敵玉が逃げられるか
        let king_escapes =
            king_effect(them_king) & !self.pieces_c(them) & !Bitboard::from_square(to);
        let occ_with_pawn = self.occupied() | Bitboard::from_square(to);

        for escape_sq in king_escapes.iter() {
            if !self.is_attacked_by(us, escape_sq, occ_with_pawn) {
                return true; // 逃げられる → 打ち歩詰めではない
            }
        }

        // 2. 歩を取れるか（玉以外の駒で）
        let attackers_to_pawn = self.attackers_to_occ(to, occ_with_pawn) & self.pieces_c(them);
        let attackers_without_king = attackers_to_pawn & !Bitboard::from_square(them_king);

        if !attackers_without_king.is_empty() {
            // 取れる駒がpinされていないかチェック
            for attacker_sq in attackers_without_king.iter() {
                let blockers = self.blockers_for_king(them);
                if !blockers.contains(attacker_sq) {
                    return true; // pinされていない駒で取れる → 打ち歩詰めではない
                }
                // pinされている駒でも、pinライン上なら取れる
                if line_bb(them_king, attacker_sq).contains(to) {
                    return true;
                }
            }
        }

        // 逃げられない、取れない → 打ち歩詰め
        false
    }

    /// 指定マスに指定手番の利きがあるか
    fn is_attacked_by(&self, c: Color, sq: Square, occupied: Bitboard) -> bool {
        !self.attackers_to_occ(sq, occupied).is_empty()
            && !(self.attackers_to_occ(sq, occupied) & self.pieces_c(c)).is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{File, Rank};

    #[test]
    fn test_generate_non_evasions_hirate() {
        let mut pos = Position::new();
        pos.set_hirate();

        let mut moves = [Move::NONE; super::super::types::MAX_MOVES];
        let count = generate_non_evasions(&pos, &mut moves);

        // 初期局面の合法手は30手
        // ただしpseudo-legalなので多めに生成される可能性あり
        assert!(count >= 30, "Generated {} moves", count);
    }

    #[test]
    fn test_generate_legal_hirate() {
        let mut pos = Position::new();
        pos.set_hirate();

        let mut list = MoveList::new();
        generate_legal(&pos, &mut list);

        // 初期局面の合法手は30手
        assert_eq!(list.len(), 30, "Generated {} legal moves", list.len());
    }

    #[test]
    fn test_pawn_drop_mask() {
        // 5筋に歩がある場合
        let sq55 = Square::new(File::File5, Rank::Rank5);
        let pawns = Bitboard::from_square(sq55);

        let mask = pawn_drop_mask(Color::Black, pawns);

        // 5筋には打てない
        assert!(!mask.contains(Square::new(File::File5, Rank::Rank6)));
        // 他の筋には打てる
        assert!(mask.contains(Square::new(File::File4, Rank::Rank5)));
        assert!(mask.contains(Square::new(File::File6, Rank::Rank5)));
    }

    #[test]
    fn test_enemy_field() {
        let black_field = enemy_field(Color::Black);
        let white_field = enemy_field(Color::White);

        // 先手の敵陣は1-3段目
        assert!(black_field.contains(Square::new(File::File5, Rank::Rank1)));
        assert!(black_field.contains(Square::new(File::File5, Rank::Rank3)));
        assert!(!black_field.contains(Square::new(File::File5, Rank::Rank4)));

        // 後手の敵陣は7-9段目
        assert!(white_field.contains(Square::new(File::File5, Rank::Rank7)));
        assert!(white_field.contains(Square::new(File::File5, Rank::Rank9)));
        assert!(!white_field.contains(Square::new(File::File5, Rank::Rank6)));
    }
}
