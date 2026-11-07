//! 注目の線から到達する元データと完了した木を合わせ、既存の後退解析で値を付ける。

use super::{export::files, master_index::MasterIndex, schema::Schema};
use crate::book_backprop::{BackpropOptions, DrawValues, MergeMode, backprop_file_with};
use crate::book_db::{
    BOOK_HEADER, BookMove, apply_usi_move, child_position_after_move, parse_move_line,
    position_from_sfen, strip_ply,
};
use anyhow::{Context, Result, ensure};
use clap::Args;
use rshogi_core::position::SFEN_HIRATE;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Args, Debug)]
pub(super) struct LineDbArgs {
    /// 定跡の元データ (TSV)
    #[arg(long)]
    master: PathBuf,
    /// 初期局面から根までの手順 (USI を空白区切り)
    #[arg(long)]
    moves: String,
    /// tree serve の出力の根。再帰的に探す (複数指定可)
    #[arg(long, num_args = 1.., required = true)]
    tree_out: Vec<PathBuf>,
    /// 出力する YaneuraOu 形式の定跡
    #[arg(long)]
    out: PathBuf,
    /// 語彙と読み条件の表を置いたディレクトリ
    #[arg(long)]
    schema: Option<PathBuf>,
    /// 先手を自分として解くときの千日手の値 (手番側から)
    #[arg(long, default_value_t = -70, allow_hyphen_values = true)]
    draw_value_black: i32,
    /// 後手を自分として解くときの千日手の値 (手番側から)
    #[arg(long, default_value_t = 70, allow_hyphen_values = true)]
    draw_value_white: i32,
    /// 既存の出力を置き換える
    #[arg(long)]
    force: bool,
}

struct TreeInput {
    dir: PathBuf,
    modified: SystemTime,
}

type Trees = BTreeMap<String, Vec<TreeInput>>;

fn discover(roots: &[PathBuf], inputs: &mut Vec<PathBuf>) -> Result<Trees> {
    let mut trees = Trees::new();
    let mut seen = BTreeSet::new();
    for root in roots {
        ensure!(root.is_dir(), "木の出力の根がディレクトリではありません: {}", root.display());
        for entry in walkdir::WalkDir::new(root).sort_by_file_name() {
            let entry = entry?;
            if !entry.file_type().is_dir() {
                continue;
            }
            let dir = entry.path().canonicalize()?;
            if !seen.insert(dir.clone()) {
                continue;
            }
            let status = dir.join("STATUS");
            inputs.push(status.clone());
            let text = match std::fs::read_to_string(&status) {
                Ok(text) => text,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    return Err(e).with_context(|| format!("{} を読めません", status.display()));
                }
            };
            let db = dir.join("tree.db");
            let d0 = dir.join("d0.jsonl");
            inputs.extend([db.clone(), d0.clone()]);
            if text.split_whitespace().next() != Some("DONE") {
                continue;
            }
            let mut line = String::new();
            BufReader::new(
                File::open(&d0).with_context(|| format!("{} を読めません", d0.display()))?,
            )
            .read_line(&mut line)?;
            #[derive(serde::Deserialize)]
            struct Root {
                key: String,
            }
            let root: Root = serde_json::from_str(&line)
                .with_context(|| format!("{} の根を読めません", d0.display()))?;
            let modified = db
                .metadata()
                .with_context(|| format!("{} を読めません", db.display()))?
                .modified()?;
            trees
                .entry(strip_ply(&root.key).to_owned())
                .or_default()
                .push(TreeInput { dir, modified });
        }
    }
    // 同じ時刻ならパス順で決め、走査順に値が左右されないようにする。
    for trees in trees.values_mut() {
        trees.sort_by(|a, b| a.modified.cmp(&b.modified).then(a.dir.cmp(&b.dir)));
    }
    Ok(trees)
}

struct SelectedMove {
    mv: BookMove,
    // None は元データ。木同士の競合は根に到達した順でなく更新時刻で解く。
    tree: Option<(SystemTime, PathBuf)>,
}

#[derive(Default)]
struct SelectedPosition {
    sfen: String,
    moves: BTreeMap<String, SelectedMove>,
}

