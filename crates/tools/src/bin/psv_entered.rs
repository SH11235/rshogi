//! 入玉の行 (どちらかの玉が敵陣三段、`king_zone` の entered tier) を別のラベルで付け直した教師を作る。
//!
//! - `extract`: 入玉の行だけを順に書き出す (40B のまま)。これを別のモデルで付け直す。
//! - `merge-dual`: 元の PSV と付け直した入玉の行から dual-label PSV を 1 回の読みで書く。
//!   入玉の行の base score (offset 32) は付け直した値、全行の offset 34 は元の score (または `--dl-scores` の sidecar)、
//!   offset 39 は入玉なら 1・それ以外は 0 (`psv_dual_label embed` と同じ形)。
//!   付け直した行の score 以外 (sfen・move・ply・result) が元の入玉の行と順に一致しなければ中断し、行数が合わなければ失敗にする。
//!
//! 入玉の判定は packed sfen の玉のマスだけを読む (`king_zone::classify_packed`)。局面を組み立てないので、
//! 数百億行でも読み書きの速さで回る。

use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tools::common::io::{partial_path, sync_directory};
use tools::king_zone::{ENTERED_TIER, classify_packed};
use tools::output_path::{
    ensure_created_paths_distinct, ensure_distinct_output_paths, ensure_no_entity_overlap,
    ensure_safe_output_path,
};

const RECORD_SIZE: usize = 40;
const SCORE_OFFSET: usize = 32;
const DL_SCORE_OFFSET: usize = 34;
const PADDING_OFFSET: usize = 39;
const BUFFER_SIZE: usize = 32 << 20;
const CHUNK_RECORDS: usize = 1 << 20;
// mask の bit 添字を chunk 内 offset で計算するため、chunk 境界を byte 境界に揃える。
const _: () = assert!(CHUNK_RECORDS.is_multiple_of(8));

