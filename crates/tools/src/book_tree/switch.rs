//! main を伝播の値で上の手に替えるかの規則と、1 回の読みの値の手を伸ばす木のキューの行。

use super::model::{child, side};
use super::refutation::SideWeights;
use super::targets::{DEFAULT_PRIORITY, Line, MAX_DEPTH, MAX_PRIORITY, Opts};
use super::walk::{check_tag, excluded_positions, queue_id};
use crate::book_backprop::{Graph, MoveValue};
use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub(super) enum SwitchRule {
    /// 値の差で決める (audit は --line、register は --keep-line)
    #[default]
    Line,
    /// 両方の手の値が手の先の局面から来ていれば差があれば替え、片方が 1 回の読みの値なら替えない
    Propagated,
}

/// 手の値が手の先の局面の値から来たか。補った相手の手番の局面 (元データに無い) で着いた手の読みの値が最善なら、
/// その値は手の 1 回の読みそのもの。最善の選び方は `tree register` の段数 (`Depths`) と揃える。
pub(super) fn from_child(graph: &Graph, in_master: impl Fn(&str) -> bool, mv: &MoveValue) -> bool {
    let Some(edge) = mv.edge else {
        return false;
    };
    let Some(best) = graph.moves(edge.to).max_by_key(|m| m.new) else {
        return false;
    };
    in_master(graph.key(edge.to)) || best.edge.is_some()
}

#[derive(Args, Debug)]
pub(super) struct SingleQueueArgs {
    /// --switch-rule propagated で、main か 1 位の値が 1 回の読みなので替えなかった局面ごとに、その局面を根にして
    /// その手を `game` に入れた tree enqueue --from の行 (JSON Lines) の出力先
    #[arg(long)]
    pub single_enqueue_out: Option<PathBuf>,
    /// キューの行の段数
    #[arg(long, default_value_t = 6, requires = "single_enqueue_out", value_parser = clap::builder::RangedU64ValueParser::<usize>::new().range(1..=MAX_DEPTH as u64))]
    pub single_depth: usize,
    /// キューの行の priority の基準。並べた順位 (0 始まり、99 で頭打ち) を足す
    #[arg(long, default_value_t = DEFAULT_PRIORITY, requires = "single_enqueue_out", value_parser = clap::value_parser!(u32).range(0..=i64::from(MAX_PRIORITY - 99)))]
    pub single_priority: u32,
    /// キューの行の数の上限 (並べた順で先から)
    #[arg(long, requires = "single_enqueue_out")]
    pub single_max: Option<usize>,
    /// キューの行の相手の手の数 (1〜8)
    #[arg(long, default_value_t = 3, requires = "single_enqueue_out")]
    pub single_opp_n: usize,
    /// キューの行の打ち切り値 (1〜30000)
    #[arg(long, default_value_t = 300, requires = "single_enqueue_out")]
    pub single_stop: i32,
    /// キューの行の読み直し回数 (0〜10)
    #[arg(long, default_value_t = 3, requires = "single_enqueue_out")]
    pub single_rounds: usize,
    /// 先手番で循環に入る手の値 (-30000〜30000)
    #[arg(long, default_value_t = -70, requires = "single_enqueue_out", allow_hyphen_values = true)]
    pub single_draw_black: i32,
    /// 後手番で循環に入る手の値 (-30000〜30000)
    #[arg(
        long,
        default_value_t = 70,
        requires = "single_enqueue_out",
        allow_hyphen_values = true
    )]
    pub single_draw_white: i32,
    /// キューの行の id `<tag>_<根の手数>_<局面の hash>_<b|w>` に入れる名前 ([A-Za-z0-9_-] の 1〜40 文字)
    #[arg(long, default_value = "single", requires = "single_enqueue_out")]
    pub single_tag: String,
    /// 既に積んだ木のキュー (JSON Lines、`master walk --exclude-queue` と同じ除外)。繰り返して複数。
    /// 局面か、1 回の読みの手を指した後の局面を同じ側で読む木があれば行を出さない
    #[arg(long, requires = "single_enqueue_out")]
    pub exclude_queue: Vec<PathBuf>,
}

