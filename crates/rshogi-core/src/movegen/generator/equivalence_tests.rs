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

// 盤面参照で駒種を判定する参照実装。
// 駒種別の移動を組み立て、打ち・王手生成・合法性判定は本体と共有する。
mod reference {
    use super::super::{
        Bitboard, ExtMoveBuffer, GenerateTargets, Move, MoveList, PieceType, Position,
        PromotionMode, Square, add_move, between_bb, bishop_effect, dragon_effect, enemy_field,
        generate_checks, generate_non_pawn_drops, generate_pawn_drops, gold_effect, horse_effect,
        king_effect, knight_effect, lance_effect, pawn_effect, rank1_bb, rank12_bb, rook_effect,
        silver_effect,
    };

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
}
