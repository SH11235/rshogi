//! 局面解析の実行単位と、世代を検査する協調停止。

use super::{LimitsType, Search, SearchInfo, SearchResult};
use crate::{position::Position, time::Instant};
use std::sync::{
    Arc,
    atomic::{AtomicU32, AtomicU64, Ordering},
};

/// 1 回の実行で追加する探索予算。停止中の時間は含まない。
#[derive(Clone, Copy, Debug)]
pub enum AnalysisBudget {
    /// 明示的な停止まで探索する。
    Unlimited,
    /// 追加のミリ秒数（1 以上）。
    Time(u64),
    /// 全 worker 合計の追加ノード数（1 以上）。
    Nodes(u64),
    /// 指定の絶対深さまで探索する（デバッグ・検証用）。
    Depth(i32),
}

/// 解析の準備・再開を拒否した理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalysisError {
    /// 予算または MultiPV が不正。
    InvalidOptions,
    /// 前回準備した実行が未消費。
    RunPending,
    /// 別セッション、古い実行、または消費済み実行。
    InvalidRun,
    /// 世代を使い切った。新しいセッションが必要。
    GenerationExhausted,
    /// 評価関数が変更された。
    EvaluationChanged,
    /// 終端局面・最大深さのため継続できない。
    NoContinuation,
    /// 探索が panic で中断され、状態を破棄する必要がある。
    RunFailed,
}
impl std::fmt::Display for AnalysisError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for AnalysisError {}

/// 実行に固定された停止ハンドル。古いハンドルでは新しい実行を停止できない。
#[derive(Clone, Debug)]
pub struct AnalysisRunHandle {
    control: Arc<AtomicU32>,
    generation: u32,
}
impl AnalysisRunHandle {
    /// 停止を要求する。既に停止済みなら成功、世代が異なるなら false。
    pub fn pause(&self) -> bool {
        match self.control.compare_exchange(
            self.generation,
            self.generation | 1,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => true,
            Err(value) => value == self.generation | 1,
        }
    }
    /// 実行識別子（停止ビットを含まない偶数値）。
    pub fn run_id(&self) -> u32 {
        self.generation
    }
    /// Wasm host が操作する専用 AtomicU32 のバイト位置。
    ///
    /// このハンドルの生存中だけ有効。共有 memory 上で Uint32Array と Atomics.compareExchange
    /// を使い、run_id から run_id | 1 への CAS のみ行う。memory.grow 後は view を作り直す。
    #[cfg(target_arch = "wasm32")]
    pub fn control_address(&self) -> usize {
        Arc::as_ptr(&self.control) as usize
    }
    fn stopped(&self) -> bool {
        self.control.load(Ordering::Acquire) != self.generation
    }
}

#[derive(Clone)]
pub(crate) struct AnalysisRun {
    pub handle: AnalysisRunHandle,
    pub resume: bool,
    pub budget: AnalysisBudget,
    pub nodes: Arc<AtomicU64>,
    pub start: Instant,
}
impl AnalysisRun {
    pub fn stopped(&self) -> bool {
        self.handle.stopped()
            || match self.budget {
                AnalysisBudget::Time(ms) => self.start.elapsed().as_millis() >= u128::from(ms),
                AnalysisBudget::Nodes(n) => self.nodes.load(Ordering::Relaxed) >= n,
                _ => false,
            }
    }
    pub fn publish_nodes(&self, delta: u64) {
        self.nodes.fetch_add(delta, Ordering::Relaxed);
    }
}

/// 解析実行が終了した理由。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalysisEndReason {
    /// 外部からの停止。
    Paused,
    /// 今回の時間またはノード予算に到達。
    BudgetReached,
    /// 指定した絶対深さに到達。
    DepthReached,
    /// 合法手なし・宣言勝ち・読み切り等で探索終了。
    Terminal,
}

/// 解析実行の終了結果。info の再発行ではなく、前回を含む完了反復を返す。
pub struct AnalysisFinished {
    /// 論理 worker ID 順（main は 0）の実行前・実行後の完了深さ。
    pub worker_depths: Vec<(i32, i32)>,
    /// 停止理由。
    pub reason: AnalysisEndReason,
    /// 同じセッションから継続可能か。
    pub resumable: bool,
    /// 今回の実行 ID。
    pub run_id: u32,
    /// main の完了反復と全 worker の今回ノード数。
    pub result: SearchResult,
    /// 今回の実行時間。
    pub time_ms: u64,
    /// 停止時間を除く累積実行時間。
    pub total_time_ms: u64,
    /// 全実行の累積ノード数。
    pub total_nodes: u64,
}

