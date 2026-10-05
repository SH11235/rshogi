//! タイムアウトをまたいで部分行を保持する、サイズ制限付きの TCP 行読み取り。

use std::io::{self, BufRead, ErrorKind};

pub(super) const MAX_CSA_LINE_BYTES: usize = 64 * 1024;

pub(super) struct LineReader<R> {
    reader: R,
    pending: Vec<u8>,
    /// 上限超過で行の途中を捨てると次の行境界が分からなくなる。残りを別の行として
    /// 返さないよう、以降の読み取りはすべて同じエラーにする。
    overflowed: bool,
}

impl<R: BufRead> LineReader<R> {
    pub(super) fn new(reader: R) -> Self {
        Self {
            reader,
            pending: Vec::new(),
            overflowed: false,
        }
    }

    pub(super) fn get_ref(&self) -> &R {
        &self.reader
    }

    pub(super) fn read_line(&mut self) -> io::Result<Option<String>> {
        if self.overflowed {
            return Err(line_too_long());
        }
        loop {
            let bytes = match self.reader.fill_buf() {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            };
            if bytes.is_empty() {
                return if self.pending.is_empty() {
                    Ok(None)
                } else {
                    self.take_line().map(Some)
                };
            }
            let newline = bytes.iter().position(|&byte| byte == b'\n');
            let content_len = newline.unwrap_or(bytes.len());
            if content_len > MAX_CSA_LINE_BYTES.saturating_sub(self.pending.len()) {
                self.pending.clear();
                self.overflowed = true;
                return Err(line_too_long());
            }
            self.pending.extend_from_slice(&bytes[..content_len]);
            self.reader.consume(content_len + usize::from(newline.is_some()));
            if newline.is_some() {
                return self.take_line().map(Some);
            }
        }
    }

    fn take_line(&mut self) -> io::Result<String> {
        String::from_utf8(std::mem::take(&mut self.pending))
            .map_err(|error| io::Error::new(ErrorKind::InvalidData, error))
    }
}

fn line_too_long() -> io::Error {
    io::Error::new(ErrorKind::InvalidData, "CSA line exceeds 64 KiB")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::io::{BufReader, Cursor, Read};

    struct InterruptedInput {
        chunks: VecDeque<io::Result<Vec<u8>>>,
        current: Cursor<Vec<u8>>,
    }

    impl Read for InterruptedInput {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            let available = self.fill_buf()?;
            let count = available.len().min(output.len());
            output[..count].copy_from_slice(&available[..count]);
            self.consume(count);
            Ok(count)
        }
    }

    impl BufRead for InterruptedInput {
        fn fill_buf(&mut self) -> io::Result<&[u8]> {
            if self.current.position() as usize == self.current.get_ref().len() {
                self.current =
                    Cursor::new(self.chunks.pop_front().transpose()?.unwrap_or_default());
            }
            self.current.fill_buf()
        }

        fn consume(&mut self, count: usize) {
            self.current.consume(count);
        }
    }

    #[test]
    fn preserves_partial_utf8_line_across_timeout() {
        let input = InterruptedInput {
            chunks: VecDeque::from([
                Ok(vec![b'L', b'O', 0xe5]),
                Err(io::Error::from(ErrorKind::TimedOut)),
                Ok(vec![0x85, 0x88, b'\r', b'\n']),
            ]),
            current: Cursor::new(Vec::new()),
        };
        let mut reader = LineReader::new(input);
        assert_eq!(reader.read_line().unwrap_err().kind(), ErrorKind::TimedOut);
        assert_eq!(reader.read_line().unwrap().as_deref(), Some("LO先\r"));
        assert!(reader.read_line().unwrap().is_none());
    }

    #[test]
    fn reads_empty_multiple_and_unterminated_lines() {
        let mut reader = LineReader::new(Cursor::new(b"\nLOGIN:OK\nEND"));
        assert_eq!(reader.read_line().unwrap().as_deref(), Some(""));
        assert_eq!(reader.read_line().unwrap().as_deref(), Some("LOGIN:OK"));
        assert_eq!(reader.read_line().unwrap().as_deref(), Some("END"));
        assert!(reader.read_line().unwrap().is_none());
    }

    #[test]
    fn accepts_limit_and_rejects_larger_unterminated_line() {
        let mut exact = vec![b'x'; MAX_CSA_LINE_BYTES];
        exact.push(b'\n');
        assert_eq!(
            LineReader::new(Cursor::new(exact)).read_line().unwrap().unwrap().len(),
            MAX_CSA_LINE_BYTES
        );
        let mut reader = LineReader::new(Cursor::new(vec![b'x'; MAX_CSA_LINE_BYTES + 1]));
        assert_eq!(reader.read_line().unwrap_err().kind(), ErrorKind::InvalidData);
        assert!(reader.pending.is_empty());
    }

    #[test]
    fn oversized_line_tail_is_not_returned_as_a_line() {
        let mut input = vec![b'x'; MAX_CSA_LINE_BYTES + 1];
        input.extend_from_slice(b"tail\nNEXT\n");
        // 上限超過の時点で残りがバッファに載り切らないよう、小さい容量で読む。
        let mut reader = LineReader::new(BufReader::with_capacity(64, Cursor::new(input)));
        for _ in 0..3 {
            assert_eq!(reader.read_line().unwrap_err().kind(), ErrorKind::InvalidData);
        }
    }

    #[test]
    fn rejects_invalid_utf8() {
        let mut reader = LineReader::new(Cursor::new([0xff, b'\n']));
        assert_eq!(reader.read_line().unwrap_err().kind(), ErrorKind::InvalidData);
    }
}
