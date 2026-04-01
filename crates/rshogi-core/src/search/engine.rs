//! 探索エンジンのエントリポイント
//!
//! USIプロトコルから呼び出すためのハイレベルインターフェース。

use crate::eval::EvalHash;
use crate::time::Instant;
use std::collections::HashMap;
// AtomicU64 is only needed for native multi-threaded builds.
// Wasm Rayon model doesn't use SearchProgress.
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use super::time_manager::{
    DEFAULT_MAX_MOVES_TO_DRAW, calculate_falling_eval, calculate_time_reduction,
    normalize_nodes_effort,
};
use super::{
    DEFAULT_DRAW_VALUE_BLACK, DEFAULT_DRAW_VALUE_WHITE, LimitsType, RootMove, SearchTuneParams,
    SearchWorker, Skill, SkillOptions, ThreadPool, TimeManagement,
};
use crate::position::Position;
use crate::tt::TranspositionTable;
use crate::types::{Depth, EnteringKingRule, MAX_PLY, Move, Value};

// =============================================================================
// SearchInfo - 探索情報（USI info出力用）
// =============================================================================

/// 探索情報（USI info出力用）
#[derive(Debug, Clone)]
pub struct SearchInfo {
    /// 探索深さ
    pub depth: Depth,
    /// 選択的深さ
    pub sel_depth: i32,
    /// 最善手のスコア
    pub score: Value,
    /// 探索ノード数
    pub nodes: u64,
    /// 経過時間（ミリ秒）
    pub time_ms: u64,
    /// NPS (nodes per second)
    pub nps: u64,
    /// 置換表使用率（千分率）
    pub hashfull: u32,
    /// Principal Variation
    pub pv: Vec<Move>,
    /// MultiPV番号（1-indexed）
    pub multi_pv: usize,
}

impl SearchInfo {
    /// USI形式のinfo文字列を生成
    pub fn to_usi_string(&self) -> String {
        let score_str =
            if self.score.is_mate_score() && self.score.raw().abs() < Value::INFINITE.raw() {
                // USIでは手数(plies)で出力し、負値は自分が詰まされる側を示す
                let mate_ply = self.score.mate_ply();
                let signed_ply = if self.score.is_loss() {
                    -mate_ply
                } else {
                    mate_ply
                };
                format!("mate {signed_ply}")
            } else {
                format!("cp {}", self.score.to_cp())
            };

        let mut s = format!(
            "info depth {depth} seldepth {sel_depth} multipv {multi_pv} score {score} nodes {nodes} time {time_ms} nps {nps} hashfull {hashfull}",
            depth = self.depth,
            sel_depth = self.sel_depth,
            multi_pv = self.multi_pv,
            score = score_str,
            nodes = self.nodes,
            time_ms = self.time_ms,
            nps = self.nps,
            hashfull = self.hashfull
        );

        if !self.pv.is_empty() {
            s.push_str(" pv");
            for m in &self.pv {
                s.push(' ');
                s.push_str(&m.to_usi());
            }
        }

        s
    }
}

/// aspiration windowを計算
pub(crate) fn compute_aspiration_window(
    rm: &RootMove,
    thread_id: usize,
    tune_params: &SearchTuneParams,
) -> (Value, Value, Value) {
    // mean_squared_score がない場合は巨大なdeltaでフルウィンドウにする
    let fallback = {
        let inf = Value::INFINITE.raw() as i64;
        inf * inf
    };
    let mean_sq = rm.mean_squared_score.unwrap_or(fallback).abs();
    let mean_sq = mean_sq.min((Value::INFINITE.raw() as i64) * (Value::INFINITE.raw() as i64));

    let thread_offset = (thread_id % 8) as i32;
    let divisor = tune_params.aspiration_mean_sq_div.max(1) as i64;
    let delta_raw = tune_params.aspiration_delta_base
        + thread_offset
        + (mean_sq / divisor).min(i32::MAX as i64) as i32;
    let delta = Value::new(delta_raw);
    let alpha_raw = (rm.average_score.raw() - delta.raw()).max(-Value::INFINITE.raw());
    let beta_raw = (rm.average_score.raw() + delta.raw()).min(Value::INFINITE.raw());

    (Value::new(alpha_raw), Value::new(beta_raw), delta)
}

/// 詰みスコアに対する深さ打ち切り判定
#[inline]
fn proven_mate_depth_exceeded(best_value: Value, depth: Depth) -> bool {
    if best_value.is_win() || best_value.is_loss() {
        let mate_ply = best_value.mate_ply();
        return (mate_ply + 2) * 5 / 2 < depth;
    }

    false
}

/// `go mate` 指定時に、要求手数以内の詰みが見つかったか判定する
#[inline]
fn mate_within_limit(
    best_value: Value,
    score_lower_bound: bool,
    score_upper_bound: bool,
    mate_limit_moves: i32,
) -> bool {
    if mate_limit_moves <= 0
        || score_lower_bound
        || score_upper_bound
        || !best_value.is_mate_score()
    {
        return false;
    }

    let mate_ply = best_value.mate_ply() as i64;
    let limit_plies = (mate_limit_moves as i64).saturating_mul(2);

    mate_ply <= limit_plies
}

// =============================================================================
// SearchResult - 探索結果
// =============================================================================

/// 探索結果
#[derive(Debug, Clone)]
pub struct SearchResult {
    /// 最善手
    pub best_move: Move,
    /// Ponder手（相手の予想応手）
    pub ponder_move: Move,
    /// 最善手のスコア
    pub score: Value,
    /// 完了した探索深さ
    pub depth: Depth,
    /// 探索ノード数
    pub nodes: u64,
    /// Principal Variation（読み筋）
    pub pv: Vec<Move>,
    /// 探索統計レポート（search-stats feature有効時のみ内容あり）
    pub stats_report: String,
}

// =============================================================================
// Search - 探索エンジン
// =============================================================================

/// 探索エンジン
///
/// USIプロトコルから呼び出すための主要インターフェース。
/// デフォルトのEvalHashサイズ（MB）
pub const DEFAULT_EVAL_HASH_SIZE_MB: usize = 64;

pub struct Search {
    /// 置換表
    tt: Arc<TranspositionTable>,
    /// 評価ハッシュ（NNUE評価値キャッシュ）
    eval_hash: Arc<EvalHash>,
    /// 置換表のサイズ（MB）
    tt_size_mb: usize,
    /// EvalHashのサイズ（MB）
    eval_hash_size_mb: usize,
    /// 停止フラグ
    stop: Arc<AtomicBool>,
    /// ponderhit通知フラグ
    ponderhit_flag: Arc<AtomicBool>,
    /// 探索開始時刻
    start_time: Option<Instant>,
    /// 時間オプション
    time_options: super::TimeOptions,
    /// Skill Level オプション
    skill_options: SkillOptions,

    /// 探索スレッド数
    num_threads: usize,
    /// 探索スレッドプール（helper threads）
    thread_pool: ThreadPool,

    /// SearchWorker（長期保持して再利用）
    /// 履歴統計を含み、usinewgameでクリア、goでは保持
    worker: Option<Box<SearchWorker>>,

    /// 直前イテレーションのスコア
    best_previous_score: Option<Value>,
    /// 直前イテレーションの平均スコア
    best_previous_average_score: Option<Value>,
    /// 直近のイテレーション値（YaneuraOuは4要素リングバッファ）
    iter_value: [Value; 4],
    /// iter_valueの書き込み位置
    iter_idx: usize,
    /// 直前に安定したとみなした深さ
    last_best_move_depth: Depth,
    /// 直前の最善手（PV変化検出用）
    last_best_move: Move,
    /// totBestMoveChanges（世代減衰込み）
    tot_best_move_changes: f64,
    /// 直前の timeReduction次手に持ち回る）
    previous_time_reduction: f64,
    /// 直前の手数（手番反転の検出用）
    last_game_ply: Option<i32>,
    /// 次のiterationで深さを伸ばすかどうか
    increase_depth: bool,
    /// helperスレッドと共有するincrease_depthフラグ（main_manager()->increaseDepth）
    increase_depth_shared: Arc<AtomicBool>,
    /// 深さを伸ばせなかった回数（aspiration時の調整に使用）
    search_again_counter: i32,

    /// 引き分けまでの最大手数（エンジンオプション）
    max_moves_to_draw: i32,
    /// YaneuraOuオプション `DrawValueBlack`
    draw_value_black: i32,
    /// YaneuraOuオプション `DrawValueWhite`
    draw_value_white: i32,
    /// SPSA向け探索係数
    search_tune_params: SearchTuneParams,
    /// 入玉宣言勝ちルール
    entering_king_rule: EnteringKingRule,
}

/// best_move_changes を集約する（並列探索対応のためのヘルパー）
///
/// - `changes`: 各スレッドのbest_move_changes
/// - 戻り値: (合計, スレッド数)。スレッド数0の場合は(0.0, 1)を返しゼロ除算を避ける。
fn aggregate_best_move_changes(changes: &[f64]) -> (f64, usize) {
    if changes.is_empty() {
        return (0.0, 1);
    }
    let sum: f64 = changes.iter().copied().sum();
    (sum, changes.len())
}

