//! YaneuraOu SFNN の整数進行度 routing。tatara の f32 routing とは分離する。
//!
//! 探索用 stack は生成・reset 時に係数の Arc を取得し、探索中はロックなしで参照する。
//! 静的 LayerStacks は視点別の部分和を DirtyPiece から更新する。係数を再設定した場合は
//! 次の探索開始（stack の reset）から反映する。stack を持たない評価 API は都度取得する。
use super::bona_piece::BonaPiece;
use super::bona_piece_halfka_hm_merged::FE_OLD_END;
use super::constants::MAX_LAYER_STACK_BUCKETS;
use super::network::{SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS, get_layer_stack_progress_buckets};
use crate::position::Position;
use crate::types::Color;
#[cfg(test)]
use crate::types::PieceType;
use std::sync::{Arc, RwLock};
static WEIGHTS: RwLock<Option<Arc<[i32]>>> = RwLock::new(None);

/// 探索開始時に取得する不変の係数。再設定後も既存の探索は同じ配列を参照する。
pub(crate) fn snapshot_weights() -> Option<Arc<[i32]>> {
    use super::network::{LayerStackBucketMode, get_layer_stack_bucket_mode};
    if get_layer_stack_bucket_mode() != LayerStackBucketMode::ProgressKPAbsQ16 {
        return None;
    }
    WEIGHTS.read().unwrap_or_else(|e| e.into_inner()).clone()
}
// 添字 N ごとの round(ln((i/N) / (1-i/N)) * 65536), i=1..N。
// 初回評価も含めて、閾値の計算・ヒープ確保を評価経路へ持ち込まない。
const THRESHOLDS: [&[i64]; MAX_LAYER_STACK_BUCKETS + 1] = [
    &[], // N=0 は使用しない。
    &[],
    &[0],
    &[-45426, 45426],
    &[-71999, 0, 71999],
    &[-90852, -26573, 26573, 90852],
    &[-105476, -45426, 0, 45426, 105476],
    &[-117425, -60050, -18854, 18854, 60050, 117425],
    &[-127527, -71999, -33477, 0, 33477, 71999, 127527],
    &[-136278, -82101, -45426, -14624, 14624, 45426, 82101, 136278],
    &[
        -143997, -90852, -55529, -26573, 0, 26573, 55529, 90852, 143997,
    ],
    &[
        -150902, -98571, -64280, -36675, -11949, 11949, 36675, 64280, 98571, 150902,
    ],
    &[
        -157148, -105476, -71999, -45426, -22051, 0, 22051, 45426, 71999, 105476, 157148,
    ],
    &[
        -162851, -111722, -78904, -53145, -30802, -10102, 10102, 30802, 53145, 78904, 111722,
        162851,
    ],
    &[
        -168097, -117425, -85150, -60050, -38521, -18854, 0, 18854, 38521, 60050, 85150, 117425,
        168097,
    ],
    &[
        -172953, -122670, -90852, -66296, -45426, -26573, -8751, 8751, 26573, 45426, 66296, 90852,
        122670, 172953,
    ],
    &[
        -177475, -127527, -96098, -71999, -51672, -33477, -16470, 0, 16470, 33477, 51672, 71999,
        96098, 127527, 177475,
    ],
];
const PADDED_THRESHOLDS: [[i64; MAX_LAYER_STACK_BUCKETS]; MAX_LAYER_STACK_BUCKETS + 1] = {
    let mut table = [[i64::MAX; MAX_LAYER_STACK_BUCKETS]; MAX_LAYER_STACK_BUCKETS + 1];
    let mut n = 1;
    while n <= MAX_LAYER_STACK_BUCKETS {
        let mut i = 0;
        while i < THRESHOLDS[n].len() {
            table[n][i] = THRESHOLDS[n][i];
            i += 1;
        }
        n += 1;
    }
    table
};