#[derive(Parser)]
#[command(
    name = "psv_entered",
    about = "入玉の行の抜き出しと、付け直した入玉の行からの dual-label PSV の作成"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 入玉の行だけを順に書き出す
    Extract {
        /// 入力 PSV
        #[arg(long)]
        input: PathBuf,
        /// 入玉の行だけの PSV
        #[arg(long)]
        out: PathBuf,
    },
    /// 元の PSV と付け直した入玉の行から dual-label PSV を書く
    MergeDual {
        /// 元の PSV (score 以外の全フィールドと、入玉でない行の base score の供給元)
        #[arg(long)]
        base: PathBuf,
        /// extract の出力を付け直した PSV (行順は extract のまま)
        #[arg(long)]
        sparse: PathBuf,
        /// offset 34 に入れる little-endian i16 × 行数の sidecar。省略すると元の PSV の score
        #[arg(long)]
        dl_scores: Option<PathBuf>,
        /// 出力 dual-label PSV
        #[arg(long)]
        out: PathBuf,
        /// 入玉の行の bitmap (LSB-first) も書く
        #[arg(long)]
        out_mask: Option<PathBuf>,
    },
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Stats {
    records: u64,
    entered: u64,
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

fn is_entered(record: &[u8], row: u64) -> Result<bool> {
    classify_packed(&record[..32])
        .map(|tier| tier == ENTERED_TIER)
        .map_err(|error| anyhow::anyhow!("row {row}: {error}"))
}

fn read_bytes<R: Read>(reader: &mut R, buffer: &mut Vec<u8>, bytes: usize) -> Result<()> {
    buffer.resize(bytes, 0);
    reader.read_exact(buffer)?;
    Ok(())
}

/// 出力ごとの staging を作って書き、全部書けたら rename で公開する。失敗したら staging を消す。
struct Staged {
    pairs: Vec<(PathBuf, PathBuf)>,
}

impl Staged {
    fn new(outputs: &[&Path], inputs: &[&Path]) -> Result<Self> {
        let pairs: Vec<(PathBuf, PathBuf)> =
            outputs.iter().map(|out| (partial_path(out), out.to_path_buf())).collect();
        let mut all: Vec<&Path> = Vec::new();
        for (staging, out) in &pairs {
            all.push(staging);
            all.push(out);
        }
        for path in &all {
            for input in inputs {
                ensure_safe_output_path(path, input)?;
            }
        }
        for i in 0..all.len() {
            for j in i + 1..all.len() {
                ensure_distinct_output_paths(all[i], all[j])?;
            }
        }
        Ok(Self { pairs })
    }

    fn create(&self) -> Result<Vec<BufWriter<File>>> {
        let mut writers = Vec::new();
        for (staging, _) in &self.pairs {
            let file = File::create(staging)
                .with_context(|| format!("{} を作成できません", staging.display()))?;
            writers.push(BufWriter::with_capacity(BUFFER_SIZE, file));
        }
        let stagings: Vec<&Path> = self.pairs.iter().map(|(s, _)| s.as_path()).collect();
        let outputs: Vec<&Path> = self.pairs.iter().map(|(_, o)| o.as_path()).collect();
        ensure_created_paths_distinct(&stagings)?;
        ensure_no_entity_overlap(&stagings, &outputs)?;
        Ok(writers)
    }

    fn publish(&self, writers: Vec<BufWriter<File>>, expected_sizes: &[u64]) -> Result<()> {
        for writer in writers {
            writer.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        }
        for ((staging, _), &expected) in self.pairs.iter().zip(expected_sizes) {
            let actual = file_size(staging)?;
            anyhow::ensure!(
                actual == expected,
                "{} のサイズが合いません: expected={expected}, actual={actual}",
                staging.display()
            );
        }
        for (staging, out) in &self.pairs {
            fs::rename(staging, out).with_context(|| {
                format!("{} -> {} の公開に失敗", staging.display(), out.display())
            })?;
        }
        for (_, out) in &self.pairs {
            let parent =
                out.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
            sync_directory(parent)?;
        }
        Ok(())
    }

    fn cleanup(&self) {
        for (staging, _) in &self.pairs {
            if let Err(error) = fs::remove_file(staging)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                eprintln!("staging file cleanup failed: {}: {error}", staging.display());
            }
        }
    }
}

fn run_staged<F>(staged: &Staged, body: F) -> Result<Stats>
where
    F: FnOnce(&mut [BufWriter<File>]) -> Result<(Stats, Vec<u64>)>,
{
    let result = (|| {
        let mut writers = staged.create()?;
        let (stats, sizes) = body(&mut writers)?;
        staged.publish(writers, &sizes)?;
        Ok(stats)
    })();
    if result.is_err() {
        staged.cleanup();
    }
    result
}

fn extract(input: &Path, out: &Path) -> Result<Stats> {
    extract_with_chunk_records(input, out, CHUNK_RECORDS)
}

fn extract_with_chunk_records(input: &Path, out: &Path, chunk_records: usize) -> Result<Stats> {
    let records = checked_psv_records(input)?;
    let staged = Staged::new(&[out], &[input])?;
    run_staged(&staged, |writers| {
        let mut reader = BufReader::with_capacity(BUFFER_SIZE, File::open(input)?);
        let mut chunk = Vec::with_capacity(chunk_records * RECORD_SIZE);
        let mut stats = Stats::default();
        while stats.records < records {
            let n = (records - stats.records).min(chunk_records as u64) as usize;
            read_bytes(&mut reader, &mut chunk, n * RECORD_SIZE)?;
            for (offset, record) in chunk.as_chunks::<RECORD_SIZE>().0.iter().enumerate() {
                if is_entered(record, stats.records + offset as u64)? {
                    writers[0].write_all(record)?;
                    stats.entered += 1;
                }
            }
            stats.records += n as u64;
        }
        let sizes = vec![stats.entered * RECORD_SIZE as u64];
        Ok((stats, sizes))
    })
}

