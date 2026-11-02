//! 変長駒余りの規則での局面の値 ([`super::DfpnSolver::clean_first`])
//!
//! 規則 (1963 年規約の「応手最長順に手余りの存する場合はこれを准詰とし、作意とする次位長手数順で手余りないもの
//! を本詰」を、次のように固定した。規約の唯一の解釈ではない):
//! - 詰上りで攻め方の持駒が空なら駒余りなし (盤上の駒数は見ない)。
//! - 受け方: 逃れる応手があれば不詰。すべて詰むなら、駒余りなしで終わる応手の中で最長。無ければ駒余りで
//!   終わる応手の中で最長。受け方の各局面で再帰的に選ぶ。
//! - 攻め方: この規則で選ばれる手数が最短の王手。同じ手数なら駒余りなし。
//! - 無駄合いは詰将棋のルール ([`super::ShortestMode::Tsume`]) の判定のまま (応手から除く)。
//!
//! 受け方の好み (駒余りなしを優先、その中で長い方) と攻め方の好み (短い方、同手数なら駒余りなし) は逆順に
//! ならない (3 手駒余りなしと 7 手駒余りは両者とも前者を選ぶ) ので、上限付きの真偽の証明 (「すべての応手を
//! L 手以内に詰ませる」) では手数を決められない。ここでは各局面の (手数, 駒余り) を、上限 L の中で決まるか
//! L を超えるか (不詰を含む) の形で求める。受け方の局面では、駒余りなしの応手が上限の中にあるとき、上限を
//! 超えた応手が駒余りで詰むかを、その局面を詰ませる手数の上界 (詰将棋のルールで証明した手数。この規則の
//! 手数は詰将棋のルールの手数以下) を上限にして解き直して確かめる。
//!
//! 決まった値は局面のキーで覚えて経路をまたいで使う。経路上の局面の反復は不詰として数え、反復で決まった値は
//! 依存する祖先より浅い局面では覚えない (詰将棋のルールの探索の詰みと同じく、覚えた値の手順が別の経路の
//! 祖先を通る場合は区別しない)。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use super::RootState;
use super::search::{Abort, NO_LEN_LIMIT, NodeArgs, Searcher};
use crate::time::Instant;
use crate::types::{Color, Hand, Move, PieceType};

/// 変長駒余りの規則での局面の値
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CfValue {
    NoMate,
    /// 詰み。`len` は手数、`clean` は詰上りで攻め方の持駒が空か
    Mate {
        len: u16,
        clean: bool,
    },
}

/// 評価器が局面に求める操作
pub(super) trait CfState {
    type Mv: Copy + Eq;

    /// 局面全体のキー (盤面・手番・持駒)
    fn key(&self) -> u64;

    /// 攻め方の持駒が空か
    fn attacker_hand_empty(&self) -> bool;

    /// 攻め方の持駒の枚数
    fn attacker_hand_count(&self) -> u32;

    /// 攻め方の手番なら王手、受け方の手番なら無駄合いでない応手を `out` に足す
    fn moves(&mut self, or_node: bool, out: &mut Vec<Self::Mv>) -> Result<(), Abort>;

    /// 攻め方の手番の局面が詰むなら、変長駒余りの規則での手数の上界 (経路を見ずに求める)
    fn mate_bound(&mut self) -> Result<Option<u16>, Abort>;

    /// 評価器のノードを 1 つ数える。制限に達したら `Abort`
    fn count_node(&mut self) -> Result<(), Abort>;

    fn do_move(&mut self, m: Self::Mv);
    fn undo_move(&mut self, m: Self::Mv);
}

/// 上限付きの値
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bounded {
    /// 上限の中で決まった値
    Within(u16, bool),
    /// 上限の手数では詰まない (不詰を含む)
    Beyond,
}

/// 経路に依存しない値であることを表す依存の深さ
const NO_DEPENDENCY: usize = usize::MAX;

