//! 探索チューニングパラメータ（SPSA向け）
//!
//! USI `setoption` で更新できる探索係数を集約する。

/// 1つのチューニング項目のUSI定義。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchTuneOptionSpec {
    /// USI option 名（`SPSA_` プレフィックス）
    pub usi_name: &'static str,
    /// デフォルト値
    pub default: i32,
    /// 最小値（inclusive）
    pub min: i32,
    /// 最大値（inclusive）
    pub max: i32,
}

/// `setoption` で1項目を適用した結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchTuneSetResult {
    /// 反映後の値（必要なら clamp 後）
    pub applied: i32,
    /// 入力値が範囲外で clamp されたか
    pub clamped: bool,
    /// 最小値（inclusive）
    pub min: i32,
    /// 最大値（inclusive）
    pub max: i32,
}

/// 探索係数の集合。
///
/// デフォルト値は現行実装の固定定数と一致させている。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchTuneParams {
    /// IIR: shallow 側の prior reduction しきい値
    pub iir_prior_reduction_threshold_shallow: i32,
    /// IIR: deep 側の prior reduction しきい値
    pub iir_prior_reduction_threshold_deep: i32,
    /// IIR: shallow/deep を切り替える深さ境界
    pub iir_depth_boundary: i32,
    /// IIR: eval 和判定しきい値
    pub iir_eval_sum_threshold: i32,

    /// draw jitter: ノード数に対するビットマスク
    pub draw_jitter_mask: i32,
    /// draw jitter: オフセット
    pub draw_jitter_offset: i32,

    /// LMR: delta スケール
    pub lmr_reduction_delta_scale: i32,
    /// LMR: non-improving 補正の分子
    pub lmr_reduction_non_improving_mult: i32,
    /// LMR: non-improving 補正の分母
    pub lmr_reduction_non_improving_div: i32,
    /// LMR: ベースオフセット
    pub lmr_reduction_base_offset: i32,

    /// Futility: 基本マージン係数
    pub futility_margin_base: i32,
    /// Futility: TT非ヒット時の減算係数
    pub futility_margin_tt_bonus: i32,
    /// Futility: improving 補正係数（/1024）
    pub futility_improving_scale: i32,
    /// Futility: opponent worsening 補正係数（/4096）
    pub futility_opponent_worsening_scale: i32,
    /// Futility: correction 絶対値補正の分母
    pub futility_correction_div: i32,

    /// Small ProbCut: beta マージン
    pub small_probcut_margin: i32,

    /// Razoring: ベースマージン
    pub razoring_margin_base: i32,
    /// Razoring: depth^2 係数
    pub razoring_margin_depth2_coeff: i32,

    /// NMP: margin の depth 係数
    pub nmp_margin_depth_mult: i32,
    /// NMP: margin の定数オフセット
    pub nmp_margin_offset: i32,
    /// NMP: reduction のベース
    pub nmp_reduction_base: i32,
    /// NMP: reduction の depth 除算
    pub nmp_reduction_depth_div: i32,
    /// NMP: verification search を有効化する深さ
    pub nmp_verification_depth_threshold: i32,
    /// NMP: nmp_min_ply 更新式の分子
    pub nmp_min_ply_update_num: i32,
    /// NMP: nmp_min_ply 更新式の分母
    pub nmp_min_ply_update_den: i32,

    /// ProbCut: beta マージン基準
    pub probcut_beta_margin_base: i32,
    /// ProbCut: improving 時の減算量
    pub probcut_beta_improving_sub: i32,
    /// ProbCut: dynamic reduction の分母
    pub probcut_dynamic_reduction_div: i32,
    /// ProbCut: 深さオフセット
    pub probcut_depth_base: i32,

    /// QSearch: futility ベース
    pub qsearch_futility_base: i32,

    /// TTMoveHistory: best==tt 時の更新量
    pub tt_move_history_bonus: i32,
    /// TTMoveHistory: best!=tt 時の更新量
    pub tt_move_history_malus: i32,
    /// prior capture countermove 更新量
    pub prior_capture_countermove_bonus: i32,
}

