//! USI (Universal Shogi Interface) protocol implementation

pub mod commands;
pub mod output;
pub mod parser;

pub use commands::{EngineOption, GameResult, GoParams, UsiCommand};
pub use output::{send_info_string, send_response, Score, SearchInfo, UsiResponse};
pub use parser::parse_usi_command;

/// Standard engine options
pub fn default_options() -> Vec<EngineOption> {
    vec![
        EngineOption::spin("USI_Hash", 16, 1, 1024),
        EngineOption::check("USI_Ponder", true),
        EngineOption::spin("Threads", 1, 1, 128),
        EngineOption::button("Clear Hash"),
    ]
}
