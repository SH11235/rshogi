//! merge_psv - 複数の PSV ファイルを順序どおり結合
//!
//! PackedSfenValue 形式（40 バイト/レコード）の複数ファイルを順に連結して
//! 1 つの出力ファイルへまとめる。入力全体をメモリへ載せず、
//! ストリーミングで少しずつ書き出す。
//!
//! # 使用例
//!
//! ```bash
//! # 明示した順序で結合
//! cargo run -p tools --release --bin merge_psv -- \
//!   --input data_000.bin,data_001.bin,data_002.bin \
//!   --output merged.psv
//!
//! # ディレクトリから glob で拾って名前順に結合
//! cargo run -p tools --release --bin merge_psv -- \
//!   --input-dir split \
//!   --pattern "train_*.bin" \
//!   --output merged.psv
//!
//! # 20 万局面ずつ読み書きしてメモリ使用量を抑える
//! cargo run -p tools --release --bin merge_psv -- \
//!   --input-dir split \
//!   --pattern "train_*.bin" \
//!   --output merged.psv \
//!   --write-chunk-records 200000
//! ```

use anyhow::{Context, Result, bail};
use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use log::{info, warn};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use tools::common::dedup::{PSV_SIZE, check_output_not_in_inputs, collect_input_paths};

const IO_BUF_SIZE: usize = 8 * 1024 * 1024;
const DEFAULT_WRITE_CHUNK_RECORDS: usize = 1_000_000;
const MAX_WRITE_CHUNK_BYTES: usize = 512 * 1024 * 1024;
const MAX_WRITE_CHUNK_RECORDS: usize = MAX_WRITE_CHUNK_BYTES / PSV_SIZE;

#[derive(Parser, Debug)]
#[command(
    name = "merge_psv",
    version,
    about = "複数の PSV ファイルを順序どおり結合",
    long_about = "PackedSfenValue 形式（40 バイト/レコード）の複数ファイルを、\
入力順のまま 1 つの出力ファイルへ結合します。\
入出力はストリーミングで行うため、大きなファイルでも少しずつ書き出せます。"
)]
struct Cli {
    /// 入力 PSV ファイル（カンマ区切りで複数指定可）
    #[arg(long, conflicts_with = "input_dir")]
    input: Option<String>,

    /// 入力ディレクトリ（--input と排他）
    #[arg(long, conflicts_with = "input")]
    input_dir: Option<PathBuf>,

    /// --input-dir 使用時の glob パターン
    #[arg(long, default_value = "*.bin")]
    pattern: String,

    /// 出力 PSV ファイル
    #[arg(short, long)]
    output: PathBuf,

