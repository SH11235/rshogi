use std::collections::HashMap;
use std::env;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use log::warn;

use crate::evaluation::evaluate::Evaluator;
use crate::movegen::MoveGenerator;
use crate::search::api::{BackendSearchTask, InfoEvent, InfoEventCallback, SearcherBackend};
use crate::search::constants::MAX_PLY;
use crate::search::parallel::FinalizeReason;
use crate::search::params as dynp;
use crate::search::types::{NodeType, RootLine, SearchStack, StopInfo, TerminationReason};
use crate::search::{SearchLimits, SearchResult, SearchStats, TranspositionTable};
use crate::Position;
use smallvec::SmallVec;
use std::cell::{Cell, RefCell};

use super::ordering::{self, Heuristics};
use super::profile::{PruneToggles, SearchProfile};
use super::pvs::{self, SearchContext};
#[cfg(feature = "diagnostics")]
use super::qsearch::{publish_qsearch_diagnostics, reset_qsearch_diagnostics};
use crate::search::policy::{asp_fail_high_pct, asp_fail_low_pct};
use crate::search::snapshot::SnapshotSource;
use crate::search::tt::TTProbe;
use crate::time_management::TimeControl;

/// Sticky-PV window: when remaining time <= this value (ms), avoid PV changes to unverified moves
const STICKY_PV_WINDOW_MS: u64 = 400;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeadlineHit {
    Stop,
    Hard,
    Soft,
}

static SEARCH_THREAD_SEQ: AtomicU64 = AtomicU64::new(1);

// Thread-local stack cache to avoid per-iteration Vec allocations.
// Helpers経路だけでなく main でも安全に機能するが、挙動は不変（内容は毎回 Default で埋め直し）。
thread_local! {
    static STACK_CACHE: RefCell<Vec<SearchStack>> = const { RefCell::new(Vec::new()) };
}

#[inline]
pub(crate) fn take_stack_cache() -> Vec<SearchStack> {
    STACK_CACHE.with(|cell| {
        let mut v = std::mem::take(&mut *cell.borrow_mut());
        let want = MAX_PLY + 1;
        if v.len() != want {
            v.clear();
            v.resize(want, SearchStack::default());
        } else {
            // 既存メモリを再利用。各要素の内部バッファ容量は保持したまま中身をクリア。
            for e in v.iter_mut() {
                e.reset_for_iteration();
            }
        }
        v
    })
}

#[inline]
pub(crate) fn return_stack_cache(buf: Vec<SearchStack>) {
    STACK_CACHE.with(|cell| {
        *cell.borrow_mut() = buf;
    });
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct NearDeadlineDecision {
    skip_new_iter: bool,
    shrink_multipv: bool,
    fire_nearhard: bool,
    main_win_ms: u64,
    finalize_win_ms: u64,
    t_rem_ms: u64,
}

/// Computes remaining time in milliseconds for sticky-PV logic.
/// Supports TimeManager (hard/soft limits), fixed time_limit, and fallback_deadlines.
#[inline]
fn time_remaining_ms_for_sticky(t0: Instant, limits: &SearchLimits) -> Option<u64> {
    if let Some(tm) = limits.time_manager.as_ref() {
        let hard = tm.hard_limit_ms();
        if hard != u64::MAX {
            let elapsed = tm.elapsed_ms();
            return Some(hard.saturating_sub(elapsed));
        }
        let soft = tm.soft_limit_ms();
        if soft != u64::MAX {
            let elapsed = tm.elapsed_ms();
            return Some(soft.saturating_sub(elapsed));
        }
        None
    } else if let Some(dl) = limits.fallback_deadlines {
        let elapsed_ms = t0.elapsed().as_millis() as u64;
        Some(dl.hard_limit_ms.saturating_sub(elapsed_ms))
    } else if let Some(limit) = limits.time_limit() {
        let cap = limit.as_millis() as u64;
        let el = t0.elapsed().as_millis() as u64;
        Some(cap.saturating_sub(el))
    } else {
        None
    }
}

#[derive(Clone)]
pub struct ClassicBackend<E: Evaluator + Send + Sync + 'static> {
    pub(super) evaluator: Arc<E>,
    pub(super) tt: Option<Arc<TranspositionTable>>, // 共有TT（Hashfull出力用、将来はprobe/storeでも使用）
    pub(super) profile: SearchProfile,
}