fn merge_dual(
    base: &Path,
    sparse: &Path,
    dl_scores: Option<&Path>,
    out: &Path,
    out_mask: Option<&Path>,
) -> Result<Stats> {
    merge_dual_with_chunk_records(base, sparse, dl_scores, out, out_mask, CHUNK_RECORDS)
}

fn merge_dual_with_chunk_records(
    base: &Path,
    sparse: &Path,
    dl_scores: Option<&Path>,
    out: &Path,
    out_mask: Option<&Path>,
    chunk_records: usize,
) -> Result<Stats> {
    anyhow::ensure!(
        chunk_records > 0 && chunk_records.is_multiple_of(8),
        "chunk_records must be a positive multiple of 8"
    );
    let records = checked_psv_records(base)?;
    let sparse_records = checked_psv_records(sparse)?;
    if let Some(scores) = dl_scores {
        let size = file_size(scores)?;
        anyhow::ensure!(
            size == records * 2,
            "{} のサイズ {size} byte が行数 {records} × 2 と合いません",
            scores.display()
        );
    }
    let mut inputs = vec![base, sparse];
    inputs.extend(dl_scores);
    let mut outputs = vec![out];
    outputs.extend(out_mask);
    let staged = Staged::new(&outputs, &inputs)?;
    run_staged(&staged, |writers| {
        let mut base_reader = BufReader::with_capacity(BUFFER_SIZE, File::open(base)?);
        let mut sparse_reader = BufReader::with_capacity(BUFFER_SIZE, File::open(sparse)?);
        let mut score_reader = dl_scores
            .map(|p| File::open(p).map(|f| BufReader::with_capacity(BUFFER_SIZE, f)))
            .transpose()?;
        let mut chunk = Vec::with_capacity(chunk_records * RECORD_SIZE);
        let mut scores = Vec::with_capacity(chunk_records * 2);
        let mut mask = vec![0u8; chunk_records / 8];
        let mut sparse_record = [0u8; RECORD_SIZE];
        let mut stats = Stats::default();
        while stats.records < records {
            let n = (records - stats.records).min(chunk_records as u64) as usize;
            read_bytes(&mut base_reader, &mut chunk, n * RECORD_SIZE)?;
            if let Some(reader) = score_reader.as_mut() {
                read_bytes(reader, &mut scores, n * 2)?;
            }
            mask.fill(0);
            for (offset, record) in chunk.as_chunks_mut::<RECORD_SIZE>().0.iter_mut().enumerate() {
                let row = stats.records + offset as u64;
                let entered = is_entered(record, row)?;
                let dl = match score_reader {
                    Some(_) => [scores[offset * 2], scores[offset * 2 + 1]],
                    None => [record[SCORE_OFFSET], record[SCORE_OFFSET + 1]],
                };
                if entered {
                    anyhow::ensure!(
                        stats.entered < sparse_records,
                        "{} の行が足りません (元の行 {row} で尽きた)",
                        sparse.display()
                    );
                    sparse_reader.read_exact(&mut sparse_record)?;
                    // score (32..34) と padding (39) は比べない
                    anyhow::ensure!(
                        sparse_record[..SCORE_OFFSET] == record[..SCORE_OFFSET]
                            && sparse_record[DL_SCORE_OFFSET..PADDING_OFFSET]
                                == record[DL_SCORE_OFFSET..PADDING_OFFSET],
                        "付け直した行 {} が元の行 {row} と一致しません",
                        stats.entered
                    );
                    record[SCORE_OFFSET..SCORE_OFFSET + 2]
                        .copy_from_slice(&sparse_record[SCORE_OFFSET..SCORE_OFFSET + 2]);
                    mask[offset / 8] |= 1 << (offset % 8);
                    stats.entered += 1;
                }
                record[DL_SCORE_OFFSET..DL_SCORE_OFFSET + 2].copy_from_slice(&dl);
                record[PADDING_OFFSET] = u8::from(entered);
            }
            writers[0].write_all(&chunk)?;
            if out_mask.is_some() {
                writers[1].write_all(&mask[..n.div_ceil(8)])?;
            }
            stats.records += n as u64;
        }
        anyhow::ensure!(
            stats.entered == sparse_records,
            "{} は {sparse_records} 行あるが、元の入玉の行は {} 行",
            sparse.display(),
            stats.entered
        );
        let mut sizes = vec![records * RECORD_SIZE as u64];
        if out_mask.is_some() {
            sizes.push(records.div_ceil(8));
        }
        Ok((stats, sizes))
    })
}

