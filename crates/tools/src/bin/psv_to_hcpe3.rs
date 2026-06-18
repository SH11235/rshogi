//! psv_to_hcpe3 - PackedSfenValue を dlshogi 学習用 hcpe3 / hcpe に変換
//!
//! YaneuraOu の PackedSfenValue（PSV, 40バイト固定長）を、dlshogi の train.py が
//! 食う形式へストリーミング変換する。PSV は局面単位（棋譜構造を持たない）ため、
//! `--format hcpe3` では各局面を「1 局面 = 1 game」の退化した hcpe3 として書く
//! （moveNum=1 / candidateNum=1 / visitNum=1、policy target は best move の one-hot）。
//! `--format hcpe` は dlshogi 同梱 psv_to_hcpe.py と同じ 38 バイト hcpe を出力する。
//!
//! hcp（HuffmanCodedPos, 32B）・手番・eval・game_result の視点変換は cshogi の
//! `to_hcp` / `move16_from_psv` と同一ロジックを `tools::packed_sfen` で再現する。
//! load-all を避けてチャンクストリーミングし、ピークメモリを入力件数に非依存にする。
//!
//! # 使用例
//!
//! ```bash
//! # PSV -> hcpe3（dlshogi train.py 用、既定）
//! cargo run -p tools --bin psv_to_hcpe3 -- \
//!   --input data.psv --output train.hcpe3
//!
//! # PSV -> hcpe（dlshogi test_data 用、38B）
//! cargo run -p tools --bin psv_to_hcpe3 -- \
//!   --input data.psv --output val.hcpe --format hcpe
//!
//! # 先頭 300 万件だけ変換し全コアを使う
//! cargo run -p tools --bin psv_to_hcpe3 -- \
//!   --input data.psv --output head.hcpe3 --limit 3000000 --threads 0
//! ```

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use rshogi_core::position::Position;
use rshogi_core::types::Color;
use tools::packed_sfen::{PackedSfenValue, pack_position_hcp, unpack_sfen};

/// hcpe3 レコード長（hcp[32] + moveNum:u16 + result:u8 + opponent:u8
/// + selectedMove16:u16 + eval:i16 + candidateNum:u16 + move16:u16 + visitNum:u16）
const HCPE3_SIZE: usize = 46;
/// hcpe レコード長（hcp[32] + eval:i16 + bestMove16:u16 + gameResult:u8 + dummy:u8）
const HCPE_SIZE: usize = 38;
/// 出力レコードバッファ。両形式の最大長を確保し `len` で実長を持つ。
const RECORD_BUF: usize = HCPE3_SIZE;

const IO_BUF_SIZE: usize = 1 << 20;

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum Format {
    /// dlshogi train.py 用の hcpe3（1 局面 = 1 game の退化形, 46B）
    Hcpe3,
    /// dlshogi test_data 用の hcpe（38B）
    Hcpe,
}

#[derive(Parser)]
#[command(
    name = "psv_to_hcpe3",
    version,
    about = "PackedSfenValue を dlshogi 学習用 hcpe3 / hcpe に変換"
)]
struct Cli {
    /// 入力 PSV ファイル
    #[arg(short, long)]
    input: PathBuf,

    /// 出力ファイル（hcpe3 または hcpe）
    #[arg(short, long)]
    output: PathBuf,

    /// 出力形式
    #[arg(long, value_enum, default_value_t = Format::Hcpe3)]
    format: Format,

    /// 処理するレコード数の上限（0=無制限）
    #[arg(long, default_value_t = 0)]
    limit: usize,

    /// スレッド数（0=全コア）
    #[arg(long, default_value_t = 0)]
    threads: usize,

    /// チャンクサイズ（レコード数）。ピークメモリはこの値に比例し、入力件数に依存しない。
    #[arg(long, default_value_t = 200_000)]
    chunk: usize,

    /// 詳細出力（変換できなかったレコードを逐次ログ）
    #[arg(short, long)]
    verbose: bool,
}

/// 1 レコードの変換結果。出力は `data[..len]`。
enum ConvResult {
    Record { data: [u8; RECORD_BUF], len: usize },
    Error(String),
}

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// game_result（手番側視点の ±1/0）を cshogi gameResult（0:DRAW / 1:BLACK_WIN / 2:WHITE_WIN）へ。
///
/// 手番側勝ち(`1`)なら勝者 = 手番側、手番側負け(`-1`)なら勝者 = 相手側になる。
/// cshogi の `to_hcp` 系と同じく BLACK=0 / WHITE=1 を `+1` した値が勝者の色を表す。
#[inline]
fn game_result_byte(game_result: i8, side_to_move: Color) -> u8 {
    let stm = side_to_move.index() as u8;
    match game_result {
        1 => stm + 1,
        -1 => 2 - stm,
        _ => 0,
    }
}

