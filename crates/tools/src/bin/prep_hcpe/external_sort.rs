use std::{
    cmp::Reverse,
    collections::BinaryHeap,
    fs::{self, File},
    io::{BufReader, BufWriter, Read, Write},
    path::PathBuf,
};

use anyhow::{Context, Result};
use tempfile::TempDir;

const MERGE_FAN_IN: u64 = 32;

pub(super) struct ExternalSort<const N: usize> {
    directory: TempDir,
    buffer: Vec<[u8; N]>,
    capacity: usize,
    runs: u64,
    count: u64,
}

pub(super) struct SortedRecords<const N: usize> {
    directory: TempDir,
    path: PathBuf,
    pub(super) count: u64,
}

impl<const N: usize> SortedRecords<N> {
    pub(super) fn open(&self) -> Result<File> {
        Ok(File::open(self.directory.path().join(&self.path))?)
    }
}

pub(super) fn read_record<const N: usize>(reader: &mut impl Read) -> Result<Option<[u8; N]>> {
    let mut record = [0; N];
    if reader.read(&mut record[..1])? == 0 {
        return Ok(None);
    }
    reader
        .read_exact(&mut record[1..])
        .context("一時レコードが途中で切れています")?;
    Ok(Some(record))
}

impl<const N: usize> ExternalSort<N> {
    pub(super) fn new(capacity: usize) -> Result<Self> {
        anyhow::ensure!(capacity > 0, "sort buffer must be nonzero");
        Ok(Self {
            directory: tempfile::tempdir()?,
            buffer: Vec::with_capacity(capacity),
            capacity,
            runs: 0,
            count: 0,
        })
    }

    pub(super) fn push(&mut self, record: [u8; N]) -> Result<()> {
        self.buffer.push(record);
        self.count += 1;
        if self.buffer.len() == self.capacity {
            self.flush_run()?;
        }
        Ok(())
    }

    fn run_path(&self, level: u32, index: u64) -> PathBuf {
        self.directory.path().join(format!("{level}-{index}.bin"))
    }

    fn flush_run(&mut self) -> Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        self.buffer.sort_unstable();
        let mut writer = BufWriter::new(File::create(self.run_path(0, self.runs))?);
        for record in &self.buffer {
            writer.write_all(record)?;
        }
        writer.flush()?;
        self.buffer.clear();
        self.runs += 1;
        Ok(())
    }

    pub(super) fn finish(mut self) -> Result<SortedRecords<N>> {
        self.flush_run()?;
        let mut level = 0;
        let mut runs = self.runs;
        if runs == 0 {
            File::create(self.run_path(0, 0))?;
        }
        while runs > 1 {
            let next_runs = runs.div_ceil(MERGE_FAN_IN);
            for group in 0..next_runs {
                let start = group * MERGE_FAN_IN;
                let end = (start + MERGE_FAN_IN).min(runs);
                let mut readers = (start..end)
                    .map(|index| Ok(BufReader::new(File::open(self.run_path(level, index))?)))
                    .collect::<Result<Vec<_>>>()?;
                let mut heap = BinaryHeap::new();
                for (index, reader) in readers.iter_mut().enumerate() {
                    if let Some(record) = read_record::<N>(reader)? {
                        heap.push(Reverse((record, index)));
                    }
                }
                let mut writer = BufWriter::new(File::create(self.run_path(level + 1, group))?);
                while let Some(Reverse((record, index))) = heap.pop() {
                    writer.write_all(&record)?;
                    if let Some(next) = read_record::<N>(&mut readers[index])? {
                        heap.push(Reverse((next, index)));
                    }
                }
                writer.flush()?;
                drop(readers);
                for index in start..end {
                    fs::remove_file(self.run_path(level, index))?;
                }
            }
            runs = next_runs;
            level += 1;
        }
        Ok(SortedRecords {
            path: PathBuf::from(format!("{level}-0.bin")),
            directory: self.directory,
            count: self.count,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_multiple_levels_with_bounded_fan_in() -> Result<()> {
        let mut sorter = ExternalSort::new(2)?;
        for value in (0..150_u16).rev() {
            sorter.push(value.to_be_bytes())?;
        }
        let sorted = sorter.finish()?;
        let mut reader = BufReader::new(sorted.open()?);
        for value in 0..150_u16 {
            assert_eq!(read_record::<2>(&mut reader)?, Some(value.to_be_bytes()));
        }
        assert_eq!(read_record::<2>(&mut reader)?, None);
        Ok(())
    }
}
