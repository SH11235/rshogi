//! スコア訂正ツール
//!
//! 前処理で誤って上書きされたスコアを、元ファイルのスコアで訂正する。
//! レコードの対応を確認しながら処理し、統計情報を出力する。

use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

use anyhow::{bail, Result};
use clap::Parser;

/// PackedSfenValueのサイズ（バイト）
const RECORD_SIZE: usize = 40;

/// SFENのオフセット（バイト）
const SFEN_OFFSET: usize = 0;
const SFEN_SIZE: usize = 32;

/// スコアのオフセット（バイト）
const SCORE_OFFSET: usize = 32;
const SCORE_SIZE: usize = 2;

#[derive(Parser, Debug)]
#[command(name = "fix_scores")]
#[command(about = "前処理で上書きされたスコアを元ファイルから訂正する")]
struct Cli {
    /// 元のpackファイル（正しいスコアを持つ）
    #[arg(long)]
    original: PathBuf,

    /// 前処理済みファイル（スコアを訂正する対象）
    #[arg(long)]
    preprocessed: PathBuf,

    /// 出力ファイル（省略時は preprocessed を上書き）
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// 最初のN件のサンプルを表示して確認
    #[arg(long, default_value_t = 5)]
    sample_count: usize,

    /// サンプル確認後に処理を続行するか
    #[arg(long, default_value_t = false)]
    yes: bool,

    /// 進捗表示の間隔（レコード数）
    #[arg(long, default_value_t = 1_000_000)]
    progress_interval: u64,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // ファイルサイズ確認
    let orig_size = std::fs::metadata(&cli.original)?.len();
    let prep_size = std::fs::metadata(&cli.preprocessed)?.len();

    if orig_size != prep_size {
        bail!("ファイルサイズが一致しません: original={orig_size}, preprocessed={prep_size}");
    }

    if orig_size % RECORD_SIZE as u64 != 0 {
        bail!("ファイルサイズが{RECORD_SIZE}の倍数ではありません: {orig_size}");
    }

    let total_records = orig_size / RECORD_SIZE as u64;
    eprintln!("レコード数: {total_records}");
    eprintln!("元ファイル: {}", cli.original.display());
    eprintln!("前処理済み: {}", cli.preprocessed.display());
    eprintln!();

    // サンプル確認
    eprintln!("=== サンプル確認（最初の{}件）===", cli.sample_count);
    let mut orig_file = File::open(&cli.original)?;
    let mut prep_file = File::open(&cli.preprocessed)?;

    let mut orig_record = [0u8; RECORD_SIZE];
    let mut prep_record = [0u8; RECORD_SIZE];

    for i in 0..cli.sample_count.min(total_records as usize) {
        orig_file.read_exact(&mut orig_record)?;
        prep_file.read_exact(&mut prep_record)?;

        let orig_sfen = &orig_record[SFEN_OFFSET..SFEN_OFFSET + SFEN_SIZE];
        let prep_sfen = &prep_record[SFEN_OFFSET..SFEN_OFFSET + SFEN_SIZE];
        let sfen_match = orig_sfen == prep_sfen;

        let orig_score =
            i16::from_le_bytes([orig_record[SCORE_OFFSET], orig_record[SCORE_OFFSET + 1]]);
        let prep_score =
            i16::from_le_bytes([prep_record[SCORE_OFFSET], prep_record[SCORE_OFFSET + 1]]);

        eprintln!(
            "レコード{}: SFEN {} | スコア: {} → {} (差: {})",
            i + 1,
            if sfen_match { "一致" } else { "変更" },
            orig_score,
            prep_score,
            prep_score - orig_score
        );
    }
    eprintln!();

    if !cli.yes {
        eprintln!("続行するには --yes オプションを付けて再実行してください");
        return Ok(());
    }

    // 出力先決定
    let output_path = cli.output.unwrap_or_else(|| cli.preprocessed.clone());
    let in_place = output_path == cli.preprocessed;

    if in_place {
        eprintln!("インプレース更新: {}", cli.preprocessed.display());
    } else {
        eprintln!("出力: {}", output_path.display());
    }

    // ファイル再オープン
    let orig_file = File::open(&cli.original)?;
    let mut orig_reader = BufReader::with_capacity(1024 * 1024, orig_file);

    let mut output_writer: BufWriter<File>;
    let mut prep_reader: Option<BufReader<File>> = None;