/// 局面履歴と設定を固定して 1 つの探索状態を所有する。
///
/// 設定変更は新しいセッションで行う。評価関数の変更も再開を失効させる。
/// 評価関数のロードと探索は、従来同様 host が排他制御する必要がある。
pub struct AnalysisSession {
    search: Search,
    position: Position,
    multi_pv: usize,
    control: Arc<AtomicU32>,
    pending: Option<(AnalysisRunHandle, AnalysisBudget)>,
    started: bool,
    unavailable: Option<AnalysisError>,
    evaluation_revision: u64,
    total_time_ms: u64,
    total_nodes: u64,
}
impl AnalysisSession {
    /// 設定済み探索器と局面の所有権を受け取る。解析では手加減を無効にする。
    pub fn new(
        mut search: Search,
        position: Position,
        multi_pv: usize,
    ) -> Result<Self, AnalysisError> {
        if multi_pv == 0 {
            return Err(AnalysisError::InvalidOptions);
        }
        search.clear_tt();
        search.clear_eval_hash();
        search.clear_histories();
        search.set_skill_options(super::SkillOptions::default());
        Ok(Self {
            search,
            position,
            multi_pv,
            control: Arc::new(AtomicU32::new(0)),
            pending: None,
            started: false,
            unavailable: None,
            evaluation_revision: crate::eval::revision(),
            total_time_ms: 0,
            total_nodes: 0,
        })
    }
    /// 状態を解放して探索器を返す。保持した停止ハンドルはこのセッションにしか作用しない。
    pub fn into_search(self) -> Search {
        self.control.fetch_or(1, Ordering::AcqRel);
        self.search
    }

    /// 実行を公開する前に世代を準備する。返却後の停止要求は run 開始時にも保持する。
    pub fn prepare(&mut self, budget: AnalysisBudget) -> Result<AnalysisRunHandle, AnalysisError> {
        if let Some(error) = self.unavailable {
            return Err(error);
        }
        if self.pending.is_some() {
            return Err(AnalysisError::RunPending);
        }
        if self.evaluation_revision != crate::eval::revision() {
            return Err(AnalysisError::EvaluationChanged);
        }
        match budget {
            AnalysisBudget::Time(0) | AnalysisBudget::Nodes(0) => {
                return Err(AnalysisError::InvalidOptions);
            }
            AnalysisBudget::Depth(d) if !(1..=crate::types::MAX_PLY).contains(&d) => {
                return Err(AnalysisError::InvalidOptions);
            }
            _ => (),
        }
        let previous = self.control.load(Ordering::Acquire) & !1;
        let generation = previous.checked_add(2).ok_or(AnalysisError::GenerationExhausted)?;
        self.search.reset_flags();
        self.control.store(generation, Ordering::Release);
        let handle = AnalysisRunHandle {
            control: Arc::clone(&self.control),
            generation,
        };
        self.pending = Some((handle.clone(), budget));
        Ok(handle)
    }
    /// 準備済みの実行を同期的に探索する。全 worker の停止後に返る。
    pub fn run<F: FnMut(&SearchInfo)>(
        &mut self,
        handle: &AnalysisRunHandle,
        on_info: Option<F>,
    ) -> Result<AnalysisFinished, AnalysisError> {
        let Some((pending, budget)) = self.pending.as_ref() else {
            return Err(AnalysisError::InvalidRun);
        };
        if !Arc::ptr_eq(&pending.control, &handle.control)
            || pending.generation != handle.generation
        {
            return Err(AnalysisError::InvalidRun);
        }
        if self.evaluation_revision != crate::eval::revision() {
            self.pending = None;
            return Err(AnalysisError::EvaluationChanged);
        }
        let budget = *budget;
        self.pending = None;
        let previous_depths = self.search.analysis_worker_depths();
        let start = Instant::now();
        let run = AnalysisRun {
            handle: handle.clone(),
            resume: self.started,
            budget,
            nodes: Arc::new(AtomicU64::new(0)),
            start,
        };
        let mut limits = LimitsType::new();
        limits.multi_pv = self.multi_pv;
        // 対局の時間配分・深さ liveness guard を使わず、共有実行予算で止める。
        limits.analysis = Some(run.clone());
        if let AnalysisBudget::Depth(depth) = budget {
            limits.depth = depth;
        }
        // native unwind では全 helper を止めてから失効する。Wasm abort は host が Worker 群を破棄する。
        let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.search.go(&mut self.position, limits, on_info)
        })) {
            Ok(result) => result,
            Err(_) => {
                handle.pause();
                self.search.stop_and_wait();
                self.unavailable = Some(AnalysisError::RunFailed);
                return Err(AnalysisError::RunFailed);
            }
        };
        let reason = if result.best_move == crate::types::Move::NONE || result.best_move.is_win() {
            AnalysisEndReason::Terminal
        } else if handle.stopped() {
            AnalysisEndReason::Paused
        } else if run.stopped() {
            AnalysisEndReason::BudgetReached
        } else if matches!(budget, AnalysisBudget::Depth(depth) if result.depth >= depth) {
            AnalysisEndReason::DepthReached
        } else {
            AnalysisEndReason::Terminal
        };
        let resumable =
            reason != AnalysisEndReason::Terminal && result.depth < crate::types::MAX_PLY;
        if !resumable {
            self.unavailable = Some(AnalysisError::NoContinuation);
        }

        self.started = true;
        let time_ms = start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        self.total_time_ms = self.total_time_ms.saturating_add(time_ms);
        self.total_nodes = self.total_nodes.saturating_add(result.nodes);
        let worker_depths =
            previous_depths.into_iter().zip(self.search.analysis_worker_depths()).collect();
        Ok(AnalysisFinished {
            worker_depths,
            reason,
            resumable,
            run_id: handle.run_id(),
            result,
            time_ms,
            total_time_ms: self.total_time_ms,
            total_nodes: self.total_nodes,
        })
    }
}
