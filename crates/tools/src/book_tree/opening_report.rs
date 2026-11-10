//! 戦型の候補ごとに、RAMU の手番の決めた局面で main の代わりに決めた手を指し、相手は `master walk` と同じ頻度で
//! 辿って、出口の値・値の確度・強い対局の実績・危ない局面を通る重みを比べる表。

use super::audit::reject_same_file;
use super::exit_report::{fmt_value, kif, kif_last, side_name};
use super::master_index::MasterIndex;
use super::master_table::{MasterPosition, mean_eval, read_master};
use super::model::side;
use super::reply_graph::HIRATE_KEY;
use super::schema::Schema;
use super::walk::records::{RecordOptions, scan_games};
use super::walk::{
    Excluded, OppOptions, Row, Sources, Stop, Tagger, WalkFrom, WalkOptions, advance, book_probe,
    excluded_positions, open_book, open_freq, walk_side_with,
};
use crate::common::io::write_atomic;
use anyhow::{Context, Result, bail, ensure};
use clap::Args;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::PathBuf;

#[derive(Args, Debug, Clone)]
pub(super) struct OpeningReportArgs {
    /// 定跡の元データの表 (TSV)
    #[arg(long)]
    master: PathBuf,
    /// 語彙と読み条件の表を置いたディレクトリ
    #[arg(long)]
    schema: Option<PathBuf>,
    /// 候補の表 (TSV、見出し `name side override`)
    #[arg(long)]
    candidates: PathBuf,
    /// 相手の手の頻度の定跡 (`master walk --opp-freq` と同じ)
    #[arg(long)]
    opp_freq: Option<PathBuf>,
    /// 相手の手の大きい定跡 (`master walk --opp-book` と同じ)
    #[arg(long)]
    opp_book: Option<PathBuf>,
    /// 強い対局の棋譜 (`master walk --opp-records` と同じ形)
    #[arg(long)]
    opp_records: Option<PathBuf>,
    /// 危ない局面の表 (TSV、列 `side`・`key` を使う。`#` で始まる行は注)
    #[arg(long)]
    danger: Option<PathBuf>,
    /// 以下 `--cont-max` まで、辿り方は `master walk` の同じ名前の指定と同じ
    #[arg(long, default_value_t = 3)]
    min_games: u64,
    #[arg(long, default_value_t = 80)]
    max_ply: usize,
    #[arg(long, default_value_t = 1e-5)]
    weight_floor: f64,
    #[arg(long, default_value_t = 30)]
    opp_book_diff: i32,
    #[arg(long, default_value_t = 0.9)]
    opp_book_best: f64,
    #[arg(long, requires = "opp_book")]
    opp_book_first: bool,
    #[arg(long, default_value_t = 100)]
    cont_max: usize,
    /// `--opp-records` で数える対局の、両者のレートの下限
    #[arg(long, default_value_t = 3800.0)]
    records_min_rate: f64,
    /// `--opp-records` で、1 局を初期局面からこの手数まで見る
    #[arg(long, default_value_t = 130)]
    records_max_ply: usize,
    /// 強い対局で危ないとみなす局面の局数の下限
    #[arg(long, default_value_t = 10)]
    danger_games: u64,
    /// 強い対局で RAMU の側の得点がこれ未満の局面を危ないとみなす
    #[arg(long, default_value_t = 0.2)]
    danger_score: f64,
    /// 先手の候補で別に数える、出口とその 10・20 手前の局面の先手の得点の上限
    #[arg(long, default_value_t = 0.8)]
    black_score: f64,
    /// 既に積んだ木のキュー (JSON Lines、繰り返し可)。読みを足す候補に、同じ根・同じ側の木があるかを書く
    #[arg(long)]
    exclude_queue: Vec<PathBuf>,
    /// 読みを足す候補 (own の出口) を側ごとに載せる数
    #[arg(long, default_value_t = 20)]
    top: usize,
    /// 危ない局面の一覧に載せる重みの下限
    #[arg(long, default_value_t = 0.001)]
    list_min_weight: f64,
    /// 分岐の後の相手の手で、割合がこれ未満の手は「その他」にまとめる
    #[arg(long, default_value_t = 0.01)]
    reply_min_share: f64,
    /// 候補ごとの表 (TSV)
    #[arg(long)]
    table: Option<PathBuf>,
    /// 報告 (Markdown)
    #[arg(long)]
    markdown: Option<PathBuf>,
    /// 報告の見出しに使う版の名前
    #[arg(long, default_value = "")]
    title: String,
}

