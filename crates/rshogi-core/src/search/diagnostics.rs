//! 1 thread の診断用カウンタ。探索の判断には使用しない。
use crate::{
    movegen::ExtMove,
    position::Position,
    types::{Move, PieceType},
};
use std::cell::{Cell, RefCell};
pub(crate) const QUIET_N_DEPTH_0: usize = 0;
pub(crate) const QUIET_BOARD_DEPTH_0: usize = 5;
pub(crate) const QUIET_PAWN_DROP_DEPTH_0: usize = 10;
pub(crate) const QUIET_OTHER_DROP_DEPTH_0: usize = 15;
pub(crate) const QUIET_K_DEPTH_0: usize = 20;
pub(crate) const QUIET_C_DEPTH_0: usize = 25;
pub(crate) const QUIET_INIT_DEPTH_0: usize = 30;
pub(crate) const QUIET_N_HIST_0: usize = 35;
pub(crate) const QUIET_BOARD_HIST_0: usize = 41;
pub(crate) const QUIET_PAWN_DROP_HIST_0: usize = 47;
pub(crate) const QUIET_OTHER_DROP_HIST_0: usize = 53;
pub(crate) const QUIET_HAND_TYPES_HIST_0: usize = 59;
pub(crate) const PICKER_STAGE_0: usize = 67;
pub(crate) const CAPTURE_INIT: usize = 83;
pub(crate) const QCAPTURE_INIT: usize = 85;
pub(crate) const PROBCUT_INIT: usize = 87;
pub(crate) const EVASION_INIT: usize = 89;
pub(crate) const EVASION_GENERATED: usize = 90;
pub(crate) const SEE_STEP14_CAPTURE_FALSE: usize = 91;
pub(crate) const SEE_STEP14_DOUBLE: usize = 115;
pub(crate) const SEE_QS_DOUBLE: usize = 116;
pub(crate) const SEE_ZERO_UNATTACKED: usize = 117;
pub(crate) const SEE_CAPTURE_UNDEFENDED: usize = 118;
pub(crate) const SORT_SHIFTS: usize = 119;
pub(crate) const QS_LOOP_NODES: usize = 120;
pub(crate) const QS_CAPTURE_PICKED: usize = 121;
pub(crate) const QS_AFTER_TWO_PICKED: usize = 122;
pub(crate) const QS_AFTER_TWO_SEARCHED: usize = 123;
pub(crate) const QS_ALL_CAPTURE_FUTILITY: usize = 124;
pub(crate) const MATE_SEARCH_CALLS: usize = 125;
pub(crate) const MATE_SEARCH_FOUND: usize = 126;
pub(crate) const MATE_QS_CALLS: usize = 127;
pub(crate) const MATE_QS_FOUND: usize = 128;
pub(crate) const MATE_QS_STAND_PAT: usize = 129;
pub(crate) const SEARCH_DEPTH_NONPOSITIVE: usize = 130;
pub(crate) const STEP14_HISTORY: usize = 131;
pub(crate) const STEP14_FUTILITY: usize = 132;
pub(crate) const STEP14_SEE: usize = 133;
pub(crate) const LMP_TRIGGERED: usize = 134;
pub(crate) const LMP_QUIETS_SUPPRESSED: usize = 135;
pub(crate) const DO_DROP: usize = 136;
pub(crate) const DO_QUIET: usize = 137;
pub(crate) const DO_CAPTURE: usize = 138;
pub(crate) const DO_PROMOTION: usize = 139;
pub(crate) const DO_KING: usize = 140;
pub(crate) const DO_NULL: usize = 141;
pub(crate) const DO_PASS: usize = 142;
pub(crate) const REPETITION_COMPARISONS: usize = 143;
pub(crate) const TT_PROBE: usize = 144;
pub(crate) const TT_MISS: usize = 145;
pub(crate) const TT_STORE: usize = 146;
pub(crate) const TT_HIT_SLOT_0: usize = 147;
pub(crate) const REPETITION_HIST_0: usize = 150;
pub(crate) const SEARCH_PRELOOP_TT: usize = 158;
pub(crate) const SEARCH_PRELOOP_REPETITION: usize = 159;
pub(crate) const SEARCH_PRELOOP_DECLARATION: usize = 160;
pub(crate) const SEARCH_PRELOOP_RAZORING: usize = 161;
pub(crate) const SEARCH_PRELOOP_FUTILITY: usize = 162;
pub(crate) const SEARCH_PRELOOP_NMP: usize = 163;
pub(crate) const SEARCH_PRELOOP_PROBCUT: usize = 164;
pub(crate) const SEARCH_PRELOOP_MATE1: usize = 165;
pub(crate) const SEARCH_PRELOOP_OTHER: usize = 166;
pub(crate) const NNUE_ACC_REQUESTS: usize = 167;
pub(crate) const NNUE_ACC_REUSED: usize = 168;
pub(crate) const NNUE_ACC_FALLBACK: usize = 169;
pub(crate) const NNUE_FT_FULL: usize = 170;
pub(crate) const NNUE_FT_INCREMENTAL: usize = 171;
pub(crate) const NNUE_FT_DIFF_ROWS: usize = 172;
pub(crate) const NNUE_THREAT_FULL: usize = 173;
#[cfg(feature = "nnue-threat")]
pub(crate) const NNUE_THREAT_FULL_ROWS: usize = 174;
pub(crate) const NNUE_FINNY_ROWS: usize = 175;
pub(crate) const NNUE_FT_FINNY: usize = 176;
pub(crate) const NNUE_ANCESTOR_HIST_0: usize = 177;
pub(crate) const NNUE_ANCESTOR_HIST_1: usize = 178;
#[cfg(feature = "nnue-threat")]
pub(crate) const NNUE_THREAT_DIFF_ROWS: usize = 186;
pub(crate) const NNUE_THREAT_INCREMENTAL: usize = 187;
pub(crate) const NNUE_FT_FULL_ROWS: usize = 188;
pub(crate) const NNUE_EVAL_FT_INCREMENTAL: usize = 189;
pub(crate) const NNUE_EVAL_FT_FINNY: usize = 190;
pub(crate) const NNUE_EVAL_FT_FULL: usize = 191;
pub(crate) const NNUE_EVAL_THREAT_INCREMENTAL: usize = 192;
pub(crate) const NNUE_EVAL_THREAT_FULL: usize = 193;
pub(crate) const NNUE_ANCESTOR_MISSING: usize = 194;
#[cfg(feature = "nnue-threat")]
pub(crate) const THREAT_FULL_P: usize = 195;
#[cfg(feature = "nnue-threat")]
pub(crate) const THREAT_FULL_ROWS_P: usize = 197;
#[cfg(feature = "nnue-threat")]
pub(crate) const THREAT_DIFF_P: usize = 199;
#[cfg(feature = "nnue-threat")]
pub(crate) const THREAT_DIFF_ROWS_P: usize = 201;
#[cfg(feature = "nnue-threat")]
pub(crate) const THREAT_FULL_REASON: usize = 203;
#[cfg(feature = "nnue-threat")]
pub(crate) const THREAT_FULL_SIDES: usize = 211;
#[cfg(feature = "nnue-threat")]
pub(crate) const THREAT_DISTANCE: usize = 214;
#[cfg(feature = "nnue-threat")]
pub(crate) const THREAT_TOUCHED: usize = 218;
#[cfg(feature = "nnue-threat")]
pub(crate) const THREAT_CHANGED: usize = 222;
#[cfg(feature = "nnue-threat")]
pub(crate) const THREAT_SOURCES: usize = 226;
#[cfg(feature = "nnue-threat")]
pub(crate) const THREAT_ENDPOINT_ROWS: usize = 230;
#[cfg(feature = "nnue-threat")]
pub(crate) const THREAT_RESCUED: usize = 238;
#[cfg(feature = "nnue-threat")]
pub(crate) const THREAT_RESCUED_ROWS: usize = 242;
pub(crate) const FT_CF: usize = 246;
pub(crate) const FT_CF_DELTA: usize = 267;
pub(crate) const FT_CF_EVAL: usize = 294;
pub(crate) const EVAL_TT: usize = 300;
pub(crate) const EVAL_HASH: usize = 301;
pub(crate) const EVAL_NNUE: usize = 303;
pub(crate) const EVAL_REPEAT: usize = 304;
pub(crate) const EVAL_COLLISION: usize = 305;
#[cfg(feature = "nnue-threat")]
pub(crate) const THREAT_PHYSICAL_EDGES: usize = 306;
#[cfg(feature = "nnue-threat")]
pub(crate) const THREAT_ENDPOINT_SUCCESS: usize = 310;
pub(crate) const FT_CF_SINGLE: usize = 314;
pub(crate) const FT_CF_EQUAL: usize = 317;
pub(crate) const FT_FINNY_EVALS: usize = 320;
const KEYS: [&str; 321] = [
    "quiet_n_depth[0]",
    "quiet_n_depth[1]",
    "quiet_n_depth[2]",
    "quiet_n_depth[3]",
    "quiet_n_depth[4]",
    "quiet_board_depth[0]",
    "quiet_board_depth[1]",
    "quiet_board_depth[2]",
    "quiet_board_depth[3]",
    "quiet_board_depth[4]",
    "quiet_pawn_drop_depth[0]",
    "quiet_pawn_drop_depth[1]",
    "quiet_pawn_drop_depth[2]",
    "quiet_pawn_drop_depth[3]",
    "quiet_pawn_drop_depth[4]",
    "quiet_other_drop_depth[0]",
    "quiet_other_drop_depth[1]",
    "quiet_other_drop_depth[2]",
    "quiet_other_drop_depth[3]",
    "quiet_other_drop_depth[4]",
    "quiet_k_depth[0]",
    "quiet_k_depth[1]",
    "quiet_k_depth[2]",
    "quiet_k_depth[3]",
    "quiet_k_depth[4]",
    "quiet_c_depth[0]",
    "quiet_c_depth[1]",
    "quiet_c_depth[2]",
    "quiet_c_depth[3]",
    "quiet_c_depth[4]",
    "quiet_init_depth[0]",
    "quiet_init_depth[1]",
    "quiet_init_depth[2]",
    "quiet_init_depth[3]",
    "quiet_init_depth[4]",
    "quiet_n_hist[0]",
    "quiet_n_hist[1]",
    "quiet_n_hist[2]",
    "quiet_n_hist[3]",
    "quiet_n_hist[4]",
    "quiet_n_hist[5]",
    "quiet_board_hist[0]",
    "quiet_board_hist[1]",
    "quiet_board_hist[2]",
    "quiet_board_hist[3]",
    "quiet_board_hist[4]",
    "quiet_board_hist[5]",
    "quiet_pawn_drop_hist[0]",
    "quiet_pawn_drop_hist[1]",
    "quiet_pawn_drop_hist[2]",
    "quiet_pawn_drop_hist[3]",
    "quiet_pawn_drop_hist[4]",
    "quiet_pawn_drop_hist[5]",
    "quiet_other_drop_hist[0]",
    "quiet_other_drop_hist[1]",
    "quiet_other_drop_hist[2]",
    "quiet_other_drop_hist[3]",
    "quiet_other_drop_hist[4]",
    "quiet_other_drop_hist[5]",
    "quiet_hand_types_hist[0]",
    "quiet_hand_types_hist[1]",
    "quiet_hand_types_hist[2]",
    "quiet_hand_types_hist[3]",
    "quiet_hand_types_hist[4]",
    "quiet_hand_types_hist[5]",
    "quiet_hand_types_hist[6]",
    "quiet_hand_types_hist[7]",
    "picker_stage[0]",
    "picker_stage[1]",
    "picker_stage[2]",
    "picker_stage[3]",
    "picker_stage[4]",
    "picker_stage[5]",
    "picker_stage[6]",
    "picker_stage[7]",
    "picker_stage[8]",
    "picker_stage[9]",
    "picker_stage[10]",
    "picker_stage[11]",
    "picker_stage[12]",
    "picker_stage[13]",
    "picker_stage[14]",
    "picker_stage[15]",
    "capture_init",
    "capture_generated",
    "qcapture_init",
    "qcapture_generated",
    "probcut_init",
    "probcut_generated",
    "evasion_init",
    "evasion_generated",
    "see_step14_capture_false",
    "see_step14_capture_true",
    "see_step14_capture_attackers",
    "see_step14_quiet_false",
    "see_step14_quiet_true",
    "see_step14_quiet_attackers",
    "see_check_bonus_false",
    "see_check_bonus_true",
    "see_check_bonus_attackers",
    "see_qs_futility_false",
    "see_qs_futility_true",
    "see_qs_futility_attackers",
    "see_qs_margin_false",
    "see_qs_margin_true",
    "see_qs_margin_attackers",
    "see_picker_capture_false",
    "see_picker_capture_true",
    "see_picker_capture_attackers",
    "see_probcut_false",
    "see_probcut_true",
    "see_probcut_attackers",
    "see_other_false",
    "see_other_true",
    "see_other_attackers",
    "see_step14_double",
    "see_qs_double",
    "see_zero_unattacked",
    "see_capture_undefended",
    "sort_shifts",
    "qs_loop_nodes",
    "qs_capture_picked",
    "qs_after_two_picked",
    "qs_after_two_searched",
    "qs_all_capture_futility",
    "mate_search_calls",
    "mate_search_found",
    "mate_qs_calls",
    "mate_qs_found",
    "mate_qs_stand_pat",
    "search_depth_nonpositive",
    "step14_history",
    "step14_futility",
    "step14_see",
    "lmp_triggered",
    "lmp_quiets_suppressed",
    "do_drop",
    "do_quiet",
    "do_capture",
    "do_promotion",
    "do_king",
    "do_null",
    "do_pass",
    "repetition_comparisons",
    "tt_probe",
    "tt_miss",
    "tt_store",
    "tt_hit_slot[0]",
    "tt_hit_slot[1]",
    "tt_hit_slot[2]",
    "repetition_hist[0]",
    "repetition_hist[1]",
    "repetition_hist[2]",
    "repetition_hist[3]",
    "repetition_hist[4]",
    "repetition_hist[5]",
    "repetition_hist[6]",
    "repetition_hist[7]",
    "search_preloop_tt",
    "search_preloop_repetition",
    "search_preloop_declaration",
    "search_preloop_razoring",
    "search_preloop_futility",
    "search_preloop_nmp",
    "search_preloop_probcut",
    "search_preloop_mate1",
    "search_preloop_other",
    "nnue_acc_requests",
    "nnue_acc_reused",
    "nnue_acc_fallback",
    "nnue_ft_full",
    "nnue_ft_incremental",
    "nnue_ft_diff_rows",
    "nnue_threat_full",
    "nnue_threat_full_rows",
    "nnue_finny_rows",
    "nnue_ft_finny",
    "nnue_ancestor_hist[0]",
    "nnue_ancestor_hist[1]",
    "nnue_ancestor_hist[2]",
    "nnue_ancestor_hist[3]",
    "nnue_ancestor_hist[4]",
    "nnue_ancestor_hist[5]",
    "nnue_ancestor_hist[6]",
    "nnue_ancestor_hist[7]",
    "nnue_ancestor_hist[8]",
    "nnue_threat_diff_rows",
    "nnue_threat_incremental",
    "nnue_ft_full_rows",
    "nnue_eval_ft_incremental",
    "nnue_eval_ft_finny",
    "nnue_eval_ft_full",
    "nnue_eval_threat_incremental",
    "nnue_eval_threat_full",
    "nnue_ancestor_missing",
    "threat_full_p[0]",
    "threat_full_p[1]",
    "threat_full_rows_p[0]",
    "threat_full_rows_p[1]",
    "threat_diff_p[0]",
    "threat_diff_p[1]",
    "threat_diff_rows_p[0]",
    "threat_diff_rows_p[1]",
    "threat_full_reason[0]",
    "threat_full_reason[1]",
    "threat_full_reason[2]",
    "threat_full_reason[3]",
    "threat_full_reason[4]",
    "threat_full_reason[5]",
    "threat_full_reason[6]",
    "threat_full_reason[7]",
    "threat_full_sides[0]",
    "threat_full_sides[1]",
    "threat_full_sides[2]",
    "threat_distance[0]",
    "threat_distance[1]",
    "threat_distance[2]",
    "threat_distance[3]",
    "threat_touched[0]",
    "threat_touched[1]",
    "threat_touched[2]",
    "threat_touched[3]",
    "threat_changed[0]",
    "threat_changed[1]",
    "threat_changed[2]",
    "threat_changed[3]",
    "threat_sources[0]",
    "threat_sources[1]",
    "threat_sources[2]",
    "threat_sources[3]",
    "threat_endpoint_rows[0]",
    "threat_endpoint_rows[1]",
    "threat_endpoint_rows[2]",
    "threat_endpoint_rows[3]",
    "threat_endpoint_rows[4]",
    "threat_endpoint_rows[5]",
    "threat_endpoint_rows[6]",
    "threat_endpoint_rows[7]",
    "threat_rescued[0]",
    "threat_rescued[1]",
    "threat_rescued[2]",
    "threat_rescued[3]",
    "threat_rescued_rows[0]",
    "threat_rescued_rows[1]",
    "threat_rescued_rows[2]",
    "threat_rescued_rows[3]",
    "ft_cf_paired[0]",
    "ft_cf_finny_rows[0]",
    "ft_cf_ancestor_rows[0]",
    "ft_cf_saved[0]",
    "ft_cf_distance[0]",
    "ft_cf_dirty[0]",
    "ft_cf_missing[0]",
    "ft_cf_paired[1]",
    "ft_cf_finny_rows[1]",
    "ft_cf_ancestor_rows[1]",
    "ft_cf_saved[1]",
    "ft_cf_distance[1]",
    "ft_cf_dirty[1]",
    "ft_cf_missing[1]",
    "ft_cf_paired[2]",
    "ft_cf_finny_rows[2]",
    "ft_cf_ancestor_rows[2]",
    "ft_cf_saved[2]",
    "ft_cf_distance[2]",
    "ft_cf_dirty[2]",
    "ft_cf_missing[2]",
    "ft_cf_delta[0]",
    "ft_cf_delta[1]",
    "ft_cf_delta[2]",
    "ft_cf_delta[3]",
    "ft_cf_delta[4]",
    "ft_cf_delta[5]",
    "ft_cf_delta[6]",
    "ft_cf_delta[7]",
    "ft_cf_delta[8]",
    "ft_cf_delta[9]",
    "ft_cf_delta[10]",
    "ft_cf_delta[11]",
    "ft_cf_delta[12]",
    "ft_cf_delta[13]",
    "ft_cf_delta[14]",
    "ft_cf_delta[15]",
    "ft_cf_delta[16]",
    "ft_cf_delta[17]",
    "ft_cf_delta[18]",
    "ft_cf_delta[19]",
    "ft_cf_delta[20]",
    "ft_cf_delta[21]",
    "ft_cf_delta[22]",
    "ft_cf_delta[23]",
    "ft_cf_delta[24]",
    "ft_cf_delta[25]",
    "ft_cf_delta[26]",
    "ft_cf_eval[0]",
    "ft_cf_eval[1]",
    "ft_cf_eval[2]",
    "ft_cf_eval[3]",
    "ft_cf_eval[4]",
    "ft_cf_eval[5]",
    "eval_tt_lazy",
    "eval_hash",
    "eval_computed_score",
    "eval_nnue",
    "eval_nnue_repeat",
    "eval_nnue_collision",
    "threat_physical_edges[0]",
    "threat_physical_edges[1]",
    "threat_physical_edges[2]",
    "threat_physical_edges[3]",
    "threat_endpoint_success[0]",
    "threat_endpoint_success[1]",
    "threat_endpoint_success[2]",
    "threat_endpoint_success[3]",
    "ft_cf_single[0]",
    "ft_cf_single[1]",
    "ft_cf_single[2]",
    "ft_cf_equal[0]",
    "ft_cf_equal[1]",
    "ft_cf_equal[2]",
    "ft_finny_evals",
];

