//! 単一 worker の policy/value Monte Carlo 木探索。

use std::mem::size_of;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Result, ensure};
use rshogi_core::movegen::{MAX_MOVES, MoveList, generate_legal_all};
use rshogi_core::position::Position;
use rshogi_core::types::{EnteringKingRule, Move};
use rshogi_dl::Evaluator;
use rshogi_dl::features::{MAX_MOVE_LABEL_NUM, make_move_label};

mod batch;
mod history;
pub use history::History;

/// 現在の手番から見た、ルールまたは木全体から確定した結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Win,
    Loss,
    Draw,
}

impl Outcome {
    fn value(self) -> f32 {
        match self {
            Self::Win => 1.0,
            Self::Loss => 0.0,
            Self::Draw => 0.5,
        }
    }
}

/// 探索前に固定する設定。
#[derive(Debug, Clone)]
pub struct Config {
    /// PUCT の事前確率に掛ける係数。
    pub cpuct: f32,
    /// 合法手の softmax temperature。
    pub temperature: f32,
    /// Node / edge arena に確保する上限 MiB。
    pub tree_memory_mb: usize,
    /// 同期推論へまとめる葉の上限。1 は従来の scalar 探索。
    pub batch_size: usize,
    /// 一回の simulation の最大深さ。
    pub max_depth: usize,
    /// 開始局面以前を含む総手数の引き分け上限。
    pub max_game_ply: i32,
    /// 入玉宣言のルール。
    pub entering_king_rule: EnteringKingRule,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            cpuct: 1.5,
            temperature: 1.0,
            tree_memory_mb: 16,
            batch_size: 1,
            max_depth: 128,
            max_game_ply: 512,
            entering_king_rule: EnteringKingRule::Point27,
        }
    }
}

/// 完了した simulation の上限と、探索開始後の時間上限。
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// 完了した simulation の最大回数。0 は推論せず合法手を返す。
    pub visits: u64,
    /// 推論の実行時間も含む wall clock 上限。実行中の forward は強制中断しない。
    pub time: Option<Duration>,
}

/// 一回の探索の結果。depth は PV 長であり αβ の反復深さではない。
#[derive(Debug, Clone)]
pub struct SearchResult {
    /// 最善手。合法手なしは NONE、宣言勝ちは WIN。内部の引き分けでは合法な fallback を返す。
    pub best_move: Move,
    /// 根の手番側の平均勝率。
    pub value: f32,
    /// 完了した simulation 数。
    pub visits: u64,
    /// evaluator に渡した局面数。batch も局面ごとに数え、backend の呼び出し数とは区別する。
    pub evaluations: u64,
    /// 根の確定結果。
    pub outcome: Option<Outcome>,
    /// 訪問数を優先した合法な読み筋。
    pub pv: Vec<Move>,
    /// arena の使用割合（千分率）。
    pub hashfull: u32,
    /// arena が満杯で打ち切ったか。
    pub memory_full: bool,
}

struct Node {
    first_edge: usize,
    edge_count: usize,
    expanded: bool,
    visits: u64,
    value_sum: f64,
    static_value: f32,
    outcome: Option<Outcome>,
    declaration_move: Move,
}

impl Node {
    fn new() -> Self {
        Self {
            first_edge: 0,
            edge_count: 0,
            expanded: false,
            visits: 0,
            value_sum: 0.0,
            static_value: 0.5,
            outcome: None,
            declaration_move: Move::NONE,
        }
    }
}

struct Edge {
    mv: Move,
    prior: f32,
    child: Option<usize>,
    visits: u64,
    value_sum: f64,
}

/// arena と推論用バッファを保持する探索器。各 go では木を再構築する。
pub struct Search {
    config: Config,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    node_limit: usize,
    edge_limit: usize,
    path: Vec<usize>,
    logits: Vec<f32>,
    evaluations: u64,
    batch: Option<batch::Workspace>,
}

