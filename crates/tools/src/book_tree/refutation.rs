//! `master audit` の反駁の点検 (YaneuraOu の `peta refutation` に当たる)。
//!
//! 読みの MultiPV の 2 位以下の値は 1 位の値より甘く出やすい。伝播で 1 位の手の先が掘られて値が下がると、
//! 先を伸ばしていない甘い手が伝播後の 1 位に繰り上がる。その局面を、伸ばしていない手を指した後の局面を
//! 根にした木のキューの行とともに出す。

use super::audit::{AuditedBook, AuditedMove, Origin, audited_moves};
use super::master_table::MasterPosition;
use super::model::child;
use super::rejoin::{WeightArgs, reach_weights};
use super::reply_graph::HIRATE_KEY;
use super::targets::{Line, Opts};
use crate::book_backprop::{delta_buckets, percentile};
use crate::book_db::BookDb;
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// 反駁の表の 1 行。値は手番の側から見た伝播後の値で、同値の -1 補正の前の値。
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Refutation {
    pub key: String,
    pub ply: String,
    pub source: String,
    pub main_move: String,
    /// main の伝播後の手。値が分からなければ `None`。
    pub main: Option<AuditedMove>,
    /// 伸ばしていない手 (`Origin::Eval`) の最善。
    pub best: AuditedMove,
    pub best_source: String,
    /// 伸ばした手 (`Origin::Propagated`) の最善。
    pub expanded_best: AuditedMove,
    /// 伝播前の読みの値で `best` より大きい手がある。
    pub swapped: bool,
    pub main_is_best: bool,
    pub weight: Option<f64>,
}

impl Refutation {
    pub fn margin(&self) -> i32 {
        self.best.value - self.expanded_best.value
    }

    fn black(&self) -> bool {
        !self.key.split(' ').nth(1).is_some_and(|s| s == "w")
    }
}

/// 手番ごと (0 = 先手番、1 = 後手番) の数。
type BySide = [usize; 2];

#[derive(Debug, Default, PartialEq)]
pub(super) struct RefutationSummary {
    /// 値の分かる手が 2 手以上の局面。
    pub comparable: BySide,
    /// 伸ばした手と伸ばしていない手の両方に値の分かる手がある局面。
    pub mixed: BySide,
    /// margin が `--refutation-margin` 以上の局面 (重みの下限で除く前)。
    pub hits: BySide,
    pub swapped: usize,
    pub main_is_best: usize,
    /// 当たる局面の margin ごとの数。
    pub margins: BTreeMap<i32, usize>,
    pub below_min_weight: usize,
}

/// 局面ごとの重み (手番の側を自分としたときの重み)。`[0]` が先手番、`[1]` が後手番の局面。
pub(super) type SideWeights = [HashMap<String, f64>; 2];

/// `--opp-freq` があれば先手と後手それぞれを自分として初期局面からの重みを求める。
pub(super) fn side_weights(
    positions: &BTreeMap<String, MasterPosition>,
    weight: &WeightArgs,
) -> Result<Option<SideWeights>> {
    let Some(path) = &weight.opp_freq else {
        return Ok(None);
    };
    let freq = BookDb::read(path)?;
    let options = weight.options();
    let (black, white) = rayon::join(
        || reach_weights(positions, &freq, "b", HIRATE_KEY, options),
        || reach_weights(positions, &freq, "w", HIRATE_KEY, options),
    );
    Ok(Some([black?, white?]))
}

