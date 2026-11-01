//! 自分が定跡の手を指した後、相手の定跡に無い合法手で定跡の局面に戻ると、戻った先の値が手前で期待した
//! 値より下がる所を一覧にする。
//!
//! 定跡の値は相手の手も定跡の手だけで伝播しているので、相手が定跡に無い手で別の定跡の局面へ合流すると、
//! 手前の値が実際より高く見えていることがある。その相手の手を読み足す局面 (掘る局面) を出す。

use super::audit::{evaluate, reject_same_file};
use super::master_table::{MasterPosition, read_master};
use super::reply_graph::{
    BookIndex, HIRATE_KEY, Preset, SideArg, build_filtered, selected_moves, thread_pool,
};
use crate::book_backprop::DrawValues;
use crate::book_db::{BookDb, apply_usi_move, position_from_sfen, strip_ply};
use crate::common::io::write_atomic;
use anyhow::{Context, Result, ensure};
use clap::Args;
use rayon::prelude::*;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::PathBuf;

#[derive(Args, Debug)]
pub(super) struct RejoinArgs {
    /// 定跡の元データの表 (TSV)
    #[arg(long)]
    master: PathBuf,
    /// 一覧 (TSV) の出力先
    #[arg(long)]
    table: PathBuf,
    /// 線。戻った先の値が、手前で期待した値からこの値以上下がる所を出す
    #[arg(long, default_value_t = 40)]
    line: i32,
    /// 自分の側
    #[arg(long, value_enum, default_value_t = SideArg::Both)]
    side: SideArg,
    /// 自分の手番で辿る手 (default = main だけ、random = main と alt)
    #[arg(long, value_enum, default_value_t = Preset::Random)]
    preset: Preset,
    /// 先手を自分として解くときの千日手の値 (対局の DrawValueBlack に揃える)
    #[arg(long, default_value_t = -70, allow_hyphen_values = true)]
    draw_value_black: i32,
    /// 後手を自分として解くときの千日手の値 (対局の DrawValueWhite に揃える)
    #[arg(long, default_value_t = 0, allow_hyphen_values = true)]
    draw_value_white: i32,
    #[command(flatten)]
    weight: WeightArgs,
    /// スレッド数 (0 = 全コア)。結果はスレッド数によらない
    #[arg(long, default_value_t = 0)]
    threads: usize,
}

/// 一覧の 1 行。値はすべて自分の側から見た値。
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Rejoin {
    pub side: &'static str,
    /// 自分が定跡の手を指す局面。
    pub key: String,
    pub ply: String,
    pub mv: String,
    pub role: &'static str,
    /// 手前で期待した値 (`mv` の伝播後の値)。
    pub expected: i32,
    /// 掘る局面 (`mv` の後の相手の手番の局面)。
    pub dig: String,
    pub rejoin_move: String,
    /// 戻った先の定跡の局面。
    pub rejoin: String,
    pub rejoin_main: String,
    /// 戻った先の main の伝播後の値。
    pub rejoin_value: i32,
    /// `key` に来る重み。
    pub weight: Option<f64>,
    /// 掘る局面での相手の局数と、そのうち `rejoin_move` の局数。
    pub dig_games: Option<u64>,
    pub rejoin_games: Option<u64>,
}

impl Rejoin {
    pub fn drop(&self) -> i32 {
        self.expected - self.rejoin_value
    }
}

#[derive(Debug, Default, PartialEq)]
pub(super) struct RejoinSummary {
    pub own_positions: usize,
    pub dig_positions: usize,
    /// 相手の定跡に無い手で定跡の局面に戻る手の数 (自分の手との組で数える)。
    pub rejoin_moves: usize,
    /// 手前の値の分からない自分の手の数。
    pub unknown_expected: usize,
    /// 戻った先の main の値の分からない組の数。
    pub unknown_rejoin: usize,
    pub findings: usize,
    /// 重みの下限で落とした数。
    pub below_min_weight: usize,
    /// 重みの下限で展開を止めた局面の数。
    pub pruned_positions: usize,
}

