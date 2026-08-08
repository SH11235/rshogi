//! 行対応 PSV の入玉ドメイン score 合成、またはゲート bitmap 生成を行う。

use std::fs::{self, File};
use std::io::{BufReader, BufWriter, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use rayon::prelude::*;
use rshogi_core::position::Position;
use rshogi_core::types::Color;
use tools::packed_sfen::{PackedSfenValue, unpack_sfen_to_parts};

const RECORD_SIZE: usize = PackedSfenValue::SIZE;
const BUFFER_SIZE: usize = 32 << 20;
const CHUNK_RECORDS: usize = 1 << 18;

#[derive(Parser)]
#[command(about = "入玉ドメインの PSV 合成またはゲート bitmap 生成")]
struct Cli {
    /// depth9 ラベル PSV
    #[arg(long)]
    d9: Option<PathBuf>,
    /// DL リスコア PSV
    #[arg(long)]
    dl: Option<PathBuf>,
    /// merge 出力 PSV
    #[arg(long)]
    out: Option<PathBuf>,
    /// mask 判定元の base PSV
    #[arg(long)]
    input: Option<PathBuf>,
    /// mask bitmap 出力
    #[arg(long)]
    out_mask: Option<PathBuf>,
    /// 対象 tier（entered,advancing のカンマ区切り）
    #[arg(long, default_value = "entered,advancing")]
    tiers: String,
    /// `|depth9 score| < N` の行だけを対象にする
    #[arg(long)]
    d9_abs_max: Option<i32>,
}

#[derive(Clone, Copy)]
struct GateConfig {
    tiers: [bool; 3],
    d9_abs_max: Option<i32>,
}

#[derive(Default)]
struct Stats {
    tiers: [u64; 3],
    gated: u64,
    records: u64,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = GateConfig {
        tiers: parse_tiers(&cli.tiers)?,
        d9_abs_max: cli.d9_abs_max,
    };
    match (cli.d9, cli.dl, cli.out, cli.input, cli.out_mask) {
        (Some(d9), Some(dl), Some(out), None, None) => {
            print_stats(&merge(&d9, &dl, &out, config)?);
        }
        (None, None, None, Some(input), Some(out_mask)) => {
            print_stats(&write_mask(&input, &out_mask, config)?);
        }
        _ => anyhow::bail!(
            "Specify exactly one mode: --d9/--dl/--out or --input/--out-mask; modes cannot be mixed"
        ),
    }
    Ok(())
}

fn parse_tiers(value: &str) -> Result<[bool; 3]> {
    let mut tiers = [false; 3];
    for name in value.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        match name {
            "entered" => tiers[0] = true,
            "advancing" => tiers[1] = true,
            other => anyhow::bail!("Unknown tier: {other} (expected entered or advancing)"),
        }
    }
    anyhow::ensure!(tiers[0] || tiers[1], "--tiers must not be empty");
    Ok(tiers)
}

/// 玉位置から 0=entered、1=advancing、2=normal を返す。
fn classify(pos: &Position) -> usize {
    let black = pos.king_square(Color::Black).rank() as usize;
    let white = pos.king_square(Color::White).rank() as usize;
    if black <= 2 || white >= 6 {
        0
    } else if (3..=5).contains(&black) || (3..=5).contains(&white) {
        1
    } else {
        2
    }
}

fn classify_record(record: &[u8], row: u64) -> Result<(usize, i16)> {
    let psv = PackedSfenValue::from_bytes(record)
        .ok_or_else(|| anyhow::anyhow!("PSV parse failed at row {row}"))?;
    let parts = unpack_sfen_to_parts(&psv.sfen)
        .map_err(|error| anyhow::anyhow!("unpack failed at row {row}: {error}"))?;
    let mut pos = Position::new();
    pos.set_from_parts(&parts.board, &parts.hands, parts.side_to_move)
        .with_context(|| format!("invalid position at row {row}"))?;
    Ok((classify(&pos), psv.score))
}

fn is_gated(config: GateConfig, tier: usize, score: i16) -> bool {
    config.tiers[tier] && config.d9_abs_max.is_none_or(|limit| i32::from(score).abs() < limit)
}

fn checked_records(path: &Path) -> Result<u64> {
    let size = std::fs::metadata(path)
        .with_context(|| format!("Failed to stat {}", path.display()))?
        .len();
    anyhow::ensure!(
        size % RECORD_SIZE as u64 == 0,
        "Input size is not a multiple of {RECORD_SIZE}: {} ({size} bytes)",
        path.display()
    );
    Ok(size / RECORD_SIZE as u64)
}

