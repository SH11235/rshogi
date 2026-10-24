//! df-pn 詰み探索
//!
//! 手番側 (攻め方) が王手を続けて相手玉を詰ませられるかを、証明数・反証数を使う
//! 深さ優先探索 (df-pn) で判定する。
//!
//! 問題の定義: 与えられた局面から始め、それ以前の対局履歴は見ない。攻め方の手はすべて王手で、
//! 受け方は合法手 (合駒・不成を含む) をすべて考える。打ち歩詰めの歩打ちは攻め方の手にならない。
//! 探索中に同じ局面へ戻る手順は (連続王手の千日手を含め) 攻め方の失敗として扱う。
//!
//! 打ち切り (時間・ノード数・停止) は必ず [`MateResult::Unknown`] になり、不詰と混ざらない。
//! 探索深さの上限 (2048 手、wasm では 1024 手) に達して読み切れず不詰と言えないとき、詰みを証明したが
//! 手順を復元できなかったときも Unknown を返す。詰みの手順は最短とは限らない。
//! 局面の同一性は 64bit のハッシュキーで判定するので、衝突による誤りの可能性は確率的にのみ排除される。
//! 玉の無い局面 (攻め方の玉が無い詰将棋など) と、手番でない側が王手されている局面は扱わない
//! ([`MateResult::Unsupported`])。

mod pv;
mod search;
mod state;
mod table;

#[cfg(test)]
mod tests;

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use search::{Budget, NO_LEN_LIMIT, NodeArgs, Scratch, Searcher};
use table::MateTable;

use crate::movegen::{MoveList, generate_legal_all};
use crate::position::Position;
use crate::time::Instant;
use crate::types::{Color, Move, PieceType};

/// 詰み探索の結果
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MateResult {
    /// 詰み。攻め方の手から始まる詰み手順 (最後の手で受け方が詰む)。
    Mate(Vec<Move>),
    /// 不詰 (攻め方が王手を続けて詰ませる手順は無い)
    NoMate,
    /// 判定できなかった (時間・ノード数・停止による打ち切り、探索深さの上限、手順の復元失敗)
    Unknown,
    /// 扱えない局面 (どちらかの玉が無い、手番でない側が王手されている)
    Unsupported,
}

/// 探索の制限。0 は無制限。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MateLimits {
    /// 探索ノード数の上限
    pub nodes: u64,
    /// 探索時間の上限 (ミリ秒)。詰み手順の復元にかかる時間も含む。
    pub time_ms: u64,
}

/// 探索中の進捗
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MateProgress {
    /// 探索したノード数
    pub nodes: u64,
    /// 探索開始からの経過時間 (ミリ秒)
    pub elapsed_ms: u64,
}

/// 詰み探索の進め方
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MateMode {
    /// 速さ優先 (不詰の多い詰めろ判定向け)。置換表は局面 (盤面と持駒) が一致するときだけ使い、
    /// 子の pn/dn の初期値も見積もらず、攻め方の局面の dn は子の和で数える。1 ノードあたりが軽く、
    /// 不詰の判定が速い。
    Speed,
    /// 解く力優先。詰み・不詰を証明駒・反証駒で置換表に書き、持駒だけ違う局面に流用する。
    /// 子の pn/dn の初期値を局面から見積もり (df-pn+)、攻め方の局面の dn を WPNS で数える。
    /// 詰む問題・長手数・難問でノードが減り、少ない置換表で解ける問題が増えるが、1 ノードあたりは重く、
    /// 不詰の判定ではノードが増える。
    Power,
    /// 証明駒を使わない探索で [`AUTO_SWITCH_NODES`] ノードまで読み、決まらなければ同じ置換表のまま
    /// 解く力優先で読み続ける (ノード数・時間の制限は合計に掛かる)。初期値の見積もりと WPNS はどちらの段階でも使う。
    /// 置換表の大きさは解く力優先と同じ配分。
    #[default]
    Auto,
}

/// [`MateMode::Auto`] で解く力優先に切り替えるノード数
pub const AUTO_SWITCH_NODES: u64 = 100_000;

impl MateMode {
    /// すべてのモード ([`MateMode::name`] の一覧に使う)
    pub const ALL: [MateMode; 3] = [MateMode::Speed, MateMode::Power, MateMode::Auto];