const SPSA_OPTION_SPECS: &[SearchTuneOptionSpec] = &[
    SearchTuneOptionSpec {
        usi_name: "SPSA_IIR_SHALLOW",
        default: 1,
        min: 0,
        max: 8,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_IIR_DEEP",
        default: 3,
        min: 0,
        max: 16,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_IIR_DEPTH_BOUNDARY",
        default: 10,
        min: 1,
        max: 64,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_IIR_EVAL_SUM",
        default: 177,
        min: 0,
        max: 5000,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_DRAW_JITTER_MASK",
        default: 2,
        min: 0,
        max: 31,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_DRAW_JITTER_OFFSET",
        default: -1,
        min: -16,
        max: 16,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_LMR_DELTA_SCALE",
        default: 757,
        min: 0,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_LMR_NON_IMPROVING_MULT",
        default: 218,
        min: 0,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_LMR_NON_IMPROVING_DIV",
        default: 512,
        min: 1,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_LMR_BASE_OFFSET",
        default: 1200,
        min: -8192,
        max: 8192,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_FUTILITY_MARGIN_BASE",
        default: 91,
        min: 0,
        max: 1024,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_FUTILITY_MARGIN_TT_BONUS",
        default: 21,
        min: 0,
        max: 512,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_FUTILITY_IMPROVING_SCALE",
        default: 2094,
        min: 0,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_FUTILITY_OPP_WORSENING_SCALE",
        default: 1324,
        min: 0,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_FUTILITY_CORRECTION_DIV",
        default: 158_105,
        min: 1,
        max: 1_000_000,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_SMALL_PROBCUT_MARGIN",
        default: 417,
        min: 0,
        max: 2048,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_RAZORING_BASE",
        default: 514,
        min: 0,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_RAZORING_DEPTH2",
        default: 294,
        min: 0,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_NMP_MARGIN_DEPTH_MULT",
        default: 18,
        min: 0,
        max: 256,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_NMP_MARGIN_OFFSET",
        default: -390,
        min: -4096,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_NMP_REDUCTION_BASE",
        default: 7,
        min: 1,
        max: 32,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_NMP_REDUCTION_DEPTH_DIV",
        default: 3,
        min: 1,
        max: 32,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_NMP_VERIFICATION_DEPTH",
        default: 16,
        min: 1,
        max: 128,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_NMP_MIN_PLY_NUM",
        default: 3,
        min: 1,
        max: 32,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_NMP_MIN_PLY_DEN",
        default: 4,
        min: 1,
        max: 32,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_PROBCUT_BETA_MARGIN",
        default: 215,
        min: 0,
        max: 2048,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_PROBCUT_IMPROVING_SUB",
        default: 60,
        min: 0,
        max: 1024,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_PROBCUT_DYNAMIC_DIV",
        default: 300,
        min: 1,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_PROBCUT_DEPTH_BASE",
        default: 5,
        min: 1,
        max: 32,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_QS_FUTILITY_BASE",
        default: 352,
        min: 0,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_TT_MOVE_BONUS",
        default: 811,
        min: -8192,
        max: 8192,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_TT_MOVE_MALUS",
        default: -848,
        min: -8192,
        max: 8192,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_PRIOR_CAPTURE_CM_BONUS",
        default: 964,
        min: -8192,
        max: 8192,
    },
];

