use super::journal::Reading;
use anyhow::{Result, bail};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

// HashMap の容量の丸めと制御領域を含めても、検査用の表を約 64 MiB 以下に抑える。
const MAX_KEYS: usize = 262_144;
const ENTRY_BYTES: usize = 41;
type Entry = [u8; ENTRY_BYTES];

struct Value {
    hash: [u8; 16],
    first: u64,
    prefetch: bool,
}

/// 入力順を保つ一時台帳とは別に、重複検査用の固定長の行をディスクへ分配する。
pub(super) struct Buckets {
    files: Vec<BufWriter<File>>,
    bits: usize,
}

impl Buckets {
    /// 入力のバイト数から初期分割数を選び、同時に開くファイル数も制限する。
    pub fn new(dir: &Path, bytes: u64) -> Result<Self> {
        let count = bytes.div_ceil(64 * 1024 * 1024).clamp(1, 64).next_power_of_two();
        let files = (0..count)
            .map(|_| Ok(BufWriter::new(tempfile::tempfile_in(dir)?)))
            .collect::<Result<_>>()?;
        Ok(Self {
            files,
            bits: count.trailing_zeros() as usize,
        })
    }

    /// 値の比較には台帳の重複検査と同じ SHA-256 の先頭 16 バイトを使う。
    pub fn push(
        &mut self,
        reading: &Reading,
        fingerprint: &str,
        row: u64,
        prefetch: bool,
    ) -> Result<()> {
        let mut entry = [0; ENTRY_BYTES];
        entry[..16].copy_from_slice(
            &Sha256::digest(serde_json::to_vec(&(fingerprint, &reading.key))?)[..16],
        );
        let values = serde_json::to_vec(&(&reading.multipv, &reading.bestmove))?;
        entry[16..32].copy_from_slice(&Sha256::digest(values)[..16]);
        entry[32..40].copy_from_slice(&row.to_le_bytes());
        entry[40] = u8::from(prefetch);
        let index = usize::from(entry[0]) & (self.files.len() - 1);
        self.files[index].write_all(&entry)?;
        Ok(())
    }

    /// 全バケットを検査し、先読み印 (bit 0)、同値重複 (bit 1)、値の衝突 (bit 2) を行番号で返す。
    pub fn finish(self, dir: &Path, rows: u64) -> Result<File> {
        let mut marks = tempfile::tempfile_in(dir)?;
        marks.set_len(rows)?;
        for writer in self.files {
            let file = writer.into_inner()?;
            validate_bucket(file, dir, self.bits, MAX_KEYS, &mut marks)?;
        }
        marks.rewind()?;
        Ok(marks)
    }
}

fn entries(file: File, mut visit: impl FnMut(Entry) -> Result<()>) -> Result<()> {
    let mut reader = BufReader::new(file);
    reader.rewind()?;
    let mut entry = [0; ENTRY_BYTES];
    loop {
        if reader.read(&mut entry[..1])? == 0 {
            return Ok(());
        }
        reader.read_exact(&mut entry[1..])?;
        visit(entry)?;
    }
}

fn validate_bucket(
    mut file: File,
    dir: &Path,
    bit: usize,
    max_keys: usize,
    marks: &mut File,
) -> Result<()> {
    let mut seen: HashMap<[u8; 16], Value> = HashMap::new();
    let mut overflow = false;
    entries(file.try_clone()?, |entry| {
        if overflow {
            return Ok(());
        }
        let key: [u8; 16] = entry[..16].try_into()?;
        let hash: [u8; 16] = entry[16..32].try_into()?;
        if let Some(old) = seen.get_mut(&key) {
            if old.hash != hash {
                marks.seek(SeekFrom::Start(u64::from_le_bytes(entry[32..40].try_into()?)))?;
                marks.write_all(&[4])?;
                return Ok(());
            }
            old.prefetch |= entry[40] != 0;
            marks.seek(SeekFrom::Start(u64::from_le_bytes(entry[32..40].try_into()?)))?;
            marks.write_all(&[2])?;
        } else if seen.len() == max_keys {
            overflow = true;
        } else {
            seen.insert(
                key,
                Value {
                    hash,
                    first: u64::from_le_bytes(entry[32..40].try_into()?),
                    prefetch: entry[40] != 0,
                },
            );
        }
        Ok(())
    })?;
    if !overflow {
        // 表の反復順によらず、入力の最初の行にだけ印を置く。追記順と出どころを変えない。
        for value in seen.into_values().filter(|v| v.prefetch) {
            marks.seek(SeekFrom::Start(value.first))?;
            marks.write_all(&[1])?;
        }
        return Ok(());
    }
    drop(seen);
    if bit >= 128 {
        bail!("取り込み元のバケットを分割できません");
    }
    let mut children = [
        BufWriter::new(tempfile::tempfile_in(dir)?),
        BufWriter::new(tempfile::tempfile_in(dir)?),
    ];
    file.rewind()?;
    entries(file, |entry| {
        let index = usize::from((entry[bit / 8] >> (bit % 8)) & 1);
        children[index].write_all(&entry)?;
        Ok(())
    })?;
    for child in children {
        validate_bucket(child.into_inner()?, dir, bit + 1, max_keys, marks)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(key: u8, value: u8, row: u64, prefetch: bool) -> Entry {
        let mut entry = [0; ENTRY_BYTES];
        entry[1] = key;
        entry[16] = value;
        entry[32..40].copy_from_slice(&row.to_le_bytes());
        entry[40] = u8::from(prefetch);
        entry
    }

    fn validate(entries: &[Entry], max_keys: usize) -> Result<Vec<u8>> {
        let dir = tempfile::tempdir()?;
        let mut file = tempfile::tempfile_in(dir.path())?;
        for entry in entries {
            file.write_all(entry)?;
        }
        let mut marks = tempfile::tempfile_in(dir.path())?;
        marks.set_len(entries.len() as u64)?;
        validate_bucket(file, dir.path(), 0, max_keys, &mut marks)?;
        marks.rewind()?;
        let mut bytes = Vec::new();
        marks.read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    #[test]
    fn recursive_split_preserves_first_rows_and_aggregates_late_prefetch() {
        let rows = [
            entry(0, 9, 0, false),
            entry(1, 8, 1, true),
            entry(2, 7, 2, false),
            entry(3, 6, 3, false),
            entry(0, 9, 4, true),
            entry(2, 7, 5, true),
            entry(1, 8, 6, false),
        ];
        let expected = vec![1, 1, 1, 0, 2, 2, 2];
        assert_eq!(validate(&rows, 2).unwrap(), expected);
        assert_eq!(validate(&rows, 2).unwrap(), expected);
        assert_eq!(validate(&rows, 10).unwrap(), expected);
    }

    #[test]
    fn recursive_split_skips_conflicts_in_late_duplicates() {
        let rows = [
            entry(0, 9, 0, false),
            entry(1, 8, 1, false),
            entry(0, 7, 2, true),
        ];
        assert_eq!(validate(&rows, 1).unwrap(), [0, 0, 4]);
    }

    #[test]
    fn repeated_key_does_not_require_more_buckets() {
        let rows: Vec<_> = (0..1000).map(|i| entry(0, 9, i, i == 999)).collect();
        let marks = validate(&rows, 1).unwrap();
        assert_eq!(marks[0], 1);
        assert_eq!(marks.iter().filter(|&&v| v & 1 != 0).count(), 1);
    }
}
