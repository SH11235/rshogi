//! 指し手生成器

use crate::bitboard::{
    between_bb, bishop_effect, check_candidate_bb, dragon_effect, gold_effect, horse_effect,
    king_effect, knight_effect, lance_effect, line_bb, pawn_effect, rook_effect, silver_effect,
    Bitboard, FILE_BB, RANK_BB,
};
use crate::position::Position;
use crate::types::{Color, Move, PieceType, Square};

use super::movelist::MoveList;
use super::types::ExtMoveBuffer;

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
fn add_move(buffer: &mut ExtMoveBuffer, mv: Move) {
    buffer.push_move(mv);
}

/// 成り生成モード
#[derive(Clone, Copy)]
enum PromotionMode {
    /// 成り・不成の両方を生成
    Both,
    /// 成りのみ生成
    PromoteOnly,
}

// ============================================================================
// 駒種別の移動生成
// ============================================================================

/// 歩の移動による指し手を生成
fn generate_pawn_moves(
    pos: &Position,
    target: Bitboard,
    buffer: &mut ExtMoveBuffer,
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
                    add_move(buffer, Move::new_move_with_piece(from, to, true, promoted_pc));
                }
                (true, PromotionMode::Both) => {
                    let promoted_pc = moved_pc.promote().unwrap();
                    add_move(buffer, Move::new_move_with_piece(from, to, true, promoted_pc));
                    if !to_is_rank1 {
                        add_move(buffer, Move::new_move_with_piece(from, to, false, moved_pc));
                    }
                }
                (false, _) => {
                    add_move(buffer, Move::new_move_with_piece(from, to, false, moved_pc))
                }
            }
        }
    }
}

/// 香の移動による指し手を生成
fn generate_lance_moves(
    pos: &Position,
    target: Bitboard,
    buffer: &mut ExtMoveBuffer,
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

    // YaneuraOu準拠: 成り手を先に全列挙、次に不成手を列挙 (2パス)
    // All=false (include_non_promotions=false) でも3段目(後手なら7段目)不成は生成する
    // 1段目不成は行き場がないため常に除外、2段目不成は All 時のみ
    let rank12 = rank12_bb(us);
    let non_promo_mask = if include_non_promotions {
        !rank1
    } else {
        !rank12
    };

    for from in lances.iter() {
        let attacks = lance_effect(us, from, occupied) & target;
        let moved_pc = pos.piece_on(from);

        // Pass 1: 成り手 (敵陣内の移動先)
        let promo_targets = attacks & promo_ranks;
        let promoted_pc = moved_pc.promote().unwrap();
        for to in promo_targets.iter() {
            add_move(buffer, Move::new_move_with_piece(from, to, true, promoted_pc));
        }

        // Pass 2: 不成手 (eligible な移動先)
        let non_promo_targets = attacks & non_promo_mask;
        for to in non_promo_targets.iter() {
            add_move(buffer, Move::new_move_with_piece(from, to, false, moved_pc));
        }
    }
}

/// 桂の移動による指し手を生成
fn generate_knight_moves(pos: &Position, target: Bitboard, buffer: &mut ExtMoveBuffer) {
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
                // 敵陣内：成る手を生成
                let promoted_pc = moved_pc.promote().unwrap();
                add_move(buffer, Move::new_move_with_piece(from, to, true, promoted_pc));

                // 桂馬の3段目不成は戦術的価値があるため常に生成
                // 1,2段目は行き場がないので不成は生成しない
                if !rank12.contains(to) {
                    add_move(buffer, Move::new_move_with_piece(from, to, false, moved_pc));
                }
            } else {
                // 敵陣外：不成のみ（成りは不可能）
                add_move(buffer, Move::new_move_with_piece(from, to, false, moved_pc));
            }
        }
    }
}

/// 銀の移動による指し手を生成
fn generate_silver_moves(pos: &Position, target: Bitboard, buffer: &mut ExtMoveBuffer) {
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

        if from_in_promo {
            // 敵陣からなら全ての移動先で成れる (YO: enemy_field(Us) & from 分岐)
            let promoted_pc = moved_pc.promote().unwrap();
            for to in attacks.iter() {
                add_move(buffer, Move::new_move_with_piece(from, to, true, promoted_pc));
                add_move(buffer, Move::new_move_with_piece(from, to, false, moved_pc));
            }
        } else {
            // 非敵陣: まず敵陣への移動(成り+不成り)、次に非敵陣への移動(不成りのみ)
            // (YO: SILVER の target2/target 分割に準拠)
            let promo_targets = attacks & promo_ranks;
            let non_promo_targets = attacks & !promo_ranks;

            let promoted_pc = moved_pc.promote().unwrap();
            for to in promo_targets.iter() {
                add_move(buffer, Move::new_move_with_piece(from, to, true, promoted_pc));
                add_move(buffer, Move::new_move_with_piece(from, to, false, moved_pc));
            }
            for to in non_promo_targets.iter() {
                add_move(buffer, Move::new_move_with_piece(from, to, false, moved_pc));
            }
        }
    }
}

/// 角+飛を1つの bitboard にまとめて生成（YaneuraOu GPM_BR 準拠）
///
/// YaneuraOu では角と飛を `pos.pieces(Us, BISHOP, ROOK)` で1つの bitboard に統合し、
/// マスの小さい順（pop順）で反復する。rshogi でも同じ順序で生成する。
fn generate_br_moves(
    pos: &Position,
    target: Bitboard,
    buffer: &mut ExtMoveBuffer,
    include_non_promotions: bool,
) {
    let us = pos.side_to_move();
    let pieces = pos.pieces(us, PieceType::Bishop) | pos.pieces(us, PieceType::Rook);

    if pieces.is_empty() {
        return;
    }

    let promo_ranks = enemy_field(us);
    let occupied = pos.occupied();

    for from in pieces.iter() {
        let pc = pos.piece_on(from);
        let pt = pc.piece_type();
        let attacks = match pt {
            PieceType::Bishop => bishop_effect(from, occupied),
            PieceType::Rook => rook_effect(from, occupied),
            _ => unreachable!(),
        } & target;
        let from_in_promo = promo_ranks.contains(from);

        if from_in_promo {
            // 移動元が敵陣なら全ての移動先で成れる (YO: canPromote(Us, from) 分岐)
            let promoted_pc = pc.promote().unwrap();
            for to in attacks.iter() {
                add_move(buffer, Move::new_move_with_piece(from, to, true, promoted_pc));
                if include_non_promotions {
                    add_move(buffer, Move::new_move_with_piece(from, to, false, pc));
                }
            }
        } else {
            // 移動元が非敵陣: まず敵陣への移動(成り)、次に非敵陣への移動(不成り)
            // (YO: GPM_BR の target2/target 分割に準拠)
            let promo_targets = attacks & promo_ranks;
            let non_promo_targets = attacks & !promo_ranks;

            let promoted_pc = pc.promote().unwrap();
            for to in promo_targets.iter() {
                add_move(buffer, Move::new_move_with_piece(from, to, true, promoted_pc));
                if include_non_promotions {
                    add_move(buffer, Move::new_move_with_piece(from, to, false, pc));
                }
            }
            for to in non_promo_targets.iter() {
                add_move(buffer, Move::new_move_with_piece(from, to, false, pc));
            }
        }
    }
}

