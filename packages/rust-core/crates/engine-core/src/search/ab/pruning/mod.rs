use crate::evaluation::evaluate::Evaluator;
use crate::search::params as dynp;
use crate::search::params::{
    NMP_BASE_R, NMP_BONUS_DELTA_BETA, NMP_HAND_SUM_DISABLE, NMP_MIN_DEPTH,
};
use crate::search::tt::TTProbe;
use crate::search::types::SearchStack;
use crate::Position;

#[cfg(any(debug_assertions, feature = "diagnostics"))]
use self::state_diagnostics::{
    capture_minimal_fingerprint, capture_position_fingerprint, check_minimal_fingerprint,
    log_state_drift,
};

#[cfg(any(debug_assertions, feature = "diagnostics"))]
use super::diagnostics;

use super::driver::ClassicBackend;
use super::ordering::{EvalMoveGuard, EvalNullGuard, Heuristics, MovePicker};
use super::profile::PruneToggles;
use super::pvs::{ABArgs, SearchContext};
use crate::search::constants::MATE_SCORE;

pub(super) struct NullMovePruneParams<'a, 'ctx> {
    pub toggles: &'a PruneToggles,
    pub depth: i32,
    pub pos: &'a Position,
    pub beta: i32,
    pub static_eval: i32,
    pub ply: u32,
    pub stack: &'a mut [SearchStack],
    pub heur: &'a mut Heuristics,
    pub tt_hits: &'a mut u64,
    pub beta_cuts: &'a mut u64,
    pub lmr_counter: &'a mut u64,
    pub ctx: &'a mut SearchContext<'ctx>,
}

pub(super) struct MaybeIidParams<'a, 'ctx> {
    pub toggles: &'a PruneToggles,
    pub depth: i32,
    pub pos: &'a Position,
    pub alpha: i32,
    pub beta: i32,
    pub ply: u32,
    pub stack: &'a mut [SearchStack],
    pub heur: &'a mut Heuristics,
    pub tt_hits: &'a mut u64,
    pub beta_cuts: &'a mut u64,
    pub lmr_counter: &'a mut u64,
    pub ctx: &'a mut SearchContext<'ctx>,
    pub tt_hint: &'a mut Option<crate::shogi::Move>,
    pub tt_depth_ok: bool,
}

pub(super) struct ProbcutParams<'a, 'ctx> {
    pub toggles: &'a PruneToggles,
    pub depth: i32,
    pub pos: &'a Position,
    pub beta: i32,
    pub static_eval: i32,
    pub ply: u32,
    pub is_pv: bool,
    pub stack: &'a mut [SearchStack],
    pub heur: &'a mut Heuristics,
    pub tt_hits: &'a mut u64,
    pub beta_cuts: &'a mut u64,
    pub lmr_counter: &'a mut u64,
    pub ctx: &'a mut SearchContext<'ctx>,
}

pub(super) struct StaticBetaPruneParams<'a> {
    pub toggles: &'a PruneToggles,
    pub depth: i32,
    pub pos: &'a Position,
    pub beta: i32,
    pub static_eval: i32,
    pub is_pv: bool,
    pub ply: u32,
    pub stack: &'a [SearchStack],
}

pub(super) struct RazorPruneParams<'a, 'ctx> {
    pub toggles: &'a PruneToggles,
    pub depth: i32,
    pub pos: &'a Position,
    pub alpha: i32,
    pub static_eval: i32,
    pub ctx: &'a mut SearchContext<'ctx>,
    pub ply: u32,
    pub is_pv: bool,
}

