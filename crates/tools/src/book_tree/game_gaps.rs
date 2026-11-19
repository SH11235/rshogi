//! 対局の手順にある元データの局面・辺の抜けを拾う。

use super::audit::reject_same_file;
use super::exits::disk::{Sorter, read_record};
use super::master_index::MasterIndex;
use super::schema::{self, Schema};
use super::seeds;
use super::targets::{Line, MAX_DEPTH, MAX_PRIORITY, Opts, parse_root, read_checked_lines};
use super::walk::check_tag;
use crate::book_db::{apply_usi_move, position_from_sfen};
use anyhow::{Context, Result, ensure};
use clap::{Args, ValueEnum};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Until {
    Exit,
    All,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum LineSides {
    Both,
    Loser,
    Winner,
}

#[derive(Args, Debug)]
pub(super) struct GameGapsArgs {
    #[arg(long)]
    master: PathBuf,
    #[arg(long)]
    line: bool,
    #[arg(long, value_enum, default_value_t = LineSides::Both)]
    line_sides: LineSides,
    #[arg(long, default_value_t = 150)]
    collapse_cp: i32,
    #[arg(long, default_value_t = 3)]
    after_collapse: u32,
    #[arg(long)]
    schema: Option<PathBuf>,
    #[arg(long, required = true)]
    games: Vec<PathBuf>,
    #[arg(long, required_unless_present = "line")]
    player: Option<String>,
    #[arg(long, value_enum, default_value_t = Until::Exit)]
    until: Until,
    #[arg(long)]
    max_ply: Option<u32>,
    #[arg(long, default_value_t = 0)]
    skip_plies: u32,
    #[arg(long, default_value_t = 2)]
    exit_secs: u32,
    #[arg(long)]
    exclude_queue: Vec<PathBuf>,
    #[arg(long)]
    table: PathBuf,
    #[arg(long, requires_all = ["priority", "tag"])]
    enqueue_out: Option<PathBuf>,
    #[arg(long)]
    priority: Option<u32>,
    /// 局ごとの受付優先度を指定する TSV (局名、優先度)
    #[arg(long, value_name = "TSV", requires = "enqueue_out")]
    game_priority: Option<PathBuf>,
    #[arg(long, requires = "enqueue_out")]
    tag: Option<String>,
    #[arg(long, default_value_t = 3)]
    depth: usize,
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
}

impl GameGapsArgs {
    fn opts(&self) -> Opts {
        Opts {
            opp_n: Some(self.opp_n),
            stop: Some(self.stop),
            rounds: Some(self.rounds),
            draw_black: Some(self.draw_black),
            draw_white: Some(self.draw_white),
            ..Opts::default()
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
struct Move {
    ply: u32,
    side_to_move: String,
    sfen_before: String,
    move_usi: String,
    #[serde(default)]
    timed_out: bool,
    #[serde(skip)]
    searched: bool,
    #[serde(default)]
    score_black: Option<i64>,
}

#[derive(Deserialize, Serialize)]
struct Game {
    name: String,
    own: String,
    moves: Vec<Move>,
    outcome: String,
}

fn own_side(black: &str, white: &str, player: &str) -> Result<String> {
    match (black.contains(player), white.contains(player)) {
        (true, false) => Ok("b".into()),
        (false, true) => Ok("w".into()),
        _ => anyhow::bail!(
            "--player={player:?} は片側だけに一致する必要があります: {black:?} / {white:?}"
        ),
    }
}

fn validate_game(game: &mut Game) -> Result<()> {
    let mut end = None;
    let mut previous: Option<(String, u32)> = None;
    for (i, m) in game.moves.iter().enumerate() {
        let terminal =
            matches!(m.move_usi.as_str(), "resign" | "win" | "timeout" | "illegal" | "none");
        let decoded = rshogi_core::types::Move::from_usi_strict(&m.move_usi);
        ensure!(
            terminal || decoded.is_some_and(|m| m.is_normal() || m.is_pass()),
            "読めない手です: {} 手目 {}",
            m.ply,
            m.move_usi
        );
        ensure!(end.is_none(), "終局後に手の行があります: {} 手目", m.ply);
        if terminal || m.timed_out {
            end = Some(i);
            continue;
        }
        let root = parse_root(&format!("sfen {}", m.sfen_before))?;
        ensure!(
            m.ply > 0 && super::model::side(&root.key) == m.side_to_move,
            "手番か手数が不正です"
        );
        if let Some((key, ply)) = &previous {
            ensure!(
                ply.checked_add(1) == Some(m.ply) && *key == root.key,
                "対局の手順が不連続です"
            );
        }
        let mut pos = position_from_sfen(&m.sfen_before)?;
        apply_usi_move(&mut pos, &m.move_usi)?;
        previous = Some((crate::book_db::strip_ply(&pos.to_sfen()).to_owned(), m.ply));
    }
    if let Some(end) = end {
        game.moves.truncate(end);
    }
    Ok(())
}

#[derive(Default, Debug, PartialEq, Eq)]
struct CsaComment {
    pv: Option<String>,
    score: Option<i64>,
}

// CSA の共通パーサーは未知の行を読み飛ばすため、手と終局の行は先に検査する。
fn csa_comments(text: &str) -> Result<Vec<CsaComment>> {
    let mut comments = Vec::new();
    let mut ended = false;
    for (n, line) in text.lines().enumerate() {
        let s = line.trim();
        let mut pv_comment = s.strip_prefix("'**");
        if s.starts_with('%') {
            let terminal = s.split_once(',').map_or(s, |(terminal, _)| terminal);
            let terminal = if matches!(terminal, "%+ILLEGAL_ACTION" | "%-ILLEGAL_ACTION") {
                terminal
            } else {
                s
            };
            let (_, parsed, _) = rshogi_csa::parse_csa_full(terminal)?;
            ensure!(
                matches!(parsed.as_slice(), [rshogi_csa::ParsedMove::Special(_)]),
                "未知の CSA 終局行: {} 行目 {s}",
                n + 1
            );
            ended = true;
        } else if s.starts_with("'summary:") {
            ended = true;
        } else if (s.starts_with('+') || s.starts_with('-')) && s != "+" && s != "-" {
            ensure!(!ended, "CSA の終局後に手があります: {} 行目", n + 1);
            let (mv, rest) = s.split_once(',').unwrap_or((s, ""));
            let bytes = mv.as_bytes();
            ensure!(
                bytes.len() == 7
                    && bytes[1..5].iter().all(u8::is_ascii_digit)
                    && bytes[5..7].iter().all(u8::is_ascii_uppercase)
                    && (rest.is_empty() || rest.starts_with('T') || rest.starts_with('\'')),
                "読めない CSA の手: {} 行目 {s}",
                n + 1
            );
            comments.push(CsaComment::default());
            pv_comment = rest.split(',').find_map(|part| part.trim().strip_prefix("'**"));
        } else {
            ensure!(
                s.is_empty()
                    || s == "+"
                    || s == "-"
                    || s.starts_with(['V', 'N', 'P', 'T', '$', '\'']),
                "読めない CSA 行: {} 行目 {s}",
                n + 1
            );
        }
        if !ended && let Some(rest) = pv_comment {
            let mut fields = rest.split_whitespace();
            if let Some(score) = fields.next().and_then(|value| value.parse::<i32>().ok())
                && let Some(comment) = comments.last_mut()
            {
                comment.score = Some(i64::from(score));
                if let Some(first) = fields.next() {
                    comment.pv = Some(first.into());
                }
            }
        }
    }
    Ok(comments)
}

fn visit_csa(
    path: &Path,
    args: &GameGapsArgs,
    name: String,
    visit: &mut impl FnMut(Game) -> Result<()>,
) -> Result<()> {
    let text = crate::kifu_publish::read_csa_text(path)?;
    let comments = csa_comments(&text)?;
    let (mut pos, parsed, info) = rshogi_csa::parse_csa_full(&text)?;
    let own = if args.line {
        String::new()
    } else {
        own_side(
            info.black_name.as_deref().unwrap_or_default(),
            info.white_name.as_deref().unwrap_or_default(),
            args.player.as_deref().unwrap_or_default(),
        )?
    };
    let mut moves = Vec::new();
    let mut own_pv = false;
    let mut times = Vec::new();
    let mut outcome = "unknown";
    for (i, m) in parsed.into_iter().enumerate() {
        let cm = match m {
            rshogi_csa::ParsedMove::Normal(cm) => cm,
            rshogi_csa::ParsedMove::Special(special) => {
                use rshogi_csa::{Color, SpecialMove};
                let black = pos.side_to_move == Color::Black;
                let winner = match special {
                    SpecialMove::Resign | SpecialMove::TimeUp | SpecialMove::IllegalMove => {
                        Some(!black)
                    }
                    SpecialMove::Win => Some(black),
                    _ => None,
                };
                outcome = match winner {
                    Some(true) => "black_win",
                    Some(false) => "white_win",
                    None => "unknown",
                };
                break;
            }
        };
        let sfen_before = pos.to_sfen();
        let move_usi = rshogi_csa::csa_move_to_usi(&cm.mv, &pos)?;
        ensure!(rshogi_csa::usi_move_to_csa(&move_usi, &pos)? == cm.mv, "CSA の駒種が不正です");
        let root = parse_root(&format!("sfen {sfen_before}"))?;
        let mut check = position_from_sfen(&sfen_before)?;
        apply_usi_move(&mut check, &move_usi)?;
        times.push(cm.time_sec);
        let pv = comments.get(i).and_then(|c| c.pv.as_deref());
        own_pv |= super::model::side(&root.key) == own && pv.is_some();
        moves.push(Move {
            ply: root.ply,
            side_to_move: super::model::side(&root.key).into(),
            sfen_before,
            move_usi,
            timed_out: false,
            searched: pv == Some(cm.mv.as_str()),
            score_black: comments.get(i).and_then(|c| c.score),
        });
        pos.apply_csa_move(&cm.mv)?;
    }
    if !own_pv && !args.line {
        eprintln!(
            "注意: {name}: 自分の読み筋つき CSA コメントが無いため --exit-secs={} で出口を判定します",
            args.exit_secs
        );
        for (m, time) in moves.iter_mut().zip(times) {
            m.searched = time.is_some_and(|t| t >= args.exit_secs);
        }
    }
    if let Some(winner) = text.lines().find_map(|line| match line.trim().split(',').next() {
        Some("%+ILLEGAL_ACTION") => Some("white_win"),
        Some("%-ILLEGAL_ACTION") => Some("black_win"),
        _ => None,
    }) {
        outcome = winner;
    }
    if let Some(summary) = info.summary {
        outcome = match summary.result {
            rshogi_csa::SummaryResult::BlackWin => "black_win",
            rshogi_csa::SummaryResult::WhiteWin => "white_win",
            rshogi_csa::SummaryResult::Draw => "draw",
        };
    }
    visit(Game {
        name,
        own,
        moves,
        outcome: outcome.into(),
    })
}

fn visit_input(
    path: &Path,
    args: &GameGapsArgs,
    mut visit: impl FnMut(Game) -> Result<()>,
) -> Result<()> {
    let name = path
        .file_stem()
        .context("対局名がありません")?
        .to_string_lossy()
        .replace(['\t', '\r', '\n'], "_");
    let first = BufReader::new(File::open(path)?)
        .lines()
        .find_map(|line| match line {
            Ok(s) if s.trim().is_empty() => None,
            line => Some(line),
        })
        .transpose()?
        .context("棋譜が空です")?;
    if !first.trim_start().starts_with('{') {
        return visit_csa(path, args, name, &mut visit);
    }
    let mut labels = None;
    let mut pair = false;
    let mut results = 0usize;
    seeds::visit_json(path, |_, v| {
        if v["type"] == "meta" {
            labels = Some((
                v["engine_cmd"]["label_black"]
                    .as_str()
                    .context("label_black がありません")?
                    .to_owned(),
                v["engine_cmd"]["label_white"]
                    .as_str()
                    .context("label_white がありません")?
                    .to_owned(),
            ));
        }
        if v["type"] == "result" {
            results += 1;
        }
        if v["type"] == "result" && (!v["pair_index"].is_null() || !v["pair_slot"].is_null()) {
            pair = true;
        }
        Ok(())
    })?;
    ensure!(results > 0, "result がありません");
    if pair || results > 1 || labels.is_none() {
        if args.line {
            return pave::pair_disk::visit_with_player(path, &name, None, |game, _| visit(game));
        }
        return visit_pair(path, &name, args.player.as_deref(), |game, _| visit(game));
    }

    let (black, white) = labels.context("meta がありません")?;
    let own = if args.line {
        String::new()
    } else {
        own_side(&black, &white, args.player.as_deref().unwrap_or_default())?
    };
    let mut moves = Vec::new();
    let mut ended = false;
    let mut outcome = String::from("unknown");
    seeds::visit_json(path, |_, v| {
        match v["type"].as_str() {
            Some("move") => {
                ensure!(!ended, "1 ファイルに複数の対局があります");
                let mut m: Move = serde_json::from_value(v.clone())?;
                m.searched =
                    v["eval"]["pv"].as_array().and_then(|p| p.first()).and_then(|p| p.as_str())
                        == Some(&m.move_usi);
                if args.line {
                    let eval: Option<seeds::Eval> = serde_json::from_value(v["eval"].clone())?;
                    m.score_black = eval
                        .as_ref()
                        .and_then(seeds::eval_score)
                        .map(|score| if m.side_to_move == "b" { score } else { -score });
                }
                moves.push(m);
            }
            Some("result") => {
                ensure!(!ended, "result が複数あります");
                ended = true;
                outcome = v["outcome"].as_str().unwrap_or("unknown").into();
            }
            _ => {}
        }
        Ok(())
    })?;
    ensure!(ended, "result がありません");
    visit(Game {
        name,
        own,
        moves,
        outcome,
    })
}

fn visit_pair(
    path: &Path,
    name: &str,
    player: Option<&str>,
    mut visit: impl FnMut(Game, &str) -> Result<()>,
) -> Result<()> {
    let valid = seeds::valid_game_ids(path, false)?;
    let mut ended = BTreeSet::new();
    seeds::visit_json(path, |_, v| {
        if matches!(v["type"].as_str(), Some("move" | "result")) {
            let id: u32 = serde_json::from_value(v["game_id"].clone())?;
            if valid.contains_key(&id) {
                if v["type"] == "result" {
                    ended.insert(id);
                } else {
                    ensure!(!ended.contains(&id), "result 後に手があります: game_id={id}");
                }
            }
        }
        Ok(())
    })?;
    seeds::visit_games(path, &valid, |result, moves| {
        visit(pair_game(name, player, result, moves)?, &result.outcome)
    })
}

fn pair_game(
    name: &str,
    player: Option<&str>,
    result: &seeds::GameResult,
    moves: &[seeds::Move],
) -> Result<Game> {
    let engine = |side| -> Result<&str> {
        let mut names = moves.iter().filter(|m| m.side_to_move == side).map(|m| m.engine.as_str());
        let name = names.next().context("対局者名がありません")?;
        ensure!(!name.is_empty() && names.all(|n| n == name), "engine が空か不一致です");
        Ok(name)
    };
    let own = if let Some(player) = player {
        own_side(engine("b")?, engine("w")?, player)?
    } else {
        match result.outcome.as_str() {
            "black_win" => "w",
            "white_win" => "b",
            _ => "",
        }
        .into()
    };
    let moves = moves
        .iter()
        .map(|m| Move {
            ply: m.ply,
            side_to_move: m.side_to_move.clone(),
            sfen_before: m.sfen_before.clone(),
            move_usi: m.move_usi.clone(),
            timed_out: m.timed_out,
            score_black: m
                .eval
                .as_ref()
                .and_then(seeds::eval_score)
                .map(|v| if m.side_to_move == "b" { v } else { -v }),
            searched: m
                .eval
                .as_ref()
                .and_then(|e| {
                    e.pv.as_ref().or_else(|| e.last_exact_primary.as_ref().map(|p| &p.pv))
                })
                .is_some_and(|pv| pv.first() == Some(&m.move_usi)),
        })
        .collect();
    Ok(Game {
        name: format!("{name}_{}", result.game_id),
        outcome: result.outcome.clone(),
        own,
        moves,
    })
}

struct Coverage {
    id: String,
    own: String,
    depth: usize,
    game: Vec<Vec<String>>,
}
type Excluded = BTreeMap<String, Vec<Coverage>>;

fn read_excluded(paths: &[PathBuf]) -> Result<Excluded> {
    let mut ids: BTreeMap<String, (Line, super::targets::Root)> = BTreeMap::new();
    let mut rejected = 0;
    let mut folded_ids = BTreeMap::new();
    for path in paths {
        read_checked_lines(path, |line, root, _| {
            let folded = line.id.to_ascii_lowercase();
            if folded_ids.get(&folded).is_some_and(|id| id != &line.id)
                || ids.get(&line.id).is_some_and(|(first, first_root)| {
                    first.own != line.own
                        || first_root.key != root.key
                        || first_root.ply != root.ply
                })
            {
                rejected += 1;
                return Ok(());
            }
            folded_ids.insert(folded, line.id.clone());
            ids.insert(line.id.clone(), (line, root));
            Ok(())
        })?;
    }
    if rejected > 0 {
        eprintln!(
            "注意: 受付と同じ id の根・手数・own の変更または大小文字だけ異なる id を {rejected} 行無視しました"
        );
    }
    let mut out: Excluded = BTreeMap::new();
    for (id, (line, root)) in ids {
        let mut roots = vec![(root, line.game)];
        for additional in &line.roots {
            roots.push((parse_root(additional)?, Vec::new()));
        }
        for (root, game) in roots {
            out.entry(root.key).or_default().push(Coverage {
                id: id.clone(),
                own: line.own.clone(),
                depth: line.depth,
                game,
            });
        }
    }
    Ok(out)
}

fn covered_ids(game: &Game, i: usize, active: &[(usize, &Coverage)]) -> BTreeSet<String> {
    active
        .iter()
        .filter(|(start, row)| {
            let actual = &game.moves[*start..=i];
            row.game.iter().any(|line| {
                line.len() >= actual.len()
                    && line.iter().zip(actual).all(|(mv, actual)| *mv == actual.move_usi)
            })
        })
        .map(|(_, row)| row.id.clone())
        .collect()
}

fn short_name(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(12)
        .collect();
    if s.is_empty() { "game".into() } else { s }
}

fn read_game_priorities(path: &Path) -> Result<BTreeMap<String, (u32, bool)>> {
    let mut priorities = BTreeMap::new();
    for (index, line) in BufReader::new(File::open(path)?).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let row = index + 1;
        let (name, priority) = line
            .split_once('\t')
            .with_context(|| format!("{row} 行目: 局名と優先度の 2 列が必要です"))?;
        ensure!(!name.is_empty(), "{row} 行目: 局名が空です");
        let priority = priority
            .trim()
            .parse::<u32>()
            .with_context(|| format!("{row} 行目: 優先度は 0〜{MAX_PRIORITY} の整数が必要です"))?;
        ensure!(
            priority <= MAX_PRIORITY,
            "{row} 行目: 優先度は 0〜{MAX_PRIORITY} の整数が必要です"
        );
        ensure!(
            priorities.insert(name.to_owned(), (priority, false)).is_none(),
            "{row} 行目: 局名が重複しています: {name:?}"
        );
    }
    Ok(priorities)
}

struct Report<'a> {
    args: &'a GameGapsArgs,
    master: MasterIndex,
    excluded: Excluded,
    table: File,
    queue: Sorter,
    game_priorities: BTreeMap<String, (u32, bool)>,
}

impl Report<'_> {
    fn game(&mut self, mut game: Game, path: &Path) -> Result<()> {
        validate_game(&mut game)?;
        let priority = self
            .game_priorities
            .get_mut(&game.name)
            .map(|(priority, seen)| {
                *seen = true;
                *priority
            })
            .or(self.args.priority);
        if self.args.line {
            return self.line_game(&game, path, priority);
        }
        let args = self.args;
        let table_priority = priority.map_or_else(String::new, |p| p.to_string());
        let mut gaps = 0;
        let mut uncovered = 0;
        let mut active = Vec::<(usize, &Coverage)>::new();
        let mut generated_end = 0u64;
        let exit = game
            .moves
            .iter()
            .find(|m| m.side_to_move == game.own && m.searched)
            .map(|m| m.ply);
        if exit.is_some_and(|ply| ply <= 4) {
            eprintln!(
                "注意: {}: 定跡の出口が 4 手目以内です ({} 手目)",
                game.name,
                exit.context("出口がありません")?
            );
        }
        for (i, m) in game.moves.iter().enumerate() {
            let root = parse_root(&format!("sfen {}", m.sfen_before))?;
            if args.max_ply.is_some_and(|max| m.ply > max)
                || (args.until == Until::Exit && exit.is_some_and(|e| m.ply >= e))
            {
                break;
            }
            let ply = u64::from(m.ply);
            active.retain(|(start, row)| i - start < row.depth);
            if let Some(rows) = self.excluded.get(&root.key) {
                active.extend(rows.iter().filter(|r| r.depth > 0).map(|r| (i, r)));
            }
            if m.ply <= args.skip_plies {
                continue;
            }
            let rows = self.master.rows(&root.key)?;
            let kind = if rows.is_empty() {
                "no-position"
            } else if rows.iter().any(|r| r.get("move") == m.move_usi) {
                continue;
            } else {
                "no-move"
            };
            gaps += 1;
            let covered = covered_ids(&game, i, &active).into_iter().collect::<Vec<_>>().join(",");
            writeln!(
                self.table,
                "{}\t{}\t{}\t{}\t{kind}\t{covered}\t{}\t{table_priority}",
                game.name, m.ply, m.side_to_move, m.move_usi, root.key
            )?;
            if !covered.is_empty() {
                continue;
            }
            uncovered += 1;
            if args.enqueue_out.is_none() || ply < generated_end {
                continue;
            }
            generated_end = ply + args.depth.min(8) as u64;
            let hash = format!("{:x}", Sha256::digest(&root.key));
            let line = Line {
                id: format!(
                    "{}_{}_{}_{}_{}",
                    args.tag.as_deref().context("--tag が必要です")?,
                    short_name(&game.name),
                    m.ply,
                    &hash[..10],
                    game.own
                ),
                root: format!("sfen {}", m.sfen_before),
                roots: vec![],
                own: game.own.clone(),
                depth: args.depth,
                priority: priority.context("--priority が必要です")?,
                hold: false,
                game: vec![game.moves[i..].iter().take(8).map(|m| m.move_usi.clone()).collect()],
                opts: args.opts(),
                group: None,
                memo: Some(format!(
                    "{} の手順で元データに無い手 ({kind}) をつなぐ根 {} 手目",
                    game.name, m.ply
                )),
            };
            line.check()?;
            self.queue.push(
                vec![
                    root.key,
                    game.own.clone(),
                    serde_json::to_string(&line.game[0])?,
                ],
                serde_json::to_string(&line)?,
            )?;
        }
        eprintln!(
            "{}: 抜け {gaps} / 覆っていない {uncovered} / 定跡を出た手数 {}",
            game.name,
            exit.map_or_else(|| "なし".into(), |p| p.to_string())
        );
        Ok(())
    }
}

pub(super) fn run(mut args: GameGapsArgs) -> Result<()> {
    ensure!(!args.line || args.collapse_cp > 0, "collapse-cp は正の値が必要です");
    ensure!(
        args.line || args.player.as_ref().is_some_and(|player| !player.is_empty()),
        "--player は空にできません"
    );
    ensure!(args.depth > 0 && args.depth <= MAX_DEPTH, "depth は 1〜{MAX_DEPTH} が必要です");
    args.opts().check()?;
    ensure!(args.priority.is_none_or(|p| p <= MAX_PRIORITY), "priority が大きすぎます");
    ensure!(
        args.game_priority.is_none() || args.enqueue_out.is_some(),
        "--game-priority には --enqueue-out が必要です"
    );
    if args.enqueue_out.is_some() {
        check_tag(args.tag.as_deref().context("--enqueue-out には --tag が必要です")?)?;
        if args.line {
            ensure!(
                args.tag.as_ref().is_some_and(|tag| tag.len() <= 16),
                "--line の --tag は 16 文字以内が必要です (id の上限は 64 文字)"
            );
        }
        ensure!(args.priority.is_some(), "--enqueue-out には --priority が必要です");
    }
    args.games = args.games.iter().map(|p| p.canonicalize()).collect::<std::io::Result<_>>()?;
    args.games.sort();
    args.games.dedup();
    let schema_paths = args.schema.as_deref().map(schema::paths).unwrap_or_default();
    let mut inputs = vec![("--master", args.master.as_path())];
    inputs.extend(args.games.iter().map(|p| ("--games", p.as_path())));
    inputs.extend(args.exclude_queue.iter().map(|p| ("--exclude-queue", p.as_path())));
    if let Some(path) = &args.game_priority {
        inputs.push(("--game-priority", path));
    }
    if args.schema.is_some() {
        inputs.extend(schema_paths.iter().map(|p| ("--schema", p.as_path())));
    }
    reject_same_file(&args.table, &inputs)?;
    inputs.push(("--table", &args.table));
    if let Some(p) = &args.enqueue_out {
        reject_same_file(p, &inputs)?;
    }
    let mut report = Report {
        args: &args,
        master: MasterIndex::open(&args.master, &Schema::load(args.schema.as_deref())?)?,
        excluded: read_excluded(&args.exclude_queue)?,
        table: tempfile::tempfile()?,
        queue: Sorter::new(),
        game_priorities: args
            .game_priority
            .as_deref()
            .map(|path| {
                read_game_priorities(path)
                    .with_context(|| format!("--game-priority: {}", path.display()))
            })
            .transpose()?
            .unwrap_or_default(),
    };
    writeln!(
        report.table,
        "{}",
        if args.line {
            "game\tply\tside\town\tend_reason\tgame_moves\tcovered_by\tkey\tpriority"
        } else {
            "game\tply\tside\tmove\tkind\tcovered_by\tkey\tpriority"
        }
    )?;
    for path in &args.games {
        visit_input(path, &args, |game| report.game(game, path))
            .with_context(|| format!("棋譜: {}", path.display()))?;
    }
    let unknown: Vec<_> = report
        .game_priorities
        .into_iter()
        .filter_map(|(name, (_, seen))| (!seen).then_some(name))
        .collect();
    ensure!(
        unknown.is_empty(),
        "--game-priority に未知の局名があります (見出しの行は置かない): {}",
        unknown.iter().map(|n| format!("{n:?}")).collect::<Vec<_>>().join(", ")
    );
    let mut queue = merge_queue(report.queue)?;
    copy_output(&mut report.table, &args.table)?;
    if let Some(p) = &args.enqueue_out {
        copy_output(&mut queue, p)?;
    }
    Ok(())
}

fn copy_output(file: &mut File, path: &Path) -> Result<()> {
    use std::io::{Seek, SeekFrom};
    file.seek(SeekFrom::Start(0))?;
    // 出力全体をメモリに載せず、検査完了後に一時ファイルから書き出す。
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let mut out = tempfile::NamedTempFile::new_in(parent)?;
    std::io::copy(file, &mut out)?;
    out.persist(path).map_err(|e| e.error)?;
    Ok(())
}

fn merge_queue(queue: Sorter) -> Result<File> {
    use std::io::{Seek, SeekFrom};
    fn flush(out: &mut File, first: &Line, games: &mut File) -> Result<()> {
        let mut value = serde_json::to_value(first)?;
        value.as_object_mut().context("受付行が object ではありません")?.remove("game");
        let mut prefix = serde_json::to_string(&value)?;
        ensure!(prefix.pop() == Some('}'), "受付行が object ではありません");
        out.write_all(prefix.as_bytes())?;
        out.write_all(b",\"game\":[")?;
        games.seek(SeekFrom::Start(0))?;
        std::io::copy(games, out)?;
        out.write_all(b"]}\n")?;
        games.set_len(0)?;
        games.seek(SeekFrom::Start(0))?;
        Ok(())
    }
    let mut rows = queue.finish()?;
    let mut out = tempfile::tempfile()?;
    let mut games = tempfile::tempfile()?;
    let mut root = None;
    let mut first: Option<Line> = None;
    let mut last_game = String::new();
    while let Some(row) = read_record(&mut rows)? {
        let next = (row.key[0].clone(), row.key[1].clone());
        let mut line: Line = serde_json::from_str(&row.value)?;
        if root.as_ref() != Some(&next) {
            if let Some(first) = &first {
                flush(&mut out, first, &mut games)?;
            }
            first = Some(line);
            root = Some(next);
            last_game.clear();
        } else if let Some(first) = &mut first {
            let extend = line.opts.extend.take();
            let previous = first.opts.extend.take();
            ensure!(first.opts == line.opts, "同じ根・own の opts が不一致です");
            first.opts.extend = previous.max(extend);
            first.priority = first.priority.min(line.priority);
        }
        if last_game != row.key[2] {
            if !last_game.is_empty() {
                games.write_all(b",")?;
            }
            games.write_all(row.key[2].as_bytes())?;
            last_game = row.key[2].clone();
        }
    }
    if let Some(first) = &first {
        flush(&mut out, first, &mut games)?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests;

pub(super) mod pave;

mod line;
