//! 初期局面から先手の main と後手の main を交互に指し、元データの定跡が切れる所を出す。

use super::audit::reject_same_file;
use super::master_index::MasterIndex;
use super::master_table::mean_eval;
use super::model::{child, side};
use super::reply_graph::HIRATE_KEY;
use super::schema::Schema;
use super::targets::{Line, MAX_DEPTH, MAX_PRIORITY, Opts};
use super::walk::{check_tag, queue_id};
use anyhow::{Context, Result, ensure};
use clap::Args;
use std::collections::BTreeSet;
use std::io::Write;
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
        let schema_paths: Vec<_> =
            self.schema.iter().flat_map(|dir| super::schema::paths(dir)).collect();
        for path in &schema_paths {
            inputs.push(("--schema", path.as_path()));
        }
        for output in [&self.table, &self.enqueue_out].into_iter().flatten() {
            reject_same_file(output, &inputs)?;
        }
        if let (Some(table), Some(out)) = (&self.table, &self.enqueue_out) {
            reject_same_file(out, &[("--table", table.as_path())])?;
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
    key: String,
    stop: Stop,
}

fn walk(index: &mut MasterIndex, max_ply: usize) -> Result<Walked> {
    let mut key = HIRATE_KEY.to_owned();
    let mut steps = Vec::new();
    let mut seen = BTreeSet::new();
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
    Ok(Walked { steps, key, stop })
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    crate::common::io::write_atomic(path, std::str::from_utf8(bytes)?)
}

pub(super) fn run(args: MainLineArgs) -> Result<()> {
    run_to(args, &mut std::io::stdout().lock())
}

fn run_to(args: MainLineArgs, stdout: &mut impl Write) -> Result<()> {
    args.check()?;
    let mut index = MasterIndex::open(&args.master, &Schema::load(args.schema.as_deref())?)?;
    let walked = walk(&mut index, args.max_ply)?;
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
    // 木の行を作って検査してから書き出し、片方だけ新しい組を残さない。
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
    Ok(())
}

#[cfg(test)]
mod tests;