impl<E: Evaluator + Send + Sync + 'static> ClassicBackend<E> {
    pub(super) fn should_static_beta_prune(&self, params: StaticBetaPruneParams<'_>) -> bool {
        let StaticBetaPruneParams {
            toggles,
            depth,
            pos,
            beta,
            static_eval,
            is_pv,
            ply,
            stack,
        } = params;
        if !(toggles.enable_static_beta_pruning && dynp::static_beta_enabled()) {
            return false;
        }
        if pos.is_in_check() {
            return false;
        }
        // Safeモード: 動的SBP（depth<=12、非PV、improving/履歴考慮の簡易版）
        if dynp::pruning_safe_mode() && dynp::sbp_dynamic_enabled() && !is_pv && depth <= 12 {
            use crate::search::constants::MATE_SCORE;
            if beta.abs() >= MATE_SCORE - 100 {
                return false;
            }
            let d = depth.clamp(1, 12);
            let improving = if ply >= 2 {
                let idx = (ply - 2) as usize;
                stack
                    .get(idx)
                    .and_then(|st| st.static_eval)
                    .is_some_and(|prev2| static_eval >= prev2 - 10)
            } else {
                false
            };
            let mut margin = dynp::sbp_margin_base() + dynp::sbp_margin_slope() * d;
            if improving {
                margin -= 40;
            }
            let cut = static_eval - margin >= beta;
            if cut {
                #[cfg(any(debug_assertions, feature = "diagnostics"))]
                super::diagnostics::record_tag(
                    pos,
                    match d {
                        1..=4 => "sbp_cut_d1_4",
                        5..=8 => "sbp_cut_d5_8",
                        _ => "sbp_cut_d9_12",
                    },
                    Some(format!("marg={} imp={}", margin, improving as i32)),
                );
            }
            return cut;
        }
        // 従来: depth<=2 の固定マージン
        if depth <= 2 {
            let margin = if depth == 1 {
                dynp::sbp_margin_d1()
            } else {
                dynp::sbp_margin_d2()
            };
            return static_eval - margin >= beta;
        }
        false
    }

    pub(super) fn razor_prune(&self, params: RazorPruneParams<'_, '_>) -> Option<i32> {
        let RazorPruneParams {
            toggles,
            depth,
            pos,
            alpha,
            static_eval,
            ctx,
            ply,
            is_pv,
        } = params;
        if !(toggles.enable_razor && dynp::razor_enabled()) {
            return None;
        }
        if pos.is_in_check() || is_pv {
            return None;
        }
        // Safeモード: YO寄りの深さ依存マージンで強い劣勢のみqsearchで刈る（非PV想定）
        if crate::search::params::pruning_safe_mode() {
            // eval < alpha - (495 + 290*depth^2) のみ対象
            // depthはi32、係数はcp単位
            let d = depth.max(1);
            let margin = 495i32.saturating_add(290i32.saturating_mul(d.saturating_mul(d)));
            if static_eval <= alpha.saturating_sub(margin) {
                #[cfg(any(debug_assertions, feature = "diagnostics"))]
                super::diagnostics::record_tag(
                    pos,
                    "razor_triggered",
                    Some(format!("depth={} margin={}", d, margin)),
                );
                let r = self.qsearch(pos, alpha, alpha + 1, ctx, ply);
                if r <= alpha {
                    return Some(r);
                }
            }
            return None;
        }

        // 従来: depth==1のみの簡易Razor
        if depth == 1 {
            let r = self.qsearch(pos, alpha, alpha + 1, ctx, ply);
            if r <= alpha {
                return Some(r);
            }
        }
        None
    }

    pub(super) fn null_move_prune(&self, params: NullMovePruneParams<'_, '_>) -> Option<i32> {
        let NullMovePruneParams {
            toggles,
            depth,
            pos,
            beta,
            static_eval,
            ply,
            stack,
            heur,
            tt_hits,
            beta_cuts,
            lmr_counter,
            ctx,
        } = params;
        if !toggles.enable_nmp || !dynp::nmp_enabled() {
            return None;
        }
        let prev_null = if ply > 0 {
            stack[(ply - 1) as usize].null_move
        } else {
            false
        };
        if depth < NMP_MIN_DEPTH || pos.is_in_check() || prev_null {
            return None;
        }
        let side = pos.side_to_move as usize;
        let hand_sum: i32 = pos.hands[side].iter().map(|&c| c as i32).sum();
        if hand_sum >= NMP_HAND_SUM_DISABLE {
            return None;
        }
        let bonus = if static_eval - beta > NMP_BONUS_DELTA_BETA {
            1
        } else {
            0
        };
        let mut r = NMP_BASE_R + (depth / 4) + bonus;
        r = r.min(depth - 1);
        let score = {
            let _guard = EvalNullGuard::new(self.evaluator.as_ref(), pos);
            let mut child = pos.clone();
            #[cfg(any(debug_assertions, feature = "diagnostics"))]
            let baseline = capture_position_fingerprint(&child);
            #[cfg(any(debug_assertions, feature = "diagnostics"))]
            let baseline_min = capture_minimal_fingerprint(&child);
            #[cfg(any(debug_assertions, feature = "diagnostics"))]
            diagnostics::record_null_event(pos, depth, beta - 1, beta, false, "null_enter");
            let undo_null = child.do_null_move();
            stack[ply as usize].null_move = true;
            let (sc, _) = self.alphabeta(
                ABArgs {
                    pos: &child,
                    depth: depth - 1 - r,
                    alpha: -(beta),
                    beta: -(beta - 1),
                    ply: ply + 1,
                    is_pv: false,
                    stack,
                    heur,
                    tt_hits,
                    beta_cuts,
                    lmr_counter,
                },
                ctx,
            );
            child.undo_null_move(undo_null);
            stack[ply as usize].null_move = false;
            #[cfg(any(debug_assertions, feature = "diagnostics"))]
            log_state_drift("null_move_prune::post_undo", &baseline, &child);
            #[cfg(any(debug_assertions, feature = "diagnostics"))]
            check_minimal_fingerprint("nmp_roundtrip", &baseline_min, &child);
            #[cfg(any(debug_assertions, feature = "diagnostics"))]
            diagnostics::record_null_event(pos, depth, beta - 1, beta, false, "null_exit");
            -sc
        };
        if score >= beta {
            Some(score)
        } else {
            None
        }
    }

    pub(super) fn maybe_iid(&self, params: MaybeIidParams<'_, '_>) {
        let MaybeIidParams {
            toggles,
            depth,
            pos,
            alpha,
            beta,
            ply,
            stack,
            heur,
            tt_hits,
            beta_cuts,
            lmr_counter,
            ctx,
            tt_hint,
            tt_depth_ok,
        } = params;
        if !(toggles.enable_iid
            && dynp::iid_enabled()
            && depth >= dynp::iid_min_depth()
            && !pos.is_in_check()
            && (!tt_depth_ok || tt_hint.is_none()))
        {
            return;
        }
        let iid_depth = depth - 2;
        #[cfg(any(debug_assertions, feature = "diagnostics"))]
        let baseline = capture_position_fingerprint(pos);
        #[cfg(any(debug_assertions, feature = "diagnostics"))]
        let baseline_min = capture_minimal_fingerprint(pos);
        #[cfg(any(debug_assertions, feature = "diagnostics"))]
        diagnostics::record_iid_event(pos, depth, alpha, beta, false, "iid_enter");
        let _ = self.alphabeta(
            ABArgs {
                pos,
                depth: iid_depth,
                alpha,
                beta,
                ply,
                is_pv: false,
                stack,
                heur,
                tt_hits,
                beta_cuts,
                lmr_counter,
            },
            ctx,
        );
        #[cfg(any(debug_assertions, feature = "diagnostics"))]
        log_state_drift("maybe_iid::post_search", &baseline, pos);
        #[cfg(any(debug_assertions, feature = "diagnostics"))]
        check_minimal_fingerprint("iid_roundtrip", &baseline_min, pos);
        #[cfg(any(debug_assertions, feature = "diagnostics"))]
        diagnostics::record_iid_event(pos, depth, alpha, beta, false, "iid_exit");
        if let Some(tt) = &self.tt {
            if let Some(entry) = tt.probe(pos.zobrist_hash(), pos.side_to_move) {
                *tt_hint = entry.get_move();
            }
        }
    }

    pub(super) fn probcut(
        &self,
        params: ProbcutParams<'_, '_>,
    ) -> Option<(i32, crate::shogi::Move)> {
        let ProbcutParams {
            toggles,
            depth,
            pos,
            beta,
            static_eval,
            ply,
            stack,
            heur,
            tt_hits,
            beta_cuts,
            lmr_counter,
            ctx,
            ..
        } = params;
        if !toggles.enable_probcut || !dynp::probcut_enabled() || pos.is_in_check() || params.is_pv
        {
            return None;
        }

        // Safeモード: YO寄りの厳しめガード + 事前qsearch + 検証探索(depth-5)
        if dynp::pruning_safe_mode() {
            // Guard: too shallow near root (verification would be too short) → disable ProbCut
            if depth <= 5 {
                return None;
            }
            // improving 推定: 2手前の静的評価と比較
            let improving = if ply >= 2 {
                let idx = (ply - 2) as usize;
                if let Some(prev2) = stack[idx].static_eval {
                    static_eval >= prev2 - 10
                } else {
                    false
                }
            } else {
                false
            };
            let margin = 215 - if improving { 60 } else { 0 };
            let threshold = beta + margin;
            #[cfg(any(debug_assertions, feature = "diagnostics"))]
            super::diagnostics::record_tag(
                pos,
                "pc_params",
                Some(format!(
                    "th={} imp={} vd={}",
                    threshold,
                    improving as i32,
                    (depth - 5).max(1)
                )),
            );

            // Guard: near mate band → disable ProbCut entirely to avoid false cuts
            if threshold.abs() >= MATE_SCORE - 100 {
                return None;
            }

            // 事前qsearchにより早すぎる試行を抑制
            let qs_alpha = threshold - 1;
            let qs_beta = threshold;
            let qs = self.qsearch(pos, qs_alpha, qs_beta, ctx, ply);
            #[cfg(any(debug_assertions, feature = "diagnostics"))]
            super::diagnostics::record_tag(
                pos,
                "pc_qs_gate",
                Some(format!(
                    "alpha={} beta={} mode=safe depth={} pv={}",
                    qs_alpha, qs_beta, depth, false
                )),
            );
            if qs < threshold {
                return None;
            }

            let see_threshold = (threshold - static_eval).max(0);
            let prev_move = if ply > 0 {
                stack[(ply - 1) as usize].current_move
            } else {
                None
            };
            let excluded = stack[ply as usize].excluded_move;
            let mut picker = MovePicker::new_probcut(pos, excluded, prev_move, see_threshold);
            let mut attempts = 0usize;
            while let Some(mv) = picker.next(&*heur) {
                // SEE良手のみに限定（過剰トリガー抑制）
                if pos.see(mv) < 0 {
                    #[cfg(any(debug_assertions, feature = "diagnostics"))]
                    super::diagnostics::record_tag(
                        pos,
                        "pc_see_neg_skip",
                        Some("mode=safe".to_string()),
                    );
                    continue;
                }
                attempts += 1;
                // 試行回数制限（1手のみ）
                if attempts > 1 {
                    break;
                }
                #[cfg(any(debug_assertions, feature = "diagnostics"))]
                super::diagnostics::record_tag(
                    pos,
                    "pc_verif_tried",
                    Some(format!("mv={} mode=safe", crate::usi::move_to_usi(&mv))),
                );
                let parent_sc = {
                    let _guard = EvalMoveGuard::new(self.evaluator.as_ref(), pos, mv);
                    let mut child = pos.clone();
                    child.do_move(mv);
                    let verify_depth = (depth - 5).max(1);
                    let (sc, _) = self.alphabeta(
                        ABArgs {
                            pos: &child,
                            depth: verify_depth,
                            alpha: -threshold,
                            beta: -threshold + 1,
                            ply: ply + 1,
                            is_pv: false,
                            stack,
                            heur,
                            tt_hits,
                            beta_cuts,
                            lmr_counter,
                        },
                        ctx,
                    );
                    -sc
                };
                if parent_sc >= threshold {
                    #[cfg(any(debug_assertions, feature = "diagnostics"))]
                    {
                        super::diagnostics::record_tag(
                            pos,
                            "pc_cut_hit",
                            Some(format!("mv={} mode=safe", crate::usi::move_to_usi(&mv))),
                        );
                        super::diagnostics::record_tag(
                            pos,
                            match depth {
                                d if d < 10 => "pc_cut_hit_dlt10",
                                d if d < 16 => "pc_cut_hit_d10_15",
                                _ => "pc_cut_hit_d16p",
                            },
                            None,
                        );
                    }
                    return Some((parent_sc, mv));
                }
            }
            return None;
        }

        // 既存（従来）モード
        if depth < 5 {
            return None;
        }
        if dynp::probcut_skip_verify_lt4() && depth < 4 {
            #[cfg(any(debug_assertions, feature = "diagnostics"))]
            super::diagnostics::record_tag(
                pos,
                "pc_skip_verify_dlt4",
                Some("mode=off".to_string()),
            );
            return None;
        }
        let margin = dynp::probcut_margin(depth);
        let threshold = beta + margin;
        let see_threshold = (threshold - static_eval).max(0);
        let prev_move = if ply > 0 {
            stack[(ply - 1) as usize].current_move
        } else {
            None
        };
        let excluded = stack[ply as usize].excluded_move;
        let mut picker = MovePicker::new_probcut(pos, excluded, prev_move, see_threshold);
        while let Some(mv) = picker.next(&*heur) {
            let parent_sc = {
                let _guard = EvalMoveGuard::new(self.evaluator.as_ref(), pos, mv);
                let mut child = pos.clone();
                child.do_move(mv);
                let (sc, _) = self.alphabeta(
                    ABArgs {
                        pos: &child,
                        depth: depth - 2,
                        alpha: -threshold,
                        beta: -threshold + 1,
                        ply: ply + 1,
                        is_pv: false,
                        stack,
                        heur,
                        tt_hits,
                        beta_cuts,
                        lmr_counter,
                    },
                    ctx,
                );
                -sc
            };
            if parent_sc >= threshold {
                return Some((parent_sc, mv));
            }
        }
        None
    }
}

