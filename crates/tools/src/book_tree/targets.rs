//! 読む対象のキュー (`<state>/targets.jsonl`) の行と、serve のチェックポイント・状態の形。

use super::model::{GameLine, Tree, TreeParams, child};
use crate::book_db::{parse_position_line, strip_ply};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub(super) const QUEUE: &str = "targets.jsonl";
pub(super) const QUEUE_LOCK: &str = "targets.lock";
pub(super) const CHECKPOINT: &str = "accepted.json";
pub(super) const STATUS_FILE: &str = "targets.status.json";
const CHECKPOINT_VERSION: &str = "book-tree-queue-v1";
pub(super) const DEFAULT_PRIORITY: u32 = 100;
const MAX_PRIORITY: u32 = 1_000_000;
const MAX_DEPTH: usize = 20;
const MAX_EXTRA: usize = 8;

/// 伸ばし方の値。書かない項目は serve の既定で解決する。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Opts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opp_n: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rounds: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draw: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Vec<String>>,
}
impl Opts {
    fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

fn default_priority() -> u32 {
    DEFAULT_PRIORITY
}

/// キューの 1 行。同じ `id` では検査を通った最後の行が有効になる。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Line {
    pub id: String,
    pub root: String,
    pub own: String,
    pub depth: usize,
    #[serde(default = "default_priority")]
    pub priority: u32,
    #[serde(default)]
    pub hold: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub game: Vec<Vec<String>>,
    #[serde(default, skip_serializing_if = "Opts::is_empty")]
    pub opts: Opts,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memo: Option<String>,
}

/// 根の局面 (手数を除く key と手数)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Root {
    pub key: String,
    pub ply: u32,
}

fn parse_root(root: &str) -> Result<Root> {
    let tokens: Vec<_> = root.split_whitespace().collect();
    let valid = match tokens.as_slice() {
        ["sfen", _, _, _, _] => true,
        ["startpos", rest @ ..] => rest.is_empty() || rest[0] == "moves",
        _ => false,
    };
    if !valid {
        bail!("root は `sfen <盤> <手番> <持駒> <手数>` か `startpos moves <手順>` が必要です");
    }
    let sfen = parse_position_line(root)?;
    let key = strip_ply(&sfen).to_owned();
    let ply: u32 = sfen
        .rsplit_once(' ')
        .map(|(_, ply)| ply)
        .context("root の手数がありません")?
        .parse()
        .context("root の手数が不正です")?;
    if ply == 0 {
        bail!("root の手数は正数が必要です");
    }
    Ok(Root { key, ply })
}

fn check_range<T: PartialOrd + std::fmt::Display>(name: &str, v: T, lo: T, hi: T) -> Result<()> {
    if v < lo || v > hi {
        bail!("{name} は {lo}〜{hi} が必要です: {v}");
    }
    Ok(())
}

/// serve の既定 (`--opp-n` など)。行の書かない項目をこれで解決する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Defaults {
    pub opp_n: usize,
    pub stop: i32,
    pub rounds: usize,
    pub draw: i32,
}
impl Defaults {
    pub fn check(&self) -> Result<()> {
        check_range("opp_n", self.opp_n, 1, 8)?;
        check_range("stop", self.stop, 1, 30000)?;
        check_range("rounds", self.rounds, 0, 10)?;
        check_range("draw", self.draw, -30000, 30000)
    }
}