fn canonicalize_predicted_path(path: &Path) -> Result<PathBuf> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("Path has no file name: {}", path.display()))?;
    let canonical_parent = parent
        .canonicalize()
        .with_context(|| format!("Failed to canonicalize parent {}", parent.display()))?;
    Ok(canonical_parent.join(file_name))
}

fn is_same_file(a: &Path, b: &Path) -> Result<bool> {
    match same_file::is_same_file(a, b) {
        Ok(same) => Ok(same),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| {
            format!("Failed to compare file identities: {} and {}", a.display(), b.display())
        }),
    }
}

/// 出力が入力実体を指す場合は、truncate 前に拒否する。
fn ensure_safe_output_path(output: &Path, input: &Path) -> Result<()> {
    let canonical_input = input
        .canonicalize()
        .with_context(|| format!("Failed to canonicalize input {}", input.display()))?;
    if let Ok(meta) = fs::symlink_metadata(output)
        && meta.file_type().is_symlink()
    {
        anyhow::bail!("Output path is a symlink: {}", output.display());
    }
    let predicted = canonicalize_predicted_path(output)?;
    if predicted == canonical_input || is_same_file(output, &canonical_input)? {
        anyhow::bail!(
            "Output path resolves to input file: {} -> {}",
            output.display(),
            canonical_input.display()
        );
    }
    Ok(())
}

fn classify_chunk(chunk: &[u8], first_row: u64) -> Vec<Result<(usize, i16)>> {
    chunk
        .par_chunks_exact(RECORD_SIZE)
        .enumerate()
        .map(|(offset, record)| classify_record(record, first_row + offset as u64))
        .collect()
}

fn read_record_chunk<R: Read>(reader: &mut R, buffer: &mut Vec<u8>, records: usize) -> Result<()> {
    let bytes = records
        .checked_mul(RECORD_SIZE)
        .ok_or_else(|| anyhow::anyhow!("chunk byte size overflow: {records} records"))?;
    buffer.resize(bytes, 0);
    reader.read_exact(buffer)?;
    Ok(())
}

fn merge(d9: &Path, dl: &Path, out: &Path, config: GateConfig) -> Result<Stats> {
    ensure_safe_output_path(out, d9)?;
    ensure_safe_output_path(out, dl)?;
    let d9_records = checked_records(d9)?;
    let dl_records = checked_records(dl)?;
    anyhow::ensure!(
        dl_records == d9_records,
        "Input record counts differ: d9={d9_records}, dl={dl_records}"
    );
    let mut d9_reader = BufReader::with_capacity(BUFFER_SIZE, File::open(d9)?);
    let mut dl_reader = BufReader::with_capacity(BUFFER_SIZE, File::open(dl)?);
    let mut writer = BufWriter::with_capacity(BUFFER_SIZE, File::create(out)?);
    let mut d9_chunk = Vec::with_capacity(CHUNK_RECORDS * RECORD_SIZE);
    let mut dl_chunk = Vec::with_capacity(CHUNK_RECORDS * RECORD_SIZE);
    let mut stats = Stats::default();
    let mut first_row = 0u64;
    while first_row < d9_records {
        let chunk_records = (d9_records - first_row).min(CHUNK_RECORDS as u64) as usize;
        read_record_chunk(&mut d9_reader, &mut d9_chunk, chunk_records)?;
        read_record_chunk(&mut dl_reader, &mut dl_chunk, chunk_records)?;
        let classifications = classify_chunk(&d9_chunk, first_row);
        for (offset, ((d9_record, dl_record), classification)) in d9_chunk
            .chunks_exact(RECORD_SIZE)
            .zip(dl_chunk.chunks_exact_mut(RECORD_SIZE))
            .zip(classifications)
            .enumerate()
        {
            let row = first_row + offset as u64;
            anyhow::ensure!(
                d9_record[..32] == dl_record[..32] && d9_record[34..39] == dl_record[34..39],
                "Non-score fields differ at row {row}"
            );
            let (tier, score) = classification?;
            stats.tiers[tier] += 1;
            if is_gated(config, tier, score) {
                dl_record[32..34].copy_from_slice(&d9_record[32..34]);
                stats.gated += 1;
            }
        }
        writer.write_all(&dl_chunk)?;
        first_row += chunk_records as u64;
        stats.records += chunk_records as u64;
    }
    writer.flush()?;
    let expected = d9_records * RECORD_SIZE as u64;
    anyhow::ensure!(fs::metadata(out)?.len() == expected, "Output size mismatch");
    Ok(stats)
}

