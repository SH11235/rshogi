use super::journal::{Reading, Readings, Settings, digest};
use crate::book_db::{child_position_after_move, position_from_sfen, strip_ply};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

type Edges = BTreeMap<String, BTreeSet<String>>;
pub(super) type Values = BTreeMap<String, BTreeMap<String, i32>>;
pub(super) type Pending = BTreeMap<String, Vec<(i32, String)>>;

#[derive(Debug)]
struct WorkLimit;
impl std::fmt::Display for WorkLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("影の処理上限")
    }
}
impl std::error::Error for WorkLimit {}

struct Work {
    remaining: Option<usize>,
}
impl Work {
    fn tick(&mut self) -> Result<()> {
        if let Some(left) = &mut self.remaining {
            if *left == 0 {
                return Err(WorkLimit.into());
            }
            *left -= 1;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PrefetchCandidate {
    pub key: String,
    pub level: usize,
    pub ply: u32,
}

#[derive(Default)]
pub(super) struct PredictionReport {
    pub candidates: Vec<PrefetchCandidate>,
    pub limited: bool,
    pub work: usize,
    pub unread: BTreeSet<String>,
    pub cyclic: BTreeSet<String>,
}

// 5,000 局面を複数回辿る構築・依存解析・展開を同じ予算で扱う。
pub(super) const PREFETCH_WORK: usize = 500_000;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Definition {
    #[serde(skip)]
    pub source: Option<PathBuf>,
    pub paths: PathBuf,
    pub paths_sha256: Option<String>,
    pub out: PathBuf,
    pub depth: usize,
    pub opp_n: usize,
    pub stop: i32,
    pub own: String,
    pub rounds: usize,
    #[serde(default)]
    pub extra: Vec<String>,
    /// 省略時も台帳の条件を使用する。
    pub fingerprint: Option<String>,
    #[serde(default)]
    pub draw: i32,
}
impl Definition {
    pub fn params(&self) -> TreeParams {
        TreeParams {
            out: self.out.clone(),
            depth: self.depth,
            opp_n: self.opp_n,
            stop: self.stop,
            own: self.own.clone(),
            rounds: self.rounds,
            extra: self.extra.clone(),
            draw: self.draw,
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        super::paths::reject_symlink(path)?;
        let mut definition: Self = toml::from_str(&std::fs::read_to_string(path)?)?;
        definition.source = Some(std::fs::canonicalize(path)?);
        let parent = path.parent().unwrap_or(Path::new("."));
        if definition.paths.is_relative() {
            definition.paths = parent.join(&definition.paths);
        }
        if definition.out.is_relative() {
            definition.out = parent.join(&definition.out);
        }
        Ok(definition)
    }
}

/// 木の伸ばし方と出力先。定義の入力 (TOML と paths.tsv、キューの行) によらない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TreeParams {
    pub out: PathBuf,
    pub depth: usize,
    pub opp_n: usize,
    pub stop: i32,
    pub own: String,
    pub rounds: usize,
    pub extra: Vec<String>,
    pub draw: i32,
}

/// 根の局面 key・手数と、根からの実戦の手順。
pub(super) type GameLine = (String, u32, Vec<String>);

pub(super) fn child(key: &str, mv: &str) -> Result<String> {
    Ok(strip_ply(&child_position_after_move(&format!("{key} 1"), mv)?.to_sfen()).to_owned())
}
pub(super) fn side(key: &str) -> &str {
    key.split_whitespace().nth(1).unwrap_or("")
}

#[derive(Debug)]
pub(super) struct Tree {
    pub def: TreeParams,
    pub roots: BTreeMap<String, u32>,
    pub root_order: Vec<String>,
    pub game: Edges,
    pub nodes: BTreeMap<String, Reading>,
    pub reading_conditions: BTreeSet<Option<String>>,
    pub level: BTreeMap<String, usize>,
    pub ply: BTreeMap<String, u32>,
    pub edge: Edges,
    pub rule: Edges,
    pub force: Edges,
    pub candidates: BTreeMap<String, Vec<String>>,
    pub compare: BTreeMap<String, Option<String>>,
    pub round: usize,
    pub round_reports: Vec<(usize, usize)>,
    pub finished: bool,
    pub ready: bool,
    pub failed: bool,
    expanded: BTreeMap<String, (usize, u32)>,
    /// 前回の `advance` が返した不足 (読みの無い局面)。
    waiting: Vec<(String, usize)>,
}
impl Tree {
    /// TOML の定義と paths.tsv から木を作る。出力先には触れない。
    pub fn load(def: Definition, settings: &Settings) -> Result<Self> {
        if def.fingerprint.as_ref().is_some_and(|fp| fp != &settings.fingerprint) {
            bail!("木の読み条件とプロセスの指紋が違います");
        }
        if let Some(expected) = &def.paths_sha256
            && &digest(&def.paths)? != expected
        {
            bail!("paths の sha256 が違います");
        }
        let mut lines = Vec::new();
        for line in std::fs::read_to_string(&def.paths)?.lines().filter(|l| !l.trim().is_empty()) {
            let cols: Vec<_> = line.split('\t').collect();
            if cols.len() < 3 {
                bail!("paths は key / ply / 手順 が必要です");
            }
            let ply: u32 = cols[1].parse()?;
            lines.push((
                cols[0].to_owned(),
                ply,
                cols[2].split_whitespace().map(str::to_owned).collect(),
            ));
        }
        Self::build(def.params(), &lines)
    }

    /// 根と実戦の手順から木を作る。出力先には触れない。
    pub fn build(def: TreeParams, lines: &[GameLine]) -> Result<Self> {
        Self::build_with_work(def, lines, &mut Work { remaining: None })
    }

    fn build_with_work(def: TreeParams, lines: &[GameLine], work: &mut Work) -> Result<Self> {
        Self::build_borrowed(
            def,
            lines.iter().map(|(key, ply, moves)| (key.as_str(), *ply, moves.as_slice())),
            work,
        )
    }

    fn build_borrowed<'a>(
        def: TreeParams,
        lines: impl IntoIterator<Item = (&'a str, u32, &'a [String])>,
        work: &mut Work,
    ) -> Result<Self> {
        if !matches!(def.own.as_str(), "b" | "w") || def.opp_n == 0 || def.stop < 0 {
            bail!("own / opp_n / stop の指定が不正です");
        }
        let mut roots = BTreeMap::new();
        let mut root_order = Vec::new();
        let mut game: Edges = BTreeMap::new();
        for (root, ply, moves) in lines {
            work.tick()?;
            if ply == 0 {
                bail!("ply は正数が必要です");
            }
            let pos = position_from_sfen(&format!("{root} {ply}"))?;
            let mut key = strip_ply(&pos.to_sfen()).to_owned();
            if key != root {
                bail!("根の key は正規形が必要です");
            }
            if !roots.contains_key(&key) {
                root_order.push(key.clone());
            }
            roots.entry(key.clone()).or_insert(ply);
            for mv in moves.iter().take(def.depth.saturating_add(1)) {
                work.tick()?;
                let next = child(&key, mv)?;
                game.entry(key).or_default().insert(mv.to_owned());
                key = next;
            }
        }
        if roots.is_empty() || roots.keys().map(|k| side(k)).collect::<BTreeSet<_>>().len() != 1 {
            bail!("根が空か、手番がそろっていません");
        }
        let level = roots.keys().map(|k| (k.clone(), 0)).collect();
        Ok(Self {
            def,
            ply: roots.clone(),
            roots,
            root_order,
            game,
            nodes: BTreeMap::new(),
            reading_conditions: BTreeSet::new(),
            level,
            edge: BTreeMap::new(),
            rule: BTreeMap::new(),
            force: BTreeMap::new(),
            candidates: BTreeMap::new(),
            compare: BTreeMap::new(),
            round: 0,
            round_reports: Vec::new(),
            finished: false,
            ready: false,
            failed: false,
            expanded: BTreeMap::new(),
            waiting: Vec::new(),
        })
    }

    fn root_candidates(&mut self, key: &str) -> Result<()> {
        if self.candidates.contains_key(key) {
            return Ok(());
        }
        let node = &self.nodes[key];
        let mut cands = Vec::new();
        let game = self.game.get(key).cloned().unwrap_or_default();
        if side(key) != self.def.own {
            for pv in node.lines().iter().take(self.def.opp_n) {
                push_unique(&mut cands, &pv.mv);
            }
            for mv in &self.def.extra {
                if child(key, mv).is_ok() {
                    push_unique(&mut cands, mv);
                }
            }
            for mv in &game {
                push_unique(&mut cands, mv);
            }
            self.compare.insert(key.into(), None);
        } else {
            if let Some(first) = node.lines().first() {
                push_unique(&mut cands, &first.mv);
            }
            for mv in &game {
                push_unique(&mut cands, mv);
            }
            if cands.len() == 1
                && let Some(second) = node.lines().get(1)
            {
                push_unique(&mut cands, &second.mv);
            }
            for mv in &self.def.extra {
                if node.lines().iter().any(|p| &p.mv == mv) {
                    push_unique(&mut cands, mv);
                }
            }
            self.compare.insert(
                key.into(),
                node.lines().iter().find(|p| !game.contains(&p.mv)).map(|p| p.mv.clone()),
            );
        }
        self.candidates.insert(key.into(), cands);
        Ok(())
    }

    fn picks(&self, key: &str) -> (BTreeSet<String>, BTreeSet<String>) {
        let mp = self.nodes[key].lines();
        let mut pick = BTreeSet::new();
        let mut extra = BTreeSet::new();
        if !mp.is_empty() {
            if self.roots.contains_key(key) {
                pick.extend(self.candidates[key].iter().cloned());
            } else if mp[0].cp.abs() < self.def.stop {
                let n = if side(key) == self.def.own {
                    1
                } else {
                    self.def.opp_n
                };
                pick.extend(mp.iter().take(n).map(|p| p.mv.clone()));
                extra.extend(self.game.get(key).into_iter().flatten().cloned());
            }
        }
        extra.extend(self.force.get(key).into_iter().flatten().cloned());
        (pick, extra)
    }

    pub fn waiting(&self) -> &[(String, usize)] {
        &self.waiting
    }

    /// 台帳から要求済みの読みだけを取り込み、展開と段の緩和が止まるまで進める。
    pub fn advance(&mut self, cache: &impl Readings) -> Result<Vec<(String, usize)>> {
        self.advance_with_work(cache, &mut Work { remaining: None })
    }

    fn advance_with_work(
        &mut self,
        cache: &impl Readings,
        budget: &mut Work,
    ) -> Result<Vec<(String, usize)>> {
        if self.finished || self.failed {
            return Ok(Vec::new());
        }
        // 不足の局面のどれにも読みが来ていなければ、全局面を辿り直しても同じ不足になる
        // (状態が変わるのは不足の局面の読みを取り込んだときだけ)。毎周の処理を不足の数に比例させる。
        if !self.waiting.is_empty() && self.waiting.iter().all(|(k, _)| cache.get(k).is_none()) {
            return Ok(self.waiting.clone());
        }
        self.waiting.clear();
        loop {
            let mut work: VecDeque<_> = self.level.keys().cloned().collect();
            while let Some(key) = work.pop_front() {
                budget.tick()?;
                if !self.nodes.contains_key(&key) {
                    let Some(reading) = cache.get(&key) else {
                        continue;
                    };
                    self.reading_conditions.insert(cache.fingerprint().map(str::to_owned));
                    self.nodes.insert(key.clone(), reading.clone());
                }
                if self.roots.contains_key(&key) {
                    self.root_candidates(&key)?;
                }
                let level = self.level[&key];
                let ply = self.ply[&key];
                if self.expanded.get(&key) == Some(&(level, ply)) {
                    continue;
                }
                self.expanded.insert(key.clone(), (level, ply));
                if level >= self.def.depth {
                    continue;
                }
                let (pick, extra) = self.picks(&key);
                self.rule.entry(key.clone()).or_default().extend(pick.iter().cloned());
                self.edge.entry(key.clone()).or_default().extend(pick.into_iter().chain(extra));
                // 既存の辺も辿り直し、浅い再発見を子孫と手数の両方へ伝える。
                for mv in self.edge.get(&key).into_iter().flatten() {
                    budget.tick()?;
                    let c = child(&key, mv)?;
                    let next_ply = ply.checked_add(1).context("手数の上限")?;
                    let changed = self.level.get(&c).is_none_or(|d| level + 1 < *d)
                        || self.ply.get(&c).is_none_or(|p| next_ply < *p);
                    self.level
                        .entry(c.clone())
                        .and_modify(|d| *d = (*d).min(level + 1))
                        .or_insert(level + 1);
                    self.ply
                        .entry(c.clone())
                        .and_modify(|p| *p = (*p).min(next_ply))
                        .or_insert(next_ply);
                    if changed {
                        work.push_back(c);
                    }
                }
            }
            let missing: Vec<_> = self
                .level
                .iter()
                .filter(|(k, _)| !self.nodes.contains_key(*k))
                .map(|(k, d)| (k.clone(), *d))
                .collect();
            if !missing.is_empty() {
                self.waiting.clone_from(&missing);
                return Ok(missing);
            }
            if self.round < self.def.rounds {
                let values = self.negamax(&self.reach(&self.edge))?;
                let pending = self.unresolved(&values);
                let mut added = 0;
                for (key, rows) in pending {
                    if self.level[&key] >= self.def.depth {
                        continue;
                    }
                    self.force
                        .entry(key.clone())
                        .or_default()
                        .extend(rows.into_iter().map(|(_, m)| m));
                    self.expanded.remove(&key);
                    added += 1;
                }
                self.round_reports.push((self.round + 2, added));
                if added > 0 {
                    self.round += 1;
                    continue;
                }
            }
            self.finished = true;
            return Ok(Vec::new());
        }
    }

    /// 終わった木の読みと辺を手放す (状態の旗と出力先だけを残す)。
    pub fn shrink(&mut self) {
        self.roots.clear();
        self.root_order.clear();
        self.game.clear();
        self.nodes.clear();
        self.level.clear();
        self.ply.clear();
        self.edge.clear();
        self.rule.clear();
        self.force.clear();
        self.candidates.clear();
        self.compare.clear();
        self.expanded.clear();
        self.waiting.clear();
    }

    pub fn reach(&self, edges: &Edges) -> BTreeSet<String> {
        self.reach_from(self.roots.keys().cloned().collect(), edges)
    }
    pub fn reach_from(&self, mut stack: Vec<String>, edges: &Edges) -> BTreeSet<String> {
        let mut seen = BTreeSet::new();
        while let Some(key) = stack.pop() {
            if !self.nodes.contains_key(&key) || !seen.insert(key.clone()) {
                continue;
            }
            stack.extend(edges.get(&key).into_iter().flatten().filter_map(|m| child(&key, m).ok()));
        }
        seen
    }

    pub fn raw_rows(&self, key: &str) -> BTreeMap<String, i32> {
        self.nodes[key].lines().iter().map(|p| (p.mv.clone(), p.cp)).collect()
    }

    pub fn negamax(&self, nodes: &BTreeSet<String>) -> Result<Values> {
        let mut children: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for key in nodes {
            let moves: BTreeSet<_> = self.nodes[key]
                .lines()
                .iter()
                .map(|p| p.mv.clone())
                .chain(self.edge.get(key).into_iter().flatten().cloned())
                .collect();
            for mv in moves {
                let c = child(key, &mv)?;
                if nodes.contains(&c) {
                    children.entry(key.clone()).or_default().push((mv, c));
                }
            }
        }
        fn val(
            tree: &Tree,
            key: &str,
            children: &BTreeMap<String, Vec<(String, String)>>,
            path: &mut BTreeSet<String>,
        ) -> (i32, BTreeMap<String, i32>) {
            let mut rows = tree.raw_rows(key);
            path.insert(key.to_owned());
            for (mv, c) in children.get(key).into_iter().flatten() {
                let value = if path.contains(c) {
                    tree.def.draw
                } else {
                    -val(tree, c, children, path).0
                };
                rows.insert(mv.clone(), value);
            }
            path.remove(key);
            // 宣言勝ちの葉は指し手の行を持たないので、`pv win` の行の値を局面の値にする
            // (d*.jsonl を読む後段の登録はこの値を使う)。行が無ければ他の葉と同じ 0。
            let reading = &tree.nodes[key];
            let win = (rows.is_empty() && reading.bestmove == "win")
                .then(|| reading.multipv.first().map(|p| p.cp))
                .flatten();
            (rows.values().copied().max().or(win).unwrap_or(0), rows)
        }
        Ok(nodes
            .iter()
            .map(|key| (key.clone(), val(self, key, &children, &mut BTreeSet::new()).1))
            .collect())
    }

    pub fn unresolved(&self, values: &Values) -> Pending {
        let mut pending = BTreeMap::new();
        for (key, rows) in values {
            let Some(edges) = self.edge.get(key).filter(|e| !e.is_empty()) else {
                continue;
            };
            if side(key) != self.def.own {
                continue;
            }
            let best = edges
                .iter()
                .map(|m| rows.get(m).copied().unwrap_or(-99999))
                .max()
                .unwrap_or(-99999);
            let un: Vec<_> = self.nodes[key]
                .lines()
                .iter()
                .filter(|p| !edges.contains(&p.mv) && p.cp > best)
                .map(|p| (p.cp, p.mv.clone()))
                .collect();
            if !un.is_empty() {
                pending.insert(key.clone(), un);
            }
        }
        pending
    }

    pub fn rank(
        &self,
        key: &str,
        rows: &BTreeMap<String, i32>,
        moves: impl IntoIterator<Item = String>,
    ) -> Vec<String> {
        let mp = self.nodes[key].lines();
        let mut moves: Vec<_> = moves.into_iter().collect();
        moves.sort_by(|a, b| {
            rows.get(b)
                .unwrap_or(&-99999)
                .cmp(rows.get(a).unwrap_or(&-99999))
                .then_with(|| {
                    mp.iter()
                        .position(|p| &p.mv == a)
                        .unwrap_or(usize::MAX)
                        .cmp(&mp.iter().position(|p| &p.mv == b).unwrap_or(usize::MAX))
                })
                .then_with(|| a.cmp(b))
        });
        moves
    }
}

impl Tree {
    /// 未読の依存と循環の祖先を除き、残った局面を子から順に一度だけ評価する。
    #[cfg(test)]
    fn prediction_values(&self, work: &mut Work) -> Result<Values> {
        self.prediction_values_report(work, &mut PredictionReport::default())
    }

    fn prediction_values_report(
        &self,
        work: &mut Work,
        report: &mut PredictionReport,
    ) -> Result<Values> {
        let mut keys = Vec::new();
        let mut index = BTreeMap::new();
        for key in self.level.keys() {
            work.tick()?;
            index.insert(key.as_str(), keys.len());
            keys.push(key);
        }
        let n = keys.len();
        let mut edges = vec![Vec::new(); n];
        let mut reverse = vec![Vec::new(); n];
        for (i, key) in keys.iter().enumerate() {
            work.tick()?;
            let Some(reading) = self.nodes.get(*key) else {
                continue;
            };
            let mut moves = BTreeSet::new();
            for mv in reading
                .lines()
                .iter()
                .map(|p| &p.mv)
                .chain(self.edge.get(*key).into_iter().flatten())
            {
                work.tick()?;
                moves.insert(mv);
            }
            for mv in moves {
                work.tick()?;
                let c = child(key, mv)?;
                if let Some(&j) = index.get(c.as_str()) {
                    edges[i].push((mv.clone(), j));
                    reverse[j].push(i);
                }
            }
        }
        let mut seen = vec![false; n];
        let mut order = Vec::new();
        for root in 0..n {
            work.tick()?;
            if seen[root] {
                continue;
            }
            seen[root] = true;
            let mut stack = vec![(root, 0)];
            while let Some((i, next)) = stack.last_mut() {
                work.tick()?;
                if let Some((_, j)) = edges[*i].get(*next) {
                    *next += 1;
                    if !seen[*j] {
                        seen[*j] = true;
                        stack.push((*j, 0));
                    }
                } else {
                    order.push(*i);
                    stack.pop();
                }
            }
        }
        let mut component = vec![usize::MAX; n];
        let mut sizes = Vec::new();
        for &root in order.iter().rev() {
            work.tick()?;
            if component[root] != usize::MAX {
                continue;
            }
            let id = sizes.len();
            let mut size = 0;
            component[root] = id;
            let mut stack = vec![root];
            while let Some(i) = stack.pop() {
                work.tick()?;
                size += 1;
                for &j in &reverse[i] {
                    work.tick()?;
                    if component[j] == usize::MAX {
                        component[j] = id;
                        stack.push(j);
                    }
                }
            }
            sizes.push(size);
        }
        let mut invalid = vec![0u8; n];
        let mut stack = Vec::new();
        for i in 0..n {
            work.tick()?;
            let mut self_loop = false;
            for (_, j) in &edges[i] {
                work.tick()?;
                self_loop |= *j == i;
            }
            invalid[i] = u8::from(!self.nodes.contains_key(keys[i]))
                | (u8::from(sizes[component[i]] > 1 || self_loop) << 1);
            if invalid[i] != 0 {
                stack.push(i);
            }
        }
        while let Some(i) = stack.pop() {
            work.tick()?;
            for &j in &reverse[i] {
                work.tick()?;
                if invalid[j] | invalid[i] != invalid[j] {
                    invalid[j] |= invalid[i];
                    stack.push(j);
                }
            }
        }
        let mut values = Values::new();
        let mut best = vec![0; n];
        for i in order {
            work.tick()?;
            if invalid[i] != 0 {
                if side(keys[i]) == self.def.own
                    && self.level[keys[i]] < self.def.depth
                    && self.edge.get(keys[i]).is_some_and(|e| !e.is_empty())
                {
                    if invalid[i] & 1 != 0 {
                        report.unread.insert(keys[i].clone());
                    }
                    if invalid[i] & 2 != 0 {
                        report.cyclic.insert(keys[i].clone());
                    }
                }
                continue;
            }
            let reading = &self.nodes[keys[i]];
            let mut rows = BTreeMap::new();
            for pv in reading.lines() {
                work.tick()?;
                rows.insert(pv.mv.clone(), pv.cp);
            }
            for (mv, j) in &edges[i] {
                work.tick()?;
                rows.insert(mv.clone(), -best[*j]);
            }
            let mut maximum = None::<i32>;
            for value in rows.values() {
                work.tick()?;
                maximum = Some(maximum.map_or(*value, |old| old.max(*value)));
            }
            best[i] = maximum.unwrap_or_else(|| {
                if reading.bestmove == "win" {
                    reading.multipv.first().map_or(0, |p| p.cp)
                } else {
                    0
                }
            });
            values.insert(keys[i].clone(), rows);
        }
        Ok(values)
    }

    #[cfg(test)]
    fn predict(&self, work: &mut Work) -> Result<Edges> {
        self.predict_report(work, &mut PredictionReport::default())
    }

    fn predict_report(&self, work: &mut Work, report: &mut PredictionReport) -> Result<Edges> {
        let values = self.prediction_values_report(work, report)?;
        let mut predicted = Edges::new();
        for (key, rows) in values {
            work.tick()?;
            if side(&key) != self.def.own || self.level[&key] >= self.def.depth {
                continue;
            }
            let Some(edges) = self.edge.get(&key).filter(|e| !e.is_empty()) else {
                continue;
            };
            let mut best = i32::MIN;
            for mv in edges {
                work.tick()?;
                best = best.max(rows.get(mv).copied().unwrap_or(-99999));
            }
            for pv in self.nodes[&key].lines() {
                work.tick()?;
                if !edges.contains(&pv.mv) && pv.cp > best {
                    predicted.entry(key.clone()).or_default().insert(pv.mv.clone());
                }
            }
        }
        Ok(predicted)
    }

    #[cfg(test)]
    fn shadow(
        &self,
        lines: &[GameLine],
        predicted: Edges,
        cache: &impl Readings,
        work: &mut Work,
    ) -> Result<Self> {
        self.shadow_borrowed(
            lines.iter().map(|(key, ply, moves)| (key.as_str(), *ply, moves.as_slice())),
            predicted,
            cache,
            work,
        )
    }

    fn shadow_borrowed<'a>(
        &self,
        lines: impl IntoIterator<Item = (&'a str, u32, &'a [String])>,
        predicted: Edges,
        cache: &impl Readings,
        work: &mut Work,
    ) -> Result<Self> {
        let mut def = self.def.clone();
        def.rounds = 0;
        let mut shadow = Self::build_borrowed(def, lines, work)?;
        for (key, moves) in self.force.iter().chain(predicted.iter()) {
            work.tick()?;
            for mv in moves {
                work.tick()?;
                shadow.force.entry(key.clone()).or_default().insert(mv.clone());
            }
        }
        shadow.advance_with_work(cache, work)?;
        Ok(shadow)
    }

    /// 同じ定義と台帳から影を作る。上限に達した試行の候補は全て捨てる。
    /// 呼び出し側は候補から本体の不足・実行中・失敗上限のキーを除く。
    #[cfg(test)]
    pub fn prefetch(
        &self,
        lines: &[GameLine],
        cache: &impl Readings,
        hold: bool,
        limit: usize,
    ) -> Result<Vec<PrefetchCandidate>> {
        Ok(self.prefetch_report(lines, cache, hold, limit)?.candidates)
    }

    /// 上限による破棄と、読み依存による予測の除外を区別する。
    #[cfg(test)]
    pub fn prefetch_report(
        &self,
        lines: &[GameLine],
        cache: &impl Readings,
        hold: bool,
        limit: usize,
    ) -> Result<PredictionReport> {
        self.prefetch_report_borrowed(
            lines.iter().map(|(key, ply, moves)| (key.as_str(), *ply, moves.as_slice())),
            cache,
            hold,
            limit,
        )
    }

    /// 入力を複製せず、予算の範囲で影の手順を辿る。
    pub fn prefetch_report_borrowed<'a>(
        &self,
        lines: impl IntoIterator<Item = (&'a str, u32, &'a [String])>,
        cache: &impl Readings,
        hold: bool,
        limit: usize,
    ) -> Result<PredictionReport> {
        let mut report = PredictionReport::default();
        if hold
            || self.finished
            || self.failed
            || self.def.depth == 0
            || self.round >= self.def.rounds
        {
            return Ok(report);
        }
        let mut work = Work {
            remaining: Some(limit),
        };
        let attempt: Result<Vec<PrefetchCandidate>> = (|| {
            let predicted = self.predict_report(&mut work, &mut report)?;
            let shadow = self.shadow_borrowed(lines, predicted, cache, &mut work)?;
            let mut candidates = Vec::new();
            for (key, level) in shadow.waiting {
                work.tick()?;
                candidates.push(PrefetchCandidate {
                    ply: shadow.ply[&key],
                    key,
                    level,
                });
            }
            Ok(candidates)
        })();
        report.work = limit - work.remaining.unwrap_or(0);
        match attempt {
            Err(err) if err.is::<WorkLimit>() => {
                report.limited = true;
                report.unread.clear();
                report.cyclic.clear();
            }
            result => report.candidates = result?,
        }
        Ok(report)
    }
}

