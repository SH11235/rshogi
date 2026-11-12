//! main を伝播の値で上の手に替えるかの規則と、1 回の読みの値の手を伸ばす木のキューの行。

use super::exit_report::{column, read_tsv};
use super::model::{child, side};
use super::queue_review::path_keys;
use super::refutation::SideWeights;
use super::targets::{DEFAULT_PRIORITY, Line, MAX_DEPTH, MAX_PRIORITY, Opts};
use super::targets::{Root, parse_root, read_checked_lines};
use super::walk::{check_tag, queue_id};
use crate::book_backprop::{Graph, MoveValue};
use anyhow::{Context, Result, ensure};
use clap::{Args, ValueEnum};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub(super) enum SwitchRule {
    /// 値の差で決める (audit は --line、register は --keep-line)
    #[default]
    Line,
    /// 両方の手の値が手の先の局面から来ていれば差があれば替え、片方が 1 回の読みの値なら替えない
    Propagated,
}

/// 手の値が手の先の局面の値から来たか。`capped` は、元データに無く、着く手の読みの値を最後の手として残した
/// 局面 (`--bridge-replies` で補った局面、`--opp-records` の record だけの局面)。その局面で残した値が最善なら、
/// 手の値は 1 回の読みそのもの。record の読みなど、辺の無いほかの手が最善なら先の局面の値として数える。
pub(super) fn from_child(graph: &Graph, capped: impl Fn(&str) -> bool, mv: &MoveValue) -> bool {
    let Some(edge) = mv.edge else {
        return false;
    };
    let moves: Vec<MoveValue> = graph.moves(edge.to).collect();
    // 同値なら後の手を取る (`tree register` の段数 `Depths` の選び方と揃える)。
    let Some((i, best)) = moves.iter().enumerate().max_by_key(|(_, m)| m.new) else {
        return false;
    };
    best.edge.is_some() || !(capped(graph.key(edge.to)) && i + 1 == moves.len())
}

#[derive(Args, Debug)]
pub(super) struct SingleQueueArgs {
    /// --switch-rule propagated で、main か 1 位の値が 1 回の読みなので替えなかった局面ごとに、その局面を根にして
    /// その手を `game` に入れた tree enqueue --from の行 (JSON Lines) の出力先
    #[arg(long)]
    pub single_enqueue_out: Option<PathBuf>,
    /// キューの行の段数
    #[arg(long, default_value_t = 6, requires = "single_enqueue_out", value_parser = clap::builder::RangedU64ValueParser::<usize>::new().range(1..=MAX_DEPTH as u64))]
    pub single_depth: usize,
    /// キューの行の priority の基準。並べた順位 (0 始まり、99 で頭打ち) を足す
    #[arg(long, default_value_t = DEFAULT_PRIORITY, requires = "single_enqueue_out", value_parser = clap::value_parser!(u32).range(0..=i64::from(MAX_PRIORITY - 99)))]
    pub single_priority: u32,
    /// キューの行の数の上限 (並べた順で先から)
    #[arg(long, requires = "single_enqueue_out")]
    pub single_max: Option<usize>,
    /// キューの行の相手の手の数 (1〜8)
    #[arg(long, default_value_t = 3, requires = "single_enqueue_out")]
    pub single_opp_n: usize,
    /// キューの行の打ち切り値 (1〜30000)
    #[arg(long, default_value_t = 300, requires = "single_enqueue_out")]
    pub single_stop: i32,
    /// キューの行の読み直し回数 (0〜10)
    #[arg(long, default_value_t = 3, requires = "single_enqueue_out")]
    pub single_rounds: usize,
    /// 先手番で循環に入る手の値 (-30000〜30000)
    #[arg(long, default_value_t = -70, requires = "single_enqueue_out", allow_hyphen_values = true)]
    pub single_draw_black: i32,
    /// 後手番で循環に入る手の値 (-30000〜30000)
    #[arg(
        long,
        default_value_t = 70,
        requires = "single_enqueue_out",
        allow_hyphen_values = true
    )]
    pub single_draw_white: i32,
    /// キューの行の id `<tag>_<根の手数>_<局面の hash>_<b|w>` に入れる名前 ([A-Za-z0-9_-] の 1〜40 文字)
    #[arg(long, default_value = "single", requires = "single_enqueue_out")]
    pub single_tag: String,
    /// 既に積んだ木のキュー (JSON Lines)。繰り返して複数。1 回の読みの手を、その局面の手番の側で
    /// `game` の depth 手以内に指す木か、その手を指した後の局面を根に同じ側で読む木があれば、その手を行に入れない
    #[arg(long, requires = "single_enqueue_out")]
    pub exclude_queue: Vec<PathBuf>,
    /// `master walk --table` の表。繰り返して複数 (重みは足す)。局面の重みを、side が局面の手番の側と同じ行のうち
    /// 手順がその局面を通るか、その局面で止まる行の重みの和 (1 行で同じ局面は 1 回) にして、重みの大きい順に並べる
    #[arg(long, requires = "single_enqueue_out")]
    pub walk_table: Vec<PathBuf>,
    /// `--walk-table` の重みがこれ未満の局面は行にしない
    #[arg(long, default_value_t = 0.0, requires = "walk_table")]
    pub single_min_weight: f64,
    /// `<重みの下限>=<priority>`。繰り返して複数。`--walk-table` の重みが下限以上の行の priority をこの値にする
    /// (当たる帯のうち下限が一番大きいもの。どの帯にも当たらない行は --single-priority と順位のまま)
    #[arg(long, value_name = "W=P", requires = "walk_table", value_parser = parse_band)]
    pub single_priority_band: Vec<(f64, u32)>,
}

