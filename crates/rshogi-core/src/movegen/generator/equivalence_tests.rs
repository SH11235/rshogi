//! 生成順序と駒情報を含む、本体とテスト専用参照実装の直接比較。

use super::*;
use crate::movegen::GenType;
use crate::position::SFEN_HIRATE;
use crate::position::playout_test_support::{PERFT_MATSURI, PERFT_MIDGAME, RandomPlayout};

#[test]
fn pawn_shift_matches_effect_on_every_square() {
    for us in [Color::Black, Color::White] {
        for from in Square::all() {
            let actual = pawn_pushes(us, Bitboard::from_square(from));
            assert_eq!(actual, pawn_effect(us, from), "{us:?} {from:?}");
            for to in actual.iter() {
                assert_eq!(from.file(), to.file());
            }
        }
        // 密な配置でも筋をまたがず、p0 bit63 / p1 bit18 以降を立てない。
        for sources in [Bitboard::ALL, rank1_bb(us), !rank1_bb(us)] {
            let expected =
                sources.iter().fold(Bitboard::EMPTY, |acc, from| acc | pawn_effect(us, from));
            assert_eq!(pawn_pushes(us, sources), expected, "{us:?}");
        }
    }
}

// ExtMove の PartialEq はスコアのみを比較するので、指し手を32bitで取り出す。
fn entries(buffer: &ExtMoveBuffer) -> Vec<(u32, i32)> {
    buffer.iter().map(|ext| (ext.mv.raw32(), ext.value)).collect()
}

fn compare_type(pos: &Position, gen_type: GenType, recapture_sq: Option<Square>) {
    let mut old = ExtMoveBuffer::new();
    let mut new = ExtMoveBuffer::new();
    // 既存の要素を残して末尾に追加する契約も確認する。
    old.push(super::super::types::ExtMove::new(Move::NULL, 123));
    new.push(super::super::types::ExtMove::new(Move::NULL, 123));
    let old_len = reference::generate_with_type(pos, gen_type, &mut old, recapture_sq);
    let new_len = generate_with_type(pos, gen_type, &mut new, recapture_sq);
    assert_eq!(old_len, new_len, "{gen_type:?} {:?}", pos.to_sfen());
    assert_eq!(
        entries(&old),
        entries(&new),
        "{gen_type:?} target={recapture_sq:?} {:?}",
        pos.to_sfen()
    );
}

fn compare_position(pos: &Position, recapture_sq: Square) {
    use GenType::*;
    for gen_type in [
        Quiets,
        Captures,
        QuietsAll,
        CapturesAll,
        CapturesProPlus,
        QuietsProMinus,
        CapturesProPlusAll,
        QuietsProMinusAll,
        Evasions,
        EvasionsAll,
        NonEvasions,
        NonEvasionsAll,
        Legal,
        LegalAll,
        Checks,
        ChecksAll,
        QuietChecks,
        QuietChecksAll,
        Recaptures,
        RecapturesAll,
    ] {
        // 各生成器の呼出し側と同じ、王手の有無に関する前提を守る。
        match gen_type {
            Evasions | EvasionsAll if !pos.in_check() => continue,
            Legal | LegalAll | Evasions | EvasionsAll => {}
            _ if pos.in_check() => continue,
            _ => {}
        }
        compare_type(pos, gen_type, Some(recapture_sq));
    }

    type BufferGenerator = fn(&Position, &mut ExtMoveBuffer) -> usize;
    let generators: [(BufferGenerator, BufferGenerator); 2] = [
        (reference::generate_all, generate_all),
        if pos.in_check() {
            (reference::generate_evasions, generate_evasions)
        } else {
            (reference::generate_non_evasions, generate_non_evasions)
        },
    ];
    for (old_gen, new_gen) in generators {
        let mut old = ExtMoveBuffer::new();
        let mut new = ExtMoveBuffer::new();
        assert_eq!(old_gen(pos, &mut old), new_gen(pos, &mut new));
        assert_eq!(entries(&old), entries(&new), "{:?}", pos.to_sfen());
    }

    type ListGenerator = fn(&Position, &mut MoveList);
    let generators: [(ListGenerator, ListGenerator); 4] = [
        (reference::generate_legal, generate_legal),
        (reference::generate_legal_all, generate_legal_all),
        (reference::generate_legal_with_pass, generate_legal_with_pass),
        (reference::generate_legal_all_with_pass, generate_legal_all_with_pass),
    ];
    for (old_gen, new_gen) in generators {
        let mut old = MoveList::new();
        let mut new = MoveList::new();
        old.push(Move::NULL);
        new.push(Move::NULL);
        old_gen(pos, &mut old);
        new_gen(pos, &mut new);
        assert_eq!(
            old.iter().map(|mv| mv.raw32()).collect::<Vec<_>>(),
            new.iter().map(|mv| mv.raw32()).collect::<Vec<_>>(),
            "{:?}",
            pos.to_sfen()
        );
    }
}