// SearchProgress is only used in native multi-threaded builds.
// Wasm Rayon model doesn't use SearchProgress (passes None to search_helper).
#[cfg(not(target_arch = "wasm32"))]
/// SearchProgress はヘルパースレッドの進捗を追跡する。
/// False Sharing を防ぐため、各フィールドを別々のキャッシュラインに配置する。
#[repr(C, align(64))]
pub(crate) struct SearchProgress {
    nodes: AtomicU64,
    _pad1: [u8; 56], // 64バイト境界までパディング
    best_move_changes_bits: AtomicU64,
    _pad2: [u8; 56], // 64バイト境界までパディング
}

#[cfg(not(target_arch = "wasm32"))]
impl SearchProgress {
    pub(crate) fn new() -> Self {
        Self {
            nodes: AtomicU64::new(0),
            _pad1: [0; 56],
            best_move_changes_bits: AtomicU64::new(0.0f64.to_bits()),
            _pad2: [0; 56],
        }
    }

    pub(crate) fn reset(&self) {
        self.nodes.store(0, Ordering::Relaxed);
        self.best_move_changes_bits.store(0.0f64.to_bits(), Ordering::Relaxed);
    }

    pub(crate) fn update(&self, nodes: u64, best_move_changes: f64) {
        self.nodes.store(nodes, Ordering::Relaxed);
        self.best_move_changes_bits
            .store(best_move_changes.to_bits(), Ordering::Relaxed);
    }

    pub(crate) fn nodes(&self) -> u64 {
        self.nodes.load(Ordering::Relaxed)
    }

    pub(crate) fn best_move_changes(&self) -> f64 {
        f64::from_bits(self.best_move_changes_bits.load(Ordering::Relaxed))
    }
}

struct ThreadSummary {
    id: usize,
    score: Value,
    completed_depth: Depth,
    best_move: Move,
    pv_len: usize,
}

impl ThreadSummary {
    fn from_worker(id: usize, worker: &SearchWorker) -> Option<Self> {
        worker.state.root_moves.get(0).map(|rm| {
            let best_move = rm.pv.first().copied().unwrap_or_else(|| rm.mv());
            Self {
                id,
                score: rm.score,
                completed_depth: worker.state.completed_depth,
                best_move,
                pv_len: rm.pv.len(),
            }
        })
    }
}

#[inline]
fn thread_voting_value(summary: &ThreadSummary, min_score: Value) -> i64 {
    (summary.score.raw() - min_score.raw() + 14) as i64 * summary.completed_depth as i64
}

#[inline]
fn is_proven_loss(score: Value) -> bool {
    score != Value::new(-Value::INFINITE.raw()) && score.is_loss()
}

fn select_best_summary_index(summaries: &[ThreadSummary]) -> usize {
    if summaries.is_empty() {
        return 0;
    }

    let min_score =
        summaries.iter().map(|s| s.score).min_by_key(|s| s.raw()).unwrap_or(Value::ZERO);

    let mut votes = HashMap::with_capacity(2 * summaries.len());
    for summary in summaries {
        *votes.entry(summary.best_move).or_insert(0i64) += thread_voting_value(summary, min_score);
    }

    let mut best_idx = 0usize;
    for (idx, summary) in summaries.iter().enumerate() {
        let best = &summaries[best_idx];
        let best_vote = *votes.get(&best.best_move).unwrap_or(&0);
        let new_vote = *votes.get(&summary.best_move).unwrap_or(&0);

        let best_in_proven_win = best.score.is_win();
        let new_in_proven_win = summary.score.is_win();
        let best_in_proven_loss = is_proven_loss(best.score);
        let new_in_proven_loss = is_proven_loss(summary.score);

        let better_voting_value = thread_voting_value(summary, min_score)
            * (summary.pv_len > 2) as i64
            > thread_voting_value(best, min_score) * (best.pv_len > 2) as i64;

        if best_in_proven_win {
            if summary.score > best.score {
                best_idx = idx;
            }
        } else if best_in_proven_loss {
            if new_in_proven_loss && summary.score < best.score {
                best_idx = idx;
            }
        } else if new_in_proven_win
            || new_in_proven_loss
            || (!summary.score.is_loss()
                && (new_vote > best_vote || (new_vote == best_vote && better_voting_value)))
        {
            best_idx = idx;
        }
    }

    // main threadと異なる手をhelper 1本だけが強く主張している場合は、
    // outlierに引っ張られて不自然な手を返しやすいためmainを優先する。
    if best_idx != 0 {
        let main = &summaries[0];
        let selected = &summaries[best_idx];
        if selected.best_move != main.best_move && !selected.score.is_mate_score() {
            let support_count =
                summaries.iter().filter(|s| s.best_move == selected.best_move).count();
            if support_count < 2 {
                return 0;
            }
        }
    }

    best_idx
}

#[inline]
fn best_thread_debug_enabled() -> bool {
    std::env::var("RSHOGI_DEBUG_BEST_THREAD")
        .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "on" | "ON"))
        .unwrap_or(false)
}

#[inline]
fn helper_search_disabled() -> bool {
    std::env::var("RSHOGI_DISABLE_HELPER_SEARCH")
        .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "on" | "ON"))
        .unwrap_or(false)
}

fn emit_best_thread_debug(
    summaries: &[ThreadSummary],
    votes: &HashMap<Move, i64>,
    selected_id: usize,
    applied: bool,
) {
    println!(
        "info string [best_thread] applied={} selected_id={} threads={}",
        applied,
        selected_id,
        summaries.len()
    );

    for summary in summaries {
        let vote = votes.get(&summary.best_move).copied().unwrap_or(0);
        println!(
            "info string [best_thread] id={} depth={} score={} move={} pv_len={} vote={}",
            summary.id,
            summary.completed_depth,
            summary.score.raw(),
            summary.best_move.to_usi(),
            summary.pv_len,
            vote
        );
    }
}

fn collect_thread_summaries(
    main_worker: &SearchWorker,
    thread_pool: &ThreadPool,
) -> Vec<ThreadSummary> {
    let mut summaries = Vec::new();
    if let Some(summary) = ThreadSummary::from_worker(0, main_worker) {
        summaries.push(summary);
    }

    // Native: Use helper_threads() to access Thread objects directly
    #[cfg(not(target_arch = "wasm32"))]
    for thread in thread_pool.helper_threads() {
        if let Some(summary) = thread.with_worker(|worker: &mut SearchWorker| {
            ThreadSummary::from_worker(thread.id(), worker)
        }) {
            summaries.push(summary);
        }
    }

    // Wasm with wasm-threads: Use helper_results() to get collected results
    #[cfg(all(target_arch = "wasm32", feature = "wasm-threads"))]
    for result in thread_pool.helper_results() {
        summaries.push(ThreadSummary {
            id: result.thread_id,
            score: result.best_score,
            completed_depth: result.completed_depth,
            best_move: result.best_move,
            // Wasm helper結果にはPV長がないため、切り詰め判定を不利にしない値を与える。
            pv_len: 3,
        });
    }

    // Wasm without wasm-threads: No helper threads, suppress unused warning
    #[cfg(all(target_arch = "wasm32", not(feature = "wasm-threads")))]
    let _ = thread_pool;

    summaries
}

fn should_use_best_thread_selection(limits: &LimitsType, skill_enabled: bool) -> bool {
    // - MultiPV=1
    // - go depth/mate では使わない
    // - Skill有効時は使わない
    limits.multi_pv == 1 && limits.depth == 0 && limits.mate == 0 && !skill_enabled
}

fn get_best_thread_id(
    main_worker: &SearchWorker,
    thread_pool: &ThreadPool,
    use_best_thread: bool,
    debug: bool,
) -> usize {
    let summaries = collect_thread_summaries(main_worker, thread_pool);
    if summaries.is_empty() {
        return 0;
    }

    let min_score =
        summaries.iter().map(|s| s.score).min_by_key(|s| s.raw()).unwrap_or(Value::ZERO);
    let mut votes = HashMap::with_capacity(2 * summaries.len());
    for summary in &summaries {
        *votes.entry(summary.best_move).or_insert(0i64) += thread_voting_value(summary, min_score);
    }

    let candidate_idx = select_best_summary_index(&summaries);
    let candidate_id = summaries[candidate_idx].id;
    let selected_id = if use_best_thread { candidate_id } else { 0 };

    if debug {
        emit_best_thread_debug(&summaries, &votes, selected_id, use_best_thread);
    }

    selected_id
}

struct BestThreadResult {
    best_move: Move,
    ponder_move: Move,
    score: Value,
    completed_depth: Depth,
    nodes: u64,
    best_previous_score: Option<Value>,
    best_previous_average_score: Option<Value>,
    pv: Vec<Move>,
}

