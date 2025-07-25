//! USI protocol command parser

use super::commands::{GameResult, GoParams, UsiCommand};
use super::{MAX_BYOYOMI_PERIODS, MIN_BYOYOMI_PERIODS};
use anyhow::{anyhow, Result};
use log::warn;

/// Parse USI command from input line
pub fn parse_usi_command(line: &str) -> Result<UsiCommand> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.is_empty() {
        return Err(anyhow!("Empty command"));
    }

    match parts[0] {
        "usi" => Ok(UsiCommand::Usi),
        "isready" => Ok(UsiCommand::IsReady),
        "quit" => Ok(UsiCommand::Quit),
        "stop" => Ok(UsiCommand::Stop),
        "ponderhit" => Ok(UsiCommand::PonderHit),

        "setoption" => parse_setoption(&parts[1..]),
        "position" => parse_position(&parts[1..]),
        "go" => parse_go(&parts[1..]),
        "gameover" => parse_gameover(&parts[1..]),

        _ => Err(anyhow!("Unknown command: {}", parts[0])),
    }
}

/// Parse setoption command
fn parse_setoption(parts: &[&str]) -> Result<UsiCommand> {
    // Expected format: name <name> [value <value>]
    if parts.len() < 2 || parts[0] != "name" {
        return Err(anyhow!("Invalid setoption format"));
    }

    // Find value keyword
    let value_pos = parts.iter().position(|&p| p == "value");

    let name = if let Some(pos) = value_pos {
        parts[1..pos].join(" ")
    } else {
        parts[1..].join(" ")
    };

    let value = value_pos.and_then(|pos| {
        if pos + 1 < parts.len() {
            let val = parts[pos + 1..].join(" ");
            if val.is_empty() {
                None // Treat empty string after "value" as None
            } else {
                Some(val)
            }
        } else {
            None // "value" keyword without actual value
        }
    });

    Ok(UsiCommand::SetOption { name, value })
}

/// Parse position command
fn parse_position(parts: &[&str]) -> Result<UsiCommand> {
    if parts.is_empty() {
        return Err(anyhow!("Invalid position format"));
    }

    let (startpos, sfen, moves_start) = if parts[0] == "startpos" {
        (true, None, 1)
    } else if parts[0] == "sfen" {
        // Find "moves" keyword
        let moves_pos = parts.iter().position(|&p| p == "moves");
        let sfen_end = moves_pos.unwrap_or(parts.len());

        if sfen_end <= 1 {
            return Err(anyhow!("Invalid SFEN format"));
        }

        let sfen = parts[1..sfen_end].join(" ");
        (false, Some(sfen), sfen_end)
    } else {
        return Err(anyhow!("Position must start with 'startpos' or 'sfen'"));
    };

    // Parse moves if present
    let moves = if moves_start < parts.len() && parts[moves_start] == "moves" {
        if moves_start + 1 >= parts.len() {
            return Err(anyhow!("'moves' keyword requires at least one move"));
        }
        parts[moves_start + 1..].iter().map(|&s| s.to_string()).collect()
    } else {
        Vec::new()
    };

    Ok(UsiCommand::Position {
        startpos,
        sfen,
        moves,
    })
}

