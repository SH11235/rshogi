//! 版ごとの出口の報告: walk の止まりと伝播の切れ目を、有望・塞ぎたい・浅い・値の出所が弱いに分ける。

use super::audit::reject_same_file;
use super::master_index::MasterIndex;
use super::master_table::mean_eval;
use super::model::{child, side};
use super::queue_review::{TreeState, tree_states};
use super::reply_graph::HIRATE_KEY;
use super::schema::Schema;
use crate::kif::usi_moves_to_short_kif;
use anyhow::{Context, Result, ensure};
use clap::Args;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

#[derive(Args, Debug)]
pub(super) struct ExitReportArgs {
    /// 定跡の元データの表 (TSV)。RAMU の最後の手の値と出所を引く
    #[arg(long)]
    master: PathBuf,
    /// 語彙と読み条件の表を置いたディレクトリ
    #[arg(long)]
    schema: Option<PathBuf>,
    /// `master walk --table` の表 (先手・後手の両方を含めてよい)
    #[arg(long)]
    walk: PathBuf,
    /// `master rejoin --table` の表
    #[arg(long)]
    rejoin: Option<PathBuf>,
    /// `master audit --bridge-replies --bridge-table` の表
    #[arg(long, requires = "bridge_audit")]
    bridge: Option<PathBuf>,
    /// 同じ audit の `--table` (main が 1 位から負ける差)
    #[arg(long, requires = "bridge")]
    bridge_audit: Option<PathBuf>,
    /// 受付と進みの写しを置いた台 (NAME=DIR、繰り返し可)。`tree queue-review --box` と同じ形
    #[arg(long = "box")]
    box_specs: Vec<String>,
    /// 出口 (walk の止まり) を載せる重みの下限
    #[arg(long, default_value_t = 0.008)]
    min_weight: f64,
    /// 伝播の切れ目と rejoin を載せる重みの下限
    #[arg(long, default_value_t = 0.001)]
    gap_min_weight: f64,
    /// main が 1 位から負ける差がこれ以上なら、補いで main が替わる局面とみなす
    #[arg(long, default_value_t = 40)]
    gap_line: i32,
    #[arg(long, default_value_t = 50, allow_hyphen_values = true)]
    target_black: i32,
    #[arg(long, default_value_t = 80, allow_hyphen_values = true)]
    target_white: i32,
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

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Class {
    Good,
    Bad,
    Gap,
    Rejoin,
    Shallow,
    Weak,
}
impl Class {
    fn name(self) -> &'static str {
        match self {
            Self::Good => "good",
            Self::Bad => "bad",
            Self::Gap => "gap",
            Self::Rejoin => "rejoin",
            Self::Shallow => "shallow",
            Self::Weak => "weak",
        }
    }
}

struct Item {
    class: Class,
    side: String,
    /// 出口は出口の局面までに指した手数、切れ目は局面の手番の手が何手目か、rejoin は表の ply。
    ply: String,
    weight: f64,
    value_black: Option<i64>,
    kind: String,
    key: String,
    moves: Vec<String>,
    /// 分類ごとの追加の欄 (名前, 値)。
    extra: Vec<(&'static str, String)>,
}

fn read_tsv(path: &Path, mut visit: impl FnMut(&BTreeMap<&str, &str>) -> Result<()>) -> Result<()> {
    let mut lines = BufReader::new(
        File::open(path).with_context(|| format!("開けません: {}", path.display()))?,
    )
    .lines();
    let header = lines.next().with_context(|| format!("表が空です: {}", path.display()))??;
    let columns: Vec<String> = header.split('\t').map(str::to_owned).collect();
    for (n, text) in lines.enumerate() {
        let text = text?;
        if text.is_empty() {
            continue;
        }
        let cells: Vec<_> = text.split('\t').collect();
        ensure!(cells.len() == columns.len(), "{}:{}: 列数が不正です", path.display(), n + 2);
        let row: BTreeMap<&str, &str> =
            columns.iter().map(String::as_str).zip(cells.iter().copied()).collect();
        visit(&row).with_context(|| format!("{}:{}", path.display(), n + 2))?;
    }
    Ok(())
}

fn column<'a>(row: &BTreeMap<&str, &'a str>, name: &str) -> Result<&'a str> {
    row.get(name).copied().with_context(|| format!("列がありません: {name}"))
}

impl ExitReportArgs {
    fn meets(&self, value: Option<i64>, side: &str) -> Option<bool> {
        value.map(|v| {
            if side == "b" {
                v >= i64::from(self.target_black)
            } else {
                v < i64::from(self.target_white)
            }
        })
    }

