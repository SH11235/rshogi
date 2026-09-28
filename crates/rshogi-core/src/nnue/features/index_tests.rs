use super::*;
use crate::movegen::{MoveList, generate_legal};
use crate::nnue::accumulator::ChangedBonaPiece;
use crate::nnue::bona_piece::{BonaPiece, ExtBonaPiece, FE_END};
use crate::nnue::ls_feature_spec::{
    HalfKaHmMergedSpec, HalfKaHmSplitSpec, HalfKaMergedSpec, HalfKaSplitSpec, HalfKpSpec,
    LsFeatureSpec,
};
use crate::nnue::piece_list::PieceNumber;
use crate::position::SFEN_HIRATE;
use crate::types::{Piece, PieceType};
use rand::{RngCore, SeedableRng};
use rand_xoshiro::Xoshiro256PlusPlus;

// 駒スロット順に push する参照実装。列挙処理の一括追記 API は使用しない。
fn reference_active<FT: LsFeatureSpec>(
    pos: &Position,
    perspective: Color,
    active: &mut IndexList<MAX_ACTIVE_FEATURES>,
) {
    let pieces = if perspective == Color::Black {
        pos.piece_list().piece_list_fb()
    } else {
        pos.piece_list().piece_list_fw()
    };
    let end = if FT::INCLUDE_KING_IN_PIECE_LIST {
        PieceNumber::NB
    } else {
        PieceNumber::KING as usize
    };
    for &bp in &pieces[..end] {
        if bp != BonaPiece::ZERO {
            let _ = active.push(FT::feature_index(bp, perspective, pos.king_square(perspective)));
        }
    }
}

fn reference_changed<FT: LsFeatureSpec>(
    dirty: &DirtyPiece,
    perspective: Color,
    king: Square,
    removed: &mut IndexList<MAX_CHANGED_FEATURES>,
    added: &mut IndexList<MAX_CHANGED_FEATURES>,
) {
    for cp in &dirty.changed_piece[..dirty.dirty_num as usize] {
        let (old, new) = if perspective == Color::Black {
            (cp.old_piece.fb, cp.new_piece.fb)
        } else {
            (cp.old_piece.fw, cp.new_piece.fw)
        };
        let included = |bp: BonaPiece| {
            bp != BonaPiece::ZERO
                && (FT::INCLUDE_KING_IN_PIECE_LIST || (bp.value() as usize) < FE_END)
        };
        if included(old) {
            let _ = removed.push(FT::feature_index(old, perspective, king));
        }
        if included(new) {
            let _ = added.push(FT::feature_index(new, perspective, king));
        }
    }
}

fn with_prefix<const N: usize>(len: usize) -> IndexList<N> {
    let mut list = IndexList::new();
    for i in 0..len {
        assert!(list.push(100 + i));
    }
    list
}

fn assert_same<const N: usize>(actual: &IndexList<N>, expected: &IndexList<N>) {
    assert_eq!(actual.iter().collect::<Vec<_>>(), expected.iter().collect::<Vec<_>>());
}

fn check_changed_capacity_boundaries<FT: LsFeatureSpec>() {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    let mut dirty = DirtyPiece::new();
    dirty.dirty_num = 2;
    dirty.changed_piece[0] = ChangedBonaPiece {
        old_piece: ExtBonaPiece::from_board(Piece::B_PAWN, Square::SQ_55),
        new_piece: ExtBonaPiece::from_hand(Color::Black, PieceType::Pawn, 1),
    };
    dirty.changed_piece[1] = ChangedBonaPiece {
        old_piece: ExtBonaPiece::from_hand(Color::White, PieceType::Silver, 1),
        new_piece: ExtBonaPiece::from_board(Piece::W_SILVER, Square::SQ_55),
    };
    // 片側が先に満杯になる場合と、同じ駒で両側が満杯になる場合を含む。
    for perspective in [Color::Black, Color::White] {
        for removed_prefix in [0, MAX_CHANGED_FEATURES - 1, MAX_CHANGED_FEATURES] {
            for added_prefix in [0, MAX_CHANGED_FEATURES - 1, MAX_CHANGED_FEATURES] {
                let mut expected_removed = with_prefix(removed_prefix);
                let mut expected_added = with_prefix(added_prefix);
                let mut actual_removed = expected_removed;
                let mut actual_added = expected_added;
                let expected_result = catch_unwind(AssertUnwindSafe(|| {
                    reference_changed::<FT>(
                        &dirty,
                        perspective,
                        Square::SQ_55,
                        &mut expected_removed,
                        &mut expected_added,
                    );
                }));
                let actual_result = catch_unwind(AssertUnwindSafe(|| {
                    FT::Feature::append_changed_indices(
                        &dirty,
                        perspective,
                        Square::SQ_55,
                        &mut actual_removed,
                        &mut actual_added,
                    );
                }));
                let overflow = removed_prefix + 2 > MAX_CHANGED_FEATURES
                    || added_prefix + 2 > MAX_CHANGED_FEATURES;
                assert_eq!(expected_result.is_err(), cfg!(debug_assertions) && overflow);
                assert_eq!(actual_result.is_err(), expected_result.is_err());
                assert_same(&actual_removed, &expected_removed);
                assert_same(&actual_added, &expected_added);
            }
        }
    }
}

#[test]
fn halfkp_changed_indices_match_push_at_capacity_boundaries() {
    check_changed_capacity_boundaries::<HalfKpSpec>();
}

#[test]
fn halfka_split_changed_indices_match_push_at_capacity_boundaries() {
    check_changed_capacity_boundaries::<HalfKaSplitSpec>();
}

