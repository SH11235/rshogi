//! static の切替で他の並列テストに干渉せず、両単相化経路を直接検証する。

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
    caches: &mut [AccumulatorCacheLayerStacks<L1>; 2],
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
            let mut old = Aligned([i16::MIN; L1]);
            let mut new = Aligned([i16::MAX; L1]);
            ft.refresh_perspective_with_cache_impl::<false>(
                pos,
                perspective,
                &mut old.0,
                #[cfg(feature = "nnue-psqt")]
                &mut [0; MAX_LAYER_STACK_BUCKETS],
                &mut caches[0],
            );
            ft.refresh_perspective_with_cache_impl::<true>(
                pos,
                perspective,
                &mut new.0,
                #[cfg(feature = "nnue-psqt")]
                &mut [0; MAX_LAYER_STACK_BUCKETS],
                &mut caches[1],
            );
            assert_eq!(old.0, expected, "old {perspective:?}");
            assert_eq!(new.0, expected, "v2 {perspective:?}");
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
    let mut caches = std::array::from_fn(|_| AccumulatorCacheLayerStacks::new());
    let mut pos = Position::new();
    let mut kings = [0; 2];
    let mut captures = 0;
    let mut drops = 0;
    let mut nulls = 0;
    let mut fast = 0;
    for sfen in [SFEN_HIRATE, "4k4/9/9/9/9/9/9/9/4K4 b R2Pbr 1", SFEN_HIRATE] {
        pos.set_sfen(sfen).unwrap();
        let mut previous = check_refresh(&ft, &pos, &mut caches);
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
            let expected = check_refresh(&ft, &pos, &mut caches);
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
                append_changed_indices_v2::<FT>(
                    &dirty,
                    perspective,
                    king,
                    &mut removed,
                    &mut added,
                );
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
                let mut old = Aligned(previous[p]);
                let mut new = Aligned(previous[p]);
                let old_ok = ft.try_apply_dirty_piece_indexed::<false, false>(
                    None,
                    &mut old.0,
                    &dirty,
                    perspective,
                    king,
                );
                let new_ok = ft.try_apply_dirty_piece_indexed::<false, true>(
                    None,
                    &mut new.0,
                    &dirty,
                    perspective,
                    king,
                );
                assert_eq!(old_ok, new_ok);
                assert_eq!(old.0, new.0);
                if new_ok {
                    fast += 1;
                    assert_eq!(new.0, expected[p]);
                    new.0.fill(0);
                    assert!(ft.try_apply_dirty_piece_indexed::<true, true>(
                        Some(&source.0),
                        &mut new.0,
                        &dirty,
                        perspective,
                        king,
                    ));
                    assert_eq!(new.0, expected[p]);
                }
            }
            if ply % 7 == 0 {
                pos.undo_move(mv);
                assert_eq!(check_refresh(&ft, &pos, &mut caches), previous);
            } else {
                previous = expected;
            }
            if ply % 11 == 0 && !pos.in_check() {
                pos.do_null_move();
                assert_eq!(check_refresh(&ft, &pos, &mut caches), previous);
                pos.undo_null_move();
                nulls += 1;
            }
            if ply % 31 == 0 {
                for cache in &mut caches {
                    cache.invalidate();
                }
            } else if ply % 13 == 0 {
                // 旧・新経路が同じ cache 内容を相互利用できることも検証。
                caches.swap(0, 1);
            }
        }
    }
    assert!(kings.into_iter().all(|n| n > 0));
    assert!(captures > 0 && drops > 0 && nulls > 0 && fast > 0);
}

#[test]
fn ls_finny_v2_random_games_match_old_and_full_refresh() {
    check_random_game::<256, HalfKaHmMergedSpec>();
    check_random_game::<32, HalfKpSpec>();
    check_random_game::<32, HalfKaSplitSpec>();
    check_random_game::<32, HalfKaMergedSpec>();
    check_random_game::<32, HalfKaHmSplitSpec>();
}

#[test]
fn ls_finny_v2_indexer_matches_all_bona_pieces_and_kings() {
    use crate::nnue::bona_piece_halfka_hm_merged::E_KING;
    use crate::types::Square;
    for perspective in [Color::Black, Color::White] {
        for king in Square::all() {
            let index = HalfKaHmMergedSpec::feature_indexer::<true>(perspective, king);
            for bp in 0..E_KING + 81 {
                let bp = BonaPiece::new(bp as u16);
                assert_eq!(index(bp), HalfKaHmMergedSpec::feature_index(bp, perspective, king));
            }
        }
    }
}