    fn check(&self) -> Result<()> {
        ensure!(
            self.table.is_some() || self.markdown.is_some(),
            "--table か --markdown の出力先が必要です"
        );
        for w in [self.min_weight, self.gap_min_weight] {
            ensure!(w.is_finite() && w >= 0.0, "重みの下限は有限の非負数にしてください: {w}");
        }
        let mut inputs = vec![
            ("--master", self.master.as_path()),
            ("--walk", self.walk.as_path()),
        ];
        for (name, path) in [
            ("--rejoin", &self.rejoin),
            ("--bridge", &self.bridge),
            ("--bridge-audit", &self.bridge_audit),
        ] {
            if let Some(path) = path {
                inputs.push((name, path.as_path()));
            }
        }
        let schema_paths: Vec<_> =
            self.schema.iter().flat_map(|dir| super::schema::paths(dir)).collect();
        for path in &schema_paths {
            inputs.push(("--schema", path.as_path()));
        }
        for output in [&self.table, &self.markdown].into_iter().flatten() {
            reject_same_file(output, &inputs)?;
        }
        if let (Some(table), Some(markdown)) = (&self.table, &self.markdown) {
            reject_same_file(markdown, &[("--table", table.as_path())])?;
        }
        Ok(())
    }
}

/// 手順を辿り、各局面のキー (指す前) を返す。最後の局面は含めない。
fn keys_before(moves: &[String]) -> Result<Vec<String>> {
    let mut key = HIRATE_KEY.to_owned();
    let mut keys = Vec::with_capacity(moves.len());
    for mv in moves {
        let next = child(&key, mv).with_context(|| format!("手順の手が不正です: {mv}"))?;
        keys.push(std::mem::replace(&mut key, next));
    }
    Ok(keys)
}

/// 手順の中で、`own` の側が最後に指した定跡の手の値 (先手から見た値) と出所。
fn last_own_value(
    index: &mut MasterIndex,
    moves: &[String],
    keys: &[String],
    own: &str,
) -> Result<(Option<i64>, String)> {
    let mut last = (None, String::new());
    for (key, mv) in keys.iter().zip(moves) {
        if side(key) != own {
            continue;
        }
        if let Some(row) = index.rows(key)?.into_iter().find(|r| r.get("move") == mv) {
            let sign = if own == "b" { 1 } else { -1 };
            last = (
                mean_eval(row.get("evals"))?.map(|v| i64::from(v) * sign),
                row.get("value_kind").to_owned(),
            );
        }
    }
    Ok(last)
}

pub(super) fn run(args: ExitReportArgs) -> Result<()> {
    run_to(args, &mut std::io::stdout().lock())
}

