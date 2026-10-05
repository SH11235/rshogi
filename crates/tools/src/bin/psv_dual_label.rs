//! 通常 PSV と sidecar を dual-label PSV に相互変換し、形式を検証する。

use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use rayon::prelude::*;
use rshogi_core::position::Position;
use tools::common::io::{partial_path, sync_directory};
use tools::king_zone::{ENTERED_TIER, classify};
use tools::output_path::{
    ensure_created_paths_distinct, ensure_distinct_output_paths, ensure_no_entity_overlap,
    ensure_safe_output_path,
};
use tools::packed_sfen::{
    PackedSfenValue, is_legal_psv_move, psv_move16_to_move, unpack_sfen_to_parts,
};

const RECORD_SIZE: usize = PackedSfenValue::SIZE;
const SCORE_OFFSET: usize = 32;
const DL_SCORE_OFFSET: usize = 34;
const PADDING_OFFSET: usize = 39;
const BUFFER_SIZE: usize = 32 << 20;
const CHUNK_RECORDS: usize = 1 << 18;
// mask の bit 添字を chunk 内 offset で計算するため、chunk 境界を byte 境界に揃える。
const _: () = assert!(CHUNK_RECORDS.is_multiple_of(8));
const DEFAULT_DL_ABS_MAX: u32 = 32_000;
const DEFAULT_MAX_MOVE_LIKE_FRAC: f64 = 0.05;

#[derive(Parser)]
#[command(name = "psv_dual_label", about = "dual-label PSV の生成・抽出・検証")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 通常 PSV の score 列を sidecar に退避する
    DumpScores {
        /// score を読み出す通常 PSV
        #[arg(long)]
        base: PathBuf,
        /// little-endian i16 × records の score sidecar
        #[arg(long)]
        out_scores: PathBuf,
    },
    /// 通常 PSV と score/mask sidecar を dual-label PSV に埋め込む
    Embed {
        /// base score を持つ通常 PSV
        #[arg(long)]
        base: PathBuf,
        /// little-endian i16 × records の DL score sidecar
        #[arg(long)]
        scores: PathBuf,
        /// LSB-first の entered bitmap
        #[arg(long)]
        mask: PathBuf,
        /// 出力 dual-label PSV
        #[arg(long)]
        out: PathBuf,
    },
    /// dual-label PSV を通常 PSV と sidecar に分解する
    Extract {
        /// 入力 dual-label PSV
        #[arg(long)]
        dual: PathBuf,
        /// move16=0、padding=0 に復元した通常 PSV
        #[arg(long)]
        out_base: Option<PathBuf>,
        /// little-endian i16 × records の DL score sidecar
        #[arg(long)]
        out_scores: Option<PathBuf>,
        /// LSB-first の entered bitmap
        #[arg(long)]
        out_mask: Option<PathBuf>,
    },
    /// dual-label PSV の形式・entered bit・DL 値・move-like 率を検証する
    Validate {
        /// 入力 dual-label PSV
        #[arg(long)]
        dual: PathBuf,
        /// 局面 decode を伴う検査の最大サンプル数（未指定なら全件）
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        sample: Option<u64>,
        /// 許容する DL score の絶対値上限
        #[arg(long, default_value_t = DEFAULT_DL_ABS_MAX)]
        dl_abs_max: u32,
        /// 通常 PSV の move16 に見える行の許容割合。この値を超えたら失敗
        /// (0 は move-like 行を 1 行も許容しない最厳設定)
        #[arg(long, default_value_t = DEFAULT_MAX_MOVE_LIKE_FRAC)]
        max_move_like_frac: f64,
    },
}