#[test]
fn fixed_positions_preserve_move_order() {
    for sfen in [
        SFEN_HIRATE,
        PERFT_MATSURI,
        PERFT_MIDGAME,
        // 既存 perft の王手回避・打ち歩詰め・歩打ちによる角筋遮断。
        "lnsgkgsnl/1r5b1/pppp1pppp/9/4R4/9/PPPP1PPPP/1B7/LNSGKGSNL w Pp 1",
        "R6sk/9/7G1/9/9/9/9/9/4K4 b P 1",
        "R8/2G1k4/9/2S6/2BN2N2/9/9/9/4K4 b P 1",
        // 金相当の全駒種・馬・龍・角・飛を升順で混在させる。
        "4k4/9/9/+P1+L1+N1+S2/1+B1+R1G1B1/8R/9/9/4K4 b - 1",
        // 両王手。
        "4k4/9/9/9/4r4/9/2b6/9/4K4 b - 1",
        // 成り・不成・端段と、7筋/8筋の語境界を含む歩。
        "4k4/PP1P1P1PP/9/9/9/9/9/9/4K4 b - 1",
    ] {
        // 盤と駒色を反転して、同じ境界条件を両手番で通す。
        let parts: Vec<_> = sfen.split_whitespace().collect();
        let board: String = parts[0]
            .chars()
            .rev()
            .map(|c| {
                if c.is_ascii_uppercase() {
                    c.to_ascii_lowercase()
                } else {
                    c.to_ascii_uppercase()
                }
            })
            .collect();
        // 成り記号は逆順で駒の後ろに来るため、SFEN の接頭辞へ戻す。
        let mut inverted_board = String::new();
        let mut chars = board.chars().peekable();
        while let Some(c) = chars.next() {
            if chars.peek() == Some(&'+') {
                inverted_board.push(chars.next().unwrap());
            }
            inverted_board.push(c);
        }
        let hand: String = parts[2]
            .chars()
            .map(|c| {
                if c.is_ascii_uppercase() {
                    c.to_ascii_lowercase()
                } else {
                    c.to_ascii_uppercase()
                }
            })
            .collect();
        let inverted =
            format!("{inverted_board} {} {hand} 1", if parts[1] == "b" { "w" } else { "b" });
        for sfen in [sfen, &inverted] {
            for rights in [0, 2] {
                let mut pos = Position::new();
                pos.set_sfen_with_pass_rights(sfen, rights, rights).unwrap();
                compare_position(&pos, Square::SQ_55);
                if !pos.in_check() {
                    for sq in Square::all() {
                        compare_type(&pos, GenType::Recaptures, Some(sq));
                        compare_type(&pos, GenType::RecapturesAll, Some(sq));
                    }
                }
            }
        }
    }
}

#[test]
fn random_playouts_preserve_move_order() {
    let mut checked = 0;
    let mut index = 0;
    let mut coverage = [[0; 2]; 2];
    let mut promotions = [0; 2];
    while checked < 8192 {
        let mut playout = RandomPlayout::new(0x6d6f_7665_6765_6e32, index);
        for ply in 0..256 {
            let pos = &playout.pos;
            let sq = playout.moves().last().map_or(Square::SQ_55, |mv| mv.to());
            compare_position(pos, sq);
            coverage[pos.side_to_move().index()][usize::from(pos.in_check())] += 1;
            let mut legal = MoveList::new();
            reference::generate_legal_all(pos, &mut legal);
            promotions[pos.side_to_move().index()] +=
                legal.iter().filter(|mv| mv.is_promotion()).count();
            checked += 1;
            if checked == 8192 || ply == 255 || playout.step().is_none() {
                break;
            }
        }
        index += 1;
    }
    assert!(coverage.iter().flatten().all(|&count| count >= 100), "{coverage:?}");
    assert!(promotions.iter().all(|&count| count >= 100), "{promotions:?}");
    eprintln!("比較局面数={checked} 手番別[通常, 王手]={coverage:?} 成り手数={promotions:?}");
}