fn collect_best_thread_result(
    worker: &SearchWorker,
    limits: &LimitsType,
    skill_enabled: bool,
    skill: &mut Skill,
) -> BestThreadResult {
    let completed_depth = worker.state.completed_depth;
    let nodes = worker.state.nodes;
    let best_previous_score = worker.state.root_moves.get(0).map(|rm| rm.score);
    let best_previous_average_score = worker.state.root_moves.get(0).map(|rm| {
        if rm.average_score.raw() == -Value::INFINITE.raw() {
            rm.score
        } else {
            rm.average_score
        }
    });

    if worker.state.root_moves.is_empty() {
        return BestThreadResult {
            best_move: Move::NONE,
            ponder_move: Move::NONE,
            score: Value::ZERO,
            completed_depth,
            nodes,
            best_previous_score,
            best_previous_average_score,
            pv: Vec::new(),
        };
    }

    let mut effective_multi_pv = limits.multi_pv;
    if skill_enabled {
        effective_multi_pv = effective_multi_pv.max(4);
    }
    effective_multi_pv = effective_multi_pv.min(worker.state.root_moves.len());

    let mut best_move = worker.state.best_move;
    if skill_enabled && effective_multi_pv > 0 {
        let mut rng = rand::rng();
        let best = skill.pick_best(&worker.state.root_moves, effective_multi_pv, &mut rng);
        if best != Move::NONE {
            best_move = best;
        }
    }

    let best_rm = worker.state.root_moves.iter().find(|rm| rm.mv() == best_move);

    let ponder_move = best_rm
        .and_then(|rm| {
            if rm.pv.len() > 1 {
                Some(rm.pv[1])
            } else {
                None
            }
        })
        .unwrap_or(Move::NONE);

    let score = best_rm
        .map(|rm| rm.score)
        .unwrap_or(worker.state.root_moves.get(0).map(|rm| rm.score).unwrap_or(Value::ZERO));

    let pv = best_rm.map(|rm| rm.pv.clone()).unwrap_or_default();

    BestThreadResult {
        best_move,
        ponder_move,
        score,
        completed_depth,
        nodes,
        best_previous_score,
        best_previous_average_score,
        pv,
    }
}

impl Search {
    /// 時間計測用のメトリクスを準備（対局/Go開始時）
    fn prepare_time_metrics(&mut self, ply: i32) {
        // 手番が変わっている場合はスコア符号を反転
        if let Some(last_ply) = self.last_game_ply
            && (last_ply - ply).abs() & 1 == 1
        {
            if let Some(prev_score) = self.best_previous_score
                && prev_score != Value::INFINITE
            {
                self.best_previous_score = Some(Value::new(-prev_score.raw()));
            }
            if let Some(prev_avg) = self.best_previous_average_score
                && prev_avg != Value::INFINITE
            {
                self.best_previous_average_score = Some(Value::new(-prev_avg.raw()));
            }
        }

        // best_previous_score が番兵(INFINITE)のときは 0 初期化
        if self.best_previous_score == Some(Value::INFINITE) {
            self.iter_value = [Value::ZERO; 4];
        } else {
            let seed = self.best_previous_score.unwrap_or(Value::ZERO);
            self.iter_value = [seed; 4];
        }
        self.iter_idx = 0;
        self.last_best_move_depth = 0;
        self.last_best_move = Move::NONE;
        self.tot_best_move_changes = 0.0;
        self.last_game_ply = Some(ply);
        self.increase_depth = true;
        self.increase_depth_shared.store(true, Ordering::Relaxed);
        self.search_again_counter = 0;
    }

    /// 新しいSearchを作成
    ///
    /// # Arguments
    /// * `tt_size_mb` - 置換表のサイズ（MB）
    pub fn new(tt_size_mb: usize) -> Self {
        Self::new_with_eval_hash(tt_size_mb, DEFAULT_EVAL_HASH_SIZE_MB)
    }

    /// 新しいSearchを作成し、EvalHashサイズも同時に指定する。
    ///
    /// # Arguments
    /// * `tt_size_mb` - 置換表のサイズ（MB）
    /// * `eval_hash_size_mb` - EvalHash のサイズ（MB）
    pub fn new_with_eval_hash(tt_size_mb: usize, eval_hash_size_mb: usize) -> Self {
        let tt = Arc::new(TranspositionTable::new(tt_size_mb));
        let eval_hash = Arc::new(EvalHash::new(eval_hash_size_mb));
        let stop = Arc::new(AtomicBool::new(false));
        let ponderhit_flag = Arc::new(AtomicBool::new(false));
        let increase_depth_shared = Arc::new(AtomicBool::new(true));
        let max_moves_to_draw = DEFAULT_MAX_MOVES_TO_DRAW;
        let search_tune_params = SearchTuneParams::default();
        let thread_pool = ThreadPool::new(
            1,
            Arc::clone(&tt),
            Arc::clone(&eval_hash),
            Arc::clone(&stop),
            Arc::clone(&ponderhit_flag),
            Arc::clone(&increase_depth_shared),
            max_moves_to_draw,
            search_tune_params,
        );

        Self {
            tt,
            eval_hash,
            tt_size_mb,
            eval_hash_size_mb,
            stop,
            ponderhit_flag,
            start_time: None,
            time_options: super::TimeOptions::default(),
            skill_options: SkillOptions::default(),
            num_threads: 1,
            thread_pool,
            // workerは遅延初期化（最初のgoで作成）
            worker: None,
            best_previous_score: Some(Value::INFINITE),
            best_previous_average_score: Some(Value::INFINITE),
            iter_value: [Value::ZERO; 4],
            iter_idx: 0,
            last_best_move_depth: 0,
            last_best_move: Move::NONE,
            tot_best_move_changes: 0.0,
            previous_time_reduction: 0.85,
            last_game_ply: None,
            increase_depth: true,
            increase_depth_shared,
            search_again_counter: 0,
            max_moves_to_draw,
            draw_value_black: DEFAULT_DRAW_VALUE_BLACK,
            draw_value_white: DEFAULT_DRAW_VALUE_WHITE,
            search_tune_params,
            entering_king_rule: EnteringKingRule::default(),
        }
    }

    /// 置換表のサイズを変更
    pub fn resize_tt(&mut self, size_mb: usize) {
        self.tt = Arc::new(TranspositionTable::new(size_mb));
        self.tt_size_mb = size_mb;
        // workerが存在する場合、TT参照を更新
        if let Some(worker) = &mut self.worker {
            worker.tt = Arc::clone(&self.tt);
        }
        self.thread_pool.update_tt(Arc::clone(&self.tt));
    }

    /// 置換表をクリア
    ///
    /// 新しい置換表を作成して置き換える。
    pub fn clear_tt(&mut self) {
        // Arc経由では&mutが取れないので、同じサイズの新しいTTを作成して置き換える
        self.tt = Arc::new(TranspositionTable::new(self.tt_size_mb));
        // workerが存在する場合、TT参照を更新
        if let Some(worker) = &mut self.worker {
            worker.tt = Arc::clone(&self.tt);
        }
        self.thread_pool.update_tt(Arc::clone(&self.tt));
    }

    /// Large Pagesで確保されているかを返す
    pub fn tt_uses_large_pages(&self) -> bool {
        self.tt.uses_large_pages()
    }

    /// EvalHashのサイズを変更
    ///
    /// # 注意
    /// このメソッドは**探索停止中にのみ**呼び出すこと。
    /// `&mut self` を取るため、探索中（`go` 実行中）には呼び出せない。
    /// USIプロトコルでは `setoption` は探索中に送られないため、
    /// 通常の使用では問題ない。
    pub fn resize_eval_hash(&mut self, size_mb: usize) {
        self.eval_hash = Arc::new(EvalHash::new(size_mb));
        self.eval_hash_size_mb = size_mb;
        // workerが存在する場合、EvalHash参照を更新
        if let Some(worker) = &mut self.worker {
            worker.eval_hash = Arc::clone(&self.eval_hash);
        }
        self.thread_pool.update_eval_hash(Arc::clone(&self.eval_hash));
    }

    /// EvalHashへの参照を取得
    pub fn eval_hash(&self) -> Arc<EvalHash> {
        Arc::clone(&self.eval_hash)
    }

    /// EvalHashの現在サイズ（MB）を返す。
    pub fn eval_hash_size_mb(&self) -> usize {
        self.eval_hash_size_mb
    }

    /// 履歴統計をクリア（usinewgame時に呼び出し）
    ///
    /// Worker::clear()相当
    pub fn clear_histories(&mut self) {
        if let Some(worker) = &mut self.worker {
            worker.clear();
        }
        self.thread_pool.clear_histories();
    }

