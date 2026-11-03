//! dlshogi の可変長 hcpe3 棋譜を 40 byte 固定長の PSV へ変換する。
//!
//! hcpe3 は 1 局を「開始局面 (HuffmanCodedPos) + 指し手列」で持つため、開始局面から
//! 指し手を順に指して各局面を復元する。候補手を持たない (探索していない) 手の局面は
//! 評価値が付いていないので出力しない。

use std::io::{self, Read};
use std::path::{Path, PathBuf};

use clap::Parser;
use rshogi_core::movegen::{MoveList, generate_legal_all};
use rshogi_core::position::Position;
use tools::common::dedup::collect_input_paths;
use tools::game_record_psv::{ConversionStats, ConvertedGame, GameRecordFormat};
use tools::packed_sfen::{
    PackedSfenValue, hcpe_move16_to_psv, hcpe_result_to_stm, move_to_hcpe_move16, pack_position,
};

/// hcp(32) + moveNum(u16) + result(u8) + opponent(u8)。
const GAME_HEADER_SIZE: usize = 36;
/// selectedMove16(u16) + eval(i16) + candidateNum(u16)。
const MOVE_INFO_SIZE: usize = 6;
/// move16(u16) + visitNum(u16)。
const MOVE_VISITS_SIZE: u64 = 4;
/// result の下位 2 bit が勝敗。上位 bit は千日手・入玉・最大手数などの終局理由。
const RESULT_MASK: u8 = 0b11;

#[derive(Parser, Debug)]
#[command(
    name = "hcpe3_to_psv",
    about = "dlshogi の hcpe3 (可変長棋譜) を PSV (PackedSfenValue 40B/レコード) に変換する"
)]
struct Args {
    /// 入力 hcpe3 ファイル（カンマ区切りで複数可）。--input-dir と排他
    #[arg(long)]
    input: Option<String>,

    /// 入力ディレクトリ。--pattern と組み合わせて使用。--input と排他
    #[arg(long)]
    input_dir: Option<PathBuf>,

    /// --input-dir 使用時の glob パターン
    #[arg(long, default_value = "*.hcpe3")]
    pattern: String,

    /// 出力ファイルパス（PSV形式）。正常終了時に既存ファイルを置換
    #[arg(long)]
    output: PathBuf,

    /// 全入力ファイルを通算した最大対局数（0 = 全件）
    #[arg(long, default_value = "0")]
    max_games: u64,
}

struct MoveRecord {
    move16: u16,
    eval: i16,
    searched: bool,
}

struct Hcpe3Game {
    hcp: [u8; 32],
    result: u8,
    moves: Vec<MoveRecord>,
}

fn invalid_data(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// 対局の境界での EOF だけを正常終了とし、途中で切れた対局は `UnexpectedEof` にする。
fn read_header(reader: &mut impl Read) -> io::Result<Option<[u8; GAME_HEADER_SIZE]>> {
    let mut header = [0u8; GAME_HEADER_SIZE];
    let mut filled = 0;
    while filled < GAME_HEADER_SIZE {
        match reader.read(&mut header[filled..]) {
            Ok(0) if filled == 0 => return Ok(None),
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!("対局ヘッダが途中で終わっています ({filled}/{GAME_HEADER_SIZE} byte)"),
                ));
            }
            Ok(n) => filled += n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(Some(header))
}

fn read_game(reader: &mut impl Read) -> io::Result<Option<Hcpe3Game>> {
    let Some(header) = read_header(reader)? else {
        return Ok(None);
    };
    let mut hcp = [0u8; 32];
    hcp.copy_from_slice(&header[..32]);
    let move_num = u16::from_le_bytes([header[32], header[33]]);
    let result = header[34] & RESULT_MASK;
    if result > 2 {
        return Err(invalid_data(format!("不正な勝敗値: result={:#04x}", header[34])));
    }

    let mut moves = Vec::with_capacity(usize::from(move_num));
    for _ in 0..move_num {
        let mut info = [0u8; MOVE_INFO_SIZE];
        reader.read_exact(&mut info)?;
        let candidate_num = u16::from_le_bytes([info[4], info[5]]);
        // 候補手の訪問数 (policy) は PSV に載せないので読み飛ばす。
        let skip = u64::from(candidate_num) * MOVE_VISITS_SIZE;
        let skipped = io::copy(&mut reader.by_ref().take(skip), &mut io::sink())?;
        if skipped != skip {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "候補手の列が途中で終わっています",
            ));
        }
        moves.push(MoveRecord {
            move16: u16::from_le_bytes([info[0], info[1]]),
            eval: i16::from_le_bytes([info[2], info[3]]),
            searched: candidate_num > 0,
        });
    }
    Ok(Some(Hcpe3Game { hcp, result, moves }))
}

