//! 定跡の展開ループ (YaneuraOu BookMiner 相当)。
//!
//! - `frontier`: book と開始局面から、次に掘る末端局面を列挙する
//! - `expand`: 末端局面を MultiPV 探索し、局面と候補手を book に追加する
//! - `run`: `frontier` → `expand` → 逆伝播 (`tools::book_backprop`) を周回する

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fmt::Write as FmtWrite;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write as IoWrite};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use rshogi_book::flip_usi_move;
use rshogi_core::movegen::{MoveList, generate_legal};
use rshogi_core::position::Position;
use rshogi_core::types::Color;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tools::book_backprop::{MergeMode, backprop_file};
use tools::book_db::{
    BookDb, BookMove, PositionEntry, apply_usi_move, canonical_key, child_position_after_move,
    parse_position_line, position_from_sfen, strip_ply,
};
use tools::common::io::write_atomic;
use tools::king_zone::{ENTERED_TIER, classify};
use tools::progress::MultiFileProgress;
use tools::selfplay::{EngineConfig, EngineProcess};

const MATE_CAP: i32 = 30_000;
const SEARCH_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);
/// `run` の逆伝播は `book_backprop` の既定値で呼ぶ。
const BACKPROP_DRAW_VALUE: i32 = 0;
const BACKPROP_MAX_ITERS: usize = 1000;

#[derive(Parser, Debug)]
#[command(
    about = "定跡の末端局面を列挙し、MultiPV 探索で展開して逆伝播を周回する (BookMiner 相当)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// book と開始局面から、次に掘る末端局面を列挙する
    Frontier(FrontierArgs),
    /// 末端局面を MultiPV 探索し、局面と候補手を book に追加する
    Expand(ExpandArgs),
    /// frontier → expand → 逆伝播 を周回する
    Run(RunArgs),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum SideArg {
    Black,
    White,
    Both,
}

#[derive(Args, Debug, Clone)]
struct FrontierOpts {
    /// 開始局面 (1 行 1 局面、`sfen ...` または `startpos moves ...`)
    #[arg(long)]
    roots: PathBuf,
    /// 採掘側。both は black 用と white 用を両方辿って和集合を取る
    #[arg(long, value_enum, default_value_t = SideArg::Both)]
    side: SideArg,
    /// 相手側局面で `value >= best - window` の手を辿る
    #[arg(long, default_value_t = 100)]
    window: i32,
    /// 相手側局面で count >= N の手も value に関係なく辿る。0 で無効
    #[arg(long, default_value_t = 0)]
    opp_min_count: u64,
    /// 採掘側局面で `value >= best - own_eps` の手を辿る
    #[arg(long, default_value_t = 0)]
    own_eps: i32,
    /// 局面の ply 上限
    #[arg(long, default_value_t = 260)]
    max_ply: u32,
    /// roots からの辿り手数の上限
    #[arg(long)]
    max_depth: Option<u32>,
    /// 出力件数上限 (roots からの手数昇順 → key 昇順で打ち切る)
    #[arg(long)]
    max_leaves: Option<usize>,
}

#[derive(Args, Debug)]
struct FrontierArgs {
    #[arg(long)]
    book: PathBuf,
    #[command(flatten)]
    opts: FrontierOpts,
    /// 末端局面リスト (`sfen <sfen>` を 1 行 1 局面)。入玉末端は `<out>.entered` にも出す
    #[arg(long)]
    out: PathBuf,
    #[arg(long)]
    report: Option<PathBuf>,
}

#[derive(Args, Debug, Clone)]
struct EngineOpts {
    #[arg(long)]
    engine: PathBuf,
    #[arg(long = "engine-option", num_args = 1)]
    engine_options: Vec<String>,
    #[arg(long, default_value = "nodes 100000")]
    go: String,
    #[arg(long, default_value_t = 1)]
    parallel: usize,
    /// 初回 MultiPV 数
    #[arg(long, default_value_t = 4)]
    multipv: usize,
    /// 1 位と K 位の差がこの値以内なら K を増やして再探索する。0 で無効
    #[arg(long, default_value_t = 100)]
    multipv_delta: i32,
    /// MultiPV 数の上限 (合法手数も上限)
    #[arg(long, default_value_t = 16)]
    multipv_max: usize,
    /// 追加した局面から最善手を N 手辿った局面も展開する
    #[arg(long, default_value_t = 0)]
    extend_ply: u32,
}

#[derive(Args, Debug)]
struct ExpandArgs {
    #[arg(long)]
    book: PathBuf,
    #[arg(long)]
    out: PathBuf,
    /// frontier の出力
    #[arg(long)]
    leaves: PathBuf,
    #[command(flatten)]
    engine: EngineOpts,
    #[arg(long)]
    journal: PathBuf,
    #[arg(long, default_value_t = false)]
    resume: bool,
    #[arg(long)]
    report: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct RunArgs {
    #[arg(long)]
    book: PathBuf,
    /// 最終周の book の書き出し先
    #[arg(long)]
    out: PathBuf,
    #[command(flatten)]
    frontier: FrontierOpts,
    #[command(flatten)]
    engine: EngineOpts,
    #[arg(long, default_value_t = 1)]
    iterations: usize,
    /// 累計追加局面数の上限
    #[arg(long)]
    max_new_positions: Option<usize>,
    #[arg(long, value_enum, default_value_t = MergeMode::Replace)]
    merge: MergeMode,
    /// 各周の成果物を `iter-XXX/` に保存するディレクトリ
    #[arg(long)]
    work_dir: PathBuf,
    /// `--work-dir` の完了済みの周の続きから再開する
    #[arg(long, default_value_t = false)]
    resume: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Frontier(args) => cmd_frontier(&args),
        Command::Expand(args) => cmd_expand(&args),
        Command::Run(args) => cmd_run(&args),
    }
}

// ---------------------------------------------------------------------------
// 共通
// ---------------------------------------------------------------------------

fn validate_frontier_opts(opts: &FrontierOpts) -> Result<()> {
    if opts.window < 0 {
        bail!("--window は 0 以上を指定してください");
    }
    if opts.own_eps < 0 {
        bail!("--own-eps は 0 以上を指定してください");
    }
    Ok(())
}

fn validate_engine_opts(opts: &EngineOpts) -> Result<()> {
    if opts.parallel == 0 {
        bail!("--parallel は 1 以上を指定してください");
    }
    if opts.multipv == 0 {
        bail!("--multipv は 1 以上を指定してください");
    }
    if opts.multipv_max < opts.multipv {
        bail!("--multipv-max は --multipv 以上を指定してください");
    }
    if opts.multipv_delta < 0 {
        bail!("--multipv-delta は 0 以上を指定してください");
    }
    Ok(())
}

/// 全ペアで正準パスの一致を拒否する。どの組が衝突しても入力か出力を破壊するため。
fn reject_path_collisions(paths: &[(&str, &Path)]) -> Result<()> {
    for (i, (a_name, a)) in paths.iter().enumerate() {
        for (b_name, b) in &paths[i + 1..] {
            let a_canon = canonicalize_output_collision_path(a)
                .with_context(|| format!("{a_name} の正準化に失敗しました: {}", a.display()))?;
            let b_canon = canonicalize_output_collision_path(b)
                .with_context(|| format!("{b_name} の正準化に失敗しました: {}", b.display()))?;
            if a_canon == b_canon {
                bail!("{a_name} と {b_name} が同じファイルを指しています: {}", a_canon.display());
            }
        }
    }
    Ok(())
}

fn canonicalize_output_collision_path(path: &Path) -> Result<PathBuf> {
    match std::fs::canonicalize(path) {
        Ok(path) => Ok(path),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let parent =
                path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
            let parent = std::fs::canonicalize(parent).with_context(|| {
                format!("親ディレクトリを正準化できません: {}", parent.display())
            })?;
            let file_name = path
                .file_name()
                .ok_or_else(|| anyhow!("ファイル名がありません: {}", path.display()))?;
            Ok(parent.join(file_name))
        }
        Err(err) => Err(err).with_context(|| format!("正準化できません: {}", path.display())),
    }
}

fn read_book_checked(path: &Path) -> Result<BookDb> {
    rshogi_book::Book::from_path(path, true)
        .with_context(|| format!("定跡を rshogi-book で読めません: {}", path.display()))?;
    BookDb::read(path)
}

/// 1 行 1 局面のファイルを読む。空行と `#` 行は無視する。
fn read_position_list(path: &Path) -> Result<Vec<String>> {
    let file = File::open(path).with_context(|| format!("開けません: {}", path.display()))?;
    let mut positions = Vec::new();
    for (line_no, line) in BufReader::new(file).lines().enumerate() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let sfen = parse_position_line(line)
            .with_context(|| format!("局面行が不正です: {}:{}", path.display(), line_no + 1))?;
        positions.push(sfen);
    }
    Ok(positions)
}

fn side_to_move(sfen: &str) -> Result<Color> {
    match sfen.split_whitespace().nth(1) {
        Some("b") => Ok(Color::Black),
        Some("w") => Ok(Color::White),
        _ => bail!("SFEN の手番が不正です: {sfen}"),
    }
}

fn legal_move_count(pos: &Position) -> usize {
    let mut list = MoveList::new();
    generate_legal(pos, &mut list);
    list.len()
}

fn mate_to_cp(mate: i32) -> i32 {
    let ply = mate.saturating_abs().min(MATE_CAP);
    if mate >= 0 {
        MATE_CAP - ply
    } else {
        -MATE_CAP + ply
    }
}

// ---------------------------------------------------------------------------
// frontier
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LeafKind {
    /// 辿った手の先が book 外。
    OutOfBook,
    /// book 内だが候補手の value が全て未設定。
    Unexplored,
}

#[derive(Debug, Clone)]
struct Leaf {
    sfen: String,
    depth: u32,
    kind: LeafKind,
    /// どちらかの玉が敵陣 3 段内にある。
    entered: bool,
}