/// raw f64 LE 係数を YaneuraOu と同じ round(w * 65536) / i32 clamp で量子化する。
pub fn load_progress_coeff_kpabs_q16_from_bytes(bytes: &[u8]) -> Result<Box<[i32]>, String> {
    if bytes.len() != SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS * 8 {
        return Err("progress Q16 coefficient size mismatch".to_string());
    }
    bytes
        .chunks_exact(8)
        .map(|chunk| {
            let value = f64::from_le_bytes(chunk.try_into().expect("exact chunk size"));
            if !value.is_finite() {
                return Err("progress Q16 coefficient must be finite".to_string());
            }
            Ok((value * 65536.0).round() as i32)
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Vec::into_boxed_slice)
}
/// Q16 係数を設定する。探索前に routing と合わせて設定すること。
/// 既存の探索用 stack への反映には再生成または reset が必要。
pub fn set_layer_stack_progress_kpabs_q16_weights(weights: Box<[i32]>) -> Result<(), String> {
    if weights.len() != SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS {
        return Err("progress Q16 weights length mismatch".to_string());
    }
    *WEIGHTS.write().map_err(|_| "progress Q16 weights lock poisoned")? = Some(Arc::from(weights));
    Ok(())
}
/// Q16 係数を未設定へ戻す。
/// 取得済みの探索用 stack は reset まで元の係数を保持する。
pub fn reset_layer_stack_progress_kpabs_q16_weights() {
    *WEIGHTS.write().unwrap_or_else(|e| e.into_inner()) = None;
}
/// Q16 logit 和を bucket へ変換する。閾値と等しい値は上側の bucket に属する。
pub fn progress_q16_sum_to_bucket(sum: i64, num_buckets: usize) -> usize {
    assert!((1..=MAX_LAYER_STACK_BUCKETS).contains(&num_buckets));
    // 有効な閾値はすべて i64::MAX - 1 未満なので、上端を丸めても bucket は変わらない。
    // i64::MAX 入力でも padding を数えず、固定長の比較を使える。
    let sum = sum.min(i64::MAX - 1);
    let thresholds = &PADDED_THRESHOLDS[num_buckets];
    #[cfg(all(target_arch = "x86_64", target_feature = "avx512f"))]
    {
        use std::arch::x86_64::{_mm512_cmple_epi64_mask, _mm512_loadu_si512, _mm512_set1_epi64};
        const { assert!(MAX_LAYER_STACK_BUCKETS == 16) };
        // SAFETY: cfg で AVX-512F を保証し、16 要素の配列内から 8 要素ずつ読む。
        // loadu はアラインメントを要求せず、配列は両 load が完了するまで有効。
        unsafe {
            let value = _mm512_set1_epi64(sum);
            let low = _mm512_loadu_si512(thresholds.as_ptr().cast());
            let high = _mm512_loadu_si512(thresholds.as_ptr().add(8).cast());
            (_mm512_cmple_epi64_mask(low, value).count_ones()
                + _mm512_cmple_epi64_mask(high, value).count_ones()) as usize
        }
    }
    #[cfg(not(all(target_arch = "x86_64", target_feature = "avx512f")))]
    thresholds.iter().map(|&threshold| usize::from(threshold <= sum)).sum()
}
pub(crate) fn configured_progress_q16_bucket(pos: &Position, stored_buckets: usize) -> usize {
    let weights = snapshot_weights();
    bucket_with_weights(pos, stored_buckets, weights.as_deref())
}

pub(crate) fn routing_bucket_count(stored_buckets: usize) -> usize {
    let count =
        get_layer_stack_progress_buckets().expect("LayerStacks Q16 routing is not configured");
    assert!(count <= stored_buckets, "Q16 routing exceeds stored buckets");
    count
}

pub(crate) fn bucket_with_weights(
    pos: &Position,
    stored_buckets: usize,
    weights: Option<&[i32]>,
) -> usize {
    let count = routing_bucket_count(stored_buckets);
    if count == 1 {
        return 0;
    }
    let weights = weights.expect("LayerStacks Q16 coefficients are not configured");
    progress_q16_sum_to_bucket(compute_progresskpabs_q16_sum(pos, weights), count)
}
/// 盤上・持駒の両視点の係数を i64 で合算する。float 差分キャッシュは使用しない。
pub fn compute_progresskpabs_q16_sum(pos: &Position, weights: &[i32]) -> i64 {
    assert_eq!(weights.len(), SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS);
    [Color::Black, Color::White]
        .into_iter()
        .map(|perspective| compute_half(pos, perspective, weight_row(pos, perspective, weights)))
        .sum()
}

pub(crate) fn weight_row<'a>(pos: &Position, perspective: Color, weights: &'a [i32]) -> &'a [i32] {
    let king = pos.king_square(perspective);
    let square = if perspective == Color::Black {
        king
    } else {
        king.inverse()
    }
    .index();
    &weights[square * FE_OLD_END..(square + 1) * FE_OLD_END]
}

