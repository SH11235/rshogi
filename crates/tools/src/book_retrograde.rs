//! 定跡グラフの後退解析。
//!
//! 千日手を 0、連続王手の千日手を王手をかけ続けた側の負けとして、各頂点で解く側 (自分) が保証できる値と
//! その値を保つ手を求める。高い閾値から順に「その値以上を保証できる頂点」を葉の側から確定し、
//! 反復は王手をかけ続けられる循環の部分だけで行うので、入力によらず止まる。
//! YaneuraOu の `source/book/peta_shock_solver.h` と同じ方式で、値は解く側から見た 1 つの尺度で持つ。

use std::collections::{BTreeMap, VecDeque};

use anyhow::{Result, anyhow, bail};

/// 手の無い頂点・葉の手を表す番号。
pub(crate) const NONE: u32 = u32::MAX;

/// 手の種類。王手かどうかは手を指した後の局面で判定する。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// 相手に王手をかける手。
    Check,
    /// 王手をかけない手。
    Quiet,
    /// 局面を進めない分岐。相手が book 外へ出るかを選ぶ頂点から、指した後の局面へ進む。
    /// 指し手ではないので王手の連続を切らず、手数にも数えない。
    Pass,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Arc {
    /// 行き先の頂点。`NONE` なら葉。
    pub to: u32,
    /// 葉の値 (解く側から見た値)。
    pub leaf: i32,
    pub step: Step,
}

/// 解く問題。値は全て解く側から見た値で、千日手が 0 になるようにずらしてある。
/// 数千万本の手を配列に持たずに済むよう、手は呼ぶたびに作ってよい。同じ引数には同じ手を返すこと。
pub(crate) trait Game {
    /// 頂点の数。
    fn vertices(&self) -> usize;
    /// 頂点が解く側の手番か。
    fn own(&self, v: usize) -> bool;
    fn degree(&self, v: usize) -> usize;
    /// 頂点 `v` の `j` 番目の手。
    fn arc(&self, v: usize, j: usize) -> Arc;
    /// 相手が連続王手を続けて反則負けになったときの値。`[解く側, 相手]` をそれぞれの側から見た値で持つ。
    fn win(&self) -> [i32; 2];

    fn owner(&self, v: usize) -> usize {
        usize::from(!self.own(v))
    }
}

pub(crate) struct Solution {
    /// 解く側から見た値。
    pub values: Vec<i32>,
    /// 値を保証する手 (頂点内の手番号)。手の無い頂点は `NONE`。
    pub guaranteed: Vec<u32>,
    /// 値を保つ手のうち、勝ちは短く負けは長く出口へ向かう手。距離が決まらない頂点は `guaranteed` と同じ。
    pub preferred: Vec<u32>,
    /// 値を保つ手だけで出口 (葉・手の無い頂点) まで進む手数。循環から抜けられない頂点は `NONE`。
    pub distance: Vec<u32>,
    /// 王手をかけ続けられる循環の頂点数。`[解く側, 相手]`。
    pub core_size: [usize; 2],
}

impl Arc {
    /// 手の手数。局面を進めない分岐は数えない。
    pub(crate) fn length(self) -> u32 {
        u32::from(self.step != Step::Pass)
    }
}

/// 頂点 `p` 側から見た値。
fn view(p: usize, value: i32) -> i32 {
    if p == 0 { value } else { -value }
}

struct Solver<'a, G: Game> {
    game: &'a G,
    n: usize,
    /// 頂点 `w` へ入る手 `(親, 親の手番号)` は `reverse[reverse_start[w]..reverse_start[w + 1]]`。
    reverse_start: Vec<u32>,
    reverse: Vec<(u32, u32)>,
    /// 頂点 `v` の手の通し番号は `arc_start[v]..arc_start[v + 1]`。
    arc_start: Vec<u32>,
    /// `cores[q]`: `q` が自分の手番で王手をかけ続けられる循環の頂点。
    cores: [Vec<u32>; 2],
    /// 全体の頂点番号から循環内の番号への対応。対象外は `NONE`。使った要素だけを戻して使い回す。
    local: Vec<u32>,
}