/// 相手の手の頻度による重みの引数。`master rejoin` と `master audit` で同じ意味。
#[derive(Args, Debug, Clone)]
pub(super) struct WeightArgs {
    /// 相手の手の頻度の定跡 (`book_from_csa` の .db、手の count を局数として使う)。渡すと重みを付ける
    #[arg(long)]
    pub opp_freq: Option<PathBuf>,
    /// 重み: 相手の局数がこれ未満の局面と手では辿るのを止める
    #[arg(long, default_value_t = 3)]
    pub min_games: u64,
    /// 重み: 初期局面からこの手数まで辿る
    #[arg(long, default_value_t = 80)]
    pub max_ply: usize,
    /// 重み: これ未満の重みの局面では辿るのを止める
    #[arg(long, default_value_t = 1e-5)]
    pub weight_floor: f64,
    /// 一覧に出す重みの下限 (`--opp-freq` を渡したときだけ使う)。rejoin は自分の手番の局面の展開にも使う
    #[arg(long, default_value_t = 0.0)]
    pub min_weight: f64,
}

impl WeightArgs {
    pub fn check(&self) -> Result<()> {
        ensure!(
            self.weight_floor >= 0.0 && self.min_weight >= 0.0,
            "重みの下限は 0 以上にしてください"
        );
        Ok(())
    }

    pub fn options(&self) -> WeightOptions {
        WeightOptions {
            min_games: self.min_games,
            max_ply: self.max_ply,
            floor: self.weight_floor,
        }
    }
}

/// 重みの付け方。
#[derive(Debug, Clone, Copy)]
pub(super) struct WeightOptions {
    pub min_games: u64,
    pub max_ply: usize,
    pub floor: f64,
}

pub(super) fn run(args: RejoinArgs) -> Result<()> {
    thread_pool(args.threads)?.install(|| run_from(args, HIRATE_KEY))
}

fn run_from(args: RejoinArgs, start: &str) -> Result<()> {
    ensure!(args.line > 0, "--line は 1 以上にしてください");
    args.weight.check()?;
    let mut inputs = vec![("--master", args.master.as_path())];
    if let Some(freq) = &args.weight.opp_freq {
        inputs.push(("--opp-freq", freq.as_path()));
    }
    reject_same_file(&args.table, &inputs)?;
    let (positions, _) = read_master(&args.master)?;
    let draws = DrawValues {
        black: args.draw_value_black,
        white: args.draw_value_white,
    };
    let values = evaluate(&positions, draws)?;
    let freq = args.weight.opp_freq.as_deref().map(BookDb::read).transpose()?;
    let index = BookIndex::new(&positions)?;
    let weight_options = args.weight.options();

    let mut table = String::from(
        "side\tkey\tply\tmove\trole\texpected\tdig\trejoin_move\trejoin\trejoin_main\trejoin_value\tdrop\texpected_black\trejoin_value_black\tweight\tdig_games\trejoin_games\n",
    );
    // 先手と後手は互いの結果を使わないので並べて解く。
    let results: Vec<(&'static str, Vec<Rejoin>, RejoinSummary)> = args
        .side
        .sides()
        .par_iter()
        .map(|&side| -> Result<_> {
            let weights = freq
                .as_ref()
                .map(|f| reach_weights(&positions, f, side, start, weight_options))
                .transpose()?;
            let query = Query {
                side,
                start,
                preset: args.preset,
                line: args.line,
                min_weight: args.weight.min_weight,
            };
            let (mut rejoins, mut summary) =
                find_rejoins(&index, &values, query, weights.as_ref(), freq.as_ref())?;
            if weights.is_some() {
                let before = rejoins.len();
                rejoins.retain(|r| r.weight.unwrap_or(0.0) >= args.weight.min_weight);
                summary.below_min_weight = before - rejoins.len();
            }
            Ok((side, rejoins, summary))
        })
        .collect::<Result<_>>()?;
    for (side, rejoins, summary) in &results {
        let side = *side;
        for r in rejoins {
            let black = |v: i32| if side == "b" { v } else { -v };
            let opt = |v: Option<u64>| v.map_or_else(String::new, |v| v.to_string());
            writeln!(
                table,
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                r.side,
                r.key,
                r.ply,
                r.mv,
                r.role,
                r.expected,
                r.dig,
                r.rejoin_move,
                r.rejoin,
                r.rejoin_main,
                r.rejoin_value,
                r.drop(),
                black(r.expected),
                black(r.rejoin_value),
                r.weight.map_or_else(String::new, |w| format!("{w:.6}")),
                opt(r.dig_games),
                opt(r.rejoin_games),
            )?;
        }
        print!("{}", format_summary(side, summary, args.line, rejoins));
    }
    write_atomic(&args.table, &table)?;
    Ok(())
}

/// 探す範囲。
#[derive(Debug, Clone, Copy)]
pub(super) struct Query<'a> {
    pub side: &'static str,
    pub start: &'a str,
    pub preset: Preset,
    pub line: i32,
    pub min_weight: f64,
}