#[derive(Default, Clone, Copy)]
struct Memo {
    value: Option<(u16, bool)>,
    /// この手数以内には決まらない (`u16::MAX` は不詰)
    beyond: Option<u16>,
}

/// 変長駒余りの規則の評価器
pub(super) struct CleanFirst<M> {
    memo: HashMap<u64, Memo>,
    /// 根から現局面の親までのキー
    path: Vec<u64>,
    /// 指し手の作業領域。局面ごとに確保し直さない
    buffers: Vec<Vec<M>>,
}

impl<M: Copy + Eq> CleanFirst<M> {
    pub fn new() -> Self {
        Self {
            memo: HashMap::new(),
            path: Vec::new(),
            buffers: Vec::new(),
        }
    }

    fn take_buffer(&mut self) -> Vec<M> {
        self.buffers
            .pop()
            .unwrap_or_else(|| Vec::with_capacity(crate::movegen::MAX_MOVES))
    }

    fn return_buffer(&mut self, mut buffer: Vec<M>) {
        buffer.clear();
        self.buffers.push(buffer);
    }

    /// 攻め方の手番の根の値と、規則どおりに両者が選ぶ手順。詰将棋のルールの上界で詰むのにこの規則の値が
    /// 決まらない (上界が経路上の反復を通っていた) ときは `None`
    pub fn solve<S: CfState<Mv = M>>(
        &mut self,
        st: &mut S,
    ) -> Result<Option<(CfValue, Vec<M>)>, Abort> {
        self.path.clear();
        let Some(bound) = st.mate_bound()? else {
            return Ok(Some((CfValue::NoMate, Vec::new())));
        };
        let Bounded::Within(len, clean) = self.eval(st, true, bound)?.0 else {
            return Ok(None);
        };
        let pv = self.line(st, len, clean)?;
        Ok(pv.map(|pv| (CfValue::Mate { len, clean }, pv)))
    }

    /// 値 (`len`, `clean`) の根から、規則どおりに選ぶ手順
    fn line<S: CfState<Mv = M>>(
        &mut self,
        st: &mut S,
        len: u16,
        clean: bool,
    ) -> Result<Option<Vec<M>>, Abort> {
        let mut pv = Vec::with_capacity(usize::from(len));
        let mut moves = self.take_buffer();
        let result = (|| {
            for ply in 0..len {
                let or_node = ply % 2 == 0;
                let rest = len - ply - 1;
                moves.clear();
                st.moves(or_node, &mut moves)?;
                self.path.push(st.key());
                let mut chosen = None;
                for &mv in moves.iter() {
                    st.do_move(mv);
                    let v = self.eval(st, !or_node, rest);
                    st.undo_move(mv);
                    if v?.0 == Bounded::Within(rest, clean) {
                        chosen = Some(mv);
                        break;
                    }
                }
                let Some(mv) = chosen else {
                    return Ok(false);
                };
                st.do_move(mv);
                pv.push(mv);
            }
            Ok(true)
        })();
        for &mv in pv.iter().rev() {
            st.undo_move(mv);
        }
        self.path.clear();
        self.return_buffer(moves);
        Ok(result?.then_some(pv))
    }