pub(super) fn remove_candidates(out: &Path) -> Result<()> {
    for name in ["own_nodes.tsv", super::output::NODE_MOVES] {
        match std::fs::remove_file(out.join(name)) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
    }
    Ok(())
}
fn push_unique(moves: &mut Vec<String>, mv: &str) {
    if !moves.iter().any(|m| m == mv) {
        moves.push(mv.into());
    }
}

#[cfg(test)]
mod prefetch_tests {
    use super::super::journal::Pv;
    use super::*;
    const ROOT: &str = "4k4/9/9/9/9/9/9/9/4K4 b -";

    fn reading(key: &str, rows: &[(&str, i32)]) -> Reading {
        Reading {
            tag: "test".into(),
            key: key.into(),
            ply: 1,
            nodes_limit: 1,
            nodes: Some(1),
            sec: 1.0,
            threads: 1,
            hash: 1,
            draw: BTreeMap::new(),
            bestmove: rows.first().map_or("resign", |r| r.0).into(),
            multipv: rows
                .iter()
                .map(|(mv, cp)| Pv {
                    mv: (*mv).into(),
                    cp: *cp,
                    depth: 1,
                })
                .collect(),
        }
    }
    fn params() -> TreeParams {
        TreeParams {
            out: "unused".into(),
            depth: 3,
            opp_n: 1,
            stop: 300,
            own: "b".into(),
            rounds: 1,
            extra: vec![],
            draw: 17,
        }
    }
    fn fixture() -> (Tree, Vec<GameLine>, BTreeMap<String, Reading>) {
        let a = child(ROOT, "5i4i").unwrap();
        let c = child(ROOT, "5i5h").unwrap();
        let second = child(&a, "5a4a").unwrap();
        let lines = vec![(ROOT.into(), 11, vec![]), (second, 7, vec![])];
        let cache = BTreeMap::from([
            (ROOT.into(), reading(ROOT, &[("5i4i", 100), ("5i5h", 80), ("5i6i", 60)])),
            (a.clone(), reading(&a, &[("5a5b", 500)])),
            (c.clone(), reading(&c, &[("5a5b", 500)])),
        ]);
        let mut tree = Tree::build(params(), &lines).unwrap();
        tree.advance(&cache).unwrap();
        (tree, lines, cache)
    }
    fn unlimited() -> Work {
        Work { remaining: None }
    }
    fn normalized(edges: &Edges) -> Edges {
        edges
            .iter()
            .filter(|(_, e)| !e.is_empty())
            .map(|(k, e)| (k.clone(), e.clone()))
            .collect()
    }
    fn assert_shadow(tree: &Tree, shadow: &Tree) {
        assert_eq!(tree.nodes.keys().collect::<Vec<_>>(), shadow.nodes.keys().collect::<Vec<_>>());
        assert_eq!(tree.level, shadow.level);
        assert_eq!(tree.ply, shadow.ply);
        assert_eq!(normalized(&tree.edge), normalized(&shadow.edge));
        assert_eq!(normalized(&tree.rule), normalized(&shadow.rule));
        assert_eq!(tree.candidates, shadow.candidates);
        assert_eq!(tree.compare, shadow.compare);
    }

