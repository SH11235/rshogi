//! 自分が定跡の手を指した後、相手の定跡に無い合法手で定跡の局面に戻ると、戻った先の値が手前で期待した
//! 値より下がる所を一覧にする (YaneuraOu の `peta rejoin` に当たる)。
//!
//! 定跡の値は相手の手も定跡の手だけで伝播しているので、相手が定跡に無い手で別の定跡の局面へ合流すると、
//! 手前の値が実際より高く見えていることがある。その相手の手を読み足す局面 (掘る局面) を出す。

use super::audit::{MasterPosition, evaluate, read_master, reject_same_file};
use super::reply_graph::{BookHashes, HIRATE_KEY, Preset, SideArg, build, selected_moves};
use crate::book_backprop::DrawValues;
use crate::book_db::{BookDb, apply_usi_move, position_from_sfen, strip_ply};
use crate::common::io::write_atomic;
use anyhow::{Context, Result, ensure};
use clap::Args;
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
    /// 相手の手の頻度の定跡 (`book_from_csa` の .db、手の count を局数として使う)。渡すと重みを付ける
    #[arg(long)]
    opp_freq: Option<PathBuf>,
    /// 重み: 相手の局数がこれ未満の局面と手では辿るのを止める
    #[arg(long, default_value_t = 3)]
    min_games: u64,
    /// 重み: 初期局面からこの手数まで辿る
    #[arg(long, default_value_t = 80)]
    max_ply: usize,
    /// 重み: これ未満の重みの局面では辿るのを止める
    #[arg(long, default_value_t = 1e-5)]
    weight_floor: f64,
    /// 一覧に出す重みの下限 (`--opp-freq` を渡したときだけ使う)
    #[arg(long, default_value_t = 0.0)]
    min_weight: f64,
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
}

/// 重みの付け方。
#[derive(Debug, Clone, Copy)]
pub(super) struct WeightOptions {
    pub min_games: u64,
    pub max_ply: usize,
    pub floor: f64,
}

pub(super) fn run(args: RejoinArgs) -> Result<()> {
    run_from(args, HIRATE_KEY)
}