    /// 局面 `st` の値を上限 `limit` 手で求める。経路上の祖先に依存したら、その最も浅い深さを添えて返す
    fn eval<S: CfState<Mv = M>>(
        &mut self,
        st: &mut S,
        or_node: bool,
        limit: u16,
    ) -> Result<(Bounded, usize), Abort> {
        st.count_node()?;
        let key = st.key();
        if let Some(depth) = self.path.iter().rposition(|&k| k == key) {
            return Ok((Bounded::Beyond, depth));
        }
        if let Some(memo) = self.memo.get(&key) {
            if let Some((len, clean)) = memo.value {
                let v = if len <= limit {
                    Bounded::Within(len, clean)
                } else {
                    Bounded::Beyond
                };
                return Ok((v, NO_DEPENDENCY));
            }
            if memo.beyond.is_some_and(|b| b >= limit) {
                return Ok((Bounded::Beyond, NO_DEPENDENCY));
            }
        }
        let mut moves = self.take_buffer();
        if let Err(abort) = st.moves(or_node, &mut moves) {
            self.return_buffer(moves);
            return Err(abort);
        }
        let depth = self.path.len();
        let result = if moves.is_empty() {
            let v = if or_node {
                Bounded::Beyond
            } else {
                Bounded::Within(0, st.attacker_hand_empty())
            };
            Ok((v, NO_DEPENDENCY))
        } else if limit == 0 {
            Ok((Bounded::Beyond, NO_DEPENDENCY))
        } else {
            self.path.push(key);
            let r = if or_node {
                self.eval_attack(st, &moves, limit)
            } else {
                self.eval_defense(st, &moves, limit)
            };
            self.path.pop();
            r
        };
        let no_moves = moves.is_empty();
        self.return_buffer(moves);
        let (v, dep) = result?;
        if dep >= depth {
            let memo = self.memo.entry(key).or_default();
            match v {
                Bounded::Within(len, clean) => memo.value = Some((len, clean)),
                Bounded::Beyond => {
                    let bound = if or_node && no_moves { u16::MAX } else { limit };
                    memo.beyond = Some(memo.beyond.map_or(bound, |b| b.max(bound)));
                }
            }
            return Ok((v, NO_DEPENDENCY));
        }
        Ok((v, dep))
    }

    fn eval_attack<S: CfState<Mv = M>>(
        &mut self,
        st: &mut S,
        moves: &[M],
        limit: u16,
    ) -> Result<(Bounded, usize), Abort> {
        let mut dep = NO_DEPENDENCY;
        // 子 (受け方の手番、手数は偶数) の上限を短い方から広げる。悪い王手の先を上限いっぱいまで読まずに済む。
        // 上限 k で初めて詰む子があれば、その手数はちょうど k
        for k in (0..limit).step_by(2) {
            let mut found: Option<bool> = None;
            for &mv in moves {
                st.do_move(mv);
                let r = self.eval(st, false, k);
                st.undo_move(mv);
                let (v, d) = r?;
                dep = dep.min(d);
                if let Bounded::Within(_, clean) = v {
                    found = Some(found.unwrap_or(false) || clean);
                    if clean {
                        break;
                    }
                }
            }
            if let Some(clean) = found {
                return Ok((Bounded::Within(k + 1, clean), dep));
            }
        }
        Ok((Bounded::Beyond, dep))
    }

    fn eval_defense<S: CfState<Mv = M>>(
        &mut self,
        st: &mut S,
        moves: &[M],
        limit: u16,
    ) -> Result<(Bounded, usize), Abort> {
        let mut longest_clean: Option<u16> = None;
        let mut longest = 0;
        let mut dep = NO_DEPENDENCY;
        let mut beyond = self.take_buffer();
        let result = (|| {
            for &mv in moves {
                st.do_move(mv);
                let r = self.eval(st, true, limit - 1);
                st.undo_move(mv);
                let (v, d) = r?;
                dep = dep.min(d);
                match v {
                    Bounded::Within(len, clean) => {
                        longest = longest.max(len);
                        if clean {
                            longest_clean = Some(longest_clean.map_or(len, |l| l.max(len)));
                        }
                    }
                    Bounded::Beyond => beyond.push(mv),
                }
            }
            if beyond.is_empty() {
                return Ok(match longest_clean {
                    Some(len) => Bounded::Within(len + 1, true),
                    None => Bounded::Within(longest + 1, false),
                });
            }
            // 上限の中に駒余りなしの応手が無ければ、上限を超えた応手が選ばれる (駒余りなしでも駒余りでも)
            let Some(len) = longest_clean else {
                return Ok(Bounded::Beyond);
            };
            // 上限を超えた応手が駒余りで詰むなら選ばれない。上限を広げて値を決める
            for &mv in beyond.iter() {
                st.do_move(mv);
                let r = self.resolve(st, limit - 1);
                st.undo_move(mv);
                let (v, d) = r?;
                dep = dep.min(d);
                if v != Bounded::Within(u16::MAX, false) {
                    return Ok(Bounded::Beyond);
                }
            }
            Ok(Bounded::Within(len + 1, true))
        })();
        self.return_buffer(beyond);
        Ok((result?, dep))
    }