impl OpeningReportArgs {
    fn check(&self) -> Result<()> {
        ensure!(
            self.table.is_some() || self.markdown.is_some(),
            "--table か --markdown の出力先が必要です"
        );
        for (name, w) in [
            ("--weight-floor", self.weight_floor),
            ("--list-min-weight", self.list_min_weight),
            ("--reply-min-share", self.reply_min_share),
        ] {
            ensure!(w.is_finite() && w >= 0.0, "{name} は有限の非負数にしてください: {w}");
        }
        ensure!(
            (0.0..=1.0).contains(&self.opp_book_best),
            "--opp-book-best は 0〜1 にしてください"
        );
        ensure!(self.opp_book_diff >= 0, "--opp-book-diff は 0 以上にしてください");
        ensure!(self.records_min_rate.is_finite(), "--records-min-rate は有限の数にしてください");
        for (name, s) in [
            ("--danger-score", self.danger_score),
            ("--black-score", self.black_score),
        ] {
            ensure!((0.0..=1.0).contains(&s), "{name} は 0〜1 にしてください: {s}");
        }
        let mut inputs = vec![
            ("--master", self.master.as_path()),
            ("--candidates", self.candidates.as_path()),
        ];
        for (name, path) in [
            ("--opp-freq", &self.opp_freq),
            ("--opp-book", &self.opp_book),
            ("--opp-records", &self.opp_records),
            ("--danger", &self.danger),
        ] {
            if let Some(path) = path {
                inputs.push((name, path.as_path()));
            }
        }
        for path in &self.exclude_queue {
            inputs.push(("--exclude-queue", path.as_path()));
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

    fn walk_options(&self) -> WalkOptions {
        WalkOptions {
            max_ply: self.max_ply,
            floor: self.weight_floor,
            cont_max: self.cont_max,
            opp: OppOptions {
                min_games: self.min_games,
                book_diff: self.opp_book_diff,
                book_best: self.opp_book_best,
                book_first: self.opp_book_first,
            },
        }
    }
}

/// 候補の表の 1 行。
struct Candidate {
    name: String,
    side: &'static str,
    /// (初期局面からの手順, そこで RAMU が指す手)。
    overrides: Vec<(Vec<String>, String)>,
    /// 手数を除いた SFEN → RAMU が指す手。
    by_key: HashMap<String, String>,
}

fn usi_list(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_owned).collect()
}

fn play(moves: &[String]) -> Result<String> {
    let mut key = HIRATE_KEY.to_owned();
    for (i, mv) in moves.iter().enumerate() {
        key = advance(&key, mv).with_context(|| format!("{} 手目", i + 1))?;
    }
    Ok(key)
}

/// 候補の表。見出しは `name side override`。override は `<手順>:<手>` を `;` で区切った並びで、空なら main だけ。
/// 手順は初期局面からの USI の手 (空白区切り、空なら初期局面)。`#` で始まる行と空行は読み飛ばす。
fn parse_candidates(text: &str) -> Result<Vec<Candidate>> {
    let mut lines = text
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty() && !l.starts_with('#'));
    let (_, header) = lines.next().context("候補の表が空です")?;
    ensure!(header == "name\tside\toverride", "候補の表の見出しは name・side・override です");
    let mut out: Vec<Candidate> = Vec::new();
    for (n, line) in lines {
        let at = || format!("候補の表の {} 行目", n + 1);
        let cells: Vec<&str> = line.split('\t').collect();
        // override が空の行は、エディタで末尾のタブが消えても読めるように 2 列も受け付ける。
        ensure!(matches!(cells.len(), 2 | 3), "{}: 列は 3 つです", at());
        let name = cells[0].trim();
        ensure!(!name.is_empty(), "{}: name が空です", at());
        ensure!(out.iter().all(|c| c.name != name), "{}: name が重なっています: {name}", at());
        let side = match cells[1].trim() {
            "b" => "b",
            "w" => "w",
            other => bail!("{}: side は b か w です: {other}", at()),
        };
        let mut overrides = Vec::new();
        let mut by_key = HashMap::new();
        let cell = cells.get(2).copied().unwrap_or_default();
        for part in cell.split(';').map(str::trim).filter(|p| !p.is_empty()) {
            let (prefix, mv) = part
                .split_once(':')
                .with_context(|| format!("{}: override は <手順>:<手> です: {part}", at()))?;
            let prefix = usi_list(prefix);
            let mv = mv.trim().to_owned();
            let key = play(&prefix).with_context(|| format!("{}: {part}", at()))?;
            ensure!(side_of(&key) == side, "{}: {part} の局面は RAMU の手番ではありません", at());
            advance(&key, &mv).with_context(|| format!("{}: {part}", at()))?;
            ensure!(
                by_key.insert(key, mv.clone()).is_none(),
                "{}: 同じ局面の override が重なっています: {part}",
                at()
            );
            overrides.push((prefix, mv));
        }
        out.push(Candidate {
            name: name.to_owned(),
            side,
            overrides,
            by_key,
        });
    }
    ensure!(!out.is_empty(), "候補の表に候補がありません");
    Ok(out)
}

fn side_of(key: &str) -> &'static str {
    if side(key) == "b" { "b" } else { "w" }
}

/// 危ない局面の表の (側, 局面)。
fn parse_danger(text: &str) -> Result<HashSet<(&'static str, String)>> {
    let mut lines = text
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty() && !l.starts_with('#'));
    let (_, header) = lines.next().context("危ない局面の表が空です")?;
    let columns: Vec<&str> = header.split('\t').collect();
    let find = |name: &str| {
        columns
            .iter()
            .position(|c| *c == name)
            .with_context(|| format!("危ない局面の表に列 {name} がありません"))
    };
    let (side_at, key_at) = (find("side")?, find("key")?);
    let mut out = HashSet::new();
    for (n, line) in lines {
        let cells: Vec<&str> = line.split('\t').collect();
        ensure!(cells.len() == columns.len(), "危ない局面の表の {} 行目: 列数が不正です", n + 1);
        let side = match cells[side_at] {
            "b" => "b",
            "w" => "w",
            other => bail!("危ない局面の表の {} 行目: side は b か w です: {other}", n + 1),
        };
        let key = normalize_key(cells[key_at])
            .with_context(|| format!("危ない局面の表の {} 行目", n + 1))?;
        out.insert((side, key));
    }
    Ok(out)
}

/// 盤・手番・持ち駒 (手数は付いていてもよい) を、辿りの局面と同じ書き方に直す。
fn normalize_key(text: &str) -> Result<String> {
    let fields: Vec<&str> = text.split_whitespace().collect();
    ensure!(matches!(fields.len(), 3 | 4), "局面は盤・手番・持ち駒 (と手数) です: {text}");
    let pos = crate::book_db::position_from_sfen(&format!("{} 1", fields[..3].join(" ")))?;
    Ok(crate::book_db::strip_ply(&pos.to_sfen()).to_owned())
}

/// 局面に番号を付け、同じ局面と同じ手の行き先を 1 回だけ求める。
#[derive(Default)]
struct Keys {
    ids: HashMap<String, u32>,
    names: Vec<String>,
    children: HashMap<(u32, String), u32>,
}

impl Keys {
    fn id(&mut self, key: &str) -> u32 {
        if let Some(&id) = self.ids.get(key) {
            return id;
        }
        let id = u32::try_from(self.names.len()).expect("局面の数が u32 に収まる");
        self.names.push(key.to_owned());
        self.ids.insert(key.to_owned(), id);
        id
    }

    fn child(&mut self, parent: u32, mv: &str) -> Result<u32> {
        if let Some(&id) = self.children.get(&(parent, mv.to_owned())) {
            return Ok(id);
        }
        let key = advance(&self.names[parent as usize], mv)?;
        let id = self.id(&key);
        self.children.insert((parent, mv.to_owned()), id);
        Ok(id)
    }

    /// 初期局面から `moves` を指した各局面 (初期局面と最後の局面を含む)。
    fn path(&mut self, moves: &[String]) -> Result<Vec<u32>> {
        let mut path = vec![self.id(HIRATE_KEY)];
        for mv in moves {
            path.push(self.child(*path.last().expect("初期局面がある"), mv)?);
        }
        Ok(path)
    }
}

/// 強い対局がその局面に着いた局数と、先手から見た得点の合計。
#[derive(Default, Clone, Copy)]
struct Games {
    games: u64,
    black_points: f64,
}

impl Games {
    fn score(&self, side: &str) -> Option<f64> {
        (self.games > 0).then(|| {
            let black = self.black_points / self.games as f64;
            if side == "b" { black } else { 1.0 - black }
        })
    }
}

/// 値の出所の分け方の番号。6 段以上の木・3〜5 段・1〜2 段・1 回の読み・複数回の平均・記録なし・値なしの順
/// (表の列 `tree6` 〜 `no_value` の順)。
fn kind_bucket(kind: &str, value: Option<i64>) -> usize {
    if value.is_none() {
        return 6;
    }
    if let Some(n) = kind.strip_prefix("tree:").and_then(|n| n.parse::<u32>().ok()) {
        return match n {
            6.. => 0,
            3..=5 => 1,
            _ => 2,
        };
    }
    match kind {
        "single" => 3,
        k if k.starts_with("mean:") => 4,
        "unknown" => 5,
        _ => 6,
    }
}

