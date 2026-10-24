//! PSV と LSB-first bitmap mask を組み合わせるツール向けの共通 I/O。

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::packed_sfen::PackedSfenValue;

/// 入力順の bit を LSB-first で書き、未出力の bitmap は最大 1 byte だけ保持する。
/// I/O エラー後の `push` / `finish` は失敗し、書込みを再試行しない。内部 writer の Drop はその型に従う。
pub struct BitmapWriter<W> {
    writer: W,
    pending: u8,
    pending_bits: u8,
    failed: bool,
}

impl<W: Write> BitmapWriter<W> {
    /// 書込み先を受け取り、空の bitmap を開始する。
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            pending: 0,
            pending_bits: 0,
            failed: false,
        }
    }

    /// 1 bit を追加し、8 bit ごとに 1 byte を書く。失敗した出力は未完成として扱う。
    pub fn push(&mut self, set: bool) -> io::Result<()> {
        self.ensure_active()?;
        if set {
            self.pending |= 1 << self.pending_bits;
        }
        self.pending_bits += 1;
        if self.pending_bits == 8 {
            self.failed = true;
            self.writer.write_all(&[self.pending])?;
            self.pending = 0;
            self.pending_bits = 0;
            self.failed = false;
        }
        Ok(())
    }

    /// 末尾の未使用 bit を 0 として書き、flush 成功後に内部 writer を返す。
    /// 空入力では byte を書かず flush する。失敗済みの場合は内部 writer を操作せずエラーを返す。
    pub fn finish(mut self) -> io::Result<W> {
        self.ensure_active()?;
        if self.pending_bits != 0 {
            self.writer.write_all(&[self.pending])?;
        }
        self.writer.flush()?;
        Ok(self.writer)
    }

    fn ensure_active(&self) -> io::Result<()> {
        if self.failed {
            return Err(io::Error::other("bitmap writer は書込みエラー後に再利用できません"));
        }
        Ok(())
    }
}

/// `<out>.partial` 形式の staging パスを返す。
pub fn partial_path(out: &Path) -> PathBuf {
    let mut path = out.as_os_str().to_os_string();
    path.push(".partial");
    PathBuf::from(path)
}

/// PSV のサイズを検証し、レコード数を返す。
pub fn checked_input_records(input: &Path) -> Result<u64> {
    let bytes = fs::metadata(input)
        .with_context(|| format!("Failed to stat input {}", input.display()))?
        .len();
    anyhow::ensure!(
        bytes % PackedSfenValue::SIZE as u64 == 0,
        "Input size is not a multiple of {}: {} ({bytes} bytes)",
        PackedSfenValue::SIZE,
        input.display()
    );
    Ok(bytes / PackedSfenValue::SIZE as u64)
}

/// mask のサイズと最終 byte の未使用 bit を検証する。
pub fn validate_mask(mask: &Path, records: u64) -> Result<()> {
    let expected_bytes = records.div_ceil(8);
    let actual_bytes = fs::metadata(mask)
        .with_context(|| format!("Failed to stat mask {}", mask.display()))?
        .len();
    anyhow::ensure!(
        actual_bytes == expected_bytes,
        "Mask size mismatch: expected {expected_bytes} bytes for {records} records, got {actual_bytes}: {}",
        mask.display()
    );

    let used_bits = (records % 8) as u32;
    if used_bits != 0 {
        let mut file = File::open(mask)?;
        file.seek(SeekFrom::End(-1))?;
        let mut last = [0u8; 1];
        file.read_exact(&mut last)?;
        let unused_mask = !((1u8 << used_bits) - 1);
        anyhow::ensure!(
            last[0] & unused_mask == 0,
            "Mask has non-zero unused bits in final byte: {}",
            mask.display()
        );
    }
    Ok(())
}

