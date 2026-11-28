//! 初期局面または指定手順の後から、先手の main と後手の main を交互に辿る。

use super::audit::reject_same_file;
use super::master_index::MasterIndex;
use super::master_table::mean_eval;
use super::model::{child, side};
use super::reply_graph::HIRATE_KEY;
use super::schema::Schema;
use super::targets::{Line, MAX_DEPTH, MAX_PRIORITY, Opts};
use super::walk::{check_tag, queue_id};
use crate::book_db::{apply_usi_move, strip_ply};
use crate::kif::{eval_comment_lines, move_text};
use crate::selfplay::EvalLog;
use anyhow::{Context, Result, ensure};
use clap::Args;
use rshogi_core::{position::Position, types::Move};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

#[derive(Args, Debug)]
pub(super) struct MainLineArgs {
    /// 定跡の元データの表 (TSV)
    #[arg(long)]
    master: PathBuf,
    /// 語彙と読み条件の表を置いたディレクトリ
    #[arg(long)]
    schema: Option<PathBuf>,
    /// 辿った手順の表 (TSV)
    #[arg(long)]
    table: Option<PathBuf>,
    /// 初期局面から main を辿り始める局面までの USI 手順 (空白区切り)
    #[arg(long)]
    start_moves: Option<String>,
    /// 初期局面からの手順を書き出す KIF
    #[arg(long)]
    kif: Option<PathBuf>,
    /// KIF に付ける伝播値の定跡 DB (master audit --out-db)
    #[arg(long, requires = "kif")]
    values_db: Option<PathBuf>,
    /// 初期局面から辿る手数の上限
    #[arg(long, default_value_t = 400)]
    max_ply: usize,
    /// 切れた局面を根にした own=bw の木の行 (tree enqueue --from に渡す JSON Lines)
    #[arg(long)]
    enqueue_out: Option<PathBuf>,
    #[arg(long, default_value_t = 10)]
    priority: u32,
    #[arg(long, default_value = "mainline")]
    tag: String,
    #[arg(long, default_value_t = 6)]
    depth: usize,
    #[arg(long)]
    opp_n: Option<usize>,
    #[arg(long)]
    stop: Option<i32>,
    #[arg(long)]
    rounds: Option<usize>,
    #[arg(long, allow_hyphen_values = true)]
    draw_black: Option<i32>,
    #[arg(long, allow_hyphen_values = true)]
    draw_white: Option<i32>,
}

impl MainLineArgs {
    fn opts(&self) -> Opts {
        Opts {
            opp_n: self.opp_n,
            stop: self.stop,
            rounds: self.rounds,
            draw_black: self.draw_black,
            draw_white: self.draw_white,
            ..Opts::default()
        }
    }

