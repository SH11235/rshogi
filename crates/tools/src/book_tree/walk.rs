//! 自分は定跡の main を指し、相手は強い対局の手の頻度か大きい定跡の手で指すとして初期局面から辿り、
//! 自分の定跡が切れて自分が読み始める局面とその重みを出す。木の根を選ぶのと、相手の定跡がその先どこまで
//! 続くかを見るのに使う。

use super::audit::reject_same_file;
use super::master_table::{MasterPosition, read_master};
use super::refutation::format_queue;
use super::reply_graph::{HIRATE_KEY, SideArg};
use super::targets::{Line, MAX_DEPTH, Opts, parse_root, read_checked_lines};
use crate::book_db::{apply_usi_move, position_from_sfen, strip_ply};
use crate::common::io::write_atomic;
use anyhow::{Context, Result, ensure};
use clap::Args;
use rshogi_book::{Book, BookEntryMove};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// `--tag` の長さの上限。id `<tag>_<手数>_<hash 16 桁>_<b|w>` が 64 文字に収まる長さ。
const MAX_TAG_LEN: usize = 40;

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
    /// 相手の手の頻度の定跡 (`book_from_csa` のテキスト .db。手の count を局数として使う)。
    /// sfen 行が手数を除いた SFEN の順に並んでいること (ファイルから引き、丸読みしない)
    #[arg(long)]
    opp_freq: Option<PathBuf>,
    /// `--opp-freq` で、局数がこれ未満の局面は頻度で指さず、局数がこれ未満の手は頻度で指さない
    #[arg(long, default_value_t = 3)]
    min_games: u64,
    /// 初期局面からこの手数で辿るのを止める (max)
    #[arg(long, default_value_t = 80)]
    max_ply: usize,
    /// 重みがこれ未満の局面で辿るのを止める (tiny)
    #[arg(long, default_value_t = 1e-5)]
    weight_floor: f64,
    /// 要約で数える局面と、キューの行に書く局面の重みの下限 (同じ局面に別の手数で来た重みは足す)
    #[arg(long, default_value_t = 0.0)]
    min_weight: f64,
    /// 相手の手の大きい定跡 (.db か YaneuraOu の .ybb)。`--opp-freq` の局数が足りない局面ではこの定跡の手で指す
    #[arg(long)]
    opp_book: Option<PathBuf>,
    /// 大きい定跡で指すとき、最善の値からこの差以内の手を指す
    #[arg(long, default_value_t = 30)]
    opp_book_diff: i32,
    /// 大きい定跡で指すとき、最善 (同じ値の手は等分) に割り当てる重み。残りを差以内の他の手で等分する
    #[arg(long, default_value_t = 0.9)]
    opp_book_best: f64,
    /// 相手の手番では大きい定跡を先に引き、局面があればその手だけで指す。無い局面だけ `--opp-freq` の頻度で指す
    /// (相手が自分の定跡を持つときの辿り方。定跡を外れた後に手順の入れ替わりで定跡に戻る線も辿る)
    #[arg(long, requires = "opp_book")]
    opp_book_first: bool,
    /// 自分が読み始める局面から、大きい定跡の最善を両者が指し続けた手順をこの手数まで辿る
    #[arg(long, default_value_t = 100)]
    cont_max: usize,
    /// 自分が読み始める局面のうち、重みが `--min-weight` 以上のものを木のキューの行 (JSON Lines) に書く
    #[arg(long)]
    enqueue_out: Option<PathBuf>,
    /// キューに書く根の手数 (SFEN) の上限
    #[arg(long, requires = "enqueue_out")]
    max_root_ply: Option<usize>,
    /// 既に積んだ木のキュー (JSON Lines)。繰り返して複数
    #[arg(long, requires = "enqueue_out")]
    exclude_queue: Vec<PathBuf>,
    /// キューの行の相手の手の数 (1〜8)
    #[arg(long, requires = "enqueue_out")]
    opp_n: Option<usize>,
    /// キューの行の打ち切り値 (1〜30000)
    #[arg(long, requires = "enqueue_out")]
    stop: Option<i32>,
    /// キューの行の読み直し回数 (0〜10)
    #[arg(long, requires = "enqueue_out")]
    rounds: Option<usize>,
    /// 先手番で循環に入る手の値 (-30000〜30000)
    #[arg(long, requires = "enqueue_out", allow_hyphen_values = true)]
    draw_black: Option<i32>,
    /// 後手番で循環に入る手の値 (-30000〜30000)
    #[arg(long, requires = "enqueue_out", allow_hyphen_values = true)]
    draw_white: Option<i32>,
    /// キューの行の段数
    #[arg(long, default_value_t = 6)]
    depth: usize,
    /// キューの行の priority の始まり (重みの順位を足す)
    #[arg(long, default_value_t = 300)]
    priority: u32,
    /// キューの行の id `<tag>_<根の手数>_<局面の hash>_<b|w>` に入れる名前 ([A-Za-z0-9_-] の 1〜40 文字)
    #[arg(long, default_value = "walk")]
    tag: String,
    /// キューの行の `game` に、大きい定跡の最善を両者が指し続けた手順を入れる (木がその手を必ず読む)
    #[arg(long, requires = "opp_book")]
    game_from_book: bool,
    /// 初期局面からこの手順 (USI を空白区切り) を指した局面から辿り始める。手は元データに無くてもよい
    #[arg(long, value_name = "USI")]
    start_moves: Option<String>,
    /// 強い対局の棋譜 (1 局 1 行の JSON Lines、`.gz` も可)。止まりの局面とその手前に着いた局数と勝敗の列を足す
    #[arg(long, value_name = "PATH")]
    opp_records: Option<PathBuf>,
    /// `--opp-records` で数える対局の、両者のレートの下限
    #[arg(long, default_value_t = 3800.0, requires = "opp_records")]
    records_min_rate: f64,
    /// `--opp-records` で、1 局を初期局面からこの手数まで見る
    #[arg(long, default_value_t = 130, requires = "opp_records")]
    records_max_ply: usize,
}