fn parse_band(text: &str) -> Result<(f64, u32), String> {
    let (weight, priority) =
        text.split_once('=').ok_or("<重みの下限>=<priority> の形にしてください")?;
    let weight: f64 = weight.parse().map_err(|e| format!("重みの下限 {weight:?}: {e}"))?;
    let priority: u32 = priority.parse().map_err(|e| format!("priority {priority:?}: {e}"))?;
    Ok((weight, priority))
}

impl SingleQueueArgs {
    fn opts(&self) -> Opts {
        Opts {
            opp_n: Some(self.single_opp_n),
            stop: Some(self.single_stop),
            rounds: Some(self.single_rounds),
            draw_black: Some(self.single_draw_black),
            draw_white: Some(self.single_draw_white),
            ..Opts::default()
        }
    }

    pub fn check(&self) -> Result<()> {
        check_tag(&self.single_tag)?;
        ensure!(
            self.single_min_weight.is_finite() && self.single_min_weight >= 0.0,
            "--single-min-weight は 0 以上にしてください"
        );
        for (i, &(weight, priority)) in self.single_priority_band.iter().enumerate() {
            ensure!(
                weight.is_finite() && weight >= 0.0,
                "--single-priority-band の重みの下限は 0 以上にしてください: {weight}"
            );
            ensure!(
                priority <= MAX_PRIORITY,
                "--single-priority-band の priority は {MAX_PRIORITY} 以下にしてください: {priority}"
            );
            ensure!(
                self.single_priority_band[..i].iter().all(|(w, _)| *w != weight),
                "--single-priority-band の重みの下限が重なっています: {weight}"
            );
        }
        self.opts().check()
    }

    /// `--walk-table` があればその辿りの重みと `--single-min-weight` で、無ければ `fallback` の重みと下限で並べる。
    pub fn rank(
        &self,
        reads: &mut Vec<SingleRead>,
        fallback: Option<&SideWeights>,
        fallback_min: f64,
    ) -> Result<()> {
        if self.walk_table.is_empty() {
            rank(reads, fallback, fallback_min);
        } else {
            let weights = walk_weights(&self.walk_table, reads)?;
            rank(reads, Some(&weights), self.single_min_weight);
        }
        Ok(())
    }

    fn priority(&self, weight: Option<f64>, rank: usize) -> u32 {
        weight
            .and_then(|w| {
                self.single_priority_band
                    .iter()
                    .filter(|(low, _)| w >= *low)
                    .max_by(|a, b| a.0.total_cmp(&b.0))
            })
            .map_or(self.single_priority + rank.min(99) as u32, |band| band.1)
    }
}

/// main か 1 位の値が 1 回の読みなので main を替えなかった局面。値は手番の側から。
#[derive(Debug, Clone, PartialEq)]
pub(super) struct SingleRead {
    pub key: String,
    pub ply: String,
    pub main: String,
    pub main_value: i32,
    pub best: String,
    pub best_value: i32,
    /// 値が 1 回の読みの手 (main が先)。
    pub moves: Vec<String>,
    pub weight: Option<f64>,
}

