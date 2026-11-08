use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Seek, Write};

const SORT_BYTES: usize = 1024 * 1024;
const SORT_ROWS: usize = 2048;

pub(super) struct Seen {
    reader: BufReader<File>,
    #[cfg(test)]
    pub(super) reads: usize,
    #[cfg(test)]
    pub(super) merges: usize,
}

impl Seen {
    pub(super) fn new(start: &str) -> Result<Self> {
        let mut writer = BufWriter::new(tempfile::tempfile()?);
        write_record(
            &mut writer,
            &Record {
                key: vec![start.into()],
                value: String::new(),
            },
        )?;
        let mut file = writer.into_inner()?;
        file.rewind()?;
        Ok(Self {
            reader: BufReader::new(file),
            #[cfg(test)]
            reads: 0,
            #[cfg(test)]
            merges: 0,
        })
    }

    pub(super) fn advance(&mut self, candidates: Sorter) -> Result<(BufReader<File>, usize)> {
        let mut candidates = candidates.finish()?;
        self.reader.rewind()?;
        let mut old = self.read_seen()?;
        let mut merged = BufWriter::new(tempfile::tempfile()?);
        let mut next = Sorter::new();
        let mut last = None;
        let mut added = 0;
        #[cfg(test)]
        {
            self.merges += 1;
        }
        while let Some(record) = read_record(&mut candidates)? {
            #[cfg(test)]
            {
                self.reads += 1;
            }
            let key = &record.key[0];
            if last.as_ref() == Some(key) {
                continue;
            }
            while old.as_ref().is_some_and(|r| r.key[0] < *key) {
                write_record(&mut merged, old.as_ref().context("到達済みの行がありません")?)?;
                old = self.read_seen()?;
            }
            if old.as_ref().is_none_or(|r| r.key[0] != *key) {
                write_record(
                    &mut merged,
                    &Record {
                        key: vec![key.clone()],
                        value: String::new(),
                    },
                )?;
                // キー順の照合後に発見順へ戻し、次層の親の展開順を保つ。
                next.push(vec![record.key[1].clone(), key.clone()], record.value)?;
                added += 1;
            }
            last = Some(key.clone());
        }
        while let Some(record) = old {
            write_record(&mut merged, &record)?;
            old = self.read_seen()?;
        }
        let mut file = merged.into_inner()?;
        file.rewind()?;
        self.reader = BufReader::new(file);
        Ok((next.finish()?, added))
    }

    fn read_seen(&mut self) -> Result<Option<Record>> {
        let record = read_record(&mut self.reader)?;
        #[cfg(test)]
        {
            self.reads += usize::from(record.is_some());
        }
        Ok(record)
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