impl WalkArgs {
    fn queue_opts(&self) -> Opts {
        Opts {
            opp_n: self.opp_n,
            stop: self.stop,
            rounds: self.rounds,
            draw_black: self.draw_black,
            draw_white: self.draw_white,
            ..Opts::default()
        }
    }
}

/// 止まり方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Stop {
    /// 自分の手番で元データに行が無い (自分が読み始める)。
    Own,
    /// 相手の手番で、局数が足りず大きい定跡にも無い (相手の手が分からない)。
    Out,
    /// 相手の手番で、局数が `--min-games` 未満の手か指せない手に行った分。
    Rare,
    /// 重みが `--weight-floor` 未満。
    Tiny,
    /// `--max-ply` に達した。
    Max,
}

impl Stop {
    pub(super) const ALL: [Stop; 5] = [Stop::Own, Stop::Out, Stop::Rare, Stop::Tiny, Stop::Max];

    pub(super) fn as_str(self) -> &'static str {
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
    /// `walk_side_with` の `Tagger` の印 (印を付けない辿りでは 0)。
    pub tag: u32,
    /// 初期局面からの手順 (同じ手数で同じ局面に来た手順のうち最初に辿ったもの)。
    pub moves: Vec<String>,
    /// `--opp-freq` のこの局面の局数。
    pub games: Option<u64>,
    /// 大きい定跡のこの局面の手の数。
    pub book_moves: Option<usize>,
    /// 大きい定跡の最善を両者が指し続けた手順 (`Own` だけ)。
    pub cont: Vec<String>,
    /// その手順で最後に定跡を引けた局面の最善の値 (先手から見た値)。
    pub cont_end_black: Option<i64>,
}

/// ply 付き SFEN で定跡の手を引く関数。
type Lookup<'a> = Box<dyn FnMut(&str) -> Result<Vec<BookEntryMove>> + 'a>;

/// 定跡の引き方。同じ局面は 1 回だけ引き、引いた局面だけを持つ。
pub(super) struct Probe<'a> {
    lookup: Lookup<'a>,
    cache: HashMap<String, Vec<BookEntryMove>>,
}

impl<'a> Probe<'a> {
    pub(super) fn new(lookup: impl FnMut(&str) -> Result<Vec<BookEntryMove>> + 'a) -> Self {
        Self {
            lookup: Box::new(lookup),
            cache: HashMap::new(),
        }
    }

    /// 定跡ファイルの順の手。無ければ空。
    pub(super) fn moves(&mut self, key: &str) -> Result<&[BookEntryMove]> {
        if !self.cache.contains_key(key) {
            let moves = (self.lookup)(&format!("{key} 1"))?;
            self.cache.insert(key.to_owned(), moves);
        }
        Ok(&self.cache[key])
    }