/// Parse go command
fn parse_go(parts: &[&str]) -> Result<UsiCommand> {
    let mut params = GoParams::default();
    let mut i = 0;

    while i < parts.len() {
        match parts[i] {
            "ponder" => params.ponder = true,
            "infinite" => params.infinite = true,

            "btime" => {
                i += 1;
                if i >= parts.len() {
                    return Err(anyhow!("go btime requires a value"));
                }
                params.btime = Some(
                    parts[i].parse().map_err(|_| anyhow!("Invalid btime value: {}", parts[i]))?,
                )
            }
            "wtime" => {
                i += 1;
                if i >= parts.len() {
                    return Err(anyhow!("go wtime requires a value"));
                }
                params.wtime = Some(
                    parts[i].parse().map_err(|_| anyhow!("Invalid wtime value: {}", parts[i]))?,
                )
            }
            "byoyomi" => {
                i += 1;
                if i >= parts.len() {
                    return Err(anyhow!("go byoyomi requires a value"));
                }
                params.byoyomi = Some(
                    parts[i].parse().map_err(|_| anyhow!("Invalid byoyomi value: {}", parts[i]))?,
                )
            }
            "binc" => {
                i += 1;
                if i >= parts.len() {
                    return Err(anyhow!("go binc requires a value"));
                }
                params.binc = Some(
                    parts[i].parse().map_err(|_| anyhow!("Invalid binc value: {}", parts[i]))?,
                )
            }
            "winc" => {
                i += 1;
                if i >= parts.len() {
                    return Err(anyhow!("go winc requires a value"));
                }
                params.winc = Some(
                    parts[i].parse().map_err(|_| anyhow!("Invalid winc value: {}", parts[i]))?,
                )
            }
            "movetime" => {
                i += 1;
                if i >= parts.len() {
                    return Err(anyhow!("go movetime requires a value"));
                }
                params.movetime = Some(
                    parts[i]
                        .parse()
                        .map_err(|_| anyhow!("Invalid movetime value: {}", parts[i]))?,
                )
            }
            "depth" => {
                i += 1;
                if i >= parts.len() {
                    return Err(anyhow!("go depth requires a value"));
                }
                params.depth = Some(
                    parts[i].parse().map_err(|_| anyhow!("Invalid depth value: {}", parts[i]))?,
                )
            }
            "nodes" => {
                i += 1;
                if i >= parts.len() {
                    return Err(anyhow!("go nodes requires a value"));
                }
                params.nodes = Some(
                    parts[i].parse().map_err(|_| anyhow!("Invalid nodes value: {}", parts[i]))?,
                )
            }
            "movestogo" => {
                i += 1;
                if i >= parts.len() {
                    return Err(anyhow!("go movestogo requires a value"));
                }
                params.moves_to_go = Some(
                    parts[i]
                        .parse()
                        .map_err(|_| anyhow!("Invalid movestogo value: {}", parts[i]))?,
                )
            }
            "periods" => {
                i += 1;
                if i >= parts.len() {
                    return Err(anyhow!("go periods requires a value"));
                }
                let periods_val = parts[i]
                    .parse::<u32>()
                    .map_err(|_| anyhow!("Invalid periods value: {}", parts[i]))?;
                if periods_val < MIN_BYOYOMI_PERIODS {
                    return Err(anyhow!("Periods must be at least {}", MIN_BYOYOMI_PERIODS));
                }
                // Clamp periods to MIN-MAX range (same as SetOption)
                let clamped_periods = periods_val.min(MAX_BYOYOMI_PERIODS);
                if periods_val != clamped_periods {
                    warn!(
                        "Periods value {periods_val} exceeds maximum {MAX_BYOYOMI_PERIODS}, clamping to {clamped_periods}"
                    );
                }
                params.periods = Some(clamped_periods);
            }
            _ => {
                // Unknown parameter, skip
                warn!("Unknown go parameter: {}", parts[i]);
            }
        }
        i += 1;
    }

    Ok(UsiCommand::Go(params))
}