#[derive(Debug, Default)]
struct FrontierStats {
    visited_nodes: usize,
    illegal_moves: BTreeSet<(String, String)>,
    pruned_ply: usize,
    pruned_depth: usize,
    leaves_before_limit: usize,
}

#[derive(Debug)]
struct FrontierResult {
    leaves: Vec<Leaf>,
    stats: FrontierStats,
}

fn compute_frontier(
    book: &BookDb,
    roots: &[String],
    opts: &FrontierOpts,
) -> Result<FrontierResult> {
    let sides: &[Color] = match opts.side {
        SideArg::Black => &[Color::Black],
        SideArg::White => &[Color::White],
        SideArg::Both => &[Color::Black, Color::White],
    };
    let mut leaves = BTreeMap::<String, Leaf>::new();
    let mut stats = FrontierStats::default();
    for &side in sides {
        traverse_side(book, roots, side, opts, &mut leaves, &mut stats)?;
    }
    for (sfen, move_usi) in &stats.illegal_moves {
        eprintln!("警告: 非合法な book 手を飛ばします: sfen={sfen} move={move_usi}");
    }
    let mut list: Vec<Leaf> = leaves.into_values().collect();
    list.sort_by(|a, b| {
        a.depth.cmp(&b.depth).then_with(|| strip_ply(&a.sfen).cmp(strip_ply(&b.sfen)))
    });
    stats.leaves_before_limit = list.len();
    if let Some(max) = opts.max_leaves {
        list.truncate(max);
    }
    Ok(FrontierResult {
        leaves: list,
        stats,
    })
}

/// 採掘側 `side` の視点で roots から BFS し、末端を `leaves` (正準 key → 末端) に加える。
fn traverse_side(
    book: &BookDb,
    roots: &[String],
    side: Color,
    opts: &FrontierOpts,
    leaves: &mut BTreeMap<String, Leaf>,
    stats: &mut FrontierStats,
) -> Result<()> {
    let mut visited = HashSet::<String>::new();
    let mut queue = VecDeque::<(String, u32)>::new();
    for root in roots {
        if visited.insert(canonical_key(root)) {
            queue.push_back((root.clone(), 0));
        }
    }

    while let Some((sfen, depth)) = queue.pop_front() {
        let Some(hit) = book.find(&sfen) else {
            add_leaf(leaves, &sfen, depth, LeafKind::OutOfBook)?;
            continue;
        };
        let entry = &book.entries[&hit.key];
        let best = entry
            .moves
            .iter()
            .filter(|m| m.move_usi.is_some() && m.is_labeled())
            .map(|m| m.value)
            .max();
        let Some(best) = best else {
            add_leaf(leaves, &sfen, depth, LeafKind::Unexplored)?;
            continue;
        };
        stats.visited_nodes += 1;

        let own = side_to_move(&sfen)? == side;
        let mut selected = BTreeSet::<String>::new();
        for book_move in &entry.moves {
            let Some(move_usi) = book_move.move_usi.as_deref() else {
                continue;
            };
            if !should_follow(book_move, best, own, opts) {
                continue;
            }
            // 反転 key でヒットした局面の手は反転座標系なので、実局面の座標へ戻す。
            let actual = if hit.flipped {
                flip_usi_move(move_usi)
            } else {
                Some(move_usi.to_string())
            };
            match actual {
                Some(actual) => {
                    selected.insert(actual);
                }
                None => {
                    stats.illegal_moves.insert((sfen.clone(), move_usi.to_string()));
                }
            }
        }

        let next_depth = depth + 1;
        for move_usi in selected {
            if opts.max_depth.is_some_and(|max| next_depth > max) {
                stats.pruned_depth += 1;
                continue;
            }
            let child = match child_position_after_move(&sfen, &move_usi) {
                Ok(child) => child,
                Err(_) => {
                    stats.illegal_moves.insert((sfen.clone(), move_usi));
                    continue;
                }
            };
            if i64::from(child.game_ply()) > i64::from(opts.max_ply) {
                stats.pruned_ply += 1;
                continue;
            }
            let child_sfen = child.to_sfen();
            if !visited.insert(canonical_key(&child_sfen)) {
                continue;
            }
            if book.find(&child_sfen).is_some() {
                queue.push_back((child_sfen, next_depth));
            } else {
                add_leaf(leaves, &child_sfen, next_depth, LeafKind::OutOfBook)?;
            }
        }
    }
    Ok(())
}

fn should_follow(book_move: &BookMove, best: i32, own: bool, opts: &FrontierOpts) -> bool {
    let labeled = book_move.is_labeled();
    if own {
        labeled && book_move.value >= best.saturating_sub(opts.own_eps)
    } else {
        (labeled && book_move.value >= best.saturating_sub(opts.window))
            || (opts.opp_min_count > 0 && book_move.count >= opts.opp_min_count)
    }
}

fn add_leaf(
    leaves: &mut BTreeMap<String, Leaf>,
    sfen: &str,
    depth: u32,
    kind: LeafKind,
) -> Result<()> {
    let key = canonical_key(sfen);
    if let Some(existing) = leaves.get(&key)
        && (existing.depth, strip_ply(&existing.sfen)) <= (depth, strip_ply(sfen))
    {
        return Ok(());
    }
    let pos = position_from_sfen(sfen)?;
    leaves.insert(
        key,
        Leaf {
            sfen: sfen.to_string(),
            depth,
            kind,
            entered: classify(&pos) == ENTERED_TIER,
        },
    );
    Ok(())
}

fn entered_path(out: &Path) -> PathBuf {
    let mut path = out.as_os_str().to_os_string();
    path.push(".entered");
    PathBuf::from(path)
}

fn write_leaves(out: &Path, leaves: &[Leaf]) -> Result<()> {
    let mut all = String::new();
    let mut entered = String::new();
    for leaf in leaves {
        writeln!(all, "sfen {}", leaf.sfen)?;
        if leaf.entered {
            writeln!(entered, "sfen {}", leaf.sfen)?;
        }
    }
    write_atomic(out, &all).with_context(|| format!("書けません: {}", out.display()))?;
    let entered_out = entered_path(out);
    write_atomic(&entered_out, &entered)
        .with_context(|| format!("書けません: {}", entered_out.display()))
}

fn frontier_report(
    result: &FrontierResult,
    opts: &FrontierOpts,
    root_count: usize,
) -> Result<String> {
    let leaves = &result.leaves;
    let stats = &result.stats;
    let count_kind = |kind| leaves.iter().filter(|l| l.kind == kind).count();
    let mut out = String::new();
    writeln!(out, "# book_mine frontier report\n")?;
    writeln!(out, "## summary\n")?;
    writeln!(out, "| metric | value |")?;
    writeln!(out, "|---|---:|")?;
    writeln!(out, "| roots | {root_count} |")?;
    writeln!(out, "| side | {:?} |", opts.side)?;
    writeln!(out, "| window | {} |", opts.window)?;
    writeln!(out, "| own_eps | {} |", opts.own_eps)?;
    writeln!(out, "| opp_min_count | {} |", opts.opp_min_count)?;
    writeln!(out, "| visited book nodes | {} |", stats.visited_nodes)?;
    writeln!(out, "| leaves (before max-leaves) | {} |", stats.leaves_before_limit)?;
    writeln!(out, "| leaves | {} |", leaves.len())?;
    writeln!(out, "| out-of-book leaves | {} |", count_kind(LeafKind::OutOfBook))?;
    writeln!(out, "| unexplored book leaves | {} |", count_kind(LeafKind::Unexplored))?;
    let entered = leaves.iter().filter(|l| l.entered).count();
    writeln!(out, "| entered-king leaves | {entered} |")?;
    writeln!(out, "| illegal book moves skipped | {} |", stats.illegal_moves.len())?;
    writeln!(out, "| pruned by max-ply | {} |", stats.pruned_ply)?;
    writeln!(out, "| pruned by max-depth | {} |", stats.pruned_depth)?;
    writeln!(out, "\n## depth distribution\n")?;
    writeln!(out, "| depth | leaves | entered |")?;
    writeln!(out, "|---:|---:|---:|")?;
    let mut by_depth = BTreeMap::<u32, (usize, usize)>::new();
    for leaf in leaves {
        let slot = by_depth.entry(leaf.depth).or_default();
        slot.0 += 1;
        slot.1 += usize::from(leaf.entered);
    }
    for (depth, (count, entered)) in by_depth {
        writeln!(out, "| {depth} | {count} | {entered} |")?;
    }
    Ok(out)
}

