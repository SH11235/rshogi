//! 自分が定跡の手を指した後、相手の定跡に無い合法手で定跡の局面に戻ると、戻った先の値が手前で期待した
//! 値より下がる所を一覧にする。
//!
//! 定跡の値は相手の手も定跡の手だけで伝播しているので、相手が定跡に無い手で別の定跡の局面へ合流すると、
//! 手前の値が実際より高く見えていることがある。その相手の手を読み足す局面 (掘る局面) を出す。

use super::audit::{
    Fill, Flips, add_opponent_reply, evaluate_for_side, evaluate_with, reject_same_file,
    report_direct_lookup,
};
use super::master_table::{MasterPosition, positions_from_rows, read_rows};
use super::reply_graph::{
    BookIndex, HIRATE_KEY, Preset, SideArg, build_filtered, selected_moves, thread_pool,
};
use crate::book_backprop::DrawValues;
use crate::book_db::{BookDb, apply_usi_move, position_from_sfen, strip_ply};
use crate::common::io::write_atomic;
use anyhow::{Context, Result, ensure};
use clap::Args;
use rayon::prelude::*;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
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
    #[arg(long, default_value_t = 70, allow_hyphen_values = true)]
    draw_value_white: i32,
    /// 定跡を直接の key だけで引く (対局で先後反転した局面の定跡を引かない) 前提で手の値を伝播する。反転した局面への
    /// 辺の値は使うが、局面の行が反転した向きの値も保つことは確かめず、反転キーで合流した局面では止めない
    #[arg(long)]
    direct_lookup: bool,
    /// --direct-lookup で確かめなかった局面 (値の一番高い手が反転した向きの値を保たない局面と、反転した向きの保証が
    /// 破れる局面) の一覧 (TSV: kind・key・保存されている向きの値・反転した向きの値) の出力先
    #[arg(long, value_name = "PATH", requires = "direct_lookup")]
    direct_lookup_table: Option<PathBuf>,
    /// 相手の手番では、読みの値のある record も点検と同じ規則で伝播に使う
    #[arg(long)]
    opp_records: bool,
    /// 相手の手番では、読みの値のある bench も点検と同じ規則で伝播に使う
    #[arg(long)]
    opp_bench: bool,
    /// 元データに無い相手の局面を点検と同じ規則で補う (反転した局面へ辺を張らない)
    #[arg(long)]
    bridge_replies: bool,
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
    /// 重み: 相手の局数がこれ未満の局面と手では頻度で指さない (`--opp-book` が無ければそこで止める)
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
    /// 重み: 相手の手の大きい定跡 (.db か .ybb)。`--opp-freq` の局数が足りない局面ではこの定跡の手で指す
    /// (`master walk --opp-book` と同じ辿り方)
    #[arg(long, requires = "opp_freq")]
    pub opp_book: Option<PathBuf>,
    /// 重み: 大きい定跡で指すとき、最善の値からこの差以内の手を指す
    #[arg(long, default_value_t = 30, requires = "opp_book")]
    pub opp_book_diff: i32,
    /// 重み: 大きい定跡で指すとき、最善 (同じ値の手は等分) に割り当てる重み。残りを差以内の他の手で等分する
    #[arg(long, default_value_t = 0.9, requires = "opp_book")]
    pub opp_book_best: f64,
    /// 重み: 相手の手番では大きい定跡を先に引き、局面があればその手だけで指す
    #[arg(long, requires = "opp_book")]
    pub opp_book_first: bool,
}

impl WeightArgs {
    pub fn check(&self) -> Result<()> {
        ensure!(
            self.weight_floor >= 0.0 && self.min_weight >= 0.0,
            "重みの下限は 0 以上にしてください"
        );
        ensure!(
            (0.0..=1.0).contains(&self.opp_book_best),
            "--opp-book-best は 0〜1 にしてください"
        );
        ensure!(self.opp_book_diff >= 0, "--opp-book-diff は 0 以上にしてください");
        Ok(())
    }