    /// 停止フラグを取得（探索スレッドに渡す用）
    pub fn stop_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.stop)
    }

    /// ponderhitフラグを取得（探索スレッドへの通知に使用）
    pub fn ponderhit_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.ponderhit_flag)
    }

    /// ponderhitを要求（外部スレッドから）
    pub fn request_ponderhit(&self) {
        self.ponderhit_flag.store(true, Ordering::SeqCst);
    }

    /// 探索を停止
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    /// stop/ponderhitフラグをリセット（go() 呼び出し前にUSI層から呼ぶ）
    ///
    /// go() 内部ではなくスレッド生成前に呼ぶことで、USI層で既にセットされた
    /// フラグが競合で失われるのを防ぐ。
    pub fn reset_flags(&self) {
        self.stop.store(false, Ordering::SeqCst);
        self.ponderhit_flag.store(false, Ordering::SeqCst);
    }

    /// 時間オプションを設定（USI setoptionから呼び出す想定）
    pub fn set_time_options(&mut self, opts: super::TimeOptions) {
        self.time_options = opts;
    }

    /// 時間オプションを取得
    pub fn time_options(&self) -> super::TimeOptions {
        self.time_options
    }

    /// Skillオプションを設定（USI setoptionから呼び出す想定）
    pub fn set_skill_options(&mut self, opts: SkillOptions) {
        self.skill_options = opts;
    }

    /// Skillオプションを取得
    pub fn skill_options(&self) -> SkillOptions {
        self.skill_options
    }

    /// 引き分けまでの最大手数を設定
    pub fn set_max_moves_to_draw(&mut self, v: i32) {
        self.max_moves_to_draw = if v > 0 { v } else { DEFAULT_MAX_MOVES_TO_DRAW };
    }

    /// 引き分けまでの最大手数を取得
    pub fn max_moves_to_draw(&self) -> i32 {
        self.max_moves_to_draw
    }

    /// YaneuraOuオプション `DrawValueBlack` を設定する。
    ///
    /// 有効範囲は `[-30000, 30000]`。
    pub fn set_draw_value_black(&mut self, v: i32) {
        self.draw_value_black = v.clamp(-30000, 30000);
        if let Some(worker) = &mut self.worker {
            worker.draw_value_black = self.draw_value_black;
        }
    }

    /// 現在の `DrawValueBlack` を取得する。
    pub fn draw_value_black(&self) -> i32 {
        self.draw_value_black
    }

    /// YaneuraOuオプション `DrawValueWhite` を設定する。
    ///
    /// 有効範囲は `[-30000, 30000]`。
    pub fn set_draw_value_white(&mut self, v: i32) {
        self.draw_value_white = v.clamp(-30000, 30000);
        if let Some(worker) = &mut self.worker {
            worker.draw_value_white = self.draw_value_white;
        }
    }

    /// 現在の `DrawValueWhite` を取得する。
    pub fn draw_value_white(&self) -> i32 {
        self.draw_value_white
    }

    /// 入玉宣言勝ちルールを設定する。
    pub fn set_entering_king_rule(&mut self, rule: EnteringKingRule) {
        self.entering_king_rule = rule;
    }

    /// 現在の入玉宣言勝ちルールを取得する。
    pub fn entering_king_rule(&self) -> EnteringKingRule {
        self.entering_king_rule
    }

    /// 探索スレッド数を設定
    pub fn set_num_threads(&mut self, num: usize) {
        // WASM builds without wasm-threads feature use single-threaded search only.
        // With wasm-threads feature, multi-threading via wasm-bindgen-rayon is supported.
        #[cfg(all(target_arch = "wasm32", not(feature = "wasm-threads")))]
        let _ = num; // シングルスレッドモードでは引数を無視
        #[cfg(all(target_arch = "wasm32", not(feature = "wasm-threads")))]
        let num = 1;
        #[cfg(not(all(target_arch = "wasm32", not(feature = "wasm-threads"))))]
        let num = num.clamp(1, 512);
        self.num_threads = num;
        self.thread_pool.set_num_threads(
            num,
            Arc::clone(&self.tt),
            Arc::clone(&self.eval_hash),
            self.max_moves_to_draw,
            self.search_tune_params,
        );
    }

    /// 探索スレッド数を取得
    pub fn num_threads(&self) -> usize {
        self.num_threads
    }

    /// 探索チューニングパラメータを取得
    pub fn search_tune_params(&self) -> SearchTuneParams {
        self.search_tune_params
    }

    /// 探索チューニングパラメータを一括設定
    pub fn set_search_tune_params(&mut self, params: SearchTuneParams) {
        self.search_tune_params = params;
        if let Some(worker) = &mut self.worker {
            worker.search_tune_params = params;
        }
        self.thread_pool.update_search_tune_params(params);
    }

    /// 探索チューニング項目を1つ更新（USI option名ベース）
    pub fn set_search_tune_option(
        &mut self,
        name: &str,
        value: i32,
    ) -> Option<super::SearchTuneSetResult> {
        let mut params = self.search_tune_params;
        let result = params.set_from_usi_name(name, value)?;
        self.set_search_tune_params(params);
        Some(result)
    }

    /// 探索を実行
    ///
    /// # Arguments
    /// * `pos` - 探索対象の局面
    /// * `limits` - 探索制限
    /// * `on_info` - 探索情報のコールバック（Optional）
    ///
    /// # Returns
    /// 探索結果
    pub fn go<F>(
        &mut self,
        pos: &mut Position,
        limits: LimitsType,
        on_info: Option<F>,
    ) -> SearchResult
    where
        F: FnMut(&SearchInfo),
    {
        let ply = pos.game_ply();
        self.prepare_time_metrics(ply);
        // 注意: stop/ponderhitフラグのリセットは go() の呼び出し元
        // (USI層の cmd_go) でスレッド生成前に行うこと。
        // ここでリセットすると、USI層で既にセットされたフラグが失われる競合が発生する。
        self.start_time = Some(Instant::now());
        // 置換表の世代を進める
        self.tt.new_search();
        // ヘルパースレッドの結果をクリア
        // スレッド数が1の場合でも呼び出し、前回のマルチスレッド探索の結果が残らないようにする
        self.thread_pool.clear_helper_results();

        // 時間管理
        let mut time_manager =
            TimeManagement::new(Arc::clone(&self.stop), Arc::clone(&self.ponderhit_flag));
        time_manager.set_options(&self.time_options);
        time_manager.set_previous_time_reduction(self.previous_time_reduction);
        // ply（現在の手数）は局面から取得、max_moves_to_drawはデフォルトを使う
        time_manager.init(&limits, pos.side_to_move(), ply, self.max_moves_to_draw);

        // workerは遅延初期化、再利用する
        let tt_clone = Arc::clone(&self.tt);
        let eval_hash_clone = Arc::clone(&self.eval_hash);
        let max_moves = self.max_moves_to_draw;
        let search_tune_params = self.search_tune_params;
        let draw_value_black = self.draw_value_black;
        let draw_value_white = self.draw_value_white;
        let worker = self.worker.get_or_insert_with(|| {
            SearchWorker::new(tt_clone, eval_hash_clone, max_moves, 0, search_tune_params)
        });

        // setoptionで変更された可能性があるため、最新値を反映
        worker.max_moves_to_draw = self.max_moves_to_draw;
        worker.search_tune_params = self.search_tune_params;
        worker.draw_value_black = self.draw_value_black;
        worker.draw_value_white = self.draw_value_white;
        worker.entering_king_rule = self.entering_king_rule;

        // 探索状態のリセット（履歴はクリアしない）
        worker.prepare_search();
        worker.allow_tt_write = true;

        // 探索深さを決定
        let max_depth = if limits.depth > 0 {
            limits.depth
        } else {
            MAX_PLY // 可能な限り深く探索
        };

        // SkillLevel設定を構築（手加減）
        let mut skill = Skill::from_options(&self.skill_options);
        let skill_enabled = skill.enabled();

        // デバッグ用の helper 有効化制御
        // go depth/go mate を含め helper を有効化する。
        // 追加の切り分けは環境変数 RSHOGI_DISABLE_HELPER_SEARCH で行う。
        let helper_search_enabled = self.num_threads > 1 && !helper_search_disabled();

        if helper_search_enabled {
            self.thread_pool.start_thinking(
                pos,
                limits.clone(),
                max_depth,
                self.time_options,
                self.max_moves_to_draw,
                draw_value_black,
                draw_value_white,
                self.entering_king_rule,
                skill_enabled,
            );
        }

        // 探索実行（コールバックなしの場合はダミーを渡す）
        let _effective_multi_pv = match on_info {
            Some(callback) => self.search_with_callback(
                pos,
                &limits,
                &mut time_manager,
                max_depth,
                callback,
                skill_enabled,
            ),
            None => {
                let mut noop = |_info: &SearchInfo| {};
                self.search_with_callback(
                    pos,
                    &limits,
                    &mut time_manager,
                    max_depth,
                    &mut noop,
                    skill_enabled,
                )
            }
        };

        if helper_search_enabled {
            self.stop.store(true, Ordering::SeqCst);
            self.thread_pool.wait_for_search_finished();
        }

        let use_best_thread =
            self.num_threads > 1 && should_use_best_thread_selection(&limits, skill_enabled);
        let debug_best_thread = best_thread_debug_enabled();

        let best_thread_id = {
            let worker = self
                .worker
                .as_ref()
                .expect("worker should be initialized by search_with_callback");
            get_best_thread_id(worker, &self.thread_pool, use_best_thread, debug_best_thread)
        };

        let best_result = if best_thread_id == 0 {
            let worker = self
                .worker
                .as_ref()
                .expect("worker should be initialized by search_with_callback");
            collect_best_thread_result(worker, &limits, skill_enabled, &mut skill)
        } else {
            // Native: Use helper_threads() to access Thread objects directly
            #[cfg(not(target_arch = "wasm32"))]
            let result = {
                let mut result = None;
                for thread in self.thread_pool.helper_threads() {
                    if thread.id() == best_thread_id {
                        result = Some(thread.with_worker(|worker: &mut SearchWorker| {
                            collect_best_thread_result(worker, &limits, skill_enabled, &mut skill)
                        }));
                        break;
                    }
                }
                result
            };

            // Wasm with wasm-threads: Use helper_results() to get collected results
            #[cfg(all(target_arch = "wasm32", feature = "wasm-threads"))]
            let result = {
                let helper_results = self.thread_pool.helper_results();
                helper_results.iter().find(|r| r.thread_id == best_thread_id).map(|r| {
                    // Apply skill-based move weakening if enabled
                    let (best_move, score) = if skill_enabled && !r.top_moves.is_empty() {
                        let mut rng = rand::rng();
                        let picked = skill.pick_best_from_pairs(&r.top_moves, &mut rng);
                        if picked != Move::NONE {
                            // Find the score of the picked move from top_moves
                            let picked_score = r
                                .top_moves
                                .iter()
                                .find(|(mv, _)| *mv == picked)
                                .map(|(_, score)| *score)
                                .unwrap_or(r.best_score);
                            (picked, picked_score)
                        } else {
                            (r.best_move, r.best_score)
                        }
                    } else {
                        (r.best_move, r.best_score)
                    };
                    BestThreadResult {
                        best_move,
                        ponder_move: Move::NONE, // Cannot get ponder from helper in Wasm
                        score,
                        completed_depth: r.completed_depth,
                        nodes: r.nodes,
                        // Use the actual best score (not skill-weakened) for time management
                        // and aspiration window initialization, matching native behavior.
                        best_previous_score: Some(r.best_score),
                        best_previous_average_score: Some(r.best_score),
                        pv: Vec::new(), // Cannot get PV from helper in Wasm
                    }
                })
            };

            // Wasm without wasm-threads: Always use main thread
            #[cfg(all(target_arch = "wasm32", not(feature = "wasm-threads")))]
            let result: Option<BestThreadResult> = None;

            result.unwrap_or_else(|| {
                let worker = self
                    .worker
                    .as_ref()
                    .expect("worker should be initialized by search_with_callback");
                collect_best_thread_result(worker, &limits, skill_enabled, &mut skill)
            })
        };

        let BestThreadResult {
            best_move,
            ponder_move,
            score,
            completed_depth,
            nodes: _best_nodes,
            best_previous_score,
            best_previous_average_score,
            pv,
        } = best_result;
        let total_nodes = {
            let main_nodes = self.worker.as_ref().map(|w| w.state.nodes).unwrap_or(0);

            // Native: Use helper_threads() to get node counts
            #[cfg(not(target_arch = "wasm32"))]
            let helper_nodes =
                self.thread_pool.helper_threads().iter().fold(0u64, |acc, thread| {
                    acc.saturating_add(thread.with_worker(|worker| worker.state.nodes))
                });

            // Wasm with wasm-threads: Use helper_nodes() to get node counts
            #[cfg(all(target_arch = "wasm32", feature = "wasm-threads"))]
            let helper_nodes = self.thread_pool.helper_nodes();

            // Wasm without wasm-threads: No helper threads
            #[cfg(all(target_arch = "wasm32", not(feature = "wasm-threads")))]
            let helper_nodes = 0u64;

            main_nodes.saturating_add(helper_nodes)
        };

        // 次の手番のために timeReduction を持ち回る
        self.previous_time_reduction = time_manager.previous_time_reduction();

        // 次回のfallingEval計算のために平均スコアを保存
        self.best_previous_score = best_previous_score;
        self.best_previous_average_score = best_previous_average_score;
        self.last_game_ply = Some(ply);

        // 探索統計レポートを取得（search-stats feature有効時のみ内容あり）
        let stats_report = self.worker.as_ref().map(|w| w.get_stats_report()).unwrap_or_default();

        SearchResult {
            best_move,
            ponder_move,
            score,
            depth: completed_depth,
            nodes: total_nodes,
            pv,
            stats_report,
        }
    }

    /// コールバック付きで探索を実行
    fn search_with_callback<F>(
        &mut self,
        pos: &mut Position,
        limits: &LimitsType,
        time_manager: &mut TimeManagement,
        max_depth: Depth,
        mut on_info: F,
        skill_enabled: bool,
    ) -> usize
    where
        F: FnMut(&SearchInfo),
    {
        // 深さペーシングの状態を初期化
        self.increase_depth = true;
        self.increase_depth_shared.store(true, Ordering::Relaxed);
        self.search_again_counter = 0;

        // workerを一時的に取り出す（借用チェッカー対策）
        let mut worker = self.worker.take().expect("worker should be available");

        // MainThreadState を構築
        let mut main_state = MainThreadState {
            ponderhit_flag: &self.ponderhit_flag,
            start_time: self.start_time.unwrap(),
            tt: &self.tt,
            thread_pool: &self.thread_pool,
            increase_depth: self.increase_depth,
            search_again_counter: self.search_again_counter,
            best_previous_average_score: self.best_previous_average_score,
            iter_value: self.iter_value,
            iter_idx: self.iter_idx,
            last_best_move: self.last_best_move,
            last_best_move_depth: self.last_best_move_depth,
            tot_best_move_changes: self.tot_best_move_changes,
            increase_depth_shared: &self.increase_depth_shared,
        };

        let mut noop_progress = |_nodes: u64, _bmc: f64| {};
        let result = iterative_deepening(
            &mut worker,
            pos,
            limits,
            time_manager,
            max_depth,
            skill_enabled,
            &self.increase_depth_shared,
            Some(&mut main_state),
            &mut on_info,
            &mut noop_progress,
        );

        // MainThreadState から書き戻し
        self.increase_depth = main_state.increase_depth;
        self.search_again_counter = main_state.search_again_counter;
        self.iter_value = main_state.iter_value;
        self.iter_idx = main_state.iter_idx;
        self.last_best_move = main_state.last_best_move;
        self.last_best_move_depth = main_state.last_best_move_depth;
        self.tot_best_move_changes = main_state.tot_best_move_changes;

        // workerを戻す
        self.worker = Some(worker);

        result
    }
}