/// 金相当+馬+龍+玉を1つの bitboard にまとめて生成（YaneuraOu GPM_GHDK 準拠）
///
/// YaneuraOu では `pos.pieces(Us, GOLDS, HDK)` で金相当の駒・馬・龍・玉を
/// 1つの bitboard に統合し、マスの小さい順（pop順）で反復する。
fn generate_ghdk_moves(pos: &Position, target: Bitboard, buffer: &mut ExtMoveBuffer) {
    let us = pos.side_to_move();
    let occupied = pos.occupied();

    // 金相当の駒 + 馬 + 龍 + 玉 を1つの bitboard に統合
    let king_sq = pos.king_square(us);
    let pieces = pos.golds_c(us)
        | pos.pieces(us, PieceType::Horse)
        | pos.pieces(us, PieceType::Dragon)
        | Bitboard::from_square(king_sq);

    for from in pieces.iter() {
        let pc = pos.piece_on(from);
        let pt = pc.piece_type();
        let attacks = match pt {
            PieceType::Gold
            | PieceType::ProPawn
            | PieceType::ProLance
            | PieceType::ProKnight
            | PieceType::ProSilver => gold_effect(us, from),
            PieceType::Horse => horse_effect(from, occupied),
            PieceType::Dragon => dragon_effect(from, occupied),
            PieceType::King => king_effect(from),
            _ => unreachable!(),
        } & target;

        for to in attacks.iter() {
            add_move(buffer, Move::new_move_with_piece(from, to, false, pc));
        }
    }
}

