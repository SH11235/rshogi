//! 自分の定跡の手と、相手の (定跡に無い手を含む) 合法手のうち定跡の局面に戻る手を辺にしたグラフ。
//! 対局で RAMU が定跡を指し続けられる局面を、初期局面から辿る。先後反転した局面は引かない
//! (対局の設定 `FlippedBook=false` と同じ)。

use super::audit::MasterPosition;
use crate::book_db::{position_from_sfen, strip_ply};
use crate::packed_sfen::{pack_position, unpack_sfen_to_parts};
use anyhow::{Context, Result, anyhow, ensure};
use clap::ValueEnum;
use rayon::prelude::*;
use rshogi_core::movegen::{MoveList, generate_legal_all};
use rshogi_core::position::Position;
use rshogi_core::types::{Color, Move};
use std::collections::{BTreeMap, HashMap};

/// 初期局面の key。
pub(super) const HIRATE_KEY: &str = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b -";

/// 自分の手番で指す手の範囲 (書き出しのプリセットと同じ)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(super) enum Preset {
    /// main だけ
    Default,
    /// main と alt
    Random,
}

/// 自分の側。
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(super) enum SideArg {
    /// 先手
    B,
    /// 後手
    W,
    /// 先手と後手
    Both,
}

impl SideArg {
    pub fn sides(self) -> &'static [&'static str] {
        match self {
            SideArg::B => &["b"],
            SideArg::W => &["w"],
            SideArg::Both => &["b", "w"],
        }
    }
}

/// 辺 (12 バイト)。`book_move` は自分の手なら元データの局面の手の番号、相手の手なら [`NONE`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ReplyEdge {
    pub mv: Move,
    pub to: u32,
    pub book_move: u32,
}

/// 元データに無い局面・相手の手を表す番号。
pub(super) const NONE: u32 = u32::MAX;

impl ReplyEdge {
    pub fn to(&self) -> usize {
        self.to as usize
    }

    /// 自分の手なら、元データの局面の手の番号 (`MasterPosition::moves` の添字)。
    pub fn book_move(&self) -> Option<usize> {
        (self.book_move != NONE).then_some(self.book_move as usize)
    }
}

/// 32 バイトに詰めた局面 (教師データの PackedSfen と同じ形)。盤・手番・持ち駒が同じなら同じ値になる。
type Packed = [u8; 32];

/// 元データの局面の索引。局面は key の順に番号を振る (番号の順は key の文字列の順と同じ)。
pub(super) struct BookIndex<'a> {
    list: Vec<(&'a str, &'a MasterPosition)>,
    packed: Vec<Packed>,
    /// 局面のハッシュ値 → 番号。同じハッシュ値の局面が 2 つ以上あれば `collided` に全部を入れる。
    by_hash: HashMap<u64, u32>,
    collided: HashMap<u64, Vec<u32>>,
}

impl<'a> BookIndex<'a> {
    /// 元データの局面ごとに、詰めた形を元に戻すと同じ key になることを確かめてから索引を作る。
    pub fn new(positions: &'a BTreeMap<String, MasterPosition>) -> Result<Self> {
        let list: Vec<(&str, &MasterPosition)> =
            positions.iter().map(|(k, p)| (k.as_str(), p)).collect();
        // エラーは key の順で最初のものを返す。
        let entries: Vec<Result<(u64, Packed)>> = list
            .par_iter()
            .map(|(key, _)| {
                let pos = position_from_sfen(&format!("{key} 1"))?;
                let packed = pack_position(&pos);
                ensure!(
                    strip_ply(&unpack(&packed)?.to_sfen()) == *key,
                    "局面を詰めた形で表せません: {key}"
                );
                Ok((pos.key(), packed))
            })
            .collect();
        let mut packed = Vec::with_capacity(list.len());
        let mut by_hash: HashMap<u64, u32> = HashMap::with_capacity(list.len());
        let mut collided: HashMap<u64, Vec<u32>> = HashMap::new();
        for (i, entry) in entries.into_iter().enumerate() {
            let (hash, p) = entry?;
            packed.push(p);
            if let Some(&first) = by_hash.get(&hash) {
                collided.entry(hash).or_insert_with(|| vec![first]).push(i as u32);
            } else {
                by_hash.insert(hash, i as u32);
            }
        }
        Ok(Self {
            list,
            packed,
            by_hash,
            collided,
        })
    }

