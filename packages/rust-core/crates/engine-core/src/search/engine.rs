//! 探索エンジンのエントリポイント
//!
//! USIプロトコルから呼び出すためのハイレベルインターフェース。

use crate::time::Instant;
// AtomicU64 is only needed for native multi-threaded builds.
// Wasm Rayon model doesn't use SearchProgress.
#[cfg(not(target_arch = "wasm32"))]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use super::time_manager::{
    calculate_falling_eval, calculate_time_reduction, normalize_nodes_effort,
    DEFAULT_MAX_MOVES_TO_DRAW,
};
use super::{LimitsType, RootMove, SearchWorker, Skill, SkillOptions, ThreadPool, TimeManagement};
use crate::position::Position;
use crate::tt::TranspositionTable;
use crate::types::{Depth, Move, Value, MAX_PLY};

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
                format!("cp {}", self.score.raw())
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

/// YaneuraOu準拠のaspiration windowを計算
pub(crate) fn compute_aspiration_window(rm: &RootMove, thread_id: usize) -> (Value, Value, Value) {
    // mean_squared_score がない場合は巨大なdeltaでフルウィンドウにする
    let fallback = {
        let inf = Value::INFINITE.raw() as i64;
        inf * inf
    };
    let mean_sq = rm.mean_squared_score.unwrap_or(fallback).abs();
    let mean_sq = mean_sq.min((Value::INFINITE.raw() as i64) * (Value::INFINITE.raw() as i64));

    let thread_offset = (thread_id % 8) as i32;
    let score_factor = rm.average_score.raw().abs() / 9000;
    let delta_raw =
        5 + thread_offset + score_factor + (mean_sq / 11131).min(i32::MAX as i64) as i32;
    let delta = Value::new(delta_raw);
    let alpha_raw = (rm.average_score.raw() - delta.raw()).max(-Value::INFINITE.raw());
    let beta_raw = (rm.average_score.raw() + delta.raw()).min(Value::INFINITE.raw());

    (Value::new(alpha_raw), Value::new(beta_raw), delta)
}

/// YaneuraOu準拠の詰みスコアに対する深さ打ち切り判定
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
}

// =============================================================================
// Search - 探索エンジン
// =============================================================================

/// 探索エンジン
///
/// USIプロトコルから呼び出すための主要インターフェース。
pub struct Search {
    /// 置換表
    tt: Arc<TranspositionTable>,
    /// 置換表のサイズ（MB）
    tt_size_mb: usize,
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

    /// SearchWorker（YaneuraOu準拠: 長期保持して再利用）
    /// 履歴統計を含み、usinewgameでクリア、goでは保持
    worker: Option<Box<SearchWorker>>,

    /// 直前イテレーションのスコア（YaneuraOu準拠）
    best_previous_score: Option<Value>,
    /// 直前イテレーションの平均スコア（YaneuraOu準拠）
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
    /// 直前の timeReduction（YO準拠で次手に持ち回る）
    previous_time_reduction: f64,
    /// 直前の手数（手番反転の検出用）
    last_game_ply: Option<i32>,
    /// 次のiterationで深さを伸ばすかどうか（YaneuraOu準拠）
    increase_depth: bool,
    /// 深さを伸ばせなかった回数（aspiration時の調整に使用）
    search_again_counter: i32,

    /// 引き分けまでの最大手数（YaneuraOu準拠のエンジンオプション）
    max_moves_to_draw: i32,
}

/// ワーカーから集約する軽量サマリ（並列探索を見据えて追加）
struct WorkerSummary {
    best_move_changes: f64,
}

impl From<&SearchWorker> for WorkerSummary {
    fn from(w: &SearchWorker) -> Self {
        Self {
            best_move_changes: w.best_move_changes,
        }
    }
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
}

impl ThreadSummary {
    fn from_worker(id: usize, worker: &SearchWorker) -> Option<Self> {
        worker.root_moves.get(0).map(|rm| Self {
            id,
            score: rm.score,
            completed_depth: worker.completed_depth,
        })
    }
}