fn start_position(hcp: &[u8; 32]) -> io::Result<Position> {
    let sfen = tools::packed_sfen::unpack_hcp(hcp)
        .map_err(|error| invalid_data(format!("HuffmanCodedPos デコード失敗: {error}")))?;
    // hcpe3 は開始局面の手数を持たないので、開始局面を 1 手目とする。
    let sfen = format!("{} 1", sfen.rsplit_once(' ').map(|(prefix, _)| prefix).unwrap_or(&sfen));
    let mut pos = Position::new();
    pos.set_sfen(&sfen)
        .map_err(|error| invalid_data(format!("SFEN パース失敗 ({sfen}): {error}")))?;
    Ok(pos)
}

fn convert_game(game: &Hcpe3Game) -> io::Result<ConvertedGame> {
    let mut pos = start_position(&game.hcp)?;
    pos.try_reserve_move_history(game.moves.len())
        .map_err(|error| io::Error::new(io::ErrorKind::OutOfMemory, error.to_string()))?;
    let searched = game.moves.iter().filter(|record| record.searched).count();
    let mut converted = ConvertedGame {
        psv: Vec::with_capacity(searched * PackedSfenValue::SIZE),
        skipped_positions: (game.moves.len() - searched) as u64,
    };

    for record in &game.moves {
        let mut legal_moves = MoveList::new();
        generate_legal_all(&pos, &mut legal_moves);
        let mv = legal_moves
            .iter()
            .find(|m| move_to_hcpe_move16(**m) == record.move16)
            .copied()
            .ok_or_else(|| {
                invalid_data(format!(
                    "不正な指し手: move16={:#06x}, game_ply={}",
                    record.move16,
                    pos.game_ply()
                ))
            })?;

        if record.searched {
            let game_ply =
                u16::try_from(pos.game_ply()).map_err(|error| invalid_data(error.to_string()))?;
            let game_result = hcpe_result_to_stm(game.result, pos.side_to_move())
                .ok_or_else(|| invalid_data(format!("不正な勝敗値: result={}", game.result)))?;
            let psv = PackedSfenValue {
                sfen: pack_position(&pos),
                score: record.eval,
                move16: hcpe_move16_to_psv(record.move16),
                game_ply,
                game_result,
                padding: 0,
            };
            converted.psv.extend_from_slice(&psv.to_bytes());
        }
        let gives_check = pos.gives_check(mv);
        pos.do_move(mv, gives_check);
    }
    Ok(converted)
}

struct Hcpe3Format;

impl GameRecordFormat for Hcpe3Format {
    type Game = Hcpe3Game;
    const TEMP_PREFIX: &'static str = ".hcpe3_to_psv_";

    fn read_game(&self, reader: &mut impl Read) -> io::Result<Option<Hcpe3Game>> {
        read_game(reader)
    }

    fn position_count(&self, game: &Hcpe3Game) -> usize {
        game.moves.len()
    }

    fn convert_game(&self, game: &Hcpe3Game) -> io::Result<ConvertedGame> {
        convert_game(game)
    }
}

fn convert_paths(paths: &[PathBuf], output: &Path, max_games: u64) -> io::Result<ConversionStats> {
    tools::game_record_psv::convert_paths(&Hcpe3Format, paths, output, max_games)
}

fn run(args: Args) -> io::Result<()> {
    let paths = collect_input_paths(args.input.as_deref(), args.input_dir.as_ref(), &args.pattern)?;
    if paths.is_empty() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "入力ファイルが見つかりません"));
    }
    let start = std::time::Instant::now();
    eprintln!("Processing {} files (max_games={})...", paths.len(), args.max_games);
    let stats = convert_paths(&paths, &args.output, args.max_games)?;
    println!("=== hcpe3 → PSV Summary ===");
    println!("Input files:        {}", paths.len());
    println!("Games:              {}", stats.games);
    println!("Positions:          {}", stats.positions);
    println!("Skipped unsearched: {}", stats.skipped_positions);
    println!("Output file:        {}", args.output.display());
    println!(
        "Output size:        {:.1} MB",
        (stats.positions * PackedSfenValue::SIZE as u64) as f64 / (1024.0 * 1024.0)
    );
    println!("Elapsed:            {:.1} sec", start.elapsed().as_secs_f64());
    Ok(())
}