    /// USI オプションなどで使う名前 (`Speed` / `Power` / `Auto`)
    pub fn name(self) -> &'static str {
        match self {
            MateMode::Speed => "Speed",
            MateMode::Power => "Power",
            MateMode::Auto => "Auto",
        }
    }

    /// [`MateMode::name`] の名前から (大文字・小文字は区別しない)
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.name().eq_ignore_ascii_case(name))
    }

    fn proof_pieces(self) -> bool {
        self != MateMode::Speed
    }
}

/// 置換表を持ち回る df-pn ソルバー
pub struct DfpnSolver {
    table: MateTable,
    scratch: Scratch<Move>,
    mode: MateMode,
    nodes: u64,
    /// 探索するスレッド数 (1 なら呼び出したスレッドだけで解く)
    threads: usize,
    /// 2 番目以降のスレッドの作業領域
    helper_scratch: Vec<Scratch<Move>>,
    /// 並列に解くとき、最初に 1 スレッドで解くノード数
    parallel_start: u64,
}

/// 並列に解くとき、最初にこのノード数までは 1 スレッドで解く
const PARALLEL_START_NODES: u64 = 10_000;

/// 2 番目以降のスレッドの 1+ε の余裕 (2 番目の子の φ に掛ける分数)。1 番目は既定の 1/3。
/// 局面ごとのノード数は探索の経路で大きく変わるので、少しずらしたスレッドどうしで置換表を共有すると、
/// 先に解けたスレッドの結論を使える
const HELPER_SLACKS: [(u64, u64); 7] = [(1, 4), (2, 5), (1, 5), (3, 10), (3, 8), (2, 7), (9, 20)];

impl DfpnSolver {
    /// 置換表を `hash_mb` MB で確保した、既定のモード ([`MateMode::Auto`]) のソルバーを作る
    pub fn new(hash_mb: usize) -> Self {
        Self::with_mode(hash_mb, MateMode::default())
    }

    /// 置換表を `hash_mb` MB で確保した、モード `mode` のソルバーを作る
    pub fn with_mode(hash_mb: usize, mode: MateMode) -> Self {
        Self {
            table: MateTable::new(hash_mb, mode.proof_pieces()),
            scratch: Scratch::new(),
            mode,
            nodes: 0,
            threads: 1,
            helper_scratch: Vec::new(),
            parallel_start: PARALLEL_START_NODES,
        }
    }

    /// 探索するスレッド数を設定する (最低 1)。置換表は全スレッドで共有する。
    ///
    /// スレッドの無い wasm (`wasm-threads` なし) では常に 1。`wasm-threads` では補助スレッドを rayon の
    /// スレッドプールで動かすので、プールを初期化 (`initThreadPool`) した Worker から solve を呼ぶ。
    /// プールが埋まっていると補助スレッドは空くまで動かない (根の結論が出ていれば動き始めてすぐ止まる)。
    ///
    /// 2 スレッド以上では、ノード数の上限を全スレッドの合計に掛けるが、合計は一定間隔でしか数えないので
    /// 上限をスレッド数 × 1024 ノードまで超えることがある。ノード数は決定的でなくなる。
    pub fn set_threads(&mut self, threads: usize) {
        self.threads = if cfg!(all(target_arch = "wasm32", not(feature = "wasm-threads"))) {
            1
        } else {
            threads.max(1)
        };
    }

    /// 並列の経路を小さな局面でも通すため、最初から複数スレッドで解く
    #[cfg(test)]
    fn parallel_from_the_start(&mut self) {
        self.parallel_start = 0;
    }

    /// 探索のモード
    pub fn mode(&self) -> MateMode {
        self.mode
    }

    /// 直前の `solve` で探索したノード数 (詰み手順の復元を含む)
    pub fn nodes(&self) -> u64 {
        self.nodes
    }

