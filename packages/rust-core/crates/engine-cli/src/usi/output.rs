//! USI protocol output formatting

use crate::utils::lock_or_recover_generic;
use engine_core::search::NodeType;
use once_cell::sync::Lazy;
use std::fmt;
use std::io::{BufWriter, Write};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;
#[cfg(feature = "buffered-io")]
use std::time::Instant;

#[cfg(test)]
static INFO_MESSAGES: Lazy<Mutex<Vec<String>>> = Lazy::new(|| Mutex::new(Vec::new()));

#[cfg(test)]
/// Take and clear all captured info string messages (test-only)
pub fn test_take_info_strings() -> Vec<String> {
    let mut guard = lock_or_recover_generic(&INFO_MESSAGES);
    let out = guard.clone();
    guard.clear();
    out
}

#[cfg(test)]
/// Clear captured info string messages (test-only)
pub fn test_clear_info_strings() {
    let mut guard = lock_or_recover_generic(&INFO_MESSAGES);
    guard.clear();
}

#[cfg(test)]
/// Get current number of captured info strings (test-only)
pub fn test_info_len() -> usize {
    let guard = lock_or_recover_generic(&INFO_MESSAGES);
    guard.len()
}

#[cfg(test)]
/// Get a snapshot of info strings from the given index (test-only)
pub fn test_info_from(start: usize) -> Vec<String> {
    let guard = lock_or_recover_generic(&INFO_MESSAGES);
    if start >= guard.len() {
        return Vec::new();
    }
    guard[start..].to_vec()
}

/// USI protocol responses
#[derive(Debug, Clone)]
pub enum UsiResponse {
    /// Engine identification - name
    IdName(String),

    /// Engine identification - author
    IdAuthor(String),

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

    /// Score bound type (exact, lowerbound, upperbound)
    pub score_bound: Option<ScoreBound>,

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
    Mate(i32),
}

/// Score bound type for USI output
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScoreBound {
    /// Exact score
    Exact,
    /// Lower bound (score is at least this value)
    LowerBound,
    /// Upper bound (score is at most this value)
    UpperBound,
}

impl From<NodeType> for ScoreBound {
    fn from(node_type: NodeType) -> Self {
        match node_type {
            NodeType::Exact => ScoreBound::Exact,
            NodeType::LowerBound => ScoreBound::LowerBound,
            NodeType::UpperBound => ScoreBound::UpperBound,
        }
    }
}

impl fmt::Display for UsiResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UsiResponse::IdName(name) => write!(f, "id name {name}"),
            UsiResponse::IdAuthor(author) => write!(f, "id author {author}"),
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
            let mut score_str = match score {
                Score::Cp(cp) => format!("score cp {cp}"),
                Score::Mate(mate) => format!("score mate {mate}"),
            };

            // Add bound type if present
            if let Some(bound) = self.score_bound {
                match bound {
                    ScoreBound::LowerBound => score_str.push_str(" lowerbound"),
                    ScoreBound::UpperBound => score_str.push_str(" upperbound"),
                    ScoreBound::Exact => {} // No suffix for exact scores
                }
            }

            parts.push(score_str);
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
const MAX_RETRY_ATTEMPTS: u32 = 8; // Increased for buffered writes

// Buffering configuration
#[cfg(feature = "buffered-io")]
const DEFAULT_FLUSH_INTERVAL_MS: u64 = 100;
#[cfg(feature = "buffered-io")]
const DEFAULT_FLUSH_MESSAGE_COUNT: u32 = 10;

/// Get flush interval from environment variable (for testing)
#[cfg(feature = "buffered-io")]
fn get_flush_interval_ms() -> u64 {
    const MAX_FLUSH_INTERVAL: u64 = 10_000; // 10秒

    std::env::var("USI_FLUSH_DELAY_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(|v| {
            if v == 0 && (cfg!(test) || std::env::var("USI_BENCH_MODE").is_ok()) {
                v
            } else {
                v.clamp(1, MAX_FLUSH_INTERVAL) // 上限値も設定
            }
        })
        .unwrap_or(DEFAULT_FLUSH_INTERVAL_MS)
}