/// PSV の YaneuraOu move16 を cshogi move16 へ（cshogi `move16_from_psv` と同一）。
///
/// 盤上マス番号・駒打ち表現（`from = 81 + 駒種`）は両者同一で、成り（YaneuraOu の bit14）
/// だけ cshogi 側で `0x1800` ずれる。bit15 を含めない全有効入力で cshogi と一致を確認済み。
#[inline]
fn cshogi_move16_from_psv(yo_move16: u16) -> u16 {
    if yo_move16 & 0x4000 != 0 {
        yo_move16.wrapping_sub(0x1800)
    } else {
        yo_move16
    }
}

fn build_hcpe3(psv: &PackedSfenValue, hcp: &[u8; 32], move16: u16, result: u8) -> ConvResult {
    let mut data = [0u8; RECORD_BUF];
    data[0..32].copy_from_slice(hcp);
    data[32..34].copy_from_slice(&1u16.to_le_bytes()); // moveNum
    data[34] = result;
    data[35] = 0; // opponent
    data[36..38].copy_from_slice(&move16.to_le_bytes()); // selectedMove16
    data[38..40].copy_from_slice(&psv.score.to_le_bytes()); // eval
    data[40..42].copy_from_slice(&1u16.to_le_bytes()); // candidateNum
    data[42..44].copy_from_slice(&move16.to_le_bytes()); // move16（= selectedMove16）
    data[44..46].copy_from_slice(&1u16.to_le_bytes()); // visitNum
    ConvResult::Record {
        data,
        len: HCPE3_SIZE,
    }
}

fn build_hcpe(psv: &PackedSfenValue, hcp: &[u8; 32], move16: u16, result: u8) -> ConvResult {
    let mut data = [0u8; RECORD_BUF];
    data[0..32].copy_from_slice(hcp);
    data[32..34].copy_from_slice(&psv.score.to_le_bytes()); // eval
    data[34..36].copy_from_slice(&move16.to_le_bytes()); // bestMove16
    data[36] = result; // gameResult
    data[37] = 0; // dummy
    ConvResult::Record {
        data,
        len: HCPE_SIZE,
    }
}

fn convert(record: &[u8; PackedSfenValue::SIZE], format: Format) -> ConvResult {
    let psv = match PackedSfenValue::from_bytes(record) {
        Some(v) => v,
        None => return ConvResult::Error("failed to parse PackedSfenValue".to_string()),
    };
    let sfen = match unpack_sfen(&psv.sfen) {
        Ok(s) => s,
        Err(e) => return ConvResult::Error(format!("failed to unpack SFEN: {e}")),
    };
    let mut pos = Position::new();
    if let Err(e) = pos.set_sfen(&sfen) {
        return ConvResult::Error(format!("failed to set SFEN: {e}"));
    }

    let hcp = pack_position_hcp(&pos);
    let move16 = cshogi_move16_from_psv(psv.move16);
    let result = game_result_byte(psv.game_result, pos.side_to_move());

    match format {
        Format::Hcpe3 => build_hcpe3(&psv, &hcp, move16, result),
        Format::Hcpe => build_hcpe(&psv, &hcp, move16, result),
    }
}

/// 並列処理結果を入力順のまま書き出す。戻り値: (書き出し件数, エラー件数)。
fn write_results(
    results: &[ConvResult],
    writer: &mut BufWriter<File>,
    verbose: bool,
) -> Result<(u64, u64)> {
    let mut written = 0u64;
    let mut errors = 0u64;
    for result in results {
        match result {
            ConvResult::Record { data, len } => {
                writer.write_all(&data[..*len])?;
                written += 1;
            }
            ConvResult::Error(e) => {
                errors += 1;
                if verbose {
                    eprintln!("Error converting record: {e}");
                }
            }
        }
    }
    Ok((written, errors))
}