fn cmd_frontier(args: &FrontierArgs) -> Result<()> {
    validate_frontier_opts(&args.opts)?;
    let entered_out = entered_path(&args.out);
    let mut paths: Vec<(&str, &Path)> = vec![
        ("--book", args.book.as_path()),
        ("--roots", args.opts.roots.as_path()),
        ("--out", args.out.as_path()),
        ("<out>.entered", entered_out.as_path()),
    ];
    if let Some(report) = &args.report {
        paths.push(("--report", report.as_path()));
    }
    reject_path_collisions(&paths)?;

    let book = read_book_checked(&args.book)?;
    let roots = read_position_list(&args.opts.roots)?;
    let result = compute_frontier(&book, &roots, &args.opts)?;
    write_leaves(&args.out, &result.leaves)?;
    if let Some(report) = &args.report {
        write_atomic(report, &frontier_report(&result, &args.opts, roots.len())?)
            .with_context(|| format!("report を書けません: {}", report.display()))?;
    }
    eprintln!(
        "book_mine frontier: leaves={} (entered={}, before max-leaves={})",
        result.leaves.len(),
        result.leaves.iter().filter(|l| l.entered).count(),
        result.stats.leaves_before_limit
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// expand: 探索
// ---------------------------------------------------------------------------

/// MultiPV の 1 行。value は探索局面の手番側視点。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PvLine {
    #[serde(rename = "move")]
    move_usi: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ponder: Option<String>,
    value: i32,
    depth: i32,
    /// score が mate 表記だった。
    #[serde(default)]
    mate: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct JournalRecord {
    /// 探索局面の ply 抜き SFEN。
    key: String,
    /// 探索局面の SFEN (ply 付き)。
    sfen: String,
    go: String,
    multipv: String,
    engine_fingerprint: String,
    /// エンジンが `bestmove win` を返した (宣言勝ち可能局面)。
    #[serde(default)]
    declaration_win: bool,
    #[serde(default)]
    lines: Vec<PvLine>,
    /// 最後の探索の MultiPV 数。
    #[serde(default)]
    multipv_used: usize,
    /// MultiPV 数を増やして再探索した回数。
    #[serde(default)]
    extensions: u32,
}

/// 探索設定。journal の再利用条件 (`go` / `multipv` / `engine_fingerprint`) を含む。
#[derive(Debug)]
struct SearchSettings {
    engine: PathBuf,
    engine_options: Vec<String>,
    go: String,
    multipv: usize,
    multipv_delta: i32,
    multipv_max: usize,
    multipv_spec: String,
    fingerprint: String,
}

impl SearchSettings {
    fn new(opts: &EngineOpts) -> Result<Self> {
        Ok(Self {
            engine: opts.engine.clone(),
            engine_options: opts.engine_options.clone(),
            go: opts.go.clone(),
            multipv: opts.multipv,
            multipv_delta: opts.multipv_delta,
            multipv_max: opts.multipv_max,
            multipv_spec: format!(
                "multipv={} delta={} max={}",
                opts.multipv, opts.multipv_delta, opts.multipv_max
            ),
            fingerprint: engine_fingerprint(&opts.engine, &opts.engine_options)?,
        })
    }

    fn matches(&self, rec: &JournalRecord) -> bool {
        rec.go == self.go
            && rec.multipv == self.multipv_spec
            && rec.engine_fingerprint == self.fingerprint
    }
}

fn engine_fingerprint(engine_path: &Path, engine_options: &[String]) -> Result<String> {
    let engine_name =
        engine_path.file_name().map(|name| name.to_string_lossy()).unwrap_or_default();
    let engine_bytes = std::fs::read(engine_path)
        .with_context(|| format!("engine binary を読めません: {}", engine_path.display()))?;
    let engine_sha256 = Sha256::digest(&engine_bytes);
    let mut normalized_options: Vec<&str> = engine_options.iter().map(String::as_str).collect();
    normalized_options
        .sort_by(|a, b| engine_option_key(a).cmp(engine_option_key(b)).then_with(|| a.cmp(b)));
    Ok(format!(
        "{engine_name}\tsha256={engine_sha256:x}\t{}",
        normalized_options.join("\n")
    ))
}

fn engine_option_key(option: &str) -> &str {
    option.split_once('=').map_or(option, |(key, _)| key)
}

fn load_journal(path: &Path, settings: &SearchSettings) -> Result<HashMap<String, JournalRecord>> {
    let mut loaded = HashMap::new();
    if !path.exists() {
        return Ok(loaded);
    }
    let file =
        File::open(path).with_context(|| format!("journal を開けません: {}", path.display()))?;
    for (line_no, line) in BufReader::new(file).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let rec: JournalRecord = serde_json::from_str(&line).with_context(|| {
            format!("journal JSON が不正です: {}:{}", path.display(), line_no + 1)
        })?;
        if settings.matches(&rec) {
            loaded.insert(rec.key.clone(), rec);
        }
    }
    Ok(loaded)
}

/// info 行から (multipv 番号, 行) を取り出す。bound 付き・score/pv 欠落の行は無視する。
fn parse_multipv_info(line: &str) -> Option<(u32, PvLine)> {
    let tokens: Vec<&str> = line.split_whitespace().take_while(|t| *t != "string").collect();
    if tokens.first() != Some(&"info") {
        return None;
    }
    let mut multipv = 1u32;
    let mut depth = 0i32;
    let mut score: Option<(i32, bool)> = None;
    let mut bound = false;
    let mut pv: &[&str] = &[];
    let mut i = 1;
    while i < tokens.len() {
        match tokens[i] {
            "multipv" => {
                multipv = tokens.get(i + 1)?.parse().ok()?;
                i += 1;
            }
            "depth" => {
                depth = tokens.get(i + 1)?.parse().ok()?;
                i += 1;
            }
            "score" => {
                let value = tokens.get(i + 2)?;
                score = match *tokens.get(i + 1)? {
                    "cp" => Some((value.parse::<i32>().ok()?.clamp(-MATE_CAP, MATE_CAP), false)),
                    "mate" => Some((mate_to_cp(value.parse::<i32>().ok()?), true)),
                    _ => return None,
                };
                i += 2;
            }
            "lowerbound" | "upperbound" => bound = true,
            "pv" => {
                pv = &tokens[i + 1..];
                break;
            }
            _ => {}
        }
        i += 1;
    }
    if bound {
        return None;
    }
    let (value, mate) = score?;
    let first = *pv.first()?;
    Some((
        multipv,
        PvLine {
            move_usi: first.to_string(),
            ponder: pv.get(1).map(|s| s.to_string()),
            value,
            depth,
            mate,
        },
    ))
}

/// MultiPV 番号ごとに最後の確定行を保持する。
#[derive(Debug, Default)]
struct MultiPvCollector {
    lines: BTreeMap<u32, PvLine>,
}

impl MultiPvCollector {
    fn update(&mut self, line: &str) {
        if let Some((index, pv_line)) = parse_multipv_info(line) {
            self.lines.insert(index, pv_line);
        }
    }

    /// multipv 番号順。同じ手が複数行に現れた場合は番号の小さい行を残す。
    fn into_lines(self) -> Vec<PvLine> {
        let mut seen = HashSet::new();
        self.lines
            .into_values()
            .filter(|line| seen.insert(line.move_usi.clone()))
            .collect()
    }
}

#[derive(Debug, Clone)]
struct SearchTask {
    key: String,
    sfen: String,
    legal_moves: usize,
}

#[derive(Debug)]
enum WorkerMessage {
    Task(Result<JournalRecord>),
    Fatal(anyhow::Error),
}

fn search_multipv(
    engine: &mut EngineProcess,
    task: &SearchTask,
    settings: &SearchSettings,
) -> Result<JournalRecord> {
    let cap = settings.multipv_max.min(task.legal_moves).max(1);
    let mut k = settings.multipv.min(cap);
    let mut extensions = 0u32;
    loop {
        engine.set_option_if_available("MultiPV", &k.to_string())?;
        let mut collector = MultiPvCollector::default();
        let outcome = {
            let mut on_info = |line: &str| collector.update(line);
            let callback: &mut dyn FnMut(&str) = &mut on_info;
            engine.search_raw_go(&task.sfen, &settings.go, SEARCH_TIMEOUT, Some(callback))?
        };
        if outcome.timed_out {
            bail!("探索がタイムアウトしました: {}", task.key);
        }
        let bestmove = outcome
            .bestmove
            .ok_or_else(|| anyhow!("bestmove が得られませんでした: {}", task.key))?;
        let mut record = JournalRecord {
            key: task.key.clone(),
            sfen: task.sfen.clone(),
            go: settings.go.clone(),
            multipv: settings.multipv_spec.clone(),
            engine_fingerprint: settings.fingerprint.clone(),
            declaration_win: false,
            lines: Vec::new(),
            multipv_used: k,
            extensions,
        };
        if bestmove == "win" {
            record.declaration_win = true;
            return Ok(record);
        }
        let lines = collector.into_lines();
        if lines.is_empty() {
            bail!("MultiPV の info score/pv が得られませんでした: {}", task.key);
        }
        // BookMiner 準拠: K 位まで 1 位と僅差なら、まだ良い手が漏れている可能性があるので K を増やす。
        if settings.multipv_delta > 0
            && k < cap
            && lines.len() >= k
            && lines[0].value - lines[k - 1].value <= settings.multipv_delta
        {
            k = (k + settings.multipv).min(cap);
            extensions += 1;
            continue;
        }
        record.lines = lines;
        return Ok(record);
    }
}

fn worker_loop(
    worker_id: usize,
    settings: &SearchSettings,
    task_rx: crossbeam_channel::Receiver<SearchTask>,
    result_tx: crossbeam_channel::Sender<WorkerMessage>,
) {
    let cfg = EngineConfig {
        path: settings.engine.clone(),
        args: Vec::new(),
        threads: 1,
        hash_mb: 256,
        network_delay: None,
        network_delay2: None,
        minimum_thinking_time: None,
        slowmover: None,
        ponder: false,
        usi_options: settings.engine_options.clone(),
    };
    let mut engine = match EngineProcess::spawn(&cfg, format!("book_mine-{worker_id}")) {
        Ok(engine) => engine,
        Err(err) => {
            let _ = result_tx.send(WorkerMessage::Fatal(
                err.context(format!("worker {worker_id}: engine 起動に失敗しました")),
            ));
            return;
        }
    };
    for task in task_rx {
        let result = search_multipv(&mut engine, &task, settings)
            .with_context(|| format!("worker {worker_id}: 探索に失敗しました: {}", task.sfen));
        // 失敗後は応答の対応関係を保証できないため、この engine を再利用しない。
        let failed = result.is_err();
        if result_tx.send(WorkerMessage::Task(result)).is_err() || failed {
            break;
        }
    }
}

/// 常駐 USI エンジン群。`run` の周をまたいで使い回す。
struct EnginePool {
    task_tx: Option<crossbeam_channel::Sender<SearchTask>>,
    task_rx: crossbeam_channel::Receiver<SearchTask>,
    result_rx: crossbeam_channel::Receiver<WorkerMessage>,
    handles: Vec<JoinHandle<()>>,
}

impl EnginePool {
    fn spawn(parallel: usize, settings: &Arc<SearchSettings>) -> Self {
        let (task_tx, task_rx) = crossbeam_channel::unbounded::<SearchTask>();
        let (result_tx, result_rx) = crossbeam_channel::unbounded::<WorkerMessage>();
        let handles = (0..parallel)
            .map(|worker_id| {
                let task_rx = task_rx.clone();
                let result_tx = result_tx.clone();
                let settings = Arc::clone(settings);
                std::thread::spawn(move || worker_loop(worker_id, &settings, task_rx, result_tx))
            })
            .collect();
        Self {
            task_tx: Some(task_tx),
            task_rx,
            result_rx,
            handles,
        }
    }

    /// 全タスクを探索し、結果を journal へ追記して `journal` に反映する。
    ///
    /// 失敗時は未着手タスクを捨て、実行中のタスクの結果を journal に回収してからエラーを返す。
    fn run(
        &self,
        tasks: Vec<SearchTask>,
        journal_path: &Path,
        journal: &mut HashMap<String, JournalRecord>,
    ) -> Result<()> {
        let task_count = tasks.len();
        let task_tx =
            self.task_tx.as_ref().ok_or_else(|| anyhow!("内部エラー: pool は停止済み"))?;
        for task in tasks {
            task_tx.send(task)?;
        }
        let progress = MultiFileProgress::new(task_count as u64, 1, "book_mine");
        let file_progress = progress.start_file("expand", 1, task_count as u64);
        let outcome = (|| -> Result<()> {
            let file =
                OpenOptions::new().create(true).append(true).open(journal_path).with_context(
                    || format!("journal を追記オープンできません: {}", journal_path.display()),
                )?;
            let mut writer = BufWriter::new(file);
            let mut first_error: Option<anyhow::Error> = None;
            let mut outstanding = task_count;
            while outstanding > 0 {
                let Ok(message) = self.result_rx.recv() else {
                    break;
                };
                let error = match message {
                    WorkerMessage::Task(Ok(record)) => {
                        outstanding -= 1;
                        file_progress.inc(1);
                        serde_json::to_writer(&mut writer, &record)?;
                        writer.write_all(b"\n")?;
                        writer.flush()?;
                        journal.insert(record.key.clone(), record);
                        None
                    }
                    WorkerMessage::Task(Err(err)) => {
                        outstanding -= 1;
                        file_progress.inc(1);
                        Some(err)
                    }
                    WorkerMessage::Fatal(err) => Some(err),
                };
                if let Some(err) = error
                    && first_error.is_none()
                {
                    first_error = Some(err);
                    let drained = self.task_rx.try_iter().count();
                    outstanding = outstanding.saturating_sub(drained);
                }
            }
            if let Some(err) = first_error {
                return Err(err.context(format!(
                    "探索中にエラーが発生しました。journal には途中結果が追記済みのため --resume で再開できます: {}",
                    journal_path.display()
                )));
            }
            if outstanding > 0 {
                bail!(
                    "全 worker が終了し探索タスクが {outstanding}/{task_count} 件未完了です。--resume で再開できます: {}",
                    journal_path.display()
                );
            }
            Ok(())
        })();
        match &outcome {
            Ok(()) => file_progress.finish_with_message("完了"),
            Err(_) => file_progress.abandon_with_message("中断"),
        }
        progress.finish();
        outcome
    }
}

impl Drop for EnginePool {
    fn drop(&mut self) {
        self.task_tx.take();
        for handle in self.handles.drain(..) {
            let _ = handle.join();
        }
    }
}

// ---------------------------------------------------------------------------
// expand: book への反映
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Expanded {
    sfen: String,
    record: JournalRecord,
    /// `--extend-ply` で辿った局面。
    from_extend_ply: bool,
}

#[derive(Debug, Default)]
struct ExpandStats {
    leaves: usize,
    searched: usize,
    journal_reused: usize,
    new_positions: usize,
    extend_ply_positions: usize,
    positions_with_added_moves: usize,
    positions_without_new_moves: usize,
    added_moves: usize,
    multipv_extended_positions: usize,
    multipv_extensions: u64,
    mate_lines: usize,
    illegal_lines: usize,
    declarations: Vec<String>,
    no_legal_moves: Vec<String>,
}

/// 展開の実行文脈。journal と engine pool を `run` の周をまたいで保持する。
struct Expander<'a> {
    settings: Arc<SearchSettings>,
    parallel: usize,
    extend_ply: u32,
    journal_path: &'a Path,
    journal: HashMap<String, JournalRecord>,
    pool: Option<EnginePool>,
}