impl Default for SearchTuneParams {
    fn default() -> Self {
        Self {
            iir_prior_reduction_threshold_shallow: 1,
            iir_prior_reduction_threshold_deep: 3,
            iir_depth_boundary: 10,
            iir_eval_sum_threshold: 177,
            draw_jitter_mask: 2,
            draw_jitter_offset: -1,
            lmr_reduction_delta_scale: 757,
            lmr_reduction_non_improving_mult: 218,
            lmr_reduction_non_improving_div: 512,
            lmr_reduction_base_offset: 1200,
            futility_margin_base: 91,
            futility_margin_tt_bonus: 21,
            futility_improving_scale: 2094,
            futility_opponent_worsening_scale: 1324,
            futility_correction_div: 158_105,
            small_probcut_margin: 417,
            razoring_margin_base: 514,
            razoring_margin_depth2_coeff: 294,
            nmp_margin_depth_mult: 18,
            nmp_margin_offset: -390,
            nmp_reduction_base: 7,
            nmp_reduction_depth_div: 3,
            nmp_verification_depth_threshold: 16,
            nmp_min_ply_update_num: 3,
            nmp_min_ply_update_den: 4,
            probcut_beta_margin_base: 215,
            probcut_beta_improving_sub: 60,
            probcut_dynamic_reduction_div: 300,
            probcut_depth_base: 5,
            qsearch_futility_base: 352,
            tt_move_history_bonus: 811,
            tt_move_history_malus: -848,
            prior_capture_countermove_bonus: 964,
        }
    }
}