pub(crate) fn compute_half(pos: &Position, perspective: Color, row: &[i32]) -> i64 {
    let list = if perspective == Color::Black {
        pos.piece_list().piece_list_fb()
    } else {
        pos.piece_list().piece_list_fw()
    };
    list[..super::piece_list::PieceNumber::KING as usize]
        .iter()
        .map(|&bp| coefficient(row, bp))
        .sum()
}

#[inline]
fn coefficient(row: &[i32], bp: BonaPiece) -> i64 {
    // 駒落ちの空き枠は係数ファイルの index 0 の値にかかわらず寄与しない。
    if bp == BonaPiece::ZERO {
        0
    } else {
        i64::from(row[bp.value() as usize])
    }
}

#[cfg(feature = "layerstack-arch")]
pub(crate) fn half_delta(
    dirty: &super::accumulator::DirtyPiece,
    perspective: Color,
    row: &[i32],
) -> i64 {
    let mut delta = 0;
    for i in 0..usize::from(dirty.dirty_num) {
        if dirty.piece_no[i].0 >= super::piece_list::PieceNumber::KING {
            continue;
        }
        let changed = &dirty.changed_piece[i];
        let (old, new) = if perspective == Color::Black {
            (changed.old_piece.fb, changed.new_piece.fb)
        } else {
            (changed.old_piece.fw, changed.new_piece.fw)
        };
        // i32 の端値同士の差も i64 へ拡張してから計算する。
        delta += coefficient(row, new) - coefficient(row, old);
    }
    delta
}