impl SingleQueueArgs {
    fn opts(&self) -> Opts {
        Opts {
            opp_n: Some(self.single_opp_n),
            stop: Some(self.single_stop),
            rounds: Some(self.single_rounds),
            draw_black: Some(self.single_draw_black),
            draw_white: Some(self.single_draw_white),
            ..Opts::default()
        }
    }

    pub fn check(&self) -> Result<()> {
        check_tag(&self.single_tag)?;
        self.opts().check()
    }
}

/// main か 1 位の値が 1 回の読みなので main を替えなかった局面。値は手番の側から。
#[derive(Debug, Clone, PartialEq)]
pub(super) struct SingleRead {
    pub key: String,
    pub ply: String,
    pub main: String,
    pub main_value: i32,
    pub best: String,
    pub best_value: i32,
    /// 値が 1 回の読みの手 (main が先)。
    pub moves: Vec<String>,
    pub weight: Option<f64>,
}

/// 重みがあれば重みの大きい順 (下限未満は除く)、無ければ 1 位と main の差の大きい順に並べる。
pub(super) fn rank(reads: &mut Vec<SingleRead>, weights: Option<&SideWeights>, min_weight: f64) {
    for r in reads.iter_mut() {
        let s = usize::from(side(&r.key) == "w");
        r.weight = weights.map(|w| w[s].get(&r.key).copied().unwrap_or(0.0));
    }
    reads.retain(|r| r.weight.is_none_or(|w| w >= min_weight));
    reads.sort_by(|a, b| {
        b.weight
            .unwrap_or(0.0)
            .total_cmp(&a.weight.unwrap_or(0.0))
            .then_with(|| (b.best_value - b.main_value).cmp(&(a.best_value - a.main_value)))
            .then_with(|| a.key.cmp(&b.key))
    });
}

/// 並べた順にキューの行にする。返す数は既存の木で除いた局面の数。
pub(super) fn queue(reads: &[SingleRead], args: &SingleQueueArgs) -> Result<(Vec<Line>, usize)> {
    let excluded = excluded_positions(&args.exclude_queue)?;
    let mut kept = Vec::new();
    for r in reads {
        let own = side(&r.key);
        let mut covered = excluded.contains_key(&(r.key.clone(), own.to_owned()));
        for mv in &r.moves {
            covered |= excluded.contains_key(&(child(&r.key, mv)?, own.to_owned()));
        }
        if !covered {
            kept.push(r);
        }
    }
    let excluded = reads.len() - kept.len();
    let lines = kept
        .into_iter()
        .take(args.single_max.unwrap_or(usize::MAX))
        .enumerate()
        .map(|(rank, r)| line(r, rank, args))
        .collect::<Result<Vec<_>>>()?;
    Ok((lines, excluded))
}

fn line(r: &SingleRead, rank: usize, args: &SingleQueueArgs) -> Result<Line> {
    let ply: usize =
        r.ply.trim().parse().ok().filter(|&p| p > 0).with_context(|| {
            format!("キューの行には 1 以上の手数が要ります: {} {:?}", r.key, r.ply)
        })?;
    let own = side(&r.key);
    let weight = r.weight.map_or_else(String::new, |w| format!(" weight={w:.5}"));
    let line = Line {
        group: None,
        id: queue_id(&args.single_tag, ply, &r.key, own),
        root: format!("sfen {} {ply}", r.key),
        roots: Vec::new(),
        own: own.to_owned(),
        depth: args.single_depth,
        priority: args.single_priority + rank.min(99) as u32,
        hold: false,
        game: r.moves.iter().map(|mv| vec![mv.clone()]).collect(),
        opts: args.opts(),
        memo: Some(format!(
            "single main={} main_value={} best={} best_value={} single={}{weight}",
            r.main,
            r.main_value,
            r.best,
            r.best_value,
            r.moves.join(",")
        )),
    };
    line.check().with_context(|| format!("キューの行の検査: {}", r.key))?;
    Ok(line)
}

#[cfg(test)]
pub(super) fn test_args() -> SingleQueueArgs {
    use clap::Parser;
    #[derive(Parser)]
    struct Wrap {
        #[command(flatten)]
        single: SingleQueueArgs,
    }
    Wrap::parse_from(["test"]).single
}
