//! `book_mine tree enqueue` と `book_mine tree wait`。

use super::targets::{
    DEFAULT_PRIORITY, ENDED, Line, Opts, QUEUE, QUEUE_LOCK, Receipt, STATUS_FILE, StatusFile,
};
use anyhow::{Context, Result, bail};
use clap::Args;
use serde_json::json;
use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// `book_mine tree enqueue` の引数。
#[derive(Args, Debug)]
pub struct EnqueueArgs {
    #[arg(long)]
    state: PathBuf,
    /// 末尾の書きかけの行 (最後の改行より後) だけを切り詰める
    #[arg(long, conflicts_with_all = ["id", "from"])]
    repair: bool,
    /// 複数の行 (JSON Lines) を全部検査してからまとめて足す
    #[arg(long, conflicts_with = "id")]
    from: Option<PathBuf>,
    #[arg(long, required_unless_present_any = ["from", "repair"])]
    id: Option<String>,
    /// `sfen <盤> <手番> <持駒> <手数>` か `startpos moves <USI の手順>`
    #[arg(long, conflicts_with_all = ["from", "repair"])]
    root: Option<String>,
    /// b / w / bw
    #[arg(long, conflicts_with_all = ["from", "repair"])]
    own: Option<String>,
    #[arg(long, conflicts_with_all = ["from", "repair"])]
    depth: Option<usize>,
    /// 小さいほど先 (目安: 0 = 急ぎ、100 = 既定、1000 = 空きを埋めるだけ)
    #[arg(long, conflicts_with_all = ["from", "repair"])]
    priority: Option<u32>,
    #[arg(long, conflicts_with_all = ["from", "repair"])]
    hold: bool,
    /// 根からの実戦の手順 (USI を空白区切り)。繰り返して複数
    #[arg(long, conflicts_with_all = ["from", "repair"])]
    game: Vec<String>,
    #[arg(long, conflicts_with_all = ["from", "repair"])]
    opp_n: Option<usize>,
    #[arg(long, conflicts_with_all = ["from", "repair"])]
    stop: Option<i32>,
    #[arg(long, conflicts_with_all = ["from", "repair"])]
    rounds: Option<usize>,
    #[arg(long, conflicts_with_all = ["from", "repair"], allow_hyphen_values = true)]
    draw: Option<i32>,
    /// 根で伸ばす手。繰り返して複数
    #[arg(long, conflicts_with_all = ["from", "repair"])]
    extra: Vec<String>,
    #[arg(long, conflicts_with_all = ["from", "repair"])]
    memo: Option<String>,
}

/// `book_mine tree wait` の引数。
#[derive(Args, Debug)]
pub struct WaitArgs {
    #[arg(long)]
    state: PathBuf,
    /// enqueue が出した受領 (JSON 1 行)
    #[arg(long)]
    receipt: String,
    /// 終わりを待つ id (カンマ区切り)。省くと全対象。受領の範囲で足した id は必ず含める
    #[arg(long, value_delimiter = ',')]
    ids: Vec<String>,
}

fn line_from_args(args: &EnqueueArgs) -> Result<Line> {
    let need = |name: &str| format!("--id では --{name} が必要です");
    Ok(Line {
        id: args.id.clone().context("--id が必要です")?,
        root: args.root.clone().with_context(|| need("root"))?,
        own: args.own.clone().with_context(|| need("own"))?,
        depth: args.depth.with_context(|| need("depth"))?,
        priority: args.priority.unwrap_or(DEFAULT_PRIORITY),
        hold: args.hold,
        game: args
            .game
            .iter()
            .map(|g| g.split_whitespace().map(str::to_owned).collect())
            .collect(),
        opts: Opts {
            opp_n: args.opp_n,
            stop: args.stop,
            rounds: args.rounds,
            draw: args.draw,
            extra: (!args.extra.is_empty()).then(|| args.extra.clone()),
        },
        memo: args.memo.clone(),
    })
}

fn open_lock(state: &Path) -> Result<File> {
    let path = state.join(QUEUE_LOCK);
    super::paths::reject_symlink(&path)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)?;
    // 書き手どうしは待ち合わせる (serve はこの lock を取らずに改行までを読む)。
    file.lock().with_context(|| format!("lock 取得失敗: {}", path.display()))?;
    Ok(file)
}

/// 検査を通った行だけをキューに追記し、受領を標準出力に出す。
pub(super) fn enqueue(args: EnqueueArgs) -> Result<()> {
    std::fs::create_dir_all(&args.state)?;
    let queue = args.state.join(QUEUE);
    super::paths::reject_symlink(&queue)?;
    if std::fs::symlink_metadata(&queue).is_ok_and(|m| !m.file_type().is_file()) {
        bail!("キューは通常のファイルにしてください: {}", queue.display());
    }
    let lines = if args.repair {
        Vec::new()
    } else if let Some(from) = &args.from {
        let mut lines = Vec::new();
        for (n, text) in BufReader::new(File::open(from)?).lines().enumerate() {
            let text = text?;
            if text.trim().is_empty() {
                continue;
            }
            let line: Line = serde_json::from_str(&text)
                .with_context(|| format!("{}:{}", from.display(), n + 1))?;
            lines.push(line);
        }
        if lines.is_empty() {
            bail!("足す行がありません: {}", from.display());
        }
        lines
    } else {
        vec![line_from_args(&args)?]
    };
    for line in &lines {
        line.check().with_context(|| format!("id={}", line.id))?;
    }
    let lock = open_lock(&args.state)?;
    if args.repair {
        repair(&queue)?;
    } else {
        let receipt = append(&queue, &lines)?;
        println!("{}", serde_json::to_string(&receipt)?);
    }
    drop(lock);
    Ok(())
}

