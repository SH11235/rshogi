//! USI protocol output formatting

use crate::utils::lock_or_recover_generic;
use crossbeam_channel::{unbounded, Receiver, Sender};
use engine_core::search::NodeType;
use once_cell::sync::Lazy;
use std::fmt;
use std::io::{BufWriter, Write};
use std::sync::Mutex;
use std::thread::JoinHandle;

#[cfg(test)]
static INFO_MESSAGES: Lazy<Mutex<Vec<String>>> = Lazy::new(|| Mutex::new(Vec::new()));

// Thread-local capture to avoid cross-test interference when tests run in parallel.
// Tests in this crate typically emit and then immediately read back logs on the same thread.
#[cfg(test)]
thread_local! {
    static TL_INFO_MESSAGES: std::cell::RefCell<Vec<String>> = std::cell::RefCell::new(Vec::new());
}

#[cfg(test)]
/// Get current number of captured info strings (test-only)
pub fn test_info_len() -> usize {
    TL_INFO_MESSAGES.with(|v| v.borrow().len())
}

#[cfg(test)]
/// Get a snapshot of info strings from the given index (test-only)
pub fn test_info_from(start: usize) -> Vec<String> {
    TL_INFO_MESSAGES.with(|v| {
        let v = v.borrow();
        if start >= v.len() {
            return Vec::new();
        }
        v[start..].to_vec()
    })
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
// Deprecated error counters/retry constants removed in single-writer model

// Flush strategy for messages (deprecated in single-writer model)

/// USI output writer with buffering capabilities
///
/// Note: We use BufWriter<Stdout> instead of BufWriter<StdoutLock> because
/// StdoutLock is !Send and cannot be stored in a static variable.
/// The performance difference is minimal as stdout() caches the handle internally.
struct UsiWriter {
    inner: Mutex<BufWriter<std::io::Stdout>>,
}

impl UsiWriter {
    fn new() -> Self {
        Self {
            inner: Mutex::new(BufWriter::with_capacity(8192, std::io::stdout())),
        }
    }

    // Deprecated write_line/flush_all removed in single-writer model

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

    // Single-writer path: write a preformatted line and flush immediately
    fn write_line_raw(&self, line: &str) -> std::io::Result<()> {
        let mut writer = lock_or_recover_generic(&self.inner);
        writeln!(writer, "{}", line)?;
        writer.flush()?;
        Ok(())
    }
}

/// Global USI writer instance
static USI_WRITER: Lazy<UsiWriter> = Lazy::new(UsiWriter::new);

// Single-writer channel and thread
pub enum OutputMsg {
    Line(String),
    Flush,
    Shutdown,
}

static OUTPUT_CHAN: Lazy<(Sender<OutputMsg>, Receiver<OutputMsg>)> = Lazy::new(unbounded);
static WRITER_THREAD: Lazy<JoinHandle<()>> = Lazy::new(|| {
    std::thread::spawn(move || {
        let (_tx, rx) = (&OUTPUT_CHAN.0, &OUTPUT_CHAN.1);
        loop {
            match rx.recv() {
                Ok(OutputMsg::Line(line)) => {
                    let _ = USI_WRITER.write_line_raw(&line);
                }
                Ok(OutputMsg::Flush) => {
                    let _ = USI_WRITER.try_flush_all();
                }
                Ok(OutputMsg::Shutdown) | Err(_) => {
                    let _ = USI_WRITER.try_flush_all();
                    break;
                }
            }
        }
    })
});

/// Send USI response with error handling and retry logic
/// Flush any buffered output immediately (non-blocking request)
pub fn flush_now() {
    let _ = OUTPUT_CHAN.0.send(OutputMsg::Flush);
}
/// Deprecated retry path removed in single-writer model
/// Error types for stdout operations
#[derive(Debug, thiserror::Error)]
pub enum StdoutError {
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
    // Skip all USI output if USI_DRY_RUN is set
    if usi_disabled() {
        return Ok(());
    }
    // Enqueue line for single-writer thread
    let line = response.to_string();
    let _ = OUTPUT_CHAN.0.send(OutputMsg::Line(line));
    Ok(())
}

/// Helper to send info string message, returning Result for error propagation
///
/// Use this in main thread and contexts where errors can be propagated up the call stack.
/// For worker threads and fire-and-forget contexts, wrap this with appropriate error handling.
pub fn send_info_string(message: impl Into<String>) -> Result<(), StdoutError> {
    let msg: String = message.into();
    #[cfg(test)]
    {
        // Keep a global copy for potential debugging, and a thread-local copy for isolation.
        let mut guard = lock_or_recover_generic(&INFO_MESSAGES);
        guard.push(msg.clone());
        TL_INFO_MESSAGES.with(|v| v.borrow_mut().push(msg.clone()));
    }
    if usi_disabled() {
        return Ok(());
    }
    let _ = OUTPUT_CHAN.0.send(OutputMsg::Line(UsiResponse::String(msg).to_string()));
    Ok(())
}

/// Ensure stdout is flushed on exit
/// Call this early in main() to set up panic and exit hooks
pub fn ensure_flush_on_exit() {
    // Force initialization of USI_WRITER
    Lazy::force(&USI_WRITER);
    // Spawn writer thread
    Lazy::force(&WRITER_THREAD);

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
    let _ = OUTPUT_CHAN.0.send(OutputMsg::Flush);
    let _ = OUTPUT_CHAN.0.send(OutputMsg::Shutdown);
    Ok(())
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
