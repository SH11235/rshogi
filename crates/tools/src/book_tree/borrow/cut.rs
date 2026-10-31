//! `book_mine borrow cut`: 大きい定跡から根ごとの部分木を切り出す。

use super::{
    BookInfo, CutParams, EDGES, EDGES_HEADER, Edge, Kind, META, META_VERSION, Meta, NODES,
    NODES_HEADER, Node, RootInfo, Subtree, columns, side, turn,
};
use crate::book_backprop::DrawValues;
use crate::book_tree::audit::{MasterValue, master_values, read_master};
use crate::book_tree::journal::digest;
use crate::book_tree::model::child;
use crate::book_tree::targets::{DEFAULT_PRIORITY, Line, Opts, parse_root};
use crate::common::io::write_atomic;
use anyhow::{Context, Result, ensure};
use clap::{Args, ValueEnum};
use rshogi_book::{Book, BookEntryMove};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, BufWriter, Write as _};
use std::path::{Path, PathBuf};

#[derive(Args, Debug)]
pub(super) struct CutArgs {
    /// 大きい定跡 (.db か YaneuraOu の .ybb)。.db は sfen 行が手数を除いた SFEN のバイト順に並んでいること (ファイル上で二分探索する)
    #[arg(long)]
    book: PathBuf,
    /// 根の一覧 (JSON Lines、1 行に id・root・own。他の項目は読まない)
    #[arg(long)]
    roots: PathBuf,
    /// 出力先 (まだ無い dir)
    #[arg(long)]
    out: PathBuf,
    /// 自分の手番で、大きい定跡の最善の値からこの差以内の手を伸ばす
    #[arg(long, default_value_t = 30)]
    own_diff: i32,
    /// 相手の手番で、大きい定跡の最善の値からこの差以内の手を伸ばす (--opp-top の手と合わせる)
    #[arg(long, default_value_t = 100)]
    opp_diff: i32,
    /// 相手の手番で、大きい定跡の値の上位この数の手を伸ばす (--opp-diff の手と合わせる)
    #[arg(long, default_value_t = 2)]
    opp_top: usize,
    /// 根からの手数の上限。この手数の局面は出口にする
    #[arg(long, default_value_t = 20)]
    max_plies: usize,
    /// 根ごとの局面数の上限。伸ばすと超える局面は出口にする
    #[arg(long, default_value_t = 20000)]
    max_positions: usize,
    /// 自分の手番の途中の局面を、自分の手のこの数おきに読む (根を含む)。0 で読まない
    #[arg(long, default_value_t = 2)]
    check_every: usize,
    /// 定跡の元データの表。ここにある局面 (根を除く) を出口にする
    #[arg(long)]
    master: Option<PathBuf>,
    /// 元データの局面の出口で、元データの値を使う (use) か、自分のエンジンで読む (read)
    #[arg(long, value_enum, default_value_t = MasterExit::Use, requires = "master")]
    master_exit: MasterExit,
    /// 足す手 (TSV、見出しに key・move。`borrow value` の marks.tsv をそのまま渡せる)。どの根でも同じ局面に足す
    #[arg(long)]
    extra: Option<PathBuf>,
    /// キューの行の id `<根の id>_<tag>_<b|w>` に入れる名前。読む局面の組を変えたら変える
    #[arg(long, default_value = "ext")]
    tag: String,
    /// キューの行の priority
    #[arg(long, default_value_t = DEFAULT_PRIORITY)]
    priority: u32,
}

#[cfg(test)]
impl CutArgs {
    pub(super) fn for_test(
        book: PathBuf,
        roots: PathBuf,
        out: PathBuf,
        master: Option<PathBuf>,
    ) -> Self {
        Self {
            book,
            roots,
            out,
            own_diff: 30,
            opp_diff: 20,
            opp_top: 1,
            max_plies: 4,
            max_positions: 100,
            check_every: 1,
            master,
            master_exit: MasterExit::Use,
            extra: None,
            tag: "ext".into(),
            priority: DEFAULT_PRIORITY,
        }
    }