/// 金相当+馬+龍を1つの bitboard にまとめて生成（YaneuraOu GPM_GHD 準拠, 玉なし版）
///
/// 王手回避手の生成で使用。玉の移動は別途生成されるため含めない。
fn generate_ghd_moves(pos: &Position, target: Bitboard, buffer: &mut ExtMoveBuffer) {
    let us = pos.side_to_move();
    let occupied = pos.occupied();

    // 金相当の駒 + 馬 + 龍（玉は含めない）
    let pieces =
        pos.golds_c(us) | pos.pieces(us, PieceType::Horse) | pos.pieces(us, PieceType::Dragon);

    for from in pieces.iter() {
        let pc = pos.piece_on(from);
        let pt = pc.piece_type();
        let attacks = match pt {
            PieceType::Gold
            | PieceType::ProPawn
            | PieceType::ProLance
            | PieceType::ProKnight
            | PieceType::ProSilver => gold_effect(us, from),
            PieceType::Horse => horse_effect(from, occupied),
            PieceType::Dragon => dragon_effect(from, occupied),
            _ => unreachable!(),
        } & target;

        for to in attacks.iter() {
            add_move(buffer, Move::new_move_with_piece(from, to, false, pc));
        }
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
fn generate_pawn_drops(pos: &Position, target: Bitboard, buffer: &mut ExtMoveBuffer) {
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
        add_move(buffer, Move::new_drop_with_piece(PieceType::Pawn, to, dropped_pc));
    }
}

/// 歩以外の駒打ちを生成
fn generate_non_pawn_drops(pos: &Position, target: Bitboard, buffer: &mut ExtMoveBuffer) {
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
            add_move(buffer, Move::new_drop_with_piece(PieceType::Lance, to, dropped_pc));
        }
    }

    // 桂（1,2段目には打てない）
    if hand.has(PieceType::Knight) {
        let dropped_pc = crate::types::Piece::make(us, PieceType::Knight);
        for to in (target & empties & !rank12).iter() {
            add_move(buffer, Move::new_drop_with_piece(PieceType::Knight, to, dropped_pc));
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
                add_move(buffer, Move::new_drop_with_piece(pt, to, dropped_pc));
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
    buffer: &mut ExtMoveBuffer,
    targets: GenerateTargets,
    include_non_promotions: bool,
    pawn_promo_mode: PromotionMode,
    include_drops: bool,
) {
    // 駒の移動 (YaneuraOu movegen.cpp:generate_general 準拠の生成順序)
    generate_pawn_moves(pos, targets.pawn, buffer, pawn_promo_mode);
    generate_lance_moves(pos, targets.general, buffer, include_non_promotions);
    generate_knight_moves(pos, targets.general, buffer);
    generate_silver_moves(pos, targets.general, buffer);
    // 角+飛: GPM_BR — 1つの bitboard にまとめて pop 順で生成
    generate_br_moves(pos, targets.general, buffer, include_non_promotions);
    // 金相当+馬+龍+玉: GPM_GHDK — 1つの bitboard にまとめて pop 順で生成
    generate_ghdk_moves(pos, targets.general, buffer);

    if include_drops {
        let drop_target = targets.drop & !pos.occupied();
        generate_pawn_drops(pos, drop_target, buffer);
        generate_non_pawn_drops(pos, drop_target, buffer);
    }
}

/// 王手がかかっていないときの全ての指し手を生成（pseudo-legal）
pub fn generate_non_evasions(pos: &Position, buffer: &mut ExtMoveBuffer) -> usize {
    let us = pos.side_to_move();
    let targets = GenerateTargets::with_drop(!pos.pieces_c(us), !pos.occupied());
    generate_non_evasions_core(pos, buffer, targets, false, PromotionMode::PromoteOnly, true);
    buffer.len()
}

/// 王手回避手を生成（pseudo-legal）
fn generate_evasions_with_promos(
    pos: &Position,
    buffer: &mut ExtMoveBuffer,
    include_non_promotions: bool,
    pawn_promo_mode: PromotionMode,
) {
    debug_assert!(pos.in_check());

    let us = pos.side_to_move();
    let them = !us;
    let king_sq = pos.king_square(us);
    let checkers = pos.checkers();
    let occupied = pos.occupied();

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

    // 玉の駒情報（王手回避手に付加するため）
    let moved_pc = pos.piece_on(king_sq);
    for to in king_targets.iter() {
        // 移動先に敵の利きがないかは後でis_legalでチェック
        add_move(buffer, Move::new_move_with_piece(king_sq, to, false, moved_pc));
    }

    // 両王手なら玉移動のみ
    if checker_count >= 2 {
        return;
    }

    // 単王手の場合：合駒・取り返しを生成
    let checker_sq = checker_sq.expect("in_checkなら王手駒が存在する");
    let between = between_bb(checker_sq, king_sq);
    let drop_target = between; // 合駒は間の升
    let move_target = between | Bitboard::from_square(checker_sq); // 移動は間 + 王手駒

    // 玉以外の駒による移動（targetを制限, YO evasion準拠の生成順序）
    generate_pawn_moves(pos, move_target, buffer, pawn_promo_mode);
    generate_lance_moves(pos, move_target, buffer, include_non_promotions);
    generate_knight_moves(pos, move_target, buffer);
    generate_silver_moves(pos, move_target, buffer);
    // 角+飛: GPM_BR
    generate_br_moves(pos, move_target, buffer, include_non_promotions);
    // 金相当+馬+龍（玉なし）: GPM_GHD
    generate_ghd_moves(pos, move_target, buffer);

    // 駒打ち（合駒のみ）
    if !drop_target.is_empty() {
        generate_pawn_drops(pos, drop_target, buffer);
        generate_non_pawn_drops(pos, drop_target, buffer);
    }
}

/// 王手回避手を生成（pseudo-legal）
pub fn generate_evasions(pos: &Position, buffer: &mut ExtMoveBuffer) -> usize {
    generate_evasions_with_promos(pos, buffer, false, PromotionMode::PromoteOnly);
    buffer.len()
}

/// 駒1枚の利きを返す
#[inline]
fn piece_effect(pt: PieceType, us: Color, from: Square, occupied: Bitboard) -> Bitboard {
    match pt {
        PieceType::Pawn => pawn_effect(us, from),
        PieceType::Lance => lance_effect(us, from, occupied),
        PieceType::Knight => knight_effect(us, from),
        PieceType::Silver => silver_effect(us, from),
        PieceType::Gold
        | PieceType::ProPawn
        | PieceType::ProLance
        | PieceType::ProKnight
        | PieceType::ProSilver => gold_effect(us, from),
        PieceType::Bishop => bishop_effect(from, occupied),
        PieceType::Rook => rook_effect(from, occupied),
        PieceType::Horse => horse_effect(from, occupied),
        PieceType::Dragon => dragon_effect(from, occupied),
        PieceType::King => king_effect(from),
    }
}

/// 1つの駒から指定targetへの移動手を生成（成り処理込み）
/// YaneuraOu movegen.cpp の make_move_target_general 相当
/// 開き王手用: targetは「pin_lineから外れた移動先」
fn generate_moves_from_sq(
    pos: &Position,
    buffer: &mut ExtMoveBuffer,
    from: Square,
    target: Bitboard,
    include_non_promotions: bool,
    pawn_promo_mode: PromotionMode,
) {
    let us = pos.side_to_move();
    let pc = pos.piece_on(from);
    let pt = pc.piece_type();
    let occupied = pos.occupied();
    let effect = piece_effect(pt, us, from, occupied);
    let attacks = effect & target;
    if attacks.is_empty() {
        return;
    }

    let promo_ranks = enemy_field(us);
    let from_in_promo = promo_ranks.contains(from);

    match pt {
        PieceType::Pawn => {
            let rank1 = rank1_bb(us);
            for to in attacks.iter() {
                let in_promo = promo_ranks.contains(to);
                match (in_promo, pawn_promo_mode) {
                    (true, PromotionMode::PromoteOnly) => {
                        add_move(
                            buffer,
                            Move::new_move_with_piece(from, to, true, pc.promote().unwrap()),
                        );
                    }
                    (true, PromotionMode::Both) => {
                        add_move(
                            buffer,
                            Move::new_move_with_piece(from, to, true, pc.promote().unwrap()),
                        );
                        if !rank1.contains(to) {
                            add_move(buffer, Move::new_move_with_piece(from, to, false, pc));
                        }
                    }
                    (false, _) => {
                        add_move(buffer, Move::new_move_with_piece(from, to, false, pc));
                    }
                }
            }
        }
        PieceType::Lance => {
            let rank1 = rank1_bb(us);
            let rank12 = rank12_bb(us);
            let non_promo_mask = if include_non_promotions {
                !rank1
            } else {
                !rank12
            };
            let promoted_pc = pc.promote().unwrap();
            // Pass 1: 成り手
            for to in (attacks & promo_ranks).iter() {
                add_move(buffer, Move::new_move_with_piece(from, to, true, promoted_pc));
            }
            // Pass 2: 不成手
            for to in (attacks & non_promo_mask).iter() {
                add_move(buffer, Move::new_move_with_piece(from, to, false, pc));
            }
        }
        PieceType::Knight => {
            let rank12 = rank12_bb(us);
            let promoted_pc = pc.promote().unwrap();
            for to in attacks.iter() {
                if promo_ranks.contains(to) {
                    add_move(buffer, Move::new_move_with_piece(from, to, true, promoted_pc));
                    if !rank12.contains(to) {
                        add_move(buffer, Move::new_move_with_piece(from, to, false, pc));
                    }
                } else {
                    add_move(buffer, Move::new_move_with_piece(from, to, false, pc));
                }
            }
        }
        PieceType::Silver => {
            let promoted_pc = pc.promote().unwrap();
            if from_in_promo {
                for to in attacks.iter() {
                    add_move(buffer, Move::new_move_with_piece(from, to, true, promoted_pc));
                    add_move(buffer, Move::new_move_with_piece(from, to, false, pc));
                }
            } else {
                for to in (attacks & promo_ranks).iter() {
                    add_move(buffer, Move::new_move_with_piece(from, to, true, promoted_pc));
                    add_move(buffer, Move::new_move_with_piece(from, to, false, pc));
                }
                for to in (attacks & !promo_ranks).iter() {
                    add_move(buffer, Move::new_move_with_piece(from, to, false, pc));
                }
            }
        }
        PieceType::Bishop | PieceType::Rook => {
            let promoted_pc = pc.promote().unwrap();
            if from_in_promo {
                for to in attacks.iter() {
                    add_move(buffer, Move::new_move_with_piece(from, to, true, promoted_pc));
                    if include_non_promotions {
                        add_move(buffer, Move::new_move_with_piece(from, to, false, pc));
                    }
                }
            } else {
                for to in (attacks & promo_ranks).iter() {
                    add_move(buffer, Move::new_move_with_piece(from, to, true, promoted_pc));
                    if include_non_promotions {
                        add_move(buffer, Move::new_move_with_piece(from, to, false, pc));
                    }
                }
                for to in (attacks & !promo_ranks).iter() {
                    add_move(buffer, Move::new_move_with_piece(from, to, false, pc));
                }
            }
        }
        // 成れない駒（金相当・馬・龍・玉）
        _ => {
            for to in attacks.iter() {
                add_move(buffer, Move::new_move_with_piece(from, to, false, pc));
            }
        }
    }
}

/// 1つの駒から直接王手となる移動手のみ生成
/// YaneuraOu movegen.cpp の make_move_check 準拠
///
/// 2パス方式:
///   Pass 1 (成り): effect ∩ check_squares(promoted_pt) ∩ target → 成り手を生成
///   Pass 2 (不成): effect ∩ check_squares(raw_pt) ∩ target → 不成手を生成
/// 各パスは独立しており、同一マスに成り/不成の両方が出ることがある。
fn generate_direct_check_from_sq(
    pos: &Position,
    buffer: &mut ExtMoveBuffer,
    from: Square,
    target: Bitboard,
    include_non_promotions: bool,
    pawn_promo_mode: PromotionMode,
) {
    let us = pos.side_to_move();
    let pc = pos.piece_on(from);
    let pt = pc.piece_type();
    let occupied = pos.occupied();
    let effect = piece_effect(pt, us, from, occupied);
    let promo_ranks = enemy_field(us);
    let from_in_promo = promo_ranks.contains(from);
    if let Some(promoted_pt) = pt.promote() {
        let promoted_pc = pc.promote().unwrap();
        let check_sq_promoted = pos.check_squares(promoted_pt);
        let check_sq_raw = pos.check_squares(pt);

        // --- Pass 1: 成り王手 (YO: make_move_target_pro<..., true>) ---
        // 成って王手になる移動先
        let promo_dst = effect & check_sq_promoted & target;
        // 成り条件: from か to が敵陣
        let promo_dst = if from_in_promo {
            promo_dst
        } else {
            promo_dst & promo_ranks
        };

        for to in promo_dst.iter() {
            add_move(buffer, Move::new_move_with_piece(from, to, true, promoted_pc));
        }

        // --- Pass 2: 不成王手 (YO: make_move_target_pro<..., false>) ---
        // 不成で王手になる移動先
        let nonpro_dst = effect & check_sq_raw & target;

        match pt {
            PieceType::Pawn => {
                // YO make_move_target_pro<PAWN, false>:
                //   All=false → !canPromote(Us, to) のみ生成
                //   All=true  → rank_of(to) != RANK_1 のみ生成
                let rank1 = rank1_bb(us);
                let mask = match pawn_promo_mode {
                    PromotionMode::PromoteOnly => !promo_ranks, // All=false: 非敵陣のみ
                    PromotionMode::Both => !rank1,              // All=true: 1段目以外
                };
                for to in (nonpro_dst & mask).iter() {
                    add_move(buffer, Move::new_move_with_piece(from, to, false, pc));
                }
            }
            PieceType::Lance => {
                // YO make_move_target_pro<LANCE, false>:
                //   All=false → rank >= 3 (先手) つまり !rank12
                //   All=true  → rank != 1 つまり !rank1
                let rank1 = rank1_bb(us);
                let rank12 = rank12_bb(us);
                let mask = if include_non_promotions {
                    !rank1
                } else {
                    !rank12
                };
                for to in (nonpro_dst & mask).iter() {
                    add_move(buffer, Move::new_move_with_piece(from, to, false, pc));
                }
            }
            PieceType::Knight => {
                // YO make_move_target_pro<KNIGHT, false>:
                //   rank >= 3 (先手) つまり !rank12。AllフラグはKNIGHTに影響しない
                let rank12 = rank12_bb(us);
                for to in (nonpro_dst & !rank12).iter() {
                    add_move(buffer, Move::new_move_with_piece(from, to, false, pc));
                }
            }
            PieceType::Silver => {
                // YO make_move_target_pro<SILVER, false>: 常に生成
                for to in nonpro_dst.iter() {
                    add_move(buffer, Move::new_move_with_piece(from, to, false, pc));
                }
            }
            PieceType::Bishop | PieceType::Rook => {
                // YO make_move_target_pro<BISHOP/ROOK, false>:
                //   !(canPromote(Us, from) || canPromote(Us, to)) || All
                //   = 成れない位置、または All=true のとき不成を生成
                let mask = if include_non_promotions {
                    Bitboard::ALL // All=true: 常に生成
                } else if from_in_promo {
                    Bitboard::EMPTY // from が敵陣: 成り優先で不成は生成しない
                } else {
                    !promo_ranks // to が非敵陣のときのみ不成を生成
                };
                for to in (nonpro_dst & mask).iter() {
                    add_move(buffer, Move::new_move_with_piece(from, to, false, pc));
                }
            }
            _ => unreachable!(),
        }
    } else {
        // 成れない駒（金相当・馬・龍）: check_squares(pt) で直接マッチ
        let check_sq = pos.check_squares(pt);
        let dst = effect & check_sq & target;
        for to in dst.iter() {
            add_move(buffer, Move::new_move_with_piece(from, to, false, pc));
        }
    }
}

/// 王手生成（YaneuraOu movegen.cpp generate_checks 準拠の順序）
///
/// 生成順序:
/// 1. blockers (開き王手候補) を LSB 順に処理
/// 2. 非 blocker の直接王手候補を LSB 順に処理
/// 3. 駒打ち王手を PAWN, LANCE, KNIGHT, SILVER, GOLD, BISHOP, ROOK の順
fn generate_checks(
    pos: &Position,
    buffer: &mut ExtMoveBuffer,
    include_non_promotions: bool,
    pawn_promo_mode: PromotionMode,
    quiet_only: bool,
) {
    let us = pos.side_to_move();
    let them = !us;
    let them_king = pos.king_square(them);
    let occupied = pos.occupied();

    let target = if quiet_only {
        !occupied
    } else {
        !pos.pieces_c(us)
    };

    // YaneuraOu準拠: y = blockers_for_king(Them) & pieces(Us)
    let blockers = pos.blockers_for_king(them) & pos.pieces_c(us);

    // --- Phase 1: blockers (開き王手候補) を LSB 順に処理 ---
    for from in blockers.iter() {
        let pin_line = line_bb(them_king, from);

        // 開き王手: pin_line から外れる移動先
        let disc_target = target & !pin_line;
        generate_moves_from_sq(
            pos,
            buffer,
            from,
            disc_target,
            include_non_promotions,
            pawn_promo_mode,
        );

        // blocker かつ直接王手候補でもある場合: pin_line 上の直接王手
        let direct_on_line = target & pin_line;
        if !direct_on_line.is_empty() {
            generate_direct_check_from_sq(
                pos,
                buffer,
                from,
                direct_on_line,
                include_non_promotions,
                pawn_promo_mode,
            );
        }
    }

    // --- Phase 2: 非 blocker の直接王手候補を LSB 順に処理 ---
    // YaneuraOu準拠: check_candidate_bb で直接王手可能な駒のみフィルタ
    let candidates = (pos.pieces(us, PieceType::Pawn)
        & check_candidate_bb(us, PieceType::Pawn, them_king))
        | (pos.pieces(us, PieceType::Lance)
            & check_candidate_bb(us, PieceType::Lance, them_king))
        | (pos.pieces(us, PieceType::Knight)
            & check_candidate_bb(us, PieceType::Knight, them_king))
        | (pos.pieces(us, PieceType::Silver)
            & check_candidate_bb(us, PieceType::Silver, them_king))
        | (pos.golds_c(us) & check_candidate_bb(us, PieceType::Gold, them_king))
        | (pos.pieces(us, PieceType::Bishop)
            & check_candidate_bb(us, PieceType::Bishop, them_king))
        | (pos.rook_dragon() & pos.pieces_c(us)) // 飛・龍は全域候補
        | (pos.pieces(us, PieceType::Horse)
            & check_candidate_bb(us, PieceType::Horse, them_king));
    let non_blockers = candidates & !blockers;
    for from in non_blockers.iter() {
        generate_direct_check_from_sq(
            pos,
            buffer,
            from,
            target,
            include_non_promotions,
            pawn_promo_mode,
        );
    }

    // --- Phase 3: 駒打ち王手 (PAWN, LANCE, KNIGHT, SILVER, GOLD, BISHOP, ROOK 順) ---
    let empties = !occupied;
    let hand = pos.hand(us);

    // 歩打ち王手
    if hand.has(PieceType::Pawn) {
        let check_target = pos.check_squares(PieceType::Pawn) & empties;
        if !check_target.is_empty() {
            // 歩の王手マスは1箇所のみ。二歩/打ち歩詰めチェックはis_legalで行う
            let rank1 = rank1_bb(us);
            let our_pawns = pos.pieces(us, PieceType::Pawn);
            let valid = check_target & !rank1 & pawn_drop_mask(us, our_pawns);
            let dropped_pc = crate::types::Piece::make(us, PieceType::Pawn);
            for to in valid.iter() {
                add_move(buffer, Move::new_drop_with_piece(PieceType::Pawn, to, dropped_pc));
            }
        }
    }

    // 香打ち王手
    if hand.has(PieceType::Lance) {
        let check_target = pos.check_squares(PieceType::Lance) & empties;
        let rank1 = rank1_bb(us);
        let dropped_pc = crate::types::Piece::make(us, PieceType::Lance);
        for to in (check_target & !rank1).iter() {
            add_move(buffer, Move::new_drop_with_piece(PieceType::Lance, to, dropped_pc));
        }
    }

    // 桂打ち王手
    if hand.has(PieceType::Knight) {
        let check_target = pos.check_squares(PieceType::Knight) & empties;
        let rank12 = rank12_bb(us);
        let dropped_pc = crate::types::Piece::make(us, PieceType::Knight);
        for to in (check_target & !rank12).iter() {
            add_move(buffer, Move::new_drop_with_piece(PieceType::Knight, to, dropped_pc));
        }
    }

    // 銀・金・角・飛打ち王手
    for pt in [
        PieceType::Silver,
        PieceType::Gold,
        PieceType::Bishop,
        PieceType::Rook,
    ] {
        if hand.has(pt) {
            let check_target = pos.check_squares(pt) & empties;
            let dropped_pc = crate::types::Piece::make(us, pt);
            for to in check_target.iter() {
                add_move(buffer, Move::new_drop_with_piece(pt, to, dropped_pc));
            }
        }
    }
}

fn generate_recaptures(
    pos: &Position,
    buffer: &mut ExtMoveBuffer,
    sq: Square,
    include_non_promotions: bool,
    pawn_promo_mode: PromotionMode,
) {
    let target = Bitboard::from_square(sq);
    // YaneuraOuのRECAPTURESは移動のみ（駒打ちは含めない）
    let targets = GenerateTargets::new(target);
    generate_non_evasions_core(
        pos,
        buffer,
        targets,
        include_non_promotions,
        pawn_promo_mode,
        false,
    );
}

/// GenType に応じた指し手生成（pseudo-legal）
pub fn generate_with_type(
    pos: &Position,
    gen_type: crate::movegen::GenType,
    buffer: &mut ExtMoveBuffer,
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
            generate_non_evasions_core(
                pos,
                buffer,
                targets,
                false,
                PromotionMode::PromoteOnly,
                true,
            );
        }
        NonEvasionsAll => {
            let targets = GenerateTargets::with_drop(!pos.pieces_c(us), empties);
            generate_non_evasions_core(pos, buffer, targets, true, PromotionMode::Both, true);
        }
        Quiets => {
            let targets = GenerateTargets::with_drop(empties, empties);
            generate_non_evasions_core(
                pos,
                buffer,
                targets,
                false,
                PromotionMode::PromoteOnly,
                true,
            );
        }
        QuietsAll => {
            let targets = GenerateTargets::with_drop(empties, empties);
            generate_non_evasions_core(pos, buffer, targets, true, PromotionMode::Both, true);
        }
        QuietsProMinus => {
            let targets = GenerateTargets::with_drop(empties, empties);
            // QUIETS_PRO_MINUS は「歩の静かな成りを含めない」以外は通常のQUIETSと同じ。
            let mut temp_buffer = ExtMoveBuffer::new();
            generate_non_evasions_core(
                pos,
                &mut temp_buffer,
                targets,
                false,
                PromotionMode::PromoteOnly,
                true,
            );

            // 歩の静かな成りを除外するためフィルタ
            for ext in temp_buffer.iter() {
                if pos.is_capture(ext.mv) {
                    buffer.push_move(ext.mv);
                    continue;
                }
                let from = ext.mv.from();
                let to = ext.mv.to();
                let pt = pos.piece_on(from).piece_type();
                if !(pt == PieceType::Pawn
                    && ext.mv.is_promotion()
                    && enemy_field(pos.side_to_move()).contains(to))
                {
                    buffer.push_move(ext.mv);
                }
            }
        }
        QuietsProMinusAll => {
            let targets = GenerateTargets::with_drop(empties, empties);
            // QUIETS_PRO_MINUS_ALL も歩の静かな成りのみ除外（不成生成は許容）
            let mut temp_buffer = ExtMoveBuffer::new();
            generate_non_evasions_core(
                pos,
                &mut temp_buffer,
                targets,
                true,
                PromotionMode::Both,
                true,
            );

            for ext in temp_buffer.iter() {
                if pos.is_capture(ext.mv) {
                    buffer.push_move(ext.mv);
                    continue;
                }
                let from = ext.mv.from();
                let to = ext.mv.to();
                let pt = pos.piece_on(from).piece_type();
                if !(pt == PieceType::Pawn
                    && ext.mv.is_promotion()
                    && enemy_field(pos.side_to_move()).contains(to))
                {
                    buffer.push_move(ext.mv);
                }
            }
        }
        Captures => {
            let targets = GenerateTargets::new(enemy);
            generate_non_evasions_core(
                pos,
                buffer,
                targets,
                false,
                PromotionMode::PromoteOnly,
                false,
            );
        }
        CapturesAll => {
            let targets = GenerateTargets::new(enemy);
            generate_non_evasions_core(pos, buffer, targets, true, PromotionMode::Both, false);
        }
        CapturesProPlus => {
            let targets = GenerateTargets::new(enemy);
            generate_non_evasions_core(
                pos,
                buffer,
                targets,
                false,
                PromotionMode::PromoteOnly,
                false,
            );
        }
        CapturesProPlusAll => {
            let targets = GenerateTargets::new(enemy);
            generate_non_evasions_core(pos, buffer, targets, true, PromotionMode::Both, false);
        }
        Recaptures => {
            let sq = recapture_sq.expect("Recaptures requires a target square");
            generate_recaptures(pos, buffer, sq, false, PromotionMode::PromoteOnly);
        }
        RecapturesAll => {
            let sq = recapture_sq.expect("RecapturesAll requires a target square");
            generate_recaptures(pos, buffer, sq, true, PromotionMode::Both);
        }
        Evasions => {
            generate_evasions_with_promos(pos, buffer, false, PromotionMode::PromoteOnly);
        }
        EvasionsAll => {
            generate_evasions_with_promos(pos, buffer, true, PromotionMode::Both);
        }
        Legal => {
            let mut temp_buffer = ExtMoveBuffer::new();
            if pos.in_check() {
                generate_evasions_with_promos(
                    pos,
                    &mut temp_buffer,
                    false,
                    PromotionMode::PromoteOnly,
                );
            } else {
                let targets = GenerateTargets::with_drop(!pos.pieces_c(us), empties);
                generate_non_evasions_core(
                    pos,
                    &mut temp_buffer,
                    targets,
                    false,
                    PromotionMode::PromoteOnly,
                    true,
                );
            };
            for ext in temp_buffer.iter() {
                if pos.is_legal(ext.mv) {
                    buffer.push_move(ext.mv);
                }
            }
        }
        LegalAll => {
            let mut temp_buffer = ExtMoveBuffer::new();
            if pos.in_check() {
                generate_evasions_with_promos(pos, &mut temp_buffer, true, PromotionMode::Both);
            } else {
                let targets = GenerateTargets::with_drop(!pos.pieces_c(us), empties);
                generate_non_evasions_core(
                    pos,
                    &mut temp_buffer,
                    targets,
                    true,
                    PromotionMode::Both,
                    true,
                );
            };
            for ext in temp_buffer.iter() {
                if pos.is_legal(ext.mv) {
                    buffer.push_move(ext.mv);
                }
            }
        }
        Checks | ChecksAll | QuietChecks | QuietChecksAll => {
            let include_non_promotions = matches!(gen_type, ChecksAll | QuietChecksAll);
            let pawn_mode = if include_non_promotions {
                PromotionMode::Both
            } else {
                PromotionMode::PromoteOnly
            };
            let quiet_only = matches!(gen_type, QuietChecks | QuietChecksAll);

            generate_checks(pos, buffer, include_non_promotions, pawn_mode, quiet_only);
        }
    }
    buffer.len()
}