    /// 1 回の読み書きで扱う局面数
    #[arg(long, default_value_t = DEFAULT_WRITE_CHUNK_RECORDS)]
    write_chunk_records: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MergeConfig {
    write_chunk_records: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MergeStats {
    input_count: usize,
    total_records: u64,
}

fn main() -> Result<()> {
    env_logger::init();
    let cli = Cli::parse();

    let inputs = collect_input_paths(cli.input.as_deref(), cli.input_dir.as_ref(), &cli.pattern)
        .context("入力ファイル一覧の収集に失敗しました")?;
    if inputs.is_empty() {
        bail!("入力ファイルが 0 件です");
    }
    check_output_not_in_inputs(&cli.output, &inputs)
        .context("出力パスが入力に含まれていないかの検証に失敗しました")?;

    let stats = merge_files(
        &inputs,
        &cli.output,
        &MergeConfig {
            write_chunk_records: cli.write_chunk_records,
        },
    )?;

    info!("入力ファイル数: {}", stats.input_count);
    info!("結合局面数: {}", stats.total_records);
    info!("出力: {}", cli.output.display());

    Ok(())
}

fn merge_files(inputs: &[PathBuf], output_path: &Path, config: &MergeConfig) -> Result<MergeStats> {
    if inputs.is_empty() {
        bail!("入力ファイルが 0 件です");
    }
    if config.write_chunk_records == 0 {
        bail!("--write-chunk-records は 1 以上を指定してください");
    }
    validate_write_chunk_records(config.write_chunk_records)?;

    ensure_parent_dir(output_path)?;

    let mut total_records = 0u64;
    for input in inputs {
        let size = std::fs::metadata(input)
            .with_context(|| format!("入力ファイル情報の取得に失敗しました: {}", input.display()))?
            .len();
        let records = size / PSV_SIZE as u64;
        let trailing = size % PSV_SIZE as u64;
        if trailing != 0 {
            warn!(
                "{} の末尾 {} バイトは完全なレコードではないため無視します",
                input.display(),
                trailing
            );
        }
        total_records += records;
    }

    let out_file = File::create(output_path).with_context(|| {
        format!("出力ファイルを作成できませんでした: {}", output_path.display())
    })?;
    let mut writer = BufWriter::with_capacity(IO_BUF_SIZE, out_file);
    let buffer_len = config
        .write_chunk_records
        .checked_mul(PSV_SIZE)
        .context("書き出しチャンクが大きすぎます")?;
    let mut buffer = vec![0u8; buffer_len];

    let progress = ProgressBar::new(total_records);
    progress.set_style(progress_style("Merging"));

    let mut written_records = 0u64;
    for input in inputs {
        let size = std::fs::metadata(input)
            .with_context(|| format!("入力ファイル情報の取得に失敗しました: {}", input.display()))?
            .len();
        let mut remaining = size / PSV_SIZE as u64;
        info!("入力: {} ({} records)", input.display(), remaining);

        let in_file = File::open(input)
            .with_context(|| format!("入力ファイルを開けませんでした: {}", input.display()))?;
        let mut reader = BufReader::with_capacity(IO_BUF_SIZE, in_file);

        while remaining > 0 {
            let to_read_records = remaining.min(config.write_chunk_records as u64) as usize;
            let byte_len =
                to_read_records.checked_mul(PSV_SIZE).context("読み込みサイズが大きすぎます")?;
            reader.read_exact(&mut buffer[..byte_len]).with_context(|| {
                format!("入力ファイル読み込み中に失敗しました: {}", input.display())
            })?;
            writer.write_all(&buffer[..byte_len]).with_context(|| {
                format!("出力ファイル書き込み中に失敗しました: {}", output_path.display())
            })?;
            remaining -= to_read_records as u64;
            written_records += to_read_records as u64;
            progress.inc(to_read_records as u64);
        }
    }

    writer
        .flush()
        .with_context(|| format!("出力ファイル flush に失敗しました: {}", output_path.display()))?;
    progress.finish_and_clear();

    Ok(MergeStats {
        input_count: inputs.len(),
        total_records: written_records,
    })
}

fn ensure_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).with_context(|| {
            format!("親ディレクトリを作成できませんでした: {}", parent.display())
        })?;
    }
    Ok(())
}

fn validate_write_chunk_records(write_chunk_records: usize) -> Result<()> {
    if write_chunk_records > MAX_WRITE_CHUNK_RECORDS {
        bail!(
            "--write-chunk-records={} は大きすぎます。最大値は {} records ({} bytes) です",
            write_chunk_records,
            MAX_WRITE_CHUNK_RECORDS,
            MAX_WRITE_CHUNK_BYTES,
        );
    }
    Ok(())
}

fn progress_style(label: &str) -> ProgressStyle {
    ProgressStyle::default_bar()
        .template(&format!(
            "[{{elapsed_precise}}] {{bar:40.cyan/blue}} {{pos}}/{{len}} ({{per_sec}}) {label}"
        ))
        .expect("valid template")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn make_records(start: usize, count: usize) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(count * PSV_SIZE);
        for i in start..start + count {
            let mut record = [0u8; PSV_SIZE];
            record[0..8].copy_from_slice(&(i as u64).to_le_bytes());
            record[32..34].copy_from_slice(&(i as i16).to_le_bytes());
            record[36..38].copy_from_slice(&(i as u16).to_le_bytes());
            bytes.extend_from_slice(&record);
        }
        bytes
    }

    #[test]
    fn merge_files_keeps_input_order() {
        let dir = tempdir().unwrap();
        let input0 = dir.path().join("part_000.bin");
        let input1 = dir.path().join("part_001.bin");
        let output = dir.path().join("merged/out.psv");

        let chunk0 = make_records(0, 5);
        let chunk1 = make_records(5, 7);
        fs::write(&input0, &chunk0).unwrap();
        fs::write(&input1, &chunk1).unwrap();

        let stats = merge_files(
            &[input0.clone(), input1.clone()],
            &output,
            &MergeConfig {
                write_chunk_records: 3,
            },
        )
        .unwrap();

        assert_eq!(
            stats,
            MergeStats {
                input_count: 2,
                total_records: 12
            }
        );

        let merged = fs::read(output).unwrap();
        let mut expected = Vec::new();
        expected.extend_from_slice(&chunk0);
        expected.extend_from_slice(&chunk1);
        assert_eq!(merged, expected);
    }

    #[test]
    fn validate_write_chunk_records_rejects_huge_value() {
        assert!(validate_write_chunk_records(MAX_WRITE_CHUNK_RECORDS + 1).is_err());
    }
}