impl Search {
    /// 設定を検証し、探索ループで使うメモリを確保する。
    pub fn new(config: Config) -> Result<Self> {
        ensure!(config.cpuct.is_finite() && config.cpuct > 0.0, "cpuct must be positive");
        ensure!(
            config.temperature.is_finite() && config.temperature > 0.0,
            "temperature must be positive"
        );
        ensure!((1..=1024).contains(&config.tree_memory_mb), "tree memory must be 1..=1024 MiB");
        ensure!((1..=512).contains(&config.max_depth), "depth must be 1..=512");
        ensure!((1..=1024).contains(&config.batch_size), "batch size must be 1..=1024");
        ensure!(config.max_game_ply > 0, "max game ply must be positive");
        let bytes = config.tree_memory_mb * 1024 * 1024;
        let node_limit = bytes / 4 / size_of::<Node>();
        let edge_limit = (bytes - node_limit * size_of::<Node>()) / size_of::<Edge>();
        let mut nodes = Vec::new();
        nodes.try_reserve_exact(node_limit)?;
        let mut edges = Vec::new();
        edges.try_reserve_exact(edge_limit)?;
        let mut path = Vec::new();
        path.try_reserve_exact(config.max_depth)?;
        Ok(Self {
            config,
            nodes,
            edges,
            node_limit,
            edge_limit,
            path,
            logits: vec![0.0; MAX_MOVE_LABEL_NUM],
            evaluations: 0,
            batch: None,
        })
    }

    /// batch 推論用に別途確保した buffer と予約 metadata の bytes。tree arena は含まない。
    pub fn batch_workspace_bytes(&self) -> usize {
        self.batch.as_ref().map_or(0, batch::Workspace::allocated_bytes)
    }

