use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};

const BUCKETS: usize = 65_536;
const SORT_BYTES: usize = 1024 * 1024;
const SORT_ROWS: usize = 2048;

pub(super) struct Seen {
    file: File,
    heads: Vec<u64>,
}

impl Seen {
    pub(super) fn new() -> Result<Self> {
        let mut file = tempfile::tempfile()?;
        file.write_all(&[0])?;
        Ok(Self {
            file,
            heads: vec![0; BUCKETS],
        })
    }

    pub(super) fn insert(&mut self, key: &str) -> Result<bool> {
        use sha2::{Digest, Sha256};
        let hash = Sha256::digest(key.as_bytes());
        let bucket = usize::from(u16::from_le_bytes([hash[0], hash[1]]));
        let mut offset = self.heads[bucket];
        while offset != 0 {
            self.file.seek(SeekFrom::Start(offset))?;
            let mut word = [0; 8];
            self.file.read_exact(&mut word)?;
            offset = u64::from_le_bytes(word);
            self.file.read_exact(&mut word)?;
            let mut text = vec![0; usize::try_from(u64::from_le_bytes(word))?];
            self.file.read_exact(&mut text)?;
            if text == key.as_bytes() {
                return Ok(false);
            }
        }
        let offset = self.file.seek(SeekFrom::End(0))?;
        self.file.write_all(&self.heads[bucket].to_le_bytes())?;
        self.file.write_all(&(key.len() as u64).to_le_bytes())?;
        self.file.write_all(key.as_bytes())?;
        self.heads[bucket] = offset;
        Ok(true)
    }
}

#[derive(Serialize, Deserialize)]
pub(super) struct Record {
    pub(super) key: Vec<String>,
    pub(super) value: String,
}

pub(super) fn read_record(reader: &mut impl BufRead) -> Result<Option<Record>> {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(&line)?))
}

fn write_record(writer: &mut impl Write, record: &Record) -> Result<()> {
    serde_json::to_writer(&mut *writer, record)?;
    writer.write_all(b"\n")?;
    Ok(())
}

pub(super) struct Sorter {
    chunk: Vec<Record>,
    bytes: usize,
    // マージを二進桁ごとに繰り上げ、塊の数に比例する管理表を持たない。
    runs: [Option<File>; 64],
}

impl Sorter {
    pub(super) fn new() -> Self {
        Self {
            chunk: Vec::new(),
            bytes: 0,
            runs: std::array::from_fn(|_| None),
        }
    }

    pub(super) fn push(&mut self, key: Vec<String>, value: String) -> Result<()> {
        self.bytes += value.len() + key.iter().map(String::len).sum::<usize>();
        self.chunk.push(Record { key, value });
        if self.bytes >= SORT_BYTES || self.chunk.len() >= SORT_ROWS {
            self.flush()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        if self.chunk.is_empty() {
            return Ok(());
        }
        self.chunk.sort_by(|a, b| a.key.cmp(&b.key));
        let mut writer = BufWriter::new(tempfile::tempfile()?);
        for record in self.chunk.drain(..) {
            write_record(&mut writer, &record)?;
        }
        self.bytes = 0;
        let mut run = writer.into_inner()?;
        for slot in &mut self.runs {
            if let Some(old) = slot.take() {
                run = merge(old, run)?;
            } else {
                *slot = Some(run);
                return Ok(());
            }
        }
        anyhow::bail!("外部ソートの行数が大きすぎます")
    }

    pub(super) fn finish(mut self) -> Result<BufReader<File>> {
        self.flush()?;
        let mut result = None;
        for run in self.runs.into_iter().flatten() {
            result = Some(match result {
                Some(old) => merge(run, old)?,
                None => run,
            });
        }
        let mut file = match result {
            Some(file) => file,
            None => tempfile::tempfile()?,
        };
        file.rewind()?;
        Ok(BufReader::new(file))
    }
}

fn merge(mut left: File, mut right: File) -> Result<File> {
    left.rewind()?;
    right.rewind()?;
    let mut left = BufReader::new(left);
    let mut right = BufReader::new(right);
    let mut a = read_record(&mut left)?;
    let mut b = read_record(&mut right)?;
    let mut out = BufWriter::new(tempfile::tempfile()?);
    while a.is_some() || b.is_some() {
        let take_left = match (&a, &b) {
            (Some(a), Some(b)) => a.key <= b.key,
            (Some(_), None) => true,
            _ => false,
        };
        if take_left {
            write_record(&mut out, a.as_ref().context("左のソート行がありません")?)?;
            a = read_record(&mut left)?;
        } else {
            write_record(&mut out, b.as_ref().context("右のソート行がありません")?)?;
            b = read_record(&mut right)?;
        }
    }
    Ok(out.into_inner()?)
}
