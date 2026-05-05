//! Selfplay/tournament の JSONL ログから KIF 棋譜を生成する CLI。
//!
//! `tournament` の出力など、`meta` / `move` / `result` 行を含む JSONL に対応。
//! `gensfen` の出力は move 行を持たないため対象外。
//!
//! # 例
//! ```text
//! # 全対局を per-game KIF に展開
//! jsonl_to_kif --input runs/tournament/x/games.jsonl --output kif_out/
//!
//! # 特定の対局だけ
//! jsonl_to_kif --input games.jsonl --output kif_out/ --game-id 42
//!
//! # 先頭 10 局
//! jsonl_to_kif --input games.jsonl --output kif_out/ --limit 10
//!
//! # 11〜20 局目（id ベースではなく順序ベース）
//! jsonl_to_kif --input games.jsonl --output kif_out/ --skip 10 --limit 10
//! ```

use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::Parser;
use tools::kif::{GameFilter, convert_jsonl_to_kif};

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Convert selfplay/tournament JSONL game logs into KIF files"
)]
struct Cli {
    /// 入力 JSONL ファイル
    #[arg(long)]
    input: PathBuf,

    /// 出力先。ディレクトリなら `<dir>/g<id:03>.kif`、
    /// ファイルパスなら単一対局はそのパス、複数は `<stem>_g<id:03>.<ext>` に展開。
    #[arg(long)]
    output: PathBuf,

    /// 抽出対象の game_id（カンマ区切りで複数指定可: `--game-id 1,5,10`）
    #[arg(long, value_delimiter = ',')]
    game_id: Vec<u32>,

    /// フィルタ適用後、先頭 N 局をスキップ
    #[arg(long, default_value_t = 0)]
    skip: usize,

    /// フィルタ適用後、最大 N 局のみ出力
    #[arg(long)]
    limit: Option<usize>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    // --game-id は対象 id を確定させる用途、--skip/--limit は順序ベースの絞り込み。
    // 両者を併用すると意味が混乱するため明示的に拒否する。
    if !cli.game_id.is_empty() && (cli.skip > 0 || cli.limit.is_some()) {
        bail!("--game-id cannot be combined with --skip / --limit. Use one or the other.");
    }
    let filter = GameFilter {
        game_ids: cli.game_id,
        skip: cli.skip,
        limit: cli.limit,
    };
    let written = convert_jsonl_to_kif(&cli.input, &cli.output, &filter)?;
    if written.len() == 1 {
        println!("kif written to {}", written[0].display());
    } else {
        println!("kif written ({} games):", written.len());
        for p in written {
            println!("  {}", p.display());
        }
    }
    Ok(())
}