    pub fn key(&self, i: usize) -> &'a str {
        self.list[i].0
    }

    pub fn position(&self, i: usize) -> &'a MasterPosition {
        self.list[i].1
    }

    /// key の局面の番号。
    pub fn find(&self, key: &str) -> Option<usize> {
        self.list.binary_search_by(|(k, _)| (*k).cmp(key)).ok()
    }

    /// ハッシュ値が一致する局面があるか (当たりを付けるだけ)。
    fn has_hash(&self, hash: u64) -> bool {
        self.by_hash.contains_key(&hash)
    }

    /// ハッシュ値と詰めた形が一致する局面の番号。
    fn find_packed(&self, hash: u64, packed: &Packed) -> Option<u32> {
        match self.collided.get(&hash) {
            Some(ids) => ids.iter().copied().find(|&i| self.packed[i as usize] == *packed),
            None => {
                self.by_hash.get(&hash).copied().filter(|&i| self.packed[i as usize] == *packed)
            }
        }
    }
}

/// 詰めた形から局面を作る (SFEN 文字列を経ない)。
fn unpack(packed: &Packed) -> Result<Position> {
    let parts = unpack_sfen_to_parts(packed).map_err(|e| anyhow!("詰めた局面を戻せません: {e}"))?;
    let mut pos = Position::new();
    pos.set_from_parts(&parts.board, &parts.hands, parts.side_to_move)
        .map_err(|e| anyhow!("詰めた局面を戻せません: {e}"))?;
    Ok(pos)
}

/// 頂点 0 が根。頂点はすべて根から辿れる。局面は詰めた形で持ち、key の文字列は出力のときだけ作る。
pub(super) struct ReplyGraph<'a> {
    index: &'a BookIndex<'a>,
    packed: Vec<Packed>,
    /// 元データの局面の番号 (元データに無ければ [`NONE`])。
    master: Vec<u32>,
    /// 自分の手番の局面か。
    pub own: Vec<bool>,
    /// 頂点 `v` の辺は `edges[edge_start[v]..edge_start[v + 1]]`。
    edge_start: Vec<u32>,
    edges: Vec<ReplyEdge>,
    /// 自分の手のうち、非合法で辺にしなかった手の数。
    pub illegal_moves: usize,
}

impl<'a> ReplyGraph<'a> {
    pub fn len(&self) -> usize {
        self.packed.len()
    }

    pub fn edges(&self, v: usize) -> &[ReplyEdge] {
        &self.edges[self.edge_start[v] as usize..self.edge_start[v + 1] as usize]
    }

    /// 頂点 `v` の `i` 番目の辺の、グラフ全体での番号。
    pub fn edge_id(&self, v: usize, i: usize) -> usize {
        self.edge_start[v] as usize + i
    }

    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    /// 元データの局面の番号。
    pub fn master(&self, v: usize) -> Option<usize> {
        (self.master[v] != NONE).then_some(self.master[v] as usize)
    }

    /// 局面の key (SFEN の手数を除いた形)。
    pub fn key(&self, v: usize) -> Result<String> {
        match self.master(v) {
            Some(m) => Ok(self.index.key(m).to_owned()),
            None => Ok(strip_ply(&unpack(&self.packed[v])?.to_sfen()).to_owned()),
        }
    }

    /// 自分の手番の局面 `v` の `i` 番目の手の、元データの行の手の文字列 (相手の手なら `None`)。
    pub fn own_move_text(&self, v: usize, i: usize) -> Option<&'a str> {
        let book_move = self.edges(v)[i].book_move()?;
        Some(self.index.position(self.master(v)?).moves[book_move].mv.as_str())
    }

    /// 根から辿れる自明でない強連結成分 (循環)。`active(頂点, 辺の番号)` が偽の辺は使わない。
    /// 成分の中は頂点の番号順、成分は先頭の頂点の番号順。
    pub fn cycles(&self, active: impl Fn(usize, usize) -> bool) -> Vec<Vec<usize>> {
        let mut reached = vec![false; self.len()];
        let mut order = Vec::new();
        let mut stack = vec![0];
        reached[0] = true;
        while let Some(v) = stack.pop() {
            order.push(v);
            for (i, e) in self.edges(v).iter().enumerate() {
                if active(v, i) && !reached[e.to()] {
                    reached[e.to()] = true;
                    stack.push(e.to());
                }
            }
        }
        order.sort_unstable();
        let local: HashMap<usize, u32> =
            order.iter().enumerate().map(|(i, &v)| (v, i as u32)).collect();
        let adjacency: Vec<Vec<u32>> = order
            .iter()
            .map(|&v| {
                self.edges(v)
                    .iter()
                    .enumerate()
                    .filter(|&(i, _)| active(v, i))
                    .map(|(_, e)| local[&e.to()])
                    .collect()
            })
            .collect();
        nontrivial_sccs(&adjacency)
            .into_iter()
            .map(|c| c.into_iter().map(|i| order[i as usize]).collect())
            .collect()
    }
}