    #[test]
    fn prediction_uses_raw_unexpanded_move_and_does_not_mutate_tree() {
        let (tree, lines, cache) = fixture();
        let before = format!("{tree:?}");
        let predicted = tree.predict(&mut unlimited()).unwrap();
        assert_eq!(predicted[ROOT], BTreeSet::from(["5i6i".into()]));
        let shadow = tree.shadow(&lines, predicted, &cache, &mut unlimited()).unwrap();
        assert!(tree.nodes.keys().all(|k| shadow.nodes.contains_key(k)));
        let candidates = tree.prefetch(&lines, &cache, false, PREFETCH_WORK).unwrap();
        let b = child(ROOT, "5i6i").unwrap();
        assert!(candidates.contains(&PrefetchCandidate {
            key: b,
            level: 1,
            ply: 12
        }));
        assert_eq!(format!("{tree:?}"), before);
        assert_shadow(&tree, &tree.shadow(&lines, Edges::new(), &cache, &mut unlimited()).unwrap());
    }

    #[test]
    fn unread_grandchild_and_level_multipv_dependency_block_prediction() {
        let (mut tree, _, cache) = fixture();
        let a = child(ROOT, "5i4i").unwrap();
        let grandchild = child(&a, "5a5b").unwrap();
        tree.level.insert(grandchild.clone(), 2);
        assert!(!tree.predict(&mut unlimited()).unwrap().contains_key(ROOT));
        let mut report = PredictionReport::default();
        tree.prediction_values_report(&mut unlimited(), &mut report).unwrap();
        assert!(report.unread.contains(ROOT));
        assert!(!report.cyclic.contains(ROOT));
        tree.nodes.insert(grandchild.clone(), reading(&grandchild, &[]));
        assert!(tree.prediction_values(&mut unlimited()).unwrap().contains_key(ROOT));
        let b = child(ROOT, "5i6i").unwrap();
        tree.level.insert(b, 1);
        assert!(!tree.prediction_values(&mut unlimited()).unwrap().contains_key(ROOT));
        assert_eq!(cache.len(), 3);
    }

