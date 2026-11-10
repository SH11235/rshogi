//! `master walk` と同じ辿りで RAMU の手番の局面を集め、元データの main の伝播の値 (`master audit --bridge-replies`
//! と同じ計算) と外部定跡の値を比べて、食い違う局面を出す。外部定跡の値は別の物差しなので、読んで確かめる所の
//! 当たりにだけ使う。

use super::records::{self, Reach, RecordOptions};
use super::{
    OppOptions, Probe, Row, Sources, Stop, WalkOptions, advance, black_to_move, book_probe,
    check_tag, continuation, excluded_positions, open_book, opponent_moves, queue_id,
};
use crate::book_backprop::DrawValues;
use crate::book_tree::audit::{self, Fill, Origin, reject_same_file};
use crate::book_tree::master_index::MasterIndex;
use crate::book_tree::master_table::{MasterPosition, read_master};
use crate::book_tree::refutation::format_queue;
use crate::book_tree::reply_graph::{HIRATE_KEY, SideArg, nontrivial_sccs, thread_pool};
use crate::book_tree::schema::{self, Schema};
use crate::book_tree::targets::{Line, MAX_DEPTH, Opts};
use crate::common::io::write_atomic;
use crate::kif::usi_moves_to_short_kif;
use anyhow::{Context, Result, ensure};
use clap::Args;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::PathBuf;

#[derive(Args, Debug)]
pub(in crate::book_tree) struct OppBookDiffArgs {
    /// 定跡の元データの表 (TSV)
    #[arg(long)]
    master: PathBuf,
    /// 語彙と読み条件の表を置いたディレクトリ
    #[arg(long)]
    schema: Option<PathBuf>,
    /// 比べる外部定跡 (.db か YaneuraOu の .ybb)。辿りでは `master walk --opp-book` と同じく相手の手にも使う
    #[arg(long)]
    opp_book: PathBuf,
    /// RAMU の側
    #[arg(long, value_enum, default_value_t = SideArg::Both)]
    side: SideArg,
    /// 相手の手の頻度の定跡 (`master walk --opp-freq` と同じ)
    #[arg(long)]
    opp_freq: Option<PathBuf>,
    #[arg(long, default_value_t = 3)]
    min_games: u64,
    #[arg(long, default_value_t = 80)]
    max_ply: usize,
    #[arg(long, default_value_t = 1e-5)]
    weight_floor: f64,
    #[arg(long, default_value_t = 30)]
    opp_book_diff: i32,
    #[arg(long, default_value_t = 0.9)]
    opp_book_best: f64,
    #[arg(long)]
    opp_book_first: bool,
    /// 定跡が切れた所からの外部定跡の続きと、比べた局面から定跡が切れる所までを辿る手数の上限
    #[arg(long, default_value_t = 100)]
    cont_max: usize,
    /// 比べる局面の辿りの重みの下限
    #[arg(long, default_value_t = 0.002)]
    min_weight: f64,
    /// 差 (外部定跡が RAMU の側に悪いと見る量) か逆向きの差がこれ以上の局面を出す
    #[arg(long, default_value_t = 80)]
    min_gap: i64,
    /// 先手を自分として伝播するときの千日手の値 (`master audit` と同じ)
    #[arg(long, default_value_t = -70, allow_hyphen_values = true)]
    draw_value_black: i32,
    /// 後手を自分として伝播するときの千日手の値 (`master audit` と同じ)
    #[arg(long, default_value_t = 70, allow_hyphen_values = true)]
    draw_value_white: i32,
    /// 伝播のスレッド数 (0 = 全コア)
    #[arg(long, default_value_t = 0)]
    threads: usize,
    /// 強い対局の棋譜 (`master walk --opp-records` と同じ)。比べた局面に着いた局数と得点の列を足す
    #[arg(long)]
    opp_records: Option<PathBuf>,
    #[arg(long, default_value_t = 3800.0, requires = "opp_records")]
    records_min_rate: f64,
    #[arg(long, default_value_t = 130, requires = "opp_records")]
    records_max_ply: usize,
    /// 既に積んだ木のキュー (`master walk --exclude-queue` と同じ照合)。繰り返して複数
    #[arg(long)]
    exclude_queue: Vec<PathBuf>,
    /// 差が `--min-gap` 以上の局面から定跡が切れた所を根にした木のキューの行 (JSON Lines)
    #[arg(long)]
    enqueue_out: Option<PathBuf>,
    #[arg(long, default_value_t = 6)]
    depth: usize,
    #[arg(long, default_value_t = 300)]
    priority: u32,
    #[arg(long, default_value = "oppdiff")]
    tag: String,
    #[arg(long, default_value_t = 3)]
    opp_n: usize,
    #[arg(long, default_value_t = 300)]
    stop: i32,
    #[arg(long, default_value_t = 3)]
    rounds: usize,
    #[arg(long, default_value_t = -70, allow_hyphen_values = true)]
    draw_black: i32,
    #[arg(long, default_value_t = 70, allow_hyphen_values = true)]
    draw_white: i32,
    /// 全部の行の表 (TSV)
    #[arg(long)]
    table: Option<PathBuf>,
    /// 報告 (Markdown)
    #[arg(long)]
    markdown: Option<PathBuf>,
    /// 報告の見出しに使う版の名前
    #[arg(long, default_value = "")]
    title: String,
}