fn write_mask(input: &Path, out: &Path, config: GateConfig) -> Result<Stats> {
    ensure_safe_output_path(out, input)?;
    let records = checked_records(input)?;
    let mut reader = BufReader::with_capacity(BUFFER_SIZE, File::open(input)?);
    let mut writer = BufWriter::with_capacity(BUFFER_SIZE, File::create(out)?);
    let mut chunk = Vec::with_capacity(CHUNK_RECORDS * RECORD_SIZE);
    let mut stats = Stats::default();
    let mut byte = 0u8;
    let mut first_row = 0u64;
    while first_row < records {
        let chunk_records = (records - first_row).min(CHUNK_RECORDS as u64) as usize;
        read_record_chunk(&mut reader, &mut chunk, chunk_records)?;
        let classifications = classify_chunk(&chunk, first_row);
        for (offset, classification) in classifications.into_iter().enumerate() {
            let row = first_row + offset as u64;
            let (tier, score) = classification?;
            stats.tiers[tier] += 1;
            if is_gated(config, tier, score) {
                byte |= 1 << (row % 8);
                stats.gated += 1;
            }
            if row % 8 == 7 {
                writer.write_all(&[byte])?;
                byte = 0;
            }
        }
        first_row += chunk_records as u64;
        stats.records += chunk_records as u64;
    }
    if records % 8 != 0 {
        writer.write_all(&[byte])?;
    }
    writer.flush()?;
    let expected = records.div_ceil(8);
    anyhow::ensure!(fs::metadata(out)?.len() == expected, "Mask size mismatch");
    Ok(stats)
}

