//! 復元型の無駄合いの判定に使う、局面を目標の局面へ戻せるかの探索
//!
//! 攻め方が王手を続けて、受け方の手番で盤面が合駒の前の局面と同じ、攻め方の持駒が目標以上の局面に着けるかを、
//! 証明数探索 (木を持つ最良優先) で読む。受け方はすべての合法手を指せるものとする。途中の合駒を攻め方が取れば
//! 持駒が増えるだけなので、途中の合駒を無駄合いかどうか判定しなくてよい。受け方が詰む・攻め方が王手できない・
//! 経路上の局面に戻る・手数の上限に達するのは着けない。

use super::search::Abort;
use super::state::MateState;
use crate::types::{Color, Hand};
use super::table::PN_INF;

/// 探索木の節点
pub(super) struct GoalNode<M> {
    /// 親から指した手 (根は `None`)
    mv: Option<M>,
    parent: u32,
    first: u32,
    count: u16,
    pn: u32,
    dn: u32,
    expanded: bool,
}

/// 結果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Reach {
    /// 必ず着ける
    Forced,
    /// 着けないことがある
    Escapes,
    /// 予算内に決まらない
    Unknown,
}

/// 着きたい局面: 受け方の手番で盤面のキーが `board` (各升の駒は `cells`)、攻め方 `attacker` の持駒が `hand` 以上
#[derive(Debug, Clone, Copy)]
pub(super) struct Goal {
    pub attacker: Color,
    pub board: u64,
    pub cells: [u8; 81],
    pub hand: Hand,
}

/// 攻め方の手番の局面 `st` から `goal` に必ず着けるかを、`plies` 手以内・節点 `budget` 個までで読む。
/// `nodes` と `moves` は作業領域。`tick` は節点を 1 つ展開するたびに呼ぶ
pub(super) fn reach<S: MateState>(
    st: &mut S,
    goal: Goal,
    plies: u16,
    budget: usize,
    nodes: &mut Vec<GoalNode<S::Mv>>,
    moves: &mut Vec<S::Mv>,
    mut tick: impl FnMut() -> Result<(), Abort>,
) -> Result<Reach, Abort> {
    nodes.clear();
    nodes.push(GoalNode {
        mv: None,
        parent: u32::MAX,
        first: 0,
        count: 0,
        pn: 1,
        dn: 1,
        expanded: false,
    });
    let mut path: Vec<u64> = Vec::with_capacity(usize::from(plies) + 1);
    let mut line: Vec<S::Mv> = Vec::with_capacity(usize::from(plies));
    loop {
        if nodes[0].pn == 0 {
            return Ok(Reach::Forced);
        }
        if nodes[0].dn == 0 {
            return Ok(Reach::Escapes);
        }
        if nodes.len() >= budget {
            return Ok(Reach::Unknown);
        }
        // 最も証明に近い葉へ降りる
        path.clear();
        line.clear();
        path.push(st.key());
        let mut cur = 0usize;
        while nodes[cur].expanded {
            let or_node = line.len() % 2 == 0;
            let (first, count) = (nodes[cur].first as usize, usize::from(nodes[cur].count));
            let pick = (first..first + count)
                .min_by_key(|&i| if or_node { nodes[i].pn } else { nodes[i].dn })
                .expect("展開した節点には子がある");
            let mv = nodes[pick].mv.expect("子には手がある");
            st.do_move(mv);
            line.push(mv);
            path.push(st.key());
            cur = pick;
        }
        if let Err(abort) = tick() {
            while let Some(mv) = line.pop() {
                st.undo_move(mv);
            }
            return Err(abort);
        }
        let or_node = line.len() % 2 == 0;
        moves.clear();
        st.generate(or_node, moves);
        if !or_node && std::env::var("DBG_NODROP").is_ok() {
            moves.retain(|&m| !st.is_drop(m));
        }
        let depth = line.len() as u16;
        if !or_node && moves.is_empty() && std::env::var("DBG_MATEOK").is_ok() {
            nodes[cur].pn = 0;
            nodes[cur].dn = PN_INF;
        } else if moves.is_empty() || depth >= plies {
            nodes[cur].pn = PN_INF;
            nodes[cur].dn = 0;
        } else {
            let first = nodes.len() as u32;
            for &mv in moves.iter() {
                let key = st.key_after(mv);
                let reached = or_node && {
                    let after = st.table_key_after(mv, goal.attacker);
                    after.board == goal.board && after.hand.is_superior_or_equal(goal.hand)
                };
                let (pn, dn) = if reached {
                    (0, PN_INF)
                } else if path.contains(&key) {
                    (PN_INF, 0)
                } else {
                    // 1 手で駒が変わる升は 2 つまでなので、盤の違いの升数の半分は残りの手数が要る。
                    // 違いが少ない局面ほど目標に近いとみて先に読む
                    st.do_move(mv);
                    let diff = st.board_diff(&goal.cells);
                    st.undo_move(mv);
                    if diff.div_ceil(2) > u32::from(plies - depth - 1) {
                        (PN_INF, 0)
                    } else {
                        (diff.max(1), 1)
                    }
                };
                nodes.push(GoalNode {
                    mv: Some(mv),
                    parent: cur as u32,
                    first: 0,
                    count: 0,
                    pn,
                    dn,
                    expanded: false,
                });
            }
            let node = &mut nodes[cur];
            node.first = first;
            node.count = moves.len() as u16;
            node.expanded = true;
            update(nodes, cur, or_node);
        }
        // 根へ戻りながら値を更新する
        while let Some(mv) = line.pop() {
            st.undo_move(mv);
            cur = nodes[cur].parent as usize;
            update(nodes, cur, line.len() % 2 == 0);
        }
    }
}

fn update<M>(nodes: &mut [GoalNode<M>], i: usize, or_node: bool) {
    let (first, count) = (nodes[i].first as usize, usize::from(nodes[i].count));
    let kids = &nodes[first..first + count];
    let (pn, dn) = if or_node {
        (
            kids.iter().map(|c| c.pn).min().unwrap_or(PN_INF),
            kids.iter().fold(0u32, |s, c| s.saturating_add(c.dn)).min(PN_INF),
        )
    } else {
        (
            kids.iter().fold(0u32, |s, c| s.saturating_add(c.pn)).min(PN_INF),
            kids.iter().map(|c| c.dn).min().unwrap_or(PN_INF),
        )
    };
    nodes[i].pn = pn;
    nodes[i].dn = dn;
}
