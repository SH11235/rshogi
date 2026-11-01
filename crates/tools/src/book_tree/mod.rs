//! 条件付き台帳を共有する、独立した定跡の木の生成。

mod add;
mod add_reads;
mod audit;
mod book_diff;
mod borrow;
mod check;
mod cycles;
mod export;
mod file_lock;
mod journal;
mod master_table;
mod model;
mod output;
mod paths;
mod queue;
mod refutation;
mod register;
mod rejoin;
mod reply_graph;
mod schema;
mod service;
#[cfg(test)]
mod simulation;
mod targets;
#[cfg(test)]
mod tests;
mod transfer;
mod walk;

pub use borrow::{BorrowArgs, run as run_borrow};

use anyhow::Result;
use clap::{Args, Subcommand};
use journal::Journal;
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
    /// キューの対象を共有エンジンで読む
    Serve(ServeArgs),
    /// キューに対象を足す (検査してから追記し、受領を出す)
    Enqueue(queue::EnqueueArgs),
    /// 受領の範囲の対象が処理され、終わるまで待つ
    Wait(queue::WaitArgs),
    /// 別の台帳の読みを、条件が同じと確かめてから取り込み先の条件で足す
    ImportJournal(transfer::ImportJournalArgs),
    /// 台帳だけから木を再生成する
    Replay(ReplayArgs),
    /// ready の木の手を定跡の元データの表に登録する
    Register(register::RegisterArgs),
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
    /// 対象の出力の根。`<out-root>/<id>/<own>/g<世代>/` に書く。state に固定される
    #[arg(long)]
    out_root: PathBuf,
    /// キューの行で opts.opp_n を省いたときの既定
    #[arg(long, default_value_t = 2)]
    opp_n: usize,
    /// キューの行で opts.stop を省いたときの既定
    #[arg(long, default_value_t = 300)]
    stop: i32,
    /// キューの行で opts.rounds を省いたときの既定
    #[arg(long, default_value_t = 3)]
    rounds: usize,
    /// キューの行で opts.draw を省いたときの既定
    #[arg(long, default_value_t = 0, allow_hyphen_values = true)]
    draw: i32,
    /// 読みごとに探索の途中の info 行を `<trace-dir>/<起動の id>.jsonl` に追記する。
    /// state に固定されず、起動ごとに付け外しできる
    #[arg(long)]
    trace_dir: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct ReplayArgs {
    #[arg(long)]
    journal: PathBuf,
    #[arg(long)]
    tree: PathBuf,
    /// serve が表示する読み条件の指紋。省略時は台帳内の唯一の条件
    #[arg(long)]
    fingerprint: Option<String>,
}

/// 定跡の元データの表を扱う入口。
#[derive(Args, Debug)]
pub struct MasterArgs {
    #[command(subcommand)]
    command: MasterCommand,
}

#[derive(Subcommand, Debug)]
enum MasterCommand {
    /// 元データへ行を追加・置換する
    Add(add::AddArgs),
    /// 読みのログから追加候補を作る
    AddReads(add_reads::AddReadsArgs),
    /// 元データから定跡・プリセット・注釈を書き出す
    Export(export::ExportArgs),
    /// 2 つの定跡の局面と手の差分を表示する
    Diff(book_diff::DiffArgs),
    /// 元データの書式・局面・合法手を全件検査する
    Validate(master_table::ValidateArgs),
    /// 書き出した定跡とプリセットの整合性・到達局面・循環への入口を検査する
    Check(check::CheckArgs),
    /// 読みの値を book_backprop で伝播し、main が伝播後の 1 位から負ける局面を点検する
    Audit(audit::AuditArgs),
    /// 相手の定跡に無い手で定跡に戻ると、戻った先の値が手前で期待した値より下がる所を一覧にする
    Rejoin(rejoin::RejoinArgs),
    /// 相手の合法手で定跡に戻る千日手の循環を、自分の手番の手を外して塞ぐ手の組を出す
    Cycles(cycles::CyclesArgs),
    /// 自分は main、相手は強い対局の頻度か大きい定跡の手で指すとして辿り、自分が読み始める局面と重みを出す
    Walk(walk::WalkArgs),
}

/// CLI の指定に従い元データを処理する。
pub fn run_master(args: MasterArgs) -> Result<()> {
    match args.command {
        MasterCommand::Add(args) => add::run(args),
        MasterCommand::AddReads(args) => add_reads::run(args),
        MasterCommand::Export(args) => export::run(args),
        MasterCommand::Diff(args) => book_diff::run(args),
        MasterCommand::Validate(args) => master_table::run_validate(args),
        MasterCommand::Check(args) => check::run(args),
        MasterCommand::Audit(args) => audit::run(args),
        MasterCommand::Rejoin(args) => rejoin::run(args),
        MasterCommand::Cycles(args) => cycles::run(args),
        MasterCommand::Walk(args) => walk::run(args),
    }
}

/// CLI の指定に従い木を処理する。
pub fn run(args: TreeArgs) -> Result<()> {
    match args.command {
        TreeCommand::Serve(args) => service::serve(args),
        TreeCommand::Enqueue(args) => queue::enqueue(args),
        TreeCommand::Wait(args) => queue::wait(args),
        TreeCommand::Register(args) => register::run(args),
        TreeCommand::ImportJournal(args) => transfer::run(args),
        TreeCommand::Replay(args) => {
            let definition = Definition::load(&args.tree)?;
            paths::reject_collisions(&args.journal, None, &[&definition])?;
            // tree.lock を先に取る。後から起動した serve はこの dir を用意する前に tree.lock を見る。
            let output_lock = journal::LockedFile::open(&definition.out.join("tree.lock"))?;
            let root_locks = paths::lock_serve_roots(&definition.out)?;
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
            drop(root_locks);
            drop(output_lock);
            if !result? {
                anyhow::bail!("未展開手が上回る局面: 登録禁止。伸ばし直し回数を増やして再開する");
            }
            Ok(())
        }
    }
}