    /// 局面 `pos` の手番側が詰ませられるかを判定する。
    ///
    /// `stop` が立つか制限に達すると [`MateResult::Unknown`] を返す (呼び出し時に立っていても同じ)。
    /// 約 1 秒ごとに `progress` を呼ぶ。`pos` は呼び出し前の局面に戻して返る。
    pub fn solve(
        &mut self,
        pos: &mut Position,
        limits: &MateLimits,
        stop: &AtomicBool,
        mut progress: impl FnMut(MateProgress),
    ) -> MateResult {
        self.nodes = 0;
        // 玉が無い側の玉の位置は既定値のままで、王手の判定が意味をなさない。
        // 手番でない側が王手されている局面は将棋として成立せず、受け方の手番の前提が崩れる。
        let us = pos.side_to_move();
        if [Color::Black, Color::White]
            .iter()
            .any(|&c| pos.pieces(c, PieceType::King).is_empty())
            || !pos.attackers_to_c(pos.king_square(!us), us).is_empty()
        {
            return MateResult::Unsupported;
        }
        self.table.clear();
        let start = Instant::now();
        let result = match self.mode {
            MateMode::Speed => self.run::<false>(pos, *limits, start, 0, stop, &mut progress),
            MateMode::Power => self.run::<true>(pos, *limits, start, 0, stop, &mut progress),
            MateMode::Auto => {
                let first = MateLimits {
                    nodes: match limits.nodes {
                        0 => AUTO_SWITCH_NODES,
                        n => n.min(AUTO_SWITCH_NODES),
                    },
                    ..*limits
                };
                let decided = self.run::<false>(pos, first, start, 0, stop, &mut progress);
                if decided.is_some() || exhausted(limits, start, self.nodes, stop) {
                    decided
                } else {
                    // 証明駒を使わない段階の置換表の値はそのまま使える
                    self.run::<true>(pos, *limits, start, self.nodes, stop, &mut progress)
                }
            }
        };
        match result {
            Some(SolveOutcome::Mate(pv)) if is_valid_mate_pv(pos, &pv) => MateResult::Mate(pv),
            Some(SolveOutcome::Mate(_)) => {
                debug_assert!(false, "復元した詰み手順が検証を通らない: {}", pos.to_sfen());
                MateResult::Unknown
            }
            Some(SolveOutcome::NoMate) => MateResult::NoMate,
            None => MateResult::Unknown,
        }
    }
}

impl DfpnSolver {
    /// `nodes` ノード読んだ続きとして解く。制限 (`limits`、`start` からの時間) は合計に掛かる。
    fn run<const PIECES: bool>(
        &mut self,
        pos: &mut Position,
        limits: MateLimits,
        start: Instant,
        nodes: u64,
        stop: &AtomicBool,
        progress: &mut dyn FnMut(MateProgress),
    ) -> Option<SolveOutcome<Move>> {
        if self.threads > 1 && nodes < self.parallel_start {
            // 易しい局面はスレッドを起こす費用の方が大きいので、先に 1 スレッドで解く
            let first = MateLimits {
                nodes: match limits.nodes {
                    0 => self.parallel_start,
                    n => n.min(self.parallel_start),
                },
                ..limits
            };
            let decided = self.run_single::<PIECES>(pos, first, start, nodes, stop, progress);
            if decided.is_some() || exhausted(&limits, start, self.nodes, stop) {
                return decided;
            }
            let nodes = self.nodes;
            return self.run_parallel::<PIECES>(pos, limits, start, nodes, stop, progress);
        }
        if self.threads > 1 {
            return self.run_parallel::<PIECES>(pos, limits, start, nodes, stop, progress);
        }
        self.run_single::<PIECES>(pos, limits, start, nodes, stop, progress)
    }

    fn run_single<const PIECES: bool>(
        &mut self,
        pos: &mut Position,
        limits: MateLimits,
        start: Instant,
        nodes: u64,
        stop: &AtomicBool,
        progress: &mut dyn FnMut(MateProgress),
    ) -> Option<SolveOutcome<Move>> {
        let budget = Budget {
            stop,
            halt: None,
            cancel: None,
            shared_nodes: None,
            node_limit: limits.nodes,
            time_limit_ms: limits.time_ms,
            start,
            progress,
        };
        let attacker = pos.side_to_move();
        let mut searcher =
            Searcher::<_, PIECES>::new(&self.table, &mut self.scratch, attacker, budget);
        searcher.continue_from(nodes);
        configure(&mut searcher, self.mode);
        let result = solve_with(&mut searcher, pos);
        self.nodes = searcher.nodes();
        result
    }