fn run_to(args: ExitReportArgs, stdout: &mut impl Write) -> Result<()> {
    args.check()?;
    let (states, box_inputs) = tree_states(&args.box_specs)?;
    for output in [&args.table, &args.markdown].into_iter().flatten() {
        let inputs: Vec<_> = box_inputs.iter().map(|p| ("--box", p.as_path())).collect();
        reject_same_file(output, &inputs)?;
    }
    let mut index = MasterIndex::open(&args.master, &Schema::load(args.schema.as_deref())?)?;
    let mut items = Vec::new();
    // 伝播の切れ目の重みは、その局面を通るか止まりの局面とする walk の行の重みの和 (同じ行で同じ局面は 1 回)。
    // 手順は bridge の表の局面の分だけ持つ (walk の全局面の手順は持たない)。
    let mut bridge_keys = BTreeSet::new();
    if let Some(bridge) = &args.bridge {
        read_tsv(bridge, |row| {
            bridge_keys.insert(column(row, "key")?.to_owned());
            Ok(())
        })?;
    }
    let mut through: BTreeMap<String, (f64, Vec<String>)> = BTreeMap::new();
    let mut walk_rows = Vec::new();
    read_tsv(&args.walk, |row| {
        let side = column(row, "side")?;
        ensure!(matches!(side, "b" | "w"), "side は b / w です: {side}");
        let weight: f64 = column(row, "weight")?.parse().context("weight が不正です")?;
        ensure!(weight.is_finite() && weight >= 0.0, "weight が不正です: {weight}");
        let moves: Vec<String> =
            column(row, "moves")?.split_whitespace().map(str::to_owned).collect();
        let keys = keys_before(&moves)?;
        let last = column(row, "key")?.to_owned();
        let mut seen = BTreeSet::new();
        // 止まりの局面にも行の重みで着くので、手順の途中の局面と同じく数える。
        for (i, key) in keys.iter().chain(std::iter::once(&last)).enumerate() {
            if bridge_keys.contains(key) && seen.insert(key) {
                let entry =
                    through.entry(key.clone()).or_insert_with(|| (0.0, moves[..i].to_vec()));
                entry.0 += weight;
            }
        }
        let stop = column(row, "stop")?;
        if weight >= args.min_weight && matches!(stop, "own" | "out" | "rare" | "max") {
            walk_rows.push((
                side.to_owned(),
                stop.to_owned(),
                column(row, "ply")?.to_owned(),
                weight,
                column(row, "key")?.to_owned(),
                moves,
                keys,
            ));
        }
        Ok(())
    })?;
    for (side, stop, ply, weight, key, moves, keys) in walk_rows {
        let (value, kind) = last_own_value(&mut index, &moves, &keys, &side)?;
        let class = if stop == "own" {
            Class::Shallow
        } else if kind.starts_with("tree") {
            if args.meets(value, &side) == Some(true) {
                Class::Good
            } else {
                Class::Bad
            }
        } else {
            Class::Weak
        };
        items.push(Item {
            class,
            side,
            ply,
            weight,
            value_black: value,
            kind,
            key,
            moves,
            extra: vec![("stop", stop)],
        });
    }
    if let (Some(bridge), Some(audit)) = (&args.bridge, &args.bridge_audit) {
        let mut gaps = BTreeMap::new();
        // audit の表は元データ全体の局面を持ちうるので、報告に載りうる局面 (重みの下限以上) で gap の大きい行だけを持つ。
        read_tsv(audit, |row| {
            let gap: i32 = column(row, "gap")?.parse().context("gap が不正です")?;
            let key = column(row, "key")?;
            if gap < args.gap_line
                || through.get(key).is_none_or(|(weight, _)| *weight < args.gap_min_weight)
            {
                return Ok(());
            }
            gaps.insert(
                column(row, "key")?.to_owned(),
                (
                    gap,
                    column(row, "best")?.to_owned(),
                    column(row, "best_value_black")?.to_owned(),
                ),
            );
            Ok(())
        })?;
        read_tsv(bridge, |row| {
            let key = column(row, "key")?;
            let Some((weight, path)) = through.get(key) else {
                return Ok(());
            };
            if *weight < args.gap_min_weight {
                return Ok(());
            }
            let side = column(row, "side")?;
            ensure!(matches!(side, "b" | "w"), "side は b / w です: {side}");
            let bridged: i64 =
                column(row, "bridged_black")?.parse().context("bridged_black が不正です")?;
            let replaced = gaps.get(key);
            if replaced.is_none() && args.meets(Some(bridged), side) != Some(false) {
                return Ok(());
            }
            let mut extra = vec![
                ("main", column(row, "main")?.to_owned()),
                ("plain_black", column(row, "plain_black")?.to_owned()),
            ];
            if let Some((_, best, best_value)) = replaced {
                extra.push(("best", best.clone()));
                extra.push(("best_value_black", best_value.clone()));
            }
            items.push(Item {
                class: Class::Gap,
                side: side.to_owned(),
                ply: (path.len() + 1).to_string(),
                weight: *weight,
                value_black: Some(bridged),
                kind: String::new(),
                key: key.to_owned(),
                moves: path.clone(),
                extra,
            });
            Ok(())
        })?;
    }
    if let Some(rejoin) = &args.rejoin {
        read_tsv(rejoin, |row| {
            let side = column(row, "side")?;
            ensure!(matches!(side, "b" | "w"), "side は b / w です: {side}");
            let value: i64 = column(row, "rejoin_value_black")?
                .parse()
                .context("rejoin_value_black が不正です")?;
            if args.meets(Some(value), side) != Some(false) {
                return Ok(());
            }
            // 重みの欄は --opp-freq なしの rejoin では空になる。0 とみなすと行が黙って消える。
            let number = |name: &str| -> Result<f64> {
                column(row, name)?.parse().with_context(|| {
                    format!("{name} が数ではありません (rejoin は --opp-freq を付けた表が必要です)")
                })
            };
            let (weight, games, dig) =
                (number("weight")?, number("rejoin_games")?, number("dig_games")?);
            // 表の weight は局面の重みなので、その手で戻る割合に直す。
            let effective = weight * games / dig.max(1.0);
            if effective < args.gap_min_weight {
                return Ok(());
            }
            items.push(Item {
                class: Class::Rejoin,
                side: side.to_owned(),
                ply: column(row, "ply")?.to_owned(),
                weight: effective,
                value_black: Some(value),
                kind: String::new(),
                key: column(row, "key")?.to_owned(),
                moves: Vec::new(),
                extra: vec![
                    ("move", column(row, "move")?.to_owned()),
                    ("expected_black", column(row, "expected_black")?.to_owned()),
                    ("rejoin_move", column(row, "rejoin_move")?.to_owned()),
                ],
            });
            Ok(())
        })?;
    }
    // 同じ重みは入力の表の順のまま (安定な並べ替え)。
    items.sort_by(|a, b| a.class.cmp(&b.class).then_with(|| b.weight.total_cmp(&a.weight)));
    let queued = |item: &Item| queued_text(&states, &item.key, &item.side);
    let mut counts = BTreeMap::new();
    for item in &items {
        *counts.entry(item.class).or_insert(0usize) += 1;
    }
    writeln!(
        stdout,
        "{}",
        [
            Class::Good,
            Class::Bad,
            Class::Gap,
            Class::Rejoin,
            Class::Shallow,
            Class::Weak
        ]
        .iter()
        .map(|c| format!("{}={}", c.name(), counts.get(c).unwrap_or(&0)))
        .collect::<Vec<_>>()
        .join(" ")
    )?;
    if let Some(path) = &args.table {
        let mut bytes =
            b"class\tside\tply\tweight\tvalue_black\tkind\ttarget\tqueued\tkey\tmoves\textra\n"
                .to_vec();
        for item in &items {
            writeln!(
                bytes,
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                item.class.name(),
                item.side,
                item.ply,
                item.weight,
                fmt_value(item.value_black),
                item.kind,
                target_text(args.meets(item.value_black, &item.side)),
                queued(item),
                item.key,
                item.moves.join(" "),
                item.extra.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(";")
            )?;
        }
        write_atomic(path, &bytes)?;
    }
    if let Some(path) = &args.markdown {
        let bytes = markdown(&args, &items, &queued)?;
        write_atomic(path, &bytes)?;
    }
    Ok(())
}

