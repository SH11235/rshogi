//! 定数除算と flags 化が探索の整数演算・枝刈り条件を維持することを検証する。

use super::super::SearchTuneParams;
use super::super::alpha_beta::{
    FutilityFlags, FutilityParams, build_reductions, late_move_pruning_limit, reduction,
};
use super::super::pruning::{quiet_history_reduction, try_futility_pruning};
use crate::types::Value;

#[test]
fn default_divisors_gate_only_specialized_parameters() {
    let defaults = SearchTuneParams::default();
    assert!(defaults.has_default_divisors());
    for value in [-1, 0, 1, 17, 32767] {
        for field in 0..3 {
            let mut tune = defaults;
            match field {
                0 => tune.main_hist_pruning_add_den = value,
                1 => tune.lmr_depth_history_div = value,
                _ => tune.lmr_reduction_non_improving_div = value,
            }
            assert!(!tune.has_default_divisors());
        }
    }
    let mut tune = defaults;
    tune.main_hist_pruning_add_num += 1;
    tune.lmr_reduction_non_improving_mult += 1;
    assert!(tune.has_default_divisors());
}

#[test]
fn constant_quiet_divisions_match_signed_runtime_divisions() {
    let tune = SearchTuneParams::default();
    // i16 履歴の全域と、ゼロ・正負の除算境界を含む継続履歴。
    for main_hist in i16::MIN..=i16::MAX {
        for cont_history in [
            -98304, -3221, -3220, -3219, -1, 0, 1, 3219, 3220, 3221, 98301,
        ] {
            let main_hist = i32::from(main_hist);
            let expected = (cont_history
                + tune.main_hist_pruning_add_num * main_hist
                    / tune.main_hist_pruning_add_den.max(1))
                / tune.lmr_depth_history_div.max(1);
            assert_eq!(quiet_history_reduction::<true>(&tune, cont_history, main_hist), expected);
            assert_eq!(quiet_history_reduction::<false>(&tune, cont_history, main_hist), expected);
        }
    }
}

#[test]
fn constant_quiet_divisions_keep_tuned_numerator() {
    let mut tune = SearchTuneParams::default();
    for numerator in [-77, -1, 0, 1, 77] {
        tune.main_hist_pruning_add_num = numerator;
        assert!(tune.has_default_divisors());
        for main_hist in [-32768, -33, -1, 0, 1, 33, 32767] {
            for cont_history in [-98304, -3221, -1, 0, 1, 3221, 98301] {
                assert_eq!(
                    quiet_history_reduction::<true>(&tune, cont_history, main_hist),
                    quiet_history_reduction::<false>(&tune, cont_history, main_hist),
                );
            }
        }
    }
}