    /// 置換表を共有して `threads` 本のスレッドで同じ局面を解く。どれかのスレッドが根の結論を出したら
    /// ほかを止め、呼び出したスレッドの結果を、無ければ補助スレッドの結果を返す (どれも同じ結論になる)。
    /// ノード数の上限は全スレッドの合計に掛ける。
    fn run_parallel<const PIECES: bool>(
        &mut self,
        pos: &mut Position,
        limits: MateLimits,
        start: Instant,
        nodes: u64,
        stop: &AtomicBool,
        progress: &mut dyn FnMut(MateProgress),
    ) -> Option<SolveOutcome<Move>> {
        let threads = self.threads;
        self.helper_scratch.resize_with(threads - 1, Scratch::new);
        self.table.set_concurrent(true);
        let halt = AtomicBool::new(false);
        let cancel = AtomicBool::new(false);
        let shared_nodes = AtomicU64::new(nodes);
        let attacker = pos.side_to_move();
        let (table, scratch, helper_scratch, mode) =
            (&self.table, &mut self.scratch, &mut self.helper_scratch, self.mode);
        let slots: Vec<OnceLock<HelperResult>> =
            (0..threads - 1).map(|_| OnceLock::new()).collect();
        let helpers: Vec<Box<dyn FnOnce() + Send + '_>> = helper_scratch
            .iter_mut()
            .zip(&slots)
            .enumerate()
            .map(|(i, (scratch, slot))| {
                let mut pos = pos.clone();
                let (halt, cancel, shared_nodes) = (&halt, &cancel, &shared_nodes);
                Box::new(move || {
                    // panic しても結果の欄を埋め、ほかのスレッドを止める (呼び出しスレッドは結果を待つので、
                    // 埋めないと待ち続けて panic が伝わらない)
                    struct Done<'a>(&'a OnceLock<HelperResult>, &'a AtomicBool);
                    impl Drop for Done<'_> {
                        fn drop(&mut self) {
                            if self.0.get().is_none() {
                                self.1.store(true, Ordering::Relaxed);
                                let _ = self.0.set((None, 0));
                            }
                        }
                    }
                    let _done = Done(slot, cancel);
                    let mut quiet = |_| {};
                    let budget = Budget {
                        stop,
                        halt: Some(halt),
                        cancel: Some(cancel),
                        shared_nodes: Some((shared_nodes, limits.nodes)),
                        node_limit: 0,
                        time_limit_ms: limits.time_ms,
                        start,
                        progress: &mut quiet,
                    };
                    let mut searcher = Searcher::<_, PIECES>::new(table, scratch, attacker, budget);
                    configure(&mut searcher, mode);
                    let (num, den) = HELPER_SLACKS[i % HELPER_SLACKS.len()];
                    searcher.set_threshold_slack(num, den);
                    let result = solve_with(&mut searcher, &mut pos);
                    let _ = slot.set((result, searcher.nodes()));
                }) as Box<dyn FnOnce() + Send + '_>
            })
            .collect();
        let (mut result, mut total) = run_with_helpers(helpers, &cancel, || {
            let budget = Budget {
                stop,
                halt: Some(&halt),
                cancel: Some(&cancel),
                shared_nodes: Some((&shared_nodes, limits.nodes)),
                node_limit: 0,
                time_limit_ms: limits.time_ms,
                start,
                progress: &mut *progress,
            };
            let mut searcher = Searcher::<_, PIECES>::new(table, scratch, attacker, budget);
            configure(&mut searcher, mode);
            let result = solve_with(&mut searcher, pos);
            let nodes = searcher.nodes();
            wait_for_helpers(&slots, &shared_nodes, start, progress);
            (result, nodes)
        });
        self.table.set_concurrent(false);
        for slot in slots {
            let (r, n) = slot.into_inner().expect("補助スレッドは結果を書いてから終わる");
            total += n;
            debug_assert!(
                !matches!(
                    (&result, &r),
                    (Some(SolveOutcome::Mate(_)), Some(SolveOutcome::NoMate))
                        | (Some(SolveOutcome::NoMate), Some(SolveOutcome::Mate(_)))
                ),
                "スレッドによって詰みと不詰が食い違う: {}",
                pos.to_sfen()
            );
            if result.is_none() {
                result = r;
            }
        }
        self.nodes = nodes + total;
        result
    }
}