impl OppBookDiffArgs {
    fn queue_opts(&self) -> Opts {
        Opts {
            opp_n: Some(self.opp_n),
            stop: Some(self.stop),
            rounds: Some(self.rounds),
            draw_black: Some(self.draw_black),
            draw_white: Some(self.draw_white),
            ..Opts::default()
        }
    }

    fn walk_options(&self) -> WalkOptions {
        WalkOptions {
            max_ply: self.max_ply,
            floor: self.weight_floor,
            cont_max: self.cont_max,
            opp: OppOptions {
                min_games: self.min_games,
                book_diff: self.opp_book_diff,
                book_best: self.opp_book_best,
                book_first: self.opp_book_first,
            },
        }
    }

    fn check(&self) -> Result<()> {
        ensure!(
            self.table.is_some() || self.markdown.is_some() || self.enqueue_out.is_some(),
            "--table・--markdown・--enqueue-out のどれかの出力先が必要です"
        );
        for w in [self.weight_floor, self.min_weight] {
            ensure!(w.is_finite() && w >= 0.0, "重みの下限は有限の非負数にしてください: {w}");
        }
        ensure!(self.min_gap > 0, "--min-gap は 1 以上にしてください");
        ensure!(
            (0.0..=1.0).contains(&self.opp_book_best),
            "--opp-book-best は 0〜1 にしてください"
        );
        ensure!(self.opp_book_diff >= 0, "--opp-book-diff は 0 以上にしてください");
        ensure!(self.depth <= MAX_DEPTH, "--depth は {MAX_DEPTH} 以下にしてください");
        ensure!(self.records_min_rate.is_finite(), "--records-min-rate は有限の数にしてください");
        check_tag(&self.tag)?;
        self.queue_opts().check()?;
        let mut inputs = vec![
            ("--master", self.master.as_path()),
            ("--opp-book", self.opp_book.as_path()),
        ];
        if let Some(path) = &self.opp_freq {
            inputs.push(("--opp-freq", path.as_path()));
        }
        if let Some(path) = &self.opp_records {
            inputs.push(("--opp-records", path.as_path()));
        }
        for path in &self.exclude_queue {
            inputs.push(("--exclude-queue", path.as_path()));
        }
        let schema_paths: Vec<_> = self.schema.iter().flat_map(|dir| schema::paths(dir)).collect();
        for path in &schema_paths {
            inputs.push(("--schema", path.as_path()));
        }
        for (name, output) in [
            ("--table", &self.table),
            ("--markdown", &self.markdown),
            ("--enqueue-out", &self.enqueue_out),
        ] {
            if let Some(output) = output {
                reject_same_file(output, &inputs)?;
                inputs.push((name, output.as_path()));
            }
        }
        Ok(())
    }
}

/// RAMU の手番の局面に来る辿りの重み。手順は最も少ない手数で最初に辿ったもの。
#[derive(Debug, Clone, PartialEq)]
struct Visit {
    weight: f64,
    ply: usize,
    moves: Vec<String>,
}

/// 1 本の辿りが通った、繰り返し得る局面の組 (番号の昇順) を番号にして持つ。
#[derive(Default)]
struct VisitedSets {
    sets: Vec<Vec<u32>>,
    ids: HashMap<Vec<u32>, u32>,
}

impl VisitedSets {
    fn new() -> Self {
        let mut sets = Self::default();
        sets.intern(Vec::new());
        sets
    }

    fn intern(&mut self, set: Vec<u32>) -> u32 {
        if let Some(&id) = self.ids.get(&set) {
            return id;
        }
        let id = u32::try_from(self.sets.len()).expect("組の数が u32 に収まる");
        self.sets.push(set.clone());
        self.ids.insert(set, id);
        id
    }

    /// 組 `id` が `bit` を含めば `None`、含まなければ `bit` を足した組。
    fn add(&mut self, id: u32, bit: u32) -> Option<u32> {
        let set = &self.sets[id as usize];
        let at = set.binary_search(&bit).err()?;
        let mut set = set.clone();
        set.insert(at, bit);
        Some(self.intern(set))
    }
}

