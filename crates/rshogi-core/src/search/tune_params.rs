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
    /// LMR Step16: ttPv時の加算
    pub lmr_ttpv_add: i32,
    /// LMR Step16: ttPv時減算のベース値
    pub lmr_step16_ttpv_sub_base: i32,
    /// LMR Step16: ttPv時減算の pv_node 係数
    pub lmr_step16_ttpv_sub_pv_node: i32,
    /// LMR Step16: ttPv時減算の tt_value_higher 係数
    pub lmr_step16_ttpv_sub_tt_value: i32,
    /// LMR Step16: ttPv時減算の tt_depth_ge ベース係数
    pub lmr_step16_ttpv_sub_tt_depth: i32,
    /// LMR Step16: ttPv時減算の tt_depth_ge かつ cut_node 追加係数
    pub lmr_step16_ttpv_sub_cut_node: i32,
    /// LMR Step16: 基本加算
    pub lmr_step16_base_add: i32,
    /// LMR Step16: move_count 乗算係数
    pub lmr_step16_move_count_mul: i32,
    /// LMR Step16: correction_value 補正の分母
    pub lmr_step16_correction_div: i32,
    /// LMR Step16: cut_node時の加算
    pub lmr_step16_cut_node_add: i32,
    /// LMR Step16: cut_node時 no_tt_move の追加加算
    pub lmr_step16_cut_node_no_tt_add: i32,
    /// LMR Step16: tt_capture 時の加算
    pub lmr_step16_tt_capture_add: i32,
    /// LMR Step16: cutoff_cnt>2 時の加算
    pub lmr_step16_cutoff_count_add: i32,
    /// LMR Step16: cutoff_cnt>2 かつ all_node 時の追加加算
    pub lmr_step16_cutoff_count_all_node_add: i32,
    /// LMR Step16: tt_move 一致時の減算
    pub lmr_step16_tt_move_penalty: i32,
    /// LMR Step16: capture stat の駒価値スケール分子（/128）
    pub lmr_step16_capture_stat_scale_num: i32,
    /// LMR Step16: stat_score 補正の分子（/8192）
    pub lmr_step16_stat_score_scale_num: i32,
    /// LMR再探索: deeper判定のベース値（43 + 2*depth の43）
    pub lmr_research_deeper_base: i32,
    /// LMR再探索: deeper判定の depth 係数（43 + 2*depth の2）
    pub lmr_research_deeper_depth_mul: i32,
    /// LMR再探索: shallower判定しきい値
    pub lmr_research_shallower_threshold: i32,

    /// Singular Extension: 発火判定の深さベース
    pub singular_min_depth_base: i32,
    /// Singular Extension: 発火判定の ttPv 加算係数
    pub singular_min_depth_tt_pv_add: i32,
    /// Singular Extension: TT depth 条件の緩和量（`tt_depth >= depth - x` の x）
    pub singular_tt_depth_margin: i32,
    /// Singular Extension: singular beta のベース係数
    pub singular_beta_margin_base: i32,
    /// Singular Extension: singular beta の `ttPv && !pvNode` 係数
    pub singular_beta_margin_tt_pv_non_pv_add: i32,
    /// Singular Extension: singular beta の除算係数
    pub singular_beta_margin_div: i32,
    /// Singular Extension: 除外探索深さの除算係数（`new_depth / x`）
    pub singular_depth_div: i32,
    /// Singular Extension: double margin のベース項
    pub singular_double_margin_base: i32,
    /// Singular Extension: double margin の pv_node 係数
    pub singular_double_margin_pv_node: i32,
    /// Singular Extension: double margin の `!tt_capture` 係数
    pub singular_double_margin_non_tt_capture: i32,
    /// Singular Extension: correction value 補正の除算係数
    pub singular_corr_val_adj_div: i32,
    /// Singular Extension: double margin の tt_move_history 係数
    pub singular_double_margin_tt_move_hist_mult: i32,
    /// Singular Extension: double margin の tt_move_history 除算係数
    pub singular_double_margin_tt_move_hist_div: i32,
    /// Singular Extension: double margin の late ply 減点
    pub singular_double_margin_late_ply_penalty: i32,
    /// Singular Extension: triple margin のベース項
    pub singular_triple_margin_base: i32,
    /// Singular Extension: triple margin の pv_node 係数
    pub singular_triple_margin_pv_node: i32,
    /// Singular Extension: triple margin の `!tt_capture` 係数
    pub singular_triple_margin_non_tt_capture: i32,
    /// Singular Extension: triple margin の tt_pv 係数
    pub singular_triple_margin_tt_pv: i32,
    /// Singular Extension: triple margin の late ply 減点
    pub singular_triple_margin_late_ply_penalty: i32,
    /// Singular Extension: `tt_value >= beta` 時の負延長量
    pub singular_negative_extension_tt_fail_high: i32,
    /// Singular Extension: cut node 時の負延長量
    pub singular_negative_extension_cut_node: i32,

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

    /// stat bonus: depth 係数（121）
    pub stat_bonus_depth_mult: i32,
    /// stat bonus: オフセット（-77）
    pub stat_bonus_offset: i32,
    /// stat bonus: 上限値（1633）
    pub stat_bonus_max: i32,
    /// stat bonus: TT手一致時の加算（375）
    pub stat_bonus_tt_bonus: i32,
    /// stat malus: depth 係数（825）
    pub stat_malus_depth_mult: i32,
    /// stat malus: オフセット（-196）
    pub stat_malus_offset: i32,
    /// stat malus: 上限値（2159）
    pub stat_malus_max: i32,
    /// stat malus: move_count 係数（16）
    pub stat_malus_move_count_mult: i32,
    /// lowPlyHistory ボーナス倍率（/1024）
    pub low_ply_history_multiplier: i32,
    /// lowPlyHistory ボーナスオフセット
    pub low_ply_history_offset: i32,
    /// continuationHistory ボーナス倍率（/1024）
    pub continuation_history_multiplier: i32,
    /// continuationHistory 近接plyオフセット
    pub continuation_history_near_ply_offset: i32,
    /// continuationHistory更新重み（1手前）
    pub continuation_history_weight_1: i32,
    /// continuationHistory更新重み（2手前）
    pub continuation_history_weight_2: i32,
    /// continuationHistory更新重み（3手前）
    pub continuation_history_weight_3: i32,
    /// continuationHistory更新重み（4手前）
    pub continuation_history_weight_4: i32,
    /// continuationHistory更新重み（5手前）
    pub continuation_history_weight_5: i32,
    /// continuationHistory更新重み（6手前）
    pub continuation_history_weight_6: i32,
    /// fail-high後 continuationHistory 更新のベース分子（/1024）
    pub fail_high_continuation_base_num: i32,
    /// fail-high後 continuationHistory 更新の近接オフセット（1手前のみ）
    pub fail_high_continuation_near_ply_offset: i32,
    /// fail-high後 continuationHistory 更新重み（1手前）
    pub fail_high_continuation_weight_1: i32,
    /// fail-high後 continuationHistory 更新重み（2手前）
    pub fail_high_continuation_weight_2: i32,
    /// fail-high後 continuationHistory 更新重み（3手前）
    pub fail_high_continuation_weight_3: i32,
    /// fail-high後 continuationHistory 更新重み（4手前）
    pub fail_high_continuation_weight_4: i32,
    /// fail-high後 continuationHistory 更新重み（5手前）
    pub fail_high_continuation_weight_5: i32,
    /// fail-high後 continuationHistory 更新重み（6手前）
    pub fail_high_continuation_weight_6: i32,
    /// pawnHistory正ボーナス倍率（/1024）
    pub pawn_history_pos_multiplier: i32,
    /// pawnHistory負ボーナス倍率（/1024）
    pub pawn_history_neg_multiplier: i32,
    /// update_all_stats: quiet best更新のスケール分子（/1024）
    pub update_all_stats_quiet_bonus_scale_num: i32,
    /// update_all_stats: quiet malus更新のスケール分子（/1024）
    pub update_all_stats_quiet_malus_scale_num: i32,
    /// update_all_stats: capture best更新のスケール分子（/1024）
    pub update_all_stats_capture_bonus_scale_num: i32,
    /// update_all_stats: capture malus更新のスケール分子（/1024）
    pub update_all_stats_capture_malus_scale_num: i32,
    /// update_all_stats: quiet early refutation penaltyのスケール分子（/1024）
    pub update_all_stats_early_refutation_penalty_scale_num: i32,

    /// prior quiet countermove: bonusScaleベース値
    pub prior_quiet_countermove_bonus_scale_base: i32,
    /// prior quiet countermove: parent stat score 除算係数
    pub prior_quiet_countermove_parent_stat_div: i32,
    /// prior quiet countermove: depth項の乗算係数
    pub prior_quiet_countermove_depth_mul: i32,
    /// prior quiet countermove: depth項の上限値
    pub prior_quiet_countermove_depth_cap: i32,
    /// prior quiet countermove: move_count 条件成立時の加算値
    pub prior_quiet_countermove_move_count_bonus: i32,
    /// prior quiet countermove: 現在ノード static_eval 条件成立時の加算値
    pub prior_quiet_countermove_eval_bonus: i32,
    /// prior quiet countermove: 現在ノード static_eval マージン
    pub prior_quiet_countermove_eval_margin: i32,
    /// prior quiet countermove: 親ノード static_eval 条件成立時の加算値
    pub prior_quiet_countermove_parent_eval_bonus: i32,
    /// prior quiet countermove: 親ノード static_eval マージン
    pub prior_quiet_countermove_parent_eval_margin: i32,
    /// prior quiet countermove: scaled_bonus式の depth 係数
    pub prior_quiet_countermove_scaled_depth_mul: i32,
    /// prior quiet countermove: scaled_bonus式のオフセット
    pub prior_quiet_countermove_scaled_offset: i32,
    /// prior quiet countermove: scaled_bonus式の上限
    pub prior_quiet_countermove_scaled_cap: i32,
    /// prior quiet countermove: continuation 出力係数分子（/32768）
    pub prior_quiet_countermove_cont_scale_num: i32,
    /// prior quiet countermove: main 出力係数分子（/32768）
    pub prior_quiet_countermove_main_scale_num: i32,
    /// prior quiet countermove: pawn 出力係数分子（/32768）
    pub prior_quiet_countermove_pawn_scale_num: i32,

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
        usi_name: "SPSA_LMR_TTPV_ADD",
        default: 946,
        min: -8192,
        max: 8192,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_LMR_STEP16_TTPV_SUB_BASE",
        default: 2618,
        min: -8192,
        max: 8192,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_LMR_STEP16_TTPV_SUB_PV_NODE",
        default: 991,
        min: -8192,
        max: 8192,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_LMR_STEP16_TTPV_SUB_TT_VALUE",
        default: 903,
        min: -8192,
        max: 8192,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_LMR_STEP16_TTPV_SUB_TT_DEPTH",
        default: 978,
        min: -8192,
        max: 8192,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_LMR_STEP16_TTPV_SUB_CUT_NODE",
        default: 1051,
        min: -8192,
        max: 8192,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_LMR_STEP16_BASE_ADD",
        default: 843,
        min: -8192,
        max: 8192,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_LMR_STEP16_MOVE_COUNT_MUL",
        default: 66,
        min: -1024,
        max: 1024,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_LMR_STEP16_CORRECTION_DIV",
        default: 30_450,
        min: 1,
        max: 1_000_000,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_LMR_STEP16_CUT_NODE_ADD",
        default: 3094,
        min: -8192,
        max: 8192,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_LMR_STEP16_CUT_NODE_NO_TT_ADD",
        default: 1056,
        min: -8192,
        max: 8192,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_LMR_STEP16_TT_CAPTURE_ADD",
        default: 1415,
        min: -8192,
        max: 8192,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_LMR_STEP16_CUTOFF_COUNT_ADD",
        default: 1051,
        min: -8192,
        max: 8192,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_LMR_STEP16_CUTOFF_COUNT_ALL_NODE_ADD",
        default: 814,
        min: -8192,
        max: 8192,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_LMR_STEP16_TT_MOVE_PENALTY",
        default: 2018,
        min: -8192,
        max: 8192,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_LMR_STEP16_CAPTURE_STAT_SCALE_NUM",
        default: 803,
        min: 0,
        max: 8192,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_LMR_STEP16_STAT_SCORE_SCALE_NUM",
        default: 794,
        min: 0,
        max: 8192,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_LMR_RESEARCH_DEEPER_BASE",
        default: 43,
        min: -1024,
        max: 1024,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_LMR_RESEARCH_DEEPER_DEPTH_MUL",
        default: 2,
        min: -64,
        max: 64,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_LMR_RESEARCH_SHALLOWER_THRESHOLD",
        default: 9,
        min: -1024,
        max: 1024,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_SINGULAR_MIN_DEPTH_BASE",
        default: 6,
        min: 0,
        max: 64,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_SINGULAR_MIN_DEPTH_TT_PV_ADD",
        default: 1,
        min: 0,
        max: 8,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_SINGULAR_TT_DEPTH_MARGIN",
        default: 3,
        min: 0,
        max: 16,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_SINGULAR_BETA_MARGIN_BASE",
        default: 56,
        min: -4096,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_SINGULAR_BETA_MARGIN_TT_PV_NON_PV_ADD",
        default: 81,
        min: -4096,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_SINGULAR_BETA_MARGIN_DIV",
        default: 60,
        min: 1,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_SINGULAR_DEPTH_DIV",
        default: 2,
        min: 1,
        max: 16,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_SINGULAR_DOUBLE_MARGIN_BASE",
        default: -4,
        min: -4096,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_SINGULAR_DOUBLE_MARGIN_PV_NODE",
        default: 198,
        min: -4096,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_SINGULAR_DOUBLE_MARGIN_NON_TT_CAPTURE",
        default: -212,
        min: -4096,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_SINGULAR_CORR_VAL_ADJ_DIV",
        default: 229_958,
        min: 1,
        max: 1_000_000,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_SINGULAR_DOUBLE_MARGIN_TT_MOVE_HIST_MULT",
        default: -921,
        min: -4096,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_SINGULAR_DOUBLE_MARGIN_TT_MOVE_HIST_DIV",
        default: 127_649,
        min: 1,
        max: 1_000_000,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_SINGULAR_DOUBLE_MARGIN_LATE_PLY_PENALTY",
        default: 45,
        min: -4096,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_SINGULAR_TRIPLE_MARGIN_BASE",
        default: 76,
        min: -4096,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_SINGULAR_TRIPLE_MARGIN_PV_NODE",
        default: 308,
        min: -4096,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_SINGULAR_TRIPLE_MARGIN_NON_TT_CAPTURE",
        default: -250,
        min: -4096,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_SINGULAR_TRIPLE_MARGIN_TT_PV",
        default: 92,
        min: -4096,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_SINGULAR_TRIPLE_MARGIN_LATE_PLY_PENALTY",
        default: 52,
        min: -4096,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_SINGULAR_NEGATIVE_EXTENSION_TT_FAIL_HIGH",
        default: -3,
        min: -8,
        max: 0,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_SINGULAR_NEGATIVE_EXTENSION_CUT_NODE",
        default: -2,
        min: -8,
        max: 0,
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
        default: 418,
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
        default: 224,
        min: 0,
        max: 2048,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_PROBCUT_IMPROVING_SUB",
        default: 64,
        min: 0,
        max: 1024,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_PROBCUT_DYNAMIC_DIV",
        default: 306,
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
        usi_name: "SPSA_STAT_BONUS_DEPTH_MULT",
        default: 121,
        min: 0,
        max: 2048,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_STAT_BONUS_OFFSET",
        default: -77,
        min: -4096,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_STAT_BONUS_MAX",
        default: 1633,
        min: 1,
        max: 8192,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_STAT_BONUS_TT_BONUS",
        default: 375,
        min: -4096,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_STAT_MALUS_DEPTH_MULT",
        default: 825,
        min: 0,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_STAT_MALUS_OFFSET",
        default: -196,
        min: -4096,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_STAT_MALUS_MAX",
        default: 2159,
        min: 1,
        max: 8192,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_STAT_MALUS_MOVE_COUNT_MULT",
        default: 16,
        min: 0,
        max: 512,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_LOW_PLY_HISTORY_MULTIPLIER",
        default: 761,
        min: 0,
        max: 2048,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_LOW_PLY_HISTORY_OFFSET",
        default: 0,
        min: -2048,
        max: 2048,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_CONT_HISTORY_MULTIPLIER",
        default: 955,
        min: 0,
        max: 2048,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_CONT_HISTORY_NEAR_PLY_OFFSET",
        default: 88,
        min: -1024,
        max: 1024,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_CONT_HISTORY_WEIGHT_1",
        default: 1157,
        min: -4096,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_CONT_HISTORY_WEIGHT_2",
        default: 648,
        min: -4096,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_CONT_HISTORY_WEIGHT_3",
        default: 288,
        min: -4096,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_CONT_HISTORY_WEIGHT_4",
        default: 576,
        min: -4096,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_CONT_HISTORY_WEIGHT_5",
        default: 140,
        min: -4096,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_CONT_HISTORY_WEIGHT_6",
        default: 441,
        min: -4096,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_FAIL_HIGH_CONT_BASE_NUM",
        default: 1412,
        min: -4096,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_FAIL_HIGH_CONT_NEAR_PLY_OFFSET",
        default: 80,
        min: -1024,
        max: 1024,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_FAIL_HIGH_CONT_WEIGHT_1",
        default: 1108,
        min: -4096,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_FAIL_HIGH_CONT_WEIGHT_2",
        default: 652,
        min: -4096,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_FAIL_HIGH_CONT_WEIGHT_3",
        default: 273,
        min: -4096,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_FAIL_HIGH_CONT_WEIGHT_4",
        default: 572,
        min: -4096,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_FAIL_HIGH_CONT_WEIGHT_5",
        default: 126,
        min: -4096,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_FAIL_HIGH_CONT_WEIGHT_6",
        default: 449,
        min: -4096,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_PAWN_HISTORY_POS_MULTIPLIER",
        default: 850,
        min: 0,
        max: 2048,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_PAWN_HISTORY_NEG_MULTIPLIER",
        default: 550,
        min: 0,
        max: 2048,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_UPDATE_ALL_QUIET_BONUS_SCALE_NUM",
        default: 881,
        min: 0,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_UPDATE_ALL_QUIET_MALUS_SCALE_NUM",
        default: 1083,
        min: 0,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_UPDATE_ALL_CAPTURE_BONUS_SCALE_NUM",
        default: 1482,
        min: 0,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_UPDATE_ALL_CAPTURE_MALUS_SCALE_NUM",
        default: 1397,
        min: 0,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_UPDATE_ALL_EARLY_REFUTE_PENALTY_SCALE_NUM",
        default: 614,
        min: 0,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_PRIOR_QUIET_CM_BONUS_SCALE_BASE",
        default: -228,
        min: -4096,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_PRIOR_QUIET_CM_PARENT_STAT_DIV",
        default: 104,
        min: 1,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_PRIOR_QUIET_CM_DEPTH_MUL",
        default: 63,
        min: -1024,
        max: 1024,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_PRIOR_QUIET_CM_DEPTH_CAP",
        default: 508,
        min: 0,
        max: 8192,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_PRIOR_QUIET_CM_MOVE_COUNT_BONUS",
        default: 184,
        min: -4096,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_PRIOR_QUIET_CM_EVAL_BONUS",
        default: 143,
        min: -4096,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_PRIOR_QUIET_CM_EVAL_MARGIN",
        default: 92,
        min: 0,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_PRIOR_QUIET_CM_PARENT_EVAL_BONUS",
        default: 149,
        min: -4096,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_PRIOR_QUIET_CM_PARENT_EVAL_MARGIN",
        default: 70,
        min: 0,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_PRIOR_QUIET_CM_SCALED_DEPTH_MUL",
        default: 144,
        min: -4096,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_PRIOR_QUIET_CM_SCALED_OFFSET",
        default: -92,
        min: -4096,
        max: 4096,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_PRIOR_QUIET_CM_SCALED_CAP",
        default: 1365,
        min: 0,
        max: 32768,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_PRIOR_QUIET_CM_CONT_SCALE_NUM",
        default: 400,
        min: -32768,
        max: 32768,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_PRIOR_QUIET_CM_MAIN_SCALE_NUM",
        default: 220,
        min: -32768,
        max: 32768,
    },
    SearchTuneOptionSpec {
        usi_name: "SPSA_PRIOR_QUIET_CM_PAWN_SCALE_NUM",
        default: 1164,
        min: -32768,
        max: 32768,
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
            lmr_ttpv_add: 946,
            lmr_step16_ttpv_sub_base: 2618,
            lmr_step16_ttpv_sub_pv_node: 991,
            lmr_step16_ttpv_sub_tt_value: 903,
            lmr_step16_ttpv_sub_tt_depth: 978,
            lmr_step16_ttpv_sub_cut_node: 1051,
            lmr_step16_base_add: 843,
            lmr_step16_move_count_mul: 66,
            lmr_step16_correction_div: 30_450,
            lmr_step16_cut_node_add: 3094,
            lmr_step16_cut_node_no_tt_add: 1056,
            lmr_step16_tt_capture_add: 1415,
            lmr_step16_cutoff_count_add: 1051,
            lmr_step16_cutoff_count_all_node_add: 814,
            lmr_step16_tt_move_penalty: 2018,
            lmr_step16_capture_stat_scale_num: 803,
            lmr_step16_stat_score_scale_num: 794,
            lmr_research_deeper_base: 43,
            lmr_research_deeper_depth_mul: 2,
            lmr_research_shallower_threshold: 9,
            singular_min_depth_base: 6,
            singular_min_depth_tt_pv_add: 1,
            singular_tt_depth_margin: 3,
            singular_beta_margin_base: 56,
            singular_beta_margin_tt_pv_non_pv_add: 81,
            singular_beta_margin_div: 60,
            singular_depth_div: 2,
            singular_double_margin_base: -4,
            singular_double_margin_pv_node: 198,
            singular_double_margin_non_tt_capture: -212,
            singular_corr_val_adj_div: 229_958,
            singular_double_margin_tt_move_hist_mult: -921,
            singular_double_margin_tt_move_hist_div: 127_649,
            singular_double_margin_late_ply_penalty: 45,
            singular_triple_margin_base: 76,
            singular_triple_margin_pv_node: 308,
            singular_triple_margin_non_tt_capture: -250,
            singular_triple_margin_tt_pv: 92,
            singular_triple_margin_late_ply_penalty: 52,
            singular_negative_extension_tt_fail_high: -3,
            singular_negative_extension_cut_node: -2,
            futility_margin_base: 91,
            futility_margin_tt_bonus: 21,
            futility_improving_scale: 2094,
            futility_opponent_worsening_scale: 1324,
            futility_correction_div: 158_105,
            small_probcut_margin: 418,
            razoring_margin_base: 514,
            razoring_margin_depth2_coeff: 294,
            nmp_margin_depth_mult: 18,
            nmp_margin_offset: -390,
            nmp_reduction_base: 7,
            nmp_reduction_depth_div: 3,
            nmp_verification_depth_threshold: 16,
            nmp_min_ply_update_num: 3,
            nmp_min_ply_update_den: 4,
            probcut_beta_margin_base: 224,
            probcut_beta_improving_sub: 64,
            probcut_dynamic_reduction_div: 306,
            probcut_depth_base: 5,
            qsearch_futility_base: 352,
            stat_bonus_depth_mult: 121,
            stat_bonus_offset: -77,
            stat_bonus_max: 1633,
            stat_bonus_tt_bonus: 375,
            stat_malus_depth_mult: 825,
            stat_malus_offset: -196,
            stat_malus_max: 2159,
            stat_malus_move_count_mult: 16,
            low_ply_history_multiplier: 761,
            low_ply_history_offset: 0,
            continuation_history_multiplier: 955,
            continuation_history_near_ply_offset: 88,
            continuation_history_weight_1: 1157,
            continuation_history_weight_2: 648,
            continuation_history_weight_3: 288,
            continuation_history_weight_4: 576,
            continuation_history_weight_5: 140,
            continuation_history_weight_6: 441,
            fail_high_continuation_base_num: 1412,
            fail_high_continuation_near_ply_offset: 80,
            fail_high_continuation_weight_1: 1108,
            fail_high_continuation_weight_2: 652,
            fail_high_continuation_weight_3: 273,
            fail_high_continuation_weight_4: 572,
            fail_high_continuation_weight_5: 126,
            fail_high_continuation_weight_6: 449,
            pawn_history_pos_multiplier: 850,
            pawn_history_neg_multiplier: 550,
            update_all_stats_quiet_bonus_scale_num: 881,
            update_all_stats_quiet_malus_scale_num: 1083,
            update_all_stats_capture_bonus_scale_num: 1482,
            update_all_stats_capture_malus_scale_num: 1397,
            update_all_stats_early_refutation_penalty_scale_num: 614,
            prior_quiet_countermove_bonus_scale_base: -228,
            prior_quiet_countermove_parent_stat_div: 104,
            prior_quiet_countermove_depth_mul: 63,
            prior_quiet_countermove_depth_cap: 508,
            prior_quiet_countermove_move_count_bonus: 184,
            prior_quiet_countermove_eval_bonus: 143,
            prior_quiet_countermove_eval_margin: 92,
            prior_quiet_countermove_parent_eval_bonus: 149,
            prior_quiet_countermove_parent_eval_margin: 70,
            prior_quiet_countermove_scaled_depth_mul: 144,
            prior_quiet_countermove_scaled_offset: -92,
            prior_quiet_countermove_scaled_cap: 1365,
            prior_quiet_countermove_cont_scale_num: 400,
            prior_quiet_countermove_main_scale_num: 220,
            prior_quiet_countermove_pawn_scale_num: 1164,
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
        macro_rules! try_apply {
            ($name:literal, $field:ident, $min:expr, $max:expr) => {
                if name == $name {
                    return Some(apply(&mut self.$field, value, $min, $max));
                }
            };
        }

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

        try_apply!("SPSA_IIR_SHALLOW", iir_prior_reduction_threshold_shallow, 0, 8);
        try_apply!("SPSA_IIR_DEEP", iir_prior_reduction_threshold_deep, 0, 16);
        try_apply!("SPSA_IIR_DEPTH_BOUNDARY", iir_depth_boundary, 1, 64);
        try_apply!("SPSA_IIR_EVAL_SUM", iir_eval_sum_threshold, 0, 5000);
        try_apply!("SPSA_DRAW_JITTER_MASK", draw_jitter_mask, 0, 31);
        try_apply!("SPSA_DRAW_JITTER_OFFSET", draw_jitter_offset, -16, 16);
        try_apply!("SPSA_LMR_DELTA_SCALE", lmr_reduction_delta_scale, 0, 4096);
        try_apply!("SPSA_LMR_NON_IMPROVING_MULT", lmr_reduction_non_improving_mult, 0, 4096);
        try_apply!("SPSA_LMR_NON_IMPROVING_DIV", lmr_reduction_non_improving_div, 1, 4096);
        try_apply!("SPSA_LMR_BASE_OFFSET", lmr_reduction_base_offset, -8192, 8192);
        try_apply!("SPSA_LMR_TTPV_ADD", lmr_ttpv_add, -8192, 8192);
        try_apply!("SPSA_LMR_STEP16_TTPV_SUB_BASE", lmr_step16_ttpv_sub_base, -8192, 8192);
        try_apply!("SPSA_LMR_STEP16_TTPV_SUB_PV_NODE", lmr_step16_ttpv_sub_pv_node, -8192, 8192);
        try_apply!("SPSA_LMR_STEP16_TTPV_SUB_TT_VALUE", lmr_step16_ttpv_sub_tt_value, -8192, 8192);
        try_apply!("SPSA_LMR_STEP16_TTPV_SUB_TT_DEPTH", lmr_step16_ttpv_sub_tt_depth, -8192, 8192);
        try_apply!("SPSA_LMR_STEP16_TTPV_SUB_CUT_NODE", lmr_step16_ttpv_sub_cut_node, -8192, 8192);
        try_apply!("SPSA_LMR_STEP16_BASE_ADD", lmr_step16_base_add, -8192, 8192);
        try_apply!("SPSA_LMR_STEP16_MOVE_COUNT_MUL", lmr_step16_move_count_mul, -1024, 1024);
        try_apply!("SPSA_LMR_STEP16_CORRECTION_DIV", lmr_step16_correction_div, 1, 1_000_000);
        try_apply!("SPSA_LMR_STEP16_CUT_NODE_ADD", lmr_step16_cut_node_add, -8192, 8192);
        try_apply!(
            "SPSA_LMR_STEP16_CUT_NODE_NO_TT_ADD",
            lmr_step16_cut_node_no_tt_add,
            -8192,
            8192
        );
        try_apply!("SPSA_LMR_STEP16_TT_CAPTURE_ADD", lmr_step16_tt_capture_add, -8192, 8192);
        try_apply!("SPSA_LMR_STEP16_CUTOFF_COUNT_ADD", lmr_step16_cutoff_count_add, -8192, 8192);
        try_apply!(
            "SPSA_LMR_STEP16_CUTOFF_COUNT_ALL_NODE_ADD",
            lmr_step16_cutoff_count_all_node_add,
            -8192,
            8192
        );
        try_apply!("SPSA_LMR_STEP16_TT_MOVE_PENALTY", lmr_step16_tt_move_penalty, -8192, 8192);
        try_apply!(
            "SPSA_LMR_STEP16_CAPTURE_STAT_SCALE_NUM",
            lmr_step16_capture_stat_scale_num,
            0,
            8192
        );
        try_apply!(
            "SPSA_LMR_STEP16_STAT_SCORE_SCALE_NUM",
            lmr_step16_stat_score_scale_num,
            0,
            8192
        );
        try_apply!("SPSA_LMR_RESEARCH_DEEPER_BASE", lmr_research_deeper_base, -1024, 1024);
        try_apply!("SPSA_LMR_RESEARCH_DEEPER_DEPTH_MUL", lmr_research_deeper_depth_mul, -64, 64);
        try_apply!(
            "SPSA_LMR_RESEARCH_SHALLOWER_THRESHOLD",
            lmr_research_shallower_threshold,
            -1024,
            1024
        );
        try_apply!("SPSA_SINGULAR_MIN_DEPTH_BASE", singular_min_depth_base, 0, 64);
        try_apply!("SPSA_SINGULAR_MIN_DEPTH_TT_PV_ADD", singular_min_depth_tt_pv_add, 0, 8);
        try_apply!("SPSA_SINGULAR_TT_DEPTH_MARGIN", singular_tt_depth_margin, 0, 16);
        try_apply!("SPSA_SINGULAR_BETA_MARGIN_BASE", singular_beta_margin_base, -4096, 4096);
        try_apply!(
            "SPSA_SINGULAR_BETA_MARGIN_TT_PV_NON_PV_ADD",
            singular_beta_margin_tt_pv_non_pv_add,
            -4096,
            4096
        );
        try_apply!("SPSA_SINGULAR_BETA_MARGIN_DIV", singular_beta_margin_div, 1, 4096);
        try_apply!("SPSA_SINGULAR_DEPTH_DIV", singular_depth_div, 1, 16);
        try_apply!("SPSA_SINGULAR_DOUBLE_MARGIN_BASE", singular_double_margin_base, -4096, 4096);
        try_apply!(
            "SPSA_SINGULAR_DOUBLE_MARGIN_PV_NODE",
            singular_double_margin_pv_node,
            -4096,
            4096
        );
        try_apply!(
            "SPSA_SINGULAR_DOUBLE_MARGIN_NON_TT_CAPTURE",
            singular_double_margin_non_tt_capture,
            -4096,
            4096
        );
        try_apply!("SPSA_SINGULAR_CORR_VAL_ADJ_DIV", singular_corr_val_adj_div, 1, 1_000_000);
        try_apply!(
            "SPSA_SINGULAR_DOUBLE_MARGIN_TT_MOVE_HIST_MULT",
            singular_double_margin_tt_move_hist_mult,
            -4096,
            4096
        );
        try_apply!(
            "SPSA_SINGULAR_DOUBLE_MARGIN_TT_MOVE_HIST_DIV",
            singular_double_margin_tt_move_hist_div,
            1,
            1_000_000
        );
        try_apply!(
            "SPSA_SINGULAR_DOUBLE_MARGIN_LATE_PLY_PENALTY",
            singular_double_margin_late_ply_penalty,
            -4096,
            4096
        );
        try_apply!("SPSA_SINGULAR_TRIPLE_MARGIN_BASE", singular_triple_margin_base, -4096, 4096);
        try_apply!(
            "SPSA_SINGULAR_TRIPLE_MARGIN_PV_NODE",
            singular_triple_margin_pv_node,
            -4096,
            4096
        );
        try_apply!(
            "SPSA_SINGULAR_TRIPLE_MARGIN_NON_TT_CAPTURE",
            singular_triple_margin_non_tt_capture,
            -4096,
            4096
        );
        try_apply!("SPSA_SINGULAR_TRIPLE_MARGIN_TT_PV", singular_triple_margin_tt_pv, -4096, 4096);
        try_apply!(
            "SPSA_SINGULAR_TRIPLE_MARGIN_LATE_PLY_PENALTY",
            singular_triple_margin_late_ply_penalty,
            -4096,
            4096
        );
        try_apply!(
            "SPSA_SINGULAR_NEGATIVE_EXTENSION_TT_FAIL_HIGH",
            singular_negative_extension_tt_fail_high,
            -8,
            0
        );
        try_apply!(
            "SPSA_SINGULAR_NEGATIVE_EXTENSION_CUT_NODE",
            singular_negative_extension_cut_node,
            -8,
            0
        );
        try_apply!("SPSA_FUTILITY_MARGIN_BASE", futility_margin_base, 0, 1024);
        try_apply!("SPSA_FUTILITY_MARGIN_TT_BONUS", futility_margin_tt_bonus, 0, 512);
        try_apply!("SPSA_FUTILITY_IMPROVING_SCALE", futility_improving_scale, 0, 4096);
        try_apply!("SPSA_FUTILITY_OPP_WORSENING_SCALE", futility_opponent_worsening_scale, 0, 4096);
        try_apply!("SPSA_FUTILITY_CORRECTION_DIV", futility_correction_div, 1, 1_000_000);
        try_apply!("SPSA_SMALL_PROBCUT_MARGIN", small_probcut_margin, 0, 2048);
        try_apply!("SPSA_RAZORING_BASE", razoring_margin_base, 0, 4096);
        try_apply!("SPSA_RAZORING_DEPTH2", razoring_margin_depth2_coeff, 0, 4096);
        try_apply!("SPSA_NMP_MARGIN_DEPTH_MULT", nmp_margin_depth_mult, 0, 256);
        try_apply!("SPSA_NMP_MARGIN_OFFSET", nmp_margin_offset, -4096, 4096);
        try_apply!("SPSA_NMP_REDUCTION_BASE", nmp_reduction_base, 1, 32);
        try_apply!("SPSA_NMP_REDUCTION_DEPTH_DIV", nmp_reduction_depth_div, 1, 32);
        try_apply!("SPSA_NMP_VERIFICATION_DEPTH", nmp_verification_depth_threshold, 1, 128);
        try_apply!("SPSA_NMP_MIN_PLY_NUM", nmp_min_ply_update_num, 1, 32);
        try_apply!("SPSA_NMP_MIN_PLY_DEN", nmp_min_ply_update_den, 1, 32);
        try_apply!("SPSA_PROBCUT_BETA_MARGIN", probcut_beta_margin_base, 0, 2048);
        try_apply!("SPSA_PROBCUT_IMPROVING_SUB", probcut_beta_improving_sub, 0, 1024);
        try_apply!("SPSA_PROBCUT_DYNAMIC_DIV", probcut_dynamic_reduction_div, 1, 4096);
        try_apply!("SPSA_PROBCUT_DEPTH_BASE", probcut_depth_base, 1, 32);
        try_apply!("SPSA_QS_FUTILITY_BASE", qsearch_futility_base, 0, 4096);
        try_apply!("SPSA_STAT_BONUS_DEPTH_MULT", stat_bonus_depth_mult, 0, 2048);
        try_apply!("SPSA_STAT_BONUS_OFFSET", stat_bonus_offset, -4096, 4096);
        try_apply!("SPSA_STAT_BONUS_MAX", stat_bonus_max, 1, 8192);
        try_apply!("SPSA_STAT_BONUS_TT_BONUS", stat_bonus_tt_bonus, -4096, 4096);
        try_apply!("SPSA_STAT_MALUS_DEPTH_MULT", stat_malus_depth_mult, 0, 4096);
        try_apply!("SPSA_STAT_MALUS_OFFSET", stat_malus_offset, -4096, 4096);
        try_apply!("SPSA_STAT_MALUS_MAX", stat_malus_max, 1, 8192);
        try_apply!("SPSA_STAT_MALUS_MOVE_COUNT_MULT", stat_malus_move_count_mult, 0, 512);
        try_apply!("SPSA_LOW_PLY_HISTORY_MULTIPLIER", low_ply_history_multiplier, 0, 2048);
        try_apply!("SPSA_LOW_PLY_HISTORY_OFFSET", low_ply_history_offset, -2048, 2048);
        try_apply!("SPSA_CONT_HISTORY_MULTIPLIER", continuation_history_multiplier, 0, 2048);
        try_apply!(
            "SPSA_CONT_HISTORY_NEAR_PLY_OFFSET",
            continuation_history_near_ply_offset,
            -1024,
            1024
        );
        try_apply!("SPSA_CONT_HISTORY_WEIGHT_1", continuation_history_weight_1, -4096, 4096);
        try_apply!("SPSA_CONT_HISTORY_WEIGHT_2", continuation_history_weight_2, -4096, 4096);
        try_apply!("SPSA_CONT_HISTORY_WEIGHT_3", continuation_history_weight_3, -4096, 4096);
        try_apply!("SPSA_CONT_HISTORY_WEIGHT_4", continuation_history_weight_4, -4096, 4096);
        try_apply!("SPSA_CONT_HISTORY_WEIGHT_5", continuation_history_weight_5, -4096, 4096);
        try_apply!("SPSA_CONT_HISTORY_WEIGHT_6", continuation_history_weight_6, -4096, 4096);
        try_apply!("SPSA_FAIL_HIGH_CONT_BASE_NUM", fail_high_continuation_base_num, -4096, 4096);
        try_apply!(
            "SPSA_FAIL_HIGH_CONT_NEAR_PLY_OFFSET",
            fail_high_continuation_near_ply_offset,
            -1024,
            1024
        );
        try_apply!("SPSA_FAIL_HIGH_CONT_WEIGHT_1", fail_high_continuation_weight_1, -4096, 4096);
        try_apply!("SPSA_FAIL_HIGH_CONT_WEIGHT_2", fail_high_continuation_weight_2, -4096, 4096);
        try_apply!("SPSA_FAIL_HIGH_CONT_WEIGHT_3", fail_high_continuation_weight_3, -4096, 4096);
        try_apply!("SPSA_FAIL_HIGH_CONT_WEIGHT_4", fail_high_continuation_weight_4, -4096, 4096);
        try_apply!("SPSA_FAIL_HIGH_CONT_WEIGHT_5", fail_high_continuation_weight_5, -4096, 4096);
        try_apply!("SPSA_FAIL_HIGH_CONT_WEIGHT_6", fail_high_continuation_weight_6, -4096, 4096);
        try_apply!("SPSA_PAWN_HISTORY_POS_MULTIPLIER", pawn_history_pos_multiplier, 0, 2048);
        try_apply!("SPSA_PAWN_HISTORY_NEG_MULTIPLIER", pawn_history_neg_multiplier, 0, 2048);
        try_apply!(
            "SPSA_UPDATE_ALL_QUIET_BONUS_SCALE_NUM",
            update_all_stats_quiet_bonus_scale_num,
            0,
            4096
        );
        try_apply!(
            "SPSA_UPDATE_ALL_QUIET_MALUS_SCALE_NUM",
            update_all_stats_quiet_malus_scale_num,
            0,
            4096
        );
        try_apply!(
            "SPSA_UPDATE_ALL_CAPTURE_BONUS_SCALE_NUM",
            update_all_stats_capture_bonus_scale_num,
            0,
            4096
        );
        try_apply!(
            "SPSA_UPDATE_ALL_CAPTURE_MALUS_SCALE_NUM",
            update_all_stats_capture_malus_scale_num,
            0,
            4096
        );
        try_apply!(
            "SPSA_UPDATE_ALL_EARLY_REFUTE_PENALTY_SCALE_NUM",
            update_all_stats_early_refutation_penalty_scale_num,
            0,
            4096
        );
        try_apply!(
            "SPSA_PRIOR_QUIET_CM_BONUS_SCALE_BASE",
            prior_quiet_countermove_bonus_scale_base,
            -4096,
            4096
        );
        try_apply!(
            "SPSA_PRIOR_QUIET_CM_PARENT_STAT_DIV",
            prior_quiet_countermove_parent_stat_div,
            1,
            4096
        );
        try_apply!("SPSA_PRIOR_QUIET_CM_DEPTH_MUL", prior_quiet_countermove_depth_mul, -1024, 1024);
        try_apply!("SPSA_PRIOR_QUIET_CM_DEPTH_CAP", prior_quiet_countermove_depth_cap, 0, 8192);
        try_apply!(
            "SPSA_PRIOR_QUIET_CM_MOVE_COUNT_BONUS",
            prior_quiet_countermove_move_count_bonus,
            -4096,
            4096
        );
        try_apply!(
            "SPSA_PRIOR_QUIET_CM_EVAL_BONUS",
            prior_quiet_countermove_eval_bonus,
            -4096,
            4096
        );
        try_apply!("SPSA_PRIOR_QUIET_CM_EVAL_MARGIN", prior_quiet_countermove_eval_margin, 0, 4096);
        try_apply!(
            "SPSA_PRIOR_QUIET_CM_PARENT_EVAL_BONUS",
            prior_quiet_countermove_parent_eval_bonus,
            -4096,
            4096
        );
        try_apply!(
            "SPSA_PRIOR_QUIET_CM_PARENT_EVAL_MARGIN",
            prior_quiet_countermove_parent_eval_margin,
            0,
            4096
        );
        try_apply!(
            "SPSA_PRIOR_QUIET_CM_SCALED_DEPTH_MUL",
            prior_quiet_countermove_scaled_depth_mul,
            -4096,
            4096
        );
        try_apply!(
            "SPSA_PRIOR_QUIET_CM_SCALED_OFFSET",
            prior_quiet_countermove_scaled_offset,
            -4096,
            4096
        );
        try_apply!("SPSA_PRIOR_QUIET_CM_SCALED_CAP", prior_quiet_countermove_scaled_cap, 0, 32768);
        try_apply!(
            "SPSA_PRIOR_QUIET_CM_CONT_SCALE_NUM",
            prior_quiet_countermove_cont_scale_num,
            -32768,
            32768
        );
        try_apply!(
            "SPSA_PRIOR_QUIET_CM_MAIN_SCALE_NUM",
            prior_quiet_countermove_main_scale_num,
            -32768,
            32768
        );
        try_apply!(
            "SPSA_PRIOR_QUIET_CM_PAWN_SCALE_NUM",
            prior_quiet_countermove_pawn_scale_num,
            -32768,
            32768
        );
        try_apply!("SPSA_TT_MOVE_BONUS", tt_move_history_bonus, -8192, 8192);
        try_apply!("SPSA_TT_MOVE_MALUS", tt_move_history_malus, -8192, 8192);
        try_apply!("SPSA_PRIOR_CAPTURE_CM_BONUS", prior_capture_countermove_bonus, -8192, 8192);

        None
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