fn print_stats(stats: &Stats) {
    let percent = |count| {
        if stats.records == 0 {
            0.0
        } else {
            count as f64 / stats.records as f64 * 100.0
        }
    };
    println!(
        "done: records={}\n  entered={} ({:.4}%) advancing={} ({:.4}%) normal={} ({:.4}%)\n  gated={} ({:.4}%)",
        stats.records,
        stats.tiers[0],
        percent(stats.tiers[0]),
        stats.tiers[1],
        percent(stats.tiers[1]),
        stats.tiers[2],
        percent(stats.tiers[2]),
        stats.gated,
        percent(stats.gated),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use tools::packed_sfen::pack_position;

    fn record(sfen: &str, score: i16) -> Result<[u8; RECORD_SIZE]> {
        let mut pos = Position::new();
        pos.set_sfen(sfen)?;
        Ok(PackedSfenValue {
            sfen: pack_position(&pos),
            score,
            move16: 0,
            game_ply: 1,
            game_result: 0,
            padding: 0,
        }
        .to_bytes())
    }

    fn samples(count: usize, score_base: i16) -> Result<Vec<u8>> {
        let sfens = [
            "4K3k/9/9/9/9/9/9/9/9 b - 1",
            "4k4/9/9/9/4K4/9/9/9/9 b - 1",
            "4k4/9/9/9/9/9/9/9/4K4 b - 1",
        ];
        let mut bytes = Vec::with_capacity(count * RECORD_SIZE);
        for i in 0..count {
            bytes.extend_from_slice(&record(sfens[i % sfens.len()], score_base + i as i16)?);
        }
        Ok(bytes)
    }

    #[test]
    fn mask_bit_order_and_partial_byte() -> Result<()> {
        for count in [7, 8, 9, 15, 16, 17] {
            let dir = tempdir()?;
            let input = dir.path().join("input.psv");
            let output = dir.path().join("mask.bin");
            std::fs::write(&input, samples(count, 0)?)?;
            write_mask(
                &input,
                &output,
                GateConfig {
                    tiers: [true, false, false],
                    d9_abs_max: None,
                },
            )?;
            let mask = std::fs::read(output)?;
            assert_eq!(mask.len(), count.div_ceil(8));
            for i in 0..count {
                assert_eq!((mask[i / 8] >> (i % 8)) & 1, u8::from(i % 3 == 0));
            }
            if count % 8 != 0 {
                assert_eq!(mask[count / 8] >> (count % 8), 0);
            }
        }
        Ok(())
    }

    #[test]
    fn merge_matches_mask_selection() -> Result<()> {
        let dir = tempdir()?;
        let d9 = dir.path().join("d9.psv");
        let dl = dir.path().join("dl.psv");
        let merged = dir.path().join("merged.psv");
        let mask = dir.path().join("mask.bin");
        std::fs::write(&d9, samples(17, 10)?)?;
        std::fs::write(&dl, samples(17, 100)?)?;
        let config = GateConfig {
            tiers: [true, true, false],
            d9_abs_max: Some(25),
        };
        merge(&d9, &dl, &merged, config)?;
        write_mask(&d9, &mask, config)?;
        let (d9b, dlb, mb, bits) = (
            std::fs::read(d9)?,
            std::fs::read(dl)?,
            std::fs::read(merged)?,
            std::fs::read(mask)?,
        );
        for i in 0..17 {
            let expected = if (bits[i / 8] >> (i % 8)) & 1 == 1 {
                &d9b
            } else {
                &dlb
            };
            assert_eq!(
                &mb[i * RECORD_SIZE + 32..i * RECORD_SIZE + 34],
                &expected[i * RECORD_SIZE + 32..i * RECORD_SIZE + 34]
            );
        }
        Ok(())
    }

    #[test]
    fn bare_relative_output_path_is_supported() -> Result<()> {
        let predicted = canonicalize_predicted_path(Path::new("out.psv"))?;
        assert_eq!(predicted.file_name(), Some(std::ffi::OsStr::new("out.psv")));
        assert_eq!(predicted.parent(), Some(std::env::current_dir()?.canonicalize()?.as_path()));
        Ok(())
    }

    #[test]
    fn d9_abs_max_boundary_is_exclusive() {
        let config = GateConfig {
            tiers: [true, false, false],
            d9_abs_max: Some(25),
        };
        assert!(is_gated(config, 0, 24));
        assert!(is_gated(config, 0, -24));
        assert!(!is_gated(config, 0, 25));
        assert!(!is_gated(config, 0, -25));
    }

    #[test]
    fn white_king_in_middle_zone_is_advancing() -> Result<()> {
        // 後手玉が中央（rank 3..=5）へ進み、先手玉は自陣に残るケース。
        let bytes = record("9/9/9/9/4k4/9/9/9/4K4 b - 1", 0)?;
        let (tier, _) = classify_record(&bytes, 0)?;
        assert_eq!(tier, 1);
        Ok(())
    }

    #[test]
    fn mask_rejects_output_equal_to_input_without_truncating() -> Result<()> {
        let dir = tempdir()?;
        let input = dir.path().join("input.psv");
        let original = samples(3, 0)?;
        std::fs::write(&input, &original)?;
        let error = write_mask(
            &input,
            &input,
            GateConfig {
                tiers: [true, true, false],
                d9_abs_max: None,
            },
        )
        .err()
        .expect("operation must fail");
        assert!(error.to_string().contains("resolves to input file"));
        assert_eq!(std::fs::read(input)?, original);
        Ok(())
    }

    #[test]
    fn mask_rejects_hardlink_output_without_truncating() -> Result<()> {
        let dir = tempdir()?;
        let input = dir.path().join("input.psv");
        let output = dir.path().join("mask.bin");
        let original = samples(3, 0)?;
        std::fs::write(&input, &original)?;
        if let Err(error) = std::fs::hard_link(&input, &output) {
            eprintln!("hardlink を作成できないためテストをスキップします: {error}");
            return Ok(());
        }
        let error = write_mask(
            &input,
            &output,
            GateConfig {
                tiers: [true, true, false],
                d9_abs_max: None,
            },
        )
        .err()
        .expect("operation must fail");
        assert!(error.to_string().contains("resolves to input file"));
        assert_eq!(std::fs::read(input)?, original);
        Ok(())
    }

    #[test]
    fn merge_rejects_output_equal_to_input_without_truncating() -> Result<()> {
        let dir = tempdir()?;
        let d9 = dir.path().join("d9.psv");
        let dl = dir.path().join("dl.psv");
        let original = samples(3, 0)?;
        std::fs::write(&d9, &original)?;
        std::fs::write(&dl, samples(3, 100)?)?;
        let error = merge(
            &d9,
            &dl,
            &d9,
            GateConfig {
                tiers: [true, true, false],
                d9_abs_max: None,
            },
        )
        .err()
        .expect("operation must fail");
        assert!(error.to_string().contains("resolves to input file"));
        assert_eq!(std::fs::read(d9)?, original);
        Ok(())
    }

    #[test]
    fn merge_count_mismatch_reports_both_counts() -> Result<()> {
        let dir = tempdir()?;
        let d9 = dir.path().join("d9.psv");
        let dl = dir.path().join("dl.psv");
        std::fs::write(&d9, samples(1, 0)?)?;
        std::fs::write(&dl, samples(2, 0)?)?;
        let error = merge(
            &d9,
            &dl,
            &dir.path().join("out.psv"),
            GateConfig {
                tiers: [true, true, false],
                d9_abs_max: None,
            },
        )
        .err()
        .expect("operation must fail");
        assert!(error.to_string().contains("d9=1, dl=2"));
        Ok(())
    }

    #[test]
    fn invalid_input_size_fails_closed() -> Result<()> {
        let dir = tempdir()?;
        let input = dir.path().join("bad.psv");
        std::fs::write(&input, [0u8; RECORD_SIZE - 1])?;
        assert!(
            write_mask(
                &input,
                &dir.path().join("mask"),
                GateConfig {
                    tiers: [true, true, false],
                    d9_abs_max: None
                }
            )
            .is_err()
        );
        Ok(())
    }
}