#[cfg(test)]
pub(super) fn reference_board_sums(pos: &Position, weights: &[i32]) -> [i64; 2] {
    assert_eq!(
        weights.len(),
        SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS,
        "progresskpabs weights length mismatch"
    );

    let sq_bk = pos.king_square(Color::Black).index();
    let sq_wk = pos.king_square(Color::White).inverse().index();
    let weights_b = &weights[sq_bk * FE_OLD_END..(sq_bk + 1) * FE_OLD_END];
    let weights_w = &weights[sq_wk * FE_OLD_END..(sq_wk + 1) * FE_OLD_END];

    let mut sum = [0i64; 2];

    for sq in pos.occupied().iter() {
        let pc = pos.piece_on(sq);
        if pc.is_none() || pc.piece_type() == PieceType::King {
            continue;
        }

        let bp_b = BonaPiece::from_piece_square(pc, sq, Color::Black);
        if bp_b != BonaPiece::ZERO {
            sum[0] += i64::from(weights_b[bp_b.value() as usize]);
        }

        let bp_w = BonaPiece::from_piece_square(pc, sq, Color::White);
        if bp_w != BonaPiece::ZERO {
            sum[1] += i64::from(weights_w[bp_w.value() as usize]);
        }
    }

    for owner in [Color::Black, Color::White] {
        let hand = pos.hand(owner);
        for &pt in &PieceType::HAND_PIECES {
            let count = hand.count(pt);
            for c in 1..=count {
                let c_u8 = u8::try_from(c).expect("hand count fits in u8");

                let bp_b = BonaPiece::from_hand_piece(Color::Black, owner, pt, c_u8);
                if bp_b != BonaPiece::ZERO {
                    sum[0] += i64::from(weights_b[bp_b.value() as usize]);
                }

                let bp_w = BonaPiece::from_hand_piece(Color::White, owner, pt, c_u8);
                if bp_w != BonaPiece::ZERO {
                    sum[1] += i64::from(weights_w[bp_w.value() as usize]);
                }
            }
        }
    }

    sum
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{Rng, SeedableRng};
    use rand_xoshiro::Xoshiro256PlusPlus;

    #[test]
    fn bucket_count_matches_partition_point() {
        let mut rng = Xoshiro256PlusPlus::seed_from_u64(0x0051_3136_434e_5400);
        for (n, thresholds) in THRESHOLDS.iter().enumerate().skip(1) {
            assert!(thresholds.iter().all(|&t| t < i64::MAX - 1));
            let check = |sum| {
                assert_eq!(
                    progress_q16_sum_to_bucket(sum, n),
                    thresholds.partition_point(|&threshold| threshold <= sum),
                    "N={n} sum={sum}"
                );
            };
            for sum in [
                i64::MIN,
                i64::MIN + 1,
                -1_000_000_000_000,
                -1,
                0,
                1,
                1_000_000_000_000,
                i64::MAX - 1,
                i64::MAX,
            ] {
                check(sum);
            }
            for &threshold in *thresholds {
                for sum in [threshold - 1, threshold, threshold + 1] {
                    check(sum);
                }
            }
            for _ in 0..4096 {
                check(rng.random::<i64>());
                check(rng.random_range(-200_000..=200_000));
            }
        }
    }

    #[test]
    fn fixed_thresholds_match_yo_for_every_supported_bucket_count() {
        for (n, thresholds) in THRESHOLDS.iter().enumerate().skip(1) {
            assert_eq!(thresholds.len(), n - 1);
            assert_eq!(progress_q16_sum_to_bucket(i64::MIN, n), 0);
            assert_eq!(progress_q16_sum_to_bucket(i64::MAX, n), n - 1);
            for i in 1..n {
                let p = i as f64 / n as f64;
                let expected = ((p / (1.0 - p)).ln() * 65536.0).round() as i64;
                assert_eq!(thresholds[i - 1], expected, "N={n} i={i}");
                assert_eq!(progress_q16_sum_to_bucket(expected - 1, n), i - 1);
                assert_eq!(progress_q16_sum_to_bucket(expected, n), i);
                assert_eq!(progress_q16_sum_to_bucket(expected + 1, n), i);
            }
        }
    }

    #[test]
    fn configured_routing_uses_integer_coefficients() {
        use super::super::network::{
            LayerStackBucketMode, configure_layer_stack_routing, layer_stack_routing_test_guard,
            reset_layer_stack_progress_buckets,
        };
        let _guard = layer_stack_routing_test_guard();
        let mut pos = Position::new();
        pos.set_sfen(crate::position::SFEN_HIRATE).unwrap();
        configure_layer_stack_routing(LayerStackBucketMode::ProgressKPAbsQ16, 4, Some(4)).unwrap();
        set_layer_stack_progress_kpabs_q16_weights(
            vec![i32::MAX; SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS].into_boxed_slice(),
        )
        .unwrap();
        assert_eq!(configured_progress_q16_bucket(&pos, 4), 3);
        set_layer_stack_progress_kpabs_q16_weights(
            vec![i32::MIN; SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS].into_boxed_slice(),
        )
        .unwrap();
        assert_eq!(configured_progress_q16_bucket(&pos, 4), 0);
        reset_layer_stack_progress_kpabs_q16_weights();
        configure_layer_stack_routing(LayerStackBucketMode::ProgressKPAbsQ16, 1, Some(1)).unwrap();
        assert_eq!(configured_progress_q16_bucket(&pos, 1), 0);
        reset_layer_stack_progress_buckets();
    }

    #[test]
    fn quantization_matches_yo_round_and_clamp() {
        let mut bytes = vec![0; SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS * 8];
        let values = [
            0.5 / 65536.0,
            -0.5 / 65536.0,
            1.49 / 65536.0,
            -1.49 / 65536.0,
            f64::MAX,
            -f64::MAX,
        ];
        for (chunk, value) in bytes.chunks_exact_mut(8).zip(values) {
            chunk.copy_from_slice(&value.to_le_bytes());
        }
        let weights = load_progress_coeff_kpabs_q16_from_bytes(&bytes).unwrap();
        assert_eq!(&weights[..6], &[1, -1, 1, -1, i32::MAX, i32::MIN]);
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            bytes[..8].copy_from_slice(&value.to_le_bytes());
            assert!(load_progress_coeff_kpabs_q16_from_bytes(&bytes).is_err());
        }
        assert!(load_progress_coeff_kpabs_q16_from_bytes(&bytes[..bytes.len() - 1]).is_err());
    }

    #[test]
    fn yo_thresholds_match_bulletou_scalar_progress_boundaries() {
        // BulletOu game/outputs.rs: 256 logit thresholds, then integer value * N / 256.
        // YaneuraOu evaluate_nnue.cpp: direct N logit thresholds with upper_bound.
        let bullet: Vec<i64> = (1..256)
            .map(|i| {
                let p = i as f64 / 256.0;
                (p.ln() - (1.0 - p).ln()).mul_add(65536.0, 0.0).round() as i64
            })
            .collect();
        for n in [2, 4, 8, 16] {
            for &threshold in &bullet {
                for sum in [threshold - 1, threshold, threshold + 1] {
                    let value = bullet.partition_point(|&t| t <= sum);
                    assert_eq!(
                        progress_q16_sum_to_bucket(sum, n),
                        value * n / 256,
                        "N={n} sum={sum}"
                    );
                }
            }
            for k in 1..n {
                let t = bullet[k * 256 / n - 1];
                assert_eq!(progress_q16_sum_to_bucket(t - 1, n), k - 1);
                assert_eq!(progress_q16_sum_to_bucket(t, n), k);
                assert_eq!(progress_q16_sum_to_bucket(t + 1, n), k);
            }
        }
    }

    #[test]
    fn board_and_hand_sum_matches_yo_piece_lists_and_color_symmetry() {
        let weights: Vec<i32> = (0..SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS)
            .map(|i| (i as i32 * 7919) % 65537 - 32768)
            .collect();
        let mut sums = Vec::new();
        for sfen in [
            "4k4/9/9/9/9/9/2P6/9/4K4 b r 1",
            "4k4/9/6p2/9/9/9/9/9/4K4 w R 1",
        ] {
            let mut pos = Position::new();
            pos.set_sfen(sfen).unwrap();
            let bk = pos.king_square(Color::Black).index() * FE_OLD_END;
            let wk = pos.king_square(Color::White).inverse().index() * FE_OLD_END;
            // YO SumQ16 enumerates the two BonaPiece lists rather than board/hand scans.
            let reference: i64 = pos
                .piece_list()
                .piece_list_fb()
                .iter()
                .zip(pos.piece_list().piece_list_fw())
                .take(38)
                .filter(|(b, w)| **b != BonaPiece::ZERO && **w != BonaPiece::ZERO)
                .map(|(b, w)| {
                    i64::from(weights[bk + b.value() as usize])
                        + i64::from(weights[wk + w.value() as usize])
                })
                .sum();
            let actual = compute_progresskpabs_q16_sum(&pos, &weights);
            assert_eq!(actual, reference);
            assert_eq!(actual, reference_board_sums(&pos, &weights).iter().sum::<i64>());
            sums.push(actual);
        }
        assert_eq!(sums[0], sums[1]);
    }
}

