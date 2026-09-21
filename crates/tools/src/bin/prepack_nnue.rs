//! LayerStacks の重みを事前展開する。
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};

#[derive(Clone, Copy, ValueEnum)]
enum FcLayout {
    RowMajor,
    Native,
}

#[derive(Parser)]
#[command(about = "LayerStacks NNUE を共有読み込み用コンテナへ変換")]
struct Args {
    /// 入力 LayerStacks .bin
    #[arg(long)]
    input: PathBuf,
    /// 出力ファイル（既存ファイルは上書きしない）
    #[arg(long)]
    output: PathBuf,
    /// universal は row-major、同じISAの固定editionは native
    #[arg(long, value_enum, default_value = "row-major")]
    fc_layout: FcLayout,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let layout = match args.fc_layout {
        FcLayout::RowMajor => rshogi_core::nnue::prepacked::FcLayout::RowMajor,
        FcLayout::Native => rshogi_core::nnue::prepacked::FcLayout::Native,
    };
    rshogi_core::nnue::prepacked::pack_layer_stacks_with_layout(&args.input, &args.output, layout)
        .context("NNUE の事前展開に失敗しました")?;
    println!("{}", args.output.display());
    Ok(())
}