fn get_best_thread_id(main_worker: &SearchWorker, thread_pool: &ThreadPool) -> usize {
    let mut summaries = Vec::new();
    if let Some(summary) = ThreadSummary::from_worker(0, main_worker) {
        summaries.push(summary);
    }

    for thread in thread_pool.helper_threads() {
        if let Some(summary) = thread.with_worker(|worker: &mut SearchWorker| {
            ThreadSummary::from_worker(thread.id(), worker)
        }) {
            summaries.push(summary);
        }
    }

    if summaries.is_empty() {
        return 0;
    }

    if let Some(win) = summaries.iter().find(|s| s.score.is_win()) {
        return win.id;
    }

    let min_score = summaries.iter().map(|s| s.score.raw()).min().unwrap_or(0);

    let mut best_id = summaries[0].id;
    let mut best_value = i64::MIN;
    for summary in summaries {
        let vote_value =
            (summary.score.raw() - min_score + 14) as i64 * summary.completed_depth as i64;
        if vote_value > best_value {
            best_value = vote_value;
            best_id = summary.id;
        }
    }

    best_id
}

struct BestThreadResult {
    best_move: Move,
    ponder_move: Move,
    score: Value,
    completed_depth: Depth,
    nodes: u64,
    best_previous_score: Option<Value>,
    best_previous_average_score: Option<Value>,
}

fn collect_best_thread_result(
    worker: &SearchWorker,
    limits: &LimitsType,
    skill_enabled: bool,
    skill: &mut Skill,
) -> BestThreadResult {
    let completed_depth = worker.completed_depth;
    let nodes = worker.nodes;
    let best_previous_score = worker.root_moves.get(0).map(|rm| rm.score);
    let best_previous_average_score = worker.root_moves.get(0).map(|rm| {
        if rm.average_score.raw() == -Value::INFINITE.raw() {
            rm.score
        } else {
            rm.average_score
        }
    });

    if worker.root_moves.is_empty() {
        return BestThreadResult {
            best_move: Move::NONE,
            ponder_move: Move::NONE,
            score: Value::ZERO,
            completed_depth,
            nodes,
            best_previous_score,
            best_previous_average_score,
        };
    }

    let mut effective_multi_pv = limits.multi_pv;
    if skill_enabled {
        effective_multi_pv = effective_multi_pv.max(4);
    }
    effective_multi_pv = effective_multi_pv.min(worker.root_moves.len());

    let mut best_move = worker.best_move;
    if skill_enabled && effective_multi_pv > 0 {
        let mut rng = rand::rng();
        let best = skill.pick_best(&worker.root_moves, effective_multi_pv, &mut rng);
        if best != Move::NONE {
            best_move = best;
        }
    }

    let ponder_move = worker
        .root_moves
        .iter()
        .find(|rm| rm.mv() == best_move)
        .and_then(|rm| {
            if rm.pv.len() > 1 {
                Some(rm.pv[1])
            } else {
                None
            }
        })
        .unwrap_or(Move::NONE);

    let score = worker
        .root_moves
        .iter()
        .find(|rm| rm.mv() == best_move)
        .map(|rm| rm.score)
        .unwrap_or(worker.root_moves.get(0).map(|rm| rm.score).unwrap_or(Value::ZERO));

    BestThreadResult {
        best_move,
        ponder_move,
        score,
        completed_depth,
        nodes,
        best_previous_score,
        best_previous_average_score,
    }
}

impl Search {
    /// 時間計測用のメトリクスを準備（対局/Go開始時）
    fn prepare_time_metrics(&mut self, ply: i32) {
        // 手番が変わっている場合はスコア符号を反転
        if let Some(last_ply) = self.last_game_ply {
            if (last_ply - ply).abs() & 1 == 1 {
                if let Some(prev_score) = self.best_previous_score {
                    if prev_score != Value::INFINITE {
                        self.best_previous_score = Some(Value::new(-prev_score.raw()));
                    }
                }
                if let Some(prev_avg) = self.best_previous_average_score {
                    if prev_avg != Value::INFINITE {
                        self.best_previous_average_score = Some(Value::new(-prev_avg.raw()));
                    }
                }
            }
        }

        // best_previous_score が番兵(INFINITE)のときは 0 初期化（YO準拠）
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
        self.search_again_counter = 0;
    }

