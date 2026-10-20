//! 条件付き台帳を共有する、独立した定跡の木の生成。

mod journal;
mod model;
mod output;
mod paths;
mod service;
#[cfg(test)]
mod tests;

use anyhow::Result;
use clap::{Args, Subcommand};
use journal::{Journal, Settings};
use model::{Definition, Tree};
use std::path::PathBuf;

/// 木の常駐・取り込み・再生の入口。
#[derive(Args, Debug)]
pub struct TreeArgs {
    #[command(subcommand)]
    command: TreeCommand,
}

#[derive(Subcommand, Debug)]
enum TreeCommand {
    /// inbox の木を共有エンジンで読む
    Serve(ServeArgs),
    /// 宣言した条件で node_calib JSONL を取り込む
    Import(ImportArgs),
    /// 台帳だけから木を再生成する
    Replay(ReplayArgs),
}

#[derive(Args, Debug, Clone)]
struct ReadingArgs {
    #[arg(long)]
    engine: PathBuf,
    #[arg(long = "engine-option")]
    engine_options: Vec<String>,
    #[arg(long)]
    nodes: u64,
    #[arg(long, default_value_t = 3)]
    multipv: usize,
    #[arg(long, default_value_t = 1024)]
    hash: u32,
}

#[derive(Args, Debug)]
struct ServeArgs {
    #[arg(long)]
    state: PathBuf,
    #[arg(long)]
    journal: Option<PathBuf>,
    #[command(flatten)]
    reading: ReadingArgs,
    #[arg(long, default_value_t = 16)]
    workers: usize,
    #[arg(long, default_value_t = 600)]
    stall_secs: u64,
    #[arg(long, default_value_t = 3)]
    max_restarts: usize,
}

#[derive(Args, Debug)]
struct ImportArgs {
    #[arg(long)]
    journal: PathBuf,
    #[command(flatten)]
    reading: ReadingArgs,
    #[arg(long, required = true, num_args = 1..)]
    input: Vec<PathBuf>,
}

#[derive(Args, Debug)]
struct ReplayArgs {
    #[arg(long)]
    journal: PathBuf,
    #[arg(long)]
    tree: PathBuf,
    /// import / serve が表示する読み条件の指紋。省略時は台帳内の唯一の条件
    #[arg(long)]
    fingerprint: Option<String>,
}

/// CLI の指定に従い木を処理する。
pub fn run(args: TreeArgs) -> Result<()> {
    match args.command {
        TreeCommand::Serve(args) => service::serve(args),
        TreeCommand::Import(args) => {
            let settings = Settings::new(&args.reading)?;
            let mut journal = Journal::open(&args.journal, &settings.fingerprint)?;
            journal.import(&args.input, &settings)?;
            println!("fingerprint={}", settings.fingerprint);
            Ok(())
        }
        TreeCommand::Replay(args) => {
            let definition = Definition::load(&args.tree)?;
            paths::reject_collisions(&args.journal, None, &[&definition])?;
            let output_lock = journal::LockedFile::open(&definition.out.join("tree.lock"))?;
            paths::reject_symlink(&args.journal)?;
            let journal_lock = journal::LockedFile::open(&args.journal)?;
            model::remove_candidates(&definition.out)?;
            let out = definition.out.clone();
            let result = (|| -> Result<bool> {
                let (journal, settings) =
                    Journal::select(journal_lock, args.fingerprint.as_deref())?;
                let mut tree = Tree::load(definition, &settings)?;
                loop {
                    let missing = tree.advance(&journal)?;
                    if tree.finished {
                        break;
                    }
                    if let Some((key, _)) = missing.first() {
                        anyhow::bail!("読みが足りない: {key} (不足 {} 局面)", missing.len());
                    }
                }
                tree.write_outputs()
            })();
            if result.is_err() {
                model::remove_candidates(&out)?;
                service::monitor(&out.join("STATUS"), "FAIL\n");
            }
            drop(output_lock);
            if !result? {
                anyhow::bail!("未展開手が上回る局面: 登録禁止。伸ばし直し回数を増やして再開する");
            }
            Ok(())
        }
    }
}
