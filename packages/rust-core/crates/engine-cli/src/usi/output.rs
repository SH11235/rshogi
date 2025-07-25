//! USI protocol output formatting

use std::fmt;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::Duration;

/// USI protocol responses
#[derive(Debug, Clone)]
pub enum UsiResponse {
    /// Engine identification
    Id { name: String, author: String },

    /// USI mode confirmed
    UsiOk,

    /// Ready confirmation
    ReadyOk,

    /// Best move found
    BestMove {
        best_move: String,
        ponder: Option<String>,
    },

    /// Search information
    Info(SearchInfo),

    /// Engine option
    Option(String),

    /// String message (for errors/warnings)
    String(String),
}

/// Search information for info command
#[derive(Debug, Clone, Default)]
pub struct SearchInfo {
    /// Search depth
    pub depth: Option<u32>,

    /// Selective depth
    pub seldepth: Option<u32>,

    /// Time spent in milliseconds
    pub time: Option<u64>,

    /// Nodes searched
    pub nodes: Option<u64>,

    /// Principal variation
    pub pv: Vec<String>,

    /// Score in centipawns or mate
    pub score: Option<Score>,

    /// Current move being searched
    pub currmove: Option<String>,

    /// Nodes per second
    pub nps: Option<u64>,

    /// Hash table usage (permille)
    pub hashfull: Option<u32>,

    /// Tablebase hits
    pub tbhits: Option<u64>,

    /// Multi-PV index
    pub multipv: Option<u32>,

    /// String message
    pub string: Option<String>,
}

/// Score representation
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Score {
    /// Centipawn score
    Cp(i32),

    /// Mate in N moves (positive = winning, negative = losing)
    /// TODO: Implement mate detection and use this variant
    #[allow(dead_code)]
    Mate(i32),
}

impl fmt::Display for UsiResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UsiResponse::Id { name, author } => {
                writeln!(f, "id name {name}")?;
                write!(f, "id author {author}")
            }
            UsiResponse::UsiOk => write!(f, "usiok"),
            UsiResponse::ReadyOk => write!(f, "readyok"),
            UsiResponse::BestMove { best_move, ponder } => {
                write!(f, "bestmove {best_move}")?;
                if let Some(ponder_move) = ponder {
                    write!(f, " ponder {ponder_move}")?;
                }
                Ok(())
            }
            UsiResponse::Info(info) => {
                let info_str = info.to_string();
                if info_str.is_empty() {
                    // Skip empty info output to avoid trailing space
                    Ok(())
                } else {
                    write!(f, "info {info_str}")
                }
            }
            UsiResponse::Option(opt) => write!(f, "{opt}"),
            UsiResponse::String(msg) => write!(f, "info string {msg}"),
        }
    }
}

impl fmt::Display for SearchInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts = Vec::new();

        if let Some(depth) = self.depth {
            parts.push(format!("depth {depth}"));
        }

        if let Some(seldepth) = self.seldepth {
            parts.push(format!("seldepth {seldepth}"));
        }

        if let Some(time) = self.time {
            parts.push(format!("time {time}"));
        }

        if let Some(nodes) = self.nodes {
            parts.push(format!("nodes {nodes}"));
        }

        if let Some(score) = self.score {
            match score {
                Score::Cp(cp) => parts.push(format!("score cp {cp}")),
                Score::Mate(mate) => parts.push(format!("score mate {mate}")),
            }
        }

        if let Some(multipv) = self.multipv {
            parts.push(format!("multipv {multipv}"));
        }

        if let Some(currmove) = &self.currmove {
            parts.push(format!("currmove {currmove}"));
        }

        if let Some(nps) = self.nps {
            parts.push(format!("nps {nps}"));
        }

        if let Some(hashfull) = self.hashfull {
            parts.push(format!("hashfull {hashfull}"));
        }

        if let Some(tbhits) = self.tbhits {
            parts.push(format!("tbhits {tbhits}"));
        }

        if !self.pv.is_empty() {
            let pv_str = format!("pv {}", self.pv.join(" "));
            parts.push(pv_str);
        }

        if let Some(string) = &self.string {
            parts.push(format!("string {string}"));
        }

        write!(f, "{}", parts.join(" "))
    }
}

// Error tracking for stdout failures
// Note: AtomicU32 is used for future thread-safety, though currently only main thread calls this
static STDOUT_ERROR_COUNT: AtomicU32 = AtomicU32::new(0);
const MAX_STDOUT_ERRORS: u32 = 5;
const MAX_RETRY_ATTEMPTS: u32 = 3;