/// 自分の手番の局面で使う手の番号。`removed` の手は使わない。
pub(super) fn selected_moves(
    position: &MasterPosition,
    preset: Preset,
    removed: Option<&[bool]>,
) -> Vec<usize> {
    let limit = match preset {
        Preset::Default => 1,
        Preset::Random => position.moves.len(),
    };
    (0..position.moves.len())
        .filter(|&i| removed.is_none_or(|r| !r[i]))
        .take(limit)
        .collect()
}

/// `threads` 本 (0 = 全コア) の rayon のスレッドプール。
pub(super) fn thread_pool(threads: usize) -> Result<rayon::ThreadPool> {
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .context("スレッドプールを作れません")
}

/// 1 局面から出る辺 (手、行き先のハッシュ値と詰めた形と元データの番号、自分の手なら元データの手の番号) と、
/// 非合法で辺にしなかった自分の手の数。
type Expansion = (Vec<(Move, u64, Packed, u32, u32)>, usize);

/// `side` を自分として `start` から辿ったグラフを作る。自分の手番の局面では `select(元データの番号, 局面)` の
/// 手を指し、相手の手番の局面では、合法手のうち元データの局面に戻る手を辺にする。相手の合法手は不成も含める
/// (探索用の手の生成は一部の不成を省くので使わない)。
///
/// 根からの段ごとに、その段の局面を並べて展開する (rayon のスレッドで)。局面の番号は 1 本の幅優先探索と同じ
/// 順に振るので、結果はスレッド数によらない。
///
/// 計算量: 相手の手番の局面ごとに全合法手 (平均 80 前後) について、局面を進めずに行き先のハッシュ値を求め、
/// 元データの局面と一致した手だけ局面を進めて詰めた形で確かめる。メモリは頂点ごとに約 60 バイト (詰めた局面と
/// 索引)、辺ごとに 12 バイト。
pub(super) fn build<'a>(
    index: &'a BookIndex<'a>,
    side: &str,
    start: &str,
    select: impl Fn(usize, &MasterPosition) -> Vec<usize> + Sync,
) -> Result<ReplyGraph<'a>> {
    let black = side == "b";
    let mut graph = ReplyGraph {
        index,
        packed: Vec::new(),
        master: Vec::new(),
        own: Vec::new(),
        edge_start: vec![0],
        edges: Vec::new(),
        illegal_moves: 0,
    };
    // 頂点のハッシュ値 → 番号。同じハッシュ値の別の局面は `collided` に入れ、詰めた形で見分ける。
    let mut by_hash: HashMap<u64, u32> = HashMap::new();
    let mut collided: HashMap<u64, Vec<u32>> = HashMap::new();
    let mut intern =
        |graph: &mut ReplyGraph, hash: u64, packed: Packed, master: u32, own: bool| -> u32 {
            let found = match collided.get(&hash) {
                Some(ids) => ids.iter().copied().find(|&v| graph.packed[v as usize] == packed),
                None => by_hash.get(&hash).copied().filter(|&v| graph.packed[v as usize] == packed),
            };
            if let Some(v) = found {
                return v;
            }
            let v = graph.packed.len() as u32;
            graph.own.push(own);
            graph.packed.push(packed);
            graph.master.push(master);
            match by_hash.get(&hash) {
                Some(&first) => collided.entry(hash).or_insert_with(|| vec![first]).push(v),
                None => {
                    by_hash.insert(hash, v);
                }
            }
            v
        };
    let root = position_from_sfen(&format!("{start} 1"))?;
    let root_packed = pack_position(&root);
    let root_master = index.find_packed(root.key(), &root_packed).unwrap_or(NONE);
    let root_own = (root.side_to_move() == Color::Black) == black;
    intern(&mut graph, root.key(), root_packed, root_master, root_own);
    let expand = |packed: &Packed, master: u32, own: bool| -> Result<Expansion> {
        let mut pos = unpack(packed)?;
        let mut edges = Vec::new();
        let mut illegal = 0;
        if own {
            if master == NONE {
                return Ok((edges, illegal));
            }
            let position = index.position(master as usize);
            for i in select(master as usize, position) {
                let mut child = pos.clone();
                let Ok(mv) = apply_usi(&mut child, &position.moves[i].mv) else {
                    illegal += 1;
                    continue;
                };
                let packed = pack_position(&child);
                let to_master = index.find_packed(child.key(), &packed).unwrap_or(NONE);
                edges.push((mv, child.key(), packed, to_master, i as u32));
            }
        } else {
            let mut list = MoveList::new();
            generate_legal_all(&pos, &mut list);
            for &mv in list.iter() {
                let hash = pos.key_after(mv);
                if !index.has_hash(hash) {
                    continue;
                }
                pos.do_move(mv, pos.gives_check(mv));
                let packed = pack_position(&pos);
                pos.undo_move(mv);
                if let Some(to_master) = index.find_packed(hash, &packed) {
                    edges.push((mv, hash, packed, to_master, NONE));
                }
            }
        }
        Ok((edges, illegal))
    };
    let mut level = 0..1usize;
    while !level.is_empty() {
        let expansions: Vec<Result<Expansion>> = level
            .clone()
            .into_par_iter()
            .map(|v| expand(&graph.packed[v], graph.master[v], graph.own[v]))
            .collect();
        let next_start = graph.len();
        for (v, expansion) in level.clone().zip(expansions) {
            let (edges, illegal) = expansion?;
            graph.illegal_moves += illegal;
            // 1 手で手番が替わる。
            let child_own = !graph.own[v];
            for (mv, hash, packed, master, book_move) in edges {
                let to = intern(&mut graph, hash, packed, master, child_own);
                graph.edges.push(ReplyEdge { mv, to, book_move });
            }
            graph
                .edge_start
                .push(u32::try_from(graph.edges.len()).context("辺が多すぎます")?);
        }
        level = next_start..graph.len();
    }
    Ok(graph)
}