type Positions = BTreeMap<String, SelectedPosition>;
/// 根より前の局面 (盤面キー) と、その局面の SFEN・指定された手。
type Prefix = BTreeMap<String, (String, BookMove)>;

fn insert_move(
    positions: &mut Positions,
    pending: &mut VecDeque<String>,
    prefix: &Prefix,
    sfen: &str,
    mv: BookMove,
    tree: Option<&TreeInput>,
) -> Result<()> {
    let Some(usi) = mv.move_usi.as_deref() else {
        return Ok(());
    };
    let key = strip_ply(sfen).to_owned();
    // 根より前の局面は指定の 1 手だけにするので、木に同じ局面があっても他の手の先は辿らない。
    if prefix.contains_key(&key) {
        return Ok(());
    }
    // 親局面が読めないのは入力の誤りなので止め、非合法手だけを捨てる。
    let mut child =
        position_from_sfen(sfen).with_context(|| format!("局面を読めません: {sfen}"))?;
    if apply_usi_move(&mut child, usi).is_err() {
        return Ok(());
    }
    pending.push_back(strip_ply(&child.to_sfen()).to_owned());
    pending.push_back(key.clone());
    let position = positions.entry(key).or_default();
    if position.sfen.is_empty() {
        position.sfen = sfen.to_owned();
    }
    let priority = tree.map(|t| (t.modified, t.dir.clone()));
    if let Some(old) = position.moves.get(usi) {
        match (&old.tree, &priority) {
            (None, _) => return Ok(()),
            (Some(old), Some(new)) if old >= new => return Ok(()),
            _ => {}
        }
    }
    position.moves.insert(usi.to_owned(), SelectedMove { mv, tree: priority });
    Ok(())
}

fn read_tree(
    tree: &TreeInput,
    positions: &mut Positions,
    pending: &mut VecDeque<String>,
    prefix: &Prefix,
) -> Result<()> {
    let path = tree.dir.join("tree.db");
    let mut sfen = String::new();
    for line in BufReader::new(File::open(&path)?).lines() {
        let line = line?;
        if let Some(rest) = line.strip_prefix("sfen ") {
            sfen = rest.trim().to_owned();
            pending.push_back(strip_ply(&sfen).to_owned());
        } else if !sfen.is_empty()
            && let Some(mv) = parse_move_line(&line)
        {
            insert_move(positions, pending, prefix, &sfen, mv, Some(tree))?;
        }
    }
    Ok(())
}

fn assemble(
    args: &LineDbArgs,
    index: &mut MasterIndex,
    mut trees: Trees,
) -> Result<(Positions, String, Vec<PathBuf>)> {
    let mut positions = Positions::new();
    let mut pending = VecDeque::new();
    let mut sfen = SFEN_HIRATE.to_owned();
    let mut prefix = Prefix::new();
    for usi in args.moves.split_whitespace() {
        let child = child_position_after_move(&sfen, usi)?;
        let mv = BookMove {
            move_usi: Some(usi.to_owned()),
            ponder_usi: None,
            value: 0,
            depth: 0,
            count: 1,
        };
        let key = strip_ply(&sfen).to_owned();
        ensure!(!prefix.contains_key(&key), "根までの手順に局面の繰り返しがあります: {key}");
        prefix.insert(key, (sfen, mv));
        sfen = child.to_sfen();
    }
    let root = strip_ply(&sfen).to_owned();
    ensure!(!prefix.contains_key(&root), "根までの手順で根の局面が繰り返しています");
    pending.push_back(root.clone());
    let mut seen = BTreeSet::new();
    let mut used = Vec::new();
    while let Some(key) = pending.pop_front() {
        if prefix.contains_key(&key) || !seen.insert(key.clone()) {
            continue;
        }
        for row in index.rows(&key)? {
            if !matches!(row.get("role"), "main" | "alt") {
                continue;
            }
            let value = row
                .get("evals")
                .split(',')
                .next()
                .filter(|v| !v.is_empty())
                .map(str::parse)
                .transpose()?
                .unwrap_or(0);
            let mv = BookMove {
                move_usi: Some(row.get("move").to_owned()),
                ponder_usi: None,
                value,
                depth: 0,
                count: if row.get("role") == "main" { 2 } else { 1 },
            };
            insert_move(
                &mut positions,
                &mut pending,
                &prefix,
                &format!("{key} {}", row.get("ply")),
                mv,
                None,
            )?;
        }
        for tree in trees.remove(&key).unwrap_or_default() {
            read_tree(&tree, &mut positions, &mut pending, &prefix)
                .with_context(|| format!("木を読めません: {}", tree.dir.display()))?;
            used.push(tree.dir);
        }
    }
    // 根より前は、木の中に同じ局面があっても指定された手順だけにする。
    for (key, (sfen, mv)) in prefix {
        let usi = mv.move_usi.clone().context("手順の手がありません")?;
        positions.insert(
            key,
            SelectedPosition {
                sfen,
                moves: BTreeMap::from([(usi, SelectedMove { mv, tree: None })]),
            },
        );
    }
    used.sort();
    Ok((positions, root, used))
}

