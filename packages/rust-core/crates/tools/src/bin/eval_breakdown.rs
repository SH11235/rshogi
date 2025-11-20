use anyhow::Result;
use clap::Parser;
use engine_core::evaluation::evaluate::Evaluator;
use engine_core::evaluation::evaluate::{
    evaluate_material_only_debug, evaluate_material_terms_debug, MaterialEvalTerms,
    MaterialEvaluator,
};
use engine_core::evaluation::yo_material::evaluate_yo_material_lv3;
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

        let material_only = evaluate_material_only_debug(&pos);
        let yo_lv3 = evaluate_yo_material_lv3(&pos);
        let terms: MaterialEvalTerms = evaluate_material_terms_debug(&pos);
        let full = evaluator.evaluate(&pos);

        println!("SFEN[{}]: {}", idx, sfen_core);
        println!(
            "  material_only_cp={}  yo_lv3_cp={}  full_cp={}  sum_terms_cp={}",
            material_only, yo_lv3, full, terms.total_cp
        );
        println!(
            "  king_safety_cp={}  king_position_cp={}  piece_safety_cp={}  king_attacker_safety_cp={}",
            terms.king_safety_cp,
            terms.king_position_cp,
            terms.piece_safety_cp,
            terms.king_attacker_safety_cp
        );
        println!();
    }

    Ok(())
}