impl<'a, G: Game> Solver<'a, G> {
    fn new(game: &'a G) -> Result<Self> {
        let n = game.vertices();
        if u32::try_from(n).is_err() {
            bail!("頂点が多すぎて後退解析できません: {n}");
        }
        let mut arc_start = Vec::with_capacity(n + 1);
        arc_start.push(0u32);
        for v in 0..n {
            let end = arc_start[v] as usize + game.degree(v);
            arc_start
                .push(u32::try_from(end).map_err(|_| anyhow!("手が多すぎて後退解析できません"))?);
        }
        let mut reverse_start = vec![0u32; n + 1];
        let mut total = 0u32;
        for v in 0..n {
            for j in 0..game.degree(v) {
                let to = game.arc(v, j).to;
                if to != NONE {
                    reverse_start[to as usize + 1] += 1;
                    total = total
                        .checked_add(1)
                        .ok_or_else(|| anyhow!("手が多すぎて後退解析できません"))?;
                }
            }
        }
        for w in 0..n {
            reverse_start[w + 1] += reverse_start[w];
        }
        let mut next = reverse_start.clone();
        let mut reverse = vec![(0, 0); total as usize];
        for v in 0..n {
            for j in 0..game.degree(v) {
                let to = game.arc(v, j).to;
                if to != NONE {
                    reverse[next[to as usize] as usize] = (v as u32, j as u32);
                    next[to as usize] += 1;
                }
            }
        }
        Ok(Self {
            game,
            n,
            reverse_start,
            reverse,
            arc_start,
            cores: [Vec::new(), Vec::new()],
            local: vec![NONE; n],
        })
    }