/// Send USI response with error handling and retry logic
fn send_response_with_retry(response: &UsiResponse) -> std::io::Result<()> {
    use std::io::{self, Write};
    
    // Get stdout handle and write response
    let mut stdout = io::stdout();
    writeln!(stdout, "{response}")?;
    
    // Then try to flush with limited retries
    for attempt in 0..MAX_RETRY_ATTEMPTS {
        match stdout.flush() {
            Ok(()) => {
                // Reset error count on success
                STDOUT_ERROR_COUNT.store(0, Ordering::Relaxed);
                return Ok(());
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {
                // EINTR - retry immediately without sleep
                log::debug!("stdout flush interrupted, retrying immediately");
                continue;
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock && attempt < MAX_RETRY_ATTEMPTS - 1 => {
                // WouldBlock - brief pause then retry
                log::debug!("stdout would block, brief retry");
                thread::sleep(Duration::from_millis(1));
                continue;
            }
            Err(e) if attempt < MAX_RETRY_ATTEMPTS - 1 => {
                // Other errors - very brief backoff to avoid blocking time-critical responses
                log::warn!("stdout flush error: {e}, retry attempt {}/{}", attempt + 1, MAX_RETRY_ATTEMPTS);
                thread::sleep(Duration::from_millis(1));
            }
            Err(e) => {
                // Final attempt failed
                return Err(e);
            }
        }
    }
    
    // Should not reach here, but return error if we do
    Err(io::Error::new(io::ErrorKind::Other, "Max retry attempts exceeded"))
}

/// Error types for stdout operations
#[derive(Debug)]
pub enum StdoutError {
    BrokenPipe,
    TooManyErrors(u32),
    CriticalMessageFailed(std::io::Error),
}

/// Helper to send USI response to stdout with automatic flush
pub fn send_response(response: UsiResponse) {
    if let Err(e) = send_response_safe(response) {
        match e {
            StdoutError::BrokenPipe => {
                log::error!("Broken pipe detected, GUI disconnected");
                // Exit with code 1 (portable) instead of 141 (Unix-specific)
                std::process::exit(1);
            }
            StdoutError::TooManyErrors(count) => {
                log::error!("Too many stdout errors ({count}), exiting");
                std::process::exit(1);
            }
            StdoutError::CriticalMessageFailed(io_err) => {
                log::error!("Failed to send critical response: {io_err}");
                std::process::exit(1);
            }
        }
    }
}

/// Send USI response with error handling, returning Result for proper error propagation
pub fn send_response_safe(response: UsiResponse) -> Result<(), StdoutError> {
    use std::io;
    
    // Determine if this is a critical response
    let is_critical = matches!(
        response,
        UsiResponse::UsiOk | UsiResponse::ReadyOk | UsiResponse::BestMove { .. }
    );
    
    // Try to send with retry
    if let Err(e) = send_response_with_retry(&response) {
        // Increment error count
        let error_count = STDOUT_ERROR_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        
        // Handle based on error type and criticality
        match e.kind() {
            io::ErrorKind::BrokenPipe => {
                Err(StdoutError::BrokenPipe)
            }
            _ if error_count >= MAX_STDOUT_ERRORS => {
                Err(StdoutError::TooManyErrors(error_count))
            }
            _ if is_critical => {
                Err(StdoutError::CriticalMessageFailed(e))
            }
            _ => {
                // Non-critical error - log and continue
                log::warn!("Failed to send response: {e} (error #{error_count})");
                Ok(())
            }
        }
    } else {
        Ok(())
    }
}

/// Helper to send info string message
pub fn send_info_string(message: impl Into<String>) {
    send_response(UsiResponse::String(message.into()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_usi_response_formatting() {
        let resp = UsiResponse::Id {
            name: "RustShogi 1.0".to_string(),
            author: "Rust Team".to_string(),
        };
        assert_eq!(resp.to_string(), "id name RustShogi 1.0\nid author Rust Team");

        let resp = UsiResponse::BestMove {
            best_move: "7g7f".to_string(),
            ponder: Some("3c3d".to_string()),
        };
        assert_eq!(resp.to_string(), "bestmove 7g7f ponder 3c3d");

        let resp = UsiResponse::BestMove {
            best_move: "7g7f".to_string(),
            ponder: None,
        };
        assert_eq!(resp.to_string(), "bestmove 7g7f");
    }

    #[test]
    fn test_search_info_formatting() {
        let info = SearchInfo {
            depth: Some(12),
            time: Some(1234),
            nodes: Some(567890),
            score: Some(Score::Cp(42)),
            pv: vec!["7g7f".to_string(), "3c3d".to_string()],
            ..Default::default()
        };

        let resp = UsiResponse::Info(info);
        assert_eq!(
            resp.to_string(),
            "info depth 12 time 1234 nodes 567890 score cp 42 pv 7g7f 3c3d"
        );

        let info = SearchInfo {
            depth: Some(20),
            score: Some(Score::Mate(7)),
            pv: vec!["2b8h+".to_string()],
            ..Default::default()
        };

        let resp = UsiResponse::Info(info);
        assert_eq!(resp.to_string(), "info depth 20 score mate 7 pv 2b8h+");
    }

    #[test]
    fn test_empty_search_info() {
        let info = SearchInfo::default();
        assert_eq!(info.to_string(), "");
    }
}