/// 全ての指し手を生成（王手の有無で分岐）
pub fn generate_all(pos: &Position, buffer: &mut ExtMoveBuffer) -> usize {
    if pos.in_check() {
        generate_evasions(pos, buffer)
    } else {
        generate_non_evasions(pos, buffer)
    }
}

/// 合法手を生成
pub fn generate_legal(pos: &Position, list: &mut MoveList) {
    let mut buffer = ExtMoveBuffer::new();
    generate_all(pos, &mut buffer);

    for ext in buffer.iter() {
        if pos.is_legal(ext.mv) {
            list.push(ext.mv);
        }
    }
}

/// 合法手を生成（不成含む）
/// 合法手を生成（不成含む）
///
/// `generate_legal()` と異なり、成ることが可能な駒が敵陣に移動する際に、
/// 成る手だけでなく成らない手も生成します。
///
/// # 使用目的
/// - UI での指し手選択肢の表示（ユーザーが不成を選択できるようにする）
/// - 棋譜再生時の指し手検証（不成の指し手も合法として認識する）
///
/// # 注意
/// 探索エンジンでの使用は非推奨です。探索では `generate_legal()` を使用してください。
pub fn generate_legal_all(pos: &Position, list: &mut MoveList) {
    let mut buffer = ExtMoveBuffer::new();
    generate_with_type(pos, crate::movegen::GenType::LegalAll, &mut buffer, None);

    for ext in buffer.iter() {
        list.push(ext.mv);
    }
}