/// 辿りの 1 行に付けた値と印。
struct Exit {
    /// 手順 (`moves`) は空にして `moves` に移す (行が多いので 1 本の文字列で持つ)。
    row: Row,
    /// 初期局面からの手順 (USI を空白区切り)。
    moves: String,
    path: Vec<u32>,
    value: Option<i64>,
    kind: String,
    /// 千日手に着いた (循環を回り続けて max で止まったか、own の出口の外部定跡の続きが同じ局面に戻る)。
    draw: bool,
    /// 通った分岐と、その局面で相手が指した手。
    branches: Vec<(usize, Option<String>)>,
    /// 最初に通った危ない局面 (強い対局の判定か `--danger` の表)。
    first_danger: Option<u32>,
    /// 強い対局で危ない局面を通った。
    auto: bool,
    /// `--danger` の表の局面を通った。
    listed: bool,
    /// 先手の候補で、出口か 10・20 手前の局面の先手の得点が `--black-score` 未満。
    black_below: bool,
}

impl Exit {
    fn move_list(&self) -> Vec<String> {
        usi_list(&self.moves)
    }
}

/// 分岐の直後の局面 (相手の手番) と、そこからの相手の手ごとの重みと値。
struct Branch {
    key: u32,
    /// 初期局面からその局面までの手順。
    moves: Vec<String>,
    /// 分岐を表す文 (手順と手)。
    label: String,
    reach: f64,
    /// 手 (USI、止まりは `stop:<種類>`) → (重み, (値, 重み) の並び)。
    replies: BTreeMap<String, (f64, Vec<(i64, f64)>)>,
}

struct Report {
    candidate: Candidate,
    exits: Vec<Exit>,
    branches: Vec<Branch>,
    override_weight: Option<f64>,
}

fn weighted_quantile(values: &[(i64, f64)], q: f64) -> Option<i64> {
    let mut values = values.to_vec();
    values.sort_by_key(|(v, _)| *v);
    let total: f64 = values.iter().map(|(_, w)| w).sum();
    if total <= 0.0 {
        return None;
    }
    let mut acc = 0.0;
    for (v, w) in &values {
        acc += w;
        if acc >= q * total - 1e-12 {
            return Some(*v);
        }
    }
    values.last().map(|(v, _)| *v)
}

/// 先手から見た値と value_kind。
type OwnValue = (Option<i64>, String);

/// 元データの手の値の引き方。
struct Values<'a> {
    index: &'a mut MasterIndex,
    /// (局面, 手) → 元データの行の (値, value_kind)。行が無ければ `None`。
    cache: HashMap<(u32, String), Option<OwnValue>>,
}

impl Values<'_> {
    /// 局面 `key` (番号 `id`、`own` の手番) で指した `mv` の行の evals の平均 (先手から見た値) と value_kind。
    /// 行が無ければ値なし。
    fn of(&mut self, id: u32, key: &str, mv: &str, own: &str) -> Result<OwnValue> {
        let slot = (id, mv.to_owned());
        if !self.cache.contains_key(&slot) {
            let found = match self.index.rows(key)?.into_iter().find(|r| r.get("move") == mv) {
                Some(row) => {
                    let sign = if own == "b" { 1 } else { -1 };
                    Some((
                        mean_eval(row.get("evals"))?.map(|v| i64::from(v) * sign),
                        row.get("value_kind").to_owned(),
                    ))
                }
                None => None,
            };
            self.cache.insert(slot.clone(), found);
        }
        Ok(self.cache[&slot].clone().unwrap_or((None, String::new())))
    }
}

/// 分岐の直後の局面。override があればその手の行き先、無ければ main の辿りで最初に来る相手の手番の局面。
fn branch_keys(
    candidate: &Candidate,
    positions: &BTreeMap<String, MasterPosition>,
    keys: &mut Keys,
) -> Result<Vec<(u32, Vec<String>, String)>> {
    let mut out = Vec::new();
    for (prefix, mv) in &candidate.overrides {
        let at = keys.path(prefix)?;
        let id = keys.child(*at.last().expect("初期局面がある"), mv)?;
        let label = if prefix.is_empty() {
            format!("初手 {}", kif_last(prefix, mv)?)
        } else {
            format!("{} で {}", kif(prefix)?, kif_last(prefix, mv)?)
        };
        // 別の override が手順違いで同じ局面に着くなら 1 つの分岐にまとめる。
        if let Some(entry) = out.iter_mut().find(|(at, _, _)| *at == id) {
            entry.2 = format!("{}、{label}", entry.2);
            continue;
        }
        let mut moves = prefix.clone();
        moves.push(mv.clone());
        out.push((id, moves, label));
    }
    if out.is_empty() {
        let root = keys.id(HIRATE_KEY);
        if candidate.side == "w" {
            out.push((root, Vec::new(), "初期局面 (main)".to_owned()));
        } else if let Some(position) = positions.get(HIRATE_KEY) {
            let mv = &position.moves[0].mv;
            let id = keys.child(root, mv)?;
            out.push((id, vec![mv.clone()], format!("初手 {} (main)", kif_last(&[], mv)?)));
        }
    }
    Ok(out)
}

fn walk_from(overrides: &HashMap<String, String>) -> WalkFrom<'_> {
    WalkFrom {
        start: HIRATE_KEY,
        start_moves: &[],
        overrides,
    }
}

/// 辿りの重みに付ける印の中身。
#[derive(Clone, Default, PartialEq, Eq, Hash)]
struct State {
    first_danger: Option<u32>,
    auto: bool,
    listed: bool,
    /// 通った分岐 (`Report::branches` の添字) と、その局面で相手が指した手 (まだなら `None`)。
    branches: Vec<(usize, Option<String>)>,
    /// RAMU が最後に指した手 (局面, 手)。出口の値はこの手の値で、同じ局面に来ても最後の手の違う重みは混ぜない。
    last_move: Option<(u32, String)>,
}

/// 局面に番号を付け、辿りで指した手の行き来を集めるだけの印。
struct Collect<'a> {
    keys: &'a mut Keys,
    edges: HashSet<(u32, u32)>,
    visited: &'a mut HashSet<u32>,
}

impl Tagger for Collect<'_> {
    fn start(&mut self, key: &str) -> Result<u32> {
        let id = self.keys.id(key);
        self.visited.insert(id);
        Ok(0)
    }
    fn step(&mut self, _: u32, parent: &str, _: &str, child: &str) -> Result<u32> {
        let edge = (self.keys.id(parent), self.keys.id(child));
        self.edges.insert(edge);
        self.visited.insert(edge.1);
        Ok(0)
    }
    fn coarsen(&mut self, tag: u32) -> u32 {
        tag
    }
}