/// 値の下がる再合流を探す。重みがあれば重みの大きい順、無ければ下がり幅の大きい順に並べる。
pub(super) fn find_rejoins(
    index: &BookIndex,
    values: &[Vec<Option<i32>>],
    query: Query<'_>,
    weights: Option<&HashMap<String, f64>>,
    freq: Option<&BookDb>,
) -> Result<(Vec<Rejoin>, RejoinSummary)> {
    let Query {
        side,
        start,
        preset,
        line,
        min_weight,
    } = query;
    let graph = build_filtered(
        index,
        side,
        start,
        |_, p| selected_moves(p, preset, None),
        // 一覧は `key` (自分の手番) の重みで絞るので、相手の手番では止めない。alt の手の先の相手の局面は
        // main だけで辿る重みが 0 になり、そこで止めると重みの大きい `key` の alt の行が消える。
        |m| {
            weights.is_none_or(|weights| {
                min_weight == 0.0 || weights.get(index.key(m)).copied().unwrap_or(0.0) >= min_weight
            })
        },
    )?;
    let mut summary = RejoinSummary {
        pruned_positions: graph.pruned_positions,
        ..RejoinSummary::default()
    };
    let mut rejoins = Vec::new();
    let mut digs = HashSet::new();
    for u in (0..graph.len()).filter(|&u| graph.own[u]) {
        let Some(m) = graph.master(u) else {
            continue;
        };
        let key = index.key(m);
        let position = index.position(m);
        summary.own_positions += 1;
        for edge in graph.edges(u) {
            let book_move =
                edge.book_move().context("内部エラー: 自分の手番に相手の手があります")?;
            let Some(expected) = values[m][book_move] else {
                summary.unknown_expected += 1;
                continue;
            };
            let p = edge.to();
            if graph.edges(p).is_empty() {
                continue;
            }
            let dig = graph.key(p)?;
            // 掘る局面で、伝播に入っている相手の手 (値の分かる main・alt)。
            let in_book: HashSet<&str> = graph.master(p).map_or_else(HashSet::new, |d| {
                index
                    .position(d)
                    .moves
                    .iter()
                    .zip(&values[d])
                    .filter(|(_, v)| v.is_some())
                    .map(|(m, _)| m.mv.as_str())
                    .collect()
            });
            let counts = freq.and_then(|f| f.entries.get(&dig));
            for reply in graph.edges(p) {
                let rejoin_move = reply.mv.to_usi();
                if in_book.contains(rejoin_move.as_str()) {
                    continue;
                }
                summary.rejoin_moves += 1;
                digs.insert(p);
                let q = graph
                    .master(reply.to())
                    .context("内部エラー: 元データに無い局面へ戻る辺です")?;
                let Some(rejoin_value) = values[q][0] else {
                    summary.unknown_rejoin += 1;
                    continue;
                };
                if expected - rejoin_value < line {
                    continue;
                }
                rejoins.push(Rejoin {
                    side,
                    key: key.to_owned(),
                    ply: position.ply.clone(),
                    mv: position.moves[book_move].mv.clone(),
                    role: if book_move == 0 { "main" } else { "alt" },
                    expected,
                    dig: dig.clone(),
                    rejoin_games: freq.map(|_| {
                        counts.map_or(0, |e| {
                            e.moves
                                .iter()
                                .filter(|m| m.move_usi.as_deref() == Some(rejoin_move.as_str()))
                                .map(|m| m.count)
                                .sum()
                        })
                    }),
                    rejoin_move,
                    rejoin: index.key(q).to_owned(),
                    rejoin_main: index.position(q).moves[0].mv.clone(),
                    rejoin_value,
                    weight: weights.map(|w| w.get(key).copied().unwrap_or(0.0)),
                    dig_games: freq
                        .map(|_| counts.map_or(0, |e| e.moves.iter().map(|m| m.count).sum())),
                });
            }
        }
    }
    summary.dig_positions = digs.len();
    summary.findings = rejoins.len();
    rejoins.sort_by(|a, b| {
        b.weight
            .unwrap_or(0.0)
            .total_cmp(&a.weight.unwrap_or(0.0))
            .then_with(|| b.drop().cmp(&a.drop()))
            .then_with(|| (&a.key, &a.mv, &a.rejoin_move).cmp(&(&b.key, &b.mv, &b.rejoin_move)))
    });
    Ok((rejoins, summary))
}

