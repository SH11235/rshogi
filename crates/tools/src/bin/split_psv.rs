//! split_psv - PSV ファイルを複数ファイルへ分割
//!
//! PackedSfenValue 形式（40 バイト/レコード）の PSV ファイルを、
//! 1 ファイルあたりの局面数または容量を指定して分割する。
//! 入力全体をメモリへ載せず、ストリーミングで少しずつ書き出す。
//!
//! # 使用例
//!
//! ```bash
//! # 1 ファイル 1 億局面で分割
//! cargo run -p tools --release --bin split_psv -- \
//!   --input data.psv \
//!   --output-prefix out/train \
//!   --records-per-file 100000000
//!
//! # 1 ファイル 4GB 目安で分割
//! cargo run -p tools --release --bin split_psv -- \
//!   --input data.psv \
//!   --output-prefix out/train \
//!   --bytes-per-file 4GB
//!
//! # 40 万局面ずつ読み書きしてメモリ使用量を抑える
//! cargo run -p tools --release --bin split_psv -- \
//!   --input data.psv \
//!   --output-prefix out/train \
//!   --records-per-file 100000000 \
//!   --write-chunk-records 400000
//! ```

use anyhow::{Context, Result, bail};
use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use log::{info, warn};
use std::ffi::OsString;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use tools::packed_sfen::PackedSfenValue;

const RECORD_SIZE: usize = PackedSfenValue::SIZE;
const IO_BUF_SIZE: usize = 8 * 1024 * 1024;
const DEFAULT_WRITE_CHUNK_RECORDS: usize = 1_000_000;

#[derive(Parser, Debug)]
#[command(
    name = "split_psv",
    version,
    about = "PSV ファイルを複数ファイルへ分割",
    long_about = "PackedSfenValue 形式（40 バイト/レコード）の PSV ファイルを、\
1 ファイルあたりの局面数または容量で分割して出力します。\
入出力はストリーミングで行うため、大きなファイルでも少しずつ書き出せます。"
)]
struct Cli {
    /// 入力 PSV ファイル
    #[arg(short, long)]
    input: PathBuf,

    /// 出力プレフィックス（例: out/train -> out/train_000.bin）
    #[arg(long)]
    output_prefix: PathBuf,

    /// 1 ファイルあたりの局面数
    #[arg(long, conflicts_with = "bytes_per_file")]
    records_per_file: Option<u64>,

    /// 1 ファイルあたりの容量（例: 4GB, 3500MiB, 4000000000）
    #[arg(long, conflicts_with = "records_per_file")]
    bytes_per_file: Option<String>,

    /// 1 回の読み書きで扱う局面数
    #[arg(long, default_value_t = DEFAULT_WRITE_CHUNK_RECORDS)]
    write_chunk_records: usize,

    /// 出力ファイルの開始インデックス
    #[arg(long, default_value_t = 0)]
    start_index: u64,

    /// 出力ファイル番号の最小桁数
    #[arg(long, default_value_t = 3)]
    digits: usize,

    /// 出力ファイル拡張子
    #[arg(long, default_value = ".bin")]
    suffix: String,
}