    pub(super) fn set_check_every(&mut self, every: usize) {
        self.check_every = every;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(super) enum MasterExit {
    Use,
    Read,
}

impl MasterExit {
    fn as_str(self) -> &'static str {
        match self {
            MasterExit::Use => "use",
            MasterExit::Read => "read",
        }
    }
}

/// 根の一覧の 1 行。BookMiner の木のキューの行もそのまま読める。
#[derive(Debug, Deserialize)]
struct RootLine {
    id: String,
    root: String,
    own: String,
}

/// 元データの局面で止めるときの値。
pub(super) struct MasterStop<'a> {
    pub values: &'a BTreeMap<String, MasterValue>,
    pub mode: MasterExit,
}

/// 元データの局面を出口にするとき、元データの値を使えるか。自分の手番は main、相手の手番は 1 位の値を使う。
pub(super) fn master_value(value: &MasterValue, own_turn: bool) -> Option<i32> {
    if own_turn { value.main } else { value.best }
}

pub(super) fn run(args: CutArgs) -> Result<()> {
    let params = CutParams {
        own_diff: args.own_diff,
        opp_diff: args.opp_diff,
        opp_top: args.opp_top,
        max_plies: args.max_plies,
        max_positions: args.max_positions,
        check_every: args.check_every,
        master_exit: args.master.as_ref().map(|_| args.master_exit.as_str().to_owned()),
    };
    ensure!(params.own_diff >= 0 && params.opp_diff >= 0, "差は 0 以上にしてください");
    ensure!(params.max_positions >= 1, "--max-positions は 1 以上にしてください");
    ensure!(
        !args.tag.is_empty() && args.tag.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'),
        "--tag は [A-Za-z0-9-] にしてください"
    );
    let roots = read_roots(&args.roots)?;
    let extras = match &args.extra {
        Some(path) => read_extras(path)?,
        None => BTreeMap::new(),
    };
    let master = match &args.master {
        // 止めるかどうかは値が分かるかだけで決まり、千日手の値には依らない。
        Some(path) => Some(master_values(&read_master(path)?, DrawValues::default())?),
        None => None,
    };
    let book = Book::open_on_the_fly(&args.book, true).with_context(|| {
        format!(
            "{} を開けません (.db は sfen 行が手数を除いた SFEN のバイト順に並んでいる必要があります。並んでいなければ並べ替えた写しを渡す)",
            args.book.display()
        )
    })?;
    let info = BookInfo {
        name: args
            .book
            .file_name()
            .context("--book のファイル名がありません")?
            .to_string_lossy()
            .into_owned(),
        bytes: std::fs::metadata(&args.book)?.len(),
        sha256: digest(&args.book)?,
    };
    ensure!(
        !info.name.contains(['\t', '\n', '\r']),
        "--book のファイル名にタブや改行は使えません"
    );
    std::fs::create_dir_all(
        args.out
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )?;
    std::fs::create_dir(&args.out).with_context(|| {
        format!("{} を作れません (まだ無い dir にしてください)", args.out.display())
    })?;

    let mut nodes_out = BufWriter::new(std::fs::File::create(args.out.join(NODES))?);
    let mut edges_out = BufWriter::new(std::fs::File::create(args.out.join(EDGES))?);
    writeln!(nodes_out, "{NODES_HEADER}")?;
    writeln!(edges_out, "{EDGES_HEADER}")?;
    let mut summary = String::from(
        "id\town\tin_book\tpositions\tinner\texit_nobook\texit_depth\texit_cap\texit_master\tchecks\treads\textra_used\n",
    );
    let mut reads: BTreeMap<String, u32> = BTreeMap::new();
    let mut queue = String::new();
    let mut extras_used: BTreeSet<(String, String)> = BTreeSet::new();
    let mut lookup = |sfen: &str| book.entry_moves(sfen).map_err(anyhow::Error::from);
    for root in &roots {
        let stop = master.as_ref().map(|values| MasterStop {
            values,
            mode: args.master_exit,
        });
        let tree = cut_root(root, &params, &mut lookup, stop.as_ref(), &extras)?;
        let used: BTreeSet<(&str, &str)> = tree
            .edges
            .iter()
            .filter(|e| e.extra)
            .map(|e| (e.key.as_str(), e.mv.as_str()))
            .collect();
        extras_used.extend(used.iter().map(|&(k, m)| (k.to_owned(), m.to_owned())));
        for (key, node) in &tree.nodes {
            writeln!(
                nodes_out,
                "{}\t{key}\t{}\t{}\t{}\t{}\t{}\t{}",
                root.id,
                node.ply,
                node.level,
                side(key),
                turn(key, &root.own),
                node.kind.as_str(),
                u8::from(node.read)
            )?;
            if node.read {
                let ply = reads.entry(key.clone()).or_insert(node.ply);
                *ply = (*ply).min(node.ply);
            }
        }
        for e in &tree.edges {
            writeln!(
                edges_out,
                "{}\t{}\t{}\t{}\t{}\t{}",
                root.id,
                e.key,
                e.mv,
                e.child,
                e.book_value.map_or_else(String::new, |v| v.to_string()),
                if e.extra { "extra" } else { "book" }
            )?;
        }
        let count = |kind: Kind| tree.nodes.values().filter(|n| n.kind == kind).count();
        let root_in_book = tree.nodes[&root.key].kind == Kind::Inner;
        let checks = tree.nodes.values().filter(|n| n.kind == Kind::Inner && n.read).count();
        let row_reads = tree.nodes.values().filter(|n| n.read).count();
        writeln!(
            summary,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{checks}\t{row_reads}\t{}",
            root.id,
            root.own,
            u8::from(root_in_book),
            tree.nodes.len(),
            count(Kind::Inner),
            count(Kind::Nobook),
            count(Kind::Depth),
            count(Kind::Cap),
            count(Kind::Master),
            used.len()
        )?;
        for line in queue_lines(root, &tree, &args.tag, args.priority)? {
            queue.push_str(&serde_json::to_string(&line)?);
            queue.push('\n');
        }
    }
    nodes_out.flush()?;
    edges_out.flush()?;
    let mut read_list = String::from("key\tply\n");
    for (key, ply) in &reads {
        writeln!(read_list, "{key}\t{ply}")?;
    }
    write_atomic(&args.out.join("reads.tsv"), &read_list)?;
    write_atomic(&args.out.join("targets.jsonl"), &queue)?;
    write_atomic(&args.out.join("summary.tsv"), &summary)?;
    let meta = Meta {
        version: META_VERSION.into(),
        book: info,
        params,
        roots,
    };
    write_atomic(&args.out.join(META), &serde_json::to_string_pretty(&meta)?)?;
    print!("{summary}");
    println!("読む局面 (根をまたいで重複を除く) {}", reads.len());
    let unused_extras = extras
        .iter()
        .flat_map(|(k, moves)| moves.iter().map(move |m| (k.clone(), m.clone())))
        .filter(|km| !extras_used.contains(km))
        .count();
    if unused_extras > 0 {
        println!(
            "足せなかった --extra の手 {unused_extras} (どの根でも部分木の途中の自分の手番の局面に無い、または非合法)"
        );
    }
    Ok(())
}