/// 自分の側の局面に来る重み。自分は main を指し、相手は局面ごとの強い対局の手の頻度で指す。相手の局数が
/// `min_games` 未満の局面・手、自分の手番で元データに無い局面、重みが `floor` 未満の局面、`max_ply` 手で止める。
/// 同じ手数で同じ局面に来た重みは足し合わせ、局面の重みは手数をまたいで足す。
pub(super) fn reach_weights(
    positions: &BTreeMap<String, MasterPosition>,
    freq: &BookDb,
    side: &str,
    start: &str,
    options: WeightOptions,
) -> Result<HashMap<String, f64>> {
    let mut reach: HashMap<String, f64> = HashMap::new();
    let mut layer: BTreeMap<String, f64> = BTreeMap::from([(start.to_owned(), 1.0)]);
    for ply in 0..=options.max_ply {
        let mut next: BTreeMap<String, f64> = BTreeMap::new();
        for (key, weight) in layer {
            *reach.entry(key.clone()).or_default() += weight;
            if ply == options.max_ply || weight < options.floor {
                continue;
            }
            let pos = position_from_sfen(&format!("{key} 1"))?;
            let mut advance = |mv: &str, w: f64| {
                let mut child = pos.clone();
                if apply_usi_move(&mut child, mv).is_ok() {
                    *next.entry(strip_ply(&child.to_sfen()).to_owned()).or_default() += w;
                }
            };
            if key.split(' ').nth(1) == Some(side) {
                if let Some(position) = positions.get(&key) {
                    advance(&position.moves[0].mv, weight);
                }
                continue;
            }
            let Some(entry) = freq.entries.get(&key) else {
                continue;
            };
            let total: u64 = entry.moves.iter().map(|m| m.count).sum();
            if total < options.min_games {
                continue;
            }
            for m in &entry.moves {
                if let Some(mv) = &m.move_usi
                    && m.count >= options.min_games
                {
                    advance(mv, weight * m.count as f64 / total as f64);
                }
            }
        }
        layer = next;
        if layer.is_empty() {
            break;
        }
    }
    Ok(reach)
}