#[derive(Debug, Clone)]
struct SplitConfig {
    records_per_file: u64,
    write_chunk_records: usize,
    start_index: u64,
    digits: usize,
    suffix: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SplitStats {
    total_records: u64,
    part_count: u64,
}

fn main() -> Result<()> {
    env_logger::init();
    let cli = Cli::parse();

    if !cli.input.exists() {
        bail!("入力ファイルが存在しません: {}", cli.input.display());
    }

    let records_per_file = resolve_records_per_file(&cli)?;
    let config = SplitConfig {
        records_per_file,
        write_chunk_records: cli.write_chunk_records,
        start_index: cli.start_index,
        digits: cli.digits.max(1),
        suffix: cli.suffix,
    };

    let stats = split_file(&cli.input, &cli.output_prefix, &config)?;

    info!("総局面数: {}", stats.total_records);
    info!("出力ファイル数: {}", stats.part_count);
    if stats.part_count > 0 {
        let last_index = cli.start_index + stats.part_count - 1;
        info!(
            "出力範囲: {}_{}..{}{}",
            cli.output_prefix.display(),
            zero_pad(cli.start_index, config.digits),
            zero_pad(last_index, config.digits),
            config.suffix,
        );
    }

    Ok(())
}

fn split_file(input_path: &Path, output_prefix: &Path, config: &SplitConfig) -> Result<SplitStats> {
    if config.records_per_file == 0 {
        bail!(
            "--records-per-file / --bytes-per-file から算出される局面数は 1 以上である必要があります"
        );
    }
    if config.write_chunk_records == 0 {
        bail!("--write-chunk-records は 1 以上を指定してください");
    }

    ensure_parent_dir(output_prefix)?;

    let file_size = std::fs::metadata(input_path)
        .with_context(|| format!("入力ファイル情報の取得に失敗しました: {}", input_path.display()))?
        .len();
    let total_records = file_size / RECORD_SIZE as u64;
    let trailing_bytes = file_size % RECORD_SIZE as u64;
    if trailing_bytes != 0 {
        warn!(
            "入力ファイル末尾の {} バイトは完全なレコードではないため無視します",
            trailing_bytes
        );
    }

    info!(
        "入力: {} ({} bytes, {} records)",
        input_path.display(),
        file_size,
        total_records
    );
    info!("分割単位: {} records/file", config.records_per_file);
    info!("書き出しチャンク: {} records", config.write_chunk_records);

    if total_records == 0 {
        warn!("完全なレコードが 0 件のため、出力ファイルは作成しません");
        return Ok(SplitStats {
            total_records: 0,
            part_count: 0,
        });
    }

    let file = File::open(input_path)
        .with_context(|| format!("入力ファイルを開けませんでした: {}", input_path.display()))?;
    let mut reader = BufReader::with_capacity(IO_BUF_SIZE, file);

    let chunk_records = config.write_chunk_records.min(config.records_per_file as usize).max(1);
    let buffer_len = chunk_records
        .checked_mul(RECORD_SIZE)
        .context("書き出しチャンクが大きすぎます")?;
    let mut buffer = vec![0u8; buffer_len];

    let progress = ProgressBar::new(total_records);
    progress.set_style(progress_style("Splitting"));

    let mut remaining = total_records;
    let mut part_index = config.start_index;
    let mut part_count = 0u64;

    while remaining > 0 {
        let output_path = build_part_path(output_prefix, part_index, config.digits, &config.suffix);
        ensure_parent_dir(&output_path)?;

        let out_file = File::create(&output_path).with_context(|| {
            format!("出力ファイルを作成できませんでした: {}", output_path.display())
        })?;
        let mut writer = BufWriter::with_capacity(IO_BUF_SIZE, out_file);

        let mut written_in_part = 0u64;
        while written_in_part < config.records_per_file && remaining > 0 {
            let to_read_records = remaining
                .min(config.records_per_file - written_in_part)
                .min(chunk_records as u64) as usize;
            let byte_len = to_read_records
                .checked_mul(RECORD_SIZE)
                .context("読み込みサイズが大きすぎます")?;
            reader.read_exact(&mut buffer[..byte_len]).with_context(|| {
                format!("入力ファイル読み込み中に失敗しました: {}", input_path.display())
            })?;
            writer.write_all(&buffer[..byte_len]).with_context(|| {
                format!("出力ファイル書き込み中に失敗しました: {}", output_path.display())
            })?;
            written_in_part += to_read_records as u64;
            remaining -= to_read_records as u64;
            progress.inc(to_read_records as u64);
        }

        writer.flush().with_context(|| {
            format!("出力ファイル flush に失敗しました: {}", output_path.display())
        })?;
        info!("part {}: {} ({} records)", part_index, output_path.display(), written_in_part);

        part_index += 1;
        part_count += 1;
    }

    progress.finish_and_clear();

    Ok(SplitStats {
        total_records,
        part_count,
    })
}

fn resolve_records_per_file(cli: &Cli) -> Result<u64> {
    match (&cli.records_per_file, &cli.bytes_per_file) {
        (Some(records), None) => {
            if *records == 0 {
                bail!("--records-per-file は 1 以上を指定してください");
            }
            Ok(*records)
        }
        (None, Some(bytes_str)) => {
            let bytes = parse_byte_size(bytes_str)?;
            if bytes < RECORD_SIZE as u64 {
                bail!("--bytes-per-file は少なくとも {} bytes 以上を指定してください", RECORD_SIZE);
            }
            let records = bytes / RECORD_SIZE as u64;
            let aligned_bytes = records * RECORD_SIZE as u64;
            if aligned_bytes != bytes {
                warn!(
                    "--bytes-per-file={} はレコード境界に合わないため、{} bytes ({} records) に切り下げます",
                    bytes_str, aligned_bytes, records
                );
            }
            Ok(records)
        }
        (Some(_), Some(_)) => {
            bail!("--records-per-file と --bytes-per-file は同時に指定できません")
        }
        (None, None) => {
            bail!("--records-per-file または --bytes-per-file のいずれかを指定してください")
        }
    }
}

fn parse_byte_size(input: &str) -> Result<u64> {
    let normalized = input.trim().replace('_', "").to_ascii_lowercase();
    if normalized.is_empty() {
        bail!("容量指定が空です");
    }

    let split_at = normalized.find(|c: char| !c.is_ascii_digit()).unwrap_or(normalized.len());
    let (value_str, suffix) = normalized.split_at(split_at);
    if value_str.is_empty() {
        bail!("容量の数値部分を解釈できません: {input}");
    }
    let value: u64 = value_str
        .parse()
        .with_context(|| format!("容量の数値部分を解釈できません: {input}"))?;

    let multiplier = match suffix {
        "" | "b" => 1u64,
        "k" | "kb" => 1_000u64,
        "m" | "mb" => 1_000_000u64,
        "g" | "gb" => 1_000_000_000u64,
        "t" | "tb" => 1_000_000_000_000u64,
        "ki" | "kib" => 1024u64,
        "mi" | "mib" => 1024u64.pow(2),
        "gi" | "gib" => 1024u64.pow(3),
        "ti" | "tib" => 1024u64.pow(4),
        _ => bail!("未対応の容量単位です: {input}"),
    };

    value
        .checked_mul(multiplier)
        .with_context(|| format!("容量が大きすぎます: {input}"))
}

fn build_part_path(prefix: &Path, index: u64, digits: usize, suffix: &str) -> PathBuf {
    let mut path = OsString::from(prefix.as_os_str());
    path.push(format!("_{}{suffix}", zero_pad(index, digits)));
    PathBuf::from(path)
}

fn zero_pad(value: u64, digits: usize) -> String {
    format!("{value:0digits$}")
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

    fn make_records(count: usize) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(count * RECORD_SIZE);
        for i in 0..count {
            let mut record = [0u8; RECORD_SIZE];
            record[0..8].copy_from_slice(&(i as u64).to_le_bytes());
            record[32..34].copy_from_slice(&(i as i16).to_le_bytes());
            record[36..38].copy_from_slice(&(i as u16).to_le_bytes());
            bytes.extend_from_slice(&record);
        }
        bytes
    }

