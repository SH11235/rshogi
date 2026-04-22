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
//! # ディレクトリ直下から glob で拾って shard 番号順に結合
//! cargo run -p tools --release --bin merge_psv -- \
//!   --input-dir split \
//!   --pattern "train_*.bin" \
//!   --output merged.psv
//!
//! # 入れ子ディレクトリも明示的に含める
//! cargo run -p tools --release --bin merge_psv -- \
//!   --input-dir split \
//!   --pattern "train_*.bin" \
//!   --recursive \
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

use tools::common::dedup::{PSV_SIZE, check_output_not_in_inputs};

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

    /// --input-dir 配下を再帰的に探索する
    #[arg(long, default_value_t = false)]
    recursive: bool,

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

    let inputs = resolve_input_paths(&cli).context("入力ファイル一覧の収集に失敗しました")?;
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

fn resolve_input_paths(cli: &Cli) -> Result<Vec<PathBuf>> {
    match (&cli.input, &cli.input_dir) {
        (Some(_), Some(_)) => bail!("--input と --input-dir は同時に指定できません"),
        (None, None) => bail!("--input または --input-dir のいずれかを指定してください"),
        (Some(input), None) => parse_explicit_input_paths(input),
        (None, Some(input_dir)) => {
            collect_merge_input_paths(input_dir, &cli.pattern, cli.recursive)
        }
    }
}

fn parse_explicit_input_paths(input: &str) -> Result<Vec<PathBuf>> {
    let paths: Vec<PathBuf> = input.split(',').map(|part| PathBuf::from(part.trim())).collect();
    for path in &paths {
        if !path.exists() {
            bail!("入力ファイルが存在しません: {}", path.display());
        }
    }
    Ok(paths)
}

fn collect_merge_input_paths(dir: &Path, pattern: &str, recursive: bool) -> Result<Vec<PathBuf>> {
    if !dir.is_dir() {
        bail!("入力ディレクトリが存在しません: {}", dir.display());
    }

    let pattern = glob::Pattern::new(pattern)
        .with_context(|| format!("無効な glob パターンです: {pattern}"))?;
    let mut paths = if recursive {
        walkdir::WalkDir::new(dir)
            .into_iter()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| entry.into_path())
            .collect::<Vec<_>>()
    } else {
        std::fs::read_dir(dir)
            .with_context(|| format!("入力ディレクトリを読み取れませんでした: {}", dir.display()))?
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                entry
                    .file_type()
                    .ok()
                    .filter(|file_type| file_type.is_file())
                    .map(|_| entry.path())
            })
            .collect::<Vec<_>>()
    };

    paths.retain(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| pattern.matches(name))
    });
    paths.sort_by(compare_merge_input_paths);

    Ok(paths)
}

fn compare_merge_input_paths(a: &PathBuf, b: &PathBuf) -> std::cmp::Ordering {
    match (a.parent(), b.parent()) {
        (Some(a_parent), Some(b_parent)) if a_parent == b_parent => {
            compare_file_names_with_numeric_suffix(a, b).then_with(|| a.cmp(b))
        }
        _ => a.cmp(b),
    }
}

fn compare_file_names_with_numeric_suffix(a: &Path, b: &Path) -> std::cmp::Ordering {
    let Some(a_key) = trailing_numeric_suffix_key(a) else {
        return a.file_name().cmp(&b.file_name());
    };
    let Some(b_key) = trailing_numeric_suffix_key(b) else {
        return a.file_name().cmp(&b.file_name());
    };

    if a_key.prefix == b_key.prefix && a_key.extension == b_key.extension {
        a_key.number.cmp(&b_key.number).then_with(|| a.file_name().cmp(&b.file_name()))
    } else {
        a.file_name().cmp(&b.file_name())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NumericSuffixKey {
    prefix: String,
    number: u64,
    extension: String,
}

fn trailing_numeric_suffix_key(path: &Path) -> Option<NumericSuffixKey> {
    let stem = path.file_stem()?.to_str()?;
    let digit_start = stem
        .char_indices()
        .rev()
        .take_while(|(_, ch)| ch.is_ascii_digit())
        .last()
        .map(|(index, _)| index)?;
    let digits = &stem[digit_start..];
    let prefix = &stem[..digit_start];
    let extension = path.extension().and_then(|ext| ext.to_str()).unwrap_or_default().to_string();

    Some(NumericSuffixKey {
        prefix: prefix.to_string(),
        number: digits.parse().ok()?,
        extension,
    })
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

    #[test]
    fn collect_merge_input_paths_sorts_numeric_suffix() {
        let dir = tempdir().unwrap();
        for name in [
            "train_099.bin",
            "train_100.bin",
            "train_1000.bin",
            "train_101.bin",
        ] {
            fs::write(dir.path().join(name), []).unwrap();
        }

        let paths = collect_merge_input_paths(dir.path(), "train_*.bin", false).unwrap();
        let names: Vec<_> = paths
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();

        assert_eq!(
            names,
            vec![
                "train_099.bin",
                "train_100.bin",
                "train_101.bin",
                "train_1000.bin"
            ]
        );
    }

    #[test]
    fn collect_merge_input_paths_ignores_nested_dirs_by_default() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("train_000.bin"), []).unwrap();
        let nested = dir.path().join("archive");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("train_001.bin"), []).unwrap();

        let paths = collect_merge_input_paths(dir.path(), "train_*.bin", false).unwrap();
        let names: Vec<_> = paths
            .iter()
            .map(|path| path.strip_prefix(dir.path()).unwrap().to_string_lossy().into_owned())
            .collect();

        assert_eq!(names, vec!["train_000.bin"]);
    }

    #[test]
    fn collect_merge_input_paths_includes_nested_dirs_when_recursive() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("train_000.bin"), []).unwrap();
        let nested = dir.path().join("archive");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("train_001.bin"), []).unwrap();

        let paths = collect_merge_input_paths(dir.path(), "train_*.bin", true).unwrap();
        let names: Vec<_> = paths
            .iter()
            .map(|path| path.strip_prefix(dir.path()).unwrap().to_string_lossy().into_owned())
            .collect();

        assert_eq!(names, vec!["archive/train_001.bin", "train_000.bin"]);
    }
}