    if in_place {
        let file = OpenOptions::new().read(true).write(true).open(&cli.preprocessed)?;
        output_writer = BufWriter::with_capacity(1024 * 1024, file);
    } else {
        let prep_file = File::open(&cli.preprocessed)?;
        prep_reader = Some(BufReader::with_capacity(1024 * 1024, prep_file));
        let out_file = File::create(&output_path)?;
        output_writer = BufWriter::with_capacity(1024 * 1024, out_file);
    }

    // 統計
    let mut sfen_same_count: u64 = 0;
    let mut sfen_diff_count: u64 = 0;
    let mut score_changed_count: u64 = 0;
    let mut processed: u64 = 0;

    let start = std::time::Instant::now();

    loop {
        match orig_reader.read_exact(&mut orig_record) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }

        if in_place {
            // インプレース更新
            let writer_inner = output_writer.get_mut();

            // 現在位置から前処理済みレコードを読む
            let pos = processed * RECORD_SIZE as u64;
            writer_inner.seek(SeekFrom::Start(pos))?;
            writer_inner.read_exact(&mut prep_record)?;

            // 統計更新
            let orig_sfen = &orig_record[SFEN_OFFSET..SFEN_OFFSET + SFEN_SIZE];
            let prep_sfen = &prep_record[SFEN_OFFSET..SFEN_OFFSET + SFEN_SIZE];
            if orig_sfen == prep_sfen {
                sfen_same_count += 1;
            } else {
                sfen_diff_count += 1;
            }

            let orig_score = &orig_record[SCORE_OFFSET..SCORE_OFFSET + SCORE_SIZE];
            let prep_score = &prep_record[SCORE_OFFSET..SCORE_OFFSET + SCORE_SIZE];
            if orig_score != prep_score {
                score_changed_count += 1;
            }

            // スコアを上書き
            let score_pos = pos + SCORE_OFFSET as u64;
            writer_inner.seek(SeekFrom::Start(score_pos))?;
            writer_inner.write_all(&orig_record[SCORE_OFFSET..SCORE_OFFSET + SCORE_SIZE])?;
        } else {
            // 別ファイル出力
            let reader = prep_reader.as_mut().unwrap();
            reader.read_exact(&mut prep_record)?;

            // 統計更新
            let orig_sfen = &orig_record[SFEN_OFFSET..SFEN_OFFSET + SFEN_SIZE];
            let prep_sfen = &prep_record[SFEN_OFFSET..SFEN_OFFSET + SFEN_SIZE];
            if orig_sfen == prep_sfen {
                sfen_same_count += 1;
            } else {
                sfen_diff_count += 1;
            }

            let orig_score = &orig_record[SCORE_OFFSET..SCORE_OFFSET + SCORE_SIZE];
            let prep_score = &prep_record[SCORE_OFFSET..SCORE_OFFSET + SCORE_SIZE];
            if orig_score != prep_score {
                score_changed_count += 1;
            }

            // スコアを差し替えて出力
            prep_record[SCORE_OFFSET..SCORE_OFFSET + SCORE_SIZE]
                .copy_from_slice(&orig_record[SCORE_OFFSET..SCORE_OFFSET + SCORE_SIZE]);
            output_writer.write_all(&prep_record)?;
        }

        processed += 1;

        if processed.is_multiple_of(cli.progress_interval) {
            let elapsed = start.elapsed().as_secs_f64();
            let rate = processed as f64 / elapsed;
            let remaining = (total_records - processed) as f64 / rate;
            eprintln!(
                "進捗: {processed}/{total_records} ({:.1}%) - {:.0} rec/s - 残り {:.0}秒",
                processed as f64 / total_records as f64 * 100.0,
                rate,
                remaining
            );
        }
    }

    output_writer.flush()?;

    let elapsed = start.elapsed();
    eprintln!();
    eprintln!("=== 完了 ===");
    eprintln!("処理レコード数: {processed}");
    eprintln!(
        "SFEN一致: {sfen_same_count} ({:.1}%)",
        sfen_same_count as f64 / processed as f64 * 100.0
    );
    eprintln!(
        "SFEN変更: {sfen_diff_count} ({:.1}%)",
        sfen_diff_count as f64 / processed as f64 * 100.0
    );
    eprintln!("スコア訂正: {score_changed_count}");
    eprintln!(
        "処理時間: {:.2}秒 ({:.0} records/sec)",
        elapsed.as_secs_f64(),
        processed as f64 / elapsed.as_secs_f64()
    );

    Ok(())
}