fn run_from(args: RejoinArgs, start: &str) -> Result<()> {
    ensure!(args.line > 0, "--line は 1 以上にしてください");
    ensure!(
        args.weight_floor >= 0.0 && args.min_weight >= 0.0,
        "重みの下限は 0 以上にしてください"
    );
    let mut inputs = vec![("--master", args.master.as_path())];
    if let Some(freq) = &args.opp_freq {
        inputs.push(("--opp-freq", freq.as_path()));
    }
    reject_same_file(&args.table, &inputs)?;
    let (positions, _) = read_master(&args.master)?;
    let draws = DrawValues {
        black: args.draw_value_black,
        white: args.draw_value_white,
    };
    let work = tempfile::tempdir().context("一時ディレクトリを作れません")?;
    let evaluated = evaluate(&positions, draws, work.path())?;
    let values: HashMap<&str, Vec<Option<i32>>> = positions
        .keys()
        .zip(&evaluated.moves)
        .map(|(k, m)| (k.as_str(), m.iter().map(|m| m.as_ref().map(|m| m.value)).collect()))
        .collect();
    drop(evaluated);
    let freq = args.opp_freq.as_deref().map(BookDb::read).transpose()?;
    let hashes = BookHashes::new(&positions)?;
    let weight_options = WeightOptions {
        min_games: args.min_games,
        max_ply: args.max_ply,
        floor: args.weight_floor,
    };

    let mut table = String::from(
        "side\tkey\tply\tmove\trole\texpected\tdig\trejoin_move\trejoin\trejoin_main\trejoin_value\tdrop\texpected_black\trejoin_value_black\tweight\tdig_games\trejoin_games\n",
    );
    for &side in args.side.sides() {
        let weights = freq
            .as_ref()
            .map(|f| reach_weights(&positions, f, side, start, weight_options))
            .transpose()?;
        let query = Query {
            side,
            start,
            preset: args.preset,
            line: args.line,
        };
        let (mut rejoins, mut summary) =
            find_rejoins(&positions, &hashes, &values, query, weights.as_ref(), freq.as_ref())?;
        if weights.is_some() {
            let before = rejoins.len();
            rejoins.retain(|r| r.weight.unwrap_or(0.0) >= args.min_weight);
            summary.below_min_weight = before - rejoins.len();
        }
        for r in &rejoins {
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
        print!("{}", format_summary(side, &summary, args.line, &rejoins));
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
}

/// 値の下がる再合流を探す。重みがあれば重みの大きい順、無ければ下がり幅の大きい順に並べる。
pub(super) fn find_rejoins(
    positions: &BTreeMap<String, MasterPosition>,
    hashes: &BookHashes,
    values: &HashMap<&str, Vec<Option<i32>>>,
    query: Query<'_>,
    weights: Option<&HashMap<String, f64>>,
    freq: Option<&BookDb>,
) -> Result<(Vec<Rejoin>, RejoinSummary)> {
    let Query {
        side,
        start,
        preset,
        line,
    } = query;
    let graph = build(positions, hashes, side, start, |_, p| selected_moves(p, preset, None))?;
    let mut summary = RejoinSummary::default();
    let mut rejoins = Vec::new();
    let mut digs = HashSet::new();
    for u in (0..graph.len()).filter(|&u| graph.own[u]) {
        let key = &graph.keys[u];
        let Some(position) = positions.get(key) else {
            continue;
        };
        summary.own_positions += 1;
        for edge in &graph.edges[u] {
            let book_move = edge.book_move.context("内部エラー: 自分の手番に相手の手があります")?;
            let Some(expected) = values[key.as_str()][book_move] else {
                summary.unknown_expected += 1;
                continue;
            };
            let p = edge.to;
            let dig = &graph.keys[p];
            // 掘る局面で、伝播に入っている相手の手 (値の分かる main・alt)。
            let in_book: HashSet<&str> = positions.get(dig).map_or_else(HashSet::new, |d| {
                d.moves
                    .iter()
                    .zip(&values[dig.as_str()])
                    .filter(|(_, v)| v.is_some())
                    .map(|(m, _)| m.mv.as_str())
                    .collect()
            });
            let counts = freq.and_then(|f| f.entries.get(dig));
            for reply in &graph.edges[p] {
                if in_book.contains(reply.mv.as_str()) {
                    continue;
                }
                summary.rejoin_moves += 1;
                digs.insert(p);
                let rejoin = &graph.keys[reply.to];
                let Some(rejoin_value) = values[rejoin.as_str()][0] else {
                    summary.unknown_rejoin += 1;
                    continue;
                };
                if expected - rejoin_value < line {
                    continue;
                }
                rejoins.push(Rejoin {
                    side,
                    key: key.clone(),
                    ply: position.ply.clone(),
                    mv: edge.mv.clone(),
                    role: if book_move == 0 { "main" } else { "alt" },
                    expected,
                    dig: dig.clone(),
                    rejoin_move: reply.mv.clone(),
                    rejoin: rejoin.clone(),
                    rejoin_main: positions[rejoin].moves[0].mv.clone(),
                    rejoin_value,
                    weight: weights.map(|w| w.get(key).copied().unwrap_or(0.0)),
                    dig_games: freq
                        .map(|_| counts.map_or(0, |e| e.moves.iter().map(|m| m.count).sum())),
                    rejoin_games: freq.map(|_| {
                        counts.map_or(0, |e| {
                            e.moves
                                .iter()
                                .filter(|m| m.move_usi.as_deref() == Some(reply.mv.as_str()))
                                .map(|m| m.count)
                                .sum()
                        })
                    }),
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
        let a = after(ROOT, &["5i4h"]);
        let text = format!(
            "#YANEURAOU-DB2016 1.00\nsfen {a} 2\n5a5b none 0 0 6\n5a4b none 0 0 3\n5a6b none 0 0 1\n"
        );
        BookDb::from_reader(text.as_bytes()).unwrap()
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
            opp_freq,
            min_games: 3,
            max_ply: 80,
            weight_floor: 1e-5,
            min_weight: 0.0,
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
    fn counts_moves_and_rejoins_without_values() {
        let a = after(ROOT, &["5i4h"]);
        let p = super::super::reply_graph::fixture::positions(&[
            (ROOT, "5i4h", "main", ""),
            (ROOT, "5i6h", "alt", "10"),
            (&after(&a, &["5a4b"]), "4h3h", "main", ""),
            (&after(ROOT, &["5i6h", "5a4b"]), "6h7h", "main", ""),
        ]);
        let values: HashMap<&str, Vec<Option<i32>>> = p
            .iter()
            .map(|(k, m)| (k.as_str(), m.moves.iter().map(|m| m.eval).collect()))
            .collect();
        let hashes = BookHashes::new(&p).unwrap();
        let query = Query {
            side: "b",
            start: ROOT,
            preset: Preset::Random,
            line: 1,
        };
        let (found, summary) = find_rejoins(&p, &hashes, &values, query, None, None).unwrap();
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
        let values: HashMap<&str, Vec<Option<i32>>> = p
            .iter()
            .map(|(k, m)| (k.as_str(), m.moves.iter().map(|m| m.eval).collect()))
            .collect();
        let hashes = BookHashes::new(&p).unwrap();
        let query = |preset| Query {
            side: "b",
            start: ROOT,
            preset,
            line: 40,
        };
        let (found, _) =
            find_rejoins(&p, &hashes, &values, query(Preset::Random), None, None).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].mv.as_str(), found[0].role, found[0].drop()), ("5i6h", "alt", 100));
        let (found, _) =
            find_rejoins(&p, &hashes, &values, query(Preset::Default), None, None).unwrap();
        assert!(found.is_empty());
    }
}