/// 伝播後の 1 位が伸ばしていない手で、伸ばした手の最善を `margin` 以上上回る局面を探す。重みがあれば重みの
/// 大きい順、無ければ margin の大きい順 (同じなら key 順) に並べる。
pub(super) fn find(
    positions: &BTreeMap<String, MasterPosition>,
    book: &AuditedBook,
    margin: i32,
    weights: Option<&SideWeights>,
    min_weight: f64,
) -> Result<(Vec<Refutation>, RefutationSummary)> {
    let mut summary = RefutationSummary::default();
    let mut rows = Vec::new();
    for ((key, position), known) in positions.iter().zip(&book.known) {
        let side = usize::from(key.split(' ').nth(1) == Some("w"));
        let mut audited = audited_moves(&book.graph, key, position, known)?;
        // 伝播の書き出しは 1 位と同値で選ばなかった手を -1 する。そのまま比べると、伸ばした手と伸ばして
        // いない手が同値でも margin 1 で当たる。
        for m in audited.iter_mut().flatten() {
            m.value += i32::from(std::mem::take(&mut m.tie_lowered));
        }
        if audited.iter().flatten().count() >= 2 {
            summary.comparable[side] += 1;
        }
        // 同じ値なら元データの順で先の手 (main が先頭) を取る。
        let best_of = |origin: Origin| {
            audited
                .iter()
                .enumerate()
                .filter_map(|(i, m)| m.as_ref().map(|m| (i, m)))
                .filter(|(_, m)| m.origin == origin)
                .reduce(|best, m| if m.1.value > best.1.value { m } else { best })
        };
        let (Some((best_index, best)), Some((_, expanded))) =
            (best_of(Origin::Eval), best_of(Origin::Propagated))
        else {
            continue;
        };
        summary.mixed[side] += 1;
        if best.value - expanded.value < margin {
            continue;
        }
        // 伝播前の順位は読みの値で比べる。深さの違う読みが混ざるので条件には使わない。
        // 同値なら入れ替わっていないとみなす。
        let pre_top = audited.iter().flatten().filter_map(|m| m.eval).max();
        let row = Refutation {
            key: key.clone(),
            ply: position.ply.clone(),
            source: position.source.clone(),
            main_move: position.moves[0].mv.clone(),
            main: audited[0].clone(),
            // 当たった局面だけ後で元データから引く。
            best_source: String::new(),
            best: best.clone(),
            expanded_best: expanded.clone(),
            swapped: pre_top > best.eval,
            main_is_best: best_index == 0,
            weight: weights.map(|w| w[side].get(key).copied().unwrap_or(0.0)),
        };
        summary.hits[side] += 1;
        summary.swapped += usize::from(row.swapped);
        summary.main_is_best += usize::from(row.main_is_best);
        *summary.margins.entry(row.margin()).or_default() += 1;
        rows.push(row);
    }
    if weights.is_some() {
        let before = rows.len();
        rows.retain(|r| r.weight.unwrap_or(0.0) >= min_weight);
        summary.below_min_weight = before - rows.len();
    }
    rows.sort_by(|a, b| {
        b.weight
            .unwrap_or(0.0)
            .total_cmp(&a.weight.unwrap_or(0.0))
            .then_with(|| b.margin().cmp(&a.margin()))
            .then_with(|| a.key.cmp(&b.key))
    });
    Ok((rows, summary))
}

/// 反駁の行の `best_source` を元データの `best` の行の source で埋める。`read_master` は手ごとの source を
/// 持たないので、当たった局面の分だけ読み直して引く。
pub(super) fn fill_best_sources(path: &Path, rows: &mut [Refutation]) -> Result<()> {
    let mut wanted: HashMap<(&str, &str), usize> = HashMap::new();
    for (i, r) in rows.iter().enumerate() {
        wanted.insert((r.key.as_str(), r.best.mv.as_str()), i);
    }
    let file =
        std::fs::File::open(path).with_context(|| format!("{} を読めません", path.display()))?;
    let mut found = vec![None; rows.len()];
    // 見出し・列の数・role は read_master で検査済み。
    for line in BufReader::new(file).lines().skip(1) {
        let row = super::master_table::Row::split_line(&line?);
        if row.get("role") == "record" {
            continue;
        }
        if let Some(&i) = wanted.get(&(row.get("key"), row.get("move"))) {
            found[i] = Some(row.get("source").to_owned());
        }
    }
    for (row, source) in rows.iter_mut().zip(found) {
        row.best_source = source.with_context(|| {
            format!("元データが読み込みの途中で変わりました: {} {}", row.key, row.best.mv)
        })?;
    }
    Ok(())
}

pub(super) fn format_table(rows: &[Refutation]) -> String {
    let mut text = String::from(
        "key\tply\tside\tmain\tmain_value\tmain_from\tbest\tbest_value\tbest_eval\texpanded_best\texpanded_best_value\texpanded_best_eval\tmargin\tswapped\tmain_is_best\tweight\tbest_value_black\texpanded_best_value_black\tsource\tbest_source\n",
    );
    for r in rows {
        let black = r.black();
        let sente = |v: i32| if black { v } else { -v };
        let eval = |m: &AuditedMove| m.eval.map_or_else(String::new, |v| v.to_string());
        let main_value = r.main.as_ref().map_or_else(String::new, |m| m.value.to_string());
        let main_from = r.main.as_ref().map_or("", |m| m.origin.as_str());
        writeln!(
            text,
            "{}\t{}\t{}\t{}\t{main_value}\t{main_from}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            r.key,
            r.ply,
            if black { "b" } else { "w" },
            r.main_move,
            r.best.mv,
            r.best.value,
            eval(&r.best),
            r.expanded_best.mv,
            r.expanded_best.value,
            eval(&r.expanded_best),
            r.margin(),
            u8::from(r.swapped),
            u8::from(r.main_is_best),
            r.weight.map_or_else(String::new, |w| format!("{w:.6}")),
            sente(r.best.value),
            sente(r.expanded_best.value),
            r.source,
            r.best_source,
        )
        .expect("String への書き込み");
    }
    text
}