impl Line {
    /// 行の形と局面・手順の合法性を検査する。enqueue と serve が同じ検査を使う。
    pub fn check(&self) -> Result<Root> {
        if self.id.is_empty()
            || self.id.len() > 64
            || !self.id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            bail!("id は [A-Za-z0-9_-] の 1〜64 文字が必要です: {}", self.id);
        }
        if !matches!(self.own.as_str(), "b" | "w" | "bw") {
            bail!("own は b / w / bw が必要です: {}", self.own);
        }
        check_range("depth", self.depth, 1, MAX_DEPTH)?;
        check_range("priority", self.priority, 0, MAX_PRIORITY)?;
        let root = parse_root(&self.root)?;
        for moves in &self.game {
            let mut key = root.key.clone();
            for mv in moves {
                key = child(&key, mv).with_context(|| format!("game の手が不正です: {mv}"))?;
            }
        }
        let o = &self.opts;
        if let Some(v) = o.opp_n {
            check_range("opp_n", v, 1, 8)?;
        }
        if let Some(v) = o.stop {
            check_range("stop", v, 1, 30000)?;
        }
        if let Some(v) = o.rounds {
            check_range("rounds", v, 0, 10)?;
        }
        if let Some(v) = o.draw {
            check_range("draw", v, -30000, 30000)?;
        }
        if let Some(extra) = &o.extra {
            if extra.len() > MAX_EXTRA {
                bail!("extra は {MAX_EXTRA} 手までです");
            }
            for mv in extra {
                child(&root.key, mv).with_context(|| format!("extra の手が根で不正です: {mv}"))?;
            }
        }
        Ok(root)
    }

    pub fn sides(&self) -> Vec<String> {
        self.own.chars().map(String::from).collect()
    }

    pub fn resolve(&self, root: &Root, defaults: &Defaults) -> Resolved {
        Resolved {
            key: root.key.clone(),
            ply: root.ply,
            depth: self.depth,
            opp_n: self.opts.opp_n.unwrap_or(defaults.opp_n),
            stop: self.opts.stop.unwrap_or(defaults.stop),
            rounds: self.opts.rounds.unwrap_or(defaults.rounds),
            draw: self.opts.draw.unwrap_or(defaults.draw),
            extra: self.opts.extra.clone().unwrap_or_default(),
            game: self.game.clone(),
        }
    }
}

/// 既定で解決した木の定義。これが今の世代と違えば新しい世代を作る。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Resolved {
    pub key: String,
    pub ply: u32,
    pub depth: usize,
    pub opp_n: usize,
    pub stop: i32,
    pub rounds: usize,
    pub draw: i32,
    pub extra: Vec<String>,
    pub game: Vec<Vec<String>>,
}
impl Resolved {
    pub fn params(&self, own: &str, out: &Path) -> TreeParams {
        TreeParams {
            out: out.to_owned(),
            depth: self.depth,
            opp_n: self.opp_n,
            stop: self.stop,
            own: own.to_owned(),
            rounds: self.rounds,
            extra: self.extra.clone(),
            draw: self.draw,
        }
    }

    pub fn lines(&self) -> Vec<GameLine> {
        if self.game.is_empty() {
            return vec![(self.key.clone(), self.ply, Vec::new())];
        }
        self.game
            .iter()
            .map(|moves| (self.key.clone(), self.ply, moves.clone()))
            .collect()
    }

    /// 記録用の paths.tsv 形式 (`tree replay` の paths にも使える)。
    pub fn record(&self) -> String {
        self.lines()
            .iter()
            .map(|(key, ply, moves)| format!("{key}\t{ply}\t{}\tqueue\n", moves.join(" ")))
            .collect()
    }

    pub fn build(&self, own: &str, out: &Path) -> Result<Tree> {
        Tree::build(self.params(own, out), &self.lines())
    }
}

/// 受け付けた対象 1 本 (`bw` は手番ごとに 2 本)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Target {
    pub id: String,
    pub own: String,
    /// `id` が最初に受け付けられた順。世代を変えても変わらない。
    pub order: u64,
    pub generation: u32,
    pub definition: Resolved,
    pub out: PathBuf,
    pub priority: u32,
    pub hold: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memo: Option<String>,
}

/// 拒否した行。位置はキューのバイト範囲 `[start, end)`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Rejected {
    pub start: u64,
    pub end: u64,
    pub id: Option<String>,
    pub reason: String,
}

