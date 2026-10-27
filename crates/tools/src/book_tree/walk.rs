//! 自分は定跡の main を指し、相手は強い対局の手の頻度か大きい定跡の手で指すとして初期局面から辿り、
//! 自分の定跡が切れて自分が読み始める局面とその重みを出す。木の根を選ぶのと、相手の定跡がその先どこまで
//! 続くかを見るのに使う。

use super::audit::{MasterPosition, read_master, reject_same_file};
use super::refutation::format_queue;
use super::rejoin::WeightArgs;
use super::reply_graph::{HIRATE_KEY, SideArg};
use super::targets::{Line, MAX_DEPTH, Opts};
use crate::book_db::{BookDb, apply_usi_move, position_from_sfen, strip_ply};
use crate::common::io::write_atomic;
use anyhow::{Context, Result, ensure};
use clap::Args;
use rshogi_book::{Book, BookEntryMove};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::PathBuf;

#[derive(Args, Debug)]
pub(super) struct WalkArgs {
    /// 定跡の元データの表 (TSV)
    #[arg(long)]
    master: PathBuf,
    /// 止まった局面の一覧 (TSV) の出力先
    #[arg(long)]
    table: PathBuf,
    /// 自分の側
    #[arg(long, value_enum, default_value_t = SideArg::Both)]
    side: SideArg,
    #[command(flatten)]
    weight: WeightArgs,
    /// 相手の手の大きい定跡 (.db か YaneuraOu の .ybb)。`--opp-freq` の局数が足りない局面ではこの定跡の手で指す
    #[arg(long)]
    opp_book: Option<PathBuf>,
    /// 大きい定跡で指すとき、最善の値からこの差以内の手を指す
    #[arg(long, default_value_t = 30)]
    opp_book_diff: i32,
    /// 大きい定跡で指すとき、最善 (同じ値の手は等分) に割り当てる重み。残りを差以内の他の手で等分する
    #[arg(long, default_value_t = 0.9)]
    opp_book_best: f64,
    /// 自分が読み始める局面から、大きい定跡の最善を両者が指し続けた手順をこの手数まで辿る
    #[arg(long, default_value_t = 100)]
    cont_max: usize,
    /// 自分が読み始める局面のうち、重みが `--min-weight` 以上のものを木のキューの行 (JSON Lines) に書く
    #[arg(long)]
    enqueue_out: Option<PathBuf>,
    /// キューの行の段数
    #[arg(long, default_value_t = 6)]
    depth: usize,
    /// キューの行の priority の始まり (重みの順位を足す)
    #[arg(long, default_value_t = 300)]
    priority: u32,
    /// キューの行の id `<tag>_<局面の hash>_<b|w>` に入れる名前
    #[arg(long, default_value = "walk")]
    tag: String,
    /// キューの行の `game` に、大きい定跡の最善を両者が指し続けた手順を入れる (木がその手を必ず読む)
    #[arg(long, requires = "opp_book")]
    game_from_book: bool,
}

/// 止まり方。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) enum Stop {
    /// 自分の手番で元データに行が無い (自分が読み始める)。
    Own,
    /// 相手の手番で、局数が足りず大きい定跡にも無い (相手の手が分からない)。
    Out,
    /// 相手の手番で、局数が `--min-games` 未満の手に行った分。
    Rare,
    /// 重みが `--weight-floor` 未満。
    Tiny,
    /// `--max-ply` に達した。
    Max,
}

impl Stop {
    const ALL: [Stop; 5] = [Stop::Own, Stop::Out, Stop::Rare, Stop::Tiny, Stop::Max];

    fn as_str(self) -> &'static str {
        match self {
            Stop::Own => "own",
            Stop::Out => "out",
            Stop::Rare => "rare",
            Stop::Tiny => "tiny",
            Stop::Max => "max",
        }
    }
}

/// 一覧の 1 行。
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Row {
    pub side: &'static str,
    pub stop: Stop,
    /// 初期局面からの手数。
    pub ply: usize,
    pub weight: f64,
    pub key: String,
    /// 初期局面からの手順 (同じ手数で同じ局面に来た手順のうち最初に辿ったもの)。
    pub moves: Vec<String>,
    /// `--opp-freq` のこの局面の局数。
    pub games: Option<u64>,
    /// 大きい定跡のこの局面の手の数。
    pub book_moves: Option<usize>,
    /// 大きい定跡の最善を両者が指し続けた手順 (`Own` だけ)。
    pub cont: Vec<String>,
    /// その手順の最後の局面の最善の値 (先手から見た値)。
    pub cont_end_black: Option<i32>,
}