/// メインスレッド固有の可変状態（YO の SearchManager に対応）
///
/// `search_with_callback` 呼び出し前に `Search` のフィールドから構築し、
/// 戻った後に書き戻す。
struct MainThreadState<'a> {
    ponderhit_flag: &'a AtomicBool,
    start_time: Instant,
    tt: &'a TranspositionTable,
    thread_pool: &'a ThreadPool,
    // owned (書き戻し対象)
    increase_depth: bool,
    search_again_counter: i32,
    best_previous_average_score: Option<Value>,
    iter_value: [Value; 4],
    iter_idx: usize,
    last_best_move: Move,
    last_best_move_depth: Depth,
    tot_best_move_changes: f64,
    increase_depth_shared: &'a AtomicBool,
}

impl MainThreadState<'_> {
    fn compute_time_factors(
        &self,
        best_value: Value,
        completed_depth: Depth,
        tot_best_move_changes: f64,
        thread_count: usize,
    ) -> (f64, f64, f64, usize) {
        let prev_avg_raw = self.best_previous_average_score.unwrap_or(Value::INFINITE).raw();
        let iter_val = self.iter_value[self.iter_idx];
        let falling_eval = calculate_falling_eval(prev_avg_raw, iter_val.raw(), best_value.raw());
        let time_reduction = calculate_time_reduction(completed_depth, self.last_best_move_depth);
        (falling_eval, time_reduction, tot_best_move_changes, thread_count)
    }

    fn update_time_factor_state(&mut self, best_value: Value, tot_best_move_changes: f64) {
        self.iter_value[self.iter_idx] = best_value;
        self.iter_idx = (self.iter_idx + 1) % self.iter_value.len();
        self.tot_best_move_changes = tot_best_move_changes;
    }
}

