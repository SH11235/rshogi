use std::io::{self, BufRead, Write};

use anyhow::{Result, ensure};
use rshogi_core::movegen::{MoveList, generate_legal_all};
use rshogi_core::position::Position;
use rshogi_dl::{features::make_move_label, tokens::encode};

fn main() -> Result<()> {
    ensure!(std::env::args().len() == 1, "usage: encode; SFEN lines on stdin");
    let stdout = io::stdout();
    let mut output = io::BufWriter::new(stdout.lock());
    for line in io::stdin().lock().lines() {
        let sfen = line?;
        if sfen.trim().is_empty() {
            continue;
        }
        let mut position = Position::new();
        position.set_sfen(&sfen)?;
        let tokens = encode(&position)?;
        let mut legal = MoveList::new();
        generate_legal_all(&position, &mut legal);
        let labels: Vec<_> = legal
            .iter()
            .map(|m| (m.to_usi(), make_move_label(*m, position.side_to_move())))
            .collect();
        serde_json::to_writer(
            &mut output,
            &serde_json::json!({
                "sfen": position.to_sfen(), "tokens": tokens.as_slice(), "labels": labels,
            }),
        )?;
        writeln!(output)?;
    }
    output.flush()?;
    Ok(())
}
