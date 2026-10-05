//! LSB-first bitmap mask で PSV の行を入力順に抽出する。

use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use tools::mask_io::{checked_input_records, partial_path, validate_chunk_records, validate_mask};
use tools::output_path::{ensure_distinct_output_paths, ensure_safe_output_path};
use tools::packed_sfen::PackedSfenValue;

const RECORD_SIZE: usize = PackedSfenValue::SIZE;
const BUFFER_SIZE: usize = 8 << 20;
const CHUNK_RECORDS: usize = 1 << 18;

// full chunk ごとに mask byte 境界へ戻し、次 chunk の bit 0 を行先頭に対応させる。
const _: () = assert!(CHUNK_RECORDS.is_multiple_of(8));

#[derive(Parser)]
#[command(about = "bitmap mask の bit=1 に対応する PSV 行を入力順に抽出")]
struct Cli {
    /// 入力 PSV shard（40 byte/record）
    #[arg(long)]
    input: PathBuf,
    /// LSB-first bitmap mask
    #[arg(long)]
    mask: PathBuf,
    /// 抽出後の compact PSV
    #[arg(long)]
    out: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
struct Stats {
    records: u64,
    selected: u64,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let stats = select_by_mask(&cli.input, &cli.mask, &cli.out, CHUNK_RECORDS)?;
    let percent = if stats.records == 0 {
        0.0
    } else {
        stats.selected as f64 / stats.records as f64 * 100.0
    };
    println!("records={} selected={} ({percent:.4}%)", stats.records, stats.selected);
    Ok(())
}

fn select_by_mask(input: &Path, mask: &Path, out: &Path, chunk_records: usize) -> Result<Stats> {
    validate_chunk_records(chunk_records)?;
    ensure_safe_output_path(out, input)?;
    ensure_safe_output_path(out, mask)?;
    let staged = partial_path(out);
    ensure_safe_output_path(&staged, input)?;
    ensure_safe_output_path(&staged, mask)?;
    ensure_distinct_output_paths(out, &staged)?;

    let records = checked_input_records(input)?;
    validate_mask(mask, records)?;

    let result = write_selected(input, mask, &staged, records, chunk_records);
    let stats = match result {
        Ok(stats) => stats,
        Err(error) => {
            let _ = fs::remove_file(&staged);
            return Err(error);
        }
    };

    let expected_bytes = stats
        .selected
        .checked_mul(RECORD_SIZE as u64)
        .ok_or_else(|| anyhow::anyhow!("Output size overflow"))?;
    let actual_bytes = fs::metadata(&staged)?.len();
    if actual_bytes != expected_bytes {
        let _ = fs::remove_file(&staged);
        anyhow::bail!("Output size mismatch: expected {expected_bytes} bytes, got {actual_bytes}");
    }

    fs::rename(&staged, out)
        .with_context(|| format!("Failed to rename {} to {}", staged.display(), out.display()))?;
    Ok(stats)
}

fn write_selected(
    input: &Path,
    mask: &Path,
    staged: &Path,
    records: u64,
    chunk_records: usize,
) -> Result<Stats> {
    let mut reader = BufReader::with_capacity(BUFFER_SIZE, File::open(input)?);
    let mut mask_reader = BufReader::with_capacity(BUFFER_SIZE, File::open(mask)?);
    let mut writer = BufWriter::with_capacity(BUFFER_SIZE, File::create(staged)?);
    let mut input_chunk = Vec::with_capacity(chunk_records * RECORD_SIZE);
    let mut mask_chunk = Vec::with_capacity(chunk_records / 8);
    let mut first_row = 0u64;
    let mut selected = 0u64;

    while first_row < records {
        let rows = (records - first_row).min(chunk_records as u64) as usize;
        input_chunk.resize(rows * RECORD_SIZE, 0);
        mask_chunk.resize(rows.div_ceil(8), 0);
        reader.read_exact(&mut input_chunk)?;
        mask_reader.read_exact(&mut mask_chunk)?;

        for (offset, record) in input_chunk.as_chunks::<RECORD_SIZE>().0.iter().enumerate() {
            if mask_chunk[offset / 8] & (1 << (offset % 8)) != 0 {
                writer.write_all(record)?;
                selected += 1;
            }
        }
        first_row += rows as u64;
    }
    writer.flush()?;
    drop(writer);

    Ok(Stats { records, selected })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn records(count: usize) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(count * RECORD_SIZE);
        for row in 0..count {
            let mut record = [0u8; RECORD_SIZE];
            for (column, byte) in record.iter_mut().enumerate() {
                *byte = row.wrapping_mul(41).wrapping_add(column) as u8;
            }
            bytes.extend_from_slice(&record);
        }
        bytes
    }

    fn expected_rows(input: &[u8], selected: &[usize]) -> Vec<u8> {
        let mut expected = Vec::with_capacity(selected.len() * RECORD_SIZE);
        for &row in selected {
            expected.extend_from_slice(&input[row * RECORD_SIZE..(row + 1) * RECORD_SIZE]);
        }
        expected
    }