/// ply 付き SFEN で大きい定跡の手を引く関数。
type Lookup<'a> = Box<dyn FnMut(&str) -> Result<Vec<BookEntryMove>> + 'a>;

/// 大きい定跡の引き方。同じ局面は 1 回だけ引く。
pub(super) struct BookProbe<'a> {
    lookup: Lookup<'a>,
    cache: HashMap<String, Vec<(String, i32)>>,
}

impl<'a> BookProbe<'a> {
    pub fn new(lookup: impl FnMut(&str) -> Result<Vec<BookEntryMove>> + 'a) -> Self {
        Self {
            lookup: Box::new(lookup),
            cache: HashMap::new(),
        }
    }

    /// 手と手番の側から見た値を定跡ファイルの順で返す。無ければ空。
    fn moves(&mut self, key: &str) -> Result<&[(String, i32)]> {
        if !self.cache.contains_key(key) {
            let moves = (self.lookup)(&format!("{key} 1"))?
                .into_iter()
                .map(|m| (m.move_usi, m.value))
                .collect();
            self.cache.insert(key.to_owned(), moves);
        }
        Ok(&self.cache[key])
    }

    /// 最善の手 (同じ値なら定跡ファイルで先の手)。
    fn best(&mut self, key: &str) -> Result<Option<(String, i32)>> {
        let moves = self.moves(key)?;
        Ok(moves
            .iter()
            .fold(None::<&(String, i32)>, |b, m| match b {
                Some(b) if b.1 >= m.1 => Some(b),
                _ => Some(m),
            })
            .cloned())
    }
}

