use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use rshogi_core::nnue::net_bin_layout::permute_ft_columns;
use tools::output_path::ensure_safe_output_path;

#[derive(Parser)]
#[command(about = "LayerStacks NNUE の FT と L1 の列を並べ替える")]
struct Cli {
    /// 入力 NNUE ファイル
    #[arg(long)]
    nnue: PathBuf,
    /// 新しい index から元の index への写像を格納した JSON 配列
    #[arg(long)]
    permutation: PathBuf,
    /// 変換後の新規 NNUE ファイル
    #[arg(long)]
    output: PathBuf,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    ensure_safe_output_path(&cli.output, &cli.nnue)?;
    ensure_safe_output_path(&cli.output, &cli.permutation)?;
    let permutation: Vec<usize> = serde_json::from_reader(
        File::open(&cli.permutation).context("failed to open permutation")?,
    )
    .context("invalid permutation JSON")?;
    let mut input = File::open(&cli.nnue).context("failed to open NNUE")?;
    let parent = cli
        .output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut staged =
        tempfile::NamedTempFile::new_in(parent).context("failed to create staged output")?;
    {
        let mut writer = BufWriter::new(staged.as_file_mut());
        permute_ft_columns(&mut input, &mut writer, &permutation)
            .context("failed to permute NNUE")?;
        writer.flush().context("failed to flush NNUE")?;
    }
    staged
        .persist_noclobber(&cli.output)
        .context("failed to create output without overwriting")?;
    Ok(())
}
