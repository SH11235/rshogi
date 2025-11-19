use anyhow::Result;
use clap::Parser;
use engine_core::evaluation::evaluate::Evaluator;
use engine_core::evaluation::evaluate::{evaluate_material_only_debug, MaterialEvaluator};
use engine_core::Position;

/// 単一局面の評価分解（material-only / full）を出力する簡易ツール。
#[derive(Parser, Debug)]
#[command(
    name = "eval_breakdown",
    about = "Print material-only and full evaluation for given SFEN(s)",
    disable_help_subcommand = true
)]
struct Args {
    /// SFEN 文字列。先頭に `sfen` が付いていてもよい。
    #[arg(long = "sfen")]
    sfens: Vec<String>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    if args.sfens.is_empty() {
        eprintln!("no --sfen arguments provided");
        std::process::exit(1);
    }

    let evaluator = MaterialEvaluator;

    for (idx, raw) in args.sfens.iter().enumerate() {
        let sfen = raw.trim();
        let sfen_core = sfen
            .strip_prefix("sfen ")
            .unwrap_or_else(|| sfen.strip_prefix("position sfen ").unwrap_or(sfen));

        let pos =
            Position::from_sfen(sfen_core).map_err(|e| anyhow::anyhow!("invalid SFEN: {e}"))?;

        let material = evaluate_material_only_debug(&pos);
        let full = evaluator.evaluate(&pos);
        println!(
            "SFEN[{}]: {}\n  material_only_cp={}  full_cp={}\n",
            idx, sfen_core, material, full
        );
    }

    Ok(())
}