    fn parents(&self, w: usize) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.reverse[self.reverse_start[w] as usize..self.reverse_start[w + 1] as usize]
            .iter()
            .map(|&(v, j)| (v as usize, j as usize))
    }

    /// `allow(頂点, 手番号, 手)` を満たす辺だけで循環に乗る頂点を、番号順に返す (再帰を使わない Kosaraju 法)。
    fn cyclic(&self, allow: impl Fn(usize, usize, Arc) -> bool) -> Vec<u32> {
        let game = self.game;
        let n = self.n;
        let mut seen = vec![false; n];
        let mut order = Vec::with_capacity(n);
        let mut dfs: Vec<(u32, u32)> = Vec::new();
        for root in 0..n {
            if seen[root] {
                continue;
            }
            seen[root] = true;
            dfs.push((root as u32, 0));
            while let Some(top) = dfs.last_mut() {
                let (v, k) = (top.0 as usize, top.1 as usize);
                if k == game.degree(v) {
                    order.push(v as u32);
                    dfs.pop();
                    continue;
                }
                top.1 += 1;
                let arc = game.arc(v, k);
                let w = arc.to;
                if w != NONE && !seen[w as usize] && allow(v, k, arc) {
                    seen[w as usize] = true;
                    dfs.push((w, 0));
                }
            }
        }
        drop(seen);
        let mut comp = vec![NONE; n];
        let mut cyclic = Vec::new();
        let mut stack = Vec::new();
        let mut members = Vec::new();
        for &root in order.iter().rev() {
            let root = root as usize;
            if comp[root] != NONE {
                continue;
            }
            comp[root] = root as u32;
            stack.push(root);
            members.clear();
            while let Some(w) = stack.pop() {
                members.push(w as u32);
                for (v, j) in self.parents(w) {
                    if comp[v] == NONE && allow(v, j, game.arc(v, j)) {
                        comp[v] = root as u32;
                        stack.push(v);
                    }
                }
            }
            // 手は必ず行き先を持つ別の頂点か自分自身へ向かうので、1 頂点の成分は自分への辺があるときだけ循環になる。
            let self_loop = members.len() == 1 && {
                let v = members[0] as usize;
                (0..game.degree(v)).any(|j| {
                    let arc = game.arc(v, j);
                    arc.to as usize == v && allow(v, j, arc)
                })
            };
            if members.len() > 1 || self_loop {
                cyclic.extend_from_slice(&members);
            }
        }
        cyclic.sort_unstable();
        cyclic
    }

    fn solve(mut self) -> Result<Solution> {
        let game = self.game;
        if !self.cyclic(|_, _, arc| arc.step != Step::Quiet).is_empty() {
            bail!("双方が王手をかけ続ける循環には対応していません。book は出力しません");
        }
        for q in 0..2 {
            self.cores[q] = self.cyclic(|v, _, arc| game.owner(v) != q || arc.step != Step::Quiet);
        }
        // 先手側の結果の配列をそのまま値と保証手の配列にする。
        let (mut values, mut guaranteed) = self.positive(0);
        let (b_value, b_choice) = self.positive(1);
        for v in 0..self.n {
            let a_value = values[v];
            if a_value != 0 && b_value[v] != 0 {
                bail!("内部エラー: 双方が正の値を保証しています");
            }
            values[v] = a_value - b_value[v];
            if !(game.owner(v) == 0 && a_value != 0) {
                guaranteed[v] = if game.owner(v) == 1 && b_value[v] != 0 {
                    b_choice[v]
                } else {
                    NONE
                };
            }
        }
        drop((b_value, b_choice));
        for q in 0..2 {
            if !self.avoid_checks(q, &values, &mut guaranteed) {
                bail!("内部エラー: 値を保ったまま自分の王手を止める手を作れません");
            }
        }
        for v in 0..self.n {
            if guaranteed[v] == NONE {
                guaranteed[v] = (0..game.degree(v))
                    .find(|&j| self.edge_value(&values, v, j) == values[v])
                    .map_or(NONE, |j| j as u32);
            }
            self.check_local(&values, &guaranteed, v)?;
        }
        let distance = self.distance(&values);
        let preferred = (0..self.n)
            .map(|v| {
                let d = distance[v];
                if d == NONE {
                    return guaranteed[v];
                }
                let matches = |j: usize| {
                    self.edge_value(&values, v, j) == values[v]
                        && self.arc_distance(&distance, v, j) == d
                };
                if guaranteed[v] != NONE && matches(guaranteed[v] as usize) {
                    guaranteed[v]
                } else {
                    (0..game.degree(v)).find(|&j| matches(j)).map_or(guaranteed[v], |j| j as u32)
                }
            })
            .collect();
        Ok(Solution {
            core_size: [self.cores[0].len(), self.cores[1].len()],
            values,
            guaranteed,
            preferred,
            distance,
        })
    }

    fn edge_value(&self, values: &[i32], v: usize, j: usize) -> i32 {
        let arc = self.game.arc(v, j);
        if arc.to == NONE {
            arc.leaf
        } else {
            values[arc.to as usize]
        }
    }

    /// 手を指してから出口まで進む手数。決まらなければ `NONE`。
    fn arc_distance(&self, distance: &[u32], v: usize, j: usize) -> u32 {
        let arc = self.game.arc(v, j);
        if arc.to == NONE {
            return 0;
        }
        match distance[arc.to as usize] {
            NONE => NONE,
            d => d + arc.length(),
        }
    }

    /// 値の局所的な整合: 保証手は値を保ち、解く側の手は値を超えず、相手の手は値を下回らない。
    fn check_local(&self, values: &[i32], guaranteed: &[u32], v: usize) -> Result<()> {
        let game = self.game;
        if game.degree(v) == 0 {
            if values[v] != 0 {
                bail!("内部エラー: 手の無い頂点の値が千日手と異なります");
            }
            return Ok(());
        }
        let g = guaranteed[v];
        if g == NONE || self.edge_value(values, v, g as usize) != values[v] {
            bail!("内部エラー: 保証手が値を保ちません");
        }
        let p = game.owner(v);
        if (0..game.degree(v)).any(|j| view(p, self.edge_value(values, v, j)) > view(p, values[v]))
        {
            bail!("内部エラー: 値より良い手が残っています");
        }
        Ok(())
    }

    /// 高い閾値から順に、`p` がその値以上を保証できる頂点と保証手を確定する。値は `p` から見た値で、0 は未確定。
    /// 王手循環の外では各頂点を 1 度だけキューに入れ、対応する逆辺も 1 度だけ辿る。
    fn positive(&mut self, p: usize) -> (Vec<i32>, Vec<u32>) {
        let game = self.game;
        let mut front = Front {
            p,
            value: vec![0; self.n],
            choice: vec![NONE; self.n],
            left: (0..self.n).map(|v| game.degree(v) as u32).collect(),
            queue: VecDeque::new(),
        };
        // 正の葉を閾値の降順に並べる。同じ閾値の葉は頂点番号・手番号の順 (手の通し番号の順) に当たる。
        // 葉は数千万本になるので、手の通し番号だけを閾値ごとの区間に数え分けて並べる。
        let leaf_level = |v: usize, j: usize| {
            let arc = game.arc(v, j);
            (arc.to == NONE && view(p, arc.leaf) > 0).then(|| view(p, arc.leaf))
        };
        let mut levels = BTreeMap::<i32, u32>::new();
        for v in 0..self.n {
            for j in 0..game.degree(v) {
                if let Some(level) = leaf_level(v, j) {
                    *levels.entry(level).or_default() += 1;
                }
            }
        }
        let win = game.win()[p];
        if win > 0 {
            levels.entry(win).or_default();
        }
        let mut ranges = Vec::with_capacity(levels.len());
        let mut next = 0u32;
        for (&level, count) in levels.iter_mut().rev() {
            ranges.push((level, next..next + *count));
            (next, *count) = (next + *count, next);
        }
        let mut leaves = vec![0u32; next as usize];
        for v in 0..self.n {
            for j in 0..game.degree(v) {
                if let Some(level) = leaf_level(v, j) {
                    let slot = levels.get_mut(&level).expect("数えた閾値");
                    leaves[*slot as usize] = self.arc_start[v] + j as u32;
                    *slot += 1;
                }
            }
        }
        drop(levels);
        for (level, range) in ranges {
            let mut v = 0;
            for &arc in &leaves[range.start as usize..range.end as usize] {
                v = self.vertex_of(v, arc);
                front.hit(game, v, arc - self.arc_start[v], level);
            }
            loop {
                while let Some(w) = front.queue.pop_front() {
                    for (v, j) in self.parents(w as usize) {
                        front.hit(game, v, j as u32, level);
                    }
                }
                // 相手の連続王手で勝つ値に届かない閾値では、循環に留まっても保証にならない。
                if win < level {
                    break;
                }
                let added = self.safety(p, level, &front.value);
                if added.is_empty() {
                    break;
                }
                // 王手循環で確定した頂点も通常のキューへ戻し、親へ伝えてから終了を判定する。
                // 通常の伝播が一度 0 件だっただけで閾値を下げると、循環の外の親に未確定の 0 が残る。
                for (v, j) in added {
                    front.admit(v, j, level);
                }
            }
        }
        (front.value, front.choice)
    }

    /// 手の通し番号 `arc` を持つ頂点。`from` 以前の頂点は持たないこと。同じ閾値の葉は番号の昇順に並ぶので、
    /// 前の葉の頂点から先へ倍々に探す。
    fn vertex_of(&self, from: usize, arc: u32) -> usize {
        let starts = &self.arc_start;
        let (mut lo, mut step) = (from, 1);
        while lo + step < self.n && starts[lo + step] <= arc {
            lo += step;
            step *= 2;
        }
        let hi = (lo + step).min(self.n);
        lo + starts[lo..hi].partition_point(|&start| start <= arc) - 1
    }

    /// 相手の王手循環の中で、`p` が閾値以上を保証できる最大の集合を求める。
    /// 閾値以上が確定した出口へ進むか、相手が王手を続けて反則負けするなら保証を満たす。
    fn safety(&mut self, p: usize, level: i32, won: &[i32]) -> Vec<(usize, u32)> {
        let game = self.game;
        let core = std::mem::take(&mut self.cores[1 - p]);
        if core.is_empty() {
            return Vec::new();
        }
        for (i, &v) in core.iter().enumerate() {
            self.local[v as usize] = i as u32;
        }
        let mut alive: Vec<bool> = core.iter().map(|&v| won[v as usize] == 0).collect();
        let mut bad = vec![false; core.len()];
        let mut count = vec![0usize; core.len()];
        let mut incoming = vec![Vec::new(); core.len()];
        let good = |v: usize, j: usize| {
            let arc = game.arc(v, j);
            if arc.to == NONE {
                view(p, arc.leaf) >= level
            } else {
                won[arc.to as usize] != 0
            }
        };
        let local = &self.local;
        let internal = |v: usize, j: usize, alive: &[bool]| {
            let arc = game.arc(v, j);
            arc.to != NONE
                && local[arc.to as usize] != NONE
                && alive[local[arc.to as usize] as usize]
                && (game.owner(v) == p || arc.step != Step::Quiet)
        };
        let mut queue = VecDeque::new();
        // 自分の手番は安全な手が 1 つあればよく、相手の手番は全ての応手が安全でなければならない。
        for (i, &v) in core.iter().enumerate() {
            let v = v as usize;
            if !alive[i] {
                continue;
            }
            for j in 0..game.degree(v) {
                if good(v, j) {
                    count[i] += 1;
                } else if internal(v, j, &alive) {
                    count[i] += 1;
                    incoming[local[game.arc(v, j).to as usize] as usize].push(i);
                } else if game.owner(v) != p {
                    bad[i] = true;
                }
            }
            if (game.owner(v) == p && count[i] == 0) || bad[i] {
                queue.push_back(i);
            }
        }
        while let Some(i) = queue.pop_front() {
            if !alive[i] {
                continue;
            }
            alive[i] = false;
            for &parent in &incoming[i] {
                if !alive[parent] {
                    continue;
                }
                count[parent] -= 1;
                if game.owner(core[parent] as usize) != p || count[parent] == 0 {
                    queue.push_back(parent);
                }
            }
        }
        let mut result = Vec::new();
        for (i, &v) in core.iter().enumerate() {
            if !alive[i] {
                continue;
            }
            let v = v as usize;
            let mut choice = NONE;
            if game.owner(v) == p {
                // 確定済みの出口を優先し、無ければ残った安全な集合の中の手を保証手にする。
                choice = (0..game.degree(v))
                    .find(|&j| good(v, j))
                    .or_else(|| (0..game.degree(v)).find(|&j| internal(v, j, &alive)))
                    .map_or(NONE, |j| j as u32);
            }
            result.push((v, choice));
        }
        for &v in &core {
            self.local[v as usize] = NONE;
        }
        self.cores[1 - p] = core;
        result
    }

    /// 正の値を保証しない頂点でも、値を保ちながら `q` 自身の王手を途中で止められる手を保証手にする。
    /// 反則負けの値の頂点は、反則負けより良い値を約束していないので対象外。
    fn avoid_checks(&mut self, q: usize, values: &[i32], guaranteed: &mut [u32]) -> bool {
        let game = self.game;
        let lose = -game.win()[1 - q];
        let active: Vec<usize> = self.cores[q]
            .iter()
            .map(|&v| v as usize)
            .filter(|&v| view(q, values[v]) > lose && view(q, values[v]) <= 0)
            .collect();
        for (i, &v) in active.iter().enumerate() {
            self.local[v] = i as u32;
        }
        let mut left = vec![0usize; active.len()];
        let mut choice = vec![NONE; active.len()];
        let mut reached = vec![false; active.len()];
        let mut pred = vec![Vec::new(); active.len()];
        let mut queue = VecDeque::new();
        let keeps = |v: usize, j: usize| self.edge_value(values, v, j) == values[v];
        for (i, &v) in active.iter().enumerate() {
            left[i] = (0..game.degree(v)).filter(|&j| game.owner(v) != q || keeps(v, j)).count();
        }
        let mut hit = |i: usize, j: usize, reached: &mut [bool], queue: &mut VecDeque<usize>| {
            if reached[i] {
                return;
            }
            if game.owner(active[i]) == q {
                reached[i] = true;
                choice[i] = j as u32;
                queue.push_back(i);
            } else {
                left[i] -= 1;
                if left[i] == 0 {
                    reached[i] = true;
                    queue.push_back(i);
                }
            }
        };
        // 自分は値を保つ手だけを選べ、相手には全応手を残す。葉・対象の外・自分の王手でない手を出口とする。
        for (i, &v) in active.iter().enumerate() {
            for j in 0..game.degree(v) {
                if game.owner(v) == q && !keeps(v, j) {
                    continue;
                }
                let arc = game.arc(v, j);
                if arc.to == NONE
                    || self.local[arc.to as usize] == NONE
                    || (game.owner(v) == q && arc.step == Step::Quiet)
                {
                    hit(i, j, &mut reached, &mut queue);
                } else {
                    pred[self.local[arc.to as usize] as usize].push((i, j));
                }
            }
        }
        while let Some(i) = queue.pop_front() {
            for &(parent, j) in &pred[i] {
                hit(parent, j, &mut reached, &mut queue);
            }
        }
        let mut ok = true;
        for (i, &v) in active.iter().enumerate() {
            if !reached[i] {
                ok = false;
            } else if game.owner(v) == q {
                guaranteed[v] = choice[i];
            }
            self.local[v] = NONE;
        }
        ok
    }

    /// 値を保つ手だけで出口までの手数を求める。勝ち (千日手以上) の頂点は最短、負けの頂点は最長。
    /// 勝ちの頂点は最初に確定した子で最短になり、負けの頂点は全ての子の確定を待てば最長になる。
    /// 局面を進めない分岐は手数 0 なので、0-1 BFS の順に確定する。循環から抜けられない頂点は決まらない。
    fn distance(&self, values: &[i32]) -> Vec<u32> {
        let game = self.game;
        let winning = |v: usize| view(game.owner(v), values[v]) >= 0;
        let mut distance = vec![NONE; self.n];
        // 勝ちの頂点は暫定の最短手数、負けの頂点はそれまでに確定した子の最長手数。
        let mut tentative = vec![u32::MAX; self.n];
        let mut pending = vec![0u32; self.n];
        let mut queue = VecDeque::new();
        for v in 0..self.n {
            let mut leaf = game.degree(v) == 0;
            for j in 0..game.degree(v) {
                if self.edge_value(values, v, j) != values[v] {
                    continue;
                }
                if game.arc(v, j).to == NONE {
                    leaf = true;
                } else {
                    pending[v] += 1;
                }
            }
            if leaf && (winning(v) || pending[v] == 0) {
                tentative[v] = 0;
                queue.push_back(v as u32);
            } else if !winning(v) {
                tentative[v] = 0;
            }
        }
        while let Some(w) = queue.pop_front() {
            let w = w as usize;
            if distance[w] != NONE {
                continue;
            }
            let d = tentative[w];
            distance[w] = d;
            for (v, j) in self.parents(w) {
                if distance[v] != NONE || self.edge_value(values, v, j) != values[v] {
                    continue;
                }
                let arc = game.arc(v, j);
                let candidate = d + arc.length();
                if winning(v) {
                    if candidate < tentative[v] {
                        tentative[v] = candidate;
                        if arc.length() == 0 {
                            queue.push_front(v as u32);
                        } else {
                            queue.push_back(v as u32);
                        }
                    }
                } else {
                    tentative[v] = tentative[v].max(candidate);
                    pending[v] -= 1;
                    if pending[v] == 0 {
                        if tentative[v] == d {
                            queue.push_front(v as u32);
                        } else {
                            queue.push_back(v as u32);
                        }
                    }
                }
            }
        }
        distance
    }
}

/// [`Solver::positive`] の作業領域。値は `p` から見た値で、0 は未確定。
struct Front {
    p: usize,
    value: Vec<i32>,
    choice: Vec<u32>,
    /// 相手の手番の頂点で、閾値以上とまだ確かめていない応手の数。
    left: Vec<u32>,
    queue: VecDeque<u32>,
}

impl Front {
    fn admit(&mut self, v: usize, j: u32, level: i32) {
        if self.value[v] != 0 {
            return;
        }
        self.value[v] = level;
        self.choice[v] = j;
        self.queue.push_back(v as u32);
    }

    /// 頂点 `v` の手 `j` が閾値以上と分かった。自分の手番なら 1 手で、相手の手番なら全応手で確定する。
    fn hit(&mut self, game: &impl Game, v: usize, j: u32, level: i32) {
        if self.value[v] != 0 {
            return;
        }
        if game.owner(v) == self.p {
            self.admit(v, j, level);
        } else {
            self.left[v] -= 1;
            if self.left[v] == 0 {
                self.admit(v, NONE, level);
            }
        }
    }
}

/// `game` を解く。
pub(crate) fn solve(game: &impl Game) -> Result<Solution> {
    Solver::new(game)?.solve()
}
