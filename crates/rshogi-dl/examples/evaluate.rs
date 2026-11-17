mod support;

use std::io::{self, BufRead, Write};

use anyhow::{Result, ensure};
use rshogi_core::movegen::{MoveList, generate_legal_all};
use rshogi_core::position::Position;
use rshogi_dl::Evaluator;
use rshogi_dl::features::{
    FEATURES1_SIZE, FEATURES2_SIZE, MAX_MOVE_LABEL_NUM, make_input_features, make_move_label,
};
use rshogi_dl::onnx::{OnnxConfig, OnnxEvaluator};

fn main() {
    support::finish(run_command());
}

fn run_command() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    ensure!(args.len() == 3, "usage: evaluate MODEL DEVICE; SFEN lines on stdin");
    let mut evaluator = OnnxEvaluator::new(&OnnxConfig {
        model_path: args[1].clone().into(),
        gpu_id: args[2].parse()?,
        use_tensorrt: false,
        tensorrt_cache: None,
        batch_size: 1,
    })?;
    let mut logits = vec![0.0; MAX_MOVE_LABEL_NUM];
    let mut first = vec![0.0; FEATURES1_SIZE];
    let mut second = vec![0.0; FEATURES2_SIZE];
    let stdout = io::stdout();
    let mut output = io::BufWriter::new(stdout.lock());
    for line in io::stdin().lock().lines() {
        let sfen = line?;
        if sfen.trim().is_empty() {
            continue;
        }
        let mut pos = Position::new();
        pos.set_sfen(&sfen)?;
        first.fill(0.0);
        second.fill(0.0);
        make_input_features(&pos, &mut first, &mut second);
        let value = Evaluator::evaluate(&mut evaluator, &pos, &mut logits)?;
        let mut legal = MoveList::new();
        generate_legal_all(&pos, &mut legal);
        let labels: Vec<_> = legal
            .iter()
            .map(|m| (m.to_usi(), make_move_label(*m, pos.side_to_move())))
            .collect();
        serde_json::to_writer(
            &mut output,
            &serde_json::json!({
                "sfen": sfen, "features1": first, "features2": second,
                "labels": labels, "policy": logits, "value": value,
            }),
        )?;
        writeln!(output)?;
    }
    output.flush()?;
    Ok(())
}