#[test]
fn constant_reduction_matches_runtime_division() {
    let mut tune = SearchTuneParams::default();
    let reductions = build_reductions(tune.lmr_table_coeff);
    for multiplier in [
        -3,
        0,
        1,
        3,
        SearchTuneParams::DEFAULT.lmr_reduction_non_improving_mult,
    ] {
        tune.lmr_reduction_non_improving_mult = multiplier;
        for depth in [-1, 0, 1, 2, 10, 63, 127, 600] {
            for move_count in [-1, 0, 1, 2, 10, 63, 127, 600] {
                for imp in [false, true] {
                    for (delta, root_delta) in [(-1, -1), (0, 0), (1, 1), (31, 32), (32000, 64000)]
                    {
                        assert_eq!(
                            reduction::<true>(
                                &reductions,
                                &tune,
                                imp,
                                depth,
                                move_count,
                                delta,
                                root_delta
                            ),
                            reduction::<false>(
                                &reductions,
                                &tune,
                                imp,
                                depth,
                                move_count,
                                delta,
                                root_delta
                            ),
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn tuned_divisions_preserve_runtime_values_and_denominator_clamping() {
    let mut tune = SearchTuneParams::default();
    let reductions = build_reductions(tune.lmr_table_coeff);
    for denominator in [-17, 0, 1, 31, 511, 3219, 32767] {
        tune.main_hist_pruning_add_den = denominator;
        tune.lmr_depth_history_div = denominator;
        tune.lmr_reduction_non_improving_div = denominator;
        assert!(!tune.has_default_divisors());
        for main_hist in [-32768, -33, -1, 0, 1, 33, 32767] {
            for cont_history in [-98304, -3221, -1, 0, 1, 3221, 98301] {
                let expected = (cont_history
                    + tune.main_hist_pruning_add_num * main_hist / denominator.max(1))
                    / denominator.max(1);
                assert_eq!(
                    quiet_history_reduction::<false>(&tune, cont_history, main_hist),
                    expected
                );
            }
        }
        for depth in [1, 10, 63] {
            for move_count in [1, 10, 63] {
                for improving in [false, true] {
                    let scale = reductions[depth as usize] * reductions[move_count as usize];
                    let expected = scale - 31 * tune.lmr_reduction_delta_scale / 64
                        + i32::from(!improving) * scale * tune.lmr_reduction_non_improving_mult
                            / denominator.max(1)
                        + tune.lmr_reduction_base_offset;
                    assert_eq!(
                        reduction::<false>(
                            &reductions,
                            &tune,
                            improving,
                            depth,
                            move_count,
                            31,
                            64
                        ),
                        expected,
                    );
                }
            }
        }
    }
}

#[test]
fn lmp_constant_division_matches_runtime_division() {
    for depth in 0..=crate::types::MAX_PLY {
        for improving in [false, true] {
            assert_eq!(
                late_move_pruning_limit(depth, improving),
                (3 + depth * depth) / (2 - i32::from(improving)),
            );
        }
    }
}

#[test]
fn futility_flags_preserve_all_boolean_combinations() {
    let tune = SearchTuneParams::default();
    for bits in 0..128 {
        let improving = bits & 1 != 0;
        let opponent_worsening = bits & 2 != 0;
        let tt_hit = bits & 4 != 0;
        let tt_move_exists = bits & 8 != 0;
        let tt_capture = bits & 16 != 0;
        let tt_pv = bits & 32 != 0;
        let in_check = bits & 64 != 0;
        let flags = FutilityFlags::new(
            improving,
            opponent_worsening,
            tt_hit,
            tt_move_exists,
            tt_capture,
            tt_pv,
            in_check,
        );
        for (flag, expected) in [
            (FutilityFlags::IMPROVING, improving),
            (FutilityFlags::OPPONENT_WORSENING, opponent_worsening),
            (FutilityFlags::TT_HIT, tt_hit),
            (FutilityFlags::TT_MOVE_EXISTS, tt_move_exists),
            (FutilityFlags::TT_CAPTURE, tt_capture),
            (FutilityFlags::TT_PV, tt_pv),
            (FutilityFlags::IN_CHECK, in_check),
        ] {
            assert_eq!(flags.contains(flag), expected);
        }
        // 旧 bool 版の式を参照し、適用条件とマージン境界の両方を比較する。
        for depth in [1, 7, 13, 14] {
            for correction_value in [-131072_i32, 0, 131072] {
                let mult =
                    tune.futility_margin_base - tune.futility_margin_tt_bonus * i32::from(!tt_hit);
                let margin = mult * depth
                    - i32::from(improving) * mult * tune.futility_improving_scale / 1024
                    - i32::from(opponent_worsening) * mult * tune.futility_opponent_worsening_scale
                        / 4096
                    + correction_value.abs() / tune.futility_correction_div.max(1);
                for beta in [Value::new(-32000), Value::new(-100), Value::new(100)] {
                    for static_eval in [
                        Value::NONE,
                        Value::new(32000),
                        beta - Value::new(1),
                        beta,
                        beta + Value::new(margin - 1),
                        beta + Value::new(margin),
                    ] {
                        let expected = if !tt_pv
                            && !in_check
                            && depth < 14
                            && static_eval != Value::NONE
                            && static_eval >= beta
                            && !beta.is_loss()
                            && !static_eval.is_win()
                            && (!tt_move_exists || tt_capture)
                            && static_eval - Value::new(margin) >= beta
                        {
                            Some(Value::new((2 * beta.raw() + static_eval.raw()) / 3))
                        } else {
                            None
                        };
                        assert_eq!(
                            try_futility_pruning(
                                FutilityParams {
                                    depth,
                                    beta,
                                    static_eval,
                                    correction_value,
                                    flags,
                                },
                                &tune
                            ),
                            expected
                        );
                    }
                }
            }
        }
    }
}