fn main() -> Result<()> {
    let stats = match Cli::parse().command {
        Command::Extract { input, out } => extract(&input, &out)?,
        Command::MergeDual {
            base,
            sparse,
            dl_scores,
            out,
            out_mask,
        } => merge_dual(&base, &sparse, dl_scores.as_deref(), &out, out_mask.as_deref())?,
    };
    println!("records={}", stats.records);
    println!("entered={}", stats.entered);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rshogi_core::position::Position;
    use tempfile::tempdir;
    use tools::packed_sfen::{PackedSfenValue, pack_position};

    // 先手玉一段目 (入玉)、玉が中段 (入玉でない)、後手玉九段目 (入玉) の繰り返し
    const SFENS: [&str; 3] = [
        "4K3k/9/9/9/9/9/9/9/9 b - 1",
        "4k4/9/9/9/4K4/9/9/9/9 b - 1",
        "9/9/9/9/4K4/9/9/9/4k4 b - 1",
    ];

    fn record(sfen: &str, score: i16, move16: u16, padding: u8) -> [u8; RECORD_SIZE] {
        let mut pos = Position::new();
        pos.set_sfen(sfen).unwrap();
        PackedSfenValue {
            sfen: pack_position(&pos),
            score,
            move16,
            game_ply: 7,
            game_result: 1,
            padding,
        }
        .to_bytes()
    }

    fn base_rows(count: usize) -> Vec<u8> {
        (0..count).flat_map(|i| record(SFENS[i % 3], i as i16, 0x1234, 3)).collect()
    }

    fn entered(i: usize) -> bool {
        i % 3 != 1
    }

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, bytes).unwrap();
        path
    }

    // extract の出力の score を付け直す (score = -1000 - 元の行番号)
    fn relabel(extracted: &[u8], count: usize) -> Vec<u8> {
        let rows: Vec<usize> = (0..count).filter(|&i| entered(i)).collect();
        let mut out = extracted.to_vec();
        for (k, rec) in out.as_chunks_mut::<RECORD_SIZE>().0.iter_mut().enumerate() {
            rec[32..34].copy_from_slice(&(-1000 - rows[k] as i16).to_le_bytes());
        }
        out
    }

    #[test]
    fn extract_then_merge_dual_writes_embed_layout() {
        for count in [1, 7, 8, 9, 16, 17, 25] {
            for chunk in [8, 16, 1 << 20] {
                let dir = tempdir().unwrap();
                let base = write(dir.path(), "base.psv", &base_rows(count));
                let ext = dir.path().join("ext.psv");
                let s = extract_with_chunk_records(&base, &ext, chunk).unwrap();
                assert_eq!(s.entered as usize, (0..count).filter(|&i| entered(i)).count());
                let sparse = write(dir.path(), "x.psv", &relabel(&fs::read(&ext).unwrap(), count));
                let out = dir.path().join("dual.psv");
                let mask = dir.path().join("mask.bits");
                merge_dual_with_chunk_records(&base, &sparse, None, &out, Some(&mask), chunk)
                    .unwrap();
                let dual = fs::read(&out).unwrap();
                let bits = fs::read(&mask).unwrap();
                assert_eq!(bits.len(), count.div_ceil(8));
                for i in 0..count {
                    let rec = &dual[i * RECORD_SIZE..(i + 1) * RECORD_SIZE];
                    let base_score = i16::from_le_bytes([rec[32], rec[33]]);
                    let dl = i16::from_le_bytes([rec[34], rec[35]]);
                    let want = if entered(i) {
                        -1000 - i as i16
                    } else {
                        i as i16
                    };
                    assert_eq!(
                        (base_score, dl, rec[39]),
                        (want, i as i16, u8::from(entered(i))),
                        "row {i}"
                    );
                    assert_eq!(&rec[36..39], &[7, 0, 1]);
                    assert_eq!((bits[i / 8] >> (i % 8)) & 1, u8::from(entered(i)));
                }
                if count % 8 != 0 {
                    assert_eq!(bits[count / 8] >> (count % 8), 0);
                }
            }
        }
    }

    #[test]
    fn merge_dual_uses_dl_sidecar_when_given() {
        let count = 9;
        let dir = tempdir().unwrap();
        let base = write(dir.path(), "base.psv", &base_rows(count));
        let ext = dir.path().join("ext.psv");
        extract(&base, &ext).unwrap();
        let sparse = write(dir.path(), "x.psv", &relabel(&fs::read(&ext).unwrap(), count));
        let dl: Vec<u8> = (0..count).flat_map(|i| (500 + i as i16).to_le_bytes()).collect();
        let dl = write(dir.path(), "dl.i16", &dl);
        let out = dir.path().join("dual.psv");
        merge_dual(&base, &sparse, Some(&dl), &out, None).unwrap();
        let dual = fs::read(&out).unwrap();
        for i in 0..count {
            let rec = &dual[i * RECORD_SIZE..];
            assert_eq!(i16::from_le_bytes([rec[34], rec[35]]), 500 + i as i16);
        }
    }

    #[test]
    fn merge_dual_rejects_mismatched_or_short_or_long_sparse() {
        let count = 9;
        let dir = tempdir().unwrap();
        let base = write(dir.path(), "base.psv", &base_rows(count));
        let ext = dir.path().join("ext.psv");
        extract(&base, &ext).unwrap();
        let good = relabel(&fs::read(&ext).unwrap(), count);
        let out = dir.path().join("dual.psv");

        let mut changed = good.clone();
        changed[RECORD_SIZE + 36] ^= 1; // 2 行目の ply
        let cases = [
            ("changed", changed),
            ("short", good[..good.len() - RECORD_SIZE].to_vec()),
            ("long", [good.as_slice(), &good[..RECORD_SIZE]].concat()),
        ];
        for (name, bytes) in cases {
            let sparse = write(dir.path(), &format!("{name}.psv"), &bytes);
            assert!(merge_dual(&base, &sparse, None, &out, None).is_err(), "{name}");
            assert!(!out.exists(), "{name}: output must not be published");
            assert!(!partial_path(&out).exists(), "{name}: staging must be removed");
        }
    }

    #[test]
    fn merge_dual_rejects_output_aliasing_an_input() {
        let count = 3;
        let dir = tempdir().unwrap();
        let bytes = base_rows(count);
        let base = write(dir.path(), "base.psv", &bytes);
        let ext = dir.path().join("ext.psv");
        extract(&base, &ext).unwrap();
        assert!(merge_dual(&base, &ext, None, &base, None).is_err());
        assert!(merge_dual(&base, &ext, None, &dir.path().join("o.psv"), Some(&ext)).is_err());
        let same = dir.path().join("same.psv");
        assert!(merge_dual(&base, &ext, None, &same, Some(&same)).is_err());
        assert_eq!(fs::read(&base).unwrap(), bytes);
    }

    #[test]
    fn merge_dual_rejects_dl_sidecar_size_mismatch() {
        let dir = tempdir().unwrap();
        let base = write(dir.path(), "base.psv", &base_rows(3));
        let ext = dir.path().join("ext.psv");
        extract(&base, &ext).unwrap();
        let dl = write(dir.path(), "dl.i16", &[0u8; 4]);
        assert!(merge_dual(&base, &ext, Some(&dl), &dir.path().join("o.psv"), None).is_err());
    }
}