impl Expander<'_> {
    fn expand(&mut self, book: &BookDb, leaves: &[String]) -> Result<(BookDb, ExpandStats)> {
        let mut stats = ExpandStats {
            leaves: leaves.len(),
            ..ExpandStats::default()
        };
        let mut expanded = BTreeMap::<String, Expanded>::new();
        let mut round: Vec<String> = leaves.to_vec();
        for ply in 0..=self.extend_ply {
            let mut tasks = Vec::new();
            let mut items = Vec::<(String, String)>::new();
            let mut seen = HashSet::new();
            for sfen in &round {
                let canonical = canonical_key(sfen);
                if expanded.contains_key(&canonical) || !seen.insert(canonical.clone()) {
                    continue;
                }
                let legal_moves = legal_move_count(&position_from_sfen(sfen)?);
                if legal_moves == 0 {
                    stats.no_legal_moves.push(sfen.clone());
                    continue;
                }
                let key = strip_ply(sfen).to_string();
                if self.journal.contains_key(&key) {
                    stats.journal_reused += 1;
                } else {
                    stats.searched += 1;
                    tasks.push(SearchTask {
                        key,
                        sfen: sfen.clone(),
                        legal_moves,
                    });
                }
                items.push((canonical, sfen.clone()));
            }
            if !tasks.is_empty() {
                let parallel = self.parallel;
                let settings = &self.settings;
                let pool = self.pool.get_or_insert_with(|| EnginePool::spawn(parallel, settings));
                pool.run(tasks, self.journal_path, &mut self.journal)?;
            }

            let mut next = Vec::new();
            for (canonical, sfen) in items {
                let record =
                    self.journal.get(strip_ply(&sfen)).cloned().ok_or_else(|| {
                        anyhow!("内部エラー: journal に探索結果がありません: {sfen}")
                    })?;
                if ply < self.extend_ply
                    && let Some(best) = record.lines.first()
                    && let Ok(child) = child_position_after_move(&sfen, &best.move_usi)
                {
                    let child_sfen = child.to_sfen();
                    if book.find(&child_sfen).is_none() {
                        next.push(child_sfen);
                    }
                }
                expanded.insert(
                    canonical,
                    Expanded {
                        sfen,
                        record,
                        from_extend_ply: ply > 0,
                    },
                );
            }
            if next.is_empty() {
                break;
            }
            round = next;
        }
        let out = apply_expansions(book, &expanded, &mut stats);
        Ok((out, stats))
    }
}

/// 合法な指し手と、子局面で合法な場合だけの ponder を返す。
fn validated_move(
    parent_sfen: &str,
    move_usi: &str,
    ponder: Option<&str>,
) -> Option<(String, Option<String>)> {
    let mut child = child_position_after_move(parent_sfen, move_usi).ok()?;
    let ponder = match ponder {
        Some(ponder) if apply_usi_move(&mut child, ponder).is_ok() => Some(ponder.to_string()),
        _ => None,
    };
    Some((move_usi.to_string(), ponder))
}

/// 探索結果を book に反映する。既存局面の既存手は変更せず、新しい手だけを `count=0` で追加する。
fn apply_expansions(
    book: &BookDb,
    expanded: &BTreeMap<String, Expanded>,
    stats: &mut ExpandStats,
) -> BookDb {
    let mut out = book.clone();
    for item in expanded.values() {
        let record = &item.record;
        if record.extensions > 0 {
            stats.multipv_extended_positions += 1;
            stats.multipv_extensions += u64::from(record.extensions);
        }
        stats.mate_lines += record.lines.iter().filter(|line| line.mate).count();
        if record.declaration_win {
            stats.declarations.push(item.sfen.clone());
            continue;
        }
        match out.find(&item.sfen) {
            Some(hit) => {
                let Some(entry) = out.entries.get_mut(&hit.key) else {
                    continue;
                };
                let mut added = 0;
                for line in &record.lines {
                    // 反転 key でヒットしたエントリへは反転座標系で書く。
                    let (move_usi, ponder) = if hit.flipped {
                        match flip_usi_move(&line.move_usi) {
                            Some(m) => (m, line.ponder.as_deref().and_then(flip_usi_move)),
                            None => {
                                stats.illegal_lines += 1;
                                continue;
                            }
                        }
                    } else {
                        (line.move_usi.clone(), line.ponder.clone())
                    };
                    if entry.moves.iter().any(|m| m.move_usi.as_deref() == Some(move_usi.as_str()))
                    {
                        continue;
                    }
                    let Some((move_usi, ponder)) =
                        validated_move(&entry.sfen, &move_usi, ponder.as_deref())
                    else {
                        stats.illegal_lines += 1;
                        continue;
                    };
                    entry.moves.push(BookMove {
                        move_usi: Some(move_usi),
                        ponder_usi: ponder,
                        value: line.value,
                        depth: line.depth,
                        count: 0,
                    });
                    added += 1;
                }
                stats.added_moves += added;
                if added > 0 {
                    stats.positions_with_added_moves += 1;
                } else {
                    stats.positions_without_new_moves += 1;
                }
            }
            None => {
                let mut moves = Vec::new();
                for line in &record.lines {
                    match validated_move(&item.sfen, &line.move_usi, line.ponder.as_deref()) {
                        Some((move_usi, ponder)) => moves.push(BookMove {
                            move_usi: Some(move_usi),
                            ponder_usi: ponder,
                            value: line.value,
                            depth: line.depth,
                            count: 0,
                        }),
                        None => stats.illegal_lines += 1,
                    }
                }
                if moves.is_empty() {
                    continue;
                }
                stats.added_moves += moves.len();
                stats.new_positions += 1;
                if item.from_extend_ply {
                    stats.extend_ply_positions += 1;
                }
                out.entries.insert(
                    strip_ply(&item.sfen).to_string(),
                    PositionEntry {
                        sfen: item.sfen.clone(),
                        moves,
                    },
                );
            }
        }
    }
    out
}