fn fmt_value(value: Option<i64>) -> String {
    match value {
        Some(v) if v > 0 => format!("+{v}"),
        Some(v) => v.to_string(),
        None => String::new(),
    }
}

fn target_text(meets: Option<bool>) -> &'static str {
    match meets {
        Some(true) => "届く",
        Some(false) => "届かない",
        None => "",
    }
}

/// 同じ根・同じ側の段 1 以上の受付の木 (`id (台, 状態)`)。bw の木は両方の側に数える。
fn queued_text(states: &[TreeState], key: &str, own: &str) -> String {
    let found: BTreeSet<_> = states
        .iter()
        .filter(|t| t.own == own && t.depth >= 1 && t.roots.contains(key))
        .map(|t| format!("{} ({}, {})", t.id, t.box_name, t.state))
        .collect();
    if found.is_empty() {
        "なし".into()
    } else {
        found.into_iter().collect::<Vec<_>>().join("; ")
    }
}

fn kif(moves: &[String]) -> Result<String> {
    usi_moves_to_short_kif(&moves.iter().map(String::as_str).collect::<Vec<_>>())
}

/// 手順の後に 1 手を足した最後の手の表記。
fn kif_last(moves: &[String], mv: &str) -> Result<String> {
    let mut all = moves.to_vec();
    all.push(mv.to_owned());
    Ok(kif(&all)?.rsplit(' ').next().unwrap_or_default().to_owned())
}

fn side_name(side: &str) -> &'static str {
    if side == "b" { "先手" } else { "後手" }
}