#[test]
fn halfka_merged_changed_indices_match_push_at_capacity_boundaries() {
    check_changed_capacity_boundaries::<HalfKaMergedSpec>();
}

#[test]
fn halfka_hm_split_changed_indices_match_push_at_capacity_boundaries() {
    check_changed_capacity_boundaries::<HalfKaHmSplitSpec>();
}

#[test]
fn halfka_hm_merged_changed_indices_match_push_at_capacity_boundaries() {
    check_changed_capacity_boundaries::<HalfKaHmMergedSpec>();
}

fn check_position<FT: LsFeatureSpec>(pos: &Position, dirty: &DirtyPiece) {
    for perspective in [Color::Black, Color::White] {
        let king = pos.king_square(perspective);
        for prefix in [0, 3] {
            let mut expected = with_prefix(prefix);
            let mut actual = expected;
            reference_active::<FT>(pos, perspective, &mut expected);
            FT::Feature::append_active_indices(pos, perspective, &mut actual);
            assert_same(&actual, &expected);
            if prefix == 0 {
                assert_same(&FT::Set::collect_active_indices(pos, perspective), &expected);
            }

            let mut expected_removed = with_prefix(prefix);
            let mut expected_added = with_prefix(prefix);
            let mut actual_removed = expected_removed;
            let mut actual_added = expected_added;
            reference_changed::<FT>(
                dirty,
                perspective,
                king,
                &mut expected_removed,
                &mut expected_added,
            );
            FT::Feature::append_changed_indices(
                dirty,
                perspective,
                king,
                &mut actual_removed,
                &mut actual_added,
            );
            assert_same(&actual_removed, &expected_removed);
            assert_same(&actual_added, &expected_added);
            if prefix == 0 {
                let (removed, added) = FT::Set::collect_changed_indices(dirty, perspective, king);
                assert_same(&removed, &expected_removed);
                assert_same(&added, &expected_added);
            }
        }
    }
}

fn check_random_positions<FT: LsFeatureSpec>() {
    let mut pos = Position::new();
    pos.set_sfen(SFEN_HIRATE).unwrap();
    // ZERO を含む片側だけの変化と、玉だけの変化も両視点で照合する。
    let mut dirty = DirtyPiece::new();
    dirty.dirty_num = 2;
    dirty.changed_piece[0] = ChangedBonaPiece {
        old_piece: ExtBonaPiece::ZERO,
        new_piece: ExtBonaPiece::from_hand(Color::Black, PieceType::Pawn, 1),
    };
    dirty.changed_piece[1] = ChangedBonaPiece {
        old_piece: ExtBonaPiece::from_board(Piece::W_PAWN, Square::SQ_55),
        new_piece: ExtBonaPiece::ZERO,
    };
    check_position::<FT>(&pos, &dirty);
    for (i, color) in [Color::Black, Color::White].into_iter().enumerate() {
        dirty.changed_piece[i] = ChangedBonaPiece {
            old_piece: ExtBonaPiece::from_board(
                Piece::new(color, PieceType::King),
                pos.king_square(color),
            ),
            new_piece: ExtBonaPiece::from_board(Piece::new(color, PieceType::King), Square::SQ_55),
        };
    }
    check_position::<FT>(&pos, &dirty);

    let mut seen = [false; 4];
    for seed in [0x1234_5678, 0x9876_abcd, 0x484b_5800] {
        let mut rng = Xoshiro256PlusPlus::seed_from_u64(seed);
        for sfen in [
            SFEN_HIRATE,
            "+B1sg1gsnl/2+N2k1b1/pP2pp2p/2p3p2/9/2PpP4/P1+p2PP1P/7R1/LN1GKGSNL w RLs3p 32",
            "4k4/9/9/9/9/9/9/9/4K4 b R2Pbr 1",
            "k8/9/9/9/9/9/9/9/8K b 2R2B4G4S4N4L18P 1",
        ] {
            let mut pos = Position::new();
            pos.set_sfen(sfen).unwrap();
            check_position::<FT>(&pos, &DirtyPiece::new());
            for _ in 0..192 {
                let mut moves = MoveList::new();
                generate_legal(&pos, &mut moves);
                if moves.is_empty() {
                    break;
                }
                let mv = *moves.iter().nth(rng.next_u32() as usize % moves.len()).unwrap();
                seen[0] |= mv.is_drop();
                seen[1] |= mv.is_promotion();
                seen[2] |= !pos.piece_on(mv.to()).is_none();
                let gives_check = pos.gives_check(mv);
                let dirty = pos.do_move(mv, gives_check);
                seen[3] |= dirty.king_moved.iter().any(|&moved| moved);
                check_position::<FT>(&pos, &dirty);
            }
        }
    }
    assert!(seen.iter().all(|&covered| covered), "打ち・成り・捕獲・玉移動: {seen:?}");
}

#[test]
fn halfkp_indices_match_push_on_random_positions() {
    check_random_positions::<HalfKpSpec>();
}

#[test]
fn halfka_split_indices_match_push_on_random_positions() {
    check_random_positions::<HalfKaSplitSpec>();
}

#[test]
fn halfka_merged_indices_match_push_on_random_positions() {
    check_random_positions::<HalfKaMergedSpec>();
}

#[test]
fn halfka_hm_split_indices_match_push_on_random_positions() {
    check_random_positions::<HalfKaHmSplitSpec>();
}

#[test]
fn halfka_hm_merged_indices_match_push_on_random_positions() {
    check_random_positions::<HalfKaHmMergedSpec>();
}
