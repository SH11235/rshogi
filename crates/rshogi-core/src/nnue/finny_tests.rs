//! Finny cache と全再計算の bit 一致を、非ゼロ重みと合法手列で検証する。

use super::accumulator::{AccumulatorCacheGeneric, DirtyPiece};
use crate::movegen::{MoveList, generate_legal_all};
use crate::position::{Position, SFEN_HIRATE};
use crate::types::PieceType;
use rand::{RngCore, SeedableRng};
use rand_xoshiro::Xoshiro256PlusPlus;

/// スロットの交換は特徴の多重集合を変えないが、位置ごとの差分には現れる。
fn check_piece_boundaries<FT: super::LsFeatureSpec>() {
    use super::{BonaPiece, FeatureSet, piece_list::PieceNumber};
    use crate::types::Color;
    const L1: usize = 32;
    let mut rng = Xoshiro256PlusPlus::seed_from_u64(0x5678);
    let weights: Vec<i16> = (0..FT::Set::DIMENSIONS * L1).map(|_| rng.next_u32() as i16).collect();
    let biases: [i16; L1] = std::array::from_fn(|_| rng.next_u32() as i16);
    let mut cache = AccumulatorCacheGeneric::new(L1);
    let mut invalid_cache = AccumulatorCacheGeneric::new(L1);
    let mut pos = Position::new();
    pos.set_sfen("4k4/9/4p4/4P4/9/9/9/9/4K4 b P 1").unwrap();
    let moves = ["5d5c+", "5a4a", "P*4d", "4a3b", "5i6h", "3b4a", "6h5i"];
    for ply in 0..=moves.len() {
        for perspective in [Color::Black, Color::White] {
            let indices = FT::Set::collect_active_indices(&pos, perspective);
            let mut expected = biases;
            for index in indices.iter() {
                for (v, &w) in expected.iter_mut().zip(&weights[index * L1..][..L1]) {
                    *v = v.wrapping_add(w);
                }
            }
            let king = pos.king_square(perspective);
            let mut actual = [0; L1];
            cache.refresh_or_cache::<L1, FT>(&pos, perspective, &biases, &mut actual, &weights);
            assert_eq!(actual, expected, "cold/hit {ply} {perspective:?}");
            let mut pieces = if perspective == Color::Black {
                *pos.piece_list().piece_list_fb()
            } else {
                *pos.piece_list().piece_list_fw()
            };
            if !FT::INCLUDE_KING_IN_PIECE_LIST {
                pieces[PieceNumber::KING as usize..].fill(BonaPiece::ZERO);
            }
            let occupied: Vec<_> = pieces[..PieceNumber::KING as usize]
                .iter()
                .enumerate()
                .filter_map(|(i, bp)| (*bp != BonaPiece::ZERO).then_some(i))
                .collect();
            assert!(occupied.len() >= 2);
            pieces.swap(occupied[0], occupied[1]);
            cache.refresh_piece_list::<L1, _>(
                (king, perspective),
                &pieces,
                &biases,
                &mut actual,
                &weights,
                |bp| FT::feature_index(bp, perspective, king),
            );
            assert_eq!(actual, expected, "slot swap {ply} {perspective:?}");
            // 元の割り当てへの復帰、同一スロット列での hit、明示的な無効化を照合する。
            for _ in 0..2 {
                cache.refresh_or_cache::<L1, FT>(&pos, perspective, &biases, &mut actual, &weights);
                assert_eq!(actual, expected, "restore {ply} {perspective:?}");
            }
            invalid_cache.invalidate();
            invalid_cache.refresh_or_cache::<L1, FT>(
                &pos,
                perspective,
                &biases,
                &mut actual,
                &weights,
            );
            assert_eq!(actual, expected, "invalid {ply} {perspective:?}");
        }
        if let Some(&usi) = moves.get(ply) {
            let mut legal = MoveList::new();
            generate_legal_all(&pos, &mut legal);
            let mv = legal
                .iter()
                .copied()
                .find(|mv| mv.to_usi() == usi)
                .unwrap_or_else(|| panic!("合法手が必要: {usi}"));
            pos.do_move(mv, pos.gives_check(mv));
        }
    }
}

#[test]
fn halfkx_finny_piece_boundaries_all_five_feature_sets() {
    check_piece_boundaries::<super::HalfKpSpec>();
    check_piece_boundaries::<super::HalfKaSplitSpec>();
    check_piece_boundaries::<super::HalfKaMergedSpec>();
    check_piece_boundaries::<super::HalfKaHmSplitSpec>();
    check_piece_boundaries::<super::HalfKaHmMergedSpec>();
}

