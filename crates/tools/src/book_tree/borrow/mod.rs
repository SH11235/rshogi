//! 大きい定跡 (ペタショック定跡など) の木の形だけを借りる。根から部分木を切り出し、出口と確認点を
//! 自分のエンジンで読み、その値を後退解析で根まで伝播して定跡の元データに登録する。
//! 大きい定跡の値は手を選ぶ当たりにだけ使い、登録する値には混ぜない。

mod cut;
#[cfg(test)]
mod tests;
mod value;

use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::BufRead;
use std::path::Path;

const META: &str = "cut.json";
const NODES: &str = "nodes.tsv";
const EDGES: &str = "edges.tsv";
const META_VERSION: &str = "book-borrow-cut-v1";
const NODES_HEADER: &str = "id\tkey\tply\tlevel\tside\tturn\tkind\tread";
const EDGES_HEADER: &str = "id\tkey\tmove\tchild\tbook_value\torigin";

/// 大きい定跡の木を借りる入口。
#[derive(Args, Debug)]
pub struct BorrowArgs {
    #[command(subcommand)]
    command: BorrowCommand,
}

#[derive(Subcommand, Debug)]
enum BorrowCommand {
    /// 大きい定跡から根ごとの部分木を切り出し、読む局面の一覧とキューの行を書く
    Cut(cut::CutArgs),
    /// 出口と確認点の読みを台帳から引いて伝播し、定跡の元データに登録する
    Value(value::ValueArgs),
}

/// CLI の指定に従い木を借りる。
pub fn run(args: BorrowArgs) -> Result<()> {
    match args.command {
        BorrowCommand::Cut(args) => cut::run(args),
        BorrowCommand::Value(args) => value::run(args),
    }
}

/// 部分木の局面の種類。`Inner` 以外が出口。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// 大きい定跡の手で伸ばした局面。
    Inner,
    /// 大きい定跡に (合法な) 手が無い。
    Nobook,
    /// 根からの手数の上限。
    Depth,
    /// 根ごとの局面数の上限。
    Cap,
    /// 定跡の元データにある局面。
    Master,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Inner => "inner",
            Kind::Nobook => "exit_nobook",
            Kind::Depth => "exit_depth",
            Kind::Cap => "exit_cap",
            Kind::Master => "exit_master",
        }
    }

    fn parse(text: &str) -> Result<Self> {
        Ok(match text {
            "inner" => Kind::Inner,
            "exit_nobook" => Kind::Nobook,
            "exit_depth" => Kind::Depth,
            "exit_cap" => Kind::Cap,
            "exit_master" => Kind::Master,
            _ => bail!("局面の種類が不正です: {text}"),
        })
    }
}

/// 部分木の局面。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Node {
    ply: u32,
    level: usize,
    kind: Kind,
    /// 自分のエンジンで読む局面 (出口と確認点)。
    read: bool,
}

/// 部分木の手。`child` は手を指した後の局面 key。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Edge {
    key: String,
    mv: String,
    child: String,
    /// 大きい定跡の値 (手番の側から)。`--extra` で足した手で大きい定跡に無ければ `None`。
    book_value: Option<i32>,
    extra: bool,
}

/// 根 1 本の部分木。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Subtree {
    nodes: BTreeMap<String, Node>,
    /// 局面 key・手の順。
    edges: Vec<Edge>,
}

/// 根の指定。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RootInfo {
    id: String,
    /// `b` / `w`。根の手番と同じ。
    own: String,
    key: String,
    ply: u32,
}

/// 切り出しの条件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CutParams {
    own_diff: i32,
    opp_diff: i32,
    opp_top: usize,
    max_plies: usize,
    max_positions: usize,
    check_every: usize,
    /// 元データの局面で止めたか、止めた局面を読むか (`use` / `read`)。元データを渡さなければ `None`。
    master_exit: Option<String>,
}

/// 大きい定跡の出所。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BookInfo {
    name: String,
    bytes: u64,
    sha256: String,
}

/// 切り出しの出力先の `cut.json`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Meta {
    version: String,
    book: BookInfo,
    params: CutParams,
    roots: Vec<RootInfo>,
}

fn side(key: &str) -> &str {
    super::model::side(key)
}

fn turn(key: &str, own: &str) -> &'static str {
    if side(key) == own { "own" } else { "opp" }
}

/// TSV の見出しから列の位置を引く。
fn columns(header: &str, names: &[&str]) -> Result<Vec<usize>> {
    let cells: Vec<&str> = header.split('\t').collect();
    names
        .iter()
        .map(|name| {
            cells
                .iter()
                .position(|c| c == name)
                .with_context(|| format!("見出しに {name} がありません"))
        })
        .collect()
}

/// 切り出しの出力を読む。根の順に部分木を返す。
fn read_cut(dir: &Path) -> Result<(Meta, Vec<Subtree>)> {
    let meta: Meta = serde_json::from_str(
        &std::fs::read_to_string(dir.join(META))
            .with_context(|| format!("{} を読めません", dir.join(META).display()))?,
    )
    .context("cut.json の形式が不正です")?;
    ensure!(meta.version == META_VERSION, "cut.json の版が違います: {}", meta.version);
    let index: BTreeMap<&str, usize> =
        meta.roots.iter().enumerate().map(|(i, r)| (r.id.as_str(), i)).collect();
    let mut trees = vec![Subtree::default(); meta.roots.len()];
    let open =
        |name: &str, header: &str| -> Result<std::io::Lines<std::io::BufReader<std::fs::File>>> {
            let path = dir.join(name);
            let file = std::fs::File::open(&path)
                .with_context(|| format!("{} を読めません", path.display()))?;
            let mut lines = std::io::BufReader::new(file).lines();
            ensure!(
                lines.next().transpose()?.as_deref() == Some(header),
                "{} の見出しが違います",
                path.display()
            );
            Ok(lines)
        };
    let tree_of = |id: &str| index.get(id).copied().with_context(|| format!("根に無い id: {id}"));
    for line in open(NODES, NODES_HEADER)? {
        let line = line?;
        let c: Vec<&str> = line.split('\t').collect();
        ensure!(c.len() == 8, "{NODES} の列の数が違います: {line}");
        let node = Node {
            ply: c[2].parse()?,
            level: c[3].parse()?,
            kind: Kind::parse(c[6])?,
            read: c[7] == "1",
        };
        trees[tree_of(c[0])?].nodes.insert(c[1].to_owned(), node);
    }
    for line in open(EDGES, EDGES_HEADER)? {
        let line = line?;
        let c: Vec<&str> = line.split('\t').collect();
        ensure!(c.len() == 6, "{EDGES} の列の数が違います: {line}");
        trees[tree_of(c[0])?].edges.push(Edge {
            key: c[1].to_owned(),
            mv: c[2].to_owned(),
            child: c[3].to_owned(),
            book_value: if c[4].is_empty() {
                None
            } else {
                Some(c[4].parse()?)
            },
            extra: c[5] == "extra",
        });
    }
    for (root, tree) in meta.roots.iter().zip(&trees) {
        ensure!(tree.nodes.contains_key(&root.key), "{}: 根の局面がありません", root.id);
        for edge in &tree.edges {
            ensure!(
                tree.nodes.get(&edge.key).is_some_and(|n| n.kind == Kind::Inner)
                    && tree.nodes.contains_key(&edge.child),
                "{}: 辺の局面が部分木にありません: {} {}",
                root.id,
                edge.key,
                edge.mv
            );
        }
    }
    Ok((meta, trees))
}