    fn run_case(count: usize, mask_bytes: &[u8], selected: &[usize], chunk: usize) -> Result<()> {
        let dir = tempdir()?;
        let input = dir.path().join("shard.psv");
        let mask = dir.path().join("entered.bits");
        let out = dir.path().join("compact.psv");
        let input_bytes = records(count);
        fs::write(&input, &input_bytes)?;
        fs::write(&mask, mask_bytes)?;

        let stats = select_by_mask(&input, &mask, &out, chunk)?;
        assert_eq!(
            stats,
            Stats {
                records: count as u64,
                selected: selected.len() as u64
            }
        );
        assert_eq!(fs::read(&out)?, expected_rows(&input_bytes, selected));
        assert!(!partial_path(&out).exists());
        Ok(())
    }

    #[test]
    fn boundary_masks_preserve_selected_records_bit_exactly() -> Result<()> {
        run_case(7, &[0b0101_0101], &[0, 2, 4, 6], 8)?;
        run_case(8, &[0b1000_0001], &[0, 7], 8)?;
        run_case(9, &[0b1010_0110, 0b0000_0001], &[1, 2, 5, 7, 8], 8)?;
        run_case(17, &[0b1000_0001, 0b0100_0010, 0b0000_0001], &[0, 7, 9, 14, 16], 24)?;
        Ok(())
    }

    #[test]
    fn all_zero_and_all_one_masks() -> Result<()> {
        run_case(17, &[0, 0, 0], &[], 8)?;
        run_case(17, &[0xff, 0xff, 0x01], &(0..17).collect::<Vec<_>>(), 8)?;
        Ok(())
    }

    #[test]
    fn selection_crosses_small_chunk_boundaries() -> Result<()> {
        run_case(17, &[0b1000_0000, 0b1000_0001, 0b0000_0001], &[7, 8, 15, 16], 8)
    }

    #[test]
    fn mask_size_mismatch_is_rejected() -> Result<()> {
        for mask_bytes in [&[0xff][..], &[0xff, 0xff, 0][..]] {
            let dir = tempdir()?;
            let input = dir.path().join("shard.psv");
            let mask = dir.path().join("entered.bits");
            fs::write(&input, records(9))?;
            fs::write(&mask, mask_bytes)?;
            assert!(select_by_mask(&input, &mask, &dir.path().join("out.psv"), 8).is_err());
        }
        Ok(())
    }

    #[test]
    fn non_zero_unused_bits_are_rejected() -> Result<()> {
        let dir = tempdir()?;
        let input = dir.path().join("shard.psv");
        let mask = dir.path().join("entered.bits");
        fs::write(&input, records(9))?;
        fs::write(&mask, [0, 0b0000_0010])?;
        assert!(select_by_mask(&input, &mask, &dir.path().join("out.psv"), 8).is_err());
        Ok(())
    }

    #[test]
    fn invalid_input_size_is_rejected() -> Result<()> {
        let dir = tempdir()?;
        let input = dir.path().join("shard.psv");
        let mask = dir.path().join("entered.bits");
        fs::write(&input, [0u8; RECORD_SIZE - 1])?;
        fs::write(&mask, [])?;
        assert!(select_by_mask(&input, &mask, &dir.path().join("out.psv"), 8).is_err());
        Ok(())
    }

    #[test]
    fn output_equal_to_input_is_rejected_without_truncation() -> Result<()> {
        let dir = tempdir()?;
        let input = dir.path().join("shard.psv");
        let mask = dir.path().join("entered.bits");
        let original = records(8);
        fs::write(&input, &original)?;
        fs::write(&mask, [0xff])?;
        assert!(select_by_mask(&input, &mask, &input, 8).is_err());
        assert_eq!(fs::read(input)?, original);
        Ok(())
    }

    #[test]
    fn staged_write_preserves_output_sentinel_on_validation_failure() -> Result<()> {
        let dir = tempdir()?;
        let input = dir.path().join("shard.psv");
        let mask = dir.path().join("entered.bits");
        let out = dir.path().join("compact.psv");
        fs::write(&input, records(9))?;
        fs::write(&mask, [0, 0b1000_0000])?;
        fs::write(&out, b"sentinel")?;

        assert!(select_by_mask(&input, &mask, &out, 8).is_err());
        assert_eq!(fs::read(&out)?, b"sentinel");
        assert!(!partial_path(&out).exists());
        Ok(())
    }

    #[test]
    fn hardlinked_staging_is_rejected_without_truncating_output() -> Result<()> {
        let dir = tempdir()?;
        let input = dir.path().join("shard.psv");
        let mask = dir.path().join("entered.bits");
        let out = dir.path().join("compact.psv");
        let staged = partial_path(&out);
        fs::write(&input, records(8))?;
        fs::write(&mask, [0xff])?;
        fs::write(&out, b"sentinel")?;
        if let Err(error) = fs::hard_link(&out, &staged) {
            eprintln!("hardlink を作成できないためテストをスキップします: {error}");
            return Ok(());
        }

        let error = select_by_mask(&input, &mask, &out, 8).expect_err("operation must fail");
        assert!(error.to_string().contains("resolve to the same file"));
        assert_eq!(fs::read(&out)?, b"sentinel");
        Ok(())
    }

    #[test]
    fn invalid_chunk_size_is_rejected() {
        assert!(validate_chunk_records(0).is_err());
        assert!(validate_chunk_records(7).is_err());
        assert!(validate_chunk_records(8).is_ok());
    }
}