/// USI の手を合法性を確かめてから指し、指した手を返す。
fn apply_usi(pos: &mut Position, usi: &str) -> Result<Move> {
    let decoded = Move::from_usi(usi).with_context(|| format!("USI 指し手が不正です: {usi}"))?;
    let mv = pos
        .to_move(decoded)
        .with_context(|| format!("局面に適用できない指し手です: {usi}"))?;
    ensure!(
        mv != Move::NONE && pos.pseudo_legal(mv) && pos.is_legal(mv),
        "非合法手です: {usi}"
    );
    pos.do_move(mv, pos.gives_check(mv));
    Ok(mv)
}

/// 自明でない強連結成分 (頂点 2 つ以上)。成分の中は番号順、成分は先頭の番号順。
pub(super) fn nontrivial_sccs(adjacency: &[Vec<u32>]) -> Vec<Vec<u32>> {
    const UNSEEN: u32 = u32::MAX;
    let n = adjacency.len();
    let mut index = vec![UNSEEN; n];
    let mut low = vec![0u32; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<u32> = Vec::new();
    let mut components = Vec::new();
    let mut next = 0u32;
    // (頂点, 次に見る辺の番号) の明示的な再帰スタック。
    let mut calls: Vec<(u32, usize)> = Vec::new();
    for root in 0..n as u32 {
        if index[root as usize] != UNSEEN {
            continue;
        }
        calls.push((root, 0));
        index[root as usize] = next;
        low[root as usize] = next;
        next += 1;
        stack.push(root);
        on_stack[root as usize] = true;
        while let Some(&mut (v, ref mut i)) = calls.last_mut() {
            if let Some(&w) = adjacency[v as usize].get(*i) {
                *i += 1;
                if index[w as usize] == UNSEEN {
                    index[w as usize] = next;
                    low[w as usize] = next;
                    next += 1;
                    stack.push(w);
                    on_stack[w as usize] = true;
                    calls.push((w, 0));
                } else if on_stack[w as usize] {
                    low[v as usize] = low[v as usize].min(index[w as usize]);
                }
                continue;
            }
            calls.pop();
            if let Some(&(parent, _)) = calls.last() {
                low[parent as usize] = low[parent as usize].min(low[v as usize]);
            }
            if low[v as usize] == index[v as usize] {
                let mut component = Vec::new();
                loop {
                    let w = stack.pop().expect("Tarjan のスタック");
                    on_stack[w as usize] = false;
                    component.push(w);
                    if w == v {
                        break;
                    }
                }
                if component.len() >= 2 {
                    component.sort_unstable();
                    components.push(component);
                }
            }
        }
    }
    components.sort_unstable_by_key(|c| c[0]);
    components
}

/// テスト用の小さい元データ。
#[cfg(test)]
pub(super) mod fixture {
    use super::super::audit::{MasterPosition, parse_master_rows};
    use super::super::register::COLUMNS;
    use crate::book_db::{apply_usi_move, position_from_sfen, strip_ply};
    use std::collections::BTreeMap;
    use std::fmt::Write as _;

    /// 玉だけの局面。先手番。
    pub const ROOT: &str = "4k4/9/9/9/9/9/9/9/4K4 b -";

    /// `key` から手を進めた局面の key。
    pub fn after(key: &str, moves: &[&str]) -> String {
        let mut pos = position_from_sfen(&format!("{key} 1")).unwrap();
        for mv in moves {
            apply_usi_move(&mut pos, mv).unwrap();
        }
        strip_ply(&pos.to_sfen()).to_owned()
    }

    /// (key, 手, role, evals) の行から元データの表を作る。
    pub fn master_text(rows: &[(&str, &str, &str, &str)]) -> String {
        let mut text = COLUMNS.join("\t");
        text.push('\n');
        for (key, mv, role, evals) in rows {
            writeln!(text, "{key}\t1\t{mv}\t{role}\tread\t{evals}\t\t\twide\tt\tn").unwrap();
        }
        text
    }

    pub fn positions(rows: &[(&str, &str, &str, &str)]) -> BTreeMap<String, MasterPosition> {
        parse_master_rows(master_text(rows).as_bytes()).unwrap().0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nontrivial_sccs_finds_cycles_only() {
        // 0 -> 1 -> 2 -> 0、2 -> 3、3 -> 4 -> 3、5 は孤立。
        let adjacency = vec![vec![1], vec![2], vec![0, 3], vec![4], vec![3], vec![]];
        assert_eq!(nontrivial_sccs(&adjacency), vec![vec![0, 1, 2], vec![3, 4]]);
        assert!(nontrivial_sccs(&[vec![1], vec![2], vec![]]).is_empty());
    }

    #[test]
    fn build_follows_own_book_moves_and_opponent_moves_back_into_book() {
        use fixture::{ROOT, after, positions};
        let a = after(ROOT, &["5i4h"]);
        let c = after(&a, &["5a5b"]);
        let p = positions(&[
            (ROOT, "5i4h", "main", "10"),
            (ROOT, "5i6h", "alt", "5"),
            (&c, "4h5i", "main", "30"),
        ]);
        let index = BookIndex::new(&p).unwrap();
        let graph =
            build(&index, "b", ROOT, |_, m| selected_moves(m, Preset::Random, None)).unwrap();
        let moves = |key: &str| -> Vec<String> {
            let v = (0..graph.len()).find(|&v| graph.key(v).unwrap() == key).unwrap();
            graph
                .edges(v)
                .iter()
                .map(|e| format!("{}>{}", e.mv.to_usi(), graph.key(e.to()).unwrap()))
                .collect()
        };
        assert_eq!(
            moves(ROOT),
            [
                format!("5i4h>{a}"),
                format!("5i6h>{}", after(ROOT, &["5i6h"]))
            ]
        );
        // 相手の手番では、合法手のうち元データの局面に戻る手だけが辺になる。
        assert_eq!(moves(&a), [format!("5a5b>{c}")]);
        // C から 4h5i、相手が 5b5a で根に戻る循環。
        assert_eq!(graph.cycles(|_, _| true).len(), 1);
        let default =
            build(&index, "b", ROOT, |_, m| selected_moves(m, Preset::Default, None)).unwrap();
        assert_eq!(default.edges(0).len(), 1);
        // 元データの局面には番号が付き、それ以外の局面 (根から 5i4h の後) には付かない。
        assert_eq!(graph.master(0), index.find(ROOT));
        let a_node = (0..graph.len()).find(|&v| graph.key(v).unwrap() == a).unwrap();
        assert_eq!(graph.master(a_node), None);
    }
}
