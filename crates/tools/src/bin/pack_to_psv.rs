//! GenSfen の可変長 .pack 棋譜を40 byte固定長のPSVへ変換する。

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use clap::Parser;
use rayon::prelude::*;
use rshogi_core::movegen::{MoveList, generate_legal_all};
use rshogi_core::position::Position;
use rshogi_core::types::Color;
use tools::common::dedup::collect_input_paths;
use tools::output_path::ensure_safe_output_path;
use tools::packed_sfen::{PackedSfenValue, hcpe_move16_to_psv, move_to_hcpe_move16, pack_position};

const MAX_BATCH_GAMES: usize = 128;
const BATCH_POSITIONS: usize = 16 * 1024;
const INPUT_BUFFER_SIZE: usize = 64 * 1024;
const OUTPUT_BUFFER_SIZE: usize = 1024 * 1024;

#[derive(Parser, Debug)]
#[command(name = "pack_to_psv")]
struct Args {
    /// 入力 .pack ファイル（カンマ区切りで複数可）。--input-dir と排他
    #[arg(long)]
    input: Option<String>,

    /// 入力ディレクトリ。--pattern と組み合わせて使用。--input と排他
    #[arg(long)]
    input_dir: Option<PathBuf>,

    /// --input-dir 使用時の glob パターン
    #[arg(long, default_value = "*.pack")]
    pattern: String,

    /// 出力ファイルパス（PSV形式）。正常終了時に既存ファイルを置換
    #[arg(long)]
    output: PathBuf,

    /// 全入力ファイルを通算した最大対局数（0 = 全件）
    #[arg(long, default_value = "0")]
    max_games: u64,
}

enum StartPosition {
    Hirate,
    Hcp { bytes: [u8; 32], game_ply: u16 },
}

impl StartPosition {
    fn game_ply(&self) -> u16 {
        match self {
            Self::Hirate => 1,
            Self::Hcp { game_ply, .. } => *game_ply,
        }
    }

    fn position(&self) -> io::Result<Position> {
        let mut pos = Position::new();
        match self {
            Self::Hirate => pos.set_hirate(),
            Self::Hcp { bytes, game_ply } => {
                let sfen = tools::packed_sfen::unpack_hcp(bytes).map_err(|error| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("HuffmanCodedPos デコード失敗: {error}"),
                    )
                })?;
                let sfen_with_ply = format!(
                    "{} {game_ply}",
                    sfen.rsplit_once(' ').map(|(prefix, _)| prefix).unwrap_or(&sfen)
                );
                pos.set_sfen(&sfen_with_ply).map_err(|error| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("SFEN パース失敗 ({sfen_with_ply}): {error}"),
                    )
                })?;
            }
        }
        Ok(pos)
    }
}

struct PackGame {
    start: StartPosition,
    moves: Vec<(u16, i16)>,
    result: u8,
}

struct PendingGame {
    game: PackGame,
    file_index: usize,
    game_number: u64,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ConversionStats {
    games: u64,
    positions: u64,
}

fn read_bytes<const N: usize>(reader: &mut impl Read) -> io::Result<[u8; N]> {
    let mut bytes = [0; N];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn is_end_marker(move16: u16) -> bool {
    let to = move16 & 0x7F;
    let from = (move16 >> 7) & 0x7F;
    to == from
}

fn read_game(reader: &mut impl Read) -> io::Result<Option<PackGame>> {
    let start_flag = match read_bytes::<1>(reader) {
        Ok([flag]) => flag,
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    };
    let start = match start_flag {
        1 => StartPosition::Hirate,
        0 => StartPosition::Hcp {
            bytes: read_bytes(reader)?,
            game_ply: u16::from_le_bytes(read_bytes(reader)?),
        },
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("不明な開始局面フラグ: {start_flag}"),
            ));
        }
    };

    // 勝敗は対局末尾にある。PSVの手数範囲で指し手と再生時の履歴を有界にする。
    let max_positions = usize::from(u16::MAX - start.game_ply()) + 1;
    let mut moves = Vec::new();
    loop {
        let move16 = u16::from_le_bytes(read_bytes(reader)?);
        if is_end_marker(move16) {
            read_bytes::<1>(reader)?;
            return Ok(Some(PackGame {
                start,
                moves,
                result: (move16 & 0x7F) as u8,
            }));
        }
        if moves.len() >= max_positions {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "対局の手数がPSVのgame_ply上限65535を超えます",
            ));
        }
        let eval = i16::from_le_bytes(read_bytes(reader)?);
        moves.push((move16, eval));
    }
}