type HelperResult = (Option<SolveOutcome<Move>>, u64);

/// 補助スレッドのスタック。探索は手数ごとに再帰するので、既定 (2 MB) では探索深さの上限まで潜れない
/// (debug ビルドは 1 手あたりの使用量が数倍になる)。確保するのは仮想領域で、使った分だけが実メモリになる。
#[cfg(not(target_arch = "wasm32"))]
const HELPER_STACK_SIZE: usize = 256 * 1024 * 1024;

/// 呼び出したスレッドが panic で抜けるとき (補助の起動の失敗も含む) に、補助スレッドを止める。
/// 止めないと、scope の終わりで補助の終わりを待ち続けて panic が伝わらない
struct CancelOnPanic<'a>(&'a AtomicBool);

impl Drop for CancelOnPanic<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.store(true, Ordering::Relaxed);
        }
    }
}

/// 補助スレッドが先に根を解いて詰み手順を復元している間も、進捗は呼び出したスレッドから知らせる
#[cfg(not(target_arch = "wasm32"))]
fn wait_for_helpers(
    slots: &[OnceLock<HelperResult>],
    shared_nodes: &AtomicU64,
    start: Instant,
    progress: &mut dyn FnMut(MateProgress),
) {
    use search::PROGRESS_INTERVAL_MS;
    let mut next_progress = start.elapsed().as_millis() as u64 + PROGRESS_INTERVAL_MS;
    while slots.iter().any(|s| s.get().is_none()) {
        let elapsed_ms = start.elapsed().as_millis() as u64;
        if elapsed_ms >= next_progress {
            next_progress = elapsed_ms + PROGRESS_INTERVAL_MS;
            progress(MateProgress {
                nodes: shared_nodes.load(Ordering::Relaxed),
                elapsed_ms,
            });
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// wasm では待たずに scope の終わりに任せる。呼び出したスレッドが rayon のプールのスレッドだと、補助の仕事が
/// 自分の deque に積まれていて、ここで待つと誰も実行しない。scope の終わりの待ちは自分で実行する。
/// その代わり、補助が詰み手順を復元している間は進捗を知らせない。
#[cfg(target_arch = "wasm32")]
fn wait_for_helpers(
    _slots: &[OnceLock<HelperResult>],
    _shared_nodes: &AtomicU64,
    _start: Instant,
    _progress: &mut dyn FnMut(MateProgress),
) {
}

/// `helpers` を別のスレッドで動かしながら `main` を呼び出したスレッドで動かし、すべて終わるのを待つ
#[cfg(not(target_arch = "wasm32"))]
fn run_with_helpers<'a, R>(
    helpers: Vec<Box<dyn FnOnce() + Send + 'a>>,
    cancel: &AtomicBool,
    main: impl FnOnce() -> R,
) -> R {
    std::thread::scope(|s| {
        let _cancel_on_panic = CancelOnPanic(cancel);
        for h in helpers {
            std::thread::Builder::new()
                .stack_size(HELPER_STACK_SIZE)
                .spawn_scoped(s, h)
                .expect("詰み探索の補助スレッドを起動できない");
        }
        main()
    })
}

/// wasm では Web Worker のスレッドプール (rayon) で動かす。`main` は呼び出したスレッドのまま動かす
/// (進捗の通知は呼び出したスレッドでしか呼べない)。呼び出したスレッドは補助の終わりを待ってブロックするので、
/// ブラウザのメインスレッドからは呼べない (Worker から呼ぶ)。
#[cfg(all(target_arch = "wasm32", feature = "wasm-threads"))]
fn run_with_helpers<'a, R>(
    helpers: Vec<Box<dyn FnOnce() + Send + 'a>>,
    cancel: &AtomicBool,
    main: impl FnOnce() -> R,
) -> R {
    rayon::in_place_scope(|s| {
        let _cancel_on_panic = CancelOnPanic(cancel);
        for h in helpers {
            s.spawn(move |_| h());
        }
        main()
    })
}

/// スレッドの無い wasm では補助スレッドを使わない (`set_threads` で 1 に抑えてある)
#[cfg(all(target_arch = "wasm32", not(feature = "wasm-threads")))]
fn run_with_helpers<'a, R>(
    helpers: Vec<Box<dyn FnOnce() + Send + 'a>>,
    cancel: &AtomicBool,
    main: impl FnOnce() -> R,
) -> R {
    debug_assert!(helpers.is_empty());
    let _cancel_on_panic = CancelOnPanic(cancel);
    main()
}