/// 重みがあれば重みの大きい順 (下限未満は除く)、無ければ 1 位と main の差の大きい順に並べる。
fn rank(reads: &mut Vec<SingleRead>, weights: Option<&SideWeights>, min_weight: f64) {
    for r in reads.iter_mut() {
        let s = usize::from(side(&r.key) == "w");
        r.weight = weights.map(|w| w[s].get(&r.key).copied().unwrap_or(0.0));
    }
    reads.retain(|r| r.weight.is_none_or(|w| w >= min_weight));
    reads.sort_by(|a, b| {
        b.weight
            .unwrap_or(0.0)
            .total_cmp(&a.weight.unwrap_or(0.0))
            .then_with(|| (b.best_value - b.main_value).cmp(&(a.best_value - a.main_value)))
            .then_with(|| a.key.cmp(&b.key))
    });
}

/// walk の表から `reads` の局面の重みを求める。局面の手番の側を自分とした辿り (side が手番の側と同じ行) だけを
/// 数える。相手の側の辿りがその局面を通る重みは、相手が自分の定跡で来る重みで、その局面で手を選ぶ側の重みではない。
fn walk_weights(paths: &[PathBuf], reads: &[SingleRead]) -> Result<SideWeights> {
    let wanted: HashSet<&str> = reads.iter().map(|r| r.key.as_str()).collect();
    let mut weights: SideWeights = [HashMap::new(), HashMap::new()];
    for path in paths {
        read_tsv(path, |row| {
            let own = column(row, "side")?;
            ensure!(matches!(own, "b" | "w"), "side は b / w です: {own}");
            let weight: f64 = column(row, "weight")?.parse().context("weight が不正です")?;
            ensure!(weight.is_finite() && weight >= 0.0, "weight が不正です: {weight}");
            let moves: Vec<&str> = column(row, "moves")?.split_whitespace().collect();
            // 手順が同じ局面に戻る行でも、1 本の辿りがその局面に来る重みは行の重み 1 回分。
            let keys: BTreeSet<String> = path_keys(&moves)?.into_iter().collect();
            for key in keys {
                if side(&key) == own && wanted.contains(key.as_str()) {
                    *weights[usize::from(own == "w")].entry(key).or_default() += weight;
                }
            }
            Ok(())
        })?;
    }
    ensure!(
        weights.iter().flat_map(HashMap::values).all(|w| w.is_finite()),
        "重みが大きすぎます"
    );
    Ok(weights)
}

/// 既に積んだ木が伸ばす手 (局面, 手, 側) と、根に読む局面 (局面, 側)。
#[derive(Default)]
struct Queued {
    moves: BTreeSet<(String, String, String)>,
    roots: BTreeSet<(String, String)>,
}

/// 局面を根にしただけの木は、1 回の読みのまま残った手を伸ばさなかった木でもあり得るので、根の一致では除かない。
fn queued(paths: &[PathBuf]) -> Result<Queued> {
    // serve と同じく、id ごとに最初の行の根と own に固定して違う行は飛ばし、game は合わせ、段数などは最後の行を使う。
    let mut latest: BTreeMap<String, (Line, Root)> = BTreeMap::new();
    for path in paths {
        read_checked_lines(path, |mut line, root, _| {
            if let Some((first, first_root)) = latest.get(&line.id) {
                if first_root != &root || first.own != line.own {
                    return Ok(());
                }
                let mut game = first.game.clone();
                for moves in line.game.drain(..) {
                    if !game.contains(&moves) {
                        game.push(moves);
                    }
                }
                line.game = game;
            }
            latest.insert(line.id.clone(), (line, root));
            Ok(())
        })?;
    }
    let mut queued = Queued::default();
    for (line, root) in latest.into_values() {
        if line.depth == 0 {
            continue;
        }
        for own in line.sides() {
            queued.roots.insert((root.key.clone(), own.clone()));
            for more in &line.roots {
                queued.roots.insert((parse_root(more)?.key, own.clone()));
            }
        }
        for moves in &line.game {
            let mut key = root.key.clone();
            // 段数 depth の局面は読むだけで手を伸ばさないので、game の手は depth 手目まで。
            for mv in moves.iter().take(line.depth) {
                if line.own.contains(side(&key)) {
                    queued.moves.insert((key.clone(), mv.clone(), side(&key).to_owned()));
                }
                key = child(&key, mv)?;
            }
        }
    }
    Ok(queued)
}