#[cfg(all(test, feature = "layerstack-arch"))]
mod incremental_tests {
    use super::*;
    use crate::movegen::{MoveList, generate_legal_all};
    use crate::nnue::accumulator_layer_stacks::{
        AccumulatorStackLayerStacks, StackEntryLayerStacks,
    };
    use crate::nnue::network::{
        LayerStackBucketMode, configure_layer_stack_routing, layer_stack_routing_test_guard,
        reset_layer_stack_progress_buckets,
    };
    use rand::{Rng, SeedableRng};
    use rand_xoshiro::Xoshiro256PlusPlus;

    fn check<const N: usize>(
        pos: &Position,
        stack: &mut AccumulatorStackLayerStacks<N>,
        weights: &[i32],
    ) {
        let expected = reference_board_sums(pos, weights);
        let sum = expected.iter().sum();
        assert_eq!(compute_progresskpabs_q16_sum(pos, weights), sum);
        for count in [2, 4, 9, 16] {
            configure_layer_stack_routing(
                LayerStackBucketMode::ProgressKPAbsQ16,
                count,
                Some(count),
            )
            .unwrap();
            assert_eq!(
                stack.ensure_progress_q16_bucket(pos, count),
                progress_q16_sum_to_bucket(sum, count)
            );
            assert_eq!(stack.current().progress_q16, expected, "{}", pos.to_sfen());
            assert_eq!(stack.current().progress_q16_valid, 3);
        }
    }

