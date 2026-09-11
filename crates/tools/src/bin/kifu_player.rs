//! PSV / tournament JSONL / CSA 共通の棋譜プレイヤー TUI。
//!
//! 詳細は `crates/tools/docs/kifu_player.md` を参照。

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Result, bail};
use clap::{ArgGroup, Parser};
use rshogi_csa_client::jsonl::sanitize_for_filename;
use tools::replay::{CsaSource, GameSource, JsonlSource, PsvSource, tui};

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "PSV / tournament JSONL / CSA の対局を TUI で再生する"
)]
#[command(group(ArgGroup::new("source").required(true).args(["psv", "tournament_dir", "csa"])))]
struct Cli {
    /// PSV (PackedSfenValue) ファイルを開く。連続した自己対局ストリームを想定する
    /// （shuffle_psv/merge_psv 等でシャッフル済みのプールは対局境界検出が機能しない）。
    #[arg(long)]
    psv: Option<PathBuf>,

    /// tournament の out-dir を開く（pair-{i}-{j}.jsonl、旧 -vs- / _vs_ 形式に対応）。
    #[arg(long)]
    tournament_dir: Option<PathBuf>,

    /// CSA 棋譜（rshogi csa_client 出力形式）を開く。ディレクトリ（配下の `*.csa` を
    /// 横断）または単一 `.csa` ファイルを指定する。
    #[arg(long)]
    csa: Option<PathBuf>,

    /// 旧形式のrshogi-csa-serverが手後へ書いた`'*`評価コメントとして解釈する。
    #[arg(long, requires = "csa")]
    legacy_server_eval_comments: bool,

    /// SECS 秒ごとに入力を再スキャンし、新しい対局を一覧へ自動追加する（値省略時 5 秒）。
    /// csa_client の記録 dir を連続対局中に開いておく用途。--psv では使えない。
    #[arg(long, value_name = "SECS", num_args = 0..=1, default_missing_value = "5")]
    live: Option<u64>,

    /// レート表 TSV（`name<TAB>rate`、`floodgate_record --ratings-cache` の出力形式）。
    /// 対局一覧に R を併記し、`rate:>N` フィルタを有効にする（ネットワークは使わない）。
    #[arg(long)]
    ratings: Option<PathBuf>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    // 排他・必須は clap の ArgGroup が保証する（同時指定・未指定はパース時に弾かれる）。
    let source: Box<dyn GameSource> = if let Some(path) = cli.psv {
        Box::new(PsvSource::new(path))
    } else if let Some(dir) = cli.tournament_dir {
        Box::new(JsonlSource::new(dir))
    } else if let Some(path) = cli.csa {
        Box::new(
            CsaSource::new(path).with_legacy_server_eval_comments(cli.legacy_server_eval_comments),
        )
    } else {
        bail!("--psv / --tournament-dir / --csa のいずれか一つを指定してください");
    };
    let mut opts = tui::RunOptions::default();
    if let Some(secs) = cli.live {
        anyhow::ensure!(secs >= 1, "--live は 1 秒以上を指定してください");
        opts.live_interval = Some(Duration::from_secs(secs));
    }
    if let Some(path) = &cli.ratings {
        // TUI 内の突き合わせは正規化キーで行うため、読み込み時に変換して渡す。
        opts.ratings = tools::common::floodgate::read_ratings_tsv(path)?
            .into_iter()
            .map(|(name, rate)| (sanitize_for_filename(&name), rate))
            .collect();
    }
    tui::run(source, opts)
}