    #[test]
    fn cyclic_components_and_their_ancestors_are_excluded() {
        let (mut tree, _, _) = fixture();
        let mut key = ROOT.to_owned();
        for (depth, mv) in ["5i4i", "5a4a", "4i5i", "4a5a"].into_iter().enumerate() {
            tree.level.insert(key.clone(), depth);
            tree.nodes.insert(key.clone(), reading(&key, &[(mv, 10)]));
            tree.edge.insert(key.clone(), BTreeSet::from([mv.into()]));
            key = child(&key, mv).unwrap();
        }
        assert_eq!(key, ROOT);
        assert!(!tree.prediction_values(&mut unlimited()).unwrap().contains_key(ROOT));
        let ancestor = child(ROOT, "5i5h").unwrap();
        let parent = child(&ancestor, "5a4a").unwrap();
        tree.level.insert(parent.clone(), 0);
        tree.nodes.insert(parent.clone(), reading(&parent, &[("5h5i", 0)]));
        assert!(!tree.prediction_values(&mut unlimited()).unwrap().contains_key(&parent));
        tree.edge.insert(parent.clone(), BTreeSet::from(["5h5i".into()]));
        let mut report = PredictionReport::default();
        tree.prediction_values_report(&mut unlimited(), &mut report).unwrap();
        assert!(report.cyclic.contains(ROOT));
        assert!(report.cyclic.contains(&parent));
    }

