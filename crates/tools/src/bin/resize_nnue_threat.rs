use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use rshogi_core::nnue::net_bin_layout::resize_threat;
use tools::output_path::ensure_safe_output_path;

#[derive(Parser)]
#[command(about = "LayerStacks NNUE の Threat 幅を縮小する")]
struct Cli {
    /// 入力 Threat NNUE
    #[arg(long)]
    nnue: PathBuf,
    /// 出力 Threat 幅 (128の倍数、入力幅以下)
    #[arg(long, default_value_t = 512)]
    width: usize,
    /// Threat を -4..=4 の乱数に置き換える seed (省略時は既存の列を保持)
    #[arg(long)]
    seed: Option<u64>,
    /// 新規出力ファイル
    #[arg(long)]
    output: PathBuf,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    ensure_safe_output_path(&cli.output, &cli.nnue)?;
    let mut input = BufReader::new(File::open(&cli.nnue).context("open input NNUE")?);
    let parent = cli
        .output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut staged = tempfile::NamedTempFile::new_in(parent).context("create staged NNUE")?;
    {
        let mut output = BufWriter::new(staged.as_file_mut());
        resize_threat(&mut input, &mut output, cli.width, cli.seed).context("resize Threat")?;
        output.flush()?;
    }
    staged
        .persist_noclobber(&cli.output)
        .context("publish NNUE without overwriting")?;
    Ok(())
}