fn check_id(id: &str) -> Result<()> {
    ensure!(
        !id.is_empty()
            && id.len() <= 64
            && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
        "id は [A-Za-z0-9_-] の 1〜64 文字が必要です: {id}"
    );
    Ok(())
}

fn read_roots(path: &Path) -> Result<Vec<RootInfo>> {
    let file =
        std::fs::File::open(path).with_context(|| format!("{} を読めません", path.display()))?;
    let mut roots = Vec::new();
    let mut ids = BTreeSet::new();
    for (n, text) in BufReader::new(file).lines().enumerate() {
        let text = text?;
        if text.trim().is_empty() {
            continue;
        }
        let at = || format!("{}:{}", path.display(), n + 1);
        let line: RootLine = serde_json::from_str(&text).with_context(at)?;
        check_id(&line.id).with_context(at)?;
        ensure!(ids.insert(line.id.clone()), "{}: id が重なっています: {}", at(), line.id);
        ensure!(matches!(line.own.as_str(), "b" | "w"), "{}: own は b / w にしてください", at());
        let root = parse_root(&line.root).with_context(at)?;
        ensure!(
            side(&root.key) == line.own,
            "{}: 根は自分の手番の局面にしてください: {}",
            at(),
            line.id
        );
        roots.push(RootInfo {
            id: line.id,
            own: line.own,
            key: root.key,
            ply: root.ply,
        });
    }
    ensure!(!roots.is_empty(), "根がありません: {}", path.display());
    Ok(roots)
}

