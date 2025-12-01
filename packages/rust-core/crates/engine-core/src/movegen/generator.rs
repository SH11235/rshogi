//! 指し手生成器

use crate::bitboard::{
    between_bb, bishop_effect, dragon_effect, gold_effect, horse_effect, king_effect,
    knight_effect, lance_effect, line_bb, pawn_effect, rook_effect, silver_effect, Bitboard,
    FILE_BB, RANK_BB,
};
use crate::position::Position;
use crate::types::{Color, Move, PieceType, Square};

use super::movelist::MoveList;

#[derive(Clone, Copy)]
struct GenerateTargets {
    /// 歩以外の駒の移動先候補
    general: Bitboard,
    /// 歩の移動先候補（敵陣成りの扱いで静かに分岐させたいときに上書きする）
    pawn: Bitboard,
    /// 駒打ちの候補
    drop: Bitboard,
}

impl GenerateTargets {
    fn new(bb: Bitboard) -> Self {
        Self {
            general: bb,
            pawn: bb,
            drop: bb,
        }
    }

    fn with_drop(bb: Bitboard, drop: Bitboard) -> Self {
        Self {
            general: bb,
            pawn: bb,
            drop,
        }
    }
}

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

/// 成り生成モード
#[derive(Clone, Copy)]
enum PromotionMode {
    /// 成り・不成の両方を生成
    Both,
    /// 成りのみ生成
    PromoteOnly,
    /// 不成のみ生成（成れる場合でも不成に固定）
    NonPromoteOnly,
}

// ============================================================================
// 駒種別の移動生成
// ============================================================================

/// 歩の移動による指し手を生成
fn generate_pawn_moves(
    pos: &Position,
    target: Bitboard,
    moves: &mut [Move],
    idx: &mut usize,
    promo_mode: PromotionMode,
) {
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
            let in_promo = promo_ranks.contains(to);
            let to_is_rank1 = rank1.contains(to);

            match (in_promo, promo_mode) {
                (true, PromotionMode::PromoteOnly) => {
                    let promoted_pc = moved_pc.promote().unwrap();
                    add_move(moves, idx, Move::new_move_with_piece(from, to, true, promoted_pc));
                }
                (true, PromotionMode::NonPromoteOnly) => {
                    if !to_is_rank1 {
                        add_move(moves, idx, Move::new_move_with_piece(from, to, false, moved_pc));
                    } else {
                        // 1段目は成り強制
                        let promoted_pc = moved_pc.promote().unwrap();
                        add_move(
                            moves,
                            idx,
                            Move::new_move_with_piece(from, to, true, promoted_pc),
                        );
                    }
                }
                (true, PromotionMode::Both) => {
                    let promoted_pc = moved_pc.promote().unwrap();
                    add_move(moves, idx, Move::new_move_with_piece(from, to, true, promoted_pc));
                    if !to_is_rank1 {
                        add_move(moves, idx, Move::new_move_with_piece(from, to, false, moved_pc));
                    }
                }
                (false, _) => {
                    add_move(moves, idx, Move::new_move_with_piece(from, to, false, moved_pc))
                }
            }
        }
    }
}