/// 相手の手の決め方。
#[derive(Debug, Clone, Copy)]
pub(super) struct OppOptions {
    pub min_games: u64,
    pub book_diff: i32,
    pub book_best: f64,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct WalkOptions {
    pub max_ply: usize,
    pub floor: f64,
    pub cont_max: usize,
    pub opp: OppOptions,
}

fn black_to_move(key: &str) -> bool {
    key.split(' ').nth(1) != Some("w")
}

fn advance(key: &str, mv: &str) -> Result<String> {
    let mut pos = position_from_sfen(&format!("{key} 1"))?;
    apply_usi_move(&mut pos, mv).with_context(|| format!("{key} で {mv} を指せません"))?;
    Ok(strip_ply(&pos.to_sfen()).to_owned())
}

/// 相手の手番の局面で、相手が指す手と重み (足して 1 未満なら残りは `Rare`)。手が分からなければ `None`。
/// 局数が `min_games` 以上の局面では、局数が `min_games` 以上の手を頻度で指す。局数の少ない手に行った分と、
/// 局数の足りない局面は、大きい定跡があればその手で指す (同じ手は重みを足す)。
fn opponent_moves(
    key: &str,
    freq: Option<&BookDb>,
    book: Option<&mut BookProbe>,
    opp: OppOptions,
) -> Result<Option<Vec<(String, f64)>>> {
    let mut out: Vec<(String, f64)> = Vec::new();
    let mut rest = 1.0;
    if let Some(entry) = freq.and_then(|f| f.entries.get(key)) {
        let total: u64 = entry.moves.iter().map(|m| m.count).sum();
        if total >= opp.min_games && total > 0 {
            for m in entry.moves.iter().filter(|m| m.count >= opp.min_games) {
                if let Some(mv) = &m.move_usi {
                    let share = m.count as f64 / total as f64;
                    out.push((mv.clone(), share));
                    rest -= share;
                }
            }
        }
    }
    let from_book = match book {
        Some(book) if rest > 1e-12 => book_moves(book.moves(key)?, opp),
        _ => Vec::new(),
    };
    if out.is_empty() && from_book.is_empty() {
        return Ok(None);
    }
    for (mv, share) in from_book {
        match out.iter_mut().find(|(m, _)| *m == mv) {
            Some(e) => e.1 += rest * share,
            None => out.push((mv, rest * share)),
        }
    }
    Ok(Some(out))
}

/// 大きい定跡で指す手と重み (足して 1)。最善 (同じ値の手は等分) に `book_best`、最善から `book_diff` 以内の
/// 残りの手に `1 - book_best` を等分する。差以内の手が最善だけなら最善に 1。
fn book_moves(moves: &[(String, i32)], opp: OppOptions) -> Vec<(String, f64)> {
    let Some(best) = moves.iter().map(|m| m.1).max() else {
        return Vec::new();
    };
    let top: Vec<&String> = moves.iter().filter(|m| m.1 == best).map(|m| &m.0).collect();
    let rest: Vec<&String> = moves
        .iter()
        .filter(|m| m.1 < best && m.1 >= best - opp.book_diff)
        .map(|m| &m.0)
        .collect();
    let top_share = if rest.is_empty() { 1.0 } else { opp.book_best };
    let mut out: Vec<(String, f64)> =
        top.iter().map(|m| ((*m).clone(), top_share / top.len() as f64)).collect();
    out.extend(rest.iter().map(|m| ((*m).clone(), (1.0 - top_share) / rest.len() as f64)));
    out
}

/// `key` から大きい定跡の最善を両者が指し続けた手順と、最後に引けた局面の最善の値 (先手から見た値)。
/// 局面が繰り返すか定跡に無くなるか `max` 手で止める。
fn continuation(key: &str, book: &mut BookProbe, max: usize) -> Result<(Vec<String>, Option<i32>)> {
    let mut key = key.to_owned();
    let mut seen = HashSet::new();
    let mut moves = Vec::new();
    let mut end = None;
    while moves.len() < max && seen.insert(key.clone()) {
        let Some((mv, value)) = book.best(&key)? else {
            break;
        };
        end = Some(if black_to_move(&key) { value } else { -value });
        key = advance(&key, &mv)?;
        moves.push(mv);
    }
    Ok((moves, end))
}

/// `side` を自分として初期局面 `start` から辿り、止まった局面を返す。同じ手数で同じ局面に来た重みは足し合わせる。
pub(super) fn walk_side(
    positions: &BTreeMap<String, MasterPosition>,
    freq: Option<&BookDb>,
    mut book: Option<&mut BookProbe>,
    side: &'static str,
    start: &str,
    options: WalkOptions,
) -> Result<Vec<Row>> {
    let games = |key: &str| {
        freq.map(|f| f.entries.get(key).map_or(0, |e| e.moves.iter().map(|m| m.count).sum()))
    };
    let mut rows = Vec::new();
    let mut layer: BTreeMap<String, (f64, Vec<String>)> =
        BTreeMap::from([(start.to_owned(), (1.0, Vec::new()))]);
    for ply in 0..=options.max_ply {
        let mut next: BTreeMap<String, (f64, Vec<String>)> = BTreeMap::new();
        let mut put = |key: String, weight: f64, path: &[String], mv: &str| {
            let entry = next.entry(key).or_insert_with(|| {
                let mut p = path.to_vec();
                p.push(mv.to_owned());
                (0.0, p)
            });
            entry.0 += weight;
        };
        for (key, (weight, path)) in layer {
            let mut stop = |stop: Stop, weight: f64, book: Option<&mut BookProbe>| -> Result<()> {
                let book_moves = match book {
                    Some(b) => Some(b.moves(&key)?.len()),
                    None => None,
                };
                rows.push(Row {
                    side,
                    stop,
                    ply,
                    weight,
                    key: key.clone(),
                    moves: path.clone(),
                    games: games(&key),
                    book_moves,
                    cont: Vec::new(),
                    cont_end_black: None,
                });
                Ok(())
            };
            if ply == options.max_ply {
                stop(Stop::Max, weight, book.as_deref_mut())?;
                continue;
            }
            if weight < options.floor {
                stop(Stop::Tiny, weight, book.as_deref_mut())?;
                continue;
            }
            let own = black_to_move(&key) == (side == "b");
            if own {
                match positions.get(&key) {
                    Some(position) => {
                        let mv = &position.moves[0].mv;
                        put(advance(&key, mv)?, weight, &path, mv);
                    }
                    None => {
                        stop(Stop::Own, weight, book.as_deref_mut())?;
                        if let Some(b) = book.as_deref_mut() {
                            let (cont, end) = continuation(&key, b, options.cont_max)?;
                            let row = rows.last_mut().expect("直前に足した行");
                            row.cont = cont;
                            row.cont_end_black = end;
                        }
                    }
                }
                continue;
            }
            let Some(moves) = opponent_moves(&key, freq, book.as_deref_mut(), options.opp)? else {
                stop(Stop::Out, weight, book.as_deref_mut())?;
                continue;
            };
            let mut used = 0.0;
            for (mv, share) in &moves {
                // 頻度の定跡の手は合法性を確かめていないので、指せない手はその分を Rare に回す。
                if let Ok(child) = advance(&key, mv) {
                    put(child, weight * share, &path, mv);
                    used += share;
                }
            }
            if used < 1.0 - 1e-9 {
                stop(Stop::Rare, weight * (1.0 - used), book.as_deref_mut())?;
            }
        }
        layer = next;
        if layer.is_empty() {
            break;
        }
    }
    Ok(rows)
}

pub(super) fn run(args: WalkArgs) -> Result<()> {
    run_from(args, HIRATE_KEY)
}

fn run_from(args: WalkArgs, start: &str) -> Result<()> {
    args.weight.check()?;
    ensure!(
        (0.0..=1.0).contains(&args.opp_book_best),
        "--opp-book-best は 0〜1 にしてください"
    );
    ensure!(args.opp_book_diff >= 0, "--opp-book-diff は 0 以上にしてください");
    ensure!(args.depth <= MAX_DEPTH, "--depth は {MAX_DEPTH} 以下にしてください");
    let mut inputs = vec![("--master", args.master.as_path())];
    if let Some(freq) = &args.weight.opp_freq {
        inputs.push(("--opp-freq", freq.as_path()));
    }
    if let Some(book) = &args.opp_book {
        inputs.push(("--opp-book", book.as_path()));
    }
    reject_same_file(&args.table, &inputs)?;
    if let Some(out) = &args.enqueue_out {
        reject_same_file(out, &inputs)?;
        ensure!(out != &args.table, "--enqueue-out と --table が同じです");
    }
    let (positions, _) = read_master(&args.master)?;
    let freq = args.weight.opp_freq.as_deref().map(BookDb::read).transpose()?;
    let book = args
        .opp_book
        .as_deref()
        .map(|path| {
            Book::open_on_the_fly(path, true)
                .with_context(|| format!("{} を開けません", path.display()))
        })
        .transpose()?;
    let mut probe = book.as_ref().map(|b| {
        BookProbe::new(move |sfen: &str| b.entry_moves(sfen).map_err(anyhow::Error::from))
    });
    let options = WalkOptions {
        max_ply: args.weight.max_ply,
        floor: args.weight.weight_floor,
        cont_max: args.cont_max,
        opp: OppOptions {
            min_games: args.weight.min_games,
            book_diff: args.opp_book_diff,
            book_best: args.opp_book_best,
        },
    };
    let mut rows = Vec::new();
    for &side in args.side.sides() {
        rows.extend(walk_side(&positions, freq.as_ref(), probe.as_mut(), side, start, options)?);
    }
    rows.sort_by(|a, b| {
        a.side
            .cmp(b.side)
            .then(b.weight.total_cmp(&a.weight))
            .then(a.key.cmp(&b.key))
            .then(a.ply.cmp(&b.ply))
    });
    write_atomic(&args.table, &format_table(&rows))?;
    let queued = match &args.enqueue_out {
        Some(out) => {
            let lines = queue_lines(&rows, &args)?;
            write_atomic(out, &format_queue(&lines)?)?;
            Some(lines.len())
        }
        None => None,
    };
    print!("{}", format_summary(&rows, args.weight.min_weight, queued));
    Ok(())
}

pub(super) fn format_table(rows: &[Row]) -> String {
    let mut text = String::from(
        "side\tstop\tply\tweight\tgames\tbook_moves\tcont_plies\tcont_end_black\tkey\tmoves\tcont\n",
    );
    let opt = |v: Option<String>| v.unwrap_or_default();
    for r in rows {
        writeln!(
            text,
            "{}\t{}\t{}\t{:.6}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            r.side,
            r.stop.as_str(),
            r.ply,
            r.weight,
            opt(r.games.map(|g| g.to_string())),
            opt(r.book_moves.map(|n| n.to_string())),
            if r.stop == Stop::Own && r.book_moves.is_some() {
                r.cont.len().to_string()
            } else {
                String::new()
            },
            opt(r.cont_end_black.map(|v| v.to_string())),
            r.key,
            r.moves.join(" "),
            r.cont.join(" "),
        )
        .expect("String への書き込み");
    }
    text
}

/// 重みが `--min-weight` 以上の `Own` の行を、重みの順にキューの行にする。同じ局面に別の手数で来た行は
/// 重みを足して 1 行にする (根の手数は最も重い行のもの)。id は局面で決まり、`--tag` が同じなら同じ局面は
/// 同じ id になる。
fn queue_lines(rows: &[Row], args: &WalkArgs) -> Result<Vec<Line>> {
    let mut merged: BTreeMap<(&str, &str), (f64, &Row)> = BTreeMap::new();
    for r in rows.iter().filter(|r| r.stop == Stop::Own) {
        let e = merged.entry((r.side, r.key.as_str())).or_insert((0.0, r));
        e.0 += r.weight;
        if r.weight > e.1.weight {
            e.1 = r;
        }
    }
    let mut picked: Vec<(f64, &Row)> =
        merged.into_values().filter(|(w, _)| *w >= args.weight.min_weight).collect();
    picked.sort_by(|a, b| {
        b.0.total_cmp(&a.0).then(a.1.side.cmp(b.1.side)).then(a.1.key.cmp(&b.1.key))
    });
    let mut lines = Vec::new();
    for (rank, (weight, r)) in picked.iter().enumerate() {
        let digest = format!("{:x}", Sha256::digest(&r.key));
        let game = if args.game_from_book && !r.cont.is_empty() {
            vec![r.cont.iter().take(args.depth + 1).cloned().collect()]
        } else {
            Vec::new()
        };
        let line = Line {
            id: format!("{}_{}_{}", args.tag, &digest[..16], r.side),
            root: format!("sfen {} {}", r.key, r.ply + 1),
            roots: Vec::new(),
            own: r.side.to_owned(),
            depth: args.depth,
            priority: args.priority + rank.min(99) as u32,
            hold: false,
            game,
            opts: Opts::default(),
            memo: Some(format!("walk weight={weight:.5} moves={}", r.moves.join(" "))),
        };
        line.check().with_context(|| format!("キューの行の検査: {}", r.key))?;
        lines.push(line);
    }
    Ok(lines)
}

fn format_summary(rows: &[Row], min_weight: f64, queued: Option<usize>) -> String {
    let mut text = String::new();
    for side in ["b", "w"] {
        let rows: Vec<&Row> = rows.iter().filter(|r| r.side == side).collect();
        if rows.is_empty() {
            continue;
        }
        let total =
            |stop: Stop| rows.iter().filter(|r| r.stop == stop).map(|r| r.weight).sum::<f64>();
        let parts: Vec<String> = Stop::ALL
            .iter()
            .filter(|&&s| total(s) > 0.0)
            .map(|&s| format!("{} {:.4}", s.as_str(), total(s)))
            .collect();
        writeln!(text, "{side}: 止まった重み {}", parts.join(" / ")).expect("String への書き込み");
        let mut bands: BTreeMap<usize, f64> = BTreeMap::new();
        for r in rows.iter().filter(|r| r.stop == Stop::Own) {
            *bands.entry(r.ply / 10 * 10).or_default() += r.weight;
        }
        let bands: Vec<String> =
            bands.iter().map(|(b, w)| format!("{b}-{}: {w:.4}", b + 9)).collect();
        writeln!(text, "{side}: 読み始める重み (手数の帯) {}", bands.join(" / "))
            .expect("String への書き込み");
        let heavy = rows.iter().filter(|r| r.stop == Stop::Own && r.weight >= min_weight).count();
        writeln!(
            text,
            "{side}: 読み始める局面 {} (重み {min_weight} 以上 {heavy})",
            rows.iter().filter(|r| r.stop == Stop::Own).count()
        )
        .expect("String への書き込み");
    }
    if let Some(n) = queued {
        writeln!(text, "キューの行 {n}").expect("String への書き込み");
    }
    text
}

#[cfg(test)]
mod tests;