/// 局面 key → 足す手。
type Extras = BTreeMap<String, Vec<String>>;

fn read_extras(path: &Path) -> Result<Extras> {
    let file =
        std::fs::File::open(path).with_context(|| format!("{} を読めません", path.display()))?;
    let mut lines = BufReader::new(file).lines();
    let header = lines.next().transpose()?.context("--extra が空です")?;
    let col = columns(&header, &["key", "move"])?;
    let mut extras: Extras = BTreeMap::new();
    for line in lines {
        let line = line?;
        if line.is_empty() {
            continue;
        }
        let c: Vec<&str> = line.split('\t').collect();
        let get = |i: usize| {
            c.get(col[i])
                .copied()
                .with_context(|| format!("--extra の列が足りません: {line}"))
        };
        let moves = extras.entry(get(0)?.to_owned()).or_default();
        let mv = get(1)?.to_owned();
        if !moves.contains(&mv) {
            moves.push(mv);
        }
    }
    Ok(extras)
}

/// 大きい定跡の手から伸ばす手を選ぶ。`moves` は (手, 値) を定跡の順に並べたもの (合法手だけ)。
pub(super) fn select(moves: &[(String, i32)], own_turn: bool, params: &CutParams) -> Vec<usize> {
    let Some(best) = moves.iter().map(|(_, v)| *v).max() else {
        return Vec::new();
    };
    // 値の降順 (同じ値は定跡の順)。
    let mut order: Vec<usize> = (0..moves.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(moves[i].1));
    order
        .into_iter()
        .enumerate()
        .filter(|&(rank, i)| {
            let gap = i64::from(best) - i64::from(moves[i].1);
            if own_turn {
                gap <= i64::from(params.own_diff)
            } else {
                rank < params.opp_top || gap <= i64::from(params.opp_diff)
            }
        })
        .map(|(_, i)| i)
        .collect()
}

