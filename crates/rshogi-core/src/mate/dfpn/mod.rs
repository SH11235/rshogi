//! df-pn 詰み探索
//!
//! 手番側 (攻め方) が王手を続けて相手玉を詰ませられるかを、証明数・反証数を使う
//! 深さ優先探索 (df-pn) で判定する。
//!
//! 問題の定義: 与えられた局面から始め、それ以前の対局履歴は見ない。攻め方の手はすべて王手で、
//! 受け方は合法手 (合駒・不成を含む) をすべて考える。打ち歩詰めの歩打ちは攻め方の手にならない。
//! 探索中に同じ局面へ戻る手順は (連続王手の千日手を含め) 攻め方の失敗として扱う。
//!
//! 結果は 3 値で、打ち切り (時間・ノード数・停止) は必ず [`MateResult::Unknown`] になり、
//! 不詰と混ざらない。探索深さの上限 (2048 手) を超える手順を読み切れずに不詰と言えないときも
//! Unknown を返す。詰みの手順は最短とは限らない。局面の同一性は 64bit のハッシュキーで
//! 判定するので、衝突による誤りの可能性は確率的にのみ排除される。

mod pv;
mod search;
mod state;
mod table;

#[cfg(test)]
mod tests;

use std::sync::atomic::AtomicBool;

use search::{Budget, NO_LEN_LIMIT, NodeArgs, Searcher};
use table::MateTable;

use crate::movegen::{MoveList, generate_legal_all};
use crate::position::Position;
use crate::time::Instant;
use crate::types::Move;

/// 詰み探索の結果
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MateResult {
    /// 詰み。攻め方の手から始まる詰み手順 (最後の手で受け方が詰む)。
    Mate(Vec<Move>),
    /// 不詰 (攻め方が王手を続けて詰ませる手順は無い)
    NoMate,
    /// 打ち切り (時間・ノード数・停止) か、探索深さの上限で判定できなかった
    Unknown,
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

/// 置換表を持ち回る df-pn ソルバー
pub struct DfpnSolver {
    table: MateTable,
    nodes: u64,
}

impl DfpnSolver {
    /// 置換表を `hash_mb` MB で確保したソルバーを作る
    pub fn new(hash_mb: usize) -> Self {
        Self {
            table: MateTable::new(hash_mb),
            nodes: 0,
        }
    }

    /// 直前の `solve` で探索したノード数 (詰み手順の復元を含む)
    pub fn nodes(&self) -> u64 {
        self.nodes
    }

    /// 局面 `pos` の手番側が詰ませられるかを判定する。
    ///
    /// `stop` が立つか制限に達すると [`MateResult::Unknown`] を返す。
    /// 約 1 秒ごとに `progress` を呼ぶ。`pos` は呼び出し前の局面に戻して返る。
    pub fn solve(
        &mut self,
        pos: &mut Position,
        limits: &MateLimits,
        stop: &AtomicBool,
        mut progress: impl FnMut(MateProgress),
    ) -> MateResult {
        self.table.clear();
        let budget = Budget {
            stop,
            node_limit: limits.nodes,
            time_limit_ms: limits.time_ms,
            start: Instant::now(),
            progress: &mut progress,
        };
        let mut searcher = Searcher::new(&mut self.table, budget);
        let result = solve_with(&mut searcher, pos);
        self.nodes = searcher.nodes();
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
fn solve_with<S: state::MateState>(
    searcher: &mut Searcher<'_, S>,
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