/// 既に積んだ木が伸ばす手を除き、並べた順にキューの行にする。返す数は手が全部除かれた局面の数。
pub(super) fn queue(reads: &[SingleRead], args: &SingleQueueArgs) -> Result<(Vec<Line>, usize)> {
    let queued = queued(&args.exclude_queue)?;
    let mut kept = Vec::new();
    for r in reads {
        let own = side(&r.key).to_owned();
        let mut moves = Vec::new();
        for mv in &r.moves {
            let covered = queued.moves.contains(&(r.key.clone(), mv.clone(), own.clone()))
                || queued.roots.contains(&(child(&r.key, mv)?, own.clone()));
            if !covered {
                moves.push(mv.clone());
            }
        }
        if !moves.is_empty() {
            kept.push(SingleRead { moves, ..r.clone() });
        }
    }
    let excluded = reads.len() - kept.len();
    let lines = kept
        .into_iter()
        .take(args.single_max.unwrap_or(usize::MAX))
        .enumerate()
        .map(|(rank, r)| line(&r, rank, args))
        .collect::<Result<Vec<_>>>()?;
    Ok((lines, excluded))
}

fn line(r: &SingleRead, rank: usize, args: &SingleQueueArgs) -> Result<Line> {
    let ply: usize =
        r.ply.trim().parse().ok().filter(|&p| p > 0).with_context(|| {
            format!("キューの行には 1 以上の手数が要ります: {} {:?}", r.key, r.ply)
        })?;
    let own = side(&r.key);
    let weight = r.weight.map_or_else(String::new, |w| format!(" weight={w:.5}"));
    let line = Line {
        group: None,
        id: queue_id(&args.single_tag, ply, &r.key, own),
        root: format!("sfen {} {ply}", r.key),
        roots: Vec::new(),
        own: own.to_owned(),
        depth: args.single_depth,
        priority: args.priority(r.weight, rank),
        hold: false,
        game: r.moves.iter().map(|mv| vec![mv.clone()]).collect(),
        opts: args.opts(),
        memo: Some(format!(
            "single main={} main_value={} best={} best_value={} single={}{weight}",
            r.main,
            r.main_value,
            r.best,
            r.best_value,
            r.moves.join(",")
        )),
    };
    line.check().with_context(|| format!("キューの行の検査: {}", r.key))?;
    Ok(line)
}