/// 根 1 本の部分木を幅優先で切り出す。同じ局面 (手数を除く key) は 1 つにまとめる。
/// `lookup` は ply 付き SFEN で大きい定跡の手を引く。
pub(super) fn cut_root(
    root: &RootInfo,
    params: &CutParams,
    lookup: &mut impl FnMut(&str) -> Result<Vec<BookEntryMove>>,
    master: Option<&MasterStop>,
    extras: &BTreeMap<String, Vec<String>>,
) -> Result<Subtree> {
    let mut tree = Subtree::default();
    tree.nodes.insert(
        root.key.clone(),
        Node {
            ply: root.ply,
            level: 0,
            kind: Kind::Inner,
            read: false,
        },
    );
    let mut queue = VecDeque::from([root.key.clone()]);
    while let Some(key) = queue.pop_front() {
        let (ply, level) = (tree.nodes[&key].ply, tree.nodes[&key].level);
        let own_turn = side(&key) == root.own;
        let kind = 'kind: {
            if level > 0
                && let Some(stop) = master
                && let Some(value) = stop.values.get(&key)
            {
                tree.nodes.get_mut(&key).expect("局面がある").read =
                    stop.mode == MasterExit::Read || master_value(value, own_turn).is_none();
                break 'kind Kind::Master;
            }
            let mut moves: Vec<(String, i32)> = Vec::new();
            let mut children: Vec<String> = Vec::new();
            for m in lookup(&format!("{key} {ply}"))? {
                if moves.iter().any(|(mv, _)| *mv == m.move_usi) {
                    continue;
                }
                // 非合法手 (定跡の誤り・別の表記) は伸ばさない。
                if let Ok(c) = child(&key, &m.move_usi) {
                    moves.push((m.move_usi, m.value));
                    children.push(c);
                }
            }
            if moves.is_empty() {
                break 'kind Kind::Nobook;
            }
            if level >= params.max_plies {
                break 'kind Kind::Depth;
            }
            let mut picked: Vec<(String, Option<i32>, String, bool)> =
                select(&moves, own_turn, params)
                    .into_iter()
                    .map(|i| (moves[i].0.clone(), Some(moves[i].1), children[i].clone(), false))
                    .collect();
            if own_turn {
                for mv in extras.get(&key).into_iter().flatten() {
                    if picked.iter().any(|(m, ..)| m == mv) {
                        continue;
                    }
                    if let Ok(c) = child(&key, mv) {
                        let value = moves.iter().find(|(m, _)| m == mv).map(|(_, v)| *v);
                        picked.push((mv.clone(), value, c, true));
                    }
                }
            }
            let new: BTreeSet<&String> = picked
                .iter()
                .map(|(.., c, _)| c)
                .filter(|c| !tree.nodes.contains_key(*c))
                .collect();
            if tree.nodes.len() + new.len() > params.max_positions {
                break 'kind Kind::Cap;
            }
            picked.sort_by(|a, b| a.0.cmp(&b.0));
            for (mv, book_value, c, extra) in picked {
                if !tree.nodes.contains_key(&c) {
                    tree.nodes.insert(
                        c.clone(),
                        Node {
                            ply: ply.checked_add(1).context("手数の上限")?,
                            level: level + 1,
                            kind: Kind::Inner,
                            read: false,
                        },
                    );
                    queue.push_back(c.clone());
                }
                tree.edges.push(Edge {
                    key: key.clone(),
                    mv,
                    child: c,
                    book_value,
                    extra,
                });
            }
            Kind::Inner
        };
        let node = tree.nodes.get_mut(&key).expect("局面がある");
        node.kind = kind;
        match kind {
            Kind::Inner => {
                node.read = own_turn
                    && params.check_every > 0
                    && (level / 2).is_multiple_of(params.check_every);
            }
            Kind::Master => {}
            _ => node.read = true,
        }
    }
    tree.edges.sort_by(|a, b| (&a.key, &a.mv).cmp(&(&b.key, &b.mv)));
    Ok(tree)
}

/// 読む局面を手番ごとに段 0 のキューの行にする (同じ行の根は同じ手番が必要)。
fn queue_lines(root: &RootInfo, tree: &Subtree, tag: &str, priority: u32) -> Result<Vec<Line>> {
    let mut lines = Vec::new();
    for s in ["b", "w"] {
        let mut keys = tree
            .nodes
            .iter()
            .filter(|(k, n)| n.read && side(k) == s)
            .map(|(k, n)| format!("sfen {k} {}", n.ply));
        let Some(first) = keys.next() else {
            continue;
        };
        let id = format!("{}_{tag}_{s}", root.id);
        check_id(&id).context("根の id か --tag を短くしてください")?;
        let line = Line {
            id,
            root: first,
            roots: keys.collect(),
            own: root.own.clone(),
            depth: 0,
            priority,
            hold: false,
            game: Vec::new(),
            opts: Opts::default(),
            memo: Some(format!("borrow cut {} の読む局面 (手番 {s})", root.id)),
        };
        line.check()?;
        lines.push(line);
    }
    Ok(lines)
}