    #[test]
    fn random_incremental_halves_match_board_scan() {
        let _guard = layer_stack_routing_test_guard();
        let mut rng = Xoshiro256PlusPlus::seed_from_u64(0x0051_3136_4c46);
        let weights: Vec<i32> = (0..SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS)
            .map(|i| match i % 97 {
                0 => i32::MIN,
                1 => i32::MAX,
                _ => rng.random_range(-20000..=20000),
            })
            .collect();
        configure_layer_stack_routing(LayerStackBucketMode::ProgressKPAbsQ16, 16, Some(16))
            .unwrap();
        set_layer_stack_progress_kpabs_q16_weights(weights.clone().into_boxed_slice()).unwrap();
        let mut eager = AccumulatorStackLayerStacks::<32>::new();
        let mut lazy = AccumulatorStackLayerStacks::<32>::new();
        let mut counts = [0usize; 6]; // 手数・先手玉・後手玉・打ち・成り・捕獲
        let mut nulls = 0;
        for game in 0..200 {
            let mut pos = Position::new();
            pos.set_sfen(if game % 2 == 0 {
                crate::position::SFEN_HIRATE
            } else {
                "4k4/9/6p2/9/9/9/2P6/9/4K4 b RBGSNLrbgsnl 1"
            })
            .unwrap();
            eager.reset();
            lazy.reset();
            check(&pos, &mut eager, &weights);
            check(&pos, &mut lazy, &weights);
            let mut played = Vec::new();
            for ply in 0..200 {
                if !pos.in_check() && ply % 11 == 0 {
                    pos.do_null_move();
                    eager.push();
                    lazy.push();
                    check(&pos, &mut eager, &weights);
                    check(&pos, &mut lazy, &weights);
                    pos.undo_null_move();
                    eager.pop();
                    lazy.pop();
                    nulls += 1;
                }
                let mut moves = MoveList::new();
                generate_legal_all(&pos, &mut moves);
                if moves.is_empty() {
                    break;
                }
                let mv = moves.at(rng.random_range(0..moves.len()));
                counts[0] += 1;
                counts[3] += usize::from(mv.is_drop());
                counts[4] += usize::from(mv.is_promotion());
                counts[5] += usize::from(!pos.piece_on(mv.to()).is_none());
                let dirty = pos.do_move(mv, pos.gives_check(mv));
                for p in 0..2 {
                    counts[p + 1] += usize::from(dirty.king_moved[p]);
                }
                eager.push();
                lazy.push();
                eager.current_mut().dirty_piece = dirty;
                lazy.current_mut().dirty_piece = dirty;
                played.push(mv);
                check(&pos, &mut eager, &weights);
                // 8 手の上限内外と、未評価の祖先を経由する経路を交互に通す。
                if ply % [1, 8, 9, 17][game % 4] == 0 {
                    check(&pos, &mut lazy, &weights);
                }
                if ply % 7 == 6 {
                    pos.undo_move(played.pop().unwrap());
                    eager.pop();
                    lazy.pop();
                    check(&pos, &mut eager, &weights);
                }
            }
            for mv in played.into_iter().rev() {
                pos.undo_move(mv);
                eager.pop();
                lazy.pop();
                check(&pos, &mut eager, &weights);
                check(&pos, &mut lazy, &weights);
            }
        }
        assert!(counts[0] >= 30000, "{counts:?}");
        assert!(counts.iter().all(|&n| n > 0));
        assert!(nulls > 0);
        eprintln!(
            "Q16 cases={counts:?}, null={nulls}, entry1536={} bytes",
            size_of::<StackEntryLayerStacks<1536>>()
        );
        reset_layer_stack_progress_kpabs_q16_weights();
        reset_layer_stack_progress_buckets();
    }

