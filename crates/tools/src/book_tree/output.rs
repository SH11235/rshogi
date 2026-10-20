use super::model::{Tree, Values, child, remove_candidates, side};
use crate::book_backprop::{BackpropOptions, MergeMode, backprop_file_with};
use crate::book_db::{BOOK_HEADER, BookDb};
use crate::common::io::write_atomic;
use anyhow::{Context, Result};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

fn fm(v: Option<i32>) -> String {
    v.map_or_else(|| "-".into(), |v| format!("{v:+}"))
}
fn rows_for<'a>(values: &'a Values, key: &str) -> Result<&'a BTreeMap<String, i32>> {
    values.get(key).context("評価集合に局面がありません")
}

impl Tree {
    pub fn write_outputs(&self) -> Result<bool> {
        remove_candidates(&self.def.out)?;
        let all: BTreeSet<_> = self.nodes.keys().cloned().collect();
        let rule_set = self.reach(&self.rule);
        let replace = self.negamax(&all)?;
        let rule = self.negamax(&rule_set)?;
        let pending = self.unresolved(&replace);
        write_atomic(
            &self.def.out.join("registration_status.json"),
            &serde_json::to_string_pretty(
                &json!({"ready": pending.is_empty(), "unresolved": pending}),
            )?,
        )?;
        // 後段は d*.jsonl を全て読むため、以前の定義の読みを残さない。
        for entry in std::fs::read_dir(&self.def.out)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('d') && name.ends_with(".jsonl") && entry.file_type()?.is_file() {
                std::fs::remove_file(entry.path())?;
            }
        }
        let mut levels: BTreeMap<usize, String> = BTreeMap::new();
        let mut tags: BTreeMap<usize, usize> = BTreeMap::new();
        for (key, reading) in &self.nodes {
            let d = self.level[key];
            let mut reading = reading.clone();
            reading.ply = self.ply[key];
            let index = tags.entry(d).or_default();
            *index += 1;
            reading.tag = format!("d{d}_{index:04}");
            let out = levels.entry(d).or_default();
            writeln!(out, "{}", serde_json::to_string(&reading)?)?;
        }
        for (d, contents) in levels {
            write_atomic(&self.def.out.join(format!("d{d}.jsonl")), &contents)?;
        }
        if !pending.is_empty() {
            write_atomic(&self.def.out.join("STATUS"), "NOT_READY\n")?;
            return Ok(false);
        }
        self.write_db("tree.db", &all, &self.edge)?;
        self.write_db("tree_rule.db", &rule_set, &self.rule)?;
        backprop_file_with(
            &self.def.out.join("tree.db"),
            &self.def.out.join("tree_bp_min.db"),
            None,
            0,
            200000,
            MergeMode::Min,
            BackpropOptions::default(),
        )?;
        let min = BookDb::read(&self.def.out.join("tree_bp_min.db"))?;
        let mut report = String::new();
        let root_side = side(self.roots.keys().next().context("根がありません")?);
        let sgn = if root_side == "b" { 1 } else { -1 };
        for key in self.roots.keys() {
            let compare = self.compare[key].as_deref().unwrap_or("None");
            writeln!(report, "\n根 {key} | 比べる手 (実戦の手でない読みの最上位): {compare}")?;
            writeln!(
                report,
                "手 | 読み (手番の側) | 伝播後 replace | 伝播後 min | replace (プラス = 先手良し) | 規則の枝だけの replace (プラス = 先手良し) | d = 比べる手との差 (プラス = 先手良し) | 読んだ局面 | 実戦の手 | replace の読み筋"
            )?;
            let mut cands = self.candidates[key].clone();
            for mv in self.edge.get(key).into_iter().flatten() {
                if !cands.contains(mv) {
                    cands.push(mv.clone());
                }
            }
            let raw = self.raw_rows(key);
            let values = rows_for(&replace, key)?;
            let vc = self.compare[key].as_ref().and_then(|m| values.get(m)).copied();
            for mv in &cands {
                let c = child(key, mv)?;
                let v = values.get(mv).copied();
                let w = min
                    .entries
                    .get(key)
                    .and_then(|e| e.moves.iter().find(|m| m.move_usi.as_ref() == Some(mv)))
                    .map(|m| m.value);
                let d = if Some(mv) == self.compare[key].as_ref() {
                    None
                } else {
                    v.zip(vc).map(|(v, c)| sgn * (v - c))
                };
                let n = self.reach_from(vec![c.clone()], &self.edge).len();
                let game = if self.game.get(key).is_some_and(|g| g.contains(mv)) {
                    "○"
                } else {
                    ""
                };
                let read = raw.get(mv).map_or_else(|| "MultiPV の外".into(), ToString::to_string);
                writeln!(
                    report,
                    "{mv} | {read} | {} | {} | {} | {} | {} | {n} | {game} | {mv} {}",
                    fm(v),
                    fm(w),
                    fm(v.map(|v| v * sgn)),
                    fm(rule.get(key).and_then(|r| r.get(mv)).map(|v| v * sgn)),
                    fm(d),
                    self.pv(&c, &replace)?
                )?;
            }
        }
        let game_children: Vec<_> = self
            .roots
            .keys()
            .flat_map(|k| self.game.get(k).into_iter().flatten().filter_map(|m| child(k, m).ok()))
            .collect();
        let under_game = self.reach_from(game_children, &self.edge);
        let own_sign = if self.def.own == "b" { 1 } else { -1 };
        let mut own_rows = Vec::new();
        for key in self.reach(&self.edge) {
            if side(&key) != self.def.own || self.nodes[&key].multipv.is_empty() {
                continue;
            }
            let values = rows_for(&replace, &key)?;
            let ranked = self.rank(&key, values, values.keys().cloned());
            let an = ranked.first().context("候補手がありません")?;
            let edges = self.edge.get(&key).filter(|e| !e.is_empty());
            let ex = if let Some(edges) = edges {
                self.rank(&key, values, edges.iter().cloned())[0].clone()
            } else {
                an.clone()
            };
            let gm = self.game.get(&key).and_then(|g| g.first()).map(String::as_str).unwrap_or("-");
            let gv = values.get(gm).copied();
            let ev = values.get(&ex).copied();
            let why = if let Some(edges) = edges {
                if edges.contains(an) {
                    "伸ばした手"
                } else {
                    "伸ばしていない手"
                }
            } else if self.nodes[&key].multipv[0].cp.abs() >= self.def.stop {
                "止めた (値の絶対値が STOP 以上)"
            } else {
                "出口 (伸ばしていない)"
            };
            let mut moves = String::new();
            for mv in &ranked {
                if !moves.is_empty() {
                    moves.push(' ');
                }
                write!(moves, "{mv}:{}", fm(values.get(mv).copied()))?;
            }
            own_rows.push(vec![
                key.clone(),
                self.ply[&key].to_string(),
                ex,
                fm(ev),
                fm(ev.map(|v| v * own_sign)),
                an.clone(),
                why.into(),
                fm(values.get(an).map(|v| v * own_sign)),
                gm.into(),
                fm(gv.map(|v| v * own_sign)),
                fm(ev.zip(gv).map(|(v, g)| v - g)),
                if under_game.contains(&key) {
                    "○".into()
                } else {
                    String::new()
                },
                moves,
            ]);
        }
        own_rows.sort_by_key(|r| r[1].parse::<u32>().unwrap_or(0));
        let under = own_rows.iter().filter(|r| r[11] == "○").count();
        writeln!(
            report,
            "\n読んだ局面 {} | 規則の枝だけで届く局面 {} | こちらの手番で伸ばした局面 {} (うち実戦の手の下 {under}、own_nodes.tsv)",
            self.nodes.len(),
            rule_set.len(),
            own_rows.len()
        )?;
        write_atomic(&self.def.out.join("report.txt"), &report)?;
        let mut own = "key\tply\t伸ばした手の中の 1 位\t値 (手番の側)\t値 (プラス = 先手良し)\t全体の 1 位\t全体の 1 位は\t全体の 1 位の値 (プラス = 先手良し)\t実戦の手\t実戦の手の値 (プラス = 先手良し)\t実戦の手の 1 位からの差 (手番の側)\t実戦の手の下\t手ごとの値 (手番の側)\n".to_owned();
        for row in own_rows {
            writeln!(own, "{}", row.join("\t"))?;
        }
        // 登録材料は全ての成果物が完成してから公開する。
        write_atomic(&self.def.out.join("STATUS"), "DONE\n")?;
        write_atomic(&self.def.out.join("own_nodes.tsv"), &own)?;
        Ok(true)
    }

    fn write_db(
        &self,
        name: &str,
        nodes: &BTreeSet<String>,
        edges: &BTreeMap<String, BTreeSet<String>>,
    ) -> Result<()> {
        let mut db = format!("{BOOK_HEADER}\n");
        for key in nodes {
            let mut rows = self.raw_rows(key);
            for mv in edges.get(key).into_iter().flatten() {
                let c = child(key, mv)?;
                if !rows.contains_key(mv)
                    && let Some(first) = self.nodes.get(&c).and_then(|n| n.multipv.first())
                {
                    rows.insert(mv.clone(), -first.cp);
                }
            }
            writeln!(db, "sfen {key} {}", self.ply[key])?;
            for mv in self.rank(key, &rows, rows.keys().cloned()) {
                writeln!(db, "{mv} none {} 0 1", rows[&mv])?;
            }
        }
        write_atomic(&self.def.out.join(name), &db)
    }

    fn pv(&self, start: &str, values: &Values) -> Result<String> {
        let mut key = start.to_owned();
        let mut pv = Vec::new();
        for _ in 0..16 {
            let Some(rows) = values.get(&key) else {
                break;
            };
            let Some(edges) = self.edge.get(&key).filter(|e| !e.is_empty()) else {
                break;
            };
            let ranked = self.rank(&key, rows, edges.iter().cloned());
            let Some(mv) = ranked.first() else {
                break;
            };
            pv.push(mv.clone());
            key = child(&key, mv)?;
        }
        Ok(pv.join(" "))
    }
}