#[derive(Debug, Default, PartialEq, Eq)]
struct EmbedStats {
    records: u64,
    overwritten_nonzero_move16: u64,
    overwritten_nonzero_padding: u64,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct DumpScoresStats {
    records: u64,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ExtractStats {
    records: u64,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ValidationStats {
    records: u64,
    trailing_bytes: u64,
    sampled_records: u64,
    reserved_padding_nonzero: u64,
    entered_mismatches: u64,
    dl_abs_exceeded: u64,
    move_like: u64,
    decode_errors: u64,
    first_reserved_padding_row: Option<u64>,
    first_entered_mismatch_row: Option<u64>,
    first_dl_abs_exceeded_row: Option<u64>,
    first_decode_error_row: Option<u64>,
}

#[derive(Debug, Default)]
struct RecordCheck {
    sampled: bool,
    reserved_padding_nonzero: bool,
    entered_mismatch: bool,
    dl_abs_exceeded: bool,
    move_like: bool,
    decode_error: bool,
}

#[derive(Clone, Copy)]
struct ValidateConfig {
    sample: Option<u64>,
    dl_abs_max: u32,
    max_move_like_frac: f64,
}

fn file_size(path: &Path) -> Result<u64> {
    Ok(fs::metadata(path)
        .with_context(|| format!("{} の情報を取得できません", path.display()))?
        .len())
}

fn checked_psv_records(path: &Path) -> Result<u64> {
    let size = file_size(path)?;
    anyhow::ensure!(
        size.is_multiple_of(RECORD_SIZE as u64),
        "{} のサイズ {size} byte が PSV レコード長 {RECORD_SIZE} の倍数ではありません",
        path.display()
    );
    Ok(size / RECORD_SIZE as u64)
}

fn checked_sidecar_sizes(scores: &Path, mask: &Path, records: u64) -> Result<()> {
    let expected_scores = records * 2;
    let actual_scores = file_size(scores)?;
    anyhow::ensure!(
        actual_scores == expected_scores,
        "score sidecar size mismatch: expected={expected_scores}, actual={actual_scores}"
    );

    let expected_mask = records.div_ceil(8);
    let actual_mask = file_size(mask)?;
    anyhow::ensure!(
        actual_mask == expected_mask,
        "mask size mismatch: expected={expected_mask}, actual={actual_mask}"
    );
    if !records.is_multiple_of(8) {
        let mut file = File::open(mask)?;
        file.seek(SeekFrom::End(-1))?;
        let mut last = [0u8; 1];
        file.read_exact(&mut last)?;
        let used_bits = records % 8;
        anyhow::ensure!(
            last[0] >> used_bits == 0,
            "mask final byte has non-zero unused bits: 0x{:02x}",
            last[0]
        );
    }
    Ok(())
}

fn read_bytes<R: Read>(reader: &mut R, buffer: &mut Vec<u8>, bytes: usize) -> Result<()> {
    buffer.resize(bytes, 0);
    reader.read_exact(buffer)?;
    Ok(())
}

fn embed(base: &Path, scores: &Path, mask: &Path, out: &Path) -> Result<EmbedStats> {
    embed_with_chunk_records(base, scores, mask, out, CHUNK_RECORDS)
}

fn remove_staging(paths: &[PathBuf]) {
    for path in paths {
        if let Err(error) = fs::remove_file(path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("staging file cleanup failed: {}: {error}", path.display());
        }
    }
}

fn dump_scores(base: &Path, out_scores: &Path) -> Result<DumpScoresStats> {
    dump_scores_with_chunk_records(base, out_scores, CHUNK_RECORDS)
}

fn dump_scores_with_chunk_records(
    base: &Path,
    out_scores: &Path,
    chunk_records: usize,
) -> Result<DumpScoresStats> {
    anyhow::ensure!(chunk_records > 0, "chunk_records must be positive");
    let staging = partial_path(out_scores);
    // preflight (サイズ・パス検査) の失敗でも過去 run の残骸 .partial を掃除するため、
    // 検査もすべて cleanup 付きクロージャの内側で行う
    let result = (|| {
        let records = checked_psv_records(base)?;
        ensure_safe_output_path(out_scores, base)?;
        ensure_safe_output_path(&staging, base)?;
        ensure_distinct_output_paths(out_scores, &staging)?;
        let mut reader = BufReader::with_capacity(BUFFER_SIZE, File::open(base)?);
        let mut writer = BufWriter::with_capacity(BUFFER_SIZE, File::create(&staging)?);
        let mut base_chunk = Vec::with_capacity(chunk_records * RECORD_SIZE);
        let mut score_chunk = Vec::with_capacity(chunk_records * 2);
        let mut stats = DumpScoresStats::default();

        while stats.records < records {
            let current_records = (records - stats.records).min(chunk_records as u64) as usize;
            read_bytes(&mut reader, &mut base_chunk, current_records * RECORD_SIZE)?;
            score_chunk.clear();
            score_chunk.reserve(current_records * 2);
            for record in base_chunk.as_chunks::<RECORD_SIZE>().0 {
                score_chunk.extend_from_slice(&record[SCORE_OFFSET..SCORE_OFFSET + 2]);
            }
            writer.write_all(&score_chunk)?;
            stats.records += current_records as u64;
        }

        writer.flush()?;
        writer.into_inner()?.sync_all()?;
        let expected = records * 2;
        let actual = file_size(&staging)?;
        anyhow::ensure!(
            actual == expected,
            "score output size mismatch: expected={expected}, actual={actual}"
        );
        publish_staged(&[(&staging, out_scores)])?;
        Ok(stats)
    })();
    if result.is_err() {
        // base が「<out>.partial」そのものだと staging == base になる — 入力を消さない
        if ensure_safe_output_path(&staging, base).is_ok() {
            remove_staging(&[staging]);
        }
    }
    result
}

fn publish_staged(outputs: &[(&Path, &Path)]) -> Result<()> {
    let mut published: Vec<&Path> = Vec::new();
    for (index, &(staging, output)) in outputs.iter().enumerate() {
        if let Err(error) = fs::rename(staging, output) {
            let unpublished_outputs: Vec<&Path> =
                outputs[index..].iter().map(|(_, path)| *path).collect();
            let unpublished_staging: Vec<PathBuf> =
                outputs[index..].iter().map(|(path, _)| (*path).to_path_buf()).collect();
            remove_staging(&unpublished_staging);
            let format_paths = |paths: &[&Path]| {
                paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let unpublished_staging_refs: Vec<&Path> =
                unpublished_staging.iter().map(PathBuf::as_path).collect();
            let publish_state = if published.is_empty() {
                "publish 未完了"
            } else {
                "部分 publish 状態"
            };
            return Err(error).with_context(|| {
                format!(
                    "{} -> {} の publish に失敗（{publish_state}）。publish 済み最終出力=[{}]; 未 publish 最終出力=[{}]; cleanup 対象の未 publish staging=[{}]",
                    staging.display(),
                    output.display(),
                    format_paths(&published),
                    format_paths(&unpublished_outputs),
                    format_paths(&unpublished_staging_refs)
                )
            });
        }
        published.push(output);
    }
    // rename の永続化: crash 後に publish 済みエントリが巻き戻らないよう親 directory を sync。
    let mut synced: Vec<&Path> = Vec::new();
    for (_, output) in outputs {
        let parent =
            output.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        if !synced.contains(&parent) {
            sync_directory(parent)?;
            synced.push(parent);
        }
    }
    Ok(())
}

fn embed_with_chunk_records(
    base: &Path,
    scores: &Path,
    mask: &Path,
    out: &Path,
    chunk_records: usize,
) -> Result<EmbedStats> {
    anyhow::ensure!(
        chunk_records > 0 && chunk_records.is_multiple_of(8),
        "chunk_records must be a positive multiple of 8"
    );
    let records = checked_psv_records(base)?;
    anyhow::ensure!(records > 0, "{} が空です (0 レコード)", base.display());
    checked_sidecar_sizes(scores, mask, records)?;
    let staging = partial_path(out);
    for input in [base, scores, mask] {
        ensure_safe_output_path(out, input)?;
        ensure_safe_output_path(&staging, input)?;
    }
    ensure_distinct_output_paths(out, &staging)?;

    let result = (|| {
        let mut base_reader = BufReader::with_capacity(BUFFER_SIZE, File::open(base)?);
        let mut score_reader = BufReader::with_capacity(BUFFER_SIZE, File::open(scores)?);
        let mut mask_reader = BufReader::with_capacity(BUFFER_SIZE, File::open(mask)?);
        let mut writer = BufWriter::with_capacity(BUFFER_SIZE, File::create(&staging)?);
        let mut base_chunk = Vec::with_capacity(chunk_records * RECORD_SIZE);
        let mut score_chunk = Vec::with_capacity(chunk_records * 2);
        let mut mask_chunk = Vec::with_capacity(chunk_records.div_ceil(8));
        let mut stats = EmbedStats::default();

        while stats.records < records {
            let current_records = (records - stats.records).min(chunk_records as u64) as usize;
            read_bytes(&mut base_reader, &mut base_chunk, current_records * RECORD_SIZE)?;
            read_bytes(&mut score_reader, &mut score_chunk, current_records * 2)?;
            read_bytes(&mut mask_reader, &mut mask_chunk, current_records.div_ceil(8))?;

            for (offset, record) in
                base_chunk.as_chunks_mut::<RECORD_SIZE>().0.iter_mut().enumerate()
            {
                if record[DL_SCORE_OFFSET..DL_SCORE_OFFSET + 2] != [0, 0] {
                    stats.overwritten_nonzero_move16 += 1;
                }
                if record[PADDING_OFFSET] != 0 {
                    stats.overwritten_nonzero_padding += 1;
                }
                record[DL_SCORE_OFFSET..DL_SCORE_OFFSET + 2]
                    .copy_from_slice(&score_chunk[offset * 2..offset * 2 + 2]);
                record[PADDING_OFFSET] = (mask_chunk[offset / 8] >> (offset % 8)) & 1;
            }
            writer.write_all(&base_chunk)?;
            stats.records += current_records as u64;
        }

        writer.flush()?;
        writer.into_inner()?.sync_all()?;
        let expected = records * RECORD_SIZE as u64;
        let actual = file_size(&staging)?;
        anyhow::ensure!(
            actual == expected,
            "output size mismatch: expected={expected}, actual={actual}"
        );
        publish_staged(&[(&staging, out)])?;
        Ok(stats)
    })();
    if result.is_err() {
        remove_staging(&[staging]);
    }
    result
}

fn output_paths<'a>(
    out_base: Option<&'a Path>,
    out_scores: Option<&'a Path>,
    out_mask: Option<&'a Path>,
) -> Vec<&'a Path> {
    [out_base, out_scores, out_mask].into_iter().flatten().collect()
}

fn check_extract_paths(dual: &Path, outputs: &[&Path], staging_paths: &[&Path]) -> Result<()> {
    anyhow::ensure!(!outputs.is_empty(), "少なくとも 1 つの出力を指定してください");
    let all_paths: Vec<&Path> = outputs.iter().chain(staging_paths).copied().collect();
    for path in &all_paths {
        ensure_safe_output_path(path, dual)?;
    }
    for i in 0..all_paths.len() {
        for j in i + 1..all_paths.len() {
            ensure_distinct_output_paths(all_paths[i], all_paths[j])?;
        }
    }
    Ok(())
}

fn create_writer(path: Option<&Path>) -> Result<Option<BufWriter<File>>> {
    path.map(|path| {
        File::create(path)
            .with_context(|| format!("{} を作成できません", path.display()))
            .map(|file| BufWriter::with_capacity(BUFFER_SIZE, file))
    })
    .transpose()
}

fn extract(
    dual: &Path,
    out_base: Option<&Path>,
    out_scores: Option<&Path>,
    out_mask: Option<&Path>,
) -> Result<ExtractStats> {
    extract_with_chunk_records(dual, out_base, out_scores, out_mask, CHUNK_RECORDS)
}

fn extract_with_chunk_records(
    dual: &Path,
    out_base: Option<&Path>,
    out_scores: Option<&Path>,
    out_mask: Option<&Path>,
    chunk_records: usize,
) -> Result<ExtractStats> {
    anyhow::ensure!(
        chunk_records > 0 && chunk_records.is_multiple_of(8),
        "chunk_records must be a positive multiple of 8"
    );
    let records = checked_psv_records(dual)?;
    anyhow::ensure!(records > 0, "{} が空です (0 レコード)", dual.display());
    let outputs = output_paths(out_base, out_scores, out_mask);
    let base_staging = out_base.map(partial_path);
    let scores_staging = out_scores.map(partial_path);
    let mask_staging = out_mask.map(partial_path);
    let staging_paths =
        output_paths(base_staging.as_deref(), scores_staging.as_deref(), mask_staging.as_deref());
    check_extract_paths(dual, &outputs, &staging_paths)?;
    let owned_staging_paths: Vec<PathBuf> =
        staging_paths.iter().map(|path| path.to_path_buf()).collect();

    let result = (|| {
        let mut reader = BufReader::with_capacity(BUFFER_SIZE, File::open(dual)?);
        let mut base_writer = create_writer(base_staging.as_deref())?;
        let mut score_writer = create_writer(scores_staging.as_deref())?;
        let mut mask_writer = create_writer(mask_staging.as_deref())?;
        // 予測パス比較は case-insensitive filesystem の alias を見逃すため、
        // 作成済み staging の実体でも同一性を検査する (書き込み前)。staging の
        // 作成が別出力の最終パスを実体化させるケースがあるため、最終出力との
        // クロス比較も掛ける。
        ensure_created_paths_distinct(&staging_paths)?;
        ensure_no_entity_overlap(&staging_paths, &outputs)?;
        let mut dual_chunk = Vec::with_capacity(chunk_records * RECORD_SIZE);
        let mut score_chunk = Vec::with_capacity(chunk_records * 2);
        let mut mask_chunk = Vec::with_capacity(chunk_records.div_ceil(8));
        let mut stats = ExtractStats::default();

        while stats.records < records {
            let current_records = (records - stats.records).min(chunk_records as u64) as usize;
            read_bytes(&mut reader, &mut dual_chunk, current_records * RECORD_SIZE)?;

            for (offset, record) in dual_chunk.as_chunks::<RECORD_SIZE>().0.iter().enumerate() {
                let row = stats.records + offset as u64;
                anyhow::ensure!(
                    record[PADDING_OFFSET] & !1 == 0,
                    "dual padding bit1-7 is non-zero at row {row}: 0x{:02x}",
                    record[PADDING_OFFSET]
                );
            }

            if let Some(writer) = &mut score_writer {
                score_chunk.clear();
                score_chunk.reserve(current_records * 2);
                for record in dual_chunk.as_chunks::<RECORD_SIZE>().0 {
                    score_chunk.extend_from_slice(&record[DL_SCORE_OFFSET..DL_SCORE_OFFSET + 2]);
                }
                writer.write_all(&score_chunk)?;
            }
            if let Some(writer) = &mut mask_writer {
                mask_chunk.clear();
                mask_chunk.resize(current_records.div_ceil(8), 0);
                for (offset, record) in dual_chunk.as_chunks::<RECORD_SIZE>().0.iter().enumerate() {
                    mask_chunk[offset / 8] |= (record[PADDING_OFFSET] & 1) << (offset % 8);
                }
                writer.write_all(&mask_chunk)?;
            }
            // score / mask の gather 後なら dual_chunk を直接 base 化してよい
            // (chunk 全量の複製を避ける)。
            if let Some(writer) = &mut base_writer {
                for record in dual_chunk.as_chunks_mut::<RECORD_SIZE>().0 {
                    record[DL_SCORE_OFFSET..DL_SCORE_OFFSET + 2].fill(0);
                    record[PADDING_OFFSET] = 0;
                }
                writer.write_all(&dual_chunk)?;
            }
            stats.records += current_records as u64;
        }

        for writer in [base_writer, score_writer, mask_writer].into_iter().flatten() {
            writer.into_inner()?.sync_all()?;
        }

        if let Some(path) = &base_staging {
            let expected = records * RECORD_SIZE as u64;
            anyhow::ensure!(file_size(path)? == expected, "base output size mismatch");
        }
        if let Some(path) = &scores_staging {
            let expected = records * 2;
            anyhow::ensure!(file_size(path)? == expected, "score output size mismatch");
        }
        if let Some(path) = &mask_staging {
            let expected = records.div_ceil(8);
            anyhow::ensure!(file_size(path)? == expected, "mask output size mismatch");
        }

        let publish_outputs: Vec<(&Path, &Path)> = [
            base_staging.as_deref().zip(out_base),
            scores_staging.as_deref().zip(out_scores),
            mask_staging.as_deref().zip(out_mask),
        ]
        .into_iter()
        .flatten()
        .collect();
        publish_staged(&publish_outputs)?;
        Ok(stats)
    })();
    if result.is_err() {
        remove_staging(&owned_staging_paths);
    }
    result
}

fn move16_is_legal(pos: &Position, move16: u16) -> bool {
    let mv = psv_move16_to_move(move16);
    !mv.is_none() && is_legal_psv_move(pos, mv)
}

/// `floor(i * total / count)` (`i=0..count`) で選ばれる等間隔行か判定する。
fn is_sampled_row(row: u64, total: u64, count: u64) -> bool {
    if count == 0 || total == 0 {
        return false;
    }
    if count >= total {
        return true;
    }
    let row = u128::from(row);
    let total = u128::from(total);
    let count = u128::from(count);
    let sample_index = (row * count).div_ceil(total);
    sample_index < count && sample_index * total / count == row
}

fn check_record(
    record: &[u8],
    row: u64,
    records: u64,
    sample_records: u64,
    dl_abs_max: u32,
) -> RecordCheck {
    let padding = record[PADDING_OFFSET];
    let dl_score = i16::from_le_bytes([record[DL_SCORE_OFFSET], record[DL_SCORE_OFFSET + 1]]);
    let sampled = is_sampled_row(row, records, sample_records);
    let mut check = RecordCheck {
        sampled,
        reserved_padding_nonzero: padding & !1 != 0,
        dl_abs_exceeded: i32::from(dl_score).unsigned_abs() > dl_abs_max,
        ..RecordCheck::default()
    };
    if !sampled {
        return check;
    }

    let psv = PackedSfenValue::from_bytes(record).expect("固定長レコード");
    let Ok(parts) = unpack_sfen_to_parts(&psv.sfen) else {
        check.decode_error = true;
        return check;
    };
    let mut pos = Position::new();
    if pos.set_from_parts(&parts.board, &parts.hands, parts.side_to_move).is_err() {
        check.decode_error = true;
        return check;
    }
    let expected_entered = classify(&pos) == ENTERED_TIER;
    check.entered_mismatch = (padding & 1 != 0) != expected_entered;
    check.move_like = move16_is_legal(
        &pos,
        u16::from_le_bytes([record[DL_SCORE_OFFSET], record[DL_SCORE_OFFSET + 1]]),
    );
    check
}

fn scan_dual(path: &Path, config: ValidateConfig) -> Result<ValidationStats> {
    anyhow::ensure!(
        config.max_move_like_frac.is_finite() && (0.0..=1.0).contains(&config.max_move_like_frac),
        "--max-move-like-frac は 0.0..=1.0 の有限値で指定してください"
    );
    let size = file_size(path)?;
    let records = size / RECORD_SIZE as u64;
    let sample_records = config.sample.unwrap_or(records).min(records);
    let mut reader = BufReader::with_capacity(BUFFER_SIZE, File::open(path)?);
    let mut chunk = Vec::with_capacity(CHUNK_RECORDS * RECORD_SIZE);
    let mut stats = ValidationStats {
        records,
        trailing_bytes: size % RECORD_SIZE as u64,
        ..ValidationStats::default()
    };
    let mut first_row = 0u64;

    while first_row < records {
        let chunk_records = (records - first_row).min(CHUNK_RECORDS as u64) as usize;
        read_bytes(&mut reader, &mut chunk, chunk_records * RECORD_SIZE)?;
        let checks: Vec<RecordCheck> = chunk
            .par_chunks_exact(RECORD_SIZE)
            .enumerate()
            .map(|(offset, record)| {
                check_record(
                    record,
                    first_row + offset as u64,
                    records,
                    sample_records,
                    config.dl_abs_max,
                )
            })
            .collect();
        for (offset, check) in checks.into_iter().enumerate() {
            let row = first_row + offset as u64;
            if check.sampled {
                stats.sampled_records += 1;
            }
            if check.reserved_padding_nonzero {
                stats.reserved_padding_nonzero += 1;
                stats.first_reserved_padding_row.get_or_insert(row);
            }
            if check.entered_mismatch {
                stats.entered_mismatches += 1;
                stats.first_entered_mismatch_row.get_or_insert(row);
            }
            if check.dl_abs_exceeded {
                stats.dl_abs_exceeded += 1;
                stats.first_dl_abs_exceeded_row.get_or_insert(row);
            }
            if check.move_like {
                stats.move_like += 1;
            }
            if check.decode_error {
                stats.decode_errors += 1;
                stats.first_decode_error_row.get_or_insert(row);
            }
        }
        first_row += chunk_records as u64;
    }
    Ok(stats)
}

fn move_like_fraction(stats: &ValidationStats) -> f64 {
    if stats.sampled_records == 0 {
        0.0
    } else {
        stats.move_like as f64 / stats.sampled_records as f64
    }
}

fn validation_failures(stats: &ValidationStats, config: ValidateConfig) -> Vec<String> {
    let mut failures = Vec::new();
    if stats.records == 0 {
        failures.push("レコードが 0 件です".to_owned());
    }
    if stats.trailing_bytes != 0 {
        failures.push(format!("末尾に {} byte の端数があります", stats.trailing_bytes));
    }
    if stats.reserved_padding_nonzero != 0 {
        failures.push(format!(
            "padding bit1-7 非ゼロ: {} 行（先頭 row {}）",
            stats.reserved_padding_nonzero,
            stats.first_reserved_padding_row.expect("count is non-zero")
        ));
    }
    if stats.entered_mismatches != 0 {
        failures.push(format!(
            "entered bit 不一致: {} 行（先頭 row {}）",
            stats.entered_mismatches,
            stats.first_entered_mismatch_row.expect("count is non-zero")
        ));
    }
    if stats.dl_abs_exceeded != 0 {
        failures.push(format!(
            "|DL score| > {}: {} 行（先頭 row {}）",
            config.dl_abs_max,
            stats.dl_abs_exceeded,
            stats.first_dl_abs_exceeded_row.expect("count is non-zero")
        ));
    }
    if stats.decode_errors != 0 {
        failures.push(format!(
            "局面 decode エラー: {} 行（先頭 row {}）",
            stats.decode_errors,
            stats.first_decode_error_row.expect("count is non-zero")
        ));
    }
    let fraction = move_like_fraction(stats);
    if stats.sampled_records != 0 && fraction > config.max_move_like_frac {
        failures
            .push(format!("move-like fraction {:.6} > {:.6}", fraction, config.max_move_like_frac));
    }
    failures
}

fn print_validation_stats(stats: &ValidationStats, config: ValidateConfig, passed: bool) {
    println!("status={}", if passed { "PASS" } else { "FAIL" });
    println!("records={}", stats.records);
    println!("trailing_bytes={}", stats.trailing_bytes);
    println!("sampled_records={}", stats.sampled_records);
    println!("reserved_padding_nonzero={}", stats.reserved_padding_nonzero);
    println!("entered_mismatches={}", stats.entered_mismatches);
    println!("dl_abs_exceeded={} (limit={})", stats.dl_abs_exceeded, config.dl_abs_max);
    println!("decode_errors={}", stats.decode_errors);
    println!(
        "move_like={} fraction={:.6} (fail_above={:.6})",
        stats.move_like,
        move_like_fraction(stats),
        config.max_move_like_frac
    );
}

fn validate(path: &Path, config: ValidateConfig) -> Result<ValidationStats> {
    let stats = scan_dual(path, config)?;
    let failures = validation_failures(&stats, config);
    print_validation_stats(&stats, config, failures.is_empty());
    anyhow::ensure!(failures.is_empty(), "{}", failures.join("; "));
    Ok(stats)
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::DumpScores { base, out_scores } => {
            let stats = dump_scores(&base, &out_scores)?;
            println!("records={}", stats.records);
        }
        Command::Embed {
            base,
            scores,
            mask,
            out,
        } => {
            let stats = embed(&base, &scores, &mask, &out)?;
            println!("records={}", stats.records);
            println!("overwritten_nonzero_move16={}", stats.overwritten_nonzero_move16);
            println!("overwritten_nonzero_padding={}", stats.overwritten_nonzero_padding);
        }
        Command::Extract {
            dual,
            out_base,
            out_scores,
            out_mask,
        } => {
            let stats =
                extract(&dual, out_base.as_deref(), out_scores.as_deref(), out_mask.as_deref())?;
            println!("records={}", stats.records);
        }
        Command::Validate {
            dual,
            sample,
            dl_abs_max,
            max_move_like_frac,
        } => {
            validate(
                &dual,
                ValidateConfig {
                    sample,
                    dl_abs_max,
                    max_move_like_frac,
                },
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rshogi_core::movegen::{MoveList, generate_legal_all};
    use tempfile::tempdir;
    use tools::packed_sfen::{move_to_psv_move16, pack_position};

    #[cfg(unix)]
    fn symlink_file(original: &Path, link: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(original, link)
    }

    #[cfg(windows)]
    fn symlink_file(original: &Path, link: &Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_file(original, link)
    }

    fn record(sfen: &str, score: i16, move16: u16, padding: u8) -> Result<[u8; RECORD_SIZE]> {
        let mut pos = Position::new();
        pos.set_sfen(sfen)?;
        Ok(PackedSfenValue {
            sfen: pack_position(&pos),
            score,
            move16,
            game_ply: 1,
            game_result: 0,
            padding,
        }
        .to_bytes())
    }

    fn base_records(count: usize, move16: u16) -> Result<Vec<u8>> {
        let sfens = [
            "4K3k/9/9/9/9/9/9/9/9 b - 1",
            "4k4/9/9/9/4K4/9/9/9/9 b - 1",
            "4k4/9/9/9/9/9/9/9/4K4 b - 1",
        ];
        let mut bytes = Vec::with_capacity(count * RECORD_SIZE);
        for i in 0..count {
            bytes.extend_from_slice(&record(sfens[i % 3], i as i16, move16, 0)?);
        }
        Ok(bytes)
    }

    fn sidecars(count: usize, dl_score: i16) -> (Vec<u8>, Vec<u8>) {
        let mut scores = Vec::with_capacity(count * 2);
        let mut mask = vec![0u8; count.div_ceil(8)];
        for i in 0..count {
            scores.extend_from_slice(&dl_score.to_le_bytes());
            if i % 3 == 0 {
                mask[i / 8] |= 1 << (i % 8);
            }
        }
        (scores, mask)
    }

    fn write_embed_inputs(
        dir: &Path,
        count: usize,
        move16: u16,
        dl_score: i16,
    ) -> Result<(PathBuf, PathBuf, PathBuf)> {
        let base = dir.join("base.psv");
        let scores = dir.join("dl.i16");
        let mask = dir.join("entered.bits");
        let (score_bytes, mask_bytes) = sidecars(count, dl_score);
        fs::write(&base, base_records(count, move16)?)?;
        fs::write(&scores, score_bytes)?;
        fs::write(&mask, mask_bytes)?;
        Ok((base, scores, mask))
    }

    fn valid_config() -> ValidateConfig {
        ValidateConfig {
            sample: None,
            dl_abs_max: DEFAULT_DL_ABS_MAX,
            max_move_like_frac: DEFAULT_MAX_MOVE_LIKE_FRAC,
        }
    }

    #[test]
    fn embed_extract_roundtrip_at_bitmap_boundaries() -> Result<()> {
        for count in [7, 8, 9, 17] {
            let dir = tempdir()?;
            let (base, scores, mask) = write_embed_inputs(dir.path(), count, 0, -1)?;
            let dual = dir.path().join("dual.psv");
            let extracted_base = dir.path().join("extracted.psv");
            let extracted_scores = dir.path().join("extracted.i16");
            let extracted_mask = dir.path().join("extracted.bits");
            embed(&base, &scores, &mask, &dual)?;
            extract(&dual, Some(&extracted_base), Some(&extracted_scores), Some(&extracted_mask))?;
            assert_eq!(fs::read(&extracted_base)?, fs::read(&base)?);
            assert_eq!(fs::read(&extracted_scores)?, fs::read(&scores)?);
            assert_eq!(fs::read(&extracted_mask)?, fs::read(&mask)?);
        }
        Ok(())
    }

    #[test]
    fn dump_embed_extract_scores_roundtrip() -> Result<()> {
        let dir = tempdir()?;
        let base = dir.path().join("plain.psv");
        let dumped_scores = dir.path().join("dumped.i16");
        let mask = dir.path().join("entered.bits");
        let dual = dir.path().join("dual.psv");
        let extracted_scores = dir.path().join("extracted.i16");
        fs::write(&base, base_records(17, 0)?)?;
        fs::write(&mask, sidecars(17, 0).1)?;

        let stats = dump_scores(&base, &dumped_scores)?;
        embed(&base, &dumped_scores, &mask, &dual)?;
        extract(&dual, None, Some(&extracted_scores), None)?;

        assert_eq!(stats.records, 17);
        assert_eq!(fs::read(&extracted_scores)?, fs::read(&dumped_scores)?);
        Ok(())
    }

    #[test]
    fn dump_scores_handles_empty_single_and_chunk_boundary_inputs() -> Result<()> {
        const TEST_CHUNK_RECORDS: usize = 3;
        for count in [0, 1, TEST_CHUNK_RECORDS + 1] {
            let dir = tempdir()?;
            let base = dir.path().join("plain.psv");
            let output = dir.path().join("scores.i16");
            fs::write(&base, base_records(count, 0x5678)?)?;

            let stats = dump_scores_with_chunk_records(&base, &output, TEST_CHUNK_RECORDS)?;

            assert_eq!(stats.records, count as u64);
            let actual = fs::read(output)?;
            let expected: Vec<u8> =
                (0..count).flat_map(|score| (score as i16).to_le_bytes()).collect();
            assert_eq!(actual, expected);
        }
        Ok(())
    }

    #[test]
    fn dump_scores_reads_score_column_little_endian_golden() -> Result<()> {
        let dir = tempdir()?;
        let base = dir.path().join("plain.psv");
        let output = dir.path().join("scores.i16");
        fs::write(&base, record("4k4/9/9/9/9/9/9/9/4K4 b - 1", 0x1234, 0x5678, 0)?)?;

        dump_scores(&base, &output)?;

        assert_eq!(fs::read(output)?, [0x34, 0x12]);
        Ok(())
    }

    #[test]
    fn dump_scores_rejects_trailing_bytes_without_output() -> Result<()> {
        let dir = tempdir()?;
        let base = dir.path().join("plain.psv");
        let output = dir.path().join("scores.i16");
        fs::write(&base, [0u8; RECORD_SIZE + 1])?;

        assert!(dump_scores(&base, &output).is_err());
        assert!(!output.exists());
        assert!(!partial_path(&output).exists());
        Ok(())
    }

    #[test]
    fn dump_scores_rejects_output_equal_to_base_without_truncating() -> Result<()> {
        let dir = tempdir()?;
        let base = dir.path().join("plain.psv");
        fs::write(&base, base_records(1, 0)?)?;
        let original = fs::read(&base)?;

        assert!(dump_scores(&base, &base).is_err());
        assert_eq!(fs::read(base)?, original);
        Ok(())
    }

    #[test]
    fn dump_scores_base_named_like_staging_is_rejected_without_deleting_input() -> Result<()> {
        let dir = tempdir()?;
        let out = dir.path().join("scores.i16");
        let base = partial_path(&out);
        let original = base_records(1, 0)?;
        fs::write(&base, &original)?;

        assert!(dump_scores(&base, &out).is_err());
        assert_eq!(fs::read(&base)?, original);
        Ok(())
    }

    #[test]
    fn dump_scores_error_preserves_existing_output_and_removes_staging() -> Result<()> {
        let dir = tempdir()?;
        let base = dir.path().join("plain.psv");
        let output = dir.path().join("scores.i16");
        let sentinel = b"existing scores";
        fs::write(&base, [0u8; RECORD_SIZE + 1])?;
        fs::write(&output, sentinel)?;
        fs::write(partial_path(&output), b"stale partial")?;

        assert!(dump_scores(&base, &output).is_err());
        assert_eq!(fs::read(&output)?, sentinel);
        assert!(!partial_path(&output).exists());
        Ok(())
    }

    #[test]
    fn embed_writes_dl_score_little_endian_golden() -> Result<()> {
        // roundtrip は両方向を同じ実装で処理するため byte swap を検出できない。
        // 生成物の生 byte を golden 値で直接固定する。
        let dir = tempdir()?;
        let (base, scores, mask) = write_embed_inputs(dir.path(), 1, 0, 0x1234)?;
        fs::write(&mask, [1u8])?;
        let dual = dir.path().join("dual.psv");
        embed(&base, &scores, &mask, &dual)?;
        let bytes = fs::read(&dual)?;
        assert_eq!(&bytes[DL_SCORE_OFFSET..DL_SCORE_OFFSET + 2], &[0x34, 0x12]);
        assert_eq!(bytes[PADDING_OFFSET], 1);

        let extracted_scores = dir.path().join("extracted.i16");
        extract(&dual, None, Some(&extracted_scores), None)?;
        assert_eq!(fs::read(&extracted_scores)?, vec![0x34, 0x12]);
        Ok(())
    }

    #[test]
    fn embed_and_extract_reject_empty_inputs() -> Result<()> {
        let dir = tempdir()?;
        let base = dir.path().join("base.psv");
        let scores = dir.path().join("dl.i16");
        let mask = dir.path().join("entered.bits");
        let dual = dir.path().join("dual.psv");
        for path in [&base, &scores, &mask, &dual] {
            fs::write(path, [])?;
        }
        assert!(embed(&base, &scores, &mask, &dir.path().join("out.psv")).is_err());
        assert!(extract(&dual, Some(&dir.path().join("out.psv")), None, None).is_err());
        Ok(())
    }

    #[test]
    fn validate_passes_clean_file_at_zero_move_like_threshold() -> Result<()> {
        let dir = tempdir()?;
        let (base, scores, mask) = write_embed_inputs(dir.path(), 3, 0, -1)?;
        let dual = dir.path().join("dual.psv");
        embed(&base, &scores, &mask, &dual)?;
        let config = ValidateConfig {
            max_move_like_frac: 0.0,
            ..valid_config()
        };
        let stats = validate(&dual, config)?;
        assert_eq!(stats.move_like, 0);
        Ok(())
    }

    #[test]
    fn embed_extract_roundtrip_across_streaming_chunk_boundary() -> Result<()> {
        const TEST_CHUNK_RECORDS: usize = 16;
        for count in [
            TEST_CHUNK_RECORDS - 1,
            TEST_CHUNK_RECORDS,
            TEST_CHUNK_RECORDS + 1,
        ] {
            let dir = tempdir()?;
            let (base, scores, mask) = write_embed_inputs(dir.path(), count, 0, -1)?;
            let dual = dir.path().join("dual.psv");
            let extracted_base = dir.path().join("extracted.psv");
            let extracted_scores = dir.path().join("extracted.i16");
            let extracted_mask = dir.path().join("extracted.bits");
            embed_with_chunk_records(&base, &scores, &mask, &dual, TEST_CHUNK_RECORDS)?;
            extract_with_chunk_records(
                &dual,
                Some(&extracted_base),
                Some(&extracted_scores),
                Some(&extracted_mask),
                TEST_CHUNK_RECORDS,
            )?;
            assert_eq!(fs::read(&extracted_base)?, fs::read(&base)?);
            assert_eq!(fs::read(&extracted_scores)?, fs::read(&scores)?);
            assert_eq!(fs::read(&extracted_mask)?, fs::read(&mask)?);
        }
        Ok(())
    }

    #[test]
    fn embed_counts_overwritten_nonzero_move16() -> Result<()> {
        let dir = tempdir()?;
        let (base, scores, mask) = write_embed_inputs(dir.path(), 3, 42, -1)?;
        let stats = embed(&base, &scores, &mask, &dir.path().join("dual.psv"))?;
        assert_eq!(stats.overwritten_nonzero_move16, 3);
        Ok(())
    }

    #[test]
    fn embed_rejects_score_size_mismatch() -> Result<()> {
        let dir = tempdir()?;
        let (base, scores, mask) = write_embed_inputs(dir.path(), 3, 0, -1)?;
        fs::write(&scores, [0u8; 5])?;
        assert!(embed(&base, &scores, &mask, &dir.path().join("dual.psv")).is_err());
        Ok(())
    }

    #[test]
    fn embed_rejects_mask_size_mismatch() -> Result<()> {
        let dir = tempdir()?;
        let (base, scores, mask) = write_embed_inputs(dir.path(), 9, 0, -1)?;
        fs::write(&mask, [0u8; 1])?;
        assert!(embed(&base, &scores, &mask, &dir.path().join("dual.psv")).is_err());
        Ok(())
    }

    #[test]
    fn embed_rejects_nonzero_unused_mask_bits() -> Result<()> {
        let dir = tempdir()?;
        let (base, scores, mask) = write_embed_inputs(dir.path(), 7, 0, -1)?;
        let mut mask_bytes = fs::read(&mask)?;
        mask_bytes[0] |= 0x80;
        fs::write(&mask, mask_bytes)?;
        assert!(embed(&base, &scores, &mask, &dir.path().join("dual.psv")).is_err());
        Ok(())
    }

    #[test]
    fn embed_overwrites_and_counts_nonzero_base_padding() -> Result<()> {
        let dir = tempdir()?;
        let (base, scores, mask) = write_embed_inputs(dir.path(), 3, 0, -1)?;
        let mut bytes = fs::read(&base)?;
        bytes[PADDING_OFFSET] = 0x02;
        bytes[RECORD_SIZE + PADDING_OFFSET] = 0x03;
        fs::write(&base, bytes)?;
        let dual = dir.path().join("dual.psv");

        let stats = embed(&base, &scores, &mask, &dual)?;

        assert_eq!(stats.overwritten_nonzero_padding, 2);
        let dual_bytes = fs::read(dual)?;
        assert_eq!(dual_bytes[PADDING_OFFSET], 1);
        assert_eq!(dual_bytes[RECORD_SIZE + PADDING_OFFSET], 0);
        assert_eq!(dual_bytes[2 * RECORD_SIZE + PADDING_OFFSET], 0);
        Ok(())
    }

    #[test]
    fn embed_error_preserves_existing_output_and_removes_staging() -> Result<()> {
        let dir = tempdir()?;
        let (base, scores, mask) = write_embed_inputs(dir.path(), 3, 0, -1)?;
        fs::write(&scores, [0u8; 5])?;
        let output = dir.path().join("dual.psv");
        let sentinel = b"existing valid output";
        fs::write(&output, sentinel)?;

        assert!(embed(&base, &scores, &mask, &output).is_err());
        assert_eq!(fs::read(&output)?, sentinel);
        assert!(!partial_path(&output).exists());
        Ok(())
    }

    #[test]
    fn embed_rejects_output_equal_to_input_without_truncating() -> Result<()> {
        let dir = tempdir()?;
        let (base, scores, mask) = write_embed_inputs(dir.path(), 3, 0, -1)?;
        let original = fs::read(&base)?;
        assert!(embed(&base, &scores, &mask, &base).is_err());
        assert_eq!(fs::read(base)?, original);
        Ok(())
    }

    #[test]
    fn embed_rejects_scores_and_mask_as_output_aliases() -> Result<()> {
        let dir = tempdir()?;
        let (base, scores, mask) = write_embed_inputs(dir.path(), 3, 0, -1)?;
        for output in [&scores, &mask] {
            let original = fs::read(output)?;
            assert!(embed(&base, &scores, &mask, output).is_err());
            assert_eq!(fs::read(output)?, original);
        }
        Ok(())
    }

    #[test]
    fn embed_rejects_staging_path_aliasing_input() -> Result<()> {
        let dir = tempdir()?;
        let (_, scores, mask) = write_embed_inputs(dir.path(), 3, 0, -1)?;
        let output = dir.path().join("dual.psv");
        let base = partial_path(&output);
        fs::write(&base, base_records(3, 0)?)?;
        let original = fs::read(&base)?;
        assert!(embed(&base, &scores, &mask, &output).is_err());
        assert_eq!(fs::read(&base)?, original);
        Ok(())
    }

    #[test]
    fn extract_rejects_reserved_padding_bits() -> Result<()> {
        let dir = tempdir()?;
        let dual = dir.path().join("dual.psv");
        fs::write(&dual, record("4k4/9/9/9/9/9/9/9/4K4 b - 1", 0, 1, 2)?)?;
        let error = extract(&dual, Some(&dir.path().join("base.psv")), None, None)
            .expect_err("reserved bit must fail");
        assert!(error.to_string().contains("row 0"));
        Ok(())
    }

    #[test]
    fn extract_error_preserves_all_existing_outputs_and_removes_staging() -> Result<()> {
        let dir = tempdir()?;
        let dual = dir.path().join("dual.psv");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&record("4k4/9/9/9/9/9/9/9/4K4 b - 1", 0, 1, 0)?);
        bytes.extend_from_slice(&record("4k4/9/9/9/9/9/9/9/4K4 b - 1", 0, 1, 2)?);
        fs::write(&dual, bytes)?;
        let outputs = [
            dir.path().join("base.psv"),
            dir.path().join("scores.i16"),
            dir.path().join("mask.bits"),
        ];
        let sentinels: [&[u8]; 3] = [b"base sentinel", b"scores sentinel", b"mask sentinel"];
        for (path, sentinel) in outputs.iter().zip(sentinels) {
            fs::write(path, sentinel)?;
        }

        assert!(extract(&dual, Some(&outputs[0]), Some(&outputs[1]), Some(&outputs[2])).is_err());
        for (path, sentinel) in outputs.iter().zip(sentinels) {
            assert_eq!(fs::read(path)?, sentinel);
            assert!(!partial_path(path).exists());
        }
        Ok(())
    }

    #[test]
    fn extract_publish_failure_keeps_already_published_output() -> Result<()> {
        let dir = tempdir()?;
        let staging_a = dir.path().join("a.partial");
        let staging_b = dir.path().join("b.partial");
        let output_a = dir.path().join("a.out");
        let output_b = dir.path().join("missing-parent").join("b.out");
        fs::write(&output_a, b"sentinel")?;
        fs::write(&staging_a, b"new validated a")?;
        fs::write(&staging_b, b"new validated b")?;

        let error = publish_staged(&[(&staging_a, &output_a), (&staging_b, &output_b)])
            .expect_err("the second publish must fail");

        assert!(!staging_a.exists());
        assert!(!staging_b.exists());
        assert_eq!(fs::read(&output_a)?, b"new validated a");
        assert!(!output_b.exists());
        let message = error.to_string();
        assert!(message.contains("部分 publish 状態"));
        assert!(message.contains("publish 済み最終出力"));
        assert!(message.contains(output_a.to_string_lossy().as_ref()));
        assert!(message.contains("未 publish 最終出力"));
        assert!(message.contains(output_b.to_string_lossy().as_ref()));
        Ok(())
    }

    #[test]
    fn extract_path_aliases_are_rejected_table_driven() -> Result<()> {
        let dir = tempdir()?;
        let dual = dir.path().join("dual.psv");
        fs::write(&dual, [0u8; RECORD_SIZE])?;

        let same = dir.path().join("same.out");
        let hardlink_a = dir.path().join("hardlink-a.out");
        let hardlink_b = dir.path().join("hardlink-b.out");
        fs::write(&hardlink_a, b"output")?;
        fs::hard_link(&hardlink_a, &hardlink_b)?;
        let dual_hardlink = dir.path().join("dual-hardlink.out");
        fs::hard_link(&dual, &dual_hardlink)?;
        let symlink_target = dir.path().join("symlink-target.out");
        let symlink_output = dir.path().join("symlink-output.out");
        fs::write(&symlink_target, b"target")?;

        let mut cases: Vec<(&str, [&Path; 2])> = vec![
            ("same path", [&same, &same]),
            ("output hardlink", [&hardlink_a, &hardlink_b]),
            ("output-dual hardlink", [&dual_hardlink, &same]),
        ];
        match symlink_file(&symlink_target, &symlink_output) {
            Ok(()) => cases.push(("symlink output", [&symlink_output, &same])),
            #[cfg(windows)]
            Err(error) if error.raw_os_error() == Some(1314) => {}
            Err(error) => return Err(error.into()),
        }
        for (name, outputs) in cases {
            assert!(check_extract_paths(&dual, &outputs, &[]).is_err(), "{name}");
        }
        Ok(())
    }

    #[test]
    fn validate_accepts_correct_dual_file() -> Result<()> {
        let dir = tempdir()?;
        let (base, scores, mask) = write_embed_inputs(dir.path(), 17, 0, -1)?;
        let dual = dir.path().join("dual.psv");
        embed(&base, &scores, &mask, &dual)?;
        validate(&dual, valid_config())?;
        Ok(())
    }

    #[test]
    fn validate_rejects_flipped_entered_bit() -> Result<()> {
        let dir = tempdir()?;
        let (base, scores, mask) = write_embed_inputs(dir.path(), 17, 0, -1)?;
        let dual = dir.path().join("dual.psv");
        embed(&base, &scores, &mask, &dual)?;
        let mut bytes = fs::read(&dual)?;
        bytes[PADDING_OFFSET] ^= 1;
        fs::write(&dual, bytes)?;
        assert!(validate(&dual, valid_config()).is_err());
        Ok(())
    }

    #[test]
    fn validate_rejects_normal_psv_as_move_like() -> Result<()> {
        let dir = tempdir()?;
        let input = dir.path().join("normal.psv");
        let mut pos = Position::new();
        pos.set_hirate();
        let mut legal_moves = MoveList::new();
        generate_legal_all(&pos, &mut legal_moves);
        let move16 = move_to_psv_move16(*legal_moves.iter().next().expect("legal move"));
        let psv = PackedSfenValue {
            sfen: pack_position(&pos),
            score: 0,
            move16,
            game_ply: 1,
            game_result: 0,
            padding: 0,
        }
        .to_bytes();
        let mut bytes = Vec::new();
        for _ in 0..20 {
            bytes.extend_from_slice(&psv);
        }
        fs::write(&input, bytes)?;
        assert!(validate(&input, valid_config()).is_err());
        Ok(())
    }

    #[test]
    fn validate_rejects_dl_score_over_limit() -> Result<()> {
        let dir = tempdir()?;
        let (base, scores, mask) = write_embed_inputs(dir.path(), 3, 0, 101)?;
        let dual = dir.path().join("dual.psv");
        embed(&base, &scores, &mask, &dual)?;
        let mut config = valid_config();
        config.dl_abs_max = 100;
        assert!(validate(&dual, config).is_err());
        Ok(())
    }

    #[test]
    fn validate_rejects_empty_file() -> Result<()> {
        let dir = tempdir()?;
        let input = dir.path().join("empty.psv");
        fs::write(&input, [])?;
        let error = validate(&input, valid_config()).expect_err("empty input must fail");
        assert!(error.to_string().contains("0 件"));
        Ok(())
    }

    #[test]
    fn deterministic_sampling_selects_requested_count_and_first_row() {
        for total in 1..30 {
            for requested in 1..=total {
                let selected: Vec<u64> =
                    (0..total).filter(|row| is_sampled_row(*row, total, requested)).collect();
                assert_eq!(selected.len(), requested as usize);
                assert_eq!(selected[0], 0);
            }
        }
    }
}