impl SearchTuneParams {
    /// SPSA向けに公開する USI option 定義を返す。
    pub fn option_specs() -> &'static [SearchTuneOptionSpec] {
        SPSA_OPTION_SPECS
    }

    /// USI option 名と値を受け取り、対応する項目を更新する。
    ///
    /// 不明な option 名の場合は `None` を返す。
    pub fn set_from_usi_name(&mut self, name: &str, value: i32) -> Option<SearchTuneSetResult> {
        fn apply(dst: &mut i32, value: i32, min: i32, max: i32) -> SearchTuneSetResult {
            let applied = value.clamp(min, max);
            *dst = applied;
            SearchTuneSetResult {
                applied,
                clamped: applied != value,
                min,
                max,
            }
        }

        match name {
            "SPSA_IIR_SHALLOW" => {
                Some(apply(&mut self.iir_prior_reduction_threshold_shallow, value, 0, 8))
            }
            "SPSA_IIR_DEEP" => {
                Some(apply(&mut self.iir_prior_reduction_threshold_deep, value, 0, 16))
            }
            "SPSA_IIR_DEPTH_BOUNDARY" => Some(apply(&mut self.iir_depth_boundary, value, 1, 64)),
            "SPSA_IIR_EVAL_SUM" => Some(apply(&mut self.iir_eval_sum_threshold, value, 0, 5000)),
            "SPSA_DRAW_JITTER_MASK" => Some(apply(&mut self.draw_jitter_mask, value, 0, 31)),
            "SPSA_DRAW_JITTER_OFFSET" => Some(apply(&mut self.draw_jitter_offset, value, -16, 16)),
            "SPSA_LMR_DELTA_SCALE" => {
                Some(apply(&mut self.lmr_reduction_delta_scale, value, 0, 4096))
            }
            "SPSA_LMR_NON_IMPROVING_MULT" => {
                Some(apply(&mut self.lmr_reduction_non_improving_mult, value, 0, 4096))
            }
            "SPSA_LMR_NON_IMPROVING_DIV" => {
                Some(apply(&mut self.lmr_reduction_non_improving_div, value, 1, 4096))
            }
            "SPSA_LMR_BASE_OFFSET" => {
                Some(apply(&mut self.lmr_reduction_base_offset, value, -8192, 8192))
            }
            "SPSA_FUTILITY_MARGIN_BASE" => {
                Some(apply(&mut self.futility_margin_base, value, 0, 1024))
            }
            "SPSA_FUTILITY_MARGIN_TT_BONUS" => {
                Some(apply(&mut self.futility_margin_tt_bonus, value, 0, 512))
            }
            "SPSA_FUTILITY_IMPROVING_SCALE" => {
                Some(apply(&mut self.futility_improving_scale, value, 0, 4096))
            }
            "SPSA_FUTILITY_OPP_WORSENING_SCALE" => {
                Some(apply(&mut self.futility_opponent_worsening_scale, value, 0, 4096))
            }
            "SPSA_FUTILITY_CORRECTION_DIV" => {
                Some(apply(&mut self.futility_correction_div, value, 1, 1_000_000))
            }
            "SPSA_SMALL_PROBCUT_MARGIN" => {
                Some(apply(&mut self.small_probcut_margin, value, 0, 2048))
            }
            "SPSA_RAZORING_BASE" => Some(apply(&mut self.razoring_margin_base, value, 0, 4096)),
            "SPSA_RAZORING_DEPTH2" => {
                Some(apply(&mut self.razoring_margin_depth2_coeff, value, 0, 4096))
            }
            "SPSA_NMP_MARGIN_DEPTH_MULT" => {
                Some(apply(&mut self.nmp_margin_depth_mult, value, 0, 256))
            }
            "SPSA_NMP_MARGIN_OFFSET" => {
                Some(apply(&mut self.nmp_margin_offset, value, -4096, 4096))
            }
            "SPSA_NMP_REDUCTION_BASE" => Some(apply(&mut self.nmp_reduction_base, value, 1, 32)),
            "SPSA_NMP_REDUCTION_DEPTH_DIV" => {
                Some(apply(&mut self.nmp_reduction_depth_div, value, 1, 32))
            }
            "SPSA_NMP_VERIFICATION_DEPTH" => {
                Some(apply(&mut self.nmp_verification_depth_threshold, value, 1, 128))
            }
            "SPSA_NMP_MIN_PLY_NUM" => Some(apply(&mut self.nmp_min_ply_update_num, value, 1, 32)),
            "SPSA_NMP_MIN_PLY_DEN" => Some(apply(&mut self.nmp_min_ply_update_den, value, 1, 32)),
            "SPSA_PROBCUT_BETA_MARGIN" => {
                Some(apply(&mut self.probcut_beta_margin_base, value, 0, 2048))
            }
            "SPSA_PROBCUT_IMPROVING_SUB" => {
                Some(apply(&mut self.probcut_beta_improving_sub, value, 0, 1024))
            }
            "SPSA_PROBCUT_DYNAMIC_DIV" => {
                Some(apply(&mut self.probcut_dynamic_reduction_div, value, 1, 4096))
            }
            "SPSA_PROBCUT_DEPTH_BASE" => Some(apply(&mut self.probcut_depth_base, value, 1, 32)),
            "SPSA_QS_FUTILITY_BASE" => Some(apply(&mut self.qsearch_futility_base, value, 0, 4096)),
            "SPSA_TT_MOVE_BONUS" => {
                Some(apply(&mut self.tt_move_history_bonus, value, -8192, 8192))
            }
            "SPSA_TT_MOVE_MALUS" => {
                Some(apply(&mut self.tt_move_history_malus, value, -8192, 8192))
            }
            "SPSA_PRIOR_CAPTURE_CM_BONUS" => {
                Some(apply(&mut self.prior_capture_countermove_bonus, value, -8192, 8192))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_matches_specs() {
        let defaults = SearchTuneParams::default();
        for spec in SearchTuneParams::option_specs() {
            let mut params = defaults;
            let res = params
                .set_from_usi_name(spec.usi_name, spec.default)
                .expect("spec must be mappable");
            assert_eq!(res.applied, spec.default);
        }
    }

    #[test]
    fn clamp_is_reported() {
        let mut params = SearchTuneParams::default();
        let res = params.set_from_usi_name("SPSA_NMP_REDUCTION_DEPTH_DIV", 0).expect("known name");
        assert!(res.clamped);
        assert_eq!(res.applied, 1);
        assert_eq!(params.nmp_reduction_depth_div, 1);
    }

    #[test]
    fn all_specs_support_min_max_clamp() {
        let defaults = SearchTuneParams::default();
        for spec in SearchTuneParams::option_specs() {
            let mut params = defaults;
            let low = params
                .set_from_usi_name(spec.usi_name, spec.min - 1)
                .expect("spec must be mappable");
            assert_eq!(low.applied, spec.min);
            assert!(low.clamped);

            let high = params
                .set_from_usi_name(spec.usi_name, spec.max + 1)
                .expect("spec must be mappable");
            assert_eq!(high.applied, spec.max);
            assert!(high.clamped);
        }
    }
}
