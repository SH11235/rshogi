//! compact PSV の score を mask 対応で元 shard に書き戻す。

use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use tools::mask_io::{checked_input_records, partial_path, validate_chunk_records, validate_mask};
use tools::output_path::{ensure_distinct_output_paths, ensure_safe_output_path};
use tools::packed_sfen::PackedSfenValue;

const RECORD_SIZE: usize = PackedSfenValue::SIZE;
const PACKED_SFEN_SIZE: usize = 32;
const SCORE_OFFSET: usize = 32;
const SCORE_SIZE: usize = 2;
const BUFFER_SIZE: usize = 8 << 20;
const CHUNK_RECORDS: usize = 1 << 18;

// full chunk ごとに mask byte 境界へ戻し、次 chunk の bit 0 を行先頭に対応させる。
const _: () = assert!(CHUNK_RECORDS.is_multiple_of(8));

#[derive(Parser)]
#[command(about = "compact PSV の score を mask 対応で元 shard に書き戻す")]
struct Cli {
    /// 元の入力 PSV shard（40 byte/record）
    #[arg(long)]
    input: PathBuf,
    /// LSB-first bitmap mask
    #[arg(long)]
    mask: PathBuf,
    /// 抽出順のまま再スコア済みの compact PSV
    #[arg(long)]
    compact: PathBuf,
    /// score 書き戻し後の PSV
    #[arg(long)]
    out: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
struct Stats {
    records: u64,
    replaced: u64,
    changed: u64,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let stats = scatter_by_mask(&cli.input, &cli.mask, &cli.compact, &cli.out, CHUNK_RECORDS)?;
    println!("{}", format_stats(&stats));
    Ok(())
}

fn format_stats(stats: &Stats) -> String {
    format!(
        "records={} replaced={} changed={}",
        stats.records, stats.replaced, stats.changed
    )
}

fn mask_popcount(mask: &Path) -> Result<u64> {
    let mut reader = BufReader::with_capacity(BUFFER_SIZE, File::open(mask)?);
    let mut buffer = vec![0u8; BUFFER_SIZE];
    let mut popcount = 0u64;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        popcount += buffer[..read].iter().map(|byte| u64::from(byte.count_ones())).sum::<u64>();
    }
    Ok(popcount)
}

fn scatter_by_mask(
    input: &Path,
    mask: &Path,
    compact: &Path,
    out: &Path,
    chunk_records: usize,
) -> Result<Stats> {
    validate_chunk_records(chunk_records)?;
    for source in [input, mask, compact] {
        ensure_safe_output_path(out, source)?;
    }
    let staged = partial_path(out);
    for source in [input, mask, compact] {
        ensure_safe_output_path(&staged, source)?;
    }
    ensure_distinct_output_paths(out, &staged)?;

    let records = checked_input_records(input)?;
    validate_mask(mask, records)?;
    let compact_records = checked_input_records(compact)?;
    let replaced = mask_popcount(mask)?;
    anyhow::ensure!(
        replaced == compact_records,
        "Mask popcount does not match compact records: mask={replaced}, compact={compact_records}"
    );

    let result = write_scattered(input, mask, compact, &staged, records, replaced, chunk_records)
        .and_then(|stats| {
            let expected_bytes = records
                .checked_mul(RECORD_SIZE as u64)
                .ok_or_else(|| anyhow::anyhow!("Output size overflow"))?;
            let actual_bytes = fs::metadata(&staged)?.len();
            anyhow::ensure!(
                actual_bytes == expected_bytes,
                "Output size mismatch: expected {expected_bytes} bytes, got {actual_bytes}"
            );
            Ok(stats)
        });
    let stats = match result {
        Ok(stats) => stats,
        Err(error) => {
            let _ = fs::remove_file(&staged);
            return Err(error);
        }
    };

    if let Err(error) = fs::rename(&staged, out)
        .with_context(|| format!("Failed to rename {} to {}", staged.display(), out.display()))
    {
        let _ = fs::remove_file(&staged);
        return Err(error);
    }
    Ok(stats)
}