    #[test]
    fn memoized_values_equal_negamax_on_acyclic_dependencies() {
        let (tree, _, _) = fixture();
        let nodes = tree.nodes.keys().cloned().collect();
        assert_eq!(
            tree.prediction_values(&mut unlimited()).unwrap(),
            tree.negamax(&nodes).unwrap()
        );
    }

    #[test]
    fn inactive_and_exhausted_trees_do_not_predict() {
        let (mut tree, lines, cache) = fixture();
        assert!(tree.prefetch(&lines, &cache, true, PREFETCH_WORK).unwrap().is_empty());
        tree.round = tree.def.rounds;
        assert!(tree.prefetch(&lines, &cache, false, PREFETCH_WORK).unwrap().is_empty());
        tree.round = 0;
        tree.failed = true;
        assert!(tree.prefetch(&lines, &cache, false, PREFETCH_WORK).unwrap().is_empty());
        tree.failed = false;
        tree.finished = true;
        assert!(tree.prefetch(&lines, &cache, false, PREFETCH_WORK).unwrap().is_empty());
        tree.finished = false;
        tree.def.depth = 0;
        assert!(tree.prefetch(&lines, &cache, false, PREFETCH_WORK).unwrap().is_empty());
    }

    #[test]
    fn budget_stops_build_graph_and_expansion_without_partial_candidates() {
        let (tree, lines, cache) = fixture();
        for limit in 0..30 {
            assert!(tree.prefetch(&lines, &cache, false, limit).unwrap().is_empty());
        }
        let mut work = Work { remaining: Some(2) };
        let long = vec![(ROOT.into(), 1, vec!["5i4i".into(), "5a4a".into(), "4i5i".into()])];
        assert!(Tree::build_with_work(params(), &long, &mut work).unwrap_err().is::<WorkLimit>());
        assert_eq!(work.remaining, Some(0));
        let mut shadow = Tree::build(params(), &lines).unwrap();
        let mut work = Work { remaining: Some(2) };
        assert!(shadow.advance_with_work(&cache, &mut work).unwrap_err().is::<WorkLimit>());
        assert_eq!(work.remaining, Some(0));
        let mut work = Work { remaining: Some(2) };
        assert!(tree.prediction_values(&mut work).unwrap_err().is::<WorkLimit>());
        assert_eq!(work.remaining, Some(0));
    }