/// 局面ごとの、通った組ごとの重みと手順。
type Layer = BTreeMap<String, (Vec<String>, BTreeMap<u32, f64>)>;

/// 辿りの局面のうち、千日手で同じ辿りに 2 回来うる局面 (局面の間の手の向きで自明でない強連結成分に入る局面)。
#[derive(Default)]
struct Cycles {
    /// 局面 → 成分の番号。
    component: HashMap<String, u32>,
    /// 成分の中の RAMU の手番の局面 → 組で使う番号。
    bits: HashMap<String, u32>,
}

/// 辿った手の向き (局面の番号の隣接リスト)。
#[derive(Default)]
struct Moves {
    ids: HashMap<String, u32>,
    keys: Vec<String>,
    adjacency: Vec<Vec<u32>>,
    seen: HashSet<(u32, u32)>,
}

impl Moves {
    fn id(&mut self, key: &str) -> u32 {
        if let Some(&id) = self.ids.get(key) {
            return id;
        }
        let id = u32::try_from(self.keys.len()).expect("局面の数が u32 に収まる");
        self.ids.insert(key.to_owned(), id);
        self.keys.push(key.to_owned());
        self.adjacency.push(Vec::new());
        id
    }

    fn add(&mut self, parent: &str, child: &str) {
        let (p, c) = (self.id(parent), self.id(child));
        if self.seen.insert((p, c)) {
            self.adjacency[p as usize].push(c);
        }
    }

    fn cycles(&self, side: &str) -> Cycles {
        let mut cycles = Cycles::default();
        for (n, members) in nontrivial_sccs(&self.adjacency).into_iter().enumerate() {
            let n = u32::try_from(n).expect("成分の数が u32 に収まる");
            for v in members {
                let key = &self.keys[v as usize];
                cycles.component.insert(key.clone(), n);
                if black_to_move(key) == (side == "b") {
                    let bit = u32::try_from(cycles.bits.len()).expect("局面の数が u32 に収まる");
                    cycles.bits.insert(key.clone(), bit);
                }
            }
        }
        cycles
    }
}

/// `master walk` と同じ規則で `side` を RAMU として `initial` から辿り、元データに main のある RAMU の手番の局面ごとに
/// 重みを返す。止まり方 (重みの下限・手数の上限) は walk と同じく、同じ手数・同じ局面の重みの合計で決める。
///
/// `cycles` を渡すと、成分の中では通った RAMU の手番の局面の組ごとに重みを分けて持ち、1 本の辿りで 2 回目以降に
/// 来た重みを数えない。成分を出た辿りはその成分に戻れないので、成分を出るときに組を空にする (組の数は成分の
/// 大きさで決まる)。`moves` を渡すと辿った手の向きを記録する。
fn visit_weights(
    positions: &BTreeMap<String, MasterPosition>,
    src: &mut Sources,
    side: &str,
    initial: &str,
    options: WalkOptions,
    cycles: Option<&Cycles>,
    mut moves: Option<&mut Moves>,
) -> Result<BTreeMap<String, Visit>> {
    let mut sets = VisitedSets::new();
    let mut visits: BTreeMap<String, Visit> = BTreeMap::new();
    let mut layer: Layer =
        BTreeMap::from([(initial.to_owned(), (Vec::new(), BTreeMap::from([(0, 1.0)])))]);
    let same_component = |parent: &str, child: &str| match cycles {
        Some(c) => c.component.get(parent).is_some_and(|n| c.component.get(child) == Some(n)),
        None => false,
    };
    let put = |next: &mut Layer, key: String, path: &[String], mv: &str, set: u32, w: f64| {
        let entry = next.entry(key).or_insert_with(|| {
            let mut p = path.to_vec();
            p.push(mv.to_owned());
            (p, BTreeMap::new())
        });
        *entry.1.entry(set).or_default() += w;
    };
    for ply in 0..options.max_ply {
        let mut next = Layer::new();
        for (key, (path, weights)) in layer {
            if weights.values().sum::<f64>() < options.floor {
                continue;
            }
            if black_to_move(&key) == (side == "b") {
                let Some(position) = positions.get(&key) else {
                    continue;
                };
                let mv = &position.moves[0].mv;
                let child = advance(&key, mv)?;
                if let Some(moves) = moves.as_deref_mut() {
                    moves.add(&key, &child);
                }
                let visit = visits.entry(key.clone()).or_insert_with(|| Visit {
                    weight: 0.0,
                    ply,
                    moves: path.clone(),
                });
                let bit = cycles.and_then(|c| c.bits.get(&key).copied());
                let stays = same_component(&key, &child);
                for (set, w) in weights {
                    let set = match bit.map(|bit| sets.add(set, bit)) {
                        Some(None) => set,
                        Some(Some(added)) => {
                            visit.weight += w;
                            added
                        }
                        None => {
                            visit.weight += w;
                            set
                        }
                    };
                    put(&mut next, child.clone(), &path, mv, if stays { set } else { 0 }, w);
                }
                continue;
            }
            let Some(replies) = opponent_moves(&key, src, options.opp)? else {
                continue;
            };
            for (mv, share) in &replies {
                // walk と同じく、指せない手の分は辿らない。
                if let Ok(child) = advance(&key, mv) {
                    if let Some(moves) = moves.as_deref_mut() {
                        moves.add(&key, &child);
                    }
                    let stays = same_component(&key, &child);
                    for (&set, &w) in &weights {
                        let set = if stays { set } else { 0 };
                        put(&mut next, child.clone(), &path, mv, set, w * share);
                    }
                }
            }
        }
        layer = next;
        if layer.is_empty() {
            break;
        }
    }
    Ok(visits)
}

