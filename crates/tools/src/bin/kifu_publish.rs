//! 棋譜を rshogi の棋譜サーバーへ公開する CLI。
//!
//! # 例
//! ```text
//! export RSHOGI_API_BASE=https://<host>/api/v1
//! export RSHOGI_ADMIN_TOKEN=...
//!
//! # csa_client の records から Floodgate 対局を取り込む（wdoor に無い局はアーカイブから）
//! kifu_publish wdoor --records-dir ~/floodgate/records --archive-dir ~/shogi-data/floodgate-archive
//!
//! # tournament / csa_client の JSONL を CSA に変換して送る（--dry-run で CSA の確認だけ）
//! kifu_publish jsonl --input runs/sprt/pair-0-1__base-vs-test.jsonl --event SPRT-2026-10 --game-id 12
//!
//! # CSA ファイルをそのまま送る
//! kifu_publish csa --input wcsc36-final.csa --event WCSC36
//! ```

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use tools::common::io::open_reader;
use tools::kifu_publish::{
    ADMIN_TOKEN_ENV, API_BASE_ENV, ApiClient, Publisher, Tally, collect_wdoor_ids, convert_jsonl,
    read_csa_text,
};

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Publish game records to the rshogi kifu server"
)]
struct Cli {
    /// API のベース URL（`/api/v1` まで含む）。省略時は環境変数 `RSHOGI_API_BASE`。
    /// `https://` のみ（`http://` は localhost / 127.0.0.1 / [::1] に限る）。
    #[arg(long, global = true)]
    api_base: Option<String>,

    /// リクエストの間隔（ミリ秒）。
    #[arg(long, global = true, default_value_t = 1000)]
    interval_ms: u64,

