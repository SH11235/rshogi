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
//! # CSA ファイル（ディレクトリなら配下の *.csa 全部）を、サーバーが読める形に整えて送る
//! kifu_publish csa --input wcsc35_kifu/ --event WCSC35
//!
//! # 電竜戦の KIF を CSA に変換して送る
//! kifu_publish kif --input kifu_dr6production/ --event 電竜戦6
//!
//! # イベントの正式名称・公式リンクを登録する
//! kifu_publish event --event WCSC35 --title 第35回世界コンピュータ将棋選手権 --official-url https://example.org/wcsc35/
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail, ensure};
use clap::{Parser, Subcommand};
use tools::common::io::open_reader;
use tools::kifu_publish::cp932::KifuEncoding;
use tools::kifu_publish::csa_normalize::{
    RoundStartTimes, StartTimeFallback, StartTimeSource, is_csa_datetime, normalize_csa,
};
use tools::kifu_publish::kif_convert::kif_to_csa;
use tools::kifu_publish::{
    ADMIN_TOKEN_ENV, API_BASE_ENV, ApiClient, EventMeta, Publisher, Rejection, Tally,
    collect_kifu_files, collect_wdoor_ids, convert_jsonl, read_kifu_text,
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

    /// サーバーへ送らず、送る内容（wdoor は対局 ID、jsonl / csa / kif は CSA、event は
    /// JSON）だけを標準出力に出す。件数などの付随情報は標準エラーに出す。
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
    /// CSA ファイルを、サーバーが読める形に正規化してアップロードする。
    Csa {
        /// 入力 CSA、またはディレクトリ（配下の `*.csa` をファイル名順に全部送る）。
        /// UTF-8 でなければ Shift_JIS として読む。
        #[arg(long)]
        input: PathBuf,

        /// 一覧に出す大会名など。
        #[arg(long)]
        event: String,

        /// `$START_TIME` が無い棋譜に入れる仮の開始時刻（`YYYY/MM/DD HH:MM:SS`、JST）。
        /// 同じディレクトリ・同じ回戦の他の棋譜に開始時刻があれば、そちらを優先する。
        /// 省略時、開始時刻が無い棋譜は送らない。
        #[arg(long)]
        fallback_start_time: Option<String>,

        /// `--dry-run` のとき、正規化した CSA をここへ入力と同じ相対パスで書く。
        #[arg(long, requires = "dry_run")]
        out_dir: Option<PathBuf>,
    },
    /// 電竜戦の KIF ファイルを CSA に変換してアップロードする。
    Kif {
        /// 入力 KIF、またはディレクトリ（配下の `*.kif` をファイル名順に全部送る）。
        /// UTF-8 でなければ Shift_JIS として読む。
        #[arg(long)]
        input: PathBuf,

        /// 一覧に出す大会名など。
        #[arg(long)]
        event: String,

        /// `--dry-run` のとき、変換した CSA をここへ入力と同じ相対パス（拡張子 `.csa`）で書く。
        #[arg(long, requires = "dry_run")]
        out_dir: Option<PathBuf>,
    },
    /// イベント（大会）の正式名称・主催・公式リンクなどを登録・更新する。
    Event {
        /// 対象のイベント。棋譜を送るときの `--event` と同じ文字列。
        #[arg(long)]
        event: String,

        /// 正式名称。
        #[arg(long)]
        title: String,

        /// 主催。
        #[arg(long)]
        organizer: Option<String>,

        /// 大会の公式ページ（`https://` か `http://`）。
        #[arg(long)]
        official_url: Option<String>,

        /// 棋譜の配布元ページ（`https://` か `http://`）。
        #[arg(long)]
        kifu_source_url: Option<String>,

        /// 利用条件の要約（1 行）。
        #[arg(long)]
        terms_note: Option<String>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let dry_run = cli.dry_run;
    match run(cli) {
        Ok(None) => ExitCode::SUCCESS,
        Ok(Some(tally)) => {
            let summary = format!(
                "created={} skipped={} excluded={} failed={}",
                tally.created, tally.skipped, tally.excluded, tally.failed
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

/// 棋譜を送るサブコマンドは送信結果の件数を返す。`event` は件数が無いので `None`。
fn run(cli: Cli) -> Result<Option<Tally>> {
    if let Command::Event {
        event,
        title,
        organizer,
        official_url,
        kifu_source_url,
        terms_note,
    } = &cli.command
    {
        let meta = EventMeta {
            title: title.clone(),
            organizer: organizer.clone(),
            official_url: official_url.clone(),
            kifu_source_url: kifu_source_url.clone(),
            terms_note: terms_note.clone(),
        };
        if cli.dry_run {
            eprintln!("PUT admin/events/{event}");
            println!("{}", serde_json::to_string_pretty(&meta)?);
        } else {
            new_api(cli.api_base.as_deref())?.put_event_meta(event, &meta)?;
            println!("updated {event}");
        }
        return Ok(None);
    }
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
        Command::Csa {
            input,
            event,
            fallback_start_time,
            out_dir,
        } => {
            if let Some(value) = fallback_start_time {
                ensure!(
                    is_csa_datetime(value),
                    "--fallback-start-time は YYYY/MM/DD HH:MM:SS の形で指定してください: {value}"
                );
            }
            let files = collect_kifu_files(input, "csa")?;
            // 開始時刻の無い棋譜のために、回戦ごとの最も早い開始時刻を先に集める。
            // 読めないファイルはここでは無視する（本処理で失敗として数える）。
            let mut round_starts = RoundStartTimes::default();
            for path in &files {
                if let Ok((text, _)) = read_kifu_text(path) {
                    round_starts.observe(path, &text);
                }
            }
            let mut filled: Vec<(String, String, StartTimeSource)> = Vec::new();
            let batch = Batch {
                input,
                event,
                out_dir: out_dir.as_deref(),
                out_extension: None,
            };
            batch.publish(&mut sink, &files, |path, text| {
                let start = StartTimeFallback {
                    same_round: round_starts.earliest_for(path),
                    given: fallback_start_time.as_deref(),
                };
                let normalized = normalize_csa(text, &start)?;
                if let Some((value, source)) = normalized.filled_start_time {
                    let name = path.file_name().unwrap_or(path.as_os_str());
                    filled.push((name.to_string_lossy().into_owned(), value, source));
                }
                Ok(normalized.csa)
            })?;
            // 補完した開始時刻は本文（= 対局 ID）に入る。同じ回戦から取った値は一緒に
            // 処理したファイルの集合で変わるので、どの棋譜に何を入れたかを残す。
            for (name, value, source) in &filled {
                let how = match source {
                    StartTimeSource::SameRound => "同じ回戦の最も早い開始時刻",
                    StartTimeSource::Given => "--fallback-start-time",
                };
                eprintln!("start-time {name}: {value} ({how})");
            }
            if !filled.is_empty() {
                eprintln!("filled_start_time={}", filled.len());
            }
        }
        Command::Kif {
            input,
            event,
            out_dir,
        } => {
            let files = collect_kifu_files(input, "kif")?;
            let batch = Batch {
                input,
                event,
                out_dir: out_dir.as_deref(),
                out_extension: Some("csa"),
            };
            batch.publish(&mut sink, &files, |_path, text| kif_to_csa(text))?;
        }
        Command::Event { .. } => bail!("event は棋譜を送るサブコマンドではありません"),
    }
    Ok(Some(*sink.tally_mut()))
}

/// `csa` / `kif` の 1 回の実行。ファイルごとに変換して送り、1 件の失敗では止めない。
struct Batch<'a> {
    input: &'a Path,
    event: &'a str,
    out_dir: Option<&'a Path>,
    /// `--out-dir` に書くときに付け替える拡張子。`None` なら入力のまま。
    out_extension: Option<&'a str>,
}

impl Batch<'_> {
    fn publish(
        &self,
        sink: &mut Sink,
        files: &[PathBuf],
        mut convert: impl FnMut(&Path, &str) -> Result<String, Rejection>,
    ) -> Result<()> {
        ensure!(!files.is_empty(), "対象の棋譜ファイルがありません: {}", self.input.display());
        let single = !self.input.is_dir();
        // 送らなかった理由ごとの件数（対象外 / 変換失敗）。理由は固定の数種類。
        let mut excluded: BTreeMap<&'static str, u32> = BTreeMap::new();
        let mut failed: BTreeMap<&'static str, u32> = BTreeMap::new();
        let mut decoded: BTreeMap<KifuEncoding, u32> = BTreeMap::new();
        let mut converted = 0_u32;
        for path in files {
            // 表示と `--out-dir` には、入力ディレクトリからの相対パスを使う。
            let relative = match path.strip_prefix(self.input) {
                Ok(relative) if !single => relative,
                _ => Path::new(path.file_name().unwrap_or(path.as_os_str())),
            };
            let label = relative.display().to_string();
            let converted_csa = match read_kifu_text(path) {
                Ok((text, encoding)) => {
                    *decoded.entry(encoding).or_default() += 1;
                    convert(path, &text)
                }
                Err(err) => {
                    *failed.entry("読めない").or_default() += 1;
                    sink.tally_mut().fail(&label, &err);
                    continue;
                }
            };
            let csa = match converted_csa {
                Ok(csa) => csa,
                Err(Rejection::Excluded(reason)) => {
                    *excluded.entry(reason).or_default() += 1;
                    sink.tally_mut().exclude(&label, reason);
                    continue;
                }
                Err(Rejection::Failed { reason, detail }) => {
                    *failed.entry(reason).or_default() += 1;
                    sink.tally_mut().fail(&label, &anyhow!("{reason}: {detail}"));
                    continue;
                }
            };
            converted += 1;
            match (&mut *sink, self.out_dir) {
                (Sink::Send(publisher), _) => publisher.publish_csa(&label, &csa, self.event),
                (Sink::DryRun(_), Some(dir)) => {
                    let mut out = dir.join(relative);
                    if let Some(extension) = self.out_extension {
                        out.set_extension(extension);
                    }
                    write_csa(&out, &csa)?;
                }
                // 標準出力に複数局を続けて出すと 1 つの CSA として読めなくなるので、
                // ディレクトリ入力では変換できたことだけを記録する。
                (Sink::DryRun(_), None) if single => print!("{csa}"),
                (Sink::DryRun(_), None) => eprintln!("ok      {label}"),
            }
        }
        for (encoding, count) in &decoded {
            eprintln!("decoded[{}]={count}", encoding.label());
        }
        eprintln!("converted={converted}");
        for (reason, count) in &excluded {
            eprintln!("excluded[{reason}]={count}");
        }
        for (reason, count) in &failed {
            eprintln!("failed[{reason}]={count}");
        }
        Ok(())
    }
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
    Ok(Publisher::new(new_api(api_base)?, Duration::from_millis(interval_ms)))
}

fn new_api(api_base: Option<&str>) -> Result<ApiClient> {
    let api_base = match api_base {
        Some(base) => base.to_owned(),
        None => std::env::var(API_BASE_ENV)
            .with_context(|| format!("--api-base か環境変数 {API_BASE_ENV} を指定してください"))?,
    };
    // 管理トークンは環境変数からだけ受け取る（引数に書くとシェル履歴やプロセス一覧に残る）。
    let token = std::env::var(ADMIN_TOKEN_ENV)
        .with_context(|| format!("環境変数 {ADMIN_TOKEN_ENV} に管理トークンを設定してください"))?;
    ApiClient::new(&api_base, token)
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