    #[test]
    fn snapshot_survives_reload_and_reset_refreshes_it() {
        let _guard = layer_stack_routing_test_guard();
        configure_layer_stack_routing(LayerStackBucketMode::ProgressKPAbsQ16, 4, Some(4)).unwrap();
        let mut pos = Position::new();
        pos.set_hirate();
        let old = vec![i32::MAX; SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS];
        let new = vec![i32::MIN; SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS];
        set_layer_stack_progress_kpabs_q16_weights(old.clone().into_boxed_slice()).unwrap();
        let mut stack = AccumulatorStackLayerStacks::<32>::new();
        check(&pos, &mut stack, &old);
        set_layer_stack_progress_kpabs_q16_weights(new.clone().into_boxed_slice()).unwrap();
        reset_layer_stack_progress_kpabs_q16_weights();
        // 未計算の子でも取得済みの係数が生存し、祖先と同じ世代を使う。
        stack.push();
        check(&pos, &mut stack, &old);
        set_layer_stack_progress_kpabs_q16_weights(new.clone().into_boxed_slice()).unwrap();
        stack.reset();
        check(&pos, &mut stack, &new);
        reset_layer_stack_progress_kpabs_q16_weights();
        configure_layer_stack_routing(LayerStackBucketMode::ProgressKPAbsQ16, 1, Some(1)).unwrap();
        stack.reset();
        assert_eq!(stack.ensure_progress_q16_bucket(&pos, 1), 0);
        reset_layer_stack_progress_buckets();
    }

    #[test]
    fn walk_bound_and_king_refresh_are_per_perspective() {
        let _guard = layer_stack_routing_test_guard();
        configure_layer_stack_routing(LayerStackBucketMode::ProgressKPAbsQ16, 4, Some(4)).unwrap();
        let weights: Vec<i32> = (0..SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS)
            .map(|i| (i as i32 * 7919) % 65537 - 32768)
            .collect();
        set_layer_stack_progress_kpabs_q16_weights(weights.clone().into_boxed_slice()).unwrap();
        let mut pos = Position::new();
        pos.set_hirate();
        let mut stack = AccumulatorStackLayerStacks::<32>::new();
        for depth in [8, 9] {
            stack.reset();
            check(&pos, &mut stack, &weights);
            for _ in 0..depth {
                pos.do_null_move();
                stack.push();
            }
            check(&pos, &mut stack, &weights);
            for index in 1..depth {
                assert_eq!(
                    stack.entry_at(index).progress_q16_valid,
                    if depth == 8 { 3 } else { 0 }
                );
            }
            for _ in 0..depth {
                pos.undo_null_move();
            }
        }
        stack.reset();
        check(&pos, &mut stack, &weights);
        for usi in ["5i6h", "null", "6h5i", "null"] {
            if usi == "null" {
                pos.do_null_move();
                stack.push();
            } else {
                let mv = pos.to_move(crate::types::Move::from_usi(usi).unwrap()).unwrap();
                assert!(pos.is_legal(mv));
                let dirty = pos.do_move(mv, pos.gives_check(mv));
                stack.push();
                stack.current_mut().dirty_piece = dirty;
            }
        }
        check(&pos, &mut stack, &weights);
        // 往復した玉も再計算が必要。もう一方の視点だけ祖先から差分を伝播する。
        for index in 1..4 {
            assert_eq!(stack.entry_at(index).progress_q16_valid, 2);
        }
        stack.push();
        stack.current_mut().previous = Some(usize::MAX);
        check(&pos, &mut stack, &weights);
        reset_layer_stack_progress_kpabs_q16_weights();
        reset_layer_stack_progress_buckets();
    }
}