fn append(queue: &Path, lines: &[Line]) -> Result<Receipt> {
    let mut text = String::new();
    for line in lines {
        text.push_str(&serde_json::to_string(line)?);
        text.push('\n');
    }
    let created = !queue.try_exists()?;
    let mut file = OpenOptions::new().create(true).read(true).append(true).open(queue)?;
    let start = file.metadata()?.len();
    if start > 0 {
        let mut last = [0];
        file.seek(SeekFrom::End(-1))?;
        file.read_exact(&mut last)?;
        if last[0] != b'\n' {
            bail!(
                "キューの末尾に書きかけの行があります。`book_mine tree enqueue --repair --state <state>` で切り詰めてから足してください"
            );
        }
    }
    file.write_all(text.as_bytes())?;
    file.sync_data()?;
    if created && let Some(parent) = queue.parent() {
        crate::common::io::sync_directory(parent)?;
    }
    Ok(Receipt {
        start,
        end: start + text.len() as u64,
        lines: lines.len(),
    })
}

fn repair(queue: &Path) -> Result<()> {
    let bytes = match std::fs::read(queue) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    let keep = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    if keep == bytes.len() {
        eprintln!("書きかけの行はありません");
        return Ok(());
    }
    eprintln!("切り詰めた書きかけの行: {}", String::from_utf8_lossy(&bytes[keep..]));
    let file = OpenOptions::new().write(true).open(queue)?;
    file.set_len(keep as u64)?;
    file.sync_data()?;
    Ok(())
}

/// serve が state の lock を持っていれば動いている。
fn serve_alive(state: &Path) -> Result<bool> {
    let file = match File::open(state.join("lock")) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    match file.try_lock_shared() {
        Ok(()) => {
            file.unlock()?;
            Ok(false)
        }
        Err(std::fs::TryLockError::WouldBlock) => Ok(true),
        Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
    }
}

fn read_status(state: &Path) -> Result<Option<StatusFile>> {
    match std::fs::read_to_string(state.join(STATUS_FILE)) {
        Ok(text) => Ok(Some(serde_json::from_str(&text).context("状態のファイルが不正です")?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn serve_run_id(state: &Path) -> Option<String> {
    std::fs::read_to_string(state.join("STATUS"))
        .ok()
        .and_then(|s| s.split_whitespace().nth(1).map(str::to_owned))
}

/// 受領の範囲の行で足した id。
fn receipt_ids(state: &Path, receipt: &Receipt) -> Result<BTreeSet<String>> {
    let bytes = std::fs::read(state.join(QUEUE))?;
    let range = usize::try_from(receipt.start)?..usize::try_from(receipt.end)?;
    let chunk = bytes.get(range).context("受領の範囲がキューの外です")?;
    let mut ids = BTreeSet::new();
    for line in chunk.split(|b| *b == b'\n').filter(|l| !l.iter().all(u8::is_ascii_whitespace)) {
        let value: serde_json::Value =
            serde_json::from_slice(line).context("受領の範囲の行が不正です")?;
        ids.insert(value["id"].as_str().context("受領の範囲の行に id がありません")?.to_owned());
    }
    Ok(ids)
}

/// 受領の範囲が処理され、対象が全部終わるまで待つ。
pub(super) fn wait(args: WaitArgs) -> Result<()> {
    let receipt: Receipt = serde_json::from_str(&args.receipt)
        .context("--receipt には enqueue の出力 (JSON 1 行) を渡してください")?;
    if receipt.start > receipt.end {
        bail!("受領の範囲が不正です");
    }
    let mut ids: BTreeSet<String> = args.ids.iter().cloned().collect();
    if !ids.is_empty() {
        ids.extend(receipt_ids(&args.state, &receipt)?);
    }
    loop {
        let alive = serve_alive(&args.state)?;
        let run_id = serve_run_id(&args.state);
        let status = read_status(&args.state)?;
        if let Some(status) = &status
            && status.processed_bytes < receipt.end
            && let Some(error) = &status.queue_error
            && alive
            && run_id.as_ref() == Some(&status.run_id)
        {
            bail!("serve がキューを受け付けていません: {error}");
        }
        if let Some(status) = status
            && status.processed_bytes >= receipt.end
        {
            let rejected: Vec<_> = status
                .rejected
                .iter()
                .filter(|r| receipt.start <= r.start && r.start < receipt.end)
                .collect();
            if !rejected.is_empty() {
                for r in &rejected {
                    eprintln!(
                        "拒否 [{}, {}) id={}: {}",
                        r.start,
                        r.end,
                        r.id.as_deref().unwrap_or("-"),
                        r.reason
                    );
                }
                bail!("受領の範囲の行が拒否されました ({} 行)", rejected.len());
            }
            let selected: Vec<_> = status
                .targets
                .iter()
                .filter(|t| ids.is_empty() || ids.contains(&t.id))
                .collect();
            let unknown: Vec<_> =
                ids.iter().filter(|id| !selected.iter().any(|t| &t.id == *id)).collect();
            if !unknown.is_empty() {
                bail!("状態に無い id: {unknown:?}");
            }
            let ended = if ids.is_empty() {
                status.all_ended
            } else {
                selected.iter().all(|t| ENDED.contains(&t.state.as_str()))
            };
            if ended && alive && run_id.as_ref() == Some(&status.run_id) {
                let targets: Vec<_> = selected
                    .iter()
                    .map(|t| json!({"id":t.id,"own":t.own,"generation":t.generation,"state":t.state,"ready":t.ready,"out":t.out}))
                    .collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &json!({"run_id":status.run_id,"targets":targets})
                    )?
                );
                return Ok(());
            }
        }
        if !alive {
            bail!("serve が動いていません: {}", args.state.display());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}