/// 受付・拒否・処理位置を 1 回の書き込みで確定する。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Checkpoint {
    pub version: String,
    pub out_root: PathBuf,
    pub fingerprint: String,
    pub processed_bytes: u64,
    pub processed_sha256: String,
    pub next_order: u64,
    pub targets: Vec<Target>,
    pub rejected: Vec<Rejected>,
}
impl Checkpoint {
    pub fn new(out_root: PathBuf, fingerprint: String) -> Self {
        Self {
            version: CHECKPOINT_VERSION.into(),
            out_root,
            fingerprint,
            processed_bytes: 0,
            processed_sha256: format!("{:x}", Sha256::digest([])),
            next_order: 0,
            targets: Vec::new(),
            rejected: Vec::new(),
        }
    }

    pub fn load(path: &Path) -> Result<Option<Self>> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let value: serde_json::Value = serde_json::from_str(&text)
            .with_context(|| format!("チェックポイントを読めません: {}", path.display()))?;
        if value.is_array() {
            bail!(
                "inbox 受付の state ({}) からは再開できません。新しい state dir で起動し、--journal で古い台帳を指定してください",
                path.display()
            );
        }
        let checkpoint: Self = serde_json::from_value(value)
            .with_context(|| format!("チェックポイントの形式が不正です: {}", path.display()))?;
        if checkpoint.version != CHECKPOINT_VERSION {
            bail!("チェックポイントの版が違います: {}", checkpoint.version);
        }
        Ok(Some(checkpoint))
    }

    pub fn position(&self, id: &str, own: &str) -> Option<usize> {
        self.targets.iter().position(|t| t.id == id && t.own == own)
    }

    /// 改行で終わる新しい行 (`chunk` はキューの `processed_bytes` から始まる) を順に処理した
    /// 次のチェックポイントと、世代が変わった対象の位置と木を返す。ファイルには書かない。
    pub fn apply(
        &self,
        chunk: &[u8],
        defaults: &Defaults,
        prefix: &Sha256,
        protected: &[PathBuf],
    ) -> (Self, Vec<(usize, Tree)>) {
        let mut next = self.clone();
        let mut built: Vec<(usize, Tree)> = Vec::new();
        let mut hasher = prefix.clone();
        let mut offset = self.processed_bytes;
        for line in chunk.split_inclusive(|b| *b == b'\n') {
            let start = offset;
            offset += line.len() as u64;
            hasher.update(line);
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            match next.accept(line, defaults, protected) {
                Ok(changed) => {
                    for (index, tree) in changed {
                        built.retain(|(i, _)| *i != index);
                        built.push((index, tree));
                    }
                }
                Err(error) => next.rejected.push(Rejected {
                    start,
                    end: offset,
                    id: serde_json::from_slice::<serde_json::Value>(line)
                        .ok()
                        .and_then(|v| v.get("id").and_then(|id| id.as_str()).map(str::to_owned)),
                    reason: format!("{error:#}"),
                }),
            }
        }
        next.processed_bytes = offset;
        next.processed_sha256 = format!("{:x}", hasher.finalize());
        (next, built)
    }

    fn accept(
        &mut self,
        bytes: &[u8],
        defaults: &Defaults,
        protected: &[PathBuf],
    ) -> Result<Vec<(usize, Tree)>> {
        let line: Line = serde_json::from_slice(bytes).context("行の形式が不正です")?;
        let root = line.check()?;
        let resolved = line.resolve(&root, defaults);
        let sides = line.sides();
        // Windows では大小文字だけ違う id が同じ出力先になる。
        if self
            .targets
            .iter()
            .any(|t| t.id != line.id && t.id.eq_ignore_ascii_case(&line.id))
        {
            bail!("大小文字だけが違う id が既にあります: {}", line.id);
        }
        let known: Vec<_> = self.targets.iter().filter(|t| t.id == line.id).collect();
        if let Some(first) = known.first() {
            let known_sides: Vec<_> = known.iter().map(|t| t.own.clone()).collect();
            if known_sides != sides
                || first.definition.key != resolved.key
                || first.definition.ply != resolved.ply
            {
                bail!(
                    "id ごとに root と own は変えられません (別の id で足してください): {}",
                    line.id
                );
            }
        }
        let order = known.first().map_or(self.next_order, |t| t.order);
        let mut staged = Vec::new();
        for side in &sides {
            let current = self.position(&line.id, side).map(|i| &self.targets[i]);
            if let Some(current) = current
                && current.definition == resolved
            {
                staged.push((
                    Target {
                        priority: line.priority,
                        hold: line.hold,
                        memo: line.memo.clone(),
                        ..current.clone()
                    },
                    None,
                ));
                continue;
            }
            let generation = current.map_or(1, |t| t.generation + 1);
            let out = self.out_root.join(&line.id).join(side).join(format!("g{generation}"));
            for path in [
                out.parent().and_then(Path::parent),
                out.parent(),
                Some(out.as_path()),
            ]
            .into_iter()
            .flatten()
            {
                super::paths::reject_symlink(path)?;
            }
            super::paths::reject_protected(&out, protected)?;
            let tree = resolved.build(side, &out)?;
            staged.push((
                Target {
                    id: line.id.clone(),
                    own: side.clone(),
                    order,
                    generation,
                    definition: resolved.clone(),
                    out,
                    priority: line.priority,
                    hold: line.hold,
                    memo: line.memo.clone(),
                },
                Some(tree),
            ));
        }
        // bw は 2 本とも作れたときだけここまで来る。
        if known.is_empty() {
            self.next_order += 1;
        }
        let mut changed = Vec::new();
        for (target, tree) in staged {
            let index = match self.position(&target.id, &target.own) {
                Some(index) => {
                    self.targets[index] = target;
                    index
                }
                None => {
                    self.targets.push(target);
                    self.targets.len() - 1
                }
            };
            if let Some(tree) = tree {
                changed.push((index, tree));
            }
        }
        Ok(changed)
    }
}