/// 制限 (停止フラグ、ノード数、時間) を使い切ったか
fn exhausted(limits: &MateLimits, start: Instant, nodes: u64, stop: &AtomicBool) -> bool {
    stop.load(Ordering::Relaxed)
        || limits.nodes != 0 && nodes > limits.nodes
        || limits.time_ms != 0 && start.elapsed().as_millis() as u64 >= limits.time_ms
}

/// モードごとの探索の設定
fn configure<S: state::MateState, const PIECES: bool>(
    searcher: &mut Searcher<'_, S, PIECES>,
    mode: MateMode,
) {
    if mode != MateMode::Speed {
        searcher.estimate_initial_values();
        searcher.count_disproof_weakly(state::PNDN_UNIT);
    }
}

/// `DfpnSolver::new(64)` で 1 回だけ解く。
///
/// 繰り返し解くときは置換表を使い回せる [`DfpnSolver`] を使う。
pub fn solve(pos: &mut Position, limits: &MateLimits, stop: &AtomicBool) -> MateResult {
    DfpnSolver::new(64).solve(pos, limits, stop, |_| {})
}

enum SolveOutcome<M> {
    Mate(Vec<M>),
    NoMate,
}

/// ルートを解き、詰みなら手順まで復元する。打ち切り・復元失敗は `None`。
fn solve_with<S: state::MateState, const PIECES: bool>(
    searcher: &mut Searcher<'_, S, PIECES>,
    st: &mut S,
) -> Option<SolveOutcome<S::Mv>> {
    let root = searcher.search(st, NodeArgs::unbounded(0, true, NO_LEN_LIMIT), &mut 0).ok()?;
    // 閾値なしの根の探索は結論が出るか打ち切りでしか戻らない。結論が手順の復元や上限のために
    // 結果にならなくても、ほかのスレッドが同じ根を読み続ける意味は無い
    searcher.halt_others();
    if root.is_disproven() {
        // ルートより前の経路は無いので反復には依存しないが、深さの上限で打ち切った手順を
        // 根拠にした不詰は、上限より長い詰みを否定できない
        return (!root.limited).then_some(SolveOutcome::NoMate);
    }
    if !root.is_proven() {
        return None;
    }
    pv::extract_pv(searcher, st, root.mate_len)
        .ok()
        .flatten()
        .map(SolveOutcome::Mate)
}

/// 詰み手順を探索とは独立に検証する: 全手が合法 (不成を含む合法手生成に含まれる)、
/// 攻め方の手はすべて王手、同じ局面に戻らない、最後の局面は王手されていて合法手が無い。
fn is_valid_mate_pv(pos: &mut Position, pv: &[Move]) -> bool {
    if pv.is_empty() || pv.len().is_multiple_of(2) {
        return false;
    }
    let mut keys = vec![pos.key()];
    let mut played = 0;
    let mut ok = true;
    for (i, &mv) in pv.iter().enumerate() {
        let mut legal = MoveList::new();
        generate_legal_all(pos, &mut legal);
        // 駒情報の有無に依らず、指し手の符号 (下位 16bit) で合法手と照合する
        let Some(&mv) = legal.iter().find(|m| m.raw() == mv.raw()) else {
            ok = false;
            break;
        };
        let gives_check = pos.gives_check(mv);
        if i % 2 == 0 && !gives_check {
            ok = false;
            break;
        }
        pos.do_move(mv, gives_check);
        played += 1;
        if keys.contains(&pos.key()) {
            ok = false;
            break;
        }
        keys.push(pos.key());
    }
    if ok {
        let mut legal = MoveList::new();
        generate_legal_all(pos, &mut legal);
        ok = pos.in_check() && legal.is_empty();
    }
    for &mv in pv[..played].iter().rev() {
        pos.undo_move(mv);
    }
    ok
}