#[test]
fn all_hand_subsets_preserve_drop_order() {
    // 桂・香の有無による段分割と、内側ループの0〜6種を両手番で網羅。
    for side in ["b", "w"] {
        for subset in 0..128 {
            let mut hand: String = ['P', 'N', 'L', 'S', 'G', 'B', 'R']
                .into_iter()
                .enumerate()
                .filter(|(i, _)| subset & (1 << i) != 0)
                .map(|(_, c)| {
                    if side == "w" {
                        c.to_ascii_lowercase()
                    } else {
                        c
                    }
                })
                .collect();
            if hand.is_empty() {
                hand.push('-');
            }
            let mut pos = Position::new();
            pos.set_sfen(&format!("4k4/9/9/9/9/9/9/9/4K4 {side} {hand} 1")).unwrap();
            compare_position(&pos, Square::SQ_55);
        }
    }
}

#[test]
fn capacity_boundaries_preserve_entries_and_overflow_behavior() {
    use crate::movegen::{ExtMove, MAX_MOVES};
    for sfen in [
        SFEN_HIRATE,
        "4k4/9/9/9/9/9/9/9/4K4 b RBGSNLP 1",
        "4k4/9/9/9/4r4/9/2b6/9/4K4 b - 1",
    ] {
        let mut pos = Position::new();
        pos.set_sfen(sfen).unwrap();
        let types = if pos.in_check() {
            &[GenType::Evasions, GenType::EvasionsAll, GenType::LegalAll][..]
        } else {
            &[
                GenType::NonEvasions,
                GenType::NonEvasionsAll,
                GenType::Legal,
                GenType::LegalAll,
                GenType::Checks,
                GenType::ChecksAll,
                GenType::RecapturesAll,
            ][..]
        };
        for &gen_type in types {
            let mut expected = ExtMoveBuffer::new();
            reference::generate_with_type(&pos, gen_type, &mut expected, Some(Square::SQ_55));
            if expected.is_empty() {
                continue;
            }
            // 全結果がちょうど収まる、1手余る、1手あふれる、満杯の境界。
            for prefix in [
                MAX_MOVES - expected.len(),
                MAX_MOVES - expected.len() - 1,
                MAX_MOVES - expected.len() + 1,
                MAX_MOVES,
            ] {
                let mut old = ExtMoveBuffer::new();
                let mut new = ExtMoveBuffer::new();
                for i in 0..prefix {
                    let entry = ExtMove::new(Move::NULL, i as i32);
                    old.push(entry);
                    new.push(entry);
                }
                let old_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    reference::generate_with_type(&pos, gen_type, &mut old, Some(Square::SQ_55))
                }));
                let new_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    generate_with_type(&pos, gen_type, &mut new, Some(Square::SQ_55))
                }));
                assert_eq!(old_result.is_err(), new_result.is_err(), "{gen_type:?}");
                assert_eq!(entries(&old), entries(&new), "{gen_type:?}");
                assert_eq!(
                    new_result.is_err(),
                    cfg!(debug_assertions) && prefix + expected.len() > MAX_MOVES
                );
            }
        }
    }
}

// 盤面参照で駒種を判定する参照実装。打ち・王手生成も本体とは独立に保持する。
mod reference {
    use super::super::{
        Bitboard, Color, ExtMoveBuffer, GenerateTargets, Move, MoveList, PieceType, Position,
        PromotionMode, Square, between_bb, bishop_effect, dragon_effect, enemy_field, gold_effect,
        horse_effect, king_effect, knight_effect, lance_effect, pawn_effect, rank1_bb, rank12_bb,
        rook_effect, silver_effect,
    };
    use crate::bitboard::{FILE_BB, check_candidate_bb, line_bb};

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

