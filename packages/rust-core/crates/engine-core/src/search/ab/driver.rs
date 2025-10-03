use std::env;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use crate::evaluation::evaluate::Evaluator;
use crate::movegen::MoveGenerator;
use crate::search::api::{BackendSearchTask, InfoEvent, InfoEventCallback, SearcherBackend};
use crate::search::types::{NodeType, RootLine, SearchStack};
use crate::search::{SearchLimits, SearchResult, SearchStats, TranspositionTable};
use crate::Position;
use smallvec::SmallVec;

use super::ordering::{self, Heuristics};
use super::profile::{PruneToggles, SearchProfile};
use super::pvs::{self, SearchContext};
use crate::search::tt::TTProbe;

#[derive(Clone)]
pub struct ClassicBackend<E: Evaluator + Send + Sync + 'static> {
    pub(super) evaluator: Arc<E>,
    pub(super) tt: Option<Arc<TranspositionTable>>, // 共有TT（Hashfull出力用、将来はprobe/storeでも使用）
    pub(super) profile: SearchProfile,
}

impl<E: Evaluator + Send + Sync + 'static> ClassicBackend<E> {
    fn currmove_throttle_ms() -> Option<u64> {
        static POLICY: OnceLock<Option<u64>> = OnceLock::new();
        *POLICY.get_or_init(|| match env::var("SHOGI_CURRMOVE_THROTTLE_MS") {
            Ok(val) => {
                let val = val.trim().to_ascii_lowercase();
                if val == "off" || val == "0" {
                    None
                } else {
                    val.parse::<u64>().ok().filter(|v| *v > 0)
                }
            }
            Err(_) => Some(150),
        })
    }

    pub fn new(evaluator: Arc<E>) -> Self {
        Self::with_profile(evaluator, SearchProfile::default())
    }

    pub fn with_tt(evaluator: Arc<E>, tt: Arc<TranspositionTable>) -> Self {
        Self::with_profile_and_tt(evaluator, tt, SearchProfile::default())
    }

    pub fn with_tt_and_toggles(
        evaluator: Arc<E>,
        tt: Arc<TranspositionTable>,
        toggles: PruneToggles,
    ) -> Self {
        let mut profile = SearchProfile::enhanced_material();
        profile.prune = toggles;
        Self::with_profile_and_tt(evaluator, tt, profile)
    }

    pub fn with_tt_and_toggles_apply_defaults(
        evaluator: Arc<E>,
        tt: Arc<TranspositionTable>,
        toggles: PruneToggles,
    ) -> Self {
        let mut profile = SearchProfile::enhanced_material();
        profile.prune = toggles;
        profile.apply_runtime_defaults();
        Self::with_profile_and_tt(evaluator, tt, profile)
    }

    pub fn with_profile(evaluator: Arc<E>, profile: SearchProfile) -> Self {
        Self {
            evaluator,
            tt: None,
            profile,
        }
    }

    pub fn with_profile_apply_defaults(evaluator: Arc<E>, profile: SearchProfile) -> Self {
        profile.apply_runtime_defaults();
        Self::with_profile(evaluator, profile)
    }

    pub fn with_profile_and_tt(
        evaluator: Arc<E>,
        tt: Arc<TranspositionTable>,
        profile: SearchProfile,
    ) -> Self {
        Self {
            evaluator,
            tt: Some(tt),
            profile,
        }
    }

    pub fn with_profile_and_tt_apply_defaults(
        evaluator: Arc<E>,
        tt: Arc<TranspositionTable>,
        profile: SearchProfile,
    ) -> Self {
        profile.apply_runtime_defaults();
        Self::with_profile_and_tt(evaluator, tt, profile)
    }

    pub(super) fn should_stop(limits: &SearchLimits) -> bool {
        if let Some(flag) = &limits.stop_flag {
            return flag.load(Ordering::Relaxed);
        }
        false
    }

    pub(super) fn iterative(
        &self,
        root: &Position,
        limits: &SearchLimits,
        info: Option<&InfoEventCallback>,
    ) -> SearchResult {
        let max_depth = limits.depth_limit_u8() as i32;
        let mut best: Option<crate::shogi::Move> = None;
        let mut best_score = 0;
        let mut nodes: u64 = 0;
        let t0 = Instant::now();
        let deadlines = limits.fallback_deadlines;
        let (soft_deadline, hard_deadline) = if let Some(dl) = deadlines {
            (
                (dl.soft_limit_ms > 0).then(|| Duration::from_millis(dl.soft_limit_ms)),
                (dl.hard_limit_ms > 0).then(|| Duration::from_millis(dl.hard_limit_ms)),
            )
        } else if let Some(limit) = limits.time_limit() {
            (Some(limit), Some(limit))
        } else {
            (None, None)
        };
        let _last_hashfull_emit_ms = 0u64;
        let mut prev_score = 0;
        // Aspiration initial params
        const ASP_DELTA0: i32 = 30;
        const ASP_DELTA_MAX: i32 = 350;
        const SELDEPTH_EXTRA_MARGIN: u32 = 32;

        // Cumulative counters for diagnostics
        let mut cum_tt_hits: u64 = 0;
        let mut cum_beta_cuts: u64 = 0;
        let mut cum_lmr_counter: u64 = 0;
        let mut cum_lmr_trials: u64 = 0;
        let mut stats_hint_exists: u64 = 0;
        let mut stats_hint_used: u64 = 0;

        self.evaluator.on_set_position(root);

        let mut final_lines: Option<SmallVec<[RootLine; 4]>> = None;
        let mut final_depth_reached: u8 = 0;
        let mut final_seldepth_reached: Option<u8> = None;
        let mut final_seldepth_raw: Option<u32> = None;
        for d in 1..=max_depth {
            if Self::should_stop(limits) {
                break;
            }
            if let Some(limit) = soft_deadline {
                if t0.elapsed() >= limit {
                    break;
                }
            }
            if let Some(limit) = hard_deadline {
                if t0.elapsed() >= limit {
                    break;
                }
            }
            let mut seldepth: u32 = 0;
            let throttle_ms = Self::currmove_throttle_ms();
            let mut last_currmove_emit = Instant::now();
            let prev_root_lines = final_lines.as_ref().map(|lines| lines.as_slice());
            // Build root move list for CurrMove events and basic ordering
            let mg = MoveGenerator::new();
            let Ok(list) = mg.generate_all(root) else {
                break;
            };
            // Root TT hint boost（存在すれば大ボーナス）
            let mut root_tt_hint_mv: Option<crate::shogi::Move> = None;
            if let Some(tt) = &self.tt {
                tt.prefetch_l2(root.zobrist_hash, root.side_to_move);
                if let Some(entry) = tt.probe(root.zobrist_hash, root.side_to_move) {
                    if let Some(ttm) = entry.get_move() {
                        root_tt_hint_mv = Some(ttm);
                    }
                }
            }
            let mut root_picker =
                ordering::RootPicker::new(root, list.as_slice(), root_tt_hint_mv, prev_root_lines);
            let mut root_moves: Vec<(crate::shogi::Move, i32)> =
                Vec::with_capacity(list.as_slice().len());
            while let Some((mv, key)) = root_picker.next() {
                root_moves.push((mv, key));
            }
            if root_moves.is_empty() {
                break;
            }
            let root_rank: Vec<crate::shogi::Move> = root_moves.iter().map(|(m, _)| *m).collect();

            let root_static_eval = self.evaluator.evaluate(root);
            let root_static_eval_i16 =
                root_static_eval.clamp(i16::MIN as i32, i16::MAX as i32) as i16;

            // MultiPV（逐次選抜）
            let k = limits.multipv.max(1) as usize;
            let mut excluded: SmallVec<[crate::shogi::Move; 32]> = SmallVec::new();
            let mut depth_lines: SmallVec<[RootLine; 4]> = SmallVec::new();

            // Counters aggregate across PVs at this depth
            let mut depth_tt_hits: u64 = 0;
            let mut depth_beta_cuts: u64 = 0;
            let mut depth_lmr_counter: u64 = 0;
            let mut depth_lmr_trials: u64 = 0;
            let mut _local_best_for_next_iter: Option<(crate::shogi::Move, i32)> = None;
            let mut depth_hint_exists: u64 = 0;
            let mut depth_hint_used: u64 = 0;
            let mut line_nodes_checkpoint = nodes;
            let mut line_time_checkpoint = t0.elapsed().as_millis() as u64;
            let mut shared_heur = Heuristics::default();
            for pv_idx in 1..=k {
                if Self::should_stop(limits) {
                    break;
                }
                if let Some(limit) = soft_deadline {
                    if t0.elapsed() >= limit {
                        break;
                    }
                }
                if let Some(limit) = hard_deadline {
                    if t0.elapsed() >= limit {
                        break;
                    }
                }
                // Aspiration window per PV head
                let mut alpha = if d == 1 {
                    i32::MIN / 2
                } else {
                    prev_score - ASP_DELTA0
                };
                let mut beta = if d == 1 {
                    i32::MAX / 2
                } else {
                    prev_score + ASP_DELTA0
                };
                let mut delta = ASP_DELTA0;
                let alpha_orig = alpha;
                let beta_orig = beta;

                // 検索用stack/heuristicsを初期化
                let mut stack = vec![SearchStack::default(); crate::search::constants::MAX_PLY + 1];
                let mut heur = std::mem::take(&mut shared_heur);
                let lmr_trials_checkpoint = heur.lmr_trials;
                let mut tt_hits: u64 = 0;
                let mut beta_cuts: u64 = 0;
                let mut lmr_counter: u64 = 0;
                let mut root_tt_hint_exists: u64 = 0;
                let mut root_tt_hint_used: u64 = 0;

                // 作業用root move配列（excludedを除外）
                let active_moves: Vec<(crate::shogi::Move, i32)> = root_moves
                    .iter()
                    .copied()
                    .filter(|(m, _)| !excluded.iter().any(|e| m.equals_without_piece_type(e)))
                    .collect();

                // 探索ループ（Aspiration）
                let mut local_best_mv = None;
                let mut local_best = i32::MIN / 2;
                loop {
                    if Self::should_stop(limits) {
                        break;
                    }
                    if let Some(limit) = soft_deadline {
                        if t0.elapsed() >= limit {
                            break;
                        }
                    }
                    if let Some(limit) = hard_deadline {
                        if t0.elapsed() >= limit {
                            break;
                        }
                    }
                    if active_moves.is_empty() {
                        break;
                    }
                    let (old_alpha, old_beta) = (alpha, beta);
                    // Root move loop with CurrMove events
                    for (idx, (mv, _)) in active_moves.iter().copied().enumerate() {
                        if Self::should_stop(limits) {
                            break;
                        }
                        if let Some(limit) = soft_deadline {
                            if t0.elapsed() >= limit {
                                break;
                            }
                        }
                        if let Some(limit) = hard_deadline {
                            if t0.elapsed() >= limit {
                                break;
                            }
                        }
                        if let Some(limit) = limits.time_limit() {
                            if t0.elapsed() >= limit {
                                break;
                            }
                        }
                        if let Some(cb) = info {
                            let emit = match throttle_ms {
                                None => true,
                                Some(ms) => {
                                    idx == 0
                                        || last_currmove_emit.elapsed() >= Duration::from_millis(ms)
                                }
                            };
                            if emit {
                                last_currmove_emit = Instant::now();
                                let number = root_rank
                                    .iter()
                                    .position(|x| x.equals_without_piece_type(&mv))
                                    .map(|pos| (pos as u32) + 1)
                                    .unwrap_or((idx as u32) + 1);
                                cb(InfoEvent::CurrMove { mv, number });
                            }
                        }
                        let mut child = root.clone();
                        let score = {
                            let _guard =
                                ordering::EvalMoveGuard::new(self.evaluator.as_ref(), root, mv);
                            child.do_move(mv);
                            if idx == 0 {
                                let mut search_ctx = SearchContext {
                                    limits,
                                    start_time: &t0,
                                    nodes: &mut nodes,
                                    seldepth: &mut seldepth,
                                };
                                let (sc, _) = self.alphabeta(
                                    pvs::ABArgs {
                                        pos: &child,
                                        depth: d - 1,
                                        alpha: -beta,
                                        beta: -alpha,
                                        ply: 1,
                                        is_pv: true,
                                        stack: &mut stack,
                                        heur: &mut heur,
                                        tt_hits: &mut tt_hits,
                                        beta_cuts: &mut beta_cuts,
                                        lmr_counter: &mut lmr_counter,
                                    },
                                    &mut search_ctx,
                                );
                                -sc
                            } else {
                                let mut search_ctx_nw = SearchContext {
                                    limits,
                                    start_time: &t0,
                                    nodes: &mut nodes,
                                    seldepth: &mut seldepth,
                                };
                                let (sc_nw, _) = self.alphabeta(
                                    pvs::ABArgs {
                                        pos: &child,
                                        depth: d - 1,
                                        alpha: -(alpha + 1),
                                        beta: -alpha,
                                        ply: 1,
                                        is_pv: false,
                                        stack: &mut stack,
                                        heur: &mut heur,
                                        tt_hits: &mut tt_hits,
                                        beta_cuts: &mut beta_cuts,
                                        lmr_counter: &mut lmr_counter,
                                    },
                                    &mut search_ctx_nw,
                                );
                                let mut s = -sc_nw;
                                if s > alpha && s < beta {
                                    let mut search_ctx_fw = SearchContext {
                                        limits,
                                        start_time: &t0,
                                        nodes: &mut nodes,
                                        seldepth: &mut seldepth,
                                    };
                                    let (sc_fw, _) = self.alphabeta(
                                        pvs::ABArgs {
                                            pos: &child,
                                            depth: d - 1,
                                            alpha: -beta,
                                            beta: -alpha,
                                            ply: 1,
                                            is_pv: true,
                                            stack: &mut stack,
                                            heur: &mut heur,
                                            tt_hits: &mut tt_hits,
                                            beta_cuts: &mut beta_cuts,
                                            lmr_counter: &mut lmr_counter,
                                        },
                                        &mut search_ctx_fw,
                                    );
                                    s = -sc_fw;
                                }
                                s
                            }
                        };
                        if score > local_best {
                            local_best = score;
                            local_best_mv = Some(mv);
                        }
                        if score > alpha {
                            alpha = score;
                        }
                        if alpha >= beta {
                            break; // fail-high
                        }
                    }

                    if Self::should_stop(limits) {
                        break;
                    }
                    if let Some(limit) = soft_deadline {
                        if t0.elapsed() >= limit {
                            break;
                        }
                    }
                    if let Some(limit) = hard_deadline {
                        if t0.elapsed() >= limit {
                            break;
                        }
                    }
                    if local_best <= old_alpha {
                        if let Some(cb) = info {
                            cb(InfoEvent::Aspiration {
                                outcome: crate::search::api::AspirationOutcome::FailLow,
                                old_alpha,
                                old_beta,
                                new_alpha: old_alpha.saturating_sub(2 * delta),
                                new_beta: old_beta,
                            });
                        }
                        alpha = old_alpha.saturating_sub(2 * delta).max(i32::MIN / 2);
                        beta = old_beta;
                        delta = (delta * 2).min(ASP_DELTA_MAX);
                        continue;
                    }
                    if local_best >= old_beta {
                        if let Some(cb) = info {
                            cb(InfoEvent::Aspiration {
                                outcome: crate::search::api::AspirationOutcome::FailHigh,
                                old_alpha,
                                old_beta,
                                new_alpha: old_alpha,
                                new_beta: old_beta.saturating_add(2 * delta),
                            });
                        }
                        alpha = old_alpha;
                        beta = old_beta.saturating_add(2 * delta).min(i32::MAX / 2);
                        delta = (delta * 2).min(ASP_DELTA_MAX);
                        continue;
                    }
                    break; // success within window
                }

                // Counters aggregate
                depth_tt_hits = depth_tt_hits.saturating_add(tt_hits);
                depth_beta_cuts = depth_beta_cuts.saturating_add(beta_cuts);
                depth_lmr_counter = depth_lmr_counter.saturating_add(lmr_counter);
                depth_lmr_trials = depth_lmr_trials
                    .saturating_add(heur.lmr_trials.saturating_sub(lmr_trials_checkpoint));
                shared_heur = heur;

                // 発火: Depth / Hashfull（深さ1回の発火で十分）
                if pv_idx == 1 {
                    if let Some(cb) = info {
                        let reported_sd =
                            seldepth.min(d as u32 + SELDEPTH_EXTRA_MARGIN).min(u8::MAX as u32);
                        cb(InfoEvent::Depth {
                            depth: d as u32,
                            seldepth: reported_sd,
                        });
                        if let Some(tt) = &self.tt {
                            let hf = tt.hashfull_permille() as u32;
                            cb(InfoEvent::Hashfull(hf));
                        }
                    }
                }

                // PV 行の生成と発火
                if let Some(m) = local_best_mv {
                    // 次反復のAspiration用に pv_idx==1 を採用
                    if pv_idx == 1 {
                        best = Some(m);
                        best_score = local_best;
                        prev_score = local_best;
                        if let Some(hint) = root_tt_hint_mv {
                            root_tt_hint_exists = 1;
                            if m.equals_without_piece_type(&hint) {
                                root_tt_hint_used = 1;
                            }
                        }
                        depth_hint_exists = root_tt_hint_exists;
                        depth_hint_used = root_tt_hint_used;
                        _local_best_for_next_iter = Some((m, local_best));
                    }
                    // 可能ならTTからPVを復元し、だめなら軽量再探索へフォールバック
                    let mut pv = self.reconstruct_root_pv_from_tt(root, d, m).unwrap_or_default();
                    if pv.is_empty() {
                        let pv_ex = self.extract_pv(root, d, m, limits, &mut nodes);
                        if pv_ex.is_empty() {
                            pv.push(m);
                        } else {
                            pv = pv_ex;
                        }
                    }
                    let elapsed_ms_total = t0.elapsed().as_millis() as u64;
                    let current_nodes = nodes;
                    let line_nodes = current_nodes.saturating_sub(line_nodes_checkpoint);
                    let line_time_ms = elapsed_ms_total.saturating_sub(line_time_checkpoint);
                    let line_nps = if line_time_ms > 0 {
                        Some((line_nodes.saturating_mul(1000)).saturating_div(line_time_ms.max(1)))
                    } else {
                        Some(0)
                    };
                    let bound = if local_best <= alpha_orig {
                        NodeType::UpperBound
                    } else if local_best >= beta_orig {
                        NodeType::LowerBound
                    } else {
                        NodeType::Exact
                    };
                    let line = RootLine {
                        multipv_index: pv_idx as u8,
                        root_move: m,
                        score_internal: local_best,
                        score_cp: local_best,
                        bound,
                        depth: d as u32,
                        seldepth: Some(
                            seldepth.min(d as u32 + SELDEPTH_EXTRA_MARGIN).min(u8::MAX as u32)
                                as u8,
                        ),
                        pv,
                        nodes: Some(line_nodes),
                        time_ms: Some(line_time_ms),
                        nps: line_nps,
                        exact_exhausted: false,
                        exhaust_reason: None,
                        mate_distance: None,
                    };
                    let line_arc = Arc::new(line);
                    if let Some(cb) = info {
                        cb(InfoEvent::PV {
                            line: Arc::clone(&line_arc),
                        });
                    }
                    depth_lines.push(match Arc::try_unwrap(line_arc) {
                        Ok(line) => line,
                        Err(arc) => (*arc).clone(),
                    });
                    // TT保存は 1行目のみ（Exact, PV=true）
                    if pv_idx == 1 {
                        if let (Some(tt), Some(best_mv_root)) = (&self.tt, best) {
                            let node_type = NodeType::Exact;
                            let store_score =
                                crate::search::common::adjust_mate_score_for_tt(best_score, 0)
                                    .clamp(i16::MIN as i32, i16::MAX as i32)
                                    as i16;
                            let mut args = crate::search::tt::TTStoreArgs::new(
                                root.zobrist_hash,
                                Some(best_mv_root),
                                store_score,
                                root_static_eval_i16,
                                d as u8,
                                node_type,
                                root.side_to_move,
                            );
                            args.is_pv = true;
                            tt.store(args);
                        }
                    }
                    // 除外へ追加
                    excluded.push(m);
                    line_nodes_checkpoint = current_nodes;
                    line_time_checkpoint = elapsed_ms_total;
                } else {
                    // 局面が詰み/手なし等でPVが取れない → 打ち切り
                    break;
                }
            }

            // 深さ集計を累積
            cum_tt_hits = cum_tt_hits.saturating_add(depth_tt_hits);
            cum_beta_cuts = cum_beta_cuts.saturating_add(depth_beta_cuts);
            cum_lmr_counter = cum_lmr_counter.saturating_add(depth_lmr_counter);
            cum_lmr_trials = cum_lmr_trials.saturating_add(depth_lmr_trials);

            // 反復ごとのrootヒント統計（最終反復で掲載）
            stats_hint_exists = depth_hint_exists;
            stats_hint_used = depth_hint_used;
            // この深さのMultiPV行を最終結果候補として保持
            final_lines = Some(depth_lines);
            final_depth_reached = d as u8;
            let capped_seldepth =
                seldepth.min(d as u32 + SELDEPTH_EXTRA_MARGIN).min(u8::MAX as u32) as u8;
            final_seldepth_reached = Some(capped_seldepth);
            final_seldepth_raw = Some(seldepth);

            let mut lead_ms = 10u64;
            if let Some(hard) = hard_deadline {
                if let Some(soft) = soft_deadline {
                    if hard > soft {
                        let diff = hard.as_millis().saturating_sub(soft.as_millis()) as u64;
                        if diff > 0 {
                            lead_ms = lead_ms.max(diff);
                        }
                    }
                }

                if t0.elapsed() + Duration::from_millis(lead_ms) >= hard {
                    break;
                }

                continue;
            }

            if let Some(limit) = limits.time_limit() {
                if t0.elapsed() + Duration::from_millis(lead_ms) >= limit {
                    break;
                }
            }
        }
        // stats は最終反復の集計値を使う
        let mut stats = SearchStats {
            nodes,
            ..Default::default()
        };
        stats.elapsed = t0.elapsed();
        stats.depth = final_depth_reached;
        stats.seldepth = final_seldepth_reached;
        stats.raw_seldepth = final_seldepth_raw.map(|v| v.min(u16::MAX as u32) as u16);
        stats.tt_hits = Some(cum_tt_hits);
        stats.lmr_count = Some(cum_lmr_counter);
        stats.lmr_trials = Some(cum_lmr_trials);
        stats.root_fail_high_count = Some(cum_beta_cuts);
        stats.root_tt_hint_exists = Some(stats_hint_exists);
        stats.root_tt_hint_used = Some(stats_hint_used);
        if let Some(first_line) = final_lines.as_ref().and_then(|lines| lines.first()) {
            stats.pv = first_line.pv.iter().copied().collect();
        }
        let mut result = SearchResult::new(best, best_score, stats);
        if let Some(lines) = final_lines {
            result.lines = Some(lines);
            result.refresh_summary();
        }
        if let Some(tt) = &self.tt {
            result.hashfull = tt.hashfull_permille() as u32;
        }
        result
    }
}

