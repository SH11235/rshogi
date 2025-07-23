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

/// Engine option types
#[derive(Debug, Clone, PartialEq)]
pub enum OptionType {
    Check {
        default: bool,
    },
    Spin {
        default: i64,
        min: i64,
        max: i64,
    },
    Combo {
        default: String,
        values: Vec<String>,
    },
    Button,
    String {
        default: String,
    },
}

/// Engine option definition
#[derive(Debug, Clone)]
pub struct EngineOption {
    pub name: String,
    pub option_type: OptionType,
}

impl EngineOption {
    /// Create a check (boolean) option
    pub fn check(name: impl Into<String>, default: bool) -> Self {
        Self {
            name: name.into(),
            option_type: OptionType::Check { default },
        }
    }

    /// Create a spin (numeric) option
    pub fn spin(name: impl Into<String>, default: i64, min: i64, max: i64) -> Self {
        Self {
            name: name.into(),
            option_type: OptionType::Spin { default, min, max },
        }
    }

    /// Create a combo (selection) option
    pub fn combo(name: impl Into<String>, default: String, values: Vec<String>) -> Self {
        Self {
            name: name.into(),
            option_type: OptionType::Combo { default, values },
        }
    }

    /// Create a button option
    pub fn button(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            option_type: OptionType::Button,
        }
    }

    /// Create a string option
    pub fn string(name: impl Into<String>, default: String) -> Self {
        Self {
            name: name.into(),
            option_type: OptionType::String { default },
        }
    }

    /// Format as USI option string
    pub fn to_usi_string(&self) -> String {
        match &self.option_type {
            OptionType::Check { default } => {
                format!("option name {} type check default {}", self.name, default)
            }
            OptionType::Spin { default, min, max } => {
                format!(
                    "option name {} type spin default {} min {} max {}",
                    self.name, default, min, max
                )
            }
            OptionType::Combo { default, values } => {
                let vars = values.iter().map(|v| format!("var {v}")).collect::<Vec<_>>().join(" ");
                format!("option name {} type combo default {} {}", self.name, default, vars)
            }
            OptionType::Button => {
                format!("option name {} type button", self.name)
            }
            OptionType::String { default } => {
                format!("option name {} type string default {}", self.name, default)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_option_formatting() {
        let opt = EngineOption::check("USI_Ponder", true);
        assert_eq!(opt.to_usi_string(), "option name USI_Ponder type check default true");

        let opt = EngineOption::spin("USI_Hash", 16, 1, 1024);
        assert_eq!(opt.to_usi_string(), "option name USI_Hash type spin default 16 min 1 max 1024");

        let opt = EngineOption::combo(
            "Style",
            "Normal".to_string(),
            vec![
                "Solid".to_string(),
                "Normal".to_string(),
                "Risky".to_string(),
            ],
        );
        assert_eq!(
            opt.to_usi_string(),
            "option name Style type combo default Normal var Solid var Normal var Risky"
        );
    }
}