fn main() -> Result<()> {
    env_logger::init();
    let cli = Cli::parse();

    if !cli.input.exists() {
        anyhow::bail!("Input file not found: {}", cli.input.display());
    }
    if cli.chunk == 0 {
        anyhow::bail!("--chunk must be greater than 0");
    }

    // 入出力が同一パスならデータ消失を防ぐためエラーにする
    let in_canonical = cli
        .input
        .canonicalize()
        .with_context(|| format!("Failed to canonicalize input path: {}", cli.input.display()))?;
    if cli.output.exists() {
        let out_canonical = cli.output.canonicalize().with_context(|| {
            format!("Failed to canonicalize output path: {}", cli.output.display())
        })?;
        if in_canonical == out_canonical {
            anyhow::bail!(
                "Input and output paths resolve to the same file: {}",
                in_canonical.display()
            );
        }
    }

    if cli.threads > 0 {
        rayon::ThreadPoolBuilder::new()
            .num_threads(cli.threads)
            .build_global()
            .context("Failed to build rayon thread pool")?;
    }

    ctrlc::set_handler(|| {
        eprintln!("\nInterrupted, finishing current chunk...");
        INTERRUPTED.store(true, Ordering::SeqCst);
    })
    .context("Failed to set Ctrl-C handler")?;

    let file_size = std::fs::metadata(&cli.input)?.len();
    let estimated_records = file_size / PackedSfenValue::SIZE as u64;
    let total = if cli.limit > 0 {
        estimated_records.min(cli.limit as u64)
    } else {
        estimated_records
    };
    eprintln!(
        "Input: {} ({} bytes, ~{} records), format={:?}",
        cli.input.display(),
        file_size,
        estimated_records,
        cli.format
    );

    let progress = ProgressBar::new(total);
    progress.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} ({per_sec}) ETA: {eta}")
            .expect("valid template"),
    );

    let in_file = File::open(&cli.input)
        .with_context(|| format!("Failed to open {}", cli.input.display()))?;
    let mut reader = BufReader::with_capacity(IO_BUF_SIZE, in_file);

    // 一時ファイルに書き、正常完了時のみ最終パスへ rename する（中断時の破損出力を防ぐ）。
    // `--output foo.tmp` でも最終パスと衝突しないよう、拡張子置換ではなくサフィックス付与する。
    let tmp_output = {
        let mut s = cli.output.clone().into_os_string();
        s.push(".partial");
        PathBuf::from(s)
    };
    let out_file = File::create(&tmp_output)
        .with_context(|| format!("Failed to create {}", tmp_output.display()))?;
    let mut writer = BufWriter::with_capacity(IO_BUF_SIZE, out_file);

    let format = cli.format;
    let verbose = cli.verbose;
    let limit = cli.limit;
    let mut remaining = if limit > 0 { limit } else { usize::MAX };
    let mut chunk: Vec<[u8; PackedSfenValue::SIZE]> = Vec::with_capacity(cli.chunk);
    let mut buffer = [0u8; PackedSfenValue::SIZE];
    let mut total_written = 0u64;
    let mut total_errors = 0u64;
    let mut interrupted = false;
    let mut reached_eof = false;

    while remaining > 0 {
        if INTERRUPTED.load(Ordering::Acquire) {
            interrupted = true;
            break;
        }

        chunk.clear();
        let chunk_target = remaining.min(cli.chunk);
        for _ in 0..chunk_target {
            match reader.read_exact(&mut buffer) {
                Ok(()) => chunk.push(buffer),
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    reached_eof = true;
                    break;
                }
                Err(e) => return Err(e.into()),
            }
        }
        if chunk.is_empty() {
            break;
        }
        remaining -= chunk.len();

        let results: Vec<ConvResult> =
            chunk.par_iter().map(|record| convert(record, format)).collect();

        let (written, errors) = write_results(&results, &mut writer, verbose)?;
        total_written += written;
        total_errors += errors;
        progress.inc(results.len() as u64);
    }

    writer.flush()?;
    drop(writer);

    if interrupted {
        progress.abandon_with_message("Interrupted");
        // 中断時は不完全な一時ファイルを削除する
        let _ = std::fs::remove_file(&tmp_output);
        eprintln!("Interrupted before completion; output not written.");
        return Ok(());
    }

    // EOF まで読み切った場合、末尾の半端なバイト（レコード長未満）は破損とみなしてカウントする。
    // `--limit` で途中終了したときは末尾まで到達していないため対象外。
    let trailing_bytes = file_size % PackedSfenValue::SIZE as u64;
    if reached_eof && trailing_bytes != 0 {
        total_errors += 1;
        if verbose {
            eprintln!("Error: {trailing_bytes} trailing bytes are not a full PSV record");
        }
    }

    std::fs::rename(&tmp_output, &cli.output).with_context(|| {
        format!("Failed to rename {} -> {}", tmp_output.display(), cli.output.display())
    })?;
    progress.set_length(total_written + total_errors);
    progress.finish();

    eprintln!("Written: {} records ({:?}) -> {}", total_written, format, cli.output.display());
    if total_errors > 0 {
        eprintln!("Note: {total_errors} records failed to convert and were skipped");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn game_result_mapping_matches_oracle() {
        // 手番側勝ち(1): 勝者 = 手番側
        assert_eq!(game_result_byte(1, Color::Black), 1); // BLACK_WIN
        assert_eq!(game_result_byte(1, Color::White), 2); // WHITE_WIN
        // 手番側負け(-1): 勝者 = 相手側
        assert_eq!(game_result_byte(-1, Color::Black), 2); // WHITE_WIN
        assert_eq!(game_result_byte(-1, Color::White), 1); // BLACK_WIN
        // 引き分け(0): DRAW
        assert_eq!(game_result_byte(0, Color::Black), 0);
        assert_eq!(game_result_byte(0, Color::White), 0);
    }

    #[test]
    fn cshogi_move16_matches_oracle() {
        // none / 通常手 / 駒打ちは無変換、成りのみ 0x1800 ずれる（cshogi move16_from_psv 準拠）。
        assert_eq!(cshogi_move16_from_psv(0x0000), 0x0000); // none
        assert_eq!(cshogi_move16_from_psv(0x162b), 0x162b); // 通常手
        assert_eq!(cshogi_move16_from_psv(0x2917), 0x2917); // 歩打ち（from=82=81+1）
        assert_eq!(cshogi_move16_from_psv(0x4b2a), 0x332a); // 成り
        assert_eq!(cshogi_move16_from_psv(0x62bb), 0x4abb); // 成り
    }
}