/// 辿りの行き来の循環 (強連結成分) に入っている局面。
fn cycle_nodes(edges: &HashSet<(u32, u32)>, len: usize) -> HashSet<u32> {
    let mut adjacency = vec![Vec::new(); len];
    for &(from, to) in edges {
        adjacency[from as usize].push(to);
    }
    super::reply_graph::nontrivial_sccs(&adjacency).into_iter().flatten().collect()
}

/// 危ない局面と分岐を通ったかで重みを分ける印。
struct Tags<'a> {
    keys: &'a mut Keys,
    states: Vec<State>,
    ids: HashMap<State, u32>,
    /// 分岐の局面 → `Report::branches` の添字。
    branches: HashMap<u32, usize>,
    auto: &'a HashSet<u32>,
    listed: &'a HashSet<u32>,
    own: &'static str,
}

impl Tags<'_> {
    fn enter(&mut self, mut state: State, id: u32) -> u32 {
        let (auto, listed) = (self.auto.contains(&id), self.listed.contains(&id));
        if !state.auto && !state.listed && (auto || listed) {
            state.first_danger = Some(id);
        }
        state.auto |= auto;
        state.listed |= listed;
        if let Some(&b) = self.branches.get(&id)
            && state.branches.iter().all(|(i, _)| *i != b)
        {
            state.branches.push((b, None));
        }
        self.intern(state)
    }

    fn intern(&mut self, state: State) -> u32 {
        if let Some(&tag) = self.ids.get(&state) {
            return tag;
        }
        let tag = u32::try_from(self.states.len()).expect("印の数が u32 に収まる");
        self.ids.insert(state.clone(), tag);
        self.states.push(state);
        tag
    }
}

impl Tagger for Tags<'_> {
    fn start(&mut self, key: &str) -> Result<u32> {
        let id = self.keys.id(key);
        Ok(self.enter(State::default(), id))
    }
    fn step(&mut self, tag: u32, parent: &str, mv: &str, child: &str) -> Result<u32> {
        let id = self.keys.id(child);
        let mut state = self.states[tag as usize].clone();
        let parent_id = self.keys.id(parent);
        if side(parent) == self.own {
            state.last_move = Some((parent_id, mv.to_owned()));
        }
        if let Some(&b) = self.branches.get(&parent_id)
            && let Some(entry) = state.branches.iter_mut().find(|(i, r)| *i == b && r.is_none())
        {
            entry.1 = Some(mv.to_owned());
        }
        Ok(self.enter(state, id))
    }
    /// 最初に通った危ない局面を忘れる (通ったかどうかは残す)。危ない局面ごとの一覧から小さく分かれた重みが抜ける。
    fn coarsen(&mut self, tag: u32) -> u32 {
        let mut state = self.states[tag as usize].clone();
        state.first_danger = None;
        self.intern(state)
    }
}