    fn check(&self) -> Result<()> {
        check_tag(&self.tag)?;
        self.opts().check()?;
        ensure!(self.depth <= MAX_DEPTH, "--depth は {MAX_DEPTH} 以下にしてください");
        ensure!(self.priority <= MAX_PRIORITY, "--priority は {MAX_PRIORITY} 以下にしてください");
        let mut inputs = vec![("--master", self.master.as_path())];
        if let Some(path) = &self.values_db {
            inputs.push(("--values-db", path.as_path()));
        }
        let schema_paths: Vec<_> =
            self.schema.iter().flat_map(|dir| super::schema::paths(dir)).collect();
        for path in &schema_paths {
            inputs.push(("--schema", path.as_path()));
        }
        for (flag, path) in [
            ("--table", self.table.as_deref()),
            ("--enqueue-out", self.enqueue_out.as_deref()),
            ("--kif", self.kif.as_deref()),
        ] {
            if let Some(path) = path {
                reject_same_file(path, &inputs)?;
                inputs.push((flag, path));
            }
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Stop {
    /// 局面が元データに無い (反転キーでも無い)。
    Missing,
    /// 反転キーでだけ元データにある。FlippedBook を使わない対局ではここで定跡が切れる。
    FlippedOnly,
    /// 局面はあるが main の行が無い。
    NoMain(Vec<String>),
    /// 同じ局面に戻った。
    Cycle,
    /// 手数の上限に達した。
    MaxPly,
}
impl Stop {
    fn name(&self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::FlippedOnly => "flipped-only",
            Self::NoMain(_) => "no-main",
            Self::Cycle => "cycle",
            Self::MaxPly => "max-ply",
        }
    }
    fn detail(&self) -> String {
        match self {
            Self::Missing => "局面が元データに無い (反転キーも無い)".into(),
            Self::FlippedOnly => "反転キーでだけ元データにある (直接キーでは切れる)".into(),
            Self::NoMain(roles) => format!("局面はあるが main が無い (role: {})", roles.join(",")),
            Self::Cycle => "同じ局面に戻った".into(),
            Self::MaxPly => "手数の上限に達した".into(),
        }
    }
    /// 木を読めば定跡が伸びる切れ方か。循環と手数の上限は読む局面ではない。
    fn readable(&self) -> bool {
        matches!(self, Self::Missing | Self::FlippedOnly | Self::NoMain(_))
    }
}

struct Step {
    key: String,
    mv: String,
    value_black: Option<i64>,
}

struct Walked {
    steps: Vec<Step>,
    start_ply: usize,
    key: String,
    stop: Stop,
}

fn walk(
    index: &mut MasterIndex,
    max_ply: usize,
    mut key: String,
    mut steps: Vec<Step>,
) -> Result<Walked> {
    let start_ply = steps.len();
    let mut seen: BTreeSet<_> = steps.iter().map(|step| step.key.clone()).collect();
    let stop = loop {
        if !seen.insert(key.clone()) {
            break Stop::Cycle;
        }
        let rows = index.rows(&key)?;
        if rows.is_empty() {
            let flipped = rshogi_book::flipped_key(&format!("{key} 1"))
                .with_context(|| format!("局面を反転できません: {key}"))?;
            let flipped = crate::book_db::strip_ply(&flipped).to_owned();
            break if index.rows(&flipped)?.is_empty() {
                Stop::Missing
            } else {
                Stop::FlippedOnly
            };
        }
        let mains: Vec<_> = rows.iter().filter(|r| r.get("role") == "main").collect();
        // 索引は行ごとの検査しかしないので、局面ごとに main が 1 つという元データの規則をここで確かめる。
        ensure!(mains.len() <= 1, "main が {} 行ある局面です: {key}", mains.len());
        let Some(main) = mains.first() else {
            let roles: BTreeSet<_> = rows.iter().map(|r| r.get("role").to_owned()).collect();
            break Stop::NoMain(roles.into_iter().collect());
        };
        if steps.len() >= max_ply {
            break Stop::MaxPly;
        }
        let sign = if side(&key) == "b" { 1 } else { -1 };
        let mv = main.get("move").to_owned();
        let next = child(&key, &mv).with_context(|| format!("元データの手: {key} {mv}"))?;
        steps.push(Step {
            key: std::mem::replace(&mut key, next),
            value_black: mean_eval(main.get("evals"))?.map(|v| i64::from(v) * sign),
            mv,
        });
    };
    Ok(Walked {
        steps,
        start_ply,
        key,
        stop,
    })
}

fn start_path(moves: &str, max_ply: usize) -> Result<(String, Vec<Step>)> {
    let mut key = HIRATE_KEY.to_owned();
    let mut steps = Vec::new();
    for (i, mv) in moves.split_whitespace().enumerate() {
        ensure!(
            Move::from_usi_strict(mv).is_some_and(|m| !m.is_pass() && !m.is_none()),
            "--start-moves の {} 手目: USI 指し手が不正です: {mv}",
            i + 1
        );
        let next =
            child(&key, mv).with_context(|| format!("--start-moves の {} 手目: {mv}", i + 1))?;
        ensure!(i < max_ply, "--start-moves の手数が --max-ply {max_ply} を超えています");
        steps.push(Step {
            key: std::mem::replace(&mut key, next),
            mv: mv.to_owned(),
            value_black: None,
        });
    }
    Ok((key, steps))
}

type LineValues<'a> = BTreeMap<&'a str, BTreeMap<&'a str, Option<i32>>>;

fn read_values<'a>(path: &Path, steps: &'a [Step]) -> Result<LineValues<'a>> {
    // 定跡 DB の大きさに関わらず、棋譜に書く手の値だけを保持する。
    let mut values = LineValues::new();
    for step in steps {
        values.entry(&step.key).or_default().insert(&step.mv, None);
    }
    let mut remaining: usize = values.values().map(BTreeMap::len).sum();
    let mut key = None;
    let file = File::open(path)
        .with_context(|| format!("--values-db を開けません: {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    let mut line_number = 0;
    while remaining > 0 {
        line.clear();
        line_number += 1;
        if reader.read_line(&mut line).with_context(|| {
            format!("--values-db {} の {line_number} 行目を読めません", path.display())
        })? == 0
        {
            break;
        }
        let text = line.trim();
        let mut fields = text.split_whitespace();
        if let Some(sfen) = text.strip_prefix("sfen ") {
            key = values.get_key_value(strip_ply(sfen)).map(|(&k, _)| k);
        } else if let Some(key) = key
            && let Some(mv) = fields.next()
            && let Some(value) = values.get_mut(key).and_then(|moves| moves.get_mut(mv))
        {
            let parsed = fields.nth(1).and_then(|field| field.parse::<i32>().ok()).with_context(
                || {
                    format!(
                        "--values-db {} の {line_number} 行目: {mv} の評価値欄に i32 の整数が必要です",
                        path.display()
                    )
                },
            )?;
            if value.is_none() {
                *value = Some(parsed);
                remaining -= 1;
            }
        }
    }
    Ok(values)
}

fn render_kif(walked: &Walked, values_db: Option<&Path>) -> Result<Vec<u8>> {
    let values = values_db
        .map(|path| read_values(path, &walked.steps))
        .transpose()?
        .unwrap_or_default();
    let mut bytes = "手合割：平手\n先手：\n後手：\n手数----指手---------消費時間--\n"
        .as_bytes()
        .to_vec();
    let mut pos = Position::new();
    pos.set_hirate();
    let mut previous_dest = None;
    for (i, step) in walked.steps.iter().enumerate() {
        let ply = i + 1;
        let mv =
            Move::from_usi(&step.mv).with_context(|| format!("KIF の {ply} 手目: {}", step.mv))?;
        writeln!(bytes, "{ply:>4} {}", move_text(&pos, mv, previous_dest))?;
        let mover = pos.side_to_move();
        apply_usi_move(&mut pos, &step.mv)?;
        previous_dest = Some(mv.to());
        if let Some(&Some(value)) =
            values.get(step.key.as_str()).and_then(|moves| moves.get(step.mv.as_str()))
        {
            let eval = EvalLog {
                score_cp: Some(value),
                ..EvalLog::default()
            };
            for comment in eval_comment_lines(&pos, mover, mv, Some(&eval)) {
                writeln!(bytes, "{comment}")?;
            }
        }
        if ply == walked.start_ply {
            writeln!(bytes, "*ここから定跡の main")?;
        }
    }
    let ply = walked.steps.len();
    writeln!(bytes, "{:>4} 中断\n\nまで{ply}手で中断", ply + 1)?;
    Ok(bytes)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    crate::common::io::write_atomic(path, std::str::from_utf8(bytes)?)
}

pub(super) fn run(args: MainLineArgs) -> Result<()> {
    run_to(args, &mut std::io::stdout().lock())
}

fn run_to(args: MainLineArgs, stdout: &mut impl Write) -> Result<()> {
    args.check()?;
    let (key, steps) = start_path(args.start_moves.as_deref().unwrap_or_default(), args.max_ply)?;
    let mut index = MasterIndex::open(&args.master, &Schema::load(args.schema.as_deref())?)?;
    let walked = walk(&mut index, args.max_ply, key, steps)?;
    let moves: Vec<_> = walked.steps.iter().map(|s| s.mv.as_str()).collect();
    let ply = walked.steps.len();
    let turn = if side(&walked.key) == "b" {
        "先手"
    } else {
        "後手"
    };
    writeln!(
        stdout,
        "{ply} 手で切れる。理由: {} ({})。次は{turn}番",
        walked.stop.name(),
        walked.stop.detail()
    )?;
    writeln!(stdout, "moves={}", moves.join(" "))?;
    writeln!(stdout, "sfen {} {}", walked.key, ply + 1)?;
    // 全出力の内容を検査してからファイルを書き出す。
    let kif = args
        .kif
        .as_ref()
        .map(|_| render_kif(&walked, args.values_db.as_deref()))
        .transpose()?;
    let queued = if args.enqueue_out.is_some() {
        let mut bytes = Vec::new();
        if walked.stop.readable() {
            let root_ply = ply + 1;
            let line = Line {
                id: queue_id(&args.tag, root_ply, &walked.key, "bw"),
                root: format!("sfen {} {root_ply}", walked.key),
                roots: Vec::new(),
                own: "bw".into(),
                depth: args.depth,
                priority: args.priority,
                hold: false,
                game: Vec::new(),
                opts: args.opts(),
                memo: Some(format!(
                    "main-line reason={} moves={}",
                    walked.stop.name(),
                    moves.join(" ")
                )),
                group: None,
            };
            line.check()?;
            serde_json::to_writer(&mut bytes, &line)?;
            writeln!(bytes)?;
        } else {
            writeln!(stdout, "{} は読む局面ではないので木の行を出しません", walked.stop.name())?;
        }
        Some(bytes)
    } else {
        None
    };
    if let Some(path) = &args.table {
        let mut bytes = b"ply\tkey\tmove\tvalue_black\treason\n".to_vec();
        for (i, step) in walked.steps.iter().enumerate() {
            writeln!(
                bytes,
                "{i}\t{}\t{}\t{}\t",
                step.key,
                step.mv,
                step.value_black.map(|v| v.to_string()).unwrap_or_default()
            )?;
        }
        writeln!(bytes, "{ply}\t{}\t\t\t{}", walked.key, walked.stop.name())?;
        write_atomic(path, &bytes)?;
    }
    if let (Some(path), Some(bytes)) = (&args.enqueue_out, &queued) {
        write_atomic(path, bytes)?;
    }
    if let (Some(path), Some(bytes)) = (&args.kif, &kif) {
        write_atomic(path, bytes)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