    /// 最善の手と手番の側から見た値 (同じ値なら定跡ファイルで先の手)。
    fn best(&mut self, key: &str) -> Result<Option<(String, i32)>> {
        let moves = self.moves(key)?;
        Ok(moves
            .iter()
            .fold(None::<&BookEntryMove>, |b, m| match b {
                Some(b) if b.value >= m.value => Some(b),
                _ => Some(m),
            })
            .map(|m| (m.move_usi.clone(), m.value)))
    }

    pub(super) fn games(&mut self, key: &str) -> Result<u64> {
        Ok(self.moves(key)?.iter().map(|m| m.move_count).sum())
    }
}

/// 相手の手の出どころ。
#[derive(Default)]
pub(super) struct Sources<'a> {
    pub freq: Option<Probe<'a>>,
    pub book: Option<Probe<'a>>,
}

/// 相手の手の決め方。
#[derive(Debug, Clone, Copy)]
pub(super) struct OppOptions {
    pub min_games: u64,
    pub book_diff: i32,
    pub book_best: f64,
    pub book_first: bool,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct WalkOptions {
    pub max_ply: usize,
    pub floor: f64,
    pub cont_max: usize,
    pub opp: OppOptions,
}

pub(super) fn black_to_move(key: &str) -> bool {
    key.split(' ').nth(1) != Some("w")
}

pub(super) fn advance(key: &str, mv: &str) -> Result<String> {
    let mut pos = position_from_sfen(&format!("{key} 1"))?;
    apply_usi_move(&mut pos, mv).with_context(|| format!("{key} で {mv} を指せません"))?;
    Ok(strip_ply(&pos.to_sfen()).to_owned())
}

/// 相手の手番の局面で、相手が指す手と重み (重みは正、足して 1 未満なら残りは `Rare`)。局数が足りず大きい定跡にも
/// 無く、手が分からなければ `None`。局数が `min_games` 以上の局面では、局数が `min_games` 以上の手を頻度で指す。
/// 局数の少ない手に行った分と、局数の足りない局面は、大きい定跡があればその手で指す (同じ手は重みを足す)。
/// `book_first` なら、大きい定跡に手のある局面はその手だけで指し、頻度は定跡に無い局面だけで使う。
pub(super) fn opponent_moves(
    key: &str,
    src: &mut Sources,
    opp: OppOptions,
) -> Result<Option<Vec<(String, f64)>>> {
    if opp.book_first
        && let Some(book) = src.book.as_mut()
    {
        let mut out = book_moves(book.moves(key)?, opp);
        out.retain(|(_, share)| *share > 0.0);
        if !out.is_empty() {
            return Ok(Some(out));
        }
    }
    let mut out: Vec<(String, f64)> = Vec::new();
    let mut rest = 1.0;
    let mut enough_games = false;
    if let Some(freq) = src.freq.as_mut() {
        let moves = freq.moves(key)?;
        let total: u64 = moves.iter().map(|m| m.move_count).sum();
        enough_games = total >= opp.min_games && total > 0;
        if enough_games {
            for m in moves.iter().filter(|m| m.move_count >= opp.min_games) {
                let share = m.move_count as f64 / total as f64;
                out.push((m.move_usi.clone(), share));
                rest -= share;
            }
        }
    }
    let from_book = match src.book.as_mut() {
        Some(book) if rest > 1e-12 => book_moves(book.moves(key)?, opp),
        _ => Vec::new(),
    };
    for (mv, share) in from_book {
        match out.iter_mut().find(|(m, _)| *m == mv) {
            Some(e) => e.1 += rest * share,
            None => out.push((mv, rest * share)),
        }
    }
    // `--opp-book-best` が 0 か 1 だと重み 0 の手ができる。辿ると到達しない局面を読み始める局面として出してしまう。
    out.retain(|(_, share)| *share > 0.0);
    Ok((enough_games || !out.is_empty()).then_some(out))
}

/// 大きい定跡で指す手と重み (足して 1)。最善 (同じ値の手は等分) に `book_best`、最善から `book_diff` 以内の
/// 残りの手に `1 - book_best` を等分する。差以内の手が最善だけなら最善に 1。
pub(super) fn book_moves(moves: &[BookEntryMove], opp: OppOptions) -> Vec<(String, f64)> {
    let Some(best) = moves.iter().map(|m| m.value).max() else {
        return Vec::new();
    };
    let top: Vec<&String> = moves.iter().filter(|m| m.value == best).map(|m| &m.move_usi).collect();
    let rest: Vec<&String> = moves
        .iter()
        .filter(|m| m.value < best && m.value >= best.saturating_sub(opp.book_diff))
        .map(|m| &m.move_usi)
        .collect();
    let top_share = if rest.is_empty() { 1.0 } else { opp.book_best };
    let mut out: Vec<(String, f64)> =
        top.iter().map(|m| ((*m).clone(), top_share / top.len() as f64)).collect();
    out.extend(rest.iter().map(|m| ((*m).clone(), (1.0 - top_share) / rest.len() as f64)));
    out
}

/// `key` から大きい定跡の最善を両者が指し続けた手順と、最後に引けた局面の最善の値 (先手から見た値)。
/// 局面が繰り返すか、定跡に無くなるか、定跡の手が指せないか、`max` 手で止める。
fn continuation(key: &str, book: &mut Probe, max: usize) -> Result<(Vec<String>, Option<i64>)> {
    let mut key = key.to_owned();
    let mut seen = HashSet::new();
    let mut moves = Vec::new();
    let mut end = None;
    while moves.len() < max && seen.insert(key.clone()) {
        let Some((mv, value)) = book.best(&key)? else {
            break;
        };
        // i32::MIN の符号を反転できるよう広げる。
        let value = i64::from(value);
        end = Some(if black_to_move(&key) { value } else { -value });
        // 大きい定跡の手は合法性を確かめていない。
        let Ok(next) = advance(&key, &mv) else {
            break;
        };
        key = next;
        moves.push(mv);
    }
    Ok((moves, end))
}

/// 止まった局面 `at` (局面と印) の行。
fn stop_row(
    src: &mut Sources,
    side: &'static str,
    stop: Stop,
    ply: usize,
    weight: f64,
    (key, tag): (&str, u32),
    path: &[String],
) -> Result<Row> {
    let games = src.freq.as_mut().map(|f| f.games(key)).transpose()?;
    let book_moves = src.book.as_mut().map(|b| b.moves(key).map(<[_]>::len)).transpose()?;
    Ok(Row {
        side,
        stop,
        ply,
        weight,
        key: key.to_owned(),
        tag,
        moves: path.to_vec(),
        games,
        book_moves,
        cont: Vec::new(),
        cont_end_black: None,
    })
}

/// 辿りの重みを、通ってきた局面で分けて持つための印。印の違う重みは同じ局面・同じ手数でも足し合わせない。
pub(super) trait Tagger {
    /// 辿り始める局面の印。
    fn start(&mut self, key: &str) -> Result<u32>;
    /// 印 `tag` の重みが `parent` から `mv` を指して `child` に進んだ後の印。
    fn step(&mut self, tag: u32, parent: &str, mv: &str, child: &str) -> Result<u32>;
    /// 重みが `--weight-floor` 未満に分かれた印を、同じ局面のほかの重みと足し合わせやすい粗い印に替える。
    fn coarsen(&mut self, tag: u32) -> u32;
}

/// 印を付けない (どの重みも印 0)。
struct NoTag;

impl Tagger for NoTag {
    fn start(&mut self, _: &str) -> Result<u32> {
        Ok(0)
    }
    fn step(&mut self, tag: u32, _: &str, _: &str, _: &str) -> Result<u32> {
        Ok(tag)
    }
    fn coarsen(&mut self, tag: u32) -> u32 {
        tag
    }
}

/// `side` を自分として、初期局面から `start_moves` を指した局面 `start` から辿り、止まった局面を返す。
/// 同じ手数で同じ局面に来た重みは足し合わせる。
fn walk_side(
    positions: &BTreeMap<String, MasterPosition>,
    src: &mut Sources,
    side: &'static str,
    start: &str,
    start_moves: &[String],
    options: WalkOptions,
) -> Result<Vec<Row>> {
    let from = WalkFrom {
        start,
        start_moves,
        overrides: &HashMap::new(),
    };
    walk_side_with(positions, src, side, &from, &mut NoTag, options)
}

/// 辿り始める局面と、自分の手番で main の代わりに指す手。
pub(super) struct WalkFrom<'a> {
    pub start: &'a str,
    pub start_moves: &'a [String],
    /// 手数を除いた SFEN → 指す手 (合法であること)。元データに無い局面でも指して辿りを続ける。
    pub overrides: &'a HashMap<String, String>,
}