/// 香の移動による指し手を生成
fn generate_lance_moves(
    pos: &Position,
    target: Bitboard,
    moves: &mut [Move],
    idx: &mut usize,
    include_non_promotions: bool,
) {
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
                if include_non_promotions && !rank1.contains(to) {
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
fn generate_knight_moves(
    pos: &Position,
    target: Bitboard,
    moves: &mut [Move],
    idx: &mut usize,
    include_non_promotions: bool,
) {
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
            if include_non_promotions && !rank12.contains(to) {
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
fn generate_bishop_moves(
    pos: &Position,
    target: Bitboard,
    moves: &mut [Move],
    idx: &mut usize,
    include_non_promotions: bool,
) {
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
                if include_non_promotions {
                    add_move(moves, idx, Move::new_move_with_piece(from, to, false, moved_pc));
                }
            } else {
                // 敵陣に関係ない → 不成のみ
                add_move(moves, idx, Move::new_move_with_piece(from, to, false, moved_pc));
            }
        }
    }
}

/// 飛車の移動による指し手を生成
fn generate_rook_moves(
    pos: &Position,
    target: Bitboard,
    moves: &mut [Move],
    idx: &mut usize,
    include_non_promotions: bool,
) {
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
                if include_non_promotions {
                    add_move(moves, idx, Move::new_move_with_piece(from, to, false, moved_pc));
                }
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
///
/// YaneuraOu でも手番は受け取るが、処理は筋単位で対称のため色依存しない。
fn pawn_drop_mask(us: Color, our_pawns: Bitboard) -> Bitboard {
    match us {
        Color::Black | Color::White => {} // 手番引数はシグネチャ整合のため保持（対称処理）
    }
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

    let empties = !pos.occupied();

    // 1段目を除外
    let rank1 = rank1_bb(us);
    let valid_targets = target & empties & !rank1;

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
    let empties = !pos.occupied();

    // 香（1段目には打てない）
    if hand.has(PieceType::Lance) {
        let dropped_pc = crate::types::Piece::make(us, PieceType::Lance);
        for to in (target & empties & !rank1).iter() {
            add_move(moves, idx, Move::new_drop_with_piece(PieceType::Lance, to, dropped_pc));
        }
    }

    // 桂（1,2段目には打てない）
    if hand.has(PieceType::Knight) {
        let dropped_pc = crate::types::Piece::make(us, PieceType::Knight);
        for to in (target & empties & !rank12).iter() {
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
            for to in (target & empties).iter() {
                add_move(moves, idx, Move::new_drop_with_piece(pt, to, dropped_pc));
            }
        }
    }
}

// ============================================================================
// メイン生成関数
// ============================================================================

/// 王手がかかっていないときの全ての指し手を生成（pseudo-legal）
fn generate_non_evasions_core(
    pos: &Position,
    moves: &mut [Move],
    targets: GenerateTargets,
    include_non_promotions: bool,
    pawn_promo_mode: PromotionMode,
    include_drops: bool,
) -> usize {
    let mut idx = 0;

    // 駒の移動
    generate_pawn_moves(pos, targets.pawn, moves, &mut idx, pawn_promo_mode);
    generate_lance_moves(pos, targets.general, moves, &mut idx, include_non_promotions);
    generate_knight_moves(pos, targets.general, moves, &mut idx, include_non_promotions);
    generate_silver_moves(pos, targets.general, moves, &mut idx);
    generate_bishop_moves(pos, targets.general, moves, &mut idx, include_non_promotions);
    generate_rook_moves(pos, targets.general, moves, &mut idx, include_non_promotions);
    generate_gold_moves(pos, targets.general, moves, &mut idx);
    generate_horse_moves(pos, targets.general, moves, &mut idx);
    generate_dragon_moves(pos, targets.general, moves, &mut idx);
    generate_king_moves(pos, targets.general, moves, &mut idx);

    if include_drops {
        let drop_target = targets.drop & !pos.occupied();
        generate_pawn_drops(pos, drop_target, moves, &mut idx);
        generate_non_pawn_drops(pos, drop_target, moves, &mut idx);
    }

    idx
}

/// 王手がかかっていないときの全ての指し手を生成（pseudo-legal）
pub fn generate_non_evasions(pos: &Position, moves: &mut [Move]) -> usize {
    let us = pos.side_to_move();
    let targets = GenerateTargets::with_drop(!pos.pieces_c(us), !pos.occupied());
    generate_non_evasions_core(pos, moves, targets, false, PromotionMode::PromoteOnly, true)
}

/// 王手回避手を生成（pseudo-legal）
fn generate_evasions_with_promos(
    pos: &Position,
    moves: &mut [Move],
    include_non_promotions: bool,
    pawn_promo_mode: PromotionMode,
) -> usize {
    debug_assert!(pos.in_check());

    let us = pos.side_to_move();
    let them = !us;
    let king_sq = pos.king_square(us);
    let checkers = pos.checkers();
    let occupied = pos.occupied();

    let mut idx = 0;

    // 王手している駒の利きを集める（玉を除いた盤面で計算）
    let occ_without_king = occupied & !Bitboard::from_square(king_sq);
    let mut checker_attacks = Bitboard::EMPTY;
    let mut checker_count = 0;
    let mut checker_sq: Option<Square> = None; // 単王手時のみ使用

    for sq in checkers.iter() {
        checker_count += 1;
        checker_sq = Some(sq);
        let pc = pos.piece_on(sq);
        let pt = pc.piece_type();

        // 王手駒の利きを集計（玉を除いた盤面で）
        let attacks_from_checker = match pt {
            PieceType::Pawn => pawn_effect(them, sq),
            PieceType::Lance => lance_effect(them, sq, occ_without_king),
            PieceType::Knight => knight_effect(them, sq),
            PieceType::Silver => silver_effect(them, sq),
            PieceType::Gold
            | PieceType::ProPawn
            | PieceType::ProLance
            | PieceType::ProKnight
            | PieceType::ProSilver => gold_effect(them, sq),
            PieceType::Bishop => bishop_effect(sq, occ_without_king),
            PieceType::Rook => rook_effect(sq, occ_without_king),
            PieceType::Horse => bishop_effect(sq, occ_without_king) | king_effect(sq),
            PieceType::Dragon => rook_effect(sq, occ_without_king) | king_effect(sq),
            PieceType::King => king_effect(sq),
        };

        checker_attacks |= attacks_from_checker;
    }

    // 玉の移動先（自駒でなく、王手駒の利きでもない場所）
    let king_targets = king_effect(king_sq) & !pos.pieces_c(us) & !checker_attacks;

    for to in king_targets.iter() {
        // 移動先に敵の利きがないかは後でis_legalでチェック
        add_move(moves, &mut idx, Move::new_move(king_sq, to, false));
    }

    // 両王手なら玉移動のみ
    if checker_count >= 2 {
        return idx;
    }

    // 単王手の場合：合駒・取り返しを生成
    let checker_sq = checker_sq.expect("in_checkなら王手駒が存在する");
    let between = between_bb(checker_sq, king_sq);
    let drop_target = between; // 合駒は間の升
    let move_target = between | Bitboard::from_square(checker_sq); // 移動は間 + 王手駒

    // 玉以外の駒による移動（targetを制限）
    generate_pawn_moves(pos, move_target, moves, &mut idx, pawn_promo_mode);
    generate_lance_moves(pos, move_target, moves, &mut idx, include_non_promotions);
    generate_knight_moves(pos, move_target, moves, &mut idx, include_non_promotions);
    generate_silver_moves(pos, move_target, moves, &mut idx);
    generate_bishop_moves(pos, move_target, moves, &mut idx, include_non_promotions);
    generate_rook_moves(pos, move_target, moves, &mut idx, include_non_promotions);
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

/// 王手回避手を生成（pseudo-legal）
pub fn generate_evasions(pos: &Position, moves: &mut [Move]) -> usize {
    generate_evasions_with_promos(pos, moves, false, PromotionMode::PromoteOnly)
}

fn generate_checks(
    pos: &Position,
    moves: &mut [Move],
    include_non_promotions: bool,
    pawn_promo_mode: PromotionMode,
    quiet_only: bool,
) -> usize {
    let us = pos.side_to_move();
    let empties = !pos.occupied();
    let targets = if quiet_only {
        GenerateTargets::with_drop(empties, empties)
    } else {
        GenerateTargets::with_drop(!pos.pieces_c(us), empties)
    };

    let mut buffer = [Move::NONE; super::types::MAX_MOVES];
    let count = generate_non_evasions_core(
        pos,
        &mut buffer,
        targets,
        include_non_promotions,
        pawn_promo_mode,
        true,
    );

    let mut idx = 0;
    for mv in buffer.iter().take(count) {
        if quiet_only && pos.is_capture(*mv) {
            continue;
        }
        if pos.gives_check(*mv) {
            moves[idx] = *mv;
            idx += 1;
        }
    }
    idx
}

fn generate_recaptures(
    pos: &Position,
    moves: &mut [Move],
    sq: Square,
    include_non_promotions: bool,
    pawn_promo_mode: PromotionMode,
) -> usize {
    let target = Bitboard::from_square(sq);
    let targets = GenerateTargets::with_drop(target, target);
    generate_non_evasions_core(pos, moves, targets, include_non_promotions, pawn_promo_mode, true)
}

/// GenType に応じた指し手生成（pseudo-legal）
pub fn generate_with_type(
    pos: &Position,
    gen_type: crate::movegen::GenType,
    moves: &mut [Move],
    recapture_sq: Option<Square>,
) -> usize {
    use crate::movegen::GenType::*;

    let us = pos.side_to_move();
    let empties = !pos.occupied();
    let enemy = pos.pieces_c(!us);

    match gen_type {
        // 通常局面
        NonEvasions => {
            let targets = GenerateTargets::with_drop(!pos.pieces_c(us), empties);
            generate_non_evasions_core(pos, moves, targets, false, PromotionMode::PromoteOnly, true)
        }
        NonEvasionsAll => {
            let targets = GenerateTargets::with_drop(!pos.pieces_c(us), empties);
            generate_non_evasions_core(pos, moves, targets, true, PromotionMode::Both, true)
        }
        Quiets => {
            let targets = GenerateTargets::with_drop(empties, empties);
            generate_non_evasions_core(pos, moves, targets, false, PromotionMode::PromoteOnly, true)
        }
        QuietsAll => {
            let targets = GenerateTargets::with_drop(empties, empties);
            generate_non_evasions_core(pos, moves, targets, true, PromotionMode::Both, true)
        }
        QuietsProMinus => {
            let targets = GenerateTargets::with_drop(empties, empties);
            generate_non_evasions_core(
                pos,
                moves,
                targets,
                false,
                PromotionMode::NonPromoteOnly,
                true,
            )
        }
        QuietsProMinusAll => {
            let targets = GenerateTargets::with_drop(empties, empties);
            generate_non_evasions_core(
                pos,
                moves,
                targets,
                true,
                PromotionMode::NonPromoteOnly,
                true,
            )
        }
        Captures => {
            let targets = GenerateTargets::new(enemy);
            generate_non_evasions_core(
                pos,
                moves,
                targets,
                false,
                PromotionMode::PromoteOnly,
                false,
            )
        }
        CapturesAll => {
            let targets = GenerateTargets::new(enemy);
            generate_non_evasions_core(pos, moves, targets, true, PromotionMode::Both, false)
        }
        CapturesProPlus => {
            let targets = GenerateTargets::new(enemy);
            generate_non_evasions_core(
                pos,
                moves,
                targets,
                false,
                PromotionMode::PromoteOnly,
                false,
            )
        }
        CapturesProPlusAll => {
            let targets = GenerateTargets::new(enemy);
            generate_non_evasions_core(pos, moves, targets, true, PromotionMode::PromoteOnly, false)
        }
        Recaptures => {
            let sq = recapture_sq.expect("Recaptures requires a target square");
            generate_recaptures(pos, moves, sq, false, PromotionMode::PromoteOnly)
        }
        RecapturesAll => {
            let sq = recapture_sq.expect("RecapturesAll requires a target square");
            generate_recaptures(pos, moves, sq, true, PromotionMode::Both)
        }
        Evasions => generate_evasions_with_promos(pos, moves, false, PromotionMode::PromoteOnly),
        EvasionsAll => generate_evasions_with_promos(pos, moves, true, PromotionMode::Both),
        Legal => {
            let mut buffer = [Move::NONE; super::types::MAX_MOVES];
            let count = if pos.in_check() {
                generate_evasions_with_promos(pos, &mut buffer, false, PromotionMode::PromoteOnly)
            } else {
                let targets = GenerateTargets::with_drop(!pos.pieces_c(us), empties);
                generate_non_evasions_core(
                    pos,
                    &mut buffer,
                    targets,
                    false,
                    PromotionMode::PromoteOnly,
                    true,
                )
            };
            let mut idx = 0;
            for mv in buffer.iter().take(count) {
                if pos.is_legal(*mv) {
                    moves[idx] = *mv;
                    idx += 1;
                }
            }
            idx
        }
        LegalAll => {
            let mut buffer = [Move::NONE; super::types::MAX_MOVES];
            let count = if pos.in_check() {
                generate_evasions_with_promos(pos, &mut buffer, true, PromotionMode::Both)
            } else {
                let targets = GenerateTargets::with_drop(!pos.pieces_c(us), empties);
                generate_non_evasions_core(
                    pos,
                    &mut buffer,
                    targets,
                    true,
                    PromotionMode::Both,
                    true,
                )
            };
            let mut idx = 0;
            for mv in buffer.iter().take(count) {
                if pos.is_legal(*mv) {
                    moves[idx] = *mv;
                    idx += 1;
                }
            }
            idx
        }
        Checks | ChecksAll | QuietChecks | QuietChecksAll => {
            let include_non_promotions = matches!(gen_type, ChecksAll | QuietChecksAll);
            let pawn_mode = if include_non_promotions {
                PromotionMode::Both
            } else {
                PromotionMode::PromoteOnly
            };
            let quiet_only = matches!(gen_type, QuietChecks | QuietChecksAll);

            generate_checks(pos, moves, include_non_promotions, pawn_mode, quiet_only)
        }
    }
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
    /// 打ち歩詰め判定用: 打たれた歩を取れる敵駒（玉以外）を列挙
    fn attackers_to_pawn(&self, c: Color, pawn_sq: Square) -> Bitboard {
        let them = !c;
        let occ = self.occupied();

        // 馬・龍は近接利きもあるため、金銀の集合に混ぜて一度のマスクで済ませる
        let horses = self.pieces(c, PieceType::Horse);
        let dragons = self.pieces(c, PieceType::Dragon);
        let hd = horses | dragons;

        let gold_like = self.pieces(c, PieceType::Gold)
            | self.pieces(c, PieceType::ProPawn)
            | self.pieces(c, PieceType::ProLance)
            | self.pieces(c, PieceType::ProKnight)
            | self.pieces(c, PieceType::ProSilver);

        let knights = knight_effect(them, pawn_sq) & self.pieces(c, PieceType::Knight);
        let silvers = silver_effect(them, pawn_sq) & (self.pieces(c, PieceType::Silver) | hd);
        let golds = gold_effect(them, pawn_sq) & (gold_like | hd);
        let bishops = bishop_effect(pawn_sq, occ) & (self.pieces(c, PieceType::Bishop) | horses);
        let rooks = rook_effect(pawn_sq, occ) & (self.pieces(c, PieceType::Rook) | dragons);

        knights | silvers | golds | bishops | rooks
    }

    /// 打ち歩詰めかどうかをチェック（YaneuraOu: legal_drop）
    fn legal_pawn_drop_check(&self, to: Square) -> bool {
        let us = self.side_to_move();
        let them = !us;
        let them_king = self.king_square(them);
        debug_assert!(pawn_effect(us, to).contains(them_king));

        // 自玉側の利きが一切なければ詰みには遠い
        let occ_with_pawn = self.occupied() | Bitboard::from_square(to);
        if (self.attackers_to_occ(to, occ_with_pawn) & self.pieces_c(us)).is_empty() {
            return true;
        }

        // 打たれた歩を敵駒が取れるか（pin判定込み）
        let attackers = self.attackers_to_pawn(them, to);
        let pinned = self.blockers_for_king(them);
        let file_mask = FILE_BB[to.file().index()];

        // pinされていない、または同じ筋方向の移動で取れるなら打ち歩詰めではない
        if !(attackers & (!pinned | file_mask)).is_empty() {
            return true;
        }

        // 玉の退路を探索
        let mut escape_bb = king_effect(them_king) & !self.pieces_c(them);
        escape_bb ^= Bitboard::from_square(to);

        for king_to in escape_bb.iter() {
            if (self.attackers_to_occ(king_to, occ_with_pawn) & self.pieces_c(us)).is_empty() {
                return true; // 退路があるので打ち歩詰めではない
            }
        }

        // 逃げられず、取れず → 打ち歩詰め
        false
    }

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
        let file_mask = FILE_BB[to.file().index()];

        // 二歩
        if !(self.pieces(us, PieceType::Pawn) & file_mask).is_empty() {
            return false;
        }

        // 歩を打つ升が敵玉の頭でなければOK
        let pawn_attack = pawn_effect(us, to);
        if !pawn_attack.contains(them_king) {
            return true;
        }

        // 敵玉の頭に歩を打つ → legal_dropで厳密判定
        self.legal_pawn_drop_check(to)
    }

    /// 指定マスに指定手番の利きがあるか
    fn is_attacked_by(&self, c: Color, sq: Square, occupied: Bitboard) -> bool {
        let attackers = self.attackers_to_occ(sq, occupied);
        !(attackers & self.pieces_c(c)).is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{File, PieceType, Rank, Square};

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

    #[test]
    fn test_pawn_drop_not_mate() {
        // 5一の玉に対して5二へ歩打ち。周囲に利きがないので合法。
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/9/9/9/9/9/9/4K4 b P 1").unwrap();

        let drop_sq = Square::new(File::File5, Rank::Rank2);
        let mv = Move::new_drop(PieceType::Pawn, drop_sq);

        assert!(pos.is_legal(mv), "打ち歩詰めでない手は合法");
    }

    #[test]
    fn test_pawn_drop_mate_is_illegal() {
        // YaneuraOuのlegal_drop相当: 5一玉に5二歩打ちが詰みになる配置は非合法。
        // 5三桂で4一/6一を利かせ、5三金で6二を、3三角で4二を抑える。5四飛で玉頭にも利きを通す。
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/3GN1B2/4R4/9/9/9/9/4K4 b P 1").unwrap();

        let drop_sq = Square::new(File::File5, Rank::Rank2);
        let mv = Move::new_drop(PieceType::Pawn, drop_sq);

        assert!(!pos.is_legal(mv), "打ち歩詰め（玉の逃げ場なし）は非合法のはず");
    }

    #[test]
    fn test_pawn_drop_is_blocked_by_nifu() {
        // 5三に自歩がある状態で5二へ歩打ちは二歩で非合法。
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/4P4/9/9/9/9/9/4K4 b P 1").unwrap();

        let drop_sq = Square::new(File::File5, Rank::Rank2);
        let mv = Move::new_drop(PieceType::Pawn, drop_sq);
        assert!(!pos.is_legal(mv), "同筋に歩があるので打ち歩は不可");
    }

    #[test]
    fn test_evasion_moves_are_legal_against_adjacent_checker() {
        // 5四の後手金による王手を回避する指し手は、玉が金の利きに飛び込まないこと。
        let mut pos = Position::new();
        pos.set_sfen("9/9/9/4g4/4K4/9/9/9/9 b - 1").unwrap();
        assert!(pos.in_check());

        let mut buffer = [Move::NONE; super::super::types::MAX_MOVES];
        let count = generate_evasions(&pos, &mut buffer);

        for mv in buffer.iter().take(count) {
            assert!(pos.is_legal(*mv), "王手回避の生成には自殺手を含めない: {mv:?}");
        }
    }

    #[test]
    fn test_generate_checks_only_returns_check_moves() {
        // 縦に並んだ玉と自駒（飛）のみの局面で、生成された手がすべて王手になることを確認。
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/9/9/9/9/9/4R4/4K4 b - 1").unwrap();
        let from = pos
            .pieces(Color::Black, PieceType::Rook)
            .iter()
            .next()
            .expect("先手飛が存在しない");

        let mut buf = [Move::NONE; super::super::types::MAX_MOVES];

        let count = generate_with_type(&pos, crate::movegen::GenType::ChecksAll, &mut buf, None);
        assert!(count > 0);

        for mv in buf.iter().take(count) {
            assert_eq!(mv.from(), from);
            assert!(pos.gives_check(*mv), "非チェック手が混入: {mv:?}");
        }
    }

    #[test]
    fn test_generate_recaptures_targets_only_given_square() {
        // 5五の後手歩を5六の先手金で取り返せる局面。Recapturesで5五のみが生成される。
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/9/4G4/4p4/9/9/9/4K4 b - 1").unwrap();

        let recapture_sq =
            pos.pieces(Color::White, PieceType::Pawn).iter().next().expect("白歩がない");
        assert!(
            pos.attackers_to_c(recapture_sq, Color::Black).is_not_empty(),
            "取り返せる先手駒がない"
        );

        let mut buf = [Move::NONE; super::super::types::MAX_MOVES];
        let count = generate_with_type(
            &pos,
            crate::movegen::GenType::Recaptures,
            &mut buf,
            Some(recapture_sq),
        );
        assert!(count > 0);
        for mv in buf.iter().take(count) {
            assert_eq!(mv.to(), recapture_sq, "他升への手が混入: {mv:?}");
        }
    }

    #[test]
    fn test_bishop_promotion_only_in_default_mode() {
        // 5二の先手角は敵陣内。通常生成では成りのみ。
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/9/9/9/9/9/4B4/4K4 b - 1").unwrap();

        let mut buf = [Move::NONE; super::super::types::MAX_MOVES];
        let count = generate_with_type(&pos, crate::movegen::GenType::NonEvasions, &mut buf, None);
        let from = pos
            .pieces(Color::Black, PieceType::Bishop)
            .iter()
            .next()
            .expect("角が存在しない");
        let enemy = enemy_field(pos.side_to_move());

        let has_non_promo = buf[..count]
            .iter()
            .any(|m| m.from() == from && enemy.contains(m.to()) && !m.is_promotion());
        assert!(!has_non_promo, "通常モードでは敵陣への角移動は成りのみのはず");
    }

    #[test]
    fn test_bishop_promotion_and_unpromotion_in_all_mode() {
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/9/9/9/9/9/4B4/4K4 b - 1").unwrap();

        let mut buf = [Move::NONE; super::super::types::MAX_MOVES];
        let count =
            generate_with_type(&pos, crate::movegen::GenType::NonEvasionsAll, &mut buf, None);
        let from = pos
            .pieces(Color::Black, PieceType::Bishop)
            .iter()
            .next()
            .expect("角が存在しない");

        let has_non_promo = buf[..count].iter().any(|m| m.from() == from && !m.is_promotion());
        assert!(has_non_promo, "All モードでは不成も生成する");
    }

    #[test]
    fn test_quiets_pro_minus_omits_pawn_promotion() {
        // 5四の歩が5三に進む静かな手は不成のみ（QuietsProMinus）。
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/9/9/9/4P4/9/9/4K4 b - 1").unwrap();

        let mut buf = [Move::NONE; super::super::types::MAX_MOVES];
        let count =
            generate_with_type(&pos, crate::movegen::GenType::QuietsProMinus, &mut buf, None);
        let from = pos.pieces(Color::Black, PieceType::Pawn).iter().next().expect("歩が存在しない");
        let to = pawn_effect(Color::Black, from).iter().next().expect("歩の利きがない");

        let moves_from: Vec<Move> = buf[..count]
            .iter()
            .copied()
            .filter(|m| m.from() == from && m.to() == to)
            .collect();
        assert!(!moves_from.is_empty(), "対象の手が生成されていない: {:?}", moves_from);

        let has_non_promo = buf[..count]
            .iter()
            .any(|m| m.from() == from && m.to() == to && !m.is_promotion());
        let has_promo = buf[..count]
            .iter()
            .any(|m| m.from() == from && m.to() == to && m.is_promotion());

        assert!(has_non_promo, "不成の静かな手は生成される");
        assert!(!has_promo, "QuietsProMinusでは歩の静かな成りは生成しないはず");
    }
}