/// YaneuraOu の iterative_deepening() に対応する統合反復深化ループ。
///
/// メインスレッドでは `main_state = Some(...)` で呼び出し、
/// ヘルパースレッドでは `main_state = None` で呼び出す。
/// YO の `if (mainThread)` パターンを `if let Some(ref mut ms) = main_state` で表現。
fn iterative_deepening<FInfo, FProgress>(
    worker: &mut SearchWorker,
    pos: &mut Position,
    limits: &LimitsType,
    time_manager: &mut TimeManagement,
    max_depth: Depth,
    skill_enabled: bool,
    increase_depth_shared: &AtomicBool,
    mut main_state: Option<&mut MainThreadState>,
    on_info: &mut FInfo,
    on_progress: &mut FProgress,
) -> usize
where
    FInfo: FnMut(&SearchInfo),
    FProgress: FnMut(u64, f64),
{
    let is_main = main_state.is_some();

    // ルート手を初期化
    worker.state.root_moves = super::RootMoves::from_legal_moves(pos, &limits.search_moves);

    // 入玉宣言勝ちチェック（YO準拠: root のみ）
    let decl_move = pos.declaration_win(worker.entering_king_rule);
    if decl_move != Move::NONE {
        // 宣言勝ち可能: root_moves に追加してスコア MATE を設定
        // searchmoves に関係なく追加する（YO準拠）
        if worker.state.root_moves.find(decl_move).is_none() {
            worker.state.root_moves.push(super::RootMove::new(decl_move));
        }
        if let Some(idx) = worker.state.root_moves.find(decl_move) {
            worker.state.root_moves[idx].score = Value::MATE;
            worker.state.root_moves.move_to_front(idx);
        }
        worker.state.best_move = decl_move;
        worker.state.completed_depth = 1;

        if is_main {
            eprintln!("info string declaration win: {}", decl_move.to_usi());
        }

        // ponder/infinite 待機: bestmove を早出ししない（USI仕様準拠）
        if let Some(ref ms) = main_state {
            while !worker.state.abort
                && !time_manager.stop_requested()
                && (time_manager.is_pondering() || limits.infinite)
            {
                if ms.ponderhit_flag.swap(false, Ordering::Relaxed) {
                    time_manager.on_ponderhit();
                }
                thread::sleep(Duration::from_millis(1));
            }
        }
        return 0;
    }

    if worker.state.root_moves.is_empty() {
        worker.state.best_move = Move::NONE;
        return 0;
    }

    // 合法手が1つの場合は500ms上限を適用
    if worker.state.root_moves.len() == 1 {
        time_manager.apply_single_move_limit();
    }

    let mut effective_multi_pv = limits.multi_pv;
    if skill_enabled {
        effective_multi_pv = effective_multi_pv.max(4);
    }
    effective_multi_pv = effective_multi_pv.min(worker.state.root_moves.len());

    // 中断時にPVを巻き戻すための保持
    let mut last_best_pv = vec![Move::NONE];
    let mut last_best_score = Value::new(-Value::INFINITE.raw());
    let mut last_best_move_depth = 0;

    // ヘルパー用のローカル search_again_counter
    let mut local_search_again_counter: i32 = 0;

    // 反復深化ループ開始前に best_move を初期化
    // nodes 制限等で depth 1 完了前に abort された場合でも有効な手を返すため
    if !worker.state.root_moves.is_empty() {
        worker.state.best_move = worker.state.root_moves[0].mv();
    }

    // 反復深化ループ
    for depth in 1..=max_depth {
        if worker.state.abort {
            break;
        }

        // search_again_counter 更新
        let inc_depth = if let Some(ref ms) = main_state {
            ms.increase_depth
        } else {
            increase_depth_shared.load(Ordering::Relaxed)
        };
        if !inc_depth {
            if let Some(ref mut ms) = main_state {
                ms.search_again_counter += 1;
            } else {
                local_search_again_counter += 1;
            }
        }

        // メインのみ: ponderhit検出、should_stop チェック
        if let Some(ref ms) = main_state {
            if ms.ponderhit_flag.swap(false, Ordering::Relaxed) {
                time_manager.on_ponderhit();
            }
            let is_pondering = time_manager.is_pondering();
            if depth > 1 && !is_pondering && time_manager.should_stop(depth) {
                break;
            }
        }

        // 詰みを読みきった場合の早期終了
        // ponder中は stop/ponderhit を待つ必要があるため早期終了しない（USI仕様準拠）
        let is_pondering_now = main_state.as_ref().is_some_and(|_| time_manager.is_pondering());
        if effective_multi_pv == 1
            && depth > 1
            && !is_pondering_now
            && !worker.state.root_moves.is_empty()
        {
            let best_value = worker.state.root_moves[0].score;

            if limits.mate == 0 {
                if proven_mate_depth_exceeded(best_value, depth) {
                    break;
                }
            } else if mate_within_limit(
                best_value,
                worker.state.root_moves[0].score_lower_bound,
                worker.state.root_moves[0].score_upper_bound,
                limits.mate,
            ) {
                // メインのみ request_stop
                if is_main {
                    time_manager.request_stop();
                }
                break;
            }
        }

        // メインのみ: if (!mainThread) continue; に対応する部分はループ末尾で処理

        // mainHistory のイテレーション間 decay（Stockfish 由来）
        // 古い history 値の蓄積を防ぎ move ordering の鮮度を保つ
        // SAFETY: 単一スレッド内で使用、探索ループ外なので排他保証
        unsafe { worker.history.as_mut_unchecked() }.main_history.decay(820, 1024);

        let search_depth = depth;
        worker.state.root_depth = search_depth;
        worker.state.sel_depth = 0;

        let search_again_counter = if let Some(ref ms) = main_state {
            ms.search_again_counter
        } else {
            local_search_again_counter
        };

        // MultiPVループ
        let mut processed_pv = 0;
        for pv_idx in 0..effective_multi_pv {
            if worker.state.abort {
                break;
            }

            // Aspiration Window（average/mean_squaredベース）
            let (mut alpha, mut beta, mut delta) = compute_aspiration_window(
                &worker.state.root_moves[pv_idx],
                worker.thread_id,
                &worker.search_tune_params,
            );
            let mut failed_high_cnt = 0;

            // Aspiration Windowループ
            loop {
                let adjusted_depth =
                    (search_depth - failed_high_cnt - (3 * (search_again_counter + 1) / 4)).max(1);

                let score = if pv_idx == 0 {
                    worker.search_root(pos, adjusted_depth, alpha, beta, limits, time_manager)
                } else {
                    worker.search_root_for_pv(
                        pos,
                        search_depth,
                        alpha,
                        beta,
                        pv_idx,
                        limits,
                        time_manager,
                    )
                };

                // aspiration loop 内ソート
                worker.state.root_moves.stable_sort_range(pv_idx, worker.state.root_moves.len());

                // nodes 制限等で探索が中断された場合の停止判定
                // abort フラグに加え、nodes 制限超過も直接チェックする
                // （check_abort は頻度制御で呼び出されるため、abort フラグが
                //   立っていないまま search_root が返ることがある）
                if worker.state.abort
                    || (limits.nodes > 0 && worker.state.nodes >= limits.nodes)
                    || time_manager.stop_requested()
                {
                    worker.state.abort = true;
                    break;
                }

                // Window調整
                if score <= alpha {
                    beta = alpha;
                    alpha = Value::new(
                        score.raw().saturating_sub(delta.raw()).max(-Value::INFINITE.raw()),
                    );
                    failed_high_cnt = 0;
                    // メインのみ
                    if is_main {
                        time_manager.reset_stop_on_ponderhit();
                    }
                } else if score >= beta {
                    alpha = Value::new((beta.raw() - delta.raw()).max(alpha.raw()));
                    beta = Value::new(
                        score.raw().saturating_add(delta.raw()).min(Value::INFINITE.raw()),
                    );
                    failed_high_cnt += 1;
                } else {
                    break;
                }

                // delta 更新
                delta = Value::new(
                    delta.raw().saturating_add(delta.raw() / 3).min(Value::INFINITE.raw()),
                );
            }

            // 安定ソート [pv_idx..]
            worker.state.root_moves.stable_sort_range(pv_idx, worker.state.root_moves.len());
            // 📝 YaneuraOu行1539: 探索済みのPVライン全体も安定ソートして順位を保つ
            worker.state.root_moves.stable_sort_range(0, pv_idx + 1);
            processed_pv = pv_idx + 1;
        }

        // MultiPVループ完了後の最終ソート（YaneuraOu行1499）
        if !worker.state.abort && effective_multi_pv > 1 {
            worker.state.root_moves.stable_sort_range(0, effective_multi_pv);
        }

        // メインのみ: info出力（GUI詰まり防止のYO仕様）
        if let Some(ref ms) = main_state
            && processed_pv > 0
        {
            let elapsed = ms.start_time.elapsed();
            let time_ms = elapsed.as_millis() as u64;

            // Native: Use helper_threads() to get node counts
            #[cfg(not(target_arch = "wasm32"))]
            let helper_nodes = ms
                .thread_pool
                .helper_threads()
                .iter()
                .fold(0u64, |acc, thread| acc.saturating_add(thread.nodes()));

            // Wasm with wasm-threads: Use helper_nodes() for realtime node counts
            #[cfg(all(target_arch = "wasm32", feature = "wasm-threads"))]
            let helper_nodes = ms.thread_pool.helper_nodes();

            // Wasm without wasm-threads: No helper threads
            #[cfg(all(target_arch = "wasm32", not(feature = "wasm-threads")))]
            let helper_nodes = 0u64;

            let total_nodes = worker.state.nodes.saturating_add(helper_nodes);
            let nps = if time_ms > 0 {
                total_nodes.saturating_mul(1000) / time_ms
            } else {
                0
            };

            for pv_idx in 0..processed_pv {
                let info = SearchInfo {
                    depth,
                    sel_depth: worker.state.root_moves[pv_idx].sel_depth,
                    score: worker.state.root_moves[pv_idx].score,
                    nodes: total_nodes,
                    time_ms,
                    nps,
                    hashfull: ms.tt.hashfull(3) as u32,
                    pv: worker.state.root_moves[pv_idx].pv.clone(),
                    multi_pv: pv_idx + 1, // 1-indexed
                };
                on_info(&info);
            }
        }

        // Depth完了後の処理
        if !worker.state.abort {
            worker.state.completed_depth = search_depth;
            worker.state.best_move = worker.state.root_moves[0].mv();

            // previous_scoreを次のiterationのためにシード
            // （YaneuraOu行1304-1305: rm.previousScore = rm.score）
            for rm in worker.state.root_moves.iter_mut() {
                rm.previous_score = rm.score;
            }

            let best_move_changes = worker.state.best_move_changes;
            worker.state.best_move_changes = 0.0;

            if let Some(ref mut ms) = main_state {
                // メインのみ: last_best_move 更新
                if worker.state.best_move != ms.last_best_move {
                    ms.last_best_move = worker.state.best_move;
                    ms.last_best_move_depth = depth;
                }

                // 評価変動・timeReduction・最善手不安定性をまとめて適用
                let best_value = if worker.state.root_moves.is_empty() {
                    Value::ZERO
                } else {
                    worker.state.root_moves[0].score
                };
                let completed_depth = worker.state.completed_depth;
                let effort = if worker.state.root_moves.is_empty() {
                    0.0
                } else {
                    worker.state.root_moves[0].effort
                };
                let nodes = worker.state.nodes;
                let root_moves_len = worker.state.root_moves.len();

                // Native: Use helper_threads() to collect best_move_changes
                #[cfg(not(target_arch = "wasm32"))]
                let (changes_sum, thread_count) = {
                    let helper_threads = ms.thread_pool.helper_threads();
                    let mut changes = Vec::with_capacity(helper_threads.len() + 1);
                    changes.push(best_move_changes);
                    for thread in helper_threads {
                        changes.push(thread.best_move_changes());
                    }
                    aggregate_best_move_changes(&changes)
                };

                // Wasm with wasm-threads: Use helper_best_move_changes() for realtime values
                #[cfg(all(target_arch = "wasm32", feature = "wasm-threads"))]
                let (changes_sum, thread_count) = {
                    let helper_changes = ms.thread_pool.helper_best_move_changes();
                    let mut changes = Vec::with_capacity(helper_changes.len() + 1);
                    changes.push(best_move_changes);
                    changes.extend(helper_changes);
                    aggregate_best_move_changes(&changes)
                };

                // Wasm without wasm-threads: Only main thread
                #[cfg(all(target_arch = "wasm32", not(feature = "wasm-threads")))]
                let (changes_sum, thread_count) = (best_move_changes, 1);

                // totBestMoveChanges /= 2
                let tot_best_move_changes = ms.tot_best_move_changes / 2.0 + changes_sum;

                if limits.use_time_management()
                    && !time_manager.stop_on_ponderhit()
                    && time_manager.search_end() == 0
                {
                    let (falling_eval, time_reduction, tot_changes, threads) = ms
                        .compute_time_factors(
                            best_value,
                            completed_depth,
                            tot_best_move_changes,
                            thread_count,
                        );
                    let total_time = time_manager.total_time_for_iteration(
                        falling_eval,
                        time_reduction,
                        tot_changes,
                        threads,
                    );

                    let nodes_effort = normalize_nodes_effort(effort, nodes);

                    let total_time = if root_moves_len == 1 {
                        total_time.min(500.0)
                    } else {
                        total_time
                    };
                    let elapsed_time = time_manager.elapsed_from_ponderhit() as f64;
                    time_manager.apply_iteration_timing(
                        time_manager.elapsed(),
                        total_time,
                        nodes_effort,
                        completed_depth,
                    );

                    // 次iterationで深さを伸ばすかの判定
                    ms.increase_depth =
                        time_manager.is_pondering() || elapsed_time <= total_time * 0.503;
                    ms.increase_depth_shared.store(ms.increase_depth, Ordering::Relaxed);

                    ms.update_time_factor_state(best_value, tot_best_move_changes);
                }
                ms.tot_best_move_changes = tot_best_move_changes;
            } else {
                // ヘルパー: progress コールバック
                on_progress(worker.state.nodes, best_move_changes);
            }

            // PVが変わったときのみ last_best_* を更新
            if !worker.state.root_moves[0].pv.is_empty()
                && worker.state.root_moves[0].pv[0] != last_best_pv[0]
            {
                last_best_pv = worker.state.root_moves[0].pv.clone();
                last_best_score = worker.state.root_moves[0].score;
                last_best_move_depth = depth;
            }

            // 詰みスコアが見つかっていたら早期終了
            // ponder中は stop/ponderhit を待つ必要があるためスキップ（USI仕様準拠）
            if effective_multi_pv == 1
                && depth > 1
                && !is_pondering_now
                && !worker.state.root_moves.is_empty()
            {
                let best_value = worker.state.root_moves[0].score;

                if limits.mate == 0 {
                    if proven_mate_depth_exceeded(best_value, depth) {
                        break;
                    }
                } else if mate_within_limit(
                    best_value,
                    worker.state.root_moves[0].score_lower_bound,
                    worker.state.root_moves[0].score_upper_bound,
                    limits.mate,
                ) {
                    if is_main {
                        time_manager.request_stop();
                    }
                    break;
                }
            }
        }
    }

    // ponder中 / go infinite中はGUIからstop/ponderhitが来るまでbestmoveを出力してはならない（YaneuraOu準拠）
    // 反復深化ループが自然に終了した場合（MAX_PLY到達や詰み確定）でもここで待機する
    if let Some(ref ms) = main_state {
        while !worker.state.abort
            && !time_manager.stop_requested()
            && (time_manager.is_pondering() || limits.infinite)
        {
            if ms.ponderhit_flag.swap(false, Ordering::Relaxed) {
                time_manager.on_ponderhit();
            }
            // YaneuraOu 同様、探索終了後の待機では短時間 sleep して busy wait を避ける。
            thread::sleep(Duration::from_millis(1));
        }
    }

    // 中断した探索で信頼できないPVになった場合のフォールバック
    if worker.state.abort
        && !worker.state.root_moves.is_empty()
        && worker.state.root_moves[0].score.is_loss()
    {
        let head = last_best_pv.first().copied().unwrap_or(Move::NONE);
        if head != Move::NONE
            && let Some(idx) = worker.state.root_moves.find(head)
        {
            worker.state.root_moves.move_to_front(idx);
            worker.state.root_moves[0].pv = last_best_pv;
            worker.state.root_moves[0].score = last_best_score;
            worker.state.completed_depth = last_best_move_depth;
        }
    }

    effective_multi_pv
}

