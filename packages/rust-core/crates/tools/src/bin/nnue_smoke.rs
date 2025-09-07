use engine_core::evaluation::evaluate::Evaluator;
use engine_core::evaluation::nnue::NNUEEvaluatorWrapper;
use engine_core::shogi::Position;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: nnue_smoke <weights_path>");
        std::process::exit(2);
    }
    let path = &args[1];

    // Load wrapper (supports classic NNUE and SINGLE_CHANNEL)
    let eval = NNUEEvaluatorWrapper::new(path)?;

    // Evaluate startpos
    let pos = Position::startpos();
    let score = eval.evaluate(&pos);
    println!("ok: loaded {} -> startpos eval {}", path, score);

    Ok(())
}
