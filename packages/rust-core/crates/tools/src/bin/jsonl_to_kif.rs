use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use tools::kif_export::convert_jsonl_to_kif;

#[derive(Parser, Debug)]
#[command(author, version, about = "Convert selfplay JSONL logs to KIF format")]
struct Cli {
    /// Input JSONL log file (from selfplay_basic)
    input: PathBuf,
    /// Output KIF file (defaults to same dir / base name with .kif)
    #[arg(long)]
    output: Option<PathBuf>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let output_path = cli.output.clone().unwrap_or_else(|| default_output_path(&cli.input));
    let written = convert_jsonl_to_kif(&cli.input, &output_path)?;
    if written.len() == 1 {
        println!("kif written to {}", written[0].display());
    } else {
        println!("kif written to:");
        for path in &written {
            println!("  {}", path.display());
        }
    }
    Ok(())
}

fn default_output_path(input: &std::path::Path) -> PathBuf {
    let parent = input.parent().unwrap_or_else(|| std::path::Path::new("."));
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
    parent.join(format!("{stem}.kif"))
}