fn convert_game_result(pack_result: u8, stm: Color) -> i8 {
    match pack_result {
        1 => {
            if stm == Color::Black {
                1
            } else {
                -1
            }
        }
        2 => {
            if stm == Color::White {
                1
            } else {
                -1
            }
        }
        _ => 0,
    }
}

fn convert_game(game: &PackGame) -> io::Result<Vec<u8>> {
    let mut pos = game.start.position()?;
    let mut output = Vec::with_capacity(game.moves.len() * PackedSfenValue::SIZE);

    for &(move16, eval) in &game.moves {
        let mut legal_moves = MoveList::new();
        generate_legal_all(&pos, &mut legal_moves);
        let mv = legal_moves
            .iter()
            .find(|m| move_to_hcpe_move16(**m) == move16)
            .copied()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("不正な指し手: move16={move16:#06x}, game_ply={}", pos.game_ply()),
                )
            })?;

        let game_ply = u16::try_from(pos.game_ply())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let psv = PackedSfenValue {
            sfen: pack_position(&pos),
            score: eval,
            move16: hcpe_move16_to_psv(move16),
            game_ply,
            game_result: convert_game_result(game.result, pos.side_to_move()),
            padding: 0,
        };
        output.extend_from_slice(&psv.to_bytes());
        let gives_check = pos.gives_check(mv);
        pos.do_move(mv, gives_check);
    }
    Ok(output)
}

fn game_error(path: &Path, game_number: u64, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{} の対局 {game_number}: {error}", path.display()))
}

fn write_batch(
    batch: &[PendingGame],
    paths: &[PathBuf],
    writer: &mut impl Write,
    stats: &mut ConversionStats,
) -> io::Result<()> {
    if batch.is_empty() {
        return Ok(());
    }
    let results: Vec<_> = batch
        .par_iter()
        .map(|pending| {
            convert_game(&pending.game)
                .map_err(|error| game_error(&paths[pending.file_index], pending.game_number, error))
        })
        .collect();
    for result in results {
        let output = result?;
        writer.write_all(&output)?;
        stats.games += 1;
        stats.positions += (output.len() / PackedSfenValue::SIZE) as u64;
    }
    Ok(())
}

fn convert_paths(paths: &[PathBuf], output: &Path, max_games: u64) -> io::Result<ConversionStats> {
    let existing_permissions = match std::fs::symlink_metadata(output) {
        Ok(metadata) if metadata.is_file() => Some(metadata.permissions()),
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("出力には通常ファイルを指定してください: {}", output.display()),
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    for path in paths {
        ensure_safe_output_path(output, path)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    }
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut builder = tempfile::Builder::new();
    builder.prefix(".pack_to_psv_").suffix(".tmp");
    #[cfg(windows)]
    if existing_permissions.as_ref().is_some_and(|permissions| permissions.readonly()) {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "出力は読み取り専用です"));
    }
    #[cfg(unix)]
    if existing_permissions.is_none() {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o666));
    }
    let mut staged = builder.tempfile_in(parent)?;
    let mut stats = ConversionStats::default();
    {
        let mut writer = BufWriter::with_capacity(OUTPUT_BUFFER_SIZE, staged.as_file_mut());
        let mut batch = Vec::with_capacity(MAX_BATCH_GAMES);
        let mut batch_positions = 0;
        let mut read_games = 0u64;
        for (file_index, path) in paths.iter().enumerate() {
            if max_games > 0 && read_games >= max_games {
                break;
            }
            eprintln!("Reading: {}", path.display());
            let mut reader = BufReader::with_capacity(INPUT_BUFFER_SIZE, File::open(path)?);
            let mut game_number = 0;
            while max_games == 0 || read_games < max_games {
                let Some(game) = read_game(&mut reader)
                    .map_err(|error| game_error(path, game_number + 1, error))?
                else {
                    break;
                };
                game_number += 1;
                read_games += 1;
                batch_positions += game.moves.len();
                batch.push(PendingGame {
                    game,
                    file_index,
                    game_number,
                });
                if batch.len() >= MAX_BATCH_GAMES || batch_positions >= BATCH_POSITIONS {
                    write_batch(&batch, paths, &mut writer, &mut stats)?;
                    batch.clear();
                    batch_positions = 0;
                }
            }
        }
        write_batch(&batch, paths, &mut writer, &mut stats)?;
        writer.flush()?;
    }
    #[cfg(unix)]
    if let Some(permissions) = existing_permissions {
        // 書込みで setuid/setgid が落ちるため、flush 後に権限を復元する。
        staged.as_file().set_permissions(permissions)?;
    }
    staged.as_file().sync_all()?;
    staged.persist(output).map_err(|error| error.error)?;
    Ok(stats)
}