/// 対象ごとの状態。`targets.status.json` に出す。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct TargetStatus {
    pub id: String,
    pub own: String,
    pub generation: u32,
    pub priority: u32,
    pub hold: bool,
    pub state: String,
    pub ready: bool,
    pub read: usize,
    pub required: usize,
    pub out: PathBuf,
}

/// `targets.status.json` の中身。チェックポイントからいつでも作り直せる。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct StatusFile {
    pub run_id: String,
    pub processed_bytes: u64,
    pub processed_sha256: String,
    pub all_ended: bool,
    /// キューの受付が止まっている理由 (処理済みの範囲の変化、チェックポイントを書けないなど)。
    pub queue_error: Option<String>,
    pub targets: Vec<TargetStatus>,
    pub rejected: Vec<Rejected>,
}

pub(super) const ENDED: [&str; 4] = ["DONE", "NOT_READY", "FAIL", "HELD"];

impl StatusFile {
    pub fn new(
        run_id: &str,
        checkpoint: &Checkpoint,
        targets: Vec<TargetStatus>,
        queue_error: Option<String>,
    ) -> Self {
        Self {
            run_id: run_id.into(),
            processed_bytes: checkpoint.processed_bytes,
            processed_sha256: checkpoint.processed_sha256.clone(),
            all_ended: targets.iter().all(|t| ENDED.contains(&t.state.as_str())),
            queue_error,
            targets,
            rejected: checkpoint.rejected.clone(),
        }
    }

    /// 再開直後、出力に触れる前に出す状態。
    pub fn resuming(run_id: &str, checkpoint: &Checkpoint) -> Self {
        let targets = checkpoint
            .targets
            .iter()
            .map(|t| TargetStatus {
                id: t.id.clone(),
                own: t.own.clone(),
                generation: t.generation,
                priority: t.priority,
                hold: t.hold,
                state: "RESUMING".into(),
                ready: false,
                read: 0,
                required: 0,
                out: t.out.clone(),
            })
            .collect();
        Self::new(run_id, checkpoint, targets, None)
    }
}

/// enqueue が標準出力に出す受領。この追記のバイト範囲 `[start, end)` と行数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Receipt {
    pub start: u64,
    pub end: u64,
    pub lines: usize,
}