fn format_summary(side: &str, summary: &RejoinSummary, line: i32, rejoins: &[Rejoin]) -> String {
    let mut text = String::new();
    let mut out = |s: String| {
        text.push_str(&s);
        text.push('\n');
    };
    out(format!(
        "{side}: 辿れる自分の手番の局面 {} / 相手の定跡に無い手で定跡に戻る手 {} (掘る局面 {})",
        summary.own_positions, summary.rejoin_moves, summary.dig_positions
    ));
    out(format!(
        "{side}: 値の分からない自分の手 {} / 戻った先の main の値が分からない組 {}",
        summary.unknown_expected, summary.unknown_rejoin
    ));
    out(format!(
        "{side}: 戻った先の値が {line} 以上下がる所 {} (重みの下限で除いた {})",
        rejoins.len(),
        summary.below_min_weight
    ));
    if summary.pruned_positions > 0 {
        out(format!("{side}: 重みの下限で展開を止めた局面 {}", summary.pruned_positions));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::super::reply_graph::fixture::{ROOT, after, master_text};
    use super::*;

    /// 根で 5i4h の後、相手の定跡の手 5a5b は +100 の局面 B に、定跡に無い 5a4b は +20 の局面 Q に入る。
    fn rows() -> Vec<(String, &'static str, &'static str, &'static str)> {
        let a = after(ROOT, &["5i4h"]);
        vec![
            (ROOT.to_owned(), "5i4h", "main", ""),
            (a.clone(), "5a5b", "main", ""),
            (after(&a, &["5a5b"]), "4h3h", "main", "100"),
            (after(&a, &["5a4b"]), "4h3h", "main", "20"),
        ]
    }

    fn freq() -> BookDb {
        BookDb::from_reader(freq_text().as_bytes()).unwrap()
    }

    fn freq_text() -> String {
        let a = after(ROOT, &["5i4h"]);
        format!(
            "#YANEURAOU-DB2016 1.00\nsfen {a} 2\n5a5b none 0 0 6\n5a4b none 0 0 3\n5a6b none 0 0 1\n"
        )
    }

    fn write_master(dir: &std::path::Path) -> PathBuf {
        let rows = rows();
        let rows: Vec<(&str, &str, &str, &str)> =
            rows.iter().map(|(k, m, r, e)| (k.as_str(), *m, *r, *e)).collect();
        let path = dir.join("moves.tsv");
        std::fs::write(&path, master_text(&rows)).unwrap();
        path
    }

    fn args(dir: &std::path::Path, line: i32, opp_freq: Option<PathBuf>) -> RejoinArgs {
        RejoinArgs {
            master: write_master(dir),
            table: dir.join("rejoin.tsv"),
            line,
            side: SideArg::B,
            preset: Preset::Random,
            draw_value_black: 0,
            draw_value_white: 0,
            weight: WeightArgs {
                opp_freq,
                min_games: 3,
                max_ply: 80,
                weight_floor: 1e-5,
                min_weight: 0.0,
            },
            threads: 0,
        }
    }

    #[test]
    fn lists_rejoin_by_opponent_move_outside_the_book_that_drops_the_value() {
        let dir = tempfile::tempdir().unwrap();
        run_from(args(dir.path(), 40, None), ROOT).unwrap();
        let table = std::fs::read_to_string(dir.path().join("rejoin.tsv")).unwrap();
        let lines: Vec<&str> = table.lines().collect();
        let a = after(ROOT, &["5i4h"]);
        let q = after(&a, &["5a4b"]);
        // 定跡の手 5a5b (+100 の局面へ) は伝播に入っているので出ない。
        assert_eq!(
            lines[1..],
            [format!(
                "b\t{ROOT}\t1\t5i4h\tmain\t100\t{a}\t5a4b\t{q}\t4h3h\t20\t80\t100\t20\t\t\t"
            )]
        );
        // 一覧の出力先が入力と同じなら止める。
        let mut same = args(dir.path(), 40, None);
        same.table = dir.path().join("moves.tsv");
        assert!(run_from(same, ROOT).is_err());
        let freq = dir.path().join("freq.db");
        std::fs::write(&freq, freq_text()).unwrap();
        let mut same = args(dir.path(), 40, Some(freq.clone()));
        same.table = freq.clone();
        assert!(run_from(same, ROOT).is_err());
        assert_eq!(std::fs::read_to_string(&freq).unwrap(), freq_text());
        // 下がり幅が線に届かなければ出ない。
        run_from(args(dir.path(), 81, None), ROOT).unwrap();
        let table = std::fs::read_to_string(dir.path().join("rejoin.tsv")).unwrap();
        assert_eq!(table.lines().count(), 1);
    }

    #[test]
    fn weights_follow_main_and_frequent_opponent_moves() {
        let dir = tempfile::tempdir().unwrap();
        let master = write_master(dir.path());
        let (positions, _) = read_master(&master).unwrap();
        let options = WeightOptions {
            min_games: 3,
            max_ply: 80,
            floor: 1e-5,
        };
        let weights = reach_weights(&positions, &freq(), "b", ROOT, options).unwrap();
        let a = after(ROOT, &["5i4h"]);
        // 5a6b は局数が min_games 未満なので辿らない (重みは全体の局数で割る)。
        assert_eq!(weights[ROOT], 1.0);
        assert_eq!(weights[&a], 1.0);
        assert_eq!(weights[&after(&a, &["5a5b"])], 0.6);
        assert_eq!(weights[&after(&a, &["5a4b"])], 0.3);
        assert!(!weights.contains_key(&after(&a, &["5a6b"])));

        let freq_path = dir.path().join("freq.db");
        std::fs::write(&freq_path, freq().to_db_string()).unwrap();
        run_from(args(dir.path(), 40, Some(freq_path)), ROOT).unwrap();
        let table = std::fs::read_to_string(dir.path().join("rejoin.tsv")).unwrap();
        let cells: Vec<&str> = table.lines().nth(1).unwrap().split('\t').collect();
        assert_eq!(cells[14..], ["1.000000", "10", "3"]);
    }

    #[test]
    fn weight_pruning_stops_only_low_weight_own_positions() {
        let p = super::super::reply_graph::fixture::positions(&[
            (ROOT, "5i4h", "main", "100"),
            (&after(ROOT, &["5i4h", "5a4b"]), "4h3h", "main", "20"),
        ]);
        let values: Vec<Vec<Option<i32>>> =
            p.values().map(|p| p.moves.iter().map(|m| m.eval).collect()).collect();
        let index = BookIndex::new(&p).unwrap();
        let a = after(ROOT, &["5i4h"]);
        let q = after(&a, &["5a4b"]);
        let mut weights =
            HashMap::from([(ROOT.to_owned(), 1.0), (a.clone(), 1.0), (q.clone(), 1.0)]);
        let query = |min_weight| Query {
            side: "b",
            start: ROOT,
            preset: Preset::Random,
            line: 40,
            min_weight,
        };
        let (boundary, summary) =
            find_rejoins(&index, &values, query(1.0), Some(&weights), None).unwrap();
        assert_eq!(boundary.len(), 1);
        assert_eq!(summary.pruned_positions, 0);
        weights.insert(a.clone(), 0.0);
        let (opponent_low, summary) =
            find_rejoins(&index, &values, query(0.5), Some(&weights), None).unwrap();
        assert_eq!(opponent_low.len(), 1);
        assert_eq!(opponent_low[0].rejoin, q);
        assert_eq!(summary.pruned_positions, 0);
        weights.insert(q.clone(), 0.1);
        let (rejoin_low, summary) =
            find_rejoins(&index, &values, query(0.5), Some(&weights), None).unwrap();
        assert_eq!(rejoin_low, opponent_low);
        assert_eq!(summary.pruned_positions, 1);
        let no_freq = find_rejoins(&index, &values, query(2.0), None, None).unwrap();
        assert_eq!(no_freq, find_rejoins(&index, &values, query(0.0), None, None).unwrap());
        let (pruned_root, summary) =
            find_rejoins(&index, &values, query(2.0), Some(&weights), None).unwrap();
        assert!(pruned_root.is_empty());
        assert_eq!(summary.pruned_positions, 1);
        assert_eq!(summary.own_positions, 1);
    }

    #[test]
    fn counts_moves_and_rejoins_without_values() {
        let a = after(ROOT, &["5i4h"]);
        let p = super::super::reply_graph::fixture::positions(&[
            (ROOT, "5i4h", "main", ""),
            (ROOT, "5i6h", "alt", "10"),
            (&after(&a, &["5a4b"]), "4h3h", "main", ""),
            (&after(ROOT, &["5i6h", "5a4b"]), "6h7h", "main", ""),
        ]);
        let values: Vec<Vec<Option<i32>>> =
            p.values().map(|m| m.moves.iter().map(|m| m.eval).collect()).collect();
        let index = BookIndex::new(&p).unwrap();
        let query = Query {
            side: "b",
            start: ROOT,
            preset: Preset::Random,
            line: 1,
            min_weight: 0.0,
        };
        let (found, summary) = find_rejoins(&index, &values, query, None, None).unwrap();
        assert!(found.is_empty());
        // 値の無い自分の手は根の 5i4h と、戻った先の 2 局面の main。5i4h の先は値が無いので相手の手を見ず、
        // 5i6h の後の相手の 5a4b で戻る局面は main の値が無い。
        assert_eq!(
            (summary.unknown_expected, summary.unknown_rejoin, summary.rejoin_moves),
            (3, 1, 1)
        );
    }

    #[test]
    fn default_preset_follows_main_only() {
        let a = after(ROOT, &["5i6h"]);
        let p = super::super::reply_graph::fixture::positions(&[
            (ROOT, "5i4h", "main", "0"),
            (ROOT, "5i6h", "alt", "100"),
            (&after(&a, &["5a4b"]), "6h7h", "main", "0"),
        ]);
        let values: Vec<Vec<Option<i32>>> =
            p.values().map(|m| m.moves.iter().map(|m| m.eval).collect()).collect();
        let index = BookIndex::new(&p).unwrap();
        let query = |preset| Query {
            side: "b",
            start: ROOT,
            preset,
            line: 40,
            min_weight: 0.0,
        };
        let (found, _) = find_rejoins(&index, &values, query(Preset::Random), None, None).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].mv.as_str(), found[0].role, found[0].drop()), ("5i6h", "alt", 100));
        let (found, _) = find_rejoins(&index, &values, query(Preset::Default), None, None).unwrap();
        assert!(found.is_empty());
    }
}