    #[test]
    fn parse_byte_size_supports_decimal_and_binary_units() {
        assert_eq!(parse_byte_size("4000").unwrap(), 4000);
        assert_eq!(parse_byte_size("4GB").unwrap(), 4_000_000_000);
        assert_eq!(parse_byte_size("4GiB").unwrap(), 4 * 1024 * 1024 * 1024);
        assert!(parse_byte_size("12XB").is_err());
    }

    #[test]
    fn split_file_streams_into_multiple_outputs() {
        let dir = tempdir().unwrap();
        let input_path = dir.path().join("input.psv");
        let output_prefix = dir.path().join("split/train");
        let original = make_records(23);
        fs::write(&input_path, &original).unwrap();

        let stats = split_file(
            &input_path,
            &output_prefix,
            &SplitConfig {
                records_per_file: 10,
                write_chunk_records: 3,
                start_index: 7,
                digits: 3,
                suffix: ".bin".to_string(),
            },
        )
        .unwrap();

        assert_eq!(
            stats,
            SplitStats {
                total_records: 23,
                part_count: 3
            }
        );

        let part0 = fs::read(dir.path().join("split/train_007.bin")).unwrap();
        let part1 = fs::read(dir.path().join("split/train_008.bin")).unwrap();
        let part2 = fs::read(dir.path().join("split/train_009.bin")).unwrap();
        assert_eq!(part0.len(), 10 * RECORD_SIZE);
        assert_eq!(part1.len(), 10 * RECORD_SIZE);
        assert_eq!(part2.len(), 3 * RECORD_SIZE);

        let mut merged = Vec::new();
        merged.extend_from_slice(&part0);
        merged.extend_from_slice(&part1);
        merged.extend_from_slice(&part2);
        assert_eq!(merged, original);
    }
}