/// `walk_side` と同じく辿り、自分の手番で `from.overrides` にある局面では main の代わりにその手を指す。
/// 重みは (局面, `tagger` の印) ごとに足し合わせ、行に印を付ける。
pub(super) fn walk_side_with(
    positions: &BTreeMap<String, MasterPosition>,
    src: &mut Sources,
    side: &'static str,
    from: &WalkFrom,
    tagger: &mut dyn Tagger,
    options: WalkOptions,
) -> Result<Vec<Row>> {
    walk_using(
        &mut |key| Ok(positions.get(key).map(|position| position.moves[0].mv.clone())),
        src,
        side,
        from,
        tagger,
        options,
    )
}

fn walk_using(
    own_move: &mut dyn FnMut(&str) -> Result<Option<String>>,
    src: &mut Sources,
    side: &'static str,
    from: &WalkFrom,
    tagger: &mut dyn Tagger,
    options: WalkOptions,
) -> Result<Vec<Row>> {
    let mut rows = Vec::new();
    type Layer = BTreeMap<(String, u32), (f64, Vec<String>)>;
    let mut layer: Layer = BTreeMap::from([(
        (from.start.to_owned(), tagger.start(from.start)?),
        (1.0, from.start_moves.to_vec()),
    )]);
    for ply in from.start_moves.len()..=options.max_ply {
        // 重みの下限は印をまたいで同じ局面に来た重みの和で見る (印で分けても止まる所を変えない)。
        // 和が下限以上の局面で下限未満に分かれた重みは、粗い印に替えて足し合わせ、分かれ方が増え続けないようにする。
        let mut total: HashMap<String, f64> = HashMap::new();
        for ((key, _), (weight, _)) in &layer {
            *total.entry(key.clone()).or_default() += weight;
        }
        let small: Vec<(String, u32)> = layer
            .iter()
            .filter(|((key, _), (weight, _))| {
                *weight < options.floor && total[key] >= options.floor
            })
            .map(|(at, _)| at.clone())
            .collect();
        for (key, tag) in small {
            let (weight, path) = layer.remove(&(key.clone(), tag)).expect("層にある");
            let coarse = tagger.coarsen(tag);
            layer.entry((key, coarse)).or_insert((0.0, path)).0 += weight;
        }
        let mut next: Layer = BTreeMap::new();
        let mut put = |key: (String, u32), weight: f64, path: &[String], mv: &str| {
            let entry = next.entry(key).or_insert_with(|| {
                let mut p = path.to_vec();
                p.push(mv.to_owned());
                (0.0, p)
            });
            entry.0 += weight;
        };
        for ((key, tag), (weight, path)) in layer {
            let stop = |src: &mut Sources, stop: Stop, weight: f64| {
                stop_row(src, side, stop, ply, weight, (&key, tag), &path)
            };
            if ply == options.max_ply {
                rows.push(stop(src, Stop::Max, weight)?);
                continue;
            }
            if total[&key] < options.floor {
                rows.push(stop(src, Stop::Tiny, weight)?);
                continue;
            }
            let own = black_to_move(&key) == (side == "b");
            if own {
                let mv = match from.overrides.get(&key) {
                    Some(mv) => mv.clone(),
                    None => match own_move(&key)? {
                        Some(mv) => mv,
                        None => {
                            let mut row = stop(src, Stop::Own, weight)?;
                            if let Some(book) = src.book.as_mut() {
                                (row.cont, row.cont_end_black) =
                                    continuation(&key, book, options.cont_max)?;
                            }
                            rows.push(row);
                            continue;
                        }
                    },
                };
                let child = advance(&key, &mv)?;
                let child_tag = tagger.step(tag, &key, &mv, &child)?;
                put((child, child_tag), weight, &path, &mv);
                continue;
            }
            let Some(moves) = opponent_moves(&key, src, options.opp)? else {
                rows.push(stop(src, Stop::Out, weight)?);
                continue;
            };
            let mut used = 0.0;
            for (mv, share) in &moves {
                // 頻度の定跡の手は合法性を確かめていないので、指せない手はその分を Rare に回す。
                if let Ok(child) = advance(&key, mv) {
                    let child_tag = tagger.step(tag, &key, mv, &child)?;
                    put((child, child_tag), weight * share, &path, mv);
                    used += share;
                }
            }
            if used < 1.0 - 1e-9 {
                rows.push(stop(src, Stop::Rare, weight * (1.0 - used))?);
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

fn check_args(args: &WalkArgs) -> Result<()> {
    ensure!(
        args.weight_floor >= 0.0 && args.min_weight >= 0.0,
        "--weight-floor と --min-weight は 0 以上にしてください"
    );
    ensure!(
        (0.0..=1.0).contains(&args.opp_book_best),
        "--opp-book-best は 0〜1 にしてください"
    );
    ensure!(args.opp_book_diff >= 0, "--opp-book-diff は 0 以上にしてください");
    ensure!(args.depth <= MAX_DEPTH, "--depth は {MAX_DEPTH} 以下にしてください");
    check_tag(&args.tag)?;
    args.queue_opts().check()?;
    let mut inputs = vec![("--master", args.master.as_path())];
    if let Some(freq) = &args.opp_freq {
        inputs.push(("--opp-freq", freq.as_path()));
    }
    if let Some(book) = &args.opp_book {
        inputs.push(("--opp-book", book.as_path()));
    }
    for path in &args.exclude_queue {
        inputs.push(("--exclude-queue", path.as_path()));
    }
    if let Some(records) = &args.opp_records {
        inputs.push(("--opp-records", records.as_path()));
    }
    ensure!(args.records_min_rate.is_finite(), "--records-min-rate は有限の数にしてください");
    reject_same_file(&args.table, &inputs)?;
    if let Some(out) = &args.enqueue_out {
        inputs.push(("--table", args.table.as_path()));
        reject_same_file(out, &inputs)?;
    }
    Ok(())
}

pub(super) fn open_book(path: &Path) -> Result<Book> {
    Book::open_on_the_fly(path, true).with_context(|| format!("{} を開けません", path.display()))
}

/// `--opp-freq` の定跡を開く。
pub(super) fn open_freq(path: &Path) -> Result<Book> {
    let book = open_book(path)?;
    // on the fly で局面数を返すのは .ybb だけ。.ybb は採択回数を持たないので局数にならない。
    ensure!(
        book.position_count().is_none(),
        "--opp-freq は採択回数を持つテキストの .db にしてください: {}",
        path.display()
    );
    Ok(book)
}

pub(super) fn book_probe(book: &Book) -> Probe<'_> {
    Probe::new(move |sfen: &str| Ok(book.entry_moves(sfen)?))
}

/// 初期局面 `initial` から `--start-moves` を指した局面と、その手順。
fn start_position(initial: &str, start_moves: Option<&str>) -> Result<(String, Vec<String>)> {
    let moves: Vec<String> =
        start_moves.unwrap_or_default().split_whitespace().map(str::to_owned).collect();
    let mut key = initial.to_owned();
    for (i, mv) in moves.iter().enumerate() {
        key = advance(&key, mv).with_context(|| format!("--start-moves の {} 手目", i + 1))?;
    }
    Ok((key, moves))
}

fn run_from(args: WalkArgs, initial: &str) -> Result<()> {
    check_args(&args)?;
    let excluded = excluded_positions(&args.exclude_queue)?;
    let (start, start_moves) = start_position(initial, args.start_moves.as_deref())?;
    ensure!(
        start_moves.len() <= args.max_ply,
        "--start-moves の手数 {} が --max-ply {} を超えています",
        start_moves.len(),
        args.max_ply
    );
    let (positions, _) = read_master(&args.master)?;
    let freq = args.opp_freq.as_deref().map(open_freq).transpose()?;
    let book = args.opp_book.as_deref().map(open_book).transpose()?;
    let mut src = Sources {
        freq: freq.as_ref().map(book_probe),
        book: book.as_ref().map(book_probe),
    };
    let options = WalkOptions {
        max_ply: args.max_ply,
        floor: args.weight_floor,
        cont_max: args.cont_max,
        opp: OppOptions {
            min_games: args.min_games,
            book_diff: args.opp_book_diff,
            book_best: args.opp_book_best,
            book_first: args.opp_book_first,
        },
    };
    let mut rows = Vec::new();
    for &side in args.side.sides() {
        rows.extend(walk_side(&positions, &mut src, side, &start, &start_moves, options)?);
    }
    rows.sort_by(|a, b| {
        a.side
            .cmp(b.side)
            .then(b.weight.total_cmp(&a.weight))
            .then(a.key.cmp(&b.key))
            .then(a.ply.cmp(&b.ply))
    });
    let mut index =
        super::master_index::MasterIndex::open(&args.master, &super::schema::Schema::load(None)?)?;
    let records = args
        .opp_records
        .as_deref()
        .map(|path| {
            let options = records::RecordOptions {
                min_rate: args.records_min_rate,
                max_ply: args.records_max_ply,
            };
            records::reach_stats(&rows, initial, path, &options)
        })
        .transpose()?;
    write_atomic(
        &args.table,
        &format_table_with_entries(&rows, &mut index, initial, records.as_deref())?,
    )?;
    let queued = match &args.enqueue_out {
        Some(out) => {
            let (lines, counts) = queue_lines(&rows, &args, &excluded)?;
            write_atomic(out, &format_queue(&lines)?)?;
            if !args.exclude_queue.is_empty() {
                print!("{}", counts.format());
            }
            Some(lines.len())
        }
        None => None,
    };
    print!("{}", format_summary(&rows, args.min_weight, queued));
    Ok(())
}

fn format_table_with_entries(
    rows: &[Row],
    index: &mut super::master_index::MasterIndex,
    initial: &str,
    records: Option<&[[records::Reach; 3]]>,
) -> Result<String> {
    let mut text = String::from(
        "side\tstop\tply\tweight\tgames\tbook_moves\tcont_plies\tcont_end_black\tkey\tmoves\tcont\tentry_move\tentry_value_black\tentry_value_kind\tentry_source",
    );
    if records.is_some() {
        text.push_str("\tn0\tscore0\tn10\tscore10\tn20\tscore20\tmedian_len_after\tblack_wins0\twhite_wins0\tdraws0");
    }
    text.push('\n');
    let opt = |v: Option<String>| v.unwrap_or_default();
    for (i, r) in rows.iter().enumerate() {
        let entry_move = r.moves.last().map(String::as_str).unwrap_or("");
        let mut value = String::new();
        let mut kind = String::new();
        let mut source = String::new();
        if !r.moves.is_empty() {
            let mut key = initial.to_owned();
            for mv in &r.moves[..r.moves.len() - 1] {
                key = advance(&key, mv)?;
            }
            if let Some(row) = index.rows(&key)?.iter().find(|row| row.get("move") == entry_move) {
                if let Some(first) = row.get("evals").split(',').next().filter(|s| !s.is_empty()) {
                    let v = i128::from(first.parse::<i64>()?);
                    value = if black_to_move(&key) { v } else { -v }.to_string();
                }
                kind = row.get("value_kind").to_owned();
                source = row.get("source").to_owned();
            }
        }
        writeln!(
            text,
            "{}\t{}\t{}\t{:.6}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
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
            entry_move,
            value,
            kind,
            source,
        )
        .expect("String への書き込み");
        if let Some(records) = records {
            text.pop();
            let [at, ten, twenty] = &records[i];
            writeln!(
                text,
                "\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                at.games,
                at.score_text(),
                ten.games,
                ten.score_text(),
                twenty.games,
                twenty.score_text(),
                at.median_after(),
                at.black_wins,
                at.white_wins,
                at.draws
            )
            .expect("String への書き込み");
        }
    }
    Ok(text)
}

/// `Own` の行を (側, 局面) ごとにまとめた重みと代表の行。代表は手数の最も小さい行 (同じ手数なら `rows` で先の行)。
fn merged_own(rows: &[Row]) -> BTreeMap<(&'static str, &str), (f64, &Row)> {
    let mut merged: BTreeMap<(&'static str, &str), (f64, &Row)> = BTreeMap::new();
    for r in rows.iter().filter(|r| r.stop == Stop::Own) {
        let e = merged.entry((r.side, r.key.as_str())).or_insert((0.0, r));
        e.0 += r.weight;
        if r.ply < e.1.ply {
            e.1 = r;
        }
    }
    merged
}

pub(super) type Excluded = BTreeMap<(String, String), BTreeSet<String>>;

pub(super) fn excluded_positions(paths: &[PathBuf]) -> Result<Excluded> {
    let mut latest: BTreeMap<String, (Line, super::targets::Root)> = BTreeMap::new();
    for path in paths {
        read_checked_lines(path, |mut line, root, _| {
            if let Some((first, first_root)) = latest.get(&line.id) {
                // serve は id ごとに最初の行の根 (局面と手数) と own を固定し、替えた行を拒否する。
                if first_root.key != root.key || first_root.ply != root.ply || first.own != line.own
                {
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
    let mut excluded = Excluded::new();
    for (id, (line, root)) in latest {
        if line.depth == 0 {
            continue;
        }
        let sides = line.sides();
        let mut add = |key: &str, own: &str| {
            excluded.entry((key.to_owned(), own.to_owned())).or_default().insert(id.clone());
        };
        for own in &sides {
            add(&root.key, own);
            for more in &line.roots {
                add(&parse_root(more)?.key, own);
            }
        }
        for moves in &line.game {
            let mut key = root.key.clone();
            for mv in moves.iter().take(line.depth + 1) {
                key = super::model::child(&key, mv)?;
                for own in &sides {
                    if super::model::side(&key) == own {
                        add(&key, own);
                    }
                }
            }
        }
    }
    Ok(excluded)
}

#[derive(Default, Debug)]
struct ExclusionCounts {
    sides: BTreeMap<String, usize>,
    ids: BTreeMap<String, usize>,
}

impl ExclusionCounts {
    fn format(&self) -> String {
        let mut text = String::new();
        for side in ["b", "w"] {
            writeln!(
                text,
                "{side}: 既存の木で除いたキューの行 {}",
                self.sides.get(side).unwrap_or(&0)
            )
            .expect("String への書き込み");
        }
        for (id, count) in &self.ids {
            writeln!(text, "既存の id={id}: 除いたキューの行 {count}")
                .expect("String への書き込み");
        }
        text
    }
}

/// 重みが `--min-weight` 以上の読み始める局面を、重みの順にキューの行にする。同じ局面に別の手数で来た行は
/// 重みを足して 1 行にし、根の手数は最も小さい手数にする。キューは同じ id の根の手数が変わる行を受け付けないので、
/// id に根の手数を入れ、重みが入れ替わっても同じ局面・同じ手数は同じ id になるようにする。
fn queue_lines(
    rows: &[Row],
    args: &WalkArgs,
    excluded: &Excluded,
) -> Result<(Vec<Line>, ExclusionCounts)> {
    let mut counts = ExclusionCounts::default();
    let mut picked: Vec<(f64, &Row)> =
        merged_own(rows).into_values().filter(|(w, _)| *w >= args.min_weight).collect();
    picked.retain(|(_, r)| {
        if args.max_root_ply.is_some_and(|max| r.ply + 1 > max) {
            return false;
        }
        if let Some(ids) = excluded.get(&(r.key.clone(), r.side.to_owned())) {
            *counts.sides.entry(r.side.to_owned()).or_default() += 1;
            for id in ids {
                *counts.ids.entry(id.clone()).or_default() += 1;
            }
            return false;
        }
        true
    });
    picked.sort_by(|a, b| {
        b.0.total_cmp(&a.0).then(a.1.side.cmp(b.1.side)).then(a.1.key.cmp(&b.1.key))
    });
    let mut lines = Vec::new();
    for (rank, (weight, r)) in picked.iter().enumerate() {
        let root_ply = r.ply + 1;
        let game = if args.game_from_book && !r.cont.is_empty() {
            vec![r.cont.iter().take(args.depth + 1).cloned().collect()]
        } else {
            Vec::new()
        };
        let line = Line {
            group: None,
            id: queue_id(&args.tag, root_ply, &r.key, r.side),
            root: format!("sfen {} {root_ply}", r.key),
            roots: Vec::new(),
            own: r.side.to_owned(),
            depth: args.depth,
            priority: args.priority + rank.min(99) as u32,
            hold: false,
            game,
            opts: args.queue_opts(),
            memo: Some(format!("walk weight={weight:.5} moves={}", r.moves.join(" "))),
        };
        line.check().with_context(|| format!("キューの行の検査: {}", r.key))?;
        lines.push(line);
    }
    Ok((lines, counts))
}

fn format_summary(rows: &[Row], min_weight: f64, queued: Option<usize>) -> String {
    let mut text = String::new();
    let merged = merged_own(rows);
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
        let own: Vec<f64> =
            merged.iter().filter(|((s, _), _)| *s == side).map(|(_, (w, _))| *w).collect();
        let heavy = own.iter().filter(|&&w| w >= min_weight).count();
        writeln!(text, "{side}: 読み始める局面 {} (重み {min_weight} 以上 {heavy})", own.len())
            .expect("String への書き込み");
    }
    if let Some(n) = queued {
        writeln!(text, "キューの行 {n}").expect("String への書き込み");
    }
    text
}

pub(super) mod opp_book_diff;
pub(super) mod records;
#[cfg(test)]
mod tests;

/// キューの id に使う tag を検査する。
pub(super) fn check_tag(tag: &str) -> Result<()> {
    ensure!(
        !tag.is_empty()
            && tag.len() <= MAX_TAG_LEN
            && tag.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
        "--tag は [A-Za-z0-9_-] の 1〜{MAX_TAG_LEN} 文字にしてください: {tag}"
    );
    Ok(())
}

/// 同じ根・手数・側で安定したキュー id を作る。
pub(super) fn queue_id(tag: &str, ply: usize, key: &str, own: &str) -> String {
    let digest = format!("{:x}", Sha256::digest(key));
    format!("{tag}_{ply}_{}_{own}", &digest[..16])
}

pub(super) mod compare;
