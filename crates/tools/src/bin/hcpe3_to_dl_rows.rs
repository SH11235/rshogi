use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use clap::Parser;
use rshogi_core::movegen::{MoveList, generate_legal_all};
use rshogi_dl::{features::make_move_label, tokens::encode};
use sha2::{Digest, Sha256};
use tools::hcpe3::{
    GameHeader, read_candidate, read_game_header, read_move_header, start_position,
};
use tools::packed_sfen::{hcpe_result_to_stm, move_to_hcpe_move16};

const HEADER: &[u8; 8] = b"RSDLHC01";
const MAX_LEGAL_MOVES: usize = 593;

#[derive(Parser)]
#[command(about = "HCPE3 を再生し、訪問数付きの学習行を標準出力へ書く")]
struct Args {
    /// HCPE3 ファイル。入力順を維持し、- は標準入力を表す。
    #[arg(required = true, value_name = "HCPE3")]
    inputs: Vec<PathBuf>,
}

struct Ply {
    selected: u16,
    visits: Vec<(u16, u16)>,
}

struct Game {
    header: GameHeader,
    key: [u8; 32],
    plies: Vec<Ply>,
}

fn read_game(reader: &mut impl Read) -> Result<Option<Game>> {
    let Some(header) = read_game_header(reader)? else {
        return Ok(None);
    };
    let mut key = Sha256::new();
    key.update(header.hcp);
    let mut plies = Vec::with_capacity(usize::from(header.move_count));
    for _ in 0..header.move_count {
        let record = read_move_header(reader)?;
        ensure!(
            usize::from(record.candidate_count) <= MAX_LEGAL_MOVES,
            "候補手数が合法手数の上限を超えています"
        );
        key.update(record.move16.to_le_bytes());
        let mut visits = Vec::with_capacity(usize::from(record.candidate_count));
        for _ in 0..record.candidate_count {
            let (candidate, count) = read_candidate(reader)?;
            if count != 0 {
                visits.push((candidate, count));
            }
        }
        plies.push(Ply {
            selected: record.move16,
            visits,
        });
    }
    Ok(Some(Game {
        header,
        key: key.finalize().into(),
        plies,
    }))
}

fn write_game(output: &mut impl Write, game: &Game) -> Result<u64> {
    let mut position = start_position(&game.header.hcp)?;
    position.try_reserve_move_history(game.plies.len())?;
    let rows = u16::try_from(game.plies.iter().filter(|ply| !ply.visits.is_empty()).count())?;
    output.write_all(&game.key)?;
    output.write_all(&game.header.move_count.to_le_bytes())?;
    output.write_all(&rows.to_le_bytes())?;

    let mut legal_moves = MoveList::new();
    let mut labels = Vec::with_capacity(MAX_LEGAL_MOVES);
    let mut candidates = Vec::with_capacity(MAX_LEGAL_MOVES);
    for (number, ply) in game.plies.iter().enumerate() {
        legal_moves.set_len(0);
        generate_legal_all(&position, &mut legal_moves);
        let selected = legal_moves
            .iter()
            .copied()
            .find(|&candidate| move_to_hcpe_move16(candidate) == ply.selected)
            .with_context(|| format!("{} 手目の実着手が非合法です", number + 1))?;
        if !ply.visits.is_empty() {
            let side = position.side_to_move();
            labels.clear();
            for &legal in &legal_moves {
                labels.push((
                    move_to_hcpe_move16(legal),
                    u16::try_from(make_move_label(legal, side))?,
                ));
            }
            candidates.clear();
            for &(requested, visits) in &ply.visits {
                let label = labels
                    .iter()
                    .find_map(|&(code, label)| (code == requested).then_some(label))
                    .with_context(|| format!("{} 手目の候補手が非合法です", number + 1))?;
                ensure!(
                    !candidates.iter().any(|&(previous, _)| previous == label),
                    "{} 手目の候補手が重複しています",
                    number + 1
                );
                candidates.push((label, visits));
            }
            let score =
                hcpe_result_to_stm(game.header.result, side).context("不正な手番基準の勝敗")?;
            output.write_all(&u16::try_from(number)?.to_le_bytes())?;
            output.write_all(&encode(&position)?)?;
            output.write_all(&u16::try_from(make_move_label(selected, side))?.to_le_bytes())?;
            output.write_all(&u16::try_from(labels.len())?.to_le_bytes())?;
            output.write_all(&u16::try_from(candidates.len())?.to_le_bytes())?;
            output.write_all(&[u8::try_from(score + 1)?])?;
            for &(_, label) in &labels {
                output.write_all(&label.to_le_bytes())?;
            }
            for &(label, visits) in &candidates {
                output.write_all(&label.to_le_bytes())?;
                output.write_all(&visits.to_le_bytes())?;
            }
        }
        let gives_check = position.gives_check(selected);
        position.do_move(selected, gives_check);
    }
    Ok(u64::from(rows))
}

