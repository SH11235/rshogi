//! YaneuraOu SFNN の整数進行度 routing。tatara の f32 routing とは分離する。
use super::bona_piece::BonaPiece;
use super::bona_piece_halfka_hm_merged::FE_OLD_END;
use super::constants::MAX_LAYER_STACK_BUCKETS;
use super::network::{SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS, get_layer_stack_progress_buckets};
use crate::position::Position;
use crate::types::{Color, PieceType};
use std::sync::{OnceLock, RwLock};
static WEIGHTS: RwLock<Option<Box<[i32]>>> = RwLock::new(None);
static THRESHOLDS: [OnceLock<Box<[i64]>>; MAX_LAYER_STACK_BUCKETS + 1] =
    [const { OnceLock::new() }; MAX_LAYER_STACK_BUCKETS + 1];
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
pub fn set_layer_stack_progress_kpabs_q16_weights(weights: Box<[i32]>) -> Result<(), String> {
    if weights.len() != SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS {
        return Err("progress Q16 weights length mismatch".to_string());
    }
    *WEIGHTS.write().map_err(|_| "progress Q16 weights lock poisoned")? = Some(weights);
    Ok(())
}
/// Q16 係数を未設定へ戻す。
pub fn reset_layer_stack_progress_kpabs_q16_weights() {
    *WEIGHTS.write().unwrap_or_else(|e| e.into_inner()) = None;
}
/// Q16 logit 和を bucket へ変換する。閾値と等しい値は上側の bucket に属する。
pub fn progress_q16_sum_to_bucket(sum: i64, num_buckets: usize) -> usize {
    assert!((1..=MAX_LAYER_STACK_BUCKETS).contains(&num_buckets));
    let thresholds = THRESHOLDS[num_buckets].get_or_init(|| {
        (1..num_buckets)
            .map(|i| {
                let p = i as f64 / num_buckets as f64;
                ((p / (1.0 - p)).ln() * 65536.0).round() as i64
            })
            .collect::<Vec<_>>()
            .into_boxed_slice()
    });
    thresholds.partition_point(|&threshold| threshold <= sum)
}
pub(crate) fn configured_progress_q16_bucket(pos: &Position, stored_buckets: usize) -> usize {
    let count =
        get_layer_stack_progress_buckets().expect("LayerStacks Q16 routing is not configured");
    assert!(count <= stored_buckets, "Q16 routing exceeds stored buckets");
    if count == 1 {
        return 0;
    }
    let guard = WEIGHTS.read().unwrap_or_else(|e| e.into_inner());
    let weights = guard.as_ref().expect("LayerStacks Q16 coefficients are not configured");
    progress_q16_sum_to_bucket(compute_progresskpabs_q16_sum(pos, weights), count)
}
/// 盤上・持駒の両視点の係数を i64 で合算する。float 差分キャッシュは使用しない。
pub fn compute_progresskpabs_q16_sum(pos: &Position, weights: &[i32]) -> i64 {
    assert_eq!(
        weights.len(),
        SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS,
        "progresskpabs weights length mismatch"
    );

    let sq_bk = pos.king_square(Color::Black).index();
    let sq_wk = pos.king_square(Color::White).inverse().index();
    let weights_b = &weights[sq_bk * FE_OLD_END..(sq_bk + 1) * FE_OLD_END];
    let weights_w = &weights[sq_wk * FE_OLD_END..(sq_wk + 1) * FE_OLD_END];

    let mut sum = 0i64;

    for sq in pos.occupied().iter() {
        let pc = pos.piece_on(sq);
        if pc.is_none() || pc.piece_type() == PieceType::King {
            continue;
        }

        let bp_b = BonaPiece::from_piece_square(pc, sq, Color::Black);
        if bp_b != BonaPiece::ZERO {
            sum += i64::from(weights_b[bp_b.value() as usize]);
        }

        let bp_w = BonaPiece::from_piece_square(pc, sq, Color::White);
        if bp_w != BonaPiece::ZERO {
            sum += i64::from(weights_w[bp_w.value() as usize]);
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
                    sum += i64::from(weights_b[bp_b.value() as usize]);
                }

                let bp_w = BonaPiece::from_hand_piece(Color::White, owner, pt, c_u8);
                if bp_w != BonaPiece::ZERO {
                    sum += i64::from(weights_w[bp_w.value() as usize]);
                }
            }
        }
    }

    sum
}

#[cfg(test)]
mod tests {
    use super::*;

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
            sums.push(actual);
        }
        assert_eq!(sums[0], sums[1]);
    }
}