    /// サーバーへ送らず、送る内容（wdoor は対局 ID、jsonl / csa は CSA）だけを標準出力に出す。
    /// 件数などの付随情報は標準エラーに出す。
    #[arg(long, global = true)]
    dry_run: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// csa_client の records にある Floodgate 対局を、対局 ID でサーバーに取り込ませる。
    Wdoor {
        /// csa_client の棋譜ディレクトリ。直下の `*.csa` の `$EVENT:` から対局 ID を集める。
        #[arg(long)]
        records_dir: PathBuf,

        /// wdoor 版 CSA のアーカイブ。wdoor に棋譜が無い局を `YYYY/MM/DD/<id>.csa` か
        /// 直下の `<id>.csa` から探して本文アップロードする。
        #[arg(long)]
        archive_dir: Option<PathBuf>,
    },
    /// tournament / csa_client の JSONL を wdoor 形式の CSA に変換してアップロードする。
    Jsonl {
        /// 入力 JSONL（`.gz` 可）。
        #[arg(long)]
        input: PathBuf,

        /// 一覧に出す大会名など。CSA の `$EVENT:` にも書く。
        #[arg(long)]
        event: String,

        /// この `game_id` の局だけを送る。省略時は終局した全局。
        #[arg(long)]
        game_id: Option<u32>,

        /// `--dry-run` のとき、標準出力の代わりにここへ `<入力名>-<game_id>.csa` を書く。
        /// 複数局を変換するときは必須（標準出力には 1 局分の CSA しか出さない）。
        #[arg(long, requires = "dry_run")]
        out_dir: Option<PathBuf>,
    },
    /// CSA ファイルをそのままアップロードする。
    Csa {
        /// 入力 CSA（UTF-8）。
        #[arg(long)]
        input: PathBuf,

        /// 一覧に出す大会名など。
        #[arg(long)]
        event: String,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let dry_run = cli.dry_run;
    match run(cli) {
        Ok(tally) => {
            let summary = format!(
                "created={} skipped={} failed={}",
                tally.created, tally.skipped, tally.failed
            );
            // `--dry-run` の標準出力は対局 ID / CSA だけにする（パイプやリダイレクトで
            // そのまま使えるように）。
            if dry_run {
                eprintln!("{summary}");
            } else {
                println!("{summary}");
            }
            if tally.failed == 0 {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<Tally> {
    let mut sink = if cli.dry_run {
        Sink::DryRun(Tally::default())
    } else {
        Sink::Send(new_publisher(cli.api_base.as_deref(), cli.interval_ms)?)
    };
    match &cli.command {
        Command::Wdoor {
            records_dir,
            archive_dir,
        } => {
            let ids = collect_wdoor_ids(records_dir)?;
            eprintln!(
                "{} 局の wdoor 対局 ID を {} から集めました",
                ids.len(),
                records_dir.display()
            );
            for id in &ids {
                match &mut sink {
                    Sink::DryRun(_) => println!("{id}"),
                    Sink::Send(publisher) => publisher.publish_wdoor(id, archive_dir.as_deref()),
                }
            }
        }
        Command::Jsonl {
            input,
            event,
            game_id,
            out_dir,
        } => {
            let reader =
                open_reader(input).with_context(|| format!("開けません: {}", input.display()))?;
            let stem = input.file_stem().map(|s| s.to_string_lossy().into_owned());
            let stem = stem.unwrap_or_else(|| "game".to_owned());
            let mut printed_to_stdout = false;
            let games = convert_jsonl(reader, event, *game_id, |id, csa| {
                let label = format!("{} game_id={id}", input.display());
                match (csa, &mut sink) {
                    (Err(err), sink) => sink.tally_mut().fail(&label, &err),
                    (Ok(csa), Sink::Send(publisher)) => publisher.publish_csa(&label, &csa, event),
                    (Ok(csa), Sink::DryRun(_)) => match out_dir {
                        Some(dir) => write_csa(&dir.join(format!("{stem}-{id}.csa")), &csa)?,
                        None => {
                            // 標準出力に複数局を続けて出すと 1 つの CSA として読めなくなる。
                            if printed_to_stdout {
                                bail!(
                                    "複数の対局があります。--game-id で 1 局に絞るか、--out-dir で\
                                     対局ごとのファイルに出力してください"
                                );
                            }
                            printed_to_stdout = true;
                            print!("{csa}");
                        }
                    },
                }
                Ok(())
            })?;
            if games == 0 {
                bail!("変換できる終局済みの対局がありません: {}", input.display());
            }
        }
        Command::Csa { input, event } => {
            let csa = read_csa_text(input)?;
            match &mut sink {
                Sink::DryRun(_) => print!("{csa}"),
                Sink::Send(publisher) => {
                    publisher.publish_csa(&input.display().to_string(), &csa, event);
                }
            }
        }
    }
    Ok(*sink.tally_mut())
}

/// 送信先。`--dry-run` では API クライアントを作らず、変換失敗の件数だけ数える。
enum Sink {
    DryRun(Tally),
    Send(Publisher),
}

impl Sink {
    fn tally_mut(&mut self) -> &mut Tally {
        match self {
            Sink::DryRun(tally) => tally,
            Sink::Send(publisher) => &mut publisher.tally,
        }
    }
}

fn new_publisher(api_base: Option<&str>, interval_ms: u64) -> Result<Publisher> {
    let api_base = match api_base {
        Some(base) => base.to_owned(),
        None => std::env::var(API_BASE_ENV)
            .with_context(|| format!("--api-base か環境変数 {API_BASE_ENV} を指定してください"))?,
    };
    // 管理トークンは環境変数からだけ受け取る（引数に書くとシェル履歴やプロセス一覧に残る）。
    let token = std::env::var(ADMIN_TOKEN_ENV)
        .with_context(|| format!("環境変数 {ADMIN_TOKEN_ENV} に管理トークンを設定してください"))?;
    let api = ApiClient::new(&api_base, token)?;
    Ok(Publisher::new(api, Duration::from_millis(interval_ms)))
}

fn write_csa(path: &Path, csa: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("ディレクトリを作れません: {}", dir.display()))?;
    }
    std::fs::write(path, csa).with_context(|| format!("書けません: {}", path.display()))?;
    eprintln!("wrote {}", path.display());
    Ok(())
}