/// 1 本の辿りで同じ局面を 1 回だけ数えた重み。辿った手に循環が無ければ 1 回、あれば循環の局面を分けて 2 回辿る。
fn side_visits(
    positions: &BTreeMap<String, MasterPosition>,
    src: &mut Sources,
    side: &str,
    initial: &str,
    options: WalkOptions,
) -> Result<BTreeMap<String, Visit>> {
    let mut moves = Moves::default();
    let visits = visit_weights(positions, src, side, initial, options, None, Some(&mut moves))?;
    let cycles = moves.cycles(side);
    drop(moves);
    if cycles.bits.is_empty() {
        return Ok(visits);
    }
    visit_weights(positions, src, side, initial, options, Some(&cycles), None)
}

/// 比べた局面から、RAMU は元データの main、相手は外部定跡の 1 位 (無ければ元データの main) を指して進んだ所。
#[derive(Debug, Clone, PartialEq)]
struct Cut {
    /// `own` (RAMU の手番で元データに無い局面に着いた)・`out` (相手の手が無い)・`cycle`・`max`。
    end: &'static str,
    /// 初期局面から、止まった局面までの手順。
    moves: Vec<String>,
    key: String,
    /// `own` の局面から外部定跡の 1 位を両者が指し続けた手順と、最後に引けた局面の 1 位の値 (先手から見た値)。
    cont: Vec<String>,
    cont_end_black: Option<i64>,
}

fn cut_after(
    positions: &BTreeMap<String, MasterPosition>,
    book: &mut Probe,
    side: &str,
    key: &str,
    moves: &[String],
    max: usize,
) -> Result<Cut> {
    let mut key = key.to_owned();
    let mut moves = moves.to_vec();
    let mut seen = HashSet::new();
    let mut steps = 0;
    let end = loop {
        if !seen.insert(key.clone()) {
            break "cycle";
        }
        let main = positions.get(&key).map(|p| p.moves[0].mv.clone());
        let own = black_to_move(&key) == (side == "b");
        if own && main.is_none() {
            break "own";
        }
        if steps == max {
            break "max";
        }
        let best = if own {
            None
        } else {
            book.best(&key)?.map(|(mv, _)| mv)
        };
        // 外部定跡の手は合法性を確かめていない。
        let Some(mv) = best.into_iter().chain(main).find(|mv| advance(&key, mv).is_ok()) else {
            break "out";
        };
        key = advance(&key, &mv)?;
        moves.push(mv);
        steps += 1;
    };
    let (cont, cont_end_black) = if end == "own" {
        continuation(&key, book, max)?
    } else {
        (Vec::new(), None)
    };
    Ok(Cut {
        end,
        moves,
        key,
        cont,
        cont_end_black,
    })
}

/// 比べた局面の 1 行。値は RAMU (局面の手番の側) から見た値。
struct Diff {
    side: &'static str,
    ply: usize,
    weight: f64,
    key: String,
    moves: Vec<String>,
    main: String,
    ours: i64,
    main_from: Origin,
    main_kind: String,
    ext_same: Option<i64>,
    ext_best: String,
    ext_best_value: i64,
    cut: Cut,
    queued: Vec<String>,
    reach: Option<Reach>,
}

impl Diff {
    /// 外部定跡が RAMU の側に悪いと見る量。外部定跡に main が無ければ外部定跡の 1 位と比べる。
    fn gap(&self) -> i64 {
        self.ours - self.ext_same.unwrap_or(self.ext_best_value)
    }

    /// 外部定跡の 1 位が RAMU の側に良いと見る量。
    fn reverse(&self) -> i64 {
        self.ext_best_value - self.ours
    }