// ============================================================================
// パス権対応の合法手生成
// ============================================================================

/// パス手を含む合法手を生成
///
/// `generate_legal()` の結果に加えて、パス可能な場合は `Move::PASS` を追加する。
///
/// # 使用条件
/// - パス権ルールが有効な場合のみ PASS が生成される
/// - 王手中はパス不可（can_pass() が false を返す）
///
/// # 注意
/// 探索の qsearch (静止探索) では使用しないこと。
/// qsearch では駒取り手のみを生成すべきであり、PASS は不要。
pub fn generate_legal_with_pass(pos: &Position, list: &mut MoveList) {
    generate_legal(pos, list);

    // パス可能な場合のみ追加
    if pos.can_pass() {
        list.push(Move::PASS);
    }
}

/// パス手を含む合法手を生成（不成含む）
/// パス手を含む合法手を生成（不成含む）
///
/// `generate_legal_all()` の結果に加えて、パス可能な場合は `Move::PASS` を追加します。
///
/// # 使用条件
/// - パス権ルールが有効な場合のみ PASS が生成される
/// - 王手中はパス不可（can_pass() が false を返す）
///
/// # 使用目的
/// - UI での指し手選択肢の表示（不成とパスの両方を含む）
/// - 棋譜再生時の指し手検証（不成とパスの両方を合法として認識）
///
/// # 注意
/// 探索の qsearch (静止探索) では使用しないこと。
/// qsearch では駒取り手のみを生成すべきであり、PASS や不成は不要。
pub fn generate_legal_all_with_pass(pos: &Position, list: &mut MoveList) {
    generate_legal_all(pos, list);

    // パス可能な場合のみ追加
    if pos.can_pass() {
        list.push(Move::PASS);
    }
}