/// 反駁の表の行から `book_mine tree enqueue --from` の行を作る。根は `best` を指した後の (相手の手番の)
/// 局面で、`own` は `best` を指す側 (定跡の自分の側)。相手番の根では木が相手の応手を `opp_n` 手まで広げる
/// ので、`best` が相手の応手に耐えるかを読める。priority は `base + min(rank, 99)` (`rank` は表の順位、
/// 0 始まり)。id は局面と `best` で決まり、同じ局面に同じ手を足し直すと同じ id になる。
pub(super) fn queue_line(row: &Refutation, rank: usize, depth: usize, base: u32) -> Result<Line> {
    let ply: u32 = row
        .ply
        .trim()
        .parse()
        .with_context(|| format!("元データの手数を読めません: {} {}", row.key, row.ply))?;
    let root = child(&row.key, &row.best.mv)?;
    let side = if row.black() { "b" } else { "w" };
    let digest = format!("{:x}", Sha256::digest(format!("{} {}", row.key, row.best.mv)));
    let line = Line {
        id: format!("{}_refut_{side}", &digest[..16]),
        root: format!("sfen {root} {}", ply + 1),
        roots: Vec::new(),
        own: side.to_owned(),
        depth,
        priority: base + rank.min(99) as u32,
        hold: false,
        game: Vec::new(),
        opts: Opts::default(),
        memo: Some(format!(
            "refutation margin={} best={} expanded_best={}",
            row.margin(),
            row.best.mv,
            row.expanded_best.mv
        )),
    };
    line.check().with_context(|| format!("キューの行の検査: {}", row.key))?;
    Ok(line)
}

pub(super) fn format_queue(lines: &[Line]) -> Result<String> {
    let mut text = String::new();
    for line in lines {
        text.push_str(&serde_json::to_string(line)?);
        text.push('\n');
    }
    Ok(text)
}

pub(super) fn format_summary(
    summary: &RefutationSummary,
    margin: i32,
    rows: usize,
    weighted: bool,
    queued: Option<usize>,
) -> String {
    let sides = |c: BySide| format!("{} (先手番 {} / 後手番 {})", c[0] + c[1], c[0], c[1]);
    let mut text = String::new();
    let mut out = |s: String| {
        text.push_str(&s);
        text.push('\n');
    };
    out(format!("反駁: 値の分かる手が 2 手以上の局面: {}", sides(summary.comparable)));
    out(format!(
        "反駁: 伸ばした手と伸ばしていない手の両方がある局面: {}",
        sides(summary.mixed)
    ));
    let hits = summary.hits[0] + summary.hits[1];
    out(format!(
        "反駁: 伸ばしていない手が伸ばした手の最善を {margin} 以上上回る局面: {} / うち swapped {} / main_is_best {}",
        sides(summary.hits),
        summary.swapped,
        summary.main_is_best
    ));
    if hits > 0 {
        let buckets = delta_buckets(&summary.margins)
            .into_iter()
            .map(|(label, count)| format!("{label}: {count}"))
            .collect::<Vec<_>>()
            .join(" / ");
        out(format!(
            "反駁: margin の中央値 {} / 90% {} / 最大 {} [{buckets}]",
            percentile(&summary.margins, hits, 50),
            percentile(&summary.margins, hits, 90),
            summary.margins.keys().next_back().copied().unwrap_or(0),
        ));
    }
    if weighted {
        out(format!("反駁: 表の行 {rows} (重みの下限で除いた {})", summary.below_min_weight));
    }
    if let Some(queued) = queued {
        out(format!("反駁: キューの行 {queued}"));
    }
    text
}

/// 反駁の点検の出力の指定。
pub(super) struct RefutationOutputs<'a> {
    pub table: Option<&'a Path>,
    pub enqueue_out: Option<&'a Path>,
    pub margin: i32,
    pub depth: usize,
    pub max: Option<usize>,
    pub priority: u32,
    pub weight: &'a WeightArgs,
}