fn expand_report(stats: &ExpandStats, settings: &SearchSettings) -> Result<String> {
    let mut out = String::new();
    writeln!(out, "# book_mine expand report\n")?;
    writeln!(out, "## summary\n")?;
    writeln!(out, "| metric | value |")?;
    writeln!(out, "|---|---:|")?;
    writeln!(out, "| go | `{}` |", settings.go)?;
    writeln!(out, "| multipv | `{}` |", settings.multipv_spec)?;
    writeln!(out, "| leaves | {} |", stats.leaves)?;
    writeln!(out, "| searched positions | {} |", stats.searched)?;
    writeln!(out, "| journal reused positions | {} |", stats.journal_reused)?;
    writeln!(out, "| new positions | {} |", stats.new_positions)?;
    writeln!(out, "| new positions via extend-ply | {} |", stats.extend_ply_positions)?;
    writeln!(
        out,
        "| existing positions with added moves | {} |",
        stats.positions_with_added_moves
    )?;
    writeln!(
        out,
        "| existing positions without new moves | {} |",
        stats.positions_without_new_moves
    )?;
    writeln!(out, "| added moves | {} |", stats.added_moves)?;
    writeln!(out, "| multipv extended positions | {} |", stats.multipv_extended_positions)?;
    writeln!(out, "| multipv extensions | {} |", stats.multipv_extensions)?;
    writeln!(out, "| mate lines | {} |", stats.mate_lines)?;
    writeln!(out, "| illegal engine moves skipped | {} |", stats.illegal_lines)?;
    writeln!(out, "| declaration-win positions | {} |", stats.declarations.len())?;
    writeln!(out, "| no-legal-move positions | {} |", stats.no_legal_moves.len())?;
    writeln!(out, "\n## declaration-win positions\n")?;
    writeln!(out, "宣言勝ち可能局面は候補手を追加しない (値は mate 1 相当、手番側視点)。\n")?;
    writeln!(out, "| value | sfen |")?;
    writeln!(out, "|---:|---|")?;
    for sfen in &stats.declarations {
        writeln!(out, "| {} | `{sfen}` |", mate_to_cp(1))?;
    }
    if !stats.no_legal_moves.is_empty() {
        writeln!(out, "\n## no-legal-move positions\n")?;
        for sfen in &stats.no_legal_moves {
            writeln!(out, "- `{sfen}`")?;
        }
    }
    Ok(out)
}