macro_rules! check_finny {
    ($name:ident, $module:ident, $ft:ident, $acc:ident, $stack:ident) => {
        #[test]
        fn $name() {
            use super::$module::{$acc, $ft, $stack};
            const L1: usize = 32;
            let mut ft = $ft::<L1>::read(&mut std::io::repeat(0)).unwrap();
            let mut rng = Xoshiro256PlusPlus::seed_from_u64(0xf177_5eed);
            // i16 全域の重みで、加減算の順序が変わっても wrapping の結果が一致することを確認。
            for weight in ft.weights.iter_mut() {
                *weight = rng.next_u32() as i16;
            }
            let mut cache = AccumulatorCacheGeneric::new(L1);
            let mut lazy_cache = AccumulatorCacheGeneric::new(L1);
            let mut pos = Position::new();
            let mut kings = [0; 2];
            let mut captures = 0;
            let mut drops = 0;
            let mut promotions = 0;
            let mut forward_updates = 0;
            // 駒落ちや持ち駒を含む局面へ set しても、同じ net の cache は再利用できる。
            for sfen in [SFEN_HIRATE, "4k4/9/9/9/9/9/9/9/4K4 b R2Pbr 1", SFEN_HIRATE] {
                pos.set_sfen(sfen).unwrap();
                let mut prev = $acc::<L1>::new();
                ft.refresh_accumulator(&pos, &mut prev);
                let mut stack = $stack::<L1>::new();
                ft.refresh_accumulator_with_cache(
                    &pos,
                    &mut stack.current_mut().accumulator,
                    &mut lazy_cache,
                );
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
                                !mv.is_drop()
                                    && pos.piece_on(mv.from()).piece_type() == PieceType::King
                            })
                            .unwrap_or(random)
                    } else {
                        random
                    };
                    captures += usize::from(!pos.piece_on(mv.to()).is_none());
                    drops += usize::from(mv.is_drop());
                    promotions += usize::from(mv.is_promotion());
                    let dirty = pos.do_move(mv, pos.gives_check(mv));
                    for (count, moved) in kings.iter_mut().zip(dirty.king_moved) {
                        *count += usize::from(moved);
                    }
                    let mut expected = $acc::<L1>::new();
                    ft.refresh_accumulator(&pos, &mut expected);
                    let mut updated = $acc::<L1>::new();
                    ft.update_accumulator_with_cache(&pos, &dirty, &mut updated, &prev, &mut cache);
                    let mut refreshed = $acc::<L1>::new();
                    ft.refresh_accumulator_with_cache(&pos, &mut refreshed, &mut cache);
                    for p in 0..2 {
                        assert_eq!(
                            updated.accumulation[p].0, expected.accumulation[p].0,
                            "update {sfen} {ply} {p}"
                        );
                        assert_eq!(
                            refreshed.accumulation[p].0, expected.accumulation[p].0,
                            "refresh {sfen} {ply} {p}"
                        );
                    }

                    // 未評価の中間ノードを残す探索経路も、全再計算と照合する。
                    stack.push(dirty);
                    if ply % 3 != 0 {
                        if let Some((source, _)) = stack.find_usable_accumulator() {
                            assert!(ft.forward_update_incremental(&pos, &mut stack, source));
                            forward_updates += 1;
                        } else {
                            ft.refresh_accumulator_with_cache(
                                &pos,
                                &mut stack.current_mut().accumulator,
                                &mut lazy_cache,
                            );
                        }
                        for p in 0..2 {
                            assert_eq!(
                                stack.current().accumulator.accumulation[p].0,
                                expected.accumulation[p].0,
                                "lazy {sfen} {ply} {p}"
                            );
                        }
                    }

                    // 兄弟局面へ戻るときも、cache は子局面の内容を保持したまま使う。
                    if ply % 7 == 0 {
                        pos.undo_move(mv);
                        stack.pop();
                        ft.refresh_accumulator_with_cache(&pos, &mut refreshed, &mut cache);
                        for p in 0..2 {
                            assert_eq!(
                                refreshed.accumulation[p].0, prev.accumulation[p].0,
                                "undo {ply} {p}"
                            );
                        }
                    } else {
                        prev = updated;
                    }
                    if ply % 31 == 0 {
                        cache.invalidate();
                        lazy_cache.invalidate();
                    }
                    if ply % 11 == 0 && !pos.in_check() {
                        pos.do_null_move();
                        ft.update_accumulator_with_cache(
                            &pos,
                            &DirtyPiece::default(),
                            &mut refreshed,
                            &prev,
                            &mut cache,
                        );
                        for p in 0..2 {
                            assert_eq!(
                                refreshed.accumulation[p].0, prev.accumulation[p].0,
                                "null {ply} {p}"
                            );
                        }
                        pos.undo_null_move();
                    }
                }
            }
            assert!(kings.into_iter().all(|n| n > 0));
            assert!(captures > 0 && drops > 0 && promotions > 0);
            assert!(forward_updates > 0);
        }
    };
}

check_finny!(
    halfkp_finny_matches_refresh,
    network_halfkp,
    FeatureTransformerHalfKP,
    AccumulatorHalfKP,
    AccumulatorStackHalfKP
);
check_finny!(
    halfka_split_finny_matches_refresh,
    network_halfka_split,
    FeatureTransformerHalfKaSplit,
    AccumulatorHalfKaSplit,
    AccumulatorStackHalfKaSplit
);
check_finny!(
    halfka_merged_finny_matches_refresh,
    network_halfka_merged,
    FeatureTransformerHalfKaMerged,
    AccumulatorHalfKaMerged,
    AccumulatorStackHalfKaMerged
);
check_finny!(
    halfka_hm_merged_finny_matches_refresh,
    network_halfka_hm_merged,
    FeatureTransformerHalfKaHmMerged,
    AccumulatorHalfKaHmMerged,
    AccumulatorStackHalfKaHmMerged
);
check_finny!(
    halfka_hm_split_finny_matches_refresh,
    network_halfka_hm_split,
    FeatureTransformerHalfKaHmSplit,
    AccumulatorHalfKaHmSplit,
    AccumulatorStackHalfKaHmSplit
);