fn run(args: Args) -> io::Result<()> {
    let paths = collect_input_paths(args.input.as_deref(), args.input_dir.as_ref(), &args.pattern)?;
    if paths.is_empty() {
        eprintln!("入力ファイルが見つかりません");
        return Ok(());
    }
    let start = std::time::Instant::now();
    eprintln!(
        "Processing {} files (max_games={}, bounded batches)...",
        paths.len(),
        args.max_games
    );
    let stats = convert_paths(&paths, &args.output, args.max_games)?;
    println!("=== Pack → PSV Summary ===");
    println!("Input files:     {}", paths.len());
    println!("Games:           {}", stats.games);
    println!("Positions:       {}", stats.positions);
    println!("Output file:     {}", args.output.display());
    println!(
        "Output size:     {:.1} MB",
        (stats.positions * PackedSfenValue::SIZE as u64) as f64 / (1024.0 * 1024.0)
    );
    println!("Elapsed:         {:.1} sec", start.elapsed().as_secs_f64());
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
    use tools::packed_sfen::move_to_psv_move16;

    fn pack_game(moves: &[&str], score: i16, result: u8) -> Vec<u8> {
        let mut bytes = vec![1];
        for usi in moves {
            bytes.extend_from_slice(
                &move_to_hcpe_move16(Move::from_usi(usi).unwrap()).to_le_bytes(),
            );
            bytes.extend_from_slice(&score.to_le_bytes());
        }
        let marker = u16::from(result) | (u16::from(result) << 7);
        bytes.extend_from_slice(&marker.to_le_bytes());
        bytes.push(0);
        bytes
    }

    fn write_input(dir: &TempDir, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn max_games_counts_across_files_and_ignores_empty_files() {
        let dir = TempDir::new().unwrap();
        let game = pack_game(&["7g7f"], 123, 0);
        let paths = vec![
            write_input(&dir, "a.pack", &game),
            write_input(&dir, "empty.pack", &[]),
            write_input(&dir, "b.pack", &game),
            write_input(&dir, "c.pack", &game),
        ];
        for (limit, games) in [(1, 1), (2, 2), (3, 3), (4, 3), (0, 3)] {
            let output = dir.path().join(format!("limit-{limit}.psv"));
            let stats = convert_paths(&paths, &output, limit).unwrap();
            assert_eq!(
                stats,
                ConversionStats {
                    games,
                    positions: games
                }
            );
            assert_eq!(std::fs::metadata(output).unwrap().len(), games * 40);
        }
    }

    #[test]
    fn max_games_keeps_whole_game_and_does_not_parse_following_data() {
        let dir = TempDir::new().unwrap();
        let mut bytes = pack_game(&["7g7f", "3c3d"], -123, 1);
        bytes.push(255);
        let first = write_input(&dir, "first.pack", &bytes);
        let second = write_input(&dir, "second.pack", &[255]);
        let output = dir.path().join("output.psv");
        let stats = convert_paths(&[first, second], &output, 1).unwrap();
        assert_eq!(
            stats,
            ConversionStats {
                games: 1,
                positions: 2
            }
        );
        let bytes = std::fs::read(output).unwrap();
        let first = PackedSfenValue::from_bytes(&bytes[..40]).unwrap();
        let second = PackedSfenValue::from_bytes(&bytes[40..]).unwrap();
        assert_eq!((first.game_result, second.game_result), (1, -1));
        assert_eq!((first.game_ply, second.game_ply), (1, 2));
        assert_eq!(first.score, -123);
        assert_eq!(first.move16, move_to_psv_move16(Move::from_usi("7g7f").unwrap()));
        let mut pos = Position::new();
        pos.set_hirate();
        assert_eq!(first.sfen, pack_position(&pos));
        let mv = pos.to_move(Move::from_usi("7g7f").unwrap()).unwrap();
        let gives_check = pos.gives_check(mv);
        pos.do_move(mv, gives_check);
        assert_eq!(second.sfen, pack_position(&pos));
    }

    #[test]
    fn zero_move_games_count_toward_limit() {
        let dir = TempDir::new().unwrap();
        let mut bytes = pack_game(&[], 0, 0);
        bytes.extend(pack_game(&["7g7f"], 0, 0));
        let input = write_input(&dir, "input.pack", &bytes);
        let output = dir.path().join("output.psv");
        assert_eq!(
            convert_paths(&[input], &output, 1).unwrap(),
            ConversionStats {
                games: 1,
                positions: 0
            }
        );
        assert!(std::fs::read(output).unwrap().is_empty());
    }

    #[test]
    fn parallel_batches_preserve_sorted_file_and_game_order() {
        let dir = TempDir::new().unwrap();
        let mut expected = Vec::new();
        let mut input_paths = Vec::new();
        for name in ["a.pack", "b.pack"] {
            let mut bytes = Vec::new();
            for score in 0..(MAX_BATCH_GAMES as i16 + 1) {
                let result = if name == "a.pack" { 1 } else { 2 };
                let game = pack_game(&["7g7f", "3c3d"], score, result);
                let parsed = read_game(&mut Cursor::new(&game)).unwrap().unwrap();
                expected.extend(convert_game(&parsed).unwrap());
                bytes.extend(game);
            }
            input_paths.push(write_input(&dir, name, &bytes));
        }
        let specs = format!("{},{}", input_paths[1].display(), input_paths[0].display());
        let paths = collect_input_paths(Some(&specs), None, "*.pack").unwrap();
        for threads in [1, 2] {
            let output = dir.path().join(format!("threads-{threads}.psv"));
            let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build().unwrap();
            pool.install(|| convert_paths(&paths, &output, 0)).unwrap();
            assert_eq!(std::fs::read(output).unwrap(), expected);
        }
    }

    #[test]
    fn malformed_input_preserves_existing_output_and_removes_staging() {
        let complete = pack_game(&["7g7f"], 123, 0);
        let mut malformed = vec![vec![255], vec![0], pack_game(&["7g7e"], 123, 0)];
        for length in 1..complete.len() {
            malformed.push(complete[..length].to_vec());
        }
        for tail in malformed {
            let dir = TempDir::new().unwrap();
            let mut bytes = complete.repeat(MAX_BATCH_GAMES + 1);
            bytes.extend(tail);
            let input = write_input(&dir, "input.pack", &bytes);
            let output = write_input(&dir, "output.psv", b"existing output");
            let error = convert_paths(std::slice::from_ref(&input), &output, 0).unwrap_err();
            assert!(matches!(
                error.kind(),
                io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof
            ));
            assert_eq!(std::fs::read(&input).unwrap(), bytes);
            assert_eq!(std::fs::read(&output).unwrap(), b"existing output");
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
            std::fs::remove_file(&output).unwrap();
            assert!(convert_paths(&[input], &output, 0).is_err());
            assert!(!output.exists());
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        }
    }

    #[test]
    fn valid_input_replaces_existing_output() {
        let dir = TempDir::new().unwrap();
        let input = write_input(&dir, "input.pack", &pack_game(&["7g7f"], 0, 0));
        let output = write_input(&dir, "output.psv", b"existing output");
        convert_paths(&[input], &output, 0).unwrap();
        assert_eq!(std::fs::metadata(output).unwrap().len(), 40);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn malformed_move_bits_do_not_replace_output() {
        let dir = TempDir::new().unwrap();
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/9/9/9/9/9/9/4K4 b P 1").unwrap();
        let mut bytes = vec![0];
        bytes.extend_from_slice(&tools::packed_sfen::pack_position_hcp(&pos));
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&pack_game(&["P*5e"], 0, 0)[1..]);
        let input = write_input(&dir, "input.pack", &bytes);
        let output = dir.path().join("output.psv");
        convert_paths(std::slice::from_ref(&input), &output, 0).unwrap();
        let valid_output = std::fs::read(&output).unwrap();
        assert_eq!(valid_output.len(), 40);
        for mask in [0x4000u16, 0x8000] {
            let mut invalid = bytes.clone();
            let move16 = u16::from_le_bytes([invalid[35], invalid[36]]) | mask;
            invalid[35..37].copy_from_slice(&move16.to_le_bytes());
            std::fs::write(&input, invalid).unwrap();
            let error = convert_paths(std::slice::from_ref(&input), &output, 0).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert_eq!(std::fs::read(&output).unwrap(), valid_output);
        }
    }

    #[test]
    fn output_directory_is_rejected_before_reading_input() {
        let dir = TempDir::new().unwrap();
        let error = convert_paths(&[dir.path().join("missing.pack")], dir.path(), 0).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("通常ファイル"));
    }

    #[cfg(unix)]
    #[test]
    fn input_directory_read_error_preserves_output() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        write_input(&dir, "valid.pack", &pack_game(&["7g7f"], 0, 0));
        let inaccessible = dir.path().join("inaccessible");
        std::fs::create_dir(&inaccessible).unwrap();
        std::fs::set_permissions(&inaccessible, std::fs::Permissions::from_mode(0)).unwrap();
        if std::fs::read_dir(&inaccessible).is_ok() {
            std::fs::set_permissions(&inaccessible, std::fs::Permissions::from_mode(0o700))
                .unwrap();
            eprintln!("permission fixture skipped: process can read mode 000 directories");
            return;
        }
        let output = write_input(&dir, "output.psv", b"existing output");
        let result = run(Args {
            input: None,
            input_dir: Some(dir.path().to_path_buf()),
            pattern: "*.pack".to_owned(),
            output: output.clone(),
            max_games: 0,
        });
        std::fs::set_permissions(&inaccessible, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(std::fs::read(output).unwrap(), b"existing output");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 3);
    }

    #[cfg(unix)]
    #[test]
    fn output_permissions_match_normal_creation_or_existing_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let input = write_input(&dir, "input.pack", &pack_game(&["7g7f"], 0, 0));
        let regular = write_input(&dir, "regular", b"");
        let normal_mode = std::fs::metadata(regular).unwrap().permissions().mode() & 0o777;
        let output = dir.path().join("output.psv");
        convert_paths(std::slice::from_ref(&input), &output, 0).unwrap();
        assert_eq!(std::fs::metadata(&output).unwrap().permissions().mode() & 0o777, normal_mode);
        for mode in [0o640, 0o4750, 0o2750, 0o7750] {
            std::fs::set_permissions(&output, std::fs::Permissions::from_mode(mode)).unwrap();
            assert_eq!(std::fs::metadata(&output).unwrap().permissions().mode() & 0o7777, mode);
            convert_paths(std::slice::from_ref(&input), &output, 0).unwrap();
            assert_eq!(std::fs::metadata(&output).unwrap().len(), 40);
            assert_eq!(
                std::fs::metadata(&output).unwrap().permissions().mode() & 0o7777,
                mode,
                "output mode {mode:o}"
            );
        }
    }

    #[test]
    fn output_aliases_are_rejected_before_staging() {
        let dir = TempDir::new().unwrap();
        let bytes = pack_game(&["7g7f"], 0, 0);
        let input = write_input(&dir, "input.pack", &bytes);
        let output = dir.path().join("output.psv");
        std::fs::hard_link(&input, &output).unwrap();
        for alias in [&input, &output] {
            assert!(convert_paths(std::slice::from_ref(&input), alias, 0).is_err());
        }
        assert_eq!(std::fs::read(&input).unwrap(), bytes);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn game_length_cannot_overflow_psv_ply() {
        let mut bytes = vec![1];
        let move_bytes = &pack_game(&["7g7f"], 0, 0)[1..5];
        for _ in 0..=u16::MAX {
            bytes.extend_from_slice(move_bytes);
        }
        let error = read_game(&mut Cursor::new(bytes)).err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("65535"));
    }

    #[test]
    fn hcp_start_preserves_position_and_last_representable_ply() {
        let mut pos = Position::new();
        pos.set_hirate();
        let mv = pos.to_move(Move::from_usi("7g7f").unwrap()).unwrap();
        let gives_check = pos.gives_check(mv);
        pos.do_move(mv, gives_check);
        let mut bytes = vec![0];
        bytes.extend_from_slice(&tools::packed_sfen::pack_position_hcp(&pos));
        bytes.extend_from_slice(&u16::MAX.to_le_bytes());
        bytes.extend_from_slice(&pack_game(&["3c3d"], -45, 2)[1..]);
        let game = read_game(&mut Cursor::new(bytes)).unwrap().unwrap();
        let converted = convert_game(&game).unwrap();
        let record = PackedSfenValue::from_bytes(&converted).unwrap();
        assert_eq!(record.sfen, pack_position(&pos));
        assert_eq!(record.game_ply, u16::MAX);
        assert_eq!(record.game_result, 1);
        assert_eq!(record.score, -45);
        let mut too_long = vec![0];
        too_long.extend_from_slice(&tools::packed_sfen::pack_position_hcp(&pos));
        too_long.extend_from_slice(&u16::MAX.to_le_bytes());
        too_long.extend_from_slice(&pack_game(&["3c3d", "2g2f"], 0, 0)[1..]);
        assert_eq!(
            read_game(&mut Cursor::new(too_long)).err().unwrap().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn writer_failure_is_propagated() {
        struct FailingWriter;
        impl Write for FailingWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                assert!(!bytes.is_empty());
                Err(io::Error::other("write failed"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let game = read_game(&mut Cursor::new(pack_game(&["7g7f"], 0, 0))).unwrap().unwrap();
        let batch = [PendingGame {
            game,
            file_index: 0,
            game_number: 1,
        }];
        let error = write_batch(
            &batch,
            &[PathBuf::from("input.pack")],
            &mut FailingWriter,
            &mut ConversionStats::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("write failed"));
    }
}