#[cfg(test)]
pub(super) fn test_args() -> SingleQueueArgs {
    use clap::Parser;
    #[derive(Parser)]
    struct Wrap {
        #[command(flatten)]
        single: SingleQueueArgs,
    }
    Wrap::parse_from(["test"]).single
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::book_tree::reply_graph::HIRATE_KEY;

    fn read(moves: &[&str], gap: i32) -> SingleRead {
        let key = path_keys(moves).unwrap().pop().unwrap();
        let best = if side(&key) == "b" { "7g7f" } else { "3c3d" };
        SingleRead {
            key,
            ply: (moves.len() + 1).to_string(),
            main: "main".into(),
            main_value: 0,
            best: best.into(),
            best_value: gap,
            moves: vec![best.into()],
            weight: None,
        }
    }

    fn walk_table(dir: &std::path::Path, name: &str, rows: &[(&str, &str, &str)]) -> PathBuf {
        let mut text = String::from("side\tstop\tweight\tmoves\n");
        for (side, weight, moves) in rows {
            text.push_str(&format!("{side}\town\t{weight}\t{moves}\n"));
        }
        let path = dir.join(name);
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn walk_weight_counts_rows_of_the_side_to_move_once_per_row() {
        let dir = tempfile::tempdir().unwrap();
        let mut args = test_args();
        args.walk_table = vec![
            walk_table(
                dir.path(),
                "a.tsv",
                &[
                    ("b", "0.5", "2g2f 8c8d 2f2e"),
                    ("b", "0.25", "2g2f 8c8d 7g7f"),
                    ("w", "0.125", "2g2f 8c8d"),
                    // 初期局面を 2 回通ってから 2g2f の局面で止まる。
                    ("w", "0.0625", "5i5h 5a5b 5h5i 5b5a 2g2f"),
                ],
            ),
            walk_table(dir.path(), "b.tsv", &[("b", "1", "5i5h 5a5b 5h5i 5b5a")]),
        ];
        let mut reads = vec![
            read(&["2g2f"], 10),
            read(&["7g7f"], 30),
            read(&["2g2f", "8c8d"], 10),
            read(&[], 10),
            read(&["2g2f", "8c8d", "2f2e", "8d8e"], 20),
        ];
        args.rank(&mut reads, None, 0.9).unwrap();
        let got: Vec<(String, Option<f64>)> =
            reads.iter().map(|r| (r.ply.clone(), r.weight)).collect();
        assert_eq!(
            got,
            [
                ("1".to_owned(), Some(1.75)),
                ("3".to_owned(), Some(0.75)),
                ("2".to_owned(), Some(0.1875)),
                // 辿りが通らない局面は重み 0 で、差の大きい順。
                ("2".to_owned(), Some(0.0)),
                ("5".to_owned(), Some(0.0)),
            ]
        );
        assert_eq!(reads[0].key, HIRATE_KEY);
        assert_eq!(reads[3].best_value, 30);

        args.single_min_weight = 0.1875;
        args.rank(&mut reads, None, 0.0).unwrap();
        assert_eq!(reads.len(), 3);
    }

    #[test]
    fn priority_bands_use_the_highest_matching_floor_and_else_the_rank() {
        let dir = tempfile::tempdir().unwrap();
        let mut args = test_args();
        args.walk_table = vec![walk_table(
            dir.path(),
            "walk.tsv",
            &[
                ("b", "0.05", ""),
                ("w", "0.04", "2g2f"),
                ("b", "0.01", "2g2f 8c8d"),
            ],
        )];
        args.single_priority_band = vec![(0.02, 7), (0.05, 1)];
        args.single_priority = 200;
        let mut reads = vec![
            read(&["2g2f", "8c8d"], 10),
            read(&["2g2f"], 10),
            read(&[], 10),
        ];
        args.rank(&mut reads, None, 0.0).unwrap();
        let (lines, excluded) = queue(&reads, &args).unwrap();
        assert_eq!(excluded, 0);
        let got: Vec<(&str, u32)> = lines.iter().map(|l| (l.root.as_str(), l.priority)).collect();
        assert_eq!(
            got,
            [
                (format!("sfen {HIRATE_KEY} 1").as_str(), 1),
                (format!("sfen {} 2", reads[1].key).as_str(), 7),
                (format!("sfen {} 3", reads[2].key).as_str(), 202),
            ]
        );
        assert!(lines[0].memo.as_deref().unwrap().ends_with(" weight=0.06000"));
        assert!(lines[2].memo.as_deref().unwrap().ends_with(" weight=0.01000"));
    }

    #[test]
    fn walk_table_and_band_arguments_are_checked() {
        use clap::Parser;
        #[derive(Parser, Debug)]
        struct Wrap {
            #[command(flatten)]
            single: SingleQueueArgs,
        }
        let parse = |more: &[&str]| {
            Wrap::try_parse_from(
                ["test", "--single-enqueue-out", "q.jsonl"]
                    .into_iter()
                    .chain(more.iter().copied()),
            )
        };
        let bands = [
            "--single-priority-band",
            "0.05=1",
            "--single-priority-band",
            "0=7",
        ];
        assert!(parse(&bands).is_err());
        assert!(parse(&["--single-min-weight", "0.1"]).is_err());
        let with_walk = |more: &[&str]| {
            parse(&[&["--walk-table", "walk.tsv"], more].concat()).map(|w| w.single)
        };
        let args = with_walk(&bands).unwrap();
        assert_eq!(args.single_priority_band, [(0.05, 1), (0.0, 7)]);
        args.check().unwrap();
        assert!(with_walk(&["--single-priority-band", "0.05"]).is_err());
        assert!(with_walk(&["--single-priority-band", "x=1"]).is_err());
        for bad in [
            &[
                "--single-priority-band",
                "0.05=1",
                "--single-priority-band",
                "0.05=7",
            ][..],
            &["--single-priority-band", "-0.1=1"],
            &["--single-priority-band", "nan=1"],
            &["--single-priority-band", "0.1=1000001"],
            &["--single-min-weight", "nan"],
        ] {
            let checked = with_walk(bad).map_err(anyhow::Error::from).and_then(|a| a.check());
            assert!(checked.is_err(), "{bad:?}");
        }

        let dir = tempfile::tempdir().unwrap();
        for (name, text) in [
            ("empty.tsv", ""),
            ("column.tsv", "side\tweight\nb\t1\n"),
            ("side.tsv", "side\tweight\tmoves\nx\t1\t\n"),
            ("weight.tsv", "side\tweight\tmoves\nb\t-1\t\n"),
            ("move.tsv", "side\tweight\tmoves\nb\t1\t2g2e\n"),
        ] {
            let path = dir.path().join(name);
            std::fs::write(&path, text).unwrap();
            let mut args = test_args();
            args.walk_table = vec![path];
            assert!(args.rank(&mut vec![read(&[], 10)], None, 0.0).is_err(), "{name}");
        }
    }
}