thread_local! {
    static COUNTS: RefCell<[u64; KEYS.len()]> = const { RefCell::new([0; KEYS.len()]) };
    static PROBE_REASON: Cell<usize> = const { Cell::new(SEARCH_PRELOOP_TT) };
    static SEE_SOURCE: Cell<usize> = const { Cell::new(7) };
    static SEE_PRIOR: Cell<bool> = const { Cell::new(false) };
}
pub(crate) fn add(index: usize, value: u64) {
    COUNTS.with_borrow_mut(|c| c[index] += value);
}

pub(crate) fn reset() {
    COUNTS.with_borrow_mut(|c| c.fill(0));
    reset_nnue_diagnostics();
    SEE_SOURCE.set(7);
    SEE_PRIOR.set(false);
}
pub(crate) fn report() -> String {
    use std::fmt::Write;
    let mut out = String::new();
    COUNTS.with_borrow(|c| {
        for (key, value) in KEYS.iter().zip(c) {
            writeln!(out, "{key}={value}").unwrap();
        }
    });
    COUNTS.with_borrow(|c| {
        for (key, base) in [
            ("quiet_n", QUIET_N_DEPTH_0),
            ("quiet_board", QUIET_BOARD_DEPTH_0),
            ("quiet_pawn_drop", QUIET_PAWN_DROP_DEPTH_0),
            ("quiet_other_drop", QUIET_OTHER_DROP_DEPTH_0),
            ("quiet_k", QUIET_K_DEPTH_0),
            ("quiet_c", QUIET_C_DEPTH_0),
            ("quiet_init", QUIET_INIT_DEPTH_0),
        ] {
            writeln!(out, "{key}={}", c[base..base + 5].iter().sum::<u64>()).unwrap();
        }
    });
    out
}
fn depth_bucket(depth: i32) -> usize {
    match depth {
        ..=0 => 4,
        1 => 0,
        2 => 1,
        3..=4 => 2,
        _ => 3,
    }
}
fn size_bucket(n: usize) -> usize {
    match n {
        0 => 0,
        1..=15 => 1,
        16..=31 => 2,
        32..=63 => 3,
        64..=127 => 4,
        _ => 5,
    }
}
pub(crate) fn quiet_init(pos: &Position, depth: i32, moves: &[ExtMove], limit: i32) {
    let mut counts = [0usize; 3];
    for ext in moves {
        counts[if !ext.mv.is_drop() {
            0
        } else if ext.mv.drop_piece_type() == PieceType::Pawn {
            1
        } else {
            2
        }] += 1;
    }
    let d = depth_bucket(depth);
    add(QUIET_INIT_DEPTH_0 + d, 1);
    for (base, hist, n) in [
        (QUIET_N_DEPTH_0, QUIET_N_HIST_0, moves.len()),
        (QUIET_BOARD_DEPTH_0, QUIET_BOARD_HIST_0, counts[0]),
        (QUIET_PAWN_DROP_DEPTH_0, QUIET_PAWN_DROP_HIST_0, counts[1]),
        (QUIET_OTHER_DROP_DEPTH_0, QUIET_OTHER_DROP_HIST_0, counts[2]),
    ] {
        add(base + d, n as u64);
        add(hist + size_bucket(n), 1);
    }
    add(
        QUIET_K_DEPTH_0 + d,
        moves.iter().filter(|ext| ext.value >= limit).count() as u64,
    );
    let hand = pos.hand(pos.side_to_move());
    let kinds = PieceType::HAND_PIECES.iter().filter(|&&pt| hand.count(pt) > 0).count();
    add(QUIET_HAND_TYPES_HIST_0 + kinds, 1);
}
pub(crate) fn quiet_returned(depth: i32) {
    add(QUIET_C_DEPTH_0 + depth_bucket(depth), 1);
}
pub(crate) fn see_caller(source: usize, prior: bool) {
    SEE_SOURCE.set(source);
    SEE_PRIOR.set(prior);
}
pub(crate) fn set_see_source(source: usize) {
    SEE_SOURCE.set(source);
}
pub(crate) fn begin_see(pos: &Position, mv: Move) -> usize {
    let source = SEE_SOURCE.replace(7);
    let prior = SEE_PRIOR.replace(false);
    if prior && source <= 1 {
        add(SEE_STEP14_DOUBLE, 1);
    }
    if prior && source == 4 {
        add(SEE_QS_DOUBLE, 1);
    }
    if !mv.is_pass() {
        let them = !pos.side_to_move();
        // 現在の占有で守りを数える。SEE の仮想占有や戻り値には触れない。
        let to_free = pos.attackers_to_color_occ(them, mv.to(), pos.occupied()).is_empty();
        if pos.piece_on(mv.to()).is_none() {
            if to_free
                && (mv.is_drop()
                    || pos.attackers_to_color_occ(them, mv.from(), pos.occupied()).is_empty())
            {
                add(SEE_ZERO_UNATTACKED, 1);
            }
        } else if to_free {
            add(SEE_CAPTURE_UNDEFENDED, 1);
        }
    }
    source
}
pub(crate) fn see_outcome(source: usize, outcome: usize) {
    add(SEE_STEP14_CAPTURE_FALSE + source * 3 + outcome, 1);
}
pub(crate) struct PreloopExit {
    pub(crate) active: bool,
    pub(crate) reason: usize,
}
impl PreloopExit {
    pub(crate) fn new() -> Self {
        Self {
            active: true,
            reason: SEARCH_PRELOOP_OTHER,
        }
    }
}
impl Drop for PreloopExit {
    fn drop(&mut self) {
        if self.active {
            add(self.reason, 1);
        }
    }
}

