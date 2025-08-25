//! Custom logger writer that flushes after each message to prevent stderr blocking
//! in subprocess contexts.

use std::io::{self, Write};

/// A writer that wraps stderr and flushes after each write operation.
/// This prevents stderr buffer from filling up and blocking the process
/// when running as a subprocess.
pub struct FlushingStderrWriter {
    stderr: io::Stderr,
}

impl FlushingStderrWriter {
    pub fn new() -> Self {
        Self {
            stderr: io::stderr(),
        }
    }
}

impl Write for FlushingStderrWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let result = self.stderr.write(buf)?;
        // Force flush after each write to prevent blocking
        self.stderr.flush()?;
        Ok(result)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stderr.flush()
    }
}

impl Default for FlushingStderrWriter {
    fn default() -> Self {
        Self::new()
    }
}