/// 反駁の表とキューの行を書き、要約を返す。
pub(super) fn write_outputs(
    master: &Path,
    positions: &BTreeMap<String, MasterPosition>,
    book: &AuditedBook,
    outputs: &RefutationOutputs<'_>,
) -> Result<String> {
    let weights = side_weights(positions, outputs.weight)?;
    let (mut rows, summary) =
        find(positions, book, outputs.margin, weights.as_ref(), outputs.weight.min_weight)?;
    fill_best_sources(master, &mut rows)?;
    if let Some(path) = outputs.table {
        crate::common::io::write_atomic(path, &format_table(&rows))?;
    }
    let queued = match outputs.enqueue_out {
        Some(path) => {
            let lines = rows
                .iter()
                .take(outputs.max.unwrap_or(usize::MAX))
                .enumerate()
                .map(|(rank, r)| queue_line(r, rank, outputs.depth, outputs.priority))
                .collect::<Result<Vec<_>>>()?;
            crate::common::io::write_atomic(path, &format_queue(&lines)?)?;
            Some(lines.len())
        }
        None => None,
    };
    Ok(format_summary(&summary, outputs.margin, rows.len(), weights.is_some(), queued))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::book_backprop::DrawValues;
    use crate::book_tree::audit::audit;
    use crate::book_tree::master_table::{COLUMNS, parse_master_rows};

    const ROOT: &str = "4k4/9/9/9/9/9/9/9/4K4 b -";
    /// ROOT から 5i4h。
    const A: &str = "4k4/9/9/9/9/9/9/5K3/9 w -";

    fn master(rows: &[(&str, &str, &str, &str, &str)]) -> String {
        let mut text = COLUMNS.join("\t");
        text.push('\n');
        for (key, mv, role, source, evals) in rows {
            writeln!(text, "{key}\t1\t{mv}\t{role}\t{source}\t{evals}\t\t\twide\tt\t").unwrap();
        }
        text
    }

    /// ROOT の main 5i4h は A の 30 で -30 (伸ばした手)、alt 5i6h は読みの値 10 (伸ばしていない手)。
    fn root_master(main_eval: &str) -> String {
        master(&[
            (ROOT, "5i4h", "main", "read", main_eval),
            (ROOT, "5i6h", "alt", "team", "10"),
            (A, "5a5b", "main", "read", "30"),
        ])
    }

    fn run_find(
        text: &str,
        margin: i32,
        weights: Option<&SideWeights>,
        min_weight: f64,
    ) -> (Vec<Refutation>, RefutationSummary) {
        let parsed = parse_master_rows(text.as_bytes()).unwrap();
        let (_, _, book) = audit(&parsed, DrawValues::both(0), 1, false).unwrap();
        let (mut rows, summary) = find(&parsed.0, &book, margin, weights, min_weight).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("moves.tsv");
        std::fs::write(&path, text).unwrap();
        fill_best_sources(&path, &mut rows).unwrap();
        (rows, summary)
    }

    #[test]
    fn unexpanded_move_above_expanded_moves_is_found() {
        let (rows, summary) = run_find(&root_master(""), 40, None, 0.0);
        assert_eq!(summary.comparable, [1, 0]);
        assert_eq!(summary.mixed, [1, 0]);
        assert_eq!(summary.hits, [1, 0]);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!((row.best.mv.as_str(), row.expanded_best.mv.as_str()), ("5i6h", "5i4h"));
        assert_eq!(row.margin(), 40);
        assert!(!row.main_is_best);
        // 伝播前に読みの値があるのは 5i6h だけなので 1 位は変わっていない。
        assert!(!row.swapped);
        let table = format_table(&rows);
        let cells: Vec<_> = table.lines().nth(1).unwrap().split('\t').collect();
        assert_eq!(
            cells[1..],
            [
                "1",
                "b",
                "5i4h",
                "-30",
                "propagated",
                "5i6h",
                "10",
                "10",
                "5i4h",
                "-30",
                "",
                "40",
                "0",
                "0",
                "",
                "10",
                "-30",
                "read",
                "team"
            ]
        );

        let (rows, summary) = run_find(&root_master(""), 41, None, 0.0);
        assert!(rows.is_empty());
        assert_eq!(summary.mixed, [1, 0]);
        assert_eq!(summary.hits, [0, 0]);
    }

    #[test]
    fn swapped_compares_evals_before_propagation() {
        // 5i4h の読みの値 50 は伝播前の 1 位だが、伝播で -30 に下がり 5i6h が 1 位に繰り上がる。
        let (rows, summary) = run_find(&root_master("50"), 1, None, 0.0);
        assert!(rows[0].swapped);
        assert_eq!(summary.swapped, 1);
    }

    #[test]
    fn equal_eval_before_propagation_is_not_swapped() {
        let (rows, summary) = run_find(&root_master("10"), 1, None, 0.0);
        assert!(!rows[0].swapped);
        assert_eq!(summary.swapped, 0);
    }

    #[test]
    fn tie_lowered_moves_are_compared_before_the_correction() {
        // 5i4h は A の -10 で 10、5i6h は読みの値 10 で同値。書き出しはどちらかを 9 にする。
        let text = master(&[
            (ROOT, "5i4h", "main", "read", ""),
            (ROOT, "5i6h", "alt", "team", "10"),
            (A, "5a5b", "main", "read", "-10"),
        ]);
        let parsed = parse_master_rows(text.as_bytes()).unwrap();
        let (_, _, book) = audit(&parsed, DrawValues::both(0), 1, false).unwrap();
        let root = &parsed.0[ROOT];
        let index = parsed.0.keys().position(|k| k == ROOT).unwrap();
        let audited = audited_moves(&book.graph, ROOT, root, &book.known[index]).unwrap();
        assert!(audited.iter().flatten().any(|m| m.tie_lowered && m.value == 9));
        let (rows, summary) = run_find(&text, 1, None, 0.0);
        assert!(rows.is_empty());
        assert_eq!(summary.mixed, [1, 0]);
    }

    #[test]
    fn main_move_is_listed_without_its_value() {
        let text = master(&[
            (ROOT, "5i5h", "main", "read", ""),
            (ROOT, "5i6h", "alt", "team", "10"),
            (ROOT, "5i4h", "alt", "read", ""),
            (A, "5a5b", "main", "read", "30"),
        ]);
        let (rows, _) = run_find(&text, 1, None, 0.0);
        assert!(rows[0].main.is_none());
        let table = format_table(&rows);
        let cells: Vec<_> = table.lines().nth(1).unwrap().split('\t').collect();
        assert_eq!(cells[3..7], ["5i5h", "", "", "5i6h"]);
    }

    #[test]
    fn main_is_best_when_main_is_the_unexpanded_move() {
        let text = master(&[
            (ROOT, "5i6h", "main", "team", "10"),
            (ROOT, "5i4h", "alt", "read", ""),
            (A, "5a5b", "main", "read", "30"),
        ]);
        let (rows, summary) = run_find(&text, 1, None, 0.0);
        assert!(rows[0].main_is_best);
        assert_eq!(summary.main_is_best, 1);
    }

    #[test]
    fn weights_filter_and_order_rows() {
        let weights: SideWeights = [HashMap::from([(ROOT.to_owned(), 0.5)]), HashMap::new()];
        let (rows, summary) = run_find(&root_master(""), 1, Some(&weights), 0.5);
        assert_eq!(rows[0].weight, Some(0.5));
        assert_eq!(summary.below_min_weight, 0);
        let (rows, summary) = run_find(&root_master(""), 1, Some(&weights), 0.6);
        assert!(rows.is_empty());
        assert_eq!(summary.below_min_weight, 1);
    }

    #[test]
    fn queue_line_roots_after_the_unexpanded_move() {
        let (rows, _) = run_find(&root_master(""), 1, None, 0.0);
        let line = queue_line(&rows[0], 0, 2, 100).unwrap();
        // own は best を指す側のまま、根は best を指した後の相手の手番の局面。
        assert_eq!(line.root, "sfen 4k4/9/9/9/9/9/9/3K5/9 w - 2");
        assert_eq!(line.own, "b");
        assert_eq!(line.depth, 2);
        assert_eq!(line.priority, 100);
        assert_eq!(queue_line(&rows[0], 150, 2, 100).unwrap().priority, 199);
        assert!(line.id.ends_with("_refut_b"));
        assert_eq!(queue_line(&rows[0], 3, 2, 100).unwrap().id, line.id);
        let mut other_best = rows[0].clone();
        other_best.best.mv = "5i5h".into();
        assert_ne!(queue_line(&other_best, 0, 2, 100).unwrap().id, line.id);
        let text = format_queue(std::slice::from_ref(&line)).unwrap();
        let parsed: Line = serde_json::from_str(text.trim_end()).unwrap();
        assert_eq!(parsed, line);
    }
}