/// Get flush message count from environment variable (for testing)
#[cfg(feature = "buffered-io")]
fn get_flush_message_count() -> u32 {
    const MAX_FLUSH_MESSAGE_COUNT: u32 = 1000; // 1000メッセージ

    std::env::var("USI_FLUSH_MESSAGE_COUNT")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .map(|v| v.clamp(1, MAX_FLUSH_MESSAGE_COUNT)) // 1以上、上限値以下
        .unwrap_or(DEFAULT_FLUSH_MESSAGE_COUNT)
}

/// Flush strategy for messages
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FlushKind {
    /// Immediate flush for critical messages (usiok, readyok, bestmove)
    Immediate,
    /// Buffered flush for non-critical messages (info)
    #[cfg(any(feature = "buffered-io", test))]
    Buffered,
}

/// USI output writer with buffering capabilities
///
/// Note: We use BufWriter<Stdout> instead of BufWriter<StdoutLock> because
/// StdoutLock is !Send and cannot be stored in a static variable.
/// The performance difference is minimal as stdout() caches the handle internally.
struct UsiWriter {
    inner: Mutex<BufWriter<std::io::Stdout>>,
    #[cfg(feature = "buffered-io")]
    last_flush: Mutex<Instant>,
    #[cfg(feature = "buffered-io")]
    message_count: AtomicU32,
}

impl UsiWriter {
    fn new() -> Self {
        Self {
            inner: Mutex::new(BufWriter::with_capacity(8192, std::io::stdout())),
            #[cfg(feature = "buffered-io")]
            last_flush: Mutex::new(Instant::now()),
            #[cfg(feature = "buffered-io")]
            message_count: AtomicU32::new(0),
        }
    }

    fn write_line(&self, response: &UsiResponse, flush_kind: FlushKind) -> std::io::Result<()> {
        // Handle poisoned mutex gracefully - important for stdout reliability
        let mut writer = lock_or_recover_generic(&self.inner);

        // Write the response
        writeln!(writer, "{response}")?;

        // Phase 1: Always flush immediately (behavior compatible)
        // Phase 2: Will implement conditional flushing based on flush_kind
        #[cfg(not(feature = "buffered-io"))]
        {
            let _ = flush_kind; // Suppress unused warning
            writer.flush()?;
        }

        #[cfg(feature = "buffered-io")]
        {
            match flush_kind {
                FlushKind::Immediate => {
                    writer.flush()?;
                    // Note: Using lock_or_recover_generic for consistency, though panic is unlikely here
                    *lock_or_recover_generic(&self.last_flush) = Instant::now();
                    self.message_count.store(0, Ordering::Relaxed);
                }
                FlushKind::Buffered => {
                    // Increment message count
                    let count = self.message_count.fetch_add(1, Ordering::Relaxed) + 1;

                    // Check if we should flush
                    let should_flush = {
                        // Note: Using lock_or_recover_generic for consistency, though panic is unlikely here
                        let last_flush = *lock_or_recover_generic(&self.last_flush);
                        let elapsed = last_flush.elapsed();

                        // Flush based on configurable thresholds
                        count >= get_flush_message_count()
                            || elapsed >= Duration::from_millis(get_flush_interval_ms())
                    };

                    if should_flush {
                        writer.flush()?;
                        // Note: Using lock_or_recover_generic for consistency, though panic is unlikely here
                        *lock_or_recover_generic(&self.last_flush) = Instant::now();
                        self.message_count.store(0, Ordering::Relaxed);
                    }
                }
            }
        }

        Ok(())
    }

    fn flush_all(&self) -> std::io::Result<()> {
        // Handle poisoned mutex gracefully - ensures final messages are sent even after panic
        let mut writer = lock_or_recover_generic(&self.inner);
        writer.flush()
    }