    /// 上限 `beyond` 手では詰まない攻め方の手番の局面が、駒余りで詰むなら `Within(u16::MAX, false)`、それ以外
    /// (駒余りなしで詰む・不詰) は `Beyond` を返す
    fn resolve<S: CfState<Mv = M>>(
        &mut self,
        st: &mut S,
        beyond: u16,
    ) -> Result<(Bounded, usize), Abort> {
        let Some(bound) = st.mate_bound()? else {
            return Ok((Bounded::Beyond, NO_DEPENDENCY));
        };
        // 攻め方の 1 手で持駒は高々 1 枚しか減らないので、持駒が攻め方の手数より多ければ必ず駒余り
        if st.attacker_hand_count() > u32::from(bound).div_ceil(2) {
            return Ok((Bounded::Within(u16::MAX, false), NO_DEPENDENCY));
        }
        // 上界まで一度に読むと、その先の受け方の局面でも同じ確かめが入れ子になって深く潜るので、短い方から広げる
        let mut dep = NO_DEPENDENCY;
        let mut limit = beyond;
        while limit < bound {
            limit = (limit + 2).min(bound);
            let (v, d) = self.eval(st, true, limit)?;
            dep = dep.min(d);
            match v {
                Bounded::Within(_, false) => return Ok((Bounded::Within(u16::MAX, false), dep)),
                Bounded::Within(_, true) => return Ok((Bounded::Beyond, dep)),
                Bounded::Beyond => {}
            }
        }
        Ok((Bounded::Beyond, dep))
    }
}

/// 実際の局面。王手・応手の生成と無駄合いの判定、手数の上界は詰将棋のルールの探索 (`searcher`、最短手順モードで
/// 無駄合いを数えない設定) で求める
pub(super) struct Solving<'s, 'a, S: RootState, const PIECES: bool> {
    pub searcher: &'s mut Searcher<'a, S, PIECES>,
    pub st: &'s mut S,
    pub attacker: Color,
    /// 評価器のノード数
    pub nodes: u64,
    /// 評価器と探索のノード数の合計の上限 (0 は無制限)
    pub node_limit: u64,
    pub time_limit_ms: u64,
    pub start: Instant,
    pub stop: &'s AtomicBool,
}

/// 評価器が停止と時間を確かめる間隔 (ノード数)
const CHECK_INTERVAL: u64 = 1024;

impl<S: RootState, const PIECES: bool> CfState for Solving<'_, '_, S, PIECES> {
    type Mv = Move;

    fn key(&self) -> u64 {
        self.st.key()
    }

    fn attacker_hand_empty(&self) -> bool {
        self.st.position().hand(self.attacker) == Hand::EMPTY
    }

    fn attacker_hand_count(&self) -> u32 {
        let hand = self.st.position().hand(self.attacker);
        PieceType::HAND_PIECES.iter().map(|&pt| hand.count(pt)).sum()
    }

    fn moves(&mut self, or_node: bool, out: &mut Vec<Move>) -> Result<(), Abort> {
        let first = out.len();
        self.st.generate(or_node, out);
        if or_node {
            return Ok(());
        }
        // 無駄合いは詰将棋のルールの判定のまま。上限は付けない (この規則の手数で判定すると、無駄合いの集合と
        // 手数が互いに依存する)
        let mut kept = first;
        for i in first..out.len() {
            let mv = out[i];
            if self.st.is_drop(mv)
                && self.searcher.futile_drop(self.st, mv, NO_LEN_LIMIT)?.is_some()
            {
                continue;
            }
            out[kept] = mv;
            kept += 1;
        }
        out.truncate(kept);
        Ok(())
    }

