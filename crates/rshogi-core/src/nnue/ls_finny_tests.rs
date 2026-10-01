//! Finny refresh と差分更新を、特徴量の全列挙・逐次加算と比較する。

use super::*;
use crate::movegen::{MoveList, generate_legal_all};
use crate::nnue::ls_feature_spec::{
    HalfKaHmMergedSpec, HalfKaHmSplitSpec, HalfKaMergedSpec, HalfKaSplitSpec, HalfKpSpec,
};
use crate::position::SFEN_HIRATE;
use crate::types::PieceType;
use rand::{RngCore, SeedableRng};
use rand_xoshiro::Xoshiro256PlusPlus;

fn check_refresh<const L1: usize, FT: LsFeatureSpec>(
    ft: &FeatureTransformerLayerStacks<L1, FT>,
    pos: &Position,
    cache: &mut AccumulatorCacheLayerStacks<L1>,
) -> [[i16; L1]; 2] {
    std::array::from_fn(|p| {
        let perspective = [Color::Black, Color::White][p];
        let mut active = IndexList::new();
        FT::Feature::append_active_indices(pos, perspective, &mut active);
        let mut expected = ft.biases.0;
        for index in active.iter() {
            for (value, weight) in expected.iter_mut().zip(&ft.weights[index * L1..][..L1]) {
                *value = value.wrapping_add(*weight);
            }
        }
        // miss/hit に続けて同じ局面を再度要求し、差分ゼロでも出力が書かれることを確認。
        for _ in 0..2 {
            let mut actual = Aligned([i16::MAX; L1]);
            ft.refresh_perspective_with_cache(
                pos,
                perspective,
                &mut actual.0,
                #[cfg(feature = "nnue-psqt")]
                &mut [0; MAX_LAYER_STACK_BUCKETS],
                cache,
            );
            assert_eq!(actual.0, expected, "{perspective:?}");
        }
        expected
    })
}

fn check_random_game<const L1: usize, FT: LsFeatureSpec>() {
    let mut ft = FeatureTransformerLayerStacks::<L1, FT>::for_accumulator_tests();
    let mut rng = Xoshiro256PlusPlus::seed_from_u64(0xf177_0203);
    for value in &mut ft.biases.0 {
        *value = rng.next_u32() as i16;
    }
    for value in ft.weights.make_mut() {
        *value = rng.next_u32() as i16;
    }
    let mut cache = AccumulatorCacheLayerStacks::new();
    let mut pos = Position::new();
    let mut kings = [0; 2];
    let mut captures = 0;
    let mut drops = 0;
    let mut nulls = 0;
    let mut fast = 0;
    for sfen in [SFEN_HIRATE, "4k4/9/9/9/9/9/9/9/4K4 b R2Pbr 1", SFEN_HIRATE] {
        pos.set_sfen(sfen).unwrap();
        let mut previous = check_refresh(&ft, &pos, &mut cache);
        for ply in 0..192 {
            let mut moves = MoveList::new();
            generate_legal_all(&pos, &mut moves);
            if moves.is_empty() {
                break;
            }
            let random = *moves.iter().nth(rng.next_u32() as usize % moves.len()).unwrap();
            let mv = if ply % 4 == 0 {
                moves
                    .iter()
                    .copied()
                    .find(|mv| {
                        !mv.is_drop() && pos.piece_on(mv.from()).piece_type() == PieceType::King
                    })
                    .unwrap_or(random)
            } else {
                random
            };
            captures += usize::from(!pos.piece_on(mv.to()).is_none());
            drops += usize::from(mv.is_drop());
            let dirty = pos.do_move(mv, pos.gives_check(mv));
            for (count, moved) in kings.iter_mut().zip(dirty.king_moved) {
                *count += usize::from(moved);
            }
            let expected = check_refresh(&ft, &pos, &mut cache);
            for perspective in [Color::Black, Color::White] {
                let p = perspective as usize;
                let king = pos.king_square(perspective);
                let mut removed = IndexList::new();
                let mut added = IndexList::new();
                let mut old_removed = IndexList::new();
                let mut old_added = IndexList::new();
                // 追記先に既存要素がある場合も順序と長さが一致する。
                for list in [&mut removed, &mut added, &mut old_removed, &mut old_added] {
                    assert!(list.push(17));
                }
                append_changed_indices::<FT>(&dirty, perspective, king, &mut removed, &mut added);
                FT::Feature::append_changed_indices(
                    &dirty,
                    perspective,
                    king,
                    &mut old_removed,
                    &mut old_added,
                );
                assert!(removed.iter().eq(old_removed.iter()));
                assert!(added.iter().eq(old_added.iter()));
                if dirty.king_moved[p] {
                    continue;
                }
                let source = Aligned(previous[p]);
                let mut new = Aligned(previous[p]);
                let new_ok = ft.try_apply_dirty_piece_fast_impl::<false>(
                    perspective,
                    king,
                    None,
                    &mut new.0,
                    &dirty,
                );
                if new_ok {
                    fast += 1;
                    assert_eq!(new.0, expected[p]);
                    new.0.fill(0);
                    assert!(ft.try_apply_dirty_piece_fast_impl::<true>(
                        perspective,
                        king,
                        Some(&source.0),
                        &mut new.0,
                        &dirty,
                    ));
                    assert_eq!(new.0, expected[p]);
                }
            }
            if ply % 7 == 0 {
                pos.undo_move(mv);
                assert_eq!(check_refresh(&ft, &pos, &mut cache), previous);
            } else {
                previous = expected;
            }
            if ply % 11 == 0 && !pos.in_check() {
                pos.do_null_move();
                assert_eq!(check_refresh(&ft, &pos, &mut cache), previous);
                pos.undo_null_move();
                nulls += 1;
            }
            if ply % 31 == 0 {
                cache.invalidate();
            }
        }
    }
    assert!(kings.into_iter().all(|n| n > 0));
    assert!(captures > 0 && drops > 0 && nulls > 0 && fast > 0);
}

#[test]
fn ls_finny_random_games_match_full_refresh() {
    check_random_game::<256, HalfKaHmMergedSpec>();
    check_random_game::<32, HalfKpSpec>();
    check_random_game::<32, HalfKaSplitSpec>();
    check_random_game::<32, HalfKaMergedSpec>();
    check_random_game::<32, HalfKaHmSplitSpec>();
}

#[test]
fn ls_finny_indexer_matches_all_bona_pieces_and_kings() {
    use crate::nnue::bona_piece_halfka_hm_merged::E_KING;
    use crate::types::Square;
    for perspective in [Color::Black, Color::White] {
        for king in Square::all() {
            let index = HalfKaHmMergedSpec::feature_indexer(perspective, king);
            for bp in 0..E_KING + 81 {
                let bp = BonaPiece::new(bp as u16);
                assert_eq!(index(bp), HalfKaHmMergedSpec::feature_index(bp, perspective, king));
            }
        }
    }
}
