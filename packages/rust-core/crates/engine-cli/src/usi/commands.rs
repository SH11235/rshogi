//! USI protocol command definitions

use std::fmt;

/// USI protocol commands
#[derive(Debug, Clone, PartialEq)]
pub enum UsiCommand {
    /// Initialize USI mode
    Usi,

    /// Check if engine is ready
    IsReady,

    /// Set engine option
    SetOption { name: String, value: Option<String> },

    /// Set position
    Position {
        startpos: bool,
        sfen: Option<String>,
        moves: Vec<String>,
    },

    /// Start search
    Go(GoParams),

    /// Ponder hit (opponent played expected move)
    PonderHit,

    /// Stop searching
    Stop,

    /// Game over notification
    GameOver { result: GameResult },

    /// New game notification (ShogiGUI extension)
    UsiNewGame,

    /// Quit the engine
    Quit,
}

/// Parameters for go command
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GoParams {
    /// Ponder mode (think on opponent's time)
    pub ponder: bool,

    /// Black time in milliseconds
    pub btime: Option<u64>,

    /// White time in milliseconds
    pub wtime: Option<u64>,

    /// Byoyomi time in milliseconds
    pub byoyomi: Option<u64>,

    /// Byoyomi periods (non-standard extension)
    pub periods: Option<u32>,

    /// Black increment in milliseconds
    pub binc: Option<u64>,

    /// White increment in milliseconds
    pub winc: Option<u64>,

    /// Fixed time per move in milliseconds
    pub movetime: Option<u64>,

    /// Maximum search depth
    pub depth: Option<u32>,

    /// Maximum nodes to search
    pub nodes: Option<u64>,

    /// Search indefinitely
    pub infinite: bool,

    /// Moves until next time control
    pub moves_to_go: Option<u32>,
}

/// Game result for gameover command
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GameResult {
    Win,
    Lose,
    Draw,
}

impl fmt::Display for GameResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GameResult::Win => write!(f, "win"),
            GameResult::Lose => write!(f, "lose"),
            GameResult::Draw => write!(f, "draw"),
        }
    }
}
