//! 元データを固定数のバケットでディスクに索引化し、必要な局面の行だけを読む。

use super::master_table::{Row, read_rows};
use super::schema::Schema;
use anyhow::{Context, Result};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom, Write};

const BUCKETS: usize = 65536;

pub(super) struct MasterIndex {
    file: File,
    heads: Vec<u64>,
}

fn bucket(key: &str) -> usize {
    key.bytes().fold(0usize, |h, b| h.wrapping_mul(31).wrapping_add(b as usize)) % BUCKETS
}

impl MasterIndex {
    pub(super) fn open(path: &std::path::Path, schema: &Schema) -> Result<Self> {
        let mut index = Self {
            file: tempfile::tempfile()?,
            heads: vec![0; BUCKETS],
        };
        // 0 は鎖の終端。行の前に同じバケットの直前の行の位置を置く。
        index.file.write_all(&[0])?;
        for row in read_rows(BufReader::new(File::open(path)?))? {
            let (_, row) = row?;
            schema.validate_row(&row)?;
            let b = bucket(row.get("key"));
            let offset = index.file.stream_position()?;
            let text = super::master_table::format_master(&[row]);
            let text = text.split_once('\n').context("元データの見出しがありません")?.1;
            index.file.write_all(&index.heads[b].to_le_bytes())?;
            index.file.write_all(&(text.len() as u64).to_le_bytes())?;
            index.file.write_all(text.as_bytes())?;
            index.heads[b] = offset;
        }
        Ok(index)
    }

    pub(super) fn rows(&mut self, key: &str) -> Result<Vec<Row>> {
        let mut offset = self.heads[bucket(key)];
        let mut rows = Vec::new();
        while offset != 0 {
            self.file.seek(SeekFrom::Start(offset))?;
            let mut word = [0; 8];
            self.file.read_exact(&mut word)?;
            offset = u64::from_le_bytes(word);
            self.file.read_exact(&mut word)?;
            let len = usize::try_from(u64::from_le_bytes(word))?;
            let mut text = vec![0; len];
            self.file.read_exact(&mut text)?;
            let text = std::str::from_utf8(&text)?.trim_end_matches('\n');
            if text.split('\t').next() == Some(key) {
                rows.push(Row::parse_line(text, 0)?);
            }
        }
        rows.reverse();
        Ok(rows)
    }
}
