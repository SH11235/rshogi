//! 相手の定跡の木 (棋譜で即指しが続いた範囲) を自分の定跡で辿り、相手の準備の中で
//! 自分が考え始める局面と、そこから先に残る相手の準備の長さを数える。

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, ValueEnum};
use rshogi_book::BookExploreList;
use rshogi_core::position::{Position, SFEN_HIRATE};
use rshogi_core::types::{Color, Move};
use tools::book_db::{BookDb, apply_usi_move, position_from_sfen, strip_ply};
use tools::common::io::write_atomic;

/// 定跡の元データが記録用の手に付ける値。これ以下の手は自分からは指さない。
const RECORD_VALUE: i32 = -30000;
/// 標準出力の「読む」の上位と危ない進行の上位に、それぞれ表示する行数の上限。
const SUMMARY_TOP: usize = 8;

#[derive(Parser, Debug)]
#[command(
    name = "book_prepwalk",
    version,
    about = "相手の定跡の木を自分の定跡で辿り、自分が考え始める局面と相手の準備の残りを集計する"
)]
struct Cli {
    /// 相手の対局の一覧 TSV (列 black / white / sente_book / eval_sente_out / moves_csa / winner)
    #[arg(long)]
    lines: PathBuf,
    /// 自分の定跡 (YANEURAOU-DB2016 テキスト .db)
    #[arg(long)]
    book: PathBuf,
    /// 止まった局面の一覧 TSV の出力先
    #[arg(long)]
    out: PathBuf,
    /// 相手の名前 (大小文字を無視した部分一致、カンマ区切り・複数指定可)。先に書いた名前を優先する
    #[arg(long = "opponent", required = true, value_delimiter = ',')]
    opponents: Vec<String>,
    /// 自分の手番。white なら相手は先手で、`--lines` は先手の定跡の一覧
    #[arg(long, value_enum, default_value_t = SideArg::White)]
    our_side: SideArg,
    /// 自分の手番で辿る手: main = 値・採択数が最大の手、all = 記録用以外の全手、preset = `--preset` の候補
    #[arg(long, value_enum, default_value_t = FollowArg::Main)]
    follow: FollowArg,
    /// 局面ごとの乱択候補ファイル (`sfen <盤面> <手番> <持ち駒> [<手数>] <手> ...`)。`--follow preset` で必須
    #[arg(long)]
    preset: Option<PathBuf>,
    /// 自分の手を差し替える TSV (`局面キー<TAB>手`、手が空なら定跡に無いとみなす)。複数指定可
    #[arg(long = "override")]
    overrides: Vec<PathBuf>,
    /// 標準出力の上位表示に載せる重みの下限
    #[arg(long, default_value_t = 0.005)]
    min_weight: f64,
    /// 重みがこれ未満の枝は辿らずに打ち切る
    #[arg(long, default_value_t = 0.0001)]
    prune: f64,
    /// この手数を超えたら辿らない
    #[arg(long, default_value_t = 200)]
    max_ply: i32,
    /// 止まり方「<名前> が読む」に使う自分の名前
    #[arg(long, default_value = "自分")]
    self_name: String,
    /// 危ない進行: 残り手数がこれ以上で出口の評価値も `--danger-eval` 以上の対局がその局面を通っている
    #[arg(long, default_value_t = 40)]
    danger_remaining: usize,
    /// 危ない進行とみなす 1 局の出口の評価値 (相手から見た値) の下限
    #[arg(long, default_value_t = 250)]
    danger_eval: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum SideArg {
    Black,
    White,
}

impl SideArg {
    fn color(self) -> Color {
        match self {
            SideArg::Black => Color::Black,
            SideArg::White => Color::White,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum FollowArg {
    Main,
    All,
    Preset,
}

/// 自分の手番で辿る手の選び方。
#[derive(Debug)]
enum Follow {
    Main,
    All,
    Preset(BookExploreList),
}

/// 相手の 1 局。
#[derive(Debug, Clone)]
struct LineGame {
    moves: Vec<String>,
    /// 相手の定跡が続いた手数。
    book_end: usize,
    /// 相手が定跡を出た直後の評価値 (先手から見た値)。
    exit_eval: Option<i64>,
    winner: String,
}

/// 相手の定跡の木を通った 1 局の記録。
#[derive(Debug, Clone)]
struct Pass {
    remaining: usize,
    exit_eval: Option<i64>,
    winner: String,
}

#[derive(Debug, Default)]
struct OpponentTree {
    /// 相手の手番の局面 → (相手の手, 局数)。並びは初出順。
    moves: HashMap<String, Vec<(String, u64)>>,
    passes: HashMap<String, Vec<Pass>>,
    /// 再生できずに途中で打ち切った対局数。
    broken_games: usize,
}

/// 自分の定跡の 1 局面。
#[derive(Debug, Clone)]
struct OurEntry {
    /// 記録用以外の手と (値, 採択数)。ファイル記載順で、同じ手の重複行も残す。
    ranked: Vec<(String, (i32, u64))>,
    /// 記録用を含む全手。乱択候補はこれとの積集合を取る。
    listed: Vec<String>,
}

impl OurEntry {
    /// 合法な手のうち (値, 採択数) が最大で、同点ならファイルで先に書かれた手。
    fn main(&self, legal: &dyn Fn(&str) -> bool) -> Option<&str> {
        let mut best: Option<(&str, (i32, u64))> = None;
        for (mv, rank) in &self.ranked {
            if legal(mv) && best.is_none_or(|(_, b)| *rank > b) {
                best = Some((mv, *rank));
            }
        }
        best.map(|(mv, _)| mv)
    }

    fn playable(&self, legal: &dyn Fn(&str) -> bool) -> Vec<String> {
        let mut moves: Vec<String> = Vec::new();
        for (mv, _) in &self.ranked {
            if !moves.contains(mv) && legal(mv) {
                moves.push(mv.clone());
            }
        }
        moves
    }
}

#[derive(Debug, Default)]
struct OurBook {
    entries: HashMap<String, OurEntry>,
}

impl OurBook {
    fn from_db(db: &BookDb) -> Self {
        let mut entries = HashMap::new();
        for (key, entry) in &db.entries {
            let mut ranked = Vec::new();
            let mut listed = Vec::new();
            for book_move in &entry.moves {
                let Some(mv) = book_move.move_usi.as_deref() else {
                    continue;
                };
                if !listed.iter().any(|m| m == mv) {
                    listed.push(mv.to_string());
                }
                if book_move.value > RECORD_VALUE {
                    ranked.push((mv.to_string(), (book_move.value, book_move.count)));
                }
            }
            entries.insert(key.clone(), OurEntry { ranked, listed });
        }
        Self { entries }
    }

    /// 差し替え TSV を適用し、定跡に無かった局面キーを返す。
    fn apply_override(&mut self, text: &str) -> Vec<String> {
        let mut missing = Vec::new();
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let mut cells = line.split('\t');
            let key = cells.next().unwrap_or_default();
            if !self.entries.contains_key(key) {
                missing.push(key.to_string());
            }
            match cells.next().filter(|mv| !mv.is_empty()) {
                Some(mv) => {
                    self.entries.insert(
                        key.to_string(),
                        OurEntry {
                            ranked: vec![(mv.to_string(), (0, 0))],
                            listed: vec![mv.to_string()],
                        },
                    );
                }
                None => {
                    self.entries.remove(key);
                }
            }
        }
        missing
    }

    /// 自分の手番で辿る、局面で合法な手。空なら自分が読む局面。
    fn choices(&self, key: &str, follow: &Follow, legal: &dyn Fn(&str) -> bool) -> Vec<String> {
        let Some(entry) = self.entries.get(key) else {
            return Vec::new();
        };
        let main = || entry.main(legal).map(str::to_string).into_iter().collect();
        match follow {
            Follow::Main => main(),
            Follow::All => entry.playable(legal),
            Follow::Preset(preset) => {
                // エンジンの乱択と同じく、候補と合法な定跡の手の積集合が空なら通常の選択に戻る。
                let picked: Vec<String> = preset
                    .moves_for(key)
                    .unwrap_or_default()
                    .iter()
                    .filter(|mv| entry.listed.contains(mv) && legal(mv))
                    .cloned()
                    .collect();
                if picked.is_empty() { main() } else { picked }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StopKind {
    MaxPly,
    Pruned,
    OpponentOut,
    WeThink,
}

#[derive(Debug, Clone)]
struct WalkConfig {
    our_side: Color,
    prune: f64,
    max_ply: i32,
    min_weight: f64,
    danger_remaining: usize,
    danger_eval: i64,
    self_name: String,
}

impl WalkConfig {
    fn label(&self, kind: StopKind) -> String {
        match kind {
            StopKind::MaxPly => "上限".to_string(),
            StopKind::Pruned => "打ち切り".to_string(),
            StopKind::OpponentOut => "相手の定跡が切れた".to_string(),
            StopKind::WeThink => format!("{} が読む", self.self_name),
        }
    }

    /// 先手から見た評価値を相手から見た値にする。
    fn for_opponent(&self, eval: i64) -> i64 {
        if self.our_side == Color::White {
            eval
        } else {
            -eval
        }
    }

    /// 相手が勝ったときの `winner` 列の値。
    fn opponent_winner(&self) -> &'static str {
        if self.our_side == Color::White {
            "b"
        } else {
            "w"
        }
    }
}

/// 一覧に載せる止まり方 (相手の定跡が切れた / 自分が読む) の 1 行。
#[derive(Debug, Clone)]
struct Row {
    weight: f64,
    kind: StopKind,
    ply: i32,
    key: String,
    path: Vec<String>,
    /// 自分が main 以外を選んだ手 (`手数:手`)。
    alts: Vec<String>,
    games: usize,
    max_remaining: usize,
    eval_median: Option<i64>,
    /// 出口の評価値のうち相手に最も良い値 (先手から見た値)。
    eval_best: Option<i64>,
    opponent_winrate: Option<f64>,
    /// 残り手数と出口の評価値が同じ 1 局で危ない進行の条件を満たす対局数。
    danger_games: usize,
}

impl Row {
    fn is_dangerous(&self) -> bool {
        self.kind == StopKind::WeThink && self.danger_games > 0
    }
}

#[derive(Debug)]
struct OpponentReport {
    name: String,
    games: usize,
    broken_games: usize,
    /// 止まり方ごとの重みの合計 (初出順)。
    totals: Vec<(StopKind, f64)>,
    rows: Vec<Row>,
    illegal: BTreeSet<(String, String)>,
}

fn position_key(pos: &Position) -> String {
    strip_ply(&pos.to_sfen()).to_string()
}

fn hirate() -> Position {
    position_from_sfen(SFEN_HIRATE).expect("平手の SFEN は常に有効")
}

/// 対局一覧から、相手ごとの対局を相手の初出順に集める。
fn parse_lines(
    text: &str,
    opponents: &[String],
    our_side: Color,
) -> Result<Vec<(String, Vec<LineGame>)>> {
    let names: Vec<String> = opponents.iter().map(|n| n.to_lowercase()).collect();
    let mut lines = text.lines();
    let header: Vec<&str> = lines.next().unwrap_or_default().split('\t').collect();
    let column = |name: &str| {
        header
            .iter()
            .position(|h| *h == name)
            .with_context(|| format!("列 {name} がありません"))
    };
    let player_col = column(if our_side == Color::White {
        "black"
    } else {
        "white"
    })?;
    let book_col = column("sente_book")?;
    let eval_col = column("eval_sente_out")?;
    let moves_col = column("moves_csa")?;
    let winner_col = column("winner")?;

    let mut grouped: Vec<(String, Vec<LineGame>)> = Vec::new();
    for (index, line) in lines.enumerate() {
        if line.is_empty() {
            continue;
        }
        let cells: Vec<&str> = line.split('\t').collect();
        let cell = |col: usize| {
            cells
                .get(col)
                .copied()
                .with_context(|| format!("{} 行目: 列が足りません", index + 2))
        };
        let player = cell(player_col)?.to_lowercase();
        let Some(name) = names.iter().find(|n| player.contains(n.as_str())) else {
            continue;
        };
        let eval = cell(eval_col)?;
        let game =
            LineGame {
                moves: cell(moves_col)?.split_whitespace().map(str::to_string).collect(),
                book_end: cell(book_col)?
                    .parse()
                    .with_context(|| format!("{} 行目: sente_book が不正です", index + 2))?,
                exit_eval: if eval.is_empty() {
                    None
                } else {
                    Some(eval.parse().with_context(|| {
                        format!("{} 行目: eval_sente_out が不正です", index + 2)
                    })?)
                },
                winner: cell(winner_col)?.to_string(),
            };
        match grouped.iter_mut().find(|(n, _)| n == name) {
            Some((_, games)) => games.push(game),
            None => grouped.push((name.clone(), vec![game])),
        }
    }
    Ok(grouped)
}

fn build_tree(games: &[LineGame], our_side: Color) -> OpponentTree {
    let mut tree = OpponentTree::default();
    for game in games {
        let mut pos = hirate();
        let mut csa = rshogi_csa::initial_position();
        for (ply_index, csa_move) in game.moves.iter().take(game.book_end).enumerate() {
            let key = position_key(&pos);
            tree.passes.entry(key.clone()).or_default().push(Pass {
                remaining: game.book_end - ply_index,
                exit_eval: game.exit_eval,
                winner: game.winner.clone(),
            });
            let Ok(usi) = rshogi_csa::csa_move_to_usi(csa_move, &csa) else {
                tree.broken_games += 1;
                break;
            };
            if csa.apply_csa_move(csa_move).is_err() || apply_usi_move(&mut pos, &usi).is_err() {
                tree.broken_games += 1;
                break;
            }
            if pos.side_to_move() == our_side {
                let counts = tree.moves.entry(key).or_default();
                match counts.iter_mut().find(|(mv, _)| *mv == usi) {
                    Some((_, count)) => *count += 1,
                    None => counts.push((usi, 1)),
                }
            }
        }
    }
    tree
}

fn add_total(totals: &mut Vec<(StopKind, f64)>, kind: StopKind, weight: f64) {
    match totals.iter_mut().find(|(k, _)| *k == kind) {
        Some((_, sum)) => *sum += weight,
        None => totals.push((kind, weight)),
    }
}

fn summarize_passes(row: &mut Row, passes: &[Pass], cfg: &WalkConfig) {
    row.games = passes.len();
    row.max_remaining = passes.iter().map(|p| p.remaining).max().unwrap_or(0);
    let mut evals: Vec<i64> = passes.iter().filter_map(|p| p.exit_eval).collect();
    evals.sort_unstable();
    let mid = evals.len() / 2;
    row.eval_median = match evals.len() {
        0 => None,
        n if n % 2 == 1 => Some(evals[mid]),
        _ => Some((evals[mid - 1] + evals[mid]) / 2),
    };
    row.eval_best = if cfg.our_side == Color::White {
        evals.last().copied()
    } else {
        evals.first().copied()
    };
    row.opponent_winrate = (!passes.is_empty()).then(|| {
        let wins = passes.iter().filter(|p| p.winner == cfg.opponent_winner()).count();
        wins as f64 / passes.len() as f64
    });
    row.danger_games = passes
        .iter()
        .filter(|p| {
            p.remaining >= cfg.danger_remaining
                && p.exit_eval.is_some_and(|e| cfg.for_opponent(e) >= cfg.danger_eval)
        })
        .count();
}

fn is_legal_usi(pos: &Position, mv: &str) -> bool {
    Move::from_usi(mv)
        .and_then(|m| pos.to_move(m))
        .is_some_and(|m| m != Move::NONE && pos.pseudo_legal(m) && pos.is_legal(m))
}

struct Walked {
    /// 止まり方ごとの重みの合計 (初出順)。
    totals: Vec<(StopKind, f64)>,
    rows: Vec<Row>,
    /// 局面で非合法だったため辿らなかった自分の手 (局面キー, 手)。
    illegal: BTreeSet<(String, String)>,
}

/// 平手から相手の木と自分の定跡を交互に辿る。深さ優先で、後に積んだ枝から辿る。
fn walk(tree: &OpponentTree, our: &OurBook, follow: &Follow, cfg: &WalkConfig) -> Result<Walked> {
    let mut totals = Vec::new();
    let mut rows = Vec::new();
    let illegal = RefCell::new(BTreeSet::new());
    let mut stack = vec![(hirate(), 1.0f64, Vec::<String>::new(), Vec::<String>::new())];
    while let Some((pos, weight, path, alts)) = stack.pop() {
        let key = position_key(&pos);
        let ply = pos.game_ply();
        if ply > cfg.max_ply {
            add_total(&mut totals, StopKind::MaxPly, weight);
            continue;
        }
        if weight < cfg.prune {
            add_total(&mut totals, StopKind::Pruned, weight);
            continue;
        }
        let (kind, children, main): (StopKind, Vec<(String, f64)>, Option<String>) =
            if pos.side_to_move() != cfg.our_side {
                let moves = tree.moves.get(&key).map(Vec::as_slice).unwrap_or_default();
                let total: u64 = moves.iter().map(|(_, c)| c).sum();
                let children =
                    moves.iter().map(|(mv, c)| (mv.clone(), weight * *c as f64 / total as f64));
                (StopKind::OpponentOut, children.collect(), None)
            } else {
                let legal = |mv: &str| {
                    let ok = is_legal_usi(&pos, mv);
                    if !ok {
                        illegal.borrow_mut().insert((key.clone(), mv.to_string()));
                    }
                    ok
                };
                let moves = our.choices(&key, follow, &legal);
                let main = our.entries.get(&key).and_then(|e| e.main(&legal)).map(str::to_string);
                // エンジンは乱択候補の局面で候補を等確率に選ぶので、複数の手には重みを等分する。
                let share = weight / moves.len() as f64;
                (StopKind::WeThink, moves.into_iter().map(|mv| (mv, share)).collect(), main)
            };
        if children.is_empty() {
            add_total(&mut totals, kind, weight);
            rows.push(Row {
                weight,
                kind,
                ply,
                key,
                path,
                alts,
                games: 0,
                max_remaining: 0,
                eval_median: None,
                eval_best: None,
                opponent_winrate: None,
                danger_games: 0,
            });
            continue;
        }
        for (mv, child_weight) in children {
            let mut child = pos.clone();
            apply_usi_move(&mut child, &mv).with_context(|| format!("局面 {key} の手 {mv}"))?;
            let mut child_path = path.clone();
            child_path.push(mv.clone());
            let mut child_alts = alts.clone();
            if kind == StopKind::WeThink && main.as_deref() != Some(mv.as_str()) {
                child_alts.push(format!("{ply}:{mv}"));
            }
            stack.push((child, child_weight, child_path, child_alts));
        }
    }
    for row in &mut rows {
        let passes = tree.passes.get(&row.key).map(Vec::as_slice).unwrap_or_default();
        summarize_passes(row, passes, cfg);
    }
    Ok(Walked {
        totals,
        rows,
        illegal: illegal.into_inner(),
    })
}

/// 乱択候補ファイルをエンジンと同じ規則で読む。読み飛ばした行の理由は stderr に出す。
fn parse_preset(text: &str) -> BookExploreList {
    BookExploreList::parse(text, |message| eprintln!("{message}"))
}

fn fmt_opt<T: std::fmt::Display>(value: Option<T>) -> String {
    value.map(|v| v.to_string()).unwrap_or_default()
}

fn tsv(reports: &[OpponentReport], cfg: &WalkConfig, with_alts: bool) -> String {
    let mut out = String::from(
        "opponent\tweight\tstop\tply\tkey\tpath\tgames\tmax_remaining\teval_out_median\teval_out_max\tsente_winrate",
    );
    if with_alts {
        out.push_str("\talt_moves");
    }
    out.push('\n');
    for report in reports {
        for row in &report.rows {
            let _ = write!(
                out,
                "{}\t{:.4}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                report.name,
                row.weight,
                cfg.label(row.kind),
                row.ply,
                row.key,
                row.path.join(" "),
                row.games,
                row.max_remaining,
                fmt_opt(row.eval_median),
                fmt_opt(row.eval_best),
                row.opponent_winrate.map(|w| format!("{w:.2}")).unwrap_or_default(),
            );
            if with_alts {
                let _ = write!(out, "\t{}", row.alts.join(" "));
            }
            out.push('\n');
        }
    }
    out
}

fn summary(reports: &[OpponentReport], cfg: &WalkConfig) -> String {
    let mut out = String::new();
    let mut danger_total = 0;
    for report in reports {
        let totals: Vec<String> =
            report.totals.iter().map(|(k, w)| format!("{} {w:.3}", cfg.label(*k))).collect();
        let _ = writeln!(
            out,
            "== {}: 対局 {}、止まり方 {}",
            report.name,
            report.games,
            totals.join(", ")
        );
        let mut top: Vec<&Row> = report
            .rows
            .iter()
            .filter(|r| r.kind == StopKind::WeThink && r.weight >= cfg.min_weight)
            .collect();
        top.sort_by(|a, b| b.weight.total_cmp(&a.weight));
        for row in top.iter().take(SUMMARY_TOP) {
            let _ = writeln!(
                out,
                "   {} {} 手目 重み {:.3} | 通った対局 {}、相手の定跡の残り 最大 {} 手、出口の評価値 中央値 {} 最大 {}、相手の勝率 {}",
                cfg.label(row.kind),
                row.ply,
                row.weight,
                row.games,
                row.max_remaining,
                fmt_opt(row.eval_median),
                fmt_opt(row.eval_best),
                row.opponent_winrate.map(|w| format!("{w:.2}")).unwrap_or_default(),
            );
        }
        let mut dangerous: Vec<&Row> = report.rows.iter().filter(|r| r.is_dangerous()).collect();
        danger_total += dangerous.len();
        let positions: HashSet<&str> = dangerous.iter().map(|r| r.key.as_str()).collect();
        let _ = writeln!(
            out,
            "   危ない進行 {} 本 (局面 {} 種、重み {:.4})",
            dangerous.len(),
            positions.len(),
            dangerous.iter().fold(0.0, |sum, r| sum + r.weight)
        );
        dangerous.sort_by(|a, b| b.weight.total_cmp(&a.weight));
        for row in dangerous.iter().take(SUMMARY_TOP) {
            let alts = if row.alts.is_empty() {
                String::new()
            } else {
                format!(" | 別の手 {}", row.alts.join(" "))
            };
            let _ = writeln!(
                out,
                "     重み {:.4} {} 手目 危ない対局 {}/{} | {}{alts}",
                row.weight,
                row.ply,
                row.danger_games,
                row.games,
                row.path.join(" "),
            );
        }
        if report.broken_games > 0 {
            let _ = writeln!(out, "   再生できずに途中で打ち切った対局 {}", report.broken_games);
        }
        if !report.illegal.is_empty() {
            let _ = writeln!(out, "   非合法で辿らなかった自分の手 {}", report.illegal.len());
        }
    }
    let _ = writeln!(
        out,
        "危ない進行 (相手の定跡の残り {} 手以上かつ出口の評価値が相手から見て +{} 以上の対局が通る) の合計: {danger_total} 本",
        cfg.danger_remaining, cfg.danger_eval
    );
    out
}

fn run_walk(
    lines_text: &str,
    opponents: &[String],
    our: &OurBook,
    follow: &Follow,
    cfg: &WalkConfig,
) -> Result<Vec<OpponentReport>> {
    let mut reports = Vec::new();
    for (name, games) in parse_lines(lines_text, opponents, cfg.our_side)? {
        let tree = build_tree(&games, cfg.our_side);
        let Walked {
            totals,
            rows,
            illegal,
        } = walk(&tree, our, follow, cfg)?;
        for (key, mv) in &illegal {
            eprintln!("{name}: 局面 {key} の手 {mv} は非合法なので辿りません");
        }
        reports.push(OpponentReport {
            name,
            games: games.len(),
            broken_games: tree.broken_games,
            totals,
            rows,
            illegal,
        });
    }
    Ok(reports)
}

fn read_text(path: &Path) -> Result<String> {
    fs::read_to_string(path).with_context(|| format!("読めません: {}", path.display()))
}

fn walk_config(cli: &Cli) -> Result<WalkConfig> {
    ensure!(cli.prune > 0.0, "--prune は正の値にしてください (0 以下だと辿りが終わりません)");
    ensure!(cli.min_weight >= 0.0, "--min-weight は 0 以上にしてください");
    Ok(WalkConfig {
        our_side: cli.our_side.color(),
        prune: cli.prune,
        max_ply: cli.max_ply,
        min_weight: cli.min_weight,
        danger_remaining: cli.danger_remaining,
        danger_eval: cli.danger_eval,
        self_name: cli.self_name.clone(),
    })
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = walk_config(&cli)?;
    let follow = match (cli.follow, &cli.preset) {
        (FollowArg::Preset, Some(path)) => {
            let preset = parse_preset(&read_text(path)?);
            let skipped = preset.skipped_lines();
            if skipped > 0 {
                eprintln!("{}: 不正な {skipped} 行を読み飛ばしました", path.display());
            }
            Follow::Preset(preset)
        }
        (FollowArg::Preset, None) => bail!("--follow preset には --preset が必要です"),
        (_, Some(_)) => bail!("--preset は --follow preset のときだけ指定できます"),
        (FollowArg::Main, None) => Follow::Main,
        (FollowArg::All, None) => Follow::All,
    };
    let mut our = OurBook::from_db(&BookDb::read(&cli.book)?);
    for path in &cli.overrides {
        for key in our.apply_override(&read_text(path)?) {
            eprintln!("{}: 局面 {key} は定跡にありません", path.display());
        }
    }
    let reports = run_walk(&read_text(&cli.lines)?, &cli.opponents, &our, &follow, &cfg)?;
    write_atomic(&cli.out, &tsv(&reports, &cfg, !matches!(follow, Follow::Main)))?;
    print!("{}", summary(&reports, &cfg));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tools::book_db::BOOK_HEADER;

    const HEADER: &str = "path\tblack\twhite\tsente_book\teval_sente_out\tmoves_csa\twinner";

    fn sfen_after(moves: &[&str]) -> String {
        let mut pos = hirate();
        for mv in moves {
            apply_usi_move(&mut pos, mv).unwrap();
        }
        pos.to_sfen()
    }

    fn key_after(moves: &[&str]) -> String {
        strip_ply(&sfen_after(moves)).to_string()
    }

    fn lines_text() -> String {
        [
            HEADER,
            "g1\tFoo_A\tx\t5\t300\t+7776FU -3334FU +2726FU -8384FU +2625FU -8485FU\tb",
            "g2\tfoo_A\tx\t3\t-100\t+7776FU -3334FU +6766FU -8384FU\tw",
            "g3\tFOO_A\tx\t4\t\t+2726FU -8384FU +2625FU -8485FU\tb",
            "g4\tbar\tx\t4\t0\t+7776FU -3334FU +2726FU -8384FU\tb",
        ]
        .join("\n")
            + "\n"
    }

    fn book(entries: &[(&[&str], &[&str])]) -> OurBook {
        let mut text = format!("{BOOK_HEADER}\n");
        for (moves, lines) in entries {
            text.push_str(&format!("sfen {}\n", sfen_after(moves)));
            for line in *lines {
                text.push_str(line);
                text.push('\n');
            }
        }
        OurBook::from_db(&BookDb::from_reader(text.as_bytes()).unwrap())
    }

    fn our_book() -> OurBook {
        book(&[(
            &["7g7f"],
            &[
                "3c3d none 100 0 2",
                "8c8d none 100 0 1",
                "4a3b none -30000 0 0",
                "9a9c none 100 0 1",
            ],
        )])
    }

    fn cfg() -> WalkConfig {
        WalkConfig {
            our_side: Color::White,
            prune: 0.0001,
            max_ply: 200,
            min_weight: 0.005,
            danger_remaining: 2,
            danger_eval: 250,
            self_name: "自分".to_string(),
        }
    }

    fn walk_with(follow: &Follow, our: &OurBook) -> Vec<OpponentReport> {
        run_walk(&lines_text(), &["foo".to_string()], our, follow, &cfg()).unwrap()
    }

    fn paths(report: &OpponentReport) -> Vec<String> {
        report.rows.iter().map(|r| r.path.join(" ")).collect()
    }

    fn row<'a>(report: &'a OpponentReport, path: &str) -> &'a Row {
        report.rows.iter().find(|r| r.path.join(" ") == path).unwrap()
    }

    fn after_7g7f_legal(mv: &str) -> bool {
        is_legal_usi(&position_from_sfen(&sfen_after(&["7g7f"])).unwrap(), mv)
    }

    fn preset(text: &str) -> Follow {
        Follow::Preset(parse_preset(text))
    }

    #[test]
    fn main_walk_splits_opponent_weight_and_reports_remaining_preparation() {
        let reports = walk_with(&Follow::Main, &our_book());
        assert_eq!(reports.len(), 1);
        let report = &reports[0];
        assert_eq!(report.games, 3);
        assert_eq!(paths(report), ["2g2f", "7g7f 3c3d 6g6f", "7g7f 3c3d 2g2f"]);
        let last = &report.rows[2];
        assert_eq!(last.kind, StopKind::WeThink);
        assert_eq!(last.ply, 4);
        assert!((last.weight - 1.0 / 3.0).abs() < 1e-12);
        assert_eq!((last.games, last.max_remaining), (1, 2));
        assert_eq!((last.eval_median, last.eval_best), (Some(300), Some(300)));
        assert_eq!(last.opponent_winrate, Some(1.0));
        assert!(last.is_dangerous());
        let first = &report.rows[0];
        assert_eq!((first.games, first.max_remaining, first.eval_best), (1, 3, None));
        assert!(!first.is_dangerous());
    }

    #[test]
    fn tsv_has_eleven_columns_in_main_mode_and_alt_column_otherwise() {
        let cfg = cfg();
        let reports = walk_with(&Follow::Main, &our_book());
        let out = tsv(&reports, &cfg, false);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0].split('\t').count(), 11);
        assert_eq!(
            lines[3],
            format!(
                "foo\t0.3333\t自分 が読む\t4\t{}\t7g7f 3c3d 2g2f\t1\t2\t300\t300\t1.00",
                key_after(&["7g7f", "3c3d", "2g2f"])
            )
        );
        assert!(tsv(&reports, &cfg, true).lines().next().unwrap().ends_with("\talt_moves"));
    }

    #[test]
    fn main_is_the_best_legal_move_when_a_higher_value_move_is_illegal() {
        let our = book(&[(&["7g7f"], &["9a9c none 200 0 5", "3c3d none 100 0 1"])]);
        let reports = walk_with(&Follow::Main, &our);
        let report = &reports[0];
        assert_eq!(paths(report), ["2g2f", "7g7f 3c3d 6g6f", "7g7f 3c3d 2g2f"]);
        assert!(report.rows.iter().all(|r| r.alts.is_empty()));
        assert_eq!(
            report.illegal.iter().cloned().collect::<Vec<_>>(),
            [(key_after(&["7g7f"]), "9a9c".to_string())]
        );
        let key = key_after(&["7g7f"]);
        let preset = preset(&format!("sfen {key} 9a9c\n"));
        assert_eq!(our.choices(&key, &preset, &after_7g7f_legal), ["3c3d"]);
    }

    #[test]
    fn crlf_lines_give_the_same_walk() {
        let crlf = lines_text().replace('\n', "\r\n");
        let names = ["foo".to_string()];
        let lf = run_walk(&lines_text(), &names, &our_book(), &Follow::Main, &cfg()).unwrap();
        let cr = run_walk(&crlf, &names, &our_book(), &Follow::Main, &cfg()).unwrap();
        assert_eq!(tsv(&lf, &cfg(), false), tsv(&cr, &cfg(), false));
    }

    #[test]
    fn all_follows_legal_non_record_moves_with_equal_weight_and_marks_alts() {
        let reports = walk_with(&Follow::All, &our_book());
        let report = &reports[0];
        let alt = row(report, "7g7f 8c8d");
        assert_eq!(alt.kind, StopKind::OpponentOut);
        assert_eq!(alt.alts, ["2:8c8d"]);
        assert!((alt.weight - 1.0 / 3.0).abs() < 1e-12);
        let main = row(report, "7g7f 3c3d 2g2f");
        assert!(main.alts.is_empty());
        assert!((main.weight - 1.0 / 6.0).abs() < 1e-12);
        assert!(report.rows.iter().all(|r| !r.path.iter().any(|m| m == "4a3b" || m == "9a9c")));
        assert_eq!(
            report.illegal.iter().cloned().collect::<Vec<_>>(),
            [(key_after(&["7g7f"]), "9a9c".to_string())]
        );
        let total: f64 = report.totals.iter().map(|(_, w)| w).sum();
        assert!((total - 1.0).abs() < 1e-12);
    }

    #[test]
    fn preset_intersects_with_legal_book_moves_and_falls_back_to_main() {
        let key = key_after(&["7g7f"]);
        let sfen = sfen_after(&["7g7f"]);
        let our = our_book();
        // 記録用の手も乱択候補に書かれていれば指す (エンジンの乱択と同じ)。非合法な手は落とす。
        let listed = preset(&format!("# preset\nsfen {sfen} 8c8d 4a3b 9a9c 9c9d 8c8d\n"));
        assert_eq!(our.choices(&key, &listed, &after_7g7f_legal), ["8c8d", "4a3b"]);

        let missing = preset(&format!("sfen {key} 9c9d\n"));
        assert_eq!(our.choices(&key, &missing, &after_7g7f_legal), ["3c3d"]);

        let later = preset(&format!("sfen {key} 8c8d\nsfen {key} 4a3b\n"));
        assert_eq!(our.choices(&key, &later, &after_7g7f_legal), ["4a3b"]);
    }

    #[test]
    fn malformed_preset_line_is_skipped_like_the_engine() {
        let key = key_after(&["7g7f"]);
        let list = parse_preset(&format!("sfen {key} 8c8d zz\nsfen bad\n"));
        assert_eq!(list.skipped_lines(), 2);
        assert!(list.is_empty());
        let reports = walk_with(&Follow::Preset(list), &our_book());
        assert_eq!(paths(&reports[0]), ["2g2f", "7g7f 3c3d 6g6f", "7g7f 3c3d 2g2f"]);
    }

    #[test]
    fn override_replaces_or_removes_our_move_and_reports_unknown_keys() {
        let key = key_after(&["7g7f"]);
        let mut our = our_book();
        assert!(our.apply_override(&format!("{key}\t4a3b\r\n")).is_empty());
        assert_eq!(our.choices(&key, &Follow::All, &after_7g7f_legal), ["4a3b"]);
        assert_eq!(our.entries[&key].main(&after_7g7f_legal), Some("4a3b"));
        our.apply_override(&format!("{key}\t\n"));
        assert!(our.choices(&key, &Follow::Main, &after_7g7f_legal).is_empty());
        let reports = walk_with(&Follow::Main, &our);
        assert_eq!(paths(&reports[0]), ["2g2f", "7g7f"]);
        assert_eq!(our.apply_override("unknown\t7g7f\n"), ["unknown"]);
    }

    #[test]
    fn median_of_even_count_truncates_toward_zero_and_best_follows_our_side() {
        let pass = |eval| Pass {
            remaining: 1,
            exit_eval: Some(eval),
            winner: "w".to_string(),
        };
        let mut row = walk_with(&Follow::Main, &our_book())[0].rows[0].clone();
        let mut black = cfg();
        black.our_side = Color::Black;
        summarize_passes(&mut row, &[pass(100), pass(-301)], &black);
        assert_eq!((row.eval_median, row.eval_best), (Some(-100), Some(-301)));
        assert_eq!(row.opponent_winrate, Some(1.0));
    }

    #[test]
    fn danger_needs_one_game_with_both_long_preparation_and_good_exit() {
        let text = [
            HEADER,
            "a\tfoo\tx\t6\t-500\t+7776FU -3334FU +2726FU -8384FU +2625FU -8485FU\tw",
            "b\tfoo\tx\t2\t900\t+7776FU -3334FU\tb",
        ]
        .join("\n");
        let mut cfg = cfg();
        cfg.danger_remaining = 3;
        let reports =
            run_walk(&text, &["foo".to_string()], &OurBook::default(), &Follow::Main, &cfg)
                .unwrap();
        let stop = row(&reports[0], "7g7f");
        assert_eq!((stop.max_remaining, stop.eval_best), (5, Some(900)));
        assert_eq!(stop.danger_games, 0);
        assert!(!stop.is_dangerous());
    }

    #[test]
    fn black_side_walks_our_first_move_and_flips_eval_direction() {
        let text = [
            HEADER,
            "g\tx\tFoo\t4\t-400\t+7776FU -3334FU +2726FU -8384FU\tw",
            "h\tFoo\ty\t4\t400\t+7776FU -3334FU +2726FU -8384FU\tb",
        ]
        .join("\n");
        let mut cfg = cfg();
        cfg.our_side = Color::Black;
        let our = book(&[(&[], &["7g7f none 50 0 1"])]);
        let reports = run_walk(&text, &["foo".to_string()], &our, &Follow::Main, &cfg).unwrap();
        assert_eq!(reports[0].games, 1);
        let stop = &reports[0].rows[0];
        assert_eq!(stop.path, ["7g7f", "3c3d"]);
        assert_eq!((stop.kind, stop.ply), (StopKind::WeThink, 3));
        assert_eq!((stop.max_remaining, stop.eval_best), (2, Some(-400)));
        assert_eq!(stop.opponent_winrate, Some(1.0));
        assert!(stop.is_dangerous());
    }

    #[test]
    fn broken_record_keeps_the_replayed_prefix() {
        let games = parse_lines(
            &format!("{HEADER}\ng\tfoo\tx\t3\t0\t+7776FU -3334XX +2726FU\tb\n"),
            &["foo".to_string()],
            Color::White,
        )
        .unwrap();
        let tree = build_tree(&games[0].1, Color::White);
        assert_eq!(tree.broken_games, 1);
        assert_eq!(tree.passes.len(), 2);
        assert_eq!(tree.moves[&key_after(&[])], [("7g7f".to_string(), 1)]);
    }

    #[test]
    fn opponent_names_match_case_insensitively_in_given_order() {
        let text = format!("{HEADER}\ng\tFooBar_1\tx\t1\t0\t+7776FU\tb\n");
        let names = ["foo".to_string(), "foobar".to_string()];
        let grouped = parse_lines(&text, &names, Color::White).unwrap();
        assert_eq!(grouped.len(), 1);
        assert_eq!(grouped[0].0, "foo");
        assert!(parse_lines(&text, &names, Color::Black).unwrap().is_empty());
    }

    #[test]
    fn prune_must_be_positive_and_min_weight_non_negative() {
        let parse = |extra: &[&str]| {
            let mut args = vec![
                "book_prepwalk",
                "--lines",
                "l",
                "--book",
                "b",
                "--out",
                "o",
                "--opponent",
                "x",
            ];
            args.extend_from_slice(extra);
            walk_config(&Cli::try_parse_from(args).unwrap())
        };
        assert!(parse(&[]).is_ok());
        assert!(parse(&["--prune", "0"]).is_err());
        assert!(parse(&["--prune=-0.1"]).is_err());
        assert!(parse(&["--min-weight=-1"]).is_err());
    }
}