fn convert(mut input: impl Read, output: &mut impl Write, name: &str) -> Result<(u64, u64)> {
    let mut games = 0u64;
    let mut rows = 0u64;
    loop {
        let context = || format!("{name} の対局 {}", games + 1);
        let Some(game) = read_game(&mut input).with_context(context)? else {
            return Ok((games, rows));
        };
        rows += write_game(output, &game).with_context(context)?;
        games += 1;
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(
        args.inputs.iter().filter(|path| path.as_os_str() == "-").count() <= 1,
        "標準入力は一度だけ指定できます"
    );
    let mut output = BufWriter::new(io::stdout().lock());
    output.write_all(HEADER)?;
    for input in &args.inputs {
        let name = input.to_string_lossy();
        let (games, rows) = if input.as_os_str() == "-" {
            convert(io::stdin().lock(), &mut output, &name)?
        } else {
            let file = File::open(input).with_context(|| format!("{name} を開けません"))?;
            convert(BufReader::new(file), &mut output, &name)?
        };
        eprintln!("{name} {games} games, {rows} policy rows");
    }
    output.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    use rshogi_core::position::Position;
    use rshogi_core::types::Move;
    use tools::packed_sfen::pack_position_hcp;

    fn move_code(usi: &str) -> u16 {
        move_to_hcpe_move16(Move::from_usi(usi).unwrap())
    }

    fn game(moves: &[(&str, &[(&str, u16)])]) -> Vec<u8> {
        let mut position = Position::new();
        position.set_hirate();
        let mut bytes = pack_position_hcp(&position).to_vec();
        bytes.extend_from_slice(&u16::try_from(moves.len()).unwrap().to_le_bytes());
        bytes.extend_from_slice(&[2 | 8, 0]);
        for &(played, visits) in moves {
            bytes.extend_from_slice(&move_code(played).to_le_bytes());
            bytes.extend_from_slice(&0i16.to_le_bytes());
            bytes.extend_from_slice(&u16::try_from(visits.len()).unwrap().to_le_bytes());
            for &(candidate, count) in visits {
                bytes.extend_from_slice(&move_code(candidate).to_le_bytes());
                bytes.extend_from_slice(&count.to_le_bytes());
            }
        }
        bytes
    }

    #[test]
    fn replays_zero_visit_moves_and_preserves_selected_and_searched_labels() {
        let raw = game(&[
            ("7g7f", &[("7g7f", 0)]),
            ("3c3d", &[("8c8d", 30), ("3c3d", 0)]),
        ]);
        let mut output = Vec::new();
        assert_eq!(convert(Cursor::new(&raw), &mut output, "input").unwrap(), (1, 1));

        let mut hash = Sha256::new();
        hash.update(&raw[..32]);
        hash.update(move_code("7g7f").to_le_bytes());
        hash.update(move_code("3c3d").to_le_bytes());
        assert_eq!(&output[..32], &hash.finalize()[..]);
        assert_eq!(&output[32..38], &[2, 0, 1, 0, 1, 0]);

        let mut position = Position::new();
        position.set_hirate();
        let first = position.to_move(Move::from_usi("7g7f").unwrap()).unwrap();
        position.do_move(first, position.gives_check(first));
        assert_eq!(&output[38..134], &encode(&position).unwrap());
        let label = |usi| -> [u8; 2] {
            let mv = position.to_move(Move::from_usi(usi).unwrap()).unwrap();
            u16::try_from(make_move_label(mv, position.side_to_move()))
                .unwrap()
                .to_le_bytes()
        };
        assert_eq!(&output[134..136], &label("3c3d"));
        assert_eq!(&output[138..141], &[1, 0, 2]);
        let legal_count = usize::from(u16::from_le_bytes(output[136..138].try_into().unwrap()));
        let visit_start = 141 + 2 * legal_count;
        assert_eq!(&output[visit_start..visit_start + 2], &label("8c8d"));
        assert_eq!(&output[visit_start + 2..], &[30, 0]);
    }

    #[test]
    fn rejects_truncation_and_invalid_positive_candidates() {
        let mut truncated = game(&[("7g7f", &[("7g7f", 4)])]);
        truncated.pop();
        let illegal = game(&[("7g7f", &[("7g7e", 4)])]);
        let duplicated = game(&[("7g7f", &[("7g7f", 4), ("7g7f", 2)])]);
        for input in [truncated, illegal, duplicated] {
            let error = convert(Cursor::new(input), &mut Vec::new(), "input").unwrap_err();
            assert!(error.to_string().contains("対局 1"), "{error:#}");
        }
    }
}