    pub fn options(&self) -> WeightOptions<'_> {
        WeightOptions {
            min_games: self.min_games,
            max_ply: self.max_ply,
            floor: self.weight_floor,
            book: self.opp_book.as_deref(),
            book_diff: self.opp_book_diff,
            book_best: self.opp_book_best,
            book_first: self.opp_book_first,
        }
    }

    /// 入力のファイル (出力と同じでないかの検査に使う)。
    pub fn inputs(&self) -> Vec<(&'static str, &std::path::Path)> {
        let mut inputs = Vec::new();
        if let Some(freq) = &self.opp_freq {
            inputs.push(("--opp-freq", freq.as_path()));
        }
        if let Some(book) = &self.opp_book {
            inputs.push(("--opp-book", book.as_path()));
        }
        inputs
    }
}

/// 重みの付け方。
#[derive(Debug, Clone, Copy)]
pub(super) struct WeightOptions<'a> {
    pub min_games: u64,
    pub max_ply: usize,
    pub floor: f64,
    pub book: Option<&'a std::path::Path>,
    pub book_diff: i32,
    pub book_best: f64,
    pub book_first: bool,
}

pub(super) fn run(args: RejoinArgs) -> Result<()> {
    thread_pool(args.threads)?.install(|| run_from(args, HIRATE_KEY))
}