    /// fallingEval / timeReduction / totBestMoveChanges を計算
    ///
    /// YaneuraOu準拠の式を簡略化して single thread で適用する。
    fn compute_time_factors(
        &self,
        best_value: Value,
        completed_depth: Depth,
        tot_best_move_changes: f64,
        thread_count: usize,
    ) -> (f64, f64, f64, usize) {
        // fallingEval
        let prev_avg_raw = self.best_previous_average_score.unwrap_or(Value::INFINITE).raw();
        let iter_val = self.iter_value[self.iter_idx];
        let falling_eval = calculate_falling_eval(prev_avg_raw, iter_val.raw(), best_value.raw());

        // timeReduction
        let time_reduction = calculate_time_reduction(completed_depth, self.last_best_move_depth);

        (falling_eval, time_reduction, tot_best_move_changes, thread_count)
    }

    /// 時間要因計算後の状態更新
    fn update_time_factor_state(&mut self, best_value: Value, tot_best_move_changes: f64) {
        self.iter_value[self.iter_idx] = best_value;
        self.iter_idx = (self.iter_idx + 1) % self.iter_value.len();
        self.tot_best_move_changes = tot_best_move_changes;
    }

    /// 新しいSearchを作成
    ///
    /// # Arguments
    /// * `tt_size_mb` - 置換表のサイズ（MB）
    pub fn new(tt_size_mb: usize) -> Self {
        let tt = Arc::new(TranspositionTable::new(tt_size_mb));
        let stop = Arc::new(AtomicBool::new(false));
        let ponderhit_flag = Arc::new(AtomicBool::new(false));
        let max_moves_to_draw = DEFAULT_MAX_MOVES_TO_DRAW;
        let thread_pool = ThreadPool::new(
            1,
            Arc::clone(&tt),
            Arc::clone(&stop),
            Arc::clone(&ponderhit_flag),
            max_moves_to_draw,
        );

        Self {
            tt,
            tt_size_mb,
            stop,
            ponderhit_flag,
            start_time: None,
            time_options: super::TimeOptions::default(),
            skill_options: SkillOptions::default(),
            num_threads: 1,
            thread_pool,
            // YaneuraOu準拠: workerは遅延初期化（最初のgoで作成）
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
            search_again_counter: 0,
            max_moves_to_draw,
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

    /// 履歴統計をクリア（usinewgame時に呼び出し）
    ///
    /// YaneuraOu準拠: Worker::clear()相当
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

    /// 探索スレッド数を設定
    pub fn set_num_threads(&mut self, num: usize) {
        let num = num.clamp(1, 512);
        // WASM builds without wasm-threads feature use single-threaded search only.
        // With wasm-threads feature, multi-threading via wasm-bindgen-rayon is supported.
        #[cfg(all(target_arch = "wasm32", not(feature = "wasm-threads")))]
        let num = 1;
        self.num_threads = num;
        self.thread_pool
            .set_num_threads(num, Arc::clone(&self.tt), self.max_moves_to_draw);
    }

    /// 探索スレッド数を取得
    pub fn num_threads(&self) -> usize {
        self.num_threads
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
        // 停止フラグをリセット
        self.stop.store(false, Ordering::SeqCst);
        // ponderhitフラグをリセット
        self.ponderhit_flag.store(false, Ordering::SeqCst);
        self.start_time = Some(Instant::now());
        // 置換表の世代を進める（YaneuraOu準拠）
        self.tt.new_search();

        // 時間管理
        let mut time_manager =
            TimeManagement::new(Arc::clone(&self.stop), Arc::clone(&self.ponderhit_flag));
        time_manager.set_options(&self.time_options);
        time_manager.set_previous_time_reduction(self.previous_time_reduction);
        // ply（現在の手数）は局面から取得、max_moves_to_drawはYaneuraOu準拠のデフォルトを使う
        time_manager.init(&limits, pos.side_to_move(), ply, self.max_moves_to_draw);

        // YaneuraOu準拠: workerは遅延初期化、再利用する
        let tt_clone = Arc::clone(&self.tt);
        let max_moves = self.max_moves_to_draw;
        let worker = self.worker.get_or_insert_with(|| SearchWorker::new(tt_clone, max_moves, 0));

        // setoptionで変更された可能性があるため、最新値を反映
        worker.max_moves_to_draw = self.max_moves_to_draw;

        // 探索状態のリセット（履歴はクリアしない、YaneuraOu準拠）
        worker.prepare_search();

        // 探索深さを決定
        let max_depth = if limits.depth > 0 {
            limits.depth
        } else {
            MAX_PLY // YaneuraOu準拠: 可能な限り深く探索
        };

        // SkillLevel設定を構築（手加減）
        let mut skill = Skill::from_options(&self.skill_options);
        let skill_enabled = skill.enabled();

        if self.num_threads > 1 {
            self.thread_pool.start_thinking(
                pos,
                limits.clone(),
                max_depth,
                self.time_options,
                self.max_moves_to_draw,
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

        if self.num_threads > 1 {
            self.stop.store(true, Ordering::SeqCst);
            self.thread_pool.wait_for_search_finished();
        }

        let best_thread_id = {
            let worker = self
                .worker
                .as_ref()
                .expect("worker should be initialized by search_with_callback");
            get_best_thread_id(worker, &self.thread_pool)
        };

        let best_result = if best_thread_id == 0 {
            let worker = self
                .worker
                .as_ref()
                .expect("worker should be initialized by search_with_callback");
            collect_best_thread_result(worker, &limits, skill_enabled, &mut skill)
        } else {
            let mut result = None;
            for thread in self.thread_pool.helper_threads() {
                if thread.id() == best_thread_id {
                    result = Some(thread.with_worker(|worker: &mut SearchWorker| {
                        collect_best_thread_result(worker, &limits, skill_enabled, &mut skill)
                    }));
                    break;
                }
            }
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
        } = best_result;
        let total_nodes = {
            let main_nodes = self.worker.as_ref().map(|w| w.nodes).unwrap_or(0);
            let helper_nodes =
                self.thread_pool.helper_threads().iter().fold(0u64, |acc, thread| {
                    acc.saturating_add(thread.with_worker(|worker| worker.nodes))
                });
            main_nodes.saturating_add(helper_nodes)
        };

        // 次の手番のために timeReduction を持ち回る
        self.previous_time_reduction = time_manager.previous_time_reduction();

        // 次回のfallingEval計算のために平均スコアを保存
        self.best_previous_score = best_previous_score;
        self.best_previous_average_score = best_previous_average_score;
        self.last_game_ply = Some(ply);

        SearchResult {
            best_move,
            ponder_move,
            score,
            depth: completed_depth,
            nodes: total_nodes,
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
        self.search_again_counter = 0;

        // workerを一時的に取り出す（借用チェッカー対策）
        let mut worker = self.worker.take().expect("worker should be available");

        // ルート手を初期化
        worker.root_moves = super::RootMoves::from_legal_moves(pos, &limits.search_moves);

        if worker.root_moves.is_empty() {
            worker.best_move = Move::NONE;
            #[cfg(debug_assertions)]
            eprintln!(
                "search_with_callback: root_moves is empty (search_moves_len={}, side_to_move={:?})",
                limits.search_moves.len(),
                pos.side_to_move()
            );
            self.worker = Some(worker);
            return 0;
        }

        #[cfg(debug_assertions)]
        eprintln!(
            "search_with_callback: root_moves_len={} first_move={}",
            worker.root_moves.len(),
            worker.root_moves.get(0).map(|rm| rm.mv().to_usi()).unwrap_or_default()
        );

        // 合法手が1つの場合は500ms上限を適用（YaneuraOu準拠）
        if worker.root_moves.len() == 1 {
            time_manager.apply_single_move_limit();
        }

        let start = self.start_time.unwrap();
        let mut effective_multi_pv = limits.multi_pv;
        if skill_enabled {
            effective_multi_pv = effective_multi_pv.max(4);
        }
        effective_multi_pv = effective_multi_pv.min(worker.root_moves.len());

        // 中断時にPVを巻き戻すための保持
        let mut last_best_pv = vec![Move::NONE];
        let mut last_best_score = Value::new(-Value::INFINITE.raw());
        let mut last_best_move_depth = 0;

        // 反復深化
        for depth in 1..=max_depth {
            #[cfg(debug_assertions)]
            if depth <= 2 {
                eprintln!(
                    "search_with_callback: depth={} nodes={} search_end={} max_time={} stop_requested={}",
                    depth,
                    worker.nodes,
                    time_manager.search_end(),
                    time_manager.maximum(),
                    time_manager.stop_requested()
                );
            }
            // 前回のiterationで深さを伸ばせなかった場合のカウンター（YO準拠）
            if depth > 1 && !self.increase_depth {
                self.search_again_counter += 1;
            }

            if worker.abort {
                break;
            }

            // ponderhitを検出した場合、時間再計算のみ行い探索は継続
            if self.ponderhit_flag.swap(false, Ordering::Relaxed) {
                time_manager.on_ponderhit();
            }

            // YaneuraOu準拠: depth 2以降は、次の深さを探索する時間があるかチェック
            // depth 1は必ず探索する（合法手が1つもない場合のresignを防ぐため）
            let is_pondering = time_manager.is_pondering();
            if depth > 1 && !is_pondering && time_manager.should_stop(depth) {
                break;
            }

            // YaneuraOu準拠: 詰みを読みきった場合の早期終了
            // 詰みまでの手数の2.5倍以上の深さを探索したら終了
            // MultiPV=1の時のみ適用（MultiPV>1では全候補を探索する必要がある）
            if effective_multi_pv == 1 && depth > 1 && !worker.root_moves.is_empty() {
                let best_value = worker.root_moves[0].score;

                if limits.mate == 0 {
                    if proven_mate_depth_exceeded(best_value, depth) {
                        break;
                    }
                } else if mate_within_limit(
                    best_value,
                    worker.root_moves[0].score_lower_bound,
                    worker.root_moves[0].score_upper_bound,
                    limits.mate,
                ) {
                    time_manager.request_stop();
                    break;
                }
            }

            let search_depth = depth;

            worker.root_depth = search_depth;
            worker.sel_depth = 0;

            // MultiPVループ（YaneuraOu準拠）
            let mut processed_pv = 0;
            for pv_idx in 0..effective_multi_pv {
                if worker.abort {
                    break;
                }

                // Aspiration Window（average/mean_squaredベース）
                let (mut alpha, mut beta, mut delta) =
                    compute_aspiration_window(&worker.root_moves[pv_idx], worker.thread_id);
                let mut failed_high_cnt = 0;

                // Aspiration Windowループ
                loop {
                    let adjusted_depth = (search_depth
                        - failed_high_cnt
                        - (3 * (self.search_again_counter + 1) / 4))
                        .max(1);
                    // pv_idx=0の場合は従来のsearch_rootを使用（後方互換性）
                    // pv_idx>0の場合のみsearch_root_for_pvを使用
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

                    if worker.abort {
                        break;
                    }

                    // Window調整
                    if score <= alpha {
                        beta = alpha;
                        alpha = Value::new(
                            score.raw().saturating_sub(delta.raw()).max(-Value::INFINITE.raw()),
                        );
                        failed_high_cnt = 0;
                        time_manager.reset_stop_on_ponderhit();
                    } else if score >= beta {
                        beta = Value::new(
                            score.raw().saturating_add(delta.raw()).min(Value::INFINITE.raw()),
                        );
                        failed_high_cnt += 1;
                    } else {
                        break;
                    }

                    delta = Value::new(delta.raw() + delta.raw() / 3);
                }

                // 安定ソート [pv_idx..]
                worker.root_moves.stable_sort_range(pv_idx, worker.root_moves.len());
                // 📝 YaneuraOu行1477-1483: 探索済みのPVライン全体も安定ソートして順位を保つ
                worker.root_moves.stable_sort_range(0, pv_idx + 1);
                processed_pv = pv_idx + 1;
            }

            // 🆕 MultiPVループ完了後の最終ソート（YaneuraOu行1499）
            if !worker.abort && effective_multi_pv > 1 {
                worker.root_moves.stable_sort_range(0, effective_multi_pv);
            }

            // info出力は深さごとにまとめて行う（GUI詰まり防止のYO仕様）
            if processed_pv > 0 {
                let elapsed = start.elapsed();
                let time_ms = elapsed.as_millis() as u64;
                let helper_nodes = self
                    .thread_pool
                    .helper_threads()
                    .iter()
                    .fold(0u64, |acc, thread| acc.saturating_add(thread.nodes()));
                let total_nodes = worker.nodes.saturating_add(helper_nodes);
                let nps = if time_ms > 0 {
                    total_nodes.saturating_mul(1000) / time_ms
                } else {
                    0
                };

                for pv_idx in 0..processed_pv {
                    let info = SearchInfo {
                        depth,
                        sel_depth: worker.root_moves[pv_idx].sel_depth,
                        score: worker.root_moves[pv_idx].score,
                        nodes: total_nodes,
                        time_ms,
                        nps,
                        hashfull: self.tt.hashfull(3) as u32,
                        pv: worker.root_moves[pv_idx].pv.clone(),
                        multi_pv: pv_idx + 1, // 1-indexed
                    };

                    on_info(&info);
                }
            }

            // Depth完了後の処理
            if !worker.abort {
                worker.completed_depth = search_depth;
                worker.best_move = worker.root_moves[0].mv();
                if worker.best_move != self.last_best_move {
                    self.last_best_move = worker.best_move;
                    self.last_best_move_depth = depth;
                }

                // 🆕 YaneuraOu準拠: previous_scoreを次のiterationのためにシード
                // （YaneuraOu行1267-1270: rm.previousScore = rm.score）
                for rm in worker.root_moves.iter_mut() {
                    rm.previous_score = rm.score;
                }

                // 評価変動・timeReduction・最善手不安定性をまとめて適用（YaneuraOu準拠）
                // 借用チェッカー対策: workerから必要な値をすべてローカルにコピー
                let summary = WorkerSummary::from(&*worker);
                let best_value = if worker.root_moves.is_empty() {
                    Value::ZERO
                } else {
                    worker.root_moves[0].score
                };
                let completed_depth = worker.completed_depth;
                let effort = if worker.root_moves.is_empty() {
                    0.0
                } else {
                    worker.root_moves[0].effort
                };
                let nodes = worker.nodes;
                let root_moves_len = worker.root_moves.len();
                let best_move_changes = summary.best_move_changes;
                worker.best_move_changes = 0.0; // 先にリセット

                let (changes_sum, thread_count) = {
                    let helper_threads = self.thread_pool.helper_threads();
                    let mut changes = Vec::with_capacity(helper_threads.len() + 1);
                    changes.push(best_move_changes);
                    for thread in helper_threads {
                        changes.push(thread.best_move_changes());
                    }
                    aggregate_best_move_changes(&changes)
                };
                let tot_best_move_changes = self.tot_best_move_changes / 2.0 + changes_sum;
                if limits.use_time_management()
                    && !time_manager.stop_on_ponderhit()
                    && time_manager.search_end() == 0
                {
                    let (falling_eval, time_reduction, tot_changes, threads) = self
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

                    // 実測 effort を正規化
                    let nodes_effort = normalize_nodes_effort(effort, nodes);

                    // 合法手が1つの場合は使う時間そのものを500msに丸める（YaneuraOu準拠）
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

                    // YaneuraOu準拠: 次iterationで深さを伸ばすかの判定
                    self.increase_depth =
                        time_manager.is_pondering() || elapsed_time <= total_time * 0.5138;

                    // 状態更新
                    self.update_time_factor_state(best_value, tot_best_move_changes);
                }
                // tot_best_move_changes は decay 後の値を保持（時間管理を使わない場合も持ち回る）
                self.tot_best_move_changes = tot_best_move_changes;

                // PVが変わったときのみ last_best_* を更新（YO準拠）
                if !worker.root_moves[0].pv.is_empty()
                    && worker.root_moves[0].pv[0] != last_best_pv[0]
                {
                    last_best_pv = worker.root_moves[0].pv.clone();
                    last_best_score = worker.root_moves[0].score;
                    last_best_move_depth = depth;
                }

                // YaneuraOu準拠: 詰みスコアが見つかっていたら早期終了
                // MultiPV=1の時のみ適用
                if effective_multi_pv == 1 && depth > 1 && !worker.root_moves.is_empty() {
                    let best_value = worker.root_moves[0].score;

                    if limits.mate == 0 {
                        if proven_mate_depth_exceeded(best_value, depth) {
                            break;
                        }
                    } else if mate_within_limit(
                        best_value,
                        worker.root_moves[0].score_lower_bound,
                        worker.root_moves[0].score_upper_bound,
                        limits.mate,
                    ) {
                        time_manager.request_stop();
                        break;
                    }
                }
            }
        }

        // 中断した探索で信頼できないPVになった場合のフォールバック（YO準拠）
        if worker.abort && !worker.root_moves.is_empty() && worker.root_moves[0].score.is_loss() {
            let head = last_best_pv.first().copied().unwrap_or(Move::NONE);
            if head != Move::NONE {
                if let Some(idx) = worker.root_moves.find(head) {
                    worker.root_moves.move_to_front(idx);
                    worker.root_moves[0].pv = last_best_pv;
                    worker.root_moves[0].score = last_best_score;
                    worker.completed_depth = last_best_move_depth;
                }
            }
        }

        // workerを戻す
        self.worker = Some(worker);

        effective_multi_pv
    }
}

// search_helper_impl is the core search logic used by helper threads.
// Progress callbacks are passed as closures to avoid including progress code in Wasm builds.
#[cfg(any(not(target_arch = "wasm32"), feature = "wasm-threads"))]
#[inline(always)]
fn search_helper_impl<F1, F2>(
    worker: &mut SearchWorker,
    pos: &mut Position,
    limits: &LimitsType,
    time_manager: &mut TimeManagement,
    max_depth: Depth,
    skill_enabled: bool,
    on_start: F1,
    mut on_depth_complete: F2,
) -> usize
where
    F1: FnOnce(),
    F2: FnMut(u64, f64),
{
    on_start();

    worker.root_moves = super::RootMoves::from_legal_moves(pos, &limits.search_moves);

    if worker.root_moves.is_empty() {
        worker.best_move = Move::NONE;
        return 0;
    }

    // 合法手が1つの場合は500ms上限を適用（YaneuraOu準拠）
    if worker.root_moves.len() == 1 {
        time_manager.apply_single_move_limit();
    }

    let mut effective_multi_pv = limits.multi_pv;
    if skill_enabled {
        effective_multi_pv = effective_multi_pv.max(4);
    }
    effective_multi_pv = effective_multi_pv.min(worker.root_moves.len());

    let mut last_best_pv = vec![Move::NONE];
    let mut last_best_score = Value::new(-Value::INFINITE.raw());
    let mut last_best_move_depth = 0;

    let search_again_counter = 0;

    for depth in 1..=max_depth {
        if worker.abort {
            break;
        }

        if effective_multi_pv == 1 && depth > 1 && !worker.root_moves.is_empty() {
            let best_value = worker.root_moves[0].score;

            if limits.mate == 0 {
                if proven_mate_depth_exceeded(best_value, depth) {
                    break;
                }
            } else if mate_within_limit(
                best_value,
                worker.root_moves[0].score_lower_bound,
                worker.root_moves[0].score_upper_bound,
                limits.mate,
            ) {
                break;
            }
        }

        let search_depth = depth;

        worker.root_depth = search_depth;
        worker.sel_depth = 0;

        for pv_idx in 0..effective_multi_pv {
            if worker.abort {
                break;
            }

            let (mut alpha, mut beta, mut delta) =
                compute_aspiration_window(&worker.root_moves[pv_idx], worker.thread_id);
            let mut failed_high_cnt = 0;

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

                if worker.abort {
                    break;
                }

                if score <= alpha {
                    beta = alpha;
                    alpha = Value::new(
                        score.raw().saturating_sub(delta.raw()).max(-Value::INFINITE.raw()),
                    );
                    failed_high_cnt = 0;
                } else if score >= beta {
                    beta = Value::new(
                        score.raw().saturating_add(delta.raw()).min(Value::INFINITE.raw()),
                    );
                    failed_high_cnt += 1;
                } else {
                    break;
                }

                delta = Value::new(delta.raw() + delta.raw() / 3);
            }

            worker.root_moves.stable_sort_range(pv_idx, worker.root_moves.len());
            worker.root_moves.stable_sort_range(0, pv_idx + 1);
        }

        if !worker.abort && effective_multi_pv > 1 {
            worker.root_moves.stable_sort_range(0, effective_multi_pv);
        }

        if !worker.abort {
            worker.completed_depth = search_depth;
            worker.best_move = worker.root_moves[0].mv();

            for rm in worker.root_moves.iter_mut() {
                rm.previous_score = rm.score;
            }

            let best_move_changes = worker.best_move_changes;
            worker.best_move_changes = 0.0;
            on_depth_complete(worker.nodes, best_move_changes);

            if !worker.root_moves[0].pv.is_empty() && worker.root_moves[0].pv[0] != last_best_pv[0]
            {
                last_best_pv = worker.root_moves[0].pv.clone();
                last_best_score = worker.root_moves[0].score;
                last_best_move_depth = search_depth;
            }

            if effective_multi_pv == 1 && depth > 1 && !worker.root_moves.is_empty() {
                let best_value = worker.root_moves[0].score;

                if limits.mate == 0 {
                    if proven_mate_depth_exceeded(best_value, depth) {
                        break;
                    }
                } else if mate_within_limit(
                    best_value,
                    worker.root_moves[0].score_lower_bound,
                    worker.root_moves[0].score_upper_bound,
                    limits.mate,
                ) {
                    break;
                }
            }
        }
    }

    if worker.abort && !worker.root_moves.is_empty() && worker.root_moves[0].score.is_loss() {
        let head = last_best_pv.first().copied().unwrap_or(Move::NONE);
        if head != Move::NONE {
            if let Some(idx) = worker.root_moves.find(head) {
                worker.root_moves.move_to_front(idx);
                worker.root_moves[0].pv = last_best_pv;
                worker.root_moves[0].score = last_best_score;
                worker.completed_depth = last_best_move_depth;
            }
        }
    }

    effective_multi_pv
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
) -> usize {
    search_helper_impl(
        worker,
        pos,
        limits,
        time_manager,
        max_depth,
        skill_enabled,
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

// Wasm version: no progress parameter. Empty closures are optimized away by LLVM.
#[cfg(all(target_arch = "wasm32", feature = "wasm-threads"))]
pub(crate) fn search_helper(
    worker: &mut SearchWorker,
    pos: &mut Position,
    limits: &LimitsType,
    time_manager: &mut TimeManagement,
    max_depth: Depth,
    skill_enabled: bool,
) -> usize {
    search_helper_impl(
        worker,
        pos,
        limits,
        time_manager,
        max_depth,
        skill_enabled,
        || {},
        |_, _| {},
    )
}

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
    fn test_worker_summary_from_worker() {
        // SearchWorker はスタックを大きく消費するため、別スレッドで実行する。
        std::thread::Builder::new()
            .stack_size(STACK_SIZE)
            .spawn(|| {
                let tt = Arc::new(TranspositionTable::new(16));
                let mut worker = SearchWorker::new(tt, DEFAULT_MAX_MOVES_TO_DRAW, 0);
                worker.best_move_changes = 3.5;

                let summary = WorkerSummary::from(&*worker);
                assert!(
                    (summary.best_move_changes - 3.5).abs() < 1e-9,
                    "best_move_changes should match"
                );
            })
            .unwrap()
            .join()
            .unwrap();
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
        assert!(usi.contains("score cp 123"));
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