/// Parse gameover command
fn parse_gameover(parts: &[&str]) -> Result<UsiCommand> {
    if parts.is_empty() {
        return Err(anyhow!("Invalid gameover format"));
    }

    let result = match parts[0] {
        "win" => GameResult::Win,
        "lose" => GameResult::Lose,
        "draw" => GameResult::Draw,
        _ => return Err(anyhow!("Invalid game result: {}", parts[0])),
    };

    Ok(UsiCommand::GameOver { result })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_commands() {
        assert_eq!(parse_usi_command("usi").unwrap(), UsiCommand::Usi);
        assert_eq!(parse_usi_command("isready").unwrap(), UsiCommand::IsReady);
        assert_eq!(parse_usi_command("quit").unwrap(), UsiCommand::Quit);
        assert_eq!(parse_usi_command("stop").unwrap(), UsiCommand::Stop);
        assert_eq!(parse_usi_command("ponderhit").unwrap(), UsiCommand::PonderHit);
    }

    #[test]
    fn test_parse_setoption() {
        let cmd = parse_usi_command("setoption name USI_Ponder value true").unwrap();
        match cmd {
            UsiCommand::SetOption { name, value } => {
                assert_eq!(name, "USI_Ponder");
                assert_eq!(value, Some("true".to_string()));
            }
            _ => panic!("Expected SetOption"),
        }

        let cmd = parse_usi_command("setoption name Clear Hash").unwrap();
        match cmd {
            UsiCommand::SetOption { name, value } => {
                assert_eq!(name, "Clear Hash");
                assert_eq!(value, None);
            }
            _ => panic!("Expected SetOption"),
        }
    }

    #[test]
    fn test_parse_position() {
        let cmd = parse_usi_command("position startpos").unwrap();
        match cmd {
            UsiCommand::Position {
                startpos,
                sfen,
                moves,
            } => {
                assert!(startpos);
                assert!(sfen.is_none());
                assert!(moves.is_empty());
            }
            _ => panic!("Expected Position"),
        }

        let cmd = parse_usi_command("position startpos moves 7g7f 3c3d").unwrap();
        match cmd {
            UsiCommand::Position {
                startpos,
                sfen,
                moves,
            } => {
                assert!(startpos);
                assert!(sfen.is_none());
                assert_eq!(moves, vec!["7g7f", "3c3d"]);
            }
            _ => panic!("Expected Position"),
        }

        let cmd = parse_usi_command(
            "position sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
        )
        .unwrap();
        match cmd {
            UsiCommand::Position {
                startpos,
                sfen,
                moves,
            } => {
                assert!(!startpos);
                assert!(sfen.is_some());
                assert!(moves.is_empty());
            }
            _ => panic!("Expected Position"),
        }
    }

    #[test]
    fn test_parse_go() {
        let cmd = parse_usi_command("go").unwrap();
        assert_eq!(cmd, UsiCommand::Go(GoParams::default()));

        let cmd = parse_usi_command("go infinite").unwrap();
        match cmd {
            UsiCommand::Go(params) => {
                assert!(params.infinite);
            }
            _ => panic!("Expected Go"),
        }

        let cmd = parse_usi_command("go btime 60000 wtime 50000 byoyomi 10000").unwrap();
        match cmd {
            UsiCommand::Go(params) => {
                assert_eq!(params.btime, Some(60000));
                assert_eq!(params.wtime, Some(50000));
                assert_eq!(params.byoyomi, Some(10000));
            }
            _ => panic!("Expected Go"),
        }

        let cmd = parse_usi_command("go ponder movetime 1000 depth 10").unwrap();
        match cmd {
            UsiCommand::Go(params) => {
                assert!(params.ponder);
                assert_eq!(params.movetime, Some(1000));
                assert_eq!(params.depth, Some(10));
            }
            _ => panic!("Expected Go"),
        }
    }

    #[test]
    fn test_parse_go_with_periods() {
        // Test periods parsing
        let cmd = parse_usi_command("go byoyomi 30000 periods 3").unwrap();
        match cmd {
            UsiCommand::Go(params) => {
                assert_eq!(params.byoyomi, Some(30000));
                assert_eq!(params.periods, Some(3));
            }
            _ => panic!("Expected Go"),
        }

        // Test periods without byoyomi (should still parse)
        let cmd = parse_usi_command("go btime 300000 wtime 300000 periods 5").unwrap();
        match cmd {
            UsiCommand::Go(params) => {
                assert_eq!(params.periods, Some(5));
            }
            _ => panic!("Expected Go"),
        }

        // Test periods 0 error
        let result = parse_usi_command("go byoyomi 30000 periods 0");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Periods must be at least 1"));

        // Test invalid periods value
        let result = parse_usi_command("go byoyomi 30000 periods abc");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid periods value"));

        // Test periods clamping to max
        let cmd = parse_usi_command("go byoyomi 30000 periods 15").unwrap();
        match cmd {
            UsiCommand::Go(params) => {
                assert_eq!(params.periods, Some(MAX_BYOYOMI_PERIODS)); // Should be clamped to MAX
            }
            _ => panic!("Expected Go"),
        }
    }

    #[test]
    fn test_parse_gameover() {
        let cmd = parse_usi_command("gameover win").unwrap();
        assert_eq!(
            cmd,
            UsiCommand::GameOver {
                result: GameResult::Win
            }
        );

        let cmd = parse_usi_command("gameover draw").unwrap();
        assert_eq!(
            cmd,
            UsiCommand::GameOver {
                result: GameResult::Draw
            }
        );
    }

    #[test]
    fn test_parse_errors() {
        assert!(parse_usi_command("").is_err());
        assert!(parse_usi_command("unknown").is_err());
        assert!(parse_usi_command("setoption").is_err());
        assert!(parse_usi_command("position").is_err());
        assert!(parse_usi_command("gameover invalid").is_err());
    }

    #[test]
    fn test_go_parameter_errors() {
        // Missing values for required parameters
        assert!(parse_usi_command("go btime").is_err());
        assert!(parse_usi_command("go wtime").is_err());
        assert!(parse_usi_command("go byoyomi").is_err());
        assert!(parse_usi_command("go movetime").is_err());
        assert!(parse_usi_command("go depth").is_err());
        assert!(parse_usi_command("go nodes").is_err());

        // Invalid numeric values
        assert!(parse_usi_command("go btime abc").is_err());
        assert!(parse_usi_command("go nodes 1.5").is_err());
    }

    #[test]
    fn test_position_moves_validation() {
        // "moves" keyword without actual moves
        let result = parse_usi_command("position startpos moves");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("requires at least one move"));
    }

    #[test]
    fn test_setoption_empty_value() {
        // "value" keyword without actual value should be None
        let cmd = parse_usi_command("setoption name SomeOption value").unwrap();
        match cmd {
            UsiCommand::SetOption { name, value } => {
                assert_eq!(name, "SomeOption");
                assert_eq!(value, None);
            }
            _ => panic!("Expected SetOption"),
        }

        // Empty string after value should be None
        let cmd = parse_usi_command("setoption name SomeOption value ").unwrap();
        match cmd {
            UsiCommand::SetOption { name, value } => {
                assert_eq!(name, "SomeOption");
                assert_eq!(value, None);
            }
            _ => panic!("Expected SetOption"),
        }
    }
}
