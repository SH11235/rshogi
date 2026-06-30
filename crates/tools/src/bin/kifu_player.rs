//! PSV / tournament JSONL 共通の棋譜プレイヤー TUI。
//!
//! 詳細は `crates/tools/docs/kifu_player.md` を参照。

use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::Parser;
use tools::replay::{GameSource, JsonlSource, PsvSource, tui};

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "PSV / tournament JSONL の対局を TUI で再生する"
)]
struct Cli {
    /// PSV (PackedSfenValue) ファイルを開く。連続した自己対局ストリームを想定する
    /// （shuffle_psv/merge_psv 等でシャッフル済みのプールは対局境界検出が機能しない）。
    #[arg(long, conflicts_with = "tournament_dir")]
    psv: Option<PathBuf>,

    /// tournament の out-dir を開く（配下の `*-vs-*.jsonl` を横断して索引する）。
    #[arg(long, conflicts_with = "psv")]
    tournament_dir: Option<PathBuf>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let source: Box<dyn GameSource> = match (cli.psv, cli.tournament_dir) {
        (Some(path), None) => Box::new(PsvSource::new(path)),
        (None, Some(dir)) => Box::new(JsonlSource::new(dir)),
        (None, None) => bail!("--psv か --tournament-dir のどちらか一方を指定してください"),
        (Some(_), Some(_)) => unreachable!("clap の conflicts_with により同時指定は弾かれる"),
    };
    tui::run(source)
}