impl<E: Evaluator + Send + Sync + 'static> SearcherBackend for ClassicBackend<E> {
    fn start_async(
        self: Arc<Self>,
        root: Position,
        mut limits: SearchLimits,
        info: Option<InfoEventCallback>,
        active_counter: Arc<AtomicUsize>,
    ) -> BackendSearchTask {
        let stop_flag =
            limits.stop_flag.get_or_insert_with(|| Arc::new(AtomicBool::new(false))).clone();
        active_counter.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        let backend = self;
        let info_cb = info;
        let handle = thread::Builder::new()
            .name("classic-backend-search".into())
            .spawn({
                let counter = Arc::clone(&active_counter);
                move || {
                    struct Guard(Arc<AtomicUsize>);
                    impl Drop for Guard {
                        fn drop(&mut self) {
                            self.0.fetch_sub(1, Ordering::SeqCst);
                        }
                    }
                    let _guard = Guard(counter);
                    let result = backend.iterative(&root, &limits, info_cb.as_ref());
                    let _ = tx.send(result);
                }
            })
            .expect("spawn classic backend search thread");
        BackendSearchTask::new(stop_flag, rx, handle)
    }

    fn think_blocking(
        &self,
        root: &Position,
        limits: &SearchLimits,
        info: Option<InfoEventCallback>,
    ) -> SearchResult {
        self.iterative(root, limits, info.as_ref())
    }

    fn update_threads(&self, _n: usize) {}
    fn update_hash(&self, _mb: usize) {
        // Engine側でshared_tt再生成＋Backend再バインド方針のため未使用
    }
}