    fn black(&self, value: i64) -> i64 {
        if self.side == "b" { value } else { -value }
    }
}

/// 比べなかった局面の数 (側ごと)。
#[derive(Debug, Default, PartialEq)]
struct Skipped {
    compared: usize,
    no_value: usize,
    not_in_book: usize,
}

pub(in crate::book_tree) fn run(args: OppBookDiffArgs) -> Result<()> {
    thread_pool(args.threads)?.install(|| run_from(args, HIRATE_KEY))
}

fn run_from(args: OppBookDiffArgs, initial: &str) -> Result<()> {
    args.check()?;
    let excluded = excluded_positions(&args.exclude_queue)?;
    let mut index = MasterIndex::open(&args.master, &Schema::load(args.schema.as_deref())?)?;
    let book_data = read_master(&args.master)?;
    let draws = DrawValues {
        black: args.draw_value_black,
        white: args.draw_value_white,
    };
    let (_, _, audited) = audit::audit(&book_data, draws, 1, Fill::Bridge, None)?;
    let positions = &book_data.0;
    let freq = args.opp_freq.as_deref().map(open_book).transpose()?;
    if let (Some(book), Some(path)) = (&freq, &args.opp_freq) {
        // on the fly で局面数を返すのは .ybb だけ。.ybb は採択回数を持たないので局数にならない。
        ensure!(
            book.position_count().is_none(),
            "--opp-freq は採択回数を持つテキストの .db にしてください: {}",
            path.display()
        );
    }
    let ext = open_book(&args.opp_book)?;
    let mut src = Sources {
        freq: freq.as_ref().map(book_probe),
        book: Some(book_probe(&ext)),
    };
    let mut picked: Vec<(&'static str, String, Visit)> = Vec::new();
    for &side in args.side.sides() {
        let visits = side_visits(positions, &mut src, side, initial, args.walk_options())?;
        picked.extend(
            visits
                .into_iter()
                .filter(|(_, v)| v.weight >= args.min_weight)
                .map(|(key, v)| (side, key, v)),
        );
    }
    let wanted: HashSet<&str> = picked.iter().map(|(_, key, _)| key.as_str()).collect();
    let order: HashMap<&str, usize> = positions
        .keys()
        .enumerate()
        .filter(|(_, key)| wanted.contains(key.as_str()))
        .map(|(i, key)| (key.as_str(), i))
        .collect();
    let book = src.book.as_mut().expect("外部定跡は必ず開く");
    let mut skipped: BTreeMap<&str, Skipped> =
        args.side.sides().iter().map(|&side| (side, Skipped::default())).collect();
    let mut diffs = Vec::new();
    for (side, key, visit) in picked {
        let count = skipped.entry(side).or_default();
        let position = &positions[&key];
        let audited_main = audit::audited_moves(
            audited.graph_for(&key),
            &key,
            position,
            &audited.known[order[key.as_str()]],
        )?
        .swap_remove(0);
        let Some(main) = audited_main else {
            count.no_value += 1;
            continue;
        };
        let ext_moves = book.moves(&key)?;
        let Some(best) = ext_moves.iter().reduce(|b, m| if m.value > b.value { m } else { b })
        else {
            count.not_in_book += 1;
            continue;
        };
        count.compared += 1;
        let ext_same = ext_moves.iter().find(|m| m.move_usi == main.mv).map(|m| i64::from(m.value));
        let (ext_best, ext_best_value) = (best.move_usi.clone(), i64::from(best.value));
        // audit と同じく、同値の手の -1 の補正を戻した値を main の値にする。
        let ours = i64::from(main.value) + i64::from(main.tie_lowered);
        let gap = ours - ext_same.unwrap_or(ext_best_value);
        if gap < args.min_gap && ext_best_value - ours < args.min_gap {
            continue;
        }
        let main_kind = index
            .rows(&key)?
            .iter()
            .find(|row| row.get("move") == main.mv)
            .map(|row| row.get("value_kind").to_owned())
            .unwrap_or_default();
        let cut = cut_after(positions, book, side, &key, &visit.moves, args.cont_max)?;
        let queued = if cut.end == "own" {
            excluded
                .get(&(cut.key.clone(), side.to_owned()))
                .map(|ids| ids.iter().cloned().collect())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        diffs.push(Diff {
            side,
            ply: visit.ply,
            weight: visit.weight,
            key,
            moves: visit.moves,
            main: main.mv,
            ours,
            main_from: main.origin,
            main_kind,
            ext_same,
            ext_best,
            ext_best_value,
            cut,
            queued,
            reach: None,
        });
    }
    diffs.sort_by(|a, b| {
        a.side
            .cmp(b.side)
            .then(b.gap().max(b.reverse()).cmp(&a.gap().max(a.reverse())))
            .then(b.weight.total_cmp(&a.weight))
            .then(a.key.cmp(&b.key))
    });
    if let Some(path) = &args.opp_records {
        let rows: Vec<Row> = diffs
            .iter()
            .map(|d| Row {
                side: d.side,
                stop: Stop::Own,
                ply: d.ply,
                weight: d.weight,
                key: d.key.clone(),
                tag: 0,
                moves: d.moves.clone(),
                games: None,
                book_moves: None,
                cont: Vec::new(),
                cont_end_black: None,
            })
            .collect();
        let options = RecordOptions {
            min_rate: args.records_min_rate,
            max_ply: args.records_max_ply,
        };
        let stats = records::reach_stats(&rows, initial, path, &options)?;
        for (diff, [at, ..]) in diffs.iter_mut().zip(stats) {
            diff.reach = Some(at);
        }
    }
    let lines = match &args.enqueue_out {
        Some(_) => Some(queue_lines(&diffs, &args)?),
        None => None,
    };
    if let Some(path) = &args.table {
        write_atomic(path, &format_table(&diffs))?;
    }
    if let Some(path) = &args.markdown {
        write_atomic(path, &markdown(&args, &diffs, &skipped))?;
    }
    if let (Some(path), Some(lines)) = (&args.enqueue_out, &lines) {
        write_atomic(path, &format_queue(lines)?)?;
    }
    print!("{}", format_summary(&args, &diffs, &skipped, lines.as_ref().map(Vec::len)));
    Ok(())
}

/// 差が `--min-gap` 以上の局面から定跡が切れた所 (`own`) を根にした行。同じ根・同じ側は 1 行にまとめ、根の手数は
/// 最も小さい手数にする。並びと priority の順位は、まとめた局面のうち差の最も大きい局面の順。既に積んだ根は除く。
fn queue_lines(diffs: &[Diff], args: &OppBookDiffArgs) -> Result<Vec<Line>> {
    let mut roots: BTreeMap<(&str, &str), Vec<&Diff>> = BTreeMap::new();
    let mut order = Vec::new();
    for d in diffs.iter().filter(|d| d.gap() >= args.min_gap && d.cut.end == "own") {
        if !d.queued.is_empty() {
            continue;
        }
        let entry = roots.entry((d.side, d.cut.key.as_str())).or_default();
        if entry.is_empty() {
            order.push((d.side, d.cut.key.as_str()));
        }
        entry.push(d);
    }
    order.sort_by(|a, b| {
        let head = |k: &(&str, &str)| {
            roots[k]
                .iter()
                .map(|d| (d.gap(), d.weight))
                .max_by(|x, y| x.0.cmp(&y.0).then(x.1.total_cmp(&y.1)))
                .expect("1 局面以上")
        };
        let (ga, wa) = head(a);
        let (gb, wb) = head(b);
        gb.cmp(&ga).then(wb.total_cmp(&wa)).then(a.cmp(b))
    });
    let mut lines = Vec::new();
    for (rank, root) in order.iter().enumerate() {
        let group = &roots[root];
        let first = group.iter().min_by_key(|d| d.cut.moves.len()).expect("1 局面以上");
        let root_ply = first.cut.moves.len() + 1;
        let memo = group
            .iter()
            .map(|d| format!("ply={} gap={} weight={:.5}", d.ply + 1, d.gap(), d.weight))
            .collect::<Vec<_>>()
            .join("; ");
        let game = if first.cut.cont.is_empty() {
            Vec::new()
        } else {
            vec![first.cut.cont.iter().take(args.depth + 1).cloned().collect()]
        };
        let line = Line {
            group: None,
            id: queue_id(&args.tag, root_ply, &first.cut.key, first.side),
            root: format!("sfen {} {root_ply}", first.cut.key),
            roots: Vec::new(),
            own: first.side.to_owned(),
            depth: args.depth,
            priority: args.priority + rank.min(99) as u32,
            hold: false,
            game,
            opts: args.queue_opts(),
            memo: Some(format!("opp-book-diff {memo}")),
        };
        line.check().with_context(|| format!("キューの行の検査: {}", first.cut.key))?;
        lines.push(line);
    }
    Ok(lines)
}

fn opt<T: ToString>(v: Option<T>) -> String {
    v.map(|v| v.to_string()).unwrap_or_default()
}

fn format_table(diffs: &[Diff]) -> String {
    let mut text = String::from(
        "side\tply\tweight\tgap\treverse\tmain\tours\tours_black\tmain_from\tmain_kind\text_same\text_same_black\text_best\text_best_value\text_best_value_black\tcut_end\tcut_ply\tcut_key\tcut_moves\tcont\tcont_end_black\tqueued\tn0\tscore0\tkey\tmoves\n",
    );
    for d in diffs {
        let after_cut = &d.cut.moves[d.moves.len()..];
        writeln!(
            text,
            "{}\t{}\t{:.6}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            d.side,
            d.ply + 1,
            d.weight,
            d.gap(),
            d.reverse(),
            d.main,
            d.ours,
            d.black(d.ours),
            d.main_from.as_str(),
            d.main_kind,
            opt(d.ext_same),
            opt(d.ext_same.map(|v| d.black(v))),
            d.ext_best,
            d.ext_best_value,
            d.black(d.ext_best_value),
            d.cut.end,
            if d.cut.end == "own" { (d.cut.moves.len() + 1).to_string() } else { String::new() },
            d.cut.key,
            after_cut.join(" "),
            d.cut.cont.join(" "),
            opt(d.cut.cont_end_black),
            d.queued.join(","),
            opt(d.reach.as_ref().map(|r| r.games)),
            d.reach.as_ref().map(Reach::score_text).unwrap_or_default(),
            d.key,
            d.moves.join(" "),
        )
        .expect("String への書き込み");
    }
    text
}

/// 差の帯の下端。`--min-gap` から 100・150・200・300・500 で区切る。
fn bands(min_gap: i64) -> Vec<i64> {
    let mut edges = vec![min_gap];
    edges.extend([100, 150, 200, 300, 500].into_iter().filter(|&e| e > min_gap));
    edges
}

fn band_counts(values: impl Iterator<Item = i64>, edges: &[i64]) -> String {
    let mut counts = vec![0usize; edges.len()];
    for v in values {
        if let Some(i) = edges.iter().rposition(|&e| v >= e) {
            counts[i] += 1;
        }
    }
    edges
        .iter()
        .enumerate()
        .map(|(i, e)| match edges.get(i + 1) {
            Some(next) => format!("{e}-{}: {}", next - 1, counts[i]),
            None => format!("{e}-: {}", counts[i]),
        })
        .collect::<Vec<_>>()
        .join(" / ")
}

fn format_summary(
    args: &OppBookDiffArgs,
    diffs: &[Diff],
    skipped: &BTreeMap<&str, Skipped>,
    queued: Option<usize>,
) -> String {
    let mut text = String::new();
    let edges = bands(args.min_gap);
    for (side, s) in skipped {
        let rows: Vec<&Diff> = diffs.iter().filter(|d| d.side == *side).collect();
        let worse: Vec<i64> = rows.iter().map(|d| d.gap()).filter(|&g| g >= args.min_gap).collect();
        let better: Vec<i64> =
            rows.iter().map(|d| d.reverse()).filter(|&g| g >= args.min_gap).collect();
        writeln!(
            text,
            "{side}: 比べた局面 {} (重み {} 以上、main の値なし {}・外部定跡に無い {} を除く)",
            s.compared, args.min_weight, s.no_value, s.not_in_book
        )
        .expect("String への書き込み");
        writeln!(
            text,
            "{side}: 外部定跡の方が悪い {} ({})",
            worse.len(),
            band_counts(worse.iter().copied(), &edges)
        )
        .expect("String への書き込み");
        writeln!(
            text,
            "{side}: 外部定跡の 1 位の方が良い {} ({})",
            better.len(),
            band_counts(better.iter().copied(), &edges)
        )
        .expect("String への書き込み");
    }
    if let Some(n) = queued {
        let excluded = diffs
            .iter()
            .filter(|d| d.gap() >= args.min_gap && !d.queued.is_empty())
            .map(|d| (d.side, d.cut.key.as_str()))
            .collect::<BTreeSet<_>>()
            .len();
        writeln!(text, "キューの行 {n} (既に積んだ根で除いた {excluded})")
            .expect("String への書き込み");
    }
    text
}

/// 漢字の棋譜。初期局面が平手でなく読めない手順は USI のまま。
fn kif(moves: &[String]) -> String {
    usi_moves_to_short_kif(&moves.iter().map(String::as_str).collect::<Vec<_>>())
        .unwrap_or_else(|_| moves.join(" "))
}

/// `moves` に `more` を続けた棋譜のうち `more` の分。
fn kif_after(moves: &[String], more: &[String]) -> String {
    let mut all = moves.to_vec();
    all.extend_from_slice(more);
    let text = kif(&all);
    let tokens: Vec<&str> = text.split(' ').collect();
    if tokens.len() == all.len() {
        tokens[moves.len()..].join(" ")
    } else {
        more.join(" ")
    }
}

fn points(v: i64) -> String {
    format!("{v:+} 点")
}

fn side_name(side: &str) -> &'static str {
    if side == "b" { "先手" } else { "後手" }
}

fn markdown(args: &OppBookDiffArgs, diffs: &[Diff], skipped: &BTreeMap<&str, Skipped>) -> String {
    let mut out = String::new();
    let title = if args.title.is_empty() {
        "外部定跡との食い違い".to_owned()
    } else {
        format!("{} の外部定跡との食い違い", args.title)
    };
    let mut w = |s: String| out.push_str(&s);
    w(format!("# {title}\n\n"));
    w(format!(
        "walk と同じ辿りで RAMU の手番に来る重みが {} 以上の局面で、RAMU の main の伝播の値 (`master audit --bridge-replies` と同じ計算) と外部定跡の値を比べた。値は先手から見た値 (プラス = 先手良し)。差 = 外部定跡が RAMU の側に悪いと見る量 (外部定跡に main が無ければ外部定跡の 1 位と比べる)。外部定跡の値は物差しの違う別の評価なので、RAMU の読みで確かめる所の当たりにだけ使う。重み = 辿りでその局面に来る割合 (1 本の辿りで同じ局面は 1 回)。定跡が切れる所 = RAMU は main、相手は外部定跡の 1 位を指して、RAMU の手番で元データに無い局面に着く所。受付 = `--exclude-queue` の木で同じ根・同じ側のもの。\n\n",
        args.min_weight
    ));
    for side in ["b", "w"] {
        let Some(s) = skipped.get(side) else {
            continue;
        };
        w(format!("## RAMU が{}\n\n", side_name(side)));
        w(format!(
            "比べた局面 {} 局面 (main の値なし {}・外部定跡に無い {} を除く)。\n\n",
            s.compared, s.no_value, s.not_in_book
        ));
        let sections = [
            ("外部定跡の方が RAMU に悪い", Diff::gap as fn(&Diff) -> i64),
            ("外部定跡の 1 位の方が RAMU に良い", Diff::reverse),
        ];
        for (name, amount) in sections {
            let mut rows: Vec<&Diff> =
                diffs.iter().filter(|d| d.side == side && amount(d) >= args.min_gap).collect();
            rows.sort_by(|a, b| {
                amount(b)
                    .cmp(&amount(a))
                    .then(b.weight.total_cmp(&a.weight))
                    .then(a.key.cmp(&b.key))
            });
            w(format!("### {name} (差 {} 点以上、{} 局面)\n\n", args.min_gap, rows.len()));
            if rows.is_empty() {
                continue;
            }
            w("| 手数 | 重み | 差 | RAMU の main | RAMU の値 (出所) | 外部定跡の同じ手 | 外部定跡の 1 位 | 定跡が切れる所 | 外部定跡の続き | 受付 | 強い対局 | 手順 |\n|---:|---:|---:|---|---|---:|---|---|---|---|---:|---|\n".to_owned());
            for d in rows {
                let one = |mv: &str| kif_after(&d.moves, &[mv.to_owned()]);
                let kind = if d.main_kind.is_empty() {
                    d.main_from.as_str().to_owned()
                } else {
                    format!("{}, {}", d.main_from.as_str(), d.main_kind)
                };
                let cut = match d.cut.end {
                    "own" => format!(
                        "{} 手目 ({})",
                        d.cut.moves.len() + 1,
                        kif_after(&d.moves, &d.cut.moves[d.moves.len()..])
                    ),
                    end => format!("切れない ({end})"),
                };
                let cont = if d.cut.cont.is_empty() {
                    String::new()
                } else {
                    format!(
                        "{} 手、終わり {} ({})",
                        d.cut.cont.len(),
                        d.cut.cont_end_black.map(points).unwrap_or_default(),
                        kif_after(&d.cut.moves, &d.cut.cont)
                    )
                };
                let queued = if d.queued.is_empty() {
                    "なし".to_owned()
                } else {
                    d.queued.join(", ")
                };
                let reach = match &d.reach {
                    Some(r) if r.games > 0 => format!("{} 局 (得点 {})", r.games, r.score_text()),
                    Some(_) => "0 局".to_owned(),
                    None => String::new(),
                };
                w(format!(
                    "| {} 手目 | {:.2} % | {} | {} | {} ({kind}) | {} | {} {} | {cut} | {cont} | {queued} | {reach} | {} |\n",
                    d.ply + 1,
                    d.weight * 100.0,
                    points(amount(d)),
                    one(&d.main),
                    points(d.black(d.ours)),
                    d.ext_same.map(|v| points(d.black(v))).unwrap_or_else(|| "なし".into()),
                    one(&d.ext_best),
                    points(d.black(d.ext_best_value)),
                    kif(&d.moves),
                ));
            }
            w("\n".to_owned());
        }
    }
    out
}

#[cfg(test)]
mod tests;