pub(super) fn run(args: OpeningReportArgs) -> Result<()> {
    args.check()?;
    let started = std::time::Instant::now();
    let text = std::fs::read_to_string(&args.candidates)
        .with_context(|| format!("開けません: {}", args.candidates.display()))?;
    let candidates = parse_candidates(&text)?;
    let listed = match &args.danger {
        Some(path) => Some(parse_danger(
            &std::fs::read_to_string(path)
                .with_context(|| format!("開けません: {}", path.display()))?,
        )?),
        None => None,
    };
    let excluded = excluded_positions(&args.exclude_queue)?;
    let (positions, _) = read_master(&args.master)?;
    let freq = args.opp_freq.as_deref().map(open_freq).transpose()?;
    let book = args.opp_book.as_deref().map(open_book).transpose()?;
    let mut src = Sources {
        freq: freq.as_ref().map(book_probe),
        book: book.as_ref().map(book_probe),
    };
    let options = args.walk_options();
    let mut keys = Keys::default();
    // 分岐の局面は辿りで来なくても強い対局の局数を出すので、先に番号を付ける。
    let mut branch_sets = Vec::with_capacity(candidates.len());
    for candidate in &candidates {
        let branches: Vec<Branch> = branch_keys(candidate, &positions, &mut keys)?
            .into_iter()
            .map(|(key, moves, label)| Branch {
                key,
                moves,
                label,
                reach: 0.0,
                replies: BTreeMap::new(),
            })
            .collect();
        branch_sets.push(branches);
    }
    // 強い対局で危ない局面かは局面ごとの局数で決まり、循環は辿りの行き来で決まるので、先に一度辿って集める。
    let mut cycles = Vec::with_capacity(candidates.len());
    let mut visited = HashSet::new();
    for candidate in &candidates {
        let from = walk_from(&candidate.by_key);
        let mut collect = Collect {
            keys: &mut keys,
            edges: HashSet::new(),
            visited: &mut visited,
        };
        walk_side_with(&positions, &mut src, candidate.side, &from, &mut collect, options)?;
        let edges = collect.edges;
        cycles.push(cycle_nodes(&edges, keys.names.len()));
    }
    let mut games = vec![Games::default(); keys.names.len()];
    if let Some(path) = &args.opp_records {
        let options = RecordOptions {
            min_rate: args.records_min_rate,
            max_ply: args.records_max_ply,
        };
        let mut seen = HashSet::new();
        scan_games(path, &options, |game| {
            if game.first {
                seen.clear();
            }
            if let Some(&id) = keys.ids.get(game.key)
                && seen.insert(id)
            {
                let g = &mut games[id as usize];
                g.games += 1;
                g.black_points += game.black_score;
            }
        })?;
    }
    let mut index = MasterIndex::open(&args.master, &Schema::load(args.schema.as_deref())?)?;
    let mut values = Values {
        index: &mut index,
        cache: HashMap::new(),
    };
    let mut reports = Vec::new();
    if let Some(listed) = &listed {
        for side in ["b", "w"] {
            let total = listed.iter().filter(|(s, _)| *s == side).count();
            if total > 0 {
                let reached = listed
                    .iter()
                    .filter(|(s, key)| {
                        *s == side && keys.ids.get(key).is_some_and(|id| visited.contains(id))
                    })
                    .count();
                println!("危ない局面の表 ({side}): {total} 局面のうち辿りで来るもの {reached}");
            }
        }
    }
    for ((candidate, cycles), branches) in candidates.into_iter().zip(&cycles).zip(branch_sets) {
        let own = candidate.side;
        let auto: HashSet<u32> = games
            .iter()
            .enumerate()
            .filter(|(_, g)| {
                g.games >= args.danger_games && g.score(own).is_some_and(|s| s < args.danger_score)
            })
            .map(|(id, _)| id as u32)
            .collect();
        let listed_ids: HashSet<u32> = listed
            .iter()
            .flatten()
            .filter(|(side, _)| *side == own)
            .map(|(_, key)| keys.id(key))
            .collect();
        let mut tags = Tags {
            keys: &mut keys,
            states: Vec::new(),
            ids: HashMap::new(),
            branches: branches.iter().enumerate().map(|(i, b)| (b.key, i)).collect(),
            auto: &auto,
            listed: &listed_ids,
            own,
        };
        let from = walk_from(&candidate.by_key);
        let rows = walk_side_with(&positions, &mut src, own, &from, &mut tags, options)?;
        let states = tags.states;
        let mut report = Report {
            override_weight: (!candidate.overrides.is_empty()).then_some(0.0),
            candidate,
            exits: Vec::with_capacity(rows.len()),
            branches,
        };
        for mut row in rows {
            let state = &states[row.tag as usize];
            let path = keys.path(&row.moves)?;
            let (value, kind) = match &state.last_move {
                Some((id, mv)) => values.of(*id, &keys.names[*id as usize], mv, own)?,
                None => (None, String::new()),
            };
            // own の出口から外部定跡の最善を両者が指し続けた手順が、出口の局面かその続きの手順の中の局面に戻るか。
            let at_exit = *path.last().expect("初期局面がある");
            let mut at = at_exit;
            let mut seen: HashSet<u32> = HashSet::from([at]);
            let mut repeated = false;
            for mv in &std::mem::take(&mut row.cont) {
                if repeated {
                    break;
                }
                // 大きい定跡の手順は walk で指せた手だけが入っている。
                at = keys.child(at, mv)?;
                repeated = !seen.insert(at);
            }
            // max で止まった局面が循環の中なら、循環から出られずに回り続けた重み。
            let draw = repeated || (row.stop == Stop::Max && cycles.contains(&at_exit));
            // 出口とその 10・20 手前は、行の手順 (同じ局面に来た手順のうち最初の 1 つ) で決める。
            let black_below = own == "b"
                && [0usize, 10, 20].iter().any(|off| {
                    path.len().checked_sub(off + 1).is_some_and(|i| {
                        let g = games.get(path[i] as usize).copied().unwrap_or_default();
                        g.games >= args.danger_games
                            && g.score("b").is_some_and(|s| s < args.black_score)
                    })
                });
            for (b, reply) in &state.branches {
                let branch = &mut report.branches[*b];
                branch.reach += row.weight;
                let reply = match reply {
                    Some(mv) => mv.clone(),
                    None => format!("stop:{}", row.stop.as_str()),
                };
                let entry = branch.replies.entry(reply).or_default();
                entry.0 += row.weight;
                if let Some(v) = value {
                    entry.1.push((v, row.weight));
                }
            }
            if !state.branches.is_empty()
                && let Some(w) = report.override_weight.as_mut()
            {
                *w += row.weight;
            }
            report.exits.push(Exit {
                moves: std::mem::take(&mut row.moves).join(" "),
                path,
                value,
                kind,
                draw,
                branches: state.branches.clone(),
                first_danger: state.first_danger,
                auto: state.auto,
                listed: state.listed,
                black_below,
                row,
            });
        }
        reports.push(report);
    }
    games.resize(keys.names.len(), Games::default());
    let summaries: Vec<Summary> = reports.iter().map(|r| Summary::of(r, &games)).collect();
    if let Some(path) = &args.table {
        write_atomic(path, &format_table(&reports, &summaries))?;
    }
    if let Some(path) = &args.markdown {
        let inputs = Inputs {
            args: &args,
            keys: &keys,
            games: &games,
            listed: listed.as_ref(),
            excluded: &excluded,
        };
        write_atomic(path, &markdown(&inputs, &reports, &summaries)?)?;
    }
    for (report, s) in reports.iter().zip(&summaries) {
        println!(
            "{} ({}): 出口 {} 行 中央値 {} 危ない局面を通る重み {:.4}",
            report.candidate.name,
            report.candidate.side,
            report.exits.len(),
            fmt_value(s.quantiles[2]),
            s.danger_any
        );
    }
    println!("局面 {} 時間 {:.1} 秒", keys.names.len(), started.elapsed().as_secs_f64());
    Ok(())
}

/// 候補ごとの数。重みは辿り全体を 1 とした割合。
struct Summary {
    mean: Option<f64>,
    /// 10・25・50・75・90% 点。
    quantiles: [Option<i64>; 5],
    /// 先手: +50 以上・+100 以上。後手: 先手から見て +80 以下・0 以下。
    hits: [f64; 2],
    draw: f64,
    kinds: [f64; 7],
    stops: [f64; 5],
    own_lt20: f64,
    own_lt30: f64,
    exit_games: u64,
    exit_score: Option<f64>,
    branch_games: u64,
    branch_score: Option<f64>,
    danger_auto: f64,
    danger_listed: f64,
    danger_both: f64,
    danger_any: f64,
    black_below: Option<f64>,
    /// 分岐の後に相手が最も多い手以外を指す重み (分岐ごとの和) と、その先の中央値。
    deviate: f64,
    deviate_median: Option<i64>,
}

const QUANTILES: [f64; 5] = [0.1, 0.25, 0.5, 0.75, 0.9];

