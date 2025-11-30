//! 探索エンジンのエントリポイント
//!
//! USIプロトコルから呼び出すためのハイレベルインターフェース。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use crate::position::Position;
use crate::tt::TranspositionTable;
use crate::types::{Depth, Move, Value};

use super::alpha_beta::init_reductions;
use super::time_manager::{
    calculate_falling_eval, calculate_time_reduction, normalize_nodes_effort,
    DEFAULT_MAX_MOVES_TO_DRAW,
};
use super::{LimitsType, RootMove, SearchWorker, Skill, SkillOptions, TimeManagement};

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
        let mut s = format!(
            "info depth {depth} seldepth {sel_depth} multipv {multi_pv} score cp {score} nodes {nodes} time {time_ms} nps {nps} hashfull {hashfull}",
            depth = self.depth,
            sel_depth = self.sel_depth,
            multi_pv = self.multi_pv,
            score = self.score.raw(),
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
pub(crate) fn compute_aspiration_window(rm: &RootMove) -> (Value, Value, Value) {
    // mean_squared_score がない場合は巨大なdeltaでフルウィンドウにする
    let fallback = {
        let inf = Value::INFINITE.raw() as i64;
        inf * inf
    };
    let mean_sq = rm.mean_squared_score.unwrap_or(fallback).abs();

    let delta_raw = 5 + (mean_sq / 11131) as i32;
    let delta = Value::new(delta_raw);
    let alpha_raw = (rm.average_score.raw() - delta.raw()).max(-Value::INFINITE.raw());
    let beta_raw = (rm.average_score.raw() + delta.raw()).min(Value::INFINITE.raw());

    (Value::new(alpha_raw), Value::new(beta_raw), delta)
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

    /// 直前イテレーションの平均スコア（YaneuraOu準拠）
    best_previous_average_score: Option<Value>,
    /// 直近のイテレーション値（YaneuraOuは4要素リングバッファ）
    iter_value: [Value; 4],
    /// iter_valueの書き込み位置
    iter_idx: usize,
    /// 直前に安定したとみなした深さ
    last_best_move_depth: Depth,
    /// totBestMoveChanges（世代減衰込み）
    tot_best_move_changes: f64,
    /// 直前の手数（手番反転の検出用）
    last_game_ply: Option<i32>,

    /// 引き分けまでの最大手数（YaneuraOu準拠のエンジンオプション）
    max_moves_to_draw: i32,
}

/// ワーカーから集約する軽量サマリ（並列探索を見据えて追加）
struct WorkerSummary {
    best_move_changes: f64,
}

impl From<&SearchWorker<'_>> for WorkerSummary {
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

impl Search {
    /// 時間計測用のメトリクスを準備（対局/Go開始時）
    fn prepare_time_metrics(&mut self, ply: i32) {
        // 手番が変わっている場合はスコア符号を反転
        if let Some(last_ply) = self.last_game_ply {
            if (last_ply - ply).abs() & 1 == 1 {
                if let Some(prev_avg) = self.best_previous_average_score {
                    self.best_previous_average_score = Some(Value::new(-prev_avg.raw()));
                }
            }
        }

        let seed = self.best_previous_average_score.unwrap_or(Value::ZERO);
        self.iter_value = [seed; 4];
        self.iter_idx = 0;
        self.last_best_move_depth = 0;
        self.tot_best_move_changes = 0.0;
        self.last_game_ply = Some(ply);
    }

    /// fallingEval / timeReduction / totBestMoveChanges を計算
    ///
    /// YaneuraOu準拠の式を簡略化して single thread で適用する。
    fn compute_time_factors(
        &mut self,
        worker: &SearchWorker,
        tot_best_move_changes: f64,
        thread_count: usize,
    ) -> (f64, f64, f64, usize) {
        let best_value = if worker.root_moves.is_empty() {
            Value::ZERO
        } else {
            worker.root_moves[0].score
        };

        // fallingEval
        let falling_eval = if let Some(prev_avg) = self.best_previous_average_score {
            let iter_val = self.iter_value[self.iter_idx];
            calculate_falling_eval(prev_avg.raw(), iter_val.raw(), best_value.raw())
        } else {
            1.0
        };

        // timeReduction
        let time_reduction =
            calculate_time_reduction(worker.completed_depth, self.last_best_move_depth);

        // 状態更新
        if let Some(prev_avg) = self.best_previous_average_score {
            let avg = (prev_avg.raw() + best_value.raw()) / 2;
            self.best_previous_average_score = Some(Value::new(avg));
        } else {
            self.best_previous_average_score = Some(best_value);
        }
        self.iter_value[self.iter_idx] = best_value;
        self.iter_idx = (self.iter_idx + 1) % self.iter_value.len();
        self.last_best_move_depth = worker.completed_depth;
        self.tot_best_move_changes = tot_best_move_changes;

        (falling_eval, time_reduction, tot_best_move_changes, thread_count)
    }

    /// 新しいSearchを作成
    ///
    /// # Arguments
    /// * `tt_size_mb` - 置換表のサイズ（MB）
    pub fn new(tt_size_mb: usize) -> Self {
        // LMRテーブルを初期化
        init_reductions();

        Self {
            tt: Arc::new(TranspositionTable::new(tt_size_mb)),
            tt_size_mb,
            stop: Arc::new(AtomicBool::new(false)),
            ponderhit_flag: Arc::new(AtomicBool::new(false)),
            start_time: None,
            time_options: super::TimeOptions::default(),
            skill_options: SkillOptions::default(),
            best_previous_average_score: None,
            iter_value: [Value::ZERO; 4],
            iter_idx: 0,
            last_best_move_depth: 0,
            tot_best_move_changes: 0.0,
            last_game_ply: None,
            max_moves_to_draw: DEFAULT_MAX_MOVES_TO_DRAW,
        }
    }

    /// 置換表のサイズを変更
    pub fn resize_tt(&mut self, size_mb: usize) {
        self.tt = Arc::new(TranspositionTable::new(size_mb));
        self.tt_size_mb = size_mb;
    }

    /// 置換表をクリア
    ///
    /// 新しい置換表を作成して置き換える。
    pub fn clear_tt(&mut self) {
        // Arc経由では&mutが取れないので、同じサイズの新しいTTを作成して置き換える
        self.tt = Arc::new(TranspositionTable::new(self.tt_size_mb));
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
        // ply（現在の手数）は局面から取得、max_moves_to_drawはYaneuraOu準拠のデフォルトを使う
        time_manager.init(&limits, pos.side_to_move(), ply, self.max_moves_to_draw);

        // 探索ワーカーを作成（ttの借用期間を限定するためArcをクローン）
        let tt_owned = Arc::clone(&self.tt);
        let mut worker = SearchWorker::new(&tt_owned, &limits, &mut time_manager);

        // 探索深さを決定
        let max_depth = if limits.depth > 0 {
            limits.depth
        } else {
            100 // 十分大きな値
        };

        // SkillLevel設定を構築（手加減）
        let mut skill = Skill::from_options(&self.skill_options);
        let skill_enabled = skill.enabled();

        // 探索実行（コールバックなしの場合はダミーを渡す）
        let effective_multi_pv = match on_info {
            Some(callback) => {
                self.search_with_callback(pos, &mut worker, max_depth, callback, skill_enabled)
            }
            None => {
                let mut noop = |_info: &SearchInfo| {};
                self.search_with_callback(pos, &mut worker, max_depth, &mut noop, skill_enabled)
            }
        };

        // Skill有効時は pick_best でbestmoveを差し替える
        if skill_enabled && !worker.root_moves.is_empty() && effective_multi_pv > 0 {
            let mut rng = rand::rng();
            let best = skill.pick_best(&worker.root_moves, effective_multi_pv, &mut rng);
            if best != Move::NONE {
                worker.best_move = best;
            }
        }

        // 結果を収集
        let best_move = worker.best_move;
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

        // 次回のfallingEval計算のために平均スコアを保存
        if let Some(best_rm) = worker.root_moves.get(0) {
            self.best_previous_average_score = Some(best_rm.score);
        }
        self.last_game_ply = Some(ply);

        SearchResult {
            best_move,
            ponder_move,
            score: if worker.root_moves.is_empty() {
                Value::ZERO
            } else {
                worker
                    .root_moves
                    .iter()
                    .find(|rm| rm.mv() == best_move)
                    .map(|rm| rm.score)
                    .unwrap_or(worker.root_moves[0].score)
            },
            depth: worker.completed_depth,
            nodes: worker.nodes,
        }
    }

    /// コールバック付きで探索を実行
    fn search_with_callback<F>(
        &mut self,
        pos: &mut Position,
        worker: &mut SearchWorker,
        max_depth: Depth,
        mut on_info: F,
        skill_enabled: bool,
    ) -> usize
    where
        F: FnMut(&SearchInfo),
    {
        // ルート手を初期化
        worker.root_moves = super::RootMoves::from_legal_moves(pos, &worker.limits.search_moves);

        if worker.root_moves.is_empty() {
            worker.best_move = Move::NONE;
            return 0;
        }

        // 合法手が1つの場合は500ms上限を適用（YaneuraOu準拠）
        if worker.root_moves.len() == 1 {
            worker.time_manager.apply_single_move_limit();
        }

        let start = self.start_time.unwrap();
        let mut effective_multi_pv = worker.limits.multi_pv;
        if skill_enabled {
            effective_multi_pv = effective_multi_pv.max(4);
        }
        effective_multi_pv = effective_multi_pv.min(worker.root_moves.len());

        // 反復深化
        for depth in 1..=max_depth {
            if worker.abort {
                break;
            }

            // YaneuraOu準拠: depth 2以降は、次の深さを探索する時間があるかチェック
            // depth 1は必ず探索する（合法手が1つもない場合のresignを防ぐため）
            if depth > 1 && worker.time_manager.should_stop(depth) {
                break;
            }

            // YaneuraOu準拠: 詰みを読みきった場合の早期終了
            // 詰みまでの手数の2.5倍以上の深さを探索したら終了
            // MultiPV=1の時のみ適用（MultiPV>1では全候補を探索する必要がある）
            if effective_multi_pv == 1 && depth > 1 && !worker.root_moves.is_empty() {
                let best_value = worker.root_moves[0].score;

                // 勝ちを読みきっている場合
                if best_value.is_win() {
                    let mate_ply = best_value.mate_ply();
                    if (mate_ply + 2) * 5 / 2 < depth {
                        break;
                    }
                }

                // 詰まされる形の場合
                if best_value.is_loss() {
                    let mate_ply = best_value.mate_ply();
                    if (mate_ply + 2) * 5 / 2 < depth {
                        break;
                    }
                }
            }

            // ponderhitを検出した場合、時間再計算のみ行い探索は継続
            if self.ponderhit_flag.swap(false, Ordering::Relaxed) {
                worker.time_manager.on_ponderhit();
            }

            worker.root_depth = depth;
            worker.sel_depth = 0;

            // MultiPVループ（YaneuraOu準拠）
            let mut processed_pv = 0;
            for pv_idx in 0..effective_multi_pv {
                if worker.abort {
                    break;
                }

                // Aspiration Window（average/mean_squaredベース）
                let (mut alpha, mut beta, mut delta) =
                    compute_aspiration_window(&worker.root_moves[pv_idx]);

                // Aspiration Windowループ
                loop {
                    // pv_idx=0の場合は従来のsearch_rootを使用（後方互換性）
                    // pv_idx>0の場合のみsearch_root_for_pvを使用
                    let score = if pv_idx == 0 {
                        worker.search_root(pos, depth, alpha, beta)
                    } else {
                        worker.search_root_for_pv(pos, depth, alpha, beta, pv_idx)
                    };

                    if worker.abort {
                        break;
                    }

                    // Window調整
                    if score <= alpha {
                        beta = Value::new((alpha.raw() + beta.raw()) / 2);
                        alpha = Value::new(score.raw().saturating_sub(delta.raw()).max(-32001));
                    } else if score >= beta {
                        beta = Value::new(score.raw().saturating_add(delta.raw()).min(32001));
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
                let nps = if time_ms > 0 {
                    worker.nodes * 1000 / time_ms
                } else {
                    0
                };

                for pv_idx in 0..processed_pv {
                    let info = SearchInfo {
                        depth,
                        sel_depth: worker.root_moves[pv_idx].sel_depth,
                        score: worker.root_moves[pv_idx].score,
                        nodes: worker.nodes,
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
                worker.completed_depth = depth;
                worker.best_move = worker.root_moves[0].mv();

                // 🆕 YaneuraOu準拠: previous_scoreを次のiterationのためにシード
                // （YaneuraOu行1267-1270: rm.previousScore = rm.score）
                for rm in worker.root_moves.iter_mut() {
                    rm.previous_score = rm.score;
                }

                // 評価変動・timeReduction・最善手不安定性をまとめて適用（YaneuraOu準拠）
                let summary = WorkerSummary::from(&*worker);
                let (changes_sum, thread_count) =
                    aggregate_best_move_changes(&[summary.best_move_changes]);
                let tot_best_move_changes = self.tot_best_move_changes / 2.0 + changes_sum;
                let (falling_eval, time_reduction, tot_changes, threads) =
                    self.compute_time_factors(worker, tot_best_move_changes, thread_count);
                let total_time = worker.time_manager.total_time_for_iteration(
                    falling_eval,
                    time_reduction,
                    tot_changes,
                    threads,
                );

                // 実測 effort を正規化
                let nodes_effort =
                    normalize_nodes_effort(worker.root_moves[0].effort, worker.nodes);

                // 合法手が1つの場合は使う時間そのものを500msに丸める（YaneuraOu準拠）
                let total_time = if worker.root_moves.len() == 1 {
                    total_time.min(500.0)
                } else {
                    total_time
                };
                worker.time_manager.apply_iteration_timing(
                    worker.time_manager.elapsed(),
                    total_time,
                    nodes_effort,
                    worker.limits.ponder,
                    worker.completed_depth,
                );

                // best_move_changes は集約後リセット
                worker.best_move_changes = 0.0;

                // YaneuraOu準拠: 詰みスコアが見つかっていたら早期終了
                // MultiPV=1の時のみ適用
                if effective_multi_pv == 1 {
                    let best_value = worker.root_moves[0].score;

                    // 勝ちを読みきっている場合
                    if best_value.is_win() {
                        let mate_ply = best_value.mate_ply();
                        if (mate_ply + 2) * 5 / 2 < depth {
                            break;
                        }
                    }

                    // 詰まされる形の場合
                    if best_value.is_loss() {
                        let mate_ply = best_value.mate_ply();
                        if (mate_ply + 2) * 5 / 2 < depth {
                            break;
                        }
                    }
                }
            }
        }

        effective_multi_pv
    }
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
        // 簡易にSearchWorkerを初期化してサマリを取る
        let mut tm =
            TimeManagement::new(Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
        let mut limits = LimitsType::new();
        limits.set_start_time();
        let tt = TranspositionTable::new(16);
        let mut worker = SearchWorker::new(&tt, &limits, &mut tm);
        worker.best_move_changes = 3.5;

        let summary = WorkerSummary::from(&worker);
        assert!((summary.best_move_changes - 3.5).abs() < 1e-9, "best_move_changes should match");
    }

    #[test]
    fn test_prepare_time_metrics_resets_iter_state() {
        let mut search = Search::new(16);
        search.best_previous_average_score = Some(Value::new(123));
        search.last_game_ply = Some(5);
        search.iter_value = [Value::new(1), Value::new(2), Value::new(3), Value::new(4)];
        search.iter_idx = 2;
        search.last_best_move_depth = 5;
        search.tot_best_move_changes = 7.5;

        search.prepare_time_metrics(6);

        assert_eq!(search.best_previous_average_score, Some(Value::new(-123)));
        assert_eq!(search.iter_value, [Value::new(-123); 4]);
        assert_eq!(search.iter_idx, 0);
        assert_eq!(search.last_best_move_depth, 0);
        assert_eq!(search.tot_best_move_changes, 0.0);
        assert_eq!(search.last_game_ply, Some(6));
    }

    #[test]
    fn test_set_max_moves_to_draw_option() {
        let mut search = Search::new(16);
        search.set_max_moves_to_draw(512);
        assert_eq!(search.max_moves_to_draw(), 512);

        search.set_max_moves_to_draw(0);
        assert_eq!(search.max_moves_to_draw(), DEFAULT_MAX_MOVES_TO_DRAW);
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
}