fn write_raw(positions: &Positions, writer: &mut impl Write) -> Result<()> {
    writeln!(writer, "{BOOK_HEADER}")?;
    for position in positions.values().filter(|p| !p.moves.is_empty()) {
        writeln!(writer, "sfen {}", position.sfen)?;
        for (usi, selected) in &position.moves {
            let mv = &selected.mv;
            writeln!(
                writer,
                "{usi} {} {} {} {}",
                mv.ponder_usi.as_deref().unwrap_or("none"),
                mv.value,
                mv.depth,
                mv.count
            )?;
        }
    }
    Ok(())
}

pub(super) fn run(args: LineDbArgs) -> Result<()> {
    run_to(args, &mut std::io::stdout().lock())
}

fn run_to(args: LineDbArgs, stdout: &mut impl Write) -> Result<()> {
    let mut inputs = vec![args.master.clone()];
    if let Some(dir) = &args.schema {
        inputs.extend(super::schema::paths(dir));
    }
    let trees = discover(&args.tree_out, &mut inputs)?;
    files::guard(std::slice::from_ref(&args.out), &inputs, args.force)?;
    let mut index = MasterIndex::open(&args.master, &Schema::load(args.schema.as_deref())?)?;
    let (positions, root, used) = assemble(&args, &mut index, trees)?;
    let temp = tempfile::tempdir()?;
    let raw = temp.path().join("raw.db");
    let propagated = temp.path().join("propagated.db");
    {
        let mut writer = BufWriter::new(File::create(&raw)?);
        write_raw(&positions, &mut writer)?;
        writer.flush()?;
    }
    let n_positions = positions.len();
    let n_moves: usize = positions.values().map(|p| p.moves.len()).sum();
    drop(positions);
    backprop_file_with(
        &raw,
        &propagated,
        None,
        DrawValues {
            black: args.draw_value_black,
            white: args.draw_value_white,
        },
        MergeMode::Replace,
        BackpropOptions {
            skip_unusable_moves: true,
            ..BackpropOptions::default()
        },
    )?;
    let parent = args
        .out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    std::io::copy(&mut File::open(&propagated)?, staged.as_file_mut())?;
    staged.flush()?;
    files::guard(std::slice::from_ref(&args.out), &inputs, args.force)?;
    if args.force {
        staged.persist(&args.out)?;
    } else {
        staged.persist_noclobber(&args.out)?;
    }
    writeln!(stdout, "局面数\t{n_positions}\n手の数\t{n_moves}")?;
    for dir in used {
        writeln!(stdout, "木\t{}", dir.display())?;
    }
    let mut at_root = false;
    for line in BufReader::new(File::open(&args.out)?).lines() {
        let line = line?;
        if let Some(sfen) = line.strip_prefix("sfen ") {
            at_root = strip_ply(sfen) == root;
        } else if at_root && let Some(mv) = parse_move_line(&line) {
            let value = i64::from(mv.value)
                * if root.split_whitespace().nth(1) == Some("w") {
                    -1
                } else {
                    1
                };
            writeln!(stdout, "根\t{}\t{value}", mv.move_usi.as_deref().unwrap_or("none"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