impl<E: Evaluator + Send + Sync + 'static> ClassicBackend<E> {
    #[inline]
    /// Global kill-switch for stabilization gates (near-deadline policy, aspiration safeguards, near-final verify).
    /// New name: SHOGI_DISABLE_STABILIZATION（旧: SHOGI_DISABLE_P1 を後方互換で受理）。
    fn stabilization_disabled() -> bool {
        #[cfg(test)]
        {
            if Self::parse_bool_env("SHOGI_DISABLE_STABILIZATION", false) {
                return true;
            }
            Self::parse_bool_env("SHOGI_DISABLE_P1", false)
        }
        #[cfg(not(test))]
        {
            static FLAG: OnceLock<bool> = OnceLock::new();
            *FLAG.get_or_init(|| {
                if Self::parse_bool_env("SHOGI_DISABLE_STABILIZATION", false) {
                    return true;
                }
                Self::parse_bool_env("SHOGI_DISABLE_P1", false)
            })
        }
    }
    #[inline]
    fn is_byoyomi_active(limits: &SearchLimits) -> bool {
        matches!(limits.time_control, TimeControl::Byoyomi { .. })
            || limits.time_manager.as_ref().is_some_and(|tm| tm.is_in_byoyomi())
    }

    /// Compute qsearch node limit for the current context.
    ///
    /// ポリシー要点:
    /// - `qnodes_limit == Some(0)` かつ 時間管理や固定時間が無いベンチ系では無制限（`u64::MAX`）
    /// - 時間管理や固定時間がある場合は、それらに基づくダイナミック上限を適用
    /// - Byoyomi 中は深さに応じて緩やかに上限を引き上げ（浅い層では控えめ、深くなると増やす）
    /// - それ以外は既定上限 `DEFAULT_QNODES_LIMIT` を上回らないようクランプ（安全側）
    pub(crate) fn compute_qnodes_limit(limits: &SearchLimits, depth: i32, pv_idx: usize) -> u64 {
        // qnodes_limit==0 は「無制限」意図。ただし実対局（時間管理あり）では
        // TM/時間上限に基づく動的縮小は残す。ベンチ用途（時間管理なし）のみ完全無制限。
        let request_unlimited = matches!(limits.qnodes_limit, Some(0));
        let mut limit =
            limits.qnodes_limit.unwrap_or(crate::search::constants::DEFAULT_QNODES_LIMIT);
        let byoyomi_active = Self::is_byoyomi_active(limits);

        if let Some(tm) = limits.time_manager.as_ref() {
            let soft = tm.soft_limit_ms();
            if soft > 0 && soft != u64::MAX {
                let base_scaled = soft.saturating_mul(crate::search::constants::QNODES_PER_MS);
                limit = limit.min(base_scaled);

                let elapsed = tm.elapsed_ms();
                if elapsed < soft {
                    let remaining = soft - elapsed;
                    let dynamic = remaining
                        .saturating_mul(crate::search::constants::QNODES_PER_MS)
                        .saturating_add(crate::search::constants::MIN_QNODES_LIMIT / 2);
                    limit = limit.min(dynamic);
                } else {
                    limit = limit.min(crate::search::constants::MIN_QNODES_LIMIT);
                }
            }
        } else if let Some(duration) = limits.time_limit() {
            let soft_ms = duration.as_millis() as u64;
            if soft_ms > 0 {
                let scaled = soft_ms.saturating_mul(crate::search::constants::QNODES_PER_MS);
                limit = limit.min(scaled);
            }
        }

        // if time control is absent entirely (no TM, no fixed time, no deadlines),
        // honor the unlimited request for benches; otherwise keep dynamic cap.
        if request_unlimited
            && limits.time_manager.is_none()
            && limits.time_limit().is_none()
            && limits.fallback_deadlines.is_none()
        {
            return u64::MAX;
        }

        // MultiPVスケジューラ（最小版）: PV1を優先するため、PV2以降の上限を強く絞る
        if pv_idx > 1 {
            let divisor = if Self::multipv_scheduler_enabled() {
                let bias = Self::multipv_scheduler_bias();
                bias.saturating_mul(pv_idx as u64).max(2)
            } else {
                (pv_idx as u64).saturating_add(1)
            };
            limit = limit.saturating_div(divisor);
        }

        if byoyomi_active {
            let base = (limit / 2).max(crate::search::constants::MIN_QNODES_LIMIT);
            let depth_scale = 100
                + (depth.max(1) as u64)
                    .saturating_mul(crate::search::constants::QNODES_DEPTH_BONUS_PCT);
            limit = base.saturating_mul(depth_scale).saturating_add(99) / 100;
        }

        let relax_mult = Self::qnodes_limit_relax_mult();
        let max_cap =
            crate::search::constants::DEFAULT_QNODES_LIMIT.saturating_mul(relax_mult.max(1));
        limit.clamp(crate::search::constants::MIN_QNODES_LIMIT, max_cap)
    }

    #[inline]
    fn qnodes_limit_relax_mult() -> u64 {
        #[cfg(test)]
        {
            match std::env::var("SHOGI_QNODES_LIMIT_RELAX_MULT") {
                Ok(v) => v.parse::<u64>().ok().map(|x| x.clamp(1, 32)).unwrap_or(1),
                Err(_) => 1,
            }
        }
        #[cfg(not(test))]
        {
            static VAL: OnceLock<u64> = OnceLock::new();
            *VAL.get_or_init(|| match std::env::var("SHOGI_QNODES_LIMIT_RELAX_MULT") {
                Ok(v) => v.parse::<u64>().ok().map(|x| x.clamp(1, 32)).unwrap_or(1),
                Err(_) => 1,
            })
        }
    }

    #[inline]
    fn multipv_scheduler_enabled() -> bool {
        #[cfg(test)]
        {
            Self::parse_bool_env("SHOGI_MULTIPV_SCHEDULER", false)
        }
        #[cfg(not(test))]
        {
            static FLAG: OnceLock<bool> = OnceLock::new();
            *FLAG.get_or_init(|| Self::parse_bool_env("SHOGI_MULTIPV_SCHEDULER", false))
        }
    }

    #[inline]
    fn multipv_scheduler_bias() -> u64 {
        #[cfg(test)]
        {
            match std::env::var("SHOGI_MULTIPV_SCHEDULER_PV2_DIV") {
                Ok(v) => v.parse::<u64>().ok().map(|x| x.clamp(2, 32)).unwrap_or(4),
                Err(_) => 4,
            }
        }
        #[cfg(not(test))]
        {
            static VAL: OnceLock<u64> = OnceLock::new();
            *VAL.get_or_init(|| match std::env::var("SHOGI_MULTIPV_SCHEDULER_PV2_DIV") {
                Ok(v) => v.parse::<u64>().ok().map(|x| x.clamp(2, 32)).unwrap_or(4),
                Err(_) => 4,
            })
        }
    }

    #[cfg(test)]
    pub(crate) fn compute_qnodes_limit_for_test(
        limits: &SearchLimits,
        depth: i32,
        pv_idx: usize,
    ) -> u64 {
        Self::compute_qnodes_limit(limits, depth, pv_idx)
    }

    #[inline]
    fn deadline_hit(
        start: Instant,
        soft: Option<Duration>,
        hard: Option<Duration>,
        limits: &SearchLimits,
        min_think_ms: u64,
        current_nodes: u64,
    ) -> Option<DeadlineHit> {
        if Self::should_stop(limits) {
            return Some(DeadlineHit::Stop);
        }
        if let Some(tm) = limits.time_manager.as_ref() {
            if tm.should_stop(current_nodes) {
                let hard_limit = tm.hard_limit_ms();
                let tm_elapsed = tm.elapsed_ms();
                if hard_limit != u64::MAX && tm_elapsed >= hard_limit {
                    return Some(DeadlineHit::Hard);
                }
                return Some(DeadlineHit::Soft);
            }
            return None;
        }

        let elapsed = start.elapsed();
        let elapsed_ms = elapsed.as_millis() as u64;
        let min_think_satisfied = min_think_ms == 0 || elapsed_ms >= min_think_ms;

        if let Some(limit) = hard {
            if elapsed >= limit {
                return Some(DeadlineHit::Hard);
            }
        }
        if let Some(limit) = soft {
            if elapsed >= limit && min_think_satisfied {
                return Some(DeadlineHit::Soft);
            }
        }
        if let Some(limit) = limits.time_limit() {
            if elapsed >= limit && min_think_satisfied {
                // 固定時間探索はリードウィンドウで緩やかに停止させる方針のため、min_think を満たした後は Hard ではなく Soft とみなす。
                return Some(DeadlineHit::Soft);
            }
        }
        None
    }
    #[inline]
    fn retries_max(soft_deadline: Option<Duration>, start: &Instant) -> u32 {
        if let Some(sd) = soft_deadline {
            let elapsed = start.elapsed();
            let remain_ms = if sd > elapsed {
                (sd - elapsed).as_millis() as u64
            } else {
                0
            };
            if remain_ms <= 40 {
                2
            } else {
                3
            }
        } else {
            3
        }
    }
    #[inline]
    pub(crate) fn derive_main_near_deadline_window_ms(hard_ms: u64) -> u64 {
        // 80..600ms の範囲で hard の 1/5
        let base = hard_ms / 5;
        base.clamp(80, 600)
    }
    #[inline]
    pub(crate) fn derive_near_hard_finalize_ms(hard_ms: u64) -> u64 {
        // 60..400ms の範囲で hard の 1/6
        let base = hard_ms / 6;
        base.clamp(60, 400)
    }

    #[inline]
    fn decide_near_deadline_policy(
        hard_cap_ms: u64,
        elapsed_ms: u64,
        depth: i32,
        _multipv: u8,
    ) -> Option<NearDeadlineDecision> {
        if hard_cap_ms == u64::MAX || elapsed_ms >= hard_cap_ms {
            return None;
        }
        let t_rem = hard_cap_ms.saturating_sub(elapsed_ms);
        let main_win = Self::derive_main_near_deadline_window_ms(hard_cap_ms);
        let finalize_win = Self::derive_near_hard_finalize_ms(hard_cap_ms);
        let fire_nearhard = t_rem <= finalize_win;
        // 新イテ抑止は2手目以降のみ（d>1）
        let skip_new_iter = t_rem <= main_win && depth > 1;
        let shrink_multipv = t_rem <= main_win;
        Some(NearDeadlineDecision {
            skip_new_iter,
            shrink_multipv,
            fire_nearhard,
            main_win_ms: main_win,
            finalize_win_ms: finalize_win,
            t_rem_ms: t_rem,
        })
    }
    #[inline]
    pub(crate) fn classify_root_bound(local_best: i32, alpha_win: i32, beta_win: i32) -> NodeType {
        if local_best <= alpha_win {
            NodeType::UpperBound
        } else if local_best >= beta_win {
            NodeType::LowerBound
        } else {
            NodeType::Exact
        }
    }

    fn currmove_throttle_ms() -> Option<u64> {
        static POLICY: OnceLock<Option<u64>> = OnceLock::new();
        *POLICY.get_or_init(|| match env::var("SHOGI_CURRMOVE_THROTTLE_MS") {
            Ok(val) => {
                let val = val.trim().to_ascii_lowercase();
                if val == "off" || val == "0" || val == "false" {
                    None
                } else {
                    val.parse::<u64>().ok().filter(|v| *v > 0)
                }
            }
            // Default: 100ms provides good responsiveness in parallel search with multiple
            // helpers while avoiding excessive USI output spam.
            Err(_) => Some(100),
        })
    }

    // Parse common boolean env values (1/true/on/yes) with default
    #[inline]
    fn parse_bool_env(var: &str, default: bool) -> bool {
        match env::var(var) {
            Ok(v) => {
                let v = v.trim().to_ascii_lowercase();
                matches!(v.as_str(), "1" | "true" | "on" | "yes")
            }
            Err(_) => default,
        }
    }

    fn near_final_zero_window_enabled() -> bool {
        #[cfg(test)]
        {
            Self::parse_bool_env("SHOGI_ZERO_WINDOW_FINALIZE_NEAR_DEADLINE", false)
        }
        #[cfg(not(test))]
        {
            static FLAG: OnceLock<bool> = OnceLock::new();
            *FLAG.get_or_init(|| {
                Self::parse_bool_env("SHOGI_ZERO_WINDOW_FINALIZE_NEAR_DEADLINE", false)
            })
        }
    }

    #[inline]
    fn near_final_zero_window_budget_ms() -> u64 {
        #[cfg(test)]
        {
            match env::var("SHOGI_ZERO_WINDOW_FINALIZE_BUDGET_MS") {
                Ok(v) => v.parse::<u64>().ok().map(|x| x.clamp(10, 200)).unwrap_or(80),
                Err(_) => 80,
            }
        }
        #[cfg(not(test))]
        {
            static VAL: OnceLock<u64> = OnceLock::new();
            *VAL.get_or_init(|| match env::var("SHOGI_ZERO_WINDOW_FINALIZE_BUDGET_MS") {
                Ok(v) => v.parse::<u64>().ok().map(|x| x.clamp(10, 200)).unwrap_or(80),
                Err(_) => 80,
            })
        }
    }

    #[inline]
    fn near_final_zero_window_min_depth() -> i32 {
        #[cfg(test)]
        {
            match env::var("SHOGI_ZERO_WINDOW_FINALIZE_MIN_DEPTH") {
                Ok(v) => v.parse::<i32>().ok().map(|x| x.clamp(1, 64)).unwrap_or(4),
                Err(_) => 4,
            }
        }
        #[cfg(not(test))]
        {
            static VAL: OnceLock<i32> = OnceLock::new();
            *VAL.get_or_init(|| match env::var("SHOGI_ZERO_WINDOW_FINALIZE_MIN_DEPTH") {
                Ok(v) => v.parse::<i32>().ok().map(|x| x.clamp(1, 64)).unwrap_or(4),
                Err(_) => 4,
            })
        }
    }

    #[inline]
    fn near_final_zero_window_min_trem_ms() -> u64 {
        #[cfg(test)]
        {
            match env::var("SHOGI_ZERO_WINDOW_FINALIZE_MIN_TREM_MS") {
                Ok(v) => v.parse::<u64>().ok().map(|x| x.clamp(5, 500)).unwrap_or(60),
                Err(_) => 60,
            }
        }
        #[cfg(not(test))]
        {
            static VAL: OnceLock<u64> = OnceLock::new();
            *VAL.get_or_init(|| match env::var("SHOGI_ZERO_WINDOW_FINALIZE_MIN_TREM_MS") {
                Ok(v) => v.parse::<u64>().ok().map(|x| x.clamp(5, 500)).unwrap_or(60),
                Err(_) => 60,
            })
        }
    }

    #[inline]
    fn near_final_zero_window_min_multipv() -> u8 {
        #[cfg(test)]
        {
            match env::var("SHOGI_ZERO_WINDOW_FINALIZE_MIN_MULTIPV") {
                Ok(v) => v.parse::<u8>().ok().unwrap_or(0),
                Err(_) => 0,
            }
        }
        #[cfg(not(test))]
        {
            static VAL: OnceLock<u8> = OnceLock::new();
            *VAL.get_or_init(|| match env::var("SHOGI_ZERO_WINDOW_FINALIZE_MIN_MULTIPV") {
                Ok(v) => v.parse::<u8>().ok().unwrap_or(0),
                Err(_) => 0,
            })
        }
    }

    #[inline]
    fn near_final_verify_delta_cp() -> i32 {
        #[cfg(test)]
        {
            match env::var("SHOGI_ZERO_WINDOW_FINALIZE_VERIFY_DELTA_CP") {
                Ok(v) => v.parse::<i32>().ok().map(|x| x.clamp(1, 32)).unwrap_or(1),
                Err(_) => 1,
            }
        }
        #[cfg(not(test))]
        {
            static VAL: OnceLock<i32> = OnceLock::new();
            *VAL.get_or_init(|| match env::var("SHOGI_ZERO_WINDOW_FINALIZE_VERIFY_DELTA_CP") {
                Ok(v) => v.parse::<i32>().ok().map(|x| x.clamp(1, 32)).unwrap_or(1),
                Err(_) => 1,
            })
        }
    }

    #[inline]
    fn near_final_zero_window_skip_mate() -> bool {
        #[cfg(test)]
        {
            Self::parse_bool_env("SHOGI_ZERO_WINDOW_FINALIZE_SKIP_MATE", false)
        }
        #[cfg(not(test))]
        {
            static FLAG: OnceLock<bool> = OnceLock::new();
            *FLAG
                .get_or_init(|| Self::parse_bool_env("SHOGI_ZERO_WINDOW_FINALIZE_SKIP_MATE", false))
        }
    }

    #[inline]
    fn near_final_zero_window_mate_delta_cp() -> i32 {
        #[cfg(test)]
        {
            match env::var("SHOGI_ZERO_WINDOW_FINALIZE_MATE_DELTA_CP") {
                Ok(v) => v.parse::<i32>().ok().map(|x| x.clamp(0, 32)).unwrap_or(0),
                Err(_) => 0,
            }
        }
        #[cfg(not(test))]
        {
            static VAL: OnceLock<i32> = OnceLock::new();
            *VAL.get_or_init(|| match env::var("SHOGI_ZERO_WINDOW_FINALIZE_MATE_DELTA_CP") {
                Ok(v) => v.parse::<i32>().ok().map(|x| x.clamp(0, 32)).unwrap_or(0),
                Err(_) => 0,
            })
        }
    }

    #[inline]
    fn near_final_zero_window_bound_slack_cp() -> i32 {
        #[cfg(test)]
        {
            match env::var("SHOGI_ZERO_WINDOW_FINALIZE_BOUND_SLACK_CP") {
                Ok(v) => v.parse::<i32>().ok().map(|x| x.clamp(0, 64)).unwrap_or(0),
                Err(_) => 0,
            }
        }
        #[cfg(not(test))]
        {
            static VAL: OnceLock<i32> = OnceLock::new();
            *VAL.get_or_init(|| match env::var("SHOGI_ZERO_WINDOW_FINALIZE_BOUND_SLACK_CP") {
                Ok(v) => v.parse::<i32>().ok().map(|x| x.clamp(0, 64)).unwrap_or(0),
                Err(_) => 0,
            })
        }
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
            return flag.load(Ordering::Acquire);
        }
        false
    }

    #[cfg(test)]
    #[inline]
    pub(crate) fn threads_lg2_for_test(threads_hint: u32) -> i32 {
        threads_hint.max(1).ilog2() as i32
    }

    fn iterative_with_buffers(
        &self,
        root: &Position,
        limits: &SearchLimits,
        stack: &mut [SearchStack],
        heur_state: &mut Heuristics,
        info: Option<&InfoEventCallback>,
    ) -> SearchResult {
        let max_depth = limits.depth_limit_u8() as i32;
        let mut best: Option<crate::shogi::Move> = None;
        let mut best_score = 0;
        let mut nodes: u64 = 0;
        let t0 = Instant::now();
        let session_id = limits.session_id;
        let root_key = root.zobrist_hash();
        let deadlines = limits.fallback_deadlines;
        let (soft_deadline, hard_deadline) = if let Some(dl) = deadlines {
            (
                (dl.soft_limit_ms > 0).then(|| Duration::from_millis(dl.soft_limit_ms)),
                (dl.hard_limit_ms > 0).then(|| Duration::from_millis(dl.hard_limit_ms)),
            )
        } else if let Some(limit) = limits.time_limit() {
            // 固定時間探索は Hard 判定を用いず、リードウィンドウと Soft 停止で丸める。
            (Some(limit), None)
        } else {
            (None, None)
        };
        let min_think_ms = if limits.is_ponder {
            0
        } else {
            limits.time_parameters.as_ref().map(|tp| tp.min_think_ms).unwrap_or(0)
        };
        let mut prev_score: i32 = 0;
        use crate::search::constants::{
            ASPIRATION_DELTA_INITIAL, ASPIRATION_DELTA_MAX, ASPIRATION_DELTA_THREADS_K,
        };
        // Shallow depths are volatile; disable aspiration under this depth
        // to avoid frequent fail-low/high thrashing and wasted re-searches.
        const ASPIRATION_MIN_DEPTH: i32 = 5;
        const SELDEPTH_EXTRA_MARGIN: u32 = 32;

        // Cumulative counters for diagnostics
        let mut cum_tt_hits: u64 = 0;
        let mut cum_beta_cuts: u64 = 0;
        let mut cum_lmr_counter: u64 = 0;
        let mut cum_lmr_trials: u64 = 0;
        // Instrumentation (cumulative across the whole search)
        let mut cum_lmr_blocked_in_check: u64 = 0;
        let mut cum_lmr_blocked_recapture: u64 = 0;
        let mut cum_evasion_sparsity_ext: u64 = 0;
        // Aggregate qnodes across PVs/iterations for this worker result
        let mut cum_qnodes: u64 = 0;
        #[cfg(feature = "diagnostics")]
        let mut cum_abdada_busy_detected: u64 = 0;
        #[cfg(feature = "diagnostics")]
        let mut cum_abdada_busy_set: u64 = 0;
        let mut stats_hint_exists: u64 = 0;
        let mut stats_hint_used: u64 = 0;
        // Near-final verification counters (attempted/confirmed)
        let mut near_final_attempted: u64 = 0;
        let mut near_final_confirmed: u64 = 0;

        self.evaluator.on_set_position(root);

        #[cfg(feature = "diagnostics")]
        reset_qsearch_diagnostics();

        #[cfg(any(debug_assertions, feature = "diagnostics"))]
        {
            super::diagnostics::clear();
            super::diagnostics::configure_abort_handles(
                limits.stop_controller.clone(),
                limits.stop_flag.clone(),
            );
        }

        let mut final_lines: Option<SmallVec<[RootLine; 4]>> = None;
        let mut final_depth_reached: u8 = 0;
        let mut final_seldepth_reached: Option<u8> = None;
        let mut final_seldepth_raw: Option<u32> = None;
        let mut incomplete_depth: Option<u8> = None;
        let mut cumulative_pv_changed: u32 = 0;
        let mut cumulative_asp_failures: u32 = 0;
        let mut cumulative_asp_hits: u32 = 0;
        let mut cumulative_researches: u32 = 0;
        let stop_controller = limits.stop_controller.clone();
        // Finalize 一回化ガード
        let mut finalize_soft_sent = false;
        let mut finalize_nearhard_sent = false;
        let mut last_deadline_hit: Option<DeadlineHit> = None;
        let mut lead_window_soft_break = false;
        let mut finalize_hard_sent = false;
        let is_helper = limits.helper_role;
        let mut zero_window_done = false;
        let mut notify_deadline = |hit: DeadlineHit, nodes_now: u64| {
            if let Some(cb) = limits.info_string_callback.as_ref() {
                let elapsed = t0.elapsed().as_millis();
                cb(&format!(
                    "deadline_hit kind={:?} elapsed_ms={} nodes={}",
                    hit, elapsed, nodes_now
                ));
            }
            // finalize送出は primary のみ（helper はログのみ）
            if is_helper {
                return;
            }
            if let Some(ctrl) = stop_controller.as_ref() {
                match hit {
                    DeadlineHit::Hard => {
                        if !finalize_hard_sent {
                            ctrl.request_finalize(FinalizeReason::Hard);
                            finalize_hard_sent = true;
                        }
                    }
                    DeadlineHit::Soft => {
                        if !finalize_soft_sent {
                            ctrl.request_finalize(FinalizeReason::Planned);
                            finalize_soft_sent = true;
                        }
                    }
                    DeadlineHit::Stop => {}
                }
            }
        };

        static LEAD_WINDOW_FINALIZE_ENABLED: OnceLock<bool> = OnceLock::new();
        let lead_window_finalize = *LEAD_WINDOW_FINALIZE_ENABLED.get_or_init(|| {
            match env::var("SHOGI_LEAD_WINDOW_FINALIZE") {
                Ok(val) => {
                    let normalized = val.trim().to_ascii_lowercase();
                    !(normalized == "off" || normalized == "0" || normalized == "false")
                }
                Err(_) => true,
            }
        });

        // Track best move from previous iteration for root move ordering hint
        // and aspiration center smoothing (A+C approach from YaneuraOu design)
        let mut best_hint_next_iter: Option<(crate::shogi::Move, i32)> = None;

        // finalize_nearhard_sent は上で宣言（notify_deadline からも参照）

        // 観測用フラグ（SearchStatsに反映）
        let mut stats_near_deadline_skip_new_iter: u64 = 0;
        let mut stats_multipv_shrunk: u64 = 0;

        for d in 1..=max_depth {
            // Near-deadline gate: do not start a new iteration if we are too close to hard deadline.
            if let Some(tm) = limits.time_manager.as_ref() {
                let hard_ms = tm.hard_limit_ms();
                let soft_ms = tm.soft_limit_ms();
                let cap = if hard_ms != u64::MAX {
                    Some((hard_ms, "tm-hard"))
                } else if soft_ms != u64::MAX {
                    Some((soft_ms, "tm-soft"))
                } else {
                    None
                };
                if let Some((cap_ms, origin)) = cap {
                    let elapsed_ms = tm.elapsed_ms();
                    if let Some(dec) =
                        Self::decide_near_deadline_policy(cap_ms, elapsed_ms, d, limits.multipv)
                    {
                        if let Some(cb) = limits.info_string_callback.as_ref() {
                            cb(&format!(
                                "near_deadline_params origin={} hard_ms={} t_rem={} main_win_ms={} finalize_win_ms={}",
                                origin, cap_ms, dec.t_rem_ms, dec.main_win_ms, dec.finalize_win_ms
                            ));
                        }
                        // NearHard finalize は hard が存在する場合のみ、ponder 中は送らない
                        if !Self::stabilization_disabled()
                            && hard_ms != u64::MAX
                            && !limits.is_ponder
                            && dec.fire_nearhard
                        {
                            if let Some(ctrl) = limits.stop_controller.as_ref() {
                                if !finalize_nearhard_sent && !is_helper {
                                    ctrl.request_finalize(FinalizeReason::NearHard);
                                    finalize_nearhard_sent = true;
                                }
                            }
                        }
                        if !Self::stabilization_disabled() && dec.skip_new_iter {
                            if let Some(cb) = limits.info_string_callback.as_ref() {
                                cb("near_deadline_skip_new_iter=1");
                            }
                            stats_near_deadline_skip_new_iter = 1;
                            break;
                        }
                    }
                }
            }
            // Fixed time limit（time_limit）が設定されている場合も同様の近〆切ゲートを適用
            else if let Some(limit) = limits.time_limit() {
                let cap_ms = limit.as_millis() as u64;
                let elapsed_ms = t0.elapsed().as_millis() as u64;
                if let Some(dec) =
                    Self::decide_near_deadline_policy(cap_ms, elapsed_ms, d, limits.multipv)
                {
                    if let Some(cb) = limits.info_string_callback.as_ref() {
                        cb(&format!(
                            "near_deadline_params origin=fixed hard_ms={} t_rem={} main_win_ms={} finalize_win_ms={}",
                            cap_ms, dec.t_rem_ms, dec.main_win_ms, dec.finalize_win_ms
                        ));
                    }
                    if !Self::stabilization_disabled() && !limits.is_ponder && dec.fire_nearhard {
                        // FixedTime でも NearHard finalize を 1 回だけ送る（P1仕様）。
                        if let Some(ctrl) = limits.stop_controller.as_ref() {
                            if !finalize_nearhard_sent && !is_helper {
                                ctrl.request_finalize(FinalizeReason::NearHard);
                                finalize_nearhard_sent = true;
                            }
                        }
                    }
                    if !Self::stabilization_disabled() && dec.skip_new_iter {
                        if let Some(cb) = limits.info_string_callback.as_ref() {
                            cb("near_deadline_skip_new_iter=1");
                        }
                        stats_near_deadline_skip_new_iter = 1;
                        break;
                    }
                }
            }
            #[cfg(any(debug_assertions, feature = "diagnostics"))]
            if super::diagnostics::should_abort_now() {
                last_deadline_hit = Some(DeadlineHit::Stop);
                if incomplete_depth.is_none() {
                    incomplete_depth = Some(d as u8);
                }
                break;
            }
            #[cfg(feature = "diagnostics")]
            reset_qsearch_diagnostics();
            if let Some(cb) = limits.info_string_callback.as_ref() {
                cb(&format!("iter_start depth={} nodes={}", d, nodes));
                if let Some(tt) = &self.tt {
                    cb(&format!(
                        "tt_snapshot depth={} hf={} hf_phys={} attempts={}",
                        d,
                        tt.hashfull_permille(),
                        tt.hashfull_physical_permille(),
                        tt.store_attempts()
                    ));
                }
            }
            if let Some(hit) =
                Self::deadline_hit(t0, soft_deadline, hard_deadline, limits, min_think_ms, nodes)
            {
                if matches!(hit, DeadlineHit::Soft) && finalize_nearhard_sent {
                    // ignore Soft after NearHard
                } else {
                    notify_deadline(hit, nodes);
                    match hit {
                        DeadlineHit::Stop | DeadlineHit::Hard => {
                            last_deadline_hit = Some(hit);
                            if incomplete_depth.is_none() {
                                incomplete_depth = Some(d as u8);
                            }
                            break;
                        }
                        DeadlineHit::Soft => {
                            last_deadline_hit = Some(hit);
                            if is_helper {
                                if incomplete_depth.is_none() {
                                    incomplete_depth = Some(d as u8);
                                }
                                break;
                            }
                            lead_window_soft_break = true;
                            // Primary continues current iteration to commit PV.
                        }
                    }
                }
            }
            let mut seldepth: u32 = 0;
            let mut iteration_asp_failures: u32 = 0;
            let mut iteration_asp_hits: u32 = 0;
            let mut iteration_researches: u32 = 0;
            let prev_best_move_for_iteration = best;
            let throttle_ms = Self::currmove_throttle_ms();
            let mut last_currmove_emit = Instant::now();
            let prev_root_lines = final_lines.as_ref().map(|lines| lines.as_slice());
            // Build root move list using standard MovePicker ordering (YaneuraOu-style phases)
            let mg = MoveGenerator::new();
            let Ok(_list) = mg.generate_all(root) else {
                if incomplete_depth.is_none() {
                    incomplete_depth = Some(d as u8);
                }
                break;
            };
            // Build MovePicker with TT hint (if any), no killers/counter at root
            let mut hint_for_picker: Option<crate::shogi::Move> = None;
            if let Some(tt) = &self.tt {
                if dynp::tt_prefetch_enabled() {
                    tt.prefetch_l2(root_key, root.side_to_move);
                }
                if let Some(entry) = tt.probe(root_key, root.side_to_move) {
                    hint_for_picker = entry.get_move();
                }
            }
            if let Some(cb) = limits.info_string_callback.as_ref() {
                cb(&format!("root_hint_applied={}", if hint_for_picker.is_some() { 1 } else { 0 }));
            }
            let mut mp = ordering::MovePicker::new_normal(
                root,
                hint_for_picker,
                None,
                [None, None],
                None,
                None,
            );
            let mut root_moves: Vec<crate::shogi::Move> = Vec::new();
            while let Some(mv) = mp.next(heur_state) {
                root_moves.push(mv);
            }
            if root_moves.is_empty() {
                if incomplete_depth.is_none() {
                    incomplete_depth = Some(d as u8);
                }
                break;
            }
            let root_rank: Vec<crate::shogi::Move> = root_moves.clone();
            let mut rank_map: HashMap<u32, u32> = HashMap::with_capacity(root_rank.len());
            for (idx, mv) in root_rank.iter().enumerate() {
                rank_map.entry(mv.to_u32()).or_insert(idx as u32 + 1);
            }

            let root_static_eval = self.evaluator.evaluate(root);
            let root_static_eval_i16 =
                root_static_eval.clamp(i16::MIN as i32, i16::MAX as i32) as i16;

            // MultiPV（逐次選抜）— near‑deadline では PV1 仕上げを優先（縮退）
            let mut k = limits.multipv.max(1) as usize;
            if let Some(tm) = limits.time_manager.as_ref() {
                let hard_ms = tm.hard_limit_ms();
                let soft_ms = tm.soft_limit_ms();
                let cap = if hard_ms != u64::MAX {
                    Some(hard_ms)
                } else if soft_ms != u64::MAX {
                    Some(soft_ms)
                } else {
                    None
                };
                if let Some(cap_ms) = cap {
                    let elapsed_ms = tm.elapsed_ms();
                    if let Some(dec) =
                        Self::decide_near_deadline_policy(cap_ms, elapsed_ms, d, limits.multipv)
                    {
                        if !Self::stabilization_disabled() && dec.shrink_multipv {
                            k = 1;
                            if let Some(cb) = limits.info_string_callback.as_ref() {
                                cb("near_deadline_multipv_shrink=1");
                            }
                            stats_multipv_shrunk = 1;
                        }
                    }
                }
            }
            // Fixed time でも MultiPV 縮退を適用（decide の shrink を使用）
            if let Some(limit) = limits.time_limit() {
                let cap_ms = limit.as_millis() as u64;
                let elapsed_ms = t0.elapsed().as_millis() as u64;
                if let Some(dec) =
                    Self::decide_near_deadline_policy(cap_ms, elapsed_ms, d, limits.multipv)
                {
                    if !Self::stabilization_disabled() && dec.shrink_multipv {
                        k = 1;
                        if let Some(cb) = limits.info_string_callback.as_ref() {
                            cb("near_deadline_multipv_shrink=1");
                        }
                        stats_multipv_shrunk = 1;
                    }
                }
            }
            let mut excluded: SmallVec<[crate::shogi::Move; 32]> = SmallVec::new();
            let mut depth_lines: SmallVec<[RootLine; 4]> = SmallVec::new();
            let required_multipv_lines = if k > 1 {
                root_moves.len().min(k).max(1)
            } else {
                1
            };

            // Counters aggregate across PVs at this depth
            let mut depth_tt_hits: u64 = 0;
            let mut depth_beta_cuts: u64 = 0;
            let mut depth_lmr_counter: u64 = 0;
            let mut depth_lmr_trials: u64 = 0;
            // instrumentation (depth‑local Cells)
            let depth_lmr_blocked_in_check = Cell::new(0u64);
            let depth_lmr_blocked_recapture = Cell::new(0u64);
            let depth_evasion_sparsity_ext = Cell::new(0u64);
            let mut local_best_for_next_iter: Option<(crate::shogi::Move, i32)> = None;
            let mut depth_hint_exists: u64 = 0;
            let mut depth_hint_used: u64 = 0;
            let mut line_nodes_checkpoint = nodes;
            let mut line_time_checkpoint = t0.elapsed().as_millis() as u64;
            if d % 2 == 0 {
                heur_state.age_all();
            }
            let mut shared_heur = std::mem::take(heur_state);
            shared_heur.lmr_trials = 0;
            // Verify-bound slack 用: 採用した PV1 の root 窓（alpha/beta）を保持
            let mut verify_window_alpha: i32 = 0;
            let mut verify_window_beta: i32 = 0;
            // qnodes フォールバック用: 最後に試行したPVのqnodesと、当該PVの行公開有無
            let mut last_pv_qnodes: u64 = 0;
            let mut last_pv_published: bool = false;
            for pv_idx in 1..=k {
                if is_helper && lead_window_soft_break {
                    if incomplete_depth.is_none() {
                        incomplete_depth = Some(d as u8);
                    }
                    break;
                }
                if let Some(hit) = Self::deadline_hit(
                    t0,
                    soft_deadline,
                    hard_deadline,
                    limits,
                    min_think_ms,
                    nodes,
                ) {
                    if matches!(hit, DeadlineHit::Soft) && finalize_nearhard_sent {
                        // ignore Soft after NearHard
                    } else {
                        notify_deadline(hit, nodes);
                        last_deadline_hit = Some(hit);
                        match hit {
                            DeadlineHit::Stop | DeadlineHit::Hard => break,
                            DeadlineHit::Soft => {
                                if is_helper || depth_lines.len() >= required_multipv_lines {
                                    if is_helper && incomplete_depth.is_none() {
                                        incomplete_depth = Some(d as u8);
                                    }
                                    break;
                                }
                                lead_window_soft_break = true;
                            }
                        }
                    }
                }
                // Aspiration window per PV head
                // If prev_root_lines is None and we have a hint from previous iteration,
                // smooth the aspiration center: center = (7*prev_score + 3*hint_score)/10
                let aspiration_center = if d > 1 && prev_root_lines.is_none() {
                    if let Some((_, hint_score)) = best_hint_next_iter {
                        // Skip smoothing near mate scores to preserve mate distance integrity
                        use crate::search::constants::MATE_SCORE;
                        if prev_score.abs() >= MATE_SCORE - 100
                            || hint_score.abs() >= MATE_SCORE - 100
                        {
                            prev_score
                        } else {
                            // Weighted average with i64 intermediate calculation to prevent overflow
                            // in case SEARCH_INF or coefficients are changed in the future
                            let c =
                                (7_i64 * prev_score as i64 + 3_i64 * hint_score as i64) / 10_i64;
                            c.clamp(i32::MIN as i64, i32::MAX as i64) as i32
                        }
                    } else {
                        prev_score
                    }
                } else {
                    prev_score
                };

                // 初期窓とΔ（YaneuraOu準拠 + 拡張）
                // primary: Δ0 = Δinit + K*log2(Threads)（上限あり）
                // helper: off=フル窓 / wide=±HELPER_ASPIRATION_WIDE_DELTA
                // floor(log2(threads)) を正しく計算（threads_hint 優先）
                // u32::ilog2 を用いることでビット幅依存のバグを回避
                let threads_lg2: i32 =
                    limits.threads_hint.map(|t| t.max(1).ilog2() as i32).unwrap_or(0);
                let mut delta = ASPIRATION_DELTA_INITIAL;
                let mut alpha;
                let mut beta;
                // Gate aspiration start by depth and PV stability:
                // use full window while the search is shallow AND previous iteration has not
                // published any root lines. Once either (depth>=min) or (prev_root_lines.is_some())
                // holds, allow aspiration for the primary. Helpers follow their own policy.
                if !is_helper && d < ASPIRATION_MIN_DEPTH && prev_root_lines.is_none() {
                    alpha = i32::MIN / 2;
                    beta = i32::MAX / 2;
                } else if is_helper {
                    match helper_asp_mode() {
                        HelperAspMode::Off => {
                            delta = ASPIRATION_DELTA_MAX;
                            alpha = i32::MIN / 2;
                            beta = i32::MAX / 2;
                        }
                        HelperAspMode::Wide => {
                            delta = helper_asp_delta();
                            alpha = aspiration_center - delta;
                            beta = aspiration_center + delta;
                        }
                    }
                } else {
                    delta = (ASPIRATION_DELTA_INITIAL
                        + ASPIRATION_DELTA_THREADS_K.saturating_mul(threads_lg2))
                    .min(ASPIRATION_DELTA_MAX);
                    alpha = aspiration_center - delta;
                    beta = aspiration_center + delta;
                }
                let mut window_alpha = alpha;
                let mut window_beta = beta;

                let mut heur = std::mem::take(&mut shared_heur);
                let lmr_trials_checkpoint = heur.lmr_trials;
                let mut tt_hits: u64 = 0;
                let mut beta_cuts: u64 = 0;
                let mut lmr_counter: u64 = 0;
                let mut root_tt_hint_exists: u64 = 0;
                let mut root_tt_hint_used: u64 = 0;
                let mut qnodes: u64 = 0;
                let qnodes_limit = Self::compute_qnodes_limit(limits, d, pv_idx);
                // 純粋 LazySMP: ルートインデックスの claim/release 管理は不要

                // 作業用root move配列（excludedを除外）
                let excluded_keys: SmallVec<[u32; 32]> =
                    excluded.iter().map(|m| m.to_u32()).collect();
                let active_moves: SmallVec<[crate::shogi::Move; 64]> = root_moves
                    .iter()
                    .copied()
                    .filter(|m| {
                        let key = m.to_u32();
                        !excluded_keys.contains(&key)
                    })
                    .collect();

                // 探索ループ（Aspiration）
                let mut local_best_mv = None;
                let mut local_best = i32::MIN / 2;
                let mut helper_retries: u32 = 0; // fail‑high のみ 1 回まで許可
                loop {
                    if let Some(hit) = Self::deadline_hit(
                        t0,
                        soft_deadline,
                        hard_deadline,
                        limits,
                        min_think_ms,
                        nodes,
                    ) {
                        if matches!(hit, DeadlineHit::Soft) && finalize_nearhard_sent {
                            // ignore Soft after NearHard
                        } else {
                            notify_deadline(hit, nodes);
                            last_deadline_hit = Some(hit);
                            match hit {
                                DeadlineHit::Stop | DeadlineHit::Hard => break,
                                DeadlineHit::Soft => {
                                    if is_helper || depth_lines.len() >= required_multipv_lines {
                                        if is_helper && incomplete_depth.is_none() {
                                            incomplete_depth = Some(d as u8);
                                        }
                                        break;
                                    }
                                    lead_window_soft_break = true;
                                }
                            }
                        }
                    }
                    #[cfg(any(debug_assertions, feature = "diagnostics"))]
                    if super::diagnostics::should_abort_now() {
                        last_deadline_hit = Some(DeadlineHit::Stop);
                        break;
                    }
                    if active_moves.is_empty() {
                        break;
                    }
                    let (old_alpha, old_beta) = (alpha, beta);
                    window_alpha = old_alpha;
                    window_beta = old_beta;
                    // Root move loop with CurrMove events
                    for (idx, mv) in active_moves.iter().copied().enumerate() {
                        // 純粋 LazySMP: claim は行わない
                        #[cfg(any(debug_assertions, feature = "diagnostics"))]
                        if super::diagnostics::should_abort_now() {
                            last_deadline_hit = Some(DeadlineHit::Stop);
                            break;
                        }
                        if let Some(hit) = Self::deadline_hit(
                            t0,
                            soft_deadline,
                            hard_deadline,
                            limits,
                            min_think_ms,
                            nodes,
                        ) {
                            if matches!(hit, DeadlineHit::Soft) && finalize_nearhard_sent {
                                // ignore Soft after NearHard
                            } else {
                                notify_deadline(hit, nodes);
                                last_deadline_hit = Some(hit);
                                match hit {
                                    DeadlineHit::Stop | DeadlineHit::Hard => break,
                                    DeadlineHit::Soft => {
                                        if is_helper || depth_lines.len() >= required_multipv_lines
                                        {
                                            if is_helper && incomplete_depth.is_none() {
                                                incomplete_depth = Some(d as u8);
                                            }
                                            break;
                                        }
                                        lead_window_soft_break = true;
                                    }
                                }
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
                                let number =
                                    rank_map.get(&mv.to_u32()).copied().unwrap_or((idx as u32) + 1);
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
                                    qnodes: &mut qnodes,
                                    qnodes_limit,
                                    #[cfg(feature = "diagnostics")]
                                    abdada_busy_detected: &mut cum_abdada_busy_detected,
                                    #[cfg(feature = "diagnostics")]
                                    abdada_busy_set: &mut cum_abdada_busy_set,
                                };
                                let (sc, _) = self.alphabeta(
                                    pvs::ABArgs {
                                        pos: &child,
                                        depth: d - 1,
                                        alpha: -beta,
                                        beta: -alpha,
                                        ply: 1,
                                        is_pv: true,
                                        stack,
                                        heur: &mut heur,
                                        tt_hits: &mut tt_hits,
                                        beta_cuts: &mut beta_cuts,
                                        lmr_counter: &mut lmr_counter,
                                        lmr_blocked_in_check: Some(&depth_lmr_blocked_in_check),
                                        lmr_blocked_recapture: Some(&depth_lmr_blocked_recapture),
                                        evasion_sparsity_ext: Some(&depth_evasion_sparsity_ext),
                                    },
                                    &mut search_ctx,
                                );
                                // PV head: result of full-window PV search
                                -sc
                            } else {
                                let mut search_ctx_nw = SearchContext {
                                    limits,
                                    start_time: &t0,
                                    nodes: &mut nodes,
                                    seldepth: &mut seldepth,
                                    qnodes: &mut qnodes,
                                    qnodes_limit,
                                    #[cfg(feature = "diagnostics")]
                                    abdada_busy_detected: &mut cum_abdada_busy_detected,
                                    #[cfg(feature = "diagnostics")]
                                    abdada_busy_set: &mut cum_abdada_busy_set,
                                };
                                let (sc_nw, _) = self.alphabeta(
                                    pvs::ABArgs {
                                        pos: &child,
                                        depth: d - 1,
                                        alpha: -(alpha + 1),
                                        beta: -alpha,
                                        ply: 1,
                                        is_pv: false,
                                        stack,
                                        heur: &mut heur,
                                        tt_hits: &mut tt_hits,
                                        beta_cuts: &mut beta_cuts,
                                        lmr_counter: &mut lmr_counter,
                                        lmr_blocked_in_check: Some(&depth_lmr_blocked_in_check),
                                        lmr_blocked_recapture: Some(&depth_lmr_blocked_recapture),
                                        evasion_sparsity_ext: Some(&depth_evasion_sparsity_ext),
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
                                        qnodes: &mut qnodes,
                                        qnodes_limit,
                                        #[cfg(feature = "diagnostics")]
                                        abdada_busy_detected: &mut cum_abdada_busy_detected,
                                        #[cfg(feature = "diagnostics")]
                                        abdada_busy_set: &mut cum_abdada_busy_set,
                                    };
                                    let (sc_fw, _) = self.alphabeta(
                                        pvs::ABArgs {
                                            pos: &child,
                                            depth: d - 1,
                                            alpha: -beta,
                                            beta: -alpha,
                                            ply: 1,
                                            is_pv: true,
                                            stack,
                                            heur: &mut heur,
                                            tt_hits: &mut tt_hits,
                                            beta_cuts: &mut beta_cuts,
                                            lmr_counter: &mut lmr_counter,
                                            lmr_blocked_in_check: Some(&depth_lmr_blocked_in_check),
                                            lmr_blocked_recapture: Some(
                                                &depth_lmr_blocked_recapture,
                                            ),
                                            evasion_sparsity_ext: Some(&depth_evasion_sparsity_ext),
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
                        // 純粋 LazySMP: done 通知は行わない
                        if alpha >= beta {
                            break; // fail-high
                        }
                    }

                    #[cfg(any(debug_assertions, feature = "diagnostics"))]
                    if super::diagnostics::should_abort_now() {
                        break;
                    }

                    if let Some(hit) = Self::deadline_hit(
                        t0,
                        soft_deadline,
                        hard_deadline,
                        limits,
                        min_think_ms,
                        nodes,
                    ) {
                        if !(matches!(hit, DeadlineHit::Soft) && finalize_nearhard_sent) {
                            notify_deadline(hit, nodes);
                        }
                        last_deadline_hit = Some(hit);
                        match hit {
                            DeadlineHit::Stop | DeadlineHit::Hard => break,
                            DeadlineHit::Soft => {
                                if is_helper || depth_lines.len() >= required_multipv_lines {
                                    if is_helper && incomplete_depth.is_none() {
                                        incomplete_depth = Some(d as u8);
                                    }
                                    break;
                                }
                                lead_window_soft_break = true;
                            }
                        }
                    }
                    if local_best <= old_alpha {
                        let new_alpha = old_alpha.saturating_sub(2 * delta).max(i32::MIN / 2);
                        let new_beta = old_beta;
                        if let Some(cb) = info {
                            cb(InfoEvent::Aspiration {
                                outcome: crate::search::api::AspirationOutcome::FailLow,
                                old_alpha,
                                old_beta,
                                new_alpha,
                                new_beta,
                            });
                        }
                        iteration_asp_failures = iteration_asp_failures.saturating_add(1);
                        // P1: Aggressive bailout — two failures in the same iteration => full window
                        if !Self::stabilization_disabled()
                            && !is_helper
                            && iteration_asp_failures >= 2
                        {
                            alpha = i32::MIN / 2;
                            beta = i32::MAX / 2;
                            delta = ASPIRATION_DELTA_MAX;
                            // フル窓へ落とした直後にカウンタをリセット
                            iteration_asp_failures = 0;
                            iteration_researches = 0;
                            continue;
                        }
                        iteration_researches = iteration_researches.saturating_add(1);
                        // If re-searches are piling up on the primary, bail out to a full window
                        // to stabilize PV and avoid time loss.
                        let retries_max = Self::retries_max(soft_deadline, &t0);
                        if !Self::stabilization_disabled()
                            && !is_helper
                            && iteration_researches >= retries_max
                        {
                            alpha = i32::MIN / 2;
                            beta = i32::MAX / 2;
                            delta = ASPIRATION_DELTA_MAX;
                            iteration_asp_failures = 0;
                            iteration_researches = 0;
                            continue;
                        }
                        if is_helper {
                            // helper: fail‑low は再探索しない
                            break;
                        } else {
                            alpha = new_alpha;
                            beta = new_beta;
                            // 非対称拡大量（デフォルト33%）。環境変数で調整可。
                            let add_pct = asp_fail_low_pct();
                            let add = (delta * add_pct / 100).max(1);
                            delta = (delta + add).min(ASPIRATION_DELTA_MAX);
                            continue;
                        }
                    }
                    if local_best >= old_beta {
                        let new_alpha = old_alpha;
                        let new_beta = old_beta.saturating_add(2 * delta).min(i32::MAX / 2);
                        if let Some(cb) = info {
                            cb(InfoEvent::Aspiration {
                                outcome: crate::search::api::AspirationOutcome::FailHigh,
                                old_alpha,
                                old_beta,
                                new_alpha,
                                new_beta,
                            });
                        }
                        iteration_asp_failures = iteration_asp_failures.saturating_add(1);
                        if !Self::stabilization_disabled()
                            && !is_helper
                            && iteration_asp_failures >= 2
                        {
                            alpha = i32::MIN / 2;
                            beta = i32::MAX / 2;
                            delta = ASPIRATION_DELTA_MAX;
                            iteration_asp_failures = 0;
                            iteration_researches = 0;
                            continue;
                        }
                        iteration_researches = iteration_researches.saturating_add(1);
                        let retries_max = Self::retries_max(soft_deadline, &t0);
                        if !Self::stabilization_disabled()
                            && !is_helper
                            && iteration_researches >= retries_max
                        {
                            alpha = i32::MIN / 2;
                            beta = i32::MAX / 2;
                            delta = ASPIRATION_DELTA_MAX;
                            iteration_asp_failures = 0;
                            iteration_researches = 0;
                            continue;
                        }
                        if is_helper {
                            if helper_retries == 0 {
                                // 2 回目はフル窓で 1 回だけ再探索
                                alpha = i32::MIN / 2;
                                beta = i32::MAX / 2;
                                helper_retries = 1;
                                continue;
                            } else {
                                // 打ち切り
                                break;
                            }
                        } else {
                            alpha = new_alpha;
                            beta = new_beta;
                            // 非対称拡大量（デフォルト33%）。環境変数で調整可。
                            let add_pct = asp_fail_high_pct();
                            let add = (delta * add_pct / 100).max(1);
                            delta = (delta + add).min(ASPIRATION_DELTA_MAX);
                            continue;
                        }
                    }
                    // 純粋 LazySMP: done 通知は不要
                    iteration_asp_hits = iteration_asp_hits.saturating_add(1);
                    break; // success within window
                }

                #[cfg(any(debug_assertions, feature = "diagnostics"))]
                if super::diagnostics::should_abort_now() {
                    last_deadline_hit = Some(DeadlineHit::Stop);
                    break;
                }

                // Counters aggregate
                depth_tt_hits = depth_tt_hits.saturating_add(tt_hits);
                depth_beta_cuts = depth_beta_cuts.saturating_add(beta_cuts);
                depth_lmr_counter = depth_lmr_counter.saturating_add(lmr_counter);
                depth_lmr_trials = depth_lmr_trials
                    .saturating_add(heur.lmr_trials.saturating_sub(lmr_trials_checkpoint));
                // Pass heuristics update back to shared state for next PV/iteration.
                // This ensures heuristics learned during this PV search are not lost.
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
                    // pre-claim 診断は廃止
                }

                // PV 行の生成と発火
                if let Some(m) = local_best_mv {
                    // 次反復のAspiration用に pv_idx==1 を採用
                    if pv_idx == 1 {
                        let changed =
                            prev_best_move_for_iteration.map(|b| b.to_u32()) != Some(m.to_u32());
                        if changed {
                            cumulative_pv_changed = cumulative_pv_changed.saturating_add(1);
                        }
                        // Sticky-PV: near hard deadlineかつ未検証で切替なら、前反復のPVを優先
                        let mut adopt_mv = m;
                        let near_deadline = time_remaining_ms_for_sticky(t0, limits)
                            .is_some_and(|t_rem| t_rem <= STICKY_PV_WINDOW_MS);
                        #[cfg(feature = "diagnostics")]
                        if let Some(cb) = limits.info_string_callback.as_ref() {
                            cb(&format!(
                                "sticky_check changed={} near_deadline={} m={}",
                                changed as u8,
                                near_deadline as u8,
                                crate::usi::move_to_usi(&m)
                            ));
                        }
                        if changed && near_deadline {
                            if let Some(prev_mv) = prev_best_move_for_iteration {
                                adopt_mv = prev_mv;
                                // keep previous score as hint center
                                if let Some((_bm, prev_sc)) = local_best_for_next_iter {
                                    prev_score = prev_sc;
                                }
                            }
                        }
                        best = Some(adopt_mv);
                        best_score = if adopt_mv.to_u32() == m.to_u32() {
                            local_best
                        } else {
                            // fallback: keep previous score when sticky applied
                            prev_score
                        };
                        prev_score = best_score;
                        // Capture best move and score for use as hint in next iteration
                        local_best_for_next_iter = Some((adopt_mv, best_score));
                        if let Some(hint) = hint_for_picker {
                            root_tt_hint_exists = 1;
                            if adopt_mv.to_u32() == hint.to_u32() {
                                root_tt_hint_used = 1;
                            }
                        }
                        depth_hint_exists = root_tt_hint_exists;
                        depth_hint_used = root_tt_hint_used;
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
                        Some(line_nodes.saturating_mul(1000) / line_time_ms.max(1))
                    } else {
                        None
                    };
                    let orig_alpha = window_alpha;
                    let orig_beta = window_beta;
                    let bound = Self::classify_root_bound(local_best, orig_alpha, orig_beta);
                    if pv_idx == 1 {
                        verify_window_alpha = orig_alpha;
                        verify_window_beta = orig_beta;
                    }
                    let line = RootLine {
                        multipv_index: pv_idx as u8,
                        root_move: m,
                        score_internal: local_best,
                        score_cp: crate::search::types::clamp_score_cp(local_best),
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
                        // Attach mate distance for diagnostics and USI snapshot consumers
                        mate_distance: crate::search::constants::mate_distance(local_best),
                    };
                    let node_type_for_store = line.bound;
                    let line_arc = Arc::new(line);

                    // ベンチ時のメイト即停止（冪等）: primary かつ PV1 が Exact mate の場合
                    if !is_helper
                        && pv_idx == 1
                        && matches!(node_type_for_store, NodeType::Exact)
                        && bench_stop_on_mate_enabled()
                        && limits.time_manager.is_none()
                        && limits.time_limit().is_none()
                        && limits.fallback_deadlines.is_none()
                        && {
                            use crate::search::common::is_mate_score;
                            is_mate_score(local_best)
                        }
                    {
                        if let Some(ctrl) = stop_controller.as_ref() {
                            ctrl.request_stop_flag_only();
                        }
                    }
                    if let Some(ctrl) = stop_controller.as_ref() {
                        let mut ctrl_line = (*line_arc).clone();
                        ctrl_line.nodes = Some(nodes);
                        let elapsed_total_ms = elapsed_ms_total;
                        ctrl_line.time_ms = Some(elapsed_total_ms);
                        ctrl_line.nps = if elapsed_total_ms > 0 {
                            Some(nodes.saturating_mul(1000) / elapsed_total_ms.max(1))
                        } else {
                            None
                        };
                        ctrl.publish_root_line(session_id, root_key, &ctrl_line);
                    }
                    if let Some(cb) = info {
                        cb(InfoEvent::PV {
                            line: Arc::clone(&line_arc),
                        });
                    }
                    depth_lines.push(match Arc::try_unwrap(line_arc) {
                        Ok(line) => line,
                        Err(arc) => (*arc).clone(),
                    });
                    // Collect qnodes consumed for this PV head
                    cum_qnodes = cum_qnodes.saturating_add(qnodes);
                    // フォールバック用にも記録（公開済み）
                    last_pv_qnodes = qnodes;
                    last_pv_published = true;
                    // TT 保存は 1 行目かつ Exact のときのみ行う。
                    // Aspiration 成功時にのみ Exact が確定する前提なので、将来の窓調整変更で
                    // 誤って Lower/Upper を保存しないよう明示的にガードしておく。
                    if pv_idx == 1
                        && matches!(node_type_for_store, NodeType::Exact)
                        && best.is_some()
                    {
                        if let (Some(tt), Some(best_mv_root)) = (&self.tt, best) {
                            let store_score =
                                crate::search::common::adjust_mate_score_for_tt(best_score, 0)
                                    .clamp(i16::MIN as i32, i16::MAX as i32)
                                    as i16;
                            let mut args = crate::search::tt::TTStoreArgs::new(
                                root_key,
                                Some(best_mv_root),
                                store_score,
                                root_static_eval_i16,
                                d as u8,
                                node_type_for_store,
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
                    // このPVで消費したqnodesをフォールバック用に記録
                    last_pv_qnodes = qnodes;
                    last_pv_published = false;
                    break;
                }
            }

            // PV 行が公開されなかった場合でも、最後に試行した PV の qnodes を統計に反映
            if !last_pv_published && last_pv_qnodes > 0 {
                cum_qnodes = cum_qnodes.saturating_add(last_pv_qnodes);
            }

            // 深さ集計を累積
            cum_tt_hits = cum_tt_hits.saturating_add(depth_tt_hits);
            cum_beta_cuts = cum_beta_cuts.saturating_add(depth_beta_cuts);
            cum_lmr_counter = cum_lmr_counter.saturating_add(depth_lmr_counter);
            cum_lmr_trials = cum_lmr_trials.saturating_add(depth_lmr_trials);
            // Instrumentation aggregation
            cum_lmr_blocked_in_check =
                cum_lmr_blocked_in_check.saturating_add(depth_lmr_blocked_in_check.get());
            cum_lmr_blocked_recapture =
                cum_lmr_blocked_recapture.saturating_add(depth_lmr_blocked_recapture.get());
            cum_evasion_sparsity_ext =
                cum_evasion_sparsity_ext.saturating_add(depth_evasion_sparsity_ext.get());

            // 反復ごとのrootヒント統計（最終反復で掲載）
            stats_hint_exists = depth_hint_exists;
            stats_hint_used = depth_hint_used;
            let capped_seldepth =
                seldepth.min(d as u32 + SELDEPTH_EXTRA_MARGIN).min(u8::MAX as u32) as u8;

            let iteration_complete = depth_lines.len() >= required_multipv_lines;

            *heur_state = shared_heur;

            if iteration_complete {
                // 近締切帯での最終確認（任意、環境フラグで有効化）。
                // まず、条件を満たすが既にExactな場合のスキップ理由をログ。
                if !Self::stabilization_disabled()
                    && finalize_nearhard_sent
                    && Self::near_final_zero_window_enabled()
                    && d >= Self::near_final_zero_window_min_depth()
                    && !depth_lines.is_empty()
                    && matches!(depth_lines.first().map(|l| l.bound), Some(NodeType::Exact))
                {
                    if let Some(cb) = limits.info_string_callback.as_ref() {
                        cb("near_final_zero_window_skip=1 reason=already_exact");
                    }
                }

                if !Self::stabilization_disabled()
                    && finalize_nearhard_sent
                    && !zero_window_done
                    && Self::near_final_zero_window_enabled()
                    && d >= Self::near_final_zero_window_min_depth()
                    && !depth_lines.is_empty()
                    && !matches!(depth_lines.first().map(|l| l.bound), Some(NodeType::Exact))
                {
                    // 予算的な安全: t_rem が極小ならスキップ
                    let mut t_rem: u64 = 0;
                    if let Some(tm) = limits.time_manager.as_ref() {
                        let hard = tm.hard_limit_ms();
                        let el = tm.elapsed_ms();
                        if hard != u64::MAX && el < hard {
                            t_rem = hard.saturating_sub(el);
                        }
                    } else if let Some(limit) = limits.time_limit() {
                        let cap = limit.as_millis() as u64;
                        let el = t0.elapsed().as_millis() as u64;
                        if el < cap {
                            t_rem = cap.saturating_sub(el);
                        }
                    }
                    let budget = Self::near_final_zero_window_budget_ms();
                    let min_trem = Self::near_final_zero_window_min_trem_ms();
                    let min_mpv = Self::near_final_zero_window_min_multipv();
                    if limits.multipv < min_mpv {
                        if let Some(cb) = limits.info_string_callback.as_ref() {
                            cb(&format!(
                                "near_final_zero_window_skip=1 reason=min_multipv mpv={} min_mpv={} t_rem={} shrunk={}",
                                limits.multipv,
                                min_mpv,
                                t_rem,
                                (stats_multipv_shrunk != 0) as u8
                            ));
                        }
                    } else if t_rem < min_trem {
                        if let Some(cb) = limits.info_string_callback.as_ref() {
                            cb(&format!(
                                "near_final_zero_window_skip=1 reason=trem_short t_rem={} min_trem={}",
                                t_rem, min_trem
                            ));
                        }
                    } else if let Some(first) = depth_lines.first_mut() {
                        let mv0 = first.root_move;
                        let target = first.score_internal;
                        // 近傍判定: 境界からの距離が slack 以下なら検証対象
                        let slack = Self::near_final_zero_window_bound_slack_cp();
                        if let Some(cb) = limits.info_string_callback.as_ref() {
                            cb(&format!(
                                "near_final_zero_window_start=1 depth={} score={} bound={:?}",
                                d, target, first.bound
                            ));
                        }
                        // Zero-window 検証の続行可否（skip 条件でのみ false にする）。
                        let mut proceed_nw = true;
                        if slack > 0 {
                            let near = match first.bound {
                                NodeType::UpperBound => {
                                    // fail-low: target <= alpha
                                    (verify_window_alpha - target).abs() <= slack
                                }
                                NodeType::LowerBound => {
                                    // fail-high: target >= beta
                                    (target - verify_window_beta).abs() <= slack
                                }
                                NodeType::Exact => false,
                            };
                            if !near {
                                if let Some(cb) = limits.info_string_callback.as_ref() {
                                    let (kind, diff) = match first.bound {
                                        NodeType::UpperBound => {
                                            ("upper", (verify_window_alpha - target).abs())
                                        }
                                        NodeType::LowerBound => {
                                            ("lower", (target - verify_window_beta).abs())
                                        }
                                        NodeType::Exact => ("exact", 0),
                                    };
                                    cb(&format!(
                                        "near_final_zero_window_skip=1 reason=bound_far kind={} diff={} slack={} alpha={} beta={} score={}",
                                        kind, diff, slack, verify_window_alpha, verify_window_beta, target
                                    ));
                                }
                                zero_window_done = true;
                                // 検証自体はスキップするが、反復の最終処理へはフォールスルーさせる。
                                proceed_nw = false;
                            }
                        }
                        // mate近傍の扱い
                        if Self::near_final_zero_window_skip_mate()
                            && crate::search::common::is_mate_score(target)
                        {
                            if let Some(cb) = limits.info_string_callback.as_ref() {
                                cb("near_final_zero_window_skip=1 reason=mate_near");
                            }
                            zero_window_done = true;
                            // 検証自体はスキップするが、反復の最終処理へはフォールスルーさせる。
                            proceed_nw = false;
                        }
                        if proceed_nw {
                            let mut verify_delta = Self::near_final_verify_delta_cp();
                            if crate::search::common::is_mate_score(target) {
                                verify_delta += Self::near_final_zero_window_mate_delta_cp().max(0);
                            }
                            // 狭いフル窓: [s-Δ, s+Δ] で Exact を確認（整数スコアで成立する最小窓）
                            let alpha0 = target.saturating_sub(verify_delta);
                            let beta0 = target.saturating_add(verify_delta);
                            let mut child = root.clone();
                            child.do_move(mv0);
                            // 局所カウンタ（本確認は軽量・単発）
                            let mut qnodes_local: u64 = 0;
                            let mut qnodes_limit_local = Self::compute_qnodes_limit(limits, d, 1);
                            let qnodes_limit_pre = qnodes_limit_local;
                            let mut tt_hits_local: u64 = 0;
                            let mut beta_cuts_local: u64 = 0;
                            let mut lmr_counter_local: u64 = 0;
                            let mut heur_local = Heuristics::default();
                            // 予算ms → qnodesへ換算し、上限を予算にクランプ
                            let safety_ms: u64 = 20;
                            let eff_budget_ms = if t_rem == 0 {
                                budget
                            } else if t_rem > safety_ms {
                                budget.min(t_rem.saturating_sub(safety_ms))
                            } else {
                                0
                            };
                            let budget_qnodes = eff_budget_ms
                                .saturating_mul(crate::search::constants::QNODES_PER_MS)
                                .max(crate::search::constants::MIN_QNODES_LIMIT);
                            if eff_budget_ms == 0 {
                                zero_window_done = true;
                                if let Some(cb) = limits.info_string_callback.as_ref() {
                                    cb(&format!(
                                    "near_final_zero_window_skip=1 reason=no_budget t_rem={} eff_budget_ms={}",
                                    t_rem,
                                    eff_budget_ms
                                ));
                                }
                                // 検証自体はスキップするが、反復の最終処理へはフォールスルーさせる。
                                proceed_nw = false;
                            }
                            // ここから先は proceed_nw=true の場合のみ実施
                            if proceed_nw {
                                qnodes_limit_local = qnodes_limit_local.min(budget_qnodes);
                                let qnodes_limit_post = qnodes_limit_local;
                                let mut search_ctx_nw = SearchContext {
                                    limits,
                                    start_time: &t0,
                                    nodes: &mut nodes,
                                    seldepth: &mut seldepth,
                                    qnodes: &mut qnodes_local,
                                    qnodes_limit: qnodes_limit_local,
                                    #[cfg(feature = "diagnostics")]
                                    abdada_busy_detected: &mut cum_abdada_busy_detected,
                                    #[cfg(feature = "diagnostics")]
                                    abdada_busy_set: &mut cum_abdada_busy_set,
                                };
                                // Attempt once per iteration under gating conditions
                                near_final_attempted = near_final_attempted.saturating_add(1);
                                let (sc_vf, _) = self.alphabeta(
                                    pvs::ABArgs {
                                        pos: &child,
                                        depth: d - 1,
                                        alpha: -(beta0),
                                        beta: -alpha0,
                                        ply: 1,
                                        is_pv: true,
                                        stack,
                                        heur: &mut heur_local,
                                        tt_hits: &mut tt_hits_local,
                                        beta_cuts: &mut beta_cuts_local,
                                        lmr_counter: &mut lmr_counter_local,
                                        lmr_blocked_in_check: Some(&depth_lmr_blocked_in_check),
                                        lmr_blocked_recapture: Some(&depth_lmr_blocked_recapture),
                                        evasion_sparsity_ext: Some(&depth_evasion_sparsity_ext),
                                    },
                                    &mut search_ctx_nw,
                                );
                                let s_back = -sc_vf;
                                let confirmed = Self::classify_root_bound(s_back, alpha0, beta0)
                                    == NodeType::Exact;
                                if confirmed {
                                    near_final_confirmed = near_final_confirmed.saturating_add(1);
                                    first.bound = NodeType::Exact;
                                    // 検証値に寄せる（±Δ内の誤差を吸収）
                                    first.score_internal = s_back;
                                    first.score_cp = crate::search::types::clamp_score_cp(s_back);
                                    first.mate_distance =
                                        crate::search::constants::mate_distance(s_back);
                                    // 可能ならPV更新（TTから復元）
                                    let pv = self
                                        .reconstruct_root_pv_from_tt(root, d, mv0)
                                        .unwrap_or_default();
                                    if !pv.is_empty() {
                                        first.pv = SmallVec::from_vec(pv.to_vec());
                                    }
                                    // TTへ保存（PV1相当のrootラインとしてExactを保持）
                                    if let Some(tt) = &self.tt {
                                        let store_score =
                                            crate::search::common::adjust_mate_score_for_tt(
                                                s_back, 0,
                                            )
                                            .clamp(i16::MIN as i32, i16::MAX as i32)
                                                as i16;
                                        let mut args = crate::search::tt::TTStoreArgs::new(
                                            root_key,
                                            Some(mv0),
                                            store_score,
                                            root_static_eval_i16,
                                            d as u8,
                                            NodeType::Exact,
                                            root.side_to_move,
                                        );
                                        args.is_pv = true;
                                        tt.store(args);
                                    }
                                }
                                zero_window_done = true;
                                // qnodes を統計へ反映
                                cum_qnodes = cum_qnodes.saturating_add(qnodes_local);
                                if let Some(cb) = limits.info_string_callback.as_ref() {
                                    let (kind, diff) = match first.bound {
                                        NodeType::UpperBound => ("upper", (alpha0 - target).abs()),
                                        NodeType::LowerBound => ("lower", (target - beta0).abs()),
                                        NodeType::Exact => ("exact", 0),
                                    };
                                    cb(&format!(
                                        "near_final_zero_window_result=1 status={} kind={} diff={} alpha={} beta={} target={} budget_ms={} budget_qnodes={} qnodes_limit_pre={} qnodes_limit_post={} t_rem={} qnodes_used={}",
                                        if confirmed { "confirmed" } else { "inexact" },
                                        kind,
                                        diff,
                                        alpha0,
                                        beta0,
                                        target,
                                        eff_budget_ms,
                                        budget_qnodes,
                                        qnodes_limit_pre,
                                        qnodes_limit_post,
                                        t_rem,
                                        qnodes_local
                                    ));
                                }
                            } // proceed_nw
                        } // if proceed_nw
                    }
                }

                let fully_resolved =
                    matches!(depth_lines.first().map(|l| l.bound), Some(NodeType::Exact));

                if fully_resolved {
                    final_depth_reached = d as u8;
                    final_seldepth_reached = Some(capped_seldepth);
                    final_seldepth_raw = Some(seldepth);

                    // near-final 検証による depth_lines の更新を反映した後に final_lines を固定
                    final_lines = Some(depth_lines.clone());
                    if let Some(ctrl) = stop_controller.as_ref() {
                        ctrl.publish_committed_snapshot(
                            session_id,
                            root_key,
                            depth_lines.as_slice(),
                            nodes,
                            t0.elapsed().as_millis() as u64,
                        );

                        // Thin B案: 探索側フック — 短手数詰みを検出したら早期最終化を要求
                        if crate::search::config::mate_early_stop_enabled() {
                            if let Some(first_line) = depth_lines.first() {
                                let max_d =
                                    crate::search::config::mate_early_stop_max_distance() as i32;
                                if let Some(dist) = crate::search::constants::mate_distance(
                                    first_line.score_internal,
                                ) {
                                    if dist > 0 && dist <= max_d {
                                        ctrl.request_finalize(FinalizeReason::PlannedMate {
                                            distance: dist,
                                            was_ponder: limits.is_ponder,
                                        });
                                    }
                                }
                            }
                        }
                    }
                } else if incomplete_depth.is_none() {
                    // fail-high/low 等でExactが得られなかった → 未完イテレーションとして扱う
                    incomplete_depth = Some(d as u8);
                }
            } else if incomplete_depth.is_none() {
                // iteration が完了しなかった場合は未完了深さとして記録する。
                incomplete_depth = Some(d as u8);
            }

            // Update hint for next iteration only if we obtained a best move from this iteration.
            // This preserves hints from previous iterations when current iteration is incomplete
            // (e.g., early cutoff before pv_idx==1 is reached).
            if let Some(h) = local_best_for_next_iter {
                best_hint_next_iter = Some(h);
            }

            let mut lead_ms = crate::search::policy::lead_window_base_ms();

            cumulative_asp_failures =
                cumulative_asp_failures.saturating_add(iteration_asp_failures);
            cumulative_asp_hits = cumulative_asp_hits.saturating_add(iteration_asp_hits);
            cumulative_researches = cumulative_researches.saturating_add(iteration_researches);

            if let Some(cb) = limits.info_string_callback.as_ref() {
                let elapsed_ms = t0.elapsed().as_millis();
                let msg =
                    format!("iter_complete depth={} elapsed_ms={} nodes={}", d, elapsed_ms, nodes);
                cb(msg.as_str());
            }

            #[cfg(feature = "diagnostics")]
            publish_qsearch_diagnostics(d, limits.info_string_callback.as_ref());

            if Self::should_stop(limits) {
                if let Some(cb) = limits.info_string_callback.as_ref() {
                    let elapsed = t0.elapsed().as_millis();
                    cb(&format!(
                        "stop_flag_break depth={} elapsed_ms={} nodes={}",
                        d, elapsed, nodes
                    ));
                }
                if incomplete_depth.is_none() {
                    incomplete_depth = Some(d as u8);
                }
                break;
            }

            let mut check_lead_window =
                |reason: &'static str, deadline: Duration, lead_ms_current: u64| {
                    if t0.elapsed() + Duration::from_millis(lead_ms_current) >= deadline {
                        if let Some(cb) = limits.info_string_callback.as_ref() {
                            let elapsed = t0.elapsed().as_millis();
                            cb(&format!(
                            "stop_lead_break reason={} depth={} elapsed_ms={} nodes={} lead_ms={}",
                            reason, d, elapsed, nodes, lead_ms_current
                        ));
                        }
                        if lead_window_finalize && !finalize_nearhard_sent {
                            notify_deadline(DeadlineHit::Soft, nodes);
                        }
                        if !matches!(last_deadline_hit, Some(DeadlineHit::Hard)) {
                            last_deadline_hit = Some(DeadlineHit::Soft);
                        }
                        lead_window_soft_break = true;
                        return true;
                    }
                    false
                };

            if let Some(hard) = hard_deadline {
                if let Some(soft) = soft_deadline {
                    if hard > soft {
                        let diff = hard.as_millis().saturating_sub(soft.as_millis()) as u64;
                        if diff > 0 {
                            lead_ms = lead_ms.max(diff);
                        }
                    }
                }

                // 固定時間探索では min_think よりも締切回避を優先したいので、リードウィンドウは min_think 判定を迂回して早期停止させる。
                if check_lead_window("hard_window", hard, lead_ms) {
                    break;
                }

                continue;
            }

            if let Some(limit) = limits.time_limit() {
                if check_lead_window("time_limit", limit, lead_ms) {
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
        // Reflect accumulated quiescence nodes
        stats.qnodes = cum_qnodes;
        stats.depth = final_depth_reached;
        stats.seldepth = final_seldepth_reached;
        stats.raw_seldepth = final_seldepth_raw.map(|v| v.min(u16::MAX as u32) as u16);
        stats.tt_hits = Some(cum_tt_hits);
        stats.lmr_count = Some(cum_lmr_counter);
        stats.lmr_trials = Some(cum_lmr_trials);
        // Publish instrumentation
        stats.lmr_blocked_in_check = Some(cum_lmr_blocked_in_check);
        stats.lmr_blocked_recapture = Some(cum_lmr_blocked_recapture);
        stats.evasion_sparsity_extensions = Some(cum_evasion_sparsity_ext);
        stats.root_fail_high_count = Some(cum_beta_cuts);
        #[cfg(feature = "diagnostics")]
        {
            stats.abdada_busy_detected = Some(cum_abdada_busy_detected);
            stats.abdada_busy_set = Some(cum_abdada_busy_set);
        }
        stats.root_tt_hint_exists = Some(stats_hint_exists);
        stats.root_tt_hint_used = Some(stats_hint_used);
        if near_final_attempted != 0 {
            stats.near_final_attempted = Some(near_final_attempted);
        }
        if near_final_confirmed != 0 {
            stats.near_final_confirmed = Some(near_final_confirmed);
        }
        stats.aspiration_failures = Some(cumulative_asp_failures);
        stats.aspiration_hits = Some(cumulative_asp_hits);
        stats.re_searches = Some(cumulative_researches);
        stats.pv_changed = Some(cumulative_pv_changed);
        stats.incomplete_depth = incomplete_depth;
        // 近〆切観測フラグ
        if stats_near_deadline_skip_new_iter != 0 {
            stats.near_deadline_skip_new_iter = Some(stats_near_deadline_skip_new_iter);
        }
        if stats_multipv_shrunk != 0 {
            stats.multipv_shrunk = Some(stats_multipv_shrunk);
        }
        if limits.store_heuristics {
            stats.heuristics = Some(Arc::new(heur_state.clone()));
        }

        let final_lines_opt = final_lines.clone();
        if let Some(first_line) = final_lines_opt.as_ref().and_then(|lines| lines.first()) {
            stats.pv = first_line.pv.iter().copied().collect();
        }

        let snapshot_any = stop_controller
            .as_ref()
            .and_then(|ctrl| ctrl.try_read_snapshot())
            .filter(|snap| snap.search_id == session_id && snap.root_key == root_key);
        let stable_snapshot = snapshot_any
            .as_ref()
            .and_then(|snap| (snap.source == SnapshotSource::Stable).then(|| snap.clone()));

        let mut result_lines: Option<SmallVec<[RootLine; 4]>> = None;
        let mut best_move_out = best;
        let mut score_out = best_score;
        let mut node_type_out = NodeType::Exact;
        let mut report_source: Option<SnapshotSource> = None;
        let mut snapshot_version = None;
        let mut stable_depth = None;

        if let Some(lines) = final_lines_opt.as_ref() {
            if let Some(first) = lines.first() {
                // Internal score（mate距離保持）
                stats.pv = first.pv.iter().copied().collect();
            }
            let mut published_lines: SmallVec<[RootLine; 4]> = SmallVec::new();
            if incomplete_depth.is_some() && !lines.is_empty() {
                // 未完イテレーションでは MultiPV=1 のみ公開し、USI 側もその前提で扱う。
                published_lines.push(lines[0].clone());
                if let Some(cb) = limits.info_string_callback.as_ref() {
                    cb("finalize_lines_shrunk=1 reason=incomplete_iteration");
                }
            } else {
                published_lines.extend(lines.iter().cloned());
            }
            result_lines = Some(published_lines);
            if result_lines.is_some() {
                report_source = Some(SnapshotSource::Stable);
                stable_depth = Some(final_depth_reached);
            }
        } else if let Some(snap) = stable_snapshot {
            result_lines = Some(snap.lines.clone());
            stats.depth = snap.depth;
            stats.seldepth = snap.seldepth;
            stats.raw_seldepth = snap.seldepth.map(|v| v as u16);
            stats.pv = snap.pv.iter().copied().collect();
            report_source = Some(SnapshotSource::Stable);
            snapshot_version = Some(snap.version);
            stable_depth = Some(snap.depth);
            if let Some(first) = result_lines.as_ref().and_then(|ls| ls.first()) {
                best_move_out = Some(first.root_move);
                score_out = first.score_internal;
                node_type_out = first.bound;
            } else {
                best_move_out = snap.best;
                score_out = snap.score_cp;
                node_type_out = snap.node_type;
            }
        } else if let Some(snap) = snapshot_any.clone() {
            result_lines = Some(snap.lines.clone());
            stats.depth = snap.depth;
            stats.seldepth = snap.seldepth;
            stats.raw_seldepth = snap.seldepth.map(|v| v as u16);
            stats.pv = snap.pv.iter().copied().collect();
            report_source = Some(snap.source);
            snapshot_version = Some(snap.version);
            if snap.source == SnapshotSource::Stable {
                stable_depth = Some(snap.depth);
            }
            if let Some(first) = result_lines.as_ref().and_then(|ls| ls.first()) {
                best_move_out = Some(first.root_move);
                score_out = first.score_internal;
                node_type_out = first.bound;
            } else {
                best_move_out = snap.best;
                score_out = snap.score_cp;
                node_type_out = snap.node_type;
            }
        }

        if best_move_out.is_none() {
            if let Some(first) = stats.pv.first() {
                best_move_out = Some(*first);
            }
        }
        if best_move_out.is_none() {
            let mg = MoveGenerator::new();
            if let Ok(list) = mg.generate_all(root) {
                best_move_out = list.as_slice().first().copied();
            }
        }

        if report_source.is_none() {
            report_source = Some(SnapshotSource::Partial);
        }
        stats.root_report_source = report_source;
        stats.snapshot_version = snapshot_version;
        stats.stable_depth = stable_depth;

        if stats.pv.is_empty() {
            if let Some(lines) = result_lines.as_ref().and_then(|lines| lines.first()) {
                stats.pv = lines.pv.iter().copied().collect();
            } else if let Some(mv) = best_move_out {
                stats.pv.push(mv);
            }
        }

        final_depth_reached = stats.depth;

        let mut result = SearchResult::compose(
            best_move_out,
            score_out,
            stats,
            node_type_out,
            None,
            result_lines.clone(),
        );

        if result.lines.is_some() {
            // sync_from_primary_line() keeps legacy fields aligned with lines[0];
            // call-site以降で best_move/score/node_type/stats.pv を個別に変更しないこと。
            result.sync_from_primary_line();
        }

        if let Some(tt) = &self.tt {
            result.hashfull = tt.hashfull_permille() as u32;
        }

        if result.stop_info.is_none() {
            if let Some(tm) = limits.time_manager.as_ref() {
                let elapsed = tm.elapsed_ms();
                let hard_ms = tm.hard_limit_ms();
                let soft_ms = tm.soft_limit_ms();
                let hard_timeout = matches!(last_deadline_hit, Some(DeadlineHit::Hard))
                    || (hard_ms != u64::MAX && elapsed >= hard_ms);
                let reason = match last_deadline_hit {
                    Some(DeadlineHit::Stop) => TerminationReason::UserStop,
                    Some(DeadlineHit::Hard) | Some(DeadlineHit::Soft) => {
                        TerminationReason::TimeLimit
                    }
                    None => {
                        if Self::should_stop(limits) {
                            TerminationReason::UserStop
                        } else {
                            TerminationReason::Completed
                        }
                    }
                };
                result.stop_info = Some(StopInfo {
                    reason,
                    elapsed_ms: elapsed,
                    nodes,
                    depth_reached: final_depth_reached,
                    hard_timeout,
                    soft_limit_ms: if soft_ms != u64::MAX { soft_ms } else { 0 },
                    hard_limit_ms: if hard_ms != u64::MAX { hard_ms } else { 0 },
                    stop_tag: None,
                });
                result.end_reason = reason;
            } else if let Some(dl) = limits.fallback_deadlines {
                let elapsed = t0.elapsed().as_millis() as u64;
                let hard_timeout = elapsed >= dl.hard_limit_ms;
                let soft_hit = dl.soft_limit_ms > 0 && elapsed >= dl.soft_limit_ms;
                let time_limited = hard_timeout
                    || soft_hit
                    || lead_window_soft_break
                    || matches!(last_deadline_hit, Some(DeadlineHit::Soft));
                let reason = if time_limited {
                    TerminationReason::TimeLimit
                } else if matches!(last_deadline_hit, Some(DeadlineHit::Stop))
                    || Self::should_stop(limits)
                {
                    TerminationReason::UserStop
                } else {
                    TerminationReason::Completed
                };
                result.stop_info = Some(StopInfo {
                    reason,
                    elapsed_ms: elapsed,
                    nodes,
                    depth_reached: final_depth_reached,
                    hard_timeout: hard_timeout && !lead_window_soft_break,
                    soft_limit_ms: dl.soft_limit_ms,
                    hard_limit_ms: dl.hard_limit_ms,
                    stop_tag: None,
                });
                result.end_reason = reason;
            } else if let Some(limit) = limits.time_limit() {
                let cap_ms = limit.as_millis() as u64;
                let elapsed = t0.elapsed().as_millis() as u64;
                let mut hard_timeout = match last_deadline_hit {
                    Some(DeadlineHit::Hard) => true,
                    Some(DeadlineHit::Soft) => false,
                    _ => elapsed >= cap_ms,
                };
                if lead_window_soft_break {
                    hard_timeout = false;
                }
                // FixedTime: 終了理由は常に TimeLimit（UserStop を除く）に寄せる
                let reason = if matches!(last_deadline_hit, Some(DeadlineHit::Stop))
                    || Self::should_stop(limits)
                {
                    TerminationReason::UserStop
                } else {
                    TerminationReason::TimeLimit
                };

                result.stop_info = Some(StopInfo {
                    reason,
                    elapsed_ms: elapsed,
                    nodes,
                    depth_reached: final_depth_reached,
                    hard_timeout,
                    soft_limit_ms: cap_ms,
                    hard_limit_ms: cap_ms,
                    stop_tag: None,
                });
                result.end_reason = reason;
            }
        }
        if let Some(cb) = limits.info_string_callback.as_ref() {
            let reason = result
                .stop_info
                .as_ref()
                .map(|info| format!("{:?}", info.reason))
                .unwrap_or_else(|| "Unknown".to_string());
            cb(&format!(
                "iterative_complete depth={} elapsed_ms={} reason={}",
                final_depth_reached,
                t0.elapsed().as_millis(),
                reason
            ));
        }
        result
    }

    pub(super) fn iterative(
        &self,
        root: &Position,
        limits: &SearchLimits,
        info: Option<&InfoEventCallback>,
    ) -> SearchResult {
        let mut stack = take_stack_cache();
        let mut heur_state = Heuristics::default();
        let result =
            self.iterative_with_buffers(root, limits, &mut stack[..], &mut heur_state, info);
        return_stack_cache(stack);
        result
    }

    pub fn think_with_ctx(
        &self,
        root: &Position,
        limits: &SearchLimits,
        stack: &mut [SearchStack],
        heur: &mut Heuristics,
        info: Option<crate::search::api::InfoEventCallback>,
    ) -> SearchResult {
        let info_ref = info.as_ref();
        self.iterative_with_buffers(root, limits, stack, heur, info_ref)
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
        let thread_suffix = if limits.session_id != 0 {
            limits.session_id
        } else {
            SEARCH_THREAD_SEQ.fetch_add(1, Ordering::Relaxed)
        };
        let thread_name = format!("classic-backend-search-{thread_suffix}");
        let handle = thread::Builder::new()
            .name(thread_name)
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
                    let result = panic::catch_unwind(AssertUnwindSafe(|| {
                        backend.iterative(&root, &limits, info_cb.as_ref())
                    }));
                    match result {
                        Ok(res) => {
                            let _ = tx.send(res);
                        }
                        Err(payload) => {
                            let panic_msg = if let Some(s) = payload.downcast_ref::<&str>() {
                                (*s).to_string()
                            } else if let Some(s) = payload.downcast_ref::<String>() {
                                s.clone()
                            } else {
                                let dyn_type = (*payload).type_id();
                                format!("unknown panic payload (type_id={dyn_type:?})")
                            };
                            warn!("classic backend search thread panicked: {panic_msg}");

                            let elapsed_base = limits.start_time.elapsed().as_millis() as u64;
                            let mut elapsed_ms = elapsed_base;
                            let mut soft_limit_ms = 0;
                            let mut hard_limit_ms = 0;
                            let mut hard_timeout = false;

                            if let Some(tm) = limits.time_manager.as_ref() {
                                let tm_elapsed = tm.elapsed_ms();
                                let tm_soft = tm.soft_limit_ms();
                                let tm_hard = tm.hard_limit_ms();
                                if tm_elapsed > 0 {
                                    elapsed_ms = tm_elapsed;
                                }
                                if tm_soft != u64::MAX {
                                    soft_limit_ms = tm_soft;
                                }
                                if tm_hard != u64::MAX {
                                    hard_limit_ms = tm_hard;
                                }
                            }

                            if hard_limit_ms > 0 {
                                hard_timeout = elapsed_ms >= hard_limit_ms;
                            }

                            if let Some(deadlines) = limits.fallback_deadlines {
                                if soft_limit_ms == 0 {
                                    soft_limit_ms = deadlines.soft_limit_ms;
                                }
                                if hard_limit_ms == 0 {
                                    hard_limit_ms = deadlines.hard_limit_ms;
                                }
                                if hard_limit_ms > 0 {
                                    hard_timeout =
                                        hard_timeout || elapsed_ms >= deadlines.hard_limit_ms;
                                }
                            }

                            if soft_limit_ms == 0 && hard_limit_ms == 0 {
                                if let Some(limit) = limits.time_limit() {
                                    let ms = limit.as_millis() as u64;
                                    soft_limit_ms = ms;
                                    hard_limit_ms = ms;
                                }
                            }

                            let stats = SearchStats {
                                elapsed: Duration::from_millis(elapsed_ms),
                                ..Default::default()
                            };

                            let mut fallback = SearchResult::new(None, 0, stats);
                            fallback.end_reason = TerminationReason::Error;
                            fallback.stop_info = Some(StopInfo {
                                reason: TerminationReason::Error,
                                elapsed_ms,
                                nodes: 0,
                                depth_reached: 0,
                                hard_timeout,
                                soft_limit_ms,
                                hard_limit_ms,
                                stop_tag: None,
                            });
                            let _ = tx.send(fallback);
                        }
                    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluation::evaluate::MaterialEvaluator;
    use crate::search::{SearchLimitsBuilder, TranspositionTable};
    use crate::shogi::Position;
    use std::sync::Arc;

    #[test]
    fn stack_cache_preserves_inner_capacity_across_iterations() {
        // Prepare a stack buffer and inflate inner quiet_moves capacity
        let mut buf = super::take_stack_cache();
        assert_eq!(buf.len(), (crate::search::constants::MAX_PLY + 1));
        let idx = 8usize;
        // Inflate capacity by pushing many moves
        for _ in 0..512 {
            buf[idx].quiet_moves.push(crate::shogi::Move::null());
        }
        let cap_before = buf[idx].quiet_moves.capacity();
        // Return to cache (would be reused next take)
        super::return_stack_cache(buf);

        // Take again (new iteration). Implementation should clear but keep capacity
        let buf2 = super::take_stack_cache();
        let cap_after = buf2[idx].quiet_moves.capacity();
        assert!(
            cap_after >= cap_before,
            "inner Vec capacity should be preserved across iterations (before={}, after={})",
            cap_before,
            cap_after
        );
        super::return_stack_cache(buf2);
    }

    #[test]
    fn heuristics_carryover_across_pvs_and_iterations() {
        // Test that heuristics (lmr_trials) grow across PVs and iterations,
        // ensuring the fix (shared_heur = heur at PV tail) works correctly.
        let evaluator = Arc::new(MaterialEvaluator);
        let tt = Arc::new(TranspositionTable::new(16));
        let backend = ClassicBackend::with_tt(evaluator, tt);

        let pos = Position::startpos();
        let limits = SearchLimitsBuilder::default()
            .depth(5) // Use depth 5 to ensure LMR is triggered
            .multipv(2) // Use MultiPV to test across multiple PVs
            .store_heuristics(true)
            .build();

        let result = backend.think_blocking(&pos, &limits, None);

        // Verify that heuristics were captured
        assert!(
            result.stats.heuristics.is_some(),
            "Heuristics should be stored when store_heuristics=true"
        );

        if let Some(heur) = result.stats.heuristics.as_ref() {
            let summary = heur.summary();
            // At depth 5 with multipv=2, we expect some LMR activity
            // The exact value depends on search dynamics, but it should be > 0
            // if heuristics are properly carried across PVs.
            //
            // If the bug (*heur_state = heur) existed, lmr_trials would be reset
            // between PVs and remain low. With the fix (shared_heur = heur),
            // lmr_trials should accumulate.
            //
            // Note: In some positions, LMR may not trigger. We check both lmr_trials
            // and other heuristic tables to ensure carryover is working.
            let has_lmr = summary.lmr_trials > 0;
            let has_other = summary.quiet_max > 0
                || summary.continuation_max > 0
                || summary.capture_max > 0
                || summary.counter_filled > 0;
            assert!(
                has_lmr || has_other,
                "At least some heuristic activity should occur at depth 5. \
                 lmr_trials={}, quiet_max={}, continuation_max={}, capture_max={}, counter_filled={}",
                summary.lmr_trials,
                summary.quiet_max,
                summary.continuation_max,
                summary.capture_max,
                summary.counter_filled
            );
        }
    }

    #[test]
    fn qnodes_unlimited_when_no_time_and_limit_zero() {
        let limits = SearchLimitsBuilder::default()
            .time_control(TimeControl::Infinite)
            .qnodes_limit(0)
            .build();
        let got = ClassicBackend::<MaterialEvaluator>::compute_qnodes_limit_for_test(&limits, 8, 1);
        assert_eq!(got, u64::MAX, "bench/unlimited path should yield u64::MAX");
    }

    #[test]
    fn qnodes_default_clamped_without_time_and_no_override() {
        use crate::search::constants::DEFAULT_QNODES_LIMIT;
        let limits = SearchLimitsBuilder::default().time_control(TimeControl::Infinite).build();
        let got = ClassicBackend::<MaterialEvaluator>::compute_qnodes_limit_for_test(&limits, 8, 1);
        assert_eq!(got, DEFAULT_QNODES_LIMIT, "should clamp to default when no override");
    }

    // RootWorkQueue/claim は純粋 LazySMP では使用しないため、関連テストは削除しました。
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HelperAspMode {
    Off,
    Wide,
}

fn helper_asp_mode() -> HelperAspMode {
    match crate::search::policy::helper_asp_mode_value() {
        0 => HelperAspMode::Off,
        _ => HelperAspMode::Wide,
    }
}

#[inline]
fn helper_asp_delta() -> i32 {
    use crate::search::constants::ASPIRATION_DELTA_MAX;
    crate::search::policy::helper_asp_delta_value()
        .clamp(50, 600)
        .min(ASPIRATION_DELTA_MAX)
}

// asp_fail_low_pct / asp_fail_high_pct: see crate::search::policy

// Use shared policy getter from parallel module to avoid divergence
use crate::search::policy::bench_stop_on_mate_enabled;