/// chunk のレコード数が正かつ mask の byte 境界に揃うことを検証する。
pub fn validate_chunk_records(chunk_records: usize) -> Result<()> {
    anyhow::ensure!(chunk_records > 0, "chunk record count must be positive");
    anyhow::ensure!(
        chunk_records.is_multiple_of(8),
        "chunk record count must be a multiple of 8: {chunk_records}"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BITS: [bool; 17] = [
        true, false, true, false, false, true, false, true, true, false, false, true, true, false,
        false, true, true,
    ];

    #[derive(Debug, Default)]
    struct FaultWriter {
        bytes: Vec<u8>,
        writes: usize,
        flushes: usize,
        fail_write_at: Option<usize>,
        fail_flush: bool,
    }

    impl Write for FaultWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.writes += 1;
            if self.fail_write_at == Some(self.writes) {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "injected write failure"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            if self.fail_flush {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "injected flush failure"));
            }
            Ok(())
        }
    }

    #[test]
    fn bitmap_bytes_are_lsb_first_and_zero_pad_the_tail() {
        let cases: &[(usize, &[u8])] = &[
            (0, &[]),
            (1, &[0x01]),
            (7, &[0x25]),
            (8, &[0xa5]),
            (9, &[0xa5, 0x01]),
            (17, &[0xa5, 0x99, 0x01]),
        ];
        for &(count, expected) in cases {
            let mut writer = BitmapWriter::new(FaultWriter::default());
            for &bit in &BITS[..count] {
                writer.push(bit).unwrap();
            }
            let inner = writer.finish().unwrap();
            assert_eq!(inner.bytes, expected, "bits={count}");
            assert_eq!(inner.flushes, 1);
        }
    }

    #[test]
    fn bitmap_preserves_partial_bytes_across_input_chunks() {
        for chunk_size in [1, 3, 5, 7, 8, 9, 16, 17] {
            let mut writer = BitmapWriter::new(Vec::new());
            for chunk in BITS.chunks(chunk_size) {
                for &bit in chunk {
                    writer.push(bit).unwrap();
                }
            }
            assert_eq!(writer.finish().unwrap(), [0xa5, 0x99, 0x01], "chunk={chunk_size}");
        }
    }

    #[test]
    fn full_byte_write_error_prevents_push_and_finish_from_retrying() {
        for fail_write_at in [1, 2] {
            let mut inner = FaultWriter {
                fail_write_at: Some(fail_write_at),
                ..Default::default()
            };
            let mut writer = BitmapWriter::new(&mut inner);
            let last_bit = fail_write_at * 8 - 1;
            for &bit in &BITS[..last_bit] {
                writer.push(bit).unwrap();
            }
            assert_eq!(writer.push(BITS[last_bit]).unwrap_err().kind(), io::ErrorKind::BrokenPipe);
            assert!(writer.push(true).is_err());
            assert!(writer.push(false).is_err());
            assert!(writer.finish().is_err());
            assert_eq!(inner.writes, fail_write_at);
            assert_eq!(inner.flushes, 0);
            assert_eq!(inner.bytes, [0xa5][..fail_write_at - 1]);
        }
    }

    #[test]
    fn tail_write_error_prevents_flush() {
        let mut inner = FaultWriter {
            fail_write_at: Some(2),
            ..Default::default()
        };
        let mut writer = BitmapWriter::new(&mut inner);
        for &bit in &BITS[..9] {
            writer.push(bit).unwrap();
        }
        assert_eq!(writer.finish().unwrap_err().kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(inner.bytes, [0xa5]);
        assert_eq!(inner.writes, 2);
        assert_eq!(inner.flushes, 0);
    }

    #[test]
    fn flush_error_is_returned_for_empty_full_and_partial_bytes() {
        let cases: &[(usize, &[u8])] = &[(0, &[]), (8, &[0xa5]), (9, &[0xa5, 0x01])];
        for &(count, expected) in cases {
            let mut inner = FaultWriter {
                fail_flush: true,
                ..Default::default()
            };
            let mut writer = BitmapWriter::new(&mut inner);
            for &bit in &BITS[..count] {
                writer.push(bit).unwrap();
            }
            assert_eq!(writer.finish().unwrap_err().kind(), io::ErrorKind::BrokenPipe);
            assert_eq!(inner.bytes, expected);
            assert_eq!(inner.flushes, 1);
        }
    }
}
