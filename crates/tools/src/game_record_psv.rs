//! 対局単位の可変長棋譜 (GenSfen `.pack`、dlshogi hcpe3 など) を 40 byte 固定長の PSV へ
//! 展開する共通処理。
//!
//! 形式ごとの差は [`GameRecordFormat`] (1 局の読み出しと PSV への展開) に閉じ込め、
//! 入力パス順の保持、有界バッチでの並列変換、出力の一時ファイル経由の置換はここで扱う。

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::output_path::ensure_safe_output_path;
use crate::packed_sfen::PackedSfenValue;

/// 1 バッチに積む最大対局数。
pub const MAX_BATCH_GAMES: usize = 128;
/// 1 バッチの局面数の目安。最後に加える 1 局は分割しない。
pub const BATCH_POSITIONS: usize = 16 * 1024;
const INPUT_BUFFER_SIZE: usize = 64 * 1024;
const OUTPUT_BUFFER_SIZE: usize = 1024 * 1024;

/// 対局単位の棋譜形式。
pub trait GameRecordFormat: Sync {
    /// 読み出した 1 局。
    type Game: Send + Sync;

    /// 出力先ディレクトリに作る一時ファイル名の接頭辞。
    const TEMP_PREFIX: &'static str;

    /// 次の 1 局を読む。ファイル末尾 (対局の境界) なら `Ok(None)`。
    fn read_game(&self, reader: &mut impl Read) -> io::Result<Option<Self::Game>>;

    /// バッチ分割に使う局面数の目安。
    fn position_count(&self, game: &Self::Game) -> usize;

    /// 1 局を PSV に展開する。
    fn convert_game(&self, game: &Self::Game) -> io::Result<ConvertedGame>;
}

/// 1 局を展開した結果。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ConvertedGame {
    /// PSV レコードを連結したバイト列。
    pub psv: Vec<u8>,
    /// 形式の規約で学習対象から外した局面数 (エラーではないもの)。
    pub skipped_positions: u64,
}

/// 変換全体の集計。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ConversionStats {
    /// 読み出した対局数。
    pub games: u64,
    /// 書き出した PSV レコード数。
    pub positions: u64,
    /// 形式の規約で学習対象から外した局面数の合計。
    pub skipped_positions: u64,
}

/// 変換待ちの 1 局と、エラー表示用の出所。
struct PendingGame<G> {
    game: G,
    file_index: usize,
    game_number: u64,
}

fn game_error(path: &Path, game_number: u64, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{} の対局 {game_number}: {error}", path.display()))
}

/// バッチ内の対局を並列に展開し、入力順に書き出す。
fn write_batch<F: GameRecordFormat>(
    format: &F,
    batch: &[PendingGame<F::Game>],
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
            format
                .convert_game(&pending.game)
                .map_err(|error| game_error(&paths[pending.file_index], pending.game_number, error))
        })
        .collect();
    for result in results {
        let converted = result?;
        writer.write_all(&converted.psv)?;
        stats.games += 1;
        stats.positions += (converted.psv.len() / PackedSfenValue::SIZE) as u64;
        stats.skipped_positions += converted.skipped_positions;
    }
    Ok(())
}

/// `paths` を順に読み、`output` を PSV で置き換える。`max_games` が 0 なら全対局。
///
/// 出力は同じディレクトリの一時ファイルへ書き、全件成功した後で置換する。失敗時は
/// 一時ファイルを破棄し、既存の出力を保持する。
pub fn convert_paths<F: GameRecordFormat>(
    format: &F,
    paths: &[PathBuf],
    output: &Path,
    max_games: u64,
) -> io::Result<ConversionStats> {
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
    builder.prefix(F::TEMP_PREFIX).suffix(".tmp");
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
                let Some(game) = format
                    .read_game(&mut reader)
                    .map_err(|error| game_error(path, game_number + 1, error))?
                else {
                    break;
                };
                game_number += 1;
                read_games += 1;
                batch_positions += format.position_count(&game);
                batch.push(PendingGame {
                    game,
                    file_index,
                    game_number,
                });
                if batch.len() >= MAX_BATCH_GAMES || batch_positions >= BATCH_POSITIONS {
                    write_batch(format, &batch, paths, &mut writer, &mut stats)?;
                    batch.clear();
                    batch_positions = 0;
                }
            }
        }
        write_batch(format, &batch, paths, &mut writer, &mut stats)?;
        writer.flush()?;
    }
    #[cfg(unix)]
    if let Some(permissions) = existing_permissions {
        use std::os::unix::fs::PermissionsExt;
        // 書込みで setuid/setgid が落ちるため、flush 後に権限を復元する。
        // fchmod が成功しても非所属 group の setgid は落ち得るので、実値を確認する。
        let expected_mode = permissions.mode() & 0o7777;
        staged.as_file().set_permissions(permissions)?;
        let actual_mode = staged.as_file().metadata()?.permissions().mode() & 0o7777;
        if actual_mode != expected_mode {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "既存出力の mode bits を保持できません: expected={expected_mode:04o}, actual={actual_mode:04o}"
                ),
            ));
        }
    }
    staged.as_file().sync_all()?;
    staged.persist(output).map_err(|error| error.error)?;
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct OneRecordFormat;

    impl GameRecordFormat for OneRecordFormat {
        type Game = ();
        const TEMP_PREFIX: &'static str = ".game_record_psv_test_";

        fn read_game(&self, _reader: &mut impl Read) -> io::Result<Option<()>> {
            Ok(None)
        }

        fn position_count(&self, _game: &()) -> usize {
            1
        }

        fn convert_game(&self, _game: &()) -> io::Result<ConvertedGame> {
            Ok(ConvertedGame {
                psv: vec![0; PackedSfenValue::SIZE],
                skipped_positions: 0,
            })
        }
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
        let batch = [PendingGame {
            game: (),
            file_index: 0,
            game_number: 1,
        }];
        let error = write_batch(
            &OneRecordFormat,
            &batch,
            &[PathBuf::from("input.bin")],
            &mut FailingWriter,
            &mut ConversionStats::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("write failed"));
    }
}