// search_helper_impl is a thin wrapper that calls iterative_deepening with main_state=None.
// Only compiled for Native and Wasm with wasm-threads (single-threaded Wasm doesn't use helper threads).
#[cfg(any(not(target_arch = "wasm32"), feature = "wasm-threads"))]
fn search_helper_impl<F1, F2>(
    worker: &mut SearchWorker,
    pos: &mut Position,
    limits: &LimitsType,
    time_manager: &mut TimeManagement,
    max_depth: Depth,
    skill_enabled: bool,
    increase_depth_shared: &AtomicBool,
    on_start: F1,
    mut on_depth_complete: F2,
) -> usize
where
    F1: FnOnce(),
    F2: FnMut(u64, f64),
{
    // 恒久修正評価のため、go depth/go mate を含め helper からのTT書き込みを有効にする。
    worker.allow_tt_write = true;

    on_start();

    let mut noop_info = |_info: &SearchInfo| {};
    iterative_deepening(
        worker,
        pos,
        limits,
        time_manager,
        max_depth,
        skill_enabled,
        increase_depth_shared,
        None,
        &mut noop_info,
        &mut on_depth_complete,
    )
}

// Native version: takes progress parameter for tracking helper thread statistics.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn search_helper(
    worker: &mut SearchWorker,
    pos: &mut Position,
    limits: &LimitsType,
    time_manager: &mut TimeManagement,
    max_depth: Depth,
    skill_enabled: bool,
    progress: Option<&SearchProgress>,
    increase_depth_shared: &AtomicBool,
) -> usize {
    search_helper_impl(
        worker,
        pos,
        limits,
        time_manager,
        max_depth,
        skill_enabled,
        increase_depth_shared,
        || {
            if let Some(p) = progress {
                p.reset();
            }
        },
        |nodes, bmc| {
            if let Some(p) = progress {
                p.update(nodes, bmc);
            }
        },
    )
}

// Wasm with wasm-threads: takes progress parameter for tracking helper thread statistics.
#[cfg(all(target_arch = "wasm32", feature = "wasm-threads"))]
pub(crate) fn search_helper(
    worker: &mut SearchWorker,
    pos: &mut Position,
    limits: &LimitsType,
    time_manager: &mut TimeManagement,
    max_depth: Depth,
    skill_enabled: bool,
    progress: Option<&super::thread::HelperProgress>,
    increase_depth_shared: &AtomicBool,
) -> usize {
    search_helper_impl(
        worker,
        pos,
        limits,
        time_manager,
        max_depth,
        skill_enabled,
        increase_depth_shared,
        || {
            if let Some(p) = progress {
                p.reset();
            }
        },
        |nodes, bmc| {
            if let Some(p) = progress {
                p.update(nodes, bmc);
            }
        },
    )
}

// Wasm without wasm-threads: search_helper is not needed because there are no helper threads.
// Single-threaded Wasm only uses the main thread search in search_with_callback().