impl Summary {
    fn of(report: &Report, games: &[Games]) -> Self {
        let own = report.candidate.side;
        let exits = &report.exits;
        let valued: Vec<(i64, f64)> =
            exits.iter().filter_map(|e| e.value.map(|v| (v, e.row.weight))).collect();
        let valued_weight: f64 = valued.iter().map(|(_, w)| w).sum();
        let mean = (valued_weight > 0.0)
            .then(|| valued.iter().map(|(v, w)| *v as f64 * w).sum::<f64>() / valued_weight);
        let quantiles = QUANTILES.map(|q| weighted_quantile(&valued, q));
        let hit = |f: &dyn Fn(i64) -> bool| -> f64 {
            // 空の和は -0.0 になるので 0.0 から足す。
            valued.iter().filter(|(v, _)| f(*v)).fold(0.0, |sum, (_, w)| sum + w)
        };
        let hits = if own == "b" {
            [hit(&|v| v >= 50), hit(&|v| v >= 100)]
        } else {
            [hit(&|v| v <= 80), hit(&|v| v <= 0)]
        };
        let weight_of = |f: &dyn Fn(&Exit) -> bool| -> f64 {
            exits.iter().filter(|e| f(e)).fold(0.0, |sum, e| sum + e.row.weight)
        };
        let mut kinds = [0.0; 7];
        for e in exits {
            kinds[kind_bucket(&e.kind, e.value)] += e.row.weight;
        }
        let stops = Stop::ALL.map(|s| weight_of(&|e| e.row.stop == s));
        let mut exit_games = 0;
        let mut exit_points = 0.0;
        let mut seen = HashSet::new();
        for e in exits {
            let id = *e.path.last().expect("初期局面がある");
            if seen.insert(id) {
                let g = games[id as usize];
                exit_games += g.games;
                exit_points += g.score(own).unwrap_or(0.0) * g.games as f64;
            }
        }
        let mut branch_games = 0;
        let mut branch_points = 0.0;
        for b in &report.branches {
            let g = games[b.key as usize];
            branch_games += g.games;
            branch_points += g.score(own).unwrap_or(0.0) * g.games as f64;
        }
        let tops: Vec<Option<&str>> = report
            .branches
            .iter()
            .map(|b| {
                b.replies
                    .iter()
                    .filter(|(m, _)| !m.starts_with("stop:"))
                    .max_by(|a, b| a.1.0.total_cmp(&b.1.0))
                    .map(|(m, _)| m.as_str())
            })
            .collect();
        // いずれかの分岐で最も多い手以外に進んだ重みを、1 本の重みにつき 1 回数える。
        let deviates = |e: &Exit| {
            e.branches
                .iter()
                .any(|(b, reply)| reply.as_deref().is_some_and(|m| Some(m) != tops[*b]))
        };
        let deviate = weight_of(&|e| deviates(e));
        let deviate_values: Vec<(i64, f64)> = exits
            .iter()
            .filter(|e| deviates(e))
            .filter_map(|e| e.value.map(|v| (v, e.row.weight)))
            .collect();
        let ratio = |points: f64, n: u64| (n > 0).then(|| points / n as f64);
        Self {
            mean,
            quantiles,
            hits,
            draw: weight_of(&|e| e.draw),
            kinds,
            stops,
            own_lt20: weight_of(&|e| e.row.stop == Stop::Own && e.row.ply < 20),
            own_lt30: weight_of(&|e| e.row.stop == Stop::Own && e.row.ply < 30),
            exit_games,
            exit_score: ratio(exit_points, exit_games),
            branch_games,
            branch_score: ratio(branch_points, branch_games),
            danger_auto: weight_of(&|e| e.auto),
            danger_listed: weight_of(&|e| e.listed),
            danger_both: weight_of(&|e| e.auto && e.listed),
            danger_any: weight_of(&|e| e.auto || e.listed),
            black_below: (own == "b").then(|| weight_of(&|e| e.black_below)),
            deviate,
            deviate_median: weighted_quantile(&deviate_values, 0.5),
        }
    }
}

struct Inputs<'a> {
    args: &'a OpeningReportArgs,
    keys: &'a Keys,
    games: &'a [Games],
    listed: Option<&'a HashSet<(&'static str, String)>>,
    excluded: &'a Excluded,
}

fn opt_f(v: Option<f64>, digits: usize) -> String {
    v.map(|v| format!("{v:.digits$}")).unwrap_or_default()
}

fn override_text(candidate: &Candidate) -> String {
    candidate
        .overrides
        .iter()
        .map(|(prefix, mv)| format!("{}:{mv}", prefix.join(" ")))
        .collect::<Vec<_>>()
        .join(";")
}

const TABLE_COLUMNS: &str = "name\tside\toverride\toverride_weight\tmean_black\tp10\tp25\tmedian\tp75\tp90\tge50\tge100\tle80\tle0\tdraw\ttree6\ttree3_5\ttree1_2\tsingle\tmean_kind\tunknown\tno_value\town\tout\trare\ttiny\tmax\town_lt20\town_lt30\texit_games\texit_score\tbranch_games\tbranch_score\tdanger_strong\tdanger_listed\tdanger_both\tdanger_any\tblack_below\tdeviate\tdeviate_median";

fn format_table(reports: &[Report], summaries: &[Summary]) -> String {
    let mut text = format!("{TABLE_COLUMNS}\n");
    for (r, s) in reports.iter().zip(summaries) {
        let w = |v: f64| format!("{v:.6}");
        let (b, wh) = if r.candidate.side == "b" {
            ([w(s.hits[0]), w(s.hits[1])], [String::new(), String::new()])
        } else {
            ([String::new(), String::new()], [w(s.hits[0]), w(s.hits[1])])
        };
        let mut cells = vec![
            r.candidate.name.clone(),
            r.candidate.side.to_owned(),
            override_text(&r.candidate),
            r.override_weight.map(w).unwrap_or_default(),
            // -0.04 を "-0.0" と書かない。
            opt_f(s.mean.map(|m| if m.abs() < 0.05 { 0.0 } else { m }), 1),
        ];
        cells.extend(s.quantiles.iter().map(|q| q.map(|v| v.to_string()).unwrap_or_default()));
        cells.extend(b);
        cells.extend(wh);
        cells.push(w(s.draw));
        cells.extend(s.kinds.iter().map(|&v| w(v)));
        cells.extend(s.stops.iter().map(|&v| w(v)));
        cells.extend([w(s.own_lt20), w(s.own_lt30)]);
        cells.extend([
            s.exit_games.to_string(),
            opt_f(s.exit_score, 3),
            s.branch_games.to_string(),
            opt_f(s.branch_score, 3),
        ]);
        cells.extend([s.danger_auto, s.danger_listed, s.danger_both, s.danger_any].map(w));
        cells.push(s.black_below.map(w).unwrap_or_default());
        cells.push(w(s.deviate));
        cells.push(s.deviate_median.map(|v| v.to_string()).unwrap_or_default());
        debug_assert_eq!(cells.len(), TABLE_COLUMNS.split('\t').count());
        text.push_str(&cells.join("\t"));
        text.push('\n');
    }
    text
}

fn pct(w: f64) -> String {
    format!("{:.1}%", w * 100.0)
}

fn games_text(g: Games, side: &str) -> String {
    match g.score(side) {
        Some(s) => format!("{} 局・{s:.2}", g.games),
        None => "0 局".to_owned(),
    }
}

fn value_text(v: Option<i64>) -> String {
    match v {
        Some(_) => fmt_value(v),
        None => "—".to_owned(),
    }
}