fn markdown(
    args: &ExitReportArgs,
    items: &[Item],
    queued: &dyn Fn(&Item) -> String,
) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let title = if args.title.is_empty() {
        "出口の報告".to_owned()
    } else {
        format!("{} の出口の報告", args.title)
    };
    writeln!(out, "# {title}\n")?;
    writeln!(
        out,
        "重み = その出口に来る割合、手数 = 出口の局面までに指した手の数、値 = RAMU が最後に指した定跡の手の値 (プラス = 先手良し)、出所 = 元データの value_kind。目標: 先手 {:+} 以上・後手 {:+} 未満。受付 = 同じ根・同じ側の段 1 以上の木 (id、台、状態)。\n",
        args.target_black, args.target_white
    )?;
    let exits = |out: &mut Vec<u8>, title: &str, note: &str, class: Class| -> Result<()> {
        writeln!(out, "## {title}\n")?;
        if !note.is_empty() {
            writeln!(out, "{note}\n")?;
        }
        for side in ["b", "w"] {
            let rows: Vec<_> =
                items.iter().filter(|i| i.class == class && i.side == side).collect();
            if rows.is_empty() {
                continue;
            }
            writeln!(out, "### RAMU が{}\n", side_name(side))?;
            writeln!(
                out,
                "| 手数 | 重み | 値 | 出所 | 目標 | 受付 | 手順 |\n|---:|---:|---:|---|---|---|---|"
            )?;
            for r in rows {
                writeln!(
                    out,
                    "| {} | {:.3} | {} | {} | {} | {} | {} |",
                    r.ply,
                    r.weight,
                    fmt_value(r.value_black),
                    r.kind,
                    target_text(args.meets(r.value_black, side)),
                    queued(r),
                    kif(&r.moves)?
                )?;
            }
            writeln!(out)?;
        }
        Ok(())
    };
    let min = args.min_weight;
    exits(
        &mut out,
        "1. 有望な出口 (出口より先まで木で読んであり目標に届く)",
        &format!(
            "止まりが out・rare・max で、RAMU の最後の手の値が木の伝播のもの。重み {min} 以上。"
        ),
        Class::Good,
    )?;
    exits(&mut out, "2a. 塞ぎたい出口: 計算済みで目標に届かない", "", Class::Bad)?;
    if args.bridge.is_some() {
        writeln!(out, "## 2b. 塞ぎたい出口: 伝播の切れ目 (`master audit --bridge-replies`)\n")?;
        writeln!(
            out,
            "補って伝播すると目標を割るか、main が 1 位から {} 以上負ける局面 (辿りの重み {} 以上)。値は main の値 (補わない → 補う)。\n",
            args.gap_line, args.gap_min_weight
        )?;
        writeln!(
            out,
            "| 側 | 何手目 | 重み | main | 補わない → 補う | 1 位 (補ったとき) | 受付 | その局面までの手順 |\n|---|---:|---:|---|---|---|---|---|"
        )?;
        for r in items.iter().filter(|i| i.class == Class::Gap) {
            let get =
                |name: &str| r.extra.iter().find(|(k, _)| *k == name).map(|(_, v)| v.as_str());
            let best = match (get("best"), get("best_value_black")) {
                (Some(best), Some(value)) => {
                    format!("{} {}", kif_last(&r.moves, best)?, fmt_value(value.parse().ok()))
                }
                _ => String::new(),
            };
            writeln!(
                out,
                "| {} | {} | {:.3} | {} | {} → {} | {best} | {} | {} |",
                side_name(&r.side),
                r.ply,
                r.weight,
                kif_last(&r.moves, get("main").unwrap_or_default())?,
                fmt_value(get("plain_black").and_then(|v| v.parse().ok())),
                fmt_value(r.value_black),
                queued(r),
                kif(&r.moves)?
            )?;
        }
        writeln!(out)?;
    }
    if args.rejoin.is_some() {
        writeln!(
            out,
            "## 2c. 塞ぎたい出口: 相手が定跡に無い手で定跡の局面に戻る (`master rejoin`)\n"
        )?;
        writeln!(
            out,
            "戻った先の main の値が目標を割る所。来る割合 = 局面の重み × その手の局数 / 局面の局数 ({} 以上)。\n",
            args.gap_min_weight
        )?;
        writeln!(
            out,
            "| 側 | 何手目 | 来る割合 | RAMU の手 (期待) | 相手の手 → 戻った先 |\n|---|---:|---:|---|---|"
        )?;
        for r in items.iter().filter(|i| i.class == Class::Rejoin) {
            let get = |name: &str| {
                r.extra
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| v.as_str())
                    .unwrap_or_default()
            };
            writeln!(
                out,
                "| {} | {} | {:.4} | {} ({}) | {} → {} |",
                side_name(&r.side),
                r.ply,
                r.weight,
                get("move"),
                fmt_value(get("expected_black").parse().ok()),
                get("rejoin_move"),
                fmt_value(r.value_black)
            )?;
        }
        writeln!(out)?;
    }
    exits(
        &mut out,
        "3. 浅い出口 (相手のよくある応手が定跡に無い、own の止まり)",
        &format!(
            "直前の RAMU の手の値は出口の値ではない (応手の後の局面は読んでいない)。重み {min} 以上。"
        ),
        Class::Shallow,
    )?;
    exits(
        &mut out,
        "4. 値の出所が弱い重い出口 (木の伝播でない値)",
        &format!("重み {min} 以上。"),
        Class::Weak,
    )?;
    Ok(out)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    crate::common::io::write_atomic(path, std::str::from_utf8(bytes)?)
}

#[cfg(test)]
mod tests;
