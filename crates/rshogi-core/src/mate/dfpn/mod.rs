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
//! 探索深さの上限 (2048 手) を超える手順を読み切れずに不詰と言えないとき、詰みを証明したが
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

use std::sync::atomic::{AtomicBool, Ordering};

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
    /// 子の pn/dn の初期値も見積もらない。1 ノードあたりが軽く、不詰の判定が速い。
    Speed,
    /// 解く力優先。詰み・不詰を証明駒・反証駒で置換表に書き、持駒だけ違う局面に流用する。
    /// 子の pn/dn の初期値を局面から見積もる (df-pn+)。詰む問題・長手数・難問でノードが減り、
    /// 少ない置換表で解ける問題が増えるが、1 ノードあたりは重く、不詰の判定ではノードが増える。
    Power,
    /// 証明駒を使わない探索で [`AUTO_SWITCH_NODES`] ノードまで読み、決まらなければ同じ置換表のまま
    /// 解く力優先で読み続ける (ノード数・時間の制限は合計に掛かる)。初期値の見積もりはどちらでも使う。
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
}

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
        }
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
                let exhausted = stop.load(Ordering::Relaxed)
                    || limits.nodes != 0 && self.nodes > limits.nodes
                    || limits.time_ms != 0 && start.elapsed().as_millis() as u64 >= limits.time_ms;
                if decided.is_some() || exhausted {
                    decided
                } else {
                    // 速さ優先の置換表の値はそのまま使える
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
        let budget = Budget {
            stop,
            node_limit: limits.nodes,
            time_limit_ms: limits.time_ms,
            start,
            progress,
        };
        let attacker = pos.side_to_move();
        let mut searcher =
            Searcher::<_, PIECES>::new(&mut self.table, &mut self.scratch, attacker, budget);
        searcher.continue_from(nodes);
        if self.mode != MateMode::Speed {
            searcher.estimate_initial_values();
        }
        let result = solve_with(&mut searcher, pos);
        self.nodes = searcher.nodes();
        result
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
