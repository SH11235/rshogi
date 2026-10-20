use super::journal::{Reading, Settings, digest};
use crate::book_db::{child_position_after_move, position_from_sfen, strip_ply};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

type Edges = BTreeMap<String, BTreeSet<String>>;
pub(super) type Values = BTreeMap<String, BTreeMap<String, i32>>;
pub(super) type Pending = BTreeMap<String, Vec<(i32, String)>>;

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

pub(super) fn child(key: &str, mv: &str) -> Result<String> {
    Ok(strip_ply(&child_position_after_move(&format!("{key} 1"), mv)?.to_sfen()).to_owned())
}
pub(super) fn side(key: &str) -> &str {
    key.split_whitespace().nth(1).unwrap_or("")
}

#[derive(Debug)]
pub(super) struct Tree {
    pub def: Definition,
    pub roots: BTreeMap<String, u32>,
    pub root_order: Vec<String>,
    pub game: Edges,
    pub nodes: BTreeMap<String, Reading>,
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
}
impl Tree {
    pub fn load(def: Definition, settings: &Settings) -> Result<Self> {
        std::fs::create_dir_all(&def.out)?;
        remove_candidates(&def.out)?;
        if !matches!(def.own.as_str(), "b" | "w") || def.opp_n == 0 || def.stop < 0 {
            bail!("own / opp_n / stop の指定が不正です");
        }
        if def.fingerprint.as_ref().is_some_and(|fp| fp != &settings.fingerprint) {
            bail!("木の読み条件とプロセスの指紋が違います");
        }
        if let Some(expected) = &def.paths_sha256
            && &digest(&def.paths)? != expected
        {
            bail!("paths の sha256 が違います");
        }
        let mut roots = BTreeMap::new();
        let mut root_order = Vec::new();
        let mut game: Edges = BTreeMap::new();
        for line in std::fs::read_to_string(&def.paths)?.lines().filter(|l| !l.trim().is_empty()) {
            let cols: Vec<_> = line.split('\t').collect();
            if cols.len() < 3 {
                bail!("paths は key / ply / 手順 が必要です");
            }
            let ply: u32 = cols[1].parse()?;
            if ply == 0 {
                bail!("ply は正数が必要です");
            }
            let pos = position_from_sfen(&format!("{} {ply}", cols[0]))?;
            let mut key = strip_ply(&pos.to_sfen()).to_owned();
            if key != cols[0] {
                bail!("paths の key は正規形が必要です");
            }
            if !roots.contains_key(&key) {
                root_order.push(key.clone());
            }
            roots.entry(key.clone()).or_insert(ply);
            for mv in cols[2].split_whitespace().take(def.depth.saturating_add(1)) {
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

    /// 台帳から要求済みの読みだけを取り込み、展開と段の緩和が止まるまで進める。
    pub fn advance(&mut self, cache: &BTreeMap<String, Reading>) -> Result<Vec<(String, usize)>> {
        if self.finished || self.failed {
            return Ok(Vec::new());
        }
        loop {
            let mut work: VecDeque<_> = self.level.keys().cloned().collect();
            while let Some(key) = work.pop_front() {
                if !self.nodes.contains_key(&key) {
                    let Some(reading) = cache.get(&key) else {
                        continue;
                    };
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
            (rows.values().copied().max().unwrap_or(0), rows)
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

pub(super) fn remove_candidates(out: &Path) -> Result<()> {
    match std::fs::remove_file(out.join("own_nodes.tsv")) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}
fn push_unique(moves: &mut Vec<String>, mv: &str) {
    if !moves.iter().any(|m| m == mv) {
        moves.push(mv.into());
    }
}