    /// 現在の根で実際に訪問した候補手と完了訪問数を返す。
    pub fn root_move_visits(&self) -> impl Iterator<Item = (Move, u64)> + '_ {
        self.nodes.first().into_iter().flat_map(|root| {
            self.edges[root.first_edge..root.first_edge + root.edge_count]
                .iter()
                .filter(|edge| edge.visits > 0)
                .map(|edge| (edge.mv, edge.visits))
        })
    }

    /// 履歴を維持して探索し、失敗や中断でも入力局面と履歴を復元する。
    pub fn run(
        &mut self,
        position: &mut Position,
        history: &mut History,
        evaluator: &mut impl Evaluator,
        limits: Limits,
        stop: &AtomicBool,
    ) -> Result<SearchResult> {
        ensure!(history.matches(position), "history does not end at the root position");
        position.try_reserve_move_history(self.config.max_depth)?;
        history.reserve(self.config.max_depth)?;
        self.nodes.clear();
        self.edges.clear();
        self.path.clear();
        self.evaluations = 0;
        self.nodes.push(Node::new());
        let mut legal = MoveList::new();
        generate_legal_all(position, &mut legal);
        self.set_terminal(0, position, history, legal.is_empty());
        let fallback = legal.as_slice().first().copied().unwrap_or(Move::NONE);
        let initial_history_len = history.len();
        let start = Instant::now();
        let mut memory_full = false;
        let batch = (self.config.batch_size > 1
            && limits.visits > 0
            && self.nodes[0].outcome.is_none()
            && !stop.load(Ordering::Relaxed)
            && !limits.time.is_some_and(|time| start.elapsed() >= time))
        .then(|| evaluator.encoded_batch_capacity().zip(evaluator.input_format()))
        .flatten()
        .filter(|(capacity, _)| *capacity > 1);
        if let Some((capacity, format)) = batch {
            let capacity = capacity.min(self.config.batch_size);
            let mut workspace = match self.batch.take() {
                Some(workspace) if workspace.matches(format, capacity) => workspace,
                _ => batch::Workspace::new(format, capacity, self)?,
            };
            let result = batch::run(
                self,
                position,
                history,
                evaluator,
                &mut workspace,
                batch::Control {
                    limits,
                    stop,
                    start,
                    history_len: initial_history_len,
                },
            );
            #[cfg(not(feature = "lazy-batch-metadata"))]
            {
                self.batch = Some(workspace);
            }
            // 部分確保で capacity と bytes が未同期になった workspace を公開しない。
            #[cfg(feature = "lazy-batch-metadata")]
            if result.is_ok() {
                self.batch = Some(workspace);
            }
            memory_full = result?;
        } else {
            while self.nodes[0].visits < limits.visits
                && self.nodes[0].outcome.is_none()
                && !stop.load(Ordering::Relaxed)
                && !limits.time.is_some_and(|t| start.elapsed() >= t)
            {
                self.path.clear();
                let simulation = self.simulate(position, history, evaluator);
                for &edge in self.path.iter().rev() {
                    position.undo_move(self.edges[edge].mv);
                }
                history.truncate(initial_history_len);
                if !simulation? {
                    memory_full = true;
                    break;
                }
            }
        }
        let root = &self.nodes[0];
        let mut pv = Vec::new();
        let mut index = 0;
        for _ in 0..self.config.max_depth {
            let Some(edge) = self.best_edge(index) else {
                break;
            };
            pv.push(self.edges[edge].mv);
            let Some(child) = self.edges[edge].child else {
                break;
            };
            index = child;
        }
        let best_move = if root.declaration_move != Move::NONE {
            root.declaration_move
        } else if root.outcome == Some(Outcome::Loss) && root.edge_count == 0 {
            Move::NONE
        } else {
            pv.first().copied().unwrap_or(fallback)
        };
        Ok(SearchResult {
            best_move,
            value: root.outcome.map(Outcome::value).unwrap_or_else(|| {
                if root.visits == 0 {
                    0.5
                } else {
                    (root.value_sum / root.visits as f64) as f32
                }
            }),
            visits: root.visits,
            evaluations: self.evaluations,
            outcome: root.outcome,
            pv,
            hashfull: ((self.edges.len() * 1000 / self.edge_limit)
                .max(self.nodes.len() * 1000 / self.node_limit)) as u32,
            memory_full,
        })
    }

    fn set_terminal(&mut self, index: usize, pos: &Position, history: &History, no_moves: bool) {
        let declaration_move = pos.declaration_win(self.config.entering_king_rule);
        let outcome = history.outcome().or_else(|| {
            if declaration_move != Move::NONE {
                Some(Outcome::Win)
            } else if no_moves {
                Some(Outcome::Loss)
            } else if pos.game_ply() > self.config.max_game_ply {
                Some(Outcome::Draw)
            } else {
                None
            }
        });
        self.nodes[index].outcome = outcome;
        self.nodes[index].declaration_move = if outcome == Some(Outcome::Win) {
            declaration_move
        } else {
            Move::NONE
        };
    }

    fn simulate(
        &mut self,
        pos: &mut Position,
        history: &mut History,
        evaluator: &mut impl Evaluator,
    ) -> Result<bool> {
        let mut index = 0;
        let value = loop {
            if let Some(outcome) = self.nodes[index].outcome {
                break outcome.value();
            }
            if !self.nodes[index].expanded {
                let mut legal = MoveList::new();
                generate_legal_all(pos, &mut legal);
                self.set_terminal(index, pos, history, legal.is_empty());
                if let Some(outcome) = self.nodes[index].outcome {
                    break outcome.value();
                }
                if self.edges.len() + legal.len() > self.edge_limit {
                    return Ok(false);
                }
                let value = evaluator.evaluate(pos, &mut self.logits)?;
                ensure!(
                    value.is_finite() && (0.0..=1.0).contains(&value),
                    "invalid value probability"
                );
                self.evaluations += 1;
                let mut priors = [0.0f32; MAX_MOVES];
                let priors = &mut priors[..legal.len()];
                for (prior, mv) in priors.iter_mut().zip(legal.iter()) {
                    *prior = self.logits[make_move_label(*mv, pos.side_to_move())];
                    ensure!(prior.is_finite(), "non-finite legal policy logit");
                }
                let max = priors.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                let mut sum = 0.0;
                for p in priors.iter_mut() {
                    *p = ((*p - max) / self.config.temperature).exp();
                    sum += *p;
                }
                let first_edge = self.edges.len();
                for (&mv, &prior) in legal.iter().zip(priors.iter()) {
                    self.edges.push(Edge {
                        mv,
                        prior: prior / sum,
                        child: None,
                        visits: 0,
                        value_sum: 0.0,
                    });
                }
                let node = &mut self.nodes[index];
                node.first_edge = first_edge;
                node.edge_count = legal.len();
                node.static_value = value;
                node.expanded = true;
                break value;
            }
            if self.path.len() == self.config.max_depth {
                break self.nodes[index].static_value;
            }
            let edge_index = self.select(index);
            if self.edges[edge_index].child.is_none() {
                if self.nodes.len() == self.node_limit {
                    return Ok(false);
                }
                self.edges[edge_index].child = Some(self.nodes.len());
                self.nodes.push(Node::new());
            }
            let mv = self.edges[edge_index].mv;
            pos.do_move(mv, pos.gives_check(mv));
            history.push(pos);
            self.path.push(edge_index);
            index = self.edges[edge_index].child.expect("child was initialized");
        };
        let node = &mut self.nodes[index];
        node.visits += 1;
        node.value_sum += f64::from(value);
        let mut value = value;
        for path_index in (0..self.path.len()).rev() {
            value = 1.0 - value;
            let edge_index = self.path[path_index];
            self.edges[edge_index].visits += 1;
            self.edges[edge_index].value_sum += f64::from(value);
            let parent = if path_index == 0 {
                0
            } else {
                self.edges[self.path[path_index - 1]].child.expect("path has a child")
            };
            self.nodes[parent].visits += 1;
            self.nodes[parent].value_sum += f64::from(value);
            self.propagate_outcome(parent);
        }
        Ok(true)
    }

    fn select(&self, index: usize) -> usize {
        let node = &self.nodes[index];
        let scale = self.config.cpuct * (node.visits.max(1) as f64).sqrt() as f32;
        let mut selected = node.first_edge;
        let mut maximum = f32::NEG_INFINITY;
        for edge_index in node.first_edge..node.first_edge + node.edge_count {
            let edge = &self.edges[edge_index];
            if edge.child.is_some_and(|c| self.nodes[c].outcome == Some(Outcome::Loss)) {
                return edge_index;
            }
            if edge.child.is_some_and(|c| self.nodes[c].outcome == Some(Outcome::Win)) {
                continue;
            }
            let q = if edge.visits == 0 {
                0.5
            } else {
                (edge.value_sum / edge.visits as f64) as f32
            };
            let score = q + scale * edge.prior / (1.0 + edge.visits as f32);
            if score > maximum {
                maximum = score;
                selected = edge_index;
            }
        }
        selected
    }

    fn propagate_outcome(&mut self, index: usize) {
        let node = &self.nodes[index];
        if !node.expanded || node.edge_count == 0 {
            return;
        }
        let mut all_known = true;
        let mut has_draw = false;
        for edge in &self.edges[node.first_edge..node.first_edge + node.edge_count] {
            match edge.child.and_then(|c| self.nodes[c].outcome) {
                Some(Outcome::Loss) => {
                    self.nodes[index].outcome = Some(Outcome::Win);
                    return;
                }
                Some(Outcome::Draw) => has_draw = true,
                Some(Outcome::Win) => {}
                None => all_known = false,
            }
        }
        if all_known {
            self.nodes[index].outcome = Some(if has_draw {
                Outcome::Draw
            } else {
                Outcome::Loss
            });
        }
    }

    fn best_edge(&self, index: usize) -> Option<usize> {
        let node = &self.nodes[index];
        let mut best = None;
        let mut key = (0, 0, f32::NEG_INFINITY);
        for i in node.first_edge..node.first_edge + node.edge_count {
            let edge = &self.edges[i];
            let rank = match edge.child.and_then(|c| self.nodes[c].outcome) {
                Some(Outcome::Loss) => 3,
                Some(Outcome::Win) => 0,
                Some(Outcome::Draw) => 1,
                None => 1,
            };
            let candidate = (rank, edge.visits, edge.prior);
            if best.is_none() || candidate > key {
                best = Some(i);
                key = candidate;
            }
        }
        best
    }
}

#[cfg(test)]
mod tests;