#[cfg(any(debug_assertions, feature = "diagnostics"))]
mod state_diagnostics {
    use super::*;
    use crate::shogi::board::{Color, Square};
    use crate::shogi::piece_constants::hand_index_to_piece_type;
    use crate::shogi::NUM_HAND_PIECE_TYPES;
    use log::warn;

    #[derive(Clone)]
    pub(super) struct MinimalFingerprint {
        side_to_move: Color,
        hash: u64,
        zobrist: u64,
        kings: [Option<Square>; 2],
    }

    pub(super) fn capture_minimal_fingerprint(pos: &Position) -> MinimalFingerprint {
        MinimalFingerprint {
            side_to_move: pos.side_to_move,
            hash: pos.hash,
            zobrist: pos.zobrist_hash,
            kings: [
                pos.board.king_square(Color::Black),
                pos.board.king_square(Color::White),
            ],
        }
    }

    impl MinimalFingerprint {
        fn diff(&self, pos: &Position) -> Option<String> {
            let mut diffs = Vec::new();
            if self.side_to_move != pos.side_to_move {
                diffs.push(format!(
                    "side_to_move {:?} -> {:?}",
                    self.side_to_move, pos.side_to_move
                ));
            }
            if self.hash != pos.hash {
                diffs.push(format!("hash {:016x} -> {:016x}", self.hash, pos.hash));
            }
            if self.zobrist != pos.zobrist_hash {
                diffs.push(format!("zobrist {:016x} -> {:016x}", self.zobrist, pos.zobrist_hash));
            }
            let current_kings = [
                pos.board.king_square(Color::Black),
                pos.board.king_square(Color::White),
            ];
            for (idx, (expected, actual)) in self.kings.iter().zip(current_kings.iter()).enumerate()
            {
                if expected != actual {
                    let color = if idx == 0 { Color::Black } else { Color::White };
                    let expected_str =
                        expected.map(|sq| sq.to_string()).unwrap_or_else(|| "-".to_string());
                    let actual_str =
                        actual.map(|sq| sq.to_string()).unwrap_or_else(|| "-".to_string());
                    diffs.push(format!("king[{color:?}] {expected_str} -> {actual_str}"));
                }
            }
            if diffs.is_empty() {
                None
            } else {
                Some(diffs.join(", "))
            }
        }
    }