/// パス手を含む合法性チェック
///
/// Move::PASS の場合は `pos.can_pass()` で判定し、
/// それ以外は `pos.is_legal()` に委譲する。
#[inline]
pub fn is_legal_with_pass(pos: &Position, m: Move) -> bool {
    if m.is_pass() {
        return pos.can_pass();
    }
    pos.is_legal(m)
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
        // PASS の場合は can_pass() で判定
        if mv.is_pass() {
            return self.can_pass();
        }

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
        let to_pc = self.piece_on(to);

        // 移動先に自駒がある/敵玉がいる手は非合法
        if to_pc.is_some() {
            if to_pc.color() == us {
                return false;
            }
            if to_pc.piece_type() == PieceType::King {
                return false;
            }
        }

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

        let mut buffer = ExtMoveBuffer::new();
        let count = generate_non_evasions(&pos, &mut buffer);

        // 初期局面の合法手は30手
        // ただしpseudo-legalなので多めに生成される可能性あり
        assert!(count >= 30, "Generated {count} moves");

        // すべての生成手がpiece情報を持つことを検証
        for ext in buffer.as_slice().iter().take(count) {
            assert!(ext.mv.has_piece_info(), "生成手はpiece情報を持つ必要がある: {:?}", ext.mv);
        }
    }

    #[test]
    fn test_generate_legal_hirate() {
        let mut pos = Position::new();
        pos.set_hirate();

        let mut list = MoveList::new();
        generate_legal(&pos, &mut list);

        // 初期局面の合法手は30手
        assert_eq!(list.len(), 30, "Generated {} legal moves", list.len());

        // すべての合法手がpiece情報を持つことを検証
        for mv in list.iter() {
            assert!(mv.has_piece_info(), "合法手はpiece情報を持つ必要がある: {:?}", mv);
        }
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

        let mut buffer = ExtMoveBuffer::new();
        let count = generate_evasions(&pos, &mut buffer);

        for ext in buffer.as_slice().iter().take(count) {
            assert!(pos.is_legal(ext.mv), "王手回避の生成には自殺手を含めない: {:?}", ext.mv);
            assert!(ext.mv.has_piece_info(), "王手回避手はpiece情報を持つ必要がある: {:?}", ext.mv);
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

        let mut buf = ExtMoveBuffer::new();

        let count = generate_with_type(&pos, crate::movegen::GenType::ChecksAll, &mut buf, None);
        assert!(count > 0);

        for ext in buf.iter() {
            assert_eq!(ext.mv.from(), from);
            assert!(pos.gives_check(ext.mv), "非チェック手が混入: {:?}", ext.mv);
            assert!(ext.mv.has_piece_info(), "王手生成手はpiece情報を持つ必要がある: {:?}", ext.mv);
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

        let mut buf = ExtMoveBuffer::new();
        let count = generate_with_type(
            &pos,
            crate::movegen::GenType::Recaptures,
            &mut buf,
            Some(recapture_sq),
        );
        assert!(count > 0);
        for ext in buf.iter() {
            assert_eq!(ext.mv.to(), recapture_sq, "他升への手が混入: {:?}", ext.mv);
        }
    }

    #[test]
    fn test_bishop_promotion_only_in_default_mode() {
        // 5二の先手角は敵陣内。通常生成では成りのみ。
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/9/9/9/9/9/4B4/4K4 b - 1").unwrap();

        let mut buf = ExtMoveBuffer::new();
        let count = generate_with_type(&pos, crate::movegen::GenType::NonEvasions, &mut buf, None);
        let from = pos
            .pieces(Color::Black, PieceType::Bishop)
            .iter()
            .next()
            .expect("角が存在しない");
        let enemy = enemy_field(pos.side_to_move());

        let has_non_promo = buf.as_slice()[..count].iter().any(|ext| {
            ext.mv.from() == from && enemy.contains(ext.mv.to()) && !ext.mv.is_promotion()
        });
        assert!(!has_non_promo, "通常モードでは敵陣への角移動は成りのみのはず");
    }

    #[test]
    fn test_bishop_promotion_and_unpromotion_in_all_mode() {
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/9/9/9/9/9/4B4/4K4 b - 1").unwrap();

        let mut buf = ExtMoveBuffer::new();
        let count =
            generate_with_type(&pos, crate::movegen::GenType::NonEvasionsAll, &mut buf, None);
        let from = pos
            .pieces(Color::Black, PieceType::Bishop)
            .iter()
            .next()
            .expect("角が存在しない");

        let has_non_promo = buf.as_slice()[..count]
            .iter()
            .any(|ext| ext.mv.from() == from && !ext.mv.is_promotion());
        assert!(has_non_promo, "All モードでは不成も生成する");
    }

    #[test]
    fn test_generate_legal_all_includes_bishop_non_promote() {
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/9/9/9/9/9/4B4/4K4 b - 1").unwrap();

        let mut list = MoveList::new();
        generate_legal_all(&pos, &mut list);

        let from = pos
            .pieces(Color::Black, PieceType::Bishop)
            .iter()
            .next()
            .expect("角が存在しない");
        let has_non_promo = list.iter().any(|m| m.from() == from && !m.is_promotion());
        assert!(has_non_promo, "generate_legal_all は不成の角移動も含むべき");
    }

    #[test]
    fn test_quiets_pro_minus_omits_pawn_promotion() {
        // 5四の歩が5三に進む静かな手は不成のみ（QuietsProMinus）。
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/9/9/9/4P4/9/9/4K4 b - 1").unwrap();

        let mut buf = ExtMoveBuffer::new();
        let count =
            generate_with_type(&pos, crate::movegen::GenType::QuietsProMinus, &mut buf, None);
        let from = pos.pieces(Color::Black, PieceType::Pawn).iter().next().expect("歩が存在しない");
        let to = pawn_effect(Color::Black, from).iter().next().expect("歩の利きがない");

        let moves_from: Vec<Move> = buf.as_slice()[..count]
            .iter()
            .map(|ext| ext.mv)
            .filter(|m| m.from() == from && m.to() == to)
            .collect();
        assert!(!moves_from.is_empty(), "対象の手が生成されていない: {moves_from:?}");

        let has_non_promo = buf.as_slice()[..count]
            .iter()
            .any(|ext| ext.mv.from() == from && ext.mv.to() == to && !ext.mv.is_promotion());
        let has_promo = buf.as_slice()[..count]
            .iter()
            .any(|ext| ext.mv.from() == from && ext.mv.to() == to && ext.mv.is_promotion());

        assert!(has_non_promo, "不成の静かな手は生成される");
        assert!(!has_promo, "QuietsProMinusでは歩の静かな成りは生成しないはず");
    }

    #[test]
    fn test_knight_capture_3a4c_is_generated() {
        // G*4c後の局面：後手番、3一の桂馬が4三の金を取る手が生成されるか
        // この局面は王手がかかっており、3a4cは王手をかけている金を取る回避手
        let mut pos = Position::new();
        pos.set_sfen(
            "6n1l/2+S1k4/2lp1G2p/1np1B2b1/3PP4/1N1S3rP/1P2+pPP+p1/1p1G5/3KG2r1 w SN2L4Pgs2p 2",
        )
        .unwrap();

        assert!(pos.in_check(), "この局面は王手がかかっている");

        let mut list = MoveList::new();
        generate_legal(&pos, &mut list);

        // 3a4c (3一桂→4三、金を取る) が含まれているか
        let found = list.iter().any(|m| m.to_usi() == "3a4c");

        assert!(found, "3a4c（桂馬で金を取る手）が生成されていない");
    }

    /// 桂馬が敵陣3段目に移動する場合、成りと不成の両方が生成されることを確認
    #[test]
    fn test_knight_to_rank3_generates_both_promote_and_non_promote() {
        // 先手の桂馬が7五(7e)から移動する局面
        // 移動先：6三(6c)と8三(8c)、どちらも3段目(c)
        // 3段目は敵陣だが行き場があるので、成り/不成の両方が生成されるべき
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/9/9/2N6/9/9/9/4K4 b - 1").unwrap();

        let mut list = MoveList::new();
        generate_legal(&pos, &mut list);

        // 6c は3段目→成り/不成両方生成
        let has_6c_promote = list.iter().any(|m| m.to_usi() == "7e6c+");
        let has_6c_non_promote = list.iter().any(|m| m.to_usi() == "7e6c");
        assert!(has_6c_promote, "桂馬の成り手 7e6c+ が生成されていない");
        assert!(
            has_6c_non_promote,
            "桂馬の不成手 7e6c が生成されていない（3段目なので不成も合法）"
        );

        // 8c も3段目→成り/不成両方生成
        let has_8c_promote = list.iter().any(|m| m.to_usi() == "7e8c+");
        let has_8c_non_promote = list.iter().any(|m| m.to_usi() == "7e8c");
        assert!(has_8c_promote, "桂馬の成り手 7e8c+ が生成されていない");
        assert!(
            has_8c_non_promote,
            "桂馬の不成手 7e8c が生成されていない（3段目なので不成も合法）"
        );
    }

    /// 桂馬が敵陣1段目に移動する場合、成りのみが生成されることを確認
    /// 1段目は行き場がないので不成は不可能
    #[test]
    fn test_knight_to_rank1_generates_only_promote() {
        // 先手の桂馬が7三(7c)から移動する局面
        // 移動先：6一(6a)と8一(8a)、どちらも1段目(a)
        // 1段目は行き場がないので成りのみ生成されるべき
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/2N6/9/9/9/9/9/4K4 b - 1").unwrap();

        let mut list = MoveList::new();
        generate_legal(&pos, &mut list);

        // 6a は1段目→成りのみ
        let has_6a_promote = list.iter().any(|m| m.to_usi() == "7c6a+");
        let has_6a_non_promote = list.iter().any(|m| m.to_usi() == "7c6a");
        assert!(has_6a_promote, "7c6a+ が生成されていない");
        assert!(!has_6a_non_promote, "7c6a（不成）は生成されてはいけない（1段目は行き場がない）");

        // 8a は1段目→成りのみ
        let has_8a_promote = list.iter().any(|m| m.to_usi() == "7c8a+");
        let has_8a_non_promote = list.iter().any(|m| m.to_usi() == "7c8a");
        assert!(has_8a_promote, "7c8a+ が生成されていない");
        assert!(!has_8a_non_promote, "7c8a（不成）は生成されてはいけない（1段目は行き場がない）");
    }

    /// 桂馬が敵陣2段目に移動する場合も成りのみが生成されることを確認
    #[test]
    fn test_knight_to_rank2_generates_only_promote() {
        // 先手の桂馬が7四(7d)から移動する局面
        // 移動先：6二(6b)と8二(8b)、どちらも2段目(b)
        // 2段目は行き場がないので成りのみ生成されるべき
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/9/2N6/9/9/9/9/4K4 b - 1").unwrap();

        let mut list = MoveList::new();
        generate_legal(&pos, &mut list);

        // 8b は2段目→成りのみ
        let has_8b_promote = list.iter().any(|m| m.to_usi() == "7d8b+");
        let has_8b_non_promote = list.iter().any(|m| m.to_usi() == "7d8b");
        assert!(has_8b_promote, "7d8b+ が生成されていない");
        assert!(!has_8b_non_promote, "7d8b（不成）は生成されてはいけない（2段目は行き場がない）");

        // 6b は2段目→成りのみ
        let has_6b_promote = list.iter().any(|m| m.to_usi() == "7d6b+");
        let has_6b_non_promote = list.iter().any(|m| m.to_usi() == "7d6b");
        assert!(has_6b_promote, "7d6b+ が生成されていない");
        assert!(!has_6b_non_promote, "7d6b（不成）は生成されてはいけない（2段目は行き場がない）");
    }

    // =========================================
    // パス権対応テスト
    // =========================================

    #[test]
    fn test_generate_legal_with_pass_no_pass_rights() {
        // パス権なしの場合、PASSは生成されない
        let mut pos = Position::new();
        pos.set_hirate();

        let mut list = MoveList::new();
        generate_legal_with_pass(&pos, &mut list);

        // 通常の合法手は生成される
        assert!(!list.is_empty());
        // PASSは含まれない
        assert!(
            !list.iter().any(|m| m.is_pass()),
            "PASS should not be generated without pass rights"
        );
    }

    #[test]
    fn test_generate_legal_with_pass_with_pass_rights() {
        // パス権ありの場合、PASSも生成される
        let mut pos = Position::new();
        pos.set_startpos_with_pass_rights(2, 2);

        let mut list = MoveList::new();
        generate_legal_with_pass(&pos, &mut list);

        // PASSが含まれる
        assert!(list.iter().any(|m| m.is_pass()), "PASS should be generated with pass rights");
    }

    #[test]
    fn test_generate_legal_with_pass_in_check() {
        // 王手中はPASSが生成されない
        // 5a: 後手玉, 5b: 先手金（後手玉に王手）, 5i: 先手玉
        let sfen = "4k4/4G4/9/9/9/9/9/9/4K4 w - 1";
        let mut pos = Position::new();
        pos.set_sfen_with_pass_rights(sfen, 2, 2).unwrap();

        // 後手番で王手されている
        assert!(pos.in_check());
        assert!(!pos.can_pass());

        let mut list = MoveList::new();
        generate_legal_with_pass(&pos, &mut list);

        // PASSは含まれない
        assert!(!list.iter().any(|m| m.is_pass()), "PASS should not be generated when in check");
    }

    #[test]
    fn test_is_legal_with_pass_normal_move() {
        let mut pos = Position::new();
        pos.set_hirate();

        // 通常の合法手
        let mv = Move::from_usi("7g7f").unwrap();
        assert!(is_legal_with_pass(&pos, mv));

        // is_legal_with_pass は通常手に対して is_legal と同じ結果を返す
        assert_eq!(is_legal_with_pass(&pos, mv), pos.is_legal(mv));
    }

    #[test]
    fn test_is_legal_with_pass_pass_move() {
        // パス権なし
        let mut pos = Position::new();
        pos.set_hirate();
        assert!(!is_legal_with_pass(&pos, Move::PASS));

        // パス権あり
        pos.set_startpos_with_pass_rights(2, 2);
        assert!(is_legal_with_pass(&pos, Move::PASS));

        // パス権あり、王手中
        let sfen = "4k4/4G4/9/9/9/9/9/9/4K4 w - 1";
        pos.set_sfen_with_pass_rights(sfen, 2, 2).unwrap();
        assert!(!is_legal_with_pass(&pos, Move::PASS));
    }

    #[test]
    fn test_generate_legal_with_pass_count() {
        // PASSが生成される場合、合法手数が1増える
        let mut pos = Position::new();
        pos.set_hirate();

        let mut list_without_pass = MoveList::new();
        generate_legal(&pos, &mut list_without_pass);

        // パス権を有効化
        pos.set_startpos_with_pass_rights(2, 2);

        let mut list_with_pass = MoveList::new();
        generate_legal_with_pass(&pos, &mut list_with_pass);

        assert_eq!(
            list_with_pass.len(),
            list_without_pass.len() + 1,
            "With pass rights, legal move count should increase by 1"
        );
    }

    /// generate_checks が旧フィルタ方式と同じ手集合（順序は問わない）を生成するか検証
    #[test]
    fn test_generate_checks_set_matches_filter() {
        use std::collections::HashSet;

        let sfens = [
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
            // 7g7f 後
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 2",
            // 中盤想定
            "ln1gk2nl/1rs1g2b1/pppppp1pp/6p2/9/2P1P4/PP1P1PPPP/1B2G2R1/LNS1KGSNL b - 1",
            // 手駒あり
            "4k4/9/9/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b GS 1",
            // 開き王手可能局面
            "4k4/4r4/4S4/9/9/9/9/9/4K4 b - 1",
        ];

        for sfen in &sfens {
            let mut pos = Position::new();
            pos.set_sfen(sfen).unwrap();
            if pos.in_check() {
                continue;
            }

            for quiet_only in [true, false] {
                for include_non_promo in [true, false] {
                    let pawn_mode = if include_non_promo {
                        PromotionMode::Both
                    } else {
                        PromotionMode::PromoteOnly
                    };

                    // 新コード
                    let mut buf_new = ExtMoveBuffer::new();
                    generate_checks(&pos, &mut buf_new, include_non_promo, pawn_mode, quiet_only);

                    // 旧フィルタ方式
                    let mut buf_old = ExtMoveBuffer::new();
                    {
                        let us = pos.side_to_move();
                        let empties = !pos.occupied();
                        let targets = if quiet_only {
                            GenerateTargets::with_drop(empties, empties)
                        } else {
                            GenerateTargets::with_drop(!pos.pieces_c(us), empties)
                        };
                        let mut temp = ExtMoveBuffer::new();
                        generate_non_evasions_core(
                            &pos,
                            &mut temp,
                            targets,
                            include_non_promo,
                            pawn_mode,
                            true,
                        );
                        for ext in temp.iter() {
                            if quiet_only && pos.is_capture(ext.mv) {
                                continue;
                            }
                            if pos.gives_check(ext.mv) {
                                buf_old.push_move(ext.mv);
                            }
                        }
                    }

                    let set_new: HashSet<u16> =
                        buf_new.as_slice().iter().map(|e| e.mv.raw()).collect();
                    let set_old: HashSet<u16> =
                        buf_old.as_slice().iter().map(|e| e.mv.raw()).collect();

                    let missing: Vec<_> = set_old.difference(&set_new).collect();
                    let extra: Vec<_> = set_new.difference(&set_old).collect();

                    assert!(missing.is_empty() && extra.is_empty(),
                        "sfen={sfen} quiet_only={quiet_only} include_non_promo={include_non_promo}\n\
                         missing={missing:?} extra={extra:?}\n\
                         new_count={} old_count={}",
                        buf_new.len(), buf_old.len());
                }
            }
        }
    }
}