pub(crate) fn probe_reason(reason: usize) {
    PROBE_REASON.set(reason);
}
pub(crate) fn last_probe_reason() -> usize {
    PROBE_REASON.get()
}

// 両視点で異なる経路を使う評価は、それぞれの経路を一回ずつ数える。
pub(crate) struct NnueRequest {
    before: [u64; 6],
}
impl NnueRequest {
    pub(crate) fn new() -> Self {
        add(NNUE_ACC_REQUESTS, 1);
        begin_ft();
        Self {
            before: Self::snapshot(),
        }
    }
    fn snapshot() -> [u64; 6] {
        COUNTS.with_borrow(|c| {
            [
                c[NNUE_FT_INCREMENTAL],
                c[NNUE_FT_FINNY],
                c[NNUE_FT_FULL],
                c[NNUE_THREAT_INCREMENTAL],
                c[NNUE_THREAT_FULL],
                c[NNUE_ANCESTOR_HIST_0..NNUE_ANCESTOR_HIST_0 + 9].iter().sum(),
            ]
        })
    }
}
impl Drop for NnueRequest {
    fn drop(&mut self) {
        end_ft();
        let after = Self::snapshot();
        for (i, &key) in [
            NNUE_EVAL_FT_INCREMENTAL,
            NNUE_EVAL_FT_FINNY,
            NNUE_EVAL_FT_FULL,
            NNUE_EVAL_THREAT_INCREMENTAL,
            NNUE_EVAL_THREAT_FULL,
        ]
        .iter()
        .enumerate()
        {
            if after[i] > self.before[i] {
                add(key, 1);
            }
        }
        if after[5] == self.before[5] {
            add(NNUE_ANCESTOR_MISSING, 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::SearchStats;

    #[test]
    fn diagnostics_follow_the_legacy_report_without_depth_one_probes() {
        reset();
        add(TT_PROBE, 7);
        let report = SearchStats::default().format_report();
        assert!(report.contains("tt_probe=7\n"));
        assert!(report.ends_with("quiet_init=0\n"));
        let legacy_end = report.find("--- LMR Cut Node Analysis ---").unwrap();
        assert!(report.find("quiet_n_depth[0]=0").unwrap() > legacy_end);
    }

    #[test]
    fn search_stats_reset_also_resets_diagnostics() {
        add(QUIET_N_DEPTH_0, 15);
        add(TT_PROBE, 7);
        see_caller(0, true);
        SearchStats::default().reset();
        COUNTS.with_borrow(|c| assert!(c.iter().all(|&v| v == 0)));
        assert_eq!(SEE_SOURCE.get(), 7);
        assert!(!SEE_PRIOR.get());
    }
}

#[path = "diagnostics_nnue.rs"]
mod nnue_counters;
pub(crate) use nnue_counters::*;