    pub(super) fn check_minimal_fingerprint(
        tag: &'static str,
        fingerprint: &MinimalFingerprint,
        pos: &Position,
    ) {
        if let Some(diff) = fingerprint.diff(pos) {
            warn!("[{tag}] Minimal roundtrip mismatch: {diff}");
            super::diagnostics::note_fault(tag);
        }
    }

    #[derive(Clone)]
    pub(super) struct PositionFingerprint {
        side_to_move: Color,
        ply: u16,
        hash: u64,
        zobrist: u64,
        hands: [[u8; NUM_HAND_PIECE_TYPES]; 2],
        kings: [Option<Square>; 2],
        history_len: usize,
        history_tail: Option<u64>,
    }

    pub(super) fn capture_position_fingerprint(pos: &Position) -> PositionFingerprint {
        PositionFingerprint {
            side_to_move: pos.side_to_move,
            ply: pos.ply,
            hash: pos.hash,
            zobrist: pos.zobrist_hash,
            hands: pos.hands,
            kings: [
                pos.board.king_square(Color::Black),
                pos.board.king_square(Color::White),
            ],
            history_len: pos.history.len(),
            history_tail: pos.history.last().copied(),
        }
    }

    impl PositionFingerprint {
        fn diff(&self, pos: &Position) -> Option<String> {
            let mut diffs = Vec::new();

            if self.side_to_move != pos.side_to_move {
                diffs.push(format!(
                    "side_to_move {:?} -> {:?}",
                    self.side_to_move, pos.side_to_move
                ));
            }

            if self.ply != pos.ply {
                diffs.push(format!("ply {} -> {}", self.ply, pos.ply));
            }

            if self.hash != pos.hash {
                diffs.push(format!("hash {:016x} -> {:016x}", self.hash, pos.hash));
            }

            if self.zobrist != pos.zobrist_hash {
                diffs.push(format!("zobrist {:016x} -> {:016x}", self.zobrist, pos.zobrist_hash));
            }

            let current_kings = [
                pos.board.king_square(Color::Black),
                pos.board.king_square(Color::White),
            ];
            for (idx, (expected, actual)) in self.kings.iter().zip(current_kings.iter()).enumerate()
            {
                if expected != actual {
                    let color = if idx == 0 { Color::Black } else { Color::White };
                    let expected_str =
                        expected.map(|sq| sq.to_string()).unwrap_or_else(|| "-".to_string());
                    let actual_str =
                        actual.map(|sq| sq.to_string()).unwrap_or_else(|| "-".to_string());
                    diffs.push(format!("king[{color:?}] {expected_str} -> {actual_str}"));
                }
            }

            for color_idx in 0..2 {
                for hand_idx in 0..NUM_HAND_PIECE_TYPES {
                    let before = self.hands[color_idx][hand_idx];
                    let after = pos.hands[color_idx][hand_idx];
                    if before != after {
                        let color = if color_idx == Color::Black as usize {
                            Color::Black
                        } else {
                            Color::White
                        };
                        let piece = hand_index_to_piece_type(hand_idx)
                            .map(|pt| format!("{pt:?}"))
                            .unwrap_or_else(|| format!("index{hand_idx}"));
                        diffs.push(format!("hand[{color:?}][{piece}] {before} -> {after}"));
                    }
                }
            }

            if self.history_len != pos.history.len() {
                diffs.push(format!("history_len {} -> {}", self.history_len, pos.history.len()));
            }

            let tail = pos.history.last().copied();
            if self.history_tail != tail {
                diffs.push(format!("history_tail {:?} -> {:?}", self.history_tail, tail));
            }

            if diffs.is_empty() {
                None
            } else {
                Some(diffs.join(", "))
            }
        }
    }

    pub(super) fn log_state_drift(tag: &str, fingerprint: &PositionFingerprint, pos: &Position) {
        if let Some(diff) = fingerprint.diff(pos) {
            warn!("[{tag}] Position state drift detected: {diff}");
        }
    }
}