    #[test]
    fn stable_prediction_contains_round_end_extensions() {
        let (mut tree, lines, mut cache) = fixture();
        let predicted = tree.predict(&mut unlimited()).unwrap();
        let other = lines[1].0.clone();
        cache.insert(other.clone(), reading(&other, &[]));
        tree.advance(&cache).unwrap();
        assert_eq!(tree.round, 1);
        assert!(
            tree.force
                .iter()
                .all(|(key, moves)| moves.is_subset(predicted.get(key).unwrap()))
        );
    }

    #[test]
    fn shadow_matches_each_partial_layer_with_force_stop_and_multiple_roots() {
        let (original, mut lines, mut full) = fixture();
        lines[0].2 = vec!["5i4i".into(), "5a4a".into(), "4i5i".into()];
        let second = lines[1].0.clone();
        full.insert(second.clone(), reading(&second, &[("4i5i", 500)]));
        let mut tree = Tree::build(original.def, &lines).unwrap();
        tree.def.rounds = 0;
        tree.force.insert(second, BTreeSet::from(["4i5i".into()]));
        let mut cache = BTreeMap::new();
        for _ in 0..5 {
            let missing = tree.advance(&cache).unwrap();
            assert_shadow(
                &tree,
                &tree.shadow(&lines, Edges::new(), &cache, &mut unlimited()).unwrap(),
            );
            for (key, _) in missing {
                cache.insert(
                    key.clone(),
                    full.get(&key).cloned().unwrap_or_else(|| reading(&key, &[])),
                );
            }
        }
    }
    #[test]
    fn simulation_reuse_matches_rebuilding_with_deferred_shadows_and_shared_keys() {
        use super::super::simulation::{
            PrefetchOptions, Sample, Timing, preflight, simulate, simulate_prefetch,
        };
        use std::sync::Arc;
        let (tree, lines, mut cache) = fixture();
        let other = lines[1].0.clone();
        let mut slow = reading(&other, &[]);
        slow.sec = 10.0;
        cache.insert(other, slow);
        let b = child(ROOT, "5i6i").unwrap();
        let mut answer = reading(&b, &[("5a5b", 500)]);
        answer.sec = 20.0;
        cache.insert(b.clone(), answer);
        let mut samples = Vec::new();
        for order in 0..7 {
            let mut sample = Sample {
                params: tree.def.clone(),
                lines: lines.clone(),
                answers: Arc::new(cache.clone()),
                requested: BTreeSet::new(),
                fingerprint: "test".into(),
                priority: 100,
                order,
            };
            preflight(&mut sample).unwrap();
            samples.push(sample);
        }
        let unrelated = child(&b, "5a4a").unwrap();
        let mut unused_answer = cache[&b].clone();
        unused_answer.sec = 1000.0;
        let mut unrelated_answer = reading(&unrelated, &[]);
        unrelated_answer.sec = 1.5;
        let mut extra = Sample {
            params: TreeParams {
                depth: 0,
                ..tree.def.clone()
            },
            lines: vec![(unrelated.clone(), 1, vec![])],
            answers: Arc::new(BTreeMap::from([
                (unrelated.clone(), unrelated_answer),
                (b.clone(), unused_answer),
            ])),
            requested: BTreeSet::new(),
            fingerprint: "unrelated".into(),
            priority: 100,
            order: 8,
        };
        preflight(&mut extra).unwrap();
        samples.push(extra);
        let samples: Vec<_> = samples.iter().collect();
        let baseline = simulate(&samples, 5, Timing::Maximum).unwrap();
        for workers in [0, 1, 2, 4] {
            let options = PrefetchOptions {
                workers,
                rebuilds_per_turn: 1,
                record_candidates: true,
                ..Default::default()
            };
            let reuse = simulate_prefetch(&samples, 5, options).unwrap();
            let fresh = simulate_prefetch(
                &samples,
                5,
                PrefetchOptions {
                    reuse: false,
                    ..options
                },
            )
            .unwrap();
            assert_eq!(reuse.candidates, fresh.candidates);
            assert_eq!(reuse.trees_done, fresh.trees_done);
            assert_eq!(reuse.workers_done, fresh.workers_done);
            for (actual, expected) in reuse.trees.iter().zip(&baseline.trees) {
                assert_shadow(expected, actual);
                assert_eq!(actual.force, expected.force);
            }
            if workers == 0 {
                assert_eq!(reuse.trees_done, baseline.elapsed);
                assert_eq!(reuse.read, 0);
            } else {
                assert!(fresh.rebuilds > reuse.rebuilds);
                assert_eq!(reuse.read, 1);
                assert_eq!(reuse.used, 1);
                assert_eq!(reuse.absent, 0);
                assert_eq!(reuse.same_wait, 12.0);
                assert_eq!(reuse.other_wait, 0.0);
                assert_eq!(reuse.trees_done, 22.0);
                assert!(reuse.trees_done < baseline.elapsed);
            }
        }
    }
    #[test]
    fn prefetch_worker_cap_changes_distribution_and_completion() {
        use super::super::simulation::{PrefetchOptions, Sample, preflight, simulate_prefetch};
        use std::sync::Arc;
        let (tree, lines, mut cache) = fixture();
        let other = lines[1].0.clone();
        let mut slow = reading(&other, &[]);
        slow.sec = 10.0;
        cache.insert(other, slow);
        cache.insert(
            ROOT.into(),
            reading(ROOT, &[("5i4i", 100), ("5i5h", 80), ("5i6i", 60), ("5i4h", 50)]),
        );
        for mv in ["5i6i", "5i4h"] {
            let key = child(ROOT, mv).unwrap();
            let mut answer = reading(&key, &[("5a5b", 500)]);
            answer.sec = 20.0;
            cache.insert(key, answer);
        }
        let mut sample = Sample {
            params: tree.def,
            lines,
            answers: Arc::new(cache),
            requested: BTreeSet::new(),
            fingerprint: "test".into(),
            priority: 0,
            order: 0,
        };
        preflight(&mut sample).unwrap();
        let run = |workers| {
            simulate_prefetch(
                &[&sample],
                5,
                PrefetchOptions {
                    workers,
                    record_candidates: true,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let one = run(1);
        let two = run(2);
        assert!(one.candidates.iter().any(|(_, keys)| keys.len() == 2));
        assert_eq!((one.max_concurrent, two.max_concurrent), (1, 2));
        assert_eq!((one.read, two.read), (1, 2));
        assert_eq!((one.no_slot, two.no_slot), (1, 0));
        assert_eq!((one.trees_done, two.trees_done), (30.0, 22.0));
        assert_eq!((one.added, two.added), (1, 1));
        assert_eq!((one.added_moves, two.added_moves), (2, 2));
        assert_eq!((one.same_positive, two.same_positive), (1, 2));
        assert_eq!((one.same_max, two.same_max), (12.0, 12.0));
        assert_eq!(one.other_positive, 0);
    }

    #[test]
    fn prefetch_report_separates_limit_from_empty_candidates() {
        let (tree, lines, cache) = fixture();
        let limited = tree.prefetch_report(&lines, &cache, false, 0).unwrap();
        assert!(limited.limited);
        assert!(limited.candidates.is_empty());
        let held = tree.prefetch_report(&lines, &cache, true, PREFETCH_WORK).unwrap();
        assert!(!held.limited);
        assert!(held.candidates.is_empty());
    }

    #[test]
    fn simulation_missing_prediction_stops_and_drains_after_tree_completion() {
        use super::super::simulation::{
            PrefetchOptions, Sample, Timing, preflight, simulate_prefetch,
        };
        use std::sync::Arc;
        let (tree, _, mut cache) = fixture();
        let q = "9/5k3/9/9/9/9/9/5K3/9 b -";
        let r = child(q, "4h4i").unwrap();
        // 合流先が level に入ると、STOP で止まった枝の MultiPV の値も伝播で変わる。
        let d = child(&r, "4b5b").unwrap();
        let a = child(ROOT, "5i4i").unwrap();
        assert_eq!(d, child(&a, "5a5b").unwrap());
        let mut slow = reading(q, &[("4h4i", 0)]);
        slow.sec = 10.0;
        cache.insert(q.into(), slow);
        let mut r_answer = reading(&r, &[("4b5b", 0)]);
        r_answer.sec = 0.0;
        cache.insert(r.clone(), r_answer);
        let mut d_answer = reading(&d, &[("4i3i", 1000)]);
        d_answer.sec = 0.0;
        cache.insert(d.clone(), d_answer);
        let mut sample = Sample {
            params: tree.def,
            lines: vec![(ROOT.into(), 1, vec![]), (q.into(), 1, vec![])],
            answers: Arc::new(cache),
            requested: BTreeSet::new(),
            fingerprint: "test".into(),
            priority: 0,
            order: 0,
        };
        preflight(&mut sample).unwrap();
        let b = child(ROOT, "5i6i").unwrap();
        assert!(!sample.requested.contains(&b));
        let mean = simulate_prefetch(&[&sample], 4, PrefetchOptions::default()).unwrap();
        let max = simulate_prefetch(
            &[&sample],
            4,
            PrefetchOptions {
                fallback: Timing::Maximum,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(mean.absent, 1);
        assert_eq!(max.absent, 1);
        assert_eq!(mean.read, 1);
        assert_eq!(mean.used, 0);
        assert_eq!(max.wasted_seconds, 10.0);
        assert!(mean.wasted_seconds < max.wasted_seconds);
        assert_eq!(mean.trees_done, max.trees_done);
        assert!(max.workers_done > max.trees_done);
    }
    #[test]
    fn borrowed_input_is_only_visited_within_budget() {
        let (tree, _, cache) = fixture();
        for count in [10, 1_000_000] {
            let visited = std::cell::Cell::new(0);
            let input = || {
                (0..count).map(|_| {
                    visited.set(visited.get() + 1);
                    (ROOT, 1, [].as_slice())
                })
            };
            let mut budget = Work { remaining: Some(1) };
            assert!(
                Tree::build_borrowed(params(), input(), &mut budget)
                    .unwrap_err()
                    .is::<WorkLimit>()
            );
            assert_eq!(visited.get(), 2);
            visited.set(0);
            let report = tree.prefetch_report_borrowed(input(), &cache, false, 1).unwrap();
            assert!(report.limited);
            assert_eq!(report.work, 1);
            assert!(visited.get() <= 2);
        }
    }

    #[test]
    fn large_input_stops_exactly_at_the_work_limit() {
        let lines: Vec<GameLine> = (0..6000)
            .map(|ply| (ROOT.into(), ply + 1, vec!["5i4i".into(), "5a4a".into()]))
            .collect();
        let mut budget = Work {
            remaining: Some(50),
        };
        let error = Tree::build_with_work(params(), &lines, &mut budget).unwrap_err();
        assert!(error.is::<WorkLimit>());
        assert_eq!(budget.remaining, Some(0));
        let mut tree = Tree::build(params(), &[(ROOT.into(), 1, vec![])]).unwrap();
        for n in 0..6000 {
            tree.level.insert(format!("unread-{n}"), 1);
        }
        let mut budget = Work {
            remaining: Some(50),
        };
        assert!(
            tree.advance_with_work(&BTreeMap::<String, Reading>::new(), &mut budget)
                .unwrap_err()
                .is::<WorkLimit>()
        );
        assert_eq!(budget.remaining, Some(0));
    }

    #[test]
    fn shadow_preserves_opponent_root_extra_and_own_root_comparison() {
        let (_, lines, full) = fixture();
        for own in ["b", "w"] {
            let mut def = params();
            def.own = own.into();
            def.stop = 0;
            def.extra = vec!["5i6i".into()];
            def.rounds = 0;
            let mut tree = Tree::build(def, &lines).unwrap();
            let mut cache = BTreeMap::new();
            for _ in 0..4 {
                let missing = tree.advance(&cache).unwrap();
                assert_shadow(
                    &tree,
                    &tree.shadow(&lines, Edges::new(), &cache, &mut unlimited()).unwrap(),
                );
                for (key, _) in missing {
                    cache.insert(
                        key.clone(),
                        full.get(&key).cloned().unwrap_or_else(|| reading(&key, &[])),
                    );
                }
            }
        }
    }
    #[test]
    fn simulation_separates_other_prefetch_occupancy_from_same_key_wait() {
        use super::super::simulation::{
            PrefetchOptions, Sample, Timing, preflight, simulate_prefetch,
        };
        use std::sync::Arc;
        let (tree, _, mut cache) = fixture();
        let q = "9/5k3/9/9/9/9/9/5K3/9 b -";
        let r = child(q, "4h4i").unwrap();
        let s = child(q, "4h5h").unwrap();
        let d = child(&r, "4b5b").unwrap();
        let b = child(ROOT, "5i6i").unwrap();
        let mut slow = reading(q, &[("4h4i", 10), ("4h5h", 0)]);
        slow.sec = 10.0;
        cache.insert(q.into(), slow);
        cache.insert(r.clone(), reading(&r, &[("4b5b", 0)]));
        cache.insert(s.clone(), reading(&s, &[("4b5b", 500)]));
        let mut leaf = reading(&d, &[("4i3i", 1000)]);
        leaf.sec = 0.0;
        cache.insert(d, leaf);
        cache.insert(b.clone(), reading(&b, &[("5a5b", 0)]));
        let mut sample = Sample {
            params: tree.def,
            lines: vec![(ROOT.into(), 1, vec![]), (q.into(), 1, vec![])],
            answers: Arc::new(cache),
            requested: BTreeSet::new(),
            fingerprint: "test".into(),
            priority: 0,
            order: 0,
        };
        preflight(&mut sample).unwrap();
        assert!(!sample.requested.contains(&b));
        let result = simulate_prefetch(
            &[&sample],
            2,
            PrefetchOptions {
                workers: 1,
                fallback: Timing::Maximum,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(result.read, 2);
        assert_eq!(result.used, 0);
        assert_eq!(result.absent, 1);
        assert_eq!(result.same_wait, 0.0);
        assert_eq!(result.other_wait, 1.0);
        assert_eq!(result.other_p95, 1.0);
        assert_eq!(result.workers_done, 14.0);
        assert_eq!(result.wasted_seconds, 11.0);
    }
    #[test]
    fn prediction_comparison_matches_unresolved_below_missing_move_sentinel() {
        let (mut tree, _, _) = fixture();
        for mv in ["5i4i", "5i5h"] {
            let key = child(ROOT, mv).unwrap();
            tree.nodes.insert(key.clone(), reading(&key, &[("5a5b", 100_000)]));
        }
        tree.nodes.get_mut(ROOT).unwrap().multipv[2].cp = -99_999;
        let values = tree.prediction_values(&mut unlimited()).unwrap();
        let expected: Edges = tree
            .unresolved(&values)
            .into_iter()
            .map(|(key, rows)| (key, rows.into_iter().map(|(_, mv)| mv).collect()))
            .collect();
        let predicted = tree.predict(&mut unlimited()).unwrap();
        assert_eq!(predicted[ROOT], BTreeSet::from(["5i6i".into()]));
        assert_eq!(predicted, expected);
    }
}