fn run_from(args: RejoinArgs, start: &str) -> Result<()> {
    ensure!(args.line > 0, "--line は 1 以上にしてください");
    args.weight.check()?;
    let mut inputs = vec![("--master", args.master.as_path())];
    inputs.extend(args.weight.inputs());
    reject_same_file(&args.table, &inputs)?;
    if let Some(table) = &args.direct_lookup_table {
        inputs.push(("--table", args.table.as_path()));
        reject_same_file(table, &inputs)?;
    }
    let draws = DrawValues {
        black: args.draw_value_black,
        white: args.draw_value_white,
    };
    let roles: Vec<_> = [(args.opp_records, "record"), (args.opp_bench, "bench")]
        .into_iter()
        .filter_map(|(on, role)| on.then_some(role))
        .collect();
    let enhanced = args.bridge_replies || !roles.is_empty();
    let mut records = BTreeMap::new();
    let reader = std::io::BufReader::new(std::fs::File::open(&args.master)?);
    let (positions, _) = positions_from_rows(read_rows(reader)?.map(|entry| {
        let (at, row) = entry?;
        add_opponent_reply(&mut records, &row, &roles)?;
        Ok((at, row))
    }))?;
    ensure!(
        !enhanced || args.direct_lookup_table.is_none(),
        "--opp-records・--opp-bench・--bridge-replies は反転した局面へ辺を張らないので --direct-lookup-table は使いません"
    );
    let values = if enhanced {
        None
    } else {
        let (values, stats) =
            evaluate_with(&positions, draws, Flips::joined(args.direct_lookup), None)?;
        report_direct_lookup(&stats, args.direct_lookup_table.as_deref())?;
        Some(values)
    };
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
            let evaluated = if enhanced {
                Some(evaluate_for_side(
                    &positions,
                    &records,
                    draws,
                    if args.bridge_replies {
                        Fill::Bridge
                    } else {
                        Fill::Off
                    },
                    side,
                )?)
            } else {
                None
            };
            let values = evaluated
                .as_ref()
                .map(|v| &v.values)
                .or(values.as_ref())
                .context("内部エラー: 伝播値がありません")?;
            let (mut rejoins, mut summary) = find_rejoins(
                &index,
                values,
                query,
                weights.as_ref(),
                freq.as_ref(),
                evaluated.as_ref().map(|v| &v.replies),
            )?;
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
    replies: Option<&BTreeMap<String, BTreeSet<String>>>,
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
                if in_book.contains(rejoin_move.as_str())
                    || replies.and_then(|r| r.get(&dig)).is_some_and(|r| r.contains(&rejoin_move))
                {
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
/// `min_games` 未満の局面・手の分は大きい定跡 (`book`) の手で指し、それも無ければ止める。自分の手番で元データに
/// 無い局面、重みが `floor` 未満の局面、`max_ply` 手で止める。
/// 同じ手数で同じ局面に来た重みは足し合わせ、局面の重みは手数をまたいで足す。
pub(super) fn reach_weights(
    positions: &BTreeMap<String, MasterPosition>,
    freq: &BookDb,
    side: &str,
    start: &str,
    options: WeightOptions<'_>,
) -> Result<HashMap<String, f64>> {
    // 先手と後手を並べて辿るので、大きい定跡は呼び出しごとに開く。
    let book = options.book.map(super::walk::open_book).transpose()?;
    let opp = super::walk::OppOptions {
        min_games: options.min_games,
        book_diff: options.book_diff,
        book_best: options.book_best,
        book_first: options.book_first,
    };
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
            // 大きい定跡の手の割り振りは `master walk --opp-book` と同じ。
            let book_shares = |book: &rshogi_book::Book| -> Result<Vec<(String, f64)>> {
                let moves = book.entry_moves(&format!("{key} 1"))?;
                let mut shares = super::walk::book_moves(&moves, opp);
                shares.retain(|(_, share)| *share > 0.0);
                Ok(shares)
            };
            if options.book_first
                && let Some(book) = &book
            {
                let shares = book_shares(book)?;
                if !shares.is_empty() {
                    for (mv, share) in shares {
                        advance(&mv, weight * share);
                    }
                    continue;
                }
            }
            let mut rest = 1.0;
            if let Some(entry) = freq.entries.get(&key) {
                let total: u64 = entry.moves.iter().map(|m| m.count).sum();
                if total >= options.min_games && total > 0 {
                    for m in &entry.moves {
                        if let Some(mv) = &m.move_usi
                            && m.count >= options.min_games
                        {
                            advance(mv, weight * m.count as f64 / total as f64);
                            rest -= m.count as f64 / total as f64;
                        }
                    }
                }
            }
            if let Some(book) = &book
                && rest > 1e-12
            {
                for (mv, share) in book_shares(book)? {
                    advance(&mv, weight * rest * share);
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
    use super::super::master_table::read_master;
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
            direct_lookup: false,
            direct_lookup_table: None,
            opp_records: false,
            opp_bench: false,
            bridge_replies: false,
            weight: WeightArgs {
                opp_freq,
                min_games: 3,
                max_ply: 80,
                weight_floor: 1e-5,
                min_weight: 0.0,
                opp_book: None,
                opp_book_diff: 30,
                opp_book_best: 0.9,
                opp_book_first: false,
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
            book: None,
            book_diff: 30,
            book_best: 0.9,
            book_first: false,
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
    fn weights_fill_rare_share_from_book_like_walk() {
        let dir = tempfile::tempdir().unwrap();
        let master = write_master(dir.path());
        let (positions, _) = read_master(&master).unwrap();
        let a = after(ROOT, &["5i4h"]);
        let book = dir.path().join("book.db");
        std::fs::write(
            &book,
            format!("#YANEURAOU-DB2016 1.00\nsfen {a} 2\n5a6b none 50 0 1\n5a5b none 40 0 1\n5a4b none 0 0 1\n"),
        )
        .unwrap();
        let mut options = WeightOptions {
            min_games: 3,
            max_ply: 80,
            floor: 1e-5,
            book: Some(&book),
            book_diff: 30,
            book_best: 0.9,
            book_first: false,
        };
        let close = |x: f64, y: f64| (x - y).abs() < 1e-12;
        // 局数が min_games 未満の 5a6b に行く 0.1 を、大きい定跡の最善 0.9・差以内の 5a5b 0.1 で割り振る。
        let weights = reach_weights(&positions, &freq(), "b", ROOT, options).unwrap();
        assert!(close(weights[&after(&a, &["5a5b"])], 0.61));
        assert!(close(weights[&after(&a, &["5a4b"])], 0.3));
        assert!(close(weights[&after(&a, &["5a6b"])], 0.09));
        options.book_first = true;
        let weights = reach_weights(&positions, &freq(), "b", ROOT, options).unwrap();
        assert!(close(weights[&after(&a, &["5a6b"])], 0.9));
        assert!(close(weights[&after(&a, &["5a5b"])], 0.1));
        assert!(!weights.contains_key(&after(&a, &["5a4b"])));

        // 頻度の定跡に局面が無ければ、局面の重み全部を大きい定跡で指す。
        options.book_first = false;
        let empty = BookDb::default();
        let weights = reach_weights(&positions, &empty, "b", ROOT, options).unwrap();
        assert!(close(weights[&after(&a, &["5a6b"])], 0.9));
        assert!(close(weights[&after(&a, &["5a5b"])], 0.1));

        let freq_path = dir.path().join("freq.db");
        std::fs::write(&freq_path, freq().to_db_string()).unwrap();
        let mut with_book = args(dir.path(), 40, Some(freq_path));
        with_book.weight.opp_book = Some(book.clone());
        with_book.table = book.clone();
        let before = std::fs::read_to_string(&book).unwrap();
        assert!(run_from(with_book, ROOT).is_err());
        assert_eq!(std::fs::read_to_string(&book).unwrap(), before);
        let mut bad = args(dir.path(), 40, None);
        bad.weight.opp_book_best = 1.5;
        assert!(bad.weight.check().is_err());
        bad.weight.opp_book_best = 0.9;
        bad.weight.opp_book_diff = -1;
        assert!(bad.weight.check().is_err());
    }

    #[test]
    fn direct_lookup_reaches_the_propagation() {
        use crate::book_backprop::FlipFailure;
        use crate::book_tree::model::child;
        // S から玉だけの 4 手で S に戻る循環。T から指すと S の反転した局面に着き、S の -30 の手は先手なら指し、
        // 後手なら循環を選ぶので、S の 1 行では両方の向きを表せない。
        const S: &str = "4k4/9/9/9/9/9/9/9/4K4 b -";
        const T: &str = "4k4/9/9/9/9/9/9/4K4/9 b -";
        let c1 = child(S, "5i4i").unwrap();
        let c2 = child(&c1, "5a4a").unwrap();
        let c3 = child(&c2, "4i5i").unwrap();
        let rows = [
            (S, "5i4i", "main", "0"),
            (S, "5i6i", "alt", "-30"),
            (c1.as_str(), "5a4a", "main", "0"),
            (c2.as_str(), "4i5i", "main", "0"),
            (c3.as_str(), "4a5a", "main", "0"),
            (T, "5h5i", "main", "0"),
        ];
        let mut text = crate::book_tree::master_table::COLUMNS.join("\t");
        text.push('\n');
        for (key, mv, role, evals) in rows {
            let line = crate::book_tree::master_table::test_line(&[
                ("key", key.to_string()),
                ("ply", "1".into()),
                ("move", mv.to_string()),
                ("role", role.to_string()),
                ("source", "read".into()),
                ("evals", evals.to_string()),
                ("tier", "wide".into()),
                ("since", "t".into()),
                ("note", "n".into()),
            ]);
            text.push_str(&line);
            text.push('\n');
        }
        let dir = tempfile::tempdir().unwrap();
        let master = dir.path().join("flip.tsv");
        std::fs::write(&master, &text).unwrap();
        let flips = dir.path().join("unchecked.tsv");
        let run = |direct_lookup: bool| {
            run_from(
                RejoinArgs {
                    master: master.clone(),
                    draw_value_black: -70,
                    draw_value_white: 70,
                    direct_lookup,
                    direct_lookup_table: direct_lookup.then(|| flips.clone()),
                    ..args(dir.path(), 40, None)
                },
                S,
            )
        };
        let error = run(false).unwrap_err();
        assert!(error.downcast_ref::<FlipFailure>().is_some(), "{error:#}");
        assert!(!dir.path().join("rejoin.tsv").exists());
        run(true).unwrap();
        assert!(dir.path().join("rejoin.tsv").exists());
        assert_eq!(
            std::fs::read_to_string(&flips).unwrap(),
            format!("kind\tkey\tstored\tflipped\nvalue\t{S}\t-30\t70\n")
        );
    }

    #[test]
    fn cli_opp_book_needs_opp_freq() {
        #[derive(clap::Parser)]
        struct Cli {
            #[command(flatten)]
            weight: WeightArgs,
        }
        use clap::Parser;
        assert!(Cli::try_parse_from(["t", "--opp-book", "b.db"]).is_err());
        assert!(Cli::try_parse_from(["t", "--opp-book", "b.db", "--opp-freq", "f.db"]).is_ok());
        assert!(Cli::try_parse_from(["t", "--opp-book-first"]).is_err());
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
            find_rejoins(&index, &values, query(1.0), Some(&weights), None, None).unwrap();
        assert_eq!(boundary.len(), 1);
        assert_eq!(summary.pruned_positions, 0);
        weights.insert(a.clone(), 0.0);
        let (opponent_low, summary) =
            find_rejoins(&index, &values, query(0.5), Some(&weights), None, None).unwrap();
        assert_eq!(opponent_low.len(), 1);
        assert_eq!(opponent_low[0].rejoin, q);
        assert_eq!(summary.pruned_positions, 0);
        weights.insert(q.clone(), 0.1);
        let (rejoin_low, summary) =
            find_rejoins(&index, &values, query(0.5), Some(&weights), None, None).unwrap();
        assert_eq!(rejoin_low, opponent_low);
        assert_eq!(summary.pruned_positions, 1);
        let no_freq = find_rejoins(&index, &values, query(2.0), None, None, None).unwrap();
        assert_eq!(no_freq, find_rejoins(&index, &values, query(0.0), None, None, None).unwrap());
        let (pruned_root, summary) =
            find_rejoins(&index, &values, query(2.0), Some(&weights), None, None).unwrap();
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
        let (found, summary) = find_rejoins(&index, &values, query, None, None, None).unwrap();
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
        let (found, _) =
            find_rejoins(&index, &values, query(Preset::Random), None, None, None).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].mv.as_str(), found[0].role, found[0].drop()), ("5i6h", "alt", 100));
        let (found, _) =
            find_rejoins(&index, &values, query(Preset::Default), None, None, None).unwrap();
        assert!(found.is_empty());
    }
    #[test]
    fn opponent_read_replies_share_audit_values_and_leave_other_holes_visible() {
        for role in ["record", "bench"] {
            for (enabled, bridge, main) in [
                (false, false, true),
                (true, false, true),
                (true, true, true),
                (true, false, false),
                (true, true, false),
            ] {
                let dir = tempfile::tempdir().unwrap();
                let mut args = args(dir.path(), 40, None);
                let dig = after(ROOT, &["5i4h"]);
                let known = after(&dig, &["5a4b"]);
                let outside = after(&dig, &["5a6b"]);
                let mut rows = vec![
                    (ROOT, "5i4h", "main", "100"),
                    (&dig, "5a4b", role, "20"),
                    (&dig, "5a6b", role, ""),
                    (&known, "4h3h", "main", "0"),
                    (&outside, "4h3h", "main", "-200"),
                ];
                if main {
                    rows.push((&dig, "5a5b", "main", "-100"));
                }
                std::fs::write(&args.master, master_text(&rows)).unwrap();
                args.opp_records = enabled && role == "record";
                args.opp_bench = enabled && role == "bench";
                args.bridge_replies = bridge;
                let table = args.table.clone();
                run_from(args, ROOT).unwrap();
                let table = std::fs::read_to_string(table).unwrap();
                let rows: Vec<Vec<_>> =
                    table.lines().skip(1).map(|l| l.split('\t').collect()).collect();
                assert_eq!(rows.iter().any(|r| r[7] == "5a4b"), !enabled, "{table}");
                let remaining = rows.iter().find(|r| r[7] == "5a6b").unwrap();
                assert_eq!(remaining[5], if enabled { "0" } else { "100" }, "{table}");
                assert_eq!(remaining[10], "-200", "{table}");
            }
        }
    }
}