fn write_scattered(
    input: &Path,
    mask: &Path,
    compact: &Path,
    staged: &Path,
    records: u64,
    replaced: u64,
    chunk_records: usize,
) -> Result<Stats> {
    let mut input_reader = BufReader::with_capacity(BUFFER_SIZE, File::open(input)?);
    let mut mask_reader = BufReader::with_capacity(BUFFER_SIZE, File::open(mask)?);
    let mut compact_reader = BufReader::with_capacity(BUFFER_SIZE, File::open(compact)?);
    let mut writer = BufWriter::with_capacity(BUFFER_SIZE, File::create(staged)?);
    let mut input_chunk = Vec::with_capacity(chunk_records * RECORD_SIZE);
    let mut mask_chunk = Vec::with_capacity(chunk_records / 8);
    let mut compact_record = [0u8; RECORD_SIZE];
    let mut first_row = 0u64;
    let mut changed = 0u64;

    while first_row < records {
        let rows = (records - first_row).min(chunk_records as u64) as usize;
        input_chunk.resize(rows * RECORD_SIZE, 0);
        mask_chunk.resize(rows.div_ceil(8), 0);
        input_reader.read_exact(&mut input_chunk)?;
        mask_reader.read_exact(&mut mask_chunk)?;

        for (offset, input_record) in input_chunk.as_chunks::<RECORD_SIZE>().0.iter().enumerate() {
            if mask_chunk[offset / 8] & (1 << (offset % 8)) == 0 {
                writer.write_all(input_record)?;
                continue;
            }

            compact_reader.read_exact(&mut compact_record)?;
            let row = first_row + offset as u64;
            anyhow::ensure!(
                input_record[..PACKED_SFEN_SIZE] == compact_record[..PACKED_SFEN_SIZE],
                "Packed SFEN mismatch at input record {row}"
            );
            let mut output_record = [0u8; RECORD_SIZE];
            output_record.copy_from_slice(input_record);
            let score = SCORE_OFFSET..SCORE_OFFSET + SCORE_SIZE;
            if input_record[score.clone()] != compact_record[score.clone()] {
                changed += 1;
            }
            output_record[score.clone()].copy_from_slice(&compact_record[score]);
            writer.write_all(&output_record)?;
        }
        first_row += rows as u64;
    }
    writer.flush()?;
    drop(writer);

    Ok(Stats {
        records,
        replaced,
        changed,
    })
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
            record[SCORE_OFFSET..SCORE_OFFSET + SCORE_SIZE]
                .copy_from_slice(&(row as i16).to_le_bytes());
            bytes.extend_from_slice(&record);
        }
        bytes
    }

    fn selected_records(input: &[u8], selected: &[usize]) -> Vec<u8> {
        let mut compact = Vec::with_capacity(selected.len() * RECORD_SIZE);
        for &row in selected {
            compact.extend_from_slice(&input[row * RECORD_SIZE..(row + 1) * RECORD_SIZE]);
        }
        compact
    }

    fn run_case(
        count: usize,
        mask_bytes: &[u8],
        selected: &[usize],
        chunk_records: usize,
    ) -> Result<()> {
        let dir = tempdir()?;
        let input = dir.path().join("shard.psv");
        let mask = dir.path().join("entered.bits");
        let compact = dir.path().join("compact.psv");
        let out = dir.path().join("out.psv");
        let input_bytes = records(count);
        let mut compact_bytes = selected_records(&input_bytes, selected);
        for (index, record) in compact_bytes.as_chunks_mut::<RECORD_SIZE>().0.iter_mut().enumerate()
        {
            record[SCORE_OFFSET..SCORE_OFFSET + SCORE_SIZE]
                .copy_from_slice(&(-(index as i16) - 100).to_le_bytes());
            record[34..].fill(0xa5);
        }
        fs::write(&input, &input_bytes)?;
        fs::write(&mask, mask_bytes)?;
        fs::write(&compact, &compact_bytes)?;

        let stats = scatter_by_mask(&input, &mask, &compact, &out, chunk_records)?;
        assert_eq!(stats.records, count as u64);
        assert_eq!(stats.replaced, selected.len() as u64);
        assert_eq!(stats.changed, selected.len() as u64);
        let output = fs::read(&out)?;
        let mut compact_index = 0;
        for row in 0..count {
            let range = row * RECORD_SIZE..(row + 1) * RECORD_SIZE;
            if selected.contains(&row) {
                let compact_range = compact_index * RECORD_SIZE..(compact_index + 1) * RECORD_SIZE;
                assert_eq!(
                    &output[range.start + SCORE_OFFSET..range.start + SCORE_OFFSET + SCORE_SIZE],
                    &compact_bytes[compact_range.start + SCORE_OFFSET
                        ..compact_range.start + SCORE_OFFSET + SCORE_SIZE]
                );
                assert_eq!(
                    &output[range.start..range.start + SCORE_OFFSET],
                    &input_bytes[range.start..range.start + SCORE_OFFSET]
                );
                assert_eq!(
                    &output[range.start + SCORE_OFFSET + SCORE_SIZE..range.end],
                    &input_bytes[range.start + SCORE_OFFSET + SCORE_SIZE..range.end]
                );
                compact_index += 1;
            } else {
                assert_eq!(&output[range.clone()], &input_bytes[range]);
            }
        }
        assert!(!partial_path(&out).exists());
        Ok(())
    }

    #[test]
    fn boundary_masks_replace_matching_scores() -> Result<()> {
        run_case(7, &[0b0101_0101], &[0, 2, 4, 6], 8)?;
        run_case(8, &[0b1000_0001], &[0, 7], 8)?;
        run_case(9, &[0b1010_0110, 0b0000_0001], &[1, 2, 5, 7, 8], 8)?;
        run_case(17, &[0b1000_0001, 0b0100_0010, 0b0000_0001], &[0, 7, 9, 14, 16], 24)?;
        Ok(())
    }

    #[test]
    fn replacement_crosses_small_chunk_boundaries() -> Result<()> {
        run_case(17, &[0b1000_0000, 0b1000_0001, 0b0000_0001], &[7, 8, 15, 16], 8)
    }

    #[test]
    fn unchanged_compact_is_bit_exact_identity() -> Result<()> {
        let dir = tempdir()?;
        let input = dir.path().join("shard.psv");
        let mask = dir.path().join("entered.bits");
        let compact = dir.path().join("compact.psv");
        let out = dir.path().join("out.psv");
        let input_bytes = records(9);
        fs::write(&input, &input_bytes)?;
        fs::write(&mask, [0b1010_0110, 1])?;
        fs::write(&compact, selected_records(&input_bytes, &[1, 2, 5, 7, 8]))?;

        let stats = scatter_by_mask(&input, &mask, &compact, &out, 8)?;
        assert_eq!(
            stats,
            Stats {
                records: 9,
                replaced: 5,
                changed: 0
            }
        );
        assert_eq!(fs::read(out)?, input_bytes);
        Ok(())
    }

    #[test]
    fn packed_sfen_mismatch_is_fail_closed() -> Result<()> {
        let dir = tempdir()?;
        let input = dir.path().join("shard.psv");
        let mask = dir.path().join("entered.bits");
        let compact = dir.path().join("compact.psv");
        let out = dir.path().join("out.psv");
        let input_bytes = records(8);
        let mut compact_bytes = selected_records(&input_bytes, &[3]);
        compact_bytes[0] ^= 1;
        fs::write(&input, input_bytes)?;
        fs::write(&mask, [0b0000_1000])?;
        fs::write(&compact, compact_bytes)?;
        fs::write(&out, b"sentinel")?;

        let error = scatter_by_mask(&input, &mask, &compact, &out, 8).expect_err("must fail");
        assert!(error.to_string().contains("input record 3"));
        assert_eq!(fs::read(&out)?, b"sentinel");
        assert!(!partial_path(&out).exists());
        Ok(())
    }

    #[test]
    fn compact_record_count_must_equal_mask_popcount() -> Result<()> {
        for compact_count in [1, 3] {
            let dir = tempdir()?;
            let input = dir.path().join("shard.psv");
            let mask = dir.path().join("entered.bits");
            let compact = dir.path().join("compact.psv");
            let out = dir.path().join("out.psv");
            fs::write(&input, records(8))?;
            fs::write(&mask, [0b0000_0011])?;
            fs::write(&compact, records(compact_count))?;
            assert!(scatter_by_mask(&input, &mask, &compact, &out, 8).is_err());
            assert!(!out.exists());
            assert!(!partial_path(&out).exists());
        }
        Ok(())
    }

    #[test]
    fn invalid_compact_size_is_rejected_before_output_creation() -> Result<()> {
        let dir = tempdir()?;
        let input = dir.path().join("shard.psv");
        let mask = dir.path().join("entered.bits");
        let compact = dir.path().join("compact.psv");
        let out = dir.path().join("out.psv");
        fs::write(&input, records(8))?;
        fs::write(&mask, [1])?;
        fs::write(&compact, [0u8; RECORD_SIZE - 1])?;

        assert!(scatter_by_mask(&input, &mask, &compact, &out, 8).is_err());
        assert!(!out.exists());
        assert!(!partial_path(&out).exists());
        Ok(())
    }

    #[test]
    fn malformed_inputs_are_rejected_before_output_creation() -> Result<()> {
        for (input_bytes, mask_bytes) in [
            (records(9), vec![0xff]),
            (records(9), vec![0, 0b0000_0010]),
            (vec![0u8; RECORD_SIZE - 1], vec![]),
        ] {
            let dir = tempdir()?;
            let input = dir.path().join("shard.psv");
            let mask = dir.path().join("entered.bits");
            let compact = dir.path().join("compact.psv");
            let out = dir.path().join("out.psv");
            fs::write(&input, input_bytes)?;
            fs::write(&mask, mask_bytes)?;
            fs::write(&compact, [])?;
            assert!(scatter_by_mask(&input, &mask, &compact, &out, 8).is_err());
            assert!(!out.exists());
            assert!(!partial_path(&out).exists());
        }
        Ok(())
    }

    #[test]
    fn output_equal_to_an_input_is_rejected_without_truncation() -> Result<()> {
        for output_source in 0..3 {
            let dir = tempdir()?;
            let input = dir.path().join("shard.psv");
            let mask = dir.path().join("entered.bits");
            let compact = dir.path().join("compact.psv");
            let input_bytes = records(8);
            let compact_bytes = selected_records(&input_bytes, &[0]);
            fs::write(&input, &input_bytes)?;
            fs::write(&mask, [1])?;
            fs::write(&compact, &compact_bytes)?;
            let out = [&input, &mask, &compact][output_source];

            assert!(scatter_by_mask(&input, &mask, &compact, out, 8).is_err());
            assert_eq!(fs::read(&input)?, input_bytes);
            assert_eq!(fs::read(&mask)?, [1]);
            assert_eq!(fs::read(&compact)?, compact_bytes);
        }
        Ok(())
    }

    #[test]
    fn statistics_format_is_exact() {
        let stats = Stats {
            records: 17,
            replaced: 5,
            changed: 3,
        };
        assert_eq!(format_stats(&stats), "records=17 replaced=5 changed=3");
    }

    #[test]
    fn invalid_chunk_size_is_rejected() -> Result<()> {
        let dir = tempdir()?;
        let path = dir.path().join("unused");
        assert!(scatter_by_mask(&path, &path, &path, &path, 0).is_err());
        assert!(scatter_by_mask(&path, &path, &path, &path, 7).is_err());
        Ok(())
    }
}