fn markdown(inputs: &Inputs, reports: &[Report], summaries: &[Summary]) -> Result<String> {
    let args = inputs.args;
    let mut out = String::new();
    let title = if args.title.is_empty() {
        "戦型の比較".to_owned()
    } else {
        format!("{} の戦型の比較", args.title)
    };
    writeln!(out, "# {title}\n")?;
    writeln!(
        out,
        "候補ごとに、RAMU の手番の決めた局面では main の代わりに決めた手を指し、相手は強い対局の頻度 (足りなければ外部定跡) で指すとして初期局面から辿った (`master walk` と同じ辿り)。重み = 辿り全体を 100% とした割合 (RAMU の側の辿りだけ)。値 = 出口の局面までに RAMU が最後に指した手の値 (元データの evals の平均、評価値、プラス = 先手良し。その手に元データの行が無ければ値なし)。値の無い出口は平均・分位に入れない。千日手 = 辿りの循環 (局面の行き来の輪) の中で max の手数に達した (循環を回り続けた) 重みと、own の出口で外部定跡の最善を両者が指し続けた手順が同じ局面に戻る重み。強い対局 = 両者 R{} 以上の対局の局数と RAMU の側の得点 (勝ち 1・引き分け 0.5)。\n",
        args.records_min_rate
    )?;
    for side in ["b", "w"] {
        let rows: Vec<(&Report, &Summary)> = reports
            .iter()
            .zip(summaries)
            .filter(|(r, _)| r.candidate.side == side)
            .collect();
        if rows.is_empty() {
            continue;
        }
        let name = side_name(side);
        writeln!(out, "## RAMU が{name}の候補\n")?;
        let (h1, h2) = if side == "b" {
            ("+50 以上", "+100 以上")
        } else {
            ("+80 以下", "0 以下")
        };
        writeln!(out, "### 出口の値 (評価値、プラス = 先手良し)\n")?;
        writeln!(
            out,
            "| 候補 | 分岐 | 分岐に来る重み | 平均 | 10% 点 | 25% 点 | 中央値 | 75% 点 | 90% 点 | {h1} の重み | {h2} の重み | 千日手の重み |\n|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|"
        )?;
        for (r, s) in &rows {
            let q = |i: usize| value_text(s.quantiles[i]);
            writeln!(
                out,
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                r.candidate.name,
                r.branches.iter().map(|b| b.label.as_str()).collect::<Vec<_>>().join("、"),
                pct(r.override_weight.unwrap_or_else(|| r.branches.iter().map(|b| b.reach).sum())),
                s.mean.map(|m| fmt_value(Some(m.round() as i64))).unwrap_or_else(|| "—".into()),
                q(0),
                q(1),
                q(2),
                q(3),
                q(4),
                pct(s.hits[0]),
                pct(s.hits[1]),
                pct(s.draw)
            )?;
        }
        writeln!(out)?;
        writeln!(out, "### 値の確度 (重み)\n")?;
        writeln!(
            out,
            "値の出所は元データの value_kind (木の段数 = tree:N の N)。止まり: own = RAMU の定跡が切れる、out = 相手の手が分からない、max = {} 手に達した、rare = 局数の少ない相手の手、tiny = 重みが小さい。\n",
            args.max_ply
        )?;
        writeln!(
            out,
            "| 候補 | 6 段以上の木 | 3〜5 段の木 | 1〜2 段の木 | 1 回の読み | 複数回の読みの平均 | 出所の記録なし | 値なし | own | out | max | rare | tiny | own (20 手目未満) | own (30 手目未満) |\n|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|"
        )?;
        for (r, s) in &rows {
            let stop = |stop: Stop| {
                pct(s.stops[Stop::ALL.iter().position(|&x| x == stop).expect("ALL にある")])
            };
            writeln!(
                out,
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                r.candidate.name,
                pct(s.kinds[0]),
                pct(s.kinds[1]),
                pct(s.kinds[2]),
                pct(s.kinds[3]),
                pct(s.kinds[4]),
                pct(s.kinds[5]),
                pct(s.kinds[6]),
                stop(Stop::Own),
                stop(Stop::Out),
                stop(Stop::Max),
                stop(Stop::Rare),
                stop(Stop::Tiny),
                pct(s.own_lt20),
                pct(s.own_lt30)
            )?;
        }
        writeln!(out)?;
        writeln!(out, "### 強い対局の実績と危ない局面を通る重み\n")?;
        writeln!(
            out,
            "強い対局の判定 = 辿りの途中か出口に、強い対局で {} 局以上・RAMU の側の得点 {} 未満の局面がある。表の局面 = `--danger` の表の局面を通る。出口の局数・得点は同じ局面を 1 回だけ数えた和。{}\n",
            args.danger_games,
            args.danger_score,
            if side == "b" {
                format!(
                    "先手の得点 {} 未満 = 出口かその 10・20 手前の局面が {} 局以上で先手の得点 {} 未満。",
                    args.black_score, args.danger_games, args.black_score
                )
            } else {
                String::new()
            }
        )?;
        let extra = if side == "b" {
            format!(" 先手の得点 {} 未満 |", args.black_score)
        } else {
            String::new()
        };
        writeln!(
            out,
            "| 候補 | 分岐の局面 (局数・得点) | 出口 (局数・得点) | 危ない局面を通る重み | 強い対局の判定 | 表の局面 | 両方 |{extra}\n|---|---:|---:|---:|---:|---:|---:|{}",
            if side == "b" { "---:|" } else { "" }
        )?;
        for (r, s) in &rows {
            let score = |n: u64, v: Option<f64>| match v {
                Some(v) => format!("{n} 局・{v:.2}"),
                None => "0 局".to_owned(),
            };
            write!(
                out,
                "| {} | {} | {} | {} | {} | {} | {} |",
                r.candidate.name,
                score(s.branch_games, s.branch_score),
                score(s.exit_games, s.exit_score),
                pct(s.danger_any),
                pct(s.danger_auto),
                if inputs.listed.is_some() {
                    pct(s.danger_listed)
                } else {
                    "—".into()
                },
                if inputs.listed.is_some() {
                    pct(s.danger_both)
                } else {
                    "—".into()
                },
            )?;
            if let Some(v) = s.black_below {
                write!(out, " {} |", pct(v))?;
            }
            writeln!(out)?;
        }
        writeln!(out)?;
        writeln!(out, "### 分岐の後の相手の手 (回避されやすさ)\n")?;
        writeln!(
            out,
            "割合 = 分岐の局面に来た重みのうち、その手に進む重み。中央値 = その手の先の出口の値の中央値。外す重み = 最も多い手以外に進む重み (辿り全体に対する割合)。割合 {} 未満の手は「その他」にまとめる。\n",
            pct(args.reply_min_share)
        )?;
        for (r, s) in &rows {
            writeln!(
                out,
                "- {}: 外す重み {} (その先の中央値 {})",
                r.candidate.name,
                pct(s.deviate),
                value_text(s.deviate_median)
            )?;
        }
        writeln!(out)?;
        for (r, _) in &rows {
            for b in &r.branches {
                writeln!(
                    out,
                    "#### {}: {} (来る重み {}、強い対局 {})\n",
                    r.candidate.name,
                    b.label,
                    pct(b.reach),
                    games_text(inputs.games[b.key as usize], side)
                )?;
                if b.reach <= 0.0 {
                    writeln!(out, "辿りはこの局面に来ない。\n")?;
                    continue;
                }
                writeln!(out, "| 相手の手 | 割合 | その先の中央値 |\n|---|---:|---:|")?;
                let mut replies: Vec<_> = b.replies.iter().collect();
                replies.sort_by(|a, b| b.1.0.total_cmp(&a.1.0).then(a.0.cmp(b.0)));
                let mut other = (0.0, Vec::new());
                for (mv, (w, vals)) in replies {
                    let share = w / b.reach;
                    if share < args.reply_min_share {
                        other.0 += w;
                        other.1.extend(vals.iter().copied());
                        continue;
                    }
                    let label = match mv.strip_prefix("stop:") {
                        Some(stop) => format!("(ここで止まり: {stop})"),
                        None => kif_last(&b.moves, mv)?,
                    };
                    writeln!(
                        out,
                        "| {label} | {} | {} |",
                        pct(share),
                        value_text(weighted_quantile(vals, 0.5))
                    )?;
                }
                if other.0 > 0.0 {
                    writeln!(
                        out,
                        "| その他 | {} | {} |",
                        pct(other.0 / b.reach),
                        value_text(weighted_quantile(&other.1, 0.5))
                    )?;
                }
                writeln!(out)?;
            }
        }
    }
    danger_section(&mut out, inputs, reports)?;
    reading_section(&mut out, inputs, reports)?;
    Ok(out)
}