// =============================================================================
// テスト
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// SearchWorkerは大きなスタック領域を使うため、テストは別スレッドで実行
    const STACK_SIZE: usize = 64 * 1024 * 1024; // 64MB

    #[test]
    fn test_aggregate_best_move_changes_empty() {
        let (sum, threads) = aggregate_best_move_changes(&[]);
        assert_eq!(sum, 0.0);
        assert_eq!(threads, 1);
    }

    #[test]
    fn test_aggregate_best_move_changes_multi() {
        let (sum, threads) = aggregate_best_move_changes(&[1.0, 2.0, 3.0]);
        assert!((sum - 6.0).abs() < 1e-9, "sum should be 6.0, got {sum}");
        assert_eq!(threads, 3);
    }

    #[test]
    fn test_should_use_best_thread_selection_yaneuraou_conditions() {
        let mut limits = LimitsType::default();
        assert!(should_use_best_thread_selection(&limits, false));

        limits.depth = 8;
        assert!(
            !should_use_best_thread_selection(&limits, false),
            "go depth ではbest thread選抜を使わない"
        );

        limits.depth = 0;
        limits.mate = 3;
        assert!(
            !should_use_best_thread_selection(&limits, false),
            "go mate ではbest thread選抜を使わない"
        );

        limits.mate = 0;
        limits.multi_pv = 2;
        assert!(
            !should_use_best_thread_selection(&limits, false),
            "MultiPV>1 ではbest thread選抜を使わない"
        );

        limits.multi_pv = 1;
        assert!(
            !should_use_best_thread_selection(&limits, true),
            "Skill有効時はbest thread選抜を使わない"
        );
    }

    #[test]
    fn test_select_best_summary_index_prefers_move_vote_over_single_outlier() {
        let m_2g2f = Move::from_usi("2g2f").expect("valid move");
        let m_6i7h = Move::from_usi("6i7h").expect("valid move");
        let summaries = vec![
            ThreadSummary {
                id: 0,
                score: Value::new(30),
                completed_depth: 10,
                best_move: m_2g2f,
                pv_len: 4,
            },
            ThreadSummary {
                id: 1,
                score: Value::new(28),
                completed_depth: 9,
                best_move: m_2g2f,
                pv_len: 4,
            },
            ThreadSummary {
                id: 2,
                score: Value::new(90),
                completed_depth: 1,
                best_move: m_6i7h,
                pv_len: 4,
            },
        ];

        let idx = select_best_summary_index(&summaries);
        assert_eq!(summaries[idx].best_move, m_2g2f);
    }

    #[test]
    fn test_select_best_summary_index_prefers_shorter_win_line() {
        let m_2g2f = Move::from_usi("2g2f").expect("valid move");
        let m_7g7f = Move::from_usi("7g7f").expect("valid move");
        let summaries = vec![
            ThreadSummary {
                id: 0,
                score: Value::mate_in(7),
                completed_depth: 12,
                best_move: m_2g2f,
                pv_len: 4,
            },
            ThreadSummary {
                id: 1,
                score: Value::mate_in(5),
                completed_depth: 8,
                best_move: m_7g7f,
                pv_len: 4,
            },
        ];

        let idx = select_best_summary_index(&summaries);
        assert_eq!(idx, 1, "勝ち筋同士ではより短手数の詰みを優先する");
    }

    #[test]
    fn test_select_best_summary_index_rejects_single_helper_outlier_move() {
        let m_2g2f = Move::from_usi("2g2f").expect("valid move");
        let m_6i7h = Move::from_usi("6i7h").expect("valid move");
        let m_7g7f = Move::from_usi("7g7f").expect("valid move");
        let summaries = vec![
            ThreadSummary {
                id: 0,
                score: Value::new(35),
                completed_depth: 8,
                best_move: m_2g2f,
                pv_len: 6,
            },
            ThreadSummary {
                id: 1,
                score: Value::new(180),
                completed_depth: 8,
                best_move: m_6i7h,
                pv_len: 6,
            },
            ThreadSummary {
                id: 2,
                score: Value::new(34),
                completed_depth: 8,
                best_move: m_7g7f,
                pv_len: 6,
            },
        ];

        let idx = select_best_summary_index(&summaries);
        assert_eq!(idx, 0, "helper単独の外れ値手ではmain threadを優先する");
    }

    #[test]
    fn test_select_best_summary_index_allows_helper_when_supported_by_multiple_threads() {
        let m_2g2f = Move::from_usi("2g2f").expect("valid move");
        let m_6i7h = Move::from_usi("6i7h").expect("valid move");
        let summaries = vec![
            ThreadSummary {
                id: 0,
                score: Value::new(35),
                completed_depth: 8,
                best_move: m_2g2f,
                pv_len: 6,
            },
            ThreadSummary {
                id: 1,
                score: Value::new(120),
                completed_depth: 8,
                best_move: m_6i7h,
                pv_len: 6,
            },
            ThreadSummary {
                id: 2,
                score: Value::new(110),
                completed_depth: 8,
                best_move: m_6i7h,
                pv_len: 6,
            },
        ];

        let idx = select_best_summary_index(&summaries);
        assert_eq!(summaries[idx].best_move, m_6i7h);
    }

    #[test]
    fn test_prepare_time_metrics_resets_iter_state() {
        std::thread::Builder::new()
            .stack_size(STACK_SIZE)
            .spawn(|| {
                let mut search = Search::new(16);
                search.best_previous_score = Some(Value::new(200));
                search.best_previous_average_score = Some(Value::new(123));
                search.last_game_ply = Some(5);
                search.iter_value = [Value::new(1), Value::new(2), Value::new(3), Value::new(4)];
                search.iter_idx = 2;
                search.last_best_move_depth = 5;
                search.tot_best_move_changes = 7.5;

                search.prepare_time_metrics(6);

                assert_eq!(search.best_previous_score, Some(Value::new(-200)));
                assert_eq!(search.best_previous_average_score, Some(Value::new(-123)));
                assert_eq!(search.iter_value, [Value::new(-200); 4]);
                assert_eq!(search.iter_idx, 0);
                assert_eq!(search.last_best_move_depth, 0);
                assert_eq!(search.tot_best_move_changes, 0.0);
                assert_eq!(search.last_game_ply, Some(6));
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn test_prepare_time_metrics_seeds_zero_for_infinite() {
        std::thread::Builder::new()
            .stack_size(STACK_SIZE)
            .spawn(|| {
                let mut search = Search::new(16);
                search.best_previous_score = Some(Value::INFINITE);
                search.best_previous_average_score = Some(Value::INFINITE);

                search.prepare_time_metrics(1);

                assert_eq!(search.iter_value, [Value::ZERO; 4]);
                assert_eq!(search.iter_idx, 0);
                assert_eq!(search.best_previous_score, Some(Value::INFINITE));
                assert_eq!(search.best_previous_average_score, Some(Value::INFINITE));
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn test_set_max_moves_to_draw_option() {
        std::thread::Builder::new()
            .stack_size(STACK_SIZE)
            .spawn(|| {
                let mut search = Search::new(16);
                search.set_max_moves_to_draw(512);
                assert_eq!(search.max_moves_to_draw(), 512);

                search.set_max_moves_to_draw(0);
                assert_eq!(search.max_moves_to_draw(), DEFAULT_MAX_MOVES_TO_DRAW);
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn test_set_draw_value_options() {
        std::thread::Builder::new()
            .stack_size(STACK_SIZE)
            .spawn(|| {
                let mut search = Search::new(16);

                search.set_draw_value_black(123);
                search.set_draw_value_white(-456);
                assert_eq!(search.draw_value_black(), 123);
                assert_eq!(search.draw_value_white(), -456);

                search.set_draw_value_black(40000);
                search.set_draw_value_white(-40000);
                assert_eq!(search.draw_value_black(), 30000);
                assert_eq!(search.draw_value_white(), -30000);
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn test_mate_within_limit_converts_moves_to_plies() {
        // mate in 9 ply is within a 5-move limit (10 ply)
        assert!(mate_within_limit(Value::mate_in(9), false, false, 5));
        assert!(!mate_within_limit(Value::mate_in(11), false, false, 5));
    }

    #[test]
    fn test_mate_within_limit_handles_mated_scores() {
        // mated in 7 ply should still trigger when limit is 4 moves (8 ply)
        assert!(mate_within_limit(Value::mated_in(7), false, false, 4));
    }

    #[test]
    fn test_mate_within_limit_requires_exact_score() {
        assert!(!mate_within_limit(Value::mate_in(7), true, false, 4));
        assert!(!mate_within_limit(Value::mate_in(7), false, true, 4));
    }

    #[test]
    fn test_search_basic() {
        // スタックサイズを増やした別スレッドで実行
        std::thread::Builder::new()
            .stack_size(STACK_SIZE)
            .spawn(|| {
                let mut search = Search::new(16);
                let mut pos = Position::new();
                pos.set_hirate();

                let limits = LimitsType {
                    depth: 3,
                    ..Default::default()
                };

                let result = search.go(&mut pos, limits, None::<fn(&SearchInfo)>);

                assert_ne!(result.best_move, Move::NONE, "Should find a best move");
                assert!(result.depth >= 1, "Should complete at least depth 1");
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn test_search_with_callback() {
        // スタックサイズを増やした別スレッドで実行
        std::thread::Builder::new()
            .stack_size(STACK_SIZE)
            .spawn(|| {
                let mut search = Search::new(16);
                let mut pos = Position::new();
                pos.set_hirate();

                let limits = LimitsType {
                    depth: 2,
                    ..Default::default()
                };

                let mut info_count = 0;
                let result = search.go(
                    &mut pos,
                    limits,
                    Some(|_info: &SearchInfo| {
                        info_count += 1;
                    }),
                );

                assert_ne!(result.best_move, Move::NONE, "Should find a best move");
                assert!(info_count >= 1, "Should have called info callback at least once");
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn test_search_info_to_usi() {
        let info = SearchInfo {
            depth: 5,
            sel_depth: 7,
            score: Value::new(123),
            nodes: 10000,
            time_ms: 500,
            nps: 20000,
            hashfull: 100,
            pv: vec![],
            multi_pv: 1,
        };

        let usi = info.to_usi_string();
        assert!(usi.contains("depth 5"));
        assert!(usi.contains("seldepth 7"));
        assert!(usi.contains("multipv 1"));
        // Value::new(123) → to_cp() = 100 * 123 / 90 = 136
        assert!(usi.contains("score cp 136"));
        assert!(usi.contains("nodes 10000"));
    }

    #[test]
    fn test_search_info_to_usi_formats_mate_score() {
        let info = SearchInfo {
            depth: 9,
            sel_depth: 9,
            score: Value::mate_in(5),
            nodes: 42,
            time_ms: 10,
            nps: 4200,
            hashfull: 0,
            pv: vec![],
            multi_pv: 1,
        };

        let usi = info.to_usi_string();
        assert!(usi.contains("score mate 5"));
    }

    #[test]
    fn test_search_info_to_usi_formats_mated_score_with_negative_sign() {
        let info = SearchInfo {
            depth: 9,
            sel_depth: 9,
            score: Value::mated_in(4),
            nodes: 42,
            time_ms: 10,
            nps: 4200,
            hashfull: 0,
            pv: vec![],
            multi_pv: 1,
        };

        let usi = info.to_usi_string();
        assert!(usi.contains("score mate -4"));
    }
}