    /// Try to flush without blocking (for panic handler)
    fn try_flush_all(&self) -> std::io::Result<()> {
        match self.inner.try_lock() {
            Ok(mut writer) => writer.flush(),
            Err(_) => {
                // Mutex is locked, possibly by the same thread in a panic
                // Skip flushing to avoid deadlock
                Ok(())
            }
        }
    }
}

/// Global USI writer instance
static USI_WRITER: Lazy<UsiWriter> = Lazy::new(UsiWriter::new);

/// Send USI response with error handling and retry logic
fn send_response_with_retry(response: &UsiResponse) -> std::io::Result<()> {
    use std::io;

    // Determine flush strategy based on response type
    let flush_kind = match response {
        UsiResponse::IdName(_)
        | UsiResponse::IdAuthor(_)
        | UsiResponse::UsiOk
        | UsiResponse::ReadyOk
        | UsiResponse::BestMove { .. } => FlushKind::Immediate,
        UsiResponse::String(_) => FlushKind::Immediate, // Error messages should flush immediately
        _ => {
            #[cfg(any(feature = "buffered-io", test))]
            {
                FlushKind::Buffered
            }
            #[cfg(not(any(feature = "buffered-io", test)))]
            {
                FlushKind::Immediate
            }
        }
    };

    // Try to write with retries
    for attempt in 0..MAX_RETRY_ATTEMPTS {
        match USI_WRITER.write_line(response, flush_kind) {
            Ok(()) => {
                // Reset error count on success
                STDOUT_ERROR_COUNT.store(0, Ordering::Relaxed);
                return Ok(());
            }
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe => {
                // BrokenPipe - no point in retrying
                log::debug!("stdout-write: broken pipe detected");
                return Err(e);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {
                // EINTR - retry immediately without sleep
                log::debug!("stdout-write: interrupted, retrying immediately");
                continue;
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock && attempt < MAX_RETRY_ATTEMPTS - 1 => {
                // WouldBlock - exponential backoff
                let delay_ms = 1u64 << attempt.min(4); // 1, 2, 4, 8, 16ms max
                log::debug!("stdout-write: would block, retry after {delay_ms}ms");
                thread::sleep(Duration::from_millis(delay_ms));
                continue;
            }
            Err(e) if attempt < MAX_RETRY_ATTEMPTS - 1 => {
                // Other errors - exponential backoff to avoid blocking time-critical responses
                let delay_ms = 1u64 << attempt.min(4); // 1, 2, 4, 8, 16ms max
                let retry_num = attempt + 1;
                log::warn!(
                    "stdout-write: failed with {e}, retry {retry_num}/{MAX_RETRY_ATTEMPTS} after {delay_ms}ms"
                );
                thread::sleep(Duration::from_millis(delay_ms));
            }
            Err(e) => {
                // Final attempt failed
                return Err(e);
            }
        }
    }

    // Should not reach here, but return error if we do
    Err(io::Error::other("Max retry attempts exceeded"))
}

/// Error types for stdout operations
#[derive(Debug, thiserror::Error)]
pub enum StdoutError {
    #[error("Broken pipe detected, GUI disconnected")]
    BrokenPipe,

    #[error("Too many stdout errors ({0})")]
    TooManyErrors(u32),

    #[error("Failed to send critical response: {0}")]
    CriticalMessageFailed(#[from] std::io::Error),
}

/// Check if USI output is disabled for debugging stdout blocking issues
fn usi_disabled() -> bool {
    std::env::var("USI_DRY_RUN").as_deref() == Ok("1")
}

/// Send USI response with error handling, returning Result for proper error propagation
///
/// Use this in main thread and contexts where errors can be propagated up the call stack.
pub fn send_response(response: UsiResponse) -> Result<(), StdoutError> {
    use std::io;

    // Skip all USI output if USI_DRY_RUN is set
    if usi_disabled() {
        return Ok(());
    }

    // Determine if this is a critical response
    let is_critical = matches!(
        response,
        UsiResponse::IdName(_)
            | UsiResponse::IdAuthor(_)
            | UsiResponse::UsiOk
            | UsiResponse::ReadyOk
            | UsiResponse::BestMove { .. }
    );

    // Try to send with retry
    if let Err(e) = send_response_with_retry(&response) {
        // Increment error count
        let error_count = STDOUT_ERROR_COUNT.fetch_add(1, Ordering::Relaxed) + 1;

        // Handle based on error type and criticality
        match e.kind() {
            io::ErrorKind::BrokenPipe => Err(StdoutError::BrokenPipe),
            _ if error_count >= MAX_STDOUT_ERRORS => Err(StdoutError::TooManyErrors(error_count)),
            _ if is_critical => Err(StdoutError::CriticalMessageFailed(e)),
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

/// Helper to send info string message, returning Result for error propagation
///
/// Use this in main thread and contexts where errors can be propagated up the call stack.
/// For worker threads and fire-and-forget contexts, wrap this with appropriate error handling.
pub fn send_info_string(message: impl Into<String>) -> Result<(), StdoutError> {
    let msg: String = message.into();
    #[cfg(test)]
    {
        let mut guard = lock_or_recover_generic(&INFO_MESSAGES);
        guard.push(msg.clone());
    }
    send_response(UsiResponse::String(msg))
}

/// Ensure stdout is flushed on exit
/// Call this early in main() to set up panic and exit hooks
pub fn ensure_flush_on_exit() {
    // Force initialization of USI_WRITER
    Lazy::force(&USI_WRITER);

    // Set panic hook to flush on panic
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        // Use try_flush_all to avoid deadlock if panic occurs during write_line
        let _ = USI_WRITER.try_flush_all();
        original_hook(panic_info);
    }));
}

/// Flush any remaining buffered output
/// Call this before normal program exit
pub fn flush_final() -> std::io::Result<()> {
    USI_WRITER.flush_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_usi_response_formatting() {
        let resp = UsiResponse::IdName("RustShogi 1.0".to_string());
        assert_eq!(resp.to_string(), "id name RustShogi 1.0");

        let resp = UsiResponse::IdAuthor("Rust Team".to_string());
        assert_eq!(resp.to_string(), "id author Rust Team");

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

    #[test]
    fn test_search_info_with_bound_flags() {
        // Test mate score with lowerbound
        let info = SearchInfo {
            depth: Some(18),
            score: Some(Score::Mate(-3)),
            score_bound: Some(ScoreBound::LowerBound),
            pv: vec!["7g7f".to_string()],
            ..Default::default()
        };
        let resp = UsiResponse::Info(info);
        assert_eq!(resp.to_string(), "info depth 18 score mate -3 lowerbound pv 7g7f");

        // Test cp score with upperbound
        let info = SearchInfo {
            depth: Some(12),
            score: Some(Score::Cp(250)),
            score_bound: Some(ScoreBound::UpperBound),
            pv: vec!["2g2f".to_string(), "8c8d".to_string()],
            ..Default::default()
        };
        let resp = UsiResponse::Info(info);
        assert_eq!(resp.to_string(), "info depth 12 score cp 250 upperbound pv 2g2f 8c8d");

        // Test exact score (no bound flag)
        let info = SearchInfo {
            depth: Some(15),
            score: Some(Score::Mate(5)),
            score_bound: Some(ScoreBound::Exact),
            pv: vec!["3g3f".to_string()],
            ..Default::default()
        };
        let resp = UsiResponse::Info(info);
        assert_eq!(resp.to_string(), "info depth 15 score mate 5 pv 3g3f");

        // Test mate 0 output
        let info = SearchInfo {
            depth: Some(12),
            score: Some(Score::Mate(0)),
            pv: vec!["7g7f".to_string()],
            ..Default::default()
        };
        let resp = UsiResponse::Info(info);
        assert_eq!(resp.to_string(), "info depth 12 score mate 0 pv 7g7f");
    }
}