/// 候補ごとに、辿りが最初に通る危ない局面 (強い対局の判定か `--danger` の表) と、そこを通る重み。
fn danger_section(out: &mut String, inputs: &Inputs, reports: &[Report]) -> Result<()> {
    let args = inputs.args;
    writeln!(out, "## 危ない局面 (候補ごとに最初に通る局面)\n")?;
    writeln!(
        out,
        "通る重み {} 以上を、候補ごとに重みの大きい順に {} 個まで。何手目 = その局面で次に指す手が何手目か。判定: 両方 = 強い対局 ({} 局以上・RAMU の側の得点 {} 未満) でも表でも危ない、強い対局だけ・表だけ = 片方だけで危ない。\n",
        pct(args.list_min_weight),
        args.top,
        args.danger_games,
        args.danger_score
    )?;
    writeln!(
        out,
        "| 候補 | 何手目 | 通る重み | 強い対局 (局数・RAMU の側の得点) | 判定 | 手順 |\n|---|---:|---:|---|---|---|"
    )?;
    for report in reports {
        let own = report.candidate.side;
        // 局面 → (通る重み, その局面までの手数が最も小さい (手数, 出口))。
        type Entry<'a> = (f64, Option<(usize, &'a Exit)>);
        let mut by_key: BTreeMap<u32, Entry> = BTreeMap::new();
        for e in &report.exits {
            let Some(id) = e.first_danger else {
                continue;
            };
            let at = e.path.iter().position(|&p| p == id);
            let entry = by_key.entry(id).or_insert((0.0, None));
            entry.0 += e.row.weight;
            if let Some(i) = at
                && entry.1.is_none_or(|(old, _)| i < old)
            {
                entry.1 = Some((i, e));
            }
        }
        let mut items: Vec<_> =
            by_key.into_iter().filter(|(_, (w, _))| *w >= args.list_min_weight).collect();
        items.sort_by(|a, b| b.1.0.total_cmp(&a.1.0).then(a.0.cmp(&b.0)));
        for (id, (weight, moves)) in items.into_iter().take(args.top) {
            let g = inputs.games[id as usize];
            let strong =
                g.games >= args.danger_games && g.score(own).is_some_and(|s| s < args.danger_score);
            let key = &inputs.keys.names[id as usize];
            let listed = inputs.listed.is_some_and(|l| l.contains(&(own, key.clone())));
            let class = match (strong, listed) {
                (true, true) => "両方",
                (true, false) => "強い対局だけ",
                _ => "表だけ",
            };
            let (ply, moves) = match moves {
                Some((i, e)) => ((i + 1).to_string(), kif(&e.move_list()[..i])?),
                None => ("—".to_owned(), format!("`{key}`")),
            };
            writeln!(
                out,
                "| {} | {ply} | {} | {} | {class} | {moves} |",
                report.candidate.name,
                pct(weight),
                games_text(g, own),
            )?;
        }
    }
    writeln!(out)?;
    Ok(())
}

/// 側ごとに、own で止まる出口を候補をまたいでまとめ、重み (候補の中の最大) の大きい順に `--top` 個。
fn reading_section(out: &mut String, inputs: &Inputs, reports: &[Report]) -> Result<()> {
    let args = inputs.args;
    writeln!(out, "## 締めまでに読みを足すと効く所 (own の出口)\n")?;
    writeln!(
        out,
        "RAMU の定跡が切れる局面。重み = その局面に来る重み (同じ局面に別の手数で来た分は足す、候補の中で最大のもの)。値 = 直前に RAMU が指した手の値 (評価値、プラス = 先手良し)。受付 = `--exclude-queue` のキューに同じ根・同じ側の木 (段 1 以上) があればその id。\n"
    )?;
    for side in ["b", "w"] {
        // 局面 → (候補の中の最大の重み, 候補の名前, 代表の出口)。
        let mut merged: BTreeMap<u32, (f64, Vec<&str>, &Exit)> = BTreeMap::new();
        for report in reports.iter().filter(|r| r.candidate.side == side) {
            let mut own: BTreeMap<u32, (f64, &Exit)> = BTreeMap::new();
            for e in report.exits.iter().filter(|e| e.row.stop == Stop::Own) {
                let id = *e.path.last().expect("初期局面がある");
                let entry = own.entry(id).or_insert((0.0, e));
                entry.0 += e.row.weight;
                if e.row.ply < entry.1.row.ply {
                    entry.1 = e;
                }
            }
            for (id, (weight, e)) in own {
                let entry = merged.entry(id).or_insert((0.0, Vec::new(), e));
                if weight > entry.0 {
                    entry.0 = weight;
                    entry.2 = e;
                }
                entry.1.push(&report.candidate.name);
            }
        }
        if merged.is_empty() {
            continue;
        }
        let mut items: Vec<_> = merged.into_iter().collect();
        items.sort_by(|a, b| b.1.0.total_cmp(&a.1.0).then(a.0.cmp(&b.0)));
        writeln!(out, "### RAMU が{}\n", side_name(side))?;
        writeln!(
            out,
            "| 手数 | 重み | 候補 | 値 | 出所 | 受付 | 手順 | 局面 (SFEN) |\n|---:|---:|---|---:|---|---|---|---|"
        )?;
        for (id, (weight, names, e)) in items.into_iter().take(args.top) {
            let key = &inputs.keys.names[id as usize];
            let queued = inputs
                .excluded
                .get(&(key.clone(), side.to_owned()))
                .map(|ids| ids.iter().cloned().collect::<Vec<_>>().join(", "))
                .unwrap_or_else(|| "なし".to_owned());
            writeln!(
                out,
                "| {} | {} | {} | {} | {} | {queued} | {} | `{key}` |",
                e.row.ply,
                pct(weight),
                names.join("・"),
                value_text(e.value),
                if e.kind.is_empty() { "—" } else { &e.kind },
                kif(&e.move_list())?
            )?;
        }
        writeln!(out)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