fn main() -> io::Result<()> {
    run(Args::parse())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rshogi_core::types::Move;
    use std::io::Cursor;
    use tempfile::TempDir;
    use tools::packed_sfen::{move_to_psv_move16, pack_position_hcp};

    fn hirate() -> Position {
        let mut pos = Position::new();
        pos.set_hirate();
        pos
    }

    /// `moves` は (USI, eval, 候補手数)。
    fn hcpe3_game(start: &Position, moves: &[(&str, i16, u16)], result: u8) -> Vec<u8> {
        let mut bytes = pack_position_hcp(start).to_vec();
        bytes.extend_from_slice(&(moves.len() as u16).to_le_bytes());
        bytes.extend_from_slice(&[result, 0]);
        for &(usi, eval, candidates) in moves {
            let move16 = move_to_hcpe_move16(Move::from_usi(usi).unwrap());
            bytes.extend_from_slice(&move16.to_le_bytes());
            bytes.extend_from_slice(&eval.to_le_bytes());
            bytes.extend_from_slice(&candidates.to_le_bytes());
            for _ in 0..candidates {
                bytes.extend_from_slice(&move16.to_le_bytes());
                bytes.extend_from_slice(&1u16.to_le_bytes());
            }
        }
        bytes
    }

    fn write_input(dir: &TempDir, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn records(bytes: &[u8]) -> Vec<PackedSfenValue> {
        bytes
            .as_chunks::<{ PackedSfenValue::SIZE }>()
            .0
            .iter()
            .map(|chunk| PackedSfenValue::from_bytes(chunk).unwrap())
            .collect()
    }

    #[test]
    fn replays_moves_and_converts_result_to_side_to_move() {
        let dir = TempDir::new().unwrap();
        // 先手勝ち (1) に千日手の終局理由 bit (4) が立っていても勝敗は下位 2 bit で読む。
        let game = hcpe3_game(&hirate(), &[("7g7f", 30, 2), ("3c3d", -25, 1)], 1 | 4);
        let input = write_input(&dir, "a.hcpe3", &game);
        let output = dir.path().join("out.psv");
        let stats = convert_paths(&[input], &output, 0).unwrap();
        assert_eq!(
            stats,
            ConversionStats {
                games: 1,
                positions: 2,
                skipped_positions: 0
            }
        );
        let out = records(&std::fs::read(output).unwrap());
        let mut pos = hirate();
        assert_eq!(out[0].sfen, pack_position(&pos));
        assert_eq!((out[0].score, out[0].game_ply, out[0].game_result), (30, 1, 1));
        assert_eq!(out[0].move16, move_to_psv_move16(Move::from_usi("7g7f").unwrap()));
        let mv = pos.to_move(Move::from_usi("7g7f").unwrap()).unwrap();
        let gives_check = pos.gives_check(mv);
        pos.do_move(mv, gives_check);
        assert_eq!(out[1].sfen, pack_position(&pos));
        assert_eq!((out[1].score, out[1].game_ply, out[1].game_result), (-25, 2, -1));
    }

    #[test]
    fn unsearched_moves_are_replayed_but_not_written() {
        let dir = TempDir::new().unwrap();
        let mut bytes = hcpe3_game(&hirate(), &[("7g7f", 0, 0), ("3c3d", -25, 1)], 2);
        bytes.extend(hcpe3_game(&hirate(), &[("2g2f", 10, 3)], 0));
        let input = write_input(&dir, "a.hcpe3", &bytes);
        let output = dir.path().join("out.psv");
        let stats = convert_paths(&[input], &output, 0).unwrap();
        assert_eq!(
            stats,
            ConversionStats {
                games: 2,
                positions: 2,
                skipped_positions: 1
            }
        );
        let out = records(&std::fs::read(output).unwrap());
        assert_eq!((out[0].score, out[0].game_ply, out[0].game_result), (-25, 2, 1));
        assert_eq!((out[1].score, out[1].game_ply, out[1].game_result), (10, 1, 0));
    }

    #[test]
    fn non_hirate_start_position_is_kept() {
        let mut start = Position::new();
        start.set_sfen("4k4/9/9/9/9/9/9/9/4K4 b P 1").unwrap();
        let game = read_game(&mut Cursor::new(hcpe3_game(&start, &[("P*5e", 0, 1)], 0)))
            .unwrap()
            .unwrap();
        let converted = convert_game(&game).unwrap();
        assert_eq!(records(&converted.psv)[0].sfen, pack_position(&start));
    }

    #[test]
    fn max_games_counts_across_files() {
        let dir = TempDir::new().unwrap();
        let game = hcpe3_game(&hirate(), &[("7g7f", 0, 1)], 0);
        let paths = vec![
            write_input(&dir, "a.hcpe3", &game),
            write_input(&dir, "b.hcpe3", &game.repeat(2)),
        ];
        let output = dir.path().join("out.psv");
        assert_eq!(convert_paths(&paths, &output, 2).unwrap().games, 2);
        assert_eq!(std::fs::metadata(output).unwrap().len(), 80);
    }

    #[test]
    fn malformed_input_preserves_existing_output() {
        let complete = hcpe3_game(&hirate(), &[("7g7f", 0, 2)], 0);
        let mut malformed = vec![
            hcpe3_game(&hirate(), &[("7g7e", 0, 1)], 0),
            hcpe3_game(&hirate(), &[("7g7f", 0, 1)], 3),
        ];
        for length in 1..complete.len() {
            malformed.push(complete[..length].to_vec());
        }
        for tail in malformed {
            let dir = TempDir::new().unwrap();
            let mut bytes = complete.clone();
            bytes.extend(tail);
            let input = write_input(&dir, "input.hcpe3", &bytes);
            let output = write_input(&dir, "output.psv", b"existing output");
            let error = convert_paths(&[input], &output, 0).unwrap_err();
            assert!(
                matches!(error.kind(), io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof),
                "{error}"
            );
            assert!(error.to_string().contains("の対局 2"), "{error}");
            assert_eq!(std::fs::read(&output).unwrap(), b"existing output");
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
        }
    }
}