    fn mate_bound(&mut self) -> Result<Option<u16>, Abort> {
        let r =
            self.searcher
                .search(self.st, NodeArgs::unbounded(0, true, NO_LEN_LIMIT), &mut 0)?;
        if !r.is_proven() {
            return Ok(None);
        }
        Ok(Some(r.mate_len))
    }

    fn count_node(&mut self) -> Result<(), Abort> {
        self.nodes += 1;
        if self.node_limit != 0 && self.nodes + self.searcher.nodes() > self.node_limit {
            return Err(Abort);
        }
        if self.nodes.is_multiple_of(CHECK_INTERVAL)
            && (self.stop.load(Ordering::Relaxed)
                || self.time_limit_ms != 0
                    && self.start.elapsed().as_millis() as u64 >= self.time_limit_ms)
        {
            return Err(Abort);
        }
        Ok(())
    }

    fn do_move(&mut self, m: Move) {
        self.st.do_move(m);
    }

    fn undo_move(&mut self, m: Move) {
        self.st.undo_move(m);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 有向グラフ上の局面。偶数手目が攻め方、奇数手目が受け方。子の無い受け方の局面が詰上りで、
    /// `clean` がその詰上りの駒余りなし、子の無い攻め方の局面は不詰
    struct CfGraph {
        edges: Vec<Vec<u32>>,
        clean: Vec<bool>,
        stack: Vec<u32>,
        nodes: u64,
    }

    impl CfGraph {
        fn new(edges: Vec<Vec<u32>>, clean: Vec<bool>, root: u32) -> Self {
            Self {
                edges,
                clean,
                stack: vec![root],
                nodes: 0,
            }
        }

        fn node(&self) -> u32 {
            *self.stack.last().unwrap()
        }
    }

    impl CfState for CfGraph {
        type Mv = u32;

        fn key(&self) -> u64 {
            // 同じ節点でも手番が違えば別の局面
            let side = (self.stack.len() % 2) as u64;
            (2 * u64::from(self.node()) + side + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        }

        fn attacker_hand_empty(&self) -> bool {
            self.clean[self.node() as usize]
        }

        fn attacker_hand_count(&self) -> u32 {
            0
        }

        fn moves(&mut self, _or_node: bool, out: &mut Vec<u32>) -> Result<(), Abort> {
            out.extend_from_slice(&self.edges[self.node() as usize]);
            Ok(())
        }

        fn mate_bound(&mut self) -> Result<Option<u16>, Abort> {
            // 詰将棋のルール (受け方は最長) の手数。経路を見ずに求める
            let mut path = Vec::new();
            Ok(tsume_len(&self.edges, self.node(), true, &mut path))
        }

        fn count_node(&mut self) -> Result<(), Abort> {
            self.nodes += 1;
            Ok(())
        }

        fn do_move(&mut self, m: u32) {
            self.stack.push(m);
        }

        fn undo_move(&mut self, _m: u32) {
            self.stack.pop();
        }
    }

    /// 詰将棋のルールの手数 (攻め方は最短、受け方は最長、反復は不詰)
    fn tsume_len(
        edges: &[Vec<u32>],
        v: u32,
        or_node: bool,
        path: &mut Vec<(u32, bool)>,
    ) -> Option<u16> {
        if path.contains(&(v, or_node)) {
            return None;
        }
        let kids = &edges[v as usize];
        if kids.is_empty() {
            return (!or_node).then_some(0);
        }
        path.push((v, or_node));
        let mut lens = kids.iter().map(|&c| tsume_len(edges, c, !or_node, path));
        let r = if or_node {
            lens.flatten().min()
        } else {
            lens.try_fold(0, |m, l| l.map(|l| m.max(l)))
        };
        path.pop();
        r.map(|l| l + 1)
    }

    /// 規則をそのまま書いた正解役 (総当たり、反復は不詰)
    fn reference(
        edges: &[Vec<u32>],
        clean: &[bool],
        v: u32,
        or_node: bool,
        path: &mut Vec<(u32, bool)>,
    ) -> CfValue {
        if path.contains(&(v, or_node)) {
            return CfValue::NoMate;
        }
        let kids = &edges[v as usize];
        if kids.is_empty() {
            return if or_node {
                CfValue::NoMate
            } else {
                CfValue::Mate {
                    len: 0,
                    clean: clean[v as usize],
                }
            };
        }
        path.push((v, or_node));
        let values: Vec<CfValue> =
            kids.iter().map(|&c| reference(edges, clean, c, !or_node, path)).collect();
        path.pop();
        let mates = values.iter().filter_map(|v| match *v {
            CfValue::Mate { len, clean } => Some((len, clean)),
            CfValue::NoMate => None,
        });
        let chosen = if or_node {
            mates.min_by_key(|&(len, clean)| (len, !clean))
        } else if values.contains(&CfValue::NoMate) {
            None
        } else {
            mates.max_by_key(|&(len, clean)| (clean, len))
        };
        match chosen {
            Some((len, clean)) => CfValue::Mate {
                len: len + 1,
                clean,
            },
            None => CfValue::NoMate,
        }
    }

    /// 評価器の値と手順が正解役と一致する
    fn assert_matches(edges: &[Vec<u32>], clean: &[bool], root: u32) -> CfValue {
        let expected = reference(edges, clean, root, true, &mut Vec::new());
        let mut graph = CfGraph::new(edges.to_vec(), clean.to_vec(), root);
        let (value, pv) = CleanFirst::new().solve(&mut graph).unwrap().unwrap();
        assert_eq!(value, expected, "{edges:?} {clean:?}");
        if let CfValue::Mate { len, clean: c } = value {
            assert_eq!(pv.len(), usize::from(len));
            // 手順の各局面の値が残りの手数と駒余りに合う
            let mut path = Vec::new();
            let mut v = root;
            for (i, &m) in pv.iter().enumerate() {
                assert!(edges[v as usize].contains(&m));
                path.push((v, i % 2 == 0));
                v = m;
                let rest = len - i as u16 - 1;
                let got = reference(edges, clean, v, i % 2 == 1, &mut path);
                assert_eq!(
                    got,
                    CfValue::Mate {
                        len: rest,
                        clean: c
                    },
                    "{pv:?}"
                );
            }
            assert!(edges[v as usize].is_empty());
        }
        assert_eq!(graph.stack, vec![root]);
        value
    }

    /// 攻め方の局面 (根) の 1 手の後、受け方が「長い駒余り」と「短い駒余りなし」から選ぶ形を作る:
    /// `lines` は受け方の応手ごとの (攻め方の手数, 詰上りの駒余りなし)
    fn defender_choice(lines: &[(u16, bool)]) -> (Vec<Vec<u32>>, Vec<bool>) {
        let mut edges = vec![vec![1], vec![]];
        let mut clean = vec![false, false];
        for &(attacks, c) in lines {
            let first = edges.len() as u32;
            edges[1].push(first);
            // 攻め方の手番 → 受け方 (1 手) → … と一本道で伸ばし、最後が詰上り
            let plies = 2 * u32::from(attacks) - 1;
            for i in 0..plies {
                edges.push(vec![first + i + 1]);
                clean.push(false);
            }
            edges.push(vec![]);
            clean.push(c);
        }
        (edges, clean)
    }

    #[test]
    fn defender_prefers_shorter_clean_line() {
        // 応手 A: 根から 9 手で駒余り、応手 B: 7 手で駒余りなし → 受け方は B
        let (edges, clean) = defender_choice(&[(4, false), (3, true)]);
        assert_eq!(
            assert_matches(&edges, &clean, 0),
            CfValue::Mate {
                len: 7,
                clean: true
            }
        );
    }

    #[test]
    fn defender_takes_longest_leftover_without_clean_line() {
        let (edges, clean) = defender_choice(&[(4, false), (2, false)]);
        assert_eq!(
            assert_matches(&edges, &clean, 0),
            CfValue::Mate {
                len: 9,
                clean: false
            }
        );
    }

    #[test]
    fn defender_takes_longest_clean_line() {
        let (edges, clean) = defender_choice(&[(4, false), (2, true), (3, true), (1, false)]);
        assert_eq!(
            assert_matches(&edges, &clean, 0),
            CfValue::Mate {
                len: 7,
                clean: true
            }
        );
    }

    #[test]
    fn attacker_prefers_same_length_clean() {
        // 根の王手 2 つがどちらも 3 手で詰み、駒余りなしは片方だけ
        let edges = vec![
            vec![1, 4],
            vec![2],
            vec![3],
            vec![],
            vec![5],
            vec![6],
            vec![],
        ];
        let clean = vec![false, false, false, false, false, false, true];
        assert_eq!(
            assert_matches(&edges, &clean, 0),
            CfValue::Mate {
                len: 3,
                clean: true
            }
        );
        // 短い駒余りは長い駒余りなしより優先
        let edges = vec![vec![1, 2], vec![], vec![3], vec![4], vec![]];
        let clean = vec![false, false, false, false, true];
        assert_eq!(
            assert_matches(&edges, &clean, 0),
            CfValue::Mate {
                len: 1,
                clean: false
            }
        );
    }

    #[test]
    fn nested_defender_choices_are_applied_recursively() {
        // 根 0 → 受け方 1。応手 2 の先の受け方 4 は、駒余りなしの短い応手 6 を選ぶので、応手 2 は駒余りなしで
        // 根から 5 手。応手 3 は根から 3 手で駒余り。受け方 1 は駒余りなしの応手 2 を選ぶ (詰将棋のルールでは
        // 受け方 4 が長い応手 8 を選び、根から 7 手で駒余り)
        let edges = vec![
            vec![1],    // 0 攻め
            vec![2, 3], // 1 受け
            vec![4],    // 2 攻め
            vec![5],    // 3 攻め
            vec![6, 8], // 4 受け
            vec![],     // 5 詰上り (駒余り)
            vec![7],    // 6 攻め → 7 詰上り (駒余りなし)
            vec![],     // 7
            vec![9],    // 8 攻め
            vec![10],   // 9 受け
            vec![11],   // 10 攻め
            vec![],     // 11 詰上り (駒余り)
        ];
        let mut clean = vec![false; 12];
        clean[7] = true;
        assert_eq!(
            assert_matches(&edges, &clean, 0),
            CfValue::Mate {
                len: 5,
                clean: true
            }
        );
    }

    #[test]
    fn repetition_is_no_mate() {
        // 受け方 1 の応手で根に戻る (反復は不詰) ので、根は詰まない
        let edges = vec![vec![1], vec![0, 3], vec![], vec![4], vec![]];
        let clean = vec![false, false, false, false, true];
        assert_eq!(assert_matches(&edges, &clean, 0), CfValue::NoMate);
    }

    #[test]
    fn matches_reference_on_random_graphs() {
        use rand::{Rng, SeedableRng};
        let mut rng = rand_xoshiro::Xoshiro256PlusPlus::seed_from_u64(0xCF01);
        let mut mates = 0;
        for _ in 0..3000 {
            let n = rng.random_range(4..14);
            let mut edges = vec![Vec::new(); n];
            for (v, kids) in edges.iter_mut().enumerate() {
                // 前向きの辺を多めにし、時々後ろ向き (反復) の辺を張る
                let k = rng.random_range(0..4);
                for _ in 0..k {
                    let to = if rng.random_bool(0.85) && v + 1 < n {
                        rng.random_range(v + 1..n)
                    } else {
                        rng.random_range(0..n)
                    };
                    if to != v && !kids.contains(&(to as u32)) {
                        kids.push(to as u32);
                    }
                }
            }
            let clean: Vec<bool> = (0..n).map(|_| rng.random_bool(0.5)).collect();
            if matches!(assert_matches(&edges, &clean, 0), CfValue::Mate { .. }) {
                mates += 1;
            }
        }
        assert!(mates >= 300, "詰みの局面が少なすぎる: {mates}");
    }
}
