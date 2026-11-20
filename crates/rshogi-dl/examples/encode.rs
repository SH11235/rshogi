use std::io::{self, BufRead, Write};

use anyhow::{Context, Result, bail};
use rshogi_core::movegen::{MoveList, generate_legal_all};
use rshogi_core::position::Position;
use rshogi_core::types::Move;
use rshogi_dl::{features::make_move_label, tokens::encode};

const BINARY_HEADER: &[u8; 8] = b"RSDLPV01";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Json,
    TrainingBinary,
}

fn main() -> Result<()> {
    let format = match std::env::args().skip(1).collect::<Vec<_>>().as_slice() {
        [] => Format::Json,
        [argument] if argument == "--training-binary" => Format::TrainingBinary,
        _ => bail!("usage: encode [--training-binary]; SFEN lines on stdin"),
    };
    let stdout = io::stdout();
    let mut output = io::BufWriter::new(stdout.lock());
    encode_positions(io::stdin().lock(), &mut output, format)?;
    output.flush()?;
    Ok(())
}

fn encode_positions(input: impl BufRead, mut output: impl Write, format: Format) -> Result<()> {
    if format == Format::TrainingBinary {
        output.write_all(BINARY_HEADER)?;
    }
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let (sfen, requested) = match format {
            Format::Json => (line.as_str(), None),
            Format::TrainingBinary => line
                .split_once('\t')
                .map_or((line.as_str(), None), |(sfen, moves)| (sfen, Some(moves))),
        };
        let mut position = Position::new();
        position.set_sfen(sfen)?;
        let tokens = encode(&position)?;
        let mut legal = MoveList::new();
        generate_legal_all(&position, &mut legal);
        let side = position.side_to_move();
        match format {
            Format::Json => {
                let labels: Vec<_> =
                    legal.iter().map(|m| (m.to_usi(), make_move_label(*m, side))).collect();
                serde_json::to_writer(
                    &mut output,
                    &serde_json::json!({
                        "sfen": position.to_sfen(), "tokens": tokens.as_slice(), "labels": labels,
                    }),
                )?;
                writeln!(output)?;
            }
            Format::TrainingBinary => {
                let labels = legal
                    .iter()
                    .map(|m| Ok((m.to_u16(), u16::try_from(make_move_label(*m, side))?)))
                    .collect::<Result<Vec<_>>>()?;
                let requested = matching_labels(requested, &labels)?;
                let legal_count = u16::try_from(labels.len())?;
                let requested_count = u16::try_from(requested.len())?;
                output.write_all(tokens.as_slice())?;
                output.write_all(&legal_count.to_le_bytes())?;
                output.write_all(&requested_count.to_le_bytes())?;
                for (_, label) in labels {
                    output.write_all(&label.to_le_bytes())?;
                }
                for label in requested {
                    output.write_all(&label.to_le_bytes())?;
                }
            }
        }
    }
    Ok(())
}

fn matching_labels(requested: Option<&str>, legal: &[(u16, u16)]) -> Result<Vec<u16>> {
    requested
        .into_iter()
        .flat_map(|moves| moves.split('\t'))
        .map(|input| {
            let code = Move::from_usi_strict(input).context("invalid USI move")?.to_u16();
            legal
                .iter()
                .find_map(|&(candidate, label)| (candidate == code).then_some(label))
                .with_context(|| format!("illegal move: {input}"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{BINARY_HEADER, Format, encode_positions};
    use rshogi_dl::tokens::TOKEN_COUNT;

    const START: &str = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";

    #[test]
    fn binary_records_match_json_labels_and_request_order() {
        let mut json = Vec::new();
        encode_positions(START.as_bytes(), &mut json, Format::Json).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&json).unwrap();
        let tokens = json["tokens"].as_array().unwrap();
        let labels = json["labels"].as_array().unwrap();
        let mut binary = Vec::new();
        let input = format!("{START}\t2g2f\t7g7f\t2g2f\n{START}\n");
        encode_positions(input.as_bytes(), &mut binary, Format::TrainingBinary).unwrap();
        let (header, mut records) = binary.split_at(BINARY_HEADER.len());
        assert_eq!(header, BINARY_HEADER);
        for requests in [vec!["2g2f", "7g7f", "2g2f"], vec![]] {
            let (row_tokens, remaining) = records.split_at(TOKEN_COUNT);
            assert!(
                row_tokens
                    .iter()
                    .zip(tokens)
                    .all(|(got, want)| Some(u64::from(*got)) == want.as_u64())
            );
            let mut shorts =
                remaining.chunks_exact(2).map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]));
            assert_eq!(usize::from(shorts.next().unwrap()), labels.len());
            assert_eq!(usize::from(shorts.next().unwrap()), requests.len());
            for label in labels {
                assert_eq!(Some(u64::from(shorts.next().unwrap())), label[1].as_u64());
            }
            for requested in &requests {
                let label =
                    labels.iter().find(|entry| entry[0].as_str() == Some(requested)).unwrap();
                assert_eq!(Some(u64::from(shorts.next().unwrap())), label[1].as_u64());
            }
            records = &remaining[2 * (2 + labels.len() + requests.len())..];
        }
        assert!(records.is_empty());
    }

    #[test]
    fn binary_rejects_invalid_or_illegal_requests_before_writing_record() {
        for requested in ["7g7f?", "7g7e", ""] {
            let mut binary = Vec::new();
            let input = format!("{START}\t{requested}\n");
            assert!(
                encode_positions(input.as_bytes(), &mut binary, Format::TrainingBinary).is_err()
            );
            assert_eq!(binary.as_slice(), BINARY_HEADER);
        }
    }
}