fn cmd_expand(args: &ExpandArgs) -> Result<()> {
    validate_engine_opts(&args.engine)?;
    let mut paths: Vec<(&str, &Path)> = vec![
        ("--book", args.book.as_path()),
        ("--out", args.out.as_path()),
        ("--leaves", args.leaves.as_path()),
        ("--journal", args.journal.as_path()),
    ];
    if let Some(report) = &args.report {
        paths.push(("--report", report.as_path()));
    }
    reject_path_collisions(&paths)?;

    let book = read_book_checked(&args.book)?;
    let leaves = read_position_list(&args.leaves)?;
    let settings = Arc::new(SearchSettings::new(&args.engine)?);
    let journal = if args.resume {
        load_journal(&args.journal, &settings)?
    } else {
        HashMap::new()
    };
    let mut expander = Expander {
        settings: Arc::clone(&settings),
        parallel: args.engine.parallel,
        extend_ply: args.engine.extend_ply,
        journal_path: &args.journal,
        journal,
        pool: None,
    };
    let (out, stats) = expander.expand(&book, &leaves)?;
    out.write(&args.out)?;
    if let Some(report) = &args.report {
        write_atomic(report, &expand_report(&stats, &settings)?)
            .with_context(|| format!("report を書けません: {}", report.display()))?;
    }
    eprintln!(
        "book_mine expand: new_positions={} added_moves={} declarations={}",
        stats.new_positions,
        stats.added_moves,
        stats.declarations.len()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// run
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct IterationSummary {
    iteration: usize,
    leaves: usize,
    new_positions: usize,
    added_moves: usize,
}

impl IterationSummary {
    /// 次の周が同じ結果になる (掘る末端が無い、または何も追加されなかった)。
    fn converged(&self) -> bool {
        self.leaves == 0 || (self.new_positions == 0 && self.added_moves == 0)
    }
}

fn iter_dir(work_dir: &Path, iteration: usize) -> PathBuf {
    work_dir.join(format!("iter-{iteration:03}"))
}

/// `iter-001` から連続する完了済みの周 (`summary.json` と `book.db` がある) を返す。
fn completed_iterations(work_dir: &Path) -> Result<Vec<IterationSummary>> {
    let mut done = Vec::new();
    for iteration in 1.. {
        let dir = iter_dir(work_dir, iteration);
        let summary_path = dir.join("summary.json");
        if !summary_path.exists() || !dir.join("book.db").exists() {
            break;
        }
        let text = std::fs::read_to_string(&summary_path)
            .with_context(|| format!("読めません: {}", summary_path.display()))?;
        let summary: IterationSummary = serde_json::from_str(&text)
            .with_context(|| format!("summary.json が不正です: {}", summary_path.display()))?;
        done.push(summary);
    }
    Ok(done)
}

fn has_previous_run(work_dir: &Path, journal_path: &Path) -> Result<bool> {
    if journal_path.exists() {
        return Ok(true);
    }
    for entry in std::fs::read_dir(work_dir)
        .with_context(|| format!("読めません: {}", work_dir.display()))?
    {
        if entry?.file_name().to_string_lossy().starts_with("iter-") {
            return Ok(true);
        }
    }
    Ok(false)
}

fn cmd_run(args: &RunArgs) -> Result<()> {
    validate_frontier_opts(&args.frontier)?;
    validate_engine_opts(&args.engine)?;
    if args.iterations == 0 {
        bail!("--iterations は 1 以上を指定してください");
    }
    reject_path_collisions(&[
        ("--book", args.book.as_path()),
        ("--out", args.out.as_path()),
        ("--roots", args.frontier.roots.as_path()),
    ])?;
    std::fs::create_dir_all(&args.work_dir)
        .with_context(|| format!("作成できません: {}", args.work_dir.display()))?;
    let journal_path = args.work_dir.join("journal.jsonl");
    if !args.resume && has_previous_run(&args.work_dir, &journal_path)? {
        bail!(
            "--work-dir に前回の実行結果があります。続きから再開するには --resume を付けてください: {}",
            args.work_dir.display()
        );
    }

    let completed = completed_iterations(&args.work_dir)?;
    read_book_checked(&args.book)?;
    let roots = read_position_list(&args.frontier.roots)?;
    let settings = Arc::new(SearchSettings::new(&args.engine)?);
    let mut expander = Expander {
        journal: load_journal(&journal_path, &settings)?,
        settings: Arc::clone(&settings),
        parallel: args.engine.parallel,
        extend_ply: args.engine.extend_ply,
        journal_path: &journal_path,
        pool: None,
    };

    let mut cumulative: usize = completed.iter().map(|s| s.new_positions).sum();
    let mut current_book = match completed.last() {
        Some(last) => iter_dir(&args.work_dir, last.iteration).join("book.db"),
        None => args.book.clone(),
    };
    let mut converged = completed.last().is_some_and(IterationSummary::converged);
    if !completed.is_empty() {
        eprintln!("book_mine run: 完了済みの {} 周から再開します", completed.len());
    }

    for iteration in completed.len() + 1..=args.iterations {
        if converged {
            eprintln!("book_mine run: 追加が無くなったため周回を終了します");
            break;
        }
        let mut frontier_opts = args.frontier.clone();
        if let Some(max) = args.max_new_positions {
            if cumulative >= max {
                eprintln!(
                    "book_mine run: 累計追加局面数が --max-new-positions ({max}) に達しました"
                );
                break;
            }
            // 残り予算で末端数を打ち切る。--extend-ply で辿った局面の分は超過しうる。
            let remaining = max - cumulative;
            frontier_opts.max_leaves =
                Some(frontier_opts.max_leaves.map_or(remaining, |m| m.min(remaining)));
        }

        let dir = iter_dir(&args.work_dir, iteration);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("作成できません: {}", dir.display()))?;
        let book = BookDb::read(&current_book)?;
        let frontier = compute_frontier(&book, &roots, &frontier_opts)?;
        write_leaves(&dir.join("leaves.txt"), &frontier.leaves)?;
        write_atomic(
            &dir.join("frontier.md"),
            &frontier_report(&frontier, &frontier_opts, roots.len())?,
        )?;

        let leaf_sfens: Vec<String> = frontier.leaves.iter().map(|l| l.sfen.clone()).collect();
        let (expanded, stats) = expander.expand(&book, &leaf_sfens)?;
        let expanded_path = dir.join("expanded.db");
        expanded.write(&expanded_path)?;
        write_atomic(&dir.join("expand.md"), &expand_report(&stats, &settings)?)?;

        let book_path = dir.join("book.db");
        backprop_file(
            &expanded_path,
            &book_path,
            Some(&dir.join("backprop.md")),
            BACKPROP_DRAW_VALUE,
            BACKPROP_MAX_ITERS,
            args.merge,
        )?;

        let summary = IterationSummary {
            iteration,
            leaves: frontier.leaves.len(),
            new_positions: stats.new_positions,
            added_moves: stats.added_moves,
        };
        // summary.json は周の完了印なので最後に書く。
        write_atomic(&dir.join("summary.json"), &(serde_json::to_string_pretty(&summary)? + "\n"))?;
        cumulative += summary.new_positions;
        converged = summary.converged();
        current_book = book_path;
        eprintln!(
            "book_mine run: iter {iteration}: leaves={} new_positions={} added_moves={} (累計追加局面 {cumulative})",
            summary.leaves, summary.new_positions, summary.added_moves
        );
    }

    let text = std::fs::read_to_string(&current_book)
        .with_context(|| format!("読めません: {}", current_book.display()))?;
    write_atomic(&args.out, &text)
        .with_context(|| format!("書けません: {}", args.out.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const START: &str = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
    const KINGS: &str = "4k4/9/9/9/9/9/9/9/4K4 b - 1";

    type FixtureMove<'a> = (&'a str, i32, i32, u64);
    type FixtureEntry<'a> = (&'a str, &'a [FixtureMove<'a>]);

    fn after(start: &str, moves: &[&str]) -> String {
        let mut sfen = start.to_string();
        for move_usi in moves {
            sfen = child_position_after_move(&sfen, move_usi).unwrap().to_sfen();
        }
        sfen
    }

    fn flipped(sfen: &str) -> String {
        rshogi_book::flipped_key(sfen).unwrap()
    }

    fn book_text(entries: &[FixtureEntry<'_>]) -> String {
        let mut out = String::from(tools::book_db::BOOK_HEADER);
        out.push('\n');
        for (sfen, moves) in entries {
            out.push_str(&format!("sfen {sfen}\n"));
            for (move_usi, value, depth, count) in *moves {
                out.push_str(&format!("{move_usi} none {value} {depth} {count}\n"));
            }
        }
        out
    }

    fn book(entries: &[FixtureEntry<'_>]) -> BookDb {
        BookDb::from_reader(book_text(entries).as_bytes()).unwrap()
    }

    fn opts(side: SideArg) -> FrontierOpts {
        FrontierOpts {
            roots: PathBuf::new(),
            side,
            window: 100,
            opp_min_count: 0,
            own_eps: 0,
            max_ply: 260,
            max_depth: None,
            max_leaves: None,
        }
    }

    fn frontier(book: &BookDb, roots: &[&str], opts: &FrontierOpts) -> FrontierResult {
        let roots: Vec<String> = roots.iter().map(|s| s.to_string()).collect();
        compute_frontier(book, &roots, opts).unwrap()
    }

    fn leaf_set(result: &FrontierResult) -> BTreeSet<String> {
        result.leaves.iter().map(|l| l.sfen.clone()).collect()
    }

    fn set<const N: usize>(items: [String; N]) -> BTreeSet<String> {
        items.into_iter().collect()
    }

    #[test]
    fn frontier_own_side_follows_best_only_with_ties_and_own_eps() {
        let db = book(&[(
            START,
            &[
                ("7g7f", 50, 10, 1),
                ("5i5h", 50, 10, 1),
                ("2g2f", 30, 10, 9),
            ],
        )]);
        let a = after(START, &["7g7f"]);
        let b = after(START, &["5i5h"]);
        let c = after(START, &["2g2f"]);

        let result = frontier(&db, &[START], &opts(SideArg::Black));
        assert_eq!(leaf_set(&result), set([a.clone(), b.clone()]));
        assert!(result.leaves.iter().all(|l| l.depth == 1 && l.kind == LeafKind::OutOfBook));

        let result = frontier(
            &db,
            &[START],
            &FrontierOpts {
                own_eps: 20,
                ..opts(SideArg::Black)
            },
        );
        assert_eq!(leaf_set(&result), set([a, b, c]));
    }

    #[test]
    fn frontier_opponent_side_uses_window() {
        let db = book(&[(
            START,
            &[
                ("7g7f", 50, 10, 1),
                ("2g2f", -30, 10, 1),
                ("5i5h", -60, 10, 1),
            ],
        )]);
        let a = after(START, &["7g7f"]);
        let b = after(START, &["2g2f"]);

        let result = frontier(&db, &[START], &opts(SideArg::White));
        assert_eq!(leaf_set(&result), set([a.clone(), b]));

        let result = frontier(
            &db,
            &[START],
            &FrontierOpts {
                window: 20,
                ..opts(SideArg::White)
            },
        );
        assert_eq!(leaf_set(&result), set([a]));
    }

    #[test]
    fn frontier_opp_min_count_follows_frequent_moves_regardless_of_value() {
        let db = book(&[(
            START,
            &[
                ("7g7f", 50, 10, 1),
                ("2g2f", -300, 10, 10),
                ("5i5h", 0, 0, 7),
                ("1g1f", -300, 10, 2),
            ],
        )]);
        let base = FrontierOpts {
            window: 0,
            ..opts(SideArg::White)
        };

        let result = frontier(&db, &[START], &base);
        assert_eq!(leaf_set(&result), set([after(START, &["7g7f"])]));

        let result = frontier(
            &db,
            &[START],
            &FrontierOpts {
                opp_min_count: 5,
                ..base
            },
        );
        assert_eq!(
            leaf_set(&result),
            set([
                after(START, &["7g7f"]),
                after(START, &["2g2f"]),
                after(START, &["5i5h"])
            ])
        );
    }

    #[test]
    fn frontier_follows_book_position_found_by_flipped_key() {
        let child = after(START, &["7g7f"]);
        // 反転局面では後手の 3c3d が先手の 7g7f になる。
        let db = book(&[
            (START, &[("7g7f", 50, 10, 1)]),
            (&flipped(&child), &[("7g7f", 20, 10, 1)]),
        ]);

        let result = frontier(&db, &[START], &opts(SideArg::Black));

        assert_eq!(leaf_set(&result), set([after(START, &["7g7f", "3c3d"])]));
        assert_eq!(result.leaves[0].depth, 2);
    }

    #[test]
    fn frontier_terminates_on_cycles() {
        let a = KINGS.to_string();
        let b = after(&a, &["5i5h"]);
        let c = after(&a, &["5i5h", "5a5b"]);
        let d = after(&a, &["5i5h", "5a5b", "5h5i"]);
        for (exit_value, expected) in [(-500, 0usize), (20, 1)] {
            let db = book(&[
                (&a, &[("5i5h", 0, 1, 3)]),
                (&b, &[("5a5b", 0, 1, 3), ("5a4a", exit_value, 1, 2)]),
                (&c, &[("5h5i", 0, 1, 3)]),
                (&d, &[("5b5a", 0, 1, 3)]),
            ]);
            let result = frontier(&db, &[&a], &opts(SideArg::Both));
            assert_eq!(result.leaves.len(), expected, "exit={exit_value}");
            if expected == 1 {
                assert_eq!(result.leaves[0].sfen, after(&a, &["5i5h", "5a4a"]));
            }
        }
    }

    #[test]
    fn frontier_respects_max_ply_and_max_depth() {
        let c1 = after(START, &["7g7f"]);
        let leaf = after(START, &["7g7f", "3c3d"]);
        let db = book(&[
            (START, &[("7g7f", 50, 10, 1)]),
            (&c1, &[("3c3d", 10, 10, 1)]),
        ]);

        let result = frontier(
            &db,
            &[START],
            &FrontierOpts {
                max_ply: 3,
                ..opts(SideArg::Both)
            },
        );
        assert_eq!(leaf_set(&result), set([leaf.clone()]));
        let result = frontier(
            &db,
            &[START],
            &FrontierOpts {
                max_ply: 2,
                ..opts(SideArg::Both)
            },
        );
        assert!(result.leaves.is_empty());
        assert!(result.stats.pruned_ply > 0);

        let result = frontier(
            &db,
            &[START],
            &FrontierOpts {
                max_depth: Some(2),
                ..opts(SideArg::Both)
            },
        );
        assert_eq!(leaf_set(&result), set([leaf]));
        let result = frontier(
            &db,
            &[START],
            &FrontierOpts {
                max_depth: Some(1),
                ..opts(SideArg::Both)
            },
        );
        assert!(result.leaves.is_empty());
        assert!(result.stats.pruned_depth > 0);
    }

    #[test]
    fn frontier_max_leaves_keeps_shallowest_then_key_order() {
        let c1 = after(START, &["7g7f"]);
        let db = book(&[
            (
                START,
                &[
                    ("7g7f", 50, 10, 1),
                    ("2g2f", 50, 10, 1),
                    ("5i5h", 50, 10, 1),
                ],
            ),
            (&c1, &[("3c3d", 10, 10, 1)]),
        ]);
        let mut shallow = [after(START, &["2g2f"]), after(START, &["5i5h"])];
        shallow.sort_by(|a, b| strip_ply(a).cmp(strip_ply(b)));
        let deep = after(START, &["7g7f", "3c3d"]);

        let result = frontier(&db, &[START], &opts(SideArg::Black));
        let all: Vec<String> = result.leaves.iter().map(|l| l.sfen.clone()).collect();
        assert_eq!(all, vec![shallow[0].clone(), shallow[1].clone(), deep]);

        let result = frontier(
            &db,
            &[START],
            &FrontierOpts {
                max_leaves: Some(1),
                ..opts(SideArg::Black)
            },
        );
        assert_eq!(result.leaves.len(), 1);
        assert_eq!(result.leaves[0].sfen, shallow[0]);
        assert_eq!(result.stats.leaves_before_limit, 3);
    }

    #[test]
    fn frontier_treats_unlabeled_book_position_as_leaf() {
        let db = book(&[(START, &[("7g7f", 0, 0, 5)])]);
        let result = frontier(&db, &[START], &opts(SideArg::Both));
        assert_eq!(result.leaves.len(), 1);
        assert_eq!(result.leaves[0].sfen, START);
        assert_eq!(result.leaves[0].depth, 0);
        assert_eq!(result.leaves[0].kind, LeafKind::Unexplored);
    }

    #[test]
    fn frontier_skips_illegal_book_moves() {
        let db = book(&[(START, &[("9a9b", 100, 10, 1), ("7g7f", 50, 10, 1)])]);
        let result = frontier(&db, &[START], &opts(SideArg::Black));
        assert!(result.leaves.is_empty());
        assert_eq!(result.stats.illegal_moves.len(), 1);
    }

    #[test]
    fn frontier_command_writes_leaves_entered_file_and_report() {
        let dir = tempfile::tempdir().unwrap();
        let book_path = dir.path().join("book.db");
        std::fs::write(&book_path, book_text(&[])).unwrap();
        let entered_root = "4k4/9/4K4/9/9/9/9/9/9 b - 1";
        let roots_path = dir.path().join("roots.txt");
        std::fs::write(&roots_path, format!("startpos\n# comment\nsfen {entered_root}\n")).unwrap();
        let out = dir.path().join("leaves.txt");
        let report = dir.path().join("frontier.md");

        cmd_frontier(&FrontierArgs {
            book: book_path,
            opts: FrontierOpts {
                roots: roots_path,
                ..opts(SideArg::Both)
            },
            out: out.clone(),
            report: Some(report.clone()),
        })
        .unwrap();

        let leaves = std::fs::read_to_string(&out).unwrap();
        assert_eq!(leaves.lines().count(), 2);
        assert!(leaves.contains(&format!("sfen {START}\n")));
        let entered = std::fs::read_to_string(entered_path(&out)).unwrap();
        assert_eq!(entered, format!("sfen {entered_root}\n"));
        let report = std::fs::read_to_string(report).unwrap();
        assert!(report.contains("| entered-king leaves | 1 |"));
    }

    #[test]
    fn multipv_info_parsing_skips_bounds_and_converts_mate() {
        let (idx, line) = parse_multipv_info(
            "info depth 12 seldepth 20 multipv 3 score cp 45 nodes 100 pv 7g7f 3c3d 2g2f",
        )
        .unwrap();
        assert_eq!(idx, 3);
        assert_eq!(line.move_usi, "7g7f");
        assert_eq!(line.ponder.as_deref(), Some("3c3d"));
        assert_eq!((line.value, line.depth, line.mate), (45, 12, false));

        let (idx, line) = parse_multipv_info("info depth 5 score mate -3 pv 5a4a").unwrap();
        assert_eq!(idx, 1);
        assert_eq!((line.value, line.ponder.clone(), line.mate), (-29_997, None, true));

        assert!(
            parse_multipv_info("info depth 9 multipv 1 score cp 50 lowerbound pv 2g2f").is_none()
        );
        assert!(parse_multipv_info("info depth 9 nodes 100").is_none());
        assert!(parse_multipv_info("info string multipv 1 score cp 1 pv 2g2f").is_none());
        let (_, line) = parse_multipv_info("info depth 3 score cp 99999 pv 2g2f").unwrap();
        assert_eq!(line.value, MATE_CAP);
    }

    #[test]
    fn collector_keeps_last_exact_line_per_index_and_dedups_moves() {
        let mut collector = MultiPvCollector::default();
        for line in [
            "info depth 1 multipv 1 score cp 10 pv 7g7f",
            "info depth 1 multipv 2 score cp 5 pv 2g2f",
            "info depth 2 multipv 1 score cp 30 upperbound pv 7g7f",
            "info depth 2 multipv 1 score cp 20 pv 2g2f",
            "info depth 2 multipv 2 score cp 15 pv 7g7f",
        ] {
            collector.update(line);
        }
        let lines = collector.into_lines();
        assert_eq!(lines.len(), 2);
        assert_eq!((lines[0].move_usi.as_str(), lines[0].value), ("2g2f", 20));
        assert_eq!((lines[1].move_usi.as_str(), lines[1].value), ("7g7f", 15));
    }

    #[cfg(unix)]
    mod engine {
        use super::*;
        use std::os::unix::fs::PermissionsExt as _;

        type Response<'a> = (&'a str, &'a str, &'a [&'a str]);

        /// 局面ごとの固定 MultiPV 応答を返す mock USI エンジン。
        ///
        /// `table.tsv` の行は `<sfen>\t<bestmove>\t<info 本体>|<info 本体>|...`。
        /// `setoption name MultiPV` の値までの行を返し、go ごとに `<multipv> <sfen>` を `log.txt` へ追記する。
        pub(super) struct MockEngine {
            pub(super) dir: tempfile::TempDir,
            pub(super) path: PathBuf,
        }

        impl MockEngine {
            pub(super) fn new(responses: &[Response<'_>]) -> Self {
                let dir = tempfile::tempdir().unwrap();
                let mut table = String::new();
                for (sfen, bestmove, lines) in responses {
                    table.push_str(&format!("{sfen}\t{bestmove}\t{}\n", lines.join("|")));
                }
                std::fs::write(dir.path().join("table.tsv"), table).unwrap();
                let path = dir.path().join("engine.sh");
                std::fs::write(
                    &path,
                    r#"#!/bin/sh
dir=$(dirname "$0")
mpv=1
pos=
while IFS= read -r line; do
  case "$line" in
    usi) printf 'id name mock\noption name MultiPV type spin default 1 min 1 max 800\nusiok\n' ;;
    isready) printf 'readyok\n' ;;
    "setoption name MultiPV value "*) mpv=${line##* } ;;
    "position sfen "*) pos=${line#position sfen } ;;
    go*)
      printf '%s %s\n' "$mpv" "$pos" >> "$dir/log.txt"
      awk -F '\t' -v p="$pos" -v k="$mpv" '
        $1 == p {
          n = split($3, a, "|")
          for (i = 1; i <= n && i <= k; i++) if (a[i] != "") printf "info depth 10 multipv %d %s\n", i, a[i]
          printf "bestmove %s\n", $2
          found = 1
          exit
        }
        END { if (!found) print "bestmove resign" }' "$dir/table.tsv"
      ;;
    quit) exit 0 ;;
  esac
done
"#,
                )
                .unwrap();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
                Self { dir, path }
            }

            pub(super) fn log(&self) -> Vec<String> {
                std::fs::read_to_string(self.dir.path().join("log.txt"))
                    .map(|text| text.lines().map(str::to_string).collect())
                    .unwrap_or_default()
            }
        }

        pub(super) fn engine_opts(engine: &MockEngine) -> EngineOpts {
            EngineOpts {
                engine: engine.path.clone(),
                engine_options: Vec::new(),
                go: "depth 10".to_string(),
                parallel: 1,
                multipv: 2,
                multipv_delta: 0,
                multipv_max: 16,
                extend_ply: 0,
            }
        }

        struct ExpandRun {
            out: String,
            report: String,
        }

        fn run_expand(
            dir: &Path,
            db: &str,
            leaves: &[&str],
            engine: EngineOpts,
            resume: bool,
            out_name: &str,
        ) -> Result<ExpandRun> {
            let book_path = dir.join("book.db");
            std::fs::write(&book_path, db).unwrap();
            let leaves_path = dir.join("leaves.txt");
            let text: String = leaves.iter().map(|s| format!("sfen {s}\n")).collect();
            std::fs::write(&leaves_path, text).unwrap();
            let out = dir.join(out_name);
            let report = dir.join(format!("{out_name}.md"));
            cmd_expand(&ExpandArgs {
                book: book_path,
                out: out.clone(),
                leaves: leaves_path,
                engine,
                journal: dir.join("journal.jsonl"),
                resume,
                report: Some(report.clone()),
            })?;
            Ok(ExpandRun {
                out: std::fs::read_to_string(out).unwrap(),
                report: std::fs::read_to_string(report).unwrap(),
            })
        }

        fn entry_block(db_text: &str, sfen: &str) -> Option<String> {
            let header = format!("sfen {sfen}\n");
            let start = db_text.find(&header)? + header.len();
            let rest = &db_text[start..];
            let end = rest.find("sfen ").unwrap_or(rest.len());
            Some(rest[..end].to_string())
        }

        #[test]
        fn expand_adds_new_position_with_multipv_moves_and_zero_count() {
            let leaf = after(START, &["7g7f"]);
            let engine = MockEngine::new(&[(
                &leaf,
                "3c3d",
                &[
                    "score cp 30 pv 3c3d 2g2f",
                    "score cp 10 pv 8c8d",
                    "score cp 5 pv 4a3b",
                ],
            )]);
            let dir = tempfile::tempdir().unwrap();
            let db = book_text(&[(START, &[("7g7f", 50, 10, 3)])]);

            let run = run_expand(dir.path(), &db, &[&leaf], engine_opts(&engine), false, "out.db")
                .unwrap();

            assert_eq!(
                entry_block(&run.out, &leaf).unwrap(),
                "3c3d 2g2f 30 10 0\n8c8d none 10 10 0\n"
            );
            assert_eq!(entry_block(&run.out, START).unwrap(), "7g7f none 50 10 3\n");
            assert_eq!(engine.log(), vec![format!("2 {leaf}")]);
            assert!(run.report.contains("| new positions | 1 |"));
            rshogi_book::Book::from_bytes(run.out.as_bytes(), true).unwrap();
        }

        #[test]
        fn expand_keeps_existing_moves_of_book_position_unchanged() {
            let leaf = after(START, &["7g7f"]);
            let engine = MockEngine::new(&[(
                &leaf,
                "3c3d",
                &["score cp 30 pv 3c3d", "score cp 10 pv 8c8d"],
            )]);
            let dir = tempfile::tempdir().unwrap();
            let db = book_text(&[(&leaf, &[("3c3d", 0, 0, 5)])]);

            let run = run_expand(dir.path(), &db, &[&leaf], engine_opts(&engine), false, "out.db")
                .unwrap();

            assert_eq!(
                entry_block(&run.out, &leaf).unwrap(),
                "8c8d none 10 10 0\n3c3d none 0 0 5\n"
            );
            assert!(run.report.contains("| existing positions with added moves | 1 |"));
        }

        #[test]
        fn expand_writes_moves_in_flipped_frame_for_flipped_book_hit() {
            let leaf = after(START, &["7g7f"]);
            let flipped_leaf = flipped(&leaf);
            let engine = MockEngine::new(&[(
                &leaf,
                "3c3d",
                &["score cp 30 pv 3c3d", "score cp 10 pv 8c8d"],
            )]);
            let dir = tempfile::tempdir().unwrap();
            let db = book_text(&[(&flipped_leaf, &[("7g7f", 0, 0, 5)])]);

            let run = run_expand(dir.path(), &db, &[&leaf], engine_opts(&engine), false, "out.db")
                .unwrap();

            assert_eq!(
                entry_block(&run.out, &flipped_leaf).unwrap(),
                "2g2f none 10 10 0\n7g7f none 0 0 5\n"
            );
            assert!(entry_block(&run.out, &leaf).is_none());
        }

        #[test]
        fn expand_increases_multipv_while_kth_line_is_within_delta() {
            let leaf = after(START, &["7g7f"]);
            let lines = [
                "score cp 30 pv 3c3d",
                "score cp 20 pv 8c8d",
                "score cp 10 pv 4a3b",
                "score cp -200 pv 9c9d",
            ];
            let engine = MockEngine::new(&[(&leaf, "3c3d", &lines)]);
            let dir = tempfile::tempdir().unwrap();
            let db = book_text(&[(START, &[("7g7f", 50, 10, 3)])]);
            let opts = EngineOpts {
                multipv_delta: 100,
                ..engine_opts(&engine)
            };

            let run = run_expand(dir.path(), &db, &[&leaf], opts, false, "out.db").unwrap();

            assert_eq!(engine.log(), vec![format!("2 {leaf}"), format!("4 {leaf}")]);
            assert_eq!(entry_block(&run.out, &leaf).unwrap().lines().count(), 4);
            assert!(run.report.contains("| multipv extensions | 1 |"));
        }

        #[test]
        fn expand_resume_reuses_journal_and_output_is_bit_identical() {
            let l1 = after(START, &["7g7f"]);
            let l2 = after(START, &["2g2f"]);
            let engine = MockEngine::new(&[
                (&l1, "3c3d", &["score cp 30 pv 3c3d", "score cp 10 pv 8c8d"]),
                (&l2, "8c8d", &["score cp 15 pv 8c8d", "score cp -5 pv 3c3d"]),
            ]);
            let dir = tempfile::tempdir().unwrap();
            let db = book_text(&[(START, &[("7g7f", 50, 10, 3), ("2g2f", 40, 10, 2)])]);
            let opts = EngineOpts {
                parallel: 2,
                ..engine_opts(&engine)
            };

            let first =
                run_expand(dir.path(), &db, &[&l2, &l1], opts.clone(), false, "a.db").unwrap();
            assert_eq!(engine.log().len(), 2);
            let second =
                run_expand(dir.path(), &db, &[&l1, &l2], opts.clone(), true, "b.db").unwrap();
            assert_eq!(engine.log().len(), 2, "journal 再利用時は探索しない");
            assert_eq!(first.out, second.out);
            assert!(second.report.contains("| journal reused positions | 2 |"));

            // 探索設定が変われば journal を再利用しない。
            let changed = EngineOpts {
                go: "depth 11".to_string(),
                ..opts
            };
            run_expand(dir.path(), &db, &[&l1, &l2], changed, true, "c.db").unwrap();
            assert_eq!(engine.log().len(), 4);
        }

        #[test]
        fn expand_records_declaration_win_without_adding_moves() {
            let leaf = after(START, &["7g7f"]);
            let engine = MockEngine::new(&[(&leaf, "win", &[])]);
            let dir = tempfile::tempdir().unwrap();
            let db = book_text(&[(START, &[("7g7f", 50, 10, 3)])]);

            let run = run_expand(dir.path(), &db, &[&leaf], engine_opts(&engine), false, "out.db")
                .unwrap();

            assert!(entry_block(&run.out, &leaf).is_none());
            assert!(run.report.contains("| declaration-win positions | 1 |"));
            assert!(run.report.contains(&format!("| 29999 | `{leaf}` |")));
        }

        #[test]
        fn expand_extend_ply_follows_best_move_into_new_positions() {
            let leaf = after(START, &["7g7f"]);
            let next = after(START, &["7g7f", "3c3d"]);
            let engine = MockEngine::new(&[
                (&leaf, "3c3d", &["score cp 30 pv 3c3d", "score cp 10 pv 8c8d"]),
                (&next, "2g2f", &["score cp 40 pv 2g2f"]),
            ]);
            let dir = tempfile::tempdir().unwrap();
            let db = book_text(&[(START, &[("7g7f", 50, 10, 3)])]);
            let opts = EngineOpts {
                extend_ply: 1,
                ..engine_opts(&engine)
            };

            let run = run_expand(dir.path(), &db, &[&leaf], opts, false, "out.db").unwrap();

            assert!(entry_block(&run.out, &leaf).is_some());
            assert_eq!(entry_block(&run.out, &next).unwrap(), "2g2f none 40 10 0\n");
            assert!(run.report.contains("| new positions via extend-ply | 1 |"));
        }

        #[test]
        fn expand_search_failure_is_an_error() {
            let leaf = after(START, &["7g7f"]);
            // table に無い局面は info 無しで bestmove resign を返す。
            let engine = MockEngine::new(&[]);
            let dir = tempfile::tempdir().unwrap();
            let db = book_text(&[(START, &[("7g7f", 50, 10, 3)])]);

            let result =
                run_expand(dir.path(), &db, &[&leaf], engine_opts(&engine), false, "out.db");

            assert!(result.is_err());
            assert!(!dir.path().join("out.db").exists());
        }
    }

    #[cfg(unix)]
    mod run {
        use super::engine::{MockEngine, engine_opts};
        use super::*;

        fn run_args(dir: &Path, engine: &MockEngine, iterations: usize, resume: bool) -> RunArgs {
            RunArgs {
                book: dir.join("book.db"),
                out: dir.join("final.db"),
                frontier: FrontierOpts {
                    roots: dir.join("roots.txt"),
                    ..opts(SideArg::Black)
                },
                engine: engine_opts(engine),
                iterations,
                max_new_positions: None,
                merge: MergeMode::Replace,
                work_dir: dir.join("work"),
                resume,
            }
        }

        #[test]
        fn run_two_iterations_writes_iter_dirs_and_resumes() {
            let l1 = after(START, &["7g7f"]);
            let l2a = after(START, &["7g7f", "3c3d"]);
            let l2b = after(START, &["7g7f", "8c8d"]);
            let engine = MockEngine::new(&[
                (&l1, "3c3d", &["score cp 20 pv 3c3d", "score cp -10 pv 8c8d"]),
                (&l2a, "2g2f", &["score cp 5 pv 2g2f"]),
                (&l2b, "2g2f", &["score cp 7 pv 2g2f"]),
            ]);
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(
                dir.path().join("book.db"),
                book_text(&[(START, &[("7g7f", 50, 10, 1)])]),
            )
            .unwrap();
            std::fs::write(dir.path().join("roots.txt"), "startpos\n").unwrap();
            let work = dir.path().join("work");

            cmd_run(&run_args(dir.path(), &engine, 1, false)).unwrap();
            let iter1 = work.join("iter-001");
            for name in [
                "leaves.txt",
                "leaves.txt.entered",
                "frontier.md",
                "expanded.db",
                "expand.md",
                "book.db",
                "backprop.md",
                "summary.json",
            ] {
                assert!(iter1.join(name).exists(), "{name}");
            }
            assert!(!work.join("iter-002").exists());
            assert_eq!(
                std::fs::read_to_string(iter1.join("leaves.txt")).unwrap(),
                format!("sfen {l1}\n")
            );
            // replace 逆伝播で START の 7g7f は -best(l1) = -20 になる。
            let book1 = std::fs::read_to_string(iter1.join("book.db")).unwrap();
            assert!(book1.contains("7g7f none -20 10 1\n"));
            assert_eq!(engine.log().len(), 1);

            // --resume 無しで既存 work-dir は拒否する。
            assert!(cmd_run(&run_args(dir.path(), &engine, 2, false)).is_err());

            cmd_run(&run_args(dir.path(), &engine, 2, true)).unwrap();
            let iter2 = work.join("iter-002");
            let leaves2 = std::fs::read_to_string(iter2.join("leaves.txt")).unwrap();
            assert_eq!(leaves2.lines().count(), 2);
            assert!(leaves2.contains(&l2a) && leaves2.contains(&l2b));
            // 1 周目は再実行しない。
            assert_eq!(engine.log().len(), 3);
            let summary: IterationSummary =
                serde_json::from_str(&std::fs::read_to_string(iter2.join("summary.json")).unwrap())
                    .unwrap();
            assert_eq!(
                summary,
                IterationSummary {
                    iteration: 2,
                    leaves: 2,
                    new_positions: 2,
                    added_moves: 2,
                }
            );
            let final_db = std::fs::read(dir.path().join("final.db")).unwrap();
            assert_eq!(final_db, std::fs::read(iter2.join("book.db")).unwrap());

            // 完了済みなら探索せずに同じ最終 book を書く。
            std::fs::remove_file(dir.path().join("final.db")).unwrap();
            cmd_run(&run_args(dir.path(), &engine, 2, true)).unwrap();
            assert_eq!(engine.log().len(), 3);
            assert_eq!(std::fs::read(dir.path().join("final.db")).unwrap(), final_db);
        }

        #[test]
        fn run_stops_at_max_new_positions() {
            let l1 = after(START, &["7g7f"]);
            let engine =
                MockEngine::new(&[(&l1, "3c3d", &["score cp 20 pv 3c3d", "score cp -10 pv 8c8d"])]);
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(
                dir.path().join("book.db"),
                book_text(&[(START, &[("7g7f", 50, 10, 1)])]),
            )
            .unwrap();
            std::fs::write(dir.path().join("roots.txt"), "startpos\n").unwrap();

            let args = RunArgs {
                max_new_positions: Some(1),
                ..run_args(dir.path(), &engine, 3, false)
            };
            cmd_run(&args).unwrap();

            assert!(dir.path().join("work/iter-001/summary.json").exists());
            assert!(!dir.path().join("work/iter-002").exists());
            assert_eq!(engine.log().len(), 1);
        }
    }
}