    pub(super) fn generate_non_evasions(pos: &Position, buffer: &mut ExtMoveBuffer) -> usize {
        let us = pos.side_to_move();
        let targets = GenerateTargets::with_drop(!pos.pieces_c(us), !pos.occupied());
        generate_non_evasions_core(pos, buffer, targets, false, PromotionMode::PromoteOnly, true);
        buffer.len()
    }

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

    pub(super) fn generate_evasions(pos: &Position, buffer: &mut ExtMoveBuffer) -> usize {
        generate_evasions_with_promos(pos, buffer, false, PromotionMode::PromoteOnly);
        buffer.len()
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

    pub(super) fn generate_with_type(
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
                // YO準拠: targetPawn = ~enemy_field & empties で歩の敵陣成りを事前除外
                let pawn_target = !enemy_field(us) & empties;
                let targets = GenerateTargets {
                    general: empties,
                    pawn: pawn_target,
                    drop: empties,
                };
                generate_non_evasions_core(
                    pos,
                    buffer,
                    targets,
                    false,
                    PromotionMode::PromoteOnly,
                    true,
                );
            }
            QuietsProMinusAll => {
                // YO準拠: targetPawn = ~enemy_field & empties で歩の敵陣成りを事前除外
                let pawn_target = !enemy_field(us) & empties;
                let targets = GenerateTargets {
                    general: empties,
                    pawn: pawn_target,
                    drop: empties,
                };
                generate_non_evasions_core(pos, buffer, targets, true, PromotionMode::Both, true);
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
                // YO準拠: targetPawn = (~pieces(Us) & enemy_field(Us)) | pieces(Them)
                // 歩は取り手に加え、敵陣への静かな成りも生成する
                let pawn_target = (!pos.pieces_c(us) & enemy_field(us)) | enemy;
                let targets = GenerateTargets {
                    general: enemy,
                    pawn: pawn_target,
                    drop: enemy,
                };
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
                let pawn_target = (!pos.pieces_c(us) & enemy_field(us)) | enemy;
                let targets = GenerateTargets {
                    general: enemy,
                    pawn: pawn_target,
                    drop: enemy,
                };
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

    pub(super) fn generate_all(pos: &Position, buffer: &mut ExtMoveBuffer) -> usize {
        if pos.in_check() {
            generate_evasions(pos, buffer)
        } else {
            generate_non_evasions(pos, buffer)
        }
    }

    pub(super) fn generate_legal(pos: &Position, list: &mut MoveList) {
        let mut buffer = ExtMoveBuffer::new();
        generate_all(pos, &mut buffer);

        // YO の swap-erase パターン:
        //   while (mlist != last) {
        //       if (!pos.legal(*mlist))
        //           *mlist = *(--last);
        //       else
        //           ++mlist;
        //   }
        let slice = buffer.as_mut_slice();
        let mut i = 0;
        let mut last = slice.len();
        while i < last {
            if !pos.is_legal(slice[i].mv) {
                last -= 1;
                slice[i] = slice[last];
            } else {
                i += 1;
            }
        }

        for ext in &slice[..last] {
            list.push(ext.mv);
        }
    }

    pub(super) fn generate_legal_all(pos: &Position, list: &mut MoveList) {
        let mut buffer = ExtMoveBuffer::new();
        generate_with_type(pos, crate::movegen::GenType::LegalAll, &mut buffer, None);

        for ext in buffer.iter() {
            list.push(ext.mv);
        }
    }

    pub(super) fn generate_legal_with_pass(pos: &Position, list: &mut MoveList) {
        generate_legal(pos, list);

        // パス可能な場合のみ追加
        if pos.can_pass() {
            list.push(Move::PASS);
        }
    }

    pub(super) fn generate_legal_all_with_pass(pos: &Position, list: &mut MoveList) {
        generate_legal_all(pos, list);

        // パス可能な場合のみ追加
        if pos.can_pass() {
            list.push(Move::PASS);
        }
    }

    fn add_move(buffer: &mut ExtMoveBuffer, mv: Move) {
        buffer.push_move(mv);
    }

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
        let mut valid_targets = valid_targets & pawn_drop_mask(us, our_pawns);

        // YO準拠: 打ち歩詰めチェック — 王手になる升は玉の前の1升のみ事前特定して1回だけ判定
        let them = !us;
        let them_king = pos.king_square(them);
        let pe = pawn_effect(them, them_king);
        if let Some(to) = (pe & valid_targets).lsb()
            && !pos.legal_pawn_drop_check(to)
        {
            valid_targets ^= pe;
        }

        let dropped_pc = crate::types::Piece::make(us, PieceType::Pawn);
        for to in valid_targets.iter() {
            add_move(buffer, Move::new_drop_with_piece(PieceType::Pawn, to, dropped_pc));
        }
    }

    fn generate_non_pawn_drops(pos: &Position, target: Bitboard, buffer: &mut ExtMoveBuffer) {
        let us = pos.side_to_move();
        let hand = pos.hand(us);

        let empties = !pos.occupied();
        let target = target & empties;

        // YO準拠: 駒種配列を桂→香→銀→金→角→飛の順で構築
        // ダミー初期値として Pawn を使用（num 未満のインデックスのみ参照される）
        let dummy = (PieceType::Pawn, crate::types::Piece::make(us, PieceType::Pawn));
        let mut drops = [dummy; 6];
        let mut num = 0usize;

        if hand.has(PieceType::Knight) {
            drops[num] = (PieceType::Knight, crate::types::Piece::make(us, PieceType::Knight));
            num += 1;
        }
        let next_to_knight = num; // 桂を除いたdropsの開始index

        if hand.has(PieceType::Lance) {
            drops[num] = (PieceType::Lance, crate::types::Piece::make(us, PieceType::Lance));
            num += 1;
        }
        let next_to_lance = num; // 香・桂を除いたdropsの開始index

        for pt in [
            PieceType::Silver,
            PieceType::Gold,
            PieceType::Bishop,
            PieceType::Rook,
        ] {
            if hand.has(pt) {
                drops[num] = (pt, crate::types::Piece::make(us, pt));
                num += 1;
            }
        }

        if num == 0 {
            return;
        }

        let drops = &drops[..num];

        if next_to_lance == 0 {
            // 香・桂を持っていない: 全マスに対して全駒種を生成
            for to in target.iter() {
                for &(pt, pc) in drops {
                    add_move(buffer, Move::new_drop_with_piece(pt, to, pc));
                }
            }
        } else {
            // 段による場合分け
            let rank1 = rank1_bb(us);
            let rank12 = rank12_bb(us);
            let rank2_only = rank12 & !rank1;

            // 1段目: 香・桂以外の駒のみ
            let target1 = target & rank1;
            if next_to_lance < num {
                for to in target1.iter() {
                    for &(pt, pc) in &drops[next_to_lance..] {
                        add_move(buffer, Move::new_drop_with_piece(pt, to, pc));
                    }
                }
            }

            // 2段目: 桂以外の駒
            let target2 = target & rank2_only;
            if next_to_knight < num {
                for to in target2.iter() {
                    for &(pt, pc) in &drops[next_to_knight..] {
                        add_move(buffer, Move::new_drop_with_piece(pt, to, pc));
                    }
                }
            }

            // 3〜9段目: 全駒種
            let target3 = target & !rank12;
            for to in target3.iter() {
                for &(pt, pc) in drops {
                    add_move(buffer, Move::new_drop_with_piece(pt, to, pc));
                }
            }
        }
    }

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

        // 歩打ち王手（YO準拠: 二歩+打ち歩詰めをgenerate内で除外）
        if hand.has(PieceType::Pawn) {
            let check_target = pos.check_squares(PieceType::Pawn) & empties;
            if !check_target.is_empty() {
                let rank1 = rank1_bb(us);
                let our_pawns = pos.pieces(us, PieceType::Pawn);
                let valid = check_target & !rank1 & pawn_drop_mask(us, our_pawns);
                let dropped_pc = crate::types::Piece::make(us, PieceType::Pawn);
                for to in valid.iter() {
                    // 歩の王手 = 必ず敵玉の頭なので打ち歩詰め判定が必要
                    if !pos.legal_pawn_drop_check(to) {
                        continue;
                    }
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
}
